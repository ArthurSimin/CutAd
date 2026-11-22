use crate::theme::{ACCENT, ACCENT_DARK};
use cutad::fonts::FontRef;
use cutad::project::{TextAlign, TextBackground, TextOutline, TitleAppearance};
use eframe::egui::{self, RichText, vec2};

pub fn font_picker(ui: &mut egui::Ui, current: Option<&FontRef>) -> bool {
    ui.label(RichText::new("Font family").strong());
    let name = current.map_or("Preset font", |face| face.family.as_str());
    ui.add_sized(
        [ui.available_width(), 32.0],
        egui::Button::new(RichText::new(format!("{name}   ·   Choose font…")).color(ACCENT))
            .fill(ACCENT_DARK)
            .truncate(),
    )
    .on_hover_text("Choose an installed font or download from DaFont")
    .clicked()
}

pub fn show_controls(ui: &mut egui::Ui, appearance: &mut TitleAppearance) {
    ui.separator();
    egui::CollapsingHeader::new("Typography")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("title_typography")
                .num_columns(2)
                .spacing(vec2(10.0, 6.0))
                .show(ui, |ui| {
                    ui.label("Font size");
                    ui.add(
                        egui::DragValue::new(&mut appearance.size_percent)
                            .range(25..=300)
                            .suffix("%"),
                    );
                    ui.end_row();
                    ui.label("Alignment");
                    egui::ComboBox::from_id_salt("title_alignment")
                        .selected_text(match appearance.align {
                            None => "Preset",
                            Some(TextAlign::Left) => "Left",
                            Some(TextAlign::Center) => "Center",
                            Some(TextAlign::Right) => "Right",
                        })
                        .show_ui(ui, |ui| {
                            for (value, name) in [
                                (None, "Preset"),
                                (Some(TextAlign::Left), "Left"),
                                (Some(TextAlign::Center), "Center"),
                                (Some(TextAlign::Right), "Right"),
                            ] {
                                ui.selectable_value(&mut appearance.align, value, name);
                            }
                        });
                    ui.end_row();
                    ui.label("Line spacing");
                    ui.add(
                        egui::DragValue::new(&mut appearance.line_spacing)
                            .range(-50..=100)
                            .suffix("%"),
                    );
                    ui.end_row();
                });
            let mut fill = appearance.fill.is_some();
            if ui.checkbox(&mut fill, "Custom text color").changed() {
                appearance.fill = fill.then_some([255, 255, 255]);
            }
            if let Some(color) = &mut appearance.fill {
                ui.color_edit_button_srgb(color);
            }
            let mut outline = appearance.outline.is_some();
            if ui.checkbox(&mut outline, "Custom outline").changed() {
                appearance.outline = outline.then_some(TextOutline {
                    width: 8,
                    color: [13, 12, 19],
                });
            }
            if let Some(outline) = &mut appearance.outline {
                ui.horizontal(|ui| {
                    ui.label("Width");
                    ui.add(
                        egui::DragValue::new(&mut outline.width)
                            .range(0..=30)
                            .suffix("%"),
                    );
                    ui.color_edit_button_srgb(&mut outline.color);
                });
            }
            let mut background = appearance.background.is_some();
            if ui.checkbox(&mut background, "Background box").changed() {
                appearance.background = background.then_some(TextBackground {
                    opacity: 65,
                    color: [16, 15, 25],
                });
            }
            if let Some(background) = &mut appearance.background {
                ui.horizontal(|ui| {
                    ui.label("Box opacity");
                    ui.add(
                        egui::DragValue::new(&mut background.opacity)
                            .range(0..=100)
                            .suffix("%"),
                    );
                    ui.color_edit_button_srgb(&mut background.color);
                });
            }
            if ui.button("Reset typography").clicked() {
                *appearance = TitleAppearance::default();
            }
        });
}
