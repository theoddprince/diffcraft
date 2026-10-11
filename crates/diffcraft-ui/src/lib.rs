//! DiffCraft's desktop user interface (egui/eframe). This is the only crate that uses egui,
//! eframe or rfd; all comparison logic lives in `diffcraft-core`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app;
mod brand;
mod folder_view;
mod hex_view;
mod home;
mod icons;
mod jobs;
mod merge_view;
mod options;
mod picture_view;
mod rules_ui;
mod sync_view;
mod text_model;
mod text_view;
mod theme;
mod toolbar;
mod view;
mod widgets;

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

pub use view::OpenRequest;

/// What to open when the window appears.
#[derive(Default)]
pub struct Startup {
    pub open: Vec<OpenRequest>,
}

/// Run the desktop app until the window closes. Returns the process exit code
/// (0 normally; for a merge started by `git mergetool`, 0 only if the result was saved
/// without unresolved conflicts).
pub fn run(startup: Startup) -> Result<i32, String> {
    let exit_code = Arc::new(AtomicI32::new(0));
    // A merge must be saved to count as success.
    if startup.open.iter().any(|r| matches!(r, OpenRequest::Merge { .. })) {
        exit_code.store(1, Ordering::Relaxed);
    }
    let renderer = match std::env::var("DIFFCRAFT_RENDERER").map(|s| s.to_ascii_lowercase()).as_deref() {
        Ok("glow") | Ok("gl") | Ok("opengl") => eframe::Renderer::Glow,
        _ => eframe::Renderer::Wgpu,
    };
    let icon = brand::window_icon();
    let mut viewport =
        egui::ViewportBuilder::default().with_title("DiffCraft").with_inner_size([1280.0, 800.0]).with_min_inner_size([640.0, 400.0]).with_drag_and_drop(true);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, renderer, persist_window: true, ..Default::default() };
    let code = exit_code.clone();
    eframe::run_native(
        "DiffCraft",
        options,
        Box::new(move |cc| {
            let app = app::DiffCraftApp::new(cc, startup.open, code);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| format!("could not open the window: {e}. Try setting DIFFCRAFT_RENDERER=glow."))?;
    Ok(exit_code.load(Ordering::Relaxed))
}
