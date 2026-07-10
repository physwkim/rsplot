//! Print-preview page editor (silx `PrintPreviewDialog` /
//! `PrintPreviewToolButton`).
//!
//! silx's print preview lets the user drop the plot onto a printable page and
//! interactively MOVE and RESIZE it (a `QGraphicsScene` holding a movable,
//! corner-resizable item) before printing. [`PrintPreview`] is the rsplot
//! analogue: a detachable window showing a white page on a gray backdrop, onto
//! which a plot snapshot (any RGBA8 image, e.g. from
//! [`crate::render::save::render_plot_rgba`]) is placed via
//! [`PrintPreview::add_image`]. Each item can be dragged to reposition and
//! resized from its bottom-right handle (keep-aspect by default, silx
//! `_GraphicsResizeRectItem`); a toolbar offers Clear All, Remove,
//! Zoom +/- (silx `_zoomPlus`/`_zoomMinus`), a per-item keep-aspect toggle, and
//! Save Page.
//!
//! Deviation: silx's final step renders the scene to a `QPrinter` (a native
//! print dialog + device). That OS print path is not portable, so the no-dep
//! analogue is **Save Page** — compose the arranged page to a PNG
//! ([`compose_page`]), mirroring silx's own print-to-file (`setOutputFileName`)
//! path. System-printer submission stays with the separate
//! [`crate::widget::print_dialog::PrintDialog`] (silx `PrintAction`).

use egui::{Color32, Pos2, Rect, Vec2, pos2, vec2};

use crate::widget::detached::{DetachedWindow, show_detached};

/// Comment-text alignment under a page item (silx `commentPosition`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CommentPosition {
    /// Left-aligned under the item.
    Left,
    /// Centered under the item (silx default).
    #[default]
    Center,
    /// Right-aligned under the item.
    Right,
}

/// Minimum on-page item size (page units) a resize may shrink an item to.
const MIN_ITEM_SIZE: f32 = 16.0;
/// Side length of the bottom-right resize handle, in page units (silx 40px).
const RESIZE_HANDLE: f32 = 40.0;
/// A4 portrait at 96 dpi — the default page (silx uses the live `QPrinter` page).
const DEFAULT_PAGE: Vec2 = vec2(794.0, 1123.0);

/// Resize `rect` by a bottom-right-handle drag of `delta` (page units), keeping
/// the top-left corner fixed. Faithful to silx `_GraphicsResizeRectItem`: with
/// `keep_ratio` the smaller of the x/y growth ratios drives a uniform scale (so
/// the item follows whichever axis grew least, preserving the aspect ratio);
/// otherwise each axis scales independently. Clamped to a `16.0`-unit minimum.
pub fn resize_item_rect(rect: Rect, delta: Vec2, keep_ratio: bool) -> Rect {
    let w = rect.width();
    let h = rect.height();
    let (nw, nh) = if keep_ratio && w > 0.0 && h > 0.0 {
        let ratio = w / h;
        let r1 = (w + delta.x) / w;
        let r2 = (h + delta.y) / h;
        if r1 < r2 {
            let nw = w + delta.x;
            (nw, nw / ratio)
        } else {
            let nh = h + delta.y;
            (nh * ratio, nh)
        }
    } else {
        (w + delta.x, h + delta.y)
    };
    Rect::from_min_size(rect.min, vec2(nw.max(MIN_ITEM_SIZE), nh.max(MIN_ITEM_SIZE)))
}

/// Place a freshly added image of pixel size `img` onto a `page` at silx's
/// default print geometry (xOffset/yOffset 0.1, width/height 0.9, keep-aspect):
/// the item fills 90% of the page width, its height follows the aspect ratio,
/// clamped to 90% of the page height (silx
/// `PrintPreviewToolButton._getViewBox`).
fn default_placement(page: Vec2, img: Vec2) -> Rect {
    let x = 0.1 * page.x;
    let y = 0.1 * page.y;
    let avail_w = 0.9 * page.x;
    let avail_h = 0.9 * page.y;
    let aspect = if img.x > 0.0 { img.y / img.x } else { 1.0 };
    let mut body_w = avail_w;
    let mut body_h = body_w * aspect;
    if body_h > avail_h {
        body_h = avail_h;
        body_w = if aspect > 0.0 {
            body_h / aspect
        } else {
            avail_w
        };
    }
    Rect::from_min_size(pos2(x, y), vec2(body_w, body_h))
}

/// One item to composite onto the page: its destination rectangle in page
/// pixels and its source RGBA8 snapshot (`src_w × src_h`, tightly packed).
pub struct PagePlacement<'a> {
    /// Destination rectangle on the page, in page pixels.
    pub dst: Rect,
    /// Source image width in pixels.
    pub src_w: u32,
    /// Source image height in pixels.
    pub src_h: u32,
    /// Source RGBA8 pixels, tightly packed `src_w * src_h * 4`.
    pub rgba: &'a [u8],
}

/// Composite `items` onto a white `page_w × page_h` page, returning tightly
/// packed RGBA8. Each item's snapshot is nearest-neighbor scaled into its
/// destination rectangle (clipped to the page) and src-over composited on white.
/// This is the no-dep substitute for silx's `QPrinter` scene render — the
/// analogue of its print-to-file path.
pub fn compose_page(page_w: u32, page_h: u32, items: &[PagePlacement]) -> Vec<u8> {
    let mut page = vec![255u8; (page_w as usize) * (page_h as usize) * 4];
    for it in items {
        blit_nearest(&mut page, page_w, page_h, it);
    }
    page
}

/// Nearest-neighbor blit of one placement onto the page buffer, src-over white.
fn blit_nearest(page: &mut [u8], pw: u32, ph: u32, it: &PagePlacement) {
    let dx = it.dst.min.x;
    let dy = it.dst.min.y;
    let dw = it.dst.width();
    let dh = it.dst.height();
    if dw <= 0.0 || dh <= 0.0 || it.src_w == 0 || it.src_h == 0 {
        return;
    }
    if it.rgba.len() < (it.src_w as usize) * (it.src_h as usize) * 4 {
        return;
    }
    let x0 = dx.floor().max(0.0) as i64;
    let y0 = dy.floor().max(0.0) as i64;
    let x1 = (dx + dw).ceil().clamp(0.0, pw as f32) as i64;
    let y1 = (dy + dh).ceil().clamp(0.0, ph as f32) as i64;
    for py in y0..y1 {
        for px in x0..x1 {
            let u = (((px as f32 + 0.5 - dx) / dw) * it.src_w as f32).floor() as i64;
            let v = (((py as f32 + 0.5 - dy) / dh) * it.src_h as f32).floor() as i64;
            if u < 0 || v < 0 || u >= it.src_w as i64 || v >= it.src_h as i64 {
                continue;
            }
            let s = ((v * it.src_w as i64 + u) * 4) as usize;
            let d = ((py * pw as i64 + px) * 4) as usize;
            let sa = it.rgba[s + 3] as f32 / 255.0;
            for c in 0..3 {
                let sc = it.rgba[s + c] as f32;
                let dc = page[d + c] as f32;
                page[d + c] = (sc * sa + dc * (1.0 - sa)).round() as u8;
            }
            page[d + 3] = 255;
        }
    }
}

/// What the in-progress pointer drag is doing to a page item.
enum DragKind {
    /// Translating the whole item.
    Move,
    /// Resizing from the bottom-right handle.
    Resize,
}

/// A drag in progress on a specific item.
struct Drag {
    item: usize,
    kind: DragKind,
}

/// One placed snapshot on the page: its source pixels + current page rectangle.
struct PageItem {
    /// Source pixel dimensions (defines the aspect ratio).
    orig: Vec2,
    /// Current rectangle in page coordinates (after moves/resizes).
    rect: Rect,
    /// Preserve aspect ratio on resize (silx `keepRatio`).
    keep_ratio: bool,
    /// Title drawn (centered) above the item.
    title: String,
    /// Comment drawn below the item.
    comment: String,
    /// Alignment of the comment under the item.
    comment_pos: CommentPosition,
    /// Source RGBA8 pixels, kept for the page-to-PNG composite.
    rgba: Vec<u8>,
    /// Lazily-created display texture (built on the first `show`).
    texture: Option<egui::TextureHandle>,
}

/// Print-preview page editor; see the module docs.
pub struct PrintPreview {
    /// Printable page size in page pixels.
    page_size: Vec2,
    /// Placed items, drawn bottom (first) to top (last).
    items: Vec<PageItem>,
    /// Index of the selected item, if any.
    selected: Option<usize>,
    /// On-screen zoom of the page view (silx `_viewScale`).
    view_scale: f32,
    /// Whether the window is open.
    open: bool,
    /// Detached-window placement state.
    win: DetachedWindow,
    /// Pointer drag in progress.
    drag: Option<Drag>,
}

impl Default for PrintPreview {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintPreview {
    /// A closed, empty preview with an A4-portrait page.
    pub fn new() -> Self {
        Self {
            page_size: DEFAULT_PAGE,
            items: Vec::new(),
            selected: None,
            view_scale: 0.5,
            open: false,
            win: DetachedWindow::new(egui::Id::new("rsplot_print_preview"), vec2(480.0, 640.0)),
            drag: None,
        }
    }

    /// Use a custom page size in pixels (default A4 portrait @96 dpi).
    pub fn with_page_size(mut self, width: u32, height: u32) -> Self {
        self.page_size = vec2(width.max(1) as f32, height.max(1) as f32);
        self
    }

    /// Whether the window is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close the window.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// The page size in pixels.
    pub fn page_size(&self) -> (u32, u32) {
        (self.page_size.x as u32, self.page_size.y as u32)
    }

    /// Number of items currently placed on the page.
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Add an RGBA8 snapshot (`width × height`, tightly packed `width*height*4`)
    /// to the page at silx's default print geometry, selecting it. `title` is
    /// drawn above and `comment` (aligned per `comment_pos`) below. A
    /// zero-sized or length-mismatched buffer is ignored.
    pub fn add_image(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        title: impl Into<String>,
        comment: impl Into<String>,
        comment_pos: CommentPosition,
    ) {
        if width == 0 || height == 0 || rgba.len() != (width as usize) * (height as usize) * 4 {
            return;
        }
        let orig = vec2(width as f32, height as f32);
        let rect = default_placement(self.page_size, orig);
        self.items.push(PageItem {
            orig,
            rect,
            keep_ratio: true,
            title: title.into(),
            comment: comment.into(),
            comment_pos,
            rgba,
            texture: None,
        });
        self.selected = Some(self.items.len() - 1);
    }

    /// Remove every item, keeping the page (silx `_clearAll`).
    pub fn clear_all(&mut self) {
        self.items.clear();
        self.selected = None;
        self.drag = None;
    }

    /// Remove the selected item, if any (silx `_remove`).
    pub fn remove_selected(&mut self) {
        if let Some(i) = self.selected.take()
            && i < self.items.len()
        {
            self.items.remove(i);
        }
        self.drag = None;
    }

    /// Zoom the page view in (silx `_zoomPlus`, ×1.2).
    pub fn zoom_in(&mut self) {
        self.view_scale = (self.view_scale * 1.2).min(8.0);
    }

    /// Zoom the page view out (silx `_zoomMinus`, ×0.8).
    pub fn zoom_out(&mut self) {
        self.view_scale = (self.view_scale * 0.8).max(0.05);
    }

    /// Compose the arranged page to a PNG byte stream — the no-dep substitute for
    /// silx's `QPrinter` render (its print-to-file path).
    pub fn take_page_png(&self) -> Result<Vec<u8>, png::EncodingError> {
        let (pw, ph) = self.page_size();
        let placements: Vec<PagePlacement> = self
            .items
            .iter()
            .map(|it| PagePlacement {
                dst: it.rect,
                src_w: it.orig.x as u32,
                src_h: it.orig.y as u32,
                rgba: &it.rgba,
            })
            .collect();
        let page = compose_page(pw, ph, &placements);
        crate::render::save::encode_png(&page, pw, ph)
    }

    /// Open a native save dialog and write the composed page PNG (the Save Page
    /// button). Returns `Ok(false)` if the dialog was cancelled.
    fn save_page_dialog(&self) -> std::io::Result<bool> {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG page", &["png"])
            .save_file()
        else {
            return Ok(false);
        };
        let png = self
            .take_page_png()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, png)?;
        Ok(true)
    }

    /// The bottom-right resize handle of `rect`, never larger than the item.
    fn handle_rect(rect: Rect) -> Rect {
        let s = RESIZE_HANDLE.min(rect.width()).min(rect.height());
        Rect::from_min_size(pos2(rect.max.x - s, rect.max.y - s), vec2(s, s))
    }

    /// Topmost item index under page-space point `p`.
    fn item_at(&self, p: Pos2) -> Option<usize> {
        (0..self.items.len())
            .rev()
            .find(|&i| self.items[i].rect.contains(p))
    }

    /// Show the preview window (when open).
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();
        let signals = show_detached(ctx, id, "Print Preview", size, pos, |ui| self.ui(ui));
        let mut open = self.open;
        self.win.apply_signals(&signals, &mut open);
        self.open = open;
    }

    /// Toolbar + page view, rendered into the detached window's `ui`.
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Clear All")
                .on_hover_text("Remove all items")
                .clicked()
            {
                self.clear_all();
            }
            let has_sel = self.selected.is_some();
            if ui
                .add_enabled(has_sel, egui::Button::new("Remove"))
                .on_hover_text("Remove the selected item")
                .clicked()
            {
                self.remove_selected();
            }
            ui.separator();
            if ui.button("Zoom +").clicked() {
                self.zoom_in();
            }
            if ui.button("Zoom -").clicked() {
                self.zoom_out();
            }
            ui.separator();
            // Per-item keep-aspect toggle (silx PrintGeometryDialog keepAspectRatio).
            let mut keep = self
                .selected
                .and_then(|i| self.items.get(i))
                .map(|it| it.keep_ratio)
                .unwrap_or(true);
            if ui
                .add_enabled(has_sel, egui::Checkbox::new(&mut keep, "Keep aspect"))
                .changed()
                && let Some(i) = self.selected
                && let Some(it) = self.items.get_mut(i)
            {
                it.keep_ratio = keep;
            }
            ui.separator();
            if ui
                .button("Save Page…")
                .on_hover_text("Save the arranged page as PNG")
                .clicked()
            {
                let _ = self.save_page_dialog();
            }
        });
        ui.separator();
        egui::ScrollArea::both().show(ui, |ui| self.page_ui(ui));
    }

    /// The page area: backdrop, white page, items, and pointer interaction.
    fn page_ui(&mut self, ui: &mut egui::Ui) {
        let view = self.page_size * self.view_scale;
        let (resp, painter) = ui.allocate_painter(view, egui::Sense::click_and_drag());
        let origin = resp.rect.min;
        let scale = self.view_scale;
        let to_screen = |p: Pos2| origin + (p.to_vec2() * scale);

        painter.rect_filled(resp.rect, egui::CornerRadius::ZERO, Color32::from_gray(150));
        painter.rect_filled(
            Rect::from_min_size(origin, view),
            egui::CornerRadius::ZERO,
            Color32::WHITE,
        );

        // Upload each item's texture lazily on its first frame.
        for it in &mut self.items {
            if it.texture.is_none() {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [it.orig.x as usize, it.orig.y as usize],
                    &it.rgba,
                );
                it.texture = Some(ui.ctx().load_texture(
                    "rsplot_print_item",
                    img,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }

        let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        for (i, it) in self.items.iter().enumerate() {
            let scr = Rect::from_min_max(to_screen(it.rect.min), to_screen(it.rect.max));
            if let Some(tex) = &it.texture {
                painter.image(tex.id(), scr, uv, Color32::WHITE);
            }
            if !it.title.is_empty() {
                painter.text(
                    pos2(scr.center().x, scr.top()),
                    egui::Align2::CENTER_BOTTOM,
                    &it.title,
                    egui::FontId::proportional(12.0),
                    Color32::BLACK,
                );
            }
            if !it.comment.is_empty() {
                let (anchor, x) = match it.comment_pos {
                    CommentPosition::Left => (egui::Align2::LEFT_TOP, scr.left()),
                    CommentPosition::Center => (egui::Align2::CENTER_TOP, scr.center().x),
                    CommentPosition::Right => (egui::Align2::RIGHT_TOP, scr.right()),
                };
                painter.text(
                    pos2(x, scr.bottom()),
                    anchor,
                    &it.comment,
                    egui::FontId::proportional(11.0),
                    Color32::BLACK,
                );
            }
            if self.selected == Some(i) {
                painter.rect_stroke(
                    scr,
                    egui::CornerRadius::ZERO,
                    egui::Stroke::new(1.0_f32, Color32::RED),
                    egui::StrokeKind::Inside,
                );
                let h = Self::handle_rect(it.rect);
                let hs = Rect::from_min_max(to_screen(h.min), to_screen(h.max));
                painter.rect_filled(hs, egui::CornerRadius::ZERO, Color32::from_rgb(255, 220, 0));
            }
        }

        if resp.drag_started() {
            if let Some(sp) = resp.interact_pointer_pos() {
                self.begin_drag(((sp - origin) / scale).to_pos2());
            }
        } else if resp.dragged() && self.drag.is_some() {
            self.apply_drag(resp.drag_delta() / scale);
        } else if resp.drag_stopped() {
            self.drag = None;
        } else if resp.clicked()
            && let Some(sp) = resp.interact_pointer_pos()
        {
            self.selected = self.item_at(((sp - origin) / scale).to_pos2());
        }
    }

    /// Begin a move/resize on the topmost item under page-space point `p`: the
    /// resize handle takes priority over the body; an empty area deselects.
    fn begin_drag(&mut self, p: Pos2) {
        for i in (0..self.items.len()).rev() {
            let rect = self.items[i].rect;
            if Self::handle_rect(rect).contains(p) {
                self.selected = Some(i);
                self.drag = Some(Drag {
                    item: i,
                    kind: DragKind::Resize,
                });
                return;
            }
            if rect.contains(p) {
                self.selected = Some(i);
                self.drag = Some(Drag {
                    item: i,
                    kind: DragKind::Move,
                });
                return;
            }
        }
        self.selected = None;
        self.drag = None;
    }

    /// Apply a per-frame page-space `delta` to the dragged item.
    fn apply_drag(&mut self, delta: Vec2) {
        let Some(drag) = &self.drag else {
            return;
        };
        let i = drag.item;
        if i >= self.items.len() {
            self.drag = None;
            return;
        }
        match drag.kind {
            DragKind::Move => {
                self.items[i].rect = self.items[i].rect.translate(delta);
            }
            DragKind::Resize => {
                let keep = self.items[i].keep_ratio;
                self.items[i].rect = resize_item_rect(self.items[i].rect, delta, keep);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected {b}, got {a}");
    }

    #[test]
    fn resize_keep_ratio_scales_uniformly_by_smaller_growth() {
        // 100×50 (ratio 2.0), drag (+40,+40): r1=1.40 (width) < r2=1.80 (height)
        // -> width-driven: new = (140, 70), aspect preserved.
        let r = resize_item_rect(
            Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 50.0)),
            vec2(40.0, 40.0),
            true,
        );
        approx(r.min.x, 10.0);
        approx(r.min.y, 20.0);
        approx(r.width(), 140.0);
        approx(r.height(), 70.0);
        approx(r.width() / r.height(), 2.0);
    }

    #[test]
    fn resize_free_scales_each_axis_independently() {
        let r = resize_item_rect(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0)),
            vec2(40.0, 40.0),
            false,
        );
        approx(r.width(), 140.0);
        approx(r.height(), 90.0);
    }

    #[test]
    fn resize_clamps_to_minimum_size() {
        let r = resize_item_rect(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(20.0, 20.0)),
            vec2(-100.0, -100.0),
            false,
        );
        approx(r.width(), MIN_ITEM_SIZE);
        approx(r.height(), MIN_ITEM_SIZE);
    }

    #[test]
    fn default_placement_fits_wide_and_tall_images_keeping_aspect() {
        // Wide image (aspect 0.5): width-bound at 90% page width.
        let r = default_placement(vec2(800.0, 1000.0), vec2(200.0, 100.0));
        approx(r.min.x, 80.0);
        approx(r.min.y, 100.0);
        approx(r.width(), 720.0);
        approx(r.height(), 360.0);
        // Tall image (aspect 4.0): height-bound at 90% page height.
        let r = default_placement(vec2(800.0, 1000.0), vec2(100.0, 400.0));
        approx(r.width(), 225.0);
        approx(r.height(), 900.0);
    }

    #[test]
    fn compose_page_blits_item_over_white_background() {
        // 4×4 white page; a 1×1 opaque red source scaled into the top-left 2×2.
        let red = vec![255u8, 0, 0, 255];
        let placements = [PagePlacement {
            dst: Rect::from_min_size(pos2(0.0, 0.0), vec2(2.0, 2.0)),
            src_w: 1,
            src_h: 1,
            rgba: &red,
        }];
        let page = compose_page(4, 4, &placements);
        let px = |x: usize, y: usize| {
            let i = (y * 4 + x) * 4;
            [page[i], page[i + 1], page[i + 2], page[i + 3]]
        };
        // Top-left 2×2 is red...
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(px(x, y), [255, 0, 0, 255], "({x},{y}) should be red");
            }
        }
        // ...the rest stays white.
        assert_eq!(px(3, 3), [255, 255, 255, 255]);
        assert_eq!(px(2, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn compose_page_alpha_composites_over_white() {
        // 50% red over white -> ~ (255, 128, 128).
        let half_red = vec![255u8, 0, 0, 128];
        let placements = [PagePlacement {
            dst: Rect::from_min_size(pos2(0.0, 0.0), vec2(1.0, 1.0)),
            src_w: 1,
            src_h: 1,
            rgba: &half_red,
        }];
        let page = compose_page(1, 1, &placements);
        assert_eq!(page[0], 255);
        assert!((page[1] as i32 - 128).abs() <= 2, "g={}", page[1]);
        assert!((page[2] as i32 - 128).abs() <= 2, "b={}", page[2]);
        assert_eq!(page[3], 255);
    }

    #[test]
    fn add_image_rejects_zero_size_and_mismatched_buffer() {
        let mut pv = PrintPreview::new();
        pv.add_image(vec![0; 4], 0, 1, "", "", CommentPosition::Center);
        pv.add_image(vec![0; 3], 1, 1, "", "", CommentPosition::Center); // wrong length
        assert_eq!(pv.item_count(), 0);
        pv.add_image(vec![1, 2, 3, 4], 1, 1, "t", "c", CommentPosition::Center);
        assert_eq!(pv.item_count(), 1);
        assert_eq!(pv.selected, Some(0));
    }

    #[test]
    fn clear_all_and_remove_selected_manage_items() {
        let mut pv = PrintPreview::new();
        pv.add_image(vec![1, 2, 3, 4], 1, 1, "", "", CommentPosition::Center);
        pv.add_image(vec![5, 6, 7, 8], 1, 1, "", "", CommentPosition::Center);
        assert_eq!(pv.item_count(), 2);
        assert_eq!(pv.selected, Some(1));
        pv.remove_selected();
        assert_eq!(pv.item_count(), 1);
        assert_eq!(pv.selected, None);
        pv.add_image(vec![9, 9, 9, 9], 1, 1, "", "", CommentPosition::Center);
        pv.clear_all();
        assert_eq!(pv.item_count(), 0);
        assert_eq!(pv.selected, None);
    }

    #[test]
    fn take_page_png_encodes_a_page_sized_image() {
        let mut pv = PrintPreview::new().with_page_size(16, 24);
        assert_eq!(pv.page_size(), (16, 24));
        pv.add_image(vec![255, 0, 0, 255], 1, 1, "", "", CommentPosition::Center);
        let png = pv.take_page_png().expect("encode page png");
        // Valid PNG signature, and the IHDR width/height (big-endian at fixed
        // offsets 16..24) match the page size.
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!((w, h), (16, 24));
    }

    #[test]
    fn zoom_in_out_stay_within_bounds() {
        let mut pv = PrintPreview::new();
        for _ in 0..100 {
            pv.zoom_in();
        }
        assert!(pv.view_scale <= 8.0);
        for _ in 0..200 {
            pv.zoom_out();
        }
        assert!(pv.view_scale >= 0.05);
    }

    #[test]
    fn show_open_renders_the_page_without_panic() {
        // Exercises the full render path (texture upload + painter + interaction)
        // headlessly, like print_dialog's render test.
        let ctx = egui::Context::default();
        let mut pv = PrintPreview::new();
        pv.set_open(true);
        pv.add_image(
            vec![10, 20, 30, 255],
            1,
            1,
            "title",
            "comment",
            CommentPosition::Center,
        );
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            pv.show(ui.ctx());
        });
        assert!(pv.is_open());
        assert_eq!(pv.item_count(), 1);
    }

    #[test]
    fn show_closed_is_a_noop() {
        let ctx = egui::Context::default();
        let mut pv = PrintPreview::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            pv.show(ui.ctx());
        });
        assert!(!pv.is_open());
    }
}
