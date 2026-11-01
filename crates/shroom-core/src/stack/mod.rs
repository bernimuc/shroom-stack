//! Focus stacking: combines frames with different focal planes into one sharp image.
//!
//! Both methods stream over the frames, so only a few full-size images are in memory at
//! any time regardless of the stack size.

mod depthmap;
mod pyramid;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::align::{AlignedFrames, Alignment, align_frames};
use crate::error::{Error, Result};
use crate::image::{Plane, RgbImage};

/// Stacking method and its parameters (named after Helicon Focus' equivalents).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Picks each pixel from the frame where it is sharpest.
    DepthMap {
        /// Radius of the focus-measure window in pixels.
        radius: u32,
        /// Strength of the depth map smoothing.
        smoothing: u32,
    },
    /// Laplacian pyramid fusion: picks the strongest detail per pyramid level.
    Pyramid {
        /// Size of the window comparing detail strength.
        smoothing: u32,
    },
}

impl Method {
    pub const DEFAULT_DEPTH_MAP: Method = Method::DepthMap {
        radius: 8,
        smoothing: 4,
    };
    pub const DEFAULT_PYRAMID: Method = Method::Pyramid { smoothing: 4 };
}

/// Supplies the frames of a stack, in focus order, as linear RGB of identical size.
/// Frames may be requested more than once.
pub trait FrameSource: Sync {
    fn len(&self) -> usize;
    fn load(&self, index: usize) -> Result<RgbImage>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Frames read from DNG files.
pub struct FileFrames {
    pub paths: Vec<PathBuf>,
}

impl FrameSource for FileFrames {
    fn len(&self) -> usize {
        self.paths.len()
    }

    fn load(&self, index: usize) -> Result<RgbImage> {
        Ok(crate::io::load_linear(&self.paths[index])?.image)
    }
}

/// Which part of the processing is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Estimating frame alignment.
    Aligning,
    /// Measuring focus / decomposing frames.
    Analysing,
    /// Assembling the output from the frames.
    Compositing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    /// Index of the frame being processed.
    pub frame: usize,
    pub total: usize,
}

/// Cooperative cancellation, checked between frames.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Everything that determines how a stack is processed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackSettings {
    pub method: Method,
    /// Align frames before stacking (recommended; compensates focus breathing).
    pub align: bool,
}

#[derive(Clone, Debug)]
pub struct StackResult {
    pub image: RgbImage,
    /// Per pixel, the (fractional) index of the frame the output was taken from.
    pub depth_map: Plane,
    /// Frame alignment, if enabled. The image covers `alignment.crop`.
    pub alignment: Option<Alignment>,
}

/// Stacks all frames of `source`.
pub fn stack(
    source: &dyn FrameSource,
    settings: &StackSettings,
    progress: &dyn Fn(Progress),
    cancel: &CancelToken,
) -> Result<StackResult> {
    if source.len() < 2 {
        return Err(Error::InvalidInput("at least two frames are needed".into()));
    }
    let alignment = if settings.align {
        Some(align_frames(source, progress, cancel)?)
    } else {
        None
    };
    let aligned;
    let frames: &dyn FrameSource = match &alignment {
        Some(alignment) => {
            aligned = AlignedFrames { source, alignment };
            &aligned
        }
        None => source,
    };
    let ctx = Context {
        source: frames,
        progress,
        cancel,
    };
    let (image, depth_map) = match settings.method {
        Method::DepthMap { radius, smoothing } => depthmap::stack(&ctx, radius as usize, smoothing as f32)?,
        Method::Pyramid { smoothing } => pyramid::stack(&ctx, smoothing as usize)?,
    };
    Ok(StackResult {
        image,
        depth_map,
        alignment,
    })
}

struct Context<'a> {
    source: &'a dyn FrameSource,
    progress: &'a dyn Fn(Progress),
    cancel: &'a CancelToken,
}

impl Context<'_> {
    /// Loads a frame after reporting progress and checking for cancellation; verifies the
    /// frame size against `expected`.
    fn load(&self, stage: Stage, index: usize, expected: Option<(usize, usize)>) -> Result<RgbImage> {
        self.cancel.check()?;
        (self.progress)(Progress {
            stage,
            frame: index,
            total: self.source.len(),
        });
        let img = self.source.load(index)?;
        if let Some((w, h)) = expected
            && (img.width(), img.height()) != (w, h)
        {
            return Err(Error::InvalidInput(format!(
                "frame {} is {}×{}, expected {w}×{h}",
                index + 1,
                img.width(),
                img.height()
            )));
        }
        Ok(img)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::filter::{gaussian_blur, merge_channels, split_channels, tests::noise};

    /// In-memory frames.
    pub(crate) struct MemFrames(pub Vec<RgbImage>);

    impl FrameSource for MemFrames {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn load(&self, index: usize) -> Result<RgbImage> {
            Ok(self.0[index].clone())
        }
    }

    /// A textured scene and `n` frames, where frame `i` is sharp only in the i-th vertical
    /// band and blurred elsewhere.
    pub(crate) fn banded_stack(w: usize, h: usize, n: usize) -> (RgbImage, MemFrames) {
        let sharp = merge_channels(&[noise(w, h, 1), noise(w, h, 2), noise(w, h, 3)]);
        let blurred = merge_channels(&split_channels(&sharp).map(|p| gaussian_blur(&p, 3.0)));
        let frames = (0..n)
            .map(|i| {
                let mut f = blurred.clone();
                for y in 0..h {
                    for x in i * w / n..(i + 1) * w / n {
                        f.set_pixel(x, y, sharp.pixel(x, y));
                    }
                }
                f
            })
            .collect();
        (sharp, MemFrames(frames))
    }

    /// Mean absolute error between two images inside columns `x0..x1`, ignoring a margin.
    pub(crate) fn band_error(a: &RgbImage, b: &RgbImage, x0: usize, x1: usize, margin: usize) -> f32 {
        let mut sum = 0.0;
        let mut n = 0;
        for y in margin..a.height() - margin {
            for x in x0 + margin..x1 - margin {
                let (p, q) = (a.pixel(x, y), b.pixel(x, y));
                sum += (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>();
                n += 3;
            }
        }
        sum / n as f32
    }

    fn unaligned(method: Method) -> StackSettings {
        StackSettings { method, align: false }
    }

    #[test]
    fn rejects_single_frame() {
        let frames = MemFrames(vec![RgbImage::new(8, 8)]);
        let err = stack(
            &frames,
            &unaligned(Method::DEFAULT_PYRAMID),
            &|_| {},
            &CancelToken::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn rejects_mismatched_sizes() {
        let frames = MemFrames(vec![RgbImage::new(16, 16), RgbImage::new(16, 15)]);
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            let err = stack(&frames, &unaligned(method), &|_| {}, &CancelToken::default()).unwrap_err();
            assert!(matches!(err, Error::InvalidInput(_)), "{method:?}: {err}");
        }
    }

    #[test]
    fn cancel_stops_processing() {
        let (_, frames) = banded_stack(32, 32, 3);
        let cancel = CancelToken::default();
        let progress = |p: Progress| {
            if p.frame == 1 {
                cancel.cancel();
            }
        };
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            cancel.0.store(false, Ordering::Relaxed);
            let err = stack(&frames, &unaligned(method), &progress, &cancel).unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{method:?}");
        }
    }

    #[test]
    fn reports_progress_for_every_frame() {
        let (_, frames) = banded_stack(32, 32, 3);
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            let seen = std::sync::Mutex::new(Vec::new());
            stack(
                &frames,
                &unaligned(method),
                &|p| seen.lock().unwrap().push(p),
                &CancelToken::default(),
            )
            .unwrap();
            let seen = seen.into_inner().unwrap();
            for i in 0..3 {
                assert!(
                    seen.iter()
                        .any(|p| p.stage == Stage::Analysing && p.frame == i && p.total == 3)
                );
            }
        }
    }

    #[test]
    fn both_methods_recover_the_sharp_scene() {
        let (w, h, n) = (192, 48, 3);
        let (sharp, frames) = banded_stack(w, h, n);
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            let result = stack(&frames, &unaligned(method), &|_| {}, &CancelToken::default()).unwrap();
            for i in 0..n {
                let (x0, x1) = (i * w / n, (i + 1) * w / n);
                let stacked = band_error(&result.image, &sharp, x0, x1, 12);
                let single = band_error(&frames.0[(i + 1) % n], &sharp, x0, x1, 12);
                assert!(
                    stacked < 0.2 * single,
                    "{method:?} band {i}: stacked {stacked}, unfocused {single}"
                );
                // Depth map points to the sharp frame in the band centre.
                let d = result.depth_map.get((x0 + x1) / 2, h / 2);
                assert!((d - i as f32).abs() < 0.5, "{method:?} band {i}: depth {d}");
            }
        }
    }
}
