//! Brush retouching: painting pixels of an (aligned) source frame into the result.
//!
//! A stroke is a series of dabs. Within one stroke every pixel keeps the highest brush
//! coverage it received, so overlapping dabs don't build up. The pixels a stroke touches
//! are recorded per tile (before and after), which makes undo/redo exact and keeps
//! memory proportional to the painted area.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::image::{Rect, RgbImage};

/// Edge length of the tiles undo data is kept in.
const TILE: usize = 64;

/// Brush shape: `radius` in image pixels; `softness` 0 = hard edge, 1 = fully soft.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Brush {
    pub radius: f32,
    pub softness: f32,
}

impl Brush {
    /// Coverage (0..=1) at distance `d` from the dab centre: 1 inside the hard core
    /// (`radius · (1 − softness)`), falling off smoothly to 0 at `radius`.
    pub fn weight(&self, d: f32) -> f32 {
        let core = self.radius * (1.0 - self.softness.clamp(0.0, 1.0));
        if d >= self.radius {
            0.0
        } else if d <= core {
            1.0
        } else {
            let t = (self.radius - d) / (self.radius - core);
            t * t * (3.0 - 2.0 * t)
        }
    }
}

struct StrokeTile {
    before: Vec<f32>,
    after: Vec<f32>,
}

/// One completed stroke, for undo/redo.
struct Stroke {
    tiles: HashMap<(usize, usize), StrokeTile>,
}

/// Tile being painted in the current stroke: original pixels and coverage so far.
struct ActiveTile {
    before: Vec<f32>,
    coverage: Vec<f32>,
}

/// A result image being retouched, with undo/redo history.
pub struct Retouch {
    image: RgbImage,
    active: HashMap<(usize, usize), ActiveTile>,
    undo: Vec<Stroke>,
    redo: Vec<Stroke>,
}

impl Retouch {
    pub fn new(image: RgbImage) -> Self {
        Self { image, active: HashMap::new(), undo: Vec::new(), redo: Vec::new() }
    }

    pub fn image(&self) -> &RgbImage {
        &self.image
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Number of completed strokes.
    pub fn strokes(&self) -> usize {
        self.undo.len()
    }

    fn tile_rect(&self, (tx, ty): (usize, usize)) -> Rect {
        let (x, y) = (tx * TILE, ty * TILE);
        Rect::new(x, y, TILE.min(self.image.width() - x), TILE.min(self.image.height() - y))
    }

    fn read(&self, r: Rect) -> Vec<f32> {
        self.image.crop(r).data().to_vec()
    }

    fn write(&mut self, r: Rect, data: &[f32]) {
        let w = self.image.width();
        let rows = self.image.data_mut();
        for y in 0..r.height {
            let start = ((r.y + y) * w + r.x) * 3;
            rows[start..start + r.width * 3].copy_from_slice(&data[y * r.width * 3..(y + 1) * r.width * 3]);
        }
    }

    /// Paints one dab of `source` (same size as the image) centred at `(cx, cy)`.
    /// Returns the changed area, if any.
    pub fn dab(&mut self, source: &RgbImage, cx: f32, cy: f32, brush: Brush) -> Option<Rect> {
        assert_eq!((source.width(), source.height()), (self.image.width(), self.image.height()));
        let (w, h) = (self.image.width() as f32, self.image.height() as f32);
        let r = brush.radius.max(0.5);
        let (x0, y0) = ((cx - r).floor().max(0.0), (cy - r).floor().max(0.0));
        let (x1, y1) = ((cx + r).ceil().min(w), (cy + r).ceil().min(h));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let area = Rect::new(x0 as usize, y0 as usize, (x1 - x0) as usize, (y1 - y0) as usize);
        let tiles: Vec<(usize, usize)> = (area.y / TILE..=(area.y + area.height - 1) / TILE)
            .flat_map(|ty| (area.x / TILE..=(area.x + area.width - 1) / TILE).map(move |tx| (tx, ty)))
            .collect();
        for &key in &tiles {
            if !self.active.contains_key(&key) {
                let rect = self.tile_rect(key);
                let before = self.read(rect);
                self.active.insert(key, ActiveTile { before, coverage: vec![0.0; rect.width * rect.height] });
            }
        }
        // Update coverage and pixels per tile (in parallel).
        let mut work: Vec<((usize, usize), ActiveTile)> =
            tiles.iter().map(|k| (*k, self.active.remove(k).unwrap())).collect();
        let rects: Vec<Rect> = work.iter().map(|(k, _)| self.tile_rect(*k)).collect();
        let results: Vec<Vec<f32>> = work
            .par_iter_mut()
            .zip(&rects)
            .map(|((_, tile), rect)| {
                let mut out = vec![0.0; rect.width * rect.height * 3];
                for y in 0..rect.height {
                    for x in 0..rect.width {
                        let (px, py) = (rect.x + x, rect.y + y);
                        let d = ((px as f32 - cx).powi(2) + (py as f32 - cy).powi(2)).sqrt();
                        let i = y * rect.width + x;
                        tile.coverage[i] = tile.coverage[i].max(brush.weight(d));
                        let a = tile.coverage[i];
                        let s = source.pixel(px, py);
                        for c in 0..3 {
                            let b = tile.before[i * 3 + c];
                            out[i * 3 + c] = b + (s[c] - b) * a;
                        }
                    }
                }
                out
            })
            .collect();
        for (((key, tile), rect), data) in work.into_iter().zip(rects).zip(results) {
            self.write(rect, &data);
            self.active.insert(key, tile);
        }
        Some(area)
    }

    /// Paints dabs along the line from `from` to `to`, spaced a fraction of the radius
    /// apart (the dab at `from` is assumed painted already). Returns the changed area.
    pub fn line(&mut self, source: &RgbImage, from: (f32, f32), to: (f32, f32), brush: Brush) -> Option<Rect> {
        let spacing = (brush.radius * 0.25).max(0.5);
        let len = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
        let steps = (len / spacing).ceil().max(1.0) as usize;
        let mut changed: Option<Rect> = None;
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let p = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            if let Some(r) = self.dab(source, p.0, p.1, brush) {
                changed = Some(changed.map_or(r, |c| union(c, r)));
            }
        }
        changed
    }

    /// Finishes the current stroke and records it for undo. Clears the redo history.
    pub fn end_stroke(&mut self) {
        if self.active.is_empty() {
            return;
        }
        let keys: Vec<_> = self.active.keys().copied().collect();
        let mut tiles = HashMap::new();
        for key in keys {
            let tile = self.active.remove(&key).unwrap();
            let after = self.read(self.tile_rect(key));
            tiles.insert(key, StrokeTile { before: tile.before, after });
        }
        self.undo.push(Stroke { tiles });
        self.redo.clear();
    }

    fn apply(&mut self, stroke: &Stroke, after: bool) -> Option<Rect> {
        let mut changed: Option<Rect> = None;
        for (key, tile) in &stroke.tiles {
            let rect = self.tile_rect(*key);
            self.write(rect, if after { &tile.after } else { &tile.before });
            changed = Some(changed.map_or(rect, |c| union(c, rect)));
        }
        changed
    }

    /// Undoes the last stroke; returns the changed area.
    pub fn undo(&mut self) -> Option<Rect> {
        self.end_stroke();
        let stroke = self.undo.pop()?;
        let changed = self.apply(&stroke, false);
        self.redo.push(stroke);
        changed
    }

    /// Redoes the last undone stroke; returns the changed area.
    pub fn redo(&mut self) -> Option<Rect> {
        let stroke = self.redo.pop()?;
        let changed = self.apply(&stroke, true);
        self.undo.push(stroke);
        changed
    }
}

/// Smallest rectangle containing both.
pub fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = ((a.x + a.width).max(b.x + b.width), (a.y + a.height).max(b.y + b.height));
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, v: f32) -> RgbImage {
        RgbImage::from_vec(w, h, vec![v; w * h * 3])
    }

    #[test]
    fn brush_weight_profile() {
        let hard = Brush { radius: 10.0, softness: 0.0 };
        assert_eq!(hard.weight(9.9), 1.0);
        assert_eq!(hard.weight(10.0), 0.0);
        let soft = Brush { radius: 10.0, softness: 1.0 };
        assert_eq!(soft.weight(0.0), 1.0);
        assert!((soft.weight(5.0) - 0.5).abs() < 1e-6);
        assert_eq!(soft.weight(10.0), 0.0);
        let half = Brush { radius: 10.0, softness: 0.5 };
        assert_eq!(half.weight(5.0), 1.0);
        assert!(half.weight(6.0) > half.weight(8.0) && half.weight(8.0) > 0.0);
    }

    #[test]
    fn dab_blends_source_by_coverage() {
        let mut r = Retouch::new(flat(200, 100, 0.0));
        let src = flat(200, 100, 1.0);
        let changed = r.dab(&src, 100.0, 50.0, Brush { radius: 20.0, softness: 1.0 }).unwrap();
        assert_eq!(changed, Rect::new(80, 30, 40, 40));
        assert_eq!(r.image().pixel(100, 50), [1.0; 3]);
        let mid = r.image().pixel(110, 50)[0];
        assert!((mid - 0.5).abs() < 1e-6, "{mid}");
        assert_eq!(r.image().pixel(130, 50), [0.0; 3]);
    }

    #[test]
    fn overlapping_dabs_do_not_build_up_within_a_stroke() {
        let mut r = Retouch::new(flat(100, 100, 0.0));
        let src = flat(100, 100, 1.0);
        let brush = Brush { radius: 20.0, softness: 1.0 };
        r.dab(&src, 50.0, 50.0, brush);
        r.dab(&src, 50.0, 50.0, brush);
        assert!((r.image().pixel(60, 50)[0] - 0.5).abs() < 1e-6);
        // A new stroke blends on top of the previous result.
        r.end_stroke();
        r.dab(&src, 50.0, 50.0, brush);
        assert!((r.image().pixel(60, 50)[0] - 0.75).abs() < 1e-6);
    }

    #[test]
    fn line_paints_a_continuous_stroke() {
        let mut r = Retouch::new(flat(300, 60, 0.0));
        let src = flat(300, 60, 1.0);
        let brush = Brush { radius: 8.0, softness: 0.0 };
        r.dab(&src, 20.0, 30.0, brush);
        r.line(&src, (20.0, 30.0), (280.0, 30.0), brush).unwrap();
        assert!((20..=280).all(|x| r.image().pixel(x, 30) == [1.0; 3]));
        assert_eq!(r.image().pixel(150, 45), [0.0; 3]);
    }

    #[test]
    fn undo_and_redo_are_exact() {
        let base = RgbImage::from_vec(130, 70, (0..130 * 70 * 3).map(|i| (i % 97) as f32 / 97.0).collect());
        let mut r = Retouch::new(base.clone());
        let src = flat(130, 70, 0.3);
        let brush = Brush { radius: 15.0, softness: 0.4 };
        r.dab(&src, 60.0, 35.0, brush);
        r.line(&src, (60.0, 35.0), (120.0, 60.0), brush);
        r.end_stroke();
        let after_first = r.image().clone();
        r.dab(&src, 10.0, 10.0, brush);
        r.end_stroke();
        let after_second = r.image().clone();
        assert_eq!(r.strokes(), 2);

        assert!(r.undo().is_some());
        assert_eq!(r.image(), &after_first);
        assert!(r.undo().is_some());
        assert_eq!(r.image(), &base);
        assert!(r.undo().is_none() && !r.can_undo());

        assert!(r.redo().is_some());
        assert!(r.redo().is_some());
        assert_eq!(r.image(), &after_second);
        assert!(!r.can_redo());

        // Painting after an undo discards the redo history.
        r.undo();
        r.dab(&src, 100.0, 20.0, brush);
        r.end_stroke();
        assert!(!r.can_redo());
    }

    #[test]
    fn dabs_outside_the_image_are_ignored() {
        let mut r = Retouch::new(flat(50, 50, 0.0));
        let src = flat(50, 50, 1.0);
        assert!(r.dab(&src, -100.0, 20.0, Brush { radius: 10.0, softness: 0.0 }).is_none());
        let edge = r.dab(&src, 0.0, 0.0, Brush { radius: 10.0, softness: 0.0 }).unwrap();
        assert_eq!(edge, Rect::new(0, 0, 10, 10));
        r.end_stroke();
        r.undo();
        assert_eq!(r.image(), &flat(50, 50, 0.0));
    }
}
