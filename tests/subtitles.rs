use anyhow::Result;
use cutad::{
    project::{Asset, Clip, Project, TitleStyle, Track, TrackKind, load, save},
    subtitles::{add_tracks, caption_clips, parse, read, write, write_transcript},
};

#[test]
fn overlapping_srt_entries_become_editable_and_keep_their_timing() -> Result<()> {
    let source = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,000\r\nHello\r\nworld\r\n\r\n2\r\n00:00:02,500 --> 00:00:04,000\r\nOlá";
    let captions = parse(source)?;
    let mut project = Project::default();
    let strip = add_tracks(&mut project, captions)?;
    let (row, column) = project.location(&strip).unwrap();
    let first = &project.tracks[row].clips[column];
    assert_eq!((first.start, first.length), (30, 60));
    assert_eq!(project.tracks[row + 1].clips[0].start, 75);
    let title = project
        .asset(&first.asset_id)
        .unwrap()
        .title
        .as_ref()
        .unwrap();
    assert_eq!(title.text, "Hello\nworld");
    assert_eq!(title.style, TitleStyle::Caption);
    let folder = tempfile::tempdir()?;
    let file = folder.path().join("captions.cutad");
    save(&project, &file)?;
    assert_eq!(load(&file)?, project);
    assert!(parse("1\n00:00:05,000 --> 00:00:01,000\nReversed").is_err());
    Ok(())
}

#[test]
fn exports_unmuted_caption_clips_in_timeline_order_and_retains_frame_boundaries() -> Result<()> {
    let mut project = Project::default();
    let title = Asset::text(TitleStyle::Headline, "Leave me out");
    let first = Asset::text(TitleStyle::Caption, "Olá\r\n\r\nworld");
    let second = Asset::text(TitleStyle::Caption, "One more line");
    let hidden = Asset::text(TitleStyle::Caption, "Hidden track");
    let mut captions = Track::new("Renamed captions", TrackKind::Video);
    captions.clips.push(Clip::new(second.id.clone(), 3, 0, 2));
    let mut overlays = Track::new("Overlays", TrackKind::Video);
    overlays.clips.push(Clip::new(title.id.clone(), 0, 0, 1));
    overlays.clips.push(Clip::new(first.id.clone(), 1, 0, 3));
    let mut muted = Track::new("Captions", TrackKind::Video);
    muted.muted = true;
    muted.clips.push(Clip::new(hidden.id.clone(), 0, 0, 6));
    project.assets.extend([title, first, second, hidden]);
    project.tracks.extend([captions, overlays, muted]);

    let first_match = caption_clips(&project, "  OLÁ  ");
    assert_eq!(first_match.len(), 1);
    assert_eq!(first_match[0].0.start, 1);
    assert_eq!(first_match[0].1, "Olá\r\n\r\nworld");
    assert!(caption_clips(&project, "Leave me out").is_empty());
    assert!(caption_clips(&project, "Hidden track").is_empty());

    let folder = tempfile::tempdir()?;
    let path = folder.path().join("captions.srt");
    assert_eq!(write(&project, &path)?, 2);
    let transcript = folder.path().join("transcript.txt");
    assert_eq!(write_transcript(&project, &transcript)?, 2);
    assert_eq!(
        std::fs::read_to_string(transcript)?,
        "Olá world\nOne more line\n"
    );
    assert_eq!(
        std::fs::read_to_string(&path)?,
        "1\n00:00:00,033 --> 00:00:00,133\nOlá\nworld\n\n2\n00:00:00,100 --> 00:00:00,166\nOne more line\n\n"
    );

    let mut reimported = Project::default();
    add_tracks(&mut reimported, read(&path)?)?;
    let first = &reimported.tracks[2].clips[0];
    let second = &reimported.tracks[3].clips[0];
    assert_eq!((first.start, first.end()), (1, 4));
    assert_eq!((second.start, second.end()), (3, 5));
    Ok(())
}

#[test]
fn subtitle_export_preserves_source_and_existing_destination_on_error() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let original = folder.path().join("source.wav");
    let destination = folder.path().join("existing.srt");
    std::fs::write(&original, "source media")?;
    std::fs::write(&destination, "previous captions")?;
    let mut project = Project::default();
    project.assets.push(Asset::video(&original, 30, true));
    assert!(write(&project, &destination).is_err());
    assert_eq!(std::fs::read_to_string(&destination)?, "previous captions");

    add_tracks(
        &mut project,
        parse("1\n00:00:00,000 --> 00:00:01,000\nHello")?,
    )?;
    assert!(write(&project, &original).is_err());
    assert_eq!(std::fs::read_to_string(&original)?, "source media");
    assert_eq!(write(&project, &destination)?, 1);
    assert!(std::fs::read_to_string(&destination)?.contains("Hello"));
    Ok(())
}
