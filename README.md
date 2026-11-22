# CutAd

[![Build, test, and release](https://github.com/ArthurSimin/CutAd/actions/workflows/build.yml/badge.svg)](https://github.com/ArthurSimin/CutAd/actions/workflows/build.yml) · [Download CutAd](https://github.com/ArthurSimin/CutAd/releases/latest) · [Connect an AI client](docs/MCP.md)

CutAd is an offline desktop video editor written in Rust for Linux and Windows. Editing and export run locally, without ads, promotions, accounts, or watermarks.

![CutAd timeline showing an original test clip, title controls, audio waveform, and full-resolution preview](docs/workspace.png)

## Get CutAd

Download the Windows x86-64 ZIP or Linux x86-64 tarball from [Releases](https://github.com/ArthurSimin/CutAd/releases). Extract the archive and run the included executable. Install FFmpeg 8 or later for media preview and export. Releases include dependency notices and SHA-256 checksums.

## Use CutAd with an AI client

Run `cutad --mcp-config codex "/absolute/path/My edit.cutad"` to generate a ready-to-paste entry. The [MCP setup guide](docs/MCP.md) also covers Claude Code, OpenCode, Kilo Code, and Antigravity. Use read-only mode to inspect a timeline or opt in to editing and export.

## Run from source

Install Rust 1.95 or later and FFmpeg 8.0 or later. Put `ffmpeg` and `ffprobe` on PATH or beside the executable. Choose a build with libx264, libvpx, libopus, FFV1, FLAC, and the drawtext filter. On Linux, the app uses OpenGL, ALSA, and X11 or Wayland. Source builds need the ALSA development package. File selection uses an XDG Desktop Portal backend or the Zenity fallback.

```sh
cargo run --locked --release
cargo build --locked --release
cargo test --locked --release
```

The release executable is `target/release/cutad` on Linux and `target/release/cutad.exe` on Windows. [FFmpeg downloads](https://ffmpeg.org/download.html)

GitHub builds Linux releases on Ubuntu 24.04. Build from source if your distribution needs a different ABI. The Windows x86-64 build uses the same Rust source. Extract the new archive to its own folder and run its `cutad.exe`. The current version is shown beside the CutAd logo.

## Edit a video

1. Import videos, audio files or still images into the left dock. Drag a media tile onto the timeline, drag a clip to move it, or drag either end to trim.
2. Click the ruler to position the playhead. Right-click clips to split, trim to the playhead, duplicate, replace media, detach audio or remove the clip.
3. Select a clip to open the right inspector. Adjust scale, position, rotation, opacity, and timing under **Video**, or gain and fades under **Audio**. The timeline displays waveforms for audible source clips. **Mask** supports rectangle, ellipse, and linear masks with position, rotation, feathering, and inversion. Use **Color** for brightness, contrast, saturation, and chroma key. Put the background you want to reveal on a lower video track. Add editable text under **Text** in the left dock. Choose a preset under **Effects** or **Filters**, and use **Apply stack** to update its strength, order, or enabled state.
4. For a transition, select the incoming clip at a cut on the same video track. Transitions use the last outgoing frame and the incoming clip. Their duration fits inside the incoming clip and leaves edit timing intact.
5. Scrub the ruler to preview the current frame automatically. **Auto** uses a responsive draft while scrubbing or playing and shows full canvas resolution when paused. Select **Full canvas** to play at canvas resolution, or **Half resolution** and **Quarter resolution** to reduce decoder and display work. Use **1:1** to inspect native pixels and **Focus** or **F** to expand the player. **Project** has a **Reduce interface motion** option.
6. New projects follow the first video’s dimensions within the 4096-pixel canvas limit. For an existing edit, choose **Canvas → Match first video**. Export uses the canvas dimensions by default. Choose **MKV · Lossless** for FFV1 video with 4:4:4 chroma and FLAC audio, or **MP4** for wider playback compatibility. The compositing pipeline works in 8-bit SDR with a 48 kHz stereo mix. Video export supports 24, 30, 60, and 120 fps. Supported high frame rates are rendered from source frames before encoding.
7. Save a `.cutad` project. Media inside the project folder is stored with relative paths. Keep that folder together when moving projects.

See the [CapCut workflow inventory](CAPCUT_WORKFLOWS.md) and [setup evidence](SETUP_EVIDENCE.md) for the public feature reference and installer inspection.

CutAd includes 41 presets across color, lens, motion, texture and audio, plus eight transitions. Higher numbered video tracks are composited in front of lower numbered tracks. The timeline uses 30 ticks per second. Select a visual clip and open **Motion** to add a position keyframe at the playhead. Move the playhead and edit X or Y to create a second keyframe. Diamonds mark the keyframes on the timeline.

## Import stickers and add text

- Move the playhead, open **Stickers**, and choose **Import sticker**. Pick a still image such as a transparent PNG or WebP. CutAd opens it on an editable overlay track.
- Select the overlay to adjust scale, position, rotation, opacity, and duration. The source image stays in its original location.
- Open **Text** for animated **Impact**, **Neon**, **Glitch**, and **Comic pop** styles. Selecting a title opens the **Text** inspector with the font family at the top. Wording, size, alignment, line spacing, fill, outline, and background update as you edit.
- Click the font family or open **Fonts** in the top bar. **Installed** lists system and CutAd fonts, with a sample for the selected face. **DaFont** lets you browse categories, filter the current page, or paste a direct DaFont font page URL. Review its license and choose **Install font**. **Open fonts** contains a curated catalog from Google Fonts.
- Use **Import font…** to add a local TTF, OTF, or ZIP. Downloads and imports are saved to the CutAd font library. Original DaFont archives are kept in the library’s `sources` folder so the author’s license and notices stay available. [DaFont FAQ](https://www.dafont.com/faq.php)
- Titles are transparent unless **Background box** is enabled. Choose **Text → Import captions…** to turn a UTF-8 SRT into editable caption clips at its original timecodes.
- Projects remember the selected family and face. Install the same font on another computer to preserve the appearance. CutAd falls back to a matching regular face or the preset font if it cannot find the selected one.

## Captions and separate exports

- In **Text**, use **Export captions…** to save visible caption clips as SRT. Install [whisper.cpp](https://github.com/ggml-org/whisper.cpp) and put `whisper-cli` beside CutAd or on PATH to use **Auto captions…**. Choose a compatible local GGML model. Recognition uses the audible timeline mix offline. CutAd adds editable subtitle clips.
- In **Project**, choose **Export audio…** for a 48 kHz stereo PCM WAV, or **Export current frame…** for a full-canvas PNG of the playhead. Both use the timeline renderer.

## Templates and drafts

Select **Templates** to start with a replaceable portrait or widescreen layout, save a reusable template, or import a local, readable CapCut JSON draft. For media stored outside the draft directory, select **Import draft with media folder** or relink the footage after import.

The importer translates supported video, still-image, audio and plain-text tracks, timeline ranges, canvas size and volume. It reports unsupported settings and creates replaceable slots for missing media. The current test coverage uses generated draft fixtures. See the [desktop workflow coverage](FEATURES.md) for supported editing features, interoperability limits, and gaps.

Use the same renderer from a terminal for batch export:

```sh
cutad --import-draft draft_content.json project.cutad ./media
cutad --render project.cutad finished.mp4
cutad --render project.cutad master.mkv
cutad --export-audio project.cutad mix.wav
cutad --export-captions project.cutad captions.srt
cutad --export-frame project.cutad still.png 30
```

## License

CutAd source is MIT-licensed. The Rust dependencies retain the licenses declared in their own manifests. The bundled Ubuntu Light and DejaVu Sans Bold fonts retain their respective licenses. See [asset sources and notices](assets/README.md). FFmpeg runs as a separate program and carries its own license. [FFmpeg licensing](https://ffmpeg.org/legal.html)

## CI and releases

GitHub Actions builds and tests on Linux and Windows for main-branch pushes and pull requests. Pushing a version tag that matches `Cargo.toml` builds both archives and publishes a release after both test jobs pass. `scripts/package.py` assembles each archive from its release executable and installed Cargo dependencies.
