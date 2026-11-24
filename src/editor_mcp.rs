//! Project-scoped MCP over local stdio.

use anyhow::{Context, Result, bail, ensure};
use cutad::{
    media,
    project::{self, Project, TitleStyle},
};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const MAX_REQUEST: u64 = 1024 * 1024;
const PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub fn serve(path: PathBuf, writable: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .context("Choose a saved CutAd project.")?;
    project::load(&path)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut initialized = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut stdin)
            .take(MAX_REQUEST + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        ensure!(
            line.len() as u64 <= MAX_REQUEST,
            "MCP request exceeds 1 MiB."
        );
        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(request) => dispatch(&path, writable, &mut initialized, &request),
            Err(_) => Some(error(Value::Null, -32700, "Invalid JSON")),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
}

fn dispatch(path: &Path, writable: bool, initialized: &mut bool, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request["method"].as_str();
    if request["jsonrpc"] != "2.0"
        || method.is_none()
        || id
            .as_ref()
            .is_some_and(|id| !id.is_string() && !id.is_i64() && !id.is_u64())
    {
        return Some(error(Value::Null, -32600, "Invalid JSON-RPC request"));
    }
    let id = id?;
    let result = match method.unwrap() {
        "initialize" => {
            let requested = request["params"]["protocolVersion"]
                .as_str()
                .unwrap_or_default();
            let version = PROTOCOLS
                .iter()
                .copied()
                .find(|v| *v == requested)
                .unwrap_or(PROTOCOLS[0]);
            *initialized = true;
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "CutAd", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Tools operate on one saved CutAd project. Timeline positions use 30 frames per second. Save and close the desktop project before MCP editing."
            })
        }
        "ping" => json!({}),
        _ if !*initialized => return Some(error(id, -32002, "Initialize the server first")),
        "tools/list" => json!({"tools": tools(writable)}),
        "tools/call" => {
            let params = &request["params"];
            let Some(name) = params["name"].as_str() else {
                return Some(error(id, -32602, "A tool name is required"));
            };
            match invoke(path, writable, name, &params["arguments"]) {
                Ok(value) => {
                    json!({"content": [{"type": "text", "text": value.to_string()}], "isError": false})
                }
                Err(e) => {
                    json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})
                }
            }
        }
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0", "id":id, "result":result}))
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

fn tools(writable: bool) -> Vec<Value> {
    let tool = |name, description, read_only: bool, properties: Value, required: &[&str]| {
        json!({
            "name":name, "description":description,
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":read_only,"destructiveHint": !read_only,"openWorldHint":false}
        })
    };
    let mut list = vec![tool(
        "inspect_project",
        "Read canvas, tracks, clip IDs, timing and title text from the saved project.",
        true,
        json!({}),
        &[],
    )];
    if writable {
        let frame = json!({"type":"integer","minimum":0});
        let id = json!({"type":"string"});
        list.extend([
            tool("add_title", "Add a transparent title at a timeline frame.", false,
                json!({"text":{"type":"string","maxLength":1024}, "frame":frame}), &["text","frame"]),
            tool("split_clip", "Split one existing clip at a timeline frame.", false,
                json!({"clip_id":id, "frame":frame}), &["clip_id","frame"]),
            tool("set_clip_gain", "Set a constant audio gain multiplier and clear the clip's gain keyframes.", false,
                json!({"clip_id":id, "gain":{"type":"number","minimum":0,"maximum":4}}), &["clip_id","gain"]),
            tool("set_gain_keyframe", "Set an audio gain multiplier at a timeline frame inside the clip.", false,
                json!({"clip_id":id, "frame":frame, "gain":{"type":"number","minimum":0,"maximum":4}}), &["clip_id","frame","gain"]),
            tool("export", "Render a new MP4, WebM, MKV, GIF, WAV, FLAC, MP3, or SRT next to the project. Existing files are protected.", false,
                json!({"filename":{"type":"string"}}), &["filename"]),
        ]);
    }
    list
}

fn invoke(path: &Path, writable: bool, name: &str, args: &Value) -> Result<Value> {
    let mut project = project::load(path)?;
    if name == "inspect_project" {
        let tracks: Vec<_> = project.tracks.iter().map(|track| {
            let clips: Vec<_> = track.clips.iter().map(|clip| {
                let asset = project.asset(&clip.asset_id);
                json!({"id":clip.id, "start":clip.start,"source":clip.source,"length":clip.length,"gain":clip.gain,
                    "label":asset.map(|a| a.label.as_str()), "title":asset.and_then(|a| a.title.as_ref()).map(|t| &t.text)})
            }).collect();
            json!({"name":track.name,"kind":track.kind,"muted":track.muted,"clips":clips})
        }).collect();
        return Ok(
            json!({"name":project.name,"width":project.width,"height":project.height,
            "duration_frames":project.duration(),"fps":project::FPS,"tracks":tracks}),
        );
    }
    ensure!(writable, "Start CutAd MCP with --write to edit or export.");
    let field = |key: &str| {
        args[key]
            .as_str()
            .with_context(|| format!("Provide {key} as a string."))
    };
    let frame = || {
        args["frame"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n < project::MAX_FRAMES)
            .context("Provide a valid timeline frame.")
    };
    let change = match name {
        "add_title" => {
            let title = field("text")?;
            ensure!(
                !title.trim().is_empty() && title.len() <= 4096,
                "Provide 1–4096 bytes of title text."
            );
            json!({"clip_id":project.add_title(TitleStyle::Headline, title, frame()?)?})
        }
        "split_clip" => json!({"clip_id":project.split(field("clip_id")?, frame()?)?}),
        "set_clip_gain" | "set_gain_keyframe" => {
            let (row, index) = project
                .location(field("clip_id")?)
                .context("Clip not found.")?;
            let gain = args["gain"]
                .as_f64()
                .filter(|x| (0.0..=4.0).contains(x))
                .context("Gain must be between 0 and 4.")?;
            if name == "set_gain_keyframe" {
                let clip = &project.tracks[row].clips[index];
                ensure!(
                    !clip.freeze
                        && !clip.slot
                        && project
                            .asset(&clip.asset_id)
                            .is_some_and(|asset| asset.audio),
                    "Choose a clip with audio."
                );
            }
            let clip = &mut project.tracks[row].clips[index];
            if name == "set_gain_keyframe" {
                let timeline = frame()?;
                let local = timeline
                    .checked_sub(clip.start)
                    .context("Choose a frame inside the clip.")?;
                clip.set_scalar_keyframe(project::AnimatedProperty::AudioGain, local, gain as f32)?;
                json!({"gain": gain, "frame": timeline})
            } else {
                clip.gain = gain as f32;
                clip.gain_animation = Default::default();
                json!({"gain": gain})
            }
        }
        "export" => return export(path, &project, field("filename")?),
        _ => bail!("Unknown CutAd tool."),
    };
    project::save(&project, path)?;
    Ok(change)
}

fn export(project_file: &Path, project: &Project, filename: &str) -> Result<Value> {
    ensure!(
        !filename.is_empty()
            && !filename.starts_with('.')
            && filename.len() <= 120
            && filename
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ')),
        "Use a plain filename with letters, digits, spaces, hyphens, or underscores."
    );
    let dir = project_file
        .parent()
        .context("Project has no parent folder.")?;
    let dest = dir.join(filename);
    ensure!(!dest.exists(), "Choose a new output filename.");
    let extension = dest
        .extension()
        .and_then(|s| s.to_str())
        .context("Provide an output extension.")?;
    let scratch = tempfile::Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile_in(dir)?
        .into_temp_path();
    let cancel = AtomicBool::new(false);
    match extension {
        "srt" => {
            cutad::subtitles::write(project, &scratch)?;
        }
        "wav" | "flac" | "mp3" => media::export_audio(
            project,
            &scratch,
            media::AudioFormat::from_path(&scratch)?,
            &cancel,
            |_| {},
        )?,
        "mp4" | "webm" | "mkv" | "gif" => {
            let format = match extension {
                "webm" => media::Format::Webm,
                "mkv" => media::Format::Mkv,
                "gif" => media::Format::Gif,
                _ => media::Format::Mp4,
            };
            media::render_with(
                project,
                &scratch,
                media::Output::for_project(format, project),
                &cancel,
                |_| {},
            )?;
        }
        _ => bail!("Choose .mp4, .webm, .mkv, .gif, .wav, .flac, .mp3, or .srt."),
    }
    scratch
        .persist_noclobber(&dest)
        .context("An output with that name already exists.")?;
    Ok(json!({"file": dest}))
}

pub fn print_config(client: &str, path: PathBuf, writable: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .context("Choose a saved CutAd project.")?;
    let exe = std::env::current_exe()?;
    let mut args = vec![
        "--mcp".to_owned(),
        path.to_str()
            .context("Project path must be Unicode.")?
            .into(),
    ];
    if writable {
        args.push("--write".into());
    }
    let value = match client {
        "codex" => {
            println!(
                "[mcp_servers.cutad]\ncommand = {}\nargs = {}\ntool_timeout_sec = 600",
                serde_json::to_string(&exe)?,
                serde_json::to_string(&args)?
            );
            return Ok(());
        }
        "claude-code" | "antigravity" => {
            json!({"mcpServers":{"cutad":{"command":exe,"args":args}}})
        }
        "opencode" | "kilo-code" => {
            let mut command = vec![
                exe.to_str()
                    .context("Executable path must be Unicode.")?
                    .to_owned(),
            ];
            command.extend(args);
            json!({"mcp":{"cutad":{"type":"local","command":command,"enabled":true,"timeout":600000}}})
        }
        _ => bail!("Use codex, claude-code, opencode, kilo-code, or antigravity."),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
