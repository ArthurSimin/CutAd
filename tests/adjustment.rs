use anyhow::Result;
use cutad::media::{Format, monitor_frame, probe, render, render_preview_frame};
use cutad::project::{ChromaKey, Clip, ColorAdjustment, Project, Track, TrackKind, load, save};
use std::sync::atomic::AtomicBool;

#[test]
fn chroma_key_reveals_lower_track_and_color_adjustment_is_rendered() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut project = Project {
        width: 96,
        height: 64,
        ..Project::default()
    };
    for (name, left, right) in [
        ("backdrop", [20, 90, 210], [20, 90, 210]),
        ("subject", [0, 255, 0], [231, 60, 20]),
    ] {
        let file = temp.path().join(format!("{name}.ppm"));
        let mut bytes = b"P6\n96 64\n255\n".to_vec();
        for _ in 0..64 {
            bytes.extend(left.repeat(48));
            bytes.extend(right.repeat(48));
        }
        std::fs::write(&file, bytes)?;
        let asset = probe(&file)?;
        let clip = Clip::new(asset.id.clone(), 0, 0, 12);
        project.assets.push(asset);
        project.tracks.push(Track {
            clips: vec![clip],
            ..Track::new(name, TrackKind::Video)
        });
    }
    let foreground = &mut project.tracks.last_mut().unwrap().clips[0];
    foreground.chroma_key = Some(ChromaKey::default());
    foreground.adjustment = ColorAdjustment {
        saturation: -100,
        ..ColorAdjustment::default()
    };
    let id = foreground.id.clone();
    let second = project.split(&id, 8)?;
    let (row, index) = project.location(&second).unwrap();
    assert_eq!(
        project.tracks[row].clips[index].chroma_key,
        Some(ChromaKey::default())
    );
    assert_eq!(project.tracks[row].clips[index].adjustment.saturation, -100);
    let path = temp.path().join("project.cutad");
    save(&project, &path)?;
    assert_eq!(project, load(&path)?);
    let cancel = AtomicBool::new(false);
    let finished = temp.path().join("finished.mp4");
    render(&project, &finished, Format::Mp4, &cancel, |_| {})?;
    let preview = render_preview_frame(&project, 5, &cancel)?;
    let exported = monitor_frame(&finished, 5, [96, 64])?;
    for pixels in [&preview, &exported] {
        let left = &pixels[(30 * 96 + 10) * 3..][..3];
        let right = &pixels[(30 * 96 + 70) * 3..][..3];
        assert!(left[2] > 170 && left[0] < 45, "{left:?}");
        assert!(right.iter().all(|v| v.abs_diff(right[0]) < 8), "{right:?}");
        assert!(right[0] > 60, "{right:?}");
    }
    Ok(())
}
