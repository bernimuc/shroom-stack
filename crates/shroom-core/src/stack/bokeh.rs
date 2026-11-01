//! Soft bokeh: keeps backgrounds that are out of focus in every frame as soft as in the
//! most blurred frame, instead of taking them from the least blurred one.
//!
//! Frames are expected in shooting order of focus bracketing, near to far: the background
//! is what is sharpest in the last frames. (Out-of-focus foreground is left alone: taken
//! from a far-focused frame it would smear over the subject behind it.)
//!
//! Analysis runs on a coarse grid (1/16 of the image size), during the stacking method's
//! analysis pass:
//! 1. Detail: the best frame's fine-scale energy relative to the most blurred frame's,
//!    which has the same noise. About 1 where nothing is ever in focus.
//! 2. Blur over the stack, at the scale where a place changes most: background beyond
//!    the stack's focus range gets sharper up to the last frame; featureless or dark parts
//!    of the subject peak inside the stack (from their edges).
//! 3. Places with neither detail nor change can't be judged and are filled in from their
//!    surroundings, so a background becomes one area. Only large areas are kept, with a
//!    feathered edge and some distance from the subject.
//! 4. Bokeh frame: per area, the frame with the least energy (most blurred), or the first
//!    frame (nearest focus).
//!
//! The result is a mask and a (fractional) frame index per coarse pixel, expanded to full
//! resolution for compositing.

use rayon::prelude::*;

use crate::compute::{Backend, gaussian_blur};
use crate::filter::{self, expand};
use crate::image::Plane;

/// Which frame the soft background is taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BokehFrame {
    /// Per area, the frame where the background has the least detail.
    MostBlurred,
    /// The first frame, focused nearest.
    NearestFocus,
}

/// Soft bokeh settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftBokeh {
    pub frame: BokehFrame,
    /// How much detail an area may have and still count as bokeh, `1..=10`; higher values
    /// detect more.
    pub threshold: u32,
}

impl SoftBokeh {
    pub const DEFAULT: SoftBokeh = SoftBokeh { frame: BokehFrame::MostBlurred, threshold: 5 };
}

/// Halvings between full resolution and the analysis grid.
const LEVELS: usize = 4;
/// Scale of the detail measure (as the depth map's focus measure).
const DETAIL_SIGMA: f32 = 1.5;
/// Pyramid levels at which blur is measured (detail of about 8, 16 and 32 pixels);
/// strongly blurred backgrounds only change visibly at the coarser ones.
const BLUR_LEVELS: [usize; 3] = [2, 3, 4];
/// Areas brighter than this (relative to white) are ignored for the detail measure:
/// clipping differs between frames.
const CLIP_LEVEL: f32 = 0.8;
/// Background: the sharpest frame is at least this far into the stack (0 = first frame,
/// 1 = last).
const MIN_FAR: f32 = 0.8;
/// Energy ratio between the sharpest and the most blurred frame from which an area counts
/// as changing over the stack.
const MIN_CONTRAST: f32 = 1.3;
/// Size of the areas considered, as a fraction of the image's longer side (Gaussian σ).
const AREA_SIGMA: f32 = 1.0 / 50.0;

/// How the energy at one scale and place changes over the stack.
struct Curve {
    sharpest: usize,
    blurriest: usize,
    /// Ratio of the highest to the lowest energy.
    contrast: f32,
}

impl Curve {
    fn new(energies: impl Iterator<Item = f32>) -> Self {
        let (mut sharpest, mut blurriest, mut max, mut min) = (0, 0, f32::MIN, f32::MAX);
        for (k, e) in energies.enumerate() {
            if e > max {
                (sharpest, max) = (k, e);
            }
            if e < min {
                (blurriest, min) = (k, e);
            }
        }
        Self { sharpest, blurriest, contrast: max / min.max(f32::MIN_POSITIVE) }
    }
}

/// Coarse per-frame measures collected during the analysis pass.
pub(super) struct Analysis {
    /// Per frame: fine-scale detail energy, averaged over coarse cells.
    detail: Vec<Plane>,
    /// Per blur level and frame: energy at that scale (how blurred the frame is), averaged
    /// over coarse cells.
    blur: [Vec<Plane>; BLUR_LEVELS.len()],
    /// Coarse brightness of the first frame.
    brightness: Option<Plane>,
    /// Plane sizes from full resolution down to the coarse grid.
    sizes: Vec<(usize, usize)>,
}

/// Soft bokeh areas and the frame to take each from, on the coarse grid.
pub(super) struct BokehMap {
    /// 1 inside bokeh areas, 0 outside, feathered.
    pub mask: Plane,
    /// Fractional frame index to take bokeh areas from.
    pub depth: Plane,
    sizes: Vec<(usize, usize)>,
}

impl Analysis {
    pub fn new() -> Self {
        Self { detail: Vec::new(), blur: Default::default(), brightness: None, sizes: Vec::new() }
    }

    /// Measures one frame; frames must be added in stack order.
    pub fn add<B: Backend>(&mut self, b: &B, frame: &B::Rgb) {
        let lum = b.luminance(frame);
        let reduce_n = |p: B::Plane, n: usize| (0..n).fold(p, |p, _| b.reduce(&p));
        if self.sizes.is_empty() {
            let (w, h) = b.rgb_size(frame);
            self.sizes = (0..=LEVELS).map(|k| (w.div_ceil(1 << k), h.div_ceil(1 << k))).collect();
        }
        let fine = b.laplacian(&gaussian_blur(b, &lum, DETAIL_SIGMA), true);
        self.detail.push(b.download_plane(&reduce_n(fine, LEVELS)));
        let mut small = lum;
        let mut level = 0;
        for (k, &target) in BLUR_LEVELS.iter().enumerate() {
            small = reduce_n(small, target - level);
            level = target;
            let energy = b.laplacian(&small, true);
            self.blur[k].push(b.download_plane(&reduce_n(energy, LEVELS - level)));
        }
        if self.brightness.is_none() {
            self.brightness = Some(b.download_plane(&reduce_n(small, LEVELS - level)));
        }
    }

    /// Detects bokeh areas; `None` if there are none.
    pub fn finish(self, settings: &SoftBokeh) -> Option<BokehMap> {
        let n = self.detail.len();
        let brightness = self.brightness?;
        let (w, h) = (brightness.width(), brightness.height());
        let sigma = AREA_SIGMA * self.sizes[0].0.max(self.sizes[0].1) as f32 / (1 << LEVELS) as f32;

        // 1. Detail of the best frame relative to the most blurred one, which has the same
        //    noise but no sharp detail: about 1 where nothing is ever in focus.
        let detail: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                if brightness.data()[i] > CLIP_LEVEL {
                    return f32::INFINITY;
                }
                let (min, max) =
                    self.detail.iter().map(|p| p.data()[i]).fold((f32::MAX, 0.0f32), |(a, b), v| (a.min(v), b.max(v)));
                (max / min.max(f32::MIN_POSITIVE)).sqrt()
            })
            .collect();
        let limit = 1.2 + 0.04 * settings.threshold as f32;

        // 2. Out of focus in every frame because the focus plane lies beyond the stack:
        //    blur decreases up to the last frame. Featureless or dark parts of the subject
        //    peak inside the stack (from their edges). Each place is judged at the scale
        //    where it changes most.
        let blur: Vec<Vec<Plane>> = self
            .blur
            .par_iter()
            .map(|level| level.iter().map(|p| filter::gaussian_blur(p, sigma / 2.0)).collect())
            .collect();
        let curves: Vec<Curve> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                blur.iter()
                    .map(|level| Curve::new(level.iter().map(|p| p.data()[i])))
                    .max_by(|a, b| a.contrast.total_cmp(&b.contrast))
                    .expect("blur levels")
            })
            .collect();
        let last = (n - 1) as f32;

        // 3. Bokeh where there is no detail and the sharpest frame is the last. Places that
        //    neither have detail nor change over the stack can't be judged: they are filled
        //    in from their surroundings, so large backgrounds become one area. Then only
        //    large areas are kept, feathered.
        let (mut value, mut weight) = (Plane::new(w, h), Plane::new(w, h));
        for (i, (d, c)) in detail.iter().zip(&curves).enumerate() {
            let has_detail = smoothstep(limit, 1.25 * limit, *d);
            let changes = smoothstep(1.0, MIN_CONTRAST, c.contrast);
            let far = smoothstep(MIN_FAR, 1.0, c.sharpest as f32 / last);
            let wt = has_detail.max(changes);
            value.data_mut()[i] = (1.0 - has_detail) * far * wt;
            weight.data_mut()[i] = wt;
        }
        let filled = super::depthmap::push_pull(&value, &weight).unwrap_or(value);
        let mut mask = filter::gaussian_blur(&filled, sigma);
        mask.data_mut().par_iter_mut().for_each(|m| *m = smoothstep(0.5, 0.8, *m));
        let mut mask = filter::gaussian_blur(&mask, sigma);
        mask.data_mut().par_iter_mut().for_each(|m| *m = smoothstep(0.1, 0.9, *m));
        if mask.data().iter().all(|&m| m < 1e-3) {
            return None;
        }

        // 4. Bokeh frame, with smooth transitions between areas preferring different frames.
        let index = curves.iter().map(|c| match settings.frame {
            BokehFrame::MostBlurred => c.blurriest as f32,
            BokehFrame::NearestFocus => 0.0,
        });
        let weighted = Plane::from_vec(w, h, index.zip(mask.data()).map(|(d, m)| d * m).collect());
        let (num, den) = (filter::gaussian_blur(&weighted, sigma), filter::gaussian_blur(&mask, sigma));
        let fallback = weighted.data().iter().sum::<f32>() / mask.data().iter().sum::<f32>();
        let data = num
            .data()
            .iter()
            .zip(den.data())
            .map(|(a, b)| if *b > 1e-4 { (a / b).clamp(0.0, last) } else { fallback })
            .collect();
        let depth = Plane::from_vec(w, h, data);
        Some(BokehMap { mask, depth, sizes: self.sizes })
    }
}

impl BokehMap {
    /// Full-resolution version of a coarse plane.
    pub fn expand(&self, p: &Plane) -> Plane {
        self.sizes.iter().rev().skip(1).fold(p.clone(), |p, &(w, h)| expand(&p, w, h))
    }

    /// Frames used by bokeh areas.
    pub fn used_frames(&self, n: usize) -> Vec<bool> {
        let mut used = vec![false; n];
        for (d, m) in self.depth.data().iter().zip(self.mask.data()) {
            if *m > 0.0 {
                used[d.floor() as usize] = true;
                used[d.ceil() as usize] = true;
            }
        }
        used
    }
}

/// Mixes `b` into `a` by `mask` (all at full resolution).
pub(super) fn mix(a: &mut [f32], b: &[f32], mask: &[f32], channels: usize) {
    a.par_chunks_mut(channels).zip(b.par_chunks(channels)).zip(mask.par_iter()).for_each(|((a, b), m)| {
        for (a, b) in a.iter_mut().zip(b) {
            *a += m * (b - *a);
        }
    });
}

/// 0 at `e0`, 1 at `e1` (either order), smooth in between.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::tests::noise;
    use crate::filter::{merge_channels, split_channels};
    use crate::image::RgbImage;
    use crate::stack::tests::MemFrames;
    use crate::stack::{CancelToken, Method, StackSettings, stack};

    const N: usize = 7;

    /// Left half: subject, sharp in the middle frame. Right half: background, blurred in
    /// every frame, least in the last and most in the first. Every frame has its own noise.
    fn scene(w: usize, h: usize) -> (MemFrames, Vec<RgbImage>) {
        let texture = merge_channels(&[1, 2, 3].map(|s| {
            let mut p = noise(w, h, s);
            p.data_mut().iter_mut().for_each(|v| *v = 0.2 + 0.6 * *v);
            p
        }));
        let blurred = |sigma: f32| {
            if sigma == 0.0 {
                texture.clone()
            } else {
                merge_channels(&split_channels(&texture).map(|p| crate::filter::gaussian_blur(&p, sigma)))
            }
        };
        let mut frames = Vec::new();
        let mut backgrounds = Vec::new();
        for k in 0..N {
            let subject = blurred(2.0 * (k as f32 - (N / 2) as f32).abs());
            let background = blurred(8.0 + 2.0 * (N - 1 - k) as f32);
            let grain = [10, 11, 12].map(|s| noise(w, h, s + 7 * k as u32));
            let mut f = RgbImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let src = if x < w / 2 { &subject } else { &background };
                    let p = src.pixel(x, y);
                    f.set_pixel(x, y, std::array::from_fn(|c| p[c] + 0.01 * (grain[c].get(x, y) - 0.5)));
                }
            }
            frames.push(f);
            backgrounds.push(background);
        }
        (MemFrames(frames), backgrounds)
    }

    fn run(method: Method, frame: BokehFrame) -> crate::stack::StackResult {
        run_on(method, frame, false)
    }

    fn run_on(method: Method, frame: BokehFrame, use_gpu: bool) -> crate::stack::StackResult {
        let (w, h) = (768, 384);
        let (frames, _) = scene(w, h);
        let settings = StackSettings {
            align: false,
            use_gpu,
            bokeh: Some(SoftBokeh { frame, ..SoftBokeh::DEFAULT }),
            ..StackSettings::new(method)
        };
        stack(&frames, &settings, &|_| {}, &CancelToken::default()).unwrap()
    }

    fn mean_in(p: &Plane, x0: f32, x1: f32) -> f32 {
        let (w, h) = (p.width(), p.height());
        let (x0, x1) = ((x0 * w as f32) as usize, (x1 * w as f32) as usize);
        let values: Vec<f32> =
            (h / 4..3 * h / 4).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| p.get(x, y)).collect();
        values.iter().sum::<f32>() / values.len() as f32
    }

    #[test]
    fn detects_background_but_not_subject() {
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            let result = run(method, BokehFrame::MostBlurred);
            let mask = result.bokeh_mask.expect("bokeh found");
            assert!(mean_in(&mask, 0.05, 0.4) < 0.05, "{method:?}: subject {}", mean_in(&mask, 0.05, 0.4));
            assert!(mean_in(&mask, 0.65, 0.95) > 0.9, "{method:?}: background {}", mean_in(&mask, 0.65, 0.95));
            // The background comes from the blurred end of the stack (frames 0…3 differ only
            // by noise there).
            let depth = mean_in(&result.depth_map, 0.65, 0.95);
            assert!(depth < 2.5, "{method:?}: depth {depth}");
        }
    }

    #[test]
    fn background_is_taken_from_the_bokeh_frame() {
        let (w, h) = (768, 384);
        let (_, backgrounds) = scene(w, h);
        for (frame, expected) in [(BokehFrame::MostBlurred, 0), (BokehFrame::NearestFocus, 0)] {
            let image = run(Method::DEFAULT_DEPTH_MAP, frame).image;
            let err = crate::stack::tests::band_error(&image, &backgrounds[expected], 3 * w / 4, w - 32, 32);
            let err_plain = crate::stack::tests::band_error(&image, &backgrounds[N - 1], 3 * w / 4, w - 32, 32);
            assert!(err < err_plain / 1.5, "{frame:?}: {err} vs {err_plain}");
        }
    }

    #[test]
    fn gpu_matches_cpu() {
        if crate::gpu::gpu().is_none() {
            return;
        }
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            let (cpu, gpu) =
                (run_on(method, BokehFrame::MostBlurred, false), run_on(method, BokehFrame::MostBlurred, true));
            let max_diff = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
            let (cm, gm) = (cpu.bokeh_mask.unwrap(), gpu.bokeh_mask.unwrap());
            assert!(max_diff(cm.data(), gm.data()) < 0.01, "{method:?} mask");
            assert!(max_diff(cpu.image.data(), gpu.image.data()) < 1e-3, "{method:?} image");
        }
    }

    #[test]
    fn flat_scene_has_no_bokeh() {
        let img = merge_channels(&[noise(256, 256, 4), noise(256, 256, 5), noise(256, 256, 6)]);
        let frames = MemFrames(vec![img.clone(), img.clone(), img]);
        let settings = StackSettings {
            align: false,
            use_gpu: false,
            bokeh: Some(SoftBokeh::DEFAULT),
            ..StackSettings::new(Method::DEFAULT_DEPTH_MAP)
        };
        let result = stack(&frames, &settings, &|_| {}, &CancelToken::default()).unwrap();
        assert!(result.bokeh_mask.is_none());
    }

    #[test]
    fn curve_finds_extremes() {
        let c = Curve::new([2.0, 4.0, 1.0, 3.0].into_iter());
        assert_eq!((c.sharpest, c.blurriest, c.contrast), (1, 2, 4.0));
    }
}
