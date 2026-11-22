mod app;
mod branding;
mod editor_mcp;
mod export_dialog;
mod font_browser;
mod gallery;
mod playback;
mod preview;
mod theme;
mod thumbnails;
mod timeline;
mod typography;
mod waveforms;

use anyhow::{Context, Result, bail};
use cutad::{capcut, media, project};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn main() {
    if let Err(error) = run() {
        eprintln!("CutAd: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--version") => {
            println!("CutAd {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--mcp") => {
            let path = args
                .next()
                .context("Usage: cutad --mcp PROJECT [--write]")?;
            return editor_mcp::serve(path.into(), mcp_write_flag(args)?);
        }
        Some("--mcp-config") => {
            let client = args
                .next()
                .and_then(|x| x.into_string().ok())
                .context("Usage: cutad --mcp-config CLIENT PROJECT [--write]")?;
            let path = args
                .next()
                .context("Usage: cutad --mcp-config CLIENT PROJECT [--write]")?;
            return editor_mcp::print_config(&client, path.into(), mcp_write_flag(args)?);
        }
        Some("--import-draft") => {
            let draft = args
                .next()
                .context("Usage: cutad --import-draft DRAFT OUTPUT [MEDIA_FOLDER]")?;
            let output = args
                .next()
                .context("Usage: cutad --import-draft DRAFT OUTPUT [MEDIA_FOLDER]")?;
            let folder = args.next().map(PathBuf::from);
            if project::same_path(Path::new(&draft), Path::new(&output)) {
                bail!("Choose a CutAd file separate from the original CapCut draft.");
            }
            let result = capcut::import(Path::new(&draft), folder.as_deref(), media::probe)?;
            project::save(&result.project, Path::new(&output))?;
            for warning in result.warnings {
                eprintln!("{warning}");
            }
            return Ok(());
        }
        Some(option @ ("--export-audio" | "--export-captions" | "--export-frame")) => {
            let usage = format!(
                "Usage: cutad {option} PROJECT OUTPUT{}",
                if option == "--export-frame" {
                    " FRAME"
                } else {
                    ""
                }
            );
            let source = args.next().with_context(|| usage.clone())?;
            let output = args.next().with_context(|| usage.clone())?;
            let source = Path::new(&source);
            let output = Path::new(&output);
            if project::same_path(source, output) {
                bail!("Choose an export separate from the project.");
            }
            let project = project::load(source)?;
            let cancel = AtomicBool::new(false);
            match option {
                "--export-audio" => media::render_audio_master(&project, output, &cancel, |_| {})?,
                "--export-captions" => {
                    cutad::subtitles::write(&project, output)?;
                }
                "--export-frame" => {
                    let frame = args
                        .next()
                        .and_then(|arg| arg.to_str()?.parse::<u32>().ok())
                        .context(usage)?;
                    media::export_frame_png(&project, output, frame, &cancel)?;
                }
                _ => unreachable!(),
            }
            return Ok(());
        }
        Some("--render") => {
            let source = args
                .next()
                .context("Usage: cutad --render PROJECT OUTPUT")?;
            let output = args
                .next()
                .context("Usage: cutad --render PROJECT OUTPUT")?;
            let source = Path::new(&source);
            let path = Path::new(&output);
            if project::same_path(source, path) {
                bail!("Choose an export separate from the project.");
            }
            let format = match path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("mp4") => media::Format::Mp4,
                Some("webm") => media::Format::Webm,
                Some("mkv") => media::Format::Mkv,
                _ => bail!("Export must end in .mp4, .webm, or .mkv."),
            };
            let project = project::load(source)?;
            return media::render_with(
                &project,
                path,
                media::Output::for_project(format, &project),
                &AtomicBool::new(false),
                |_| {},
            );
        }
        Some(option) if option.starts_with("--") => bail!("Unknown option {option}."),
        _ => {}
    }
    let path = first.map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([920.0, 630.0])
            .with_icon(branding::icon()),
        ..Default::default()
    };
    eframe::run_native(
        "CutAd",
        options,
        Box::new(move |creation| Ok(Box::new(app::Editor::new(&creation.egui_ctx, path)))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(())
}

fn mcp_write_flag(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<bool> {
    match args.next() {
        None => Ok(false),
        Some(flag) if flag == "--write" && args.next().is_none() => Ok(true),
        _ => bail!("The only optional MCP argument is --write."),
    }
}
