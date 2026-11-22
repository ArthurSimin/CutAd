use anyhow::Result;
use cutad::effects::{Transition, TransitionKind};
use cutad::media::{
    Format, Output, PreviewQuality, monitor_frame, probe, render_preview_frame_with, render_with,
};
use cutad::project::{Clip, Project, Track, TrackKind, load, save};
use std::sync::atomic::AtomicBool;

#[test]
fn visual_fades_reveal_the_lower_track_and_survive_scrubbing_and_splits() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let mut project = Project {
        width: 32,
        height: 32,
        tracks: vec![
            Track::new("Background", TrackKind::Video),
            Track::new("Overlay", TrackKind::Video),
        ],
        ..Project::default()
    };
    for (track, (name, color)) in [("blue", [0, 0, 240]), ("red", [240, 0, 0])]
        .into_iter()
        .enumerate()
    {
        let path = folder.path().join(format!("{name}.ppm"));
        let mut ppm = b"P6\n32 32\n255\n".to_vec();
        ppm.extend(color.repeat(32 * 32));
        std::fs::write(&path, ppm)?;
        let asset = probe(&path)?;
        let (start, length) = if track == 0 { (0, 60) } else { (6, 48) };
        project.tracks[track]
            .clips
            .push(Clip::new(asset.id.clone(), start, 0, length));
        project.assets.push(asset);
    }
    let mut document = serde_json::to_value(&project)?;
    document["tracks"][1]["clips"][0]["visual_fade"] =
        serde_json::json!({"in_frames": 16, "out_frames": 16});
    let mut project: Project = serde_json::from_value(document)?;
    let cancel = AtomicBool::new(false);
    let export = folder.path().join("fades.mkv");
    render_with(
        &project,
        &export,
        Output {
            fps: 60,
            ..Output::new(Format::Mkv)
        },
        &cancel,
        |_| {},
    )?;
    let preview = |project: &Project, frame| {
        render_preview_frame_with(project, frame, PreviewQuality::Full, &cancel)
    };
    let pixel = |data: &[u8]| {
        let center = (16 * 32 + 16) * 3;
        [data[center], data[center + 2]]
    };
    let expected = [
        (6, [0, 240]),
        (14, [120, 120]),
        (30, [240, 0]),
        (46, [120, 120]),
        (53, [15, 225]),
    ];
    for (frame, expected) in expected {
        let scrub = pixel(&preview(&project, frame)?);
        let exported = pixel(&monitor_frame(&export, frame, [32, 32])?);
        for (actual, reference) in [scrub, exported].into_iter().zip([expected, scrub]) {
            assert!(
                actual
                    .iter()
                    .zip(reference)
                    .all(|(a, b)| a.abs_diff(b) <= 6),
                "frame {frame}: expected {reference:?}, got {actual:?}"
            );
        }
    }
    let id = project.tracks[1].clips[0].id.clone();
    let tail = project.split(&id, 12)?;
    project.split(&tail, 45)?;
    let path = folder.path().join("fades.cutad");
    save(&project, &path)?;
    let split = load(&path)?;
    for (frame, expected) in expected {
        let actual = pixel(&preview(&split, frame)?);
        assert!(
            actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 6),
            "split frame {frame}: expected {expected:?}, got {actual:?}"
        );
    }
    Ok(())
}

#[test]
fn incoming_fade_remains_transparent_during_a_cut_transition() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let mut project = Project {
        width: 32,
        height: 32,
        ..Project::default()
    };
    for (index, color) in [[240, 0, 0], [0, 0, 240]].into_iter().enumerate() {
        let path = folder.path().join(format!("clip{index}.ppm"));
        let mut ppm = b"P6\n32 32\n255\n".to_vec();
        ppm.extend(color.repeat(32 * 32));
        std::fs::write(&path, ppm)?;
        let asset = probe(&path)?;
        let mut clip = Clip::new(asset.id.clone(), index as u32 * 30, 0, 30);
        if index == 1 {
            clip.set_visual_fade(30, 0);
            clip.transition = Some(Transition {
                kind: TransitionKind::Dissolve,
                frames: 12,
            });
        }
        project.tracks[0].clips.push(clip);
        project.assets.push(asset);
    }
    let cancel = AtomicBool::new(false);
    let export = folder.path().join("transition.mkv");
    render_with(&project, &export, Output::new(Format::Mkv), &cancel, |_| {})?;
    let preview = render_preview_frame_with(&project, 36, PreviewQuality::Full, &cancel)?;
    let full = monitor_frame(&export, 36, [32, 32])?;
    for pixels in [&preview, &full] {
        let [red, _, blue] = pixels[..3] else {
            unreachable!()
        };
        assert!(
            red > 65 && blue < 70,
            "the incoming clip should still be mostly transparent, red={red}, blue={blue}"
        );
    }
    Ok(())
}
