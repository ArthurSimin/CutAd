use crate::theme::{ACCENT, BACK, LINE, MUTED, TEXT};
use cutad::effects::{Category, Preset};
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

#[derive(Default)]
pub struct Gallery {
    query: String,
    category: Option<Category>,
}

impl Gallery {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected: bool,
        only: Option<Category>,
    ) -> Option<Preset> {
        ui.heading(match only {
            Some(Category::Color) => "Color filters",
            Some(Category::Audio) => "Audio processing",
            _ => "Effects",
        });
        ui.add_sized(
            [ui.available_width(), 29.0],
            egui::TextEdit::singleline(&mut self.query).hint_text("Search effects"),
        );
        if only.is_none() {
            egui::ComboBox::from_id_salt("effect_category")
                .width(ui.available_width() - 14.0)
                .selected_text(self.category.map_or("All categories", Category::name))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.category, None, "All categories");
                    for kind in Category::ALL {
                        ui.selectable_value(&mut self.category, Some(kind), kind.name());
                    }
                });
        }
        ui.label(
            egui::RichText::new(if selected {
                "Click a preset to apply it."
            } else {
                "Select a clip to apply effects."
            })
            .color(MUTED)
            .small(),
        );
        let query = self.query.trim().to_lowercase();
        let presets: Vec<_> = Preset::ALL
            .iter()
            .copied()
            .filter(|preset| {
                only.or(self.category)
                    .is_none_or(|category| preset.category() == category)
                    && (query.is_empty()
                        || [
                            preset.name(),
                            preset.description(),
                            preset.category().name(),
                        ]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&query)))
            })
            .collect();
        if presets.is_empty() {
            ui.label("No matching presets.");
            return None;
        }
        let mut applied = None;
        egui::ScrollArea::vertical()
            .id_salt("effects_library")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let columns = if ui.available_width() >= 200.0 { 2 } else { 1 };
                let gap = ui.spacing().item_spacing.x;
                let width = ((ui.available_width() - gap * (columns - 1) as f32 - 7.0)
                    / columns as f32)
                    .max(80.0);
                for row in presets.chunks(columns) {
                    ui.horizontal(|ui| {
                        for &preset in row {
                            if card(ui, preset, width).clicked() {
                                applied = Some(preset);
                            }
                        }
                    });
                }
            });
        applied
    }
}

fn card(ui: &mut egui::Ui, preset: Preset, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 91.0), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, preset.name()));
    let painter = ui.painter();
    let border = if response.hovered() || response.has_focus() {
        ACCENT
    } else {
        LINE
    };
    painter.rect(rect, 7, BACK, Stroke::new(1.0, border), StrokeKind::Inside);
    let art = Rect::from_min_max(
        rect.min + vec2(5.0, 5.0),
        pos2(rect.right() - 5.0, rect.top() + 60.0),
    );
    let layer = painter.with_clip_rect(art);
    let index = Preset::ALL
        .iter()
        .position(|item| *item == preset)
        .unwrap_or_default();
    let (mid, bright) = match preset.category() {
        Category::Color => (
            Color32::from_rgb(89, 50, 109),
            Color32::from_rgb(236, 139, 138),
        ),
        Category::Lens => (
            Color32::from_rgb(45, 69, 134),
            Color32::from_rgb(119, 191, 237),
        ),
        Category::Motion => (
            Color32::from_rgb(101, 66, 117),
            Color32::from_rgb(202, 172, 254),
        ),
        Category::Texture => (
            Color32::from_rgb(113, 57, 80),
            Color32::from_rgb(242, 162, 109),
        ),
        Category::Audio => (
            Color32::from_rgb(33, 95, 102),
            Color32::from_rgb(128, 225, 207),
        ),
    };
    layer.rect_filled(art, 4, mid.gamma_multiply(0.65));
    let cx = art.left() + art.width() * (0.39 + 0.06 * (index % 5) as f32);
    let cy = art.center().y + (index % 3) as f32 * 3.0 - 3.0;
    match preset.category() {
        Category::Color => {
            for offset in 0..4 {
                let r = 12.0 + offset as f32 * 3.5;
                layer.circle_filled(
                    pos2(cx - offset as f32 * 12.0 + 15.0, cy),
                    r,
                    bright.gamma_multiply(1.0 - offset as f32 * 0.19),
                );
            }
        }
        Category::Lens => {
            layer.circle_filled(pos2(cx, cy), 20.0, mid);
            for r in [8.0, 15.0, 23.0, 35.0] {
                layer.circle_stroke(
                    pos2(cx, cy),
                    r,
                    Stroke::new(1.5, bright.gamma_multiply((37.0 - r) / 40.0 + 0.25)),
                );
            }
            layer.circle_filled(pos2(cx + 3.0, cy - 4.0), 3.0, bright);
        }
        Category::Motion => {
            for i in 0..5 {
                let x = art.left() + i as f32 * art.width() / 4.0;
                let y = art.bottom() - i as f32 * 9.0;
                layer.line_segment(
                    [pos2(x - 20.0, y + 19.0), pos2(x + 26.0, y - 14.0)],
                    Stroke::new(
                        1.5 + i as f32,
                        bright.gamma_multiply(0.25 + i as f32 * 0.14),
                    ),
                );
            }
        }
        Category::Texture => {
            let size = 8.0 + (index % 4) as f32 * 2.0;
            for row in 0..7 {
                for col in 0..16 {
                    let level = (row * 19 + col * 37 + index * 11) % 33;
                    let point = art.min + vec2(col as f32 * size, row as f32 * size);
                    layer.rect_filled(
                        Rect::from_min_size(point, vec2(size - 1.0, size - 1.0)),
                        0,
                        bright.gamma_multiply(0.13 + level as f32 / 45.0),
                    );
                }
            }
        }
        Category::Audio => {
            for i in 0..14 {
                let x = art.left() + (i + 1) as f32 * art.width() / 15.0;
                let h = 3.0 + ((i * 13 + index * 7) % 18) as f32;
                layer.line_segment([pos2(x, cy - h), pos2(x, cy + h)], Stroke::new(2.3, bright));
            }
        }
    }
    painter.with_clip_rect(rect.shrink2(vec2(6.0, 0.0))).text(
        pos2(rect.left() + 8.0, rect.bottom() - 15.0),
        Align2::LEFT_CENTER,
        preset.name(),
        FontId::proportional(12.0),
        TEXT,
    );
    response.on_hover_text(preset.description())
}
