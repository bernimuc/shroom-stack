//! Integration tests against the real example stacks in `examples/` (not in git).
//! Run with `cargo test --release -- --ignored`; they are skipped if the folder is missing.

use std::path::{Path, PathBuf};

use shroom_core::io;

fn examples_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    dir.is_dir().then_some(dir)
}

fn dngs(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")))
        .collect();
    files.sort();
    files
}

/// Camera files have 8-character names; the Helicon results contain brackets.
fn is_helicon(p: &Path) -> bool {
    p.file_name().unwrap().to_string_lossy().contains('(')
}

fn stacks() -> Vec<PathBuf> {
    let Some(dir) = examples_dir() else {
        eprintln!("examples/ not found, skipping");
        return Vec::new();
    };
    let mut stacks: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    stacks.sort();
    stacks
}

#[test]
#[ignore]
fn frame_info_of_all_examples() {
    for stack in stacks() {
        let mut frames: Vec<_> = dngs(&stack).iter().map(|p| io::read_frame_info(p).unwrap()).collect();
        io::sort_frames(&mut frames);
        let inputs: Vec<_> = frames.iter().filter(|f| !is_helicon(&f.path)).collect();
        assert!(inputs.len() >= 2, "{}", stack.display());
        for f in &inputs {
            assert!(f.capture_time.is_some(), "{}", f.file_name);
            assert!(f.preview.is_some(), "{}", f.file_name);
            assert_eq!((f.width, f.height), (inputs[0].width, inputs[0].height));
            assert_eq!(f.camera, inputs[0].camera);
        }
        // Shooting order equals file name order for these stacks.
        let names: Vec<_> = inputs.iter().map(|f| &f.file_name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "{}", stack.display());
    }
}

#[test]
#[ignore]
fn raw_to_linear_dng_roundtrip() {
    let Some(stack) = stacks().into_iter().next() else {
        return;
    };
    let input = dngs(&stack).into_iter().find(|p| !is_helicon(p)).unwrap();
    let frame = io::load_linear(&input).unwrap();
    let info = io::read_frame_info(&input).unwrap();
    assert_eq!((frame.image.width(), frame.image.height()), (info.width, info.height));

    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("linear.dng");
    io::write_linear_dng(&out, &frame.image, &frame.template, "Shroom Stack test").unwrap();
    let back = io::load_linear(&out).unwrap();
    assert_eq!(back.image.width(), frame.image.width());
    assert_eq!(back.image.height(), frame.image.height());
    let max_err = frame
        .image
        .data()
        .iter()
        .zip(back.image.data())
        .map(|(a, b)| (a.clamp(0.0, 1.0) - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_err <= 1.0 / 65535.0, "max error {max_err}");
    assert!((back.color.wb[0] - frame.color.wb[0]).abs() < 1e-3);

    let back_info = io::read_frame_info(&out).unwrap();
    assert_eq!(back_info.camera, info.camera);
    assert_eq!(back_info.capture_time, info.capture_time);
}

#[test]
#[ignore]
fn helicon_results_are_readable() {
    for stack in stacks() {
        let files = dngs(&stack);
        let input = io::read_frame_info(files.iter().find(|p| !is_helicon(p)).unwrap()).unwrap();
        for result in files.iter().filter(|p| is_helicon(p)) {
            let frame = io::load_linear(result).unwrap();
            assert!(frame.image.width() <= input.width && frame.image.height() <= input.height);
            assert!(frame.color.xyz_to_cam.is_some());
        }
    }
}
