use cutad::project::{Asset, Clip, Project, load, save, template};
use tempfile::tempdir;

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
