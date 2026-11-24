use anyhow::Result;
use cutad::{
    media::{self, PreviewQuality},
    project::{self, Clip, Project},
};
use std::{process::Command, sync::atomic::AtomicBool};

#[test]
fn preview_proxy_is_used_at_reduced_resolution_and_delivery_keeps_original() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("Pattern.mp4");
    let generated = Command::new(media::executable("ffmpeg")?)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1200x720:r=60:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=523:sample_rate=48000:duration=1",
            "-c:v",
            "libx264",
            "-threads:v",
            "2",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .output()?;
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let mut project = Project {
        width: 480,
        height: 288,
        ..Project::default()
    };
    let mut asset = media::probe(&source)?;
    let cancel = AtomicBool::new(false);
    let path = root.path().join("Proxy.mp4");
    let proxy = media::create_preview_proxy(&asset, &path, &cancel, |_| {})?;
    let proxy_asset = media::probe(&path)?;
    assert!(asset.audio && !proxy_asset.audio);
    let geometry = proxy_asset.geometry.unwrap();
    assert!(geometry.width <= 960 && geometry.height <= 540);
    assert_eq!(geometry.fps, 60);
    assert_eq!(asset.frames, proxy_asset.frames);

    project.tracks[0]
        .clips
        .push(Clip::new(asset.id.clone(), 0, 2, 24));
    project.assets.push(asset.clone());
    let inspect = |project: &Project, quality| {
        media::render_preview_frame_with(project, 14, quality, &cancel)
    };
    let original = inspect(&project, PreviewQuality::Half)?;
    let full = inspect(&project, PreviewQuality::Full)?;
    let delivered = root.path().join("original.png");
    media::export_frame_png(&project, &delivered, 14, &cancel)?;
    let preview_audio = |project: &Project, label: &str| -> Result<Vec<u8>> {
        let preview = root.path().join(label);
        media::render_preview_with(project, &preview, 4..20, PreviewQuality::Half, &cancel)?;
        let audio = Command::new(media::executable("ffmpeg")?)
            .args(["-v", "error", "-i"])
            .arg(preview)
            .args(["-map", "0:a", "-f", "s16le", "pipe:1"])
            .output()?;
        assert!(
            audio.status.success(),
            "{}",
            String::from_utf8_lossy(&audio.stderr)
        );
        Ok(audio.stdout)
    };
    let source_audio = preview_audio(&project, "original.mp4")?;
    assert!(source_audio.iter().any(|sample| *sample != 0));

    let mut proxy_only = project.clone();
    proxy_only.assets[0].path = Some(path.clone());
    proxy_only.assets[0].audio = false;
    let expected = inspect(&proxy_only, PreviewQuality::Half)?;
    assert_ne!(expected, original);
    asset.proxy = Some(proxy);
    project.assets[0] = asset;
    let file = root.path().join("With proxy.cutad");
    project::save(&project, &file)?;
    let project = project::load(&file)?;
    assert_eq!(inspect(&project, PreviewQuality::Half)?, expected);
    assert_eq!(inspect(&project, PreviewQuality::Full)?, full);
    assert_eq!(preview_audio(&project, "linked.mp4")?, source_audio);
    let delivered_with_proxy = root.path().join("linked.png");
    media::export_frame_png(&project, &delivered_with_proxy, 14, &cancel)?;
    assert_eq!(
        std::fs::read(&delivered)?,
        std::fs::read(delivered_with_proxy)?
    );
    std::fs::rename(path, root.path().join("Moved.mp4"))?;
    assert_eq!(inspect(&project, PreviewQuality::Half)?, original);
    Ok(())
}
