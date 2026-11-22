use anyhow::{Result, ensure};
use cutad::media::{Format, executable, probe, render};
use cutad::project::{AssetKind, Clip, Project, Track, TrackKind};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

fn ffmpeg(args: &[&str]) -> Result<Vec<u8>> {
    let result = Command::new(executable("ffmpeg")?)
        .args(["-nostdin", "-v", "error", "-y"])
        .args(args)
        .output()?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn render_layers_mixed_rate_video_and_delayed_audio() -> Result<()> {
    let folder = tempdir()?;
    let red = folder.path().join("red base.mov");
    let blue = folder.path().join("blue overlay.ppm");
    let tone = folder.path().join("sine tone.wav");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=red:s=160x90:r=25:d=2",
        "-c:v",
        "mpeg4",
        text(&red),
    ])?;
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=blue:s=160x90",
        "-frames:v",
        "1",
        text(&blue),
    ])?;
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=1",
        text(&tone),
    ])?;
    let assets = vec![probe(&red)?, probe(&blue)?, probe(&tone)?];
    assert_eq!(assets[1].kind, AssetKind::Image);
    let track = |index: usize, kind, start, duration| Track {
        clips: vec![Clip::new(assets[index].id.clone(), start, 0, duration)],
        ..Track::new("Track", kind)
    };
    let project = Project {
        width: 160,
        height: 90,
        tracks: vec![
            track(0, TrackKind::Video, 0, 60),
            track(1, TrackKind::Video, 30, 15),
            track(2, TrackKind::Audio, 30, 30),
        ],
        assets,
        ..Project::default()
    };
    let output = folder.path().join("edited.mp4");
    render(
        &project,
        &output,
        Format::Mp4,
        &AtomicBool::new(false),
        |_| {},
    )?;
    assert_eq!(probe(&output)?.frames, 60);
    assert!(probe(&output)?.audio);
    let pixel = |time| {
        ffmpeg(&[
            "-ss",
            time,
            "-i",
            text(&output),
            "-frames:v",
            "1",
            "-vf",
            "scale=1:1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
    };
    assert!(pixel("0.5")?[0] > 180);
    assert!(pixel("1.2")?[2] > 180);
    assert!(pixel("1.8")?[0] > 180);
    let sound = |time| {
        ffmpeg(&[
            "-ss",
            time,
            "-i",
            text(&output),
            "-t",
            "0.4",
            "-vn",
            "-f",
            "s16le",
            "-",
        ])
    };
    assert_eq!(sound("0.3")?.into_iter().max(), Some(0));
    assert!(sound("1.3")?.into_iter().max().unwrap_or_default() > 20);
    assert!(red.is_file() && blue.is_file() && tone.is_file());
    let webm = folder.path().join("edited.webm");
    render(
        &project,
        &webm,
        Format::Webm,
        &AtomicBool::new(false),
        |_| {},
    )?;
    assert_eq!(probe(&webm)?.frames, 60);
    Ok(())
}

#[test]
fn canceled_export_preserves_existing_destination() -> Result<()> {
    let folder = tempdir()?;
    let path = folder.path().join("edited.mp4");
    std::fs::write(&path, b"previous output")?;
    let error = render(
        &Project::default(),
        &path,
        Format::Mp4,
        &AtomicBool::new(true),
        |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("canceled"));
    assert_eq!(std::fs::read(path)?, b"previous output");
    Ok(())
}

#[test]
fn visual_transforms_and_title_clips_render_into_the_export() -> Result<()> {
    use cutad::media::{Output, Quality, monitor_frame, render_with};
    use cutad::project::{TitleStyle, Transform};

    let folder = tempdir()?;
    let path = folder.path().join("red.ppm");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=red:s=160x90",
        "-frames:v",
        "1",
        text(&path),
    ])?;
    let asset = probe(&path)?;
    let mut clip = Clip::new(asset.id.clone(), 0, 0, 45);
    clip.transform = Transform {
        scale: 0.5,
        x: 0.2,
        ..Transform::default()
    };
    let mut project = Project {
        width: 160,
        height: 90,
        assets: vec![asset],
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new("Video", TrackKind::Video)
        }],
        ..Project::default()
    };
    project.add_title(TitleStyle::Caption, "Test title", 10)?;
    project.validate()?;
    let output = folder.path().join("edit.mp4");
    render_with(
        &project,
        &output,
        Output {
            format: Format::Mp4,
            quality: Quality::High,
            short_edge: None,
            fps: 30,
        },
        &AtomicBool::new(false),
        |_| {},
    )?;
    let first = monitor_frame(&output, 3, [160, 90])?;
    let later = monitor_frame(&output, 13, [160, 90])?;
    assert!(first[(45 * 160 + 5) * 3] < 75);
    assert!(first[(45 * 160 + 110) * 3] > 180);
    assert!(
        later
            .chunks_exact(3)
            .any(|pixel| pixel.iter().all(|v| *v > 170))
    );
    assert_ne!(first, later);
    Ok(())
}
