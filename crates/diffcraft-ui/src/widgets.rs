//! Widgets shared by the views: path bars, the difference overview strip, confirmations.

use std::path::{Path, PathBuf};

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::theme::Palette;

/// What happened in a [`path_bar`].
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathBarResult {
    /// Enter was pressed or a path was picked with Browse: (re)load.
    pub submitted: bool,
}

/// An editable path with a Browse button. `folder` picks folders instead of files.
pub fn path_bar(ui: &mut Ui, id: &str, path: &mut String, folder: bool, hint: &str) -> PathBarResult {
    let mut res = PathBarResult::default();
    ui.horizontal(|ui| {
        let browse_w = 28.0;
        let w = (ui.available_width() - browse_w - ui.spacing().item_spacing.x).max(60.0);
        let edit = egui::TextEdit::singleline(path).id_salt(id).hint_text(hint).desired_width(w);
        let r = ui.add(edit);
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            res.submitted = true;
        }
        let start = start_dir(path);
        if crate::toolbar::small(ui, crate::icons::Icon::FolderOpen, if folder { "Browse for a folder" } else { "Browse for a file" }, true) {
            let picked = if folder { pick_folder(start.as_deref()) } else { pick_file(start.as_deref()) };
            if let Some(p) = picked {
                *path = p.display().to_string();
                res.submitted = true;
            }
        }
    });
    res
}

fn start_dir(path: &str) -> Option<PathBuf> {
    let p = Path::new(path.trim());
    if p.as_os_str().is_empty() {
        return None;
    }
    if p.is_dir() { Some(p.to_path_buf()) } else { p.parent().filter(|d| d.is_dir()).map(Path::to_path_buf) }
}

pub fn pick_file(start: Option<&Path>) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new();
    if let Some(s) = start {
        d = d.set_directory(s);
    }
    d.pick_file()
}

pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new();
    if let Some(s) = start {
        d = d.set_directory(s);
    }
    d.pick_folder()
}

/// Ask for a destination file. `name` is the suggested file name.
pub fn save_file(start: Option<&Path>, name: &str, filter: Option<(&str, &[&str])>) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new().set_file_name(name);
    if let Some(s) = start {
        d = d.set_directory(s);
    }
    if let Some((label, exts)) = filter {
        d = d.add_filter(label, exts);
    }
    d.save_file()
}

/// Trim and expand a user-typed path (`~` → home directory, surrounding quotes removed).
pub fn clean_path(s: &str) -> PathBuf {
    let s = s.trim().trim_matches('"').trim();
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\"))
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    if s == "~"
        && let Some(home) = home_dir()
    {
        return home;
    }
    PathBuf::from(s)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// A mark in the overview strip: a fraction range of the document and a colour.
pub struct OverviewMark {
    pub start: f32,
    pub end: f32,
    pub color: Color32,
}

/// The thin strip at the right edge showing where the differences are. Clicking or dragging
/// returns the fraction (0..1) to scroll to. `view` is the visible fraction range.
pub fn overview_strip(ui: &mut Ui, rect: Rect, marks: &[OverviewMark], view: (f32, f32), pal: &Palette) -> Option<f32> {
    let resp = ui.interact(rect, ui.id().with("overview"), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, pal.gutter_bg);
    let h = rect.height().max(1.0);
    for m in marks {
        let y0 = rect.top() + m.start.clamp(0.0, 1.0) * h;
        let y1 = (rect.top() + m.end.clamp(0.0, 1.0) * h).max(y0 + 2.0);
        p.rect_filled(Rect::from_min_max(egui::pos2(rect.left() + 2.0, y0), egui::pos2(rect.right() - 2.0, y1)), 1.0, m.color);
    }
    let vy0 = rect.top() + view.0.clamp(0.0, 1.0) * h;
    let vy1 = (rect.top() + view.1.clamp(0.0, 1.0) * h).max(vy0 + 4.0);
    p.rect_stroke(
        Rect::from_min_max(egui::pos2(rect.left() + 0.5, vy0), egui::pos2(rect.right() - 0.5, vy1)),
        2.0,
        Stroke::new(1.0, pal.dim_text),
        StrokeKind::Inside,
    );
    if (resp.clicked() || resp.dragged())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        return Some(((pos.y - rect.top()) / h).clamp(0.0, 1.0));
    }
    None
}

/// A horizontal scrollbar for panes that scroll together. Returns the new offset.
pub fn hscrollbar(ui: &mut Ui, rect: Rect, offset: f32, content_w: f32, view_w: f32, pal: &Palette) -> f32 {
    let max = (content_w - view_w).max(0.0);
    if max <= 0.0 {
        return 0.0;
    }
    let id = ui.id().with(("hscroll", rect.left() as i32));
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, pal.gutter_bg);
    let frac_w = (view_w / content_w).clamp(0.05, 1.0);
    let thumb_w = rect.width() * frac_w;
    let x = rect.left() + (rect.width() - thumb_w) * (offset / max).clamp(0.0, 1.0);
    let thumb = Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), vec2(thumb_w, rect.height() - 4.0));
    p.rect_filled(thumb, 3.0, if resp.hovered() || resp.dragged() { pal.dim_text } else { pal.dim_text.gamma_multiply(0.6) });
    if (resp.dragged() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - rect.left() - thumb_w / 2.0) / (rect.width() - thumb_w).max(1.0)).clamp(0.0, 1.0);
        return f * max;
    }
    offset.clamp(0.0, max)
}

/// A modal yes/no question. Returns `Some(true)` on confirm, `Some(false)` on cancel/Esc.
pub fn confirm_modal(ctx: &egui::Context, id: &str, title: &str, body: &str, ok: &str) -> Option<bool> {
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new(id)).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.heading(title);
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            ui.label(body);
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button(ok).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    answer
}

/// Human-readable byte size.
pub fn format_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS.get(u).copied().unwrap_or("B")) }
}

/// Number with thousands separators.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Local-time-free timestamp for listings (`YYYY-MM-DD HH:MM`, UTC).
pub fn format_mtime(t: Option<std::time::SystemTime>) -> String {
    match t {
        Some(t) => {
            let s = diffcraft_report::format_time(t);
            s.get(..16).unwrap_or(&s).to_owned()
        }
        None => String::new(),
    }
}

/// Paint a centered message in an empty pane.
pub fn empty_message(ui: &mut Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(egui::RichText::new(text).color(Palette::get(ui.ctx()).dim_text));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(format_size(10), "10 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(12), "12");
        assert_eq!(clean_path("  \"/tmp/x\" "), PathBuf::from("/tmp/x"));
    }
}
