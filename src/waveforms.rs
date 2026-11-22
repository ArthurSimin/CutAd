//! Bounded waveform tiles decoded only for media visible on the timeline.

use cutad::{
    media,
    project::{Asset, FPS, seconds},
};
use eframe::egui;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
};

pub const BLOCK_FRAMES: u32 = 5 * FPS;
const SAMPLE_RATE: u32 = 6_000;
const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE / FPS) as usize;
const MAX_TILES: usize = 144;
pub type Tile = [u8; BLOCK_FRAMES as usize];

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    start: u32,
}

#[derive(Default)]
pub struct Waveforms {
    cache: HashMap<Key, Option<Box<Tile>>>,
    order: VecDeque<Key>,
    worker: Option<(Key, Receiver<Option<Box<Tile>>>)>,
}

impl Waveforms {
    pub fn collect(&mut self) {
        let Some((_, channel)) = &self.worker else {
            return;
        };
        let result = match channel.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Disconnected) => None,
            Err(TryRecvError::Empty) => return,
        };
        let (key, _) = self.worker.take().expect("completed waveform tile");
        if self.order.len() == MAX_TILES
            && let Some(old) = self.order.pop_front()
        {
            self.cache.remove(&old);
        }
        self.order.push_back(key.clone());
        self.cache.insert(key, result);
    }

    pub fn tile(&mut self, ctx: &egui::Context, asset: &Asset, start: u32) -> Option<&Tile> {
        if !asset.audio {
            return None;
        }
        let path = asset.path.clone()?;
        let key = Key { path, start };
        if !self.cache.contains_key(&key) && self.worker.is_none() {
            let (sender, receiver) = mpsc::channel();
            let input = key.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = sender.send(decode(&input.path, input.start));
                ctx.request_repaint();
            });
            self.worker = Some((key.clone(), receiver));
        }
        self.cache.get(&key).and_then(|item| item.as_deref())
    }
}

fn decode(path: &Path, start: u32) -> Option<Box<Tile>> {
    let mut command = Command::new(media::executable("ffmpeg").ok()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &seconds(start),
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-t",
            &seconds(BLOCK_FRAMES),
            "-ac",
            "1",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| Box::new(peaks(&output.stdout)))
}

fn peaks(pcm: &[u8]) -> Tile {
    let mut result = [0; BLOCK_FRAMES as usize];
    for (frame, samples) in pcm
        .chunks(SAMPLES_PER_FRAME * 2)
        .take(result.len())
        .enumerate()
    {
        let max = samples
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]).unsigned_abs())
            .max()
            .unwrap_or_default();
        result[frame] = (max / 128).min(255) as u8;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_tracks_signed_sample_peaks_and_pads_short_sources() {
        let mut pcm = vec![0; SAMPLES_PER_FRAME * 4];
        pcm[48..50].copy_from_slice(&i16::MIN.to_le_bytes());
        let second = SAMPLES_PER_FRAME * 2;
        pcm[second..second + 2].copy_from_slice(&8192_i16.to_le_bytes());
        let envelope = peaks(&pcm);
        assert_eq!(&envelope[..4], &[255, 64, 0, 0]);
    }
}
