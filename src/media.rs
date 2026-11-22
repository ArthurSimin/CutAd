//! Local media probing and a single compositing path for preview and export.

use crate::effects::Transition;
use crate::fonts;
use crate::project::{
    Asset, AssetKind, Clip, ClipMask, ColorAdjustment, FPS, MAX_FRAMES, MaskShape, Project,
    TextAlign, Title, TitleStyle, TrackKind, Transform, VideoGeometry, same_path, seconds,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{File, read_to_string};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const DEMUXERS: &str = "mov,matroska,webm,avi,mpeg,mpegts,mpegvideo,asf,flv,ogg,wav,mp3,flac,aac,aiff,opus,image2,png_pipe,jpeg_pipe,webp_pipe,bmp_pipe,tiff_pipe,ppm_pipe,pgm_pipe,gif,apng";
const SAMPLE_RATE: u32 = 48_000;
const SAMPLES_PER_FRAME: i64 = (SAMPLE_RATE / FPS) as i64;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tiff", "tif", "ppm"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Preview,
    Mp4,
    Webm,
    Mkv,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Webm => "webm",
            Self::Mkv => "mkv",
            Self::Preview | Self::Mp4 => "mp4",
        }
    }

    pub fn size(self, project: &Project) -> [usize; 2] {
        let (w, h) = (project.width, project.height);
        if self != Self::Preview {
            return [w as usize, h as usize];
        }
        let ratio = (640.0 / f64::from(w)).min(360.0 / f64::from(h)).min(1.0);
        [w, h].map(|n| ((f64::from(n) * ratio) as usize & !1).max(16))
    }

    fn codecs(self, quality: Quality, full_preview: bool) -> Vec<&'static str> {
        match self {
            Self::Preview if full_preview => vec![
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-crf",
                "16",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ],
            Self::Mkv => vec![
                "-c:v",
                "ffv1",
                "-level",
                "3",
                "-slicecrc",
                "1",
                "-c:a",
                "flac",
            ],
            Self::Preview => vec![
                "-c:v",
                "mpeg4",
                "-q:v",
                "4",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ],
            Self::Mp4 => vec![
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-crf",
                quality.crf(false),
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                if quality == Quality::High {
                    "320k"
                } else {
                    "192k"
                },
                "-movflags",
                "+faststart",
            ],
            Self::Webm => vec![
                "-c:v",
                "libvpx-vp9",
                "-pix_fmt",
                "yuv420p",
                "-crf",
                quality.crf(true),
                "-b:v",
                "0",
                "-c:a",
                "libopus",
                "-b:a",
                "160k",
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    Compact,
    #[default]
    Balanced,
    High,
}

impl Quality {
    fn crf(self, webm: bool) -> &'static str {
        match (self, webm) {
            (Self::Compact, false) => "27",
            (Self::Balanced, false) => "21",
            (Self::High, false) => "14",
            (Self::Compact, true) => "37",
            (Self::Balanced, true) => "28",
            (Self::High, true) => "20",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    pub format: Format,
    pub quality: Quality,
    pub short_edge: Option<u32>,
    pub fps: u32,
}

impl Output {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            quality: Quality::Balanced,
            short_edge: None,
            fps: FPS,
        }
    }

    pub fn for_project(format: Format, project: &Project) -> Self {
        let mut output = Self::new(format);
        output.quality = Quality::High;
        if let Some(source) = project.source_video() {
            output.fps = match source.fps {
                0..=21 => 30,
                22..=24 => 24,
                25..=30 => 30,
                31..=60 => 60,
                _ => 120,
            };
        }
        output
    }

    pub fn size(self, project: &Project) -> [usize; 2] {
        let size = self.format.size(project);
        let short = size[0].min(size[1]);
        let output = self
            .short_edge
            .map_or(short, |edge| edge as usize)
            .clamp(16, short);
        size.map(|dimension| ((dimension * output / short) & !1).max(16))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewQuality {
    #[default]
    Auto,
    Full,
    Half,
    Quarter,
}

impl PreviewQuality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Full => "Full canvas",
            Self::Half => "Half resolution",
            Self::Quarter => "Quarter resolution",
        }
    }

    pub fn size(self, project: &Project, still: bool) -> [usize; 2] {
        let [w, h] = [project.width as usize, project.height as usize];
        match self {
            Self::Full => [w, h],
            Self::Auto if still => [w, h],
            Self::Auto => Format::Preview.size(project),
            Self::Half => [w, h].map(|n| ((n / 2) & !1).max(16)),
            Self::Quarter => [w, h].map(|n| ((n / 4) & !1).max(16)),
        }
    }
}

#[derive(Clone, Copy)]
struct Raster {
    width: usize,
    height: usize,
    rate: u32,
    full_chroma: bool,
}

impl Raster {
    fn alpha(self) -> &'static str {
        if self.full_chroma {
            "yuva444p"
        } else {
            "yuva420p"
        }
    }

    fn opaque(self) -> &'static str {
        if self.full_chroma {
            "yuv444p"
        } else {
            "yuv420p"
        }
    }

    fn overlay(self) -> &'static str {
        if self.full_chroma { "yuv444" } else { "yuv420" }
    }
}

struct Layer<'a> {
    kind: TrackKind,
    clip: &'a Clip,
    asset: &'a Asset,
    input: Option<usize>,
    mask_input: Option<usize>,
    font: Option<String>,
    seek: u32,
    start: u32,
    skip: u32,
    length: u32,
    transition: Option<Transition>,
}

fn stage_mask(path: &Path, mask: ClipMask, raster: Raster, cancel: &AtomicBool) -> Result<()> {
    let (width, height) = (raster.width, raster.height);
    let (w, h) = (width as f64, height as f64);
    let half_width = w * f64::from(mask.width) * 0.5;
    let half_height = h * f64::from(mask.height) * 0.5;
    let (cx, cy) = (w * f64::from(mask.x), h * f64::from(mask.y));
    let (sin, cos) = f64::from(mask.rotation).to_radians().sin_cos();
    let feather = w.min(h) * f64::from(mask.feather);
    let mut output = BufWriter::new(File::create(path)?);
    write!(output, "P5\n{width} {height}\n255\n")?;
    let mut row = vec![0; width];
    for y in 0..height {
        if y % 64 == 0 {
            check_cancel(cancel)?;
        }
        let dy = y as f64 + 0.5 - cy;
        for (x, alpha) in row.iter_mut().enumerate() {
            let dx = x as f64 + 0.5 - cx;
            let u = (dx * cos + dy * sin).abs();
            let v = -dx * sin + dy * cos;
            let distance = match mask.shape {
                MaskShape::Rectangle => {
                    let x = u - half_width;
                    let y = v.abs() - half_height;
                    let outside_x = x.max(0.0);
                    let outside_y = y.max(0.0);
                    -(x.max(y).min(0.0) + (outside_x * outside_x + outside_y * outside_y).sqrt())
                }
                MaskShape::Ellipse => {
                    let u = u / half_width;
                    let v = v / half_height;
                    let radius = (u * u + v * v).sqrt();
                    let grad_u = u / half_width;
                    let grad_v = v / half_height;
                    let gradient = (grad_u * grad_u + grad_v * grad_v).sqrt();
                    // Near the ellipse edge this estimates the perpendicular distance in pixels.
                    if gradient > 0.0 {
                        (1.0 - radius) * radius / gradient
                    } else {
                        half_width.min(half_height)
                    }
                }
                MaskShape::Linear => v,
            };
            let coverage = if feather == 0.0 {
                f64::from(distance >= 0.0)
            } else {
                (0.5 + distance / feather).clamp(0.0, 1.0)
            };
            let value = (255.0 * coverage).round() as u8;
            *alpha = if mask.invert { 255 - value } else { value };
        }
        output.write_all(&row)?;
    }
    Ok(output.flush()?)
}

fn tool_in(dir: impl AsRef<Path>, name: &str) -> Option<PathBuf> {
    let path = dir
        .as_ref()
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

pub fn executable(name: &str) -> Result<PathBuf> {
    let app_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned));
    app_dir
        .and_then(|dir| tool_in(dir, name))
        .or_else(|| {
            std::env::var_os("PATH")
                .and_then(|path| std::env::split_paths(&path).find_map(|dir| tool_in(dir, name)))
        })
        .with_context(|| format!("{name} is required. Install FFmpeg and add its tools to PATH."))
}

fn command(name: &str) -> Result<Command> {
    let mut command = Command::new(executable(name)?);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    Ok(command)
}

pub fn probe(path: &Path) -> Result<Asset> {
    let path = std::path::absolute(path)?;
    ensure!(path.is_file(), "Media is missing: {}", path.display());
    let output = command("ffprobe")?
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(&path)
        .output()?;
    ensure!(
        output.status.success(),
        "Unsupported or unreadable media: {}",
        path.display()
    );
    let data: Value = serde_json::from_slice(&output.stdout)?;
    let streams = data["streams"]
        .as_array()
        .context("Media contains no readable streams.")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1);
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    let main = video
        .or(audio)
        .context("Media contains no video or audio stream.")?;
    if video.is_some()
        && path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
    {
        return Ok(Asset::local(path, AssetKind::Image, 1, false));
    }
    let seconds = |value: &Value| {
        value
            .as_str()
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|n| n.is_finite())
    };
    let duration = [
        seconds(&main["duration"]),
        seconds(&data["format"]["duration"]),
    ]
    .into_iter()
    .flatten()
    .find(|n| *n > 0.0)
    .context("Media has no usable duration.")?;
    let frames = (duration * f64::from(FPS)).round().max(1.0);
    ensure!(
        frames <= f64::from(MAX_FRAMES),
        "Media is longer than the supported 24 hours."
    );
    let kind = if video.is_some() {
        AssetKind::Video
    } else {
        AssetKind::Audio
    };
    let mut asset = Asset::local(path, kind, frames as u32, audio.is_some());
    asset.geometry = video.and_then(|stream| {
        let mut width = u32::try_from(stream["width"].as_u64()?).ok()?;
        let mut height = u32::try_from(stream["height"].as_u64()?).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        if let Some((num, den)) = stream["sample_aspect_ratio"]
            .as_str()
            .and_then(|ratio| ratio.split_once(':'))
            .and_then(|(num, den)| Some((num.parse::<u32>().ok()?, den.parse::<u32>().ok()?)))
            .filter(|(num, den)| *num != 0 && *den != 0)
        {
            width = (u64::from(width) * u64::from(num) / u64::from(den))
                .clamp(1, u64::from(u32::MAX)) as u32;
        }
        let rotation = stream["side_data_list"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|side| side["rotation"].as_i64())
            .unwrap_or_default();
        if rotation.rem_euclid(180) == 90 {
            std::mem::swap(&mut width, &mut height);
        }
        let fps = stream["avg_frame_rate"]
            .as_str()
            .and_then(|rate| rate.split_once('/'))
            .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?))
            .filter(|rate| rate.is_finite())
            .map_or(FPS as u16, |rate| rate.round().clamp(1.0, 240.0) as u16);
        Some(VideoGeometry { width, height, fps })
    });
    asset.preview_seek = video.is_some_and(|video| {
        let rate = video["r_frame_rate"].as_str();
        matches!(
            rate,
            Some("15/1" | "24/1" | "25/1" | "30/1" | "48/1" | "50/1" | "60/1" | "120/1")
        ) && video["avg_frame_rate"].as_str() == rate
            && seconds(&video["start_time"]) == Some(0.0)
    }) && seconds(&data["format"]["start_time"]) == Some(0.0)
        && audio.is_none_or(|audio| seconds(&audio["start_time"]) == Some(0.0));
    if let (Some(video), Some(audio)) = (video, audio) {
        let video_start = seconds(&video["start_time"]).unwrap_or_default();
        let audio_start = seconds(&audio["start_time"]).unwrap_or_default();
        asset.audio_offset_samples =
            ((audio_start - video_start) * f64::from(SAMPLE_RATE)).round() as i64;
    }
    Ok(asset)
}

pub fn render(
    project: &Project,
    destination: &Path,
    format: Format,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    render_with(project, destination, Output::new(format), cancel, progress)
}

pub fn render_with(
    project: &Project,
    destination: &Path,
    settings: Output,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    render_range(
        project,
        destination,
        settings,
        0..project.duration(),
        None,
        RenderPass::Timeline,
        cancel,
        progress,
    )
}

/// Render the audible portion of a timeline range as 16 kHz mono PCM for transcription.
pub fn render_audio_mix(
    project: &Project,
    destination: &Path,
    range: Range<u32>,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    render_range(
        project,
        destination,
        Output::new(Format::Preview),
        range,
        None,
        RenderPass::Audio {
            sample_rate: 16_000,
            channels: 1,
        },
        cancel,
        progress,
    )
}

/// Export the full timeline mix as 48 kHz stereo PCM WAV.
pub fn render_audio_master(
    project: &Project,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    render_range(
        project,
        destination,
        Output::new(Format::Preview),
        0..project.duration(),
        None,
        RenderPass::Audio {
            sample_rate: SAMPLE_RATE,
            channels: 2,
        },
        cancel,
        progress,
    )
}

pub fn render_preview(
    project: &Project,
    destination: &Path,
    range: Range<u32>,
    cancel: &AtomicBool,
) -> Result<()> {
    render_preview_with(project, destination, range, PreviewQuality::Auto, cancel)
}

pub fn render_preview_with(
    project: &Project,
    destination: &Path,
    range: Range<u32>,
    quality: PreviewQuality,
    cancel: &AtomicBool,
) -> Result<()> {
    render_range(
        project,
        destination,
        Output::new(Format::Preview),
        range,
        Some(quality),
        RenderPass::Timeline,
        cancel,
        |_| {},
    )
}

pub fn render_preview_frame(project: &Project, frame: u32, cancel: &AtomicBool) -> Result<Vec<u8>> {
    preview_frame(project, frame, None, cancel)
}

pub fn render_preview_frame_with(
    project: &Project,
    frame: u32,
    quality: PreviewQuality,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    preview_frame(project, frame, Some(quality), cancel)
}

pub fn export_frame_png(
    project: &Project,
    destination: &Path,
    frame: u32,
    cancel: &AtomicBool,
) -> Result<()> {
    render_range(
        project,
        destination,
        Output::new(Format::Preview),
        still_range(frame)?,
        Some(PreviewQuality::Full),
        RenderPass::Png,
        cancel,
        |_| {},
    )
}

fn still_range(frame: u32) -> Result<Range<u32>> {
    Ok(frame
        ..frame
            .checked_add(1)
            .context("Frame is outside the timeline.")?)
}

fn preview_frame(
    project: &Project,
    frame: u32,
    quality: Option<PreviewQuality>,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("frame.rgb");
    render_range(
        project,
        &path,
        Output::new(Format::Preview),
        still_range(frame)?,
        quality,
        RenderPass::Still,
        cancel,
        |_| {},
    )?;
    let [w, h] = quality.map_or_else(
        || Format::Preview.size(project),
        |quality| quality.size(project, true),
    );
    let pixels = std::fs::read(path)?;
    ensure!(
        pixels.len() == w * h * 3,
        "Preview frame dimensions do not match."
    );
    Ok(pixels)
}

fn composition_start(project: &Project, mut start: u32) -> u32 {
    if start == 0 {
        return 0;
    }
    let cuts: Vec<_> = project
        .tracks
        .iter()
        .filter(|track| !track.muted && track.kind == TrackKind::Video)
        .flat_map(|track| {
            let mut ordered: Vec<_> = track.clips.iter().collect();
            ordered.sort_unstable_by_key(|clip| clip.start);
            ordered
                .windows(2)
                .filter_map(|pair| {
                    let (prior, clip) = (pair[0], pair[1]);
                    let len = clip.transition?.frames.min(clip.length);
                    (len > 1 && prior.end() == clip.start).then_some(clip.start..clip.start + len)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // A transition samples the previous composited frame, which may contain another transition.
    while let Some(earlier) = cuts
        .iter()
        .filter(|cut| cut.contains(&start))
        .map(|cut| cut.start - 1)
        .min()
    {
        start = earlier;
    }
    start
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderPass {
    Timeline,
    Still,
    Png,
    Audio { sample_rate: u32, channels: u8 },
}

impl RenderPass {
    fn is_still(self) -> bool {
        matches!(self, Self::Still | Self::Png)
    }
}

fn audible(clip: &Clip, asset: &Asset) -> bool {
    !clip.freeze && asset.audio && clip.gain > 0.0
}

fn render_range(
    project: &Project,
    destination: &Path,
    settings: Output,
    range: Range<u32>,
    preview: Option<PreviewQuality>,
    pass: RenderPass,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    let format = settings.format;
    let still = pass.is_still();
    let audio_only = matches!(pass, RenderPass::Audio { .. });
    ensure!(
        matches!(settings.fps, 24 | 30 | 60 | 120),
        "Choose 24, 30, 60, or 120 fps."
    );
    check_cancel(cancel)?;
    project.validate()?;
    ensure!(
        project.duration() > 0,
        "Add a clip to the timeline before rendering."
    );
    ensure!(
        range.start < range.end && range.end <= project.duration(),
        "Preview is outside the timeline."
    );
    let [width, height] = preview.map_or_else(
        || settings.size(project),
        |quality| quality.size(project, still),
    );
    let full_preview = preview.is_some_and(|quality| {
        quality == PreviewQuality::Full || quality == PreviewQuality::Auto && still
    });
    let raster = Raster {
        width,
        height,
        rate: settings.fps.max(FPS),
        full_chroma: format == Format::Mkv || settings.quality == Quality::High || full_preview,
    };
    let origin = if audio_only {
        range.start
    } else {
        composition_start(project, range.start)
    };
    let frames = range.end - range.start;
    let destination = std::path::absolute(destination)?;
    ensure!(
        project
            .assets
            .iter()
            .filter_map(|asset| asset.path.as_deref())
            .all(|path| !same_path(path, &destination)),
        "Choose an output separate from the original media."
    );
    let parent = destination
        .parent()
        .context("Choose a local output file.")?;
    let folder = tempfile::tempdir_in(parent)?;
    let mut clips = Vec::new();
    let mut inputs = Vec::new();
    let mut staged_fonts = HashMap::new();
    for track in project
        .tracks
        .iter()
        .filter(|track| !track.muted && (!still || track.kind == TrackKind::Video))
    {
        let mut ordered: Vec<_> = track.clips.iter().collect();
        ordered.sort_unstable_by_key(|clip| clip.start);
        let mut previous_end = None;
        for clip in ordered {
            let adjacent = previous_end == Some(clip.start);
            previous_end = Some(clip.end());
            let first = clip.start.max(origin);
            let last = clip.end().min(range.end);
            if first >= last {
                continue;
            }
            let skip = first - clip.start;
            let asset = project
                .asset(&clip.asset_id)
                .context("Clip media is missing.")?;
            if audio_only && !audible(clip, asset) {
                continue;
            }
            let mut font = None;
            let mut seek = 0;
            let input = if let Some(title) = &asset.title {
                std::fs::write(
                    folder.path().join(format!("title{}.txt", clips.len())),
                    &title.text,
                )?;
                if let Some(path) = title.appearance.font.as_ref().and_then(fonts::resolve) {
                    if !staged_fonts.contains_key(&path) && path.is_file() {
                        let filename = format!("custom-{}.ttf", staged_fonts.len());
                        std::fs::copy(&path, folder.path().join(&filename)).with_context(|| {
                            format!("Could not read the selected font {}", path.display())
                        })?;
                        staged_fonts.insert(path.clone(), filename);
                    }
                    font = staged_fonts.get(&path).cloned();
                }
                None
            } else {
                let path = asset
                    .path
                    .as_ref()
                    .filter(|path| !clip.slot && path.is_file())
                    .with_context(|| format!("Replace or relink media for {}.", asset.label))?;
                if !audio_only
                    && format == Format::Preview
                    && asset.preview_seek
                    && asset.kind == AssetKind::Video
                    && !clip
                        .effects
                        .iter()
                        .any(|effect| effect.enabled && effect.amount != 0)
                {
                    let first_frame = if clip.freeze {
                        clip.source
                    } else {
                        clip.source + skip
                    };
                    seek = first_frame.saturating_sub(FPS) / FPS * FPS;
                }
                let index = inputs.len();
                inputs.push((std::path::absolute(path)?, seek));
                Some(index)
            };
            let transition = clip
                .transition
                .filter(|_| adjacent && clip.start > origin && skip == 0);
            let mask_input = if let Some(mask) = clip
                .mask
                .filter(|_| !audio_only && track.kind == TrackKind::Video)
            {
                let index = inputs.len();
                let path = folder.path().join(format!("mask{index}.pgm"));
                stage_mask(&path, mask, raster, cancel)?;
                inputs.push((path, 0));
                Some(index)
            } else {
                None
            };
            clips.push(Layer {
                kind: track.kind,
                clip,
                asset,
                input,
                mask_input,
                font,
                seek,
                start: first - origin,
                skip,
                length: last - first,
                transition,
            });
        }
    }
    if clips.iter().any(|layer| layer.asset.title.is_some()) {
        std::fs::write(
            folder.path().join("cutad-font.ttf"),
            epaint_default_fonts::UBUNTU_LIGHT,
        )?;
        std::fs::write(
            folder.path().join("cutad-bold.ttf"),
            include_bytes!("../assets/DejaVuSans-Bold.ttf"),
        )?;
    }
    let graph = folder.path().join("filters.txt");
    let skipped = range.start - origin;
    std::fs::write(
        &graph,
        filter_graph(raster, &clips, range.end - origin, skipped, pass),
    )?;
    let suffix = match pass {
        RenderPass::Timeline => format.extension(),
        RenderPass::Still => "rgb",
        RenderPass::Png => "png",
        RenderPass::Audio { .. } => "wav",
    };
    let output = tempfile::Builder::new()
        .suffix(&format!(".{suffix}"))
        .tempfile_in(parent)?
        .into_temp_path();
    let errors = folder.path().join("stderr");
    let status = folder.path().join("progress");
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg.current_dir(folder.path());
    ffmpeg.args([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
        "-filter_complex_threads",
        "2",
    ]);
    for (path, seek) in &inputs {
        if *seek != 0 {
            ffmpeg.args(["-ss", &seconds(*seek)]);
        }
        ffmpeg
            .args([
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                DEMUXERS,
                "-threads",
                "2",
                "-i",
            ])
            .arg(path);
    }
    ffmpeg.arg("-/filter_complex").arg(graph);
    if !audio_only {
        ffmpeg.args(["-map", "[video]"]);
    }
    if let RenderPass::Audio {
        sample_rate,
        channels,
    } = pass
    {
        ffmpeg.args([
            "-map",
            "[audio]",
            "-vn",
            "-c:a",
            "pcm_s16le",
            "-ac",
            &channels.to_string(),
            "-ar",
            &sample_rate.to_string(),
        ]);
    } else if still {
        ffmpeg.args([
            "-frames:v",
            "1",
            "-an",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
        ]);
        if pass == RenderPass::Png {
            ffmpeg.args(["-c:v", "png", "-f", "image2", "-update", "1"]);
        } else {
            ffmpeg.args(["-f", "rawvideo"]);
        }
    } else {
        ffmpeg
            .args([
                "-map",
                "[audio]",
                "-t",
                &seconds(frames),
                "-threads",
                "4",
                "-r",
                &settings.fps.to_string(),
            ])
            .args(format.codecs(settings.quality, full_preview));
    }
    ffmpeg
        .arg(output.as_os_str())
        .stdout(File::create(&status)?)
        .stderr(File::create(&errors)?);
    let mut child = ffmpeg.spawn()?;
    let poll = if format == Format::Preview { 12 } else { 80 };
    let result = wait_for_render(
        &mut child, &status, &errors, frames, cancel, &progress, poll,
    );
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    check_cancel(cancel)?;
    output
        .persist(&destination)
        .context("Could not replace the destination file.")?;
    progress(100);
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Render canceled.");
    Ok(())
}

fn wait_for_render(
    child: &mut Child,
    status: &Path,
    errors: &Path,
    duration: u32,
    cancel: &AtomicBool,
    progress: &impl Fn(u8),
    poll_ms: u64,
) -> Result<()> {
    let mut report = BufReader::new(File::open(status)?);
    let mut line = String::new();
    let mut pending = String::new();
    loop {
        check_cancel(cancel)?;
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let message = read_to_string(errors).unwrap_or_default();
                let tail = message.lines().rev().take(8).collect::<Vec<_>>();
                let summary = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                bail!("FFmpeg could not render the edit. {summary}");
            }
            return Ok(());
        }
        while report.read_line(&mut line)? != 0 {
            pending.push_str(&line);
            if pending.ends_with('\n') {
                if let Some(microseconds) = pending
                    .trim()
                    .strip_prefix("out_time_us=")
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    let percent = microseconds.saturating_mul(u64::from(FPS) * 100)
                        / (u64::from(duration) * 1_000_000);
                    progress(percent.min(99) as u8);
                }
                pending.clear();
            }
            line.clear();
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }
}

fn filter_graph(
    raster: Raster,
    clips: &[Layer<'_>],
    frames: u32,
    skip: u32,
    pass: RenderPass,
) -> String {
    let still = pass.is_still();
    let audio_only = matches!(pass, RenderPass::Audio { .. });
    let Raster {
        width,
        height,
        rate,
        ..
    } = raster;
    let factor = rate / FPS;
    let alpha = raster.alpha();
    let opaque = raster.opaque();
    let samples = i64::from(frames) * SAMPLES_PER_FRAME;
    let mut graph = Vec::new();
    if !audio_only {
        graph.push(format!(
            "color=c=0x111319:s={width}x{height}:r={rate}:d={},format={opaque}[base]",
            seconds(frames)
        ));
    }
    let mut video = "base".to_owned();
    let mut audio = vec!["[silence]".to_owned()];
    if !still {
        graph.push(format!(
            "anullsrc=r={SAMPLE_RATE}:cl=stereo,atrim=end_sample={samples}[silence]"
        ));
    }
    for (index, layer) in clips.iter().enumerate() {
        let (clip, asset) = (layer.clip, layer.asset);
        let effects = |audio| {
            let rate = if audio { FPS } else { rate };
            clip.effects
                .iter()
                .filter(|e| e.enabled && e.amount > 0 && e.preset.audio() == audio)
                .map(|e| {
                    format!(
                        ",{}",
                        e.preset.filter_at_rate(
                            e.amount,
                            clip.length * (rate / FPS),
                            [width, height],
                            rate
                        )
                    )
                })
                .collect::<String>()
        };
        if !audio_only && layer.kind == TrackKind::Video {
            let video_fx = effects(false);
            let history = if video_fx.is_empty() { 0 } else { layer.skip };
            let source_offset = layer.skip - history;
            let needed = layer.length + history;
            let mut source = if let Some(title) = &asset.title {
                title_source(
                    index,
                    title,
                    raster,
                    needed,
                    source_offset,
                    layer.font.as_deref(),
                )
            } else {
                let input = layer.input.expect("media input");
                let timing = if clip.freeze {
                    let start = (clip.source - layer.seek) * factor;
                    let end = start + 1;
                    let frames = needed * factor - 1;
                    format!(
                        "tpad=stop_mode=clone:stop=1,trim=start_frame={start}:end_frame={end},\
                             settb=1/{rate},setpts=N,tpad=stop_mode=clone:stop={frames}"
                    )
                } else {
                    let offset = if asset.kind == AssetKind::Image {
                        0
                    } else {
                        clip.source + source_offset - layer.seek
                    };
                    let pad = if asset.kind == AssetKind::Image {
                        needed
                    } else {
                        1
                    };
                    let end = (offset + needed) * factor;
                    let offset = offset * factor;
                    format!(
                        "tpad=stop_mode=clone:stop_duration={},trim=start_frame={offset}:end_frame={end}",
                        seconds(pad)
                    )
                };
                format!(
                    "[{input}:V:0]setpts=PTS-STARTPTS,fps={rate},{timing},settb=1/{rate},setpts=N,\
                         scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2:reset_sar=1:flags=lanczos,\
                         setsar=1,format={alpha},pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black@0"
                )
            };
            if let Some(key) = clip.chroma_key {
                let color = ink(key.color);
                let threshold = (f64::from(key.tolerance) / 100.0).max(0.00001);
                let softness = f64::from(key.softness) / 100.0;
                graph.push(format!(
                    "{source},format=rgba,split=3[keybase{index}][keysource{index}][keyalpha{index}];\n\
                    [keysource{index}]colorkey={color}:{threshold:.5}:{softness:.5},alphaextract[keymask{index}];\n\
                    [keyalpha{index}]alphaextract[originalalpha{index}];\n\
                    [originalalpha{index}][keymask{index}]blend=all_mode=multiply[combinedalpha{index}];\n\
                    [keybase{index}][combinedalpha{index}]alphamerge[keyed{index}]"
                ));
                source = format!("[keyed{index}]null");
            }
            let grade = clip.adjustment;
            if grade != ColorAdjustment::default() {
                let brightness = f64::from(grade.brightness) / 300.0;
                let contrast = 1.0 + f64::from(grade.contrast) / 100.0;
                let saturation = 1.0 + f64::from(grade.saturation) / 100.0;
                source.push_str(&format!(
                    ",format={alpha},eq=brightness={brightness:.3}:contrast={contrast:.3}:saturation={saturation:.3}"
                ));
            }
            source.push_str(&video_fx);
            if history != 0 {
                let history = history * factor;
                let needed = needed * factor;
                source.push_str(&format!(
                    ",trim=start_frame={history}:end_frame={needed},settb=1/{rate},setpts=N"
                ));
            }
            if let Some(mask) = layer.mask_input {
                graph.push(format!(
                    "{source},format={alpha},settb=1/{rate},setpts=N,split[maskbase{index}][masksource{index}];\n\
                    [masksource{index}]alphaextract[contentalpha{index}];\n\
                    [{mask}:v:0]format=gray,settb=1/{rate},setpts=0[maskalpha{index}];\n\
                    [contentalpha{index}][maskalpha{index}]blend=all_mode=multiply:repeatlast=1[outputalpha{index}];\n\
                    [maskbase{index}][outputalpha{index}]alphamerge[masked{index}]"
                ));
                source = format!("[masked{index}]null");
            }
            let start = layer.start * factor;
            let end = start + layer.length * factor;
            if clip.transform == Transform::default() && clip.position_animation.is_empty() {
                graph.push(format!("{source},setsar=1,format={alpha},settb=1/{rate},setpts=N+{start}[visual{index}]"));
            } else {
                transformed_layer(&mut graph, &source, layer, index, raster);
            }
            if let Some(transition) = layer.transition.filter(|_| clip.length > 1) {
                video = transition_layer(&mut graph, &video, index, layer, transition, raster);
            } else {
                graph.push(overlay(
                    &video,
                    &format!("visual{index}"),
                    &format!("layer{index}"),
                    start,
                    end,
                    raster,
                ));
                video = format!("layer{index}");
            }
        }
        if !still
            && let Some(input) = layer.input
            && audible(clip, asset)
        {
            let source_start =
                i64::from(clip.source) * SAMPLES_PER_FRAME - asset.audio_offset_samples;
            let window_skip = i64::from(layer.skip) * SAMPLES_PER_FRAME;
            let end = source_start + window_skip + i64::from(layer.length) * SAMPLES_PER_FRAME;
            if end > 0 {
                let fx = effects(true);
                let seek = i64::from(layer.seek) * SAMPLES_PER_FRAME;
                let from = if fx.is_empty() {
                    source_start + window_skip
                } else {
                    source_start
                };
                let before = (source_start + window_skip).min(0);
                let delay = i64::from(layer.start) * SAMPLES_PER_FRAME - before;
                let post_trim = (window_skip + source_start.min(0)).max(0);
                let final_trim = if fx.is_empty() || post_trim == 0 {
                    String::new()
                } else {
                    format!(",atrim=start_sample={post_trim},asetpts=PTS-STARTPTS")
                };
                let fade = audio_fade_filter(clip, from.max(0) - source_start, end - source_start);
                graph.push(format!("[{input}:a:0]aresample={SAMPLE_RATE},atrim=start_sample={}:end_sample={},\
                    asetpts=PTS-STARTPTS,aformat=channel_layouts=stereo,volume={:.5}{fx}{fade}{final_trim},adelay={delay}S:all=1[sound{index}]",
                    from.max(0) - seek, end - seek, clip.gain));
                audio.push(format!("[sound{index}]"));
            }
        }
    }
    if !audio_only {
        graph.push(format!(
            "[{video}]trim=start_frame={},settb=1/{rate},setpts=N,format={opaque}[video]",
            skip * factor
        ));
    }
    if !still {
        let skip_samples = i64::from(skip) * SAMPLES_PER_FRAME;
        graph.push(format!("{}amix=inputs={}:duration=first:normalize=0,\
            alimiter=limit=0.95:level=0:latency=1,atrim=start_sample={skip_samples}:end_sample={samples},asetpts=PTS-STARTPTS[audio]",
            audio.concat(), audio.len()));
    }
    graph.join(
        ";
",
    )
}

fn audio_fade_filter(clip: &Clip, first_sample: i64, end_sample: i64) -> String {
    let fade = clip.audio_fade;
    let (fade_in, fade_out) = fade.anchors(clip.length);
    let mut gain = String::new();
    let in_samples = i64::from(fade.in_frames) * SAMPLES_PER_FRAME;
    let in_at = fade_in * SAMPLES_PER_FRAME;
    if in_samples > 0 && first_sample < in_at + in_samples {
        let offset = first_sample - in_at;
        gain.push_str(&format!("*clip((n+({offset}))/{in_samples},0,1)"));
    }
    let out_samples = i64::from(fade.out_frames) * SAMPLES_PER_FRAME;
    let out_at = fade_out * SAMPLES_PER_FRAME;
    if out_samples > 0 && end_sample > out_at - out_samples {
        let offset = out_at - first_sample;
        gain.push_str(&format!("*clip((({offset})-n)/{out_samples},0,1)"));
    }
    if gain.is_empty() {
        String::new()
    } else {
        format!(",aeval='val(ch){gain}':c=same")
    }
}

fn ink(rgb: [u8; 3]) -> String {
    format!("0x{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn title_source(
    index: usize,
    title: &Title,
    raster: Raster,
    duration: u32,
    offset: u32,
    custom_font: Option<&str>,
) -> String {
    let Raster {
        width: w,
        height: h,
        rate,
        ..
    } = raster;
    let look = &title.appearance;
    let short = w.min(h);
    let y;
    let (base_size, x, fill, outline, shadow) = match title.style {
        TitleStyle::Headline => {
            y = "(h-th)/2";
            (short / 10, "(w-tw)/2", "white", (0, "black"), "")
        }
        TitleStyle::LowerThird => {
            y = "h*0.77";
            (short / 16, "w*0.09", "white", (0, "black"), "")
        }
        TitleStyle::Caption => {
            y = "h-th-h*0.11";
            (short / 18, "(w-tw)/2", "white", (0, "black"), "")
        }
        TitleStyle::Impact => {
            y = "h-th-h*0.12+h*0.13*exp(-18*t)";
            (
                short * 13 / 100,
                "(w-tw)/2",
                "white",
                (10, "0x101018"),
                ":shadowx=2:shadowy=5:shadowcolor=0x100b16",
            )
        }
        TitleStyle::Neon => {
            y = "(h-th)/2";
            (
                short * 12 / 100,
                "(w-tw)/2",
                "0xffebff",
                (2, "0xe47fff"),
                "",
            )
        }
        TitleStyle::Glitch => {
            y = "h-th-h*0.14";
            (
                short * 12 / 100,
                "(w-tw)/2",
                "white",
                (0, "black"),
                ":shadowx=1:shadowy=2:shadowcolor=black",
            )
        }
        TitleStyle::Pop => {
            y = "h-th-h*0.15-h*0.24*sin(15*t)*exp(-10*t)";
            (
                short * 13 / 100,
                "(w-tw)/2",
                "0xffe868",
                (10, "0x312249"),
                ":shadowx=3:shadowy=5:shadowcolor=0x6e3286",
            )
        }
    };
    let canvas = format!(
        "color=c=black@0:s={w}x{h}:r={rate}:d={},format=rgba,settb=1/{rate},setpts=N+{}",
        seconds(duration),
        offset * (rate / FPS)
    );
    let bold = matches!(
        title.style,
        TitleStyle::Impact | TitleStyle::Neon | TitleStyle::Glitch | TitleStyle::Pop
    );
    let font = custom_font.unwrap_or(if bold {
        "cutad-bold.ttf"
    } else {
        "cutad-font.ttf"
    });
    let x = match look.align {
        Some(TextAlign::Left) => "w*0.075",
        Some(TextAlign::Center) => "(w-tw)/2",
        Some(TextAlign::Right) => "w-tw-w*0.075",
        None => x,
    };
    let text_align = match look.align {
        Some(TextAlign::Center) => "C",
        Some(TextAlign::Right) => "R",
        _ => "L",
    };
    let size = (base_size.max(11) * usize::from(look.size_percent) / 100).max(4);
    let gap = (i32::from(look.line_spacing) * size as i32 / 100).max(-((size / 2) as i32));
    let draw = |color: &str, x: &str, y: &str, options: &str| {
        format!(
            "drawtext=fontfile={font}:textfile=title{index}.txt:expansion=none:fontsize={size}:fontcolor={color}:x='{x}':y='{y}':text_align={text_align}:line_spacing={gap}{options}"
        )
    };
    let border = |width: u8, color: &str| {
        if width == 0 {
            String::new()
        } else {
            format!(
                ":borderw={}:bordercolor={color}",
                (size * usize::from(width) / 100).max(1)
            )
        }
    };
    let border = if let Some(custom) = look.outline {
        border(custom.width, &ink(custom.color))
    } else {
        border(outline.0, outline.1)
    };
    let pad = (size / 4).max(2);
    let backdrop = match look.background {
        Some(custom) if custom.opacity == 0 => String::new(),
        Some(custom) => format!(
            ":box=1:boxcolor={}@{:.3}:boxborderw={pad}",
            ink(custom.color),
            f64::from(custom.opacity) / 100.0
        ),
        None => String::new(),
    };
    let mut passes = vec![canvas];
    if title.style == TitleStyle::Neon {
        passes.push(draw(
            "0xf58bff@0.7",
            x,
            y,
            &format!(
                ":borderw={}:bordercolor=0xa64bff@0.6:alpha='0.70+0.30*sin(9*t)*sin(9*t)'",
                (size / 5).max(2)
            ),
        ));
    }
    if title.style == TitleStyle::Glitch {
        passes.push(draw(
            "0x37ecff",
            &format!("{x}-w*0.014*(1+sin(71*t))"),
            &format!("{y}-3*sin(29*t)"),
            "",
        ));
        passes.push(draw(
            "0xff42b8",
            &format!("{x}+w*0.014*(1+sin(57*t))"),
            &format!("{y}+3*sin(31*t)"),
            "",
        ));
    }
    let fill = look.fill.map(ink).unwrap_or_else(|| fill.into());
    passes.push(draw(&fill, x, y, &format!("{border}{shadow}{backdrop}")));
    passes.join(",")
}

fn position_expression(layer: &Layer<'_>, axis: usize, dimension: usize) -> String {
    let clip = layer.clip;
    let keys = &clip.position_animation.keys;
    let pixel = |position: f32| f64::from(position) * dimension as f64;
    let Some(last) = keys.last() else {
        return format!("{:.0}", pixel([clip.transform.x, clip.transform.y][axis]));
    };
    let tick = format!(
        "(t*{FPS}+{})",
        i64::from(layer.skip) + i64::from(clip.position_animation.offset)
    );
    let mut expression = format!("{:.6}", pixel(last.position[axis]));
    for pair in keys.windows(2).rev() {
        let (a, b) = (pair[0], pair[1]);
        let start = pixel(a.position[axis]);
        let difference = pixel(b.position[axis]) - start;
        let span = b.frame - a.frame;
        expression = format!(
            "if(lt({tick},{}),{start:.6}+({difference:.6})*max(0,({tick}-({}))/{span}),{expression})",
            b.frame, a.frame
        );
    }
    format!("round({expression})")
}

fn transformed_layer(
    graph: &mut Vec<String>,
    source: &str,
    layer: &Layer<'_>,
    index: usize,
    raster: Raster,
) {
    let Raster {
        width: w,
        height: h,
        rate,
        ..
    } = raster;
    let alpha = raster.alpha();
    let transform = layer.clip.transform;
    let scale = transform.scale;
    let angle = f64::from(transform.rotation).to_radians();
    let x = position_expression(layer, 0, w);
    let y = position_expression(layer, 1, h);
    let width = (((w as f32 * scale).round() as usize) & !1).max(2);
    let height = (((h as f32 * scale).round() as usize) & !1).max(2);
    let opacity = transform.opacity;
    graph.push(format!("{source},format=rgba,scale={width}:{height}:flags=lanczos,\
        rotate={angle:.7}:ow='ceil(rotw({angle:.7})/2)*2':oh='ceil(roth({angle:.7})/2)*2':c=black@0,\
        colorchannelmixer=aa={opacity:.5},setsar=1,format={alpha},settb=1/{rate},setpts=N[shape{index}]"));
    graph.push(format!(
        "color=c=black@0:s={w}x{h}:r={rate}:d={},format={alpha}[clear{index}]",
        seconds(layer.length)
    ));
    graph.push(format!("[clear{index}][shape{index}]overlay=x='(W-w)/2+({x})':y='(H-h)/2+({y})':\
        eof_action=pass:repeatlast=0:format=auto,format={alpha},settb=1/{rate},setpts=N+{}[visual{index}]", layer.start * (rate / FPS)));
}

fn overlay(
    background: &str,
    foreground: &str,
    output: &str,
    start: u32,
    end: u32,
    raster: Raster,
) -> String {
    let chroma = raster.overlay();
    format!(
        "[{background}][{foreground}]overlay=eof_action=pass:repeatlast=0:format={chroma}:\
             enable='gte(n,{start})*lt(n,{end})'[{output}]"
    )
}

fn transition_layer(
    graph: &mut Vec<String>,
    background: &str,
    index: usize,
    layer: &Layer<'_>,
    t: Transition,
    raster: Raster,
) -> String {
    let rate = raster.rate;
    let factor = rate / FPS;
    let start = layer.start * factor;
    let duration = t.frames.min(layer.clip.length);
    let frames = duration * factor;
    let start_tail = start - 1;
    let cross_length = frames + 1;
    graph.extend([
        format!("[{background}]split=2[base{index}][previous{index}]"),
        format!("[previous{index}]trim=start_frame={start_tail}:end_frame={start},\
                 settb=1/{rate},setpts=N,format=yuv444p,tpad=stop_mode=clone:stop={frames}[from{index}]"),
        format!("[visual{index}]split=2[current{index}][incoming{index}]"),
        format!("[incoming{index}]trim=end_frame={cross_length},settb=1/{rate},\
                 setpts=N,format=yuv444p,tpad=stop_mode=clone:stop=1[to{index}]"),
        format!("[from{index}][to{index}]xfade=transition={}:duration={}:offset=0,\
                 trim=end_frame={frames},settb=1/{rate},setpts=N+{start}[blend{index}]", t.kind.filter(), seconds(duration)),
        overlay(&format!("base{index}"), &format!("current{index}"), &format!("full{index}"), start, start + layer.length * factor, raster),
        overlay(&format!("full{index}"), &format!("blend{index}"), &format!("layer{index}"), start, start + frames, raster),
    ]);
    format!("layer{index}")
}

pub fn monitor_frame(path: &Path, frame: u32, size: [usize; 2]) -> Result<Vec<u8>> {
    let result = command("ffmpeg")?
        .args([
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "2",
            "-ss",
            &seconds(frame),
            "-i",
        ])
        .arg(path)
        .args([
            "-an",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
            "-",
        ])
        .output()?;
    ensure!(
        result.status.success() && result.stdout.len() == size[0] * size[1] * 3,
        "Could not read this preview frame."
    );
    Ok(result.stdout)
}

pub fn preview_frames(path: &Path, frame: u32) -> Result<Child> {
    command("ffmpeg")?
        .args([
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "2",
            "-ss",
            &seconds(frame),
            "-i",
        ])
        .arg(path)
        .args([
            "-an", "-f", "rawvideo", "-pix_fmt", "rgb24", "-threads", "1", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not decode preview frames.")
}

pub const THUMBNAIL_SIZE: [usize; 2] = [144, 81];

pub fn source_thumbnail(path: &Path, frame: u32) -> Result<Vec<u8>> {
    let result = command("ffmpeg")?
        .args([
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-ss",
            &seconds(frame),
            "-i",
        ])
        .arg(std::path::absolute(path)?)
        .args([
            "-map",
            "0:V:0",
            "-an",
            "-frames:v",
            "1",
            "-vf",
            "scale=144:81:force_original_aspect_ratio=increase,crop=144:81,setsar=1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
            "-",
        ])
        .output()?;
    ensure!(
        result.status.success()
            && result.stdout.len() == THUMBNAIL_SIZE.iter().product::<usize>() * 3,
        "No readable video thumbnail."
    );
    Ok(result.stdout)
}
