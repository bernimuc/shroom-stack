//! Laplacian pyramid stacking.
//!
//! Every frame is decomposed into a Laplacian pyramid (band-pass detail levels plus a
//! low-resolution base). Per level and pixel the coefficient with the highest local
//! detail energy (summed over channels, window radius derived from `smoothing`) is kept;
//! base levels are averaged. Collapsing the fused pyramid gives the output.

use super::{Context, Stage};
use crate::compute::{Backend, box_blur};
use crate::error::Result;
use crate::image::{Plane, RgbImage};

/// Coarsest level is at least this many pixels on its shorter side.
const MIN_BASE_SIZE: usize = 16;

/// Fused pyramid, its per-level energies and the finest-level source map.
type Fused<P> = (Pyramid<P>, Vec<P>, P);

struct Pyramid<P> {
    /// Detail levels, finest first; three channels each.
    levels: Vec<[P; 3]>,
    base: [P; 3],
}

pub(super) fn stack<B: Backend>(ctx: &Context<B>, smoothing: usize) -> Result<(RgbImage, Plane)> {
    let b = ctx.backend;
    let n = ctx.source.len();
    let radius = (smoothing / 2).max(1);

    // Fused pyramid, its per-level energies and the finest-level source map (which
    // frame supplied the detail, as a visualisation of the result).
    let mut state: Option<Fused<B::Plane>> = None;
    ctx.for_each_frame(Stage::Analysing, 0..n, |i, frame| {
        let (w, h) = b.rgb_size(&frame);
        let pyr = decompose(b, &frame, level_count(w, h));
        match &mut state {
            None => {
                let energies = pyr.levels.iter().map(|l| energy(b, l, radius)).collect();
                state = Some((pyr, energies, b.zeros_plane(w, h)));
            }
            Some((fused, fused_energy, source)) => {
                for (k, level) in pyr.levels.iter().enumerate() {
                    let e = energy(b, level, radius);
                    let map = if k == 0 { Some(&mut *source) } else { None };
                    b.fuse(&mut fused.levels[k], &mut fused_energy[k], level, &e, map, i);
                }
                for (dst, src) in fused.base.iter_mut().zip(&pyr.base) {
                    b.add_assign(dst, src);
                }
            }
        }
        let map = state.as_ref().map(|(_, _, source)| source);
        ctx.snapshot(Stage::Analysing, i, &frame, map, i + 1 == n);
        Ok(())
    })?;
    let (mut fused, _, source) = state.expect("at least two frames");
    for base in &mut fused.base {
        b.scale(base, 1.0 / n as f32);
    }

    (ctx.progress)(super::Progress { stage: Stage::Compositing, frame: n - 1, total: n });
    let mut image = collapse(b, fused);
    b.clamp_min_rgb(&mut image, 0.0);
    Ok((b.download_rgb(&image), b.download_plane(&source)))
}

fn level_count(w: usize, h: usize) -> usize {
    let mut levels = 0;
    let mut m = w.min(h);
    while m / 2 >= MIN_BASE_SIZE {
        m = m.div_ceil(2);
        levels += 1;
    }
    levels.max(1)
}

fn decompose<B: Backend>(b: &B, img: &B::Rgb, levels: usize) -> Pyramid<B::Plane> {
    let mut current = b.split(img);
    let mut out = Vec::with_capacity(levels);
    for _ in 0..levels {
        let smaller = current.each_ref().map(|p| b.reduce(p));
        let detail = std::array::from_fn(|c| {
            let (w, h) = b.plane_size(&current[c]);
            b.sub(&current[c], &b.expand(&smaller[c], w, h))
        });
        out.push(detail);
        current = smaller;
    }
    Pyramid { levels: out, base: current }
}

fn collapse<B: Backend>(b: &B, pyr: Pyramid<B::Plane>) -> B::Rgb {
    let mut current = pyr.base;
    for detail in pyr.levels.into_iter().rev() {
        current = std::array::from_fn(|c| {
            let (w, h) = b.plane_size(&detail[c]);
            let mut up = b.expand(&current[c], w, h);
            b.add_assign(&mut up, &detail[c]);
            up
        });
    }
    b.merge(&current)
}

/// Local detail energy of one level: windowed mean of the squared coefficients.
fn energy<B: Backend>(b: &B, level: &[B::Plane; 3], radius: usize) -> B::Plane {
    box_blur(b, &b.sum_squares(level), radius)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::Cpu;
    use crate::filter::merge_channels;
    use crate::filter::tests::noise;

    #[test]
    fn level_count_keeps_base_large_enough() {
        assert_eq!(level_count(16, 16), 1);
        assert_eq!(level_count(64, 40), 1);
        assert_eq!(level_count(64, 64), 2);
        assert_eq!(level_count(7128, 4732), 8);
    }

    #[test]
    fn decompose_collapse_is_lossless() {
        let img = merge_channels(&[noise(53, 37, 1), noise(53, 37, 2), noise(53, 37, 3)]);
        let back = collapse(&Cpu, decompose(&Cpu, &img, 3));
        let err = img.data().iter().zip(back.data()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "{err}");
    }

    #[test]
    fn identical_frames_give_the_frame() {
        let img = merge_channels(&[noise(40, 40, 4), noise(40, 40, 5), noise(40, 40, 6)]);
        let frames = crate::stack::tests::MemFrames(vec![img.clone(), img.clone(), img.clone()]);
        let cancel = Default::default();
        let ctx = Context::new(&Cpu, &frames, None, &|_| {}, None, &cancel);
        let out = stack(&ctx, 4).unwrap().0;
        let err = img.data().iter().zip(out.data()).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "{err}");
    }
}
