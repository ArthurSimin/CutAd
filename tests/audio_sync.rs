use anyhow::Result;
use cutad::media::{audio_sync_offsets, probe};
use cutad::project::{Clip, FPS, Project, Track, TrackKind};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn write_recording(path: &Path, levels: &[i16], polarity: i16) -> Result<()> {
    const RATE: u32 = 48_000;
    let samples_per_tick = RATE / FPS;
    let size = levels.len() as u32 * samples_per_tick * 2;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(size + 36).to_le_bytes())?;
    file.write_all(b"WAVEfmt \x10\0\0\0\x01\0\x01\0")?;
    file.write_all(&RATE.to_le_bytes())?;
    file.write_all(&(RATE * 2).to_le_bytes())?;
    file.write_all(b"\x02\0\x10\0data")?;
    file.write_all(&size.to_le_bytes())?;
    for &level in levels {
        for sample in 0..samples_per_tick {
            let sign = if sample % 160 < 80 {
                polarity
            } else {
                -polarity
            };
            let value = level * sign;
            file.write_all(&value.to_le_bytes())?;
        }
    }
    Ok(file.flush()?)
}

#[test]
fn audio_sync_matches_trimmed_recordings_and_moves_clips_atomically() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut seed = 42u32;
    let levels: Vec<i16> = (0..300)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (500 + (seed >> 20)) as i16
        })
        .collect();
    let a = directory.path().join("reference.wav");
    let b = directory.path().join("second.wav");
    let c = directory.path().join("early.wav");
    write_recording(&a, &levels, 1)?;
    write_recording(
        &b,
        &levels[51..]
            .iter()
            .map(|level| level / 2)
            .collect::<Vec<_>>(),
        -1,
    )?;
    let mut early = vec![0; 23];
    early.extend(&levels);
    write_recording(&c, &early, 1)?;
    let first = probe(&a)?;
    let second = probe(&b)?;
    let third = probe(&c)?;
    let anchor = Clip::new(first.id.clone(), 120, 8, 240);
    let follower = Clip::new(second.id.clone(), 35, 3, 200);
    let earlier = Clip::new(third.id.clone(), 65, 0, 240);
    let clips = vec![
        (anchor.clone(), first.clone()),
        (follower.clone(), second.clone()),
        (earlier.clone(), third.clone()),
    ];
    let offsets = audio_sync_offsets(&clips, &AtomicBool::new(false), |_| {})?;
    assert_eq!(offsets, [0, 46, -31]);
    let mut project = Project {
        assets: vec![first, second, third],
        tracks: clips
            .iter()
            .map(|(clip, asset)| Track {
                clips: vec![clip.clone()],
                ..Track::new(&asset.label, TrackKind::Audio)
            })
            .collect(),
        ..Project::default()
    };
    let placements: Vec<_> = clips
        .iter()
        .zip(offsets)
        .map(|((clip, _), offset)| (clip.id.as_str(), offset))
        .collect();
    let blocked = Clip::new(follower.asset_id.clone(), 290, 0, 20);
    project.tracks[1].clips.push(blocked);
    let original = project.clone();
    assert!(project.align_clip_starts(&placements).is_err());
    assert_eq!(project, original);
    project.tracks[1].clips.pop();
    project.align_clip_starts(&placements)?;
    project.validate()?;
    assert_eq!(project.tracks[0].clips[0], anchor);
    assert_eq!(project.tracks[1].clips[0].start, 166);
    assert_eq!(project.tracks[1].clips[0].source, 3);
    assert_eq!(project.tracks[2].clips[0].start, 89);
    assert!(audio_sync_offsets(&clips, &AtomicBool::new(true), |_| {}).is_err());
    Ok(())
}
