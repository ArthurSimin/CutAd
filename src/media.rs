//! Local media probing and a single compositing path for preview and export.

use crate::effects::Transition;
use crate::fonts;
use crate::project::{
    Asset, AssetKind, Clip, ClipMask, ColorAdjustment, FPS, MAX_FRAMES, MaskShape, MaskStroke,
    Project, TextAlign, Title, TitleStyle, TrackKind, Transform, VideoGeometry, same_path, seconds,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{File, read_to_string};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
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
    Gif,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Webm => "webm",
            Self::Mkv => "mkv",
            Self::Gif => "gif",
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
            Self::Gif => vec!["-c:v", "gif", "-loop", "0"],
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportDetails {
    pub title: String,
    pub cover_frame: Option<u32>,
}

impl Output {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            quality: Quality::Balanced,
            short_edge: (format == Format::Gif).then_some(480),
            fps: if format == Format::Gif { 15 } else { FPS },
        }
    }

    pub fn for_project(format: Format, project: &Project) -> Self {
        let mut output = Self::new(format);
        output.quality = Quality::High;
        if format != Format::Gif
            && let Some(source) = project.source_video()
        {
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
    video_input: Option<usize>,
    mask_input: Option<usize>,
    font: Option<String>,
    lut: Option<String>,
    video_seek: u32,
    audio_seek: u32,
    start: u32,
    skip: u32,
    history: u32,
    length: u32,
    transition: Option<Transition>,
}

fn paint_shape(
    pixels: &mut [u8],
    raster: Raster,
    mask: ClipMask,
    cancel: &AtomicBool,
) -> Result<()> {
    let (w, h) = (raster.width as f64, raster.height as f64);
    let half_width = w * f64::from(mask.width) * 0.5;
    let half_height = h * f64::from(mask.height) * 0.5;
    let (cx, cy) = (w * f64::from(mask.x), h * f64::from(mask.y));
    let (sin, cos) = f64::from(mask.rotation).to_radians().sin_cos();
    let feather = w.min(h) * f64::from(mask.feather);
    for (y, row) in pixels.chunks_exact_mut(raster.width).enumerate() {
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
    }
    Ok(())
}

fn paint_stroke(
    pixels: &mut [u8],
    raster: Raster,
    stroke: &MaskStroke,
    cancel: &AtomicBool,
) -> Result<()> {
    let (width, height) = (raster.width, raster.height);
    let radius = stroke.radius * width.min(height) as f32;
    let bounds = |start: f32, end: f32, limit: usize| {
        let low = (start.min(end) - radius - 1.0).floor().max(0.0) as usize;
        let high = ((start.max(end) + radius + 1.0).ceil() as usize).min(limit);
        low..high
    };
    let points: Vec<_> = stroke
        .points
        .iter()
        .map(|[x, y]| [x * width as f32, y * height as f32])
        .collect();
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for [x, y] in &points {
        min = [min[0].min(*x), min[1].min(*y)];
        max = [max[0].max(*x), max[1].max(*y)];
    }
    let columns = bounds(min[0], max[0], width);
    let rows = bounds(min[1], max[1], height);
    let stride = columns.len();
    let mut coverage = vec![0; stride * rows.len()];
    let ends = points.iter().skip(1).chain(points.last());
    for ([x0, y0], [x1, y1]) in points.iter().zip(ends) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let length_squared = dx * dx + dy * dy;
        for y in bounds(*y0, *y1, height) {
            if y % 64 == 0 {
                check_cancel(cancel)?;
            }
            for x in bounds(*x0, *x1, width) {
                let index = (y - rows.start) * stride + x - columns.start;
                if coverage[index] == 255 {
                    continue;
                }
                let (px, py) = (x as f32 + 0.5 - x0, y as f32 + 0.5 - y0);
                let t = if length_squared > 0.0 {
                    ((px * dx + py * dy) / length_squared).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (px, py) = (px - t * dx, py - t * dy);
                let distance = (px * px + py * py).sqrt();
                let alpha = ((radius + 0.5 - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
                coverage[index] = coverage[index].max(alpha);
            }
        }
    }
    let target = if stroke.restore { 255 } else { 0 };
    for (row, brush_row) in rows.zip(coverage.chunks_exact(stride)) {
        let start = row * width + columns.start;
        for (alpha, brush) in pixels[start..start + stride].iter_mut().zip(brush_row) {
            let brush = u16::from(*brush);
            *alpha = ((u16::from(*alpha) * (255 - brush) + target * brush + 127) / 255) as u8;
        }
    }
    Ok(())
}

fn stage_mask(path: &Path, clip: &Clip, raster: Raster, cancel: &AtomicBool) -> Result<()> {
    let (width, height) = (raster.width, raster.height);
    let mut pixels = vec![255; width * height];
    if let Some(mask) = clip.mask {
        paint_shape(&mut pixels, raster, mask, cancel)?;
    }
    for stroke in &clip.mask_strokes {
        check_cancel(cancel)?;
        paint_stroke(&mut pixels, raster, stroke, cancel)?;
    }
    let mut output = BufWriter::new(File::create(path)?);
    write!(output, "P5\n{width} {height}\n255\n")?;
    output.write_all(&pixels)?;
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

pub(crate) fn command(name: &str) -> Result<Command> {
    let mut command = Command::new(executable(name)?);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    Ok(command)
}

/// Synthesize an editable audio asset from local text.
pub fn synthesize_narration(text: &str, voice: &str, destination: &Path) -> Result<Asset> {
    ensure!(
        !text.trim().is_empty() && text.len() <= 4096,
        "Choose a nonempty title up to 4096 bytes."
    );
    ensure!(!destination.exists(), "Choose a new WAV file.");
    let destination = std::path::absolute(destination)?;
    let parent = destination
        .parent()
        .context("Choose a folder for the narration.")?;
    let scratch = tempfile::tempdir_in(parent)?;
    let input = scratch.path().join("narration.txt");
    let audio = scratch.path().join("narration.wav");
    std::fs::write(&input, text)?;
    let output = command("espeak-ng")
        .context("Install eSpeak NG and put espeak-ng on PATH or beside CutAd.")?
        .args(["-b", "1", "-v", voice, "-f"])
        .arg(input)
        .arg("-w")
        .arg(&audio)
        .output()?;
    ensure!(
        output.status.success(),
        "eSpeak NG could not generate narration. {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut asset = probe(&audio)?;
    tempfile::TempPath::try_from_path(audio)?.persist_noclobber(&destination)?;
    asset.path = Some(destination.clone());
    asset.label = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    Ok(asset)
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
    render_timeline_section(
        project,
        destination,
        settings,
        0..project.duration(),
        cancel,
        progress,
    )
}

/// Export a half-open range of 30 fps timeline ticks using the normal delivery settings.
pub fn render_timeline_section(
    project: &Project,
    destination: &Path,
    settings: Output,
    range: Range<u32>,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    render_range(
        project,
        destination,
        settings,
        range,
        if settings.format == Format::Gif {
            RenderPass::Gif(settings.fps)
        } else {
            RenderPass::Timeline(None)
        },
        cancel,
        progress,
    )
}

/// Export delivery metadata and, for MP4, a thumbnail rendered from the timeline.
pub fn render_with_details(
    project: &Project,
    destination: &Path,
    settings: Output,
    range: Range<u32>,
    details: &ExportDetails,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    if details == &ExportDetails::default() {
        return render_timeline_section(project, destination, settings, range, cancel, progress);
    }
    ensure!(
        matches!(settings.format, Format::Mp4 | Format::Mkv | Format::Webm),
        "Choose MP4, Matroska, or WebM for title metadata."
    );
    ensure!(
        details.title.len() <= 2048 && !details.title.contains('\0'),
        "Choose a shorter title without control characters."
    );
    ensure!(
        details.cover_frame.is_none() || settings.format == Format::Mp4,
        "Embedded covers require MP4."
    );
    render_range(
        project,
        destination,
        settings,
        range,
        RenderPass::Delivery(details),
        cancel,
        progress,
    )
}

/// Detect hard cuts in source footage and return frame offsets within the supplied range.
pub fn detect_scene_cuts(
    path: &Path,
    source: u32,
    length: u32,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<Vec<u32>> {
    const MAX_CUTS: usize = 1_000;
    check_cancel(cancel)?;
    if length < 2 {
        return Ok(Vec::new());
    }
    let scratch = tempfile::tempdir()?;
    let cuts = scratch.path().join("cuts");
    let errors = scratch.path().join("errors");
    let status = scratch.path().join("progress");
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg
        .current_dir(scratch.path())
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-nostats", "-progress", "pipe:1"])
        .args(["-protocol_whitelist", "file", "-format_whitelist", DEMUXERS])
        .args(["-ss", &seconds(source), "-t", &seconds(length), "-i"])
        .arg(std::path::absolute(path)?)
        .args([
            "-map", "0:V:0", "-an", "-fps_mode", "passthrough",
            "-vf",
            &format!("setpts=PTS-STARTPTS,fps={FPS},trim=end_frame={length},scale=320:180:flags=area,\
                 format=yuv420p,scdet=threshold=10:sc_pass=1,metadata=mode=print:key=lavfi.scd.time:file=cuts"),
            "-frames:v", &(MAX_CUTS + 1).to_string(), "-f", "null", "-",
        ])
        .stdout(File::create(&status)?)
        .stderr(File::create(&errors)?);
    let mut child = ffmpeg.spawn()?;
    if let Err(error) = wait_for_render(&mut child, &status, &errors, length, cancel, &progress, 60)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    check_cancel(cancel)?;
    let raw = read_to_string(cuts)?;
    let mut offsets = raw
        .lines()
        .filter_map(|line| line.strip_prefix("lavfi.scd.time=")?.parse::<f64>().ok())
        .filter_map(|time| {
            let frame = (time * f64::from(FPS)).round() as u32;
            (frame > 0 && frame < length).then_some(frame)
        })
        .collect::<Vec<_>>();
    offsets.sort_unstable();
    offsets.dedup();
    ensure!(
        offsets.len() <= MAX_CUTS,
        "Too many cuts. Trim the clip and scan a shorter section."
    );
    progress(100);
    Ok(offsets)
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
        RenderPass::Audio {
            sample_rate: crate::beats::SAMPLE_RATE,
            channels: 1,
            format: AudioFormat::Wav,
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
    export_audio(project, destination, AudioFormat::Wav, cancel, progress)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Flac,
    Mp3,
}

impl AudioFormat {
    pub const ALL: [Self; 3] = [Self::Wav, Self::Flac, Self::Mp3];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Wav => "WAV · PCM",
            Self::Flac => "FLAC · Lossless",
            Self::Mp3 => "MP3 · 320 kbps",
        }
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let extension = path.extension().unwrap_or_default();
        Self::ALL
            .into_iter()
            .find(|format| extension.eq_ignore_ascii_case(format.extension()))
            .context("Choose a .wav, .flac, or .mp3 file.")
    }

    fn codec(self) -> &'static [&'static str] {
        match self {
            Self::Wav => &["-c:a", "pcm_s16le"],
            Self::Flac => &[
                "-c:a",
                "flac",
                "-sample_fmt",
                "s32",
                "-bits_per_raw_sample",
                "24",
            ],
            Self::Mp3 => &["-c:a", "libmp3lame", "-b:a", "320k"],
        }
    }
}

pub fn export_audio(
    project: &Project,
    destination: &Path,
    format: AudioFormat,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    render_range(
        project,
        destination,
        Output::new(Format::Preview),
        0..project.duration(),
        RenderPass::Audio {
            sample_rate: SAMPLE_RATE,
            channels: 2,
            format,
        },
        cancel,
        progress,
    )
}

fn isolated_clip_audio(clip: &Clip, asset: &Asset) -> Result<Project> {
    ensure!(
        asset.audio && !clip.freeze && !clip.slot,
        "Select a clip with audio."
    );
    let mut clip = clip.clone();
    clip.start = 0;
    clip.gain = 1.0;
    Ok(Project {
        assets: vec![asset.clone()],
        tracks: vec![crate::project::Track {
            clips: vec![clip],
            ..crate::project::Track::new("Clip audio", TrackKind::Audio)
        }],
        ..Project::default()
    })
}

/// Analyze the selected clip locally and return a linear gain and its estimated LUFS.
pub fn normalize_clip_loudness(
    clip: &Clip,
    asset: &Asset,
    cancel: &AtomicBool,
) -> Result<(f32, f64)> {
    let project = isolated_clip_audio(clip, asset)?;
    let length = clip.length;
    let scratch = tempfile::tempdir()?;
    let mix = scratch.path().join("mix.wav");
    render_audio_master(&project, &mix, cancel, |_| {})?;
    check_cancel(cancel)?;
    let status = scratch.path().join("progress");
    let analysis = scratch.path().join("loudness");
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg
        .args([
            "-hide_banner",
            "-nostdin",
            "-nostats",
            "-progress",
            "pipe:1",
            "-i",
        ])
        .arg(&mix)
        .args([
            "-af",
            "loudnorm=I=-16:TP=-1.5:print_format=json",
            "-f",
            "null",
            "-",
        ])
        .stdout(File::create(&status)?)
        .stderr(File::create(&analysis)?);
    let mut child = ffmpeg.spawn()?;
    if let Err(error) = wait_for_render(&mut child, &status, &analysis, length, cancel, &|_| {}, 50)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    check_cancel(cancel)?;
    let report = read_to_string(&analysis)?;
    let start = report
        .rfind('{')
        .context("FFmpeg returned no loudness measurement.")?;
    let metrics = serde_json::Deserializer::from_str(&report[start..])
        .into_iter::<Value>()
        .next()
        .transpose()?
        .context("No loudness measurement.")?;
    let parse = |key: &str| {
        metrics[key]
            .as_str()
            .and_then(|text| text.parse::<f64>().ok())
            .filter(|reading| reading.is_finite())
            .context("The clip is silent or too short to measure its loudness.")
    };
    let loudness = parse("input_i")?;
    let true_peak = parse("input_tp")?;
    let change_db = (-16.0 - loudness).min(-1.5 - true_peak);
    let gain = 10.0_f64.powf(change_db / 20.0).min(4.0);
    Ok((gain as f32, loudness + 20.0 * gain.log10()))
}

/// Find recurring beat positions in the selected clip's timeline range.
pub fn detect_clip_beats(clip: &Clip, asset: &Asset, cancel: &AtomicBool) -> Result<Vec<u32>> {
    ensure!(
        clip.end() <= MAX_FRAMES,
        "The clip is outside the timeline."
    );
    let project = isolated_clip_audio(clip, asset)?;
    let scratch = tempfile::tempdir()?;
    let source = scratch.path().join("beat-analysis.wav");
    render_audio_mix(&project, &source, 0..clip.length, cancel, |_| {})?;
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args(["-f", "s16le", "pipe:1"]);
    let beats = cutout_pipe(&mut ffmpeg, scratch.path(), "Decode beat audio", |stream| {
        crate::beats::detect(BufReader::new(stream), clip.length, cancel)
    })?;
    Ok(beats.into_iter().map(|tick| clip.start + tick).collect())
}

/// Align selected source audio to the first clip, returning signed timeline tick offsets.
pub fn audio_sync_offsets(
    clips: &[(Clip, Asset)],
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<Vec<i32>> {
    ensure!(
        (2..=9).contains(&clips.len()),
        "Select two to nine clips with matching audio."
    );
    let scratch = tempfile::tempdir()?;
    let mut tracks = Vec::with_capacity(clips.len());
    for (index, (original, asset)) in clips.iter().enumerate() {
        ensure!(
            original.length as usize >= crate::audio_sync::MIN_OVERLAP,
            "Choose clips with at least three seconds of audio."
        );
        let mut clip = original.clone();
        clip.effects.clear();
        clip.gain_animation = Default::default();
        clip.audio_fade = Default::default();
        clip.audio_channels = Default::default();
        clip.audio_balance = 0;
        clip.pitch_semitones = 0;
        let analysis = isolated_clip_audio(&clip, asset)?;
        let wav = scratch.path().join(format!("sync-{index}.wav"));
        let frames = clip.length.min(crate::audio_sync::MAX_ANALYSIS);
        render_audio_mix(&analysis, &wav, 0..frames, cancel, |_| {})?;
        let mut decoder = command("ffmpeg")?;
        decoder
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&wav)
            .args(["-f", "s16le", "pipe:1"]);
        let signal = cutout_pipe(
            &mut decoder,
            scratch.path(),
            "Decode sync audio",
            |mut stream| {
                let sample_rate = crate::beats::SAMPLE_RATE as usize;
                let count = frames as usize * sample_rate / FPS as usize;
                let mut pcm = vec![0; count * 2];
                stream.read_exact(&mut pcm)?;
                let envelope = (0..frames as usize)
                    .map(|frame| {
                        let first = frame * sample_rate / FPS as usize;
                        let end = (frame + 1) * sample_rate / FPS as usize;
                        let power =
                            pcm[first * 2..end * 2]
                                .chunks_exact(2)
                                .fold(0.0, |sum, sample| {
                                    let value =
                                        f64::from(i16::from_le_bytes([sample[0], sample[1]]))
                                            / 32768.0;
                                    sum + value * value
                                })
                                / (end - first) as f64;
                        (power + 1e-9).ln()
                    })
                    .collect::<Vec<_>>();
                Ok(envelope)
            },
        )?;
        tracks.push(signal);
        progress(((index + 1) * 90 / clips.len()) as u8);
    }
    let mut offsets = vec![0];
    for signal in &tracks[1..] {
        offsets.push(crate::audio_sync::offset(&tracks[0], signal, cancel)?);
    }
    progress(100);
    Ok(offsets)
}

/// Write a reversed copy of the selected source span and return its fractional start.
pub fn reverse_clip(
    clip: &Clip,
    asset: &Asset,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<(Asset, u8)> {
    check_cancel(cancel)?;
    ensure!(
        !clip.freeze && !clip.slot && matches!(asset.kind, AssetKind::Video | AssetKind::Audio),
        "Choose a video or audio clip to reverse."
    );
    let source = std::path::absolute(asset.path.as_ref().context("Relink the source first.")?)?;
    let destination = std::path::absolute(destination)?;
    ensure!(
        !destination.exists() && !same_path(&source, &destination),
        "Choose a new file for the reversed media."
    );
    let parent = destination
        .parent()
        .context("Choose a local file for the reversed media.")?;
    let scratch = tempfile::tempdir_in(parent)?;
    let first = clip.source_units_at(0);
    let end = first + clip.source_duration_units();
    let start_frame = u32::try_from(first / 100)?;
    let end_frame = u32::try_from((end + 99) / 100)?;
    ensure!(
        start_frame < end_frame && end_frame <= asset.frames,
        "The selected clip extends past the source."
    );
    let geometry = asset
        .geometry
        .or_else(|| probe(&source).ok().and_then(|media| media.geometry));
    let video = asset.kind == AssetKind::Video;
    let pixel_format = if video {
        let result = command("ffprobe")?
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                DEMUXERS,
                "-select_streams",
                "V:0",
                "-show_entries",
                "stream=pix_fmt",
                "-of",
                "csv=p=0",
            ])
            .arg(&source)
            .output()?;
        let name = String::from_utf8_lossy(&result.stdout);
        let name = name.trim();
        match name {
            "yuv420p" | "yuv422p" | "yuv444p" | "yuv420p10le" | "yuv422p10le" | "yuv444p10le"
            | "yuv420p12le" | "yuv422p12le" | "yuv444p12le" | "yuva444p" => name.to_owned(),
            "bgra" | "rgba" => "bgra".into(),
            _ => "yuva444p".into(),
        }
    } else {
        String::new()
    };
    let (rate, pixels) = geometry.map_or((FPS, 4096_u64 * 4096), |size| {
        (
            match size.fps {
                0..=30 => 30,
                31..=60 => 60,
                _ => 120,
            },
            u64::from(size.width) * u64::from(size.height),
        )
    });
    // Bound the buffers used by FFmpeg's reverse filters by encoding short chunks.
    let chunk = if video {
        (128 * 1024 * 1024 / (pixels.saturating_mul(8 * u64::from(rate / FPS))).max(1))
            .clamp(1, 5 * u64::from(FPS)) as u32
    } else {
        15 * FPS
    };
    let mut last = end_frame;
    let mut list = String::new();
    let mut part = 0;
    while last > start_frame {
        let first = last.saturating_sub(chunk).max(start_frame);
        let length = last - first;
        let seek = if asset.preview_seek && asset.audio_offset_samples == 0 {
            first.saturating_sub(FPS) / FPS * FPS
        } else {
            0
        };
        let filename = format!("part{part}.mkv");
        let mut ffmpeg = command("ffmpeg")?;
        ffmpeg
            .current_dir(scratch.path())
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-nostats",
                "-progress",
                "pipe:1",
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                DEMUXERS,
                "-ss",
                &seconds(seek),
                "-i",
            ])
            .arg(&source);
        let audio_input = if seek > 0 && asset.audio {
            ffmpeg
                .args([
                    "-protocol_whitelist",
                    "file",
                    "-format_whitelist",
                    DEMUXERS,
                    "-i",
                ])
                .arg(&source);
            1
        } else {
            0
        };
        let mut graph = Vec::new();
        if video {
            let factor = rate / FPS;
            let from = (first - seek) * factor;
            let to = (last - seek) * factor;
            graph.push(format!("[0:V:0]setpts=PTS-STARTPTS,fps={rate},format={pixel_format},trim=start_frame={from}:end_frame={to},reverse,settb=1/{rate},setpts=N[v]"));
            ffmpeg.args(["-map", "[v]", "-c:v", "ffv1", "-threads:v", "2"]);
        }
        if asset.audio {
            let source_offset = asset.audio_offset_samples;
            let start = i64::from(first) * SAMPLES_PER_FRAME - source_offset.min(0);
            let samples = i64::from(length) * SAMPLES_PER_FRAME;
            let end = start + samples;
            let delay = source_offset.max(0);
            graph.push(format!("[{audio_input}:a:0]aresample={SAMPLE_RATE},asetpts=PTS-STARTPTS,adelay={delay}S:all=1,apad,atrim=start_sample={start}:end_sample={end},areverse,asetpts=N/SR/TB[a]"));
            ffmpeg.args(["-map", "[a]", "-c:a", "flac"]);
        }
        ffmpeg.args([
            "-filter_complex_threads",
            "2",
            "-filter_complex",
            &graph.join(";"),
            &filename,
        ]);
        run_media_preparation(&mut ffmpeg, scratch.path(), length, cancel, &|_| {})?;
        list.push_str(&format!("file {filename}\nduration {}\n", seconds(length)));
        last = first;
        part += 1;
        progress(((end_frame - last) as u64 * 95 / u64::from(end_frame - start_frame)) as u8);
    }
    std::fs::write(scratch.path().join("parts.txt"), list)?;
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg.current_dir(scratch.path()).args([
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
        "-protocol_whitelist",
        "file",
        "-f",
        "concat",
        "-safe",
        "1",
        "-i",
        "parts.txt",
        "-map",
        "0",
        "-c",
        "copy",
        "reversed.mkv",
    ]);
    run_media_preparation(
        &mut ffmpeg,
        scratch.path(),
        end_frame - start_frame,
        cancel,
        &|_| {},
    )?;
    let staged = scratch.path().join("reversed.mkv");
    let mut reversed = probe(&staged)?;
    ensure!(
        reversed.frames >= end_frame - start_frame,
        "Reversed media was shorter than the selected span."
    );
    check_cancel(cancel)?;
    tempfile::TempPath::try_from_path(staged)?
        .persist_noclobber(&destination)
        .context("Could not save the new reversed media file.")?;
    reversed.path = Some(destination.clone());
    reversed.label = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    progress(100);
    Ok((reversed, (i64::from(end_frame) * 100 - end) as u8))
}

/// Generate motion-compensated 120 fps footage without changing clip timing.
pub fn interpolate_clip_motion(
    clip: &Clip,
    asset: &Asset,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<(Asset, u8)> {
    prepare_clip_motion(clip, asset, destination, false, cancel, progress)
}

/// Stabilize camera shake while retaining the selected source span.
pub fn stabilize_clip(
    clip: &Clip,
    asset: &Asset,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<(Asset, u8)> {
    prepare_clip_motion(clip, asset, destination, true, cancel, progress)
}

fn prepare_clip_motion(
    clip: &Clip,
    asset: &Asset,
    destination: &Path,
    stabilize: bool,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<(Asset, u8)> {
    check_cancel(cancel)?;
    ensure!(
        !clip.freeze && !clip.slot && asset.kind == AssetKind::Video,
        "Choose a video clip for motion processing."
    );
    let source = std::path::absolute(asset.path.as_ref().context("Relink the source first.")?)?;
    let destination = std::path::absolute(destination)?;
    ensure!(
        !destination.exists() && !same_path(&source, &destination),
        "Choose a new file for the prepared media."
    );
    let geometry = asset
        .geometry
        .or_else(|| probe(&source).ok()?.geometry)
        .context("The source has no video geometry.")?;
    ensure!(
        stabilize || geometry.fps < 120,
        "Choose source footage below 120 fps."
    );
    let rate = if stabilize {
        match geometry.fps {
            0..=30 => 30,
            31..=60 => 60,
            _ => 120,
        }
    } else {
        120
    };
    let pixel_format = command("ffprobe")?
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-select_streams",
            "V:0",
            "-show_entries",
            "stream=pix_fmt",
            "-of",
            "csv=p=0",
        ])
        .arg(&source)
        .output()?;
    let format = String::from_utf8_lossy(&pixel_format.stdout);
    ensure!(
        pixel_format.status.success()
            && matches!(
                format.trim(),
                "yuv420p"
                    | "yuv422p"
                    | "yuv444p"
                    | "yuvj420p"
                    | "yuvj422p"
                    | "yuvj444p"
                    | "rgb24"
                    | "bgr24"
                    | "rgb0"
                    | "bgr0"
                    | "0rgb"
                    | "0bgr"
                    | "gray"
                    | "nv12"
                    | "nv21"
            ),
        "Motion processing requires opaque 8-bit footage."
    );
    let first = clip.source_units_at(0);
    let end = first + clip.source_duration_units();
    let source_in = u32::try_from(first / 100)?;
    let source_out = u32::try_from((end + 99) / 100)?;
    ensure!(
        source_in < source_out && source_out <= asset.frames,
        "The clip extends past its source."
    );
    let factor = rate / FPS;
    let start = source_in * factor;
    let end = source_out * factor;
    let length = source_out - source_in;
    let parent = destination
        .parent()
        .context("Choose a local file for the prepared media.")?;
    let scratch = tempfile::tempdir_in(parent)?;
    let staged = scratch.path().join("prepared.mkv");
    let seek = if asset.preview_seek && !asset.audio {
        source_in.saturating_sub(FPS) / FPS * FPS
    } else {
        0
    };
    let start = start - seek * factor;
    let end = end - seek * factor;
    let motion_filter = if stabilize {
        format!("fps={rate},deshake=rx=16:ry=16:edge=mirror:search=less:contrast=50")
    } else {
        format!("minterpolate=fps={rate}:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1")
    };
    let mut graph = format!(
        "[0:V:0]setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop=3,{motion_filter},trim=start_frame={start}:end_frame={end},settb=1/{rate},setpts=N[v]"
    );
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-nostats",
            "-progress",
            "pipe:1",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-ss",
            &seconds(seek),
            "-i",
        ])
        .arg(&source)
        .args(["-map", "[v]", "-c:v", "ffv1", "-threads:v", "2"]);
    if asset.audio {
        let offset = asset.audio_offset_samples;
        let start = i64::from(source_in) * SAMPLES_PER_FRAME - offset.min(0);
        let end = start + i64::from(length) * SAMPLES_PER_FRAME;
        let delay = offset.max(0);
        graph.push_str(&format!(
            ";[0:a:0]aresample={SAMPLE_RATE},adelay={delay}S:all=1,apad,atrim=start_sample={start}:end_sample={end},asetpts=N/SR/TB[a]"));
        ffmpeg.args(["-map", "[a]", "-c:a", "flac"]);
    }
    ffmpeg
        .args(["-filter_complex_threads", "2", "-filter_complex", &graph])
        .arg(&staged);
    run_media_preparation(&mut ffmpeg, scratch.path(), length, cancel, &progress)?;
    let mut prepared = probe(&staged)?;
    ensure!(
        prepared.frames >= length,
        "Prepared media was shorter than the selected span."
    );
    check_cancel(cancel)?;
    tempfile::TempPath::try_from_path(staged)?
        .persist_noclobber(&destination)
        .context("Could not save the new media file.")?;
    prepared.label = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    prepared.path = Some(destination);
    progress(100);
    Ok((prepared, (first % 100) as u8))
}

/// Encode a bounded-resolution video proxy with the source's presentation timing.
pub fn create_preview_proxy(
    asset: &Asset,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<crate::project::PreviewProxy> {
    check_cancel(cancel)?;
    ensure!(asset.kind == AssetKind::Video, "Select a video source.");
    let source = asset
        .path
        .as_deref()
        .context("Relink the original video first.")?;
    let destination = std::path::absolute(destination)?;
    ensure!(
        !destination.exists() && !same_path(source, &destination),
        "Choose a new MP4 file for the proxy."
    );
    let source_metadata = source.metadata()?;
    let source_modified = source_metadata.modified()?;
    let probe = command("ffprobe")?
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-select_streams",
            "V:0",
            "-show_entries",
            "stream=pix_fmt,color_transfer:stream_tags=alpha_mode",
            "-of",
            "json",
        ])
        .arg(source)
        .output()?;
    ensure!(
        probe.status.success(),
        "Could not inspect the original video."
    );
    let information: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
    let stream = &information["streams"][0];
    ensure!(
        matches!(
            stream["pix_fmt"].as_str(),
            Some(
                "yuv420p"
                    | "yuv422p"
                    | "yuv444p"
                    | "yuvj420p"
                    | "yuvj422p"
                    | "yuvj444p"
                    | "rgb24"
                    | "bgr24"
                    | "rgb0"
                    | "bgr0"
                    | "0rgb"
                    | "0bgr"
                    | "gray"
                    | "nv12"
                    | "nv21"
            )
        ) && !matches!(
            stream["color_transfer"].as_str(),
            Some("smpte2084" | "arib-std-b67")
        ) && stream["tags"]["alpha_mode"].as_str() != Some("1"),
        "Preview proxies currently support opaque 8-bit SDR sources."
    );
    let parent = destination
        .parent()
        .context("Choose a local proxy folder.")?;
    let scratch = tempfile::tempdir_in(parent)?;
    let staged = scratch.path().join("proxy.mp4");
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg.args(["-hide_banner", "-nostdin", "-v", "error", "-nostats", "-progress", "pipe:1",
        "-protocol_whitelist", "file", "-format_whitelist", DEMUXERS, "-i"])
        .arg(source)
        .args(["-map", "0:V:0", "-an", "-vf",
            "scale=w='min(iw,960)':h='min(ih,540)':force_original_aspect_ratio=decrease:force_divisible_by=2:reset_sar=1:flags=area,settb=1/90000,setpts=PTS-STARTPTS",
            "-fps_mode", "passthrough", "-enc_time_base", "1:90000", "-video_track_timescale", "90000",
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "22", "-g", "15", "-bf", "0",
            "-threads:v", "2", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(&staged);
    run_media_preparation(&mut ffmpeg, scratch.path(), asset.frames, cancel, &progress)?;
    let original = source.metadata()?;
    ensure!(
        original.modified()? == source_modified && original.len() == source_metadata.len(),
        "The original video changed during proxy generation."
    );
    let proxy_metadata = staged.metadata()?;
    let proxy = crate::project::PreviewProxy {
        path: destination.clone(),
        source_bytes: original.len(),
        source_modified,
        proxy_bytes: proxy_metadata.len(),
        proxy_modified: proxy_metadata.modified()?,
    };
    check_cancel(cancel)?;
    tempfile::TempPath::try_from_path(staged)?
        .persist_noclobber(&destination)
        .context("Could not save the preview proxy.")?;
    progress(100);
    Ok(proxy)
}

fn run_media_preparation(
    command: &mut Command,
    folder: &Path,
    frames: u32,
    cancel: &AtomicBool,
    progress: &impl Fn(u8),
) -> Result<()> {
    check_cancel(cancel)?;
    let status = folder.join("progress");
    let errors = folder.join("errors");
    let mut child = command
        .stdout(File::create(&status)?)
        .stderr(File::create(&errors)?)
        .spawn()?;
    if let Err(error) = wait_for_render(&mut child, &status, &errors, frames, cancel, progress, 50)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    check_cancel(cancel)
}

/// Create a local transparent copy, retaining the clip's source timing and audio.
pub fn remove_background(
    clip: &Clip,
    asset: &Asset,
    destination: &Path,
    python: &Path,
    model: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<(Asset, u8)> {
    check_cancel(cancel)?;
    let still = asset.kind == AssetKind::Image;
    ensure!(
        !clip.freeze && !clip.slot && (still || asset.kind == AssetKind::Video),
        "Choose a video or image clip for background removal."
    );
    let source = std::path::absolute(asset.path.as_ref().context("Relink the source first.")?)?;
    let destination = std::path::absolute(destination)?;
    let model = std::path::absolute(model)?;
    ensure!(model.is_file(), "Choose a local U²-Net ONNX model.");
    ensure!(
        !destination.exists() && !same_path(&source, &destination),
        "Choose a new file for the cutout."
    );
    let extension = if still { "png" } else { "mkv" };
    ensure!(
        destination
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case(extension)),
        "Save the cutout as .{extension}."
    );
    let scratch = tempfile::tempdir_in(destination.parent().context("Choose an output folder.")?)?;
    let staged = scratch.path().join(format!("cutout.{extension}"));
    let first = if still { 0 } else { clip.source_units_at(0) };
    let source_in = u32::try_from(first / 100)?;
    let source_out = if still {
        1
    } else {
        u32::try_from((first + clip.source_duration_units() + 99) / 100)?
    };
    ensure!(
        source_in < source_out && source_out <= asset.frames,
        "The clip extends past its source."
    );
    let length = source_out - source_in;
    let rate = match asset.geometry.map(|geometry| geometry.fps) {
        Some(31..=60) => 60,
        Some(61..) => 120,
        _ => FPS,
    };
    let factor = rate / FPS;
    let expected = if still { 1 } else { length * factor };
    // One PNG per frame carries its own dimensions and preserves existing source alpha.
    let mut decode = command("ffmpeg")?;
    decode
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-i",
        ])
        .arg(&source)
        .args(["-map", "0:V:0", "-filter_threads", "2"]);
    if !still {
        decode.args(["-vf", &format!(
            "setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop_duration=0.1,fps={rate},trim=start_frame={}:end_frame={},setpts=PTS-STARTPTS,scale=w='max(1,round(iw*sar))':h=ih:flags=lanczos,setsar=1",
            source_in * factor, source_out * factor
        )]);
    }
    decode.args([
        "-frames:v",
        &expected.to_string(),
        "-c:v",
        "png",
        "-compression_level",
        "1",
        "-pix_fmt",
        "rgba",
        "-threads:v",
        "1",
        "-fps_mode",
        "passthrough",
        "-f",
        "image2pipe",
        "pipe:1",
    ]);
    let mut infer = subject_model_command(python, &model, expected);
    let mut encode = command("ffmpeg")?;
    encode.args([
        "-hide_banner",
        "-nostdin",
        "-v",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
        "-f",
        "image2pipe",
        "-framerate",
        &rate.to_string(),
        "-c:v",
        "png",
        "-i",
        "pipe:0",
    ]);
    if asset.audio && !still {
        encode
            .args([
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                DEMUXERS,
                "-i",
            ])
            .arg(&source);
    }
    encode.args(["-map", "0:v:0"]);
    if still {
        encode.args(["-frames:v", "1", "-update", "1"]);
    } else {
        encode.args(["-c:v", "ffv1", "-pix_fmt", "bgra", "-threads:v", "2"]);
    }
    if asset.audio && !still {
        let offset = asset.audio_offset_samples;
        let start = i64::from(source_in) * SAMPLES_PER_FRAME - offset.min(0);
        let end = start + i64::from(length) * SAMPLES_PER_FRAME;
        let delay = offset.max(0);
        encode.args(["-map", "1:a:0", "-af", &format!(
                "aresample={SAMPLE_RATE},adelay={delay}S:all=1,apad,atrim=start_sample={start}:end_sample={end},asetpts=N/SR/TB"
            ), "-c:a", "flac"]);
    }
    encode.arg(&staged);
    cutout_pipe(&mut decode, scratch.path(), "Decode", |decoded| {
        infer.stdin(decoded);
        cutout_pipe(&mut infer, scratch.path(), "Background model", |frames| {
            encode.stdin(frames);
            run_media_preparation(&mut encode, scratch.path(), length, cancel, &progress)
        })
    })?;
    let mut prepared = probe(&staged)?;
    ensure!(
        prepared.frames >= length,
        "The cutout was shorter than the selected span."
    );
    check_cancel(cancel)?;
    tempfile::TempPath::try_from_path(staged)?
        .persist_noclobber(&destination)
        .context("Could not save the cutout file.")?;
    prepared.label = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    prepared.path = Some(destination);
    progress(100);
    Ok((prepared, (first % 100) as u8))
}

/// Fit a source into the current canvas and suggest editable subject-following positions.
pub fn reframe_subject(
    clip: &Clip,
    asset: &Asset,
    canvas: [u32; 2],
    python: &Path,
    model: &Path,
    cancel: &AtomicBool,
) -> Result<(f32, crate::project::PositionAnimation)> {
    check_cancel(cancel)?;
    let still = asset.kind == AssetKind::Image;
    ensure!(
        !clip.slot && !clip.freeze && (still || asset.kind == AssetKind::Video),
        "Choose a video or image clip for auto reframe."
    );
    ensure!(
        clip.transform.rotation == 0.0
            && clip.rotation_animation.is_empty()
            && clip.framing == crate::project::Framing::default(),
        "Reset rotation and crop before applying auto reframe."
    );
    ensure!(
        canvas[0] > 0 && canvas[1] > 0 && clip.length > 0,
        "Invalid canvas or clip length."
    );
    let source = asset.path.as_ref().context("Relink the source first.")?;
    let model = std::path::absolute(model)?;
    ensure!(model.is_file(), "Choose a local U²-Net ONNX model.");
    let last = clip.length - 1;
    let stride = last
        .div_ceil(crate::project::MAX_POSITION_KEYS as u32 - 1)
        .max(1);
    let mut ticks: Vec<_> = if still {
        vec![0]
    } else {
        (0..=last).step_by(stride as usize).collect()
    };
    if !still && ticks.last().copied() != Some(last) {
        ticks.push(last);
    }
    let resize = "scale=w='max(1,min(320,round(iw*sar*min(1,320/max(iw*sar,ih)))))':h='max(1,round(ih*min(1,320/max(iw*sar,ih))))':flags=area,setsar=1";
    let filter = if still {
        resize.to_owned()
    } else {
        let source_us = clip.source_units_at(0) * 10_000 / i64::from(FPS);
        let speed = f64::from(clip.speed_percent) / 100.0;
        format!(
            "settb=1/1000000,setpts=(PTS-STARTPTS-{source_us})/{speed:.2},tpad=stop_mode=clone:stop_duration=0.1,fps={FPS}:start_time=0,trim=end_frame={},select='not(mod(n,{stride}))+eq(n,{last})',{resize}",
            clip.length
        )
    };
    let scratch = tempfile::tempdir()?;
    let mut decode = command("ffmpeg")?;
    decode
        .args([
            "-v",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            DEMUXERS,
            "-i",
        ])
        .arg(source)
        .args([
            "-map",
            "0:V:0",
            "-filter_threads",
            "2",
            "-vf",
            &filter,
            "-frames:v",
            &ticks.len().to_string(),
            "-fps_mode",
            "passthrough",
            "-c:v",
            "png",
            "-pix_fmt",
            "rgba",
            "-threads:v",
            "1",
            "-f",
            "image2pipe",
            "pipe:1",
        ]);
    let mut infer = subject_model_command(python, &model, ticks.len() as u32);
    infer.arg("subjects");
    let result = cutout_pipe(&mut decode, scratch.path(), "Decode", |decoded| {
        infer.stdin(decoded);
        run_subject_analysis(&mut infer, scratch.path(), cancel)
    })?;
    let mut dimensions = [0.0_f32; 2];
    let mut centers = Vec::with_capacity(ticks.len());
    for result in result.lines() {
        let values: Vec<f32> = result
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            matches!(values.len(), 2 | 4),
            "Invalid subject model output."
        );
        dimensions = [values[0], values[1]];
        centers.push(values.get(2..4).map(|pair| [pair[0], pair[1]]));
    }
    ensure!(
        centers.len() == ticks.len() && dimensions.iter().all(|n| *n > 0.0),
        "Could not analyze the full clip."
    );
    ensure!(
        centers.iter().any(Option::is_some),
        "No foreground subject was detected."
    );
    if let Some(geometry) = asset.geometry {
        dimensions = [geometry.width as f32, geometry.height as f32];
    }
    let canvas = canvas.map(|n| n as f32);
    let ratios = [canvas[0] / dimensions[0], canvas[1] / dimensions[1]];
    let fit = ratios[0].min(ratios[1]);
    let fill = ratios[0].max(ratios[1]);
    let scale = fill / fit;
    ensure!(
        scale <= crate::project::MAX_CLIP_SCALE,
        "This aspect ratio needs a crop beyond the supported zoom."
    );
    let displayed = dimensions.map(|n| n * fill);
    let mut framed = clip.clone();
    framed.position_animation = Default::default();
    let first = centers.iter().flatten().next().copied().unwrap_or([0.5; 2]);
    let mut center = first;
    for (tick, found) in ticks.into_iter().zip(centers) {
        if let Some(found) = found {
            center = std::array::from_fn(|axis| center[axis] * 0.25 + found[axis] * 0.75);
        }
        let position = std::array::from_fn(|axis| {
            let reach = (displayed[axis] - canvas[axis]).max(0.0) * 0.5;
            ((0.5 - center[axis]) * displayed[axis]).clamp(-reach, reach) / canvas[axis]
        });
        framed.set_position_keyframe(tick, position)?;
    }
    Ok((scale, framed.position_animation))
}

fn run_subject_analysis(
    command: &mut Command,
    folder: &Path,
    cancel: &AtomicBool,
) -> Result<String> {
    let output = folder.join("subjects.txt");
    let errors = folder.join("subjects.log");
    let mut child = command
        .stdout(File::create(&output)?)
        .stderr(File::create(&errors)?)
        .spawn()
        .context("Could not start Python for subject analysis.")?;
    let finished = (|| loop {
        check_cancel(cancel)?;
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        std::thread::sleep(Duration::from_millis(40));
    })();
    let status = match finished {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    if !status.success() {
        let log = read_to_string(errors).unwrap_or_default();
        let detail = log.lines().last().unwrap_or_default();
        bail!("Subject analysis failed. {detail}");
    }
    Ok(read_to_string(output)?)
}

fn subject_model_command(python: &Path, model: &Path, frames: u32) -> Command {
    let mut command = Command::new(python);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .env("OMP_NUM_THREADS", "2")
        .env("NUMBA_DISABLE_JIT", "1")
        .args(["-c", include_str!("cutout_worker.py")])
        .arg(model)
        .arg(frames.to_string());
    command
}

fn cutout_pipe<T>(
    command: &mut Command,
    folder: &Path,
    stage: &str,
    consume: impl FnOnce(ChildStdout) -> Result<T>,
) -> Result<T> {
    let log = folder.join(format!("{stage}.log"));
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(File::create(&log)?)
        .spawn()
        .with_context(|| format!("Could not start {stage}."))?;
    let output = child
        .stdout
        .take()
        .context("Could not open the cutout pipeline.")?;
    let result = consume(output);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    if !status.success() {
        let diagnostic = read_to_string(&log).unwrap_or_default();
        if let Some(summary) = diagnostic.lines().last().filter(|line| !line.is_empty()) {
            bail!("{stage} failed. {summary}");
        }
    }
    let value = result?;
    ensure!(status.success(), "{stage} failed ({status}).");
    Ok(value)
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
        RenderPass::Timeline(Some(quality)),
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
        RenderPass::Still(quality),
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
enum RenderPass<'a> {
    Timeline(Option<PreviewQuality>),
    Delivery(&'a ExportDetails),
    Gif(u32),
    Still(Option<PreviewQuality>),
    Png,
    Audio {
        sample_rate: u32,
        channels: u8,
        format: AudioFormat,
    },
}

impl RenderPass<'_> {
    fn is_still(self) -> bool {
        matches!(self, Self::Still(_) | Self::Png)
    }

    fn preview_quality(self) -> Option<PreviewQuality> {
        match self {
            Self::Timeline(quality) | Self::Still(quality) => quality,
            Self::Png => Some(PreviewQuality::Full),
            _ => None,
        }
    }

    fn wants_audio(self) -> bool {
        matches!(
            self,
            Self::Timeline(_) | Self::Delivery(_) | Self::Audio { .. }
        )
    }
}

fn audible(clip: &Clip, asset: &Asset) -> bool {
    !clip.freeze && asset.audio && clip.has_audible_gain()
}

fn render_range(
    project: &Project,
    destination: &Path,
    settings: Output,
    range: Range<u32>,
    pass: RenderPass<'_>,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<()> {
    let format = settings.format;
    let still = pass.is_still();
    let preview = pass.preview_quality();
    let audio_only = matches!(pass, RenderPass::Audio { .. });
    if format == Format::Gif {
        ensure!(
            matches!(settings.fps, 12 | 15 | 24 | 30),
            "Choose 12, 15, 24, or 30 fps for GIF."
        );
    } else {
        ensure!(
            matches!(settings.fps, 24 | 30 | 60 | 120),
            "Choose 24, 30, 60, or 120 fps."
        );
    }
    check_cancel(cancel)?;
    project.validate()?;
    ensure!(
        project.duration() > 0,
        "Add a clip to the timeline before rendering."
    );
    ensure!(
        range.start < range.end && range.end <= project.duration(),
        "The render range is outside the timeline."
    );
    let [width, height] = preview.map_or_else(
        || settings.size(project),
        |quality| quality.size(project, still),
    );
    let full_preview = preview.is_some_and(|quality| {
        quality == PreviewQuality::Full || quality == PreviewQuality::Auto && still
    });
    let use_proxies = preview.is_some() && !full_preview && !audio_only;
    let raster = Raster {
        width,
        height,
        rate: settings.fps.max(FPS),
        full_chroma: matches!(format, Format::Mkv | Format::Gif)
            || settings.quality == Quality::High
            || full_preview,
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
            .source_paths()
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
    let mut staged_luts = HashMap::new();
    for track in project
        .tracks
        .iter()
        .filter(|track| !track.muted && (pass.wants_audio() || track.kind == TrackKind::Video))
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
            let active = || {
                clip.effects
                    .iter()
                    .filter(|effect| effect.enabled && effect.amount != 0)
            };
            let preroll = active().filter(|effect| !effect.preset.audio()).try_fold(
                0_u32,
                |frames, effect| {
                    effect
                        .preset
                        .visual_preroll()
                        .map(|needed| frames.saturating_add(needed))
                },
            );
            let history = preroll.unwrap_or(skip).min(skip);
            let asset = project
                .asset(&clip.asset_id)
                .context("Clip media is missing.")?;
            if audio_only && !audible(clip, asset) {
                continue;
            }
            let mut font = None;
            let mut video_seek = 0;
            let mut audio_seek = 0;
            let (input, video_input) = if let Some(title) = &asset.title {
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
                (None, None)
            } else {
                let path = asset
                    .path
                    .as_ref()
                    .filter(|path| !clip.slot && path.is_file())
                    .with_context(|| format!("Replace or relink media for {}.", asset.label))?;
                if !audio_only
                    && (track.kind == TrackKind::Video || format == Format::Preview)
                    && asset.preview_seek
                    && asset.kind == AssetKind::Video
                    && clip.speed_percent == crate::project::NORMAL_SPEED
                    && clip.source_subframe == 0
                    && preroll.is_some()
                {
                    let first_frame = if clip.freeze {
                        clip.source
                    } else {
                        clip.source + skip - history
                    };
                    // Whole seconds keep the same frame-rate conversion phase as decoding
                    // from the beginning. Leave a second of input before the first frame.
                    video_seek = first_frame.saturating_sub(FPS) / FPS * FPS;
                    if format == Format::Preview
                        && clip.pitch_semitones == 0
                        && !active().any(|effect| effect.preset.audio())
                    {
                        audio_seek = video_seek;
                    }
                }
                let proxy = asset.proxy.as_ref().filter(|proxy| {
                    use_proxies
                        && track.kind == TrackKind::Video
                        && asset.kind == AssetKind::Video
                        && proxy.matches_source(path)
                });
                let has_video = !audio_only && track.kind == TrackKind::Video;
                let has_audio = pass.wants_audio() && audible(clip, asset);
                let shared =
                    proxy.is_none() && has_video && (!has_audio || video_seek == audio_seek);
                let input = if shared || has_audio {
                    let index = inputs.len();
                    let seek = if shared { video_seek } else { audio_seek };
                    inputs.push((std::path::absolute(path)?, seek));
                    Some(index)
                } else {
                    None
                };
                let video_input = if has_video && !shared {
                    let index = inputs.len();
                    let source = proxy.map_or(path, |proxy| &proxy.path);
                    inputs.push((std::path::absolute(source)?, video_seek));
                    Some(index)
                } else {
                    input
                };
                (input, video_input)
            };
            let transition = clip
                .transition
                .filter(|_| adjacent && clip.start > origin && skip == 0);
            let mask_input = if !audio_only
                && track.kind == TrackKind::Video
                && (clip.mask.is_some() || !clip.mask_strokes.is_empty())
            {
                let index = inputs.len();
                let path = folder.path().join(format!("mask{index}.pgm"));
                stage_mask(&path, clip, raster, cancel)?;
                inputs.push((path, 0));
                Some(index)
            } else {
                None
            };
            let lut = if let Some(path) = clip
                .color_lut
                .as_ref()
                .filter(|_| !audio_only && track.kind == TrackKind::Video)
            {
                if let Some(existing) = staged_luts.get(path) {
                    Some(String::clone(existing))
                } else {
                    const MAX_BYTES: u64 = 16 * 1024 * 1024;
                    let filename = format!("look{}.cube", staged_luts.len());
                    let metadata = path
                        .metadata()
                        .with_context(|| format!("Relink the color LUT {}.", path.display()))?;
                    ensure!(
                        metadata.is_file() && metadata.len() <= MAX_BYTES,
                        "Choose a .cube LUT up to 16 MiB."
                    );
                    let mut file = File::open(path)?.take(MAX_BYTES + 1);
                    let bytes = std::io::copy(
                        &mut file,
                        &mut File::create(folder.path().join(&filename))?,
                    )?;
                    ensure!(bytes <= MAX_BYTES, "Choose a .cube LUT up to 16 MiB.");
                    staged_luts.insert(path.clone(), filename.clone());
                    Some(filename)
                }
            } else {
                None
            };
            clips.push(Layer {
                kind: track.kind,
                clip,
                asset,
                input,
                video_input,
                mask_input,
                font,
                lut,
                video_seek,
                audio_seek,
                start: first - origin,
                skip,
                history,
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
        RenderPass::Timeline(_) | RenderPass::Delivery(_) | RenderPass::Gif(_) => {
            format.extension()
        }
        RenderPass::Still(_) => "rgb",
        RenderPass::Png => "png",
        RenderPass::Audio { format, .. } => format.extension(),
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
    let artwork = if let RenderPass::Delivery(ExportDetails {
        cover_frame: Some(frame),
        ..
    }) = pass
    {
        let cover = folder.path().join("cover.png");
        export_frame_png(project, &cover, *frame, cancel)?;
        ffmpeg
            .args(["-f", "png_pipe", "-protocol_whitelist", "file", "-i"])
            .arg(cover);
        Some(inputs.len())
    } else {
        None
    };
    ffmpeg.arg("-/filter_complex").arg(graph);
    if !audio_only {
        ffmpeg.args(["-map", "[video]"]);
    }
    if let RenderPass::Audio {
        sample_rate,
        channels,
        format,
    } = pass
    {
        ffmpeg
            .args([
                "-map",
                "[audio]",
                "-vn",
                "-ac",
                &channels.to_string(),
                "-ar",
                &sample_rate.to_string(),
            ])
            .args(format.codec());
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
    } else if matches!(pass, RenderPass::Gif(_)) {
        ffmpeg.args(format.codecs(settings.quality, full_preview));
    } else {
        ffmpeg
            .args([
                "-map",
                "[audio]",
                "-t",
                &seconds(frames),
                "-threads",
                "4",
                "-r:v:0",
                &settings.fps.to_string(),
            ])
            .args(format.codecs(settings.quality, full_preview));
    }
    if let RenderPass::Delivery(details) = pass
        && !details.title.is_empty()
    {
        ffmpeg
            .arg("-metadata")
            .arg(format!("title={}", details.title));
    }
    if let Some(artwork) = artwork {
        ffmpeg.args([
            "-map",
            &format!("{artwork}:v:0"),
            "-c:v:1",
            "png",
            "-pix_fmt:v:1",
            "rgb24",
            "-disposition:v:1",
            "attached_pic",
        ]);
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
    pass: RenderPass<'_>,
) -> String {
    let audio_only = matches!(pass, RenderPass::Audio { .. });
    let wants_audio = pass.wants_audio();
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
    if wants_audio {
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
            let history = layer.history;
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
                let input = layer.video_input.expect("video input");
                let retimed =
                    clip.speed_percent != crate::project::NORMAL_SPEED || clip.source_subframe != 0;
                let video_clock = if retimed {
                    let source_us = clip.source_units_at(0) * 10_000 / i64::from(FPS);
                    let speed = f64::from(clip.speed_percent) / 100.0;
                    format!(
                        "settb=1/1000000,setpts=(PTS-STARTPTS-{source_us})/{speed:.2},fps={rate}:start_time=0"
                    )
                } else if layer.video_seek == 0 {
                    format!("setpts=PTS-STARTPTS,fps={rate}")
                } else {
                    // Input seeking shifts PTS by the requested time. Keep any
                    // residual offset when a source has no frame at that instant.
                    format!("fps={rate}:start_time=0")
                };
                let (timing, hold) = if clip.freeze {
                    let start = (clip.source - layer.video_seek) * factor;
                    let end = start + 1;
                    let frames = needed * factor - 1;
                    // Crop and resize the held frame once, before duplicating it.
                    (
                        format!(
                            "tpad=stop_mode=clone:stop=1,trim=start_frame={start}:end_frame={end}"
                        ),
                        format!(",tpad=stop_mode=clone:stop={frames}"),
                    )
                } else {
                    let offset = if retimed {
                        source_offset
                    } else if asset.kind == AssetKind::Image {
                        0
                    } else {
                        clip.source + source_offset - layer.video_seek
                    };
                    let pad = if asset.kind == AssetKind::Image {
                        needed
                    } else {
                        1
                    };
                    let end = (offset + needed) * factor;
                    let offset = offset * factor;
                    (
                        format!(
                            "tpad=stop_mode=clone:stop_duration={},trim=start_frame={offset}:end_frame={end}",
                            seconds(pad)
                        ),
                        String::new(),
                    )
                };
                let framing = framing_filters(clip.framing);
                format!(
                    "[{input}:V:0]{video_clock},{timing},settb=1/{rate},setpts=N{framing},\
                         scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2:reset_sar=1:flags=lanczos,\
                         setsar=1,format={alpha},pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black@0{hold}"
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
            for ((_, channel), range) in crate::project::HUE_RANGES
                .into_iter()
                .zip(clip.hue_adjustments)
            {
                if range != crate::project::HueAdjustment::default() {
                    let saturation = f64::from(range.saturation) / 100.0;
                    let intensity = f64::from(range.intensity) / 100.0;
                    source.push_str(&format!(",format=rgba,huesaturation=colors={channel}:hue={}:saturation={saturation:.2}:intensity={intensity:.2}", range.hue));
                }
            }
            if let Some(lut) = &layer.lut {
                source.push_str(&format!(",format=rgba,lut3d=file={lut}:interp=tetrahedral"));
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
            append_visual_fades(&mut source, layer, raster);
            if !clip.opacity_animation.is_empty() {
                append_opacity_animation(&mut graph, &mut source, layer, index, raster);
            }
            let start = layer.start * factor;
            if clip.transform == Transform::default()
                && clip.position_animation.is_empty()
                && clip.rotation_animation.is_empty()
                && clip.scale_animation.is_empty()
            {
                graph.push(format!("{source},setsar=1,format={alpha},settb=1/{rate},setpts=N+{start}[visual{index}]"));
            } else {
                transformed_layer(&mut graph, &source, layer, index, raster);
            }
            if let Some(transition) = layer.transition.filter(|_| clip.length > 1) {
                video = transition_layer(&mut graph, &video, index, layer, transition, raster);
            } else {
                compose_layer(
                    &mut graph,
                    &video,
                    &format!("layer{index}"),
                    index,
                    layer,
                    raster,
                );
                video = format!("layer{index}");
            }
        }
        if wants_audio
            && let Some(input) = layer.input
            && audible(clip, asset)
        {
            if clip.speed_percent != crate::project::NORMAL_SPEED
                || clip.source_subframe != 0
                || clip.pitch_semitones != 0
            {
                retimed_audio(&mut graph, layer, index, input, &effects(true));
                audio.push(format!("[sound{index}]"));
                continue;
            }
            let source_start =
                i64::from(clip.source) * SAMPLES_PER_FRAME - asset.audio_offset_samples;
            let window_skip = i64::from(layer.skip) * SAMPLES_PER_FRAME;
            let end = source_start + window_skip + i64::from(layer.length) * SAMPLES_PER_FRAME;
            if end > 0 {
                let fx = effects(true);
                let seek = i64::from(layer.audio_seek) * SAMPLES_PER_FRAME;
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
                let gain = audio_gain_filter(clip, from.max(0) - source_start);
                let channels = channel_filter(clip);
                graph.push(format!("[{input}:a:0]aresample={SAMPLE_RATE},atrim=start_sample={}:end_sample={},\
                    asetpts=PTS-STARTPTS,aformat=channel_layouts=stereo{channels}{gain}{fx}{fade}{final_trim},adelay={delay}S:all=1[sound{index}]",
                    from.max(0) - seek, end - seek));
                audio.push(format!("[sound{index}]"));
            }
        }
    }
    if let RenderPass::Gif(fps) = pass {
        graph.extend([
            format!("[{video}]trim=start_frame={},settb=1/{rate},setpts=N,fps={fps}:eof_action=pass,format=rgb24,split[gifframes][gifcolors]", skip * factor),
            "[gifcolors]palettegen=stats_mode=single[gifpalette]".into(),
            "[gifframes][gifpalette]paletteuse=new=1:dither=sierra2_4a[video]".into(),
        ]);
    } else if !audio_only {
        graph.push(format!(
            "[{video}]trim=start_frame={},settb=1/{rate},setpts=N,format={opaque}[video]",
            skip * factor
        ));
    }
    if wants_audio {
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

fn retimed_audio(
    graph: &mut Vec<String>,
    layer: &Layer<'_>,
    index: usize,
    input: usize,
    effects: &str,
) {
    let clip = layer.clip;
    let source_start =
        clip.source_units_at(0) * (SAMPLES_PER_FRAME / 100) - layer.asset.audio_offset_samples;
    let pre_silence = (-source_start).max(0);
    let pitch = 2.0_f64.powf(f64::from(clip.pitch_semitones) / 12.0);
    let pitch_rate = (f64::from(SAMPLE_RATE) * pitch).round() as u32;
    let mut speed =
        f64::from(clip.speed_percent) / 100.0 * f64::from(SAMPLE_RATE) / f64::from(pitch_rate);
    let mut tempo = String::new();
    if pitch_rate != SAMPLE_RATE {
        tempo.push_str(&format!(",asetrate={pitch_rate},aresample={SAMPLE_RATE}"));
    }
    while speed < 0.5 {
        tempo.push_str(",atempo=0.5");
        speed *= 2.0;
    }
    while speed > 2.0 {
        tempo.push_str(",atempo=2.0");
        speed /= 2.0;
    }
    if speed != 1.0 {
        tempo.push_str(&format!(",atempo={speed:.4}"));
    }
    let window_start = i64::from(layer.skip) * SAMPLES_PER_FRAME;
    let window_end = window_start + i64::from(layer.length) * SAMPLES_PER_FRAME;
    let delay = i64::from(layer.start) * SAMPLES_PER_FRAME;
    let fade = audio_fade_filter(clip, 0, window_end);
    let gain = audio_gain_filter(clip, 0);
    let channels = channel_filter(clip);
    graph.push(format!(
        "[{input}:a:0]aresample={SAMPLE_RATE},atrim=start_sample={},asetpts=PTS-STARTPTS,\
         aformat=channel_layouts=stereo{channels},adelay={pre_silence}S:all=1{tempo},apad,atrim=end_sample={window_end}\
         {gain}{effects}{fade},atrim=start_sample={window_start},asetpts=PTS-STARTPTS,\
         adelay={delay}S:all=1[sound{index}]",
        source_start.max(0)
    ));
}

fn channel_filter(clip: &Clip) -> String {
    use crate::project::AudioChannels;
    if clip.audio_channels == AudioChannels::Stereo && clip.audio_balance == 0 {
        return String::new();
    }
    let balance = f32::from(clip.audio_balance) / 100.0;
    let left = 1.0 - balance.max(0.0);
    let right = 1.0 + balance.min(0.0);
    let channel = |side, volume| match clip.audio_channels {
        AudioChannels::Stereo => format!("{volume:.4}*c{side}"),
        AudioChannels::Mono => format!("{:.4}*c0+{:.4}*c1", volume * 0.5, volume * 0.5),
        AudioChannels::Left => format!("{volume:.4}*c0"),
        AudioChannels::Right => format!("{volume:.4}*c1"),
    };
    format!(
        ",pan=stereo|c0={}|c1={}",
        channel(0, left),
        channel(1, right)
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

fn audio_gain_filter(clip: &Clip, first_sample: i64) -> String {
    let animation = &clip.gain_animation;
    if animation.is_empty() {
        return format!(",volume={:.5}", clip.gain);
    }
    let first = first_sample + i64::from(animation.offset) * SAMPLES_PER_FRAME;
    let gain = scalar_expression(animation, &format!("((n+({first}))/{SAMPLES_PER_FRAME})"));
    format!(",aeval='val(ch)*({gain})':c=same")
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

fn framing_filters(framing: crate::project::Framing) -> String {
    let mut filters = String::new();
    if framing.flip_horizontal {
        filters.push_str(",hflip");
    }
    if framing.flip_vertical {
        filters.push_str(",vflip");
    }
    let crate::project::Framing {
        left,
        right,
        top,
        bottom,
        ..
    } = framing;
    if left != 0 || right != 0 || top != 0 || bottom != 0 {
        let width = 100 - left - right;
        let height = 100 - top - bottom;
        filters.push_str(&format!(
            ",crop=w='max(1,iw*{width}/100)':h='max(1,ih*{height}/100)':x=iw*{left}/100:y=ih*{top}/100:exact=1"
        ));
    }
    filters
}

fn append_visual_fades(source: &mut String, layer: &Layer<'_>, raster: Raster) {
    let fade = layer.clip.visual_fade;
    if fade.is_empty() {
        return;
    }
    let rate = raster.rate;
    let alpha = raster.alpha();
    let (first, end) = fade.anchors(layer.clip.length);
    for (kind, start, duration) in [
        ("in", first, fade.in_frames),
        ("out", end - i64::from(fade.out_frames), fade.out_frames),
    ] {
        let start = start - i64::from(layer.skip);
        if duration == 0
            || (kind == "in" && start + i64::from(duration) <= 0)
            || (kind == "out" && start >= i64::from(layer.length))
        {
            continue;
        }
        // Shift timestamps when the preview starts inside a fade so it retains its progress.
        let elapsed = (-start).max(0) * i64::from(rate / FPS);
        let delay = seconds(start.max(0) as u32);
        let duration = seconds(duration);
        source.push_str(&format!(
            ",format={alpha},settb=1/{rate},setpts=N+{elapsed},\
             fade=t={kind}:st={delay}:d={duration}:alpha=1,setpts=N"
        ));
    }
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
        let fraction = format!("(max(0,({tick}-({})))/{span})", a.frame);
        let progress = easing_expression(clip.position_animation.easing, fraction);
        expression = format!(
            "if(lt({tick},{}),{start:.6}+({difference:.6})*{progress},{expression})",
            b.frame
        );
    }
    format!("round({expression})")
}

fn easing_expression(easing: crate::project::Easing, fraction: String) -> String {
    match easing {
        crate::project::Easing::Linear => fraction,
        crate::project::Easing::EaseIn => format!("pow({fraction},2)"),
        crate::project::Easing::EaseOut => format!("(1-pow(1-{fraction},2))"),
        crate::project::Easing::EaseInOut => format!("(pow({fraction},2)*(3-2*{fraction}))"),
        crate::project::Easing::Hold => "0".into(),
    }
}

fn scalar_expression(animation: &crate::project::ScalarAnimation, tick: &str) -> String {
    let mut expression = format!("{:.6}", animation.keys.last().unwrap().value);
    for pair in animation.keys.windows(2).rev() {
        let (a, b) = (pair[0], pair[1]);
        let delta = b.value - a.value;
        let span = b.frame - a.frame;
        let fraction = format!("(max(0,{tick}-({}))/{span})", a.frame);
        let progress = easing_expression(animation.easing, fraction);
        expression = format!(
            "if(lt({tick},{}),{:.6}+({delta:.6})*{progress},{expression})",
            b.frame, a.value
        );
    }
    expression
}

fn append_opacity_animation(
    graph: &mut Vec<String>,
    source: &mut String,
    layer: &Layer<'_>,
    index: usize,
    raster: Raster,
) {
    let animation = &layer.clip.opacity_animation;
    let factor = raster.rate / FPS;
    let skip = i64::from(layer.skip) + i64::from(animation.offset);
    let expression = scalar_expression(animation, &format!("(N/{factor}+({skip}))"));
    let Raster {
        width,
        height,
        rate,
        ..
    } = raster;
    let alpha = raster.alpha();
    let duration = seconds(layer.length);
    // Compute the animated alpha on a tiny matte and expand it before multiplying the original alpha.
    graph.extend([
        format!("{source},format={alpha},settb=1/{rate},setpts=N,split=2[opacitybase{index}][opacitysrc{index}]"),
        format!("[opacitysrc{index}]alphaextract[opacityoriginal{index}]"),
        format!("nullsrc=s=2x2:r={rate}:d={duration},format=gray,geq=lum='round(255*({expression}))',\
                 scale={width}:{height}:flags=neighbor,settb=1/{rate},setpts=N[opacitycurve{index}]"),
        format!("[opacityoriginal{index}][opacitycurve{index}]blend=all_mode=multiply[opacitymask{index}]"),
        format!("[opacitybase{index}][opacitymask{index}]alphamerge[opacitylayer{index}]"),
    ]);
    *source = format!("[opacitylayer{index}]null");
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
    let angle = f64::from(transform.rotation).to_radians();
    let rotation = &layer.clip.rotation_animation;
    let rotate = if rotation.is_empty() {
        format!(
            "rotate={angle:.7}:ow='ceil(rotw({angle:.7})/2)*2':oh='ceil(roth({angle:.7})/2)*2':c=black@0"
        )
    } else {
        let offset = i64::from(layer.skip) + i64::from(rotation.offset);
        let expression = scalar_expression(rotation, &format!("(t*{FPS}+({offset}))"));
        let bounds = "ceil(hypot(iw,ih)/2)*2";
        format!("rotate='({expression})*PI/180':ow='{bounds}':oh='{bounds}':c=black@0")
    };
    let x = position_expression(layer, 0, w);
    let y = position_expression(layer, 1, h);
    let scale = &layer.clip.scale_animation;
    let maximum = scale
        .keys
        .iter()
        .map(|key| key.value)
        .reduce(f32::max)
        .unwrap_or(transform.scale);
    let width = (((w as f64 * f64::from(maximum)).round() as usize) & !1).max(2);
    let height = (((h as f64 * f64::from(maximum)).round() as usize) & !1).max(2);
    let resize = if scale.is_empty() {
        format!("scale={width}:{height}:flags=lanczos")
    } else {
        let offset = i64::from(layer.skip) + i64::from(scale.offset);
        let expression = scalar_expression(scale, &format!("(t*{FPS}+({offset}))"));
        format!(
            "scale=w='min({width},max(2,2*trunc((iw*({expression})+0.5)/2)))':\
            h='min({height},max(2,2*trunc((ih*({expression})+0.5)/2)))':eval=frame:flags=lanczos,\
            pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black@0:eval=frame"
        )
    };
    let opacity = if layer.clip.opacity_animation.is_empty() {
        transform.opacity
    } else {
        1.0
    };
    graph.push(format!("{source},format=rgba,settb=1/{rate},setpts=N,{resize},{rotate},\
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

fn compose_layer(
    graph: &mut Vec<String>,
    background: &str,
    output: &str,
    index: usize,
    layer: &Layer<'_>,
    raster: Raster,
) {
    let factor = raster.rate / FPS;
    let start = layer.start * factor;
    let end = start + layer.length * factor;
    let visual = format!("visual{index}");
    let Some(mode) = layer.clip.blend_mode.filter() else {
        graph.push(overlay(background, &visual, output, start, end, raster));
        return;
    };
    graph.extend([
        format!("[{background}]split=2[blendbase{index}][blendcolor{index}]"),
        format!("[{visual}]split=2[blendinput{index}][blendalpha{index}]"),
        format!("[blendalpha{index}]alphaextract[blendmask{index}]"),
        format!("[blendinput{index}]format=gbrp[blendfg{index}]"),
        format!("[blendcolor{index}]format=gbrp[blendbg{index}]"),
        format!("[blendbg{index}][blendfg{index}]blend=all_mode={mode}:eof_action=pass:repeatlast=0,format=gbrap[blendrgb{index}]"),
        format!("[blendrgb{index}][blendmask{index}]alphamerge[blended{index}]"),
        overlay(&format!("blendbase{index}"), &format!("blended{index}"), output, start, end, raster),
    ]);
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
    graph.push(format!(
        "[{background}]split=2[base{index}][previous{index}]"
    ));
    compose_layer(
        graph,
        &format!("base{index}"),
        &format!("scene{index}"),
        index,
        layer,
        raster,
    );
    graph.extend([
        format!("[previous{index}]trim=start_frame={start_tail}:end_frame={start},\
                 settb=1/{rate},setpts=N,format=yuv444p,tpad=stop_mode=clone:stop={frames}[from{index}]"),
        format!("[scene{index}]split=2[full{index}][incoming{index}]"),
        format!("[incoming{index}]trim=start_frame={start}:end_frame={},settb=1/{rate},\
                 setpts=N,format=yuv444p,tpad=stop_mode=clone:stop=1[to{index}]", start + cross_length),
        format!("[from{index}][to{index}]xfade=transition={}:duration={}:offset=0,\
                 trim=end_frame={frames},settb=1/{rate},setpts=N+{start}[blend{index}]", t.kind.filter(), seconds(duration)),
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

#[derive(Clone, Copy)]
struct ShotTone {
    shadows: f64,
    highlights: f64,
    saturation: f64,
}

fn shot_tone(asset: &Asset, frame: u32) -> Result<ShotTone> {
    ensure!(
        matches!(asset.kind, AssetKind::Image | AssetKind::Video),
        "Choose a video or image to analyze."
    );
    let path = asset
        .path
        .as_ref()
        .context("Relink the clip before analyzing it.")?;
    let mut ffmpeg = command("ffmpeg")?;
    ffmpeg.args([
        "-v",
        "error",
        "-nostdin",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        DEMUXERS,
    ]);
    if asset.kind == AssetKind::Video {
        ffmpeg.args(["-ss", &seconds(frame.min(asset.frames.saturating_sub(1)))]);
    }
    let output = ffmpeg
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:V:0",
            "-frames:v",
            "1",
            "-filter_threads",
            "1",
            "-vf",
            "scale=160:160:force_original_aspect_ratio=decrease",
            "-threads:v",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "Could not read the source frame. {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut histogram = [0_u32; 256];
    let mut saturation = 0.0;
    let mut count = 0;
    for pixel in output
        .stdout
        .chunks_exact(4)
        .filter(|pixel| pixel[3] >= 128)
    {
        let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(u32::from);
        let luma = (54 * r + 183 * g + 19 * b + 128) >> 8;
        histogram[luma as usize] += 1;
        saturation += f64::from(r.max(g).max(b) - r.min(g).min(b)) / 255.0;
        count += 1;
    }
    ensure!(count > 0, "The source frame has no visible pixels.");
    let percentile = |percent: u32| {
        let rank = (count * percent / 100).max(1);
        let mut total = 0;
        histogram
            .iter()
            .position(|n| {
                total += n;
                total >= rank
            })
            .unwrap_or_default() as f64
            / 255.0
    };
    Ok(ShotTone {
        shadows: percentile(10),
        highlights: percentile(90),
        saturation: saturation / f64::from(count),
    })
}

/// Suggest editable brightness, contrast, and saturation for one source frame.
pub fn suggest_color(
    asset: &Asset,
    frame: u32,
    reference: Option<&Asset>,
    cancel: &AtomicBool,
) -> Result<ColorAdjustment> {
    check_cancel(cancel)?;
    let source = shot_tone(asset, frame)?;
    check_cancel(cancel)?;
    let target = if let Some(reference) = reference {
        shot_tone(reference, 0)?
    } else {
        ShotTone {
            shadows: 0.12,
            highlights: 0.88,
            saturation: source.saturation.max(0.12),
        }
    };
    check_cancel(cancel)?;
    let source_span = source.highlights - source.shadows;
    let target_span = target.highlights - target.shadows;
    let contrast = if source_span < 0.04 {
        1.0
    } else {
        (target_span / source_span).clamp(0.65, 1.35)
    };
    let source_midpoint = (source.shadows + source.highlights) * 0.5;
    let target_midpoint = (target.shadows + target.highlights) * 0.5;
    let brightness = target_midpoint - (0.5 + (source_midpoint - 0.5) * contrast);
    let saturation = if source.saturation < 0.025 {
        0.0
    } else {
        (target.saturation / source.saturation - 1.0).clamp(-0.65, 0.4)
    };
    Ok(ColorAdjustment {
        brightness: (brightness * 300.0).round().clamp(-100.0, 100.0) as i8,
        contrast: ((contrast - 1.0) * 100.0).round() as i8,
        saturation: (saturation * 100.0).round() as i8,
    })
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
