//! The Home tab: a Sessions tree (new, auto-saved, saved) on the left, details of the selected
//! session with Open/Edit, large tiles to start a new comparison, and a tips strip.

use diffcraft_session::{Session, SessionKind, now_secs};
use egui::{Align2, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::icons::{self, Icon};
use crate::theme::Palette;
use crate::toolbar;
use crate::view::{Cmd, Cx, OpenRequest, View};
use crate::widgets;

pub const HOME_TITLE: &str = "Home";

/// Which list a selected session lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sel {
    Recent(usize),
    Saved(usize),
}

/// Age buckets for auto-saved sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    Today,
    Yesterday,
    ThisWeek,
    Older,
}

impl Age {
    const ALL: [Age; 4] = [Age::Today, Age::Yesterday, Age::ThisWeek, Age::Older];

    fn label(self) -> &'static str {
        match self {
            Age::Today => "Today",
            Age::Yesterday => "Yesterday",
            Age::ThisWeek => "Earlier this week",
            Age::Older => "Older",
        }
    }

    /// Bucket for a session last used at `t` (seconds) seen at `now`.
    pub fn of(t: u64, now: u64) -> Age {
        let day = 86_400;
        let today_start = now - now % day;
        if t >= today_start {
            Age::Today
        } else if t >= today_start.saturating_sub(day) {
            Age::Yesterday
        } else if t >= today_start.saturating_sub(6 * day) {
            Age::ThisWeek
        } else {
            Age::Older
        }
    }
}

struct EditForm {
    sel: Sel,
    name: String,
    left: String,
    right: String,
}

pub struct HomeView {
    sel: Option<Sel>,
    filter: String,
    open_new: bool,
    open_auto: bool,
    open_saved: bool,
    closed_ages: Vec<Age>,
    edit: Option<EditForm>,
    left: String,
    right: String,
}

impl Default for HomeView {
    fn default() -> Self {
        HomeView {
            sel: None,
            filter: String::new(),
            open_new: true,
            open_auto: true,
            open_saved: true,
            closed_ages: vec![Age::Older],
            edit: None,
            left: String::new(),
            right: String::new(),
        }
    }
}

/// One row of the sessions tree. `expand` is `Some(open)` for a branch.
fn tree_row(ui: &mut Ui, depth: usize, icon: Option<Icon>, label: &str, selected: bool, expand: Option<bool>) -> egui::Response {
    let pal = Palette::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 2.0, pal.active_bg);
    } else if resp.hovered() {
        p.rect_filled(rect, 2.0, pal.hover_bg);
    }
    let x = rect.left() + 6.0 + depth as f32 * 16.0;
    if let Some(open) = expand {
        let c = pos2(x + 4.0, rect.center().y);
        let pts = if open {
            vec![c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0), c + vec2(0.0, 2.5)]
        } else {
            vec![c + vec2(-2.0, -3.5), c + vec2(2.5, 0.0), c + vec2(-2.0, 3.5)]
        };
        p.add(egui::Shape::convex_polygon(pts, pal.dim_text, Stroke::NONE));
    }
    let mut tx = x + 14.0;
    if let Some(icon) = icon {
        icons::paint(p, Rect::from_center_size(pos2(tx + 7.0, rect.center().y), vec2(15.0, 15.0)), icon, &pal, true);
        tx += 20.0;
    }
    let clip = Rect::from_min_max(pos2(tx, rect.top()), rect.max);
    p.with_clip_rect(clip).text(pos2(tx, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), pal.text);
    resp
}

fn matches(s: &Session, filter: &str) -> bool {
    if filter.trim().is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    s.title().to_lowercase().contains(&f) || s.left.to_lowercase().contains(&f) || s.right.to_lowercase().contains(&f)
}

fn new_session(kind: SessionKind, cx: &Cx<'_>) -> Session {
    Session { kind, text: cx.settings.default_text.clone(), folder: cx.settings.default_folder.clone(), ..Session::default() }
}

impl HomeView {
    fn selected<'s>(&self, cx: &'s Cx<'_>) -> Option<&'s Session> {
        match self.sel? {
            Sel::Recent(i) => cx.settings.recent.get(i),
            Sel::Saved(i) => cx.settings.saved.get(i),
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let pal = Palette::get(ui.ctx());
        egui::Panel::left("home-sessions").resizable(true).default_size(250.0).size_range(180.0..=480.0).frame(egui::Frame::NONE.fill(pal.panel_bg)).show(
            ui,
            |ui| {
                toolbar::header_strip(ui, 24.0, |ui| {
                    ui.label(RichText::new("Sessions").strong());
                });
                // Bottom: add/remove + filter.
                egui::Panel::bottom("home-sessions-bottom").frame(egui::Frame::NONE.fill(pal.header_bg).inner_margin(egui::Margin::symmetric(4, 4))).show(
                    ui,
                    |ui| {
                        ui.horizontal(|ui| {
                            let add = toolbar::small_resp(ui, Icon::Plus, "New session", true);
                            egui::Popup::menu(&add).show(|ui| {
                                for k in SessionKind::ALL {
                                    if toolbar::menu_item(ui, icons::session_icon(k), k.label(), "", true) {
                                        cx.open.push(OpenRequest::Session(Box::new(new_session(k, cx))));
                                        ui.close();
                                    }
                                }
                            });
                            if toolbar::small(ui, Icon::Minus, "Remove the selected session from the list", self.sel.is_some()) {
                                self.remove_selected(cx);
                            }
                            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter sessions").desired_width(f32::INFINITY));
                        });
                    },
                );
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add_space(4.0);
                    if tree_row(ui, 0, Some(Icon::Plus), "New", false, Some(self.open_new)).clicked() {
                        self.open_new = !self.open_new;
                    }
                    if self.open_new {
                        for k in SessionKind::ALL {
                            let r = tree_row(ui, 1, Some(icons::session_icon(k)), k.label(), false, None).on_hover_text(k.description());
                            if r.clicked() {
                                cx.open.push(OpenRequest::Session(Box::new(new_session(k, cx))));
                            }
                        }
                    }
                    if tree_row(ui, 0, Some(Icon::Sessions), "Auto-saved", false, Some(self.open_auto)).clicked() {
                        self.open_auto = !self.open_auto;
                    }
                    if self.open_auto {
                        let now = now_secs();
                        for age in Age::ALL {
                            let items: Vec<usize> = cx
                                .settings
                                .recent
                                .iter()
                                .enumerate()
                                .filter(|(_, s)| Age::of(s.last_used, now) == age && matches(s, &self.filter))
                                .map(|(i, _)| i)
                                .collect();
                            if items.is_empty() {
                                continue;
                            }
                            let open = !self.closed_ages.contains(&age);
                            if tree_row(ui, 1, Some(Icon::Folder), age.label(), false, Some(open)).clicked() {
                                if open {
                                    self.closed_ages.push(age);
                                } else {
                                    self.closed_ages.retain(|a| *a != age);
                                }
                            }
                            if open {
                                for i in items {
                                    let Some(s) = cx.settings.recent.get(i) else { continue };
                                    let r = tree_row(ui, 2, Some(icons::session_icon(s.kind)), &s.title(), self.sel == Some(Sel::Recent(i)), None)
                                        .on_hover_text(format!("{}\n{}\n{}", s.kind.label(), s.left, s.right));
                                    if r.clicked() {
                                        self.sel = Some(Sel::Recent(i));
                                        self.edit = None;
                                    }
                                    if r.double_clicked() {
                                        cx.open.push(OpenRequest::Session(Box::new(s.clone())));
                                    }
                                }
                            }
                        }
                    }
                    if tree_row(ui, 0, Some(Icon::Save), "Saved", false, Some(self.open_saved)).clicked() {
                        self.open_saved = !self.open_saved;
                    }
                    if self.open_saved {
                        if cx.settings.saved.is_empty() {
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                ui.add_space(40.0);
                                ui.label(RichText::new("Session › Save Session… adds one").color(pal.dim_text).small());
                            });
                        }
                        for (i, s) in cx.settings.saved.iter().enumerate() {
                            if !matches(s, &self.filter) {
                                continue;
                            }
                            let r = tree_row(ui, 1, Some(icons::session_icon(s.kind)), &s.title(), self.sel == Some(Sel::Saved(i)), None)
                                .on_hover_text(format!("{}\n{}\n{}", s.kind.label(), s.left, s.right));
                            if r.clicked() {
                                self.sel = Some(Sel::Saved(i));
                                self.edit = None;
                            }
                            if r.double_clicked() {
                                cx.open.push(OpenRequest::Session(Box::new(s.clone())));
                            }
                        }
                    }
                });
            },
        );
    }

    fn remove_selected(&mut self, cx: &mut Cx<'_>) {
        match self.sel.take() {
            Some(Sel::Recent(i)) if i < cx.settings.recent.len() => {
                cx.settings.recent.remove(i);
            }
            Some(Sel::Saved(i)) if i < cx.settings.saved.len() => {
                cx.settings.saved.remove(i);
            }
            _ => return,
        }
        self.edit = None;
        *cx.settings_changed = true;
    }

    /// Top of the main area: details of the selected session, or quick compare.
    fn details(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let pal = Palette::get(ui.ctx());
        let selected = self.selected(cx).cloned();
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(20, 14)).show(ui, |ui| {
            let Some(s) = selected else {
                crate::brand::wordmark(ui, 40.0);
                ui.label(RichText::new("Compare and merge files, folders, binaries and pictures").color(pal.dim_text));
                ui.add_space(14.0);
                ui.label(RichText::new("Quick compare").size(16.0).strong());
                ui.label(
                    RichText::new("Choose two files or folders, or drop them onto the window. The right kind of comparison is picked for you.")
                        .color(pal.dim_text),
                );
                ui.add_space(6.0);
                ui.set_max_width(760.0);
                let l = widgets::path_bar(ui, "home-left", &mut self.left, false, "Left file or folder");
                let r = widgets::path_bar(ui, "home-right", &mut self.right, false, "Right file or folder");
                ui.horizontal(|ui| {
                    let ready = !self.left.trim().is_empty() && !self.right.trim().is_empty();
                    let go = ui.add_enabled(ready, egui::Button::new("   Compare   ")).clicked();
                    if ui.button("Swap").clicked() {
                        std::mem::swap(&mut self.left, &mut self.right);
                    }
                    if ready && (go || l.submitted || r.submitted) {
                        cx.open.push(OpenRequest::Paths { left: widgets::clean_path(&self.left), right: widgets::clean_path(&self.right), kind: None });
                    }
                });
                return;
            };
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                icons::paint(ui.painter(), r, icons::session_icon(s.kind), &pal, true);
                ui.label(RichText::new(s.title()).size(17.0).strong());
                ui.label(RichText::new(s.kind.label()).color(pal.dim_text));
            });
            ui.add_space(4.0);
            let path_line = |ui: &mut Ui, label: &str, p: &str, folder: bool| {
                if p.is_empty() {
                    return;
                }
                ui.horizontal(|ui| {
                    ui.add_sized([48.0, 18.0], egui::Label::new(RichText::new(label).color(pal.dim_text)));
                    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    icons::paint(ui.painter(), r, if folder { Icon::Folder } else { Icon::File }, &pal, true);
                    ui.label(RichText::new(p).monospace());
                });
            };
            let folder = s.kind.is_folder();
            path_line(ui, "Left", &s.left, folder);
            path_line(ui, "Right", &s.right, folder);
            if let Some(b) = &s.base {
                path_line(ui, "Base", b, false);
            }
            if let Some(o) = &s.output {
                path_line(ui, "Output", o, false);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("     Open     ")).clicked() {
                    cx.open.push(OpenRequest::Session(Box::new(s.clone())));
                }
                if ui.add(egui::Button::new("     Edit     ")).clicked()
                    && let Some(sel) = self.sel
                {
                    self.edit = Some(EditForm { sel, name: s.name.clone(), left: s.left.clone(), right: s.right.clone() });
                }
                if matches!(self.sel, Some(Sel::Recent(_)))
                    && ui.add(egui::Button::new("  Keep in Saved  ")).on_hover_text("Copy this session to the Saved list").clicked()
                {
                    let mut named = s.clone();
                    if named.name.trim().is_empty() {
                        named.name = s.title();
                    }
                    cx.settings.save_session(named);
                    *cx.settings_changed = true;
                    cx.toasts.success("Session saved");
                }
            });
            let mut close_edit = false;
            if let Some(f) = &mut self.edit {
                ui.add_space(8.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_max_width(700.0);
                    egui::Grid::new("home-edit").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(520.0));
                        ui.end_row();
                        ui.label("Left");
                        widgets::path_bar(ui, "home-edit-left", &mut f.left, folder, "Left path");
                        ui.end_row();
                        ui.label("Right");
                        widgets::path_bar(ui, "home-edit-right", &mut f.right, folder, "Right path");
                        ui.end_row();
                    });
                    let mut done = None;
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(false);
                        }
                    });
                    if done == Some(true) {
                        let slot = match f.sel {
                            Sel::Recent(i) => cx.settings.recent.get_mut(i),
                            Sel::Saved(i) => cx.settings.saved.get_mut(i),
                        };
                        if let Some(slot) = slot {
                            slot.name = f.name.trim().to_owned();
                            slot.left = f.left.trim().to_owned();
                            slot.right = f.right.trim().to_owned();
                            *cx.settings_changed = true;
                        }
                    }
                    close_edit = done.is_some();
                });
            }
            if close_edit {
                self.edit = None;
            }
        });
    }

    fn tiles(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let pal = Palette::get(ui.ctx());
        let tile = vec2(150.0, 118.0);
        let gap = 18.0;
        let cols = (((ui.available_width() + gap) / (tile.x + gap)).floor() as usize).clamp(1, 6);
        let rows = SessionKind::ALL.len().div_ceil(cols);
        let total = vec2(cols as f32 * (tile.x + gap) - gap, rows as f32 * (tile.y + gap) - gap);
        let avail = ui.available_rect_before_wrap();
        let origin = pos2(avail.center().x - total.x / 2.0, avail.top() + ((avail.height() - total.y) / 2.0).max(10.0));
        for (k, kind) in SessionKind::ALL.into_iter().enumerate() {
            let (c, r) = (k % cols, k / cols);
            let rect = Rect::from_min_size(origin + vec2(c as f32 * (tile.x + gap), r as f32 * (tile.y + gap)), tile);
            let resp = ui.interact(rect, ui.id().with(("tile", k)), Sense::click()).on_hover_text(kind.description());
            let p = ui.painter();
            if resp.hovered() {
                p.rect(rect, 6.0, pal.hover_bg, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
            }
            icons::paint_session_tile(p, Rect::from_center_size(rect.center_top() + vec2(0.0, 44.0), vec2(72.0, 72.0)), kind, &pal);
            p.text(rect.center_bottom() - vec2(0.0, 16.0), Align2::CENTER_CENTER, kind.label(), FontId::proportional(13.5), pal.text);
            if resp.clicked() {
                cx.open.push(OpenRequest::Session(Box::new(new_session(kind, cx))));
            }
        }
        ui.allocate_rect(avail, Sense::hover());
    }

    fn tips(&self, ui: &mut Ui) {
        let pal = Palette::get(ui.ctx());
        egui::Panel::bottom("home-tips").frame(egui::Frame::NONE.fill(pal.active_bg).inner_margin(egui::Margin::symmetric(18, 12))).show(ui, |ui| {
            ui.columns(3, |cols| {
                let tip = |ui: &mut Ui, icon: Icon, title: &str, body: &str| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                        icons::paint(ui.painter(), r, icon, &pal, true);
                        ui.label(RichText::new(title).strong());
                    });
                    ui.label(RichText::new(body).size(12.0));
                };
                if let [a, b, c] = cols {
                    tip(
                        a,
                        Icon::Help,
                        "KEYBOARD",
                        "Ctrl+N / Ctrl+P jump between differences, Ctrl+R / Ctrl+L copy them across. Help › Keyboard Shortcuts lists everything.",
                    );
                    tip(b, Icon::Git, "GIT", "Tools › Git Integration… makes DiffCraft your difftool and mergetool, or run `diffcraft-cli setup-git --apply`.");
                    tip(
                        c,
                        Icon::ShowAll,
                        "COMMAND LINE",
                        "`diffcraft left right` opens a comparison; `diffcraft-cli diff`, `merge`, `report` and `sync` run without a window.",
                    );
                }
            });
        });
    }
}

impl View for HomeView {
    fn kind(&self) -> SessionKind {
        SessionKind::TextCompare
    }

    fn title(&self) -> String {
        HOME_TITLE.to_owned()
    }

    fn tooltip(&self) -> String {
        "Start a comparison or reopen a session".to_owned()
    }

    fn is_home(&self) -> bool {
        true
    }

    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        // Keep the selection valid when lists change elsewhere.
        let valid = match self.sel {
            Some(Sel::Recent(i)) => i < cx.settings.recent.len(),
            Some(Sel::Saved(i)) => i < cx.settings.saved.len(),
            None => true,
        };
        if !valid {
            self.sel = None;
            self.edit = None;
        }
        self.sidebar(ui, cx);
        self.tips(ui);
        let pal = Palette::get(ui.ctx());
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(pal.pane_bg)).show(ui, |ui| {
            egui::Panel::top("home-details").frame(egui::Frame::NONE.fill(pal.pane_bg)).show(ui, |ui| {
                self.details(ui, cx);
                let r = ui.max_rect();
                ui.painter().line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, pal.border));
            });
            self.tiles(ui, cx);
        });
    }

    fn supports(&self, cmd: Cmd) -> bool {
        matches!(cmd, Cmd::Delete) && self.sel.is_some()
    }

    fn run(&mut self, cmd: Cmd, _ctx: &egui::Context, cx: &mut Cx<'_>) {
        if cmd == Cmd::Delete {
            self.remove_selected(cx);
        }
    }

    fn session(&self) -> Option<Session> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_buckets() {
        let day = 86_400;
        let now = 100 * day + 3600;
        assert_eq!(Age::of(now - 10, now), Age::Today);
        assert_eq!(Age::of(100 * day - 1, now), Age::Yesterday);
        assert_eq!(Age::of(96 * day, now), Age::ThisWeek);
        assert_eq!(Age::of(10, now), Age::Older);
        assert_eq!(Age::of(0, 0), Age::Today);
    }

    #[test]
    fn filter_matches_title_and_paths() {
        let s = Session::new(SessionKind::TextCompare, "/a/alpha.txt", "/b/beta.txt");
        assert!(matches(&s, ""));
        assert!(matches(&s, "ALPHA"));
        assert!(matches(&s, "/b/"));
        assert!(!matches(&s, "gamma"));
    }
}
