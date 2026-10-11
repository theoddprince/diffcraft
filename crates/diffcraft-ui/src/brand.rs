//! The DiffCraft logo mark (split "D") and wordmark, embedded in the binary.

use std::sync::Arc;

use egui::{ColorImage, Rect, TextureHandle, TextureOptions, Ui, vec2};

const ICON_256: &[u8] = include_bytes!("../../../assets/brand/diffcraft-icon-256.png");
const ICON_64: &[u8] = include_bytes!("../../../assets/brand/diffcraft-icon-64.png");
const WORDMARK_ON_LIGHT: &[u8] = include_bytes!("../../../assets/brand/diffcraft-wordmark-on-light-120.png");
const WORDMARK_ON_DARK: &[u8] = include_bytes!("../../../assets/brand/diffcraft-wordmark-on-dark-120.png");

fn decode(bytes: &[u8]) -> Option<::image::RgbaImage> {
    ::image::load_from_memory_with_format(bytes, ::image::ImageFormat::Png).ok().map(|i| i.to_rgba8())
}

/// The window / taskbar icon.
pub fn window_icon() -> Option<egui::IconData> {
    let img = decode(ICON_256)?;
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

/// Uploaded logo textures (created once per context).
pub struct Textures {
    pub mark: TextureHandle,
    pub wordmark_on_light: TextureHandle,
    pub wordmark_on_dark: TextureHandle,
}

fn texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let img = decode(bytes)?;
    let size = [img.width() as usize, img.height() as usize];
    let pixels = img.as_raw();
    // Checked so a mismatch can never reach egui's size assertion.
    if pixels.len() != size[0].saturating_mul(size[1]).saturating_mul(4) {
        return None;
    }
    let color = ColorImage::from_rgba_unmultiplied(size, pixels);
    Some(ctx.load_texture(name, color, TextureOptions::LINEAR))
}

/// The logo textures, loading them on first use.
pub fn textures(ctx: &egui::Context) -> Option<Arc<Textures>> {
    let id = egui::Id::new("diffcraft-brand-textures");
    if let Some(t) = ctx.data(|d| d.get_temp::<Arc<Textures>>(id)) {
        return Some(t);
    }
    let t = Arc::new(Textures {
        mark: texture(ctx, "brand-mark", ICON_64)?,
        wordmark_on_light: texture(ctx, "brand-wordmark-light", WORDMARK_ON_LIGHT)?,
        wordmark_on_dark: texture(ctx, "brand-wordmark-dark", WORDMARK_ON_DARK)?,
    });
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

/// Draw the wordmark `height` points tall, in the variant that suits the current theme.
pub fn wordmark(ui: &mut Ui, height: f32) {
    let Some(t) = textures(ui.ctx()) else {
        ui.heading("DiffCraft");
        return;
    };
    let tex = if ui.visuals().dark_mode { &t.wordmark_on_dark } else { &t.wordmark_on_light };
    let s = tex.size_vec2();
    let w = if s.y > 0.0 { s.x * height / s.y } else { height * 3.0 };
    ui.add(egui::Image::new((tex.id(), vec2(w, height))));
}

/// Paint the logo mark into `rect`.
pub fn paint_mark(ui: &Ui, rect: Rect) {
    if let Some(t) = textures(ui.ctx()) {
        ui.painter().image(t.mark.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_decode() {
        let icon = window_icon().unwrap();
        assert_eq!((icon.width, icon.height), (256, 256));
        for b in [ICON_64, WORDMARK_ON_LIGHT, WORDMARK_ON_DARK] {
            let img = decode(b).unwrap();
            assert!(img.width() > 0 && img.height() > 0);
        }
        // The icon has transparent corners.
        let img = decode(ICON_256).unwrap();
        assert_eq!(img.get_pixel(0, 0).0[3], 0);
    }
}
