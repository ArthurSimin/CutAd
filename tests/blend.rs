use anyhow::Result;
use cutad::media::{
    Format, PreviewQuality, monitor_frame, probe, render, render_preview_frame_with,
};
use cutad::project::{BlendMode, Clip, Project, Track, TrackKind};
use std::sync::atomic::AtomicBool;

#[test]
fn multiply_blends_the_timed_layer_rgb_and_respects_its_opacity() -> Result<()> {
    let work = tempfile::tempdir()?;
    let mut project = Project {
        width: 32,
        height: 32,
        ..Project::default()
    };
    project.tracks.push(Track::new("Overlay", TrackKind::Video));
    for (row, color) in [(0, [200, 100, 40]), (2, [128, 255, 128])] {
        let path = work.path().join(format!("solid{row}.ppm"));
        let mut ppm = b"P6\n32 32\n255\n".to_vec();
        ppm.extend(color.repeat(32 * 32));
        std::fs::write(&path, ppm)?;
        let asset = probe(&path)?;
        let mut clip = Clip::new(asset.id.clone(), 0, 0, 60);
        if row == 2 {
            clip.start = 4;
            clip.length = 48;
            clip.transform.opacity = 0.75;
            clip.blend_mode = BlendMode::Multiply;
            clip.set_opacity_keyframe(0, 0.0)?;
            clip.set_opacity_keyframe(16, 1.0)?;
        }
        project.tracks[row].clips.push(clip);
        project.assets.push(asset);
    }
    let cancel = AtomicBool::new(false);
    let preview = |project: &Project, time| {
        render_preview_frame_with(project, time, PreviewQuality::Full, &cancel)
    };
    let export = work.path().join("blend.mkv");
    render(&project, &export, Format::Mkv, &cancel, |_| {})?;
    for (time, expected) in [
        (0, [200, 100, 40]),
        (4, [200, 100, 40]),
        (12, [150, 100, 30]),
        (20, [100, 100, 20]),
        (54, [200, 100, 40]),
    ] {
        let preview = preview(&project, time)?;
        let delivery = monitor_frame(&export, time, [32, 32])?;
        for frame in [&preview, &delivery] {
            let rgb = &frame[(16 * 32 + 16) * 3..][..3];
            assert!(
                rgb.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 6),
                "{time}: {rgb:?}"
            );
        }
    }
    let before = preview(&project, 12)?;
    let id = project.tracks[2].clips[0].id.clone();
    let tail = project.split(&id, 10)?;
    let (row, col) = project.location(&tail).unwrap();
    assert!((project.tracks[row].clips[col].opacity_at(2) - 0.5).abs() < f32::EPSILON);
    let after = preview(&project, 12)?;
    assert!(
        before == after,
        "split changed the opacity at the same timeline position"
    );
    Ok(())
}
