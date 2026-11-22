use cutad::{capcut::import, project::Asset};
use serde_json::json;
use std::{fs, path::Path};
use tempfile::tempdir;

#[test]
fn draft_preserves_local_timing_and_reports_omitted_features() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let media = dir.path().join("original.mp4");
    fs::write(&media, b"fixture stub")?;
    let draft = dir.path().join("draft_content.json");
    fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "canvas_config": {"width": 720, "height": 1280},
            "materials": {"videos": [
                {"id": "v", "type": "video", "path": "original.mp4"},
                {"id": "p", "type": "photo", "path": "https://example.invalid/protected.png"}
            ]},
            "tracks": [
                {"type": "video", "segments": [
                    {"material_id": "v", "source_timerange": {"start": 1000000, "duration": 2000000},
                     "target_timerange": {"start": 2000000, "duration": 2000000}, "volume": 0.5,
                     "clip": {"scale": {"x": 1.2, "y": 1.2}}},
                    {"material_id": "p", "target_timerange": {"start": 4000000, "duration": 1000000}}
                ]},
                {"type": "text", "segments": [{"material_id": "text"}]}
            ]
        }))?,
    )?;
    let result = import(&draft, None, |path: &Path| {
        Ok(Asset::video(path, 180, true))
    })?;
    let project = &result.project;
    assert_eq!((project.width, project.height), (720, 1280));
    let clips = &project.tracks[0].clips;
    assert_eq!(
        (
            clips[0].start,
            clips[0].source,
            clips[0].length,
            clips[0].gain
        ),
        (60, 30, 60, 0.5)
    );
    assert!(!clips[0].slot && clips[1].slot);
    assert!(result.warnings.iter().any(|line| line.contains("text")));
    assert!(
        result
            .warnings
            .iter()
            .any(|line| line.contains("transform"))
    );
    Ok(())
}

#[test]
fn encrypted_or_unrelated_input_is_rejected() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("draft_content.json");
    fs::write(&path, b"not JSON")?;
    let error = import(&path, None, |_| unreachable!()).unwrap_err();
    assert!(error.to_string().contains("readable"));
    Ok(())
}

#[test]
fn draft_plain_text_remains_editable_at_the_original_time() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let path = dir.path().join("draft_info.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "materials": {"texts":[{"id":"title","content": "{\"text\":\"Hello world\"}"}]},
            "tracks": [{"type":"text", "segments":[{"material_id":"title",
                "target_timerange":{"start":1000000,"duration":2000000}}]}]
        }))?,
    )?;
    let result = import(&path, None, |_| unreachable!())?;
    let clip = &result.project.tracks[0].clips[0];
    assert_eq!((clip.start, clip.length), (30, 60));
    assert_eq!(
        result
            .project
            .asset(&clip.asset_id)
            .unwrap()
            .title
            .as_ref()
            .unwrap()
            .text,
        "Hello world"
    );
    Ok(())
}
