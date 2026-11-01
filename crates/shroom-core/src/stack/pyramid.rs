//! Laplacian pyramid stacking.
//!
//! Every frame is decomposed into a Laplacian pyramid (band-pass detail levels plus a
//! low-resolution base). Per level and pixel the coefficient with the highest local
//! detail energy (summed over channels, window radius derived from `smoothing`) is kept;
//! base levels are averaged. Collapsing the fused pyramid gives the output.
//!
//! The fusion runs on log-encoded values. In linear values, mixing detail of different
//! frames undershoots below black next to bright edges; clipping those channels
//! independently crushed the shadows and tinted them (typically purple). In the log
//! domain halos are relative to the local brightness and the result stays positive.

use super::{Context, Stage};
use crate::compute::{Backend, box_blur};
use crate::error::Result;
use crate::image::{Plane, RgbImage};

/// Offset of the log encoding (fraction of white). Below it the encoding is close to
/// linear, so shadow noise isn't exaggerated (a smaller offset brings colourful noise
/// back), while undershoot next to bright edges stays relative to the brightness (a
/// larger offset brings back crushed, tinted shadows). Tuned on the example stacks.
const LOG_OFFSET: f32 = 1.0 / 256.0;

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
    ctx.for_each_frame(Stage::Analysing, 0..n, |i, mut frame| {
        let (w, h) = b.rgb_size(&frame);
        // Live view: the frame (before encoding) and the source map so far.
        let map = state.as_ref().map(|(_, _, source)| source);
        ctx.snapshot(Stage::Analysing, i, &frame, map, false);
        b.log_rgb(&mut frame, LOG_OFFSET);
        let pyr = decompose(b, &frame, level_count(w, h));
        drop(frame);
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
        Ok(())
    })?;
    let (mut fused, _, source) = state.expect("at least two frames");
    for base in &mut fused.base {
        b.scale(base, 1.0 / n as f32);
    }

    (ctx.progress)(super::Progress { stage: Stage::Compositing, frame: n - 1, total: n });
    let mut image = collapse(b, fused);
    b.exp_rgb(&mut image, LOG_OFFSET);
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

    /// Regression test: a dark, textured area next to a bright edge, sharp in different
    /// frames. Fusing linear values undershot below black there, which crushed the
    /// shadows and, clipped per channel, tinted them.
    #[test]
    fn shadows_next_to_bright_edges_are_not_crushed() {
        use crate::filter::{gaussian_blur, split_channels};
        let (w, h) = (128, 64);
        let dark_texture = noise(w, h, 9);
        let scene = |x: usize, y: usize| -> [f32; 3] {
            if x < w / 2 {
                [0.9, 0.85, 0.8]
            } else {
                let t = 0.004 + 0.004 * dark_texture.get(x, y);
                [t * 0.7, t, t * 0.5]
            }
        };
        let mut sharp = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                sharp.set_pixel(x, y, scene(x, y));
            }
        }
        let blurred = merge_channels(&split_channels(&sharp).map(|p| gaussian_blur(&p, 4.0)));
        // Frame 0: bright side sharp, dark side blurred; frame 1 the other way round.
        let mut f0 = blurred.clone();
        let mut f1 = blurred.clone();
        for y in 0..h {
            for x in 0..w {
                if x < w / 2 { f0.set_pixel(x, y, sharp.pixel(x, y)) } else { f1.set_pixel(x, y, sharp.pixel(x, y)) }
            }
        }
        let frames = crate::stack::tests::MemFrames(vec![f0, f1]);
        let cancel = Default::default();
        let ctx = Context::new(&Cpu, &frames, None, &|_| {}, None, &cancel);
        let out = stack(&ctx, 4).unwrap().0;

        // Dark strip next to the edge (where halos appear), away from the image border.
        let (mut clipped, mut sum_out, mut sum_true, mut n) = (0, [0.0f32; 3], [0.0f32; 3], 0);
        for y in 8..h - 8 {
            for x in w / 2 + 2..w / 2 + 20 {
                let (p, t) = (out.pixel(x, y), sharp.pixel(x, y));
                clipped += p.iter().filter(|v| **v <= 0.0).count();
                (0..3).for_each(|c| {
                    sum_out[c] += p[c];
                    sum_true[c] += t[c];
                });
                n += 3;
            }
        }
        // Linear fusion: 11 % clipped, levels 4–7× (R and B more than G: purple).
        // Some halo is inherent to pyramid fusion; it must stay moderate and neutral.
        assert!(clipped * 100 < n, "{clipped} of {n} samples clipped to black");
        let ratios = [0, 1, 2].map(|c| sum_out[c] / sum_true[c]);
        assert!(ratios.iter().all(|r| (0.8..3.0).contains(r)), "shadow levels {ratios:?} × the true level");
        let spread = ratios.iter().cloned().fold(f32::MIN, f32::max) / ratios.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread < 1.5, "colour shift in the shadows: channel levels {ratios:?}");
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
