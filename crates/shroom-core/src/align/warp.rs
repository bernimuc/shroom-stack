//! Resampling of images under an affine transform with Catmull–Rom bicubic
//! interpolation (sharper than bilinear, which matters for focus stacking).

use rayon::prelude::*;

use super::transform::Affine;
use crate::image::{Plane, Rect, RgbImage};

/// Produces the `rect` region (in output coordinates) of `src` warped by `t`:
/// `out(x, y) = src(t(rect.x + x, rect.y + y))`. Outside samples clamp to the edge.
pub fn warp_rgb(src: &RgbImage, t: &Affine, rect: Rect) -> RgbImage {
    if t.is_identity(1e-9) {
        return src.crop(rect);
    }
    let (sw, sh) = (src.width(), src.height());
    let mut out = RgbImage::new(rect.width, rect.height);
    out.data_mut()
        .par_chunks_mut(rect.width * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..rect.width {
                let (sx, sy) = t.apply((rect.x + x) as f64, (rect.y + y) as f64);
                let (ix, iy) = (sx.floor() as isize, sy.floor() as isize);
                let (wx, wy) = (
                    cubic_weights((sx - ix as f64) as f32),
                    cubic_weights((sy - iy as f64) as f32),
                );
                let mut acc = [0.0f32; 3];
                for (j, wyj) in wy.iter().enumerate() {
                    let yy = (iy + j as isize - 1).clamp(0, sh as isize - 1) as usize;
                    for (i, wxi) in wx.iter().enumerate() {
                        let xx = (ix + i as isize - 1).clamp(0, sw as isize - 1) as usize;
                        let p = &src.data()[(yy * sw + xx) * 3..][..3];
                        let w = wxi * wyj;
                        acc.iter_mut().zip(p).for_each(|(a, v)| *a += w * v);
                    }
                }
                row[x * 3..x * 3 + 3].copy_from_slice(&acc.map(|v| v.max(0.0)));
            }
        });
    out
}

/// Bilinear sample of a plane; `None` if outside the image.
pub(super) fn sample_bilinear(p: &Plane, x: f64, y: f64) -> Option<f32> {
    let (w, h) = (p.width() as f64, p.height() as f64);
    if x < 0.0 || y < 0.0 || x > w - 1.0 || y > h - 1.0 {
        return None;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(p.width() - 1), (y0 + 1).min(p.height() - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let top = p.get(x0, y0) * (1.0 - fx) + p.get(x1, y0) * fx;
    let bottom = p.get(x0, y1) * (1.0 - fx) + p.get(x1, y1) * fx;
    Some(top * (1.0 - fy) + bottom * fy)
}

/// Catmull–Rom weights for taps at offsets −1, 0, 1, 2 given fraction `t`.
fn cubic_weights(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, [x as f32, y as f32, (x + y) as f32]);
            }
        }
        img
    }

    #[test]
    fn weights_sum_to_one_and_interpolate() {
        for t in [0.0, 0.25, 0.5, 0.9] {
            assert!((cubic_weights(t).iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
        assert_eq!(cubic_weights(0.0), [0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn integer_shift_copies_pixels() {
        let img = ramp(10, 8);
        let out = warp_rgb(&img, &Affine::translation(2.0, 1.0), Rect::new(0, 0, 6, 5));
        assert_eq!(out.pixel(0, 0), img.pixel(2, 1));
        assert_eq!(out.pixel(5, 4), img.pixel(7, 5));
    }

    #[test]
    fn linear_content_is_resampled_exactly() {
        let img = ramp(40, 40);
        let t = Affine::similarity((20.0, 20.0), 1.05, 0.03, (0.3, -0.7));
        let out = warp_rgb(&img, &t, Rect::new(8, 8, 24, 24));
        for (x, y) in [(0, 0), (10, 13), (23, 23)] {
            let (sx, sy) = t.apply((8 + x) as f64, (8 + y) as f64);
            let p = out.pixel(x, y);
            assert!(
                (p[0] as f64 - sx).abs() < 1e-3 && (p[1] as f64 - sy).abs() < 1e-3,
                "{p:?} vs {sx},{sy}"
            );
        }
    }

    #[test]
    fn bilinear_sampling() {
        let p = Plane::from_vec(2, 2, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(sample_bilinear(&p, 0.5, 0.5), Some(1.5));
        assert_eq!(sample_bilinear(&p, 1.0, 1.0), Some(3.0));
        assert_eq!(sample_bilinear(&p, 1.01, 0.0), None);
    }
}
