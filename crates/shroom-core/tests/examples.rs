//! Integration tests against the real example stacks in `examples/` (not in git).
//! Run with `cargo test --release -- --ignored`; they are skipped if the folder is missing.

use std::path::{Path, PathBuf};

use shroom_core::io;

/// Keeps learned camera values of these tests out of the user's configuration.
fn isolate_camera_store() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let path = std::env::temp_dir().join(format!("shroom-test-cameras-{}.json", std::process::id()));
        // SAFETY: set once, before any test reads files (all tests call this first).
        unsafe { std::env::set_var("SHROOM_CAMERA_STORE", path) };
    });
}

fn examples_dir() -> Option<PathBuf> {
    isolate_camera_store();
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
    // Stacks of DNG files; folders with other raw formats (e.g. CR3) are not covered here.
    let mut stacks: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && dngs(p).len() >= 2)
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

    // Colour calibration and camera profile must survive unchanged, or raw converters
    // render different colours (missing ForwardMatrix → oversaturated in Lightroom).
    let root = |p: &Path| {
        let source = rawler::rawsource::RawSource::new(p).unwrap();
        rawler::get_decoder(&source).unwrap().ifd(rawler::decoders::WellKnownIFD::Root).unwrap().unwrap()
    };
    let (src_root, out_root) = (root(&input), root(&out));
    for tag in [
        50721u16, 50722, 50723, 50724, 50727, 50728, 50730, 50778, 50779, 50931, 50932, 50936, 50937, 50938, 50939,
        50964, 50965, 50981, 50982,
    ] {
        if let Some(src) = src_root.get_entry(tag) {
            assert_eq!(out_root.get_entry(tag).map(|e| &e.value), Some(&src.value), "tag {tag}");
        }
    }

    // XMP (labels, lens) is carried over with a new document identity.
    let xmp = |root: &rawler::formats::tiff::IFD| match root.get_entry(700u16).map(|e| &e.value) {
        Some(rawler::formats::tiff::Value::Byte(b) | rawler::formats::tiff::Value::Undefined(b)) => {
            String::from_utf8_lossy(b).into_owned()
        }
        _ => String::new(),
    };
    let (src_xmp, out_xmp) = (xmp(&src_root), xmp(&out_root));
    assert!(src_xmp.contains("aux:Lens="), "source has no lens in XMP");
    let lens = src_xmp.lines().find(|l| l.contains("aux:Lens=")).unwrap();
    assert!(out_xmp.contains(lens), "{out_xmp}");
    let doc_id = src_xmp.lines().find(|l| l.contains("xmpMM:DocumentID=")).unwrap();
    assert!(!out_xmp.contains(doc_id));

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

/// A CR3 (Stack 5) and Lightroom's DNG of the same exposure (Stack 1) give the same image
/// data, and a result from the CR3 carries the values Lightroom needs.
#[test]
#[ignore]
fn cr3_matches_lightroom_dng() {
    let Some(dir) = examples_dir() else { return };
    let (cr3, dng) = (dir.join("Stack 5/1Z4A4075.CR3"), dir.join("Stack 1/1Z4A4075.dng"));
    if !cr3.exists() || !dng.exists() {
        return;
    }
    let a = io::load_linear(&cr3).unwrap();
    let b = io::load_linear(&dng).unwrap();
    // rawler crops 4 px more on each side and normalises to the sensor's clipping point
    // (black 2047, white 14888) instead of Adobe's full range (2047…16384).
    let factor = (16384.0f32 - 2047.0) / (14888.0 - 2047.0);
    let (dx, dy) = (4, 4);
    let mut max_rel = 0.0f32;
    for y in (0..a.image.height()).step_by(11) {
        for x in (0..a.image.width()).step_by(11) {
            let (p, q) = (a.image.pixel(x, y), b.image.pixel(x + dx, y + dy));
            for c in 0..3 {
                if q[c] > 0.05 && p[c] < 0.99 {
                    max_rel = max_rel.max((p[c] / factor - q[c]).abs() / q[c]);
                }
            }
        }
    }
    assert!(max_rel < 0.002, "image data differs by up to {:.3} %", max_rel * 100.0);
    for c in 0..3 {
        assert!((a.color.wb[c] - b.color.wb[c]).abs() < 1e-4, "white balance {:?} vs {:?}", a.color.wb, b.color.wb);
    }

    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("from-cr3.dng");
    io::write_linear_dng(&path, &a.image, &a.template, "test").unwrap();
    let source = rawler::rawsource::RawSource::new(&path).unwrap();
    let decoder = rawler::get_decoder(&source).unwrap();
    let root = decoder.ifd(rawler::decoders::WellKnownIFD::Root).unwrap().unwrap();
    let rational = |tag: u16| match root.get_entry(tag).map(|e| &e.value) {
        Some(rawler::formats::tiff::Value::SRational(v)) => v[0].n as f32 / v[0].d as f32,
        other => panic!("tag {tag}: {other:?}"),
    };
    let expected_be = 0.39 - factor.log2();
    assert!((rational(50730) - expected_be).abs() < 0.002, "BaselineExposure {}", rational(50730));
    assert!((rational(50723) - 1.017).abs() < 1e-3, "CameraCalibration1");
    let info = io::read_frame_info(&path).unwrap();
    assert_eq!(info.camera, "Canon EOS R5");
    let xmp = match root.get_entry(700u16).map(|e| &e.value) {
        Some(rawler::formats::tiff::Value::Byte(b) | rawler::formats::tiff::Value::Undefined(b)) => {
            String::from_utf8_lossy(b).into_owned()
        }
        _ => String::new(),
    };
    assert!(xmp.contains(r#"aux:Lens="RF35mm F1.8 MACRO IS STM""#), "{xmp}");
    assert!(xmp.contains(r#"aux:LensID="61182""#), "{xmp}");
    let exif = decoder.ifd(rawler::decoders::WellKnownIFD::Exif).unwrap().unwrap();
    let lens_model = match exif.get_entry(0xA434u16).map(|e| &e.value) {
        Some(rawler::formats::tiff::Value::Ascii(a)) => a.strings()[0].clone(),
        other => panic!("LensModel {other:?}"),
    };
    assert_eq!(lens_model, "RF35mm F1.8 MACRO IS STM");
}
