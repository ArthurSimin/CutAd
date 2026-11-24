use anyhow::Result;
use cutad::effects::{Effect, Preset};
use cutad::media::{
    AudioFormat, Format, Output, detect_clip_beats, executable, export_audio,
    normalize_clip_loudness, probe, render, render_audio_master, render_audio_mix, render_preview,
    render_with,
};
use cutad::project::{
    AnimatedProperty, Asset, AssetKind, AudioChannels, Clip, Project, Track, TrackKind, load, save,
};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn ffmpeg(args: &[&str]) -> Result<Vec<u8>> {
    let result = Command::new(executable("ffmpeg")?)
        .args(["-nostdin", "-v", "error", "-y"])
        .args(args)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

#[test]
fn beat_detection_respects_clip_trim_and_timeline_position() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let music = folder.path().join("pulse.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        r"aevalsrc=0.25*sin(2*PI*100*t)*exp(-70*mod(t+0.25\,0.5))*gte(t\,0.25)+0.04*sin(2*PI*330*t):s=48000:d=6",
        "-c:a",
        "pcm_s16le",
        music.to_str().unwrap(),
    ])?;
    let song = probe(&music)?;
    let clip = Clip::new(song.id.clone(), 28, 15, 150);
    let cancel = AtomicBool::new(false);
    let positions = detect_clip_beats(&clip, &song, &cancel)?;
    let expected: Vec<u32> = (0..10).map(|beat| 36 + 15 * beat).collect();
    assert_eq!(positions.len(), expected.len(), "{positions:?}");
    assert!(
        positions
            .iter()
            .zip(expected)
            .all(|(found, expected)| found.abs_diff(expected) <= 1),
        "{positions:?}"
    );

    let steady = folder.path().join("drone.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=330:sample_rate=48000:duration=5",
        steady.to_str().unwrap(),
    ])?;
    let asset = probe(&steady)?;
    let clip = Clip::new(asset.id.clone(), 0, 0, asset.frames);
    assert!(detect_clip_beats(&clip, &asset, &cancel)?.is_empty());
    Ok(())
}

fn audio(path: &Path) -> Result<Vec<i16>> {
    Ok(ffmpeg(&[
        "-i",
        path.to_str().unwrap(),
        "-vn",
        "-f",
        "s16le",
        "-ac",
        "1",
        "-ar",
        "48000",
        "-",
    ])?
    .chunks_exact(2)
    .map(|bytes| i16::from_le_bytes(bytes.try_into().unwrap()))
    .collect())
}

#[test]
fn gain_keyframes_survive_split_reopen_and_range_rendering() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join("tone.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=440:sample_rate=48000:duration=4",
        path.to_str().unwrap(),
    ])?;
    let asset = probe(&path)?;
    let mut clip = Clip::new(asset.id.clone(), 8, 14, 65);
    let id = clip.id.clone();
    for (frame, gain) in [(0, 0.0), (30, 1.0), (64, 0.0)] {
        clip.set_scalar_keyframe(AnimatedProperty::AudioGain, frame, gain)?;
    }
    let mut project = Project {
        assets: vec![asset],
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new("Tone", TrackKind::Audio)
        }],
        ..Project::default()
    };
    let cancel = AtomicBool::new(false);
    let full = folder.path().join("full.wav");
    render_audio_master(&project, &full, &cancel, |_| {})?;
    let original = audio(&full)?;
    assert!(tone_rms(&original, 37) > tone_rms(&original, 15) * 3.0);
    assert!(tone_rms(&original, 37) > tone_rms(&original, 69) * 3.0);

    project.split(&id, 25)?;
    let file = folder.path().join("gain.cutad");
    save(&project, &file)?;
    let restored = load(&file)?;
    assert_eq!(restored, project);
    let split = folder.path().join("split.wav");
    render_audio_master(&restored, &split, &cancel, |_| {})?;
    assert_eq!(audio(&split)?, original);

    let range = folder.path().join("range.wav");
    render_audio_mix(&restored, &range, 36..52, &cancel, |_| {})?;
    let ranged = audio(&range)?;
    assert!((tone_rms(&ranged, 4) - tone_rms(&original, 40)).abs() < 20.0);
    Ok(())
}

fn tone_rms(samples: &[i16], frame: usize) -> f64 {
    let segment = &samples[frame * 1600..(frame + 1) * 1600];
    (segment.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / segment.len() as f64).sqrt()
}

fn audio_stream(path: &Path) -> Result<serde_json::Value> {
    let probe = Command::new(executable("ffprobe")?)
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(path)
        .output()?;
    assert!(probe.status.success());
    let channels: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
    let streams = channels["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1);
    Ok(streams[0].clone())
}

#[test]
fn gain_and_fades_survive_project_round_trip_split_and_preview_window() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let tone = folder.path().join("tone.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=450:sample_rate=48000:duration=3",
        tone.to_str().unwrap(),
    ])?;
    let asset = probe(&tone)?;
    let mut clip = Clip::new(asset.id.clone(), 6, 3, 60);
    clip.gain = 0.5;
    clip.set_audio_fade(18, 18);
    let id = clip.id.clone();
    let mut project = Project {
        width: 32,
        height: 32,
        assets: vec![asset],
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new("Audio", TrackKind::Audio)
        }],
        ..Project::default()
    };
    let cancel = AtomicBool::new(false);
    let export = folder.path().join("full.mkv");
    render(&project, &export, Format::Mkv, &cancel, |_| {})?;
    let full = audio(&export)?;
    let middle = tone_rms(&full, 30);
    assert!((850.0..=1_500.0).contains(&middle), "{middle}");
    assert!(tone_rms(&full, 6) < middle * 0.08);
    assert!((0.45..=0.6).contains(&(tone_rms(&full, 15) / middle)));
    assert!((0.45..=0.6).contains(&(tone_rms(&full, 56) / middle)));
    assert!(tone_rms(&full, 65) < middle * 0.08);

    let preview = folder.path().join("preview.mp4");
    render_preview(&project, &preview, 12..63, &cancel)?;
    let window = audio(&preview)?;
    for start in [3, 19, 44, 49] {
        let difference = (tone_rms(&window, start) - tone_rms(&full, start + 12)).abs();
        assert!(difference < middle * 0.06, "frame {start}: {difference}");
    }

    let silent_image = Asset::local(
        folder.path().join("offline.ppm"),
        AssetKind::Image,
        1,
        false,
    );
    project.tracks.push(Track {
        clips: vec![Clip::new(silent_image.id.clone(), 0, 0, 10)],
        ..Track::new("Silent video", TrackKind::Video)
    });
    project.assets.push(silent_image);
    let transcription = folder.path().join("transcription.wav");
    render_audio_mix(&project, &transcription, 0..63, &cancel, |_| {})?;
    let stream = audio_stream(&transcription)?;
    assert_eq!(stream["codec_name"], "pcm_s16le");
    assert_eq!(stream["sample_rate"], "16000");
    assert_eq!(stream["channels"], 1);
    let mix = audio(&transcription)?;
    for frame in [0, 6, 14, 15, 23, 30, 48, 56, 57] {
        let difference = (tone_rms(&mix, frame) - tone_rms(&full, frame)).abs();
        assert!(difference < middle * 0.02, "frame {frame}: {difference}");
    }
    project.tracks.pop();
    project.assets.pop();

    let tail = project.split(&id, 15)?;
    project.split(&tail, 57)?;
    let path = folder.path().join("project.cutad");
    save(&project, &path)?;
    let split = folder.path().join("split.mkv");
    render(&load(&path)?, &split, Format::Mkv, &cancel, |_| {})?;
    let split = audio(&split)?;
    for frame in [6, 14, 15, 23, 30, 48, 56, 57, 65] {
        let difference = (tone_rms(&split, frame) - tone_rms(&full, frame)).abs();
        assert!(difference < middle * 0.02, "frame {frame}: {difference}");
    }
    Ok(())
}

#[test]
fn stereo_wav_master_matches_the_full_mixer_at_a_different_video_frame_rate() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let tone = folder.path().join("tone.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=450:sample_rate=48000:duration=2",
        tone.to_str().unwrap(),
    ])?;
    let asset = probe(&tone)?;
    let mut clip = Clip::new(asset.id.clone(), 6, 0, 60);
    clip.effects.push(Effect::new(Preset::AudioFadeOut));
    clip.set_audio_fade(12, 4);
    let project = Project {
        width: 32,
        height: 32,
        assets: vec![asset],
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new("Audio", TrackKind::Audio)
        }],
        ..Project::default()
    };
    let path = folder.path().join("master.wav");
    let cancel = AtomicBool::new(false);
    render_audio_master(&project, &path, &cancel, |_| {})?;
    let stream = audio_stream(&path)?;
    assert_eq!(stream["codec_name"], "pcm_s16le");
    assert_eq!(stream["sample_rate"], "48000");
    assert_eq!(stream["channels"], 2);
    let master = audio(&path)?;
    assert_eq!(master.len(), 66 * 1600);
    assert!(tone_rms(&master, 60) < tone_rms(&master, 35) * 0.4);
    let flac = folder.path().join("master.flac");
    export_audio(&project, &flac, AudioFormat::Flac, &cancel, |_| {})?;
    let stream = audio_stream(&flac)?;
    assert_eq!(stream["codec_name"], "flac");
    assert_eq!(stream["bits_per_raw_sample"], "24");
    let lossless = audio(&flac)?;
    assert_eq!(master.len(), lossless.len());
    let difference = master
        .iter()
        .zip(lossless)
        .map(|(a, b)| a.abs_diff(b))
        .max();
    assert!(
        difference.is_some_and(|peak| peak <= 2),
        "FLAC mix differed by {difference:?} quantization steps"
    );
    let mp3 = folder.path().join("master.mp3");
    export_audio(&project, &mp3, AudioFormat::Mp3, &cancel, |_| {})?;
    let stream = audio_stream(&mp3)?;
    assert_eq!(stream["codec_name"], "mp3");
    assert_eq!(stream["channels"], 2);
    let compressed = audio(&mp3)?;
    assert!(compressed.len().abs_diff(master.len()) <= 1152);
    assert!((tone_rms(&compressed, 35) - tone_rms(&master, 35)).abs() < 30.0);

    let av = folder.path().join("movie.mkv");
    render_with(
        &project,
        &av,
        Output {
            fps: 60,
            ..Output::new(Format::Mkv)
        },
        &cancel,
        |_| {},
    )?;
    let av = audio(&av)?;
    for frame in [0, 6, 14, 35, 48, 60, 65] {
        let difference = (tone_rms(&master, frame) - tone_rms(&av, frame)).abs();
        assert!(difference < 5.0, "frame {frame}: {difference}");
    }
    Ok(())
}

#[test]
fn loudness_normalization_sets_a_gain_that_reaches_the_export_target() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let tone = folder.path().join("tone.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=450:sample_rate=48000:duration=3",
        tone.to_str().unwrap(),
    ])?;
    let asset = probe(&tone)?;
    let mut clip = Clip::new(asset.id.clone(), 9, 8, 75);
    clip.gain = 0.25;
    let cancel = AtomicBool::new(false);
    let (gain, estimated) = normalize_clip_loudness(&clip, &asset, &cancel)?;
    assert!((1.0..4.0).contains(&gain), "gain {gain}");
    assert!((estimated + 16.0).abs() < 0.1);
    clip.gain = gain;
    clip.start = 0;
    let project = Project {
        assets: vec![asset],
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new("Normalized", TrackKind::Audio)
        }],
        ..Project::default()
    };
    let output = folder.path().join("normalized.wav");
    render_audio_master(&project, &output, &cancel, |_| {})?;
    let report = Command::new(executable("ffmpeg")?)
        .args(["-nostdin", "-hide_banner", "-i"])
        .arg(&output)
        .args(["-af", "loudnorm=print_format=json", "-f", "null", "-"])
        .output()?;
    assert!(report.status.success());
    let report = String::from_utf8_lossy(&report.stderr);
    let json = &report[report.rfind('{').unwrap()..=report.rfind('}').unwrap()];
    let measured: serde_json::Value = serde_json::from_str(json)?;
    let value = measured["input_i"].as_str().unwrap().parse::<f64>()?;
    assert!((value + 16.0).abs() < 0.25, "{value} LUFS");
    Ok(())
}

#[test]
fn channel_routing_and_balance_are_applied_to_stereo_export() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("right-only.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=0|0.2*sin(2*PI*440*t):s=48000:d=2",
        source.to_str().unwrap(),
    ])?;
    let asset = probe(&source)?;
    let mut clip = Clip::new(asset.id.clone(), 0, 0, 60);
    clip.audio_channels = AudioChannels::Right;
    clip.audio_balance = -100;
    clip.set_audio_fade(5, 5);
    let project = Project {
        assets: vec![asset],
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new("Right source", TrackKind::Audio)
        }],
        ..Project::default()
    };
    let output = folder.path().join("left-only.wav");
    render_audio_master(&project, &output, &AtomicBool::new(false), |_| {})?;
    let audio = ffmpeg(&["-i", output.to_str().unwrap(), "-f", "s16le", "-"])?;
    let samples = audio
        .chunks_exact(4)
        .map(|sample| {
            [
                i16::from_le_bytes([sample[0], sample[1]]),
                i16::from_le_bytes([sample[2], sample[3]]),
            ]
        })
        .collect::<Vec<_>>();
    assert!(samples.iter().any(|sample| sample[0] > 6000));
    assert!(samples.iter().all(|sample| sample[1] == 0));
    Ok(())
}
