//! Local translation tracking for overlays.

use crate::media::{self, PreviewQuality};
use crate::project::{MAX_POSITION_KEYS, PositionAnimation, Project, TrackKind};
use anyhow::{Context, Result, ensure};
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

/// A centered patch in the picture beneath the overlay at its first frame.
#[derive(Clone, Copy)]
pub struct Seed {
    pub x: u8,
    pub y: u8,
    pub size: u8,
}

impl Default for Seed {
    fn default() -> Self {
        Self {
            x: 50,
            y: 50,
            size: 14,
        }
    }
}

pub fn track_overlay(
    mut project: Project,
    id: &str,
    seed: Seed,
    cancel: &AtomicBool,
) -> Result<PositionAnimation> {
    let canceled = || -> Result<()> {
        ensure!(!cancel.load(Ordering::Relaxed), "Motion tracking canceled.");
        Ok(())
    };
    canceled()?;
    let (row, index) = project.location(id).context("Select an overlay clip.")?;
    let mut overlay = project.tracks[row].clips[index].clone();
    ensure!(
        project.tracks[row].kind == TrackKind::Video && row > 0,
        "Choose a visual overlay above the footage to track."
    );
    ensure!(
        seed.x <= 100 && seed.y <= 100 && (5..=35).contains(&seed.size),
        "Choose a valid tracking patch."
    );
    project.tracks.truncate(row);
    project
        .tracks
        .retain(|track| track.kind == TrackKind::Video && !track.muted);
    let frames = overlay
        .end()
        .min(project.duration())
        .saturating_sub(overlay.start);
    ensure!(
        frames >= 2,
        "The overlay needs at least two frames over lower footage."
    );
    for clip in project.tracks.iter_mut().flat_map(|track| &mut track.clips) {
        clip.gain = 0.0;
        clip.gain_animation = Default::default();
    }
    let full = project.width.max(project.height);
    let small = |n: u32| ((n * 192 / full) & !1).max(16);
    (project.width, project.height) = (small(project.width), small(project.height));
    let [width, height] = [project.width as usize, project.height as usize];
    let radius = ((width.min(height) * usize::from(seed.size)) / 200).max(2);
    let center =
        |size: usize, percent| (size * usize::from(percent) / 100).clamp(radius, size - radius - 1);
    let initial = (center(width, seed.x), center(height, seed.y));
    let stride = (frames - 1).div_ceil(MAX_POSITION_KEYS as u32 - 1);
    let scratch = tempfile::tempdir()?;
    let footage = scratch.path().join("tracking.mp4");
    media::render_preview_with(
        &project,
        &footage,
        overlay.start..overlay.start + frames,
        PreviewQuality::Full,
        cancel,
    )?;
    let mut raw = media::command("ffmpeg")?
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(footage)
        .args([
            "-an",
            "-vf",
            &format!("select='not(mod(n,{stride}))',format=gray"),
            "-fps_mode",
            "passthrough",
            "-threads",
            "1",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = raw
        .stdout
        .take()
        .context("Could not read tracking frames.")?;
    let result = (|| {
        let mut frame = vec![0; width * height];
        input
            .read_exact(&mut frame)
            .context("Could not decode the tracking reference.")?;
        let mut patch = Vec::new();
        for y in initial.1 - radius..=initial.1 + radius {
            let start = y * width + initial.0 - radius;
            patch.extend_from_slice(&frame[start..start + radius * 2 + 1]);
        }
        let low = *patch.iter().min().unwrap();
        let high = *patch.iter().max().unwrap();
        ensure!(
            high - low >= 24,
            "Choose a tracking patch with a visible edge or texture."
        );
        let base = overlay.position_at(0);
        overlay.position_animation = PositionAnimation::default();
        overlay.set_position_keyframe(0, base)?;
        let mut position = initial;
        for tick in (stride..frames).step_by(stride as usize) {
            canceled()?;
            input
                .read_exact(&mut frame)
                .context("Could not decode a tracking frame.")?;
            let Some((x, y)) =
                find_patch(&frame, [width, height], &patch, radius, position, stride)
            else {
                break;
            };
            position = (x, y);
            overlay.set_position_keyframe(
                tick,
                [
                    (base[0] + (x as f32 - initial.0 as f32) / width as f32).clamp(-1.5, 1.5),
                    (base[1] + (y as f32 - initial.1 as f32) / height as f32).clamp(-1.5, 1.5),
                ],
            )?;
        }
        ensure!(
            overlay.position_animation.keys.len() >= 2,
            "The selected tracking patch was lost."
        );
        Ok(overlay.position_animation)
    })();
    drop(input);
    let _ = raw.kill();
    let _ = raw.wait();
    result
}

fn find_patch(
    frame: &[u8],
    [width, height]: [usize; 2],
    patch: &[u8],
    radius: usize,
    (previous_x, previous_y): (usize, usize),
    stride: u32,
) -> Option<(usize, usize)> {
    let search = (3 + stride as usize * 2).min(32);
    let patch_width = radius * 2 + 1;
    let mut best = (u32::MAX, u32::MAX, previous_x, previous_y);
    for y in previous_y.saturating_sub(search).max(radius)
        ..=(previous_y + search).min(height - radius - 1)
    {
        for x in previous_x.saturating_sub(search).max(radius)
            ..=(previous_x + search).min(width - radius - 1)
        {
            let error = patch
                .chunks_exact(patch_width)
                .enumerate()
                .map(|(row, values)| {
                    let start = (y + row - radius) * width + x - radius;
                    values
                        .iter()
                        .zip(&frame[start..start + patch_width])
                        .map(|(a, b)| u32::from(a.abs_diff(*b)))
                        .sum::<u32>()
                })
                .sum::<u32>();
            let movement = (x.abs_diff(previous_x) + y.abs_diff(previous_y)) as u32;
            if (error, movement) < (best.0, best.1) {
                best = (error, movement, x, y);
            }
        }
    }
    (best.0 <= patch.len() as u32 * 32).then_some((best.2, best.3))
}
