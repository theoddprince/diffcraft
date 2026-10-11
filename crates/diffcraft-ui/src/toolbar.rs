//! Toolbars: large icon-over-label buttons, toggles, drop-down buttons and small icon buttons.

use egui::{Align2, FontId, Margin, Rect, Response, Sense, Stroke, Ui, vec2};

use crate::icons::{self, Icon};
use crate::theme::Palette;

const BIG_H: f32 = 46.0;
const ICON: f32 = 20.0;

/// A full-width toolbar strip at the top of the current area.
pub fn bar(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let pal = Palette::get(ui.ctx());
    egui::Panel::top(id.to_owned()).frame(egui::Frame::NONE.fill(pal.toolbar_bg).inner_margin(Margin::symmetric(6, 3))).show(ui, |ui| {
        let r = ui.max_rect();
        ui.painter().line_segment([r.left_bottom() + vec2(-6.0, 3.0), r.right_bottom() + vec2(6.0, 3.0)], Stroke::new(1.0, pal.border));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 1.0;
            add(ui);
        });
    });
}

fn big(ui: &mut Ui, icon: Icon, label: &str, selected: bool, enabled: bool, dropdown: bool) -> Response {
    let pal = Palette::get(ui.ctx());
    let font = FontId::proportional(11.5);
    let label_w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), pal.text).size().x);
    let w = (label_w + 14.0 + if dropdown { 8.0 } else { 0.0 }).max(44.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, BIG_H), sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if selected {
            p.rect(rect, 3.0, pal.active_bg, Stroke::new(1.0, pal.accent.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        } else if enabled && resp.hovered() {
            p.rect(rect, 3.0, pal.hover_bg, Stroke::NONE, egui::StrokeKind::Inside);
        }
        let ir = Rect::from_center_size(rect.center_top() + vec2(0.0, 5.0 + ICON / 2.0), vec2(ICON, ICON));
        icons::paint(p, ir, icon, &pal, enabled);
        let tc = if enabled { pal.text } else { pal.dim_text.gamma_multiply(0.7) };
        let ty = rect.bottom() - 9.0;
        let tx = rect.center().x - if dropdown { 4.0 } else { 0.0 };
        p.text(egui::pos2(tx, ty), Align2::CENTER_CENTER, label, font, tc);
        if dropdown {
            let c = egui::pos2(rect.right() - 7.0, ty);
            p.add(egui::Shape::convex_polygon(vec![c + vec2(-3.0, -1.5), c + vec2(3.0, -1.5), c + vec2(0.0, 2.0)], tc, Stroke::NONE));
        }
    }
    resp
}

/// A large toolbar button. Returns true when clicked.
pub fn button(ui: &mut Ui, icon: Icon, label: &str, tip: &str, enabled: bool) -> bool {
    big(ui, icon, label, false, enabled, false).on_hover_text(tip).clicked()
}

/// A large toggle button shown pressed when `selected`.
pub fn toggle(ui: &mut Ui, icon: Icon, label: &str, tip: &str, selected: bool, enabled: bool) -> bool {
    big(ui, icon, label, selected, enabled, false).on_hover_text(tip).clicked()
}

/// A large button with a drop-down arrow that opens a menu.
pub fn menu(ui: &mut Ui, icon: Icon, label: &str, tip: &str, enabled: bool, add: impl FnOnce(&mut Ui)) {
    let resp = big(ui, icon, label, false, enabled, true).on_hover_text(tip);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(220.0);
        add(ui);
    });
}

/// A vertical separator between toolbar groups.
pub fn separator(ui: &mut Ui) {
    let pal = Palette::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, BIG_H), Sense::hover());
    ui.painter().line_segment([rect.center_top() + vec2(0.0, 6.0), rect.center_bottom() - vec2(0.0, 6.0)], Stroke::new(1.0, pal.border));
}

/// A small square icon button (path bars, pane headers). Returns true when clicked.
pub fn small(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool) -> bool {
    small_resp(ui, icon, tip, enabled).clicked()
}

pub fn small_resp(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool) -> Response {
    let pal = Palette::get(ui.ctx());
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 22.0), sense);
    if enabled && resp.hovered() {
        ui.painter().rect(rect, 3.0, pal.hover_bg, Stroke::NONE, egui::StrokeKind::Inside);
    }
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, &pal, enabled);
    resp.on_hover_text(tip)
}

/// A menu entry with an icon.
pub fn menu_item(ui: &mut Ui, icon: Icon, label: &str, shortcut: &str, enabled: bool) -> bool {
    let pal = Palette::get(ui.ctx());
    let b = egui::Button::new(format!("      {label}")).shortcut_text(shortcut);
    let r = ui.add_enabled(enabled, b);
    let ir = Rect::from_center_size(egui::pos2(r.rect.left() + 13.0, r.rect.center().y), vec2(15.0, 15.0));
    icons::paint(ui.painter(), ir, icon, &pal, enabled);
    r.clicked()
}

/// A slim header strip (pane titles, file info) with the given height.
pub fn header_strip(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
    let pal = Palette::get(ui.ctx());
    egui::Frame::NONE.fill(pal.header_bg).inner_margin(Margin::symmetric(6, 1)).show(ui, |ui| {
        ui.allocate_ui_with_layout(vec2(ui.available_width(), height), egui::Layout::left_to_right(egui::Align::Center), add);
    });
}

/// The status bar at the bottom of a view.
pub fn status_bar(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    let pal = Palette::get(ui.ctx());
    egui::Panel::bottom(id.to_owned()).frame(egui::Frame::NONE.fill(pal.header_bg).inner_margin(Margin::symmetric(8, 3))).show(ui, |ui| {
        let r = ui.max_rect();
        ui.painter().line_segment([r.left_top() + vec2(-8.0, -3.0), r.right_top() + vec2(8.0, -3.0)], Stroke::new(1.0, pal.border));
        ui.horizontal(|ui| {
            ui.set_min_height(18.0);
            add(ui);
        });
    });
}

/// A thin vertical divider between status bar segments.
pub fn status_sep(ui: &mut Ui) {
    let pal = Palette::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 16.0), Sense::hover());
    ui.painter().line_segment([rect.center_top(), rect.center_bottom()], Stroke::new(1.0, pal.border));
}

/// A status bar segment with an icon.
pub fn status_icon(ui: &mut Ui, icon: Icon) {
    let pal = Palette::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    icons::paint(ui.painter(), rect.shrink(1.0), icon, &pal, true);
}

/// The Home and Sessions buttons every comparison toolbar starts with.
pub fn home_and_sessions(ui: &mut Ui, cx: &mut crate::view::Cx<'_>) {
    use crate::view::OpenRequest;
    if button(ui, Icon::Home, "Home", "Show the Home tab (Ctrl+T)", true) {
        cx.open.push(OpenRequest::Home);
    }
    let recent = cx.settings.recent.clone();
    let saved = cx.settings.saved.clone();
    menu(ui, Icon::Sessions, "Sessions", "Open a recent or saved session", true, |ui| {
        if !saved.is_empty() {
            ui.label(egui::RichText::new("Saved").weak());
            for s in saved.iter().take(15) {
                if menu_item(ui, icons::session_icon(s.kind), &s.title(), "", true) {
                    cx.open.push(OpenRequest::Session(Box::new(s.clone())));
                    ui.close();
                }
            }
            ui.separator();
        }
        ui.label(egui::RichText::new("Recent").weak());
        if recent.is_empty() {
            ui.label(egui::RichText::new("No recent sessions").weak());
        }
        for s in recent.iter().take(15) {
            if menu_item(ui, icons::session_icon(s.kind), &s.title(), "", true) {
                cx.open.push(OpenRequest::Session(Box::new(s.clone())));
                ui.close();
            }
        }
    });
    separator(ui);
}
