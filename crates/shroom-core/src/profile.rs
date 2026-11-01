//! Timing breakdown of the per-frame processing steps on a real example frame.
//! Run with `cargo test --release -p shroom-core profile -- --ignored --nocapture`.

use std::path::Path;
use std::time::Instant;

use crate::align::{Affine, warp_rgb};
use crate::filter::{box_blur, gaussian_blur, laplacian, luminance, reduce};
use crate::image::Rect;
use crate::io::{decode_raw, load_linear};
use crate::raw::demosaic;

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("{label:>28}: {:8.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    r
}

#[test]
#[ignore]
fn profile_frame_steps() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 1/1Z4A4075.dng");
    if !path.exists() {
        return;
    }
    time("load_linear (total)", || load_linear(&path).unwrap());
    let raw = time("decode_raw", || decode_raw(&path).unwrap());
    let rgb = time("demosaic", || demosaic(&raw.mosaic, raw.pattern));
    let (w, h) = (rgb.width(), rgb.height());
    let t = Affine::similarity((w as f64 / 2.0, h as f64 / 2.0), 0.99, 0.001, (1.3, -0.7));
    let warped = time("warp_rgb", || warp_rgb(&rgb, &t, Rect::new(1, 1, w - 2, h - 2)));
    let lum = time("luminance", || luminance(&warped));
    let g = time("gaussian_blur σ1.5", || gaussian_blur(&lum, 1.5));
    let l = time("laplacian", || laplacian(&g));
    time("box_blur r8", || box_blur(&l, 8));
    time("reduce (1 plane)", || reduce(&lum));
    time("gaussian_blur σ32 (smoothing)", || gaussian_blur(&lum, 32.0));
}

#[test]
#[ignore]
fn profile_gpu_steps() {
    use crate::compute::{Backend, box_blur, gaussian_blur};
    let Some(g) = crate::gpu::gpu() else { return };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 1/1Z4A4075.dng");
    if !path.exists() {
        return;
    }
    let raw = decode_raw(&path).unwrap();
    let sync = || g.sync();
    for round in 0..2 {
        println!("-- round {round}");
        let mosaic = time("upload mosaic", || {
            let m = g.upload_plane(&raw.mosaic);
            sync();
            m
        });
        let rgb = time("demosaic", || {
            let r = g.demosaic(&mosaic, raw.pattern);
            sync();
            r
        });
        let (w, h) = g.rgb_size(&rgb);
        let t = Affine::similarity((w as f64 / 2.0, h as f64 / 2.0), 0.99, 0.001, (1.3, -0.7));
        let warped = time("warp", || {
            let r = g.warp(&rgb, &t, Rect::new(1, 1, w - 2, h - 2));
            sync();
            r
        });
        time("focus measure", || {
            let l = g.laplacian(&gaussian_blur(g, &g.luminance(&warped), 1.5), true);
            let f = box_blur(g, &l, 8);
            sync();
            f
        });
        let planes = time("split", || {
            let p = g.split(&warped);
            sync();
            p
        });
        time("reduce+expand+sub ×3", || {
            for p in &planes {
                let (pw, ph) = g.plane_size(p);
                let r = g.reduce(p);
                let _ = g.sub(p, &g.expand(&r, pw, ph));
            }
            sync();
        });
        time("energy (sumsq + box r2)", || {
            let e = box_blur(g, &g.sum_squares(&planes), 2);
            sync();
            e
        });
    }
}

/// Lists isolated outlier photosites (likely hot pixels) per frame of Stack 1 and how
/// many frames share each position.
#[test]
#[ignore]
fn find_hot_pixels() {
    use std::collections::HashMap;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 1");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !p.to_string_lossy().contains('('))
        .collect();
    files.sort();
    let mut seen: HashMap<(usize, usize), (usize, u8, f32, f32)> = HashMap::new();
    for path in &files {
        let raw = decode_raw(path).unwrap();
        // decode_raw already removed hot pixels; count what the filter finds on top.
        let mut again = raw.mosaic.clone();
        let extra = crate::raw::remove_hot_pixels(&mut again);
        let m = &raw.mosaic;
        let mut count = 0;
        for y in 2..m.height() - 2 {
            for x in 2..m.width() - 2 {
                let v = m.get(x, y);
                let nb = [
                    m.get(x - 2, y),
                    m.get(x + 2, y),
                    m.get(x, y - 2),
                    m.get(x, y + 2),
                    m.get(x - 2, y - 2),
                    m.get(x + 2, y - 2),
                    m.get(x - 2, y + 2),
                    m.get(x + 2, y + 2),
                ];
                let max = nb.iter().cloned().fold(f32::MIN, f32::max);
                if v > 0.02 && v > 3.0 * max.max(0.002) {
                    count += 1;
                    let e = seen.entry((x, y)).or_insert((0, raw.pattern.color_at(x, y), v, max));
                    e.0 += 1;
                }
            }
        }
        println!(
            "{}: {count} outliers left, filter would replace {extra} more",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    let mut common: Vec<_> = seen.into_iter().filter(|(_, (n, ..))| *n >= files.len() / 2).collect();
    common.sort_by_key(|(p, _)| *p);
    println!("positions outlying in ≥ half of {} frames: {}", files.len(), common.len());
    for ((x, y), (n, c, v, max)) in common.iter().take(20) {
        println!("  ({x}, {y}) colour {c} in {n} frames, e.g. value {v:.4} vs neighbours ≤ {max:.4}");
    }
}

/// Compares the linear image data of a CR3 and the Lightroom DNG of the same exposure:
/// finds the crop offset and the per-channel level ratio.
#[test]
#[ignore]
fn compare_cr3_with_dng() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let (cr3, dng) = (root.join("Stack 5/1Z4A4075.CR3"), root.join("Stack 1/1Z4A4075.dng"));
    if !cr3.exists() || !dng.exists() {
        return;
    }
    let a = load_linear(&cr3).unwrap().image;
    let b = load_linear(&dng).unwrap().image;
    println!("CR3 {}x{}, DNG {}x{}", a.width(), a.height(), b.width(), b.height());
    // Offset of the CR3 area inside the DNG area, by matching a central patch.
    let (px, py, n) = (a.width() / 2, a.height() / 2, 256);
    let diff = |dx: usize, dy: usize| -> f64 {
        let mut s = 0.0;
        for y in 0..n {
            for x in 0..n {
                let (p, q) = (a.pixel(px + x, py + y), b.pixel(px + x + dx, py + y + dy));
                s += (0..3).map(|c| (p[c] - q[c]).abs() as f64).sum::<f64>();
            }
        }
        s
    };
    let mut best = (0, 0, f64::MAX);
    for dy in 0..=b.height() - a.height() {
        for dx in 0..=b.width() - a.width() {
            let d = diff(dx, dy);
            if d < best.2 {
                best = (dx, dy, d);
            }
        }
    }
    let (dx, dy, _) = best;
    let mut sum = [[0.0f64; 3]; 2];
    let mut max_rel = 0.0f64;
    for y in (0..a.height()).step_by(7) {
        for x in (0..a.width()).step_by(7) {
            let (p, q) = (a.pixel(x, y), b.pixel(x + dx, y + dy));
            for c in 0..3 {
                sum[0][c] += p[c] as f64;
                sum[1][c] += q[c] as f64;
                if q[c] > 0.05 {
                    max_rel = max_rel.max(((p[c] - q[c]) / q[c]).abs() as f64);
                }
            }
        }
    }
    println!("offset of CR3 area in DNG: ({dx}, {dy})");
    println!("level ratio CR3/DNG per channel: {:?}", [0, 1, 2].map(|c| sum[0][c] / sum[1][c]));
    println!("largest relative pixel difference (values > 5 %): {max_rel:.5}");
}

/// Prints what rawler reports about the lens and white level of a CR3.
#[test]
#[ignore]
fn cr3_lens_and_levels() {
    use rawler::decoders::RawDecodeParams;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/Stack 5/1Z4A4075.CR3");
    if !path.exists() {
        return;
    }
    let source = rawler::rawsource::RawSource::new(&path).unwrap();
    let decoder = rawler::get_decoder(&source).unwrap();
    let md = decoder.raw_metadata(&source, &RawDecodeParams::default()).unwrap();
    println!("exif lens_model {:?}, lens_make {:?}", md.exif.lens_model, md.exif.lens_make);
    println!("lens {:?}", md.lens.as_ref().map(|l| (&l.identifiers, &l.lens_model)));
    let raw = decoder.raw_image(&source, &RawDecodeParams::default(), true).unwrap();
    println!(
        "white {:?} black {:?} model {:?} clean {:?}",
        raw.whitelevel,
        raw.blacklevel.levels.first(),
        raw.model,
        raw.clean_model
    );
    println!("camera hints {:?}", raw.camera.hints);
}
