//! UI tests with egui's headless test harness.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::ShroomApp;

fn harness(files: Vec<PathBuf>) -> Harness<'static, ShroomApp> {
    Harness::builder()
        .with_size([1400.0, 900.0])
        .build_eframe(move |cc| ShroomApp::new(cc, files))
}

#[test]
fn render_without_images_reports_error() {
    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label_contains("Add DNG files");
    h.get_by_label("Render").click();
    h.run();
    h.get_by_label_contains("At least two images");
}

#[test]
fn switching_method_shows_its_parameters() {
    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label("Radius");
    h.get_by_label("Method").click();
    h.run();
    h.get_by_label("Pyramid").click();
    h.run();
    assert!(h.query_by_label("Radius").is_none());
    h.get_by_label("Smoothing");
}

/// Loads Stack 3 from `examples/` and writes screenshots to `target/ui-screenshots/`.
/// Run with `cargo test -p shroom-ui -- --ignored`.
#[test]
#[ignore]
fn screenshot_with_example_stack() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 3");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let files: Vec<PathBuf> = entries
        .map(|e| e.unwrap().path())
        .filter(|p| !p.to_string_lossy().contains('('))
        .collect();
    let mut h = harness(files);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !h.state().is_idle() {
        assert!(Instant::now() < deadline, "loading timed out");
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
    h.run();
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&out).unwrap();
    h.render().unwrap().save(out.join("preparation.png")).unwrap();

    h.get_by_label("1:1").click();
    h.run();
    h.render().unwrap().save(out.join("preparation_1to1.png")).unwrap();

    h.get_by_label("Render").click();
    h.run();
    h.render().unwrap().save(out.join("preflight.png")).unwrap();
}
