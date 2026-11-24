//! Non-blocking previews. Still frames follow the playhead and playback buffers short sections.

use crate::playback::Playback;
use anyhow::Result;
use cutad::media::PreviewQuality;
use cutad::{
    media,
    project::{FPS, Project},
};
use eframe::egui;
use rodio::{OutputStream, OutputStreamBuilder};
use std::{
    collections::VecDeque,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

const SECTION: u32 = 2 * FPS;
const FRAME_SETTLE: Duration = Duration::from_millis(24);
const CACHED_FRAMES: usize = 16;
const CACHE_BYTES: usize = 25 * 1024 * 1024;

struct Section {
    _folder: tempfile::TempDir,
    path: PathBuf,
    range: Range<u32>,
}

struct Playing {
    player: Playback,
    section: Arc<Section>,
}

#[derive(Clone, PartialEq, Eq)]
enum Request {
    Frame(u32),
    Draft(u32),
    Play(Range<u32>),
}

enum Prepared {
    Frame(u32, bool, Vec<u8>),
    Play(Arc<Section>),
}

struct Pending {
    key: Request,
    cancel: Arc<AtomicBool>,
    result: Receiver<Result<Prepared>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Preview {
    pending: Option<Pending>,
    sections: VecDeque<Arc<Section>>,
    active: Option<Playing>,
    next: Option<Playing>,
    audio: Option<OutputStream>,
    tried_audio: bool,
    wants_play: bool,
    quality: PreviewQuality,
    frame_ready: Option<u32>,
    draft_frame: Option<u32>,
    frames: VecDeque<(u32, Arc<Vec<u8>>)>,
    frame_settle: Option<(u32, Instant)>,
    failed: Option<(Request, String)>,
}

pub struct Frame {
    pub size: [usize; 2],
    pub pixels: Arc<Vec<u8>>,
}

impl Preview {
    pub fn quality(&self) -> PreviewQuality {
        self.quality
    }

    pub fn set_quality(&mut self, quality: PreviewQuality) {
        if self.quality != quality {
            self.quality = quality;
            self.invalidate();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.wants_play
    }

    pub fn is_output_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn status(&self) -> Option<&str> {
        if let Some((_, message)) = &self.failed {
            return Some(message);
        }
        if self.active.is_some() {
            return self
                .audio
                .is_none()
                .then_some("Preview audio output unavailable");
        }
        if self.wants_play {
            return Some("Buffering preview…");
        }
        self.pending.as_ref().map(|_| "Updating preview…")
    }

    pub fn invalidate(&mut self) {
        let resume = self.wants_play;
        self.pause();
        self.wants_play = resume;
        self.sections.clear();
        self.frames.clear();
        self.draft_frame = None;
        self.frame_ready = None;
        self.frame_settle = None;
        self.failed = None;
    }

    pub fn pause(&mut self) {
        if self.wants_play && self.quality == PreviewQuality::Auto {
            self.frame_ready = None;
        }
        self.wants_play = false;
        self.active = None;
        self.next = None;
        self.pending = None;
        self.frame_settle = None;
    }

    pub fn play(&mut self) {
        self.wants_play = true;
        self.failed = None;
        self.pending = None;
    }

    pub fn update(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        cursor: &mut u32,
    ) -> Option<Frame> {
        let duration = project.duration();
        if duration == 0 {
            return None;
        }
        let still_size = self.quality.size(project, true);
        let playback_size = self.quality.size(project, false);
        let at = (*cursor).min(duration - 1);
        let mut image = None;
        if let Some(work) = &self.pending {
            match work.result.try_recv() {
                Ok(result) => {
                    let key = self.pending.take().unwrap().key.clone();
                    match result {
                        Ok(Prepared::Frame(frame, true, pixels))
                            if frame == at && !self.wants_play =>
                        {
                            self.draft_frame = Some(frame);
                            self.frame_ready = None;
                            image = Some(Frame {
                                size: media::Format::Preview.size(project),
                                pixels: Arc::new(pixels),
                            });
                        }
                        Ok(Prepared::Frame(frame, false, pixels))
                            if frame == at && !self.wants_play =>
                        {
                            self.draft_frame = None;
                            let pixels = Arc::new(pixels);
                            let retained = (CACHE_BYTES / pixels.len()).min(CACHED_FRAMES);
                            while retained > 0 && self.frames.len() >= retained {
                                self.frames.pop_front();
                            }
                            if retained > 0 {
                                self.frames.push_back((frame, pixels.clone()));
                            }
                            self.frame_ready = Some(frame);
                            image = Some(Frame {
                                size: still_size,
                                pixels,
                            });
                        }
                        Ok(Prepared::Play(section)) => {
                            self.sections.push_back(section);
                            if self.sections.len() > 5 {
                                self.sections.pop_front();
                            }
                        }
                        Ok(Prepared::Frame(_, _, _)) => {}
                        Err(error) => self.failed = Some((key, format!("{error:#}"))),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    let key = self.pending.take().unwrap().key.clone();
                    self.failed = Some((key, "The preview worker stopped.".into()));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(active) = &mut self.active {
            let update = active.player.update();
            if let Some((local, pixels)) = update.frame {
                *cursor = active.section.range.start + local;
                self.draft_frame = None;
                self.frame_ready = Some(*cursor);
                image = Some(Frame {
                    size: playback_size,
                    pixels: Arc::new(pixels),
                });
            }
            if update.finished {
                *cursor = active.section.range.end;
                self.active = self.next.take();
                if *cursor >= duration {
                    self.pause();
                    return image;
                }
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if self.wants_play
            && self.active.is_none()
            && self
                .failed
                .as_ref()
                .is_some_and(|(key, _)| matches!(key, Request::Play(_)))
        {
            self.wants_play = false;
            return image;
        }
        if self.wants_play {
            if self.active.is_none() {
                let position = (*cursor).min(duration - 1);
                if let Some(section) = self.section_at(position) {
                    self.active = self.prepare_player(ctx, section, playback_size, position);
                } else {
                    self.enqueue(
                        ctx,
                        project,
                        Request::Play(position..(position + SECTION).min(duration)),
                    );
                }
            }
            if let Some(active) = &self.active {
                let next = active.section.range.end;
                if next < duration && self.next.is_none() {
                    if let Some(section) = self.section_at(next) {
                        self.next = self.prepare_player(ctx, section, playback_size, next);
                    } else {
                        self.enqueue(
                            ctx,
                            project,
                            Request::Play(next..(next + SECTION).min(duration)),
                        );
                    }
                }
            }
            if self.active.is_some() {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        } else if self.frame_ready != Some(at) {
            if let Some(index) = self.frames.iter().position(|(frame, _)| *frame == at) {
                let (frame, pixels) = self.frames.remove(index).expect("cached frame");
                self.frames.push_back((frame, pixels.clone()));
                self.pending = None;
                self.frame_ready = Some(frame);
                self.failed = None;
                return Some(Frame {
                    size: still_size,
                    pixels,
                });
            }
            let draft = self.quality == PreviewQuality::Auto
                && still_size != playback_size
                && ctx.input(|input| input.pointer.primary_down());
            if draft && self.draft_frame == Some(at) {
                return image;
            }
            let request = if draft {
                Request::Draft(at)
            } else {
                Request::Frame(at)
            };
            let settled = match self.frame_settle {
                Some((frame, since)) if frame == at => since.elapsed() >= FRAME_SETTLE,
                _ => {
                    self.frame_settle = Some((at, Instant::now()));
                    self.pending = None;
                    self.failed = None;
                    false
                }
            };
            if settled {
                self.enqueue(ctx, project, request);
            } else {
                ctx.request_repaint_after(FRAME_SETTLE);
            }
        }
        image
    }

    fn section_at(&self, frame: u32) -> Option<Arc<Section>> {
        self.sections
            .iter()
            .rev()
            .find(|s| s.range.contains(&frame))
            .cloned()
    }

    fn prepare_player(
        &mut self,
        ctx: &egui::Context,
        section: Arc<Section>,
        size: [usize; 2],
        frame: u32,
    ) -> Option<Playing> {
        if !self.tried_audio {
            self.audio = OutputStreamBuilder::open_default_stream().ok();
            if let Some(stream) = &mut self.audio {
                stream.log_on_drop(false);
            }
            self.tried_audio = true;
        }
        match Playback::new(
            ctx,
            &section.path,
            size,
            frame - section.range.start,
            section.range.end - section.range.start,
            self.audio.as_ref(),
        ) {
            Ok(player) => Some(Playing { player, section }),
            Err(error) => {
                self.wants_play = false;
                self.failed = Some((Request::Frame(frame), format!("{error:#}")));
                None
            }
        }
    }

    fn enqueue(&mut self, ctx: &egui::Context, project: &Project, key: Request) {
        if self.pending.is_some()
            || self
                .failed
                .as_ref()
                .is_some_and(|(failed, _)| *failed == key)
        {
            return;
        }
        self.failed = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, result) = mpsc::channel();
        let snapshot = project.clone();
        let control = cancel.clone();
        let request = key.clone();
        let quality = self.quality;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let prepare = || -> Result<Prepared> {
                match request {
                    Request::Frame(frame) => Ok(Prepared::Frame(
                        frame,
                        false,
                        media::render_preview_frame_with(&snapshot, frame, quality, &control)?,
                    )),
                    Request::Draft(frame) => Ok(Prepared::Frame(
                        frame,
                        true,
                        media::render_preview_frame(&snapshot, frame, &control)?,
                    )),
                    Request::Play(range) => {
                        let folder = tempfile::tempdir()?;
                        let path = folder.path().join("section.mp4");
                        media::render_preview_with(
                            &snapshot,
                            &path,
                            range.clone(),
                            quality,
                            &control,
                        )?;
                        Ok(Prepared::Play(Arc::new(Section {
                            _folder: folder,
                            path,
                            range,
                        })))
                    }
                }
            };
            let _ = sender.send(prepare());
            ctx.request_repaint();
        });
        self.pending = Some(Pending {
            key,
            cancel,
            result,
        });
    }
}
