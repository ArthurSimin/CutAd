use cutad::media::{PreviewQuality, render_preview_frame_with};
use cutad::project::{AnimatedProperty, Asset, AssetKind, Clip, Project, load, save, template};
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[test]
fn ripple_delete_shifts_only_later_clips_on_the_selected_track() -> anyhow::Result<()> {
    let mut project = Project::default();
    let asset = Asset::video("scene.mp4", 120, true);
    let id = asset.id.clone();
    project.assets.push(asset);
    let preceding = Clip::new(id.clone(), 0, 0, 10);
    let removed = Clip::new(id.clone(), 13, 10, 16);
    let removed_id = removed.id.clone();
    let following = Clip::new(id.clone(), 34, 26, 20);
    let audio = Clip::new(id, 15, 0, 20);
    project.tracks[0]
        .clips
        .extend([preceding, removed, following]);
    project.tracks[1].clips.push(audio.clone());

    assert_eq!(project.ripple_delete(&removed_id)?, 13);
    assert!(project.location(&removed_id).is_none());
    let clips = &project.tracks[0].clips;
    assert_eq!(
        (clips[0].start, clips[1].start, clips[1].source),
        (0, 18, 26)
    );
    assert_eq!(project.tracks[1].clips, [audio]);
    project.validate()?;
    Ok(())
}

#[test]
fn pasting_visual_settings_keeps_destination_footage_timing_and_audio() -> anyhow::Result<()> {
    let mut source = Clip::new("source".into(), 35, 20, 30);
    source.transform.scale = 0.75;
    source.set_scalar_keyframe(AnimatedProperty::Rotation, 10, 25.0)?;
    source.gain = 1.8;
    let mut dest = Clip::new("destination".into(), 5, 4, 40);
    let id = dest.id.clone();
    dest.paste_settings(&source, true, false);
    assert_eq!(dest.id, id);
    assert_eq!(dest.asset_id, "destination");
    assert_eq!((dest.start, dest.source, dest.length), (5, 4, 40));
    assert_eq!(dest.gain, 1.0);
    assert_eq!(dest.transform.scale, 0.75);
    assert_eq!(dest.animated_value(AnimatedProperty::Rotation, 10), 25.0);
    Ok(())
}

#[test]
fn named_timeline_markers_stay_ordered_and_survive_project_reopen() -> anyhow::Result<()> {
    let folder = tempdir()?;
    let mut project = Project::default();
    project.set_marker(90, "Outro")?;
    project.set_marker(24, "First cut")?;
    project.set_marker(24, "New name")?;
    assert_eq!(project.markers.len(), 2);
    assert_eq!(
        (project.markers[0].frame, project.markers[0].label.as_str()),
        (24, "New name")
    );
    assert!(project.set_marker(25, "  ").is_err());
    let path = folder.path().join("markers.cutad");
    save(&project, &path)?;
    let mut reopened = load(&path)?;
    assert_eq!(reopened.markers, project.markers);
    reopened.remove_marker(24);
    assert_eq!(reopened.markers[0].frame, 90);
    Ok(())
}

#[test]
fn trim_split_and_round_trip() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let media = dir.path().join("video.mp4");
    let mut project = Project::default();
    let asset = Asset::video(&media, 120, true);
    let id = asset.id.clone();
    project.assets.push(asset);
    let clip = Clip::new(id, 15, 10, 60);
    let clip_id = clip.id.clone();
    project.tracks[0].clips.push(clip);
    project.split(&clip_id, 40)?;
    let clips = &project.tracks[0].clips;
    assert_eq!(
        (clips[0].start, clips[0].source, clips[0].length),
        (15, 10, 25)
    );
    assert_eq!(
        (clips[1].start, clips[1].source, clips[1].length),
        (40, 35, 35)
    );
    let file = dir.path().join("test.cutad");
    save(&project, &file)?;
    assert!(std::fs::read_to_string(&file)?.contains(r#""path": "video.mp4""#));
    assert_eq!(load(&file)?, project);
    Ok(())
}

#[test]
fn overlaps_cannot_be_persisted() {
    let mut project = Project::default();
    let asset = Asset::video("video.mp4", 120, false);
    project.tracks[0].clips.extend([
        Clip::new(asset.id.clone(), 0, 0, 100),
        Clip::new(asset.id.clone(), 90, 0, 10),
    ]);
    project.assets.push(asset);
    assert!(
        project
            .validate()
            .unwrap_err()
            .to_string()
            .contains("overlap")
    );
}

#[test]
fn template_slots_replace_independently() -> anyhow::Result<()> {
    let mut project = template("Portrait montage")?;
    let id = project.tracks[0].clips[0].id.clone();
    project.replace_media(&id, Asset::video("replacement.mp4", 60, true))?;
    project.validate()?;
    assert!(!project.tracks[0].clips[0].slot);
    assert!(project.tracks[0].clips[1].slot);
    assert_eq!(project.duration(), 180);
    Ok(())
}

#[test]
fn the_two_panel_and_reveal_templates_are_editable_and_render_after_reopen() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let mut files = Vec::new();
    for (name, color) in [("before", [240, 12, 8]), ("after", [8, 240, 12])] {
        let path = dir.path().join(format!("{name}.ppm"));
        let mut bytes = b"P6\n32 32\n255\n".to_vec();
        bytes.extend(color.repeat(32 * 32));
        std::fs::write(&path, bytes)?;
        files.push(path);
    }
    let cancel = AtomicBool::new(false);
    for (name, expected_canvas, width, height, duration) in [
        ("Side-by-side review", (1280, 720), 160, 90, 120),
        ("Vertical reveal", (720, 1280), 90, 160, 150),
    ] {
        let mut project = template(name)?;
        assert_eq!((project.width, project.height), expected_canvas);
        assert_eq!(project.duration(), duration);
        (project.width, project.height) = (width, height);
        let slots = project
            .tracks
            .iter()
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.slot)
            .map(|clip| clip.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(slots.len(), 2);
        for (id, file) in slots.iter().zip(&files) {
            project.replace_media(id, Asset::local(file, AssetKind::Image, 1, false))?;
        }
        let saved = dir.path().join(format!("{name}.cutad"));
        save(&project, &saved)?;
        let project = load(&saved)?;
        assert_eq!(project.duration(), duration);
        let preview =
            |tick| render_preview_frame_with(&project, tick, PreviewQuality::Full, &cancel);
        let pixel = |pixels: &[u8], x, y, channel| pixels[(y * width as usize + x) * 3 + channel];
        if name == "Side-by-side review" {
            let frame = preview(20)?;
            assert!(pixel(&frame, 40, 45, 0) > 210 && pixel(&frame, 120, 45, 1) > 210);
        } else {
            assert!(pixel(&preview(30)?, 45, 68, 0) > 210);
            assert!(pixel(&preview(75)?, 45, 68, 1) > 210);
        }
    }
    Ok(())
}
