use crate::branding;
use crate::export_dialog::{Decision, ExportDialog};
use crate::font_browser::{Choice as FontChoice, FontBrowser};
use crate::gallery::Gallery;
use crate::preview::Preview;
use crate::recovery::{self, Autosave, Candidate};
use crate::theme::{self, ACCENT, ACCENT_DARK, BACK, LIME, LINE, MUTED, TEXT};
use crate::thumbnails::Thumbnails;
use crate::timeline::{ClipAction, MediaDrag, Placement, Selection, Timeline, View};
use anyhow::{Context, Result};
use cutad::capcut::{self, DraftImport};
use cutad::effects::{Category, Effect, Preset, Transition, TransitionKind};
use cutad::media::{self, Output, PreviewQuality};
use cutad::project::{
    self, AnimatedProperty, Asset, AssetKind, ChromaKey, ClipMask, ColorAdjustment, FPS,
    MAX_FRAMES, MaskShape, MaskStroke, Project, TEMPLATES, Title, TitleStyle, Track, TrackKind,
    Transform, VideoGeometry, timestamp,
};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, StrokeKind, vec2,
};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc::{self, Receiver},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const VIDEO_FILES: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "mpg", "mpeg", "gif",
];
const AUDIO_FILES: &[&str] = &["wav", "mp3", "m4a", "flac", "ogg", "opus", "aac", "aiff"];
const IMAGE_FILES: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "ppm"];

enum Completed {
    Media(Vec<Asset>, Vec<String>),
    Replace(String, Asset, bool),
    Sticker(Asset, u32),
    Draft(DraftImport),
    Export(PathBuf),
    PortableCopy(PathBuf),
    MatchCanvas(String, VideoGeometry),
    Subtitles(Vec<cutad::subtitles::Cue>),
    EnglishSubtitles(
        Vec<cutad::subtitles::Cue>,
        Option<Vec<cutad::subtitles::Cue>>,
    ),
    CaptionExport(PathBuf, usize),
    Scenes(project::Clip, Vec<u32>),
    Prepared(MediaPreparation, Box<project::Clip>, Asset, u8),
    Tracked(project::Clip, project::PositionAnimation),
    Loudness(project::Clip, f32, f64),
    ColorSuggested(project::Clip, ColorAdjustment, bool),
    Reframed(project::Clip, [u32; 2], f32, project::PositionAnimation),
    Voiceover(Asset, u32, usize),
    Beats(project::Clip, Vec<u32>),
    Proxy(Asset, project::PreviewProxy),
    Synced(Vec<(project::Clip, Asset)>, Vec<i32>),
}

#[derive(Clone, Copy)]
enum MediaPreparation {
    Reverse,
    InterpolateMotion,
    Stabilize,
    Cutout,
}

struct Job {
    title: &'static str,
    cancel: Arc<AtomicBool>,
    progress: Option<Arc<AtomicU8>>,
    result: Receiver<Result<Completed>>,
    worker: JoinHandle<()>,
    cancellable: bool,
}

struct VoiceoverSetup {
    devices: Vec<String>,
    monitor: bool,
    device: Option<String>,
}

struct ActiveVoiceover {
    take: crate::voiceover::Recording,
    start: u32,
    row: usize,
    monitor: bool,
    requested_playback: bool,
}

struct CopiedClipSettings {
    clip: project::Clip,
    visual: bool,
    audio: bool,
}

struct CameraEdit {
    angles: Vec<project::Clip>,
    switches: BTreeMap<u32, usize>,
    reference_audio: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobStyle {
    Read,
    Export,
    Cancellable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LibraryTab {
    Media,
    Audio,
    Text,
    Stickers,
    Effects,
    Transitions,
    Filters,
    Templates,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Video,
    Motion,
    Text,
    Color,
    Mask,
    Audio,
    Effects,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiveControl {
    Text,
    Transform,
    Framing,
    Blend,
    Motion,
    OpacityAnimation,
    RotationAnimation,
    ScaleAnimation,
    AudioGainAnimation,
    VisualFade,
    Color,
    Mask,
    Gain,
    Pitch,
    AudioChannels,
}

pub struct Editor {
    logo: egui::TextureHandle,
    project: Project,
    saved: Project,
    file: Option<PathBuf>,
    history: Vec<Project>,
    future: Vec<Project>,
    last_live_edit: Option<(LiveControl, Instant)>,
    selection: Selection,
    multi_selection: HashSet<String>,
    camera_edit: Option<CameraEdit>,
    asset_id: Option<String>,
    edit: Option<(Placement, f32)>,
    copied_clip_settings: Option<CopiedClipSettings>,
    library: LibraryTab,
    show_library: bool,
    show_inspector: bool,
    restore_panels: Option<(bool, bool)>,
    reduced_motion: bool,
    pixel_view: bool,
    match_first_video: bool,
    gallery: Gallery,
    media_query: String,
    media_kind: Option<AssetKind>,
    audio_presets: bool,
    show_transcript: bool,
    tracking_seed: cutad::tracking::Seed,
    show_tracking_patch: bool,
    mask_brush: Option<bool>,
    mask_radius: f32,
    mask_stroke: Option<MaskStroke>,
    transcript_query: String,
    inspector_tab: InspectorTab,
    transform: Transform,
    title: Option<Title>,
    font_browser: Option<FontBrowser>,
    export_dialog: Option<ExportDialog>,
    effects: Vec<Effect>,
    transition: Option<Transition>,
    frame: u32,
    zoom: f32,
    snap: bool,
    main_track_magnet: bool,
    timeline: Timeline,
    thumbnails: Thumbnails,
    preview: Preview,
    autosave: Autosave,
    recovery_candidates: Option<Vec<Candidate>>,
    recent_projects: Vec<PathBuf>,
    draft_saved: bool,
    voiceover_setup: Option<VoiceoverSetup>,
    voiceover: Option<ActiveVoiceover>,
    texture: Option<egui::TextureHandle>,
    job: Option<Job>,
    status: String,
    report: Option<(String, Vec<String>)>,
}

impl Editor {
    pub fn new(ctx: &egui::Context, path: Option<PathBuf>) -> Self {
        theme::apply(ctx);
        let mut editor = Self {
            logo: branding::logo(ctx),
            project: Project::default(),
            saved: Project::default(),
            file: None,
            history: Vec::new(),
            future: Vec::new(),
            last_live_edit: None,
            selection: Selection { row: 0, clip: None },
            multi_selection: HashSet::new(),
            camera_edit: None,
            asset_id: None,
            edit: None,
            copied_clip_settings: None,
            library: LibraryTab::Media,
            show_library: true,
            show_inspector: true,
            restore_panels: None,
            reduced_motion: false,
            pixel_view: false,
            match_first_video: true,
            gallery: Gallery::default(),
            media_query: String::new(),
            media_kind: None,
            audio_presets: false,
            show_transcript: false,
            tracking_seed: cutad::tracking::Seed::default(),
            show_tracking_patch: false,
            mask_brush: None,
            mask_radius: 0.05,
            mask_stroke: None,
            transcript_query: String::new(),
            inspector_tab: InspectorTab::Video,
            transform: Transform::default(),
            title: None,
            font_browser: None,
            export_dialog: None,
            effects: Vec::new(),
            transition: None,
            frame: 0,
            zoom: 96.0,
            snap: true,
            main_track_magnet: false,
            timeline: Timeline::default(),
            thumbnails: Thumbnails::default(),
            preview: Preview::default(),
            autosave: Autosave::default(),
            recovery_candidates: None,
            recent_projects: recovery::recent_projects(),
            draft_saved: false,
            voiceover_setup: None,
            voiceover: None,
            texture: None,
            job: None,
            status: "Import local files or start from a template.".into(),
            report: None,
        };
        if let Some(path) = path {
            editor.open_path(path);
        } else if let Ok(candidates) = recovery::candidates()
            && !candidates.is_empty()
        {
            editor.recovery_candidates = Some(candidates);
        }
        editor
    }

    fn is_dirty(&self) -> bool {
        self.project != self.saved
    }

    fn may_discard(&self) -> bool {
        !self.is_dirty()
            || MessageDialog::new()
                .set_title("Discard unsaved changes?")
                .set_description("Save the current edit first if you want to keep these changes.")
                .set_buttons(MessageButtons::YesNo)
                .show()
                == MessageDialogResult::Yes
    }

    fn stop_player(&mut self) {
        self.preview.pause();
    }

    fn collect_voiceover(&mut self, ctx: &egui::Context) {
        let Some(active) = &mut self.voiceover else {
            return;
        };
        if let Some(result) = active.take.result() {
            let active = self.voiceover.take().expect("active voiceover");
            self.stop_player();
            match result {
                Ok(Some(path)) => {
                    let (start, row) = (active.start, active.row);
                    self.start_job(ctx, "Adding voiceover", JobStyle::Read, move |_, _| {
                        Ok(Completed::Voiceover(media::probe(&path)?, start, row))
                    });
                }
                Ok(None) => self.status = "Voiceover discarded.".into(),
                Err(error) => self.error("Record voiceover", error),
            }
            return;
        }
        if active.take.ready() && !active.take.capturing() && !active.take.finishing() {
            if !active.monitor || self.preview.is_output_active() {
                active.take.begin();
            } else if !active.requested_playback {
                self.preview.play();
                active.requested_playback = true;
            } else if !self.preview.is_playing() {
                active.take.stop(true);
                self.error("Monitor timeline", anyhow::anyhow!("The preview could not start. Record with timeline playback disabled or repair the preview error."));
            }
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn voiceover_dialog(&mut self, ctx: &egui::Context) {
        if let Some(setup) = &mut self.voiceover_setup {
            let mut record = false;
            let mut cancel = false;
            let can_monitor = self.frame < self.project.duration();
            egui::Modal::new(egui::Id::new("voiceover_setup")).show(ctx, |ui| {
                ui.set_min_width(315.0);
                ui.heading("Record voiceover");
                egui::ComboBox::from_id_salt("voiceover_input")
                    .selected_text(setup.device.as_deref().unwrap_or("Default microphone"))
                    .width(280.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut setup.device, None, "Default microphone");
                        for device in &setup.devices {
                            ui.selectable_value(&mut setup.device, Some(device.clone()), device);
                        }
                    });
                ui.add_enabled(
                    can_monitor,
                    egui::Checkbox::new(&mut setup.monitor, "Play timeline while recording"),
                );
                ui.label(format!("Insert at {}", timestamp(self.frame)));
                ui.horizontal(|ui| {
                    record = ui.button("Choose file and record…").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if cancel {
                self.voiceover_setup = None;
            } else if record {
                let setup = self.voiceover_setup.take().expect("voiceover setup");
                if let Some(destination) = self.choose_export_path(
                    "Save voiceover recording",
                    "Voiceover.wav",
                    "WAV audio",
                    "wav",
                ) {
                    self.stop_player();
                    self.voiceover = Some(ActiveVoiceover {
                        take: crate::voiceover::Recording::start(
                            setup.device,
                            destination,
                            ctx.clone(),
                        ),
                        start: self.frame,
                        row: self.selection.row,
                        monitor: can_monitor && setup.monitor,
                        requested_playback: false,
                    });
                } else {
                    self.voiceover_setup = Some(setup);
                }
            }
        }
        if let Some(active) = &self.voiceover {
            let take = &active.take;
            let elapsed = take.elapsed();
            egui::Window::new("Voiceover")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::RIGHT_TOP, [-18.0, 82.0])
                .show(ctx, |ui| {
                    ui.set_min_width(212.0);
                    if take.finishing() {
                        ui.spinner();
                        ui.label("Closing recording…");
                    } else if take.capturing() {
                        ui.label(format!(
                            "Recording  {:02}:{:02}.{:01}",
                            elapsed.as_secs() / 60,
                            elapsed.as_secs() % 60,
                            elapsed.subsec_millis() / 100,
                        ));
                        ui.add(egui::ProgressBar::new(take.peak()).text("Input level"));
                    } else {
                        ui.spinner();
                        ui.label(if take.ready() {
                            "Buffering timeline…"
                        } else {
                            "Opening microphone…"
                        });
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                take.capturing() && elapsed >= Duration::from_millis(34),
                                egui::Button::new("Stop and keep"),
                            )
                            .clicked()
                        {
                            take.stop(false);
                        }
                        if ui
                            .add_enabled(!take.finishing(), egui::Button::new("Discard"))
                            .clicked()
                        {
                            take.stop(true);
                        }
                    });
                });
        }
    }

    fn set_document(&mut self, project: Project, file: Option<PathBuf>, imported: bool) {
        self.autosave.clear();
        self.draft_saved = false;
        if imported {
            self.autosave.mark_dirty();
        }
        self.match_first_video = file.is_none() && !imported;
        self.preview.pause();
        self.preview.invalidate();
        self.texture = None;
        self.frame = 0;
        self.asset_id = None;
        self.history.clear();
        self.future.clear();
        self.camera_edit = None;
        self.saved = if imported {
            Project::default()
        } else {
            project.clone()
        };
        self.project = project;
        self.file = file;
        self.select(Selection { row: 0, clip: None });
    }

    fn select(&mut self, selection: Selection) {
        self.multi_selection.clear();
        let new_clip = self.selection.clip != selection.clip;
        if new_clip {
            self.show_tracking_patch = false;
            self.mask_brush = None;
            self.mask_stroke = None;
        }
        self.selection = selection;
        self.last_live_edit = None;
        self.sync_selection();
        if new_clip && self.title.is_some() {
            self.inspector_tab = InspectorTab::Text;
            if self.restore_panels.is_none() {
                self.show_inspector = true;
            }
        } else if self.title.is_none() && self.inspector_tab == InspectorTab::Text
            || matches!(
                self.inspector_tab,
                InspectorTab::Motion | InspectorTab::Mask
            ) && self
                .edit
                .as_ref()
                .is_none_or(|(edit, _)| self.project.tracks[edit.row].kind != TrackKind::Video)
        {
            self.inspector_tab = InspectorTab::Video;
        }
    }

    fn toggle_clip_selection(&mut self, selection: Selection) {
        let Some(id) = selection.clip.clone() else {
            return;
        };
        let mut ids = std::mem::take(&mut self.multi_selection);
        if let Some(primary) = &self.selection.clip {
            ids.insert(primary.clone());
        }
        if !ids.remove(&id) {
            ids.insert(id);
            self.select(selection);
        } else if let Some((row, clip)) = ids
            .iter()
            .find_map(|id| self.project.location(id).map(|(row, _)| (row, id.clone())))
        {
            self.select(Selection {
                row,
                clip: Some(clip),
            });
        } else {
            self.select(Selection {
                row: selection.row,
                clip: None,
            });
        }
        self.multi_selection = ids;
    }

    fn group_selection(&mut self) {
        if self.multi_selection.len() < 2 {
            return;
        }
        let ids = self.multi_selection.clone();
        match self.commit(|project| project.group_clips(&ids)) {
            Ok(()) => {
                self.multi_selection.clear();
                self.status = "Grouped clips. Drag any member to move the group.".into();
            }
            Err(error) => self.error("Group clips", error),
        }
    }

    fn sync_selected_audio(&mut self, ctx: &egui::Context) -> Result<()> {
        let anchor = self
            .selection
            .clip
            .as_deref()
            .context("Select a reference clip.")?;
        let main_video = self
            .project
            .tracks
            .iter()
            .position(|track| track.kind == TrackKind::Video);
        let mut rows = HashSet::new();
        let mut clips = Vec::new();
        for id in std::iter::once(anchor).chain(
            self.multi_selection
                .iter()
                .map(String::as_str)
                .filter(|id| *id != anchor),
        ) {
            let (row, index) = self
                .project
                .location(id)
                .context("The selected clip is missing.")?;
            let clip = &self.project.tracks[row].clips[index];
            let asset = self
                .project
                .asset(&clip.asset_id)
                .context("The selected media is missing.")?;
            anyhow::ensure!(
                rows.insert(row)
                    && asset.audio
                    && !clip.freeze
                    && !clip.slot
                    && clip.speed_percent == project::NORMAL_SPEED,
                "Select one normal-speed clip with audio on each track."
            );
            anyhow::ensure!(
                id == anchor || !self.main_track_magnet || main_video != Some(row),
                "Turn off Main track magnet to move a clip on the main track."
            );
            clips.push((clip.clone(), asset.clone()));
        }
        self.stop_player();
        self.start_job(
            ctx,
            "Matching source audio",
            JobStyle::Cancellable,
            move |cancel, progress| {
                let offsets = media::audio_sync_offsets(&clips, cancel, |value| {
                    progress.store(value, Ordering::Relaxed);
                })?;
                Ok(Completed::Synced(clips, offsets))
            },
        );
        Ok(())
    }

    fn sync_selected_markers(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.main_track_magnet,
            "Turn off Main track magnet before aligning sync points."
        );
        let anchor = self
            .selection
            .clip
            .as_deref()
            .context("Select a reference clip.")?;
        let ids = self.multi_selection.clone();
        let anchor = anchor.to_owned();
        let selection: Vec<_> = std::iter::once(anchor.as_str())
            .chain(ids.iter().map(String::as_str).filter(|id| *id != anchor))
            .collect();
        self.commit(|project| project.align_clip_markers(&selection))?;
        self.status = "Aligned clips to the reference sync point.".into();
        Ok(())
    }

    fn start_camera_edit(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.main_track_magnet,
            "Turn off Main track magnet before assembling camera angles."
        );
        let mut indexed: Vec<_> = self
            .multi_selection
            .iter()
            .filter_map(|id| self.project.location(id))
            .collect();
        indexed.sort_unstable_by_key(|&(row, index)| {
            let id = self.project.tracks[row].clips[index].id.as_str();
            (self.selection.clip.as_deref() != Some(id), row, index)
        });
        let mut rows = HashSet::new();
        let mut angles = Vec::new();
        for (row, index) in indexed {
            let track = &self.project.tracks[row];
            let clip = &track.clips[index];
            anyhow::ensure!(
                rows.insert(row)
                    && track.kind == TrackKind::Video
                    && !clip.slot
                    && !clip.freeze
                    && clip.group_id.is_none()
                    && self
                        .project
                        .asset(&clip.asset_id)
                        .is_some_and(|asset| asset.kind == AssetKind::Video),
                "Select one ungrouped video clip on each camera track."
            );
            angles.push(clip.clone());
        }
        anyhow::ensure!(
            (2..=9).contains(&angles.len()),
            "Select two to nine video angles."
        );
        let reference = &angles[0];
        let reference_audio = self
            .project
            .asset(&reference.asset_id)
            .is_some_and(|asset| asset.audio);
        self.seek(reference.start);
        self.camera_edit = Some(CameraEdit {
            angles,
            switches: BTreeMap::new(),
            reference_audio,
        });
        Ok(())
    }

    fn camera_editor(&mut self, ctx: &egui::Context) {
        let Some(edit) = &mut self.camera_edit else {
            return;
        };
        let reference = &edit.angles[0];
        let start = reference.start;
        let end = reference.end();
        let mut seek = None;
        let mut apply = false;
        let mut show = true;
        egui::Window::new("Multicamera cut")
            .open(&mut show)
            .collapsible(false)
            .default_width(490.0)
            .show(ctx, |ui| {
                let at = self.frame.clamp(start, end - 1);
                let active = edit
                    .switches
                    .range(..=at)
                    .next_back()
                    .map_or(0, |(_, &index)| index);
                let active = if at < edit.angles[active].end() {
                    active
                } else {
                    0
                };
                ui.label("Choose an angle at the playhead to add a cut.");
                let mut frame = at;
                if ui
                    .add(
                        egui::Slider::new(&mut frame, start..=end - 1)
                            .show_value(false)
                            .text("Playhead"),
                    )
                    .changed()
                {
                    seek = Some(frame);
                }
                ui.label(RichText::new(timestamp(at)).monospace().color(MUTED));
                let available = ui.available_width();
                let columns = if available >= 450.0 { 3 } else { 2 };
                let tile_width = ((available - ui.spacing().item_spacing.x * (columns - 1) as f32)
                    / columns as f32)
                    .clamp(90.0, 200.0);
                for first in (0..edit.angles.len()).step_by(columns) {
                    ui.horizontal(|ui| {
                        for index in first..(first + columns).min(edit.angles.len()) {
                            let clip = &edit.angles[index];
                            let asset = self.project.asset(&clip.asset_id);
                            let label = asset.map_or("Missing media", |asset| asset.label.as_str());
                            let available = (start..end).contains(&self.frame)
                                && (clip.start..clip.end()).contains(&at)
                                && asset.and_then(|asset| asset.path.as_ref()).is_some();
                            ui.vertical(|ui| {
                                let size = vec2(tile_width, tile_width * 9.0 / 16.0);
                                let (rect, click) = ui.allocate_exact_size(size, Sense::click());
                                ui.painter().rect_filled(rect, 5, BACK);
                                if available && let Some(asset) = asset {
                                    let tick = at - clip.start;
                                    let step = FPS / 2;
                                    let tick = if self.preview.is_playing() {
                                        tick / step * step
                                    } else {
                                        tick
                                    };
                                    let source_frame = clip.source_frame_at(i64::from(tick));
                                    if let Some(texture) =
                                        self.thumbnails.image(ctx, asset, source_frame)
                                    {
                                        ui.painter().image(
                                            texture.id(),
                                            rect,
                                            egui::Rect::from_min_max(
                                                egui::Pos2::ZERO,
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            Color32::WHITE,
                                        );
                                    }
                                    if click.clicked() {
                                        edit.switches.insert(at, index);
                                    }
                                }
                                if available && active == index {
                                    ui.painter().rect_stroke(
                                        rect,
                                        5,
                                        Stroke::new(2.0, ACCENT),
                                        StrokeKind::Inside,
                                    );
                                }
                                let text = if available {
                                    label
                                } else {
                                    "Outside angle range"
                                };
                                ui.add_sized([tile_width, 18.0], egui::Label::new(text).truncate())
                                    .on_hover_text(label);
                            });
                        }
                    });
                }
                ui.separator();
                if self
                    .project
                    .asset(&reference.asset_id)
                    .is_some_and(|asset| asset.audio)
                {
                    ui.checkbox(&mut edit.reference_audio, "Use reference audio throughout");
                }
                let mut remove = None;
                egui::ScrollArea::vertical()
                    .max_height(115.0)
                    .show(ui, |ui| {
                        for (&time, &angle) in &edit.switches {
                            ui.horizontal(|ui| {
                                let name = self
                                    .project
                                    .asset(&edit.angles[angle].asset_id)
                                    .map_or("Missing media", |asset| asset.label.as_str());
                                if ui.button(format!("{}  {name}", timestamp(time))).clicked() {
                                    seek = Some(time);
                                }
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(time);
                                }
                            });
                        }
                    });
                if let Some(remove) = remove {
                    edit.switches.remove(&remove);
                }
                apply = ui
                    .add_enabled(
                        !edit.switches.is_empty(),
                        egui::Button::new("Apply angle cuts"),
                    )
                    .clicked();
            });
        if let Some(seek) = seek {
            self.seek(seek);
        }
        if !show {
            self.camera_edit = None;
        } else if apply {
            let edit = self.camera_edit.take().expect("camera edit is open");
            let magnet = self.main_track_magnet;
            let result = self.commit(|project| {
                anyhow::ensure!(
                    !magnet,
                    "Turn off Main track magnet before assembling camera angles."
                );
                anyhow::ensure!(!edit.switches.is_empty(), "Choose at least one camera cut.");
                project.assemble_camera_angles(&edit.angles, &edit.switches, edit.reference_audio)
            });
            match result {
                Ok(id) => {
                    self.seek(start);
                    self.select_inserted(Ok(id), "Applied angle cuts");
                }
                Err(error) => {
                    self.camera_edit = Some(edit);
                    self.error("Apply angle cuts", error);
                }
            }
        }
    }

    fn sync_selection(&mut self) {
        self.effects.clear();
        self.transition = None;
        self.transform = Transform::default();
        self.title = None;
        self.edit = self.selection.clip.as_deref().and_then(|id| {
            let (row, index) = self.project.location(id)?;
            let c = &self.project.tracks[row].clips[index];
            self.effects.clone_from(&c.effects);
            self.transition = c.transition;
            self.transform = c.transform;
            self.title = self
                .project
                .asset(&c.asset_id)
                .and_then(|asset| asset.title.clone());
            self.selection.row = row;
            Some((
                Placement {
                    id: id.into(),
                    row,
                    start: c.start,
                    source: c.source,
                    length: c.length,
                },
                c.gain,
            ))
        });
        if self.edit.is_none() {
            self.selection.clip = None;
            self.selection.row = self.selection.row.min(self.project.tracks.len() - 1);
        }
    }

    fn commit<T>(&mut self, change: impl FnOnce(&mut Project) -> Result<T>) -> Result<T> {
        self.last_live_edit = None;
        self.update_project(false, change)
    }

    fn commit_live(
        &mut self,
        control: LiveControl,
        change: impl FnOnce(&mut Project) -> Result<()>,
    ) -> Result<()> {
        let now = Instant::now();
        let coalesce = self.last_live_edit.is_some_and(|(last, at)| {
            last == control && now.duration_since(at) < Duration::from_millis(800)
        });
        self.update_project(coalesce, change)?;
        self.last_live_edit = Some((control, now));
        Ok(())
    }

    fn update_project<T>(
        &mut self,
        coalesce: bool,
        change: impl FnOnce(&mut Project) -> Result<T>,
    ) -> Result<T> {
        let mut updated = self.project.clone();
        let output = change(&mut updated)?;
        updated.validate()?;
        if self.main_track_magnet
            && let Some(row) = updated
                .tracks
                .iter()
                .position(|track| track.kind == TrackKind::Video)
            && updated.tracks[row]
                .clips
                .iter()
                .map(|clip| (&clip.id, clip.start, clip.length))
                .ne(self
                    .project
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Video)
                    .into_iter()
                    .flat_map(|track| &track.clips)
                    .map(|clip| (&clip.id, clip.start, clip.length)))
        {
            updated.close_track_gaps(row)?;
        }
        if self.project != updated {
            let previous = std::mem::replace(&mut self.project, updated);
            if !coalesce {
                self.history.push(previous);
                if self.history.len() > 100 {
                    self.history.remove(0);
                }
            }
            self.future.clear();
            self.changed();
        }
        Ok(output)
    }

    fn changed(&mut self) {
        if self.is_dirty() {
            self.autosave.mark_dirty();
        } else {
            self.autosave.clear();
            self.draft_saved = false;
        }
        self.preview.invalidate();
        let duration = self.project.duration();
        if duration == 0 {
            self.texture = None;
        }
        self.frame = self.frame.min(duration);
        self.sync_selection();
    }

    fn undo(&mut self) {
        self.last_live_edit = None;
        if let Some(project) = self.history.pop() {
            self.future
                .push(std::mem::replace(&mut self.project, project));
            self.changed();
            self.status = "Undid last change.".into();
        }
    }

    fn redo(&mut self) {
        self.last_live_edit = None;
        if let Some(project) = self.future.pop() {
            self.history
                .push(std::mem::replace(&mut self.project, project));
            self.changed();
            self.status = "Redid last change.".into();
        }
    }

    fn error(&mut self, title: &str, error: anyhow::Error) {
        let message = format!("{error:#}");
        self.status = message.lines().next().unwrap_or_default().to_owned();
        self.report = Some((title.into(), vec![message]));
    }

    fn open_path(&mut self, path: PathBuf) {
        match project::load(&path) {
            Ok(project) => {
                let is_template = path.extension().is_some_and(|ext| ext == "cutad-template");
                self.status = format!("Opened {}.", project.name);
                if !is_template && let Ok(recent) = recovery::remember_project(&path) {
                    self.recent_projects = recent;
                }
                self.set_document(project, (!is_template).then_some(path), is_template);
            }
            Err(error) => self.error("Open project", error),
        }
    }

    fn open(&mut self) {
        if self.may_discard()
            && let Some(path) = FileDialog::new()
                .add_filter("CutAd projects and templates", &["cutad", "cutad-template"])
                .pick_file()
        {
            self.open_path(path);
        }
    }

    fn save(&mut self, save_as: bool) {
        self.last_live_edit = None;
        let path = if save_as { None } else { self.file.clone() };
        let path = path.or_else(|| {
            FileDialog::new()
                .add_filter("CutAd project", &["cutad"])
                .set_file_name("Project.cutad")
                .save_file()
        });
        let Some(path) = path else {
            return;
        };
        let path = with_extension(path, "cutad");
        match project::save(&self.project, &path) {
            Ok(()) => {
                self.status = format!("Saved {}.", filename(&path));
                self.saved = self.project.clone();
                if let Ok(recent) = recovery::remember_project(&path) {
                    self.recent_projects = recent;
                }
                self.file = Some(path);
                self.autosave.clear();
                self.draft_saved = false;
            }
            Err(error) => self.error("Save project", error),
        }
    }

    fn save_template(&mut self) {
        if let Some(path) = FileDialog::new()
            .add_filter("CutAd template", &["cutad-template"])
            .set_file_name("My template.cutad-template")
            .save_file()
        {
            let path = with_extension(path, "cutad-template");
            match project::save(&project::as_template(&self.project), &path) {
                Ok(()) => self.status = format!("Saved template {}.", filename(&path)),
                Err(error) => self.error("Save template", error),
            }
        }
    }

    fn recovery_dialog(&mut self, ctx: &egui::Context) {
        let Some(candidates) = &self.recovery_candidates else {
            return;
        };
        let mut selected = None;
        let mut close = false;
        egui::Modal::new(egui::Id::new("recover_draft")).show(ctx, |ui| {
            ui.set_min_width(335.0);
            ui.heading("Recover an autosaved draft");
            if candidates.is_empty() {
                ui.label("No recovery drafts are available.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(290.0)
                    .show(ui, |ui| {
                        for (index, candidate) in candidates.iter().enumerate() {
                            let age = candidate.modified.elapsed().unwrap_or_default().as_secs();
                            let age = if age < 60 {
                                "Less than a minute ago".to_owned()
                            } else if age < 3600 {
                                format!("{} min ago", age / 60)
                            } else if age < 86_400 {
                                format!("{} h ago", age / 3600)
                            } else {
                                format!("{} d ago", age / 86_400)
                            };
                            ui.horizontal(|ui| {
                                if ui.button(&candidate.name).clicked() {
                                    selected = Some(index);
                                }
                                ui.label(RichText::new(age).small().color(MUTED));
                            });
                        }
                    });
            }
            close = ui.button("Close").clicked();
        });
        if close {
            self.recovery_candidates = None;
        } else if let Some(index) = selected
            && self.may_discard()
        {
            let choices = self.recovery_candidates.take().expect("recovery list");
            match Autosave::recover(&choices[index]) {
                Ok((autosave, project)) => {
                    self.set_document(project, None, true);
                    self.autosave = autosave;
                    self.draft_saved = true;
                    self.status = "Recovered a draft. Use Save to give it a project file.".into();
                }
                Err(error) => self.error("Recover draft", error),
            }
        }
    }

    fn use_template(&mut self, name: &str) {
        if self.may_discard()
            && let Ok(project) = project::template(name)
        {
            self.set_document(project, None, true);
            self.status = "Select a slot and use Replace media.".into();
        }
    }

    fn start_job(
        &mut self,
        ctx: &egui::Context,
        title: &'static str,
        style: JobStyle,
        work: impl FnOnce(&AtomicBool, &AtomicU8) -> Result<Completed> + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let (sender, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU8::new(0));
        let flags = (cancel.clone(), progress.clone());
        let ctx = ctx.clone();
        let worker = std::thread::spawn(move || {
            let completion = work(&flags.0, &flags.1);
            let _ = sender.send(completion);
            ctx.request_repaint();
        });
        self.job = Some(Job {
            title,
            cancel,
            progress: (style == JobStyle::Export).then_some(progress),
            cancellable: style != JobStyle::Read,
            result,
            worker,
        });
    }

    fn import_media(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.start_job(ctx, "Reading media", JobStyle::Read, move |_, _| {
            let mut imported = Vec::new();
            let mut failures = Vec::new();
            for path in paths {
                match media::probe(&path) {
                    Ok(asset) => imported.push(asset),
                    Err(error) => failures.push(format!("{} · {error:#}", filename(&path))),
                }
            }
            Ok(Completed::Media(imported, failures))
        });
    }

    fn import_draft(&mut self, ctx: &egui::Context, with_media_folder: bool) {
        if !self.may_discard() {
            return;
        }
        let draft = FileDialog::new()
            .add_filter("CapCut draft JSON", &["json", "capcut"])
            .set_title("Choose a local CapCut draft")
            .pick_file();
        let Some(draft) = draft else {
            return;
        };
        let folder = if with_media_folder {
            let folder = FileDialog::new()
                .set_title("Choose the folder containing your source media")
                .pick_folder();
            if folder.is_none() {
                return;
            }
            folder
        } else {
            None
        };
        self.start_job(ctx, "Importing draft", JobStyle::Read, move |_, _| {
            Ok(Completed::Draft(capcut::import(
                &draft,
                folder.as_deref(),
                media::probe,
            )?))
        });
    }

    fn replace_media(&mut self, ctx: &egui::Context, keep_source: bool) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        if let Some(path) = media_dialog()
            .set_title("Choose replacement media")
            .pick_file()
        {
            self.start_job(
                ctx,
                "Reading replacement media",
                JobStyle::Read,
                move |_, _| Ok(Completed::Replace(id, media::probe(&path)?, keep_source)),
            );
        }
    }

    fn insert(&mut self, id: &str) {
        self.insert_at(id, self.selection.row, self.frame);
    }

    fn insert_at(&mut self, id: &str, row: usize, frame: u32) {
        let adopt = self.match_first_video && self.project.source_video().is_none();
        let size = adopt
            .then(|| self.project.asset(id).and_then(|asset| asset.geometry))
            .flatten()
            .map(VideoGeometry::canvas);
        match self.commit(|project| {
            let clip = project.insert_asset(row, id, frame)?;
            if let Some((width, height)) = size {
                (project.width, project.height) = (width, height);
            }
            Ok(clip)
        }) {
            Ok(clip) => {
                self.select(Selection {
                    row,
                    clip: Some(clip),
                });
                self.status = "Added clip to the timeline.".into();
            }
            Err(error) => self.error("Add to timeline", error),
        }
    }

    fn apply_placement(&mut self, placement: Placement, gain: f32) {
        let p = placement;
        match self.commit(|project| {
            let (row, index) = project.location(&p.id).context("Select a clip.")?;
            let clip = &project.tracks[row].clips[index];
            if clip.group_id.is_some() && p.source == clip.source && p.length == clip.length {
                project.move_clip_group(&p.id, p.row, p.start)
            } else {
                project.update_clip(&p.id, p.row, p.start, p.source, p.length, gain)
            }
        }) {
            Ok(()) => self.select(Selection {
                row: p.row,
                clip: Some(p.id),
            }),
            Err(error) => {
                self.error("Edit clip", error);
                self.sync_selection();
            }
        }
    }

    fn split(&mut self) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        let frame = self.frame;
        match self.commit(|project| project.split(&id, frame)) {
            Ok(id) => self.select(Selection {
                row: self.selection.row,
                clip: Some(id),
            }),
            Err(error) => self.error("Split clip", error),
        }
    }

    fn delete(&mut self) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        let selection = self.multi_selection.clone();
        let result = self.commit(|project| {
            for track in &mut project.tracks {
                track
                    .clips
                    .retain(|clip| clip.id != id && !selection.contains(&clip.id));
            }
            Ok(())
        });
        if let Err(error) = result {
            self.error("Remove clip", error);
        } else {
            self.multi_selection.clear();
        }
    }

    fn clip_action(&mut self, ctx: &egui::Context, action: ClipAction) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        match action {
            ClipAction::Split => self.split(),
            ClipAction::Reverse => self.prepare_selected_clip(ctx, &id, MediaPreparation::Reverse),
            ClipAction::InterpolateMotion => {
                self.prepare_selected_clip(ctx, &id, MediaPreparation::InterpolateMotion)
            }
            ClipAction::Stabilize => {
                self.prepare_selected_clip(ctx, &id, MediaPreparation::Stabilize)
            }
            ClipAction::CreateProxy => self.create_proxy(ctx, &id),
            ClipAction::UnlinkProxy => {
                let result = self.commit(|project| {
                    let (row, index) = project.location(&id).context("Select a video clip.")?;
                    let asset_id = project.tracks[row].clips[index].asset_id.clone();
                    let asset = project
                        .assets
                        .iter_mut()
                        .find(|asset| asset.id == asset_id)
                        .context("The video source is missing.")?;
                    asset.proxy = None;
                    Ok(())
                });
                match result {
                    Ok(()) => self.status = "Unlinked the preview proxy.".into(),
                    Err(error) => self.error("Unlink preview proxy", error),
                }
            }
            ClipAction::SetSyncPoint | ClipAction::RemoveSyncPoint => {
                let frame = self.frame;
                let result = self.commit(|project| {
                    let (row, index) = project.location(&id).context("Select a clip.")?;
                    let clip = &mut project.tracks[row].clips[index];
                    clip.sync_source = if matches!(action, ClipAction::RemoveSyncPoint) {
                        None
                    } else {
                        anyhow::ensure!(
                            (clip.start..clip.end()).contains(&frame) && !clip.freeze && !clip.slot,
                            "Put the playhead inside the selected clip."
                        );
                        Some(u32::try_from(
                            clip.source_units_at(i64::from(frame - clip.start)),
                        )?)
                    };
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        self.status = if matches!(action, ClipAction::RemoveSyncPoint) {
                            "Removed the sync point.".into()
                        } else {
                            format!("Set a clip sync point at {}.", timestamp(frame))
                        };
                    }
                    Err(error) => self.error("Edit clip sync point", error),
                }
            }
            ClipAction::RemoveBackground => {
                self.prepare_selected_clip(ctx, &id, MediaPreparation::Cutout)
            }
            ClipAction::Freeze => {
                let frame = self.frame;
                let row = self.selection.row;
                match self.commit(|project| project.insert_freeze(&id, frame, 3 * FPS)) {
                    Ok(clip) => {
                        self.select(Selection {
                            row,
                            clip: Some(clip),
                        });
                        self.status = "Inserted a 3-second freeze frame.".into();
                    }
                    Err(error) => self.error("Insert freeze frame", error),
                }
            }
            ClipAction::SplitScenes => {
                let Some((row, index)) = self.project.location(&id) else {
                    return;
                };
                let clip = self.project.tracks[row].clips[index].clone();
                let Some(path) = self
                    .project
                    .asset(&clip.asset_id)
                    .and_then(|asset| asset.path.clone())
                else {
                    self.status = "Relink the video before detecting scenes.".into();
                    return;
                };
                self.start_job(
                    ctx,
                    "Detecting scene changes",
                    JobStyle::Cancellable,
                    move |cancel, _| {
                        let cuts = media::detect_scene_cuts(
                            &path,
                            clip.source,
                            clip.length,
                            cancel,
                            |_| {},
                        )?;
                        Ok(Completed::Scenes(clip, cuts))
                    },
                );
            }
            ClipAction::Remove => self.delete(),
            ClipAction::CopySettings => {
                let Some((row, index)) = self.project.location(&id) else {
                    return;
                };
                let clip = &self.project.tracks[row].clips[index];
                let (visual, audio) = Self::clip_setting_types(&self.project, row, clip);
                if visual || audio {
                    self.copied_clip_settings = Some(CopiedClipSettings {
                        clip: clip.clone(),
                        visual,
                        audio,
                    });
                    self.status = "Copied clip settings.".into();
                }
            }
            ClipAction::PasteSettings => {
                let Some(settings) = self.copied_clip_settings.take() else {
                    return;
                };
                let result = self.commit(|project| {
                    let (row, index) = project.location(&id).context("Select a clip.")?;
                    let (visual, audio) =
                        Self::clip_setting_types(project, row, &project.tracks[row].clips[index]);
                    let (visual, audio) = (visual && settings.visual, audio && settings.audio);
                    anyhow::ensure!(
                        visual || audio,
                        "The selected clip has no compatible settings."
                    );
                    project.tracks[row].clips[index].paste_settings(&settings.clip, visual, audio);
                    Ok(())
                });
                self.copied_clip_settings = Some(settings);
                match result {
                    Ok(()) => self.status = "Pasted clip settings.".into(),
                    Err(error) => self.error("Paste clip settings", error),
                }
            }
            ClipAction::RippleDelete => match self.commit(|project| project.ripple_delete(&id)) {
                Ok(frame) => {
                    self.seek(frame.min(self.project.duration()));
                    self.status = "Removed the clip and closed its span on this track.".into();
                }
                Err(error) => self.error("Ripple delete", error),
            },
            ClipAction::Replace => self.replace_media(ctx, false),
            ClipAction::Effects => {
                self.library = LibraryTab::Effects;
                self.inspector_tab = InspectorTab::Effects;
            }
            ClipAction::TrimLeft | ClipAction::TrimRight => {
                if let Some((mut edit, gain)) = self.edit.clone() {
                    let offset = self.frame.saturating_sub(edit.start);
                    if offset == 0 || offset >= edit.length {
                        return;
                    }
                    if matches!(action, ClipAction::TrimLeft) {
                        edit.start = self.frame;
                        if let Some((row, index)) = self.project.location(&id) {
                            edit.source = self.project.tracks[row].clips[index]
                                .source_frame_at(i64::from(offset));
                        }
                        edit.length -= offset;
                    } else {
                        edit.length = offset;
                    }
                    self.apply_placement(edit, gain);
                }
            }
            ClipAction::Duplicate => {
                let result = self.commit(|project| {
                    let (row, i) = project.location(&id).context("Select a clip.")?;
                    let track = &mut project.tracks[row];
                    let mut copy = track.clips[i].clone();
                    copy.start = track
                        .clips
                        .iter()
                        .map(|c| c.end())
                        .max()
                        .unwrap_or_default();
                    copy.id = project::new_id();
                    copy.group_id = None;
                    let id = copy.id.clone();
                    track.clips.push(copy);
                    Ok((row, id))
                });
                match result {
                    Ok((row, clip)) => self.select(Selection {
                        row,
                        clip: Some(clip),
                    }),
                    Err(error) => self.error("Duplicate clip", error),
                }
            }
            ClipAction::Ungroup => match self.commit(|project| project.ungroup_clip(&id)) {
                Ok(()) => self.status = "Ungrouped clips.".into(),
                Err(error) => self.error("Ungroup clips", error),
            },
            ClipAction::DetachAudio => {
                let result = self.commit(|project| {
                    let (row, index) = project.location(&id).context("Select a clip.")?;
                    let original = &mut project.tracks[row].clips[index];
                    let mut audio = original.clone();
                    audio.id = project::new_id();
                    audio.transition = None;
                    audio.effects.retain(|effect| effect.preset.audio());
                    original.gain = 0.0;
                    original.gain_animation = Default::default();
                    original.effects.retain(|effect| !effect.preset.audio());
                    let index = project.tracks.len();
                    let mut track = Track::new("Detached audio", TrackKind::Audio);
                    let id = audio.id.clone();
                    track.clips.push(audio);
                    project.tracks.push(track);
                    Ok((index, id))
                });
                match result {
                    Ok((row, clip)) => self.select(Selection {
                        row,
                        clip: Some(clip),
                    }),
                    Err(error) => self.error("Detach audio", error),
                }
            }
        }
    }

    fn create_proxy(&mut self, ctx: &egui::Context, id: &str) {
        let Some((row, index)) = self.project.location(id) else {
            return;
        };
        let clip = &self.project.tracks[row].clips[index];
        let Some(asset) = self.project.asset(&clip.asset_id).cloned() else {
            return;
        };
        let Some(destination) = self.choose_export_path(
            "Save preview proxy",
            "Preview proxy.mp4",
            "MP4 video",
            "mp4",
        ) else {
            return;
        };
        self.stop_player();
        self.start_job(
            ctx,
            "Encoding preview proxy",
            JobStyle::Export,
            move |cancel, progress| {
                let proxy = media::create_preview_proxy(&asset, &destination, cancel, |value| {
                    progress.store(value, Ordering::Relaxed);
                })?;
                Ok(Completed::Proxy(asset, proxy))
            },
        );
    }

    fn prepare_selected_clip(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        preparation: MediaPreparation,
    ) {
        let Some((row, index)) = self.project.location(id) else {
            return;
        };
        let clip = self.project.tracks[row].clips[index].clone();
        let Some(asset) = self.project.asset(&clip.asset_id).cloned() else {
            return;
        };
        let (title, filename, operation) = match preparation {
            MediaPreparation::Reverse => {
                ("Save reversed clip", "Reversed clip.mkv", "Reversing clip")
            }
            MediaPreparation::InterpolateMotion => (
                "Save interpolated clip",
                "Optical flow.mkv",
                "Generating in-between frames",
            ),
            MediaPreparation::Stabilize => (
                "Save stabilized clip",
                "Stabilized clip.mkv",
                "Stabilizing camera motion",
            ),
            MediaPreparation::Cutout => (
                "Save transparent cutout",
                if asset.kind == AssetKind::Image {
                    "Cutout.png"
                } else {
                    "Cutout.mkv"
                },
                "Removing background",
            ),
        };
        let model = if matches!(preparation, MediaPreparation::Cutout) {
            let Some(model) = choose_subject_model() else {
                return;
            };
            Some(model)
        } else {
            None
        };
        let (kind, extension) = if filename.ends_with(".png") {
            ("PNG image", "png")
        } else {
            ("Matroska media", "mkv")
        };
        let Some(path) = self.choose_export_path(title, filename, kind, extension) else {
            return;
        };
        self.stop_player();
        self.start_job(ctx, operation, JobStyle::Export, move |cancel, progress| {
            let report = |value| progress.store(value, Ordering::Relaxed);
            let (asset, subframe) = match preparation {
                MediaPreparation::Reverse => {
                    media::reverse_clip(&clip, &asset, &path, cancel, report)?
                }
                MediaPreparation::InterpolateMotion => {
                    media::interpolate_clip_motion(&clip, &asset, &path, cancel, report)?
                }
                MediaPreparation::Stabilize => {
                    media::stabilize_clip(&clip, &asset, &path, cancel, report)?
                }
                MediaPreparation::Cutout => {
                    media::remove_background(
                        &clip, &asset, &path, &subject_python(),
                        &model.context("Choose a local U²-Net ONNX model.")?,
                        cancel, report,
                    ).context("Background removal needs Python with rembg[cpu] installed. Set CUTAD_CUTOUT_PYTHON to that interpreter if needed.")?
                }
            };
            Ok(Completed::Prepared(preparation, Box::new(clip), asset, subframe))
        });
    }

    fn auto_reframe(&mut self, ctx: &egui::Context, clip: project::Clip) {
        let Some(asset) = self.project.asset(&clip.asset_id).cloned() else {
            return;
        };
        let Some(model) = choose_subject_model() else {
            return;
        };
        let canvas = [self.project.width, self.project.height];
        self.stop_player();
        self.start_job(
            ctx,
            "Finding the subject",
            JobStyle::Cancellable,
            move |cancel, _| {
                let (scale, animation) = media::reframe_subject(
                    &clip,
                    &asset,
                    canvas,
                    &subject_python(),
                    &model,
                    cancel,
                )?;
                Ok(Completed::Reframed(clip, canvas, scale, animation))
            },
        );
    }

    fn clip_setting_types(project: &Project, row: usize, clip: &project::Clip) -> (bool, bool) {
        (
            project.tracks[row].kind == TrackKind::Video,
            !clip.freeze
                && project
                    .asset(&clip.asset_id)
                    .is_some_and(|asset| asset.audio),
        )
    }

    fn add_track(&mut self, kind: TrackKind) {
        let result = self.commit(|project| {
            let number = project
                .tracks
                .iter()
                .filter(|track| track.kind == kind)
                .count()
                + 1;
            let index = project.tracks.len();
            project
                .tracks
                .push(Track::new(format!("{} {number}", kind.label()), kind));
            Ok(index)
        });
        if let Ok(row) = result {
            self.select(Selection { row, clip: None });
        }
    }

    fn seek(&mut self, frame: u32) {
        self.stop_player();
        self.frame = frame.min(self.project.duration());
    }

    fn play(&mut self, ctx: &egui::Context) {
        if self.preview.is_playing() {
            self.stop_player();
        } else if self.project.duration() > 0 {
            if self.frame >= self.project.duration() {
                self.frame = 0;
            }
            self.preview.play();
        }
        ctx.request_repaint();
    }

    fn export(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        settings: Output,
        range: std::ops::Range<u32>,
        details: media::ExportDetails,
    ) {
        if self
            .file
            .as_ref()
            .is_some_and(|file| project::same_path(file, &path))
        {
            self.error(
                "Export video",
                anyhow::anyhow!("Choose an output separate from the saved project."),
            );
            return;
        }
        self.stop_player();
        let snapshot = self.project.clone();
        self.start_job(
            ctx,
            "Exporting video",
            JobStyle::Export,
            move |cancel, progress| {
                media::render_with_details(
                    &snapshot,
                    &path,
                    settings,
                    range,
                    &details,
                    cancel,
                    |value| progress.store(value, Ordering::Relaxed),
                )?;
                Ok(Completed::Export(path))
            },
        );
    }

    fn collect_work(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.job {
            match job.result.try_recv() {
                Ok(result) => {
                    let job = self.job.take().unwrap();
                    let _ = job.worker.join();
                    match result {
                        Ok(result) => self.complete(result),
                        Err(_) if job.cancel.load(Ordering::Relaxed) => {
                            self.status = "Render canceled.".into()
                        }
                        Err(error) => self.error(job.title, error),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.job = None;
                    self.error(
                        "Editor operation",
                        anyhow::anyhow!("The background worker stopped unexpectedly."),
                    );
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100))
                }
            }
        }
        if self.job.is_none()
            && let Some(frame) = self.preview.update(ctx, &self.project, &mut self.frame)
        {
            self.update_texture(ctx, frame.size, &frame.pixels);
        }
    }

    fn open_fonts(&mut self, ctx: &egui::Context) {
        let font = self
            .title
            .as_ref()
            .and_then(|title| title.appearance.font.clone());
        self.font_browser = Some(FontBrowser::new(ctx, font, self.title.is_some()));
    }

    fn apply_font(&mut self, font: Option<cutad::fonts::FontRef>) {
        if let Some(mut title) = self.title.clone() {
            self.last_live_edit = None;
            title.appearance.font = font;
            self.apply_title(title);
            self.last_live_edit = None;
        }
    }

    fn apply_title(&mut self, title: Title) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        let result = self.commit_live(LiveControl::Text, |project| {
            project.update_title(&id, title)
        });
        if let Err(error) = result {
            self.error("Edit text", error);
        }
    }

    fn update_selected_clip(
        &mut self,
        control: LiveControl,
        update: impl FnOnce(&mut project::Clip),
    ) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        if let Err(error) = self.commit_live(control, |project| {
            let (row, index) = project.location(&id).context("Select a clip.")?;
            update(&mut project.tracks[row].clips[index]);
            Ok(())
        }) {
            self.error("Edit clip", error);
        }
    }

    fn complete(&mut self, result: Completed) {
        match result {
            Completed::Proxy(original, proxy) => {
                let result = self.commit(|project| {
                    let asset = project
                        .assets
                        .iter_mut()
                        .find(|asset| asset.id == original.id)
                        .context("The video source is no longer in the project.")?;
                    anyhow::ensure!(
                        asset.path == original.path,
                        "The video was relinked during encoding."
                    );
                    asset.proxy = Some(proxy);
                    Ok(())
                });
                match result {
                    Ok(()) => self.status = "Linked a proxy for reduced-resolution preview.".into(),
                    Err(error) => self.error("Link preview proxy", error),
                }
            }
            Completed::Media(assets, failures) => {
                let first = assets.first().map(|asset| asset.id.clone());
                let count = assets.len();
                if let Err(error) = self.commit(move |project| {
                    project.assets.extend(assets);
                    Ok(())
                }) {
                    self.error("Import media", error);
                    return;
                }
                self.asset_id = first;
                self.status = format!(
                    "Imported {count} media file{}.",
                    if count == 1 { "" } else { "s" }
                );
                if !failures.is_empty() {
                    self.report = Some(("Some files could not be imported".into(), failures));
                }
            }
            Completed::Replace(id, asset, keep_source) => {
                let result = self.commit(|project| {
                    let (row, index) = project
                        .location(&id)
                        .context("The clip is no longer available.")?;
                    let clip = &project.tracks[row].clips[index];
                    let source = clip.source;
                    let subframe = clip.source_subframe;
                    project.replace_media(&id, asset)?;
                    if keep_source {
                        let clip = &mut project.tracks[row].clips[index];
                        clip.source = source;
                        clip.source_subframe = subframe;
                    }
                    Ok(())
                });
                match result {
                    Ok(()) => self.status = "Updated clip media.".into(),
                    Err(error) => self.error("Replace media", error),
                }
            }
            Completed::Sticker(asset, frame) => {
                let result = self.commit(|project| project.add_sticker(asset, frame));
                self.select_inserted(result, "Imported sticker");
            }
            Completed::Draft(import) => {
                self.set_document(import.project, None, true);
                self.status = "Imported a local draft. Save it as a CutAd project.".into();
                if !import.warnings.is_empty() {
                    self.report = Some(("Draft import report".into(), import.warnings));
                }
            }
            Completed::Export(path) => self.status = format!("Exported {}.", filename(&path)),
            Completed::PortableCopy(path) => {
                self.status = format!("Saved portable project to {}", path.display());
            }
            Completed::Synced(originals, offsets) => {
                let positions: Vec<_> = originals
                    .iter()
                    .zip(offsets)
                    .map(|((clip, _), offset)| (clip.id.as_str(), offset))
                    .collect();
                match self.commit(|project| {
                    for (clip, asset) in &originals {
                        let (row, index) = project
                            .location(&clip.id)
                            .context("A synchronized clip is missing.")?;
                        anyhow::ensure!(
                            project.tracks[row].clips[index] == *clip
                                && project.asset(&clip.asset_id) == Some(asset),
                            "A clip changed during audio synchronization."
                        );
                    }
                    project.align_clip_starts(&positions)
                }) {
                    Ok(()) => self.status = "Aligned clips to the reference audio.".into(),
                    Err(error) => self.error("Align clips", error),
                }
            }
            Completed::CaptionExport(path, count) => {
                self.status = format!("Exported {count} captions to {}.", filename(&path));
            }
            Completed::Prepared(preparation, original, asset, subframe) => {
                let label = asset.label.clone();
                let result = self.commit(move |project| {
                    let (row, index) = project
                        .location(&original.id)
                        .context("The selected clip is no longer available.")?;
                    anyhow::ensure!(
                        project.tracks[row].clips[index] == *original,
                        "The clip changed while preparing its new media."
                    );
                    project.replace_media(&original.id, asset)?;
                    project.tracks[row].clips[index].source_subframe = subframe;
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        self.status = match preparation {
                            MediaPreparation::Reverse => format!("Reversed clip using {label}."),
                            MediaPreparation::InterpolateMotion => {
                                format!("Applied 120 fps optical flow using {label}.")
                            }
                            MediaPreparation::Stabilize => {
                                format!("Stabilized clip using {label}.")
                            }
                            MediaPreparation::Cutout => {
                                format!("Removed the background and saved {label}.")
                            }
                        }
                    }
                    Err(error) => self.error("Apply prepared media", error),
                }
            }
            Completed::Loudness(original, gain, lufs) => {
                let has_keyframes = !original.gain_animation.is_empty();
                let result = self.commit(|project| {
                    let (row, index) = project
                        .location(&original.id)
                        .context("The analyzed clip is no longer available.")?;
                    let clip = &mut project.tracks[row].clips[index];
                    anyhow::ensure!(*clip == original, "The clip changed during analysis.");
                    if has_keyframes {
                        for key in &mut clip.gain_animation.keys {
                            key.value = (key.value * gain).min(4.0);
                        }
                    } else {
                        clip.gain = gain;
                    }
                    Ok(())
                });
                match result {
                    Ok(()) if has_keyframes => {
                        self.status = format!(
                            "Scaled gain keyframes by {gain:.2}×. Estimated loudness {lufs:.1} LUFS."
                        );
                    }
                    Ok(()) => {
                        self.status =
                            format!("Set gain to {gain:.2}×. Estimated loudness {lufs:.1} LUFS.")
                    }
                    Err(error) => self.error("Normalize loudness", error),
                }
            }
            Completed::ColorSuggested(original, adjustment, matched) => {
                let result = self.commit(|project| {
                    let (row, index) = project
                        .location(&original.id)
                        .context("The analyzed clip is no longer available.")?;
                    let clip = &mut project.tracks[row].clips[index];
                    anyhow::ensure!(*clip == original, "The clip changed during analysis.");
                    clip.adjustment = adjustment;
                    Ok(())
                });
                match result {
                    Ok(()) if matched => self.status = "Applied the reference tone.".into(),
                    Ok(()) => self.status = "Applied automatic color correction.".into(),
                    Err(error) => self.error("Apply color correction", error),
                }
            }
            Completed::Reframed(original, canvas, scale, animation) => {
                let result = self.commit(|project| {
                    anyhow::ensure!(
                        [project.width, project.height] == canvas,
                        "The canvas changed during analysis."
                    );
                    let (row, index) = project
                        .location(&original.id)
                        .context("The analyzed clip is no longer available.")?;
                    let clip = &mut project.tracks[row].clips[index];
                    anyhow::ensure!(*clip == original, "The clip changed during analysis.");
                    clip.transform.scale = scale;
                    clip.scale_animation = Default::default();
                    clip.position_animation = animation;
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        self.status =
                            "Reframed the subject and added editable position keyframes.".into()
                    }
                    Err(error) => self.error("Apply auto reframe", error),
                }
            }
            Completed::Voiceover(asset, start, preferred_row) => {
                let media_id = asset.id.clone();
                let label = asset.label.clone();
                let end = start.saturating_add(asset.frames);
                let result = self.commit(|project| {
                    let accepts = |track: &Track| {
                        track.kind == TrackKind::Audio
                            && !track.muted
                            && track
                                .clips
                                .iter()
                                .all(|clip| start >= clip.end() || end <= clip.start)
                    };
                    let row = if project.tracks.get(preferred_row).is_some_and(accepts) {
                        preferred_row
                    } else if let Some(row) = project.tracks.iter().position(accepts) {
                        row
                    } else {
                        project
                            .tracks
                            .push(Track::new("Voiceover", TrackKind::Audio));
                        project.tracks.len() - 1
                    };
                    project.assets.push(asset);
                    let id = project.insert_asset(row, &media_id, start)?;
                    Ok((row, id))
                });
                match result {
                    Ok((row, id)) => {
                        self.asset_id = Some(media_id);
                        self.select(Selection {
                            row,
                            clip: Some(id),
                        });
                        self.seek(start);
                        self.status = format!("Added {label} at {}.", timestamp(start));
                    }
                    Err(error) => self.error("Add saved voiceover", error),
                }
            }
            Completed::Beats(original, positions) => {
                if positions.is_empty() {
                    self.status = "No consistent beat was detected in this clip.".into();
                    return;
                }
                let result = self.commit(|project| {
                    let (row, index) = project
                        .location(&original.id)
                        .context("The analyzed clip is no longer available.")?;
                    anyhow::ensure!(
                        project.tracks[row].clips[index] == original,
                        "The clip changed during analysis."
                    );
                    let mut added = 0;
                    for position in positions {
                        if project
                            .markers
                            .binary_search_by_key(&position, |marker| marker.frame)
                            .is_err()
                        {
                            project.set_marker(position, "Beat")?;
                            added += 1;
                        }
                    }
                    Ok(added)
                });
                match result {
                    Ok(0) => self.status = "Beat markers are already present.".into(),
                    Ok(added) => self.status = format!("Added {added} beat markers."),
                    Err(error) => self.error("Add beat markers", error),
                }
            }
            Completed::Tracked(original, animation) => {
                let points = animation.keys.len();
                let result = self.commit(|project| {
                    let (row, index) = project
                        .location(&original.id)
                        .context("The overlay is no longer available.")?;
                    let clip = &mut project.tracks[row].clips[index];
                    anyhow::ensure!(*clip == original, "The overlay changed during tracking.");
                    clip.position_animation = animation;
                    Ok(())
                });
                match result {
                    Ok(()) => self.status = format!("Added {points} tracking keyframes."),
                    Err(error) => self.error("Apply motion tracking", error),
                }
            }
            Completed::Scenes(original, cuts) => {
                if cuts.is_empty() {
                    self.status = "No hard scene changes found.".into();
                    return;
                }
                let result = self.commit(|project| {
                    let (row, index) = project
                        .location(&original.id)
                        .context("The scanned clip is no longer available.")?;
                    anyhow::ensure!(
                        project.tracks[row].clips[index] == original,
                        "The clip changed during the scan."
                    );
                    let mut tail = original.id.clone();
                    for offset in &cuts {
                        tail = project.split(&tail, original.start + offset)?;
                    }
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        self.status = format!("Split the clip into {} scenes.", cuts.len() + 1)
                    }
                    Err(error) => self.error("Split at scene changes", error),
                }
            }

            Completed::Subtitles(cues) => {
                let result = self.commit(|project| cutad::subtitles::add_tracks(project, cues));
                self.select_inserted(result, "Imported captions");
                if let Some((placement, _)) = &self.edit {
                    self.seek(placement.start);
                }
            }
            Completed::EnglishSubtitles(english, original) => {
                let result = self.commit(|project| {
                    let bilingual = original.is_some();
                    if let Some(original) = original {
                        cutad::subtitles::add_tracks(project, original)?;
                    }
                    cutad::subtitles::add_english_tracks(project, english, bilingual)
                });
                self.select_inserted(result, "Added translated captions");
            }

            Completed::MatchCanvas(id, geometry) => {
                self.match_first_video = false;
                let result = self.commit(|project| {
                    if let Some(asset) = project.assets.iter_mut().find(|asset| asset.id == id) {
                        asset.geometry = Some(geometry);
                    }
                    (project.width, project.height) = geometry.canvas();
                    Ok(())
                });
                match result {
                    Ok(()) => self.status = "Canvas matched to source video.".into(),
                    Err(error) => self.error("Match source", error),
                }
            }
        }
    }

    fn update_texture(&mut self, ctx: &egui::Context, size: [usize; 2], pixels: &[u8]) {
        let image = egui::ColorImage::from_rgb(size, pixels);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("program_monitor", image, egui::TextureOptions::LINEAR));
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.image((self.logo.id(), vec2(29.0, 29.0)));
            ui.label(RichText::new("CutAd").size(21.0).strong().color(TEXT));
            ui.label(
                RichText::new(env!("CARGO_PKG_VERSION"))
                    .small()
                    .color(MUTED),
            );
            ui.separator();
            ui.menu_button("Project", |ui| {
                if ui.button("New edit").clicked() {
                    ui.close();
                    if self.may_discard() {
                        self.set_document(Project::default(), None, false);
                        self.status = "Created a new edit.".into();
                    }
                }
                if ui.button("Open…").clicked() {
                    ui.close();
                    self.open();
                }
                let mut recent = None;
                ui.add_enabled_ui(!self.recent_projects.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for path in &self.recent_projects {
                            if ui
                                .button(filename(path))
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                recent = Some(path.clone());
                                ui.close();
                            }
                        }
                    });
                });
                if let Some(path) = recent
                    && self.may_discard()
                {
                    self.open_path(path);
                }
                if ui.button("Recover autosaved draft…").clicked() {
                    ui.close();
                    match recovery::candidates() {
                        Ok(candidates) => self.recovery_candidates = Some(candidates),
                        Err(error) => self.error("Find recovery drafts", error),
                    }
                }
                if ui.button("Save").clicked() {
                    ui.close();
                    self.save(false);
                }
                if ui.button("Save as…").clicked() {
                    ui.close();
                    self.save(true);
                }
                if ui.button("Save portable copy…").clicked() {
                    ui.close();
                    if let Some(parent) = FileDialog::new()
                        .set_title("Choose a folder for the portable copy")
                        .pick_folder()
                    {
                        let project = self.project.clone();
                        self.start_job(
                            ui.ctx(),
                            "Copying project and media",
                            JobStyle::Export,
                            move |cancel, progress| {
                                let path = project::save_portable_copy(
                                    &project,
                                    &parent,
                                    cancel,
                                    |value| {
                                        progress.store(value, Ordering::Relaxed);
                                    },
                                )?;
                                Ok(Completed::PortableCopy(path))
                            },
                        );
                    }
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.project.duration() > 0,
                        egui::Button::new("Export current frame…"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.export_still(ui.ctx());
                }
                ui.add_enabled_ui(
                    cutad::transcription::has_audible_clip(&self.project),
                    |ui| {
                        ui.menu_button("Export audio", |ui| {
                            for format in media::AudioFormat::ALL {
                                if ui.button(format.label()).clicked() {
                                    ui.close();
                                    self.export_audio(ui.ctx(), format);
                                }
                            }
                        });
                    },
                );
                ui.separator();
                if ui
                    .checkbox(&mut self.reduced_motion, "Reduce interface motion")
                    .changed()
                {
                    theme::set_reduced_motion(ui.ctx(), self.reduced_motion);
                }
            });
            if ui.button("Fonts").clicked() {
                self.open_fonts(ui.ctx());
            }
            if ui
                .selectable_label(self.show_inspector, "Inspector")
                .clicked()
            {
                self.restore_panels = None;
                self.show_inspector = !self.show_inspector;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let save = ui.button("Save");
                if save.clicked() {
                    self.save(false);
                }
                let export = egui::Button::new(RichText::new("Export video").color(BACK).strong())
                    .fill(ACCENT);
                if ui
                    .add_enabled(self.project.duration() > 0, export)
                    .clicked()
                {
                    let selection = self
                        .selection
                        .clip
                        .as_deref()
                        .and_then(|id| self.project.location(id))
                        .map(|(row, index)| {
                            let clip = &self.project.tracks[row].clips[index];
                            clip.start..clip.end()
                        });
                    self.export_dialog =
                        Some(ExportDialog::new(&self.project, selection, self.frame));
                }
            });
        });
        let dirty = if self.is_dirty() { "  •" } else { "" };
        let bounds = ui.max_rect();
        let center = bounds.center();
        ui.painter()
            .with_clip_rect(egui::Rect::from_center_size(
                center,
                vec2((bounds.width() - 660.0).clamp(80.0, 360.0), bounds.height()),
            ))
            .text(
                center,
                Align2::CENTER_CENTER,
                format!("{}{dirty}", self.project.name),
                FontId::proportional(14.0),
                TEXT,
            );
    }

    fn workspace_rail(&mut self, ui: &mut egui::Ui) {
        let tools = [
            (LibraryTab::Media, "Media"),
            (LibraryTab::Audio, "Audio"),
            (LibraryTab::Text, "Text"),
            (LibraryTab::Stickers, "Stickers"),
            (LibraryTab::Effects, "Effects"),
            (LibraryTab::Transitions, "Transitions"),
            (LibraryTab::Filters, "Filters"),
            (LibraryTab::Templates, "Templates"),
        ];
        ui.spacing_mut().item_spacing.y = 3.0;
        egui::ScrollArea::vertical()
            .id_salt("tool_rail_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                for (tab, name) in tools {
                    let id = ui.id().with(name);
                    let (rect, hit) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                    hit.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name)
                    });
                    if hit.clicked() {
                        let same = self.library == tab;
                        self.library = tab;
                        self.show_library = !same || !self.show_library;
                        self.restore_panels = None;
                        if self.show_library {
                            self.inspector_tab = match tab {
                                LibraryTab::Effects
                                | LibraryTab::Transitions
                                | LibraryTab::Filters => InspectorTab::Effects,
                                LibraryTab::Audio => InspectorTab::Audio,
                                LibraryTab::Text if self.title.is_some() => InspectorTab::Text,
                                _ => InspectorTab::Video,
                            };
                        }
                    }
                    let selected = self.library == tab && self.show_library;
                    let ease = ui.style().animation_time;
                    let active =
                        ui.ctx()
                            .animate_bool_with_time(id.with("selected"), selected, ease);
                    let hover =
                        ui.ctx()
                            .animate_bool_with_time(id.with("hover"), hit.hovered(), ease);
                    let painter = ui.painter().with_clip_rect(rect);
                    let surface = theme::PANEL
                        .lerp_to_gamma(LINE, hover * 0.7)
                        .lerp_to_gamma(ACCENT_DARK, active);
                    painter.rect_filled(rect, 8, surface);
                    let marker = egui::Rect::from_center_size(
                        egui::pos2(rect.left() + 1.5, rect.center().y),
                        vec2(3.0, 26.0 * active),
                    );
                    painter.rect_filled(marker, 2, ACCENT.gamma_multiply(active));
                    let color = MUTED.lerp_to_gamma(ACCENT, active.max(hover * 0.35));
                    dock_icon(
                        &painter,
                        tab,
                        egui::pos2(rect.center().x, rect.top() + 15.0),
                        color,
                    );
                    painter.text(
                        egui::pos2(rect.center().x, rect.bottom() - 9.0),
                        Align2::CENTER_CENTER,
                        name,
                        FontId::proportional(10.0),
                        color,
                    );
                }
            });
    }

    fn library_panel(&mut self, ui: &mut egui::Ui) {
        match self.library {
            LibraryTab::Media => self.media_panel(ui, false),
            LibraryTab::Audio => {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.audio_presets, false, "Local clips");
                    ui.selectable_value(&mut self.audio_presets, true, "Audio effects");
                });
                if self.audio_presets {
                    self.preset_panel(ui, Some(Category::Audio));
                } else {
                    self.media_panel(ui, true);
                }
            }
            LibraryTab::Text => self.titles_panel(ui),
            LibraryTab::Stickers => self.stickers_panel(ui),
            LibraryTab::Effects => self.preset_panel(ui, None),
            LibraryTab::Filters => self.preset_panel(ui, Some(Category::Color)),
            LibraryTab::Transitions => self.transitions_panel(ui),
            LibraryTab::Templates => self.templates_panel(ui),
        }
    }

    fn preset_panel(&mut self, ui: &mut egui::Ui, only: Option<Category>) {
        if let Some(preset) = self.gallery.show(ui, self.selection.clip.is_some(), only) {
            self.apply_preset(preset);
        }
    }

    fn choose_export_path(
        &mut self,
        title: &str,
        file_name: &str,
        kind: &str,
        extension: &str,
    ) -> Option<PathBuf> {
        let path = with_extension(
            FileDialog::new()
                .set_title(title)
                .set_file_name(file_name)
                .add_filter(kind, &[extension])
                .save_file()?,
            extension,
        );
        if self
            .file
            .as_ref()
            .is_some_and(|file| project::same_path(file, &path))
        {
            self.error(title, anyhow::anyhow!("Choose a separate output file."));
            return None;
        }
        Some(path)
    }

    fn export_captions(&mut self, ctx: &egui::Context, row: Option<usize>) {
        let Some(path) = self.choose_export_path(
            "Export captions",
            "CutAd captions.srt",
            "SubRip subtitles",
            "srt",
        ) else {
            return;
        };
        let project = self.project.clone();
        self.start_job(ctx, "Exporting captions", JobStyle::Read, move |_, _| {
            let count = match row {
                Some(row) => cutad::subtitles::write_track(&project, &path, row)?,
                None => cutad::subtitles::write(&project, &path)?,
            };
            Ok(Completed::CaptionExport(path, count))
        });
    }

    fn export_still(&mut self, ctx: &egui::Context) {
        if self.project.duration() == 0 {
            return;
        }
        self.stop_player();
        let frame = self.frame.min(self.project.duration() - 1);
        let Some(path) = self.choose_export_path(
            "Export current frame",
            "CutAd frame.png",
            "PNG image",
            "png",
        ) else {
            return;
        };
        let project = self.project.clone();
        self.start_job(
            ctx,
            "Exporting frame",
            JobStyle::Cancellable,
            move |cancel, _| {
                media::export_frame_png(&project, &path, frame, cancel)?;
                Ok(Completed::Export(path))
            },
        );
    }

    fn export_audio(&mut self, ctx: &egui::Context, format: media::AudioFormat) {
        let extension = format.extension();
        let Some(path) = self.choose_export_path(
            "Export audio",
            &format!("CutAd audio.{extension}"),
            format.label(),
            extension,
        ) else {
            return;
        };
        self.stop_player();
        let project = self.project.clone();
        self.start_job(
            ctx,
            "Exporting audio",
            JobStyle::Export,
            move |cancel, progress| {
                media::export_audio(&project, &path, format, cancel, |value| {
                    progress.store(value, Ordering::Relaxed)
                })?;
                Ok(Completed::Export(path))
            },
        );
    }

    fn auto_captions(&mut self, ctx: &egui::Context, translate: bool, bilingual: bool) {
        if !cutad::transcription::has_audible_clip(&self.project) {
            self.status = "Add audible media before generating captions.".into();
            return;
        }
        if let Err(error) = cutad::transcription::executable() {
            self.error("Auto captions", error);
            return;
        }
        let Some(model) = FileDialog::new()
            .set_title(if translate {
                "Select a multilingual whisper.cpp model"
            } else {
                "Select a local whisper.cpp model"
            })
            .add_filter("Whisper model", &["bin"])
            .pick_file()
        else {
            return;
        };
        self.stop_player();
        let project = self.project.clone();
        self.start_job(
            ctx,
            if translate {
                "Translating captions to English"
            } else {
                "Generating captions"
            },
            JobStyle::Cancellable,
            move |cancel, _| {
                if translate {
                    let english =
                        cutad::transcription::translate_to_english(&project, &model, cancel)?;
                    let original = if bilingual {
                        Some(cutad::transcription::generate(&project, &model, cancel)?)
                    } else {
                        None
                    };
                    Ok(Completed::EnglishSubtitles(english, original))
                } else {
                    Ok(Completed::Subtitles(cutad::transcription::generate(
                        &project, &model, cancel,
                    )?))
                }
            },
        );
    }

    fn titles_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Titles");
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Import captions…")
                .on_hover_text("Import an editable .srt subtitle track")
                .clicked()
                && let Some(path) = FileDialog::new()
                    .add_filter("SubRip subtitles", &["srt"])
                    .pick_file()
            {
                self.start_job(ui.ctx(), "Reading captions", JobStyle::Read, move |_, _| {
                    Ok(Completed::Subtitles(cutad::subtitles::read(&path)?))
                });
            }
            ui.menu_button("Export captions…", |ui| {
                if ui.button("All visible captions").clicked() {
                    ui.close();
                    self.export_captions(ui.ctx(), None);
                }
                if ui.button("Selected track only").clicked() {
                    ui.close();
                    self.export_captions(ui.ctx(), Some(self.selection.row));
                }
            });
            if ui
                .button("Auto captions…")
                .on_hover_text("Generate offline with whisper.cpp and a local model")
                .clicked()
            {
                self.auto_captions(ui.ctx(), false, false);
            }
            if ui.button("Translate speech to English…").clicked() {
                self.auto_captions(ui.ctx(), true, false);
            }
            if ui.button("Original and English captions…").clicked() {
                self.auto_captions(ui.ctx(), true, true);
            }
            if ui.button("Browse fonts…").clicked() {
                self.open_fonts(ui.ctx());
            }
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.show_transcript, false, "Text presets");
            ui.selectable_value(&mut self.show_transcript, true, "Transcript");
        });
        if self.show_transcript {
            self.transcript_panel(ui);
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("title_templates")
            .show(ui, |ui| {
                let gap = ui.spacing().item_spacing.x;
                let width = ((ui.available_width() - gap - 10.0) / 2.0).max(94.0);
                for row in TitleStyle::ALL.chunks(2) {
                    ui.horizontal(|ui| {
                        for &style in row {
                            if title_tile(ui, style, width).clicked() {
                                let frame = self.frame;
                                let result = self.commit(|project| {
                                    project.add_title(style, style.sample(), frame)
                                });
                                self.select_inserted(result, "Added text preset");
                            }
                        }
                    });
                }
            });
    }

    fn transcript_panel(&mut self, ui: &mut egui::Ui) {
        if ui.button("Export transcript…").clicked()
            && let Some(path) = self.choose_export_path(
                "Export transcript",
                "CutAd transcript.txt",
                "Plain text",
                "txt",
            )
        {
            let project = self.project.clone();
            self.start_job(
                ui.ctx(),
                "Exporting transcript",
                JobStyle::Read,
                move |_, _| {
                    let count = cutad::subtitles::write_transcript(&project, &path)?;
                    Ok(Completed::CaptionExport(path, count))
                },
            );
        }
        ui.add(
            egui::TextEdit::singleline(&mut self.transcript_query)
                .hint_text("Search captions")
                .desired_width(f32::INFINITY),
        );
        let captions = cutad::subtitles::caption_clips(&self.project, &self.transcript_query);
        if captions.is_empty() {
            ui.label(if self.transcript_query.trim().is_empty() {
                "Import or generate captions to see them here."
            } else {
                "No matching captions."
            });
            return;
        }
        let mut selected = None;
        let mut remove_range = None;
        egui::ScrollArea::vertical()
            .id_salt("caption_transcript")
            .show_rows(ui, 33.0, captions.len(), |ui, rows| {
                for index in rows {
                    let (clip, text) = captions[index];
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let label = format!("{}  {text}", timestamp(clip.start));
                    let active = self.selection.clip.as_deref() == Some(clip.id.as_str());
                    let response = ui
                        .add_sized(
                            [ui.available_width(), 33.0],
                            egui::Button::new(label).selected(active).truncate(),
                        )
                        .on_hover_text(text);
                    if response.clicked() {
                        selected = Some((clip.id.clone(), clip.start));
                    }
                    response.context_menu(|ui| {
                        if ui
                            .button("Remove this time range from all tracks")
                            .clicked()
                        {
                            remove_range = Some((clip.start, clip.end()));
                            ui.close();
                        }
                    });
                }
            });
        if let Some((start, end)) = remove_range {
            match self.commit(|project| project.ripple_delete_range(start, end)) {
                Ok(()) => {
                    self.seek(start);
                    self.status = format!("Removed {} from all tracks.", timestamp(end - start));
                }
                Err(error) => self.error("Remove transcript range", error),
            }
            return;
        }
        if let Some((id, start)) = selected
            && let Some((row, _)) = self.project.location(&id)
        {
            self.select(Selection {
                row,
                clip: Some(id),
            });
            self.inspector_tab = InspectorTab::Text;
            self.seek(start);
        }
    }

    fn select_inserted(&mut self, result: Result<String>, action: &str) {
        match result {
            Ok(id) => {
                self.select(Selection {
                    row: self.selection.row,
                    clip: Some(id),
                });
                if self.title.is_none() {
                    self.inspector_tab = InspectorTab::Video;
                }
                self.status = format!("{action}.");
            }
            Err(error) => self.error(action, error),
        }
    }

    fn stickers_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Stickers");
        ui.label(RichText::new("Add an image or animated GIF at the playhead.").color(MUTED));
        if ui
            .button(RichText::new("Import sticker…").strong())
            .clicked()
            && let Some(path) = FileDialog::new()
                .add_filter("Still images", IMAGE_FILES)
                .add_filter("Animated GIF", &["gif"])
                .pick_file()
        {
            self.import_sticker(ui.ctx(), path);
        }
        ui.separator();
        ui.label(
            RichText::new(
                "Select the sticker on the timeline to change its position, size, or duration.",
            )
            .color(MUTED),
        );
    }

    fn import_sticker(&mut self, ctx: &egui::Context, path: PathBuf) {
        let frame = self.frame;
        self.start_job(ctx, "Reading sticker", JobStyle::Read, move |_, _| {
            Ok(Completed::Sticker(media::probe(&path)?, frame))
        });
    }

    fn transitions_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Transitions");
        ui.label(RichText::new("Select the incoming clip at a cut.").color(MUTED));
        for options in TransitionKind::ALL.chunks(2) {
            ui.horizontal(|ui| {
                let width = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(60.0);
                for &kind in options {
                    if ui
                        .add_sized([width, 37.0], egui::Button::new(kind.name()))
                        .clicked()
                    {
                        self.apply_transition(kind);
                    }
                }
            });
        }
    }

    fn apply_transition(&mut self, kind: TransitionKind) {
        let Some(id) = self.selection.clip.clone() else {
            self.status = "Select the incoming video clip first.".into();
            return;
        };
        let result = self.commit(|project| {
            let (row, index) = project.location(&id).context("Select a video clip.")?;
            let track = &mut project.tracks[row];
            let start = track.clips[index].start;
            let length = track.clips[index].length;
            anyhow::ensure!(
                track.kind == TrackKind::Video
                    && length >= 2
                    && track.clips.iter().any(|clip| clip.end() == start),
                "The transition needs two adjacent clips on the same video track."
            );
            track.clips[index].transition = Some(Transition {
                kind,
                frames: length.min(FPS / 2),
            });
            Ok(())
        });
        match result {
            Ok(()) => {
                self.inspector_tab = InspectorTab::Effects;
                self.status = format!("Added {} at the cut.", kind.name());
            }
            Err(error) => self.error("Apply transition", error),
        }
    }

    fn templates_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Templates");
        for (name, detail) in TEMPLATES.into_iter().zip([
            "9:16 · 3 media slots",
            "16:9 · 3 media slots",
            "16:9 · 2 panels and a title",
            "9:16 · 2 media slots and a reveal transition",
        ]) {
            let width = ui.available_width();
            egui::Frame::new()
                .fill(BACK)
                .stroke(Stroke::new(1.0, LINE))
                .corner_radius(11)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.set_min_width((width - 20.0).max(100.0));
                    if ui.button(RichText::new(name).strong()).clicked() {
                        self.use_template(name);
                    }
                    ui.label(RichText::new(detail).color(MUTED).small());
                });
        }
        ui.separator();
        if ui.button("Open CutAd template…").clicked() {
            self.open();
        }
        if ui
            .add_enabled(
                self.project.duration() > 0,
                egui::Button::new("Save as reusable template…"),
            )
            .clicked()
        {
            self.save_template();
        }
        ui.separator();
        if ui.button("Import local CapCut draft…").clicked() {
            self.import_draft(ui.ctx(), false);
        }
        if ui.button("Import draft with media folder…").clicked() {
            self.import_draft(ui.ctx(), true);
        }
    }

    fn apply_preset(&mut self, preset: Preset) {
        let Some(id) = self.selection.clip.clone() else {
            self.status = "Select a clip on the timeline before applying an effect.".into();
            return;
        };
        let result = self.commit(|project| {
            let (row, column) = project.location(&id).context("Select a clip first.")?;
            let track = &project.tracks[row];
            let asset = project
                .asset(&track.clips[column].asset_id)
                .context("The clip has no media.")?;
            if preset.audio() {
                anyhow::ensure!(asset.audio, "Choose a clip with audio for this preset.");
            } else {
                anyhow::ensure!(
                    track.kind == TrackKind::Video,
                    "Choose a video or image clip for this preset."
                );
            }
            project.tracks[row].clips[column]
                .effects
                .push(Effect::new(preset));
            Ok(())
        });
        match result {
            Ok(()) => {
                self.inspector_tab = InspectorTab::Effects;
                self.status = format!("Applied {}.", preset.name());
            }
            Err(error) => self.error("Apply effect", error),
        }
    }

    fn effect_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selection.clip.clone() else {
            ui.label(RichText::new("Select a clip to inspect its effects.").color(MUTED));
            return;
        };
        let Some((row, column)) = self.project.location(&id) else {
            return;
        };
        if let Some(transition) = &mut self.transition {
            ui.label(RichText::new(transition.kind.name()).strong());
            ui.add(egui::Slider::new(&mut transition.frames, 2..=2 * FPS).text("Frames"));
            if ui.button("Remove transition").clicked() {
                self.transition = None;
            }
            ui.separator();
        }
        if self.effects.is_empty() && self.transition.is_none() {
            ui.label(RichText::new("Choose an effect from the library.").color(MUTED));
        }
        let mut move_effect = None;
        let mut delete = None;
        let count = self.effects.len();
        for (index, effect) in self.effects.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                egui::Frame::new()
                    .fill(BACK)
                    .stroke(Stroke::new(1.0, LINE))
                    .corner_radius(11)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.checkbox(&mut effect.enabled, effect.preset.name());
                        ui.add(
                            egui::Slider::new(&mut effect.amount, 0..=100)
                                .suffix("%")
                                .text("Strength"),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(index > 0, egui::Button::new("Up"))
                                .on_hover_text("Move up")
                                .clicked()
                            {
                                move_effect = Some((index, index - 1));
                            }
                            if ui
                                .add_enabled(index + 1 < count, egui::Button::new("Down"))
                                .on_hover_text("Move down")
                                .clicked()
                            {
                                move_effect = Some((index, index + 1));
                            }
                            if ui.button("Remove").clicked() {
                                delete = Some(index);
                            }
                        });
                    });
            });
        }
        if let Some(index) = delete {
            self.effects.remove(index);
        }
        if let Some((a, b)) = move_effect {
            self.effects.swap(a, b);
        }
        let clip = &self.project.tracks[row].clips[column];
        let differs = self.effects != clip.effects || self.transition != clip.transition;
        if ui
            .add_enabled(differs, egui::Button::new("Apply stack"))
            .clicked()
        {
            let effects = self.effects.clone();
            let transition = self.transition;
            let result = self.commit(|project| {
                project.tracks[row].clips[column].effects = effects;
                project.tracks[row].clips[column].transition = transition;
                Ok(())
            });
            match result {
                Ok(()) => self.status = "Updated effects.".into(),
                Err(error) => self.error("Edit effect stack", error),
            }
        }
    }

    fn media_panel(&mut self, ui: &mut egui::Ui, audio_only: bool) {
        ui.horizontal_top(|ui| {
            if !audio_only {
                ui.allocate_ui_with_layout(
                    vec2(82.0, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| {
                        for (category, name) in [
                            (None, "All media"),
                            (Some(AssetKind::Video), "Video"),
                            (Some(AssetKind::Audio), "Audio"),
                            (Some(AssetKind::Image), "Images"),
                        ] {
                            ui.selectable_value(&mut self.media_kind, category, name);
                        }
                    },
                );
                ui.separator();
            }
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if ui.button("Import…").clicked() {
                        let dialog = if audio_only {
                            FileDialog::new().add_filter("Audio", AUDIO_FILES)
                        } else {
                            media_dialog()
                        };
                        if let Some(paths) = dialog.pick_files() {
                            self.import_media(ui.ctx(), paths);
                        }
                    }
                    ui.add_sized(
                        [ui.available_width().max(56.0), 29.0],
                        egui::TextEdit::singleline(&mut self.media_query).hint_text("Find media"),
                    );
                });
                let query = self.media_query.trim().to_lowercase();
                let filter = if audio_only {
                    Some(AssetKind::Audio)
                } else {
                    self.media_kind
                };
                let assets: Vec<_> = self
                    .project
                    .assets
                    .iter()
                    .filter(|asset| {
                        asset.path.is_some()
                            && filter.is_none_or(|kind| asset.kind == kind)
                            && (query.is_empty() || asset.label.to_lowercase().contains(&query))
                    })
                    .collect();
                let mut add = None;
                if assets.is_empty() {
                    let message = if self.project.assets.is_empty() {
                        "Drop files here, or import from your device."
                    } else {
                        "No matching media."
                    };
                    ui.label(RichText::new(message).color(MUTED));
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("media_grid")
                        .max_height((ui.available_height() - 42.0).max(48.0))
                        .show(ui, |ui| {
                            let spacing = ui.spacing().item_spacing.x;
                            let columns = if ui.available_width() >= 230.0 { 2 } else { 1 };
                            let width =
                                ((ui.available_width() - spacing * ((columns - 1) as f32) - 3.0)
                                    / columns as f32)
                                    .clamp(80.0, 170.0);
                            for row in assets.chunks(columns) {
                                ui.horizontal(|ui| {
                                    for asset in row {
                                        let (rect, hit) = ui.allocate_exact_size(
                                            vec2(width, 87.0),
                                            Sense::click_and_drag(),
                                        );
                                        hit.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::WidgetType::Button,
                                                true,
                                                &asset.label,
                                            )
                                        });
                                        hit.dnd_set_drag_payload(MediaDrag(asset.id.clone()));
                                        let frame = if self.asset_id.as_deref() == Some(&asset.id)
                                            || hit.hovered()
                                        {
                                            ACCENT
                                        } else {
                                            LINE
                                        };
                                        ui.painter().rect(
                                            rect,
                                            6,
                                            BACK,
                                            Stroke::new(1.0, frame),
                                            StrokeKind::Inside,
                                        );
                                        let media = egui::Rect::from_min_size(
                                            rect.min + vec2(4.0, 4.0),
                                            vec2(rect.width() - 8.0, 58.0),
                                        );
                                        if let Some(image) =
                                            self.thumbnails.image(ui.ctx(), asset, 0)
                                        {
                                            ui.painter().image(
                                                image.id(),
                                                media,
                                                egui::Rect::from_min_max(
                                                    egui::Pos2::ZERO,
                                                    egui::pos2(1.0, 1.0),
                                                ),
                                                Color32::WHITE,
                                            );
                                        } else {
                                            let type_label = match asset.kind {
                                                AssetKind::Audio => "Audio",
                                                AssetKind::Image => "Image",
                                                _ => "Video",
                                            };
                                            ui.painter().text(
                                                media.center(),
                                                Align2::CENTER_CENTER,
                                                type_label,
                                                FontId::proportional(13.0),
                                                MUTED,
                                            );
                                        }
                                        ui.painter().with_clip_rect(rect.shrink(5.0)).text(
                                            egui::pos2(rect.left() + 6.0, rect.bottom() - 11.0),
                                            Align2::LEFT_CENTER,
                                            &asset.label,
                                            FontId::proportional(11.0),
                                            TEXT,
                                        );
                                        if hit.clicked() {
                                            self.asset_id = Some(asset.id.clone());
                                        }
                                        if hit.double_clicked() {
                                            add = Some(asset.id.clone());
                                        }
                                    }
                                });
                            }
                        });
                }
                if ui
                    .add_enabled(
                        self.asset_id.is_some(),
                        egui::Button::new("Add to timeline"),
                    )
                    .clicked()
                {
                    add.clone_from(&self.asset_id);
                }
                if let Some(id) = add {
                    self.insert(&id);
                }
            });
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if self.title.is_some() {
                ui.selectable_value(&mut self.inspector_tab, InspectorTab::Text, "Text");
            }
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Video, "Video");
            if self
                .edit
                .as_ref()
                .is_some_and(|(edit, _)| self.project.tracks[edit.row].kind == TrackKind::Video)
            {
                ui.selectable_value(&mut self.inspector_tab, InspectorTab::Motion, "Animation");
                ui.selectable_value(&mut self.inspector_tab, InspectorTab::Mask, "Mask");
            }
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Color, "Color");
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Audio, "Audio");
            ui.selectable_value(&mut self.inspector_tab, InspectorTab::Effects, "Effects");
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt(("inspector_scroll", self.inspector_tab as u8))
            .show(ui, |ui| match self.inspector_tab {
                InspectorTab::Text => self.text_inspector(ui),
                InspectorTab::Video => self.clip_inspector(ui),
                InspectorTab::Motion => self.motion_inspector(ui),
                InspectorTab::Color => self.color_inspector(ui),
                InspectorTab::Mask => self.mask_inspector(ui),
                InspectorTab::Audio => self.audio_inspector(ui),
                InspectorTab::Effects => self.effect_inspector(ui),
            });
    }

    fn text_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(mut title) = self.title.clone() else {
            ui.label("Select a title on the timeline.");
            return;
        };
        if let Some((placement, _)) = &self.edit {
            let (start, row) = (placement.start, placement.row);
            let mut selected_voice = None;
            ui.menu_button("Generate narration…", |ui| {
                for (label, voice) in [
                    ("English", "en"),
                    ("Portuguese (Brazil)", "pt-br"),
                    ("Spanish", "es"),
                    ("French", "fr"),
                ] {
                    if ui.button(label).clicked() {
                        selected_voice = Some(voice);
                        ui.close();
                    }
                }
            });
            if let Some(voice) = selected_voice
                && let Some(path) =
                    self.choose_export_path("Save narration", "Narration.wav", "WAV audio", "wav")
            {
                let text = title.text.clone();
                self.stop_player();
                self.start_job(
                    ui.ctx(),
                    "Generating narration",
                    JobStyle::Read,
                    move |_, _| {
                        let asset = media::synthesize_narration(&text, voice, &path)?;
                        Ok(Completed::Voiceover(asset, start, row))
                    },
                );
            }
        }
        if crate::typography::font_picker(ui, title.appearance.font.as_ref()) {
            self.open_fonts(ui.ctx());
        }
        ui.label("Text");
        ui.add(
            egui::TextEdit::multiline(&mut title.text)
                .char_limit(4096)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            ui.label("Preset");
            egui::ComboBox::from_id_salt("title_style")
                .selected_text(title.style.label())
                .show_ui(ui, |ui| {
                    for style in TitleStyle::ALL {
                        ui.selectable_value(&mut title.style, style, style.label());
                    }
                });
        });
        crate::typography::show_controls(ui, &mut title.appearance);
        if self.title.as_ref() != Some(&title) {
            title.text.truncate(title.text.floor_char_boundary(4096));
            title.text.retain(|c| c != '\0');
            self.apply_title(title);
        }
    }

    fn motion_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selection.clip.clone() else {
            ui.label("Select a video, image, or text clip.");
            return;
        };
        let Some((row, index)) = self.project.location(&id) else {
            return;
        };
        let clip = &self.project.tracks[row].clips[index];
        if !clip.freeze
            && !clip.slot
            && self.project.asset(&clip.asset_id).is_some_and(|asset| {
                matches!(asset.kind, AssetKind::Video | AssetKind::Image)
            })
            && ui.button("Auto reframe…").on_hover_text("Fit the current canvas and replace scale and position animation with subject framing").clicked()
        {
            self.auto_reframe(ui.ctx(), clip.clone());
            return;
        }
        let fade = clip.visual_fade;
        let mut fades = [fade.in_frames, fade.out_frames];
        let mut apply_preset = false;
        ui.heading("Clip fades");
        Self::fade_controls(ui, "visual_fades", clip.length, &mut fades);
        ui.horizontal(|ui| {
            if ui.button("Fade both").clicked() {
                let duration = (FPS / 2).min(clip.length.div_ceil(2));
                fades = [duration; 2];
                apply_preset = true;
            }
            if ui.button("Clear fades").clicked() {
                fades = [0; 2];
            }
        });
        ui.separator();
        ui.heading("Position keyframes");
        let mut easing = clip.position_animation.easing;
        let mut slide = None;
        if clip.position_animation.is_empty() && clip.length > 1 {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Slide in from left").clicked() {
                    slide = Some(-1.5);
                }
                if ui.button("Slide in from right").clicked() {
                    slide = Some(1.5);
                }
            });
        } else if !clip.position_animation.is_empty() {
            egui::ComboBox::from_id_salt("position_easing")
                .selected_text(easing.label())
                .show_ui(ui, |ui| {
                    for option in project::Easing::ALL {
                        ui.selectable_value(&mut easing, option, option.label());
                    }
                });
        }
        let in_clip = (clip.start..clip.end()).contains(&self.frame);
        let local = self.frame.saturating_sub(clip.start).min(clip.length - 1);
        let mut position = clip.position_at(local);
        let keyed = clip
            .position_animation
            .times(clip.length)
            .any(|frame| frame == local);
        let times: Vec<_> = clip
            .position_animation
            .times(clip.length)
            .map(|frame| clip.start + frame)
            .collect();
        if !in_clip {
            ui.label(RichText::new("Move the playhead inside this clip.").color(MUTED));
        }
        let position_before = position;
        let mut jumped = None;
        let mut add = false;
        let mut remove = false;
        ui.add_enabled_ui(in_clip, |ui| {
            egui::Grid::new("motion_position")
                .num_columns(2)
                .show(ui, |ui| {
                    for (label, value) in
                        ["Position X", "Position Y"].into_iter().zip(&mut position)
                    {
                        ui.label(label);
                        let mut percent = *value * 100.0;
                        if ui
                            .add(
                                egui::DragValue::new(&mut percent)
                                    .range(-150.0..=150.0)
                                    .suffix("%")
                                    .fixed_decimals(0),
                            )
                            .changed()
                        {
                            *value = percent / 100.0;
                        }
                        ui.end_row();
                    }
                });
            ui.horizontal_wrapped(|ui| {
                add = ui
                    .add_enabled(!keyed, egui::Button::new("Add keyframe"))
                    .clicked();
                remove = ui
                    .add_enabled(keyed, egui::Button::new("Remove keyframe"))
                    .clicked();
            });
        });
        if !times.is_empty() {
            ui.separator();
            for time in times {
                if ui
                    .selectable_label(time == self.frame, timestamp(time))
                    .clicked()
                {
                    jumped = Some(time);
                }
            }
        }
        if let Some(x) = slide {
            let result = self.commit(|project| {
                let (row, index) = project.location(&id).context("Select a clip.")?;
                let clip = &mut project.tracks[row].clips[index];
                let [end_x, y] = [clip.transform.x, clip.transform.y];
                clip.set_position_keyframe(0, [x, y])?;
                clip.set_position_keyframe((FPS / 2).min(clip.length - 1), [end_x, y])?;
                clip.position_animation.easing = project::Easing::EaseOut;
                Ok(())
            });
            if let Err(error) = result {
                self.error("Apply slide animation", error);
            }
        } else if easing != clip.position_animation.easing {
            self.update_selected_clip(LiveControl::Motion, |clip| {
                clip.position_animation.easing = easing;
            });
        } else if let Some(frame) = jumped {
            self.seek(frame);
        } else if remove {
            self.last_live_edit = None;
            self.update_selected_clip(LiveControl::Motion, |clip| {
                clip.remove_position_keyframe(local)
            });
            self.last_live_edit = None;
        } else if add || position != position_before {
            if add {
                self.last_live_edit = None;
            }
            let result = self.commit_live(LiveControl::Motion, |project| {
                let (row, index) = project.location(&id).context("Select a clip.")?;
                let clip = &mut project.tracks[row].clips[index];
                if add || !clip.position_animation.is_empty() {
                    clip.set_position_keyframe(local, position)?;
                } else {
                    [clip.transform.x, clip.transform.y] = position;
                }
                Ok(())
            });
            if let Err(error) = result {
                self.error("Edit motion", error);
            }
        }
        if apply_preset || fades != [fade.in_frames, fade.out_frames] {
            self.update_selected_clip(LiveControl::VisualFade, |clip| {
                if apply_preset {
                    clip.visual_fade = project::ClipFade::default();
                }
                clip.set_visual_fade(fades[0], fades[1]);
            });
        }
        self.scalar_keyframes(ui, &id, project::AnimatedProperty::Opacity);
        self.scalar_keyframes(ui, &id, project::AnimatedProperty::Rotation);
        self.scalar_keyframes(ui, &id, project::AnimatedProperty::Scale);
        self.tracking_controls(ui, &id);
    }

    fn tracking_controls(&mut self, ui: &mut egui::Ui, id: &str) {
        let Some((row, index)) = self.project.location(id) else {
            return;
        };
        if !self.project.tracks[..row]
            .iter()
            .any(|track| track.kind == TrackKind::Video && !track.muted && !track.clips.is_empty())
        {
            return;
        }
        ui.separator();
        ui.collapsing("Motion tracking", |ui| {
            let clip = self.project.tracks[row].clips[index].clone();
            ui.label("Choose a patch in the footage beneath the overlay’s first frame.");
            if ui.button("Go to first frame").clicked() {
                self.seek(clip.start);
                self.show_tracking_patch = true;
            }
            ui.checkbox(&mut self.show_tracking_patch, "Show tracking patch");
            let seed = &mut self.tracking_seed;
            egui::Grid::new("tracking_patch").show(ui, |ui| {
                for (label, value, bounds) in [
                    ("Center X", &mut seed.x, 0..=100),
                    ("Center Y", &mut seed.y, 0..=100),
                    ("Patch size", &mut seed.size, 5..=35),
                ] {
                    ui.label(label);
                    ui.add(egui::DragValue::new(value).range(bounds).suffix("%"));
                    ui.end_row();
                }
            });
            if ui
                .button("Track position")
                .on_hover_text("Create new position keyframes for this overlay.")
                .clicked()
            {
                let seed = *seed;
                let snapshot = self.project.clone();
                self.start_job(
                    ui.ctx(),
                    "Tracking motion",
                    JobStyle::Cancellable,
                    move |cancel, _| {
                        let animation =
                            cutad::tracking::track_overlay(snapshot, &clip.id, seed, cancel)?;
                        Ok(Completed::Tracked(clip, animation))
                    },
                );
            }
        });
    }

    fn scalar_keyframes(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        property: project::AnimatedProperty,
    ) {
        let Some((row, index)) = self.project.location(id) else {
            return;
        };
        let clip = &self.project.tracks[row].clips[index];
        let local = self.frame.saturating_sub(clip.start).min(clip.length - 1);
        let in_clip = (clip.start..clip.end()).contains(&self.frame);
        let (control, factor, range, unit) = match property {
            project::AnimatedProperty::Opacity => {
                (LiveControl::OpacityAnimation, 100.0, 0.0..=100.0, "%")
            }
            project::AnimatedProperty::Rotation => {
                (LiveControl::RotationAnimation, 1.0, -180.0..=180.0, "°")
            }
            project::AnimatedProperty::Scale => (
                LiveControl::ScaleAnimation,
                100.0,
                10.0..=project::MAX_CLIP_SCALE * 100.0,
                "%",
            ),
            project::AnimatedProperty::AudioGain => {
                (LiveControl::AudioGainAnimation, 100.0, 0.0..=400.0, "%")
            }
        };
        let original = clip.animated_value(property, local) * factor;
        let mut value = original;
        let keyed = clip
            .animation(property)
            .times(clip.length)
            .any(|time| time == local);
        let original_easing = clip.animation(property).easing;
        let mut easing = original_easing;
        let mut add = false;
        let mut remove = false;
        ui.separator();
        ui.heading(format!("{} keyframes", property.label()));
        if !clip.animation(property).is_empty() {
            egui::ComboBox::from_id_salt(("scalar_easing", property.label()))
                .selected_text(easing.label())
                .show_ui(ui, |ui| {
                    for option in project::Easing::ALL {
                        ui.selectable_value(&mut easing, option, option.label());
                    }
                });
        }
        let static_audio =
            property == project::AnimatedProperty::AudioGain && clip.gain_animation.is_empty();
        ui.add_enabled_ui(in_clip || static_audio, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(property.label());
                ui.add(
                    egui::DragValue::new(&mut value)
                        .range(range)
                        .suffix(unit)
                        .fixed_decimals(0),
                );
                add = ui
                    .add_enabled(in_clip && !keyed, egui::Button::new("Add keyframe"))
                    .clicked();
                remove = ui
                    .add_enabled(keyed, egui::Button::new("Remove keyframe"))
                    .clicked();
            });
        });
        let seek = clip
            .animation(property)
            .times(clip.length)
            .map(|time| clip.start + time)
            .find(|&time| {
                ui.selectable_label(time == self.frame, timestamp(time))
                    .clicked()
            });
        if let Some(frame) = seek {
            self.seek(frame);
        } else if easing != original_easing {
            self.update_selected_clip(control, |clip| clip.set_scalar_easing(property, easing));
        } else if remove {
            self.last_live_edit = None;
            self.update_selected_clip(control, |clip| clip.remove_scalar_keyframe(property, local));
            self.last_live_edit = None;
        } else if add || value != original {
            if add {
                self.last_live_edit = None;
            }
            let result = self.commit_live(control, |project| {
                let (row, index) = project.location(id).context("Select a clip.")?;
                let clip = &mut project.tracks[row].clips[index];
                if add || !clip.animation(property).is_empty() {
                    clip.set_scalar_keyframe(property, local, value / factor)?;
                } else {
                    clip.set_static_value(property, value / factor);
                }
                Ok(())
            });
            if let Err(error) = result {
                self.error("Edit keyframe", error);
            }
        }
    }

    fn color_inspector(&mut self, ui: &mut egui::Ui) {
        let Some(clip) = self
            .selection
            .clip
            .as_deref()
            .and_then(|id| self.project.location(id))
            .filter(|(row, _)| self.project.tracks[*row].kind == TrackKind::Video)
            .map(|(row, index)| &self.project.tracks[row].clips[index])
        else {
            ui.label("Select a video, image, or title clip.");
            return;
        };
        let mut grade = clip.adjustment;
        let mut chroma = clip.chroma_key;
        let mut lut = clip.color_lut.clone();
        let mut hue_ranges = clip.hue_adjustments;
        ui.heading("Color correction");
        if let Some(asset) = self
            .project
            .asset(&clip.asset_id)
            .filter(|asset| !clip.slot && matches!(asset.kind, AssetKind::Video | AssetKind::Image))
        {
            let mut auto = false;
            let mut match_tone = false;
            ui.horizontal_wrapped(|ui| {
                auto = ui
                    .button("Auto color")
                    .on_hover_text(
                        "Set brightness, contrast, and saturation from the playhead frame",
                    )
                    .clicked();
                match_tone = ui
                    .button("Match tone…")
                    .on_hover_text(
                        "Match brightness, contrast, and saturation to a local image or video",
                    )
                    .clicked();
            });
            if auto || match_tone {
                let reference = if match_tone {
                    let Some(path) = FileDialog::new()
                        .set_title("Choose a reference image or video")
                        .add_filter("Images", IMAGE_FILES)
                        .add_filter("Videos", VIDEO_FILES)
                        .pick_file()
                    else {
                        return;
                    };
                    Some(path)
                } else {
                    None
                };
                let clip = clip.clone();
                let asset = asset.clone();
                let local = self.frame.saturating_sub(clip.start).min(clip.length - 1);
                let source = (clip.source_units_at(i64::from(local)) / 100) as u32;
                self.stop_player();
                self.start_job(
                    ui.ctx(),
                    "Analyzing color",
                    JobStyle::Cancellable,
                    move |cancel, _| {
                        let reference = reference.as_deref().map(media::probe).transpose()?;
                        let grade =
                            media::suggest_color(&asset, source, reference.as_ref(), cancel)?;
                        Ok(Completed::ColorSuggested(clip, grade, reference.is_some()))
                    },
                );
                return;
            }
        }
        egui::Grid::new("color_correction")
            .num_columns(2)
            .show(ui, |ui| {
                for (label, value) in [
                    ("Brightness", &mut grade.brightness),
                    ("Contrast", &mut grade.contrast),
                    ("Saturation", &mut grade.saturation),
                ] {
                    ui.label(label);
                    ui.add(egui::DragValue::new(value).range(-100..=100));
                    ui.end_row();
                }
            });
        if ui.small_button("Reset color").clicked() {
            grade = ColorAdjustment::default();
        }
        ui.collapsing("Hue, saturation, and intensity", |ui| {
            let id = ui.make_persistent_id("selected_hue_range");
            let mut selected = ui
                .data(|data| data.get_temp::<usize>(id))
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("hue_range")
                .selected_text(project::HUE_RANGES[selected].0)
                .show_ui(ui, |ui| {
                    for (index, (label, _)) in project::HUE_RANGES.into_iter().enumerate() {
                        ui.selectable_value(&mut selected, index, label);
                    }
                });
            ui.data_mut(|data| data.insert_temp(id, selected));
            let range = &mut hue_ranges[selected];
            egui::Grid::new("hue_adjustments")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Hue");
                    ui.add(
                        egui::DragValue::new(&mut range.hue)
                            .range(-180..=180)
                            .suffix("°"),
                    );
                    ui.end_row();
                    for (label, value) in [
                        ("Saturation", &mut range.saturation),
                        ("Intensity", &mut range.intensity),
                    ] {
                        ui.label(label);
                        ui.add(egui::DragValue::new(value).range(-100..=100).suffix("%"));
                        ui.end_row();
                    }
                });
            if ui.small_button("Reset color ranges").clicked() {
                hue_ranges = [project::HueAdjustment::default(); 6];
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Choose color LUT…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("3D color LUT", &["cube"])
                    .pick_file()
            {
                lut = Some(path);
            }
            if let Some(path) = &lut {
                ui.label(filename(path));
                if ui.small_button("Remove LUT").clicked() {
                    lut = None;
                }
            }
        });
        ui.separator();
        if self.title.is_none() {
            let mut use_key = chroma.is_some();
            if ui.checkbox(&mut use_key, "Chroma key").changed() {
                chroma = use_key.then(ChromaKey::default);
            }
            if let Some(key) = &mut chroma {
                egui::Grid::new("chroma_key").num_columns(2).show(ui, |ui| {
                    ui.label("Key color");
                    ui.color_edit_button_srgb(&mut key.color);
                    ui.end_row();
                    for (label, value) in [
                        ("Tolerance", &mut key.tolerance),
                        ("Softness", &mut key.softness),
                    ] {
                        ui.label(label);
                        ui.add(egui::DragValue::new(value).range(0..=100).suffix("%"));
                        ui.end_row();
                    }
                });
            }
        }
        if (grade, chroma) != (clip.adjustment, clip.chroma_key)
            || lut != clip.color_lut
            || hue_ranges != clip.hue_adjustments
        {
            self.update_selected_clip(LiveControl::Color, |clip| {
                clip.adjustment = grade;
                clip.chroma_key = chroma;
                clip.color_lut = lut;
                clip.hue_adjustments = hue_ranges;
            });
        }
    }

    fn mask_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Clip mask");
        let Some(clip) = self
            .selection
            .clip
            .as_deref()
            .and_then(|id| self.project.location(id))
            .filter(|(row, _)| self.project.tracks[*row].kind == TrackKind::Video)
            .map(|(row, index)| &self.project.tracks[row].clips[index])
        else {
            ui.label("Select a video, image, or text clip.");
            return;
        };
        if !clip.freeze
            && !clip.slot
            && self
                .project
                .asset(&clip.asset_id)
                .is_some_and(|asset| matches!(asset.kind, AssetKind::Video | AssetKind::Image))
        {
            if ui
                .button("Remove background…")
                .on_hover_text("Create a transparent copy using a local ONNX model")
                .clicked()
            {
                self.clip_action(ui.ctx(), ClipAction::RemoveBackground);
                return;
            }
            ui.separator();
        }
        let before = clip.mask;
        let stroke_count = clip.mask_strokes.len();
        ui.horizontal(|ui| {
            for (restore, label) in [(false, "Erase brush"), (true, "Restore brush")] {
                if ui
                    .add_enabled(
                        stroke_count < MaskStroke::MAX_STROKES,
                        egui::Button::selectable(self.mask_brush == Some(restore), label),
                    )
                    .clicked()
                {
                    self.mask_stroke = None;
                    self.mask_brush = (self.mask_brush != Some(restore)).then_some(restore);
                }
            }
        });
        if self.mask_brush.is_some() && stroke_count < MaskStroke::MAX_STROKES {
            ui.horizontal(|ui| {
                ui.label("Brush radius");
                let mut percent = self.mask_radius * 100.0;
                if ui
                    .add(egui::Slider::new(&mut percent, 0.5..=25.0).suffix("%"))
                    .changed()
                {
                    self.mask_radius = percent / 100.0;
                }
            });
            ui.label(
                RichText::new("Pause inside the selected clip, then paint in Preview.")
                    .color(MUTED),
            );
        } else if stroke_count >= MaskStroke::MAX_STROKES {
            ui.label("Brush stroke limit reached. Undo or clear the strokes to keep painting.");
        }
        if ui
            .add_enabled(stroke_count > 0, egui::Button::new("Clear brush strokes"))
            .clicked()
        {
            let id = self.selection.clip.clone().expect("selected clip");
            let cleared = self.commit(|project| {
                let (row, index) = project.location(&id).context("Select a clip.")?;
                project.tracks[row].clips[index].mask_strokes.clear();
                Ok(())
            });
            if let Err(error) = cleared {
                self.error("Clear mask brush strokes", error);
            }
            self.mask_stroke = None;
        }
        ui.separator();
        let mut mask = before;
        let mut enabled = mask.is_some();
        if ui.checkbox(&mut enabled, "Enable shape mask").changed() {
            mask = enabled.then(ClipMask::default);
        }
        if let Some(mask) = &mut mask {
            egui::ComboBox::from_id_salt("mask_shape")
                .selected_text(mask.shape.label())
                .show_ui(ui, |ui| {
                    for shape in [MaskShape::Rectangle, MaskShape::Ellipse, MaskShape::Linear] {
                        ui.selectable_value(&mut mask.shape, shape, shape.label());
                    }
                });
            egui::Grid::new("clip_mask_settings")
                .num_columns(2)
                .spacing(vec2(12.0, 6.0))
                .show(ui, |ui| {
                    for (name, value, minimum, maximum) in [
                        ("Center X", &mut mask.x, 0.0, 100.0),
                        ("Center Y", &mut mask.y, 0.0, 100.0),
                        ("Width", &mut mask.width, 1.0, 100.0),
                        ("Height", &mut mask.height, 1.0, 100.0),
                        ("Feather", &mut mask.feather, 0.0, 100.0),
                    ] {
                        if mask.shape == MaskShape::Linear && matches!(name, "Width" | "Height") {
                            continue;
                        }
                        ui.label(name);
                        let mut percent = *value * 100.0;
                        if ui
                            .add(
                                egui::DragValue::new(&mut percent)
                                    .range(minimum..=maximum)
                                    .suffix("%")
                                    .fixed_decimals(1),
                            )
                            .changed()
                        {
                            *value = percent / 100.0;
                        }
                        ui.end_row();
                    }
                    ui.label("Rotation");
                    ui.add(
                        egui::DragValue::new(&mut mask.rotation)
                            .range(-180.0..=180.0)
                            .suffix("°"),
                    );
                    ui.end_row();
                });
            ui.checkbox(&mut mask.invert, "Invert mask");
        }
        if mask != before {
            self.update_selected_clip(LiveControl::Mask, |clip| clip.mask = mask);
        }
    }

    fn audio_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Audio");
        let Some(id) = self.selection.clip.as_deref() else {
            ui.label(RichText::new("Select an audio or video clip.").color(MUTED));
            return;
        };
        let Some((row, index)) = self.project.location(id) else {
            return;
        };
        let clip = &self.project.tracks[row].clips[index];
        let has_audio = !clip.freeze
            && self
                .project
                .asset(&clip.asset_id)
                .is_some_and(|asset| asset.audio);
        if !has_audio {
            ui.label(RichText::new("This clip has no audio.").color(MUTED));
            return;
        }
        let original_pitch = clip.pitch_semitones;
        let mut pitch = original_pitch;
        let original_channels = (clip.audio_channels, clip.audio_balance);
        let (mut channels, mut balance) = original_channels;
        let id = id.to_owned();
        let original_fades = [clip.audio_fade.in_frames, clip.audio_fade.out_frames];
        let mut fades = original_fades;
        Self::fade_controls(ui, "audio_fades", clip.length, &mut fades);
        ui.horizontal(|ui| {
            ui.label("Pitch");
            ui.add(
                egui::DragValue::new(&mut pitch)
                    .range(-12..=12)
                    .suffix(" semitones"),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Channels");
            egui::ComboBox::from_id_salt("clip_audio_channels")
                .selected_text(channels.label())
                .show_ui(ui, |ui| {
                    for mode in project::AudioChannels::ALL {
                        ui.selectable_value(&mut channels, mode, mode.label());
                    }
                });
        });
        ui.add(
            egui::Slider::new(&mut balance, -100..=100)
                .text("Balance")
                .suffix("%"),
        );
        if fades != original_fades {
            self.update_selected_clip(LiveControl::Gain, |clip| {
                clip.set_audio_fade(fades[0], fades[1]);
            });
        }
        if pitch != original_pitch {
            self.update_selected_clip(LiveControl::Pitch, |clip| clip.pitch_semitones = pitch);
        }
        if (channels, balance) != original_channels {
            self.update_selected_clip(LiveControl::AudioChannels, |clip| {
                clip.audio_channels = channels;
                clip.audio_balance = balance;
            });
        }
        if ui
            .button("Normalize loudness")
            .on_hover_text("Targets −16 LUFS within the clip gain and peak limits.")
            .clicked()
        {
            self.normalize_selected_audio(ui.ctx());
        }
        if ui
            .button("Detect beats")
            .on_hover_text("Add timeline markers at beats in the selected clip")
            .clicked()
        {
            self.detect_selected_beats(ui.ctx());
        }
        if ui.button("Browse audio effects").clicked() {
            self.library = LibraryTab::Audio;
            self.show_library = true;
            self.audio_presets = true;
            self.inspector_tab = InspectorTab::Effects;
        }
        self.scalar_keyframes(ui, &id, project::AnimatedProperty::AudioGain);
    }

    fn normalize_selected_audio(&mut self, ctx: &egui::Context) {
        let Some((row, index)) = self
            .selection
            .clip
            .as_deref()
            .and_then(|id| self.project.location(id))
        else {
            return;
        };
        let clip = self.project.tracks[row].clips[index].clone();
        let Some(asset) = self.project.asset(&clip.asset_id).cloned() else {
            return;
        };
        self.start_job(
            ctx,
            "Measuring clip loudness",
            JobStyle::Cancellable,
            move |cancel, _| {
                let (gain, lufs) = media::normalize_clip_loudness(&clip, &asset, cancel)?;
                Ok(Completed::Loudness(clip, gain, lufs))
            },
        );
    }

    fn detect_selected_beats(&mut self, ctx: &egui::Context) {
        let Some((row, index)) = self
            .selection
            .clip
            .as_deref()
            .and_then(|id| self.project.location(id))
        else {
            return;
        };
        let clip = self.project.tracks[row].clips[index].clone();
        let Some(asset) = self.project.asset(&clip.asset_id).cloned() else {
            return;
        };
        self.stop_player();
        self.start_job(
            ctx,
            "Finding beats",
            JobStyle::Cancellable,
            move |cancel, _| {
                let positions = media::detect_clip_beats(&clip, &asset, cancel)?;
                Ok(Completed::Beats(clip, positions))
            },
        );
    }

    fn fade_controls(ui: &mut egui::Ui, id: &str, clip_length: u32, fades: &mut [u32; 2]) {
        egui::Grid::new(id).num_columns(2).show(ui, |ui| {
            for (name, length) in ["Fade in", "Fade out"].into_iter().zip(fades) {
                ui.label(name);
                let mut seconds = f64::from(*length) / f64::from(FPS);
                if ui
                    .add(
                        egui::DragValue::new(&mut seconds)
                            .range(0.0..=f64::from(clip_length.max(*length)) / f64::from(FPS))
                            .speed(0.033)
                            .suffix(" s")
                            .fixed_decimals(2),
                    )
                    .changed()
                {
                    *length = (seconds * f64::from(FPS)).round() as u32;
                }
                ui.end_row();
            }
        });
    }

    fn clip_inspector(&mut self, ui: &mut egui::Ui) {
        self.speed_controls(ui);
        if let Some((edit, gain)) = &mut self.edit {
            let old_transform = self.transform;
            let clip = &self.project.tracks[edit.row]
                .clips
                .iter()
                .find(|clip| clip.id == edit.id)
                .unwrap();
            let has_source = clip.source > 0;
            let original_framing = clip.framing;
            let mut framing = original_framing;
            let original_blend = clip.blend_mode;
            let mut blend_mode = original_blend;
            let animated_position = !clip.position_animation.is_empty();
            let animated_opacity = !clip.opacity_animation.is_empty();
            let animated_rotation = !clip.rotation_animation.is_empty();
            let animated_scale = !clip.scale_animation.is_empty();
            if let Some(asset) = self.project.asset(&clip.asset_id) {
                let title = if clip.slot {
                    format!("Slot · {}", asset.label)
                } else {
                    asset.label.clone()
                };
                ui.label(RichText::new(title).strong());
            }
            let track_is_video = self.project.tracks[edit.row].kind == TrackKind::Video;
            if track_is_video {
                ui.heading("Transform");
                let transform = &mut self.transform;
                egui::Grid::new("transform_options")
                    .spacing(vec2(10.0, 6.0))
                    .show(ui, |ui| {
                        for (label, amount, min, max) in [
                            ("Scale", &mut transform.scale, 0.1, project::MAX_CLIP_SCALE),
                            ("Position X", &mut transform.x, -1.5, 1.5),
                            ("Position Y", &mut transform.y, -1.5, 1.5),
                            ("Opacity", &mut transform.opacity, 0.0, 1.0),
                        ] {
                            if (animated_position && label.starts_with("Position"))
                                || (animated_opacity && label == "Opacity")
                                || (animated_scale && label == "Scale")
                            {
                                continue;
                            }
                            ui.label(label);
                            let mut percent = *amount * 100.0;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut percent)
                                        .range(min * 100.0..=max * 100.0)
                                        .suffix("%")
                                        .fixed_decimals(0),
                                )
                                .changed()
                            {
                                *amount = percent / 100.0;
                            }
                            ui.end_row();
                        }
                        if !animated_rotation {
                            ui.label("Rotation");
                            ui.add(
                                egui::DragValue::new(&mut transform.rotation)
                                    .range(-180.0..=180.0)
                                    .suffix("°"),
                            );
                            ui.end_row();
                        }
                    });
                if ui.button("Animations and keyframes…").clicked() {
                    self.inspector_tab = InspectorTab::Motion;
                }
                if ui.button("Reset transform").clicked() {
                    self.transform = Transform::default();
                }
                ui.horizontal(|ui| {
                    ui.label("Blend");
                    egui::ComboBox::from_id_salt("clip_blend_mode")
                        .selected_text(blend_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in project::BlendMode::ALL {
                                ui.selectable_value(&mut blend_mode, mode, mode.label());
                            }
                        });
                });
                if self.title.is_none() {
                    ui.collapsing("Crop and mirror", |ui| {
                        ui.checkbox(&mut framing.flip_horizontal, "Mirror horizontally");
                        ui.checkbox(&mut framing.flip_vertical, "Flip vertically");
                        egui::Grid::new("crop_edges").show(ui, |ui| {
                            let [left, right, top, bottom] =
                                [framing.left, framing.right, framing.top, framing.bottom];
                            for (label, value, opposite) in [
                                ("Left", &mut framing.left, right),
                                ("Right", &mut framing.right, left),
                                ("Top", &mut framing.top, bottom),
                                ("Bottom", &mut framing.bottom, top),
                            ] {
                                ui.label(label);
                                ui.add(
                                    egui::DragValue::new(value)
                                        .range(0..=95 - opposite)
                                        .suffix("%"),
                                );
                                ui.end_row();
                            }
                        });
                        if ui.button("Reset crop and mirror").clicked() {
                            framing = project::Framing::default();
                        }
                    });
                }
            }
            ui.separator();
            ui.label(RichText::new("Timing").strong());
            ui.add_space(7.0);
            egui::Grid::new("clip_properties")
                .spacing(vec2(12.0, 6.0))
                .show(ui, |ui| {
                    for (name, value, min) in [
                        ("Start frame", &mut edit.start, 0),
                        ("Source in", &mut edit.source, 0),
                        ("Length (frames)", &mut edit.length, 1),
                    ] {
                        ui.label(name);
                        ui.add(egui::DragValue::new(value).range(min..=MAX_FRAMES));
                        ui.end_row();
                    }
                });
            let values = (edit.clone(), *gain);
            if ui.button("Apply timing").clicked() {
                self.apply_placement(values.0, values.1);
            }
            if self.transform != old_transform {
                let transform = self.transform;
                self.update_selected_clip(LiveControl::Transform, |clip| {
                    clip.transform = transform
                });
            }
            if framing != original_framing {
                self.update_selected_clip(LiveControl::Framing, |clip| clip.framing = framing);
            }
            if blend_mode != original_blend {
                self.update_selected_clip(LiveControl::Blend, |clip| clip.blend_mode = blend_mode);
            }
            if self.title.is_none() {
                ui.separator();
                if ui.button("Replace media…").clicked() {
                    self.replace_media(ui.ctx(), false);
                }
                if has_source && ui.button("Relink original…").clicked() {
                    self.replace_media(ui.ctx(), true);
                }
            }
        } else {
            ui.label(
                RichText::new("Select a clip to edit its timing or replace media.").color(MUTED),
            );
            ui.add_space(8.0);
            ui.separator();
            self.canvas_options(ui);
        }
    }

    fn speed_controls(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        let Some((row, index)) = self.project.location(&id) else {
            return;
        };
        let clip = &self.project.tracks[row].clips[index];
        if clip.freeze
            || clip.slot
            || !self
                .project
                .asset(&clip.asset_id)
                .is_some_and(|asset| matches!(asset.kind, AssetKind::Video | AssetKind::Audio))
        {
            return;
        }
        let original = clip.speed_percent;
        let mut speed = original;
        ui.horizontal(|ui| {
            ui.label("Playback speed");
            egui::ComboBox::from_id_salt("clip_speed")
                .selected_text(format!("{:.2}×", f32::from(speed) / 100.0))
                .show_ui(ui, |ui| {
                    for percent in [25, 50, 75, 100, 125, 150, 200, 300, 400] {
                        ui.selectable_value(
                            &mut speed,
                            percent,
                            format!("{:.2}×", f32::from(percent) / 100.0),
                        );
                    }
                });
        });
        if speed != original {
            match self.commit(|project| project.set_clip_speed(&id, speed)) {
                Ok(()) => {
                    self.status = format!("Playback speed set to {:.2}×.", f32::from(speed) / 100.0)
                }
                Err(error) => self.error("Change playback speed", error),
            }
        }
    }

    fn canvas_options(&mut self, ui: &mut egui::Ui) {
        let mut size = (self.project.width, self.project.height);
        egui::ComboBox::from_id_salt("canvas")
            .selected_text(format!("Canvas  {} × {}", size.0, size.1))
            .show_ui(ui, |ui| {
                for (w, h, label) in [
                    (1280, 720, "HD"),
                    (1920, 1080, "Full HD"),
                    (2560, 1440, "QHD"),
                    (3840, 2160, "4K"),
                    (720, 1280, "HD portrait"),
                    (1080, 1920, "Full HD portrait"),
                    (1080, 1080, "Square"),
                ] {
                    if ui
                        .selectable_label(size == (w, h), format!("{label} · {w} × {h}"))
                        .clicked()
                    {
                        size = (w, h);
                    }
                }
            });
        if size != (self.project.width, self.project.height) {
            self.match_first_video = false;
            if let Err(error) = self.commit(|project| {
                (project.width, project.height) = size;
                Ok(())
            }) {
                self.error("Change canvas", error);
            }
        }
        if ui.button("Match first video").clicked() {
            self.match_source(ui.ctx());
        }
    }

    fn match_source(&mut self, ctx: &egui::Context) {
        let first = self
            .project
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .flat_map(|track| &track.clips)
            .filter_map(|clip| {
                self.project
                    .asset(&clip.asset_id)
                    .filter(|asset| asset.kind == AssetKind::Video && !clip.slot)
                    .map(|asset| (clip.start, asset))
            })
            .min_by_key(|(start, _)| *start);
        let Some((_, asset)) = first else {
            self.status = "Add a video to the timeline to match its size.".into();
            return;
        };
        let id = asset.id.clone();
        if let Some(geometry) = asset.geometry {
            self.complete(Completed::MatchCanvas(id, geometry));
        } else if let Some(path) = asset.path.clone() {
            self.start_job(
                ctx,
                "Reading source dimensions",
                JobStyle::Read,
                move |_, _| {
                    let geometry = media::probe(&path)?
                        .geometry
                        .context("The video has no readable dimensions.")?;
                    Ok(Completed::MatchCanvas(id, geometry))
                },
            );
        }
    }

    fn toggle_focus_view(&mut self) {
        if let Some((library, inspector)) = self.restore_panels.take() {
            self.show_library = library;
            self.show_inspector = inspector;
        } else {
            self.restore_panels = Some((self.show_library, self.show_inspector));
            self.show_library = false;
            self.show_inspector = false;
        }
    }

    fn paint_preview_mask(&mut self, ui: &mut egui::Ui, image: egui::Rect, area: egui::Rect) {
        let target = self
            .mask_brush
            .filter(|_| self.show_inspector && self.inspector_tab == InspectorTab::Mask)
            .filter(|_| !self.preview.is_playing())
            .zip(self.selection.clip.as_ref())
            .and_then(|(restore, id)| {
                let (row, index) = self.project.location(id)?;
                let track = &self.project.tracks[row];
                let clip = &track.clips[index];
                let valid = track.kind == TrackKind::Video
                    && !track.muted
                    && (clip.start..clip.end()).contains(&self.frame)
                    && clip.mask_strokes.len() < MaskStroke::MAX_STROKES;
                valid.then_some((restore, clip))
            });
        let Some((restore, clip)) = target else {
            self.mask_stroke = None;
            return;
        };
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.mask_brush = None;
            self.mask_stroke = None;
            return;
        }
        let frame = self.frame - clip.start;
        let scale = clip.animated_value(AnimatedProperty::Scale, frame);
        let angle = clip.animated_value(AnimatedProperty::Rotation, frame);
        let rotation = egui::emath::Rot2::from_angle(angle.to_radians());
        let [x, y] = clip.position_at(frame);
        let center = image.center() + vec2(x * image.width(), y * image.height());
        let shown = image.intersect(area);
        let response = ui.interact(shown, ui.id().with("mask_brush"), Sense::click_and_drag());
        let (hover, press, release, down) = ui.input(|input| {
            let mut press = None;
            let mut release = None;
            for event in &input.events {
                if let egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pos,
                    pressed,
                    ..
                } = event
                {
                    if *pressed {
                        press = Some(*pos);
                    } else {
                        release = Some(*pos);
                    }
                }
            }
            (
                input.pointer.interact_pos(),
                press,
                release,
                input.pointer.primary_down(),
            )
        });
        let to_canvas = |pointer: egui::Pos2| {
            let relative = rotation.inverse() * (pointer - center) / scale;
            [
                0.5 + relative.x / image.width(),
                0.5 + relative.y / image.height(),
            ]
        };
        let in_canvas = |point: &[f32; 2]| point.iter().all(|v| (0.0..=1.0).contains(v));
        if let Some(point) = press
            .filter(|pos| shown.contains(*pos) && ui.ctx().layer_id_at(*pos) == Some(ui.layer_id()))
            .map(to_canvas)
            .filter(in_canvas)
        {
            self.mask_stroke = Some(MaskStroke {
                radius: self.mask_radius,
                restore,
                points: vec![point],
            });
        }
        let painter = ui.painter().with_clip_rect(shown);
        let color = if restore {
            LIME
        } else {
            Color32::from_rgb(255, 138, 138)
        };
        if let Some(stroke) = &mut self.mask_stroke {
            if let Some([x, y]) = release.or(hover).map(to_canvas) {
                let next = [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)];
                let [last_x, last_y] = stroke.points[stroke.points.len() - 1];
                let delta = vec2(
                    (next[0] - last_x) * image.width(),
                    (next[1] - last_y) * image.height(),
                );
                if delta.length() * scale >= 0.75 {
                    if stroke.points.len() == MaskStroke::MAX_POINTS {
                        stroke.points = stroke.points.iter().step_by(2).copied().collect();
                    }
                    stroke.points.push(next);
                }
            }
            let radius = stroke.radius * image.width().min(image.height()) * scale;
            let displayed: Vec<_> = stroke
                .points
                .iter()
                .map(|[x, y]| {
                    center
                        + rotation
                            * vec2((x - 0.5) * image.width(), (y - 0.5) * image.height())
                            * scale
                })
                .collect();
            let ink = color.gamma_multiply(0.45);
            painter.circle_filled(displayed[0], radius, ink);
            painter.add(egui::Shape::line(displayed, Stroke::new(radius * 2.0, ink)));
        }
        if let Some(pointer) =
            hover.filter(|p| in_canvas(&to_canvas(*p)) && response.contains_pointer())
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let radius = self.mask_radius * image.width().min(image.height()) * scale;
            painter.circle_stroke(pointer, radius, Stroke::new(1.5, color));
        }
        if release.is_some()
            && let Some(stroke) = self.mask_stroke.take()
        {
            let id = clip.id.clone();
            let result = self.commit(|project| {
                let (row, index) = project.location(&id).context("Select a clip.")?;
                project.tracks[row].clips[index].mask_strokes.push(stroke);
                Ok(())
            });
            if let Err(error) = result {
                self.error("Paint clip mask", error);
            }
        } else if !down {
            self.mask_stroke = None;
        }
    }

    fn monitor(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Preview");
            let current = self.preview.quality();
            let mut quality = current;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                egui::ComboBox::from_id_salt("preview_quality").selected_text(quality.label())
                    .show_ui(ui, |ui| {
                        for option in [PreviewQuality::Auto, PreviewQuality::Full, PreviewQuality::Half, PreviewQuality::Quarter] {
                            ui.selectable_value(&mut quality, option, option.label());
                        }
                    }).response.on_hover_text("Auto uses the full canvas when paused and lower resolution while scrubbing or playing.");
                if ui.button(if self.restore_panels.is_some() { "Restore" } else { "Focus" }).clicked() {
                    self.toggle_focus_view();
                }
                if ui.selectable_label(self.pixel_view, "1:1").on_hover_text("Inspect at one display pixel per preview pixel").clicked() {
                    self.pixel_view = !self.pixel_view;
                }
                ui.menu_button("Canvas", |ui| self.canvas_options(ui));
            });
            if quality != current {
                self.preview.set_quality(quality);
                ui.ctx().request_repaint();
            }
        });
        let available = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(
            vec2(available.x, (available.y - 65.0).max(58.0)),
            Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect(
            rect,
            10,
            Color32::from_rgb(9, 11, 14),
            Stroke::new(1.0, LINE),
            StrokeKind::Inside,
        );
        if let Some(texture) = &self.texture {
            let original = texture.size_vec2();
            let area = rect.shrink(6.0);
            let factor = if self.pixel_view {
                1.0 / ui.ctx().pixels_per_point()
            } else {
                (area.width() / original.x).min(area.height() / original.y)
            };
            let mut image_rect = egui::Rect::from_center_size(area.center(), original * factor);
            if self.pixel_view {
                let dpi = ui.ctx().pixels_per_point();
                let snapped = egui::pos2(
                    (image_rect.min.x * dpi).round() / dpi,
                    (image_rect.min.y * dpi).round() / dpi,
                );
                image_rect = egui::Rect::from_min_size(snapped, original / dpi);
            }
            painter.with_clip_rect(area).image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            if self.show_tracking_patch
                && self.inspector_tab == InspectorTab::Motion
                && self
                    .edit
                    .as_ref()
                    .is_some_and(|(clip, _)| clip.start == self.frame)
            {
                let size = image_rect.width().min(image_rect.height())
                    * f32::from(self.tracking_seed.size)
                    / 100.0;
                let half = size / 2.0;
                let x = (image_rect.left()
                    + image_rect.width() * f32::from(self.tracking_seed.x) / 100.0)
                    .clamp(image_rect.left() + half, image_rect.right() - half);
                let y = (image_rect.top()
                    + image_rect.height() * f32::from(self.tracking_seed.y) / 100.0)
                    .clamp(image_rect.top() + half, image_rect.bottom() - half);
                painter.with_clip_rect(area).rect_stroke(
                    egui::Rect::from_center_size(egui::pos2(x, y), vec2(size, size)),
                    2.0,
                    Stroke::new(2.0, ACCENT),
                    StrokeKind::Inside,
                );
            }
            self.paint_preview_mask(ui, image_rect, area);
        } else {
            let label = if self.project.duration() == 0 {
                "The timeline is empty"
            } else {
                "Preparing preview…"
            };
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(16.0),
                MUTED,
            );
        }
        let duration = self.project.duration();
        let mut frame = self.frame.min(duration);
        let slider = egui::Slider::new(&mut frame, 0..=duration.max(1))
            .show_value(false)
            .trailing_fill(true);
        if ui
            .add_enabled_ui(duration > 0, |ui| {
                ui.spacing_mut().slider_width = ui.available_width();
                ui.add(slider).on_hover_text("Scrub the preview").changed()
            })
            .inner
        {
            self.seek(frame);
        }
        ui.horizontal(|ui| {
            let playing = self.preview.is_playing();
            if ui
                .add_enabled(duration > 0 && self.frame > 0, egui::Button::new("‹ Frame"))
                .clicked()
            {
                self.seek(self.frame.saturating_sub(1));
            }
            let play = egui::Button::new(
                RichText::new(if playing { "Pause" } else { "Play" })
                    .strong()
                    .color(ACCENT),
            )
            .fill(ACCENT_DARK)
            .min_size(vec2(66.0, 26.0));
            if ui.add_enabled(duration > 0, play).clicked() {
                self.play(ui.ctx());
            }
            if ui
                .add_enabled(self.frame < duration, egui::Button::new("Frame ›"))
                .clicked()
            {
                self.seek(self.frame.saturating_add(1));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(timestamp(self.frame))
                        .monospace()
                        .strong()
                        .color(ACCENT)
                        .size(15.0),
                );
                if let Some(texture) = &self.texture {
                    let [w, h] = texture.size();
                    ui.label(RichText::new(format!("{w} × {h}")).small().color(MUTED));
                }
            });
        });
    }

    fn marker_menu(&mut self, ui: &mut egui::Ui) {
        let frame = self.frame;
        let existing = self
            .project
            .markers
            .iter()
            .find(|marker| marker.frame == frame);
        let mut label = existing.map_or_else(String::new, |marker| marker.label.clone());
        let input_id = ui.make_persistent_id(("marker_name", frame));
        let saved = ui.data(|data| data.get_temp::<String>(input_id));
        if let Some(saved) = saved {
            label = saved;
        }
        if ui
            .add(
                egui::TextEdit::singleline(&mut label)
                    .char_limit(64)
                    .hint_text("Marker name"),
            )
            .changed()
        {
            ui.data_mut(|data| data.insert_temp(input_id, label.clone()));
        }
        let mut remove = None;
        let mut seek = None;
        if ui
            .button(if existing.is_some() {
                "Update marker"
            } else {
                "Add marker at playhead"
            })
            .clicked()
        {
            let fallback = format!("Marker {}", self.project.markers.len() + 1);
            let name = if label.trim().is_empty() {
                &fallback
            } else {
                &label
            };
            let result = self.commit(|project| project.set_marker(frame, name));
            if let Err(error) = result {
                self.error("Set marker", error);
            } else {
                ui.data_mut(|data| data.remove::<String>(input_id));
            }
            ui.close();
        }
        if !self.project.markers.is_empty() {
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for marker in &self.project.markers {
                        ui.horizontal(|ui| {
                            let title = format!("{}  {}", timestamp(marker.frame), marker.label);
                            if ui
                                .add_enabled(
                                    marker.frame <= self.project.duration(),
                                    egui::Button::new(title).truncate(),
                                )
                                .clicked()
                            {
                                seek = Some(marker.frame);
                            }
                            if ui.small_button("Remove").clicked() {
                                remove = Some(marker.frame);
                            }
                        });
                    }
                });
        }
        if let Some(frame) = remove {
            let _ = self.commit(|project| {
                project.remove_marker(frame);
                Ok(())
            });
        } else if let Some(frame) = seek {
            self.seek(frame);
            ui.close();
        }
    }

    fn timeline_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Timeline");
            if ui
                .add_enabled(!self.history.is_empty(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(!self.future.is_empty(), egui::Button::new("Redo"))
                .clicked()
            {
                self.redo();
            }
            ui.separator();
            if ui
                .add_enabled(self.selection.clip.is_some(), egui::Button::new("Split"))
                .clicked()
            {
                self.split();
            }
            if ui
                .add_enabled(self.selection.clip.is_some(), egui::Button::new("Remove"))
                .clicked()
            {
                self.delete();
            }
            if ui
                .button("Voiceover…")
                .on_hover_text("Record a microphone at the playhead")
                .clicked()
            {
                self.stop_player();
                match crate::voiceover::input_names() {
                    Ok(devices) => {
                        self.voiceover_setup = Some(VoiceoverSetup {
                            devices,
                            monitor: true,
                            device: None,
                        });
                    }
                    Err(error) => self.error("Open microphone settings", error),
                }
            }
            ui.menu_button("Tracks", |ui| {
                if ui.button("Add video track").clicked() {
                    ui.close();
                    self.add_track(TrackKind::Video);
                }
                if ui.button("Add audio track").clicked() {
                    ui.close();
                    self.add_track(TrackKind::Audio);
                }
                if ui.button("Close gaps on selected track").clicked() {
                    ui.close();
                    let row = self.selection.row;
                    match self.commit(|project| project.close_track_gaps(row)) {
                        Ok(frames) => {
                            self.status = format!("Closed {} of gaps.", timestamp(frames))
                        }
                        Err(error) => self.error("Close track gaps", error),
                    }
                }
                if ui
                    .add_enabled(
                        self.multi_selection.len() >= 2,
                        egui::Button::new("Group selected clips"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.group_selection();
                }
                if ui
                    .add_enabled(
                        (2..=9).contains(&self.multi_selection.len()),
                        egui::Button::new("Sync selected clips by audio"),
                    )
                    .on_hover_text("Align with the last clip selected")
                    .clicked()
                {
                    ui.close();
                    if let Err(error) = self.sync_selected_audio(ui.ctx()) {
                        self.error("Sync clips by audio", error);
                    }
                }
                if ui
                    .add_enabled(
                        (2..=9).contains(&self.multi_selection.len()),
                        egui::Button::new("Sync selected clips by points"),
                    )
                    .on_hover_text("Align the clips using their marked sync points")
                    .clicked()
                {
                    ui.close();
                    if let Err(error) = self.sync_selected_markers() {
                        self.error("Sync clips by points", error);
                    }
                }
                if ui
                    .add_enabled(
                        (2..=9).contains(&self.multi_selection.len()),
                        egui::Button::new("Edit camera angles…"),
                    )
                    .on_hover_text("Use the last clip selected as the reference angle")
                    .clicked()
                {
                    ui.close();
                    if let Err(error) = self.start_camera_edit() {
                        self.error("Edit camera angles", error);
                    }
                }
                let row = self.selection.row;
                let label = if self.project.tracks[row].muted {
                    "Enable selected track"
                } else {
                    "Disable selected track"
                };
                if ui.button(label).clicked() {
                    ui.close();
                    let _ = self.commit(|project| {
                        project.tracks[row].muted = !project.tracks[row].muted;
                        Ok(())
                    });
                }
                if ui
                    .add_enabled(
                        self.project.tracks[row].clips.is_empty() && self.project.tracks.len() > 1,
                        egui::Button::new("Remove empty track"),
                    )
                    .clicked()
                {
                    ui.close();
                    let _ = self.commit(|project| {
                        project.tracks.remove(row);
                        Ok(())
                    });
                }
            });
            egui::containers::menu::MenuButton::new("Markers")
                .config(
                    egui::containers::menu::MenuConfig::new()
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| self.marker_menu(ui));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::Slider::new(&mut self.zoom, 12.0..=220.0)
                        .show_value(false)
                        .text("Zoom"),
                );
                ui.checkbox(&mut self.snap, "Snap");
                let mut magnet = self.main_track_magnet;
                if ui
                    .checkbox(&mut magnet, "Main track magnet")
                    .on_hover_text("Keep clips on the first video track contiguous")
                    .changed()
                {
                    if magnet {
                        let row = self
                            .project
                            .tracks
                            .iter()
                            .position(|track| track.kind == TrackKind::Video);
                        let result = row.map_or(Ok(0), |row| {
                            self.commit(|project| project.close_track_gaps(row))
                        });
                        match result {
                            Ok(_) => self.main_track_magnet = true,
                            Err(error) => self.error("Enable main track magnet", error),
                        }
                    } else {
                        self.main_track_magnet = false;
                    }
                }
            });
        });
        let events = self.timeline.show(
            ui,
            View {
                can_paste_settings: self.copied_clip_settings.is_some(),
                project: &self.project,
                selection: &self.selection,
                multi_selection: &self.multi_selection,
                frame: self.frame,
                zoom: self.zoom,
                snap: self.snap,
            },
            &mut self.thumbnails,
        );
        if let Some((asset, row, frame)) = events.add {
            self.insert_at(&asset, row, frame);
        }
        if let Some(selection) = events.select {
            self.select(selection);
        }
        if let Some(selection) = events.toggle_selection {
            self.toggle_clip_selection(selection);
        }
        if let Some(frame) = events.seek {
            self.seek(frame);
        }
        if let Some(placement) = events.edit
            && let Some((row, index)) = self.project.location(&placement.id)
        {
            let gain = self.project.tracks[row].clips[index].gain;
            self.apply_placement(placement, gain);
        }
        if let Some((id, action)) = events.action
            && let Some((row, _)) = self.project.location(&id)
        {
            self.select(Selection {
                row,
                clip: Some(id),
            });
            self.clip_action(ui.ctx(), action);
        }
    }
}

impl eframe::App for Editor {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.collect_work(ctx);
        self.collect_voiceover(ctx);
        self.thumbnails.collect(ctx);
        match self.autosave.tick(ctx, &self.project) {
            Ok(true) => self.draft_saved = true,
            Ok(false) => {}
            Err(error) => {
                self.draft_saved = false;
                self.status = format!("Autosave failed. {error}");
            }
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.voiceover.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if let Some(job) = &self.job {
                job.cancel.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if !self.may_discard() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                self.autosave.clear();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.job.is_none()
            && self.report.is_none()
            && self.export_dialog.is_none()
            && self.font_browser.is_none()
            && self.voiceover.is_none()
            && self.voiceover_setup.is_none()
            && self.recovery_candidates.is_none()
        {
            let save =
                ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
            if save {
                self.save(false);
            }
            if !ctx.egui_wants_keyboard_input() {
                if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z))
                {
                    self.undo();
                }
                if ctx.input_mut(|input| {
                    input.consume_key(
                        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
                        egui::Key::Z,
                    )
                }) {
                    self.redo();
                }
                if ctx.input(|input| input.key_pressed(egui::Key::Delete)) {
                    self.delete();
                }
                if ctx.input_mut(|input| {
                    input.consume_key(
                        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
                        egui::Key::G,
                    )
                }) {
                    self.clip_action(&ctx, ClipAction::Ungroup);
                } else if ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::G))
                {
                    self.group_selection();
                }
                if ctx.input(|input| input.key_pressed(egui::Key::F)) {
                    self.toggle_focus_view();
                }
                if ctx.input(|input| input.key_pressed(egui::Key::Space))
                    && self.project.duration() > 0
                {
                    self.play(&ctx);
                }
            }
            let mut dropped: Vec<PathBuf> = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_owned())
                    .collect()
            });
            if self.library == LibraryTab::Stickers && dropped.len() == 1 {
                self.import_sticker(&ctx, dropped.pop().expect("exactly one dropped file"));
            } else {
                self.import_media(&ctx, dropped);
            }
        }
        if self.voiceover.is_some() {
            ui.disable();
        }
        ui.painter().rect_filled(ui.max_rect(), 0, BACK);
        let room = ui.available_size();
        egui::Panel::top("header")
            .exact_size(47.0)
            .show_separator_line(false)
            .frame(theme::panel().inner_margin(egui::Margin::symmetric(11, 5)))
            .show(ui, |ui| self.header(ui));
        egui::Panel::bottom("status")
            .exact_size(25.0)
            .frame(egui::Frame::new().fill(BACK).inner_margin(7))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let status = self.preview.status().unwrap_or(&self.status);
                    ui.add(egui::Label::new(RichText::new(status).color(MUTED).small()).truncate())
                        .on_hover_text(status);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Duration  {}",
                                timestamp(self.project.duration())
                            ))
                            .color(MUTED)
                            .monospace(),
                        );
                        if self.draft_saved {
                            ui.label(RichText::new("Draft backed up").color(MUTED).small());
                        }
                    });
                });
            });
        let mut timeline_visible = self.restore_panels.is_none();
        egui::Panel::bottom("timeline")
            .show_separator_line(false)
            .default_size((room.y * 0.36).clamp(215.0, 410.0))
            .min_size(170.0)
            .max_size((room.y * 0.57).max(230.0))
            .resizable(true)
            .frame(theme::panel())
            .drag_to_open(false)
            .show_collapsible(ui, &mut timeline_visible, |ui| self.timeline_panel(ui));
        egui::Panel::left("workspace_tools")
            .exact_size(68.0)
            .show_separator_line(false)
            .frame(theme::panel().inner_margin(3))
            .show(ui, |ui| self.workspace_rail(ui));

        let mut library = self.show_library;
        egui::Panel::left("media")
            .show_separator_line(false)
            .default_size((room.x * 0.205).clamp(230.0, 340.0))
            .min_size(214.0)
            .max_size((room.x * 0.34).max(236.0))
            .resizable(true)
            .frame(theme::panel())
            .show_collapsible(ui, &mut library, |ui| self.library_panel(ui));
        self.show_library = library;

        let mut inspector = self.show_inspector;
        egui::Panel::right("inspector")
            .show_separator_line(false)
            .default_size((room.x * 0.255).clamp(250.0, 368.0))
            .min_size(220.0)
            .max_size((room.x * 0.32).clamp(250.0, 420.0))
            .resizable(true)
            .frame(theme::panel())
            .show_collapsible(ui, &mut inspector, |ui| self.inspector(ui));
        self.show_inspector = inspector;
        egui::CentralPanel::default()
            .frame(theme::panel())
            .show(ui, |ui| self.monitor(ui));
        self.voiceover_dialog(&ctx);
        self.camera_editor(&ctx);
        self.recovery_dialog(&ctx);
        if let Some(browser) = &mut self.font_browser
            && let Some(choice) = browser.show(&ctx)
        {
            self.font_browser = None;
            if let FontChoice::Choose(font) = choice {
                self.apply_font(font);
            }
        }
        if self.job.is_none()
            && self.report.is_none()
            && let Some(dialog) = &mut self.export_dialog
            && let Some(decision) = dialog.show(&ctx, &self.project, self.texture.as_ref())
        {
            self.export_dialog = None;
            if let Decision::Render(path, settings, range, details) = decision {
                self.export(&ctx, path, settings, range, details);
            }
        }
        if let Some(job) = &self.job {
            egui::Modal::new(egui::Id::new("job")).show(&ctx, |ui| {
                ui.set_min_width(335.0);
                ui.heading(job.title);
                if let Some(progress) = &job.progress {
                    let percent = progress.load(Ordering::Relaxed);
                    ui.add(egui::ProgressBar::new(f32::from(percent) / 100.0).show_percentage());
                } else {
                    ui.spinner();
                }
                if job.cancellable
                    && ui
                        .add_enabled(
                            !job.cancel.load(Ordering::Relaxed),
                            egui::Button::new("Cancel"),
                        )
                        .clicked()
                {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
        } else if let Some((title, lines)) = &self.report {
            let response = egui::Modal::new(egui::Id::new("report")).show(&ctx, |ui| {
                ui.set_min_width(390.0);
                ui.set_max_width(625.0);
                ui.heading(title);
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for line in lines {
                            ui.label(RichText::new(line).color(TEXT));
                        }
                    });
                ui.button("Close").clicked()
            });
            if response.inner || response.should_close() {
                self.report = None;
            }
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.stop_player();
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            if job.progress.is_some() {
                let _ = job.worker.join();
            }
        }
    }
}

fn title_tile(ui: &mut egui::Ui, style: TitleStyle, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 90.0), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, style.label()));
    let p = ui.painter().with_clip_rect(rect.shrink(1.0));
    p.rect(
        rect,
        11,
        BACK,
        Stroke::new(1.0, if response.hovered() { ACCENT } else { LINE }),
        StrokeKind::Inside,
    );
    let pos = rect.center() + vec2(0.0, -10.0);
    let sample = style.sample();
    let font = FontId::proportional(if sample.len() > 10 && width < 155.0 {
        15.0
    } else {
        18.0
    });
    let (ink, shadow) = match style {
        TitleStyle::Neon => (
            Color32::from_rgb(255, 223, 255),
            Color32::from_rgb(186, 81, 252),
        ),
        TitleStyle::Glitch => (TEXT, Color32::from_rgb(246, 78, 170)),
        TitleStyle::Pop => (LIME, Color32::from_rgb(111, 58, 160)),
        _ => (TEXT, ACCENT_DARK),
    };
    p.rect_filled(
        egui::Rect::from_min_max(
            rect.min + vec2(5.0, 5.0),
            egui::pos2(rect.right() - 5.0, rect.top() + 64.0),
        ),
        8,
        shadow.gamma_multiply(0.24),
    );
    if style == TitleStyle::Glitch {
        p.text(
            pos - vec2(2.7, 1.7),
            Align2::CENTER_CENTER,
            sample,
            font.clone(),
            Color32::from_rgb(54, 217, 236),
        );
    }
    for (x, y) in [(-2.0, 1.0), (0.0, 3.0), (2.0, 1.0)] {
        p.text(
            pos + vec2(x, y),
            Align2::CENTER_CENTER,
            sample,
            font.clone(),
            shadow,
        );
    }
    p.text(pos, Align2::CENTER_CENTER, sample, font, ink);
    p.text(
        rect.min + vec2(9.0, 77.0),
        Align2::LEFT_CENTER,
        style.label(),
        FontId::proportional(11.5),
        MUTED,
    );
    response
}

fn dock_icon(painter: &egui::Painter, tab: LibraryTab, at: egui::Pos2, color: Color32) {
    let stroke = Stroke::new(1.35, color);
    let p = |x, y| at + vec2(x, y);
    let line = |a, b| painter.line_segment([a, b], stroke);
    match tab {
        LibraryTab::Media => {
            painter.rect_stroke(
                egui::Rect::from_center_size(at, vec2(16.0, 12.0)),
                2,
                stroke,
                StrokeKind::Inside,
            );
            painter.add(egui::Shape::convex_polygon(
                vec![p(-2.0, -3.0), p(4.0, 0.0), p(-2.0, 3.0)],
                color,
                Stroke::NONE,
            ));
        }
        LibraryTab::Audio => {
            for (index, h) in [3.0, 5.0, 8.0, 6.0, 3.0].into_iter().enumerate() {
                let x = index as f32 * 3.0 - 6.0;
                line(p(x, -h), p(x, h));
            }
        }
        LibraryTab::Text => {
            line(p(-6.0, -6.0), p(6.0, -6.0));
            line(p(0.0, -6.0), p(0.0, 6.0));
            line(p(-3.0, 6.0), p(3.0, 6.0));
        }
        LibraryTab::Stickers => {
            let outline = [
                p(-7.5, -7.5),
                p(7.5, -7.5),
                p(7.5, 1.6),
                p(1.6, 7.5),
                p(-7.5, 7.5),
            ];
            painter.add(egui::Shape::closed_line(outline.to_vec(), stroke));
            line(p(1.6, 7.5), p(1.6, 1.6));
            line(p(1.6, 1.6), p(7.5, 1.6));
        }
        LibraryTab::Effects => {
            painter.add(egui::Shape::closed_line(
                vec![
                    p(0.0, -8.0),
                    p(2.6, -2.6),
                    p(8.0, 0.0),
                    p(2.6, 2.6),
                    p(0.0, 8.0),
                    p(-2.6, 2.6),
                    p(-8.0, 0.0),
                    p(-2.6, -2.6),
                ],
                stroke,
            ));
        }
        LibraryTab::Transitions => {
            painter.add(egui::Shape::closed_line(
                vec![p(-8.0, -6.0), p(0.0, 0.0), p(-8.0, 6.0)],
                stroke,
            ));
            painter.add(egui::Shape::closed_line(
                vec![p(8.0, -6.0), p(0.0, 0.0), p(8.0, 6.0)],
                stroke,
            ));
        }
        LibraryTab::Filters => {
            for (x, y) in [(-3.4, 2.0), (3.4, 2.0), (0.0, -3.5)] {
                painter.circle_stroke(p(x, y), 4.3, stroke);
            }
        }
        LibraryTab::Templates => {
            painter.rect_stroke(
                egui::Rect::from_center_size(at, vec2(16.0, 13.0)),
                2,
                stroke,
                StrokeKind::Inside,
            );
            line(p(-8.0, -1.0), p(8.0, -1.0));
            line(p(1.5, -1.0), p(1.5, 6.0));
        }
    }
}

fn media_dialog() -> FileDialog {
    FileDialog::new()
        .add_filter("Video", VIDEO_FILES)
        .add_filter("Audio", AUDIO_FILES)
        .add_filter("Images", IMAGE_FILES)
        .add_filter("Animated GIF", &["gif"])
        .add_filter("All files", &["*"])
}

fn choose_subject_model() -> Option<PathBuf> {
    FileDialog::new()
        .set_title("Choose a local U²-Net ONNX model")
        .add_filter("ONNX model", &["onnx"])
        .pick_file()
}

fn subject_python() -> PathBuf {
    std::env::var_os("CUTAD_CUTOUT_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }))
}

fn with_extension(path: PathBuf, extension: &str) -> PathBuf {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
    {
        path
    } else {
        let mut filename = path.file_name().unwrap_or_default().to_owned();
        filename.push(format!(".{extension}"));
        path.with_file_name(filename)
    }
}

fn filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
