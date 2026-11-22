//! A bounded video queue, with audio from the same rendered preview file.

use anyhow::Result;
use cutad::{media, project::FPS};
use eframe::egui;
use rodio::{Decoder, OutputStream, Sink, Source};
use std::{
    fs::File,
    io::Read,
    path::Path,
    process::Child,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

struct Audio {
    sink: Sink,
}

impl Audio {
    fn load(path: &Path, frame: u32, stream: &OutputStream) -> Result<Self> {
        let mut decoder = Decoder::try_from(File::open(path)?)?;
        if frame > 0 {
            decoder
                .try_seek(time_at(frame))
                .map_err(|error| anyhow::anyhow!("Cannot seek preview audio: {error}"))?;
        }
        let sink = Sink::connect_new(stream.mixer());
        sink.pause();
        sink.append(decoder);
        Ok(Self { sink })
    }
}

pub struct Playback {
    decoder: Child,
    frames: Receiver<Vec<u8>>,
    clock: Option<Instant>,
    next: u32,
    first: u32,
    end: u32,
    audio: Option<Audio>,
}

pub struct Update {
    pub frame: Option<(u32, Vec<u8>)>,
    pub finished: bool,
}

impl Playback {
    pub fn new(
        ctx: &egui::Context,
        path: &Path,
        size: [usize; 2],
        start: u32,
        end: u32,
        audio_device: Option<&OutputStream>,
    ) -> Result<Self> {
        let mut decoder = media::preview_frames(path, start)?;
        let mut video = decoder.stdout.take().expect("preview command pipes video");
        let frame_bytes = size[0] * size[1] * 3;
        let queue = (6 * 1024 * 1024 / frame_bytes).clamp(1, 4);
        let (sender, frames) = mpsc::sync_channel(queue);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(()) = {
                let mut frame = vec![0; frame_bytes];
                video
                    .read_exact(&mut frame)
                    .and_then(|_| sender.send(frame).map_err(std::io::Error::other))
            } {
                ctx.request_repaint();
            }
            drop(sender);
            ctx.request_repaint();
        });
        let audio = audio_device.and_then(|stream| Audio::load(path, start, stream).ok());
        Ok(Self {
            decoder,
            frames,
            clock: None,
            next: start,
            first: start,
            end,
            audio,
        })
    }

    pub fn update(&mut self) -> Update {
        let mut newest = None;
        let elapsed = self
            .clock
            .map(|clock| {
                let wall = clock.elapsed();
                self.audio
                    .as_ref()
                    .filter(|audio| !audio.sink.empty())
                    .map_or(wall, |audio| {
                        wall.min(audio.sink.get_pos() + Duration::from_millis(38))
                    })
            })
            .unwrap_or_default();
        let want = self
            .first
            .saturating_add((elapsed.as_secs_f64() * f64::from(FPS)) as u32);
        while self.next <= want && self.next < self.end {
            match self.frames.try_recv() {
                Ok(rgb) => {
                    if self.clock.is_none() {
                        if let Some(audio) = &self.audio {
                            audio.sink.play();
                        }
                        self.clock = Some(Instant::now());
                    }
                    newest = Some((self.next, rgb));
                    self.next += 1;
                }
                Err(TryRecvError::Empty) => {
                    return Update {
                        frame: newest,
                        finished: false,
                    };
                }
                Err(TryRecvError::Disconnected) => {
                    return Update {
                        frame: newest,
                        finished: true,
                    };
                }
            }
        }
        Update {
            frame: newest,
            finished: self.next >= self.end && want >= self.end,
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // Release the audio file before the preview directory is cleaned up.
        drop(self.audio.take());
        let _ = self.decoder.kill();
        let _ = self.decoder.wait();
    }
}

fn time_at(frame: u32) -> Duration {
    Duration::from_secs_f64(f64::from(frame) / f64::from(FPS))
}
