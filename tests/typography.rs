use anyhow::Result;
use cutad::fonts;
use cutad::media::{Format, monitor_frame, render};
use cutad::project::{Project, TextAlign, TextBackground, TextOutline, TitleStyle, load, save};
use std::fs;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use tempfile::tempdir;

#[test]
fn zip_import_copies_font_without_using_archive_paths() -> Result<()> {
    let test = tempdir()?;
    let source = test.path().join("typeface.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&source)?);
    writer.start_file("../../Bold.ttf", zip::write::SimpleFileOptions::default())?;
    writer.write_all(include_bytes!("../assets/DejaVuSans-Bold.ttf"))?;
    writer.finish()?;
    let target = test.path().join("fonts");
    let fonts = fonts::import_into(&source, &target)?;
    assert_eq!(fonts.len(), 1);
    assert_eq!(fonts[0].family, "DejaVu Sans");
    assert_eq!(fs::read_dir(target)?.count(), 1);
    assert!(!test.path().join("Bold.ttf").exists());
    Ok(())
}

#[test]
fn custom_type_persists_and_renders() -> Result<()> {
    let test = tempdir()?;
    let mut project = Project {
        width: 320,
        height: 180,
        ..Project::default()
    };
    project.add_title(TitleStyle::Impact, "Custom", 0)?;
    let title = project.assets[0].title.as_mut().unwrap();
    title.appearance.size_percent = 145;
    title.appearance.fill = Some([245, 60, 53]);
    title.appearance.align = Some(TextAlign::Left);
    title.appearance.outline = Some(TextOutline {
        width: 8,
        color: [225, 225, 225],
    });
    title.appearance.background = Some(TextBackground {
        opacity: 75,
        color: [200, 15, 15],
    });
    title.appearance.line_spacing = 14;
    let file = test.path().join("typography.cutad");
    save(&project, &file)?;
    assert_eq!(load(&file)?, project);
    let output = test.path().join("typography.mp4");
    render(
        &project,
        &output,
        Format::Preview,
        &AtomicBool::new(false),
        |_| {},
    )?;
    let rgb = monitor_frame(&output, 14, [320, 180])?;
    let colored = rgb
        .chunks_exact(3)
        .enumerate()
        .filter(|(_, p)| p[0] > 145 && p[1] < 105 && p[2] < 105);
    let positions: Vec<_> = colored.map(|(i, _)| i % 320).collect();
    assert!(positions.len() > 300);
    assert!(*positions.iter().min().unwrap() < 35);
    assert!(*positions.iter().max().unwrap() < 240);
    Ok(())
}

#[test]
fn editing_a_split_title_only_changes_the_selected_clip() -> Result<()> {
    let mut project = Project::default();
    let first = project.add_title(TitleStyle::Headline, "Original", 0)?;
    let second = project.split(&first, 30)?;
    let (row, index) = project.location(&second).unwrap();
    let current = &project.tracks[row].clips[index];
    let mut revised = project
        .asset(&current.asset_id)
        .unwrap()
        .title
        .clone()
        .unwrap();
    revised.text = "Changed".into();
    project.update_title(&second, revised)?;
    for (id, expected) in [(first, "Original"), (second, "Changed")] {
        let (row, index) = project.location(&id).unwrap();
        let clip = &project.tracks[row].clips[index];
        let title = project
            .asset(&clip.asset_id)
            .unwrap()
            .title
            .as_ref()
            .unwrap();
        assert_eq!(title.text, expected);
    }
    project.validate()?;
    Ok(())
}
