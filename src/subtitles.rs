//! Exchange local SubRip cues and editable CutAd caption clips.

use crate::project::{
    Asset, Clip, FPS, MAX_FRAMES, Project, TitleStyle, Track, TrackKind, same_path,
};
use anyhow::{Context, Result, ensure};
use std::{collections::HashMap, fs::File, io::Read, io::Write, path::Path};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub struct Cue {
    start: u32,
    end: u32,
    text: String,
}

impl Cue {
    pub(crate) fn normalize_generated(&mut self, end: u32) -> bool {
        self.end = self.end.min(end);
        self.text = self.text.trim().to_owned();
        self.start < self.end && !self.text.is_empty()
    }
}

pub fn read(path: &Path) -> Result<Vec<Cue>> {
    let mut data = String::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_string(&mut data)
        .context("Use a UTF-8 SRT subtitle file.")?;
    ensure!(data.len() as u64 <= MAX_BYTES, "The SRT exceeds 2 MiB.");
    parse(&data)
}

pub fn write(project: &Project, path: &Path) -> Result<usize> {
    project.validate()?;
    let path = std::path::absolute(path)?;
    ensure!(
        project
            .assets
            .iter()
            .filter_map(|asset| asset.path.as_deref())
            .all(|source| !same_path(source, &path)),
        "Choose an SRT file separate from source media."
    );
    let captions: HashMap<_, _> = project
        .assets
        .iter()
        .filter_map(|asset| {
            let title = asset.title.as_ref()?;
            (title.style == TitleStyle::Caption && !title.text.trim().is_empty())
                .then_some((asset.id.as_str(), title.text.as_str()))
        })
        .collect();
    let mut cues: Vec<_> = project
        .tracks
        .iter()
        .filter(|track| !track.muted && track.kind == TrackKind::Video)
        .flat_map(|track| &track.clips)
        .filter_map(|clip| Some((clip, captions.get(clip.asset_id.as_str())?)))
        .collect();
    ensure!(
        !cues.is_empty(),
        "Add a caption to an unmuted track before exporting subtitles."
    );
    cues.sort_by_key(|(clip, _)| clip.start);

    let parent = path.parent().context("Choose an SRT file.")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    for (index, (clip, text)) in cues.iter().enumerate() {
        writeln!(temp, "{}", index + 1)?;
        writeln!(
            temp,
            "{} --> {}",
            timecode(clip.start),
            timecode(clip.end())
        )?;
        // Blank lines delimit cues in SubRip, and titles can contain either line ending.
        for line in text
            .split(['\r', '\n'])
            .filter(|line| !line.trim().is_empty())
        {
            writeln!(temp, "{line}")?;
        }
        writeln!(temp)?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path)
        .context("Could not replace the SRT file.")?;
    Ok(cues.len())
}

fn timecode(frame: u32) -> String {
    // Rounding down here preserves the timeline frame when the importer rounds up.
    let ms = u64::from(frame) * 1000 / u64::from(FPS);
    let seconds = ms / 1000;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        ms % 1000
    )
}

pub fn parse(data: &str) -> Result<Vec<Cue>> {
    let mut cues = Vec::new();
    let mut block = Vec::new();
    for line in data.trim_start_matches('\u{feff}').lines().chain([""]) {
        if !line.trim().is_empty() {
            block.push(line);
            continue;
        }
        if block.is_empty() {
            continue;
        }
        let number = cues.len() + 1;
        let start = usize::from(block[0].trim().parse::<u32>().is_ok());
        let (from, until) = block
            .get(start)
            .and_then(|line| line.split_once("-->"))
            .with_context(|| format!("Subtitle {number} needs a start and end time."))?;
        let time = |part: &str| part.split_whitespace().next().and_then(milliseconds);
        let from =
            time(from).with_context(|| format!("Subtitle {number} has an invalid start time."))?;
        let until =
            time(until).with_context(|| format!("Subtitle {number} has an invalid end time."))?;
        ensure!(
            from < until && until <= 86_400_000,
            "Subtitle {number} has an invalid time range."
        );
        let frame = |ms: u64| (ms * u64::from(FPS)).div_ceil(1_000) as u32;
        let start_frame = frame(from).min(MAX_FRAMES - 1);
        let end_frame = frame(until).max(start_frame + 1);
        let text = block[start + 1..].join("\n");
        let text = ["<b>", "</b>", "<i>", "</i>", "<u>", "</u>"]
            .iter()
            .fold(text, |text, tag| text.replace(tag, ""));
        ensure!(
            !text.is_empty() && text.len() <= 4096 && !text.contains('\0'),
            "Subtitle {number} has empty or invalid text."
        );
        ensure!(
            number <= 8000,
            "One import can contain up to 8,000 subtitles."
        );
        cues.push(Cue {
            start: start_frame,
            end: end_frame,
            text,
        });
        block.clear();
    }
    ensure!(!cues.is_empty(), "The SRT contains no subtitles.");
    cues.sort_by_key(|cue| cue.start);
    Ok(cues)
}

fn milliseconds(timestamp: &str) -> Option<u64> {
    let (h, rest) = timestamp.split_once(':')?;
    let (m, rest) = rest.split_once(':')?;
    let (s, ms) = rest.split_once([',', '.'])?;
    if h.len() != 2 || m.len() != 2 || s.len() != 2 || ms.len() != 3 {
        return None;
    }
    let h = h.parse::<u64>().ok()?;
    let m = m.parse::<u64>().ok()?;
    let s = s.parse::<u64>().ok()?;
    let ms = ms.parse::<u64>().ok()?;
    (m < 60 && s < 60).then_some(((h * 60 + m) * 60 + s) * 1_000 + ms)
}

pub fn add_tracks(project: &mut Project, cues: Vec<Cue>) -> Result<String> {
    let mut tracks: Vec<Track> = Vec::new();
    let mut assets = Vec::with_capacity(cues.len());
    let mut first = None;
    for cue in cues {
        let lane = tracks
            .iter()
            .position(|track| {
                track
                    .clips
                    .last()
                    .is_none_or(|clip| clip.end() <= cue.start)
            })
            .unwrap_or(tracks.len());
        if lane == tracks.len() {
            ensure!(
                lane < 8,
                "The SRT has more than eight simultaneous captions."
            );
            let name = if lane == 0 {
                "Captions".into()
            } else {
                format!("Captions {}", lane + 1)
            };
            tracks.push(Track::new(name, TrackKind::Video));
        }
        let mut asset = Asset::text(TitleStyle::Caption, cue.text);
        if let Some(title) = &asset.title {
            asset.label = title
                .text
                .lines()
                .next()
                .unwrap_or("Caption")
                .chars()
                .take(52)
                .collect();
        }
        let mut clip = Clip::new(asset.id.clone(), cue.start, 0, cue.end - cue.start);
        clip.transform.y = -(lane as f32) * 0.10;
        first.get_or_insert_with(|| clip.id.clone());
        assets.push(asset);
        tracks[lane].clips.push(clip);
    }
    let first = first.context("The SRT contains no subtitles.")?;
    project.assets.extend(assets);
    project.tracks.extend(tracks);
    Ok(first)
}
