//! Laplacian pyramid stacking.
//!
//! Every frame is decomposed into a Laplacian pyramid (band-pass detail levels plus a
//! low-resolution base). Per level and pixel the coefficient with the highest local
//! detail energy (summed over channels, window radius derived from `smoothing`) is kept;
//! base levels are averaged. Collapsing the fused pyramid gives the output.

use rayon::prelude::*;

use super::{Context, Stage};
use crate::error::Result;
use crate::filter::{box_blur, expand, merge_channels, reduce, split_channels};
use crate::image::{Plane, RgbImage};

/// Coarsest level is at least this many pixels on its shorter side.
const MIN_BASE_SIZE: usize = 16;

struct Pyramid {
    /// Detail levels, finest first; three channels each.
    levels: Vec<[Plane; 3]>,
    base: [Plane; 3],
}

pub(super) fn stack(ctx: &Context, smoothing: usize) -> Result<(RgbImage, Plane)> {
    let n = ctx.source.len();
    let radius = (smoothing / 2).max(1);

    let first = ctx.load(Stage::Analysing, 0, None)?;
    let size = (first.width(), first.height());
    let depth_count = level_count(size.0, size.1);
    let mut fused = decompose(&first, depth_count);
    drop(first);
    let mut fused_energy: Vec<Plane> = fused.levels.iter().map(|l| energy(l, radius)).collect();
    // Which frame supplied the finest-level detail, as a visualisation of the result.
    let mut source = Plane::new(size.0, size.1);

    for i in 1..n {
        let pyr = decompose(&ctx.load(Stage::Analysing, i, Some(size))?, depth_count);
        for (k, level) in pyr.levels.iter().enumerate() {
            let e = energy(level, radius);
            let take: Vec<bool> = e
                .data()
                .par_iter()
                .zip(fused_energy[k].data().par_iter())
                .map(|(a, b)| a > b)
                .collect();
            for (dst, src) in fused.levels[k].iter_mut().zip(level) {
                select(dst, src, &take);
            }
            select(&mut fused_energy[k], &e, &take);
            if k == 0 {
                source
                    .data_mut()
                    .par_iter_mut()
                    .zip(take.par_iter())
                    .for_each(|(s, t)| {
                        if *t {
                            *s = i as f32;
                        }
                    });
            }
        }
        for c in 0..3 {
            fused.base[c]
                .data_mut()
                .par_iter_mut()
                .zip(pyr.base[c].data().par_iter())
                .for_each(|(a, b)| *a += b);
        }
    }
    for c in 0..3 {
        fused.base[c].data_mut().par_iter_mut().for_each(|v| *v /= n as f32);
    }

    (ctx.progress)(super::Progress {
        stage: Stage::Compositing,
        frame: n - 1,
        total: n,
    });
    let mut image = collapse(fused);
    image.data_mut().par_iter_mut().for_each(|v| *v = v.max(0.0));
    Ok((image, source))
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

fn decompose(img: &RgbImage, levels: usize) -> Pyramid {
    let mut current = split_channels(img);
    let mut out = Vec::with_capacity(levels);
    for _ in 0..levels {
        let smaller = current.each_ref().map(reduce);
        let detail = std::array::from_fn(|c| {
            let up = expand(&smaller[c], current[c].width(), current[c].height());
            let data = current[c]
                .data()
                .par_iter()
                .zip(up.data().par_iter())
                .map(|(a, b)| a - b)
                .collect();
            Plane::from_vec(current[c].width(), current[c].height(), data)
        });
        out.push(detail);
        current = smaller;
    }
    Pyramid {
        levels: out,
        base: current,
    }
}

fn collapse(pyr: Pyramid) -> RgbImage {
    let mut current = pyr.base;
    for detail in pyr.levels.into_iter().rev() {
        current = std::array::from_fn(|c| {
            let mut up = expand(&current[c], detail[c].width(), detail[c].height());
            up.data_mut()
                .par_iter_mut()
                .zip(detail[c].data().par_iter())
                .for_each(|(u, d)| *u += d);
            up
        });
    }
    merge_channels(&current)
}

/// Local detail energy of one level: windowed mean of the squared coefficients.
fn energy(level: &[Plane; 3], radius: usize) -> Plane {
    let data = (0..level[0].data().len())
        .into_par_iter()
        .map(|i| level.iter().map(|p| p.data()[i] * p.data()[i]).sum())
        .collect();
    box_blur(&Plane::from_vec(level[0].width(), level[0].height(), data), radius)
}

fn select(dst: &mut Plane, src: &Plane, take: &[bool]) {
    dst.data_mut()
        .par_iter_mut()
        .zip(src.data().par_iter())
        .zip(take.par_iter())
        .for_each(|((d, s), t)| {
            if *t {
                *d = *s;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let back = collapse(decompose(&img, 3));
        let err = img
            .data()
            .iter()
            .zip(back.data())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-5, "{err}");
    }

    #[test]
    fn identical_frames_give_the_frame() {
        let img = merge_channels(&[noise(40, 40, 4), noise(40, 40, 5), noise(40, 40, 6)]);
        let frames = crate::stack::tests::MemFrames(vec![img.clone(), img.clone(), img.clone()]);
        let ctx = Context {
            source: &frames,
            progress: &|_| {},
            cancel: &Default::default(),
        };
        let out = stack(&ctx, 4).unwrap().0;
        let err = img
            .data()
            .iter()
            .zip(out.data())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-5, "{err}");
    }
}
