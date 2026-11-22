use crate::theme::{ACCENT, BACK, LINE, MUTED};
use cutad::{
    media::{Format, Output, Quality},
    project::{Project, timestamp},
};
use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke, StrokeKind, vec2};
use std::path::PathBuf;

pub enum Decision {
    Cancel,
    Render(PathBuf, Output),
}

pub struct ExportDialog {
    settings: Output,
    destination: Option<PathBuf>,
}

impl ExportDialog {
    pub fn new(project: &Project) -> Self {
        Self {
            settings: Output::for_project(Format::Mp4, project),
            destination: None,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        texture: Option<&egui::TextureHandle>,
    ) -> Option<Decision> {
        let mut decision = None;
        let response = egui::Modal::new(egui::Id::new("render_export")).show(ctx, |ui| {
            ui.set_min_width(520.0);
            ui.heading("Export edit");
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(190.0, 142.0), Sense::hover());
                ui.painter()
                    .rect(rect, 8, BACK, Stroke::new(1.0, LINE), StrokeKind::Inside);
                if let Some(texture) = texture {
                    let size = texture.size_vec2();
                    let scale = (182.0 / size.x).min(134.0 / size.y);
                    ui.painter().image(
                        texture.id(),
                        egui::Rect::from_center_size(rect.center(), size * scale),
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &project.name,
                        egui::FontId::proportional(14.0),
                        MUTED,
                    );
                }
                ui.add_space(8.0);
                egui::Grid::new("export_options")
                    .spacing(vec2(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.label("Format");
                        let previous = self.settings.format;
                        egui::ComboBox::from_id_salt("format")
                            .width(156.0)
                            .selected_text(format_name(self.settings.format))
                            .show_ui(ui, |ui| {
                                for format in [Format::Mp4, Format::Webm, Format::Mkv] {
                                    ui.selectable_value(
                                        &mut self.settings.format,
                                        format,
                                        format_name(format),
                                    );
                                }
                            });
                        if previous != self.settings.format {
                            self.destination = None;
                        }
                        ui.end_row();
                        ui.label("Resolution");
                        let [width, height] = self.settings.size(project);
                        egui::ComboBox::from_id_salt("size")
                            .width(156.0)
                            .selected_text(format!("{width} × {height}"))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.settings.short_edge,
                                    None,
                                    "Canvas size",
                                );
                                for (label, edge) in [
                                    ("Up to 1080p", 1080),
                                    ("Up to 720p", 720),
                                    ("Up to 480p", 480),
                                ] {
                                    ui.selectable_value(
                                        &mut self.settings.short_edge,
                                        Some(edge),
                                        label,
                                    );
                                }
                            });
                        ui.end_row();
                        ui.label("Frame rate");
                        egui::ComboBox::from_id_salt("fps")
                            .width(156.0)
                            .selected_text(format!("{} fps", self.settings.fps))
                            .show_ui(ui, |ui| {
                                for rate in [24, 30, 60, 120] {
                                    ui.selectable_value(
                                        &mut self.settings.fps,
                                        rate,
                                        format!("{rate} fps"),
                                    );
                                }
                            });
                        ui.end_row();
                        ui.label("Quality");
                        if self.settings.format == Format::Mkv {
                            ui.label("Lossless video and audio");
                        } else {
                            egui::ComboBox::from_id_salt("quality")
                                .width(156.0)
                                .selected_text(quality_name(self.settings.quality))
                                .show_ui(ui, |ui| {
                                    for quality in
                                        [Quality::High, Quality::Balanced, Quality::Compact]
                                    {
                                        ui.selectable_value(
                                            &mut self.settings.quality,
                                            quality,
                                            quality_name(quality),
                                        );
                                    }
                                });
                        }
                        ui.end_row();
                    });
            });
            if self.settings.format == Format::Mkv {
                ui.label(
                    RichText::new("FFV1 and FLAC preserve the rendered edit. Files can be large.")
                        .color(MUTED),
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let extension = self.settings.format.extension();
                if ui.button("Choose output file…").clicked() {
                    self.destination = rfd::FileDialog::new()
                        .set_title("Save video")
                        .set_file_name(format!("CutAd export.{extension}"))
                        .add_filter(format_name(self.settings.format), &[extension])
                        .save_file()
                        .map(|mut file| {
                            if !file
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
                            {
                                file.set_extension(extension);
                            }
                            file
                        });
                }
                if let Some(dest) = &self.destination {
                    ui.add(egui::Label::new(dest.display().to_string()).truncate());
                } else {
                    ui.label(RichText::new("Choose where to save the video").color(MUTED));
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Duration  {}", timestamp(project.duration())))
                        .monospace()
                        .color(MUTED),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let export =
                        egui::Button::new(RichText::new("Export video").strong().color(BACK))
                            .fill(ACCENT);
                    if ui.add_enabled(self.destination.is_some(), export).clicked()
                        && let Some(path) = self.destination.clone()
                    {
                        decision = Some(Decision::Render(path, self.settings));
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(Decision::Cancel);
                    }
                });
            });
        });
        if response.should_close() {
            Some(Decision::Cancel)
        } else {
            decision
        }
    }
}

fn format_name(format: Format) -> &'static str {
    match format {
        Format::Mp4 | Format::Preview => "MP4 · H.264",
        Format::Webm => "WebM · VP9",
        Format::Mkv => "MKV · Lossless",
    }
}

fn quality_name(quality: Quality) -> &'static str {
    match quality {
        Quality::Compact => "Smaller file",
        Quality::Balanced => "Balanced",
        Quality::High => "High quality",
    }
}
