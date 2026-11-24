use anyhow::{Context, Result};
use cutad::{
    media,
    project::{Clip, Project},
    subtitles, transcription,
};
use std::{path::PathBuf, sync::atomic::AtomicBool};

#[test]
#[ignore = "Requires eSpeak NG on PATH"]
fn local_narration_creates_audio_and_protects_existing_files() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let path = folder.path().join("Narration.wav");
    let asset = media::synthesize_narration("Welcome to the editing timeline.", "en", &path)?;
    assert_eq!(asset.path.as_deref(), Some(path.as_path()));
    assert!(asset.audio && asset.frames > 15);
    let original = std::fs::read(&path)?;
    assert!(media::synthesize_narration("Replacement", "en", &path).is_err());
    assert_eq!(std::fs::read(&path)?, original);
    Ok(())
}

#[test]
fn automatic_captions_require_a_model_and_respect_cancellation() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let model = folder.path().join("missing.bin");
    let error = transcription::generate(&Project::default(), &model, &AtomicBool::new(false))
        .err()
        .context("A missing model should fail.")?;
    assert!(error.to_string().contains("model file"));
    let error = transcription::generate(&Project::default(), &model, &AtomicBool::new(true))
        .err()
        .context("A canceled job should fail.")?;
    assert!(error.to_string().contains("canceled"));
    std::fs::write(&model, "Model file placeholder")?;
    let error = transcription::generate(&Project::default(), &model, &AtomicBool::new(false))
        .err()
        .context("A silent project should fail.")?;
    assert!(error.to_string().contains("audible media"));
    Ok(())
}

#[test]
#[ignore = "Requires a local whisper-cli, GGML model, and the synthetic speech fixture"]
fn whisper_transcribes_the_local_timeline_into_editable_captions() -> Result<()> {
    let env_path = |variable: &str| {
        std::env::var_os(variable)
            .map(PathBuf::from)
            .with_context(|| format!("Set {variable} to the local fixture path."))
    };
    let model = env_path("CUTAD_WHISPER_TEST_MODEL")?;
    let speech = env_path("CUTAD_WHISPER_TEST_SPEECH")?;
    let asset = media::probe(&speech)?;
    let mut project = Project::default();
    project.tracks[1]
        .clips
        .push(Clip::new(asset.id.clone(), 15, 0, asset.frames));
    project.assets.push(asset);
    let audio_end = project.duration();

    let cues = transcription::generate(&project, &model, &AtomicBool::new(false))?;
    let first_id = subtitles::add_tracks(&mut project, cues)?;
    let (row, col) = project
        .location(&first_id)
        .context("Missing caption clip.")?;
    let first = &project.tracks[row].clips[col];
    let caption = project
        .asset(&first.asset_id)
        .unwrap()
        .title
        .as_ref()
        .unwrap();
    assert_eq!(caption.text, caption.text.trim());
    let folder = tempfile::tempdir()?;
    let output = folder.path().join("captions.srt");
    subtitles::write(&project, &output)?;
    let text = std::fs::read_to_string(output)?.to_lowercase();
    assert!(text.contains("save the project"), "{text}");
    assert_eq!(project.duration(), audio_end);
    project.validate()?;
    Ok(())
}

#[test]
#[ignore = "Requires whisper-cli, a multilingual model, and the Spanish speech fixture"]
fn whisper_translates_spanish_to_editable_english_captions() -> Result<()> {
    let model =
        PathBuf::from(std::env::var_os("CUTAD_WHISPER_TEST_MODEL").context("Set the model path.")?);
    let speech = PathBuf::from(
        std::env::var_os("CUTAD_WHISPER_TEST_SPANISH").context("Set the Spanish speech path.")?,
    );
    let asset = media::probe(&speech)?;
    let mut project = Project::default();
    project.tracks[1]
        .clips
        .push(Clip::new(asset.id.clone(), 0, 0, asset.frames));
    project.assets.push(asset);
    let duration = project.duration();
    let cues = transcription::translate_to_english(&project, &model, &AtomicBool::new(false))?;
    let original = transcription::generate(&project, &model, &AtomicBool::new(false))?;
    subtitles::add_tracks(&mut project, original)?;
    let id = subtitles::add_english_tracks(&mut project, cues, true)?;
    let (row, col) = project
        .location(&id)
        .context("Missing English subtitle clip.")?;
    assert_eq!(project.tracks[row].name, "English captions");
    assert!(project.tracks[row].clips[col].transform.y < 0.0);
    let folder = tempfile::tempdir()?;
    let output = folder.path().join("translated.srt");
    subtitles::write_track(&project, &output, row)?;
    let text = std::fs::read_to_string(output)?.to_lowercase();
    assert!(
        text.contains("thank") && text.contains("tomorrow"),
        "{text}"
    );
    assert_eq!(project.duration(), duration);
    Ok(())
}
