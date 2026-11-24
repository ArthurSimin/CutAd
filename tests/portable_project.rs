use anyhow::Result;
use cutad::project::{self, Asset, Clip, Project};
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn portable_copy_reopens_after_moving_and_preserves_shared_media() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Source.mp4");
    let lut = root.path().join("Grade.cube");
    std::fs::write(&source, b"source video fixture")?;
    std::fs::write(&lut, b"color LUT fixture")?;
    let picture = Asset::video(&source, 30, false);
    let second = Asset::video(&source, 30, false);
    let mut clip = Clip::new(picture.id.clone(), 0, 0, 30);
    clip.color_lut = Some(lut.clone());
    let mut project = Project::default();
    project.tracks[0].clips.push(clip);
    project.assets.extend([picture, second]);
    let saved =
        project::save_portable_copy(&project, root.path(), &AtomicBool::new(false), |_| {})?;
    let moved = root.path().join("Moved copy");
    std::fs::rename(saved.parent().unwrap(), &moved)?;
    let reopened = project::load(&moved.join("Project.cutad"))?;
    let first = reopened.assets[0].path.as_deref().unwrap();
    assert_eq!(reopened.assets[1].path.as_deref(), Some(first));
    assert!(first.starts_with(&moved));
    assert_eq!(std::fs::read(first)?, std::fs::read(&source)?);
    let copied_lut = reopened.tracks[0].clips[0].color_lut.as_deref().unwrap();
    assert!(copied_lut.starts_with(&moved));
    assert_eq!(std::fs::read(copied_lut)?, std::fs::read(&lut)?);
    assert_eq!(project.assets[0].path.as_deref(), Some(source.as_path()));
    assert_eq!(std::fs::read_dir(moved.join("Media"))?.count(), 2);
    Ok(())
}

#[test]
fn canceled_copy_cleans_up_staged_files() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Source.mp4");
    std::fs::write(&source, vec![7; 2 * 1024 * 1024])?;
    let mut project = Project::default();
    project.assets.push(Asset::video(source, 30, false));
    let cancel = AtomicBool::new(false);
    assert!(
        project::save_portable_copy(&project, root.path(), &cancel, |_| cancel
            .store(true, Ordering::Relaxed))
        .is_err()
    );
    assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
    Ok(())
}
