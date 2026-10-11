//! Editors for comparison rules, shared by the views' toolbars and the Options dialog.

use diffcraft_core::folder::{CompareMode, Filter, FolderCompareOptions};
use diffcraft_core::text::{Algorithm, TextCompareOptions, WhitespaceMode};

/// Edit text comparison rules. Returns true when anything changed.
pub fn text_rules(ui: &mut egui::Ui, o: &mut TextCompareOptions, id: &str) -> bool {
    let mut changed = false;
    ui.label("Whitespace:");
    for w in WhitespaceMode::ALL {
        changed |= ui.radio_value(&mut o.whitespace, w, w.label()).changed();
    }
    changed |= ui.checkbox(&mut o.ignore_case, "Ignore case").changed();
    changed |= ui.checkbox(&mut o.ignore_blank_lines, "Inserted/deleted blank lines are unimportant").changed();
    ui.horizontal(|ui| {
        ui.label("Algorithm:");
        egui::ComboBox::from_id_salt((id, "alg")).selected_text(o.algorithm.label()).show_ui(ui, |ui| {
            for a in Algorithm::ALL {
                changed |= ui.selectable_value(&mut o.algorithm, a, a.label()).changed();
            }
        });
    });
    ui.label("Unimportant text (one regular expression per line):")
        .on_hover_text("Text matching these patterns is ignored when comparing lines, e.g.\n\\$Id:[^$]*\\$\n\\d{4}-\\d{2}-\\d{2}[T ]\\d{2}:\\d{2}:\\d{2}");
    let mut text = o.unimportant_patterns.join("\n");
    if ui.add(egui::TextEdit::multiline(&mut text).id_salt((id, "pat")).desired_rows(3).code_editor().desired_width(f32::INFINITY)).changed() {
        o.unimportant_patterns = text.lines().map(str::to_owned).collect();
        changed = true;
    }
    for p in o.unimportant_patterns.iter().filter(|p| !p.trim().is_empty()) {
        if let Err(e) = regex_check(p) {
            ui.colored_label(crate::theme::Palette::get(ui.ctx()).error, format!("`{p}`: {e}"));
        }
    }
    changed
}

fn regex_check(p: &str) -> Result<(), String> {
    diffcraft_core::text::Normalizer::new(&TextCompareOptions { unimportant_patterns: vec![p.to_owned()], ..TextCompareOptions::default() })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Edit folder comparison options. `filter_text` is the editable filter string.
/// Returns true when anything changed.
pub fn folder_rules(ui: &mut egui::Ui, o: &mut FolderCompareOptions, filter_text: &mut String, id: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Compare by:");
        egui::ComboBox::from_id_salt((id, "mode")).selected_text(o.mode.label()).show_ui(ui, |ui| {
            for m in CompareMode::ALL {
                changed |= ui.selectable_value(&mut o.mode, m, m.label()).changed();
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("Timestamp tolerance (s):");
        changed |= ui.add(egui::DragValue::new(&mut o.time_tolerance_secs).range(0..=86_400)).changed();
    });
    changed |= ui.checkbox(&mut o.ignore_dst_shift, "Ignore exact 1-hour differences (daylight saving)").changed();
    changed |= ui.checkbox(&mut o.case_sensitive, "Case-sensitive names").changed();
    changed |= ui.checkbox(&mut o.follow_symlinks, "Follow symbolic links").changed();
    changed |= ui.checkbox(&mut o.filter.show_hidden, "Show hidden files").changed();
    ui.label("Filter:").on_hover_text("`;`-separated. `*.rs;*.toml` includes; `-*.bak` excludes files; `-target/` excludes folders.");
    if ui.add(egui::TextEdit::singleline(filter_text).id_salt((id, "filter")).hint_text("*.*;-.git/;-target/").desired_width(f32::INFINITY)).changed() {
        let hidden = o.filter.show_hidden;
        let (min, max) = (o.filter.min_size, o.filter.max_size);
        o.filter = Filter { show_hidden: hidden, min_size: min, max_size: max, ..Filter::parse(filter_text) };
        changed = true;
    }
    if o.mode == CompareMode::TextRules {
        ui.collapsing("Text rules", |ui| {
            changed |= text_rules(ui, &mut o.text, &format!("{id}-text"));
        });
    }
    changed
}
