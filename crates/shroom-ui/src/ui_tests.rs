//! UI tests with egui's headless test harness.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::{ShroomApp, Workspace};

fn harness(files: Vec<PathBuf>) -> Harness<'static, ShroomApp> {
    Harness::builder().with_size([1400.0, 900.0]).build_eframe(move |cc| ShroomApp::new(cc, files))
}

#[test]
fn render_without_images_reports_error() {
    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label_contains("Add DNG files");
    assert!(h.query_by_label("Clear").is_some());
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
    let files: Vec<PathBuf> =
        entries.map(|e| e.unwrap().path()).filter(|p| !p.to_string_lossy().contains('(')).collect();
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

fn step_until(h: &mut Harness<'static, ShroomApp>, what: &str, done: impl Fn(&ShroomApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
        h.step();
    }
    h.run();
}

/// Stacks five frames of Stack 3 through the UI: processing view, result, saving, and
/// discarding the result. Writes screenshots to `target/ui-screenshots/`.
#[test]
#[ignore]
fn stack_through_ui() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 3");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<PathBuf> =
        entries.map(|e| e.unwrap().path()).filter(|p| !p.to_string_lossy().contains('(')).collect();
    files.sort();
    files.truncate(5);
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&out).unwrap();

    let mut h = harness(files);
    step_until(&mut h, "files", |a| a.is_idle());
    h.get_by_label("Render").click();
    h.run();
    assert_eq!(h.state().workspace(), Workspace::Processing);
    step_until(&mut h, "live images", |a| a.has_live_map() || a.workspace() != Workspace::Processing);
    if h.state().workspace() == Workspace::Processing {
        h.render().unwrap().save(out.join("processing.png")).unwrap();
    }
    step_until(&mut h, "result", |a| a.workspace() == Workspace::Postprocessing && a.is_idle());
    h.render().unwrap().save(out.join("postprocessing.png")).unwrap();
    h.get_by_label_contains("Stacked 5 images");

    h.get_by_label("Show depth map").click();
    h.run();
    h.render().unwrap().save(out.join("postprocessing_map.png")).unwrap();
    h.get_by_label("Show depth map").click();
    h.run();

    // Retouch: paint a stroke from the last frame into the result, then undo it.
    h.state_mut().select_for_test(4);
    step_until(&mut h, "brush source", |a| a.is_idle());
    let before = h.state().result_pixel(3000, 2300);
    h.get_by_label("Brush").click();
    h.run();
    assert!(h.state_mut().paint_for_test(&[(2600.0, 2000.0), (3000.0, 2300.0), (3600.0, 2600.0)]));
    h.run();
    let painted = h.state().result_pixel(3000, 2300);
    assert_ne!(before, painted, "stroke should change the result");
    h.render().unwrap().save(out.join("retouch.png")).unwrap();
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(h.state().result_pixel(3000, 2300), before);
    h.get_by_label("Redo").click();
    h.run();
    assert_eq!(h.state().result_pixel(3000, 2300), painted);

    let saved = std::env::temp_dir().join("shroom-ui-test.dng");
    let _ = std::fs::remove_file(&saved);
    h.state_mut().save_for_test(saved.clone());
    step_until(&mut h, "saving", |a| a.is_idle());
    h.get_by_label_contains("Saved");
    let info = shroom_core::io::read_frame_info(&saved).unwrap();
    assert!(info.width > 6900 && info.height > 4600, "{}×{}", info.width, info.height);
    std::fs::remove_file(&saved).unwrap();

    h.get_by_label("New stack").click();
    h.run();
    h.render().unwrap().save(out.join("confirm_discard.png")).unwrap();
    h.get_by_label("Keep").click();
    h.run();
    assert_eq!(h.state().workspace(), Workspace::Postprocessing);
    h.get_by_label("Preparation").click();
    h.run();
    h.get_by_label("Discard").click();
    h.run();
    assert_eq!(h.state().workspace(), Workspace::Preparation);
}

/// Cancelling during processing returns to Preparation.
#[test]
#[ignore]
fn cancel_returns_to_preparation() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 3");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let files: Vec<PathBuf> =
        entries.map(|e| e.unwrap().path()).filter(|p| !p.to_string_lossy().contains('(')).collect();
    let mut h = harness(files);
    step_until(&mut h, "files", |a| a.is_idle());
    h.get_by_label("Render").click();
    h.run();
    h.get_by_label("Cancel").click();
    step_until(&mut h, "cancel", |a| a.workspace() == Workspace::Preparation);
    h.get_by_label("Stacking cancelled.");
}
