use anyhow::Result;
use cutad::media::{PreviewQuality, probe, render_preview_frame_with};
use cutad::project::{Clip, Project, load, save};
use std::sync::atomic::AtomicBool;

#[test]
fn a_position_animation_survives_split_and_matches_scrubbed_frames() -> Result<()> {
    let work = tempfile::tempdir()?;
    let file = work.path().join("marker.ppm");
    let mut ppm = b"P6\n64 64\n255\n".to_vec();
    ppm.extend([240, 20, 15].repeat(64 * 64));
    std::fs::write(&file, ppm)?;
    let marker = probe(&file)?;
    let mut project = Project {
        width: 160,
        height: 90,
        ..Project::default()
    };
    let mut clip = Clip::new(marker.id.clone(), 0, 0, 60);
    clip.transform.scale = 0.35;
    clip.set_position_keyframe(0, [-0.30, 0.0])?;
    clip.set_position_keyframe(40, [0.30, 0.0])?;
    let id = clip.id.clone();
    project.assets.push(marker);
    project.tracks[0].clips.push(clip);
    let render = |project: &Project, frame| {
        render_preview_frame_with(
            project,
            frame,
            PreviewQuality::Full,
            &AtomicBool::new(false),
        )
    };
    let left = render(&project, 4)?;
    let right = render(&project, 32)?;
    let pixel = |data: &[u8], x: usize| data[(45 * 160 + x) * 3];
    assert!(pixel(&left, 42) > 175 && pixel(&left, 108) < 80);
    assert!(pixel(&right, 108) > 175 && pixel(&right, 42) < 80);
    let new = project.split(&id, 25)?;
    let (row, index) = project.location(&new).unwrap();
    assert!((project.tracks[row].clips[index].position_at(7)[0] - 0.18).abs() < 0.001);
    let saved = work.path().join("move.cutad");
    save(&project, &saved)?;
    assert_eq!(right, render(&load(&saved)?, 32)?);
    Ok(())
}
