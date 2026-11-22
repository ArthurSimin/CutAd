use anyhow::Result;
use cutad::effects::{Effect, Preset};
use cutad::media::{
    Format, Output, executable, probe, render, render_audio_master, render_audio_mix,
    render_preview, render_with,
};
use cutad::project::{Asset, AssetKind, Clip, Project, Track, TrackKind, load, save};
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
