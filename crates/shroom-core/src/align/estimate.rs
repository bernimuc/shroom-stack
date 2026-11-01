//! Estimation of the similarity transform between two frames.
//!
//! Gauss–Newton minimisation of the squared intensity difference on a coarse-to-fine
//! pyramid of blurred, downscaled luminance. Neighbouring frames of a focus stack are
//! similar enough in focus that plain intensity matching is robust.

use rayon::prelude::*;

use super::transform::Affine;
use super::warp::sample_bilinear;
use crate::error::{Error, Result};
use crate::filter::{gaussian_blur, luminance, reduce};
use crate::image::{Plane, RgbImage};

/// Coarsest pyramid level keeps at least this many pixels on its shorter side.
const MIN_LEVEL_SIZE: usize = 48;
const MAX_ITERATIONS: usize = 50;
/// Fraction of reference pixels that must overlap the moving image.
const MIN_OVERLAP: f64 = 0.5;

/// Downscaled luminance pyramid of a frame, finest level first.
pub(super) struct AlignImage {
    levels: Vec<Plane>,
    /// Downscale factor of `levels[0]` relative to the full-resolution frame.
    factor: usize,
}

impl AlignImage {
    /// Prepares `frame` for alignment at a working resolution of at most `max_size`
    /// pixels on the longer side.
    pub(super) fn new(frame: &RgbImage, max_size: usize) -> Self {
        let factor = frame.width().max(frame.height()).div_ceil(max_size).max(1);
        let base = gaussian_blur(&downsample_box(&luminance(frame), factor), 1.0);
        let mut levels = vec![base];
        while levels.last().unwrap().width().min(levels.last().unwrap().height()) / 2 >= MIN_LEVEL_SIZE {
            let next = reduce(levels.last().unwrap());
            levels.push(next);
        }
        Self { levels, factor }
    }
}

/// Mean over `factor × factor` blocks (partial blocks at the border are dropped).
fn downsample_box(src: &Plane, factor: usize) -> Plane {
    if factor == 1 {
        return src.clone();
    }
    let (w, h) = (src.width() / factor, src.height() / factor);
    let mut out = Plane::new(w, h);
    let norm = 1.0 / (factor * factor) as f32;
    out.data_mut().par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for sy in y * factor..(y + 1) * factor {
                sum += src.data()[sy * src.width() + x * factor..][..factor]
                    .iter()
                    .sum::<f32>();
            }
            *o = sum * norm;
        }
    });
    out
}

/// Estimates the transform mapping full-resolution pixel coordinates of `reference` to
/// the corresponding coordinates in `moving`.
pub(super) fn estimate(reference: &AlignImage, moving: &AlignImage) -> Result<Affine> {
    assert_eq!(reference.factor, moving.factor);
    assert_eq!(reference.levels.len(), moving.levels.len());
    // Parameters (a, b, tx, ty) about the image centre, see `Affine::from_params`.
    let mut p = [0.0f64; 4];
    for level in (0..reference.levels.len()).rev() {
        if level + 1 < reference.levels.len() {
            p[2] *= 2.0;
            p[3] *= 2.0;
        }
        p = refine(&reference.levels[level], &moving.levels[level], p)?;
    }
    let base = &reference.levels[0];
    let working = Affine::from_params(centre(base), p[0], p[1], p[2], p[3]);
    // Working pixel x covers full-resolution pixels f·x … f·x+f−1.
    let f = reference.factor as f64;
    let to_full = Affine::translation((f - 1.0) / 2.0, (f - 1.0) / 2.0).then_after(&Affine::scale(f));
    let to_working = to_full.inverse().expect("scale is invertible");
    Ok(to_full.then_after(&working).then_after(&to_working))
}

fn centre(p: &Plane) -> (f64, f64) {
    ((p.width() as f64 - 1.0) / 2.0, (p.height() as f64 - 1.0) / 2.0)
}

/// Gradient by central differences (one-sided at the border).
fn gradients(p: &Plane) -> (Plane, Plane) {
    let (w, h) = (p.width(), p.height());
    let mut gx = Plane::new(w, h);
    let mut gy = Plane::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (l, r) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let (u, d) = (y.saturating_sub(1), (y + 1).min(h - 1));
            gx.set(x, y, (p.get(r, y) - p.get(l, y)) / (r - l).max(1) as f32);
            gy.set(x, y, (p.get(x, d) - p.get(x, u)) / (d - u).max(1) as f32);
        }
    }
    (gx, gy)
}

/// Gauss–Newton iterations on one pyramid level.
#[allow(clippy::needless_range_loop)] // symmetric-matrix packing reads clearer with indices
fn refine(reference: &Plane, moving: &Plane, mut p: [f64; 4]) -> Result<[f64; 4]> {
    let (gx, gy) = gradients(moving);
    let c = centre(reference);
    let total = (reference.width() * reference.height()) as f64;
    for _ in 0..MAX_ITERATIONS {
        let warp = Affine::from_params(c, p[0], p[1], p[2], p[3]);
        // Accumulates the normal equations: 10 entries of the symmetric 4×4 JᵀJ, 4 of
        // Jᵀe, and the number of overlapping pixels.
        let acc = (0..reference.height())
            .into_par_iter()
            .map(|y| {
                let mut acc = [0.0f64; 15];
                for x in 0..reference.width() {
                    let (wx, wy) = warp.apply(x as f64, y as f64);
                    let (Some(iw), Some(dx), Some(dy)) = (
                        sample_bilinear(moving, wx, wy),
                        sample_bilinear(&gx, wx, wy),
                        sample_bilinear(&gy, wx, wy),
                    ) else {
                        continue;
                    };
                    let (u, v) = (x as f64 - c.0, y as f64 - c.1);
                    let (dx, dy) = (dx as f64, dy as f64);
                    let j = [dx * u + dy * v, -dx * v + dy * u, dx, dy];
                    let e = (iw - reference.get(x, y)) as f64;
                    let mut k = 0;
                    for r in 0..4 {
                        for s in r..4 {
                            acc[k] += j[r] * j[s];
                            k += 1;
                        }
                        acc[10 + r] += j[r] * e;
                    }
                    acc[14] += 1.0;
                }
                acc
            })
            .reduce(
                || [0.0; 15],
                |mut a, b| {
                    a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
                    a
                },
            );
        if acc[14] < MIN_OVERLAP * total {
            return Err(Error::Alignment("frames overlap too little".into()));
        }
        let mut h = [[0.0f64; 4]; 4];
        let mut k = 0;
        for r in 0..4 {
            for s in r..4 {
                h[r][s] = acc[k];
                h[s][r] = acc[k];
                k += 1;
            }
        }
        let g = [acc[10], acc[11], acc[12], acc[13]];
        let delta = solve4(h, g.map(|v| -v)).ok_or_else(|| Error::Alignment("no usable image structure".into()))?;
        p.iter_mut().zip(delta).for_each(|(p, d)| *p += d);
        if delta[2].abs().max(delta[3].abs()) < 1e-3 && delta[0].abs().max(delta[1].abs()) < 1e-6 {
            break;
        }
    }
    Ok(p)
}

/// Solves `h·x = g` by Gaussian elimination with partial pivoting.
#[allow(clippy::needless_range_loop)] // index form mirrors the textbook algorithm
fn solve4(mut h: [[f64; 4]; 4], mut g: [f64; 4]) -> Option<[f64; 4]> {
    let scale = h.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
    for col in 0..4 {
        let pivot = (col..4).max_by(|&a, &b| h[a][col].abs().total_cmp(&h[b][col].abs()))?;
        if h[pivot][col].abs() <= 1e-12 * scale {
            return None;
        }
        h.swap(col, pivot);
        g.swap(col, pivot);
        for row in col + 1..4 {
            let f = h[row][col] / h[col][col];
            for k in col..4 {
                h[row][k] -= f * h[col][k];
            }
            g[row] -= f * g[col];
        }
    }
    let mut x = [0.0; 4];
    for row in (0..4).rev() {
        let rest: f64 = (row + 1..4).map(|k| h[row][k] * x[k]).sum();
        x[row] = (g[row] - rest) / h[row][row];
    }
    Some(x)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Smooth, non-periodic analytic test scene.
    pub(crate) fn scene(x: f64, y: f64) -> f32 {
        let blobs = [
            (80.0, 60.0, 25.0),
            (260.0, 200.0, 40.0),
            (330.0, 70.0, 18.0),
            (150.0, 230.0, 30.0),
        ];
        let b: f64 = blobs
            .iter()
            .map(|(cx, cy, r)| (-((x - cx).powi(2) + (y - cy).powi(2)) / (r * r)).exp())
            .sum();
        (0.5 + 0.15 * (0.11 * x + 0.05 * y).sin() + 0.1 * (0.07 * x - 0.13 * y).cos() + 0.3 * b) as f32
    }

    /// Renders the scene as seen by a frame with transform `t` (frame → scene coords).
    pub(crate) fn render(w: usize, h: usize, t: &Affine) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = t.apply(x as f64, y as f64);
                let v = scene(sx, sy);
                img.set_pixel(x, y, [v, v, v]);
            }
        }
        img
    }

    fn assert_close(a: &Affine, b: &Affine, w: usize, h: usize, tol: f64) {
        for (x, y) in [(0.0, 0.0), (w as f64, 0.0), (0.0, h as f64), (w as f64, h as f64)] {
            let (pa, pb) = (a.apply(x, y), b.apply(x, y));
            assert!((pa.0 - pb.0).hypot(pa.1 - pb.1) < tol, "{pa:?} vs {pb:?}\n{a:?}\n{b:?}");
        }
    }

    #[test]
    fn recovers_known_similarity() {
        let (w, h) = (400, 300);
        let truth = Affine::similarity((200.0, 150.0), 1.025, 0.004, (3.4, -2.2));
        let reference = AlignImage::new(&render(w, h, &Affine::IDENTITY), 400);
        let moving = AlignImage::new(&render(w, h, &truth.inverse().unwrap()), 400);
        // `moving` shows the scene point `p` at `truth(p)`, so ref coords map by `truth`.
        let est = estimate(&reference, &moving).unwrap();
        assert_close(&est, &truth, w, h, 0.05);
    }

    #[test]
    fn works_at_reduced_working_resolution() {
        let (w, h) = (400, 300);
        let truth = Affine::similarity((200.0, 150.0), 0.98, -0.003, (-5.0, 1.5));
        let reference = AlignImage::new(&render(w, h, &Affine::IDENTITY), 200);
        let moving = AlignImage::new(&render(w, h, &truth.inverse().unwrap()), 200);
        assert_eq!(reference.factor, 2);
        assert_close(&estimate(&reference, &moving).unwrap(), &truth, w, h, 0.3);
    }

    #[test]
    fn featureless_frames_fail() {
        let flat = RgbImage::from_vec(200, 150, vec![0.5; 200 * 150 * 3]);
        let a = AlignImage::new(&flat, 200);
        assert!(matches!(estimate(&a, &a), Err(Error::Alignment(_))));
    }

    #[test]
    fn solves_linear_system() {
        let h = [
            [4.0, 1.0, 0.0, 0.0],
            [1.0, 3.0, 0.0, 1.0],
            [0.0, 0.0, 2.0, 0.0],
            [0.0, 1.0, 0.0, 5.0],
        ];
        let x = [1.0, -2.0, 0.5, 3.0];
        let g: Vec<f64> = h.iter().map(|r| r.iter().zip(&x).map(|(a, b)| a * b).sum()).collect();
        let sol = solve4(h, [g[0], g[1], g[2], g[3]]).unwrap();
        sol.iter().zip(x).for_each(|(a, b)| assert!((a - b).abs() < 1e-9));
    }

    #[test]
    fn box_downsample_averages() {
        let p = Plane::from_vec(4, 2, vec![0.0, 2.0, 4.0, 6.0, 2.0, 4.0, 6.0, 8.0]);
        assert_eq!(downsample_box(&p, 2).data(), &[2.0, 6.0]);
    }
}
