use anyhow::Result;
use cutad::media::{
    Format, Output, executable, interpolate_clip_motion, monitor_frame, probe,
    render_timeline_section, reverse_clip, stabilize_clip,
};
use cutad::project::{Clip, Project, load, save};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

fn samples(path: &Path) -> Result<Vec<i16>> {
    let output = Command::new(executable("ffmpeg")?)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "s16le", "-",
        ])
        .output()?;
    assert!(output.status.success());
    Ok(output
        .stdout
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

#[test]
fn reversal_keeps_the_trimmed_picture_order_and_audio_across_chunks() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("source.mkv");
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=64x64:r=30:d=9",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.1*sin(2*PI*440*t)*t/9:s=48000:d=9",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
        ])
        .arg(&source)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asset = probe(&source)?;
    let clip = Clip::new(asset.id.clone(), 17, 15, 200);
    let id = clip.id.clone();
    let cancel = AtomicBool::new(false);
    let target = folder.path().join("reversed.mkv");
    let (reversed, subframe) = reverse_clip(&clip, &asset, &target, &cancel, |_| {})?;
    assert_eq!(reversed.frames, 200);
    assert_eq!(subframe, 0);
    for tick in [0, 20, 149, 150, 190, 199] {
        let expected = monitor_frame(&source, 214 - tick, [64, 64])?;
        let actual = monitor_frame(&target, tick, [64, 64])?;
        let average = actual
            .iter()
            .zip(&expected)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / actual.len() as f64;
        assert!(
            average < 4.0,
            "Reversed frame {tick} had mean RGB error {average}"
        );
    }
    let input = samples(&source)?;
    let actual = samples(&target)?;
    assert_eq!(actual.len(), 200 * 1600);
    let reference = &input[15 * 1600..215 * 1600];
    let mean = actual
        .iter()
        .zip(reference.iter().rev())
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / actual.len() as f64;
    assert!(mean < 12.0, "Reversed audio mean sample error {mean}");

    let mut project = Project::default();
    project.assets.push(asset.clone());
    project.tracks[0].clips.push(clip.clone());
    project.replace_media(&id, reversed)?;
    assert_eq!(
        (
            project.tracks[0].clips[0].start,
            project.tracks[0].clips[0].length
        ),
        (17, 200)
    );
    let saved = folder.path().join("reverse.cutad");
    save(&project, &saved)?;
    assert_eq!(load(&saved)?, project);
    assert!(reverse_clip(&clip, &asset, &target, &cancel, |_| {}).is_err());
    let canceled = folder.path().join("canceled.mkv");
    assert!(reverse_clip(&clip, &asset, &canceled, &AtomicBool::new(true), |_| {}).is_err());
    assert!(!canceled.exists());
    let cancel_midway = AtomicBool::new(false);
    let partial = folder.path().join("partial.mkv");
    assert!(
        reverse_clip(&clip, &asset, &partial, &cancel_midway, |_| {
            cancel_midway.store(true, Ordering::Relaxed);
        })
        .is_err()
    );
    assert!(!partial.exists());
    Ok(())
}

#[test]
fn optical_flow_generates_new_frames_without_retiming_the_source_audio() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("movement.mkv");
    let ffmpeg = executable("ffmpeg")?;
    let fixture = Command::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=96x64:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=450:sample_rate=48000:duration=2",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
        ])
        .arg(&source)
        .output()?;
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let asset = probe(&source)?;
    let mut project = Project::default();
    let clip = Clip::new(asset.id.clone(), 7, 8, 30);
    let id = clip.id.clone();
    project.assets.push(asset.clone());
    project.tracks[0].clips.push(clip);
    project.set_clip_speed(&id, 25)?;
    let clip = &project.tracks[0].clips[0];
    let output = folder.path().join("flow.mkv");
    let cancel = AtomicBool::new(false);
    let (result, subframe) = interpolate_clip_motion(clip, &asset, &output, &cancel, |_| {})?;
    assert_eq!(
        (result.frames, result.geometry.unwrap().fps, subframe),
        (30, 120, 0)
    );
    let decoded = Command::new(&ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args([
            "-an", "-f", "rawvideo", "-pix_fmt", "rgb24", "-threads", "1", "-",
        ])
        .output()?;
    assert!(decoded.status.success());
    let frames: Vec<_> = decoded.stdout.chunks_exact(96 * 64 * 3).collect();
    assert_eq!(frames.len(), 120);
    assert!((0..29).any(|i| {
        let anchor = i * 4;
        frames[anchor] != frames[anchor + 2] && frames[anchor + 4] != frames[anchor + 2]
    }));
    let actual_audio = samples(&output)?;
    let original = samples(&source)?;
    assert!(
        actual_audio == original[8 * 1600..38 * 1600],
        "The prepared file changed audio timing or samples."
    );
    project.replace_media(&id, result)?;
    assert_eq!(project.tracks[0].clips[0].length, 120);
    assert_eq!(project.tracks[0].clips[0].speed_percent, 25);
    project.validate()?;
    (project.width, project.height) = (96, 64);
    let slow = folder.path().join("quarter-speed.mkv");
    render_timeline_section(
        &project,
        &slow,
        Output::new(Format::Mkv),
        7..23,
        &cancel,
        |_| {},
    )?;
    let sampled = [0, 1, 2, 3].map(|tick| monitor_frame(&slow, tick, [96, 64]));
    let sampled = sampled.into_iter().collect::<Result<Vec<_>>>()?;
    assert!(
        sampled.windows(2).any(|pair| pair[0] != pair[1]),
        "Quarter-speed export duplicated every frame in the first source tick."
    );
    assert!(
        interpolate_clip_motion(
            &project.tracks[0].clips[0],
            &asset,
            &output,
            &cancel,
            |_| {}
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn camera_stabilization_reduces_synthetic_shake_and_keeps_the_clip_length() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let texture = folder.path().join("texture.png");
    let source = folder.path().join("shaky.mkv");
    let target = folder.path().join("stabilized.mkv");
    let ffmpeg = executable("ffmpeg")?;
    let image = Command::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=192x144",
            "-vf",
            "drawgrid=w=17:h=17:t=1:c=white@0.7",
            "-frames:v",
            "1",
            "-threads",
            "1",
        ])
        .arg(&texture)
        .output()?;
    assert!(
        image.status.success(),
        "{}",
        String::from_utf8_lossy(&image.stderr)
    );
    let fixture = Command::new(&ffmpeg)
        .args(["-v", "error", "-loop", "1", "-framerate", "30", "-i"])
        .arg(&texture)
        .args([
            "-t",
            "2",
            "-vf",
            "crop=w=160:h=112:x='16+8*sin(n*2.1)':y='16+5*cos(n*2.7)'",
            "-c:v",
            "ffv1",
            "-threads",
            "1",
        ])
        .arg(&source)
        .output()?;
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let asset = probe(&source)?;
    let clip = Clip::new(asset.id.clone(), 12, 0, 60);
    let (result, phase) = stabilize_clip(&clip, &asset, &target, &AtomicBool::new(false), |_| {})?;
    assert_eq!(
        (result.frames, result.geometry.unwrap().fps, phase),
        (60, 30, 0)
    );
    let change = |path: &Path| -> Result<f64> {
        let frame_size = 112 * 64;
        let video = Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args([
                "-vf",
                "crop=112:64",
                "-pix_fmt",
                "gray",
                "-threads",
                "1",
                "-f",
                "rawvideo",
                "-",
            ])
            .output()?;
        assert!(video.status.success());
        let before = &video.stdout[..video.stdout.len() - frame_size];
        let after = &video.stdout[frame_size..];
        Ok(before
            .iter()
            .zip(after)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / after.len() as f64)
    };
    let before = change(&source)?;
    let after = change(&target)?;
    assert!(
        after < before * 0.8,
        "Frame-to-frame change went from {before} to {after}."
    );
    Ok(())
}
