//! Picture Compare: two images side by side, as a difference mask, blended, or with a swipe divider.
//!
//! Decoding, diffing and building display copies all run in background jobs; the UI thread only
//! uploads finished previews as textures.

use std::path::Path;
use std::sync::Arc;

use diffcraft_core::image::{self as img, ImageDiff, RgbaImage};
use diffcraft_session::{Session, SessionKind, Settings};
use egui::{
    Align, Align2, Color32, ColorImage, CursorIcon, FontId, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, TextureFilter, TextureHandle, TextureOptions,
    TextureWrapMode, Ui, UiBuilder, Vec2, pos2, vec2,
};

use crate::icons::Icon;
use crate::jobs::{self, Job};
use crate::theme::Palette;
use crate::toolbar;
use crate::view::{Cmd, Cx, View};
use crate::widgets;

/// Largest texture side we upload (safe on every GPU we target); bigger images are downscaled for display.
const MAX_TEXTURE_SIDE: usize = 8192;
const MIN_ZOOM: f32 = 0.01;
const MAX_ZOOM: f32 = 64.0;
/// Zoom levels the Zoom In / Zoom Out buttons step through.
const ZOOM_STEPS: [f32; 18] = [0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0];
/// Width of the gutter between the two panes.
const PANE_GAP: f32 = 8.0;
/// Height of a pane's title bar.
const TITLE_H: f32 = 22.0;
/// Margin around the fitted picture inside a pane.
const FIT_MARGIN: f32 = 8.0;
/// Checkerboard cell size in screen pixels.
const CHECKER: f32 = 8.0;
/// Height of the big toolbar buttons (matches `toolbar`).
const BIG_H: f32 = 46.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    SideBySide,
    Difference,
    Blend,
    Swipe,
}

impl Mode {
    const ALL: [Mode; 4] = [Mode::SideBySide, Mode::Difference, Mode::Blend, Mode::Swipe];

    fn label(self) -> &'static str {
        match self {
            Mode::SideBySide => "Side by Side",
            Mode::Difference => "Difference",
            Mode::Blend => "Blend",
            Mode::Swipe => "Swipe",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Mode::SideBySide => Icon::Picture,
            Mode::Difference => Icon::ShowDiffs,
            Mode::Blend => Icon::Minor,
            Mode::Swipe => Icon::Swap,
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Mode::SideBySide => "Show both pictures side by side",
            Mode::Difference => "Show the differing pixels over a greyscale copy of the left picture",
            Mode::Blend => "Show the right picture over the left one, half transparent",
            Mode::Swipe => "Show the left picture left of a divider and the right picture right of it; drag the divider",
        }
    }
}

/// One side's decoded image, or why there is none.
#[derive(Clone, Default)]
struct Side {
    image: Option<Arc<RgbaImage>>,
    error: Option<String>,
    /// Size of the file on disk.
    file_size: Option<u64>,
}

/// A display copy ready for upload.
struct Preview {
    image: ColorImage,
    downscaled: bool,
}

/// A computed difference with its mask already turned into a display copy.
struct DiffOut {
    /// Statistics (the full-resolution mask is dropped once the preview is built).
    diff: ImageDiff,
    tolerance: u8,
    preview: Option<Preview>,
}

/// Result of a background load.
struct Loaded {
    left: Side,
    right: Side,
    left_preview: Option<Preview>,
    right_preview: Option<Preview>,
    diff: Option<DiffOut>,
}

#[derive(Default)]
struct Textures {
    left: Option<TextureHandle>,
    right: Option<TextureHandle>,
    mask: Option<TextureHandle>,
    left_downscaled: bool,
    right_downscaled: bool,
    mask_downscaled: bool,
}

pub struct PictureView {
    session: Session,
    left_path: String,
    right_path: String,
    left: Side,
    right: Side,
    diff: Option<ImageDiff>,
    diff_tolerance: Option<u8>,
    tolerance: u8,
    tex: Textures,
    checker: Option<(bool, TextureHandle)>,
    load_job: Option<Job<Loaded>>,
    diff_job: Option<Job<DiffOut>>,
    mode: Mode,
    zoom: f32,
    fit: bool,
    /// Screen offset of the image from its centred position.
    pan: Vec2,
    opacity: f32,
    /// Swipe divider position as a fraction of the pane width.
    swipe: f32,
    swiping: bool,
    /// Canvas pixel under the pointer (last frame).
    hover: Option<(u32, u32)>,
}

impl PictureView {
    pub fn from_session(s: &Session, settings: &Settings, ctx: &egui::Context) -> Self {
        let mut v = PictureView {
            session: s.clone(),
            left_path: s.left.clone(),
            right_path: s.right.clone(),
            left: Side::default(),
            right: Side::default(),
            diff: None,
            diff_tolerance: None,
            tolerance: settings.picture_tolerance,
            tex: Textures::default(),
            checker: None,
            load_job: None,
            diff_job: None,
            mode: Mode::SideBySide,
            zoom: 1.0,
            fit: true,
            pan: Vec2::ZERO,
            opacity: 0.5,
            swipe: 0.5,
            swiping: false,
            hover: None,
        };
        v.reload(ctx);
        v
    }

    fn max_side(ctx: &egui::Context) -> usize {
        ctx.input(|i| i.max_texture_side).clamp(1, MAX_TEXTURE_SIDE)
    }

    /// Decode both images, compare them and build display copies, in the background.
    fn reload(&mut self, ctx: &egui::Context) {
        let (l, r, tol, max_side) = (self.left_path.clone(), self.right_path.clone(), self.tolerance, Self::max_side(ctx));
        if let Some(j) = self.diff_job.take() {
            j.cancel();
        }
        if let Some(j) = self.load_job.take() {
            j.cancel();
        }
        self.load_job = Some(Job::spawn("Loading images", ctx, move |p| {
            p.set_current(l.trim());
            let left = load_side(&l);
            p.set_current(r.trim());
            let right = load_side(&r);
            if p.is_cancelled() {
                return Err("cancelled".into());
            }
            p.set_current("comparing");
            let diff = match (&left.image, &right.image) {
                (Some(a), Some(b)) => Some(compute_diff(a, b, tol, max_side)),
                _ => None,
            };
            p.set_current("preparing previews");
            let left_preview = left.image.as_deref().and_then(|i| preview(i, max_side));
            let right_preview = right.image.as_deref().and_then(|i| preview(i, max_side));
            Ok(Loaded { left, right, left_preview, right_preview, diff })
        }));
    }

    /// Recompute the difference for the loaded images (tolerance changed or sides swapped).
    fn rediff(&mut self, ctx: &egui::Context) {
        if self.load_job.is_some() {
            // The load in flight would replace these images; the tolerance is re-checked when it lands.
            return;
        }
        let (Some(a), Some(b)) = (self.left.image.clone(), self.right.image.clone()) else { return };
        if let Some(j) = self.diff_job.take() {
            j.cancel();
        }
        let (tol, max_side) = (self.tolerance, Self::max_side(ctx));
        self.diff_job = Some(Job::spawn("Comparing", ctx, move |_| Ok(compute_diff(&a, &b, tol, max_side))));
    }

    fn poll_jobs(&mut self, ctx: &egui::Context, cx: &mut Cx<'_>) {
        if let Some(job) = self.load_job.as_mut()
            && let Some(result) = job.poll()
        {
            self.load_job = None;
            match result {
                Ok(l) => {
                    for (name, side) in [("left", &l.left), ("right", &l.right)] {
                        if let Some(e) = &side.error {
                            cx.toasts.error(format!("Could not load the {name} image: {e}"));
                        }
                    }
                    self.left = l.left;
                    self.right = l.right;
                    (self.tex.left, self.tex.left_downscaled) = upload(ctx, "picture-left", l.left_preview);
                    (self.tex.right, self.tex.right_downscaled) = upload(ctx, "picture-right", l.right_preview);
                    self.apply_diff(ctx, l.diff);
                    if self.diff_tolerance.is_some_and(|t| t != self.tolerance) {
                        self.rediff(ctx);
                    }
                    self.fit = true;
                    self.pan = Vec2::ZERO;
                }
                Err(e) => cx.toasts.error(format!("Picture compare failed: {e}")),
            }
        }
        if let Some(job) = self.diff_job.as_mut()
            && let Some(result) = job.poll()
        {
            self.diff_job = None;
            match result {
                Ok(d) => self.apply_diff(ctx, Some(d)),
                Err(e) => cx.toasts.error(format!("Picture compare failed: {e}")),
            }
        }
    }

    fn apply_diff(&mut self, ctx: &egui::Context, d: Option<DiffOut>) {
        match d {
            Some(d) => {
                (self.tex.mask, self.tex.mask_downscaled) = upload(ctx, "picture-diff", d.preview);
                self.diff_tolerance = Some(d.tolerance);
                self.diff = Some(d.diff);
            }
            None => {
                self.tex.mask = None;
                self.tex.mask_downscaled = false;
                self.diff_tolerance = None;
                self.diff = None;
            }
        }
    }

    fn swap(&mut self, ctx: &egui::Context) {
        std::mem::swap(&mut self.left_path, &mut self.right_path);
        std::mem::swap(&mut self.left, &mut self.right);
        std::mem::swap(&mut self.tex.left, &mut self.tex.right);
        std::mem::swap(&mut self.tex.left_downscaled, &mut self.tex.right_downscaled);
        if self.load_job.is_some() {
            // A load in flight would apply the old order: start it again with the swapped paths.
            self.reload(ctx);
        } else {
            // The mask's greyscale backdrop comes from the left image, so recompute it.
            self.rediff(ctx);
        }
    }

    /// Size of the comparison canvas (large enough for both images).
    fn canvas(&self) -> Vec2 {
        let dims = |s: &Side| s.image.as_ref().map_or((0, 0), |i| i.dimensions());
        let (a, b) = (dims(&self.left), dims(&self.right));
        vec2(a.0.max(b.0) as f32, a.1.max(b.1) as f32)
    }

    fn set_zoom(&mut self, z: f32) {
        self.fit = false;
        self.zoom = z.clamp(MIN_ZOOM, MAX_ZOOM);
        self.pan = Vec2::ZERO;
    }

    fn toolbar(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        toolbar::bar(ui, "picture-toolbar", |ui| {
            toolbar::home_and_sessions(ui, cx);
            for m in Mode::ALL {
                if toolbar::toggle(ui, m.icon(), m.label(), m.tip(), self.mode == m, true) {
                    self.mode = m;
                }
            }
            toolbar::separator(ui);
            if toolbar::toggle(ui, Icon::Fit, "Fit", "Fit the picture in the window", self.fit, true) {
                self.fit = true;
                self.pan = Vec2::ZERO;
            }
            let zoom_tip = |dir: &str| format!("Zoom {dir} (Ctrl+wheel zooms about the pointer; drag to pan)");
            if toolbar::button(ui, Icon::ZoomIn, "Zoom In", &zoom_tip("in"), self.zoom < MAX_ZOOM) {
                self.set_zoom(zoom_step(self.zoom, true));
            }
            if toolbar::button(ui, Icon::ZoomOut, "Zoom Out", &zoom_tip("out"), self.zoom > MIN_ZOOM) {
                self.set_zoom(zoom_step(self.zoom, false));
            }
            let actual = !self.fit && (self.zoom - 1.0).abs() < 1e-4;
            if text_toggle(ui, "1:1", "100%", "Show the picture at its actual size", actual) {
                self.set_zoom(1.0);
            }
            toolbar::separator(ui);
            let pal = Palette::get(ui.ctx());
            labelled_group(ui, "Tolerance", &pal, |ui| {
                ui.spacing_mut().slider_width = 96.0;
                let r = ui
                    .add(egui::Slider::new(&mut self.tolerance, 0..=255))
                    .on_hover_text("A pixel differs when any channel differs by more than this. The difference is recomputed when you release the slider.");
                if (r.drag_stopped() || (r.changed() && !r.dragged())) && self.diff_tolerance != Some(self.tolerance) {
                    self.rediff(ui.ctx());
                }
            });
            if self.mode == Mode::Blend {
                labelled_group(ui, "Right Opacity", &pal, |ui| {
                    ui.spacing_mut().slider_width = 96.0;
                    ui.add(egui::Slider::new(&mut self.opacity, 0.0..=1.0).fixed_decimals(2)).on_hover_text("Opacity of the right picture over the left one");
                });
            }
            toolbar::separator(ui);
            if toolbar::button(ui, Icon::Swap, "Swap", "Swap the left and right pictures", true) {
                self.swap(ui.ctx());
            }
            if toolbar::button(ui, Icon::Reload, "Reload", "Load both pictures again (F5)", true) {
                self.reload(ui.ctx());
            }
        });
    }

    /// Path rows, picture information and job progress.
    fn header(&mut self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        let ctx = ui.ctx().clone();
        egui::Panel::top("picture-paths").frame(egui::Frame::NONE.fill(pal.toolbar_bg)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let side_w = ((ui.available_width() - PANE_GAP) / 2.0).max(0.0);
            let mut reload = false;
            two_columns(ui, "picture-cols", side_w, PANE_GAP, &pal, |ui, left| {
                let (path, side, id, hint) = if left {
                    (&mut self.left_path, &self.left, "picture-left", "Left image")
                } else {
                    (&mut self.right_path, &self.right, "picture-right", "Right image")
                };
                toolbar::header_strip(ui, 26.0, |ui| {
                    reload |= widgets::path_bar(ui, id, path, false, hint).submitted;
                });
                info_strip(ui, &side_info(side), side.error.as_deref(), &pal);
            });
            if reload {
                self.reload(&ctx);
            }
            let mut cancel = false;
            if self.load_job.is_some() || self.diff_job.is_some() {
                egui::Frame::NONE.fill(pal.toolbar_bg).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
                    if let Some(job) = &self.load_job {
                        cancel |= jobs::progress_ui(ui, job);
                    } else if let Some(job) = &self.diff_job {
                        cancel |= jobs::progress_ui(ui, job);
                    }
                });
            }
            if cancel {
                self.load_job = None;
                self.diff_job = None;
            }
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        toolbar::status_bar(ui, "picture-status", |ui| {
            match &self.diff {
                Some(d) if d.is_identical() => {
                    toolbar::status_icon(ui, Icon::Check);
                    ui.label(RichText::new("Pictures are identical").color(pal.success));
                }
                Some(d) => {
                    toolbar::status_icon(ui, Icon::ShowDiffs);
                    ui.label(format!("{} differing pixels ({:.2}%)", widgets::thousands(d.differing_pixels), d.differing_percent()));
                    toolbar::status_sep(ui);
                    ui.label(format!("Max delta {}", d.max_delta)).on_hover_text("Largest difference of any channel (0–255)");
                }
                None if self.load_job.is_some() || self.diff_job.is_some() => {
                    ui.label(RichText::new("Comparing…").color(pal.dim_text));
                }
                None => {
                    ui.label(RichText::new("Nothing compared").color(pal.dim_text));
                }
            }
            toolbar::status_sep(ui);
            ui.label(RichText::new(format!("Zoom {:.0}%", self.zoom * 100.0)).color(pal.dim_text));
            if self.tex.left_downscaled || self.tex.right_downscaled || self.tex.mask_downscaled {
                toolbar::status_sep(ui);
                ui.label(RichText::new("Preview downscaled").color(pal.warning));
            }
            if let Some((x, y)) = self.hover {
                toolbar::status_sep(ui);
                ui.label(RichText::new(format!("{x}, {y}")).monospace());
                toolbar::status_sep(ui);
                let px = |s: &Side| s.image.as_ref().and_then(|i| i.get_pixel_checked(x, y)).map(|p| p.0);
                let (l, r) = (px(&self.left), px(&self.right));
                let color = if l != r { pal.changed_text } else { pal.text };
                ui.label(RichText::new(format!("L {}   R {}", fmt_rgba(l), fmt_rgba(r))).monospace().color(color)).on_hover_text("Red, green, blue and alpha");
            }
        });
    }

    fn checker(&mut self, ctx: &egui::Context, pal: &Palette) -> TextureHandle {
        if let Some((dark, t)) = &self.checker
            && *dark == pal.dark
        {
            return t.clone();
        }
        let (a, b) = (pal.pane_bg, pal.filler_bg);
        let image = ColorImage::new([2, 2], vec![a, b, b, a]);
        let opts = TextureOptions { wrap_mode: TextureWrapMode::Repeat, ..TextureOptions::NEAREST };
        let t = ctx.load_texture("picture-checker", image, opts);
        self.checker = Some((pal.dark, t.clone()));
        t
    }

    /// Title bar text for each pane: (label, name, details).
    fn pane_titles(&self) -> Vec<(String, String, String)> {
        let name = |p: &str| file_name(p);
        let dims = |s: &Side| s.image.as_ref().map(|i| format!("{} × {}", i.width(), i.height())).unwrap_or_default();
        match self.mode {
            Mode::SideBySide => {
                vec![("Left:".to_owned(), name(&self.left_path), dims(&self.left)), ("Right:".to_owned(), name(&self.right_path), dims(&self.right))]
            }
            Mode::Difference => {
                let c = self.canvas();
                let details = if c.x > 0.0 { format!("{} × {}", c.x as u32, c.y as u32) } else { String::new() };
                vec![("Difference:".to_owned(), format!("{}  vs  {}", name(&self.left_path), name(&self.right_path)), details)]
            }
            Mode::Blend => vec![(
                "Blend:".to_owned(),
                format!("{}  over  {}", name(&self.right_path), name(&self.left_path)),
                format!("{:.0}% opacity", self.opacity.clamp(0.0, 1.0) * 100.0),
            )],
            Mode::Swipe => vec![("Swipe:".to_owned(), format!("{}  |  {}", name(&self.left_path), name(&self.right_path)), "drag the divider".to_owned())],
        }
    }

    fn body(&mut self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        let checker = self.checker(ui.ctx(), &pal);
        let full = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(full, Sense::click_and_drag());
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, pal.pane_bg);
        let canvas = self.canvas();

        // Pane outlines (title bar included) and the picture areas below the title bars.
        let outer: Vec<Rect> = if self.mode == Mode::SideBySide {
            let w = ((full.width() - PANE_GAP) / 2.0).max(1.0);
            vec![Rect::from_min_size(full.min, vec2(w, full.height())), Rect::from_min_max(pos2(full.left() + w + PANE_GAP, full.top()), full.max)]
        } else {
            vec![full]
        };
        let panes: Vec<Rect> = outer.iter().map(|r| Rect::from_min_max(pos2(r.left(), (r.top() + TITLE_H).min(r.bottom())), r.max)).collect();
        let pane0 = panes.first().copied().unwrap_or(full);
        if self.fit {
            self.zoom = fit_zoom(pane0.size() - Vec2::splat(2.0 * FIT_MARGIN), canvas);
            self.pan = Vec2::ZERO;
        }

        // Zoom about the pointer (Ctrl/Cmd + wheel or pinch), pan with the wheel or by dragging.
        let pointer = resp.hover_pos();
        let hovered_pane = pointer.and_then(|p| panes.iter().copied().find(|r| r.contains(p)));
        if let (Some(p), Some(pane)) = (pointer, hovered_pane) {
            let (zoom_delta, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if (zoom_delta - 1.0).abs() > f32::EPSILON {
                let nz = (self.zoom * zoom_delta).clamp(MIN_ZOOM, MAX_ZOOM);
                self.pan = zoom_about(pane.center(), canvas, self.zoom, self.pan, p, nz);
                self.zoom = nz;
                self.fit = false;
            } else if scroll != Vec2::ZERO {
                self.pan += scroll;
                self.fit = false;
            }
        }
        let divider_x = |pane: Rect, f: f32| pane.left() + f.clamp(0.0, 1.0) * pane.width();
        let (mode, swipe) = (self.mode, self.swipe);
        let near_divider = |p: Pos2| mode == Mode::Swipe && (p.x - divider_x(pane0, swipe)).abs() < 8.0;
        if resp.drag_started() {
            self.swiping = resp.interact_pointer_pos().is_some_and(near_divider);
        }
        if resp.dragged() {
            if self.swiping {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.swipe = ((p.x - pane0.left()) / pane0.width().max(1.0)).clamp(0.0, 1.0);
                }
            } else {
                self.pan += resp.drag_delta();
                self.fit = false;
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
        }
        if resp.drag_stopped() {
            self.swiping = false;
        }
        if self.swiping || pointer.is_some_and(near_divider) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }

        // Pixel under the pointer.
        self.hover = match (pointer, hovered_pane) {
            (Some(p), Some(pane)) => screen_to_pixel(origin(pane.center(), canvas, self.zoom, self.pan), self.zoom, p, canvas),
            _ => None,
        };

        let dims = |s: &Side| s.image.as_ref().map_or(Vec2::ZERO, |i| vec2(i.width() as f32, i.height() as f32));
        let (ldim, rdim) = (dims(&self.left), dims(&self.right));
        let draw = |p: &egui::Painter, pane: Rect, tex: Option<&TextureHandle>, size: Vec2, tint: Color32, checkerboard: bool| {
            let Some(tex) = tex else { return };
            let rect = Rect::from_min_size(origin(pane.center(), canvas, self.zoom, self.pan), size * self.zoom);
            if checkerboard {
                let uv = Rect::from_min_max(Pos2::ZERO, (rect.size() / (2.0 * CHECKER)).to_pos2());
                p.image(checker.id(), rect, uv, Color32::WHITE);
                p.rect_stroke(rect, 0.0, Stroke::new(1.0, pal.border), egui::StrokeKind::Outside);
            }
            p.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), tint);
        };
        let pane_painter = painter.with_clip_rect(pane0);
        match self.mode {
            Mode::SideBySide => {
                for (pane, tex, size, side) in
                    [(pane0, &self.tex.left, ldim, &self.left), (panes.get(1).copied().unwrap_or(full), &self.tex.right, rdim, &self.right)]
                {
                    let p = ui.painter_at(pane);
                    draw(&p, pane, tex.as_ref(), size, Color32::WHITE, true);
                    if tex.is_none() && self.load_job.is_none() {
                        p.text(pane.center(), Align2::CENTER_CENTER, side_message(side), FontId::proportional(14.0), pal.dim_text);
                    }
                }
            }
            Mode::Difference => {
                draw(&pane_painter, pane0, self.tex.mask.as_ref(), canvas, Color32::WHITE, true);
            }
            Mode::Blend => {
                draw(&pane_painter, pane0, self.tex.left.as_ref(), ldim, Color32::WHITE, true);
                draw(&pane_painter, pane0, self.tex.right.as_ref(), rdim, Color32::WHITE.gamma_multiply(self.opacity.clamp(0.0, 1.0)), false);
            }
            Mode::Swipe => {
                let x = divider_x(pane0, self.swipe);
                let lp = painter.with_clip_rect(Rect::from_min_max(pane0.min, pos2(x, pane0.bottom())));
                let rp = painter.with_clip_rect(Rect::from_min_max(pos2(x, pane0.top()), pane0.max));
                draw(&lp, pane0, self.tex.left.as_ref(), ldim, Color32::WHITE, true);
                draw(&rp, pane0, self.tex.right.as_ref(), rdim, Color32::WHITE, true);
                pane_painter.line_segment([pos2(x, pane0.top()), pos2(x, pane0.bottom())], Stroke::new(2.0, pal.accent));
                pane_painter.circle_filled(pos2(x, pane0.center().y), 6.0, pal.accent);
            }
        }
        if self.mode != Mode::SideBySide && (self.tex.left.is_none() || self.tex.right.is_none()) && self.load_job.is_none() {
            let msg = format!("Left: {}   Right: {}", side_message(&self.left), side_message(&self.right));
            if self.mode == Mode::Difference || self.tex.left.is_none() && self.tex.right.is_none() {
                pane_painter.text(pane0.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), pal.dim_text);
            } else {
                pane_painter.text(pos2(pane0.center().x, pane0.top() + 16.0), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), pal.dim_text);
            }
        }

        // Title bars, the gutter between the panes and thin borders.
        let border = Stroke::new(1.0, pal.border);
        for (rect, (label, name, details)) in outer.iter().zip(self.pane_titles()) {
            let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_H.min(rect.height())));
            painter.rect_filled(bar, 0.0, pal.header_bg);
            painter.line_segment([bar.left_bottom() - vec2(0.0, 0.5), bar.right_bottom() - vec2(0.0, 0.5)], border);
            let tp = painter.with_clip_rect(bar.shrink2(vec2(8.0, 0.0)));
            let mut x = bar.left() + 8.0;
            let cy = bar.center().y;
            let font = FontId::proportional(12.5);
            for (text, color) in [(label, pal.dim_text), (name, pal.text), (details, pal.dim_text)] {
                if text.is_empty() {
                    continue;
                }
                let r = tp.text(pos2(x, cy), Align2::LEFT_CENTER, text, font.clone(), color);
                x = r.right() + 10.0;
            }
        }
        if let [a, b] = outer.as_slice() {
            let gutter = Rect::from_min_max(pos2(a.right(), full.top()), pos2(b.left(), full.bottom()));
            painter.rect_filled(gutter, 0.0, pal.gutter_bg);
            for x in [gutter.left() + 0.5, gutter.right() - 0.5] {
                painter.line_segment([pos2(x, full.top()), pos2(x, full.bottom())], border);
            }
        }
    }
}

/// Two side-by-side columns (left `left_w` wide, then a `gap`, then the rest) filled by `add`.
fn two_columns(ui: &mut Ui, id: &str, left_w: f32, gap: f32, pal: &Palette, mut add: impl FnMut(&mut Ui, bool)) {
    let origin = ui.cursor().min;
    let full_w = ui.available_width();
    let h = ui.available_height().max(1.0);
    let mut bottom = origin.y;
    for (x, w, left) in [(origin.x, left_w, true), (origin.x + left_w + gap, (full_w - left_w - gap).max(0.0), false)] {
        let rect = Rect::from_min_size(pos2(x, origin.y), vec2(w, h));
        let mut child = ui.new_child(UiBuilder::new().id_salt((id, left)).max_rect(rect).layout(Layout::top_down(Align::Min)));
        child.spacing_mut().item_spacing.y = 0.0;
        add(&mut child, left);
        bottom = bottom.max(child.min_rect().bottom());
    }
    let gutter = Rect::from_min_max(pos2(origin.x + left_w, origin.y), pos2(origin.x + left_w + gap, bottom));
    let p = ui.painter();
    p.rect_filled(gutter, 0.0, pal.gutter_bg);
    for x in [gutter.left() + 0.5, gutter.right() - 0.5] {
        p.line_segment([pos2(x, gutter.top()), pos2(x, gutter.bottom())], Stroke::new(1.0, pal.border));
    }
    p.line_segment([pos2(origin.x, bottom - 0.5), pos2(origin.x + full_w, bottom - 0.5)], Stroke::new(1.0, pal.border));
    ui.allocate_rect(Rect::from_min_max(origin, pos2(origin.x + full_w, bottom)), Sense::hover());
}

/// A slim strip of file facts under a path row.
fn info_strip(ui: &mut Ui, items: &[String], error: Option<&str>, pal: &Palette) {
    egui::Frame::NONE.fill(pal.toolbar_bg).inner_margin(Margin::symmetric(8, 0)).show(ui, |ui| {
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 20.0), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if let Some(e) = error {
                ui.label(RichText::new(jobs::shorten(e, 120)).size(11.5).color(pal.error)).on_hover_text(e);
            }
            for it in items {
                ui.label(RichText::new(it).size(11.5).color(pal.dim_text));
            }
        });
    });
}

/// Dimensions and file size of a loaded side.
fn side_info(s: &Side) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(i) = &s.image {
        v.push(format!("{} × {} px", i.width(), i.height()));
    } else if s.error.is_none() {
        v.push("No file".to_owned());
    }
    if let Some(n) = s.file_size.filter(|_| s.image.is_some()) {
        v.push(format!("{} ({} bytes)", widgets::format_size(n), widgets::thousands(n)));
    }
    v
}

/// A large toolbar toggle showing `glyph` (plain text) in place of an icon, styled like `toolbar::toggle`.
fn text_toggle(ui: &mut Ui, glyph: &str, label: &str, tip: &str, selected: bool) -> bool {
    let pal = Palette::get(ui.ctx());
    let font = FontId::proportional(11.5);
    let label_w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), pal.text).size().x);
    let (rect, resp) = ui.allocate_exact_size(vec2((label_w + 14.0).max(44.0), BIG_H), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if selected {
            p.rect(rect, 3.0, pal.active_bg, Stroke::new(1.0, pal.accent.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            p.rect(rect, 3.0, pal.hover_bg, Stroke::NONE, egui::StrokeKind::Inside);
        }
        let ir = Rect::from_center_size(rect.center_top() + vec2(0.0, 15.0), vec2(22.0, 16.0));
        p.rect(ir, 2.0, Color32::TRANSPARENT, Stroke::new(1.2, pal.icon), egui::StrokeKind::Inside);
        p.text(ir.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(10.5), pal.icon);
        p.text(pos2(rect.center().x, rect.bottom() - 9.0), Align2::CENTER_CENTER, label, font, pal.text);
    }
    resp.on_hover_text(tip).clicked()
}

/// A control with a small caption under it, as tall as the big toolbar buttons.
fn labelled_group(ui: &mut Ui, caption: &str, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(150.0, BIG_H), Layout::top_down(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(5.0);
        add(ui);
        ui.label(RichText::new(caption).size(11.5).color(pal.text));
    });
    ui.add_space(4.0);
}

/// The next zoom level up or down from `zoom`.
fn zoom_step(zoom: f32, up: bool) -> f32 {
    let next = if up { ZOOM_STEPS.iter().copied().find(|&z| z > zoom * 1.001) } else { ZOOM_STEPS.iter().rev().copied().find(|&z| z < zoom / 1.001) };
    next.unwrap_or(if up { MAX_ZOOM } else { MIN_ZOOM })
}

/// The file name of a path (the whole text when it has none).
fn file_name(path: &str) -> String {
    let p = widgets::clean_path(path);
    p.file_name().map_or_else(|| path.trim().to_owned(), |n| n.to_string_lossy().into_owned())
}

fn side_message(s: &Side) -> String {
    match (&s.image, &s.error) {
        (Some(_), _) => "loaded".to_owned(),
        (None, Some(e)) => format!("could not load ({})", jobs::shorten(e, 80)),
        (None, None) => "No file".to_owned(),
    }
}

fn load_side(path: &str) -> Side {
    if path.trim().is_empty() {
        return Side::default();
    }
    let p = widgets::clean_path(path);
    let file_size = std::fs::metadata(Path::new(&p)).ok().map(|m| m.len());
    match img::load(&p) {
        Ok(i) => Side { image: Some(Arc::new(i)), error: None, file_size },
        Err(e) => Side { image: None, error: Some(e.to_string()), file_size },
    }
}

fn compute_diff(a: &RgbaImage, b: &RgbaImage, tolerance: u8, max_side: usize) -> DiffOut {
    let mut diff = img::compare(a, b, tolerance);
    let preview = preview(&diff.mask, max_side);
    diff.mask = RgbaImage::new(0, 0);
    DiffOut { diff, tolerance, preview }
}

/// Upload a preview as a texture: crisp pixels when magnified, smooth when shrunk.
fn upload(ctx: &egui::Context, name: &str, p: Option<Preview>) -> (Option<TextureHandle>, bool) {
    match p {
        Some(p) => {
            let opts = TextureOptions { magnification: TextureFilter::Nearest, ..TextureOptions::LINEAR };
            (Some(ctx.load_texture(name, p.image, opts)), p.downscaled)
        }
        None => (None, false),
    }
}

/// Integer downscale factor so neither side exceeds `max_side`.
fn downscale_factor(w: usize, h: usize, max_side: usize) -> usize {
    w.max(h).div_ceil(max_side.max(1)).max(1)
}

/// A display copy of `image`, nearest-sampled down to at most `max_side` pixels per side.
/// `None` for empty images (nothing to upload).
fn preview(image: &RgbaImage, max_side: usize) -> Option<Preview> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let f = downscale_factor(w, h, max_side);
    let (dw, dh) = (w.div_ceil(f), h.div_ceil(f));
    let raw = image.as_raw();
    let mut pixels = Vec::with_capacity(dw.saturating_mul(dh));
    for y in 0..dh {
        let row = y * f * w;
        for x in 0..dw {
            let i = (row + x * f) * 4;
            let c = match raw.get(i..i + 4) {
                Some(&[r, g, b, a]) => Color32::from_rgba_unmultiplied(r, g, b, a),
                _ => Color32::TRANSPARENT,
            };
            pixels.push(c);
        }
    }
    Some(Preview { image: ColorImage::new([dw, dh], pixels), downscaled: f > 1 })
}

/// Zoom that fits `canvas` inside `avail`.
fn fit_zoom(avail: Vec2, canvas: Vec2) -> f32 {
    if canvas.x <= 0.0 || canvas.y <= 0.0 || avail.x <= 0.0 || avail.y <= 0.0 {
        return 1.0;
    }
    (avail.x / canvas.x).min(avail.y / canvas.y).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Screen position of the canvas's top-left corner in a pane centred at `center`.
fn origin(center: Pos2, canvas: Vec2, zoom: f32, pan: Vec2) -> Pos2 {
    center - canvas * zoom / 2.0 + pan
}

/// New pan after zooming from `zoom` to `new_zoom` so the point under `cursor` stays put.
fn zoom_about(center: Pos2, canvas: Vec2, zoom: f32, pan: Vec2, cursor: Pos2, new_zoom: f32) -> Vec2 {
    let o = origin(center, canvas, zoom, pan);
    let p = (cursor - o) / zoom.max(MIN_ZOOM);
    let new_origin = cursor - p * new_zoom;
    new_origin - (center - canvas * new_zoom / 2.0)
}

/// Canvas pixel under screen position `p`, if inside the canvas.
fn screen_to_pixel(origin: Pos2, zoom: f32, p: Pos2, canvas: Vec2) -> Option<(u32, u32)> {
    let v = (p - origin) / zoom.max(MIN_ZOOM);
    let (x, y) = (v.x.floor(), v.y.floor());
    (x >= 0.0 && y >= 0.0 && x < canvas.x && y < canvas.y).then_some((x as u32, y as u32))
}

fn fmt_rgba(p: Option<[u8; 4]>) -> String {
    match p {
        Some([r, g, b, a]) => format!("{r:3} {g:3} {b:3} {a:3}"),
        None => "  —".to_owned(),
    }
}

impl View for PictureView {
    fn kind(&self) -> SessionKind {
        SessionKind::PictureCompare
    }

    fn title(&self) -> String {
        self.session().map(|s| s.title()).unwrap_or_default()
    }

    fn tooltip(&self) -> String {
        format!("Left: {}\nRight: {}", self.left_path, self.right_path)
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        self.poll_jobs(&ctx, cx);
        self.toolbar(ui, cx);
        self.header(ui);
        self.status_bar(ui);
        self.body(ui);
    }

    fn supports(&self, cmd: Cmd) -> bool {
        matches!(cmd, Cmd::Refresh | Cmd::SwapSides)
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context, _cx: &mut Cx<'_>) {
        match cmd {
            Cmd::Refresh => self.reload(ctx),
            Cmd::SwapSides => self.swap(ctx),
            _ => {}
        }
    }

    fn session(&self) -> Option<Session> {
        Some(Session { kind: SessionKind::PictureCompare, left: self.left_path.clone(), right: self.right_path.clone(), ..self.session.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_and_zoom_math() {
        assert!((fit_zoom(vec2(200.0, 100.0), vec2(400.0, 100.0)) - 0.5).abs() < 1e-6);
        assert!((fit_zoom(vec2(200.0, 100.0), vec2(10.0, 10.0)) - 10.0).abs() < 1e-6);
        assert_eq!(fit_zoom(vec2(200.0, 100.0), Vec2::ZERO), 1.0);
        assert_eq!(fit_zoom(vec2(-10.0, 100.0), vec2(10.0, 10.0)), 1.0);
        // Zooming keeps the canvas point under the cursor fixed.
        let (c, canvas, cursor) = (pos2(100.0, 100.0), vec2(50.0, 40.0), pos2(120.0, 90.0));
        let (z, pan) = (1.0, vec2(5.0, -3.0));
        let before = (cursor - origin(c, canvas, z, pan)) / z;
        let pan2 = zoom_about(c, canvas, z, pan, cursor, 4.0);
        let after = (cursor - origin(c, canvas, 4.0, pan2)) / 4.0;
        assert!((before - after).length() < 1e-4);
    }

    #[test]
    fn zoom_steps() {
        assert_eq!(zoom_step(1.0, true), 1.5);
        assert_eq!(zoom_step(1.0, false), 0.75);
        assert_eq!(zoom_step(0.6, true), 0.75);
        assert_eq!(zoom_step(0.6, false), 0.5);
        assert_eq!(zoom_step(MAX_ZOOM, true), MAX_ZOOM);
        assert_eq!(zoom_step(MIN_ZOOM, false), MIN_ZOOM);
        assert_eq!(zoom_step(f32::NAN, true), MAX_ZOOM);
        assert_eq!(file_name("/a/b/pic.png"), "pic.png");
        assert_eq!(file_name(""), "");
    }

    #[test]
    fn pixel_under_pointer() {
        let canvas = vec2(10.0, 5.0);
        assert_eq!(screen_to_pixel(pos2(0.0, 0.0), 2.0, pos2(5.0, 3.0), canvas), Some((2, 1)));
        assert_eq!(screen_to_pixel(pos2(0.0, 0.0), 2.0, pos2(-1.0, 3.0), canvas), None);
        assert_eq!(screen_to_pixel(pos2(0.0, 0.0), 2.0, pos2(20.0, 3.0), canvas), None);
        assert_eq!(fmt_rgba(Some([1, 2, 3, 255])), "  1   2   3 255");
    }

    #[test]
    fn previews_downscale_large_images() {
        assert_eq!(downscale_factor(100, 50, 8192), 1);
        assert_eq!(downscale_factor(16384, 10, 8192), 2);
        assert_eq!(downscale_factor(16385, 10, 8192), 3);
        let mut i = RgbaImage::new(5, 3);
        set_px(&mut i, 4, 2, [9, 8, 7, 6]);
        let p = preview(&i, 8192).unwrap();
        assert_eq!(p.image.size, [5, 3]);
        assert!(!p.downscaled);
        assert_eq!(p.image.pixels.last().copied(), Some(Color32::from_rgba_unmultiplied(9, 8, 7, 6)));
        let p = preview(&i, 2).unwrap();
        assert_eq!(p.image.size, [2, 1]);
        assert_eq!(p.image.pixels.len(), 2);
        assert!(p.downscaled);
        assert!(preview(&RgbaImage::new(0, 0), 8192).is_none());
    }

    fn set_px(i: &mut RgbaImage, x: u32, y: u32, c: [u8; 4]) {
        if let Some(p) = i.get_pixel_mut_checked(x, y) {
            p.0 = c;
        }
    }

    #[test]
    fn view_loads_compares_and_swaps() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.png"), dir.path().join("b.png"));
        let mut img = RgbaImage::new(4, 4);
        img.save(&a).unwrap();
        set_px(&mut img, 1, 1, [200, 0, 0, 255]);
        img.save(&b).unwrap();
        let ctx = egui::Context::default();
        let s = Session::new(SessionKind::PictureCompare, a.display().to_string(), b.display().to_string());
        let mut settings = Settings::default();
        let mut v = PictureView::from_session(&s, &settings, &ctx);
        let (mut changed, mut toasts, mut open, mut exit) = (false, crate::view::Toasts::default(), Vec::new(), 0);
        let mut cx = Cx { settings: &mut settings, settings_changed: &mut changed, toasts: &mut toasts, open: &mut open, exit_code: &mut exit };
        let wait = |v: &mut PictureView, cx: &mut Cx<'_>| {
            for _ in 0..1000 {
                v.poll_jobs(&ctx, cx);
                if v.load_job.is_none() && v.diff_job.is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait(&mut v, &mut cx);
        assert_eq!(v.diff.as_ref().map(|d| d.differing_pixels), Some(1));
        assert!(v.tex.left.is_some() && v.tex.mask.is_some());
        assert_eq!(v.canvas(), vec2(4.0, 4.0));
        let info = side_info(&v.left);
        assert_eq!(info.first().map(String::as_str), Some("4 × 4 px"));
        assert!(info.get(1).is_some_and(|s| s.contains("bytes")));
        assert_eq!(v.pane_titles().len(), 2);
        assert!(v.pane_titles()[0].1.ends_with("a.png"));
        v.mode = Mode::Difference;
        assert_eq!(v.pane_titles().len(), 1);
        v.run(Cmd::SwapSides, &ctx, &mut cx);
        wait(&mut v, &mut cx);
        assert_eq!(v.session().unwrap().left, b.display().to_string());
        assert_eq!(v.diff.as_ref().map(|d| d.differing_pixels), Some(1));
        v.tolerance = 255;
        v.rediff(&ctx);
        wait(&mut v, &mut cx);
        assert!(v.diff.as_ref().unwrap().is_identical());
        assert!(!v.supports(Cmd::NextDiff));
    }

    #[test]
    fn missing_and_bad_files() {
        assert!(load_side("").image.is_none() && load_side("").error.is_none());
        let s = load_side("/definitely/not/here.png");
        assert!(s.image.is_none() && s.error.is_some());
        assert!(side_message(&s).starts_with("could not load"));
        assert!(side_info(&s).is_empty());
        assert_eq!(side_info(&Side::default()), vec!["No file".to_owned()]);
    }
}
