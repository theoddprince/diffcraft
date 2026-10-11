//! The Options dialog.

use diffcraft_session::{FileFormatRule, Settings, Theme};

use crate::rules_ui;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Text,
    Folder,
    FileFormats,
    HexPicture,
}

pub struct OptionsState {
    draft: Settings,
    page: Page,
    folder_filter: String,
    selected_rule: Option<usize>,
}

pub enum Outcome {
    Open,
    Apply(Box<Settings>),
    Cancel,
}

impl OptionsState {
    pub fn new(s: &Settings) -> Self {
        OptionsState { folder_filter: s.default_folder.filter.to_spec(), draft: s.clone(), page: Page::General, selected_rule: None }
    }
}

pub fn show(ctx: &egui::Context, st: &mut OptionsState) -> Outcome {
    let mut outcome = Outcome::Open;
    let mut open = true;
    egui::Window::new("Options").open(&mut open).collapsible(false).resizable(true).default_size([640.0, 480.0]).show(ctx, |ui| {
        ui.horizontal(|ui| {
            for (p, l) in [
                (Page::General, "General"),
                (Page::Text, "Text rules"),
                (Page::Folder, "Folder compare"),
                (Page::FileFormats, "File formats"),
                (Page::HexPicture, "Hex & picture"),
            ] {
                ui.selectable_value(&mut st.page, p, l);
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(ui.available_height() - 40.0).auto_shrink([false, true]).show(ui, |ui| match st.page {
            Page::General => general(ui, &mut st.draft),
            Page::Text => {
                ui.label("Default rules for text comparisons (a matching file-format rule takes precedence):");
                rules_ui::text_rules(ui, &mut st.draft.default_text, "opt-text");
            }
            Page::Folder => {
                rules_ui::folder_rules(ui, &mut st.draft.default_folder, &mut st.folder_filter, "opt-folder");
            }
            Page::FileFormats => file_formats(ui, st),
            Page::HexPicture => {
                ui.horizontal(|ui| {
                    ui.label("Hex bytes per row:");
                    for n in [8usize, 16, 32] {
                        ui.radio_value(&mut st.draft.hex_bytes_per_row, n, n.to_string());
                    }
                });
                ui.checkbox(&mut st.draft.hex_align, "Align inserted/deleted bytes (slower on large files)");
                ui.horizontal(|ui| {
                    ui.label("Picture tolerance:");
                    ui.add(egui::Slider::new(&mut st.draft.picture_tolerance, 0..=255));
                });
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("OK").clicked() {
                let mut s = st.draft.clone();
                s.sanitize();
                outcome = Outcome::Apply(Box::new(s));
            }
            if ui.button("Cancel").clicked() {
                outcome = Outcome::Cancel;
            }
            if ui.button("Restore Defaults").clicked() {
                let keep = (st.draft.recent.clone(), st.draft.saved.clone());
                st.draft = Settings { recent: keep.0, saved: keep.1, ..Settings::default() };
                st.folder_filter = st.draft.default_folder.filter.to_spec();
            }
        });
    });
    if !open {
        outcome = Outcome::Cancel;
    }
    outcome
}

fn general(ui: &mut egui::Ui, s: &mut Settings) {
    ui.horizontal(|ui| {
        ui.label("Theme:");
        for t in Theme::ALL {
            ui.radio_value(&mut s.theme, t, t.label());
        }
    });
    ui.horizontal(|ui| {
        ui.label("Text size:");
        ui.add(egui::Slider::new(&mut s.font_size, 8.0..=32.0).step_by(1.0));
    });
    ui.horizontal(|ui| {
        ui.label("Interface scale:");
        ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0).step_by(0.05));
    });
    ui.horizontal(|ui| {
        ui.label("Tab width:");
        ui.add(egui::DragValue::new(&mut s.tab_width).range(1..=16));
    });
    ui.checkbox(&mut s.show_line_numbers, "Show line numbers");
    ui.checkbox(&mut s.intraline_highlight, "Highlight differences within lines");
    ui.checkbox(&mut s.auto_reload, "Reload files changed on disk (when not edited)");
    ui.separator();
    ui.checkbox(&mut s.confirm_file_operations, "Confirm before copying, moving or deleting files");
    ui.checkbox(&mut s.delete_to_trash, "Delete to the Recycle Bin / Trash");
    ui.checkbox(&mut s.backup_on_save, "Keep a .bak copy when saving over a file");
}

fn file_formats(ui: &mut egui::Ui, st: &mut OptionsState) {
    ui.label("Rules applied to text comparisons of matching files (first match wins).");
    ui.horizontal(|ui| {
        if ui.button("Add").clicked() {
            st.draft.file_rules.push(FileFormatRule { name: "New rule".into(), masks: "*.ext".into(), ..FileFormatRule::default() });
            st.selected_rule = Some(st.draft.file_rules.len() - 1);
        }
        if ui.add_enabled(st.selected_rule.is_some(), egui::Button::new("Remove")).clicked()
            && let Some(i) = st.selected_rule.take()
            && i < st.draft.file_rules.len()
        {
            st.draft.file_rules.remove(i);
        }
        if ui.button("Add common presets").clicked() {
            for (name, masks, ws) in [
                ("C, C++, C#, Java, Rust, Go", "*.c;*.h;*.cpp;*.hpp;*.cc;*.cs;*.java;*.rs;*.go", diffcraft_core::text::WhitespaceMode::IgnoreLeadingTrailing),
                ("Scripts", "*.py;*.js;*.ts;*.sh;*.ps1;*.rb", diffcraft_core::text::WhitespaceMode::Compare),
                ("Markup and data", "*.xml;*.html;*.json;*.yaml;*.yml;*.toml", diffcraft_core::text::WhitespaceMode::IgnoreChanges),
            ] {
                if !st.draft.file_rules.iter().any(|r| r.name == name) {
                    let mut r = FileFormatRule { name: name.into(), masks: masks.into(), ..FileFormatRule::default() };
                    r.text.whitespace = ws;
                    st.draft.file_rules.push(r);
                }
            }
        }
    });
    ui.separator();
    for (i, r) in st.draft.file_rules.iter().enumerate() {
        if ui.selectable_label(st.selected_rule == Some(i), format!("{}  —  {}", r.name, r.masks)).clicked() {
            st.selected_rule = Some(i);
        }
    }
    if let Some(r) = st.selected_rule.and_then(|i| st.draft.file_rules.get_mut(i)) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut r.name);
        });
        ui.horizontal(|ui| {
            ui.label("Masks:");
            ui.text_edit_singleline(&mut r.masks).on_hover_text("e.g. *.c;*.h;Makefile");
        });
        let mut custom_tab = r.tab_width.is_some();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut custom_tab, "Tab width:").changed() {
                r.tab_width = if custom_tab { Some(4) } else { None };
            }
            if let Some(tw) = &mut r.tab_width {
                ui.add(egui::DragValue::new(tw).range(1..=16));
            }
        });
        rules_ui::text_rules(ui, &mut r.text, "opt-rule");
    }
}
