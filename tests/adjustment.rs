use anyhow::Result;
use cutad::media::{Format, monitor_frame, probe, render, render_preview_frame};
use cutad::project::{
    ChromaKey, Clip, ColorAdjustment, HueAdjustment, Project, Track, TrackKind, load, save,
};
use std::sync::atomic::AtomicBool;

#[test]
fn automatic_color_and_reference_tone_improve_dark_source() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let mut stills = Vec::new();
    for (name, tones) in [("source", [40, 110]), ("reference", [95, 175])] {
        let path = folder.path().join(format!("{name}.ppm"));
        let mut bytes = b"P6\n64 64\n255\n".to_vec();
        for _ in 0..64 {
            for tone in tones {
                bytes.extend([tone; 96]);
            }
        }
        std::fs::write(&path, bytes)?;
        stills.push(probe(&path)?);
    }
    let [source, reference] = stills.as_slice() else {
        unreachable!("created two media fixtures")
    };
    let cancel = AtomicBool::new(false);
    let automatic = cutad::media::suggest_color(source, 0, None, &cancel)?;
    assert!(automatic.brightness > 30);
    assert!(automatic.contrast > 20);
    let matched = cutad::media::suggest_color(source, 0, Some(reference), &cancel)?;
    let mut project = Project {
        width: 64,
        height: 64,
        assets: vec![source.clone()],
        ..Project::default()
    };
    let mut clip = Clip::new(source.id.clone(), 0, 0, 8);
    clip.adjustment = matched;
    project.tracks[0].clips.push(clip);
    let preview = render_preview_frame(&project, 0, &cancel)?;
    let path = folder.path().join("matched.mkv");
    render(&project, &path, Format::Mkv, &cancel, |_| {})?;
    let export = monitor_frame(&path, 0, [64, 64])?;
    for pixels in [preview, export] {
        let shadows = pixels[(32 * 64 + 10) * 3];
        let highlights = pixels[(32 * 64 + 50) * 3];
        assert!(shadows.abs_diff(95) < 20, "Shadow tone measured {shadows}");
        assert!(
            highlights.abs_diff(175) < 20,
            "Highlight tone measured {highlights}"
        );
    }
    Ok(())
}

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

#[test]
fn local_cube_lut_survives_save_and_matches_preview_and_export() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("red.ppm");
    let mut bytes = b"P6\n32 32\n255\n".to_vec();
    bytes.extend([200, 20, 15].repeat(32 * 32));
    std::fs::write(&file, bytes)?;
    let lut = temp.path().join("Swap channels (test).cube");
    let cube = "LUT_3D_SIZE 2\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n";
    std::fs::write(&lut, cube)?;
    let mut project = Project {
        width: 32,
        height: 32,
        ..Project::default()
    };
    let asset = probe(&file)?;
    let mut clip = Clip::new(asset.id.clone(), 0, 0, 15);
    clip.color_lut = Some(lut.clone());
    project.assets.push(asset);
    project.tracks[0].clips.push(clip);
    let saved = temp.path().join("LUT.cutad");
    save(&project, &saved)?;
    let project = load(&saved)?;
    let output = temp.path().join("Grade.mkv");
    let cancel = AtomicBool::new(false);
    render(&project, &output, Format::Mkv, &cancel, |_| {})?;
    for frame in [
        render_preview_frame(&project, 3, &cancel)?,
        monitor_frame(&output, 3, [32, 32])?,
    ] {
        assert!(
            frame[0] < 25 && frame[1] < 35 && frame[2] > 185,
            "{:?}",
            &frame[..3]
        );
    }
    assert!(render(&project, &lut, Format::Mkv, &cancel, |_| {}).is_err());
    assert_eq!(std::fs::read_to_string(lut)?, cube);
    Ok(())
}

#[test]
fn adjusting_red_hues_keeps_blue_in_preview_and_export() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("colors.ppm");
    let mut image = b"P6\n64 32\n255\n".to_vec();
    for _ in 0..32 {
        image.extend([250, 5, 5].repeat(32));
        image.extend([5, 5, 250].repeat(32));
    }
    std::fs::write(&file, image)?;
    let asset = probe(&file)?;
    let mut project = Project {
        width: 64,
        height: 32,
        ..Project::default()
    };
    let mut clip = Clip::new(asset.id.clone(), 0, 0, 15);
    clip.hue_adjustments[0] = HueAdjustment {
        hue: 120,
        ..HueAdjustment::default()
    };
    project.tracks[0].clips.push(clip);
    project.assets.push(asset);
    let saved = temp.path().join("Hue ranges.cutad");
    save(&project, &saved)?;
    let project = load(&saved)?;
    let output = temp.path().join("HSV.mkv");
    let cancel = AtomicBool::new(false);
    render(&project, &output, Format::Mkv, &cancel, |_| {})?;
    for pixels in [
        render_preview_frame(&project, 3, &cancel)?,
        monitor_frame(&output, 3, [64, 32])?,
    ] {
        let left = &pixels[(16 * 64 + 12) * 3..][..3];
        let right = &pixels[(16 * 64 + 50) * 3..][..3];
        assert!(left[0] < 40 && left[1] > 200, "{left:?}");
        assert!(
            right[0] < 40 && right[1] < 40 && right[2] > 200,
            "{right:?}"
        );
    }
    Ok(())
}
