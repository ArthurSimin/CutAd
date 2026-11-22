use anyhow::{Result, ensure};
use cutad::media::{Format, executable, monitor_frame, probe, render};
use cutad::project::{AssetKind, Project, TitleStyle, load, save};
use std::fs;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[test]
fn sticker_import_keeps_transparency_through_save_and_export() -> Result<()> {
    let dir = tempdir()?;
    let raw = dir.path().join("test.rgba");
    let file = dir.path().join("My sticker.png");
    let mut pixels = vec![0_u8; 96 * 96 * 4];
    for y in 24..72 {
        for x in 24..72 {
            let i = (y * 96 + x) * 4;
            pixels[i..i + 4].copy_from_slice(&[240, 30, 50, 255]);
        }
    }
    fs::write(&raw, pixels)?;
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", "96x96", "-i",
        ])
        .arg(&raw)
        .args(["-frames:v", "1"])
        .arg(&file)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asset = probe(&file)?;
    assert_eq!(asset.kind, AssetKind::Image);
    let mut project = Project {
        width: 320,
        height: 180,
        ..Project::default()
    };
    let id = project.add_sticker(asset, 15)?;
    let (row, _) = project.location(&id).unwrap();
    assert_eq!(project.tracks[row].name, "Stickers");
    project.add_title(TitleStyle::Impact, "TEST TITLE", 25)?;
    let data = dir.path().join("Project.cutad");
    save(&project, &data)?;
    assert_eq!(load(&data)?, project);
    let output = dir.path().join("Sticker preview.mp4");
    render(
        &project,
        &output,
        Format::Preview,
        &AtomicBool::new(false),
        |_| {},
    )?;
    let before = monitor_frame(&output, 3, [320, 180])?;
    let sticker = monitor_frame(&output, 20, [320, 180])?;
    let center = (90 * 320 + 160) * 3;
    assert!(sticker[center] > 200 && before[center] < 40);
    assert!(sticker[0] < 35, "Transparent edges should show the canvas");
    assert_ne!(sticker, monitor_frame(&output, 36, [320, 180])?);
    Ok(())
}
