//! Malvar–He–Cutler gradient-corrected linear demosaicing.
//!
//! Chosen as the CPU reference because it is fast, has no data-dependent branches (maps
//! directly to a GPU kernel) and is noticeably sharper than bilinear interpolation, which
//! matters for focus measurement.

use rayon::prelude::*;

use super::cfa::{CfaPattern, GREEN, RED};
use crate::image::{Plane, RgbImage};

/// Demosaics a normalised Bayer mosaic into linear RGB. Negative overshoots are clamped
/// to zero; values above 1.0 are kept.
pub fn demosaic(mosaic: &Plane, pattern: CfaPattern) -> RgbImage {
    let (w, h) = (mosaic.width(), mosaic.height());
    assert!(w >= 3 && h >= 3, "image too small to demosaic");
    let mut out = RgbImage::new(w, h);
    out.data_mut().par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let rgb = demosaic_pixel(mosaic, pattern, x, y);
            row[x * 3..x * 3 + 3].copy_from_slice(&rgb.map(|v| v.max(0.0)));
        }
    });
    out
}

/// Reflects an out-of-range coordinate without repeating the edge pixel ("reflect-101"),
/// which preserves CFA parity.
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    let i = if i < 0 { -i } else { i };
    (if i >= n { 2 * (n - 1) - i } else { i }) as usize
}

fn demosaic_pixel(m: &Plane, pattern: CfaPattern, x: usize, y: usize) -> [f32; 3] {
    let at = |dx: isize, dy: isize| m.get(reflect(x as isize + dx, m.width()), reflect(y as isize + dy, m.height()));
    let c = at(0, 0);
    let color = pattern.color_at(x, y);

    // Neighbour sums used by the kernels.
    let cross1 = at(-1, 0) + at(1, 0) + at(0, -1) + at(0, 1);
    let cross2 = at(-2, 0) + at(2, 0) + at(0, -2) + at(0, 2);
    let diag = at(-1, -1) + at(1, -1) + at(-1, 1) + at(1, 1);
    let horiz1 = at(-1, 0) + at(1, 0);
    let vert1 = at(0, -1) + at(0, 1);
    let horiz2 = at(-2, 0) + at(2, 0);
    let vert2 = at(0, -2) + at(0, 2);

    if color == GREEN {
        // Colour of horizontal neighbours vs. vertical neighbours.
        let row_val = (5.0 * c - diag - horiz2 + 0.5 * vert2 + 4.0 * horiz1) / 8.0;
        let col_val = (5.0 * c - diag - vert2 + 0.5 * horiz2 + 4.0 * vert1) / 8.0;
        if pattern.color_at(x + 1, y) == RED { [row_val, c, col_val] } else { [col_val, c, row_val] }
    } else {
        let g = (4.0 * c - cross2 + 2.0 * cross1) / 8.0;
        let opposite = (6.0 * c - 1.5 * cross2 + 2.0 * diag) / 8.0;
        if color == RED { [c, g, opposite] } else { [opposite, g, c] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mosaic_from(w: usize, h: usize, pattern: CfaPattern, f: impl Fn(usize, usize) -> [f32; 3]) -> Plane {
        let mut m = Plane::new(w, h);
        for y in 0..h {
            for x in 0..w {
                m.set(x, y, f(x, y)[pattern.color_at(x, y) as usize]);
            }
        }
        m
    }

    #[test]
    fn reflect_keeps_parity() {
        assert_eq!(reflect(-1, 10), 1);
        assert_eq!(reflect(-2, 10), 2);
        assert_eq!(reflect(10, 10), 8);
        assert_eq!(reflect(11, 10), 7);
        assert_eq!(reflect(5, 10), 5);
    }

    #[test]
    fn flat_colour_is_reproduced_everywhere() {
        for name in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let p = CfaPattern::from_name(name).unwrap();
            let rgb = demosaic(&mosaic_from(9, 7, p, |_, _| [0.2, 0.5, 0.8]), p);
            for y in 0..7 {
                for x in 0..9 {
                    let px = rgb.pixel(x, y);
                    for c in 0..3 {
                        assert!((px[c] - [0.2, 0.5, 0.8][c]).abs() < 1e-6, "{name} at {x},{y}: {px:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn linear_gradients_are_exact_in_interior() {
        let f = |x: usize, y: usize| {
            let (x, y) = (x as f32, y as f32);
            [0.01 * x + 0.02 * y, 0.03 * x - 0.01 * y + 0.5, 0.2 + 0.005 * x + 0.015 * y]
        };
        for name in ["RGGB", "GBRG"] {
            let p = CfaPattern::from_name(name).unwrap();
            let rgb = demosaic(&mosaic_from(16, 16, p, f), p);
            for y in 2..14 {
                for x in 2..14 {
                    let (got, want) = (rgb.pixel(x, y), f(x, y));
                    for c in 0..3 {
                        assert!((got[c] - want[c]).abs() < 1e-5, "{name} {x},{y} c{c}: {got:?} vs {want:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn measured_samples_are_kept() {
        let p = CfaPattern::RGGB;
        let m = mosaic_from(8, 8, p, |x, y| [(x * 7 + y) as f32 / 64.0, 0.3, 0.6]);
        let rgb = demosaic(&m, p);
        for y in 0..8 {
            for x in 0..8 {
                let c = p.color_at(x, y);
                assert_eq!(rgb.pixel(x, y)[c as usize], m.get(x, y));
            }
        }
    }

    #[test]
    fn negative_overshoot_is_clamped() {
        let p = CfaPattern::RGGB;
        // A single bright green pixel creates negative lobes in its neighbourhood.
        let m = mosaic_from(8, 8, p, |x, y| if (x, y) == (3, 4) { [0.0, 1.0, 0.0] } else { [0.0; 3] });
        assert!(demosaic(&m, p).data().iter().all(|&v| v >= 0.0));
    }
}
