//! Compute backends: the stacking algorithms are written once against [`Backend`] and run
//! either on the CPU (reference implementation, always available) or on the GPU.

mod cpu;

pub(crate) use cpu::Cpu;

use crate::align::Affine;
use crate::image::{Plane, Rect, RgbImage};
use crate::raw::CfaPattern;

/// Image operations used by the stacking pipeline. Images live in backend memory
/// (`Self::Plane`, `Self::Rgb`) and are only transferred with `upload_*`/`download_*`.
pub(crate) trait Backend: Sync {
    type Plane: Send + Sync;
    type Rgb: Send + Sync;

    fn name(&self) -> String;
    /// Waits for queued work to finish and releases memory of dropped images. Called
    /// once per frame, after the next frame was decoded, so decoding overlaps with
    /// computation while memory use stays bounded.
    fn finish(&self) {}
    /// Releases cached memory after a stacking run.
    fn trim(&self) {}
    fn plane_size(&self, p: &Self::Plane) -> (usize, usize);
    fn rgb_size(&self, p: &Self::Rgb) -> (usize, usize);

    fn upload_plane(&self, p: &Plane) -> Self::Plane;
    fn upload_rgb(&self, img: &RgbImage) -> Self::Rgb;
    fn download_plane(&self, p: &Self::Plane) -> Plane;
    fn download_rgb(&self, p: &Self::Rgb) -> RgbImage;
    fn zeros_plane(&self, w: usize, h: usize) -> Self::Plane;
    fn zeros_rgb(&self, w: usize, h: usize) -> Self::Rgb;

    fn demosaic(&self, mosaic: &Self::Plane, pattern: CfaPattern) -> Self::Rgb;
    /// See [`crate::align::warp_rgb`].
    fn warp(&self, src: &Self::Rgb, t: &Affine, rect: Rect) -> Self::Rgb;
    fn luminance(&self, rgb: &Self::Rgb) -> Self::Plane;
    /// Separable convolution with an odd-length, centred kernel (border clamped).
    fn convolve(&self, p: &Self::Plane, kernel: &[f32]) -> Self::Plane;
    fn laplacian(&self, p: &Self::Plane, square: bool) -> Self::Plane;
    fn reduce(&self, p: &Self::Plane) -> Self::Plane;
    fn expand(&self, p: &Self::Plane, w: usize, h: usize) -> Self::Plane;
    fn downsample_box(&self, p: &Self::Plane, factor: usize) -> Self::Plane;
    fn downsample_rgb(&self, p: &Self::Rgb, factor: usize) -> Self::Rgb;

    fn sub(&self, a: &Self::Plane, b: &Self::Plane) -> Self::Plane;
    fn add_assign(&self, a: &mut Self::Plane, b: &Self::Plane);
    fn scale(&self, a: &mut Self::Plane, f: f32);
    fn clamp_min_rgb(&self, a: &mut Self::Rgb, min: f32);
    fn split(&self, rgb: &Self::Rgb) -> [Self::Plane; 3];
    fn merge(&self, planes: &[Self::Plane; 3]) -> Self::Rgb;
    fn sum_squares(&self, planes: &[Self::Plane; 3]) -> Self::Plane;

    /// Depth map pass 1: `sum += focus`; where `focus > best`: `best = focus`, `depth = index`.
    fn argmax_update(
        &self,
        best: &mut Self::Plane,
        sum: &mut Self::Plane,
        depth: &mut Self::Plane,
        focus: &Self::Plane,
        index: usize,
    );
    /// Pyramid fusion: where `energy > fused_energy`, copy `level` into `fused`, keep the
    /// higher energy and (if given) write `index` into `source_map`.
    fn fuse(
        &self,
        fused: &mut [Self::Plane; 3],
        fused_energy: &mut Self::Plane,
        level: &[Self::Plane; 3],
        energy: &Self::Plane,
        source_map: Option<&mut Self::Plane>,
        index: usize,
    );
    /// Depth map pass 2: `out += frame · max(0, 1 − |depth − index|)`.
    fn blend(&self, out: &mut Self::Rgb, frame: &Self::Rgb, depth: &Self::Plane, index: usize);
}

pub(crate) fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil() as isize;
    let mut kernel: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|v| *v /= sum);
    kernel
}

pub(crate) fn gaussian_blur<B: Backend>(b: &B, p: &B::Plane, sigma: f32) -> B::Plane {
    b.convolve(p, &gaussian_kernel(sigma.max(1e-3)))
}

pub(crate) fn box_blur<B: Backend>(b: &B, p: &B::Plane, radius: usize) -> B::Plane {
    let n = 2 * radius + 1;
    b.convolve(p, &vec![1.0 / n as f32; n])
}
