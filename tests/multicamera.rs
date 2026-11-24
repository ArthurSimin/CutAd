use anyhow::Result;
use cutad::media::{self, Format, executable};
use cutad::project::{Clip, Project, Track, TrackKind};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn camera(path: &Path, color: &str) -> Result<cutad::project::Asset> {
    let output = Command::new(executable("ffmpeg")?)
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color={color}:s=64x64:r=30:d=2"))
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=523:sample_rate=48000:d=2",
            "-c:v",
            "ffv1",
            "-c:a",
            "flac",
            "-threads",
            "1",
        ])
        .arg(path)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    media::probe(path)
}

#[test]
fn camera_switches_preserve_trims_and_keep_reference_audio() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let a = camera(&scratch.path().join("red.mkv"), "red")?;
    let b = camera(&scratch.path().join("blue.mkv"), "blue")?;
    let reference = Clip::new(a.id.clone(), 0, 3, 48);
    let second = Clip::new(b.id.clone(), 8, 6, 24);
    let later = Clip::new(b.id.clone(), 54, 0, 4);
    let project = Project {
        width: 64,
        height: 64,
        tracks: vec![
            Track {
                clips: vec![reference.clone()],
                ..Track::new("Cam A", TrackKind::Video)
            },
            Track {
                clips: vec![second.clone(), later.clone()],
                ..Track::new("Cam B", TrackKind::Video)
            },
        ],
        assets: vec![a, b],
        ..Project::default()
    };
    let mut edit = project.clone();
    let angles = [reference.clone(), second.clone()];
    assert!(
        edit.assemble_camera_angles(&angles, &BTreeMap::from([(1, 1)]), true)
            .is_err()
    );
    assert_eq!(project, edit);
    let id = edit.assemble_camera_angles(&angles, &BTreeMap::from([(12, 1)]), true)?;
    edit.validate()?;
    let (row, _) = edit.location(&id).unwrap();
    let tracks = &edit.tracks;
    let intervals: Vec<_> = tracks[row]
        .clips
        .iter()
        .map(|clip| {
            (
                clip.start,
                clip.end(),
                clip.source,
                clip.asset_id.as_str(),
                clip.gain,
            )
        })
        .collect();
    assert_eq!(
        intervals,
        [
            (0, 12, 3, reference.asset_id.as_str(), 0.0),
            (12, 32, 10, second.asset_id.as_str(), 0.0),
            (32, 48, 35, reference.asset_id.as_str(), 0.0),
        ]
    );
    assert!(tracks[0].muted);
    assert!(tracks[0].archived);
    assert_eq!(tracks[0].clips[0], reference);
    assert_eq!(tracks[1].clips, [later]);
    assert!(tracks.last().unwrap().muted);
    assert_eq!(tracks.last().unwrap().clips[0], second);
    assert_eq!(tracks[row + 1].clips[0].asset_id, reference.asset_id);
    let path = scratch.path().join("edit.cutad");
    cutad::project::save(&edit, &path)?;
    let edit = cutad::project::load(&path)?;
    let path = scratch.path().join("result.mkv");
    media::render(&edit, &path, Format::Mkv, &AtomicBool::new(false), |_| {})?;
    for (tick, color) in [(4, 0), (18, 2), (35, 0)] {
        let pixels = media::monitor_frame(&path, tick, [64, 64])?;
        assert!(pixels[color] > 230, "wrong camera at frame {tick}");
    }
    assert!(media::probe(&path)?.audio);
    Ok(())
}

#[test]
fn source_sync_points_survive_splits_and_archive_tails_do_not_extend_delivery() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let a = camera(&scratch.path().join("a.mkv"), "red")?;
    let b = camera(&scratch.path().join("b.mkv"), "blue")?;
    let mut reference = Clip::new(a.id.clone(), 5, 0, 35);
    let mut second = Clip::new(b.id.clone(), 10, 2, 50);
    reference.sync_source = Some(1200);
    second.sync_source = Some(1000);
    let mut project = Project {
        assets: vec![a, b],
        tracks: vec![
            Track {
                clips: vec![reference.clone()],
                ..Track::new("A", TrackKind::Video)
            },
            Track {
                clips: vec![second.clone()],
                ..Track::new("B", TrackKind::Video)
            },
        ],
        ..Project::default()
    };
    project.align_clip_markers(&[&reference.id, &second.id])?;
    assert_eq!(project.tracks[1].clips[0].start, 9);
    let mut split = project.clone();
    let following = split.split(&second.id, 13)?;
    let (row, index) = split.location(&following).unwrap();
    assert_eq!(split.tracks[row].clips[index].sync_offset(), Some(4));
    assert_eq!(split.tracks[row].clips[0].sync_offset(), None);
    let angles = [reference, project.tracks[1].clips[0].clone()];
    project.assemble_camera_angles(&angles, &BTreeMap::from([(14, 1)]), true)?;
    assert_eq!(project.duration(), 40);
    project.tracks[1].muted = false;
    assert_eq!(project.duration(), 59);
    Ok(())
}
