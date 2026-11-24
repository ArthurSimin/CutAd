use anyhow::{Context, Result, ensure};
use cutad::media::{
    PreviewQuality, executable, export_frame_png, probe, reframe_subject, remove_background,
    render_preview_frame_with,
};
use cutad::project::{AssetKind, Clip, Project};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn decode_frame(path: &Path) -> Result<Vec<u8>> {
    let output = Command::new(executable("ffmpeg")?)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-threads:v",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn decode_audio(path: &Path) -> Result<Vec<u8>> {
    let output = Command::new(executable("ffmpeg")?)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "s16le", "-",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn model_fixture() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let variable = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .with_context(|| format!("Set {name}."))
    };
    Ok((
        variable("CUTAD_CUTOUT_PYTHON")?,
        variable("CUTAD_TEST_ONNX")?,
        variable("CUTAD_TEST_CUTOUT_PHOTO")?,
    ))
}

#[test]
#[ignore = "requires a local rembg Python environment, ONNX weights, and openly licensed photo"]
fn automatic_cutout_preserves_audio_alpha_and_timeline_compositing() -> Result<()> {
    let (python, model, photo) = model_fixture()?;
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("source.mkv");
    let output = Command::new(executable("ffmpeg")?)
        .args(["-v", "error", "-loop", "1", "-framerate", "30", "-i"])
        .arg(&photo)
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-vf",
            "scale=128:128",
            "-t",
            "0.2",
            "-threads:v",
            "1",
            "-c:v",
            "ffv1",
            "-c:a",
            "flac",
        ])
        .arg(&source)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asset = probe(&source)?;
    let clip = Clip::new(asset.id.clone(), 2, 1, 4);
    let cancel = AtomicBool::new(false);
    let cutout = folder.path().join("cutout.mkv");
    let (prepared, subframe) =
        remove_background(&clip, &asset, &cutout, &python, &model, &cancel, |_| {})?;
    assert_eq!(prepared.frames, 4);
    assert_eq!(subframe, 0);
    assert!(prepared.audio);
    let source_audio = decode_audio(&source)?;
    assert_eq!(decode_audio(&cutout)?, &source_audio[3_200..16_000]);
    let rgba = decode_frame(&cutout)?;
    assert_eq!(rgba.len(), 128 * 128 * 4);
    assert!(rgba.chunks_exact(4).filter(|pixel| pixel[3] < 16).count() > 800);
    assert!(rgba.chunks_exact(4).filter(|pixel| pixel[3] > 239).count() > 800);

    let mut project = Project {
        width: 128,
        height: 128,
        assets: vec![asset],
        ..Project::default()
    };
    project.tracks[0].clips.push(clip.clone());
    project.replace_media(&clip.id, prepared)?;
    let exported = folder.path().join("composited.png");
    export_frame_png(&project, &exported, 2, &cancel)?;
    let pixels = decode_frame(&exported)?;
    let empty = folder.path().join("empty.png");
    export_frame_png(&project, &empty, 0, &cancel)?;
    let background = decode_frame(&empty)?;
    let difference = |a: &[u8], b: &[u8]| {
        a[..3]
            .iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or_default()
    };
    let samples = || {
        rgba.chunks_exact(4)
            .zip(pixels.chunks_exact(4))
            .zip(background.chunks_exact(4))
    };
    let transparent = samples()
        .filter(|((cut, result), base)| cut[3] < 5 && difference(result, base) <= 5)
        .count();
    let foreground = samples()
        .filter(|((cut, result), base)| cut[3] > 239 && difference(result, base) > 24)
        .count();
    assert!(
        transparent > 500,
        "Only {transparent} pixels reveal the canvas."
    );
    assert!(
        foreground > 500,
        "Only {foreground} pixels show foreground detail."
    );

    let still = probe(&photo)?;
    let image_clip = Clip::new(still.id.clone(), 2, 0, 60);
    let image = folder.path().join("cutout.png");
    let (prepared, _) = remove_background(
        &image_clip,
        &still,
        &image,
        &python,
        &model,
        &cancel,
        |_| {},
    )?;
    assert_eq!(prepared.kind, AssetKind::Image);
    let pixels = decode_frame(&image)?;
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));

    let canceled = folder.path().join("canceled.png");
    assert!(
        remove_background(
            &image_clip,
            &still,
            &canceled,
            &python,
            &model,
            &AtomicBool::new(true),
            |_| {},
        )
        .is_err()
    );
    assert!(!canceled.exists());
    Ok(())
}

#[test]
#[ignore = "requires a local rembg Python environment, ONNX weights, and openly licensed photo"]
fn automatic_reframe_follows_a_subject_across_a_wide_clip() -> Result<()> {
    let (python, model, photo) = model_fixture()?;
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("moving subject.mkv");
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=0x555555:s=256x128:r=30",
        ])
        .args(["-loop", "1", "-framerate", "30", "-i"])
        .arg(photo)
        .args([
            "-filter_complex_threads",
            "1",
            "-filter_complex",
            "[1:v]scale=128:128[figure];[0:v][figure]overlay=x='n*12':y=0",
            "-frames:v",
            "9",
            "-threads:v",
            "1",
            "-c:v",
            "ffv1",
        ])
        .arg(&source)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asset = probe(&source)?;
    let mut clip = Clip::new(asset.id.clone(), 0, 1, 7);
    let cancel = AtomicBool::new(false);
    let (scale, position) = reframe_subject(&clip, &asset, [128, 128], &python, &model, &cancel)?;
    assert!((scale - 2.0).abs() < 0.01);
    assert_eq!(position.keys.len(), 7);
    clip.transform.scale = scale;
    clip.position_animation = position;
    let start = clip.position_at(0);
    let end = clip.position_at(6);
    assert!(start[0] - end[0] > 0.22, "{start:?} → {end:?}");
    assert!(start[1].abs() < 0.01 && end[1].abs() < 0.01);
    let mut project = Project {
        width: 128,
        height: 128,
        assets: vec![asset],
        ..Project::default()
    };
    project.tracks[0].clips.push(clip);
    for frame in [0, 6] {
        let pixels = render_preview_frame_with(&project, frame, PreviewQuality::Full, &cancel)?;
        let active = pixels
            .chunks_exact(3)
            .filter(|pixel| pixel[0] > 160 && pixel[1] < 120)
            .count();
        assert!(
            active > 300,
            "Only {active} suit-colored pixels remain at frame {frame}"
        );
    }
    Ok(())
}
