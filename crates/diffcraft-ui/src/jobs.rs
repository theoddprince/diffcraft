//! Background work (loading, diffing, scanning, syncing) off the UI thread, with progress,
//! cancellation and panic containment.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use diffcraft_core::folder::Progress;

/// A running background computation producing `Result<T, String>`.
pub struct Job<T> {
    rx: Receiver<Result<T, String>>,
    pub progress: Arc<Progress>,
    pub label: String,
    pub started: Instant,
}

impl<T: Send + 'static> Job<T> {
    /// Run `f` on a new thread. A panic inside `f` becomes an `Err`, never a crash.
    pub fn spawn(label: impl Into<String>, ctx: &egui::Context, f: impl FnOnce(&Progress) -> Result<T, String> + Send + 'static) -> Job<T> {
        let (tx, rx) = channel();
        let progress = Arc::new(Progress::default());
        let p = progress.clone();
        let ctx = ctx.clone();
        let label = label.into();
        let thread_label = label.clone();
        let spawned = std::thread::Builder::new().name(format!("diffcraft: {thread_label}")).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&p)))
                .unwrap_or_else(|e| Err(format!("internal error while {thread_label}: {}", panic_message(e.as_ref()))));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        let job = Job { rx, progress, label, started: Instant::now() };
        if let Err(e) = spawned {
            // Could not start a thread: report through the channel-less path.
            let (tx, rx) = channel();
            let _ = tx.send(Err(format!("could not start a worker thread: {e}")));
            return Job { rx, ..job };
        }
        job
    }
}

impl<T> Job<T> {
    /// The result, once finished.
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("the background task stopped unexpectedly".into())),
        }
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// One-line progress text.
    pub fn status_text(&self) -> String {
        let p = &self.progress;
        let total = p.files_to_compare.load(Ordering::Relaxed);
        let done = p.files_compared.load(Ordering::Relaxed);
        let scanned = p.items_scanned.load(Ordering::Relaxed);
        let mut s = format!("{}…", self.label);
        if total > 0 {
            s.push_str(&format!(" {done}/{total}"));
        } else if scanned > 0 {
            s.push_str(&format!(" {scanned} items"));
        }
        let cur = p.current();
        if !cur.is_empty() {
            s.push_str("  ");
            s.push_str(&shorten(&cur, 60));
        }
        s
    }
}

pub(crate) fn panic_message(e: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Shorten a string from the left, keeping its end (paths).
pub fn shorten(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_owned();
    }
    let tail: String = s.chars().skip(n - max_chars.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// A spinner + label + Cancel button for a running job. Returns true if Cancel was clicked.
pub fn progress_ui<T>(ui: &mut egui::Ui, job: &Job<T>) -> bool {
    let mut cancel = false;
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(job.status_text());
        if ui.button("Cancel").clicked() {
            job.cancel();
            cancel = true;
        }
    });
    ui.ctx().request_repaint_after(Duration::from_millis(100));
    cancel
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_become_errors() {
        let ctx = egui::Context::default();
        #[allow(clippy::panic)]
        let mut job: Job<()> = Job::spawn("testing", &ctx, |_| panic!("boom"));
        let r = loop {
            if let Some(r) = job.poll() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let e = r.unwrap_err();
        assert!(e.contains("boom"), "{e}");
    }

    #[test]
    fn results_arrive() {
        let ctx = egui::Context::default();
        let mut job = Job::spawn("adding", &ctx, |_| Ok(2 + 2));
        let r = loop {
            if let Some(r) = job.poll() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(r, Ok(4));
        assert_eq!(shorten("abcdef", 4), "…def");
    }
}
