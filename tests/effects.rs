use anyhow::{Result, ensure};
use cutad::effects::{Effect, Preset};
use cutad::media::{Format, executable, monitor_frame, probe, render};
use cutad::project::{Asset, Clip, Project, Track, TrackKind, load, save};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[test]
fn effect_stacks_survive_split_and_project_reload() -> Result<()> {
    let dir = tempdir()?;
    let mut project = Project::default();
    let media = Asset::video(dir.path().join("source.mov"), 120, true);
    let mut clip = Clip::new(media.id.clone(), 0, 0, 120);
    let id = clip.id.clone();
    clip.effects.push(Effect::new(Preset::DeepMono));
    project.assets.push(media);
    project.tracks[0].clips.push(clip);
    project.split(&id, 60)?;
    assert_eq!(
        project.tracks[0].clips[1].effects,
        project.tracks[0].clips[0].effects
    );
    let file = dir.path().join("effects.cutad");
    save(&project, &file)?;
    assert_eq!(load(&file)?, project);
    Ok(())
}

#[test]
fn every_preset_runs_through_the_shared_renderer() -> Result<()> {
    let dir = tempdir()?;
    let source = dir.path().join("preset source.mp4");
    let output = Command::new(executable("ffmpeg")?)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=96x64:r=30:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=duration=1:sample_rate=48000",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asset = probe(&source)?;
    let mut baseline = None;
    for preset in std::iter::once(None).chain(Preset::ALL.iter().copied().map(Some)) {
        let mut clip = Clip::new(asset.id.clone(), 0, 0, 18);
        if let Some(preset) = preset {
            clip.effects.push(Effect::new(preset));
        }
        let project = Project {
            width: 96,
            height: 64,
            assets: vec![asset.clone()],
            tracks: vec![Track {
                clips: vec![clip],
                ..Track::new("Clip", TrackKind::Video)
            }],
            ..Project::default()
        };
        let path = dir.path().join("preset.mp4");
        render(
            &project,
            &path,
            Format::Preview,
            &AtomicBool::new(false),
            |_| {},
        )
        .map_err(|error| anyhow::anyhow!("{preset:?}: {error:#}"))?;
        let frames = [3, 9, 15]
            .into_iter()
            .map(|frame| monitor_frame(&path, frame, [96, 64]))
            .collect::<Result<Vec<_>>>()?;
        if let Some(preset) = preset {
            if !preset.audio() {
                assert_ne!(
                    Some(&frames),
                    baseline.as_ref(),
                    "{preset:?} must change the image"
                );
            }
        } else {
            baseline = Some(frames);
        }
    }
    Ok(())
}

#[test]
fn transitions_work_at_a_cut_without_lengthening_it() -> Result<()> {
    use cutad::effects::{Transition, TransitionKind};

    let folder = tempdir()?;
    for (name, color) in [("red.ppm", "red"), ("blue.ppm", "blue")] {
        let output = Command::new(executable("ffmpeg")?)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("color={color}:s=160x90"),
                "-frames:v",
                "1",
            ])
            .arg(folder.path().join(name))
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let assets = vec![
        probe(&folder.path().join("red.ppm"))?,
        probe(&folder.path().join("blue.ppm"))?,
    ];
    for kind in TransitionKind::ALL {
        let red = Clip::new(assets[0].id.clone(), 0, 0, 30);
        let mut blue = Clip::new(assets[1].id.clone(), 30, 0, 30);
        blue.transition = Some(Transition { kind, frames: 12 });
        let project = Project {
            width: 160,
            height: 90,
            assets: assets.clone(),
            tracks: vec![Track {
                clips: vec![red, blue],
                ..Track::new("Video", TrackKind::Video)
            }],
            ..Project::default()
        };
        let path = folder.path().join("transition.mp4");
        render(
            &project,
            &path,
            Format::Preview,
            &AtomicBool::new(false),
            |_| {},
        )
        .map_err(|error| anyhow::anyhow!("{kind:?}: {error:#}"))?;
        assert_eq!(probe(&path)?.frames, 60);
        let middle = monitor_frame(&path, 36, [160, 90])?;
        let final_frame = monitor_frame(&path, 46, [160, 90])?;
        assert_ne!(middle, final_frame);
        assert!(final_frame[2] > 180);
    }
    Ok(())
}
