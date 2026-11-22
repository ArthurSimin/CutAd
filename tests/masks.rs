use anyhow::Result;
use cutad::media::{
    Format, PreviewQuality, executable, export_frame_png, monitor_frame, probe, render,
    render_preview_frame, render_preview_frame_with,
};
use cutad::project::{Clip, Project, Track, TrackKind, load, save};
use serde_json::{Value, json};
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn set_mask(clip: &mut Clip, mask: Value) -> Result<()> {
    let mut data = serde_json::to_value(&*clip)?;
    data["mask"] = mask;
    *clip = serde_json::from_value(data)?;
    Ok(())
}

fn sample(pixels: &[u8], x: usize, y: usize) -> [u8; 3] {
    let start = (64 * y + x) * 3;
    pixels[start..start + 3].try_into().unwrap()
}

#[test]
fn shape_mask_preserves_source_alpha_and_matches_export() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let background = folder.path().join("background.ppm");
    let foreground = folder.path().join("foreground.png");
    let rgba = folder.path().join("foreground.rgba");
    let mut ppm = b"P6\n64 64\n255\n".to_vec();
    ppm.extend([0, 0, 255].repeat(64 * 64));
    std::fs::write(&background, ppm)?;
    let row = [[255, 0, 0, 0].repeat(32), [255, 0, 0, 255].repeat(32)].concat();
    std::fs::write(&rgba, row.repeat(64))?;
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-video_size",
            "64x64",
            "-pixel_format",
            "rgba",
            "-i",
        ])
        .arg(rgba)
        .args(["-frames:v", "1", "-threads", "1"])
        .arg(&foreground)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let assets = vec![probe(&background)?, probe(&foreground)?];
    let clip = |index: usize| Clip::new(assets[index].id.clone(), 0, 0, 8);
    let mut foreground_clip = clip(1);
    set_mask(
        &mut foreground_clip,
        json!({
            "shape": "ellipse", "x": 0.5, "y": 0.5, "width": 0.75, "height": 0.75,
            "rotation": 0, "feather": 0.25, "invert": false
        }),
    )?;
    let mut project = Project {
        width: 64,
        height: 64,
        tracks: vec![
            Track {
                clips: vec![clip(0)],
                ..Track::new("Base", TrackKind::Video)
            },
            Track {
                clips: vec![foreground_clip],
                ..Track::new("Masked", TrackKind::Video)
            },
        ],
        assets,
        ..Project::default()
    };
    let path = folder.path().join("project.cutad");
    save(&project, &path)?;
    assert_eq!(project, load(&path)?);
    let cancel = AtomicBool::new(false);
    let preview = render_preview_frame(&project, 5, &cancel)?;
    let export = folder.path().join("edit.mkv");
    render(&project, &export, Format::Mkv, &cancel, |_| {})?;
    let exported = monitor_frame(&export, 5, [64, 64])?;
    for pixels in [&preview, &exported] {
        let transparent = sample(pixels, 22, 32);
        let inside = sample(pixels, 40, 32);
        let feathered = sample(pixels, 55, 32);
        let outside = sample(pixels, 60, 3);
        assert!(
            transparent[2] > 220 && transparent[0] < 25,
            "{transparent:?}"
        );
        assert!(inside[0] > 220 && inside[2] < 25, "{inside:?}");
        assert!(feathered[0] > 75 && feathered[2] > 75, "{feathered:?}");
        assert!(outside[2] > 220 && outside[0] < 25, "{outside:?}");
    }
    for (mask, visible, hidden) in [
        (
            json!({"shape": "rectangle", "x": 0.68, "y": 0.5, "width": 0.22, "height": 0.7,
                "rotation": 90, "feather": 0, "invert": false}),
            (53, 32),
            (43, 46),
        ),
        (
            json!({"shape": "linear", "x": 0.5, "y": 0.5, "width": 0.6, "height": 0.6,
                "rotation": 0, "feather": 0, "invert": true}),
            (45, 16),
            (45, 48),
        ),
    ] {
        set_mask(&mut project.tracks[1].clips[0], mask)?;
        let pixels = render_preview_frame(&project, 5, &cancel)?;
        let visible = sample(&pixels, visible.0, visible.1);
        let hidden = sample(&pixels, hidden.0, hidden.1);
        assert!(
            visible[0] > 220 && hidden[2] > 220,
            "{visible:?} {hidden:?}"
        );
    }
    project.width = 672;
    project.height = 400;
    let still = folder.path().join("still.png");
    export_frame_png(&project, &still, 5, &cancel)?;
    let bytes = std::fs::read(&still)?;
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into()?), 672);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into()?), 400);
    assert_eq!(
        monitor_frame(&still, 0, [672, 400])?,
        render_preview_frame_with(&project, 5, PreviewQuality::Full, &cancel)?
    );
    assert!(export_frame_png(&project, &still, 5, &AtomicBool::new(true)).is_err());
    assert_eq!(std::fs::read(&still)?, bytes);
    assert!(export_frame_png(&project, &foreground, 5, &cancel).is_err());
    Ok(())
}
