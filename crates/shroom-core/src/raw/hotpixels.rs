//! Removal of hot (stuck) pixels from Bayer data.
//!
//! A hot photosite sits at the same sensor position in every frame. After alignment the
//! frames are shifted and scaled differently, so in a stacked result each frame's copy
//! lands elsewhere and a single defect becomes a dotted line; both stacking methods
//! also see a lone bright pixel as maximal detail and keep it. Raw converters hide
//! such pixels, so they go unnoticed in the individual frames.

use rayon::prelude::*;

use crate::image::Plane;

/// A photosite brighter than this multiple of all its same-colour neighbours…
const RATIO: f32 = 3.0;
/// …and than this multiple of its direct neighbours (lower, because those are other
/// colours whose sensitivity differs, e.g. green reads about twice as bright as blue)…
const ADJACENT_RATIO: f32 = 1.5;
/// …and by at least this much (fraction of white) counts as hot. The photosite must also
/// outshine its direct neighbours (other colours): real detail, even a tiny glint, is
/// spread over several adjacent photosites by the lens; a defect stands alone.
const MIN_EXCESS: f32 = 0.01;

/// Same-colour neighbours in a Bayer mosaic (two photosites away, valid for every colour).
const NEIGHBOURS: [(isize, isize); 8] = [(-2, -2), (0, -2), (2, -2), (-2, 0), (2, 0), (-2, 2), (0, 2), (2, 2)];
/// Directly adjacent photosites (other colours, or green for green on the diagonals).
const ADJACENT: [(isize, isize); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

/// Replaces hot photosites with the median of their same-colour neighbours; returns how
/// many were replaced. Photosites within two pixels of the border are left alone.
pub fn remove_hot_pixels(mosaic: &mut Plane) -> usize {
    let (w, h) = (mosaic.width(), mosaic.height());
    if w < 5 || h < 5 {
        return 0;
    }
    let wi = w as isize;
    let same = NEIGHBOURS.map(|(dx, dy)| dy * wi + dx);
    let adjacent = ADJACENT.map(|(dx, dy)| dy * wi + dx);
    let data = mosaic.data();
    let fixes: Vec<(usize, f32)> = (2..h - 2)
        .into_par_iter()
        .flat_map_iter(|y| {
            (2..w - 2).filter_map(move |x| {
                let i = y * w + x;
                let v = data[i];
                if v <= MIN_EXCESS {
                    return None;
                }
                let at = |offset: isize| data[(i as isize + offset) as usize];
                // Early exit: almost every photosite already fails at its first neighbour.
                let same_bound = (v - MIN_EXCESS).min(v / RATIO);
                if !same.iter().all(|&o| at(o) < same_bound) || !adjacent.iter().all(|&o| at(o) < v / ADJACENT_RATIO) {
                    return None;
                }
                let mut nb = same.map(at);
                nb.sort_by(f32::total_cmp);
                Some((i, 0.5 * (nb[3] + nb[4])))
            })
        })
        .collect();
    let data = mosaic.data_mut();
    for &(i, v) in &fixes {
        data[i] = v;
    }
    fixes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::tests::noise;

    /// Dark, slightly noisy mosaic like a shadow area.
    fn dark(w: usize, h: usize) -> Plane {
        let mut p = noise(w, h, 3);
        p.data_mut().iter_mut().for_each(|v| *v = 0.003 + 0.002 * *v);
        p
    }

    #[test]
    fn removes_isolated_hot_pixels() {
        let mut m = dark(40, 30);
        let original = m.clone();
        m.set(10, 10, 0.06);
        m.set(25, 17, 0.2);
        assert_eq!(remove_hot_pixels(&mut m), 2);
        for (x, y) in [(10, 10), (25, 17)] {
            assert!(m.get(x, y) < 0.006, "({x}, {y}) = {}", m.get(x, y));
        }
        // Everything else is untouched.
        let changed = m.data().iter().zip(original.data()).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 2);
    }

    #[test]
    fn keeps_real_detail_and_noise() {
        // Noise alone.
        let mut m = dark(40, 30);
        assert_eq!(remove_hot_pixels(&mut m), 0);
        // A small bright spot spread over several photosites (as the lens renders it); its
        // centre outshines all same-colour neighbours, but not the adjacent photosites.
        let mut m = dark(40, 30);
        for y in 14..17 {
            for x in 14..17 {
                m.set(x, y, 0.3);
            }
        }
        assert_eq!(remove_hot_pixels(&mut m), 0);
        // A bright line.
        let mut m = dark(40, 30);
        for x in 0..40 {
            m.set(x, 12, 0.5);
        }
        assert_eq!(remove_hot_pixels(&mut m), 0);
        // A moderately brighter photosite in a bright area.
        let mut m = Plane::from_vec(40, 30, vec![0.5; 1200]);
        m.set(20, 20, 0.6);
        assert_eq!(remove_hot_pixels(&mut m), 0);
    }

    #[test]
    fn hot_blue_pixel_next_to_brighter_green_is_found() {
        // Neutral shadow in camera RGB: green reads about twice as bright as blue.
        let mut m = Plane::new(40, 30);
        for y in 0..30 {
            for x in 0..40 {
                let green = (x + y) % 2 == 1;
                m.set(x, y, if green { 0.012 } else { 0.006 });
            }
        }
        m.set(20, 20, 0.024);
        assert_eq!(remove_hot_pixels(&mut m), 1);
        assert_eq!(m.get(20, 20), 0.006);
    }

    #[test]
    fn border_and_tiny_images_are_safe() {
        let mut m = dark(40, 30);
        m.set(1, 1, 0.5);
        assert_eq!(remove_hot_pixels(&mut m), 0);
        let mut tiny = Plane::new(3, 3);
        assert_eq!(remove_hot_pixels(&mut tiny), 0);
    }
}
