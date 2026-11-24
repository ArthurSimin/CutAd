use anyhow::Result;
use cutad::media::{
    Format, PreviewQuality, executable, monitor_frame, probe, render, render_preview_frame_with,
};
use cutad::project::{Clip, Project, load, save};
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[test]
fn slow_motion_keeps_its_source_phase_through_split_and_export() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("motion.mkv");
    let ffmpeg = executable("ffmpeg")?;
    let fixture = Command::new(&ffmpeg)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=64x64:r=30:d=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=4",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "-threads",
            "1",
        ])
        .arg(&source)
        .output()?;
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let asset = probe(&source)?;
    let mut project = Project {
        width: 64,
        height: 64,
        ..Project::default()
    };
    let first = Clip::new(asset.id.clone(), 0, 10, 45);
    let id = first.id.clone();
    project.tracks[0].clips.push(first);
    let next = Clip::new(asset.id.clone(), 50, 90, 6);
    let next_id = next.id.clone();
    project.tracks[0].clips.push(next);
    project.assets.push(asset);
    project.set_clip_speed(&id, 200)?;
    project.set_clip_speed(&id, 100)?;
    assert_eq!(project.tracks[0].clips[0].length, 45);
    project.set_clip_speed(&id, 75)?;
    assert_eq!(project.tracks[0].clips[0].length, 60);
    let (row, index) = project.location(&next_id).unwrap();
    assert_eq!(project.tracks[row].clips[index].start, 65);

    let cancel = AtomicBool::new(false);
    let export = folder.path().join("retimed.mkv");
    render(&project, &export, Format::Mkv, &cancel, |_| {})?;
    assert_eq!(probe(&export)?.frames, 71);
    let audio = Command::new(&ffmpeg)
        .args(["-nostdin", "-v", "error", "-ss", "0.5", "-i"])
        .arg(&export)
        .args(["-t", "0.2", "-vn", "-ac", "1", "-f", "s16le", "-"])
        .output()?;
    assert!(
        audio.status.success(),
        "{}",
        String::from_utf8_lossy(&audio.stderr)
    );
    let samples = audio
        .stdout
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let rising_crossings = samples
        .windows(2)
        .filter(|pair| pair[0] <= 0 && pair[1] > 0)
        .count();
    assert!(
        (195..=205).contains(&rising_crossings),
        "{rising_crossings}"
    );

    let tail = project.split(&id, 21)?;
    let (row, index) = project.location(&tail).unwrap();
    assert_eq!(project.tracks[row].clips[index].source, 25);
    assert_eq!(project.tracks[row].clips[index].source_subframe, 75);
    let path = folder.path().join("retimed.cutad");
    save(&project, &path)?;
    let project = load(&path)?;
    for time in [20, 21, 22, 39, 59] {
        let expected = monitor_frame(&export, time, [64, 64])?;
        let actual = render_preview_frame_with(&project, time, PreviewQuality::Full, &cancel)?;
        assert!(expected == actual, "video mismatch at timeline tick {time}");
    }
    Ok(())
}

#[test]
fn pitch_shift_changes_frequency_without_changing_delivery_duration() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let source = folder.path().join("tone.wav");
    let ffmpeg = executable("ffmpeg")?;
    let fixture = Command::new(&ffmpeg)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
        ])
        .arg(&source)
        .output()?;
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let asset = probe(&source)?;
    let mut clip = Clip::new(asset.id.clone(), 0, 10, 45);
    clip.pitch_semitones = 12;
    let mut project = Project {
        width: 32,
        height: 32,
        ..Project::default()
    };
    project.assets.push(asset);
    project.tracks[1].clips.push(clip);
    let output = folder.path().join("pitch.mkv");
    render(
        &project,
        &output,
        Format::Mkv,
        &AtomicBool::new(false),
        |_| {},
    )?;
    assert_eq!(probe(&output)?.frames, 45);
    let audio = Command::new(&ffmpeg)
        .args(["-nostdin", "-v", "error", "-ss", "0.5", "-i"])
        .arg(output)
        .args(["-t", "0.25", "-vn", "-ac", "1", "-f", "s16le", "-"])
        .output()?;
    assert!(
        audio.status.success(),
        "{}",
        String::from_utf8_lossy(&audio.stderr)
    );
    let samples = audio
        .stdout
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let rising_crossings = samples
        .windows(2)
        .filter(|pair| pair[0] <= 0 && pair[1] > 0)
        .count();
    assert!(
        (217..=223).contains(&rising_crossings),
        "{rising_crossings}"
    );
    Ok(())
}
