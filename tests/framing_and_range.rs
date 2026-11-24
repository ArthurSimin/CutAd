use anyhow::Result;
use cutad::media::{
    ExportDetails, Format, Output, PreviewQuality, detect_scene_cuts, executable, monitor_frame,
    probe, render_preview_frame_with, render_timeline_section, render_with_details,
};
use cutad::project::{Asset, AssetKind, Clip, Framing, Project, load, save};
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[test]
fn mp4_cover_and_title_keep_the_export_range_and_primary_video() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let mut project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    for (index, (name, color)) in [("red", [240, 20, 10]), ("blue", [12, 35, 245])]
        .into_iter()
        .enumerate()
    {
        let path = folder.path().join(format!("{name}.ppm"));
        let mut picture = b"P6\n64 64\n255\n".to_vec();
        picture.extend(color.repeat(64 * 64));
        std::fs::write(&path, picture)?;
        let asset = probe(&path)?;
        project.tracks[0]
            .clips
            .push(Clip::new(asset.id.clone(), index as u32 * 30, 0, 30));
        project.assets.push(asset);
    }
    let title = "Local delivery — café";
    let destination = folder.path().join("delivery.mp4");
    render_with_details(
        &project,
        &destination,
        Output::new(Format::Mp4),
        10..30,
        &ExportDetails {
            title: title.into(),
            cover_frame: Some(40),
        },
        &AtomicBool::new(false),
        |_| {},
    )?;
    let result = Command::new(executable("ffprobe")?)
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&destination)
        .output()?;
    assert!(result.status.success());
    let metadata: serde_json::Value = serde_json::from_slice(&result.stdout)?;
    assert_eq!(metadata["format"]["tags"]["title"], title);
    let streams = metadata["streams"].as_array().unwrap();
    assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
    let cover = streams
        .iter()
        .find(|stream| stream["disposition"]["attached_pic"] == 1)
        .unwrap();
    assert_eq!(cover["codec_name"], "png");
    assert_eq!(probe(&destination)?.frames, 20);
    let actual = monitor_frame(&destination, 0, [64, 64])?;
    assert!(actual[0] > 200 && actual[2] < 40);
    let thumbnail = Command::new(executable("ffmpeg")?)
        .args(["-v", "error", "-i"])
        .arg(destination)
        .args([
            "-map",
            "0:v:1",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()?;
    assert!(thumbnail.status.success());
    assert!(thumbnail.stdout[0] < 45 && thumbnail.stdout[2] > 200);
    Ok(())
}

#[test]
fn framing_persists_and_the_delivery_range_matches_the_timeline() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let file = folder.path().join("stripes.ppm");
    let mut image = b"P6\n64 64\n255\n".to_vec();
    for _ in 0..64 {
        image.extend([240, 4, 7].repeat(32));
        image.extend([12, 219, 35].repeat(32));
    }
    std::fs::write(&file, image)?;
    let asset = probe(&file)?;
    let mut project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    let mut clip = Clip::new(asset.id.clone(), 15, 0, 30);
    clip.framing = Framing {
        left: 50,
        flip_horizontal: true,
        ..Framing::default()
    };
    project.tracks[0].clips.push(clip);
    project.assets.push(asset);
    let path = folder.path().join("stripes.cutad");
    save(&project, &path)?;
    let project = load(&path)?;
    let cancel = AtomicBool::new(false);
    let expected = render_preview_frame_with(&project, 20, PreviewQuality::Full, &cancel)?;
    let center = (32 * 64 + 32) * 3;
    assert!(expected[center] > 220 && expected[center + 1] < 25);
    assert!(expected[(32 * 64 + 8) * 3] < 50);
    let export = folder.path().join("section.mkv");
    render_timeline_section(
        &project,
        &export,
        Output::new(Format::Mkv),
        20..29,
        &cancel,
        |_| {},
    )?;
    assert_eq!(probe(&export)?.frames, 9);
    assert_eq!(monitor_frame(&export, 0, [64, 64])?, expected);
    assert!(
        render_timeline_section(
            &project,
            &export,
            Output::new(Format::Mkv),
            20..46,
            &cancel,
            |_| {},
        )
        .is_err()
    );
    assert_eq!(probe(&export)?.frames, 9);
    Ok(())
}

#[test]
fn freeze_inserts_a_hold_and_resumes_at_the_same_source_frame() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join("pattern.mkv");
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=64x64:r=30:d=2",
            "-c:v",
            "ffv1",
            "-threads",
            "1",
        ])
        .arg(&path)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asset = probe(&path)?;
    let clip = Clip::new(asset.id.clone(), 0, 8, 32);
    let next = Clip::new(asset.id.clone(), 40, 0, 12);
    let id = clip.id.clone();
    let next_id = next.id.clone();
    let mut project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    project.assets.push(asset);
    project.tracks[0].clips.extend([clip, next]);
    let cancel = AtomicBool::new(false);
    let preview = |project: &Project, frame| {
        render_preview_frame_with(project, frame, PreviewQuality::Full, &cancel)
    };
    let frame = preview(&project, 12)?;
    let following = preview(&project, 13)?;
    assert_ne!(frame, following);
    let hold = project.insert_freeze(&id, 12, 20)?;
    let (row, index) = project.location(&hold).unwrap();
    assert_eq!(project.tracks[row].clips[index].source, 20);
    let (row, index) = project.location(&next_id).unwrap();
    assert_eq!(project.tracks[row].clips[index].start, 60);
    for time in [12, 24, 31, 32] {
        assert_eq!(frame, preview(&project, time)?);
    }
    assert_eq!(following, preview(&project, 33)?);
    Ok(())
}

#[test]
fn scene_detection_returns_cuts_relative_to_the_trimmed_source() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join("hard-cut.mkv");
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=black:s=64x64:r=30:d=1",
            "-f",
            "lavfi",
            "-i",
            "color=white:s=64x64:r=30:d=1",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0",
            "-c:v",
            "ffv1",
            "-threads",
            "1",
        ])
        .arg(&path)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cancel = AtomicBool::new(false);
    assert_eq!(detect_scene_cuts(&path, 0, 60, &cancel, |_| {})?, [30]);
    assert_eq!(detect_scene_cuts(&path, 12, 30, &cancel, |_| {})?, [18]);
    assert!(detect_scene_cuts(&path, 35, 20, &cancel, |_| {})?.is_empty());
    assert!(detect_scene_cuts(&path, 0, 60, &AtomicBool::new(true), |_| {}).is_err());
    Ok(())
}

#[test]
fn gif_exports_only_picture_from_the_requested_timeline_range() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let mut project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    for (index, color) in [[220, 15, 20], [14, 25, 220]].into_iter().enumerate() {
        let path = folder.path().join(format!("scene{index}.ppm"));
        let mut pixels = b"P6\n64 64\n255\n".to_vec();
        pixels.extend(color.repeat(64 * 64));
        std::fs::write(&path, pixels)?;
        let asset = probe(&path)?;
        project.tracks[0]
            .clips
            .push(Clip::new(asset.id.clone(), index as u32 * 30, 0, 30));
        project.assets.push(asset);
    }
    let audio = Asset::local(
        folder.path().join("missing.wav"),
        AssetKind::Audio,
        60,
        true,
    );
    project.tracks[1]
        .clips
        .push(Clip::new(audio.id.clone(), 0, 0, 60));
    project.assets.push(audio);
    let output = folder.path().join("animated.gif");
    render_timeline_section(
        &project,
        &output,
        Output::new(Format::Gif),
        15..45,
        &AtomicBool::new(false),
        |_| {},
    )?;
    let metadata = Command::new(executable("ffprobe")?)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()?;
    assert!(metadata.status.success());
    let value: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let streams = value["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0]["codec_name"], "gif");
    assert_eq!(streams[0]["nb_read_frames"], "15");
    let data = std::fs::read(&output)?;
    assert!(
        data.windows(16)
            .any(|bytes| bytes == b"NETSCAPE2.0\x03\x01\0\0\0")
    );
    let first = monitor_frame(&output, 0, [64, 64])?;
    let last = monitor_frame(&output, 24, [64, 64])?;
    assert!(first[0] > 190 && first[2] < 40 && last[0] < 40 && last[2] > 190);

    let asset = probe(&output)?;
    let mut sticker_project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    let sticker = sticker_project.add_sticker(asset, 3)?;
    let (row, index) = sticker_project.location(&sticker).unwrap();
    assert_eq!(sticker_project.tracks[row].clips[index].length, 30);
    let cancel = AtomicBool::new(false);
    let early = render_preview_frame_with(&sticker_project, 4, PreviewQuality::Full, &cancel)?;
    let late = render_preview_frame_with(&sticker_project, 27, PreviewQuality::Full, &cancel)?;
    let center = (32 * 64 + 32) * 3;
    assert!(early[center] > 190 && late[center + 2] > 190);
    let still = folder.path().join("one-frame.gif");
    render_timeline_section(
        &project,
        &still,
        Output {
            fps: 12,
            ..Output::new(Format::Gif)
        },
        15..16,
        &cancel,
        |_| {},
    )?;
    assert!(monitor_frame(&still, 0, [64, 64])?[0] > 190);
    Ok(())
}
