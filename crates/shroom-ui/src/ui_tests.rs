//! UI tests with egui's headless test harness.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use crate::app::{ShroomApp, Workspace};

fn harness(files: Vec<PathBuf>) -> Harness<'static, ShroomApp> {
    // Keep learned camera values of the tests out of the user's configuration.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let path = std::env::temp_dir().join(format!("shroom-ui-test-cameras-{}.json", std::process::id()));
        // SAFETY: set once, before the first harness loads any file.
        unsafe { std::env::set_var("SHROOM_CAMERA_STORE", path) };
    });
    // Realistic frame time, so double-clicks (two clicks within 0.3 s) are recognised.
    Harness::builder()
        .with_size([1400.0, 900.0])
        .with_step_dt(1.0 / 60.0)
        .build_eframe(move |cc| ShroomApp::new(cc, files))
}

#[test]
fn render_without_images_reports_error() {
    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label_contains("Add raw files");
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

#[test]
fn soft_bokeh_shows_its_parameters() {
    let mut h = harness(Vec::new());
    h.run();
    assert!(h.query_by_label("Threshold").is_none());
    h.get_by_label("Soft bokeh").click();
    h.run();
    h.get_by_label("Threshold");
    h.get_by_label("From").click();
    h.run();
    h.get_by_label("Nearest focus").click();
    h.run();
    h.get(egui_kittest::kittest::by().label("From").value("Nearest focus"));
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

    // Double-click switches to 1:1, keeping the clicked point under the pointer…
    let point = eframe::egui::Pos2::new(700.0, 400.0);
    let double_click = |h: &mut Harness<'static, ShroomApp>| {
        for _ in 0..2 {
            h.drag_at(point);
            h.step();
            h.event(eframe::egui::Event::PointerButton {
                pos: point,
                button: eframe::egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            });
            h.step();
        }
        h.run();
    };
    h.hover_at(point);
    h.run();
    assert!(h.state().view().fit);
    double_click(&mut h);
    let view = h.state().view();
    // The harness renders at one pixel per point, so 1:1 is zoom 1.
    assert!(!view.fit && (view.zoom - 1.0).abs() < 0.02, "1:1 expected, zoom {}", view.zoom);
    h.render().unwrap().save(out.join("preparation_1to1.png")).unwrap();

    // …dragging pans…
    let before = h.state().view().center;
    h.drag_at(point);
    h.step();
    for dx in [20.0, 60.0, 100.0] {
        h.hover_at(point + eframe::egui::Vec2::new(dx, 0.0));
        h.step();
    }
    h.drop_at(point + eframe::egui::Vec2::new(100.0, 0.0));
    h.run();
    let moved = before.x - h.state().view().center.x;
    assert!(moved > 50.0, "panned by {moved} image pixels");

    // …and a second double-click goes back to fit.
    // (Half a second later, or egui would count it as part of a triple click.)
    h.hover_at(point);
    h.run_steps(30);
    double_click(&mut h);
    assert!(h.state().view().fit);

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

/// Portrait stack (Stack 4, shot at 270°): shown upright, rotatable, and the saved file
/// carries the chosen orientation.
#[test]
#[ignore]
fn portrait_stack_orientation() {
    use shroom_core::Orientation;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 4");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<PathBuf> =
        entries.map(|e| e.unwrap().path()).filter(|p| !p.to_string_lossy().contains('(')).collect();
    files.sort();
    files.truncate(3);
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&out).unwrap();

    let mut h = harness(files);
    step_until(&mut h, "files", |a| a.is_idle());
    assert_eq!(h.state().orientation(), Orientation::Cw270, "taken from EXIF");
    h.render().unwrap().save(out.join("portrait_preparation.png")).unwrap();

    h.get_by_label("Render").click();
    h.run();
    step_until(&mut h, "result", |a| a.workspace() == Workspace::Postprocessing && a.is_idle());
    h.render().unwrap().save(out.join("portrait_postprocessing.png")).unwrap();

    // Rotate the result once more to the right (270° + 90° = upright sensor orientation).
    h.get_by_label("Rotate right").click();
    h.run();
    assert_eq!(h.state().orientation(), Orientation::Normal);
    let saved = std::env::temp_dir().join("shroom-ui-portrait.dng");
    let _ = std::fs::remove_file(&saved);
    h.state_mut().save_for_test(saved.clone());
    step_until(&mut h, "saving", |a| a.is_idle());
    assert_eq!(shroom_core::io::read_frame_info(&saved).unwrap().orientation, Orientation::Normal);

    // Back to 270° via the keyboard shortcut; saving again writes it.
    h.key_press_modifiers(eframe::egui::Modifiers::COMMAND, eframe::egui::Key::OpenBracket);
    h.run();
    assert_eq!(h.state().orientation(), Orientation::Cw270);
    h.state_mut().save_for_test(saved.clone());
    step_until(&mut h, "saving", |a| a.is_idle());
    assert_eq!(shroom_core::io::read_frame_info(&saved).unwrap().orientation, Orientation::Cw270);
    std::fs::remove_file(&saved).unwrap();
}

/// CR3 files (Stack 5) load, pass the checks without calibration warning, and stack.
#[test]
#[ignore]
fn cr3_stack() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 5");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
    files.sort();
    files.truncate(3);
    let mut h = harness(files);
    step_until(&mut h, "files", |a| a.is_idle());
    h.get_by_label_contains("1Z4A4075.CR3");
    h.get_by_label("Render").click();
    h.run();
    assert!(h.query_by_label_contains("No Adobe calibration").is_none());
    step_until(&mut h, "result", |a| a.workspace() == Workspace::Postprocessing && a.is_idle());
    h.get_by_label_contains("Stacked 3 images");
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&out).unwrap();
    h.render().unwrap().save(out.join("cr3_postprocessing.png")).unwrap();
}

/// Stack 2 (blurred forest background) with soft bokeh: the detected area can be shown.
/// Writes a screenshot to `target/ui-screenshots/`.
#[test]
#[ignore]
fn soft_bokeh_stack() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 2");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let files: Vec<PathBuf> =
        entries.map(|e| e.unwrap().path()).filter(|p| !p.to_string_lossy().contains('(')).collect();
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&out).unwrap();
    let mut h = harness(files);
    step_until(&mut h, "files", |a| a.is_idle());
    h.get_by_label("Soft bokeh").click();
    h.run();
    h.get_by_label("Render").click();
    h.run();
    step_until(&mut h, "result", |a| a.workspace() == Workspace::Postprocessing && a.is_idle());
    h.get_by_label_contains("· soft bokeh ·");
    h.get_by_label("Show soft bokeh area").click();
    h.run();
    h.render().unwrap().save(out.join("soft_bokeh.png")).unwrap();
}

#[test]
fn session_without_folder_explains_itself() {
    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label("Session").click();
    h.run();
    assert_eq!(h.state().workspace(), Workspace::Session);
    h.get_by_label_contains("Open a folder");
    assert!(h.query_by_label("Open folder…").is_some());
    assert!(h.query_by_label("Add images…").is_none());
}

/// A folder with Stack 4, the first six frames of Stack 2 and a Helicon result (linked,
/// not copied): grouped into two stacks, edited, restored on reopening, and the second
/// stack rendered in a batch. Writes a screenshot to `target/ui-screenshots/`.
#[test]
#[ignore]
fn session_groups_edits_and_renders() {
    use shroom_core::session::{SESSION_FILE_NAME, SessionFile};
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let raw = |stack: &str| -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(examples.join(stack))
            .map(|rd| rd.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default();
        files.sort();
        files
    };
    let (stack4, stack2) = (raw("Stack 4"), raw("Stack 2"));
    if stack4.is_empty() || stack2.is_empty() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let helicon = stack4.iter().find(|p| p.to_string_lossy().contains('(')).unwrap();
    let photos4: Vec<&PathBuf> = stack4.iter().filter(|p| !p.to_string_lossy().contains('(')).collect();
    let photos2: Vec<&PathBuf> = stack2.iter().filter(|p| !p.to_string_lossy().contains('(')).take(6).collect();
    for file in photos4.iter().chain(&photos2).copied().chain([helicon]) {
        std::os::unix::fs::symlink(file, dir.path().join(file.file_name().unwrap())).unwrap();
    }

    let mut h = harness(Vec::new());
    h.run();
    h.get_by_label("Session").click();
    h.state_mut().open_folder_for_test(dir.path().to_path_buf());
    step_until(&mut h, "grouping", |a| a.is_idle());
    let sizes: Vec<usize> = h.state_mut().session().stacks().iter().map(|s| s.frames.len()).collect();
    let mut sorted = sizes.clone();
    sorted.sort();
    assert_eq!(sorted, vec![6, photos4.len()]);
    h.get_by_label_contains("stacking results not listed");
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&out).unwrap();
    h.render().unwrap().save(out.join("session.png")).unwrap();

    // Splitting the six-frame stack is written to the session file and survives reopening.
    let small = sizes.iter().position(|&n| n == 6).unwrap();
    h.state_mut().session().edit_for_test((small, 3));
    h.run();
    let file = SessionFile::load(dir.path()).unwrap().unwrap();
    assert_eq!(file.stacks.len(), 3);
    h.state_mut().open_folder_for_test(dir.path().to_path_buf());
    step_until(&mut h, "reopening", |a| a.is_idle());
    let reopened: Vec<usize> = h.state_mut().session().stacks().iter().map(|s| s.frames.len()).collect();
    assert_eq!(reopened.iter().filter(|&&n| n == 3).count(), 2, "{reopened:?}");

    let three = reopened.iter().position(|&n| n == 3).unwrap();
    assert_eq!(h.query_all_by_label("Open").count(), 3);

    // Batch: check all, then uncheck all but one three-frame stack in the UI.
    h.state_mut().session().check_all_for_test();
    h.run();
    h.get_by_label("Render selected (3)");
    for i in (0..3).filter(|&i| i != three) {
        h.get_by_label(&format!("Stack {}", i + 1)).click();
        h.run();
    }
    h.get_by_label("Render selected (1)").click();
    h.run();
    step_until(&mut h, "batch", |a| a.is_idle());
    let message = h.state_mut().session().message.clone();
    assert!(message.as_ref().is_some_and(|(e, m)| !e && m.contains("Saved 1 results")), "{message:?}");
    let first = h.state_mut().session().stacks()[three].frames[0].clone();
    let result = dir.path().join(format!("{}-stacked.dng", first.file_stem().unwrap().to_string_lossy()));
    assert!(result.exists(), "{}", result.display());
    let file = SessionFile::load(dir.path()).unwrap().unwrap();
    assert!(file.stacks.iter().any(|s| s.result.is_some()));
    assert!(dir.path().join(SESSION_FILE_NAME).exists());

    // The result is not listed as a photo when the folder is reopened.
    h.state_mut().open_folder_for_test(dir.path().to_path_buf());
    step_until(&mut h, "reopening", |a| a.is_idle());
    h.get_by_label_contains("2 stacking results not listed");

    // Opening a stack loads it into Preparation.
    h.query_all_by_label("Open").next().unwrap().click();
    h.step();
    h.step();
    assert_eq!(h.state().workspace(), Workspace::Preparation);
    step_until(&mut h, "files", |a| a.is_idle());
    assert!(h.state().frame_count() >= 3);
}
