//! Read-only translation of local, unencrypted CapCut JSON projects into CutAd.

use crate::project::{
    Asset, AssetKind, Clip, FPS, MAX_FRAMES, Project, TitleStyle, Track, TrackKind, valid_canvas,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct DraftImport {
    pub project: Project,
    pub warnings: Vec<String>,
}

fn list(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn name(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

fn frames(us: u64) -> Result<u32> {
    let converted = (u128::from(us) * u128::from(FPS) + 500_000) / 1_000_000;
    ensure!(
        converted <= u128::from(MAX_FRAMES),
        "Draft timing exceeds 24 hours."
    );
    Ok(converted as u32)
}

fn local_media(raw: &str, draft_dir: &Path, media_dir: &Path) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains("://") || raw.contains('\0') {
        return None;
    }
    let normalized = raw.replace('\\', "/");
    let path = Path::new(&normalized);
    let basename = path.file_name()?;
    [
        draft_dir.join(path),
        media_dir.join(path),
        media_dir.join(basename),
    ]
    .into_iter()
    .filter_map(|candidate| candidate.canonicalize().ok())
    .find(|candidate| {
        candidate.is_file()
            && (candidate.starts_with(draft_dir) || candidate.starts_with(media_dir))
    })
}

pub fn import(
    path: &Path,
    media_dir: Option<&Path>,
    probe: impl Fn(&Path) -> Result<Asset>,
) -> Result<DraftImport> {
    let path = if path.is_dir() {
        ["draft_content.json", "draft_info.json"]
            .into_iter()
            .map(|name| path.join(name))
            .find(|p| p.is_file())
            .context("No local CapCut draft JSON found in this folder.")?
    } else {
        path.to_owned()
    };
    let path = path.canonicalize()?;
    let draft_dir = path.parent().context("Choose a local CapCut JSON file.")?;
    let media_dir = media_dir
        .map(Path::canonicalize)
        .transpose()?
        .unwrap_or_else(|| draft_dir.to_owned());
    let mut bytes = Vec::new();
    const LIMIT: u64 = 32 * 1024 * 1024;
    std::fs::File::open(&path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "The draft exceeds the supported 32 MiB JSON size."
    );
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let document: Value = serde_json::from_slice(body)
        .context("Choose a readable, unencrypted CapCut JSON draft.")?;
    let tracks = document["tracks"]
        .as_array()
        .context("The draft has no track list.")?;
    let materials = document["materials"]
        .as_object()
        .context("The draft has no material list.")?;
    let mut project = Project {
        name: name(&document["name"])
            .unwrap_or_else(|| {
                draft_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Imported template")
            })
            .to_owned(),
        tracks: Vec::new(),
        ..Project::default()
    };
    let canvas = &document["canvas_config"];
    if let (Some(width), Some(height)) = (canvas["width"].as_u64(), canvas["height"].as_u64()) {
        if width <= 4096 && height <= 4096 {
            let (width, height) = (width as u32 & !1, height as u32 & !1);
            if valid_canvas(width, height) {
                project.width = width;
                project.height = height;
            }
        }
    }
    let mut warnings = Vec::new();
    if document["fps"]
        .as_f64()
        .is_some_and(|fps| fps != f64::from(FPS))
    {
        warnings.push("Draft timing was converted to CutAd’s 30 fps timeline.".into());
    }
    let mut index = HashMap::new();
    for (group, default_kind) in [
        ("videos", AssetKind::Video),
        ("audios", AssetKind::Audio),
        ("texts", AssetKind::Text),
    ] {
        if let Some(values) = materials.get(group) {
            for item in list(values) {
                if let Some(id) = name(&item["id"]) {
                    let kind = if matches!(item["type"].as_str(), Some("photo" | "image")) {
                        AssetKind::Image
                    } else {
                        default_kind
                    };
                    index.insert(id, (item, kind));
                }
            }
        }
    }

    let mut probes = HashMap::<PathBuf, Option<Asset>>::new();
    for (row, raw_track) in tracks.iter().enumerate() {
        let row = row + 1;
        let segments = list(&raw_track["segments"]);
        let is_text = raw_track["type"] == "text";
        let kind = match raw_track["type"].as_str() {
            Some("video" | "text") => TrackKind::Video,
            Some("audio") => TrackKind::Audio,
            other => {
                if !segments.is_empty() {
                    warnings.push(format!(
                        "Skipped {} track {row}.",
                        other.unwrap_or("unknown")
                    ));
                }
                continue;
            }
        };
        let track_name = name(&raw_track["name"])
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{} {row}", if is_text { "Titles" } else { kind.label() }));
        let mut track = Track::new(track_name, kind);
        track.muted = raw_track["mute"].as_bool().unwrap_or(false);
        for (column, segment) in segments.iter().enumerate() {
            let column = column + 1;
            let current = if is_text {
                format!("Track {row}, text clip {column}")
            } else {
                format!("Track {row}, clip {column}")
            };
            let Ok((start, source, length)) = ranges(segment) else {
                warnings.push(format!("{current}: skipped invalid timing."));
                continue;
            };
            if track
                .clips
                .iter()
                .any(|clip| start < clip.end() && clip.start < start + length)
            {
                warnings.push(format!("{current}: skipped an overlap on the same track."));
                continue;
            }
            if segment["visible"] == Value::Bool(false) {
                warnings.push(format!("{current}: skipped a hidden clip."));
                continue;
            }
            report_settings(segment, length, &current, &mut warnings);
            let fallback = if kind == TrackKind::Video {
                AssetKind::Video
            } else {
                AssetKind::Audio
            };
            let (material, default_kind) = name(&segment["material_id"])
                .and_then(|id| index.get(id).copied())
                .unwrap_or((&Value::Null, fallback));
            if is_text {
                let Some(text) = plain_text(material) else {
                    warnings.push(format!("{current}: skipped unsupported text content."));
                    continue;
                };
                let asset = Asset::text(TitleStyle::Caption, text);
                track
                    .clips
                    .push(Clip::new(asset.id.clone(), start, 0, length));
                project.assets.push(asset);
                warnings.push(format!("{current}: used the CutAd caption style."));
                continue;
            }
            let raw_path = material["path"].as_str().unwrap_or("");
            let asset = local_media(raw_path, draft_dir, &media_dir)
                .and_then(|path| {
                    probes
                        .entry(path.clone())
                        .or_insert_with(|| probe(&path).ok())
                        .clone()
                })
                .filter(|asset| {
                    kind.accepts(asset)
                        && (asset.kind == AssetKind::Image || source + length <= asset.frames)
                });
            let asset = asset.unwrap_or_else(|| {
                let basename = (!raw_path.contains("://"))
                    .then(|| raw_path.rsplit(['/', '\\']).next())
                    .flatten()
                    .filter(|base| !base.is_empty());
                let asset_name = name(&material["material_name"])
                    .or_else(|| name(&material["name"]))
                    .or(basename)
                    .unwrap_or(&current);
                warnings.push(format!(
                    "{current}: replace the missing or incompatible media for {asset_name}."
                ));
                let placeholder = if kind == TrackKind::Video && default_kind == AssetKind::Image {
                    default_kind
                } else {
                    fallback
                };
                Asset::placeholder(asset_name, placeholder, source + length)
            });
            let mut clip = Clip::new(
                asset.id.clone(),
                start,
                if asset.kind == AssetKind::Image {
                    0
                } else {
                    source
                },
                length,
            );
            clip.slot = asset.path.is_none();
            if let Some(gain) = segment["volume"].as_f64() {
                if (0.0..=4.0).contains(&gain) {
                    clip.gain = gain as f32;
                } else {
                    warnings.push(format!("{current}: unsupported volume was reset."));
                }
            }
            if project.asset(&asset.id).is_none() {
                project.assets.push(asset);
            }
            track.clips.push(clip);
        }
        project.tracks.push(track);
    }
    if project.duration() == 0 {
        bail!("The draft contains no supported video, image, or audio clips.");
    }
    let used: HashSet<_> = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| c.asset_id.as_str())
        .collect();
    project
        .assets
        .retain(|asset| used.contains(asset.id.as_str()));
    project.validate()?;
    Ok(DraftImport { project, warnings })
}

fn plain_text(material: &Value) -> Option<String> {
    let content = &material["content"];
    let body = match content {
        Value::Object(_) => name(&content["text"]).map(str::to_owned),
        Value::String(raw) if raw.trim_start().starts_with('{') => {
            serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|document| name(&document["text"]).map(str::to_owned))
        }
        Value::String(_) => name(content).map(str::to_owned),
        _ => name(&material["text"]).map(str::to_owned),
    }?;
    (body.len() <= 4096 && !body.contains('\0')).then_some(body)
}

fn ranges(segment: &Value) -> Result<(u32, u32, u32)> {
    let target = &segment["target_timerange"];
    let start_us = target["start"]
        .as_u64()
        .context("Missing timeline start.")?;
    let duration_us = target["duration"]
        .as_u64()
        .filter(|n| *n > 0)
        .context("Missing duration.")?;
    let start = frames(start_us)?;
    let end = frames(
        start_us
            .checked_add(duration_us)
            .context("Duration overflow.")?,
    )?;
    let length = (end - start).max(1);
    let source = frames(segment["source_timerange"]["start"].as_u64().unwrap_or(0))?;
    ensure!(
        start + length <= MAX_FRAMES && source + length <= MAX_FRAMES,
        "Clip is too long."
    );
    Ok((start, source, length))
}

fn report_settings(segment: &Value, length: u32, current: &str, warnings: &mut Vec<String>) {
    if segment["speed"].as_f64().is_some_and(|speed| speed != 1.0)
        || segment["source_timerange"]["duration"]
            .as_u64()
            .and_then(|n| frames(n).ok())
            .is_some_and(|frames| frames.abs_diff(length) > 1)
    {
        warnings.push(format!("{current}: speed changes were omitted."));
    }
    let clip = &segment["clip"];
    let changed = |value: &Value, default: f64| value.as_f64().is_some_and(|n| n != default);
    if changed(&clip["alpha"], 1.0)
        || changed(&clip["rotation"], 0.0)
        || ["x", "y"].into_iter().any(|axis| {
            changed(&clip["scale"][axis], 1.0) || changed(&clip["transform"][axis], 0.0)
        })
        || ["horizontal", "vertical"]
            .into_iter()
            .any(|axis| clip["flip"][axis] == Value::Bool(true))
        || ["common_keyframes", "keyframe_refs"]
            .into_iter()
            .any(|key| !list(&segment[key]).is_empty())
    {
        warnings.push(format!("{current}: transform or keyframes were omitted."));
    }
    if !list(&segment["extra_material_refs"]).is_empty()
        || ["effects", "mask", "transition"]
            .into_iter()
            .any(|key| !segment[key].is_null())
    {
        warnings.push(format!(
            "{current}: linked effects or other settings may have been omitted."
        ));
    }
}
