use rayon::prelude::*;

use super::Backend;
use crate::align::{Affine, warp_rgb};
use crate::filter;
use crate::image::{Plane, Rect, RgbImage};
use crate::raw::{CfaPattern, demosaic};

/// Reference backend on the CPU (multi-threaded with rayon).
pub(crate) struct Cpu;

fn zip_mut(a: &mut Plane, b: &Plane, f: impl Fn(&mut f32, f32) + Sync) {
    a.data_mut().par_iter_mut().zip(b.data().par_iter()).for_each(|(a, b)| f(a, *b));
}

impl Backend for Cpu {
    type Plane = Plane;
    type Rgb = RgbImage;

    fn name(&self) -> String {
        "CPU".into()
    }
    fn plane_size(&self, p: &Plane) -> (usize, usize) {
        (p.width(), p.height())
    }
    fn rgb_size(&self, p: &RgbImage) -> (usize, usize) {
        (p.width(), p.height())
    }
    fn upload_plane(&self, p: &Plane) -> Plane {
        p.clone()
    }
    fn upload_rgb(&self, img: &RgbImage) -> RgbImage {
        img.clone()
    }
    fn download_plane(&self, p: &Plane) -> Plane {
        p.clone()
    }
    fn download_rgb(&self, p: &RgbImage) -> RgbImage {
        p.clone()
    }
    fn zeros_plane(&self, w: usize, h: usize) -> Plane {
        Plane::new(w, h)
    }
    fn zeros_rgb(&self, w: usize, h: usize) -> RgbImage {
        RgbImage::new(w, h)
    }
    fn demosaic(&self, mosaic: &Plane, pattern: CfaPattern) -> RgbImage {
        demosaic(mosaic, pattern)
    }
    fn warp(&self, src: &RgbImage, t: &Affine, rect: Rect) -> RgbImage {
        warp_rgb(src, t, rect)
    }
    fn luminance(&self, rgb: &RgbImage) -> Plane {
        filter::luminance(rgb)
    }
    fn convolve(&self, p: &Plane, kernel: &[f32]) -> Plane {
        filter::convolve_separable(p, kernel)
    }
    fn laplacian(&self, p: &Plane, square: bool) -> Plane {
        let mut l = filter::laplacian(p);
        if square {
            l.data_mut().par_iter_mut().for_each(|v| *v *= *v);
        }
        l
    }
    fn reduce(&self, p: &Plane) -> Plane {
        filter::reduce(p)
    }
    fn expand(&self, p: &Plane, w: usize, h: usize) -> Plane {
        filter::expand(p, w, h)
    }
    fn downsample_box(&self, p: &Plane, factor: usize) -> Plane {
        filter::downsample_box(p, factor)
    }
    fn downsample_rgb(&self, p: &RgbImage, factor: usize) -> RgbImage {
        let planes = filter::split_channels(p).map(|c| filter::downsample_box(&c, factor));
        filter::merge_channels(&planes)
    }
    fn sub(&self, a: &Plane, b: &Plane) -> Plane {
        let mut out = a.clone();
        zip_mut(&mut out, b, |a, b| *a -= b);
        out
    }
    fn add_assign(&self, a: &mut Plane, b: &Plane) {
        zip_mut(a, b, |a, b| *a += b);
    }
    fn scale(&self, a: &mut Plane, f: f32) {
        a.data_mut().par_iter_mut().for_each(|v| *v *= f);
    }
    fn clamp_min_rgb(&self, a: &mut RgbImage, min: f32) {
        a.data_mut().par_iter_mut().for_each(|v| *v = v.max(min));
    }
    fn split(&self, rgb: &RgbImage) -> [Plane; 3] {
        filter::split_channels(rgb)
    }
    fn merge(&self, planes: &[Plane; 3]) -> RgbImage {
        filter::merge_channels(planes)
    }
    fn sum_squares(&self, planes: &[Plane; 3]) -> Plane {
        let (w, h) = (planes[0].width(), planes[0].height());
        let data = (0..w * h).into_par_iter().map(|i| planes.iter().map(|p| p.data()[i] * p.data()[i]).sum()).collect();
        Plane::from_vec(w, h, data)
    }
    fn argmax_update(&self, best: &mut Plane, sum: &mut Plane, depth: &mut Plane, focus: &Plane, index: usize) {
        best.data_mut()
            .par_iter_mut()
            .zip(sum.data_mut().par_iter_mut())
            .zip(depth.data_mut().par_iter_mut())
            .zip(focus.data().par_iter())
            .for_each(|(((b, s), d), f)| {
                *s += f;
                if *f > *b {
                    *b = *f;
                    *d = index as f32;
                }
            });
    }
    fn fuse(
        &self,
        fused: &mut [Plane; 3],
        fused_energy: &mut Plane,
        level: &[Plane; 3],
        energy: &Plane,
        source_map: Option<&mut Plane>,
        index: usize,
    ) {
        let take: Vec<bool> = fused_energy
            .data_mut()
            .par_iter_mut()
            .zip(energy.data().par_iter())
            .map(|(fe, e)| {
                let t = *e > *fe;
                if t {
                    *fe = *e;
                }
                t
            })
            .collect();
        let select = |dst: &mut Plane, src: &Plane| {
            dst.data_mut().par_iter_mut().zip(src.data().par_iter()).zip(take.par_iter()).for_each(|((d, s), t)| {
                if *t {
                    *d = *s;
                }
            })
        };
        for (dst, src) in fused.iter_mut().zip(level) {
            select(dst, src);
        }
        if let Some(map) = source_map {
            map.data_mut().par_iter_mut().zip(take.par_iter()).for_each(|(m, t)| {
                if *t {
                    *m = index as f32;
                }
            });
        }
    }
    fn blend(&self, out: &mut RgbImage, frame: &RgbImage, depth: &Plane, index: usize) {
        out.data_mut().par_chunks_mut(3).zip(frame.data().par_chunks(3)).zip(depth.data().par_iter()).for_each(
            |((o, f), d)| {
                let w = (1.0 - (d - index as f32).abs()).max(0.0);
                if w > 0.0 {
                    o.iter_mut().zip(f).for_each(|(o, f)| *o += w * f);
                }
            },
        );
    }
}
