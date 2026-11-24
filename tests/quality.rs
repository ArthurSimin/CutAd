use anyhow::{Result, ensure};
use cutad::media::{
    Format, Output, PreviewQuality, executable, probe, render_preview_frame_with, render_with,
};
use cutad::project::{Clip, Project, TitleStyle};
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[test]
fn full_preview_is_canvas_resolution_without_a_preset_backdrop() -> Result<()> {
    let mut project = Project {
        width: 960,
        height: 540,
        ..Project::default()
    };
    project.add_title(TitleStyle::Headline, "Plain title", 0)?;
    let pixels =
        render_preview_frame_with(&project, 0, PreviewQuality::Full, &AtomicBool::new(false))?;
    assert_eq!(pixels.len(), 960 * 540 * 3);
    let purple = pixels
        .chunks_exact(3)
        .filter(|p| p[0] > 30 && p[2].saturating_sub(p[0]) > 7 && p[1] < p[0])
        .count();
    assert!(
        purple < 25,
        "Found {purple} tinted pixels around plain text"
    );
    Ok(())
}

#[test]
fn full_cadence_lossless_export_keeps_distinct_source_frames() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let input = folder.path().join("source.mkv");
    let success = Command::new(executable("ffmpeg")?)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=192x108:r=60:d=1",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv444p",
        ])
        .arg(&input)
        .status()?;
    ensure!(success.success(), "Could not generate the 60 fps source.");
    let asset = probe(&input)?;
    let mut project = Project {
        width: 192,
        height: 108,
        ..Project::default()
    };
    project.tracks[0]
        .clips
        .push(Clip::new(asset.id.clone(), 0, 0, asset.frames));
    project.assets.push(asset);
    assert_eq!(Output::for_project(Format::Mkv, &project).fps, 60);
    let file = folder.path().join("result.mkv");
    render_with(
        &project,
        &file,
        Output::for_project(Format::Mkv, &project),
        &AtomicBool::new(false),
        |_| {},
    )?;
    let decode = |path: &std::path::Path| -> Result<Vec<u8>> {
        let samples = Command::new(executable("ffmpeg")?)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args([
                "-an",
                "-frames:v",
                "6",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-threads",
                "1",
                "-",
            ])
            .output()?;
        ensure!(
            samples.status.success(),
            "Could not decode rendered frames."
        );
        Ok(samples.stdout)
    };
    let pixels = decode(&file)?;
    let frames: Vec<_> = pixels.chunks_exact(192 * 108 * 3).collect();
    assert_eq!(frames.len(), 6);
    assert!(frames.windows(2).all(|pair| pair[0] != pair[1]));
    assert_eq!(
        pixels,
        decode(&input)?,
        "An unchanged 4:4:4 frame should survive the lossless route"
    );
    Ok(())
}
