//! Basic filters on [`Plane`]s: separable convolution, blurs, Laplacian, and the
//! reduce/expand operations of Gaussian/Laplacian pyramids.
//!
//! All filters clamp coordinates at the image border (edge pixels are repeated).

use rayon::prelude::*;

use crate::image::{Plane, RgbImage};

/// Convolves with `kernel` horizontally and vertically. `kernel` has odd length and is
/// centred.
pub fn convolve_separable(src: &Plane, kernel: &[f32]) -> Plane {
    assert!(kernel.len() % 2 == 1, "kernel length must be odd");
    let r = (kernel.len() / 2) as isize;
    let (w, h) = (src.width(), src.height());
    let clamp = |i: isize, n: usize| i.clamp(0, n as isize - 1) as usize;

    let mut tmp = Plane::new(w, h);
    tmp.data_mut().par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let line = &src.data()[y * w..(y + 1) * w];
        for (x, out) in row.iter_mut().enumerate() {
            *out = kernel
                .iter()
                .enumerate()
                .map(|(k, kv)| kv * line[clamp(x as isize + k as isize - r, w)])
                .sum();
        }
    });
    let mut out = Plane::new(w, h);
    out.data_mut().par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (k, kv) in kernel.iter().enumerate() {
            let sy = clamp(y as isize + k as isize - r, h);
            let line = &tmp.data()[sy * w..(sy + 1) * w];
            row.iter_mut().zip(line).for_each(|(o, v)| *o += kv * v);
        }
    });
    out
}

/// Mean over a `(2r+1)²` window.
pub fn box_blur(src: &Plane, radius: usize) -> Plane {
    let n = 2 * radius + 1;
    convolve_separable(src, &vec![1.0 / n as f32; n])
}

/// Gaussian blur with standard deviation `sigma` (kernel truncated at 3σ).
pub fn gaussian_blur(src: &Plane, sigma: f32) -> Plane {
    if sigma <= 0.0 {
        return src.clone();
    }
    let r = (3.0 * sigma).ceil() as isize;
    let mut kernel: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|v| *v /= sum);
    convolve_separable(src, &kernel)
}

/// 4-neighbour discrete Laplacian.
pub fn laplacian(src: &Plane) -> Plane {
    let (w, h) = (src.width(), src.height());
    let mut out = Plane::new(w, h);
    out.data_mut().par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (up, down) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let (left, right) = (x.saturating_sub(1), (x + 1).min(w - 1));
            *o = src.get(left, y) + src.get(right, y) + src.get(x, up) + src.get(x, down) - 4.0 * src.get(x, y);
        }
    });
    out
}

/// Binomial 5-tap kernel used for pyramids.
const BINOMIAL5: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Blurs and halves the resolution (rounding up).
pub fn reduce(src: &Plane) -> Plane {
    let blurred = convolve_separable(src, &BINOMIAL5);
    let (w, h) = (src.width().div_ceil(2), src.height().div_ceil(2));
    let mut out = Plane::new(w, h);
    out.data_mut().par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = blurred.get(2 * x, 2 * y);
        }
    });
    out
}

/// Upsamples `src` to `width × height` (≈ 2× its size), the counterpart of [`reduce`]:
/// `out(x) = Σ_j k(x − 2j)·src(j)` with the binomial kernel scaled by 2 per axis.
pub fn expand(src: &Plane, width: usize, height: usize) -> Plane {
    // Taps for even and odd output positions: x = 2j → {1,6,1}/8 at j-1, j, j+1;
    // x = 2j+1 → {4,4}/8 at j, j+1.
    let expand_1d = |get: &dyn Fn(usize) -> f32, n_src: usize, x: usize| -> f32 {
        let c = |j: isize| get(j.clamp(0, n_src as isize - 1) as usize);
        let j = (x / 2) as isize;
        if x.is_multiple_of(2) {
            (c(j - 1) + 6.0 * c(j) + c(j + 1)) / 8.0
        } else {
            (c(j) + c(j + 1)) / 2.0
        }
    };
    let (sw, sh) = (src.width(), src.height());
    let mut tmp = Plane::new(width, sh);
    tmp.data_mut().par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let line = &src.data()[y * sw..(y + 1) * sw];
        for (x, o) in row.iter_mut().enumerate() {
            *o = expand_1d(&|i| line[i], sw, x);
        }
    });
    let mut out = Plane::new(width, height);
    out.data_mut().par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = expand_1d(&|i| tmp.get(x, i), sh, y);
        }
    });
    out
}

/// Mean over `factor × factor` blocks (partial blocks at the border are dropped).
pub fn downsample_box(src: &Plane, factor: usize) -> Plane {
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

/// Splits an RGB image into three planes.
pub fn split_channels(img: &RgbImage) -> [Plane; 3] {
    [0, 1, 2].map(|c| {
        Plane::from_vec(
            img.width(),
            img.height(),
            img.data().iter().skip(c).step_by(3).copied().collect(),
        )
    })
}

/// Inverse of [`split_channels`].
pub fn merge_channels(planes: &[Plane; 3]) -> RgbImage {
    let (w, h) = (planes[0].width(), planes[0].height());
    let mut img = RgbImage::new(w, h);
    for (c, plane) in planes.iter().enumerate() {
        img.data_mut()
            .iter_mut()
            .skip(c)
            .step_by(3)
            .zip(plane.data())
            .for_each(|(d, s)| *d = *s);
    }
    img
}

/// Luminance-like sum of the camera channels, used for focus measurement.
pub fn luminance(img: &RgbImage) -> Plane {
    Plane::from_vec(
        img.width(),
        img.height(),
        img.data()
            .par_chunks(3)
            .map(|p| (p[0] + 2.0 * p[1] + p[2]) / 4.0)
            .collect(),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deterministic pseudo-random texture in `0..1`.
    pub(crate) fn noise(w: usize, h: usize, seed: u32) -> Plane {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        let data = (0..w * h)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state % 10_000) as f32 / 10_000.0
            })
            .collect();
        Plane::from_vec(w, h, data)
    }

    fn max_diff(a: &Plane, b: &Plane) -> f32 {
        a.data()
            .iter()
            .zip(b.data())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn blurs_preserve_constants() {
        let p = Plane::from_vec(7, 5, vec![0.3; 35]);
        assert!(max_diff(&box_blur(&p, 3), &p) < 1e-6);
        assert!(max_diff(&gaussian_blur(&p, 2.0), &p) < 1e-6);
        assert!(laplacian(&p).data().iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn box_blur_averages_window() {
        let mut p = Plane::new(5, 5);
        p.set(2, 2, 9.0);
        let b = box_blur(&p, 1);
        assert!((b.get(1, 1) - 1.0).abs() < 1e-6);
        assert!((b.get(3, 3) - 1.0).abs() < 1e-6);
        assert_eq!(b.get(0, 0), 0.0);
    }

    #[test]
    fn gaussian_blur_reduces_detail() {
        let p = noise(64, 64, 1);
        let energy = |p: &Plane| laplacian(p).data().iter().map(|v| v * v).sum::<f32>();
        assert!(energy(&gaussian_blur(&p, 2.0)) < 0.05 * energy(&p));
    }

    #[test]
    fn laplacian_of_quadratic_is_constant() {
        let mut p = Plane::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                p.set(x, y, (x * x + y * y) as f32);
            }
        }
        assert_eq!(laplacian(&p).get(4, 4), 4.0);
    }

    #[test]
    fn reduce_and_expand_sizes() {
        let p = noise(13, 6, 2);
        let r = reduce(&p);
        assert_eq!((r.width(), r.height()), (7, 3));
        let e = expand(&r, 13, 6);
        assert_eq!((e.width(), e.height()), (13, 6));
    }

    #[test]
    fn expand_preserves_constants_and_linear_ramps() {
        let c = Plane::from_vec(5, 4, vec![0.7; 20]);
        assert!(max_diff(&expand(&c, 10, 8), &Plane::from_vec(10, 8, vec![0.7; 80])) < 1e-6);
        // A linear ramp sampled at even positions is interpolated exactly in the interior.
        let ramp = Plane::from_vec(6, 1, (0..6).map(|x| 2.0 * x as f32).collect());
        let e = expand(&ramp, 12, 1);
        for x in 1..10 {
            assert!((e.get(x, 0) - x as f32).abs() < 1e-5, "x={x}: {}", e.get(x, 0));
        }
    }

    #[test]
    fn box_downsample_averages() {
        let p = Plane::from_vec(4, 2, vec![0.0, 2.0, 4.0, 6.0, 2.0, 4.0, 6.0, 8.0]);
        assert_eq!(downsample_box(&p, 2).data(), &[2.0, 6.0]);
    }

    #[test]
    fn channel_split_roundtrip() {
        let mut img = RgbImage::new(3, 2);
        img.set_pixel(1, 1, [0.1, 0.2, 0.3]);
        let planes = split_channels(&img);
        assert_eq!(planes[2].get(1, 1), 0.3);
        assert_eq!(merge_channels(&planes), img);
    }
}
