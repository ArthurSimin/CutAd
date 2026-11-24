use anyhow::Result;
use cutad::media::{executable, probe};
use cutad::project::{Clip, Project, load, save};
use cutad::tracking::{Seed, track_overlay};
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[test]
fn an_overlay_follows_a_moving_patch_and_keeps_its_keyframes_after_reopen() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let patch = folder.path().join("patch.png");
    let target = folder.path().join("target.mkv");
    let ffmpeg = executable("ffmpeg")?;
    let image = Command::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=44x44",
            "-frames:v",
            "1",
            "-threads",
            "1",
        ])
        .arg(&patch)
        .output()?;
    assert!(
        image.status.success(),
        "{}",
        String::from_utf8_lossy(&image.stderr)
    );
    let fixture = Command::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x202020:s=192x128:r=30:d=1",
            "-loop",
            "1",
            "-i",
        ])
        .arg(&patch)
        .args([
            "-filter_complex",
            "[0:v][1:v]overlay=x='64+30*t':y=42:shortest=1",
            "-c:v",
            "ffv1",
            "-threads",
            "1",
        ])
        .arg(&target)
        .output()?;
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let asset = probe(&target)?;
    let mut project = Project {
        width: 192,
        height: 128,
        ..Project::default()
    };
    project.tracks[0]
        .clips
        .push(Clip::new(asset.id.clone(), 0, 0, 30));
    project.assets.push(asset);
    let overlay = project.add_sticker(probe(&patch)?, 0)?;
    let seed = Seed {
        x: 45,
        y: 50,
        size: 24,
    };
    let animation = track_overlay(project.clone(), &overlay, seed, &AtomicBool::new(false))?;
    let end = animation.keys.last().unwrap();
    assert_eq!(end.frame, 29);
    assert!(
        (end.position[0] - 28.0 / 192.0).abs() < 0.025,
        "{:?}",
        end.position
    );
    assert!(end.position[1].abs() < 0.025, "{:?}", end.position);
    let (row, index) = project.location(&overlay).unwrap();
    project.tracks[row].clips[index].position_animation = animation;
    let saved = folder.path().join("tracking.cutad");
    save(&project, &saved)?;
    assert_eq!(load(&saved)?, project);
    assert!(track_overlay(project, &overlay, seed, &AtomicBool::new(true)).is_err());
    Ok(())
}
