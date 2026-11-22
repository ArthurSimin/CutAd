//! Stock, offline presets implemented as FFmpeg filters with stable project IDs.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Color,
    Lens,
    Motion,
    Texture,
    Audio,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Color,
        Self::Lens,
        Self::Motion,
        Self::Texture,
        Self::Audio,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Color => "Color",
            Self::Lens => "Lens",
            Self::Motion => "Motion",
            Self::Texture => "Texture",
            Self::Audio => "Audio",
        }
    }
}

macro_rules! presets {
    ($($id:ident, $cat:ident, $name:literal, $detail:literal;)*) => {
        #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
        #[serde(rename_all = "snake_case")]
        pub enum Preset { $($id,)* }

        impl Preset {
            pub const ALL: &'static [Self] = &[$(Self::$id,)*];

            pub fn name(self) -> &'static str {
                match self { $(Self::$id => $name,)* }
            }

            pub fn description(self) -> &'static str {
                match self { $(Self::$id => $detail,)* }
            }

            pub fn category(self) -> Category {
                match self { $(Self::$id => Category::$cat,)* }
            }
        }
    }
}

presets! {
    DeepMono, Color, "Deep mono", "Black and white with firm contrast";
    WarmAmber, Color, "Warm amber", "Golden midtones and softer shadows";
    CoolSteel, Color, "Cool steel", "Clean blue highlights and cool shadows";
    SepiaPaper, Color, "Sepia paper", "Warm monochrome with an archival tone";
    FadedPrint, Color, "Faded print", "Lifted blacks and softer saturation";
    SaturatedPop, Color, "Color pop", "Stronger color and gently raised contrast";
    Moonlight, Color, "Moonlight", "Dim, cool shadows and restrained color";
    TealEmber, Color, "Teal and ember", "Cool shadows with warmer highlights";

    SoftFocus, Lens, "Soft focus", "Smooth the image with a Gaussian blur";
    DetailBoost, Lens, "Detail boost", "Sharpen edges and recover local contrast";
    DarkCorners, Lens, "Dark corners", "Darken the edges and draw focus to the center";
    BrightCorners, Lens, "Bright corners", "Lift illumination near the edges";
    ChromaticSplit, Lens, "Chromatic split", "Separate the horizontal color channels";
    RadialWarp, Lens, "Radial warp", "Bow the image around its center";
    EdgeSketch, Lens, "Edge sketch", "Draw high-contrast lines around details";
    DreamSoft, Lens, "Dream diffusion", "Combine soft focus with lifted gamma";

    SlowPush, Motion, "Slow push", "Move steadily into the center of the shot";
    SlowPull, Motion, "Slow pull", "Pull away from the center of the shot";
    Handheld, Motion, "Handheld drift", "Add a continuous handheld roll";
    PulseZoom, Motion, "Pulse zoom", "Cycle in and out around the center";
    FadeIn, Motion, "Fade in", "Reveal the clip from transparency";
    FadeOut, Motion, "Fade out", "Fade the clip into transparency";
    WhiteReveal, Motion, "White reveal", "Reveal the image from a white flash";
    MotionTrail, Motion, "Motion trail", "Leave fading traces behind moving highlights";

    FilmNoise, Texture, "Film grain", "Add uniform animated grain";
    PixelBlocks, Texture, "Pixel blocks", "Reduce the image to large color blocks";
    SignalNoise, Texture, "Signal noise", "Add television-like noise and color separation";
    PosterPrint, Texture, "Poster print", "Reduce color steps for screen-print texture";
    ColorNegative, Texture, "Color negative", "Invert the image colors";
    HardLight, Texture, "Hard light", "Crushed shadows and bold contrast";
    NightVision, Texture, "Night scope", "Tint monochrome footage green with fine noise";
    Blueprint, Texture, "Blueprint", "Trace edges with a cool technical tint";

    VoiceClarity, Audio, "Voice clarity", "Reduce rumble and bring speech forward";
    RadioVoice, Audio, "Radio voice", "Narrow the sound to a midrange broadcast tone";
    LowPass, Audio, "Underwater", "Soften treble for an underwater sound";
    BassLift, Audio, "Bass lift", "Boost low frequencies";
    TrebleLift, Audio, "Air and detail", "Boost higher frequencies";
    Compress, Audio, "Level peaks", "Reduce loud peaks with compression";
    NoiseClean, Audio, "Noise cleanup", "Reduce steady background noise";
    AudioFadeIn, Audio, "Audio fade in", "Raise volume gradually at the start";
    AudioFadeOut, Audio, "Audio fade out", "Lower volume gradually at the end";
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Effect {
    pub preset: Preset,
    pub amount: u8,
    pub enabled: bool,
}

impl Effect {
    pub fn new(preset: Preset) -> Self {
        Self {
            preset,
            amount: 75,
            enabled: true,
        }
    }
}

impl Preset {
    pub fn audio(self) -> bool {
        self.category() == Category::Audio
    }

    pub fn filter(self, amount: u8, frames: u32, size: [usize; 2]) -> String {
        self.filter_at_rate(amount, frames, size, crate::project::FPS)
    }

    pub fn filter_at_rate(self, amount: u8, frames: u32, size: [usize; 2], rate: u32) -> String {
        let a = f64::from(amount) / 100.0;
        let [width, height] = size;
        let fade = ((a * f64::from(frames) * 0.45).ceil() as u32)
            .max(1)
            .min(frames);
        let tail = frames - fade;
        let fade_time = f64::from(fade) / 30.0;
        let tail_time = f64::from(tail) / 30.0;
        let frame_divisor = frames.saturating_sub(1).max(1);
        let zoom = 0.28 * a;
        let center = |z: String| {
            format!(
                "zoompan=z='{z}':x='(iw-iw/zoom)/2':y='(ih-ih/zoom)/2':d=1:s={width}x{height}:fps={rate}"
            )
        };
        let chroma = (a * 15.0).ceil() as u32;
        match self {
            Self::DeepMono => format!("hue=s={:.4},eq=contrast={:.4}", 1.0 - a, 1.0 + 0.28 * a),
            Self::WarmAmber => format!(
                "colorbalance=rm={:.4}:gm={:.4}:bm={:.4}:pl=1",
                0.24 * a,
                0.07 * a,
                -0.22 * a
            ),
            Self::CoolSteel => format!(
                "colorbalance=rs={:.4}:bs={:.4}:bh={:.4}:pl=1",
                -0.22 * a,
                0.24 * a,
                0.18 * a
            ),
            Self::SepiaPaper => format!(
                "hue=s={:.4},colorbalance=rm={:.4}:gm={:.4}:bm={:.4}",
                1.0 - a,
                0.30 * a,
                0.09 * a,
                -0.27 * a
            ),
            Self::FadedPrint => format!(
                "eq=contrast={:.4}:saturation={:.4}:gamma={:.4}",
                1.0 - 0.35 * a,
                1.0 - 0.44 * a,
                1.0 + 0.38 * a
            ),
            Self::SaturatedPop => format!(
                "eq=saturation={:.4}:contrast={:.4}",
                1.0 + 0.85 * a,
                1.0 + 0.12 * a
            ),
            Self::Moonlight => format!(
                "eq=brightness={:.4}:saturation={:.4},colorbalance=rs={:.4}:bm={:.4}",
                -0.20 * a,
                1.0 - 0.35 * a,
                -0.19 * a,
                0.30 * a
            ),
            Self::TealEmber => format!(
                "colorbalance=rs={:.4}:gs={:.4}:bs={:.4}:rh={:.4}:gh={:.4}:bh={:.4}:pl=1",
                -0.35 * a,
                0.18 * a,
                0.18 * a,
                0.28 * a,
                0.08 * a,
                -0.22 * a
            ),

            Self::SoftFocus => format!("gblur=sigma={:.4}:steps=2", 11.0 * a),
            Self::DetailBoost => format!("unsharp=5:5:{:.4}:5:5:0", 1.9 * a),
            Self::DarkCorners => format!("vignette=angle={:.4}", 1.30 * a),
            Self::BrightCorners => format!("vignette=angle={:.4}:mode=backward", 0.64 * a),
            Self::ChromaticSplit => format!("chromashift=cbh={chroma}:crh=-{chroma}:edge=smear"),
            Self::RadialWarp => format!("lenscorrection=k1={:.4}:k2={:.4}", -0.28 * a, 0.19 * a),
            Self::EdgeSketch => format!(
                "edgedetect=low={:.4}:high={:.4}:mode=wires",
                0.11 - 0.07 * a,
                0.35 - 0.21 * a
            ),
            Self::DreamSoft => format!(
                "gblur=sigma={:.4},eq=gamma={:.4}:contrast={:.4}",
                3.8 * a,
                1.0 + 0.28 * a,
                1.0 - 0.11 * a
            ),

            Self::SlowPush => center(format!("1+{zoom:.5}*min(on/{frame_divisor},1)")),
            Self::SlowPull => center(format!("1+{zoom:.5}*(1-min(on/{frame_divisor},1))")),
            Self::Handheld => format!(
                "rotate='({a:.4}*0.065)*sin(t*8)+({a:.4}*0.027)*sin(t*17)':fillcolor=black@0"
            ),
            Self::PulseZoom => center(format!("1+{zoom:.5}*(1-cos(on/{rate}*2*PI*1.7))/2")),
            Self::FadeIn => format!("fade=t=in:s=0:n={fade}:alpha=1"),
            Self::FadeOut => format!("fade=t=out:s={tail}:n={fade}:alpha=1"),
            Self::WhiteReveal => format!("fade=t=in:s=0:n={fade}:color=white"),
            Self::MotionTrail => format!("lagfun=decay={:.4}", 0.65 + 0.33 * a),

            Self::FilmNoise => format!(
                "noise=alls={}:allf=t+u:all_seed=403",
                (3.0 + 43.0 * a).round() as u32
            ),
            Self::PixelBlocks => {
                let block = (2.0 + 24.0 * a).round() as u32;
                format!("pixelize=w={block}:h={block}")
            }
            Self::SignalNoise => format!(
                "noise=alls={}:allf=t:all_seed=117,chromashift=cbh={chroma}:crh=-{chroma}:edge=smear",
                (7.0 + 51.0 * a).round() as u32
            ),
            Self::PosterPrint => {
                let step = (2.0 + 75.0 * a).round() as u32;
                format!(
                    "lutrgb=r='floor(val/{step})*{step}':g='floor(val/{step})*{step}':b='floor(val/{step})*{step}'"
                )
            }
            Self::ColorNegative => format!("negate,eq=saturation={:.4}", 0.65 + 0.35 * a),
            Self::HardLight => format!(
                "eq=contrast={:.4}:gamma={:.4}:brightness={:.4}",
                1.0 + 0.85 * a,
                1.0 - 0.21 * a,
                -0.06 * a
            ),
            Self::NightVision => format!(
                "hue=s=0,colorbalance=gm={:.4}:gh={:.4}:rm={:.4},noise=alls={}:allf=t+u:all_seed=802",
                0.52 * a,
                0.25 * a,
                -0.15 * a,
                (21.0 * a).round() as u32
            ),
            Self::Blueprint => format!(
                "edgedetect=low=0.06:high=0.18:mode=wires,colorbalance=rs={:.4}:bs={:.4}:bh={:.4}",
                -0.21 * a,
                0.20 * a,
                0.45 * a
            ),

            Self::VoiceClarity => format!(
                "highpass=f={:.0},equalizer=f=2800:t=q:w=1.1:g={:.3}",
                45.0 + 85.0 * a,
                7.0 * a
            ),
            Self::RadioVoice => format!(
                "highpass=f={:.0},lowpass=f={:.0}",
                50.0 + 650.0 * a,
                18_000.0 - 15_900.0 * a
            ),
            Self::LowPass => format!("lowpass=f={:.0}", 900.0 + 16_000.0 * (1.0 - a).powi(2)),
            Self::BassLift => format!("bass=f=110:g={:.3}", 12.0 * a),
            Self::TrebleLift => format!("treble=f=4700:g={:.3}", 12.0 * a),
            Self::Compress => format!(
                "acompressor=threshold={:.5}:ratio={:.4}:attack=8:release=100",
                0.27 - 0.23 * a,
                1.0 + 9.0 * a
            ),
            Self::NoiseClean => format!("afftdn=nr={:.3}:nf=-50", 2.0 + 24.0 * a),
            Self::AudioFadeIn => format!("afade=t=in:st=0:d={fade_time:.6}"),
            Self::AudioFadeOut => format!("afade=t=out:st={tail_time:.6}:d={fade_time:.6}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Dissolve,
    DipBlack,
    DipWhite,
    WipeLeft,
    WipeUp,
    SlideLeft,
    CircleOpen,
    SoftBlur,
}

impl TransitionKind {
    pub const ALL: [Self; 8] = [
        Self::Dissolve,
        Self::DipBlack,
        Self::DipWhite,
        Self::WipeLeft,
        Self::WipeUp,
        Self::SlideLeft,
        Self::CircleOpen,
        Self::SoftBlur,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Dissolve => "Dissolve",
            Self::DipBlack => "Dip to black",
            Self::DipWhite => "Dip to white",
            Self::WipeLeft => "Wipe left",
            Self::WipeUp => "Wipe up",
            Self::SlideLeft => "Slide left",
            Self::CircleOpen => "Circle open",
            Self::SoftBlur => "Blur reveal",
        }
    }

    pub fn filter(self) -> &'static str {
        match self {
            Self::Dissolve => "fade",
            Self::DipBlack => "fadeblack",
            Self::DipWhite => "fadewhite",
            Self::WipeLeft => "wipeleft",
            Self::WipeUp => "wipeup",
            Self::SlideLeft => "slideleft",
            Self::CircleOpen => "circleopen",
            Self::SoftBlur => "hblur",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub frames: u32,
}
