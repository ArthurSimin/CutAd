use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, vec2};

pub const BACK: Color32 = Color32::from_rgb(13, 16, 19);
pub const PANEL: Color32 = Color32::from_rgb(23, 27, 32);
pub const LINE: Color32 = Color32::from_rgb(48, 54, 61);
pub const TEXT: Color32 = Color32::from_rgb(240, 244, 247);
pub const MUTED: Color32 = Color32::from_rgb(156, 170, 181);
pub const ACCENT: Color32 = Color32::from_rgb(147, 234, 209);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(31, 68, 61);
pub const LIME: Color32 = Color32::from_rgb(221, 248, 137);

pub fn panel() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(9)
        .inner_margin(11)
        .outer_margin(egui::Margin::same(5))
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.animation_time = 0.14;
    style.spacing.item_spacing = vec2(8.0, 7.0);
    style.spacing.button_padding = vec2(9.0, 4.0);
    style.spacing.interact_size.y = 26.0;
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(17.0));

    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.window_corner_radius = CornerRadius::same(15);
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.extreme_bg_color = BACK;
    visuals.faint_bg_color = BACK;
    visuals.selection.bg_fill = ACCENT_DARK;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    for states in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        states.corner_radius = CornerRadius::same(6);
    }
    visuals.widgets.inactive.bg_fill = LINE;
    visuals.widgets.inactive.weak_bg_fill = LINE;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(53, 61, 69);
    visuals.widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals = visuals;
    ctx.set_style_of(egui::Theme::Dark, style);
}

pub fn set_reduced_motion(ctx: &egui::Context, reduced: bool) {
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.animation_time = if reduced { 0.0 } else { 0.14 };
    ctx.set_style_of(egui::Theme::Dark, style);
}
