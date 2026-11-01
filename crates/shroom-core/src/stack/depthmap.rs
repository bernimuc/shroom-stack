//! Depth map stacking.
//!
//! 1. Per frame, focus = local mean of the squared Laplacian of Gaussian of luminance
//!    (window radius `radius`). The frame with the highest focus wins each pixel.
//! 2. The depth map is regularised using a confidence (how much the best frame stands out
//!    from the average frame, relative to the noise level): confident pixels keep their
//!    depth, while featureless areas and out-of-focus backgrounds, where all frames look
//!    alike, take theirs from the textured surroundings – across arbitrarily large areas
//!    (push–pull interpolation). Without this, smooth surfaces and bokeh turn into a
//!    blotchy patchwork of frames.
//! 3. The output blends the two frames adjacent to each pixel's fractional depth.

use rayon::prelude::*;

use super::{Context, Stage};
use crate::compute::{Backend, box_blur, gaussian_blur};
use crate::error::Result;
use crate::filter::{expand, reduce};
use crate::image::{Plane, RgbImage};

pub(super) fn stack<B: Backend>(ctx: &Context<B>, radius: usize, smoothing: f32) -> Result<(RgbImage, Plane)> {
    let b = ctx.backend;
    let n = ctx.source.len();

    // Pass 1: arg-max of the focus measure (best, sum, depth), plus the first frame's
    // brightness for noise normalisation.
    let mut state: Option<[B::Plane; 4]> = None;
    ctx.for_each_frame(Stage::Analysing, 0..n, |i, frame| {
        let focus = focus_measure(b, &frame, radius);
        match &mut state {
            None => {
                let (w, h) = b.rgb_size(&frame);
                let mut sum = b.zeros_plane(w, h);
                b.add_assign(&mut sum, &focus);
                let brightness = box_blur(b, &b.luminance(&frame), radius);
                state = Some([focus, sum, b.zeros_plane(w, h), brightness]);
            }
            Some([best, sum, depth, _]) => b.argmax_update(best, sum, depth, &focus, i),
        }
        Ok(())
    })?;
    let [best, sum, depth, brightness] = state.expect("at least two frames");
    let size = b.plane_size(&best);

    let confidence = confidence(
        &b.download_plane(&best),
        &b.download_plane(&sum),
        &b.download_plane(&brightness),
        n,
    );
    drop((best, sum, brightness));
    let (sigma, self_weight) = smoothing_params(radius, smoothing);
    let blur = |p: &Plane, sigma: f32| b.download_plane(&gaussian_blur(b, &b.upload_plane(p), sigma));
    let depth = smooth_depth(&b.download_plane(&depth), &confidence, sigma, self_weight, n, &blur);
    drop(confidence);

    // Pass 2: blend frames by fractional depth; frames no pixel uses are skipped.
    let mut used = vec![false; n];
    for d in depth.data() {
        used[d.floor() as usize] = true;
        used[d.ceil() as usize] = true;
    }
    let depth_dev = b.upload_plane(&depth);
    let mut out = b.zeros_rgb(size.0, size.1);
    ctx.for_each_frame(Stage::Compositing, (0..n).filter(|&i| used[i]), |i, frame| {
        b.blend(&mut out, &frame, &depth_dev, i);
        Ok(())
    })?;
    Ok((b.download_rgb(&out), depth))
}

/// Confidences below this multiple of the image's 10th percentile count as noise.
const NOISE_FLOOR_FACTOR: f32 = 4.0;
/// Areas brighter than this (relative to white) are treated as clipped: clipping differs
/// between frames and says nothing about focus.
const CLIP_LEVEL: f32 = 0.8;
/// Dark-noise term of the brightness normalisation (read noise ≈ shot noise here).
const DARK_NOISE: f32 = 1e-3;
/// How strongly coarse-scale estimates fill in, relative to the mean confidence.
const FILL_STRENGTH: f64 = 0.05;

/// Per-pixel confidence of the depth estimate: how much the best frame's focus stands out
/// from the mean focus, in amplitude units (sqrt of energy, so a few very high-contrast
/// edges don't dominate) and normalised by the shot-noise level of the local brightness,
/// minus a noise floor.
fn confidence(best: &Plane, sum: &Plane, brightness: &Plane, n: usize) -> Plane {
    let mut conf = Plane::from_vec(
        best.width(),
        best.height(),
        best.data()
            .par_iter()
            .zip(sum.data().par_iter())
            .zip(brightness.data().par_iter())
            .map(|((b, s), l)| {
                if *l > CLIP_LEVEL {
                    return 0.0;
                }
                let contrast = (b.sqrt() - (s / n as f32).sqrt()).max(0.0);
                contrast / (l.max(0.0) + DARK_NOISE).sqrt()
            })
            .collect(),
    );
    let floor = NOISE_FLOOR_FACTOR * percentile(&conf, 0.1);
    conf.data_mut().par_iter_mut().for_each(|c| *c = (*c - floor).max(0.0));
    conf
}

/// Scale of the focus measure. A plain pixel-level Laplacian mostly measures sensor
/// noise, which also differs between resampled (aligned) and untouched frames; detail
/// at this slightly coarser scale reflects real focus.
const FOCUS_SIGMA: f32 = 1.5;

/// Local energy of the Laplacian of Gaussian of the frame's luminance.
fn focus_measure<B: Backend>(b: &B, frame: &B::Rgb, radius: usize) -> B::Plane {
    let lap = b.laplacian(&gaussian_blur(b, &b.luminance(frame), FOCUS_SIGMA), true);
    box_blur(b, &lap, radius)
}

/// Maps the user parameters to the Gaussian width and the weight of a pixel's own depth
/// estimate. More smoothing widens the neighbourhood and trusts single pixels less.
fn smoothing_params(radius: usize, smoothing: f32) -> (f32, f32) {
    if smoothing <= 0.0 {
        return (0.0, 0.0);
    }
    (smoothing * radius as f32, 12.0 / smoothing)
}

/// Smooths `depth` with a Gaussian weighted by `conf`, plus the pixel's own estimate
/// weighted by `self_weight · conf`: confident pixels (edges, texture) keep their depth,
/// featureless areas take theirs from the surroundings. The result stays in `0..=n-1`.
fn smooth_depth(
    depth: &Plane,
    conf: &Plane,
    sigma: f32,
    self_weight: f32,
    n: usize,
    blur: &dyn Fn(&Plane, f32) -> Plane,
) -> Plane {
    if sigma <= 0.0 {
        return depth.clone();
    }
    let weighted = Plane::from_vec(
        depth.width(),
        depth.height(),
        depth
            .data()
            .par_iter()
            .zip(conf.data().par_iter())
            .map(|(d, c)| d * c)
            .collect(),
    );
    let (mut num, mut den) = (blur(&weighted, sigma), blur(conf, sigma));
    num.data_mut()
        .par_iter_mut()
        .zip(den.data_mut().par_iter_mut())
        .zip(depth.data().par_iter().zip(conf.data().par_iter()))
        .for_each(|((a, b), (d, c))| {
            *a += self_weight * c * d;
            *b += self_weight * c;
        });
    let max = (n - 1) as f32;
    let mut out = push_pull(&num, &den).unwrap_or_else(|| depth.clone());
    out.data_mut().par_iter_mut().for_each(|d| *d = d.clamp(0.0, max));
    out
}

/// Normalised interpolation `num / den` that fills areas where `den` is (nearly) zero
/// from coarser scales, so arbitrarily large featureless regions get a consistent value
/// from their surroundings. `None` if `den` is zero everywhere.
fn push_pull(num: &Plane, den: &Plane) -> Option<Plane> {
    let mean_den = den.data().par_iter().map(|&v| v as f64).sum::<f64>() / den.data().len() as f64;
    if mean_den <= 0.0 {
        return None;
    }
    let tau = (FILL_STRENGTH * mean_den) as f32;
    let mut levels = vec![(num.clone(), den.clone())];
    while levels.last().unwrap().0.width().min(levels.last().unwrap().0.height()) >= 8 {
        let (n, d) = levels.last().unwrap();
        levels.push((reduce(n), reduce(d)));
    }
    let (n, d) = levels.pop().unwrap();
    let coarse_mean = n.data().iter().sum::<f32>() / d.data().iter().sum::<f32>().max(f32::MIN_POSITIVE);
    let mut current = Plane::from_vec(
        n.width(),
        n.height(),
        n.data()
            .iter()
            .zip(d.data())
            .map(|(a, b)| if *b > 0.0 { a / b } else { coarse_mean })
            .collect(),
    );
    for (n, d) in levels.into_iter().rev() {
        let up = expand(&current, n.width(), n.height());
        let data = n
            .data()
            .par_iter()
            .zip(d.data().par_iter())
            .zip(up.data().par_iter())
            .map(|((a, b), u)| (a + tau * u) / (b + tau))
            .collect();
        current = Plane::from_vec(n.width(), n.height(), data);
    }
    Some(current)
}

/// Approximate `q`-quantile of the plane's values (sampled).
fn percentile(p: &Plane, q: f32) -> f32 {
    let step = (p.data().len() / 100_000).max(1);
    let mut v: Vec<f32> = p.data().iter().step_by(step).copied().collect();
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * q) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::Cpu;

    fn cpu_blur(p: &Plane, sigma: f32) -> Plane {
        crate::filter::gaussian_blur(p, sigma)
    }

    #[test]
    fn sharper_frame_has_higher_focus() {
        let (_, frames) = crate::stack::tests::banded_stack(64, 32, 2);
        let (f0, f1) = (
            focus_measure(&Cpu, &frames.0[0], 4),
            focus_measure(&Cpu, &frames.0[1], 4),
        );
        assert!(f0.get(10, 16) > 10.0 * f1.get(10, 16));
        assert!(f1.get(54, 16) > 10.0 * f0.get(54, 16));
    }

    #[test]
    fn push_pull_fills_large_holes_consistently() {
        // Known values (depth 3) only in a narrow border column; the rest has no weight.
        let (w, h) = (200, 100);
        let den = Plane::from_vec(w, h, (0..w * h).map(|i| if i % w < 4 { 1.0 } else { 0.0 }).collect());
        let num = Plane::from_vec(w, h, den.data().iter().map(|d| 3.0 * d).collect());
        let filled = push_pull(&num, &den).unwrap();
        for x in [10, 100, 199] {
            assert!((filled.get(x, 50) - 3.0).abs() < 1e-3, "x={x}: {}", filled.get(x, 50));
        }
        assert!(push_pull(&num, &Plane::new(w, h)).is_none());
    }

    #[test]
    fn push_pull_keeps_confident_values() {
        let den = Plane::from_vec(64, 64, vec![1.0; 64 * 64]);
        let num = Plane::from_vec(64, 64, (0..64 * 64).map(|i| (i % 64 / 32) as f32).collect());
        let out = push_pull(&num, &den).unwrap();
        assert!(
            out.get(10, 10) < 0.06 && out.get(50, 10) > 0.94,
            "{} {}",
            out.get(10, 10),
            out.get(50, 10)
        );
    }

    #[test]
    fn confidence_ignores_noise_and_clipping() {
        let (w, h) = (100, 10);
        // Columns 0..50: one frame clearly sharper; 50..95: barely (noise); 95..: clipped.
        let best = Plane::from_vec(
            w,
            h,
            (0..w * h).map(|i| if i % w < 50 { 1.0 } else { 0.0101 }).collect(),
        );
        let sum = Plane::from_vec(w, h, vec![0.04; w * h]);
        let bright = Plane::from_vec(w, h, (0..w * h).map(|i| if i % w >= 95 { 0.95 } else { 0.2 }).collect());
        let mut best = best;
        for y in 0..h {
            for x in 95..w {
                best.set(x, y, 1.0);
            }
        }
        let c = confidence(&best, &sum, &bright, 4);
        assert!(c.get(10, 5) > 0.0);
        assert_eq!(c.get(70, 5), 0.0);
        assert_eq!(c.get(97, 5), 0.0);
    }

    #[test]
    fn percentile_of_ramp() {
        let p = Plane::from_vec(101, 1, (0..=100).map(|v| v as f32).collect());
        assert_eq!(percentile(&p, 0.1), 10.0);
        assert_eq!(percentile(&p, 1.0), 100.0);
    }

    #[test]
    fn smoothing_parameters() {
        assert_eq!(smoothing_params(8, 0.0), (0.0, 0.0));
        let (s4, w4) = smoothing_params(8, 4.0);
        let (s8, w8) = smoothing_params(8, 8.0);
        assert!(s8 > s4 && w8 < w4);
    }

    #[test]
    fn smoothing_fills_low_confidence_areas() {
        // Left half confidently depth 2, right half unknown (zero focus) at depth 0.
        let depth = Plane::from_vec(40, 4, (0..160).map(|i| if i % 40 < 20 { 2.0 } else { 0.0 }).collect());
        let focus = Plane::from_vec(40, 4, (0..160).map(|i| if i % 40 < 20 { 1.0 } else { 0.0 }).collect());
        let s = smooth_depth(&depth, &focus, 3.0, 0.0, 3, &cpu_blur);
        assert!((s.get(25, 2) - 2.0).abs() < 1e-3, "{}", s.get(25, 2));
        // Confident pixels keep their own depth even next to a confident different depth.
        let depth = Plane::from_vec(40, 4, (0..160).map(|i| if i % 40 < 20 { 2.0 } else { 0.0 }).collect());
        let focus = Plane::from_vec(40, 4, vec![1.0; 160]);
        let blurred = smooth_depth(&depth, &focus, 3.0, 0.0, 3, &cpu_blur);
        let kept = smooth_depth(&depth, &focus, 3.0, 20.0, 3, &cpu_blur);
        assert!((kept.get(18, 2) - 2.0).abs() < (blurred.get(18, 2) - 2.0).abs() / 3.0);
        assert_eq!(smooth_depth(&depth, &focus, 0.0, 0.0, 3, &cpu_blur), depth);
    }
}
