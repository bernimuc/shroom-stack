//! Shroom Stack desktop application.

mod app;
mod frames;
mod loader;
mod processing;
mod viewer;

#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    // Files given on the command line are loaded at start.
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 900.0]).with_title("Shroom Stack"),
        ..Default::default()
    };
    eframe::run_native("Shroom Stack", options, Box::new(|cc| Ok(Box::new(app::ShroomApp::new(cc, files)))))
}
