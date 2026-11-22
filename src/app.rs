use crate::branding;
use crate::export_dialog::{Decision, ExportDialog};
use crate::font_browser::{Choice as FontChoice, FontBrowser};
use crate::gallery::Gallery;
use crate::preview::Preview;
use crate::theme::{self, ACCENT, ACCENT_DARK, BACK, LIME, LINE, MUTED, TEXT};
use crate::thumbnails::Thumbnails;
use crate::timeline::{ClipAction, MediaDrag, Placement, Selection, Timeline, View};
use anyhow::{Context, Result};
use cutad::capcut::{self, DraftImport};
use cutad::effects::{Category, Effect, Preset, Transition, TransitionKind};
use cutad::media::{self, Output, PreviewQuality};
use cutad::project::{
    self, Asset, AssetKind, ChromaKey, ClipMask, ColorAdjustment, FPS, MAX_FRAMES, MaskShape,
    Project, TEMPLATES, Title, TitleStyle, Track, TrackKind, Transform, VideoGeometry, timestamp,
};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, StrokeKind, vec2,
};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult};
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
    MatchCanvas(String, VideoGeometry),
    Subtitles(Vec<cutad::subtitles::Cue>),
    CaptionExport(PathBuf, usize),
}

struct Job {
    title: &'static str,
    cancel: Arc<AtomicBool>,
    progress: Option<Arc<AtomicU8>>,
    result: Receiver<Result<Completed>>,
    worker: JoinHandle<()>,
    cancellable: bool,
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
    Motion,
    VisualFade,
    Color,
    Mask,
    Gain,
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
    asset_id: Option<String>,
    edit: Option<(Placement, f32)>,
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
    timeline: Timeline,
    thumbnails: Thumbnails,
    preview: Preview,
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
            asset_id: None,
            edit: None,
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
            timeline: Timeline::default(),
            thumbnails: Thumbnails::default(),
            preview: Preview::default(),
            texture: None,
            job: None,
            status: "Import local files or start from a template.".into(),
            report: None,
        };
        if let Some(path) = path {
            editor.open_path(path);
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

    fn set_document(&mut self, project: Project, file: Option<PathBuf>, imported: bool) {
        self.match_first_video = file.is_none() && !imported;
        self.preview.pause();
        self.preview.invalidate();
        self.texture = None;
        self.frame = 0;
        self.asset_id = None;
        self.history.clear();
        self.future.clear();
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
        let new_clip = self.selection.clip != selection.clip;
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
                self.file = Some(path);
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
        match self
            .commit(|project| project.update_clip(&p.id, p.row, p.start, p.source, p.length, gain))
        {
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
        let result = self.commit(|project| {
            let (row, index) = project.location(&id).context("Select a clip first.")?;
            project.tracks[row].clips.remove(index);
            Ok(())
        });
        if let Err(error) = result {
            self.error("Remove clip", error);
        }
    }

    fn clip_action(&mut self, ctx: &egui::Context, action: ClipAction) {
        let Some(id) = self.selection.clip.clone() else {
            return;
        };
        match action {
            ClipAction::Split => self.split(),
            ClipAction::Remove => self.delete(),
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
                        let frozen = self.project.location(&id).is_some_and(|(row, index)| {
                            self.project.tracks[row].clips[index].freeze
                        });
                        if !frozen {
                            edit.source += offset;
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
            ClipAction::DetachAudio => {
                let result = self.commit(|project| {
                    let (row, index) = project.location(&id).context("Select a clip.")?;
                    let original = &mut project.tracks[row].clips[index];
                    let mut audio = original.clone();
                    audio.id = project::new_id();
                    audio.transition = None;
                    audio.effects.retain(|effect| effect.preset.audio());
                    original.gain = 0.0;
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

    fn export(&mut self, ctx: &egui::Context, path: PathBuf, settings: Output) {
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
        let snapshot = self.project.clone();
        self.start_job(
            ctx,
            "Exporting video",
            JobStyle::Export,
            move |cancel, progress| {
                media::render_with(&snapshot, &path, settings, cancel, |value| {
                    progress.store(value, Ordering::Relaxed)
                })?;
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
                    let source = project.tracks[row].clips[index].source;
                    project.replace_media(&id, asset)?;
                    if keep_source {
                        project.tracks[row].clips[index].source = source;
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
            Completed::CaptionExport(path, count) => {
                self.status = format!("Exported {count} captions to {}.", filename(&path));
            }

            Completed::Subtitles(cues) => {
                let result = self.commit(|project| cutad::subtitles::add_tracks(project, cues));
                self.select_inserted(result, "Imported captions");
                if let Some((placement, _)) = &self.edit {
                    self.seek(placement.start);
                }
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
                if ui.button("Save").clicked() {
                    ui.close();
                    self.save(false);
                }
                if ui.button("Save as…").clicked() {
                    ui.close();
                    self.save(true);
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
                if ui
                    .add_enabled(
                        cutad::transcription::has_audible_clip(&self.project),
                        egui::Button::new("Export audio…"),
                    )
                    .clicked()
                {
                    ui.close();
                    self.export_audio(ui.ctx());
                }
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
                    self.export_dialog = Some(ExportDialog::new(&self.project));
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

    fn export_captions(&mut self, ctx: &egui::Context) {
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
            let count = cutad::subtitles::write(&project, &path)?;
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

    fn export_audio(&mut self, ctx: &egui::Context) {
        let Some(path) =
            self.choose_export_path("Export audio", "CutAd audio.wav", "WAV audio", "wav")
        else {
            return;
        };
        self.stop_player();
        let project = self.project.clone();
        self.start_job(
            ctx,
            "Exporting audio",
            JobStyle::Export,
            move |cancel, progress| {
                media::render_audio_master(&project, &path, cancel, |value| {
                    progress.store(value, Ordering::Relaxed)
                })?;
                Ok(Completed::Export(path))
            },
        );
    }

    fn auto_captions(&mut self, ctx: &egui::Context) {
        if !cutad::transcription::has_audible_clip(&self.project) {
            self.status = "Add audible media before generating captions.".into();
            return;
        }
        if let Err(error) = cutad::transcription::executable() {
            self.error("Auto captions", error);
            return;
        }
        let Some(model) = FileDialog::new()
            .set_title("Select a local whisper.cpp model")
            .add_filter("Whisper model", &["bin"])
            .pick_file()
        else {
            return;
        };
        self.stop_player();
        let project = self.project.clone();
        self.start_job(
            ctx,
            "Generating captions",
            JobStyle::Cancellable,
            move |cancel, _| {
                let cues = cutad::transcription::generate(&project, &model, cancel)?;
                Ok(Completed::Subtitles(cues))
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
            if ui.button("Export captions…").clicked() {
                self.export_captions(ui.ctx());
            }
            if ui
                .button("Auto captions…")
                .on_hover_text("Generate offline with whisper.cpp and a local model")
                .clicked()
            {
                self.auto_captions(ui.ctx());
            }
            if ui.button("Browse fonts…").clicked() {
                self.open_fonts(ui.ctx());
            }
        });
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
        ui.label(
            RichText::new(
                "Add your own image at the playhead. PNG and WebP preserve transparency.",
            )
            .color(MUTED),
        );
        if ui
            .button(RichText::new("Import sticker…").strong())
            .clicked()
            && let Some(path) = FileDialog::new()
                .add_filter("Still images", IMAGE_FILES)
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
        for (name, detail) in [
            (TEMPLATES[0], "9:16  •  3 media slots"),
            (TEMPLATES[1], "16:9  •  3 media slots"),
        ] {
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
                                    .min(170.0)
                                    .max(80.0);
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
        if let Some(frame) = jumped {
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
        ui.heading("Color correction");
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
        if (grade, chroma) != (clip.adjustment, clip.chroma_key) {
            self.update_selected_clip(LiveControl::Color, |clip| {
                clip.adjustment = grade;
                clip.chroma_key = chroma;
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
        let before = clip.mask;
        let mut mask = before;
        let mut enabled = mask.is_some();
        if ui.checkbox(&mut enabled, "Enable mask").changed() {
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
        let mut gain = clip.gain;
        let original_fades = [clip.audio_fade.in_frames, clip.audio_fade.out_frames];
        let mut fades = original_fades;
        ui.add(
            egui::Slider::new(&mut gain, 0.0..=4.0)
                .text("Clip gain")
                .suffix("×"),
        );
        Self::fade_controls(ui, "audio_fades", clip.length, &mut fades);
        if gain != clip.gain || fades != original_fades {
            self.update_selected_clip(LiveControl::Gain, |clip| {
                clip.gain = gain;
                clip.set_audio_fade(fades[0], fades[1]);
            });
        }
        if ui.button("Browse audio effects").clicked() {
            self.library = LibraryTab::Audio;
            self.show_library = true;
            self.audio_presets = true;
            self.inspector_tab = InspectorTab::Effects;
        }
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
        if let Some((edit, gain)) = &mut self.edit {
            let old_transform = self.transform;
            let clip = &self.project.tracks[edit.row]
                .clips
                .iter()
                .find(|clip| clip.id == edit.id)
                .unwrap();
            let has_source = clip.source > 0;
            let animated_position = !clip.position_animation.is_empty();
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
                            ("Scale", &mut transform.scale, 0.1, 3.0),
                            ("Position X", &mut transform.x, -1.5, 1.5),
                            ("Position Y", &mut transform.y, -1.5, 1.5),
                            ("Opacity", &mut transform.opacity, 0.0, 1.0),
                        ] {
                            if animated_position && label.starts_with("Position") {
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
                        ui.label("Rotation");
                        ui.add(
                            egui::DragValue::new(&mut transform.rotation)
                                .range(-180.0..=180.0)
                                .suffix("°"),
                        );
                        ui.end_row();
                    });
                if ui.button("Animations and keyframes…").clicked() {
                    self.inspector_tab = InspectorTab::Motion;
                }
                if ui.button("Reset transform").clicked() {
                    self.transform = Transform::default();
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
            ui.menu_button("Tracks", |ui| {
                if ui.button("Add video track").clicked() {
                    ui.close();
                    self.add_track(TrackKind::Video);
                }
                if ui.button("Add audio track").clicked() {
                    ui.close();
                    self.add_track(TrackKind::Audio);
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::Slider::new(&mut self.zoom, 12.0..=220.0)
                        .show_value(false)
                        .text("Zoom"),
                );
                ui.checkbox(&mut self.snap, "Snap");
            });
        });
        let events = self.timeline.show(
            ui,
            View {
                project: &self.project,
                selection: &self.selection,
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
        if let Some(frame) = events.seek {
            self.seek(frame);
        }
        if let Some(placement) = events.edit {
            if let Some((row, index)) = self.project.location(&placement.id) {
                let gain = self.project.tracks[row].clips[index].gain;
                self.apply_placement(placement, gain);
            }
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
        self.thumbnails.collect(ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            if let Some(job) = &self.job {
                job.cancel.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if !self.may_discard() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.job.is_none()
            && self.report.is_none()
            && self.export_dialog.is_none()
            && self.font_browser.is_none()
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
            if let Decision::Render(path, settings) = decision {
                self.export(&ctx, path, settings);
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
        .add_filter("All files", &["*"])
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
