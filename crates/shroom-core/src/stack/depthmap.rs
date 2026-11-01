//! Depth map stacking.
//!
//! 1. Per frame, focus = local mean of the squared Laplacian of luminance (window radius
//!    `radius`). The frame with the highest focus wins each pixel.
//! 2. The resulting depth map is smoothed with a confidence-weighted Gaussian, so flat,
//!    featureless areas take their depth from textured surroundings.
//! 3. The output blends the two frames adjacent to each pixel's fractional depth.

use rayon::prelude::*;

use super::{Context, StackResult, Stage};
use crate::error::Result;
use crate::filter::{box_blur, gaussian_blur, laplacian, luminance};
use crate::image::{Plane, RgbImage};

pub(super) fn stack(ctx: &Context, radius: usize, smoothing: f32) -> Result<StackResult> {
    let n = ctx.source.len();

    // Pass 1: arg-max of the focus measure.
    let first = ctx.load(Stage::Analysing, 0, None)?;
    let size = (first.width(), first.height());
    let mut best = focus_measure(&first, radius);
    drop(first);
    let mut depth = Plane::new(size.0, size.1);
    for i in 1..n {
        let focus = focus_measure(&ctx.load(Stage::Analysing, i, Some(size))?, radius);
        best.data_mut()
            .par_iter_mut()
            .zip(depth.data_mut().par_iter_mut())
            .zip(focus.data().par_iter())
            .for_each(|((b, d), f)| {
                if *f > *b {
                    *b = *f;
                    *d = i as f32;
                }
            });
    }

    let depth = smooth_depth(&depth, &best, smoothing * radius as f32 / 4.0, n);
    drop(best);

    // Pass 2: blend frames by fractional depth; frames no pixel uses are skipped.
    let mut used = vec![false; n];
    for d in depth.data() {
        used[d.floor() as usize] = true;
        used[d.ceil() as usize] = true;
    }
    let mut out = RgbImage::new(size.0, size.1);
    for i in (0..n).filter(|&i| used[i]) {
        let frame = ctx.load(Stage::Compositing, i, Some(size))?;
        out.data_mut()
            .par_chunks_mut(3)
            .zip(frame.data().par_chunks(3))
            .zip(depth.data().par_iter())
            .for_each(|((o, f), d)| {
                let w = (1.0 - (d - i as f32).abs()).max(0.0);
                if w > 0.0 {
                    o.iter_mut().zip(f).for_each(|(o, f)| *o += w * f);
                }
            });
    }
    Ok(StackResult {
        image: out,
        depth_map: depth,
    })
}

/// Local energy of the Laplacian of the frame's luminance.
fn focus_measure(frame: &RgbImage, radius: usize) -> Plane {
    let mut lap = laplacian(&luminance(frame));
    lap.data_mut().par_iter_mut().for_each(|v| *v *= *v);
    box_blur(&lap, radius)
}

/// Gaussian-smooths `depth` weighted by focus confidence; the result stays in `0..=n-1`.
fn smooth_depth(depth: &Plane, focus: &Plane, sigma: f32, n: usize) -> Plane {
    if sigma <= 0.0 {
        return depth.clone();
    }
    // sqrt compresses the huge dynamic range of the focus energy, so a few very
    // high-contrast edges don't dominate their neighbourhood.
    let conf = Plane::from_vec(
        depth.width(),
        depth.height(),
        focus.data().par_iter().map(|f| f.max(0.0).sqrt()).collect(),
    );
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
    let (num, den) = (gaussian_blur(&weighted, sigma), gaussian_blur(&conf, sigma));
    let max = (n - 1) as f32;
    let data = num
        .data()
        .par_iter()
        .zip(den.data().par_iter())
        .zip(depth.data().par_iter())
        .map(|((a, b), d)| if *b > 1e-12 { (a / b).clamp(0.0, max) } else { *d })
        .collect();
    Plane::from_vec(depth.width(), depth.height(), data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharper_frame_has_higher_focus() {
        let (_, frames) = crate::stack::tests::banded_stack(64, 32, 2);
        let (f0, f1) = (focus_measure(&frames.0[0], 4), focus_measure(&frames.0[1], 4));
        assert!(f0.get(10, 16) > 10.0 * f1.get(10, 16));
        assert!(f1.get(54, 16) > 10.0 * f0.get(54, 16));
    }

    #[test]
    fn smoothing_fills_low_confidence_areas() {
        // Left half confidently depth 2, right half unknown (zero focus) at depth 0.
        let depth = Plane::from_vec(40, 4, (0..160).map(|i| if i % 40 < 20 { 2.0 } else { 0.0 }).collect());
        let focus = Plane::from_vec(40, 4, (0..160).map(|i| if i % 40 < 20 { 1.0 } else { 0.0 }).collect());
        let s = smooth_depth(&depth, &focus, 3.0, 3);
        assert!((s.get(25, 2) - 2.0).abs() < 1e-3, "{}", s.get(25, 2));
        assert_eq!(smooth_depth(&depth, &focus, 0.0, 3), depth);
    }
}
