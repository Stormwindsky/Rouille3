//! Costume decoding (.png / .jpg / .svg) and GPU textures.
#![allow(dead_code)]
use crate::project::Project;
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::sync::Arc;

/// Size and rotation center of a costume, in Scratch stage units.
#[derive(Clone, Copy, Debug)]
pub struct CostumeMeta {
    pub w: f32,
    pub h: f32,
    pub rcx: f32,
    pub rcy: f32,
}

impl Default for CostumeMeta {
    fn default() -> Self { Self { w: 1.0, h: 1.0, rcx: 0.5, rcy: 0.5 } }
}

pub struct TexEntry {
    pub handle: TextureHandle,
    pub image: Arc<ColorImage>,
}

#[derive(Default)]
pub struct Gfx {
    pub tex: HashMap<String, TexEntry>,
    variants: HashMap<(String, i32, i32), TextureHandle>,
}

const SVG_RENDER_SCALE: f32 = 3.0;

/// Returns the image and its logical size (stage units at bitmapResolution 1).
pub fn decode_image(bytes: &[u8], format: &str) -> anyhow::Result<(ColorImage, f32, f32)> {
    if format.eq_ignore_ascii_case("svg") {
        use resvg::{tiny_skia, usvg};
        let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())?;
        let size = tree.size();
        let (w, h) = (size.width().max(1.0), size.height().max(1.0));
        let pw = ((w * SVG_RENDER_SCALE).ceil() as u32).clamp(1, 4096);
        let ph = ((h * SVG_RENDER_SCALE).ceil() as u32).clamp(1, 4096);
        let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| anyhow::anyhow!("pixmap"))?;
        let transform = tiny_skia::Transform::from_scale(pw as f32 / w, ph as f32 / h);
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        let img = ColorImage::from_rgba_premultiplied([pw as usize, ph as usize], pixmap.data());
        Ok((img, w, h))
    } else {
        let img = image::load_from_memory(bytes)?.to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        Ok((ColorImage::from_rgba_unmultiplied([w, h], &img), w as f32, h as f32))
    }
}

/// Builds textures for every costume of the project plus per-costume metadata.
pub fn build(
    ctx: &egui::Context,
    project: &Project,
    assets: &HashMap<String, Vec<u8>>,
) -> (Gfx, Vec<Vec<CostumeMeta>>) {
    let mut gfx = Gfx::default();
    let mut metas = Vec::new();
    for (ti, target) in project.targets.iter().enumerate() {
        let mut tm = Vec::new();
        for c in &target.costumes {
            let is_svg = c.data_format.eq_ignore_ascii_case("svg");
            let decoded = assets
                .get(&c.md5ext)
                .ok_or_else(|| anyhow::anyhow!("asset missing"))
                .and_then(|b| decode_image(b, &c.data_format));
            let (lw, lh) = match decoded {
                Ok((img, lw, lh)) => {
                    if !gfx.tex.contains_key(&c.md5ext) {
                        let handle = ctx.load_texture(
                            format!("{ti}:{}", c.md5ext), img.clone(), TextureOptions::LINEAR);
                        gfx.tex.insert(c.md5ext.clone(), TexEntry { handle, image: Arc::new(img) });
                    }
                    (lw, lh)
                }
                Err(e) => {
                    eprintln!("[assets] costume '{}' ({}): {e}", c.name, c.md5ext);
                    (1.0, 1.0)
                }
            };
            let br = if is_svg { 1.0 } else { c.bitmap_resolution.unwrap_or(1.0).max(0.01) as f32 };
            tm.push(CostumeMeta {
                w: lw / br,
                h: lh / br,
                rcx: c.rotation_center_x as f32 / br,
                rcy: c.rotation_center_y as f32 / br,
            });
        }
        metas.push(tm);
    }
    (gfx, metas)
}

fn apply_effects(src: &ColorImage, hue_deg: f32, bright: f32) -> ColorImage {
    let (s, c) = hue_deg.to_radians().sin_cos();
    let m = [
        [0.213 + c * 0.787 - s * 0.213, 0.715 - c * 0.715 - s * 0.715, 0.072 - c * 0.072 + s * 0.928],
        [0.213 - c * 0.213 + s * 0.143, 0.715 + c * 0.285 + s * 0.140, 0.072 - c * 0.072 - s * 0.283],
        [0.213 - c * 0.213 - s * 0.787, 0.715 - c * 0.715 + s * 0.715, 0.072 + c * 0.928 + s * 0.072],
    ];
    let add = bright * 2.55;
    let pixels = src
        .pixels
        .iter()
        .map(|p| {
            let [r, g, b, a] = p.to_srgba_unmultiplied();
            let (r, g, b) = (r as f32, g as f32, b as f32);
            let nr = (m[0][0] * r + m[0][1] * g + m[0][2] * b + add).clamp(0.0, 255.0);
            let ng = (m[1][0] * r + m[1][1] * g + m[1][2] * b + add).clamp(0.0, 255.0);
            let nb = (m[2][0] * r + m[2][1] * g + m[2][2] * b + add).clamp(0.0, 255.0);
            Color32::from_rgba_unmultiplied(nr as u8, ng as u8, nb as u8, a)
        })
        .collect();
    ColorImage { size: src.size, pixels }
}

impl Gfx {
    /// Texture for a costume with the Scratch COLOR / BRIGHTNESS effects applied
    /// (quantised so the cache stays small).
    pub fn texture_for(
        &mut self,
        ctx: &egui::Context,
        md5: &str,
        color_effect: f64,
        brightness: f64,
    ) -> Option<egui::TextureId> {
        let entry = self.tex.get(md5)?;
        let hue_q = (((color_effect * 1.8) / 6.0).round() as i32).rem_euclid(60); // 6° steps
        let br_q = ((brightness / 5.0).round() as i32).clamp(-20, 20);
        if hue_q == 0 && br_q == 0 {
            return Some(entry.handle.id());
        }
        let key = (md5.to_string(), hue_q, br_q);
        if !self.variants.contains_key(&key) {
            if self.variants.len() > 400 {
                self.variants.clear();
            }
            let img = apply_effects(&entry.image, hue_q as f32 * 6.0, br_q as f32 * 5.0);
            let h = ctx.load_texture(format!("fx:{md5}:{hue_q}:{br_q}"), img, TextureOptions::LINEAR);
            self.variants.insert(key.clone(), h);
        }
        self.variants.get(&key).map(|h| h.id())
    }
}
