//! Focus stacking: combines frames with different focal planes into one sharp image.
//!
//! Both methods stream over the frames, so only a few full-size images are in memory at
//! any time regardless of the stack size.

mod depthmap;
mod pyramid;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::align::{Alignment, align_with};
use crate::compute::{Backend, Cpu};
use crate::error::{Error, Result};
use crate::image::{Plane, RgbImage};
use crate::raw::CfaPattern;

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
    pub const DEFAULT_DEPTH_MAP: Method = Method::DepthMap { radius: 8, smoothing: 4 };
    pub const DEFAULT_PYRAMID: Method = Method::Pyramid { smoothing: 4 };
}

/// A frame as delivered by a [`FrameSource`].
pub enum SourceFrame {
    /// Linear RGB, normalised to `0..=1`.
    Rgb(RgbImage),
    /// Normalised Bayer mosaic; demosaiced by the compute backend.
    Mosaic(Plane, CfaPattern),
}

/// Supplies the frames of a stack, in focus order, all of identical size.
/// Frames may be requested more than once.
pub trait FrameSource: Sync {
    fn len(&self) -> usize;
    fn load(&self, index: usize) -> Result<RgbImage>;

    /// The frame in its most compact form; by default [`Self::load`].
    fn load_frame(&self, index: usize) -> Result<SourceFrame> {
        self.load(index).map(SourceFrame::Rgb)
    }

    /// Frame size if cheaply known in advance.
    fn frame_size(&self) -> Option<(usize, usize)> {
        None
    }

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

    fn load_frame(&self, index: usize) -> Result<SourceFrame> {
        crate::io::load_for_stacking(&self.paths[index])
    }

    fn frame_size(&self) -> Option<(usize, usize)> {
        let info = crate::io::read_frame_info(self.paths.first()?).ok()?;
        Some((info.width, info.height))
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
        if self.is_cancelled() { Err(Error::Cancelled) } else { Ok(()) }
    }
}

/// Everything that determines how a stack is processed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackSettings {
    pub method: Method,
    /// Align frames before stacking (recommended; compensates focus breathing).
    pub align: bool,
    /// Use the GPU if one is available (falls back to the CPU otherwise).
    pub use_gpu: bool,
}

impl StackSettings {
    pub fn new(method: Method) -> Self {
        Self { method, align: true, use_gpu: true }
    }
}

#[derive(Clone, Debug)]
pub struct StackResult {
    pub image: RgbImage,
    /// Per pixel, the (fractional) index of the frame the output was taken from.
    pub depth_map: Plane,
    /// Frame alignment, if enabled. The image covers `alignment.crop`.
    pub alignment: Option<Alignment>,
    /// Compute backend that was used, e.g. "CPU" or "GPU: Apple M5 (Metal)".
    pub backend: String,
}

/// Low-resolution view of the processing state, for live display.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub stage: Stage,
    pub frame_index: usize,
    /// The frame just processed (aligned, once alignment has been estimated), downscaled.
    pub frame: RgbImage,
    /// Depth map (depth map method) or detail source map (pyramid) so far, same scale.
    pub map: Option<Plane>,
    /// Full-resolution size of what the snapshot shows.
    pub full_size: (usize, usize),
}

/// Longer side of snapshot images.
const SNAPSHOT_SIZE: usize = 1024;
/// Snapshots are taken at most this often (they cost GPU time and a download).
const SNAPSHOT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Stacks all frames of `source`.
pub fn stack(
    source: &dyn FrameSource,
    settings: &StackSettings,
    progress: &dyn Fn(Progress),
    cancel: &CancelToken,
) -> Result<StackResult> {
    stack_with_snapshots(source, settings, progress, None, cancel)
}

/// Like [`stack`], additionally passing periodic [`Snapshot`]s to `snapshot`.
pub fn stack_with_snapshots(
    source: &dyn FrameSource,
    settings: &StackSettings,
    progress: &dyn Fn(Progress),
    snapshot: Option<&dyn Fn(Snapshot)>,
    cancel: &CancelToken,
) -> Result<StackResult> {
    if source.len() < 2 {
        return Err(Error::InvalidInput("at least two frames are needed".into()));
    }
    if settings.use_gpu
        && let Some(gpu) = crate::gpu::gpu()
        && source.frame_size().is_none_or(|(w, h)| gpu.fits(w, h))
    {
        return stack_with(gpu, source, settings, progress, snapshot, cancel);
    }
    stack_with(&Cpu, source, settings, progress, snapshot, cancel)
}

fn stack_with<B: Backend>(
    backend: &B,
    source: &dyn FrameSource,
    settings: &StackSettings,
    progress: &dyn Fn(Progress),
    snapshot: Option<&dyn Fn(Snapshot)>,
    cancel: &CancelToken,
) -> Result<StackResult> {
    let run = || {
        let alignment =
            if settings.align { Some(align_with(backend, source, progress, snapshot, cancel)?) } else { None };
        let ctx = Context::new(backend, source, alignment.as_ref(), progress, snapshot, cancel);
        let (image, depth_map) = match settings.method {
            Method::DepthMap { radius, smoothing } => depthmap::stack(&ctx, radius as usize, smoothing as f32)?,
            Method::Pyramid { smoothing } => pyramid::stack(&ctx, smoothing as usize)?,
        };
        Ok(StackResult { image, depth_map, alignment, backend: backend.name() })
    };
    let result = run();
    backend.trim();
    result
}

/// Brings a source frame into backend memory as linear RGB.
pub(crate) fn prepare<B: Backend>(backend: &B, frame: SourceFrame) -> B::Rgb {
    match frame {
        SourceFrame::Rgb(img) => backend.upload_rgb(&img),
        SourceFrame::Mosaic(mosaic, pattern) => backend.demosaic(&backend.upload_plane(&mosaic), pattern),
    }
}

/// Number of frames decoded ahead. More doesn't help: decoding already uses all cores,
/// and with the GPU the per-frame computation is the longer part.
const PREFETCH: usize = 1;

pub(crate) struct Context<'a, B: Backend> {
    pub backend: &'a B,
    pub source: &'a dyn FrameSource,
    pub alignment: Option<&'a Alignment>,
    pub progress: &'a dyn Fn(Progress),
    pub snapshot: Option<&'a dyn Fn(Snapshot)>,
    pub cancel: &'a CancelToken,
    last_snapshot: std::cell::Cell<Option<std::time::Instant>>,
}

impl<'a, B: Backend> Context<'a, B> {
    pub fn new(
        backend: &'a B,
        source: &'a dyn FrameSource,
        alignment: Option<&'a Alignment>,
        progress: &'a dyn Fn(Progress),
        snapshot: Option<&'a dyn Fn(Snapshot)>,
        cancel: &'a CancelToken,
    ) -> Self {
        Self { backend, source, alignment, progress, snapshot, cancel, last_snapshot: Default::default() }
    }

    /// Passes a downscaled view of `frame` and `map` to the snapshot callback, if any and
    /// if the last snapshot is old enough (or `force`).
    pub(crate) fn snapshot(&self, stage: Stage, index: usize, frame: &B::Rgb, map: Option<&B::Plane>, force: bool) {
        let Some(callback) = self.snapshot else { return };
        let now = std::time::Instant::now();
        if !force && self.last_snapshot.get().is_some_and(|t| now - t < SNAPSHOT_INTERVAL) {
            return;
        }
        self.last_snapshot.set(Some(now));
        let b = self.backend;
        let full_size = b.rgb_size(frame);
        let factor = full_size.0.max(full_size.1).div_ceil(SNAPSHOT_SIZE).max(1);
        callback(Snapshot {
            stage,
            frame_index: index,
            frame: b.download_rgb(&b.downsample_rgb(frame, factor)),
            map: map.map(|m| b.download_plane(&b.downsample_box(m, factor))),
            full_size,
        });
    }

    /// Calls `f` with each frame of `indices` (aligned, if enabled), reporting progress
    /// and checking for cancellation. Up to [`PREFETCH`] upcoming frames are decoded on
    /// background threads while the current one is processed. All frames must have the
    /// same size.
    pub(crate) fn for_each_frame(
        &self,
        stage: Stage,
        indices: impl IntoIterator<Item = usize>,
        mut f: impl FnMut(usize, B::Rgb) -> Result<()>,
    ) -> Result<()> {
        let indices: Vec<usize> = indices.into_iter().collect();
        let source = self.source;
        std::thread::scope(|scope| {
            let fetch = |i: usize| scope.spawn(move || source.load_frame(i));
            let mut pending: std::collections::VecDeque<_> = indices.iter().take(PREFETCH).map(|&i| fetch(i)).collect();
            let mut size = None;
            for (k, &index) in indices.iter().enumerate() {
                let frame = pending.pop_front().expect("frame requested").join().expect("decoder thread panicked")?;
                self.cancel.check()?;
                if let Some(&i) = indices.get(k + PREFETCH) {
                    pending.push_back(fetch(i));
                }
                (self.progress)(Progress { stage, frame: index, total: self.source.len() });
                self.backend.finish();
                let mut img = prepare(self.backend, frame);
                if let Some(alignment) = self.alignment {
                    img = self.backend.warp(&img, &alignment.transforms[index], alignment.crop);
                }
                let (w, h) = self.backend.rgb_size(&img);
                let expected = *size.get_or_insert((w, h));
                if (w, h) != expected {
                    return Err(Error::InvalidInput(format!(
                        "frame {} is {w}×{h}, expected {}×{}",
                        index + 1,
                        expected.0,
                        expected.1
                    )));
                }
                f(index, img)?;
            }
            Ok(())
        })
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
        // Texture between 5 % and 95 % of white, like a real scene (not pure black).
        let texture = |seed| {
            let mut p = noise(w, h, seed);
            p.data_mut().iter_mut().for_each(|v| *v = 0.05 + 0.9 * *v);
            p
        };
        let sharp = merge_channels(&[texture(1), texture(2), texture(3)]);
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
        StackSettings { method, align: false, use_gpu: false }
    }

    #[test]
    fn rejects_single_frame() {
        let frames = MemFrames(vec![RgbImage::new(8, 8)]);
        let err = stack(&frames, &unaligned(Method::DEFAULT_PYRAMID), &|_| {}, &CancelToken::default()).unwrap_err();
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
            stack(&frames, &unaligned(method), &|p| seen.lock().unwrap().push(p), &CancelToken::default()).unwrap();
            let seen = seen.into_inner().unwrap();
            for i in 0..3 {
                assert!(seen.iter().any(|p| p.stage == Stage::Analysing && p.frame == i && p.total == 3));
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
                // The pyramid fuses log-encoded values (see `pyramid`), which mixes coarse
                // levels of sharp and blurred frames slightly inexactly on this extreme
                // test texture; on real stacks it is as sharp as the linear variant.
                let limit = if matches!(method, Method::Pyramid { .. }) { 0.35 } else { 0.2 };
                assert!(stacked < limit * single, "{method:?} band {i}: stacked {stacked}, unfocused {single}");
                // Depth map points to the sharp frame in the band centre.
                let d = result.depth_map.get((x0 + x1) / 2, h / 2);
                assert!((d - i as f32).abs() < 0.5, "{method:?} band {i}: depth {d}");
            }
        }
    }
}
