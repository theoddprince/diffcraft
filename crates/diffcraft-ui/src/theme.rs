//! Colours and styling. Every view takes its colours from [`Palette`], never hard-coded.

use diffcraft_core::folder::Status;
use diffcraft_core::text::RowKind;
use diffcraft_session::{Settings, Theme};
use egui::{Color32, FontFamily, FontId, TextStyle};

/// Semantic colours for the whole UI, in a light and a dark variant.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Menu bar and tab strip.
    pub chrome_bg: Color32,
    /// Toolbars and the active tab.
    pub toolbar_bg: Color32,
    /// Side panels and the Home page.
    pub panel_bg: Color32,
    /// Pane headers and file information bars.
    pub header_bg: Color32,
    pub border: Color32,
    pub hover_bg: Color32,
    pub active_bg: Color32,
    pub text: Color32,
    pub dim_text: Color32,
    pub gutter_bg: Color32,
    pub gutter_text: Color32,
    pub pane_bg: Color32,
    /// Text colour of important differences.
    pub changed_text: Color32,
    /// Both sides present but different.
    pub changed_bg: Color32,
    /// The differing characters inside a changed line.
    pub changed_strong: Color32,
    /// Lines only on the left.
    pub left_only_bg: Color32,
    /// Lines only on the right.
    pub right_only_bg: Color32,
    /// Placeholder where the other side has lines (drawn hatched).
    pub filler_bg: Color32,
    pub hatch: Color32,
    /// "N filtered lines" separator bars.
    pub separator_bg: Color32,
    /// Unimportant differences (whitespace, ignored patterns).
    pub minor_text: Color32,
    pub minor_bg: Color32,
    pub conflict_bg: Color32,
    pub conflict_strong: Color32,
    pub resolved_bg: Color32,
    pub selection_bg: Color32,
    pub current_outline: Color32,
    pub find_bg: Color32,
    pub accent: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    /// Icon colours.
    pub icon: Color32,
    pub icon_yellow: Color32,
    pub icon_green: Color32,
    pub icon_red: Color32,
    pub icon_blue: Color32,
    pub folder_fill: Color32,
    pub folder_stroke: Color32,
    pub page_fill: Color32,
    pub page_stroke: Color32,
    /// Folder compare: newer side, older side, orphans.
    pub newer_text: Color32,
    pub older_text: Color32,
    pub orphan_text: Color32,
    pub different_text: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Palette {
    pub fn light() -> Self {
        Palette {
            dark: false,
            chrome_bg: rgb(0xe9, 0xeb, 0xee),
            toolbar_bg: rgb(0xf7, 0xf8, 0xfa),
            panel_bg: rgb(0xf3, 0xf4, 0xf6),
            header_bg: rgb(0xec, 0xee, 0xf1),
            border: rgb(0xcf, 0xd3, 0xd9),
            hover_bg: rgb(0xe0, 0xe5, 0xec),
            active_bg: rgb(0xd3, 0xe1, 0xf5),
            text: rgb(0x1e, 0x1f, 0x22),
            dim_text: rgb(0x76, 0x7b, 0x83),
            gutter_bg: rgb(0xf4, 0xf5, 0xf7),
            gutter_text: rgb(0x9a, 0x9f, 0xa6),
            pane_bg: rgb(0xff, 0xff, 0xff),
            changed_text: rgb(0xb4, 0x16, 0x16),
            changed_bg: rgb(0xfd, 0xe8, 0xe7),
            changed_strong: rgb(0xf8, 0xb9, 0xb4),
            left_only_bg: rgb(0xfd, 0xe8, 0xe7),
            right_only_bg: rgb(0xfd, 0xe8, 0xe7),
            filler_bg: rgb(0xf3, 0xf4, 0xf6),
            hatch: rgb(0xdb, 0xde, 0xe3),
            separator_bg: rgb(0xe6, 0xe8, 0xec),
            minor_text: rgb(0x2a, 0x63, 0xc4),
            minor_bg: rgb(0xec, 0xf2, 0xfd),
            conflict_bg: rgb(0xfb, 0xd5, 0xd5),
            conflict_strong: rgb(0xd0, 0x2f, 0x2f),
            resolved_bg: rgb(0xe3, 0xf3, 0xe6),
            selection_bg: Color32::from_rgba_unmultiplied(0x2a, 0x6f, 0xdb, 50),
            current_outline: rgb(0x2a, 0x6f, 0xdb),
            find_bg: rgb(0xff, 0xe8, 0x6b),
            accent: rgb(0x2a, 0x6f, 0xdb),
            error: rgb(0xc6, 0x28, 0x28),
            warning: rgb(0xb4, 0x6b, 0x00),
            success: rgb(0x1e, 0x8e, 0x3e),
            icon: rgb(0x4a, 0x4f, 0x57),
            icon_yellow: rgb(0xe0, 0x9a, 0x14),
            icon_green: rgb(0x2e, 0x9e, 0x4f),
            icon_red: rgb(0xd0, 0x3a, 0x3a),
            icon_blue: rgb(0x2a, 0x6f, 0xdb),
            folder_fill: rgb(0xf3, 0xcf, 0x7a),
            folder_stroke: rgb(0xc9, 0x9a, 0x3a),
            page_fill: rgb(0xff, 0xff, 0xff),
            page_stroke: rgb(0x8a, 0x90, 0x99),
            newer_text: rgb(0xc6, 0x28, 0x28),
            older_text: rgb(0x8a, 0x91, 0x99),
            orphan_text: rgb(0x6a, 0x3d, 0xc4),
            different_text: rgb(0xc6, 0x28, 0x28),
        }
    }

    pub fn dark() -> Self {
        Palette {
            dark: true,
            chrome_bg: rgb(0x1b, 0x1c, 0x1f),
            toolbar_bg: rgb(0x2b, 0x2d, 0x30),
            panel_bg: rgb(0x25, 0x26, 0x29),
            header_bg: rgb(0x2f, 0x31, 0x35),
            border: rgb(0x41, 0x44, 0x49),
            hover_bg: rgb(0x3a, 0x3d, 0x42),
            active_bg: rgb(0x2e, 0x43, 0x63),
            text: rgb(0xd4, 0xd6, 0xd9),
            dim_text: rgb(0x8b, 0x8f, 0x96),
            gutter_bg: rgb(0x21, 0x22, 0x25),
            gutter_text: rgb(0x6b, 0x70, 0x77),
            pane_bg: rgb(0x1b, 0x1c, 0x1f),
            changed_text: rgb(0xff, 0x8f, 0x87),
            changed_bg: rgb(0x3b, 0x1f, 0x22),
            changed_strong: rgb(0x7d, 0x2c, 0x31),
            left_only_bg: rgb(0x3b, 0x1f, 0x22),
            right_only_bg: rgb(0x3b, 0x1f, 0x22),
            filler_bg: rgb(0x21, 0x22, 0x25),
            hatch: rgb(0x34, 0x36, 0x3b),
            separator_bg: rgb(0x2a, 0x2c, 0x30),
            minor_text: rgb(0x6f, 0xa8, 0xf5),
            minor_bg: rgb(0x1d, 0x26, 0x36),
            conflict_bg: rgb(0x4d, 0x1f, 0x24),
            conflict_strong: rgb(0xf0, 0x5d, 0x5d),
            resolved_bg: rgb(0x1f, 0x36, 0x27),
            selection_bg: Color32::from_rgba_unmultiplied(0x4d, 0x8a, 0xf0, 70),
            current_outline: rgb(0x4d, 0x8a, 0xf0),
            find_bg: rgb(0x7a, 0x66, 0x10),
            accent: rgb(0x4d, 0x8a, 0xf0),
            error: rgb(0xf0, 0x6a, 0x6a),
            warning: rgb(0xe8, 0xa8, 0x3a),
            success: rgb(0x5f, 0xb8, 0x65),
            icon: rgb(0xc8, 0xcb, 0xd0),
            icon_yellow: rgb(0xf0, 0xb4, 0x3c),
            icon_green: rgb(0x5f, 0xb8, 0x65),
            icon_red: rgb(0xe8, 0x5a, 0x5a),
            icon_blue: rgb(0x58, 0x9d, 0xf6),
            folder_fill: rgb(0xd9, 0xb0, 0x5b),
            folder_stroke: rgb(0xa8, 0x82, 0x34),
            page_fill: rgb(0xe9, 0xeb, 0xee),
            page_stroke: rgb(0x8b, 0x8f, 0x96),
            newer_text: rgb(0xff, 0x8f, 0x87),
            older_text: rgb(0x8b, 0x8f, 0x96),
            orphan_text: rgb(0xb3, 0x8b, 0xf5),
            different_text: rgb(0xff, 0x8f, 0x87),
        }
    }

    /// egui widget styling matching the palette.
    pub fn visuals(&self) -> egui::Visuals {
        use egui::{CornerRadius, Stroke};
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        let r = CornerRadius::same(3);
        v.panel_fill = self.panel_bg;
        v.window_fill = self.toolbar_bg;
        v.window_stroke = Stroke::new(1.0, self.border);
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(4);
        v.extreme_bg_color = self.pane_bg;
        v.faint_bg_color = self.header_bg;
        v.code_bg_color = self.header_bg;
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.accent.gamma_multiply(if self.dark { 0.55 } else { 0.35 });
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.override_text_color = Some(self.text);
        v.weak_text_color = Some(self.dim_text);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.panel_bg;
        w.noninteractive.weak_bg_fill = self.panel_bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = self.header_bg;
        w.inactive.weak_bg_fill = self.header_bg;
        w.inactive.bg_stroke = Stroke::new(1.0, self.border);
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = self.hover_bg;
        w.hovered.weak_bg_fill = self.hover_bg;
        w.hovered.bg_stroke = Stroke::new(1.0, self.accent.gamma_multiply(0.7));
        w.hovered.fg_stroke = Stroke::new(1.5, self.text);
        w.hovered.corner_radius = r;
        w.active.bg_fill = self.active_bg;
        w.active.weak_bg_fill = self.active_bg;
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.fg_stroke = Stroke::new(1.5, self.text);
        w.active.corner_radius = r;
        w.open.bg_fill = self.active_bg;
        w.open.weak_bg_fill = self.active_bg;
        w.open.corner_radius = r;
        v
    }

    /// Fill `rect` with diagonal hatching (the "no lines here" pattern).
    pub fn hatch_rect(&self, painter: &egui::Painter, rect: egui::Rect) {
        painter.rect_filled(rect, 0.0, self.filler_bg);
        let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        let step = 7.0;
        let h = rect.height();
        // Anchored to absolute coordinates so stacked rows join into one continuous pattern.
        let mut x = ((rect.left() - h + rect.bottom()) / step).floor() * step - rect.bottom();
        while x < rect.right() {
            p.line_segment([egui::pos2(x, rect.bottom()), egui::pos2(x + h, rect.top())], egui::Stroke::new(1.0, self.hatch));
            x += step;
        }
    }

    /// The palette for the context's current light/dark mode.
    pub fn get(ctx: &egui::Context) -> Self {
        if ctx.global_style().visuals.dark_mode { Palette::dark() } else { Palette::light() }
    }

    /// Row background for a side-by-side text row. `present` is false for filler rows.
    pub fn row_bg(&self, kind: RowKind, present: bool, unimportant: bool) -> Color32 {
        if !present {
            return self.filler_bg;
        }
        if unimportant && kind != RowKind::Same {
            return self.minor_bg;
        }
        match kind {
            RowKind::Same => self.pane_bg,
            RowKind::Minor => self.minor_bg,
            RowKind::Changed => self.changed_bg,
            RowKind::LeftOnly => self.left_only_bg,
            RowKind::RightOnly => self.right_only_bg,
        }
    }

    /// Small marker colour for the overview bar.
    pub fn overview(&self, kind: RowKind, unimportant: bool) -> Color32 {
        if unimportant {
            return self.minor_text.gamma_multiply(0.6);
        }
        match kind {
            RowKind::Changed => self.changed_strong,
            RowKind::LeftOnly => self.conflict_strong,
            RowKind::RightOnly => self.success,
            RowKind::Minor => self.minor_text.gamma_multiply(0.6),
            RowKind::Same => Color32::TRANSPARENT,
        }
    }

    /// Text colour for a folder-compare entry on one side.
    pub fn status_text(&self, status: Status, left_side: bool) -> Color32 {
        match status {
            Status::Same => self.text,
            Status::Different => self.different_text,
            Status::LeftNewer => {
                if left_side {
                    self.newer_text
                } else {
                    self.older_text
                }
            }
            Status::RightNewer => {
                if left_side {
                    self.older_text
                } else {
                    self.newer_text
                }
            }
            Status::LeftOnly | Status::RightOnly => self.orphan_text,
            Status::Error => self.warning,
        }
    }
}

/// Apply theme, zoom and font size from settings.
pub fn apply(ctx: &egui::Context, settings: &Settings) {
    let pref = match settings.theme {
        Theme::System => egui::ThemePreference::System,
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::Light => egui::ThemePreference::Light,
    };
    ctx.set_visuals_of(egui::Theme::Dark, Palette::dark().visuals());
    ctx.set_visuals_of(egui::Theme::Light, Palette::light().visuals());
    ctx.set_theme(pref);
    if (ctx.zoom_factor() - settings.ui_scale).abs() > f32::EPSILON {
        ctx.set_zoom_factor(settings.ui_scale);
    }
    let mono = settings.font_size;
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(TextStyle::Monospace, FontId::new(mono, FontFamily::Monospace));
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 3.0);
        style.interaction.tooltip_delay = 0.4;
    });
}

/// System fonts tried as fallbacks (first found per group is used), so symbols, CJK and other
/// scripts in file contents and names render instead of showing boxes. Nothing is bundled:
/// a missing font is simply skipped.
const FALLBACK_FONTS: &[&[&str]] = &[
    // Symbols and arrows.
    &[
        "C:\\Windows\\Fonts\\seguisym.ttf",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/local/share/fonts/dejavu/DejaVuSans.ttf",
    ],
    // Chinese, Japanese, Korean.
    &[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\YuGothR.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    ],
    // Korean (Noto Sans CJK above already covers it on Linux).
    &["C:\\Windows\\Fonts\\malgun.ttf", "/System/Library/Fonts/AppleSDGothicNeo.ttc"],
    // Arabic, Hebrew, Devanagari, Thai and more.
    &["C:\\Windows\\Fonts\\segoeui.ttf", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf", "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf"],
];

/// Append available system fonts as fallbacks for both font families.
pub fn install_fallback_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let mut added = 0;
    for group in FALLBACK_FONTS {
        for path in *group {
            let Ok(bytes) = std::fs::read(path) else { continue };
            let name = format!("fallback-{added}");
            defs.font_data.insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                defs.families.entry(family).or_default().push(name.clone());
            }
            added += 1;
            break;
        }
    }
    if added > 0 {
        ctx.set_fonts(defs);
    }
}

/// The monospace font used by all diff panes.
pub fn mono(ctx: &egui::Context) -> FontId {
    ctx.global_style().text_styles.get(&TextStyle::Monospace).cloned().unwrap_or_else(|| FontId::monospace(13.0))
}
