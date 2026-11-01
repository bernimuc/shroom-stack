//! Renders the app icon to `assets/icon.png` (1024×1024):
//! `cargo run -p shroom-ui --example make_icon`.

use image::{Rgba, RgbaImage};

const SIZE: u32 = 1024;
/// Supersampling factor per axis for anti-aliased edges.
const SS: u32 = 4;

type Colour = [f32; 4];

/// Signed distance to a rounded rectangle centred at (cx, cy).
fn rounded_rect(x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let (dx, dy) = ((x - cx).abs() - hw + r, (y - cy).abs() - hh + r);
    (dx.max(0.0).hypot(dy.max(0.0))) + dx.max(dy).min(0.0) - r
}

/// Colour of the icon at (x, y) in 0..1024 coordinates, front to back.
fn shade(x: f32, y: f32) -> Option<Colour> {
    // Mushroom: cap (upper half of an ellipse with a flat underside) and stem.
    let (cap_cx, cap_cy, cap_rx, cap_ry) = (512.0, 520.0, 300.0, 230.0);
    let in_cap = y <= cap_cy && ((x - cap_cx) / cap_rx).powi(2) + ((y - cap_cy) / cap_ry).powi(2) <= 1.0;
    if in_cap {
        // Spots on the cap.
        for (sx, sy, r) in [(420.0, 400.0, 42.0), (590.0, 360.0, 34.0), (650.0, 465.0, 26.0), (350.0, 480.0, 22.0)] {
            if (x - sx).hypot(y - sy) <= r {
                return Some([0.98, 0.94, 0.86, 1.0]);
            }
        }
        let t = ((y - (cap_cy - cap_ry)) / cap_ry).clamp(0.0, 1.0);
        return Some([0.80 - 0.10 * t, 0.36 - 0.06 * t, 0.20, 1.0]);
    }
    let gill = rounded_rect(x, y, 512.0, 538.0, 270.0, 18.0, 18.0) <= 0.0;
    if gill {
        return Some([0.86, 0.76, 0.62, 1.0]);
    }
    if rounded_rect(x, y, 512.0, 680.0, 78.0, 150.0, 60.0) <= 0.0 {
        let shade = 1.0 - ((x - 512.0) / 78.0).powi(2) * 0.18;
        return Some([0.96 * shade, 0.91 * shade, 0.80 * shade, 1.0]);
    }
    // Stacked focus layers behind the mushroom.
    for (i, offset) in [0.0, 70.0, 140.0].iter().enumerate() {
        let cy = 690.0 + offset - 70.0;
        if rounded_rect(x, y, 512.0, cy, 360.0 - *offset * 0.25, 42.0, 30.0) <= 0.0 {
            let a = 0.55 + 0.15 * i as f32;
            return Some([0.22 * a + 0.1, 0.55 * a + 0.1, 0.78 * a + 0.1, 1.0]);
        }
    }
    // Background tile.
    if rounded_rect(x, y, 512.0, 512.0, 460.0, 460.0, 200.0) <= 0.0 {
        let t = y / SIZE as f32;
        return Some([0.10 + 0.04 * t, 0.12 + 0.05 * t, 0.17 + 0.07 * t, 1.0]);
    }
    None
}

fn main() {
    let mut img = RgbaImage::new(SIZE, SIZE);
    for (px, py, pixel) in img.enumerate_pixels_mut() {
        let mut acc = [0.0f32; 4];
        for sy in 0..SS {
            for sx in 0..SS {
                let x = px as f32 + (sx as f32 + 0.5) / SS as f32;
                let y = py as f32 + (sy as f32 + 0.5) / SS as f32;
                if let Some(c) = shade(x, y) {
                    // Premultiplied accumulation.
                    for i in 0..3 {
                        acc[i] += c[i] * c[3];
                    }
                    acc[3] += c[3];
                }
            }
        }
        let n = (SS * SS) as f32;
        let alpha = acc[3] / n;
        let rgb = |i: usize| if acc[3] > 0.0 { acc[i] / acc[3] } else { 0.0 };
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        *pixel = Rgba([to_u8(rgb(0)), to_u8(rgb(1)), to_u8(rgb(2)), to_u8(alpha)]);
    }
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    img.save(&out).expect("write icon");
    println!("wrote {}", out.display());
}
