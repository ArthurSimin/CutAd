//! Generate editable captions from the rendered timeline with a local Whisper model.

use crate::{
    media,
    project::Project,
    subtitles::{self, Cue},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

pub fn executable() -> Result<PathBuf> {
    media::executable("whisper-cli")
        .map_err(|_| anyhow!("Install whisper.cpp and put whisper-cli beside CutAd or on PATH."))
}

pub fn has_audible_clip(project: &Project) -> bool {
    project
        .tracks
        .iter()
        .filter(|track| !track.muted)
        .flat_map(|track| &track.clips)
        .any(|clip| {
            clip.has_audible_gain()
                && !clip.freeze
                && !clip.slot
                && project
                    .asset(&clip.asset_id)
                    .is_some_and(|asset| asset.audio)
        })
}

pub fn generate(project: &Project, model: &Path, cancel: &AtomicBool) -> Result<Vec<Cue>> {
    generate_cues(project, model, false, cancel)
}

pub fn translate_to_english(
    project: &Project,
    model: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<Cue>> {
    generate_cues(project, model, true, cancel)
}

fn generate_cues(
    project: &Project,
    model: &Path,
    translate: bool,
    cancel: &AtomicBool,
) -> Result<Vec<Cue>> {
    check_cancel(cancel)?;
    let model = std::path::absolute(model)?;
    ensure!(
        model.is_file(),
        "Choose a local whisper.cpp GGML model file."
    );
    ensure!(
        has_audible_clip(project),
        "Add audible media before generating captions."
    );
    let whisper = executable()?;

    let mut scratch = tempfile::Builder::new();
    scratch.prefix("cutad-captions-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        scratch.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let folder = scratch.tempdir()?;
    let duration = project.duration();
    let audio = folder.path().join("audio.wav");
    media::render_audio_mix(project, &audio, 0..duration, cancel, |_| {})?;
    check_cancel(cancel)?;

    let mut command = Command::new(std::path::absolute(whisper)?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut log = tempfile::tempfile_in(folder.path())?;
    if translate {
        command.args(["--translate", "--output-json"]);
    }
    let mut child = command
        .current_dir(folder.path())
        .arg("-m")
        .arg(model)
        .args(["-f", "audio.wav", "-l", "auto", "-osrt", "-of", "captions"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.try_clone()?)
        .spawn()
        .context("Could not start whisper-cli.")?;
    let status = wait_for_exit(&mut child, cancel);
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if !status?.success() {
        bail!(
            "Whisper could not generate captions. {}",
            diagnostics(&mut log)
        );
    }
    check_cancel(cancel)?;
    if translate {
        let report: serde_json::Value = serde_json::from_reader(
            File::open(folder.path().join("captions.json"))
                .context("Whisper did not report its translation settings.")?,
        )?;
        ensure!(
            report["params"]["translate"].as_bool() == Some(true),
            "Choose a multilingual Whisper model for English translation."
        );
    }
    let path = folder.path().join("captions.srt");
    let mut cues = subtitles::read(&path).with_context(|| {
        if path.is_file() {
            "Whisper produced no usable captions."
        } else {
            "Whisper did not write an SRT file."
        }
    })?;
    cues.retain_mut(|cue| cue.normalize_generated(duration));
    check_cancel(cancel)?;
    ensure!(
        !cues.is_empty(),
        "Whisper produced no captions within the timeline."
    );
    Ok(cues)
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Caption generation canceled."
    );
    Ok(())
}

fn wait_for_exit(child: &mut Child, cancel: &AtomicBool) -> Result<ExitStatus> {
    loop {
        check_cancel(cancel)?;
        if let Some(status) = child
            .try_wait()
            .context("Could not wait for whisper-cli.")?
        {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(80));
    }
}

fn diagnostics(log: &mut File) -> String {
    let mut tail = Vec::new();
    let end = log.metadata().map_or(0, |metadata| metadata.len());
    if log.seek(SeekFrom::Start(end.saturating_sub(2048))).is_ok() {
        let _ = log.take(2048).read_to_end(&mut tail);
    }
    String::from_utf8_lossy(&tail).trim().to_owned()
}
