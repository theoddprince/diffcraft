//! DiffCraft's own vector icons, drawn with the painter on a 16×16 grid so they are crisp at
//! any size and never depend on which glyphs a font happens to contain.

use diffcraft_session::SessionKind;
use egui::epaint::{PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};

use crate::theme::Palette;

/// Toolbar and menu icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Home,
    Sessions,
    ShowAll,
    ShowDiffs,
    ShowSame,
    Context,
    Minor,
    Rules,
    Format,
    CopyRight,
    CopyLeft,
    NextDiff,
    PrevDiff,
    FirstDiff,
    LastDiff,
    Swap,
    Reload,
    Save,
    FolderOpen,
    File,
    Folder,
    Undo,
    Redo,
    Find,
    ExpandAll,
    CollapseAll,
    Delete,
    Report,
    Edit,
    Settings,
    Plus,
    Minus,
    Filter,
    ZoomIn,
    ZoomOut,
    Fit,
    Merge,
    Hex,
    Picture,
    Sync,
    Conflict,
    Check,
    Close,
    Git,
    Help,
}

/// Maps the 16×16 icon grid onto a screen rect.
struct Pen<'a> {
    p: &'a Painter,
    o: Pos2,
    s: f32,
}

impl Pen<'_> {
    fn pt(&self, x: f32, y: f32) -> Pos2 {
        self.o + vec2(x * self.s, y * self.s)
    }
    fn w(&self, w: f32) -> f32 {
        (w * self.s).max(1.0)
    }
    fn line(&self, pts: &[(f32, f32)], c: Color32, w: f32) {
        let points: Vec<Pos2> = pts.iter().map(|&(x, y)| self.pt(x, y)).collect();
        self.p.add(Shape::Path(PathShape { points, closed: false, fill: Color32::TRANSPARENT, stroke: PathStroke::new(self.w(w), c) }));
    }
    fn closed(&self, pts: &[(f32, f32)], fill: Color32, stroke: Color32, w: f32) {
        let points: Vec<Pos2> = pts.iter().map(|&(x, y)| self.pt(x, y)).collect();
        self.p.add(Shape::Path(PathShape { points, closed: true, fill, stroke: PathStroke::new(self.w(w), stroke) }));
    }
    fn fill(&self, pts: &[(f32, f32)], c: Color32) {
        self.closed(pts, c, Color32::TRANSPARENT, 0.0);
    }
    #[allow(clippy::too_many_arguments)]
    fn rect(&self, x: f32, y: f32, w: f32, h: f32, fill: Color32, stroke: Color32, sw: f32) {
        let r = Rect::from_min_size(self.pt(x, y), vec2(w * self.s, h * self.s));
        self.p.rect(r, 1.0 * self.s, fill, Stroke::new(if sw > 0.0 { self.w(sw) } else { 0.0 }, stroke), egui::StrokeKind::Inside);
    }
    fn circle(&self, x: f32, y: f32, r: f32, fill: Color32, stroke: Color32, sw: f32) {
        self.p.circle(self.pt(x, y), r * self.s, fill, Stroke::new(if sw > 0.0 { self.w(sw) } else { 0.0 }, stroke));
    }
    /// An arc from angle `a0` to `a1` (radians, 0 = east, clockwise on screen).
    #[allow(clippy::too_many_arguments)]
    fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, c: Color32, w: f32) {
        let n = 18;
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.line(&pts, c, w);
    }
    fn text(&self, x: f32, y: f32, t: &str, size: f32, c: Color32) {
        self.p.text(self.pt(x, y), egui::Align2::CENTER_CENTER, t, egui::FontId::monospace(size * self.s), c);
    }
}

/// Paint `icon` into `rect` (square area; the icon is centred).
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, pal: &Palette, enabled: bool) {
    let side = rect.width().min(rect.height());
    let o = rect.center() - vec2(side / 2.0, side / 2.0);
    let pen = Pen { p: painter, o, s: side / 16.0 };
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.35) };
    let fg = dim(pal.icon);
    let yellow = dim(pal.icon_yellow);
    let green = dim(pal.icon_green);
    let red = dim(pal.icon_red);
    let blue = dim(pal.icon_blue);
    let t = Color32::TRANSPARENT;
    match icon {
        Icon::Home => {
            pen.closed(&[(2.0, 8.0), (8.0, 2.5), (14.0, 8.0)], t, fg, 1.4);
            pen.closed(&[(4.0, 7.0), (4.0, 14.0), (12.0, 14.0), (12.0, 7.0)], t, fg, 1.4);
            pen.rect(6.8, 9.5, 2.4, 4.5, blue, t, 0.0);
        }
        Icon::Sessions => {
            pen.rect(4.0, 2.0, 10.0, 8.0, t, fg, 1.2);
            pen.rect(2.0, 5.0, 10.0, 9.0, dim(pal.toolbar_bg), fg, 1.2);
            pen.line(&[(4.0, 8.5), (10.0, 8.5)], blue, 1.2);
            pen.line(&[(4.0, 11.0), (8.5, 11.0)], fg, 1.0);
        }
        Icon::ShowAll => {
            for y in [3.5, 6.5, 9.5, 12.5] {
                pen.line(&[(2.5, y), (13.5, y)], fg, 1.4);
            }
        }
        Icon::ShowDiffs => {
            pen.line(&[(3.0, 6.0), (13.0, 6.0)], red, 1.6);
            pen.line(&[(3.0, 10.0), (13.0, 10.0)], red, 1.6);
            pen.line(&[(10.5, 2.5), (5.5, 13.5)], red, 1.6);
        }
        Icon::ShowSame => {
            pen.line(&[(3.0, 6.0), (13.0, 6.0)], fg, 1.6);
            pen.line(&[(3.0, 10.0), (13.0, 10.0)], fg, 1.6);
        }
        Icon::Context => {
            pen.line(&[(2.5, 3.0), (13.5, 3.0)], fg, 1.2);
            pen.rect(1.5, 6.0, 13.0, 4.0, red.gamma_multiply(0.45), red, 1.0);
            pen.line(&[(2.5, 13.0), (13.5, 13.0)], fg, 1.2);
        }
        Icon::Minor => {
            for y in [5.5, 10.0] {
                pen.line(&[(2.5, y + 0.8), (4.5, y - 0.8), (6.5, y + 0.8), (8.5, y - 0.8), (10.5, y + 0.8), (12.5, y - 0.8), (13.5, y)], blue, 1.4);
            }
        }
        Icon::Rules => {
            pen.line(&[(2.0, 4.5), (14.0, 4.5)], fg, 1.2);
            pen.line(&[(2.0, 11.5), (14.0, 11.5)], fg, 1.2);
            pen.rect(9.0, 2.5, 3.0, 4.0, blue, t, 0.0);
            pen.rect(4.0, 9.5, 3.0, 4.0, yellow, t, 0.0);
        }
        Icon::Format => {
            pen.text(5.5, 8.5, "A", 10.0, fg);
            pen.text(11.5, 9.5, "a", 8.0, blue);
            pen.line(&[(2.0, 14.0), (14.0, 14.0)], fg, 1.0);
        }
        Icon::CopyRight => {
            pen.fill(&[(2.0, 6.0), (8.5, 6.0), (8.5, 2.5), (14.5, 8.0), (8.5, 13.5), (8.5, 10.0), (2.0, 10.0)], yellow);
        }
        Icon::CopyLeft => {
            pen.fill(&[(14.0, 6.0), (7.5, 6.0), (7.5, 2.5), (1.5, 8.0), (7.5, 13.5), (7.5, 10.0), (14.0, 10.0)], yellow);
        }
        Icon::NextDiff => {
            pen.fill(&[(6.0, 2.0), (10.0, 2.0), (10.0, 8.0), (13.5, 8.0), (8.0, 14.0), (2.5, 8.0), (6.0, 8.0)], yellow);
        }
        Icon::PrevDiff => {
            pen.fill(&[(6.0, 14.0), (10.0, 14.0), (10.0, 8.0), (13.5, 8.0), (8.0, 2.0), (2.5, 8.0), (6.0, 8.0)], yellow);
        }
        Icon::FirstDiff => {
            pen.line(&[(2.5, 2.5), (13.5, 2.5)], fg, 1.6);
            pen.fill(&[(6.0, 14.0), (10.0, 14.0), (10.0, 9.0), (13.5, 9.0), (8.0, 4.0), (2.5, 9.0), (6.0, 9.0)], yellow);
        }
        Icon::LastDiff => {
            pen.line(&[(2.5, 13.5), (13.5, 13.5)], fg, 1.6);
            pen.fill(&[(6.0, 2.0), (10.0, 2.0), (10.0, 7.0), (13.5, 7.0), (8.0, 12.0), (2.5, 7.0), (6.0, 7.0)], yellow);
        }
        Icon::Swap => {
            pen.line(&[(2.5, 5.0), (13.0, 5.0)], fg, 1.4);
            pen.line(&[(10.0, 2.0), (13.0, 5.0), (10.0, 8.0)], fg, 1.4);
            pen.line(&[(13.5, 11.0), (3.0, 11.0)], fg, 1.4);
            pen.line(&[(6.0, 8.0), (3.0, 11.0), (6.0, 14.0)], fg, 1.4);
        }
        Icon::Reload => {
            pen.arc(8.0, 8.5, 5.0, -0.6, 4.4, green, 1.6);
            pen.fill(&[(10.2, 1.8), (14.4, 4.4), (10.2, 6.6)], green);
        }
        Icon::Save => {
            pen.closed(&[(2.5, 2.5), (11.5, 2.5), (13.5, 4.5), (13.5, 13.5), (2.5, 13.5)], t, fg, 1.2);
            pen.rect(5.0, 2.5, 5.5, 3.5, blue, t, 0.0);
            pen.rect(4.5, 8.5, 7.0, 5.0, t, fg, 1.0);
        }
        Icon::FolderOpen => {
            pen.closed(&[(1.5, 4.0), (6.0, 4.0), (7.5, 5.5), (13.0, 5.5), (13.0, 7.0), (1.5, 7.0)], dim(pal.folder_stroke), dim(pal.folder_stroke), 1.0);
            pen.closed(&[(1.5, 13.5), (3.5, 7.0), (15.0, 7.0), (13.0, 13.5)], dim(pal.folder_fill), dim(pal.folder_stroke), 1.0);
        }
        Icon::Folder => {
            pen.closed(&[(1.5, 3.5), (6.0, 3.5), (7.5, 5.0), (14.5, 5.0), (14.5, 13.0), (1.5, 13.0)], dim(pal.folder_fill), dim(pal.folder_stroke), 1.0);
        }
        Icon::File => {
            pen.closed(&[(3.5, 1.5), (10.0, 1.5), (12.5, 4.0), (12.5, 14.5), (3.5, 14.5)], dim(pal.page_fill), dim(pal.page_stroke), 1.0);
            pen.line(&[(10.0, 1.5), (10.0, 4.0), (12.5, 4.0)], dim(pal.page_stroke), 1.0);
            for y in [7.0, 9.0, 11.0] {
                pen.line(&[(5.5, y), (10.5, y)], dim(pal.page_stroke), 0.8);
            }
        }
        Icon::Undo => {
            pen.arc(8.5, 9.0, 4.5, 3.6, 7.4, fg, 1.5);
            pen.fill(&[(2.0, 4.6), (6.2, 2.4), (6.0, 7.6)], fg);
        }
        Icon::Redo => {
            pen.arc(7.5, 9.0, 4.5, -0.6, 2.9, fg, 1.5);
            pen.fill(&[(14.0, 4.6), (9.8, 2.4), (10.0, 7.6)], fg);
        }
        Icon::Find => {
            pen.circle(6.5, 6.5, 4.2, t, fg, 1.5);
            pen.line(&[(9.6, 9.6), (14.0, 14.0)], fg, 2.0);
        }
        Icon::ExpandAll | Icon::CollapseAll => {
            pen.rect(2.5, 2.5, 11.0, 11.0, t, fg, 1.2);
            pen.line(&[(5.0, 8.0), (11.0, 8.0)], fg, 1.4);
            if icon == Icon::ExpandAll {
                pen.line(&[(8.0, 5.0), (8.0, 11.0)], fg, 1.4);
            }
        }
        Icon::Delete => {
            pen.line(&[(2.5, 4.0), (13.5, 4.0)], red, 1.4);
            pen.line(&[(6.0, 4.0), (6.5, 2.0), (9.5, 2.0), (10.0, 4.0)], red, 1.2);
            pen.closed(&[(4.0, 4.5), (12.0, 4.5), (11.0, 14.0), (5.0, 14.0)], t, red, 1.3);
        }
        Icon::Report => {
            pen.closed(&[(3.0, 1.5), (13.0, 1.5), (13.0, 14.5), (3.0, 14.5)], dim(pal.page_fill), dim(pal.page_stroke), 1.0);
            pen.rect(5.0, 9.0, 1.6, 3.5, blue, t, 0.0);
            pen.rect(7.4, 6.0, 1.6, 6.5, green, t, 0.0);
            pen.rect(9.8, 7.5, 1.6, 5.0, red, t, 0.0);
        }
        Icon::Edit => {
            pen.closed(&[(3.0, 13.0), (3.5, 10.0), (11.0, 2.5), (13.5, 5.0), (6.0, 12.5)], t, fg, 1.2);
            pen.line(&[(9.5, 4.0), (12.0, 6.5)], fg, 1.0);
        }
        Icon::Settings => {
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                pen.line(&[(8.0 + 4.6 * a.cos(), 8.0 + 4.6 * a.sin()), (8.0 + 6.6 * a.cos(), 8.0 + 6.6 * a.sin())], fg, 2.0);
            }
            pen.circle(8.0, 8.0, 4.4, t, fg, 1.5);
            pen.circle(8.0, 8.0, 1.6, fg, t, 0.0);
        }
        Icon::Plus => {
            pen.line(&[(3.0, 8.0), (13.0, 8.0)], fg, 1.6);
            pen.line(&[(8.0, 3.0), (8.0, 13.0)], fg, 1.6);
        }
        Icon::Minus => pen.line(&[(3.0, 8.0), (13.0, 8.0)], fg, 1.6),
        Icon::Filter => pen.closed(&[(2.0, 3.0), (14.0, 3.0), (9.5, 8.5), (9.5, 13.5), (6.5, 12.0), (6.5, 8.5)], t, fg, 1.3),
        Icon::ZoomIn | Icon::ZoomOut => {
            pen.circle(6.5, 6.5, 4.5, t, fg, 1.4);
            pen.line(&[(9.8, 9.8), (14.0, 14.0)], fg, 2.0);
            pen.line(&[(4.5, 6.5), (8.5, 6.5)], fg, 1.2);
            if icon == Icon::ZoomIn {
                pen.line(&[(6.5, 4.5), (6.5, 8.5)], fg, 1.2);
            }
        }
        Icon::Fit => {
            for (x, y, dx, dy) in [(2.5, 2.5, 1.0, 1.0), (13.5, 2.5, -1.0, 1.0), (2.5, 13.5, 1.0, -1.0), (13.5, 13.5, -1.0, -1.0)] {
                pen.line(&[(x, y + 3.5 * dy), (x, y), (x + 3.5 * dx, y)], fg, 1.4);
            }
        }
        Icon::Merge => {
            pen.line(&[(3.0, 2.0), (3.0, 5.0), (8.0, 9.0)], blue, 1.5);
            pen.line(&[(13.0, 2.0), (13.0, 5.0), (8.0, 9.0)], green, 1.5);
            pen.line(&[(8.0, 9.0), (8.0, 12.0)], fg, 1.5);
            pen.fill(&[(5.5, 11.5), (10.5, 11.5), (8.0, 14.5)], fg);
        }
        Icon::Hex => {
            pen.rect(1.5, 2.5, 13.0, 11.0, dim(pal.page_fill), dim(pal.page_stroke), 1.0);
            pen.text(8.0, 6.0, "01", 5.5, blue);
            pen.text(8.0, 10.5, "10", 5.5, red);
        }
        Icon::Picture => {
            pen.rect(1.5, 2.5, 13.0, 11.0, dim(pal.page_fill), dim(pal.page_stroke), 1.0);
            pen.circle(11.0, 5.5, 1.4, yellow, t, 0.0);
            pen.fill(&[(2.5, 12.5), (6.5, 7.0), (9.5, 10.5), (11.0, 9.0), (13.5, 12.5)], green);
        }
        Icon::Sync => {
            pen.arc(8.0, 8.0, 5.0, 3.5, 5.9, green, 1.6);
            pen.fill(&[(11.0, 2.6), (14.4, 4.6), (11.2, 6.6)], green);
            pen.arc(8.0, 8.0, 5.0, 0.4, 2.8, blue, 1.6);
            pen.fill(&[(5.0, 13.4), (1.6, 11.4), (4.8, 9.4)], blue);
        }
        Icon::Conflict => {
            pen.closed(&[(8.0, 1.5), (15.0, 14.0), (1.0, 14.0)], red.gamma_multiply(0.3), red, 1.3);
            pen.line(&[(8.0, 6.0), (8.0, 10.0)], red, 1.6);
            pen.circle(8.0, 12.0, 0.9, red, t, 0.0);
        }
        Icon::Check => pen.line(&[(2.5, 8.5), (6.5, 12.5), (13.5, 3.5)], green, 2.0),
        Icon::Close => {
            pen.line(&[(4.0, 4.0), (12.0, 12.0)], fg, 1.4);
            pen.line(&[(12.0, 4.0), (4.0, 12.0)], fg, 1.4);
        }
        Icon::Git => {
            pen.line(&[(5.0, 3.0), (5.0, 13.0)], fg, 1.4);
            pen.line(&[(5.0, 9.5), (11.0, 6.0)], fg, 1.4);
            pen.circle(5.0, 3.0, 1.7, red, t, 0.0);
            pen.circle(5.0, 13.0, 1.7, red, t, 0.0);
            pen.circle(11.0, 5.5, 1.7, red, t, 0.0);
        }
        Icon::Help => {
            pen.circle(8.0, 8.0, 6.2, t, blue, 1.3);
            pen.text(8.0, 8.3, "?", 9.0, blue);
        }
    }
}

/// The small icon for a session type (tabs, lists).
pub fn session_icon(kind: SessionKind) -> Icon {
    match kind {
        SessionKind::FolderCompare => Icon::Folder,
        SessionKind::FolderSync => Icon::Sync,
        SessionKind::TextCompare => Icon::File,
        SessionKind::TextMerge => Icon::Merge,
        SessionKind::HexCompare => Icon::Hex,
        SessionKind::PictureCompare => Icon::Picture,
    }
}

/// A large illustration for the Home page tiles (`rect` ≈ 64×64).
pub fn paint_session_tile(painter: &Painter, rect: Rect, kind: SessionKind, pal: &Palette) {
    let side = rect.width().min(rect.height());
    let o = rect.center() - vec2(side / 2.0, side / 2.0);
    let pen = Pen { p: painter, o, s: side / 16.0 };
    let folder = |dx: f32, dy: f32, shade: f32| {
        let fill = pal.folder_fill.gamma_multiply(shade);
        pen.closed(
            &[(1.5 + dx, 3.5 + dy), (5.5 + dx, 3.5 + dy), (6.8 + dx, 4.8 + dy), (13.5 + dx, 4.8 + dy), (13.5 + dx, 12.5 + dy), (1.5 + dx, 12.5 + dy)],
            fill,
            pal.folder_stroke,
            0.35,
        );
        pen.line(&[(1.5 + dx, 6.2 + dy), (13.5 + dx, 6.2 + dy)], pal.folder_stroke.gamma_multiply(0.6), 0.25);
    };
    let page = |dx: f32, dy: f32, fill: Color32| {
        pen.closed(
            &[(4.0 + dx, 1.5 + dy), (10.0 + dx, 1.5 + dy), (12.0 + dx, 3.5 + dy), (12.0 + dx, 13.5 + dy), (4.0 + dx, 13.5 + dy)],
            fill,
            pal.page_stroke,
            0.3,
        );
        pen.line(&[(10.0 + dx, 1.5 + dy), (10.0 + dx, 3.5 + dy), (12.0 + dx, 3.5 + dy)], pal.page_stroke, 0.3);
        for k in 0..5 {
            let y = 5.0 + dy + k as f32 * 1.6;
            pen.line(&[(5.5 + dx, y), (10.5 + dx - (k % 2) as f32 * 1.5, y)], pal.page_stroke.gamma_multiply(0.8), 0.3);
        }
    };
    match kind {
        SessionKind::FolderCompare => {
            folder(-0.8, -0.8, 0.85);
            folder(1.2, 1.2, 1.0);
        }
        SessionKind::FolderSync => {
            folder(0.0, 0.0, 1.0);
            let c = pal.icon_green;
            pen.arc(10.5, 10.5, 3.0, 3.4, 6.0, c, 0.7);
            pen.fill(&[(12.2, 7.0), (14.4, 8.2), (12.4, 9.5)], c);
            pen.arc(10.5, 10.5, 3.0, 0.3, 2.9, pal.icon_blue, 0.7);
            pen.fill(&[(8.8, 14.0), (6.6, 12.8), (8.6, 11.5)], pal.icon_blue);
        }
        SessionKind::TextCompare => {
            page(-1.6, -0.6, pal.page_fill.gamma_multiply(0.92));
            page(1.6, 0.6, pal.page_fill);
        }
        SessionKind::TextMerge => {
            page(-2.2, -0.6, pal.page_fill.gamma_multiply(0.9));
            page(2.2, -0.6, pal.page_fill.gamma_multiply(0.9));
            page(0.0, 0.8, Color32::from_rgb(0xfb, 0xe8, 0xb0));
        }
        SessionKind::HexCompare => {
            page(0.0, 0.0, pal.page_fill);
            pen.rect(5.0, 4.5, 6.0, 8.0, pal.page_fill, Color32::TRANSPARENT, 0.0);
            pen.text(8.0, 6.2, "0F A1", 1.8, pal.icon_blue);
            pen.text(8.0, 8.4, "3C 7E", 1.8, pal.icon_red);
            pen.text(8.0, 10.6, "B2 05", 1.8, pal.icon_blue);
        }
        SessionKind::PictureCompare => {
            pen.rect(1.5, 2.5, 13.0, 11.0, pal.page_fill, pal.page_stroke, 0.35);
            pen.rect(2.5, 3.5, 11.0, 9.0, Color32::from_rgb(0xbf, 0xdc, 0xf5), Color32::TRANSPARENT, 0.0);
            pen.circle(11.0, 5.8, 1.2, pal.icon_yellow, Color32::TRANSPARENT, 0.0);
            pen.fill(&[(2.5, 12.5), (6.5, 7.0), (9.5, 10.5), (11.0, 9.0), (13.5, 12.5)], pal.icon_green);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn every_icon_paints_without_panicking() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let pal = Palette::dark();
            let p = ui.painter();
            let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(20.0, 20.0));
            for icon in [
                Icon::Home,
                Icon::Sessions,
                Icon::ShowAll,
                Icon::ShowDiffs,
                Icon::ShowSame,
                Icon::Context,
                Icon::Minor,
                Icon::Rules,
                Icon::Format,
                Icon::CopyRight,
                Icon::CopyLeft,
                Icon::NextDiff,
                Icon::PrevDiff,
                Icon::FirstDiff,
                Icon::LastDiff,
                Icon::Swap,
                Icon::Reload,
                Icon::Save,
                Icon::FolderOpen,
                Icon::File,
                Icon::Folder,
                Icon::Undo,
                Icon::Redo,
                Icon::Find,
                Icon::ExpandAll,
                Icon::CollapseAll,
                Icon::Delete,
                Icon::Report,
                Icon::Edit,
                Icon::Settings,
                Icon::Plus,
                Icon::Minus,
                Icon::Filter,
                Icon::ZoomIn,
                Icon::ZoomOut,
                Icon::Fit,
                Icon::Merge,
                Icon::Hex,
                Icon::Picture,
                Icon::Sync,
                Icon::Conflict,
                Icon::Check,
                Icon::Close,
                Icon::Git,
                Icon::Help,
            ] {
                paint(p, r, icon, &pal, true);
                paint(p, r, icon, &pal, false);
            }
            for k in SessionKind::ALL {
                paint_session_tile(p, Rect::from_min_size(pos2(0.0, 0.0), vec2(64.0, 64.0)), k, &pal);
            }
        });
        out.textures_delta.clear();
    }
}
