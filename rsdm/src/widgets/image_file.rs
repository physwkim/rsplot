//! `RsdmImage` — a static image *file* displayed in a panel (MEDM `image`).
//!
//! MEDM's `image` widget shows a GIF/TIFF file from disk (the `image name`),
//! with no channel — distinct from [`crate::widgets::image_view::RsdmImageView`],
//! which renders a live array-data PV. This widget decodes the file once (via the
//! `image` crate, which is already in the build graph through the vendored egui),
//! uploads it to an egui texture, and paints it scaled to the widget bounds. It
//! holds no [`crate::engine::Engine`] connection.
//!
//! The decode step ([`decode_color_image`]) is pure and unit-tested; the texture
//! upload and paint run on the GUI thread. A file that is missing or cannot be
//! decoded at run time draws a labelled placeholder instead of panicking — there
//! is no build-time guarantee the path resolves on the running host.

use std::path::{Path, PathBuf};

use rsplot::egui::{self, Color32, TextureHandle, TextureOptions, Vec2};

/// Default footprint when neither an explicit size nor a decoded texture gives
/// one (points).
const DEFAULT_SIZE: Vec2 = Vec2::new(40.0, 40.0);

/// A static image file rendered into the UI (MEDM `image`).
pub struct RsdmImage {
    path: PathBuf,
    /// Explicit draw size (MEDM geometry); `None` falls back to the texture's
    /// native pixel size.
    size: Option<Vec2>,
    /// The uploaded texture, decoded lazily on first [`show`](Self::show).
    texture: Option<TextureHandle>,
    /// Set once decoding fails so the file is not re-read every frame.
    failed: bool,
}

impl RsdmImage {
    /// A static image loaded from `path` (the MEDM `image name`). The file is
    /// resolved relative to the running app's working directory and decoded on
    /// the first frame.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            size: None,
            texture: None,
            failed: false,
        }
    }

    /// Draw the image at `size` points (builder style; MEDM scales the file to
    /// the widget geometry). Without it the texture's native pixel size is used.
    pub fn with_size(mut self, size: Vec2) -> Self {
        self.size = Some(size);
        self
    }

    /// The image file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Decode + upload the texture on first use; a decode failure is latched.
    fn ensure_texture(&mut self, ui: &egui::Ui) {
        if self.texture.is_some() || self.failed {
            return;
        }
        match load_color_image(&self.path) {
            Ok(image) => {
                self.texture = Some(ui.ctx().load_texture(
                    self.path.to_string_lossy(),
                    image,
                    TextureOptions::LINEAR,
                ));
            }
            Err(_) => self.failed = true,
        }
    }

    /// Render the image this frame, returning the widget response.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        self.ensure_texture(ui);
        let size = self.size.unwrap_or_else(|| {
            self.texture
                .as_ref()
                .map(TextureHandle::size_vec2)
                .unwrap_or(DEFAULT_SIZE)
        });
        let size = crate::widgets::base::justified_size(
            crate::widgets::base::layout_justify(ui),
            ui,
            size,
        );
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
        if !ui.is_rect_visible(rect) {
            return response;
        }
        match &self.texture {
            Some(texture) => {
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
            }
            None => self.paint_missing(ui, rect),
        }
        response
    }

    /// Draw a bordered placeholder naming the file when it could not be loaded.
    fn paint_missing(&self, ui: &egui::Ui, rect: egui::Rect) {
        let painter = ui.painter();
        painter.rect_stroke(
            rect,
            egui::CornerRadius::ZERO,
            egui::Stroke::new(1.0_f32, Color32::from_rgb(180, 60, 60)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("[image: {}]", self.path.display()),
            egui::FontId::proportional(11.0),
            Color32::from_rgb(180, 60, 60),
        );
    }
}

/// Read and decode an image file to an egui [`ColorImage`](egui::ColorImage).
fn load_color_image(path: &Path) -> Result<egui::ColorImage, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    decode_color_image(&bytes)
}

/// Decode encoded image bytes (GIF/PNG/TIFF) to an RGBA [`ColorImage`](egui::ColorImage).
/// Pure: the format is detected from the bytes' magic number.
pub fn decode_color_image(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let decoded = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let rgba = decoded.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a tiny solid-colour PNG with the `image` crate, then decode it back
    /// through the widget's path — exercises the real decoder without a fixture
    /// file on disk.
    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let buf = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = std::io::Cursor::new(Vec::new());
        buf.write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    #[test]
    fn decode_yields_the_right_dimensions_and_pixels() {
        let bytes = png_bytes(3, 2, [10, 20, 30, 255]);
        let image = decode_color_image(&bytes).expect("decode");
        assert_eq!(image.size, [3, 2]);
        // Every pixel is the solid colour it was encoded with.
        assert_eq!(image.pixels[0], Color32::from_rgb(10, 20, 30));
        assert_eq!(image.pixels.len(), 6);
    }

    #[test]
    fn decode_rejects_non_image_bytes() {
        assert!(decode_color_image(b"not an image").is_err());
    }

    #[test]
    fn missing_file_latches_failed_without_panicking() {
        // A path that does not resolve must not panic on load — it latches.
        let mut img = RsdmImage::new("/no/such/file/at/all.gif");
        assert!(load_color_image(img.path()).is_err());
        img.failed = true;
        assert!(img.texture.is_none());
    }
}
