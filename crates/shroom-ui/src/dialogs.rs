//! Native file dialogs, run off the UI frame.
//!
//! A blocking dialog opened while egui draws a frame runs a nested event loop; on macOS
//! winit then receives events while still inside the frame callback and aborts the
//! process. rfd's async dialogs (a sheet on macOS) are started from a background thread
//! instead, and the result is picked up in a later frame.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;

pub enum Picked {
    /// Files to add (empty if the dialog was cancelled).
    Open(Vec<PathBuf>),
    /// Where to save the result, if not cancelled.
    Save(Option<PathBuf>),
}

pub struct Dialogs {
    tx: Sender<Picked>,
    rx: Receiver<Picked>,
    ctx: egui::Context,
    open: bool,
}

impl Dialogs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, ctx, open: false }
    }

    /// Whether a dialog is currently shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn pick_dngs(&mut self) {
        self.spawn(|| async {
            let files = rfd::AsyncFileDialog::new().add_filter("DNG", &["dng", "DNG"]).pick_files().await;
            Picked::Open(files.unwrap_or_default().iter().map(|f| f.path().to_path_buf()).collect())
        });
    }

    pub fn save_dng(&mut self, file_name: String, directory: Option<PathBuf>) {
        self.spawn(move || async move {
            let mut dialog = rfd::AsyncFileDialog::new().add_filter("DNG", &["dng"]).set_file_name(file_name);
            if let Some(dir) = directory {
                dialog = dialog.set_directory(dir);
            }
            Picked::Save(dialog.save_file().await.map(|f| f.path().to_path_buf()))
        });
    }

    fn spawn<F: std::future::Future<Output = Picked>>(&mut self, dialog: impl FnOnce() -> F + Send + 'static) {
        if self.open {
            return;
        }
        self.open = true;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(pollster::block_on(dialog()));
            ctx.request_repaint();
        });
    }

    /// The result of a closed dialog, if any.
    pub fn poll(&mut self) -> Option<Picked> {
        let picked = self.rx.try_recv().ok()?;
        self.open = false;
        Some(picked)
    }
}
