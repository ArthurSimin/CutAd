use anyhow::Result;
use cutad::project::{Asset, Clip, Project, load, save};

#[test]
fn transcript_range_removal_splits_media_and_moves_later_markers() -> Result<()> {
    let mut project = Project::default();
    let asset = Asset::video("test.mkv", 100, true);
    project.tracks[0]
        .clips
        .push(Clip::new(asset.id.clone(), 0, 10, 70));
    project.tracks[1]
        .clips
        .push(Clip::new(asset.id.clone(), 15, 0, 50));
    project.assets.push(asset);
    project.set_marker(22, "Removed")?;
    project.set_marker(51, "Kept")?;
    project.ripple_delete_range(20, 40)?;
    assert_eq!(project.duration(), 50);
    assert_eq!(project.tracks[0].clips[1].source, 50);
    assert_eq!(project.tracks[0].clips[1].start, 20);
    assert_eq!(project.tracks[1].clips[1].source, 25);
    assert_eq!(project.markers.len(), 1);
    assert_eq!(project.markers[0].frame, 31);
    project.validate()
}

#[test]
fn closing_track_gaps_keeps_source_timing_and_clip_settings() -> Result<()> {
    let mut project = Project::default();
    let asset = Asset::video("test.mkv", 90, false);
    let mut first = Clip::new(asset.id.clone(), 12, 7, 15);
    first.transform.rotation = 12.0;
    let second = Clip::new(asset.id.clone(), 40, 23, 20);
    project.assets.push(asset);
    project.tracks[0]
        .clips
        .extend([second.clone(), first.clone()]);
    assert_eq!(project.close_track_gaps(0)?, 25);
    first.start = 0;
    let mut second = second;
    second.start = 15;
    assert_eq!(project.tracks[0].clips, [first, second]);
    assert_eq!(project.close_track_gaps(0)?, 0);
    Ok(())
}

#[test]
fn groups_preserve_offsets_and_reject_overlapping_moves_atomically() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let asset = Asset::video(folder.path().join("source.mkv"), 90, true);
    let picture = Clip::new(asset.id.clone(), 10, 0, 8);
    let sound = Clip::new(asset.id.clone(), 15, 3, 8);
    let blocker = Clip::new(asset.id.clone(), 40, 0, 8);
    let anchor = picture.id.clone();
    let mut project = Project::default();
    project.assets.push(asset);
    project.tracks[0].clips.extend([picture, blocker]);
    project.tracks[1].clips.push(sound.clone());
    project.group_clips(&[anchor.clone(), sound.id.clone()].into_iter().collect())?;
    let prior = project.clone();
    assert!(project.move_clip_group(&anchor, 0, 40).is_err());
    assert!(project.move_clip_group(&anchor, 1, 20).is_err());
    assert_eq!(project, prior);
    project.move_clip_group(&anchor, 0, 20)?;
    assert_eq!(project.tracks[1].clips[0].start, 25);
    assert_eq!(project.tracks[1].clips[0].source, 3);
    let file = folder.path().join("group.cutad");
    save(&project, &file)?;
    let mut reopened = load(&file)?;
    assert_eq!(reopened, project);
    reopened.ungroup_clip(&anchor)?;
    assert!(
        reopened
            .tracks
            .iter()
            .flat_map(|track| &track.clips)
            .all(|clip| clip.group_id.is_none())
    );
    Ok(())
}
