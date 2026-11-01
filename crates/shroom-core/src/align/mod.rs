//! Frame alignment: compensates shift, rotation and focus breathing between frames.
//!
//! Each frame is registered to its predecessor (neighbouring frames look most alike) and
//! the transforms are chained, so every frame is mapped into the geometry of the first
//! frame. The output is cropped to the area covered by all frames.

mod estimate;
mod transform;
mod warp;

pub use transform::Affine;
pub use warp::warp_rgb;

use estimate::AlignImage;

use crate::error::{Error, Result};
use crate::image::{Rect, RgbImage};
use crate::stack::{CancelToken, FrameSource, Progress, Stage};

/// Longer side of the downscaled images used for estimation.
const WORKING_SIZE: usize = 2048;
/// Neighbouring frames differing more than this in scale are treated as a failure.
const MAX_SCALE_STEP: f64 = 0.1;

/// Result of aligning a stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Alignment {
    /// Per frame, maps output (first-frame) coordinates to that frame's coordinates.
    pub transforms: Vec<Affine>,
    /// Region of the first frame covered by all frames; the output size.
    pub crop: Rect,
}

/// Estimates the alignment of all frames of `source`.
pub fn align_frames(source: &dyn FrameSource, progress: &dyn Fn(Progress), cancel: &CancelToken) -> Result<Alignment> {
    let n = source.len();
    let report = |frame| {
        progress(Progress {
            stage: Stage::Aligning,
            frame,
            total: n,
        })
    };
    report(0);
    let first = source.load(0)?;
    let (w, h) = (first.width(), first.height());
    let mut previous = AlignImage::new(&first, WORKING_SIZE);
    drop(first);

    let mut transforms = vec![Affine::IDENTITY];
    for i in 1..n {
        cancel.check()?;
        report(i);
        let frame = source.load(i)?;
        if (frame.width(), frame.height()) != (w, h) {
            return Err(Error::InvalidInput(format!("frame {} has a different size", i + 1)));
        }
        let current = AlignImage::new(&frame, WORKING_SIZE);
        let step = estimate::estimate(&previous, &current)
            .map_err(|e| Error::Alignment(format!("frames {} and {}: {e}", i, i + 1)))?;
        if (step.scale_factor() - 1.0).abs() > MAX_SCALE_STEP {
            return Err(Error::Alignment(format!("frames {} and {} differ too much", i, i + 1)));
        }
        // Output → frame i−1 → frame i.
        transforms.push(step.then_after(transforms.last().unwrap()));
        previous = current;
    }
    let crop =
        common_area(&transforms, w, h).ok_or_else(|| Error::Alignment("the frames have no common area".into()))?;
    Ok(Alignment { transforms, crop })
}

/// Largest axis-aligned rectangle (conservatively) inside every frame, in output
/// coordinates, with a one-pixel margin for interpolation.
fn common_area(transforms: &[Affine], w: usize, h: usize) -> Option<Rect> {
    let (mut left, mut top, mut right, mut bottom) = (0.0f64, 0.0f64, (w - 1) as f64, (h - 1) as f64);
    let (wf, hf) = ((w - 1) as f64, (h - 1) as f64);
    for t in transforms {
        let inv = t.inverse()?;
        let [tl, tr, bl, br] = [(0.0, 0.0), (wf, 0.0), (0.0, hf), (wf, hf)].map(|(x, y)| inv.apply(x, y));
        // Each frame is a convex quad; for small rotations the bounds below lie inside it.
        left = left.max(tl.0).max(bl.0);
        right = right.min(tr.0).min(br.0);
        top = top.max(tl.1).max(tr.1);
        bottom = bottom.min(bl.1).min(br.1);
    }
    let (x0, y0) = (left.ceil() as isize + 1, top.ceil() as isize + 1);
    let (x1, y1) = (right.floor() as isize - 1, bottom.floor() as isize - 1);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0 as usize, y0 as usize, (x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize))
}

/// Frames of `source` warped into the output geometry and cropped to the common area.
pub struct AlignedFrames<'a> {
    pub source: &'a dyn FrameSource,
    pub alignment: &'a Alignment,
}

impl FrameSource for AlignedFrames<'_> {
    fn len(&self) -> usize {
        self.source.len()
    }

    fn load(&self, index: usize) -> Result<RgbImage> {
        let frame = self.source.load(index)?;
        Ok(warp_rgb(&frame, &self.alignment.transforms[index], self.alignment.crop))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack::tests::MemFrames;
    use estimate::tests::render;

    #[test]
    fn common_area_of_identity_is_inset_by_margin() {
        assert_eq!(common_area(&[Affine::IDENTITY], 100, 50), Some(Rect::new(1, 1, 98, 48)));
    }

    #[test]
    fn common_area_shrinks_for_shift_and_zoom() {
        // Frame 2 sees output point p at p + (10, -4): its left/bottom part is missing.
        let shifted = Affine::translation(10.0, -4.0);
        let r = common_area(&[Affine::IDENTITY, shifted], 100, 50).unwrap();
        assert_eq!(r, Rect::new(1, 5, 88, 44));
        // A frame magnified 2× about the centre only covers the middle half.
        let zoom = Affine::similarity((49.5, 24.5), 2.0, 0.0, (0.0, 0.0));
        let r = common_area(&[Affine::IDENTITY, zoom], 100, 50).unwrap();
        assert!(
            r.x >= 25 && r.x + r.width <= 75 && r.y >= 12 && r.y + r.height <= 38,
            "{r:?}"
        );
        assert!(common_area(&[Affine::IDENTITY, Affine::translation(200.0, 0.0)], 100, 50).is_none());
    }

    /// Frames of a scene with gradually increasing magnification (focus breathing) and
    /// a slight drift, like a real focus stack.
    fn breathing_stack(n: usize) -> (Vec<Affine>, MemFrames) {
        let (w, h) = (400, 300);
        let truths: Vec<Affine> = (0..n)
            .map(|i| {
                Affine::similarity(
                    (200.0, 150.0),
                    1.0 + 0.008 * i as f64,
                    0.0005 * i as f64,
                    (0.7 * i as f64, -0.4 * i as f64),
                )
            })
            .collect();
        // Frame i shows scene point p at truth_i(p); the output is frame 0's geometry.
        let frames = truths.iter().map(|t| render(w, h, &t.inverse().unwrap())).collect();
        (truths, MemFrames(frames))
    }

    #[test]
    fn aligns_a_breathing_stack() {
        let (truths, frames) = breathing_stack(5);
        let alignment = align_frames(&frames, &|_| {}, &CancelToken::default()).unwrap();
        for (est, truth) in alignment.transforms.iter().zip(&truths) {
            for (x, y) in [(50.0, 50.0), (350.0, 250.0)] {
                let (a, b) = (est.apply(x, y), truth.apply(x, y));
                assert!((a.0 - b.0).hypot(a.1 - b.1) < 0.1, "{a:?} vs {b:?}");
            }
        }
        // Aligned frames all show the same content.
        let aligned = AlignedFrames {
            source: &frames,
            alignment: &alignment,
        };
        let (first, last) = (aligned.load(0).unwrap(), aligned.load(4).unwrap());
        assert_eq!(
            (first.width(), first.height()),
            (alignment.crop.width, alignment.crop.height)
        );
        let err = first
            .data()
            .iter()
            .zip(last.data())
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / first.data().len() as f32;
        assert!(err < 0.002, "mean difference after alignment {err}");
    }

    #[test]
    fn stacking_with_alignment_outputs_the_common_area() {
        use crate::stack::{Method, StackSettings, stack};
        let (_, frames) = breathing_stack(4);
        let settings = StackSettings {
            method: Method::DEFAULT_PYRAMID,
            align: true,
        };
        let result = stack(&frames, &settings, &|_| {}, &CancelToken::default()).unwrap();
        let crop = result.alignment.unwrap().crop;
        assert_eq!((result.image.width(), result.image.height()), (crop.width, crop.height));
        // All frames show the same (smooth) scene, so the stack equals the aligned first frame.
        let first = frames.0[0].crop(crop);
        let err = first
            .data()
            .iter()
            .zip(result.image.data())
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / first.data().len() as f32;
        assert!(err < 0.002, "{err}");
    }

    #[test]
    fn alignment_reports_progress_and_can_be_cancelled() {
        let (_, frames) = breathing_stack(3);
        let cancel = CancelToken::default();
        let err = align_frames(
            &frames,
            &|p| {
                if p.frame == 1 {
                    cancel.cancel()
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled));
    }
}
