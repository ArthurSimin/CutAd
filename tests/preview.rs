use anyhow::{Result, ensure};
use cutad::effects::{Transition, TransitionKind};
use cutad::media::{
    Format, executable, monitor_frame, probe, render, render_preview, render_preview_frame,
};
use cutad::project::{Asset, Clip, Project, TitleStyle};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn still_preview_matches_full_render_during_a_cut_and_title_animation() -> Result<()> {
    let folder = tempdir()?;
    let mut project = Project {
        width: 320,
        height: 180,
        ..Project::default()
    };
    for (index, rgb) in [[242, 5, 5], [14, 43, 240]].into_iter().enumerate() {
        let picture = folder.path().join(format!("color{index}.ppm"));
        let mut bytes = b"P6\n320 180\n255\n".to_vec();
        bytes.extend(rgb.repeat(320 * 180));
        std::fs::write(&picture, bytes)?;
        let asset = probe(&picture)?;
        let mut clip = Clip::new(asset.id.clone(), index as u32 * 30, 0, 30);
        if index == 1 {
            clip.transition = Some(Transition {
                kind: TransitionKind::Dissolve,
                frames: 14,
            });
        }
        project.assets.push(asset);
        project.tracks[0].clips.push(clip);
    }
    project.add_title(TitleStyle::Impact, "Fast preview", 27)?;
    let cancel = AtomicBool::new(false);
    let full = folder.path().join("full.mp4");
    render(&project, &full, Format::Preview, &cancel, |_| {})?;
    for frame in [31, 37, 49] {
        let expected = monitor_frame(&full, frame, [320, 180])?;
        let still = render_preview_frame(&project, frame, &cancel)?;
        assert_close(&expected, &still, frame);
    }
    Ok(())
}

#[test]
fn short_playback_does_not_open_later_source_media() -> Result<()> {
    let folder = tempdir()?;
    let mut project = Project {
        width: 320,
        height: 180,
        ..Project::default()
    };
    project.add_title(TitleStyle::Neon, "Quick start", 0)?;
    let missing = Asset::video(folder.path().join("later.mp4"), 120, false);
    project.tracks[0]
        .clips
        .push(Clip::new(missing.id.clone(), 120, 0, 120));
    project.assets.push(missing);
    let path = folder.path().join("first-section.mp4");
    render_preview(&project, &path, 0..60, &AtomicBool::new(false))?;
    let section = probe(&path)?;
    assert_eq!(section.frames, 60);
    assert!(section.audio);
    Ok(())
}

#[test]
fn preview_seeks_into_trimmed_60fps_video_and_preserves_timing() -> Result<()> {
    let folder = tempdir()?;
    let source = folder.path().join("source.mp4");
    let result = Command::new(executable("ffmpeg")?)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1280x720:r=60:d=12",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=12:sample_rate=48000",
            "-c:v",
            "libx264",
            "-threads",
            "2",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .output()?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let asset = probe(&source)?;
    assert!(asset.preview_seek);
    let mut project = Project::default();
    project.tracks[0]
        .clips
        .push(Clip::new(asset.id.clone(), 0, 30, 330));
    project.assets.push(asset);
    let cancel = AtomicBool::new(false);
    let full = folder.path().join("full.mp4");
    let section = folder.path().join("section.mp4");
    let now = Instant::now();
    render(&project, &full, Format::Preview, &cancel, |_| {})?;
    let full_time = now.elapsed();
    let now = Instant::now();
    render_preview(&project, &section, 155..215, &cancel)?;
    let section_time = now.elapsed();
    eprintln!("11 s full render {full_time:?}, 2 s preview section {section_time:?}");
    assert_eq!(probe(&section)?.frames, 60);
    let size = Format::Preview.size(&project);
    for frame in [155, 166, 214] {
        let expected = monitor_frame(&full, frame, size)?;
        let within_section = monitor_frame(&section, frame - 155, size)?;
        assert_close(&expected, &within_section, frame);
    }
    let still = render_preview_frame(&project, 166, &cancel)?;
    assert_close(&monitor_frame(&full, 166, size)?, &still, 166);
    Ok(())
}

fn assert_close(expected: &[u8], actual: &[u8], frame: u32) {
    assert_eq!(actual.len(), expected.len());
    let error: f64 = expected
        .iter()
        .zip(actual)
        .map(|(a, b)| f64::from(a.abs_diff(*b)))
        .sum::<f64>()
        / expected.len() as f64;
    if error >= 4.5 {
        eprintln!(
            "first pixel {:?} vs {:?}, middle pixel {:?} vs {:?}",
            &expected[0..3],
            &actual[0..3],
            &expected[expected.len() / 2..][0..3],
            &actual[actual.len() / 2..][0..3]
        );
    }
    assert!(error < 4.5, "frame {frame} has mean RGB error {error}");
}
