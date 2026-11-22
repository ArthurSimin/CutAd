use crate::effects::{Effect, Transition};
use crate::fonts::FontRef;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub const FPS: u32 = 30;
pub const MAX_FRAMES: u32 = 24 * 60 * 60 * FPS;
pub const TEMPLATES: [&str; 2] = ["Portrait montage", "Widescreen opener"];

pub fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Video,
    Audio,
    Image,
    Text,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TitleStyle {
    Headline,
    LowerThird,
    Caption,
    Impact,
    Neon,
    Glitch,
    Pop,
}

impl TitleStyle {
    pub const ALL: [Self; 7] = [
        Self::Impact,
        Self::Neon,
        Self::Glitch,
        Self::Pop,
        Self::Headline,
        Self::LowerThird,
        Self::Caption,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Headline => "Headline",
            Self::LowerThird => "Lower third",
            Self::Caption => "Caption",
            Self::Impact => "Impact",
            Self::Neon => "Neon",
            Self::Glitch => "Glitch",
            Self::Pop => "Comic pop",
        }
    }

    pub fn sample(self) -> &'static str {
        match self {
            Self::Impact => "CAUGHT IN 4K",
            Self::Neon => "AFTER HOURS",
            Self::Glitch => "NO SIGNAL",
            Self::Pop => "PLOT TWIST!",
            Self::Headline | Self::LowerThird | Self::Caption => self.label(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Title {
    pub text: String,
    pub style: TitleStyle,
    #[serde(default)]
    pub appearance: TitleAppearance,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TitleAppearance {
    pub font: Option<FontRef>,
    pub size_percent: u16,
    pub fill: Option<[u8; 3]>,
    pub align: Option<TextAlign>,
    pub outline: Option<TextOutline>,
    pub background: Option<TextBackground>,
    pub line_spacing: i8,
}

impl Default for TitleAppearance {
    fn default() -> Self {
        Self {
            font: None,
            size_percent: 100,
            fill: None,
            align: None,
            outline: None,
            background: None,
            line_spacing: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TextOutline {
    pub width: u8,
    pub color: [u8; 3],
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TextBackground {
    pub opacity: u8,
    pub color: [u8; 3],
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct VideoGeometry {
    pub width: u32,
    pub height: u32,
    pub fps: u16,
}

impl VideoGeometry {
    pub fn canvas(self) -> (u32, u32) {
        let edge = u64::from(self.width.max(self.height).max(1));
        let scaled_edge = edge.min(4096);
        let fit = |n: u32| (((u64::from(n) * scaled_edge / edge) as u32) & !1).max(16);
        (fit(self.width), fit(self.height))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Asset {
    pub id: String,
    pub path: Option<PathBuf>,
    pub kind: AssetKind,
    pub frames: u32,
    pub audio: bool,
    pub label: String,
    #[serde(default)]
    pub audio_offset_samples: i64,
    #[serde(default)]
    pub preview_seek: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<VideoGeometry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Title>,
}

impl Asset {
    pub fn video(path: impl AsRef<Path>, frames: u32, audio: bool) -> Self {
        Self::local(path, AssetKind::Video, frames, audio)
    }

    pub fn local(path: impl AsRef<Path>, kind: AssetKind, frames: u32, audio: bool) -> Self {
        let path = path.as_ref();
        Self {
            id: new_id(),
            path: Some(path.to_owned()),
            kind,
            frames,
            audio,
            label: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            audio_offset_samples: 0,
            preview_seek: false,
            geometry: None,
            title: None,
        }
    }

    pub fn placeholder(label: impl Into<String>, kind: AssetKind, frames: u32) -> Self {
        Self {
            id: new_id(),
            path: None,
            kind,
            frames,
            audio: kind == AssetKind::Audio,
            label: label.into(),
            audio_offset_samples: 0,
            preview_seek: false,
            geometry: None,
            title: None,
        }
    }

    pub fn text(style: TitleStyle, text: impl Into<String>) -> Self {
        let mut asset = Self::placeholder(style.label(), AssetKind::Text, 1);
        asset.title = Some(Title {
            text: text.into(),
            style,
            appearance: TitleAppearance::default(),
        });
        asset
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Transform {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub opacity: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ColorAdjustment {
    pub brightness: i8,
    pub contrast: i8,
    pub saturation: i8,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChromaKey {
    pub color: [u8; 3],
    pub tolerance: u8,
    pub softness: u8,
}

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            color: [0, 255, 0],
            tolerance: 18,
            softness: 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MaskShape {
    Rectangle,
    Ellipse,
    Linear,
}

impl MaskShape {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Linear => "Linear",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ClipMask {
    pub shape: MaskShape,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub rotation: f32,
    pub feather: f32,
    pub invert: bool,
}

impl Default for ClipMask {
    fn default() -> Self {
        Self {
            shape: MaskShape::Rectangle,
            x: 0.5,
            y: 0.5,
            width: 0.6,
            height: 0.6,
            rotation: 0.0,
            feather: 0.0,
            invert: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ClipFade {
    pub in_frames: u32,
    pub out_frames: u32,
    // Signed frame offsets from the respective clip edges retain partial fades after a cut.
    #[serde(skip_serializing_if = "is_zero")]
    in_offset: i32,
    #[serde(skip_serializing_if = "is_zero")]
    out_offset: i32,
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

pub type AudioFade = ClipFade;

impl ClipFade {
    pub(crate) fn is_empty(&self) -> bool {
        self.in_frames == 0 && self.out_frames == 0
    }

    pub(crate) fn anchors(self, length: u32) -> (i64, i64) {
        (
            i64::from(self.in_offset),
            i64::from(length) + i64::from(self.out_offset),
        )
    }

    fn set(&mut self, in_frames: u32, out_frames: u32, length: u32) {
        if self.in_frames != in_frames {
            self.in_frames = in_frames.min(length);
            self.in_offset = 0;
        }
        if self.out_frames != out_frames {
            self.out_frames = out_frames.min(length);
            self.out_offset = 0;
        }
    }

    fn split(&mut self, offset: u32, tail_length: u32) -> Self {
        let mut tail = *self;
        if tail.in_frames > 0 {
            tail.in_offset -= offset as i32;
            tail.discard_inactive();
        }
        if self.out_frames > 0 {
            self.out_offset += tail_length as i32;
            self.discard_inactive();
        }
        tail
    }

    fn trim(&mut self, leading: i32, trailing: i32) {
        if self.in_offset != 0 {
            self.in_offset -= leading;
        }
        if self.out_offset != 0 {
            self.out_offset += trailing;
        }
        self.discard_inactive();
    }

    fn valid(self) -> bool {
        self.in_frames <= MAX_FRAMES
            && self.out_frames <= MAX_FRAMES
            && self.in_offset.unsigned_abs() <= 2 * MAX_FRAMES
            && self.out_offset.unsigned_abs() <= 2 * MAX_FRAMES
    }

    fn discard_inactive(&mut self) {
        if i64::from(self.in_offset) + i64::from(self.in_frames) <= 0 {
            self.in_frames = 0;
            self.in_offset = 0;
        }
        if i64::from(self.out_offset) - i64::from(self.out_frames) >= 0 {
            self.out_frames = 0;
            self.out_offset = 0;
        }
    }
}

pub const MAX_POSITION_KEYS: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct PositionKeyframe {
    pub frame: i32,
    pub position: [f32; 2],
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PositionAnimation {
    pub offset: i32,
    pub keys: Vec<PositionKeyframe>,
}

impl PositionAnimation {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn times(&self, clip_length: u32) -> impl Iterator<Item = u32> + '_ {
        self.keys.iter().filter_map(move |key| {
            let relative = u32::try_from(i64::from(key.frame) - i64::from(self.offset)).ok()?;
            (relative < clip_length).then_some(relative)
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Clip {
    pub id: String,
    pub asset_id: String,
    pub start: u32,
    pub source: u32,
    pub length: u32,
    pub gain: f32,
    pub slot: bool,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default, skip_serializing_if = "PositionAnimation::is_empty")]
    pub position_animation: PositionAnimation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_key: Option<ChromaKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<ClipMask>,
    #[serde(default)]
    pub adjustment: ColorAdjustment,
    #[serde(default, skip_serializing_if = "AudioFade::is_empty")]
    pub audio_fade: AudioFade,
    #[serde(default, skip_serializing_if = "ClipFade::is_empty")]
    pub visual_fade: ClipFade,
    #[serde(default)]
    pub freeze: bool,
}

impl Clip {
    pub fn new(asset_id: String, start: u32, source: u32, length: u32) -> Self {
        Self {
            id: new_id(),
            asset_id,
            start,
            source,
            length,
            gain: 1.0,
            slot: false,
            effects: Vec::new(),
            transition: None,
            transform: Transform::default(),
            position_animation: PositionAnimation::default(),
            chroma_key: None,
            mask: None,
            adjustment: ColorAdjustment::default(),
            audio_fade: AudioFade::default(),
            visual_fade: ClipFade::default(),
            freeze: false,
        }
    }

    pub fn set_audio_fade(&mut self, in_frames: u32, out_frames: u32) {
        self.audio_fade.set(in_frames, out_frames, self.length);
    }

    pub fn set_visual_fade(&mut self, in_frames: u32, out_frames: u32) {
        self.visual_fade.set(in_frames, out_frames, self.length);
    }

    pub fn position_at(&self, relative_frame: u32) -> [f32; 2] {
        let animation = &self.position_animation;
        let frame = animation.offset.saturating_add(relative_frame as i32);
        let (before, after) = animation
            .keys
            .split_at(animation.keys.partition_point(|key| key.frame <= frame));
        match (before.last(), after.first()) {
            (Some(a), Some(b)) => {
                let t = (frame - a.frame) as f32 / (b.frame - a.frame) as f32;
                std::array::from_fn(|i| a.position[i] + (b.position[i] - a.position[i]) * t)
            }
            (Some(key), None) | (None, Some(key)) => key.position,
            (None, None) => [self.transform.x, self.transform.y],
        }
    }

    pub fn set_position_keyframe(&mut self, relative_frame: u32, position: [f32; 2]) -> Result<()> {
        ensure!(
            relative_frame < self.length,
            "Place the playhead inside this clip."
        );
        ensure!(
            position.into_iter().all(|p| (-1.5..=1.5).contains(&p)),
            "Position is out of range."
        );
        let animation = &mut self.position_animation;
        let frame = animation.offset.saturating_add(relative_frame as i32);
        let key = PositionKeyframe { frame, position };
        match animation.keys.binary_search_by_key(&frame, |key| key.frame) {
            Ok(index) => animation.keys[index] = key,
            Err(index) => {
                ensure!(
                    animation.keys.len() < MAX_POSITION_KEYS,
                    "This clip already has 32 position keyframes."
                );
                animation.keys.insert(index, key);
            }
        }
        Ok(())
    }

    pub fn remove_position_keyframe(&mut self, relative_frame: u32) {
        let [x, y] = self.position_at(relative_frame);
        let animation = &mut self.position_animation;
        let frame = animation.offset.saturating_add(relative_frame as i32);
        if let Ok(index) = animation.keys.binary_search_by_key(&frame, |key| key.frame) {
            animation.keys.remove(index);
            if animation.keys.is_empty() {
                animation.offset = 0;
                self.transform.x = x;
                self.transform.y = y;
            }
        }
    }

    pub fn end(&self) -> u32 {
        self.start.saturating_add(self.length)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

impl TrackKind {
    pub fn accepts(self, asset: &Asset) -> bool {
        match self {
            Self::Video => matches!(
                asset.kind,
                AssetKind::Video | AssetKind::Image | AssetKind::Text
            ),
            Self::Audio => asset.audio,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub muted: bool,
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            name: name.into(),
            kind,
            clips: Vec::new(),
            muted: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Project {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub assets: Vec<Asset>,
    pub tracks: Vec<Track>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled edit".into(),
            width: 1280,
            height: 720,
            assets: Vec::new(),
            tracks: vec![
                Track::new("Video 1", TrackKind::Video),
                Track::new("Audio 1", TrackKind::Audio),
            ],
        }
    }
}

impl Project {
    pub fn duration(&self) -> u32 {
        self.tracks
            .iter()
            .flat_map(|track| &track.clips)
            .map(Clip::end)
            .max()
            .unwrap_or(0)
    }

    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    pub fn source_video(&self) -> Option<VideoGeometry> {
        self.tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .flat_map(|track| &track.clips)
            .filter_map(|clip| {
                self.asset(&clip.asset_id)
                    .filter(|asset| asset.kind == AssetKind::Video)
                    .and_then(|asset| asset.geometry)
                    .map(|geometry| (clip.start, geometry))
            })
            .min_by_key(|(start, _)| *start)
            .map(|(_, geometry)| geometry)
    }

    pub fn location(&self, id: &str) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(row, track)| {
            track
                .clips
                .iter()
                .position(|clip| clip.id == id)
                .map(|column| (row, column))
        })
    }

    pub fn split(&mut self, id: &str, frame: u32) -> Result<String> {
        let (row, column) = self.location(id).context("Select a clip to split.")?;
        let clip = &mut self.tracks[row].clips[column];
        ensure!(
            clip.start < frame && frame < clip.end(),
            "Place the playhead inside the clip."
        );
        let offset = frame - clip.start;
        let mut tail = clip.clone();
        tail.id = new_id();
        tail.start = frame;
        tail.source += if clip.freeze { 0 } else { offset };
        tail.length -= offset;
        if !tail.position_animation.is_empty() {
            tail.position_animation.offset =
                tail.position_animation.offset.saturating_add(offset as i32);
        }
        tail.audio_fade = clip.audio_fade.split(offset, tail.length);
        tail.visual_fade = clip.visual_fade.split(offset, tail.length);
        tail.transition = None;
        clip.length = offset;
        let id = tail.id.clone();
        self.tracks[row].clips.insert(column + 1, tail);
        Ok(id)
    }

    pub fn replace_media(&mut self, id: &str, asset: Asset) -> Result<()> {
        let (row, column) = self
            .location(id)
            .context("Select a clip or template slot first.")?;
        let track = &mut self.tracks[row];
        ensure!(
            track.kind.accepts(&asset),
            "Choose media that matches this track."
        );
        let clip = &mut track.clips[column];
        ensure!(
            matches!(asset.kind, AssetKind::Image | AssetKind::Text)
                || clip.freeze && asset.kind == AssetKind::Video
                || asset.frames >= clip.length,
            "The replacement is shorter than the selected clip."
        );
        clip.asset_id.clone_from(&asset.id);
        clip.source = 0;
        clip.slot = false;
        clip.freeze &= asset.kind == AssetKind::Video;
        if self.asset(&asset.id).is_none() {
            self.assets.push(asset);
        }
        Ok(())
    }

    pub fn insert_asset(
        &mut self,
        mut row: usize,
        asset_id: &str,
        position: u32,
    ) -> Result<String> {
        let asset = self
            .asset(asset_id)
            .context("Select an imported media file.")?;
        let kind = if asset.kind == AssetKind::Audio {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let length = if matches!(asset.kind, AssetKind::Image | AssetKind::Text) {
            FPS * 3
        } else {
            asset.frames
        };
        ensure!(
            asset.path.is_some() || asset.title.is_some(),
            "Replace this template slot with a media file first."
        );
        if !self
            .tracks
            .get(row)
            .is_some_and(|track| track.kind.accepts(asset))
        {
            row = self
                .tracks
                .iter()
                .position(|track| track.kind == kind)
                .unwrap_or(self.tracks.len());
        }
        if row == self.tracks.len() {
            self.tracks.push(Track::new(kind.label(), kind));
        }
        let track = &mut self.tracks[row];
        let fits = track
            .clips
            .iter()
            .all(|clip| position >= clip.end() || position.saturating_add(length) <= clip.start);
        let start = if fits {
            position
        } else {
            track.clips.iter().map(Clip::end).max().unwrap_or(0)
        };
        let clip = Clip::new(asset_id.into(), start, 0, length);
        let id = clip.id.clone();
        track.clips.push(clip);
        Ok(id)
    }

    pub fn add_title(
        &mut self,
        style: TitleStyle,
        text: impl Into<String>,
        position: u32,
    ) -> Result<String> {
        let row = self
            .tracks
            .iter()
            .position(|track| track.name == "Titles" && track.kind == TrackKind::Video)
            .unwrap_or_else(|| {
                self.tracks.push(Track::new("Titles", TrackKind::Video));
                self.tracks.len() - 1
            });
        let asset = Asset::text(style, text);
        let id = asset.id.clone();
        self.assets.push(asset);
        self.insert_asset(row, &id, position)
    }

    pub fn update_title(&mut self, id: &str, title: Title) -> Result<()> {
        let (row, column) = self.location(id).context("Select a text clip.")?;
        let asset_id = &self.tracks[row].clips[column].asset_id;
        let index = self
            .assets
            .iter()
            .position(|asset| &asset.id == asset_id && asset.title.is_some())
            .context("Title media is missing.")?;
        if self.assets[index].title.as_ref() == Some(&title) {
            return Ok(());
        }
        let shared = self
            .tracks
            .iter()
            .flat_map(|track| &track.clips)
            .any(|clip| clip.asset_id == *asset_id && clip.id != id);
        if shared {
            let mut asset = self.assets[index].clone();
            asset.id = new_id();
            asset.label = title.style.label().into();
            asset.title = Some(title);
            self.tracks[row].clips[column]
                .asset_id
                .clone_from(&asset.id);
            self.assets.push(asset);
        } else {
            self.assets[index].label = title.style.label().into();
            self.assets[index].title = Some(title);
        }
        Ok(())
    }

    pub fn add_sticker(&mut self, asset: Asset, position: u32) -> Result<String> {
        ensure!(
            asset.kind == AssetKind::Image && asset.path.is_some(),
            "Choose a local still image."
        );
        let length = 2 * FPS;
        ensure!(
            position.saturating_add(length) <= MAX_FRAMES,
            "The sticker exceeds the timeline limit."
        );
        let mut clip = Clip::new(asset.id.clone(), position, 0, length);
        clip.transform.scale = 0.48;
        let id = clip.id.clone();
        let row = self
            .tracks
            .iter()
            .rposition(|track| track.kind == TrackKind::Video)
            .filter(|&row| {
                let track = &self.tracks[row];
                track.name == "Stickers"
                    && !track.muted
                    && track
                        .clips
                        .iter()
                        .all(|other| clip.start >= other.end() || clip.end() <= other.start)
            })
            .unwrap_or_else(|| {
                self.tracks.push(Track::new("Stickers", TrackKind::Video));
                self.tracks.len() - 1
            });
        self.assets.push(asset);
        self.tracks[row].clips.push(clip);
        Ok(id)
    }

    pub fn update_clip(
        &mut self,
        id: &str,
        row: usize,
        start: u32,
        source: u32,
        length: u32,
        gain: f32,
    ) -> Result<()> {
        ensure!(row < self.tracks.len(), "Choose a track.");
        let (previous, index) = self.location(id).context("Select a clip.")?;
        let mut clip = self.tracks[previous].clips.remove(index);
        let end = start.saturating_add(length);
        let (leading_trim, trailing_trim) = if clip.end() == end {
            let delta = start.min(MAX_FRAMES) as i32 - clip.start as i32;
            if !clip.position_animation.is_empty() {
                clip.position_animation.offset =
                    clip.position_animation.offset.saturating_add(delta);
            }
            (delta, 0)
        } else if clip.start == start {
            (0, clip.end() as i32 - end.min(MAX_FRAMES) as i32)
        } else {
            (0, 0)
        };
        clip.audio_fade.trim(leading_trim, trailing_trim);
        clip.visual_fade.trim(leading_trim, trailing_trim);
        clip.start = start;
        clip.source = source;
        clip.length = length;
        clip.gain = gain;
        self.tracks[row].clips.push(clip);
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_canvas(self.width, self.height),
            "Use even canvas dimensions between 16 and 4096."
        );
        ensure!(
            !self.tracks.is_empty(),
            "The project needs at least one track."
        );
        let mut assets = HashMap::new();
        for asset in &self.assets {
            ensure!(
                !asset.id.is_empty()
                    && assets.insert(asset.id.as_str(), asset).is_none()
                    && (1..=MAX_FRAMES).contains(&asset.frames)
                    && asset.audio_offset_samples.unsigned_abs() <= u64::from(MAX_FRAMES) * 1600,
                "Invalid or duplicate media asset."
            );
            ensure!(
                matches!(asset.kind, AssetKind::Text) == asset.title.is_some()
                    && asset
                        .title
                        .as_ref()
                        .is_none_or(|title| title.text.len() <= 4096
                            && !title.text.contains('\0')
                            && (25..=300).contains(&title.appearance.size_percent)
                            && (-50..=100).contains(&title.appearance.line_spacing)
                            && title
                                .appearance
                                .outline
                                .is_none_or(|outline| outline.width <= 30)
                            && title
                                .appearance
                                .background
                                .is_none_or(|box_| box_.opacity <= 100)
                            && title.appearance.font.as_ref().is_none_or(|font| !font
                                .family
                                .is_empty()
                                && !font.postscript.is_empty()
                                && font.family.len() <= 250
                                && font.postscript.len() <= 250)),
                "Invalid title content."
            );
        }
        let mut seen = HashSet::new();
        for track in &self.tracks {
            let mut clips: Vec<_> = track.clips.iter().collect();
            clips.sort_unstable_by_key(|clip| clip.start);
            let mut previous_end = 0;
            for clip in clips {
                let asset = assets
                    .get(clip.asset_id.as_str())
                    .context("A clip references unknown media.")?;
                ensure!(
                    !clip.id.is_empty()
                        && seen.insert(&clip.id)
                        && clip.length > 0
                        && clip.end() <= MAX_FRAMES
                        && clip.source <= MAX_FRAMES
                        && (0.0..=4.0).contains(&clip.gain),
                    "Invalid clip settings."
                );
                let transform = clip.transform;
                ensure!(
                    (0.1..=3.0).contains(&transform.scale)
                        && (-1.5..=1.5).contains(&transform.x)
                        && (-1.5..=1.5).contains(&transform.y)
                        && (-180.0..=180.0).contains(&transform.rotation)
                        && (0.0..=1.0).contains(&transform.opacity),
                    "Invalid clip transform."
                );
                let animation = &clip.position_animation;
                ensure!(
                    animation.offset.unsigned_abs() <= MAX_FRAMES * 2
                        && animation.keys.len() <= MAX_POSITION_KEYS
                        && animation
                            .keys
                            .windows(2)
                            .all(|pair| pair[0].frame < pair[1].frame)
                        && animation
                            .keys
                            .iter()
                            .all(|key| key.frame.unsigned_abs() <= MAX_FRAMES * 3
                                && key.position.into_iter().all(|p| (-1.5..=1.5).contains(&p))),
                    "Invalid position keyframes."
                );
                let correction = clip.adjustment;
                ensure!(
                    [
                        correction.brightness,
                        correction.contrast,
                        correction.saturation
                    ]
                    .into_iter()
                    .all(|value| (-100..=100).contains(&value))
                        && clip
                            .chroma_key
                            .is_none_or(|key| key.tolerance <= 100 && key.softness <= 100),
                    "Invalid color or chroma key settings."
                );
                ensure!(
                    clip.mask.is_none_or(|mask| (0.0..=1.0).contains(&mask.x)
                        && (0.0..=1.0).contains(&mask.y)
                        && (0.01..=1.0).contains(&mask.width)
                        && (0.01..=1.0).contains(&mask.height)
                        && (-180.0..=180.0).contains(&mask.rotation)
                        && (0.0..=1.0).contains(&mask.feather)),
                    "Invalid clip mask."
                );
                ensure!(
                    clip.audio_fade.valid() && clip.visual_fade.valid(),
                    "Invalid clip fade settings."
                );
                ensure!(
                    track.kind.accepts(asset),
                    "Media type does not match this track."
                );
                ensure!(
                    clip.effects.len() <= 24
                        && clip.effects.iter().all(|effect| effect.amount <= 100),
                    "A clip can have up to 24 effects with strengths between 0 and 100."
                );
                ensure!(
                    clip.transition
                        .is_none_or(|t| (2..=2 * FPS).contains(&t.frames)),
                    "Transitions need a duration between 2 frames and 2 seconds."
                );
                ensure!(
                    if clip.freeze && asset.kind == AssetKind::Video {
                        clip.source < asset.frames && track.kind == TrackKind::Video
                    } else {
                        matches!(asset.kind, AssetKind::Image | AssetKind::Text)
                            || clip.source + clip.length <= asset.frames
                    },
                    "A clip extends past its source media."
                );
                ensure!(
                    clip.start >= previous_end,
                    "Clips cannot overlap on the same track."
                );
                previous_end = clip.end();
            }
        }
        Ok(())
    }
}

pub fn valid_canvas(width: u32, height: u32) -> bool {
    [width, height]
        .into_iter()
        .all(|n| (16..=4096).contains(&n) && n.is_multiple_of(2))
}

pub fn timestamp(frame: u32) -> String {
    let sec = frame / FPS;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        sec / 3600,
        sec % 3600 / 60,
        sec % 60,
        frame % FPS
    )
}

pub fn seconds(frame: u32) -> String {
    format!("{:.6}", f64::from(frame) / f64::from(FPS))
}

#[derive(Deserialize, Serialize)]
struct ProjectFile {
    format: String,
    version: u32,
    fps: u32,
    #[serde(flatten)]
    project: Project,
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn save(project: &Project, path: &Path) -> Result<()> {
    project.validate()?;
    let path = std::path::absolute(path)?;
    let parent = path.parent().context("Choose a project file.")?;
    let mut copy = project.clone();
    for asset in &mut copy.assets {
        if let Some(media) = &mut asset.path {
            let absolute = std::path::absolute(&*media)?;
            ensure!(
                !same_path(&absolute, &path),
                "Choose a project file separate from source media."
            );
            *media = absolute
                .strip_prefix(parent)
                .unwrap_or(&absolute)
                .to_owned();
            #[cfg(windows)]
            {
                *media = PathBuf::from(
                    media
                        .to_str()
                        .context("Invalid media path.")?
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(
        temp.as_file_mut(),
        &ProjectFile {
            format: "cutad".into(),
            version: 1,
            fps: FPS,
            project: copy,
        },
    )?;
    writeln!(temp)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .context("Could not replace the project file.")?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Project> {
    let path = std::path::absolute(path)?;
    let parent = path.parent().context("Choose a project file.")?;
    let mut data: ProjectFile =
        serde_json::from_reader(BufReader::new(std::fs::File::open(&path)?))
            .context("This file is not a readable CutAd project.")?;
    ensure!(
        data.format == "cutad" && data.version == 1 && data.fps == FPS,
        "Unsupported CutAd project format or version."
    );
    for asset in &mut data.project.assets {
        if let Some(path) = &mut asset.path {
            *path = parent.join(&*path);
        }
    }
    let empty: HashSet<_> = data
        .project
        .assets
        .iter()
        .filter(|asset| asset.path.is_none() && asset.title.is_none())
        .map(|asset| asset.id.as_str())
        .collect();
    for clip in data
        .project
        .tracks
        .iter_mut()
        .flat_map(|track| &mut track.clips)
    {
        if empty.contains(clip.asset_id.as_str()) {
            clip.slot = true;
        }
    }
    data.project.validate()?;
    Ok(data.project)
}

pub fn template(name: &str) -> Result<Project> {
    let (width, height, slots) = match name {
        "Portrait montage" => (
            720,
            1280,
            [("Opening", 60), ("Detail", 60), ("Final shot", 60)],
        ),
        "Widescreen opener" => (
            1280,
            720,
            [("Wide shot", 90), ("Close-up", 60), ("End card", 90)],
        ),
        _ => bail!("Unknown CutAd template."),
    };
    let mut project = Project {
        name: name.into(),
        width,
        height,
        ..Project::default()
    };
    for (name, length) in slots {
        let asset = Asset::placeholder(name, AssetKind::Image, length);
        let mut clip = Clip::new(asset.id.clone(), project.duration(), 0, length);
        clip.slot = true;
        project.assets.push(asset);
        project.tracks[0].clips.push(clip);
    }
    Ok(project)
}

pub fn as_template(project: &Project) -> Project {
    let mut template = project.clone();
    template.assets.clear();
    for track in &mut template.tracks {
        for clip in &mut track.clips {
            if let Some(asset) = project
                .asset(&clip.asset_id)
                .filter(|asset| asset.title.is_some())
            {
                if !template.assets.iter().any(|item| item.id == asset.id) {
                    template.assets.push(asset.clone());
                }
                continue;
            }
            let name = project
                .asset(&clip.asset_id)
                .map_or("Media slot", |asset| &asset.label);
            let kind = if track.kind == TrackKind::Video {
                AssetKind::Image
            } else {
                AssetKind::Audio
            };
            let asset = Asset::placeholder(name, kind, clip.length);
            clip.asset_id.clone_from(&asset.id);
            clip.slot = true;
            clip.source = 0;
            template.assets.push(asset);
        }
    }
    template
}
