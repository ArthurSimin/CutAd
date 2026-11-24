use anyhow::Result;
use cutad::media::{
    Format, PreviewQuality, monitor_frame, probe, render, render_preview_frame_with,
};
use cutad::project::{AnimatedProperty, Clip, Easing, Project, load, save};
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

#[test]
fn easing_progress_is_preserved_when_scrubbing_after_a_split() -> Result<()> {
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
    let mut clip = Clip::new(marker.id.clone(), 3, 0, 48);
    clip.transform.scale = 0.15;
    clip.set_position_keyframe(0, [-0.4, 0.0])?;
    clip.set_position_keyframe(40, [0.4, 0.0])?;
    clip.position_animation.easing = Easing::EaseIn;
    assert!((clip.position_at(20)[0] + 0.2).abs() < 0.001);
    let id = clip.id.clone();
    project.assets.push(marker);
    project.tracks[0].clips.push(clip);
    let cancel = AtomicBool::new(false);
    let before = render_preview_frame_with(&project, 23, PreviewQuality::Full, &cancel)?;
    let pixel = |data: &[u8], x: usize| data[(45 * 160 + x) * 3];
    assert!(pixel(&before, 48) > 175 && pixel(&before, 80) < 80);
    project.split(&id, 17)?;
    assert_eq!(
        before,
        render_preview_frame_with(&project, 23, PreviewQuality::Full, &cancel)?
    );
    Ok(())
}

#[test]
fn rotation_and_scale_keyframes_match_export_and_preserve_progress_after_split() -> Result<()> {
    let work = tempfile::tempdir()?;
    let file = work.path().join("stripes.ppm");
    let mut ppm = b"P6\n32 32\n255\n".to_vec();
    for _ in 0..32 {
        ppm.extend([240, 20, 15].repeat(16));
        ppm.extend([15, 20, 240].repeat(16));
    }
    std::fs::write(&file, ppm)?;
    let asset = probe(&file)?;
    let mut project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    let mut clip = Clip::new(asset.id.clone(), 3, 0, 40);
    clip.set_scalar_keyframe(AnimatedProperty::Rotation, 0, 0.0)?;
    clip.set_scalar_keyframe(AnimatedProperty::Rotation, 30, 90.0)?;
    clip.set_scalar_keyframe(AnimatedProperty::Scale, 0, 0.5)?;
    clip.set_scalar_keyframe(AnimatedProperty::Scale, 30, 1.0)?;
    clip.set_scalar_easing(AnimatedProperty::Rotation, Easing::EaseIn);
    clip.set_scalar_easing(AnimatedProperty::Scale, Easing::EaseOut);
    assert_eq!(clip.animated_value(AnimatedProperty::Rotation, 15), 22.5);
    assert_eq!(clip.animated_value(AnimatedProperty::Scale, 15), 0.875);
    let id = clip.id.clone();
    project.tracks[0].clips.push(clip);
    project.assets.push(asset);
    let cancel = AtomicBool::new(false);
    let preview = |project: &Project, tick| {
        render_preview_frame_with(project, tick, PreviewQuality::Full, &cancel)
    };
    let horizontal = preview(&project, 3)?;
    let vertical = preview(&project, 33)?;
    let rgb = |x: usize, y: usize, channel: usize| (y * 64 + x) * 3 + channel;
    assert!(horizontal[rgb(24, 32, 0)] > 200 && horizontal[rgb(40, 32, 2)] > 200);
    assert!(horizontal[rgb(12, 32, 0)] < 40);
    assert!(vertical[rgb(32, 12, 0)] > 200 && vertical[rgb(32, 52, 2)] > 200);
    let mid = preview(&project, 22)?;
    let export = work.path().join("rotate.mkv");
    render(&project, &export, Format::Mkv, &cancel, |_| {})?;
    assert!(mid == monitor_frame(&export, 22, [64, 64])?);
    project.split(&id, 15)?;
    let file = work.path().join("rotate.cutad");
    save(&project, &file)?;
    assert!(mid == preview(&load(&file)?, 22)?);
    Ok(())
}
