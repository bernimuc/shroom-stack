//! Simple display rendering of linear camera RGB: white balance, camera → sRGB matrix and
//! sRGB transfer curve. Intended for on-screen previews only, not to match a raw converter.

use rayon::prelude::*;

use crate::image::{DisplayImage, Plane, RgbImage};

/// Colour information of a camera image needed for display rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorInfo {
    /// White balance multipliers for camera R, G, B (as shot).
    pub wb: [f32; 3],
    /// DNG `ColorMatrix` for D65 (XYZ → camera), if known.
    pub xyz_to_cam: Option<[[f32; 3]; 3]>,
}

impl Default for ColorInfo {
    fn default() -> Self {
        Self { wb: [1.0; 3], xyz_to_cam: None }
    }
}

/// Linear sRGB (D65) → XYZ.
const SRGB_TO_XYZ: [[f32; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175],
    [0.019_333_9, 0.119_192, 0.950_304_1],
];

impl ColorInfo {
    /// Matrix converting white-balanced camera RGB into linear sRGB.
    pub fn cam_to_srgb(&self) -> [[f32; 3]; 3] {
        let Some(xyz_to_cam) = self.xyz_to_cam else {
            return IDENTITY;
        };
        // Rows are normalised so that sRGB white maps to camera (1,1,1), i.e. to a
        // white-balanced neutral.
        let mut srgb_to_cam = mat_mul(&xyz_to_cam, &SRGB_TO_XYZ);
        for row in &mut srgb_to_cam {
            let sum: f32 = row.iter().sum();
            if sum.abs() > f32::EPSILON {
                row.iter_mut().for_each(|v| *v /= sum);
            }
        }
        mat_inverse(&srgb_to_cam).unwrap_or(IDENTITY)
    }
}

/// Renders `image` for display, downscaling by an integer box filter so that neither side
/// exceeds `max_size` (if given).
pub fn render_display(image: &RgbImage, color: &ColorInfo, max_size: Option<usize>) -> DisplayImage {
    let factor = max_size.map(|m| image.width().max(image.height()).div_ceil(m.max(1))).unwrap_or(1).max(1);
    let (w, h) = (image.width() / factor, image.height() / factor);
    let m = color.cam_to_srgb();
    // Normalise WB so the smallest multiplier is 1 and highlights clip consistently.
    let wb_min = color.wb.iter().copied().fold(f32::INFINITY, f32::min).max(f32::EPSILON);
    let wb = color.wb.map(|v| v / wb_min);
    let mut data = vec![0u8; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for sy in y * factor..(y + 1) * factor {
                for sx in x * factor..(x + 1) * factor {
                    let p = image.pixel(sx, sy);
                    (0..3).for_each(|c| acc[c] += p[c]);
                }
            }
            let n = (factor * factor) as f32;
            let cam = [0, 1, 2].map(|c| (acc[c] / n * wb[c]).min(1.0));
            for c in 0..3 {
                let lin = m[c][0] * cam[0] + m[c][1] * cam[1] + m[c][2] * cam[2];
                row[x * 3 + c] = (srgb_encode(lin) * 255.0 + 0.5) as u8;
            }
        }
    });
    DisplayImage { width: w, height: h, data }
}

/// Renders a depth map (frame index per pixel, `0..=max_index`) in false colour: blue for
/// the first frame through green to red for the last. Downscales like [`render_display`].
pub fn render_depth_map(depth: &Plane, max_index: f32, max_size: Option<usize>) -> DisplayImage {
    let factor = max_size.map(|m| depth.width().max(depth.height()).div_ceil(m.max(1))).unwrap_or(1).max(1);
    let (w, h) = (depth.width() / factor, depth.height() / factor);
    let mut data = vec![0u8; w * h * 3];
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let t = depth.get(x * factor, y * factor) / max_index.max(f32::EPSILON);
            row[x * 3..x * 3 + 3].copy_from_slice(&false_colour(t));
        }
    });
    DisplayImage { width: w, height: h, data }
}

/// Blue → cyan → green → yellow → red for `t` in `0..=1`.
fn false_colour(t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0) * 4.0;
    let (r, g, b) = match t {
        t if t < 1.0 => (0.0, t, 1.0),
        t if t < 2.0 => (0.0, 1.0, 2.0 - t),
        t if t < 3.0 => (t - 2.0, 1.0, 0.0),
        t => (1.0, 4.0 - t, 0.0),
    };
    [r, g, b].map(|v| (v * 255.0 + 0.5) as u8)
}

/// sRGB transfer function; input is clamped to `0..=1`.
pub fn srgb_encode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn mat_mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn mat_inverse(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = [
        [e * i - f * h, c * h - b * i, b * f - c * e],
        [f * g - d * i, a * i - c * g, c * d - a * f],
        [d * h - e * g, b * g - a * h, a * e - b * d],
    ];
    Some(inv.map(|row| row.map(|v| v / det)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_curve_endpoints_and_midpoint() {
        assert_eq!(srgb_encode(0.0), 0.0);
        assert!((srgb_encode(1.0) - 1.0).abs() < 1e-6);
        assert!((srgb_encode(0.18) - 0.4613).abs() < 1e-3);
        assert_eq!(srgb_encode(-1.0), 0.0);
        assert!((srgb_encode(2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn inverse_roundtrip() {
        let inv = mat_inverse(&SRGB_TO_XYZ).unwrap();
        let id = mat_mul(&SRGB_TO_XYZ, &inv);
        for i in 0..3 {
            for j in 0..3 {
                assert!((id[i][j] - IDENTITY[i][j]).abs() < 1e-5);
            }
        }
        assert!(mat_inverse(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]).is_none());
    }

    #[test]
    fn neutral_stays_neutral_with_matrix() {
        // Typical camera matrix (Canon EOS R5, D65).
        let color = ColorInfo {
            wb: [1.0; 3],
            xyz_to_cam: Some([[0.9766, -0.3416, -0.0464], [-0.4176, 1.1999, 0.2463], [-0.0786, 0.1513, 0.6127]]),
        };
        let m = color.cam_to_srgb();
        let out: Vec<f32> = (0..3).map(|c| m[c].iter().sum()).collect();
        for v in out {
            assert!((v - 1.0).abs() < 1e-4, "camera neutral must map to sRGB white");
        }
    }

    #[test]
    fn depth_map_colours() {
        let depth = Plane::from_vec(3, 1, vec![0.0, 2.0, 4.0]);
        let d = render_depth_map(&depth, 4.0, None);
        assert_eq!(&d.data[0..3], &[0, 0, 255]);
        assert_eq!(&d.data[3..6], &[0, 255, 0]);
        assert_eq!(&d.data[6..9], &[255, 0, 0]);
    }

    #[test]
    fn white_balance_and_downscale() {
        let mut img = RgbImage::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                // Camera-neutral grey after WB of [2, 1, 4].
                img.set_pixel(x, y, [0.1, 0.2, 0.05]);
            }
        }
        let color = ColorInfo { wb: [2.0, 1.0, 4.0], xyz_to_cam: None };
        let d = render_display(&img, &color, Some(2));
        assert_eq!((d.width, d.height), (2, 1));
        let expected = (srgb_encode(0.2) * 255.0 + 0.5) as u8;
        assert!(d.data.iter().all(|&v| v == expected), "{:?}", d.data);
    }
}
