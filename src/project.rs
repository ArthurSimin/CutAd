use crate::effects::{Effect, Transition, TransitionKind};
use crate::fonts::FontRef;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub const FPS: u32 = 30;
pub const NORMAL_SPEED: u16 = 100;
pub const MAX_FRAMES: u32 = 24 * 60 * 60 * FPS;
pub const TEMPLATES: [&str; 4] = [
    "Portrait montage",
    "Widescreen opener",
    "Side-by-side review",
    "Vertical reveal",
];

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
pub struct PreviewProxy {
    pub path: PathBuf,
    pub(crate) source_bytes: u64,
    pub(crate) source_modified: std::time::SystemTime,
    pub(crate) proxy_bytes: u64,
    pub(crate) proxy_modified: std::time::SystemTime,
}

impl PreviewProxy {
    pub fn matches_source(&self, source: &Path) -> bool {
        let matches = |path: &Path, size, modified| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata.len() == size
                    && metadata.modified().ok() == Some(modified)
            })
        };
        matches(source, self.source_bytes, self.source_modified)
            && matches(&self.path, self.proxy_bytes, self.proxy_modified)
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
    pub proxy: Option<PreviewProxy>,
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
            proxy: None,
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
            proxy: None,
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
#[serde(default)]
pub struct Framing {
    pub left: u8,
    pub right: u8,
    pub top: u8,
    pub bottom: u8,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Darken,
    Lighten,
    Difference,
    Add,
}

impl BlendMode {
    pub const ALL: [Self; 7] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Darken,
        Self::Lighten,
        Self::Difference,
        Self::Add,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::Difference => "Difference",
            Self::Add => "Add",
        }
    }

    pub(crate) fn filter(self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::Multiply => Some("multiply"),
            Self::Screen => Some("screen"),
            Self::Darken => Some("darken"),
            Self::Lighten => Some("lighten"),
            Self::Difference => Some("difference"),
            Self::Add => Some("addition"),
        }
    }

    fn is_normal(&self) -> bool {
        *self == Self::Normal
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioChannels {
    #[default]
    Stereo,
    Mono,
    Left,
    Right,
}

impl AudioChannels {
    pub const ALL: [Self; 4] = [Self::Stereo, Self::Mono, Self::Left, Self::Right];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::Mono => "Mix to mono",
            Self::Left => "Use left channel",
            Self::Right => "Use right channel",
        }
    }

    fn is_stereo(&self) -> bool {
        *self == Self::Stereo
    }
}

impl Framing {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn valid(self) -> bool {
        u16::from(self.left) + u16::from(self.right) <= 95
            && u16::from(self.top) + u16::from(self.bottom) <= 95
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ColorAdjustment {
    pub brightness: i8,
    pub contrast: i8,
    pub saturation: i8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HueAdjustment {
    pub hue: i16,
    pub saturation: i8,
    pub intensity: i8,
}

pub const HUE_RANGES: [(&str, &str); 6] = [
    ("Red", "r"),
    ("Yellow", "y"),
    ("Green", "g"),
    ("Cyan", "c"),
    ("Blue", "b"),
    ("Magenta", "m"),
];

fn no_hue_adjustments(ranges: &[HueAdjustment; 6]) -> bool {
    *ranges == [HueAdjustment::default(); 6]
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MaskStroke {
    /// Radius as a fraction of the shorter canvas edge.
    pub radius: f32,
    pub restore: bool,
    /// Coordinates in the clip's canvas before scale, rotation, and translation.
    pub points: Vec<[f32; 2]>,
}

impl MaskStroke {
    pub const MAX_STROKES: usize = 128;
    pub const MAX_POINTS: usize = 512;

    fn valid(&self) -> bool {
        (0.005..=0.25).contains(&self.radius)
            && (1..=Self::MAX_POINTS).contains(&self.points.len())
            && self
                .points
                .iter()
                .flatten()
                .all(|v| (0.0..=1.0).contains(v))
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

fn normal_speed() -> u16 {
    NORMAL_SPEED
}

fn is_normal_speed(value: &u16) -> bool {
    *value == NORMAL_SPEED
}

fn zero_subframe(value: &u8) -> bool {
    *value == 0
}

fn zero_duration_remainder(value: &u16) -> bool {
    *value == 0
}

fn is_zero_i8(value: &i8) -> bool {
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
pub const MAX_CLIP_SCALE: f32 = 4.0;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct PositionKeyframe {
    pub frame: i32,
    pub position: [f32; 2],
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Hold,
}

impl Easing {
    pub const ALL: [Self; 5] = [
        Self::Linear,
        Self::EaseIn,
        Self::EaseOut,
        Self::EaseInOut,
        Self::Hold,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::EaseIn => "Ease in",
            Self::EaseOut => "Ease out",
            Self::EaseInOut => "Ease in out",
            Self::Hold => "Hold",
        }
    }

    fn apply(self, t: f32) -> f32 {
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t).powi(2),
            Self::EaseInOut => t * t * (3.0 - 2.0 * t),
            Self::Hold => 0.0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PositionAnimation {
    pub offset: i32,
    pub keys: Vec<PositionKeyframe>,
    #[serde(default)]
    pub easing: Easing,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct ScalarKeyframe {
    pub frame: i32,
    #[serde(alias = "opacity")]
    pub value: f32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ScalarAnimation {
    pub offset: i32,
    pub keys: Vec<ScalarKeyframe>,
    #[serde(default)]
    pub easing: Easing,
}

impl ScalarAnimation {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn times(&self, clip_length: u32) -> impl Iterator<Item = u32> + '_ {
        self.keys.iter().filter_map(move |key| {
            let relative = key.frame.checked_sub(self.offset)?;
            let frame = u32::try_from(relative).ok()?;
            (frame < clip_length).then_some(frame)
        })
    }

    fn valid(&self, range: std::ops::RangeInclusive<f32>) -> bool {
        self.offset.unsigned_abs() <= MAX_FRAMES * 2
            && self.keys.len() <= MAX_POSITION_KEYS
            && self
                .keys
                .windows(2)
                .all(|pair| pair[0].frame < pair[1].frame)
            && self
                .keys
                .iter()
                .all(|key| key.frame.unsigned_abs() <= MAX_FRAMES * 3 && range.contains(&key.value))
    }

    fn shift(&mut self, frames: i32) {
        if !self.is_empty() {
            self.offset = self.offset.saturating_add(frames);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnimatedProperty {
    Opacity,
    Rotation,
    Scale,
    AudioGain,
}

impl AnimatedProperty {
    pub fn label(self) -> &'static str {
        match self {
            Self::Opacity => "Opacity",
            Self::Rotation => "Rotation",
            Self::Scale => "Scale",
            Self::AudioGain => "Gain",
        }
    }

    fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            Self::Opacity => 0.0..=1.0,
            Self::Rotation => -180.0..=180.0,
            Self::Scale => 0.1..=MAX_CLIP_SCALE,
            Self::AudioGain => 0.0..=4.0,
        }
    }

    fn parts_mut(self, clip: &mut Clip) -> (&mut ScalarAnimation, &mut f32) {
        match self {
            Self::Opacity => (&mut clip.opacity_animation, &mut clip.transform.opacity),
            Self::Rotation => (&mut clip.rotation_animation, &mut clip.transform.rotation),
            Self::Scale => (&mut clip.scale_animation, &mut clip.transform.scale),
            Self::AudioGain => (&mut clip.gain_animation, &mut clip.gain),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Clip {
    pub id: String,
    pub asset_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_source: Option<u32>,
    pub start: u32,
    pub source: u32,
    #[serde(default, skip_serializing_if = "zero_subframe")]
    pub source_subframe: u8,
    #[serde(default = "normal_speed", skip_serializing_if = "is_normal_speed")]
    pub speed_percent: u16,
    /// Preserve the fractional source tail when switching between playback speeds.
    #[serde(default, skip_serializing_if = "zero_duration_remainder")]
    pub speed_remainder: u16,
    #[serde(default, skip_serializing_if = "is_zero_i8")]
    pub pitch_semitones: i8,
    #[serde(default, skip_serializing_if = "is_zero_i8")]
    pub audio_balance: i8,
    #[serde(default, skip_serializing_if = "AudioChannels::is_stereo")]
    pub audio_channels: AudioChannels,
    pub length: u32,
    pub gain: f32,
    #[serde(default, skip_serializing_if = "ScalarAnimation::is_empty")]
    pub gain_animation: ScalarAnimation,
    pub slot: bool,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    #[serde(default)]
    pub transform: Transform,
    #[serde(default, skip_serializing_if = "Framing::is_default")]
    pub framing: Framing,
    #[serde(default, skip_serializing_if = "BlendMode::is_normal")]
    pub blend_mode: BlendMode,
    #[serde(default, skip_serializing_if = "PositionAnimation::is_empty")]
    pub position_animation: PositionAnimation,
    #[serde(default, skip_serializing_if = "ScalarAnimation::is_empty")]
    pub opacity_animation: ScalarAnimation,
    #[serde(default, skip_serializing_if = "ScalarAnimation::is_empty")]
    pub rotation_animation: ScalarAnimation,
    #[serde(default, skip_serializing_if = "ScalarAnimation::is_empty")]
    pub scale_animation: ScalarAnimation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_key: Option<ChromaKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<ClipMask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask_strokes: Vec<MaskStroke>,
    #[serde(default)]
    pub adjustment: ColorAdjustment,
    #[serde(default, skip_serializing_if = "no_hue_adjustments")]
    pub hue_adjustments: [HueAdjustment; 6],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_lut: Option<PathBuf>,
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
            group_id: None,
            sync_source: None,
            start,
            source,
            source_subframe: 0,
            speed_percent: NORMAL_SPEED,
            speed_remainder: 0,
            pitch_semitones: 0,
            audio_balance: 0,
            audio_channels: AudioChannels::Stereo,
            length,
            gain: 1.0,
            gain_animation: ScalarAnimation::default(),
            slot: false,
            effects: Vec::new(),
            transition: None,
            transform: Transform::default(),
            framing: Framing::default(),
            blend_mode: BlendMode::default(),
            position_animation: PositionAnimation::default(),
            opacity_animation: ScalarAnimation::default(),
            rotation_animation: ScalarAnimation::default(),
            scale_animation: ScalarAnimation::default(),
            chroma_key: None,
            mask: None,
            mask_strokes: Vec::new(),
            adjustment: ColorAdjustment::default(),
            hue_adjustments: [HueAdjustment::default(); 6],
            color_lut: None,
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

    fn split_tail(&mut self, frame: u32) -> Self {
        let offset = frame - self.start;
        let mut tail = self.clone();
        tail.id = new_id();
        tail.start = frame;
        tail.set_source_units(self.source_units_at(i64::from(offset)));
        tail.length -= offset;
        if !tail.position_animation.is_empty() {
            tail.position_animation.offset =
                tail.position_animation.offset.saturating_add(offset as i32);
        }
        tail.opacity_animation.shift(offset as i32);
        tail.rotation_animation.shift(offset as i32);
        tail.scale_animation.shift(offset as i32);
        tail.gain_animation.shift(offset as i32);
        tail.audio_fade = self.audio_fade.split(offset, tail.length);
        tail.visual_fade = self.visual_fade.split(offset, tail.length);
        tail.transition = None;
        self.length = offset;
        self.speed_remainder = 0;
        tail
    }

    pub fn source_units_at(&self, local: i64) -> i64 {
        let start = i64::from(self.source) * 100 + i64::from(self.source_subframe);
        start
            + if self.freeze {
                0
            } else {
                local * i64::from(self.speed_percent)
            }
    }

    pub fn sync_offset(&self) -> Option<u32> {
        let distance = i64::from(self.sync_source?) - self.source_units_at(0);
        if self.freeze || self.slot || distance < 0 {
            return None;
        }
        let speed = i64::from(self.speed_percent.max(1));
        let frame = u32::try_from((distance + speed / 2) / speed).ok()?;
        (frame < self.length).then_some(frame)
    }

    pub fn source_frame_at(&self, local: i64) -> u32 {
        (self.source_units_at(local).max(0) / 100) as u32
    }

    fn set_source_units(&mut self, units: i64) {
        self.source = (units / 100) as u32;
        self.source_subframe = (units % 100) as u8;
    }

    pub(crate) fn source_duration_units(&self) -> i64 {
        i64::from(self.length) * i64::from(self.speed_percent) + i64::from(self.speed_remainder)
    }

    pub fn position_at(&self, relative_frame: u32) -> [f32; 2] {
        let animation = &self.position_animation;
        let frame = animation.offset.saturating_add(relative_frame as i32);
        let (before, after) = animation
            .keys
            .split_at(animation.keys.partition_point(|key| key.frame <= frame));
        match (before.last(), after.first()) {
            (Some(a), Some(b)) => {
                let t = animation
                    .easing
                    .apply((frame - a.frame) as f32 / (b.frame - a.frame) as f32);
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
                *animation = PositionAnimation::default();
                self.transform.x = x;
                self.transform.y = y;
            }
        }
    }

    pub fn opacity_at(&self, relative_frame: u32) -> f32 {
        self.animated_value(AnimatedProperty::Opacity, relative_frame)
    }

    pub(crate) fn has_audible_gain(&self) -> bool {
        if self.gain_animation.is_empty() {
            self.gain > 0.0
        } else {
            self.gain_animation.keys.iter().any(|key| key.value > 0.0)
        }
    }

    pub fn animation(&self, property: AnimatedProperty) -> &ScalarAnimation {
        match property {
            AnimatedProperty::Opacity => &self.opacity_animation,
            AnimatedProperty::Rotation => &self.rotation_animation,
            AnimatedProperty::Scale => &self.scale_animation,
            AnimatedProperty::AudioGain => &self.gain_animation,
        }
    }

    pub fn animated_value(&self, property: AnimatedProperty, relative_frame: u32) -> f32 {
        let animation = self.animation(property);
        let frame = animation.offset.saturating_add(relative_frame as i32);
        let (before, after) = animation
            .keys
            .split_at(animation.keys.partition_point(|key| key.frame <= frame));
        match (before.last(), after.first()) {
            (Some(a), Some(b)) => {
                let progress = animation
                    .easing
                    .apply((frame - a.frame) as f32 / (b.frame - a.frame) as f32);
                a.value + (b.value - a.value) * progress
            }
            (Some(key), None) | (None, Some(key)) => key.value,
            (None, None) => match property {
                AnimatedProperty::Opacity => self.transform.opacity,
                AnimatedProperty::Rotation => self.transform.rotation,
                AnimatedProperty::Scale => self.transform.scale,
                AnimatedProperty::AudioGain => self.gain,
            },
        }
    }

    pub fn set_opacity_keyframe(&mut self, relative_frame: u32, opacity: f32) -> Result<()> {
        self.set_scalar_keyframe(AnimatedProperty::Opacity, relative_frame, opacity)
    }

    pub fn set_scalar_keyframe(
        &mut self,
        property: AnimatedProperty,
        relative_frame: u32,
        value: f32,
    ) -> Result<()> {
        ensure!(
            relative_frame < self.length,
            "Place the playhead inside this clip."
        );
        ensure!(
            property.range().contains(&value),
            "{} is out of range.",
            property.label()
        );
        let (animation, _) = property.parts_mut(self);
        let frame = animation.offset.saturating_add(relative_frame as i32);
        let key = ScalarKeyframe { frame, value };
        match animation.keys.binary_search_by_key(&frame, |key| key.frame) {
            Ok(index) => animation.keys[index] = key,
            Err(index) => {
                ensure!(
                    animation.keys.len() < MAX_POSITION_KEYS,
                    "This property already has 32 keyframes."
                );
                animation.keys.insert(index, key);
            }
        }
        Ok(())
    }

    pub fn set_static_value(&mut self, property: AnimatedProperty, value: f32) {
        *property.parts_mut(self).1 = value;
    }

    pub fn set_scalar_easing(&mut self, property: AnimatedProperty, easing: Easing) {
        property.parts_mut(self).0.easing = easing;
    }

    pub fn remove_scalar_keyframe(&mut self, property: AnimatedProperty, relative_frame: u32) {
        let value = self.animated_value(property, relative_frame);
        let (animation, base) = property.parts_mut(self);
        let frame = animation.offset.saturating_add(relative_frame as i32);
        if let Ok(index) = animation.keys.binary_search_by_key(&frame, |key| key.frame) {
            animation.keys.remove(index);
            if animation.is_empty() {
                *animation = ScalarAnimation::default();
                *base = value;
            }
        }
    }

    /// Apply settings for the selected media types without changing the destination edit.
    pub fn paste_settings(&mut self, source: &Clip, visual: bool, audio: bool) {
        if visual {
            self.transform = source.transform;
            self.framing = source.framing;
            self.blend_mode = source.blend_mode;
            self.position_animation
                .clone_from(&source.position_animation);
            self.opacity_animation.clone_from(&source.opacity_animation);
            self.rotation_animation
                .clone_from(&source.rotation_animation);
            self.scale_animation.clone_from(&source.scale_animation);
            self.mask = source.mask;
            self.mask_strokes.clone_from(&source.mask_strokes);
            self.chroma_key = source.chroma_key;
            self.adjustment = source.adjustment;
            self.hue_adjustments = source.hue_adjustments;
            self.color_lut.clone_from(&source.color_lut);
            self.visual_fade = source.visual_fade;
        }
        if audio {
            self.gain = source.gain;
            self.gain_animation.clone_from(&source.gain_animation);
            self.pitch_semitones = source.pitch_semitones;
            self.audio_channels = source.audio_channels;
            self.audio_balance = source.audio_balance;
            self.audio_fade = source.audio_fade;
        }
        let replaces = |effect: &Effect| if effect.preset.audio() { audio } else { visual };
        self.effects.retain(|effect| !replaces(effect));
        self.effects.extend(
            source
                .effects
                .iter()
                .filter(|effect| replaces(effect))
                .cloned(),
        );
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            name: name.into(),
            kind,
            clips: Vec::new(),
            muted: false,
            archived: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Marker {
    pub frame: u32,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Project {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub assets: Vec<Asset>,
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled edit".into(),
            width: 1280,
            height: 720,
            assets: Vec::new(),
            markers: Vec::new(),
            tracks: vec![
                Track::new("Video 1", TrackKind::Video),
                Track::new("Audio 1", TrackKind::Audio),
            ],
        }
    }
}

impl Project {
    pub fn set_marker(&mut self, frame: u32, label: &str) -> Result<()> {
        let label = label.trim();
        ensure!(
            frame <= MAX_FRAMES && !label.is_empty() && label.len() <= 256,
            "Choose a valid timeline position and marker name."
        );
        let marker = Marker {
            frame,
            label: label.into(),
        };
        match self
            .markers
            .binary_search_by_key(&frame, |marker| marker.frame)
        {
            Ok(index) => self.markers[index] = marker,
            Err(index) => self.markers.insert(index, marker),
        }
        Ok(())
    }

    pub fn remove_marker(&mut self, frame: u32) {
        if let Ok(index) = self
            .markers
            .binary_search_by_key(&frame, |marker| marker.frame)
        {
            self.markers.remove(index);
        }
    }

    pub(crate) fn source_paths(&self) -> impl Iterator<Item = &Path> {
        self.assets
            .iter()
            .flat_map(|asset| {
                asset
                    .path
                    .as_deref()
                    .into_iter()
                    .chain(asset.proxy.iter().map(|proxy| proxy.path.as_path()))
            })
            .chain(
                self.tracks
                    .iter()
                    .flat_map(|track| &track.clips)
                    .filter_map(|clip| clip.color_lut.as_deref()),
            )
    }

    fn source_paths_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        self.assets
            .iter_mut()
            .flat_map(|asset| {
                asset
                    .path
                    .as_mut()
                    .into_iter()
                    .chain(asset.proxy.iter_mut().map(|proxy| &mut proxy.path))
            })
            .chain(
                self.tracks
                    .iter_mut()
                    .flat_map(|track| &mut track.clips)
                    .filter_map(|clip| clip.color_lut.as_mut()),
            )
    }

    pub fn duration(&self) -> u32 {
        self.tracks
            .iter()
            .filter(|track| !track.archived || !track.muted)
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
            .filter(|track| track.kind == TrackKind::Video && (!track.archived || !track.muted))
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
        let tail = clip.split_tail(frame);
        let id = tail.id.clone();
        self.tracks[row].clips.insert(column + 1, tail);
        Ok(id)
    }

    /// Apply signed start offsets relative to the first clip without changing source trims.
    pub fn align_clip_starts(&mut self, clips: &[(&str, i32)]) -> Result<()> {
        let Some((&(anchor, 0), moved)) = clips.split_first() else {
            bail!("Choose a reference clip for synchronization.");
        };
        ensure!(!moved.is_empty(), "Select at least two clips to sync.");
        let (anchor_row, anchor_index) = self
            .location(anchor)
            .context("Reference clip is missing.")?;
        let start = i64::from(self.tracks[anchor_row].clips[anchor_index].start);
        let mut rows = HashSet::from([anchor_row]);
        let mut placements = Vec::with_capacity(moved.len());
        for &(id, offset) in moved {
            let (row, index) = self.location(id).context("A clip is missing.")?;
            ensure!(rows.insert(row), "Select at most one clip per track.");
            let clip = &self.tracks[row].clips[index];
            ensure!(
                clip.group_id.is_none(),
                "Ungroup clips before syncing them."
            );
            let position = start + i64::from(offset);
            let end = position + i64::from(clip.length);
            ensure!(
                position >= 0 && end <= i64::from(MAX_FRAMES),
                "The synchronized clip would be outside the timeline. Move the reference clip and try again."
            );
            ensure!(
                self.tracks[row].clips.iter().all(|other| {
                    other.id == id
                        || i64::from(other.end()) <= position
                        || i64::from(other.start) >= end
                }),
                "A synchronized clip would overlap another clip on the same track."
            );
            placements.push((row, index, position as u32));
        }
        for (row, index, position) in placements {
            self.tracks[row].clips[index].start = position;
        }
        Ok(())
    }

    pub fn align_clip_markers(&mut self, clips: &[&str]) -> Result<()> {
        let mut markers = clips.iter().map(|id| {
            let (row, index) = self.location(id).context("A camera clip is missing.")?;
            self.tracks[row].clips[index]
                .sync_offset()
                .map(|offset| (*id, offset as i32))
                .context("Set a sync point inside each selected clip before aligning.")
        });
        let anchor = markers.next().context("Select a reference clip.")??;
        let mut placement = vec![(anchor.0, 0)];
        for other in markers {
            let (id, position) = other?;
            placement.push((id, anchor.1 - position));
        }
        self.align_clip_starts(&placement)
    }

    /// Compile multicamera choices into editable timeline cuts and retain the original angles.
    pub fn assemble_camera_angles(
        &mut self,
        angles: &[Clip],
        switches: &BTreeMap<u32, usize>,
        reference_audio: bool,
    ) -> Result<String> {
        ensure!(
            (2..=9).contains(&angles.len()),
            "Select two to nine angles."
        );
        let reference = &angles[0];
        let mut rows = HashSet::new();
        let mut locations = Vec::with_capacity(angles.len());
        for angle in angles {
            let (row, index) = self
                .location(&angle.id)
                .context("A camera clip is missing.")?;
            ensure!(
                rows.insert(row)
                    && self.tracks[row].kind == TrackKind::Video
                    && self.tracks[row].clips[index] == *angle
                    && !angle.slot
                    && !angle.freeze
                    && angle.group_id.is_none()
                    && self
                        .asset(&angle.asset_id)
                        .is_some_and(|asset| asset.kind == AssetKind::Video),
                "Use one ungrouped video clip per track. Reopen the angle editor after editing the source clips."
            );
            locations.push((row, index));
        }
        let mut result = Track::new("Multicamera cut", TrackKind::Video);
        let mut choices = switches.clone();
        choices.entry(reference.start).or_insert(0);
        let assemble = |track: &mut Track, angle: &Clip, start: u32, end: u32| {
            let mut cut = angle.clone();
            if cut.start < start {
                cut = cut.split_tail(start);
            }
            if end < cut.end() {
                cut.split_tail(end);
            }
            cut.id = new_id();
            cut.transition = None;
            if reference_audio {
                cut.gain = 0.0;
                cut.gain_animation = ScalarAnimation::default();
            }
            track.clips.push(cut);
        };
        let mut cuts = choices.iter().peekable();
        while let Some((&start, &angle)) = cuts.next() {
            let end = cuts.peek().map_or(reference.end(), |&(frame, _)| *frame);
            let source = angles.get(angle).context("Invalid camera angle.")?;
            ensure!(
                (reference.start..reference.end()).contains(&start)
                    && end <= reference.end()
                    && (source.start..source.end()).contains(&start),
                "A camera cut is outside its source or the reference timeline range."
            );
            let cut_end = end.min(source.end());
            assemble(&mut result, source, start, cut_end);
            if cut_end < end {
                assemble(&mut result, reference, cut_end, end);
            }
        }
        let audio = if reference_audio {
            ensure!(
                self.asset(&reference.asset_id)
                    .is_some_and(|asset| asset.audio),
                "The reference angle has no source audio."
            );
            let mut audio = reference.clone();
            audio.id = new_id();
            audio.transition = None;
            Some(Track {
                clips: vec![audio],
                ..Track::new("Multicamera audio", TrackKind::Audio)
            })
        } else {
            None
        };
        let mut archives = Vec::new();
        for ((row, index), angle) in locations.into_iter().zip(angles) {
            let track = &mut self.tracks[row];
            if track.clips.len() == 1 {
                track.muted = true;
                track.archived = true;
            } else {
                let name = self
                    .assets
                    .iter()
                    .find(|asset| asset.id == angle.asset_id)
                    .map_or("Camera source", |asset| asset.label.as_str());
                let mut archive = Track::new(name, TrackKind::Video);
                archive.clips.push(track.clips.remove(index));
                archive.muted = true;
                archive.archived = true;
                archives.push(archive);
            }
        }
        let first = result.clips[0].id.clone();
        self.tracks
            .insert(rows.into_iter().max().unwrap_or(0) + 1, result);
        self.tracks.extend(audio);
        self.tracks.extend(archives);
        Ok(first)
    }

    /// Insert a silent frame hold and move the remaining clips on the same track forward.
    pub fn insert_freeze(&mut self, id: &str, frame: u32, length: u32) -> Result<String> {
        let (row, index) = self.location(id).context("Select a video clip.")?;
        let clip = &self.tracks[row].clips[index];
        ensure!(
            !clip.freeze
                && !clip.slot
                && self
                    .asset(&clip.asset_id)
                    .is_some_and(|asset| asset.kind == AssetKind::Video),
            "Choose a regular video clip to freeze."
        );
        ensure!(
            (clip.start..clip.end()).contains(&frame),
            "Place the playhead inside the clip."
        );
        ensure!(
            length > 0
                && self.tracks[row]
                    .clips
                    .iter()
                    .filter(|other| other.end() > frame)
                    .all(|other| other.end().saturating_add(length) <= MAX_FRAMES),
            "The freeze frame would extend past the timeline limit."
        );

        let mut hold = clip.clone();
        let local = frame - clip.start;
        [hold.transform.x, hold.transform.y] = clip.position_at(local);
        hold.transform.opacity = clip.opacity_at(local);
        hold.transform.rotation = clip.animated_value(AnimatedProperty::Rotation, local);
        hold.transform.scale = clip.animated_value(AnimatedProperty::Scale, local);
        hold.position_animation = PositionAnimation::default();
        hold.opacity_animation = ScalarAnimation::default();
        hold.rotation_animation = ScalarAnimation::default();
        hold.scale_animation = ScalarAnimation::default();
        hold.id = new_id();
        hold.source = clip.source_frame_at(i64::from(local));
        hold.source_subframe = 0;
        hold.speed_percent = NORMAL_SPEED;
        hold.speed_remainder = 0;
        hold.pitch_semitones = 0;
        hold.start = frame;
        hold.length = length;
        hold.freeze = true;
        hold.gain = 0.0;
        hold.gain_animation = ScalarAnimation::default();
        hold.effects.retain(|effect| !effect.preset.audio());
        hold.audio_fade = ClipFade::default();
        hold.visual_fade = ClipFade::default();
        hold.transition = None;

        if local > 0 {
            self.split(id, frame)?;
        } else {
            self.tracks[row].clips[index].transition = None;
        }
        let track = &mut self.tracks[row];
        for clip in &mut track.clips {
            if clip.start >= frame {
                clip.start += length;
            }
        }
        let hold_id = hold.id.clone();
        track.clips.push(hold);
        Ok(hold_id)
    }

    /// Remove an interval from every track and bring the remaining timeline together.
    pub fn ripple_delete_range(&mut self, start: u32, end: u32) -> Result<()> {
        ensure!(
            start < end && end <= self.duration(),
            "Choose a range within the timeline."
        );
        for boundary in [start, end] {
            let crossing: Vec<_> = self
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .filter(|clip| clip.start < boundary && boundary < clip.end())
                .map(|clip| clip.id.clone())
                .collect();
            for id in crossing {
                self.split(&id, boundary)?;
            }
        }
        let length = end - start;
        for track in &mut self.tracks {
            track
                .clips
                .retain(|clip| clip.start < start || clip.start >= end);
            for clip in &mut track.clips {
                if clip.start >= end {
                    clip.start -= length;
                }
            }
        }
        self.markers
            .retain(|marker| marker.frame < start || marker.frame >= end);
        for marker in &mut self.markers {
            if marker.frame >= end {
                marker.frame -= length;
            }
        }
        Ok(())
    }

    pub fn ripple_delete(&mut self, id: &str) -> Result<u32> {
        let (row, index) = self.location(id).context("Select a clip to remove.")?;
        let track = &mut self.tracks[row];
        let removed = track.clips.remove(index);
        for clip in &mut track.clips {
            if clip.start >= removed.end() {
                clip.start -= removed.length;
            }
        }
        Ok(removed.start)
    }

    pub fn close_track_gaps(&mut self, row: usize) -> Result<u32> {
        let track = self.tracks.get_mut(row).context("Choose a track.")?;
        let mut ordered: Vec<_> = track.clips.iter().collect();
        ordered.sort_by_key(|clip| clip.start);
        ensure!(
            ordered
                .windows(2)
                .all(|pair| pair[0].end() <= pair[1].start),
            "Resolve overlapping clips first."
        );
        let mut packed_start = 0;
        for clip in ordered {
            ensure!(
                clip.group_id.is_none() || clip.start == packed_start,
                "Ungroup clips that would shift when closing gaps on this track."
            );
            packed_start += clip.length;
        }
        track.clips.sort_by_key(|clip| clip.start);
        let mut end = 0;
        let mut removed = 0;
        let mut original_end = 0;
        for clip in &mut track.clips {
            removed += clip.start - original_end;
            original_end = clip.end();
            clip.start = end;
            end = clip.end();
        }
        Ok(removed)
    }

    pub fn group_clips(&mut self, ids: &HashSet<String>) -> Result<()> {
        ensure!(ids.len() >= 2, "Select at least two clips to group.");
        let mut groups = HashSet::new();
        for id in ids {
            let (row, index) = self
                .location(id)
                .context("A selected clip is no longer present.")?;
            if let Some(group) = &self.tracks[row].clips[index].group_id {
                groups.insert(group.clone());
            }
        }
        let group = new_id();
        for clip in self.tracks.iter_mut().flat_map(|track| &mut track.clips) {
            if ids.contains(&clip.id)
                || clip.group_id.as_ref().is_some_and(|id| groups.contains(id))
            {
                clip.group_id = Some(group.clone());
            }
        }
        Ok(())
    }

    pub fn ungroup_clip(&mut self, id: &str) -> Result<()> {
        let (row, index) = self.location(id).context("Choose a clip in a group.")?;
        let group = self.tracks[row].clips[index]
            .group_id
            .clone()
            .context("This clip is ungrouped.")?;
        for clip in self.tracks.iter_mut().flat_map(|track| &mut track.clips) {
            if clip.group_id.as_deref() == Some(group.as_str()) {
                clip.group_id = None;
            }
        }
        Ok(())
    }

    pub fn move_clip_group(&mut self, id: &str, destination_row: usize, start: u32) -> Result<()> {
        let (row, index) = self.location(id).context("Choose a clip in a group.")?;
        ensure!(
            destination_row < self.tracks.len(),
            "Choose a destination track."
        );
        let anchor = &self.tracks[row].clips[index];
        let group = anchor.group_id.clone().context("This clip is ungrouped.")?;
        let shift_row = destination_row as i64 - row as i64;
        let shift_time = i64::from(start) - i64::from(anchor.start);
        let in_group = |clip: &Clip| clip.group_id.as_deref() == Some(group.as_str());
        let mut placements = Vec::new();
        for (row, track) in self.tracks.iter().enumerate() {
            for clip in track.clips.iter().filter(|clip| in_group(clip)) {
                let target_row = usize::try_from(row as i64 + shift_row)
                    .ok()
                    .filter(|row| *row < self.tracks.len())
                    .context("The group would extend past the tracks.")?;
                let target = &self.tracks[target_row];
                ensure!(
                    target.kind == track.kind,
                    "The group needs compatible tracks."
                );
                let start = u32::try_from(i64::from(clip.start) + shift_time)
                    .context("The group would start before the timeline.")?;
                let end = start.saturating_add(clip.length);
                ensure!(
                    end <= MAX_FRAMES,
                    "The group would extend past the timeline."
                );
                ensure!(
                    target
                        .clips
                        .iter()
                        .filter(|other| !in_group(other))
                        .all(|other| start >= other.end() || end <= other.start),
                    "The group would overlap another clip."
                );
                let mut moved = clip.clone();
                moved.start = start;
                placements.push((target_row, moved));
            }
        }
        for track in &mut self.tracks {
            track.clips.retain(|clip| !in_group(clip));
        }
        for (row, clip) in placements {
            self.tracks[row].clips.push(clip);
        }
        Ok(())
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
                || i64::from(asset.frames) * 100 >= clip.source_duration_units(),
            "The replacement is shorter than the selected clip."
        );
        clip.asset_id.clone_from(&asset.id);
        clip.source = 0;
        clip.source_subframe = 0;
        clip.sync_source = None;
        clip.slot = false;
        clip.freeze &= asset.kind == AssetKind::Video;
        if matches!(asset.kind, AssetKind::Image | AssetKind::Text) {
            clip.speed_percent = NORMAL_SPEED;
            clip.speed_remainder = 0;
        }
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
        let gif = asset.kind == AssetKind::Video
            && !asset.audio
            && asset
                .path
                .as_ref()
                .and_then(|path| path.extension())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"));
        ensure!(
            (asset.kind == AssetKind::Image || gif) && asset.path.is_some(),
            "Choose a local still image or animated GIF."
        );
        let length = if gif { asset.frames } else { 2 * FPS };
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

    pub fn set_clip_speed(&mut self, id: &str, speed: u16) -> Result<()> {
        ensure!(
            (25..=400).contains(&speed),
            "Choose a speed between 0.25× and 4×."
        );
        let (row, index) = self.location(id).context("Select a clip to retime.")?;
        let track = &self.tracks[row];
        let clip = &track.clips[index];
        ensure!(
            !clip.freeze
                && !clip.slot
                && self
                    .asset(&clip.asset_id)
                    .is_some_and(|asset| matches!(asset.kind, AssetKind::Video | AssetKind::Audio)),
            "Choose a video or audio clip to retime."
        );
        if clip.speed_percent == speed {
            return Ok(());
        }
        let source_duration = clip.source_duration_units();
        let length = u32::try_from(source_duration / i64::from(speed))?;
        ensure!(length > 0, "The clip is too short for that speed.");
        let delta = i64::from(length) - i64::from(clip.length);
        let end = clip.end();
        ensure!(
            clip.start.saturating_add(length) <= MAX_FRAMES
                && track
                    .clips
                    .iter()
                    .filter(|other| other.start >= end)
                    .all(|other| i64::from(other.end()) + delta <= i64::from(MAX_FRAMES)),
            "The retimed clip would extend past the timeline limit."
        );
        let track = &mut self.tracks[row];
        for other in &mut track.clips {
            if other.start >= end {
                other.start = (i64::from(other.start) + delta) as u32;
            }
        }
        let clip = &mut track.clips[index];
        clip.length = length;
        clip.speed_percent = speed;
        clip.speed_remainder = (source_duration % i64::from(speed)) as u16;
        Ok(())
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
            clip.opacity_animation.shift(delta);
            clip.rotation_animation.shift(delta);
            clip.scale_animation.shift(delta);
            clip.gain_animation.shift(delta);
            (delta, 0)
        } else if clip.start == start {
            (0, clip.end() as i32 - end.min(MAX_FRAMES) as i32)
        } else {
            (0, 0)
        };
        clip.audio_fade.trim(leading_trim, trailing_trim);
        clip.visual_fade.trim(leading_trim, trailing_trim);
        let shifted_source = clip.source_units_at(i64::from(leading_trim));
        if leading_trim != 0 && shifted_source >= 0 && shifted_source / 100 == i64::from(source) {
            clip.set_source_units(shifted_source);
        } else if source != clip.source {
            clip.set_source_units(i64::from(source) * 100);
            clip.speed_remainder = 0;
        }
        if trailing_trim != 0 || leading_trim == 0 && length != clip.length {
            clip.speed_remainder = 0;
        }
        clip.start = start;
        clip.length = length;
        clip.gain = gain;
        self.tracks[row].clips.push(clip);
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.markers
                .windows(2)
                .all(|pair| pair[0].frame < pair[1].frame)
                && self.markers.iter().all(|marker| marker.frame <= MAX_FRAMES
                    && !marker.label.trim().is_empty()
                    && marker.label.len() <= 256),
            "Invalid timeline markers."
        );
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
                        && clip.source_subframe < 100
                        && clip
                            .sync_source
                            .is_none_or(|frame| frame <= MAX_FRAMES * 100)
                        && clip
                            .group_id
                            .as_ref()
                            .is_none_or(|id| !id.is_empty() && id.len() <= 64)
                        && (25..=400).contains(&clip.speed_percent)
                        && clip.speed_remainder < clip.speed_percent
                        && (-12..=12).contains(&clip.pitch_semitones)
                        && (-100..=100).contains(&clip.audio_balance)
                        && (0.0..=4.0).contains(&clip.gain),
                    "Invalid clip settings."
                );
                let transform = clip.transform;
                ensure!(
                    AnimatedProperty::Scale.range().contains(&transform.scale)
                        && (-1.5..=1.5).contains(&transform.x)
                        && (-1.5..=1.5).contains(&transform.y)
                        && (-180.0..=180.0).contains(&transform.rotation)
                        && (0.0..=1.0).contains(&transform.opacity),
                    "Invalid clip transform."
                );
                ensure!(clip.framing.valid(), "Invalid crop percentages.");
                let animation = &clip.position_animation;
                ensure!(
                    clip.opacity_animation
                        .valid(AnimatedProperty::Opacity.range())
                        && clip
                            .rotation_animation
                            .valid(AnimatedProperty::Rotation.range()),
                    "Invalid opacity or rotation keyframes."
                );
                ensure!(
                    clip.scale_animation.valid(AnimatedProperty::Scale.range()),
                    "Invalid scale keyframes."
                );
                ensure!(
                    clip.gain_animation
                        .valid(AnimatedProperty::AudioGain.range()),
                    "Invalid gain keyframes."
                );
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
                    clip.hue_adjustments.into_iter().all(|range| {
                        (-180..=180).contains(&range.hue)
                            && (-100..=100).contains(&range.saturation)
                            && (-100..=100).contains(&range.intensity)
                    }),
                    "Invalid hue-range adjustments."
                );
                ensure!(
                    clip.color_lut.as_ref().is_none_or(|path| path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("cube"))),
                    "Select a .cube color LUT."
                );
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
                    clip.mask_strokes.len() <= MaskStroke::MAX_STROKES
                        && clip.mask_strokes.iter().all(MaskStroke::valid),
                    "Invalid clip mask brush strokes."
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
                            || clip.source_units_at(0) + clip.source_duration_units()
                                <= i64::from(asset.frames) * 100
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

/// Copy local media and LUTs into a self-contained folder with a relative-path project.
pub fn save_portable_copy(
    project: &Project,
    parent: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u8),
) -> Result<PathBuf> {
    let canceled = || -> Result<()> {
        ensure!(!cancel.load(Ordering::Relaxed), "Portable copy canceled.");
        Ok(())
    };
    canceled()?;
    project.validate()?;
    let mut copy = project.clone();
    for asset in &mut copy.assets {
        asset.proxy = None;
    }
    let parent = std::path::absolute(parent)?;
    let folder = tempfile::Builder::new()
        .prefix("CutAd Project-")
        .tempdir_in(parent)?;
    let media = folder.path().join("Media");
    std::fs::create_dir(&media)?;

    let mut sources = HashMap::<PathBuf, (PathBuf, std::fs::Metadata)>::new();
    let mut total = 0_u64;
    for path in copy.source_paths_mut() {
        canceled()?;
        let original = path
            .canonicalize()
            .with_context(|| format!("Relink or locate {} before copying.", path.display()))?;
        if !sources.contains_key(&original) {
            let metadata = original.metadata()?;
            ensure!(
                metadata.is_file(),
                "Choose a regular file for {}.",
                path.display()
            );
            let extension = path.extension().and_then(|ext| ext.to_str()).filter(|ext| {
                !ext.is_empty()
                    && ext.len() <= 12
                    && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
            let mut output = media.join(format!("Media-{:05}", sources.len() + 1));
            if let Some(extension) = extension {
                output.set_extension(extension);
            }
            total = total.saturating_add(metadata.len());
            sources.insert(original.clone(), (output, metadata));
        }
        path.clone_from(&sources[&original].0);
    }

    let mut copied = 0_u64;
    let mut buffer = vec![0; 1024 * 1024];
    for (source, (destination, before)) in sources {
        let mut input = std::fs::File::open(&source)?;
        let mut output = std::fs::File::create_new(destination)?;
        loop {
            canceled()?;
            let size = input.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            output.write_all(&buffer[..size])?;
            copied = copied.saturating_add(size as u64);
            progress((copied.saturating_mul(99) / total.max(1)).min(99) as u8);
        }
        let after = input.metadata()?;
        ensure!(
            before.len() == after.len() && before.modified()? == after.modified()?,
            "Source changed while copying {}.",
            source.display()
        );
    }
    canceled()?;
    save(&copy, &folder.path().join("Project.cutad"))?;
    canceled()?;
    let file = folder.keep().join("Project.cutad");
    progress(100);
    Ok(file)
}

pub fn save(project: &Project, path: &Path) -> Result<()> {
    project.validate()?;
    let path = std::path::absolute(path)?;
    let parent = path.parent().context("Choose a project file.")?;
    let mut copy = project.clone();
    for media in copy.source_paths_mut() {
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
    for path in data.project.source_paths_mut() {
        *path = parent.join(&*path);
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
    fn slot<'a>(
        project: &'a mut Project,
        row: usize,
        label: &str,
        start: u32,
        length: u32,
    ) -> &'a mut Clip {
        let asset = Asset::placeholder(label, AssetKind::Image, length);
        let mut clip = Clip::new(asset.id.clone(), start, 0, length);
        clip.slot = true;
        project.assets.push(asset);
        project.tracks[row].clips.push(clip);
        project.tracks[row].clips.last_mut().unwrap()
    }

    let mut project = Project {
        name: name.into(),
        ..Project::default()
    };
    match name {
        "Side-by-side review" => {
            project.tracks[0].name = "Left panel".into();
            project
                .tracks
                .push(Track::new("Right panel", TrackKind::Video));
            for (row, label, x) in [(0, "Left shot", -0.25), (2, "Right shot", 0.25)] {
                slot(&mut project, row, label, 0, 120).transform = Transform {
                    scale: 0.48,
                    x,
                    ..Transform::default()
                };
            }
            project.add_title(TitleStyle::LowerThird, "Side-by-side review", 30)?;
            return Ok(project);
        }
        "Vertical reveal" => {
            (project.width, project.height) = (720, 1280);
            slot(&mut project, 0, "Setup", 0, 60).set_visual_fade(9, 0);
            let reveal = slot(&mut project, 0, "Reveal", 60, 90);
            reveal.transition = Some(Transition {
                kind: TransitionKind::CircleOpen,
                frames: 12,
            });
            reveal.set_visual_fade(0, 12);
            project.add_title(TitleStyle::LowerThird, "The reveal", 60)?;
            return Ok(project);
        }
        _ => {}
    }

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
    (project.width, project.height) = (width, height);
    for (label, length) in slots {
        let start = project.duration();
        slot(&mut project, 0, label, start, length);
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
            clip.source_subframe = 0;
            clip.speed_percent = NORMAL_SPEED;
            clip.speed_remainder = 0;
            template.assets.push(asset);
        }
    }
    template
}
