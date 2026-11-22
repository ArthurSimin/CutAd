use eframe::egui::{self, ColorImage, Context, TextureHandle, TextureOptions};

const LOGO: &[u8] = include_bytes!("../assets/logo.png");

pub fn icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(LOGO).expect("embedded CutAd icon")
}

pub fn logo(ctx: &Context) -> TextureHandle {
    let icon = icon();
    ctx.load_texture(
        "cutad-logo",
        ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba),
        TextureOptions::LINEAR,
    )
}
