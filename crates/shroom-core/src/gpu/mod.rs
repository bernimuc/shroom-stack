//! GPU backend (wgpu compute shaders; Metal, Vulkan or DirectX 12).

mod context;

use std::sync::OnceLock;

use context::{GpuContext, Params};

use crate::align::Affine;
use crate::compute::Backend;
use crate::image::{Plane, Rect, RgbImage};
use crate::raw::CfaPattern;

pub(crate) struct GpuPlane {
    buf: Pooled,
    w: usize,
    h: usize,
}

pub(crate) struct GpuRgb {
    buf: Pooled,
    w: usize,
    h: usize,
}

/// A storage buffer that goes back to the shared GPU's pool when dropped.
struct Pooled(Option<wgpu::Buffer>);

impl std::ops::Deref for Pooled {
    type Target = wgpu::Buffer;
    fn deref(&self) -> &wgpu::Buffer {
        self.0.as_ref().expect("buffer present until drop")
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if let (Some(buffer), Some(gpu)) = (self.0.take(), gpu()) {
            gpu.ctx.release(buffer);
        }
    }
}

/// The shared GPU, opened on first use; `None` if no usable GPU is present.
pub(crate) fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| GpuContext::new().map(|ctx| Gpu { ctx })).as_ref()
}

pub(crate) struct Gpu {
    ctx: GpuContext,
}

impl Gpu {
    /// Blocks until all submitted GPU work is done (for profiling).
    #[cfg(test)]
    pub fn sync(&self) {
        self.ctx.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }

    /// Whether RGB images of this size fit into a single GPU buffer.
    pub fn fits(&self, w: usize, h: usize) -> bool {
        (w * h * 3 * 4) as u64 <= self.ctx.max_buffer
    }
}

macro_rules! kernel {
    ($name:literal) => {
        ($name, include_str!(concat!("shaders/", $name, ".wgsl")))
    };
}

fn params(w: usize, h: usize) -> Params {
    Params { w: w as u32, h: h as u32, ..Default::default() }
}

impl Gpu {
    fn run(&self, k: (&'static str, &str), p: Params, buffers: &[&wgpu::Buffer], w: usize, h: usize) {
        self.ctx.run(k.0, k.1, p, buffers, w, h);
    }

    /// Plane with undefined contents; every kernel writing one covers all pixels.
    fn plane(&self, w: usize, h: usize) -> GpuPlane {
        GpuPlane { buf: Pooled(Some(self.ctx.storage(w * h))), w, h }
    }

    fn rgb(&self, w: usize, h: usize) -> GpuRgb {
        GpuRgb { buf: Pooled(Some(self.ctx.storage(w * h * 3))), w, h }
    }

    fn binary(&self, op: u32, a: &GpuPlane, b: &GpuPlane) -> GpuPlane {
        let out = self.plane(a.w, a.h);
        let p = Params { i: [op, 0, 0, 0], ..params(a.w, a.h) };
        self.run(kernel!("binary"), p, &[&a.buf, &b.buf, &out.buf], a.w, a.h);
        out
    }

    fn unary(&self, buf: &wgpu::Buffer, w: usize, h: usize, op: u32, f: f32) {
        let p = Params { i: [op, 0, 0, 0], f: [f, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..params(w, h) };
        self.run(kernel!("unary"), p, &[buf], w, h);
    }
}

impl Backend for Gpu {
    type Plane = GpuPlane;
    type Rgb = GpuRgb;

    fn name(&self) -> String {
        format!("GPU: {}", self.ctx.adapter_name)
    }
    fn finish(&self) {
        self.ctx.device.poll(wgpu::PollType::wait_indefinitely()).expect("GPU poll failed");
    }
    fn plane_size(&self, p: &GpuPlane) -> (usize, usize) {
        (p.w, p.h)
    }
    fn rgb_size(&self, p: &GpuRgb) -> (usize, usize) {
        (p.w, p.h)
    }
    fn upload_plane(&self, p: &Plane) -> GpuPlane {
        GpuPlane { buf: Pooled(Some(self.ctx.upload(p.data()))), w: p.width(), h: p.height() }
    }
    fn upload_rgb(&self, img: &RgbImage) -> GpuRgb {
        GpuRgb { buf: Pooled(Some(self.ctx.upload(img.data()))), w: img.width(), h: img.height() }
    }
    fn download_plane(&self, p: &GpuPlane) -> Plane {
        Plane::from_vec(p.w, p.h, self.ctx.download(&p.buf, p.w * p.h))
    }
    fn download_rgb(&self, p: &GpuRgb) -> RgbImage {
        RgbImage::from_vec(p.w, p.h, self.ctx.download(&p.buf, p.w * p.h * 3))
    }
    fn zeros_plane(&self, w: usize, h: usize) -> GpuPlane {
        GpuPlane { buf: Pooled(Some(self.ctx.zeroed(w * h))), w, h }
    }
    fn zeros_rgb(&self, w: usize, h: usize) -> GpuRgb {
        GpuRgb { buf: Pooled(Some(self.ctx.zeroed(w * h * 3))), w, h }
    }
    fn trim(&self) {
        self.finish();
        self.ctx.trim();
    }

    fn demosaic(&self, mosaic: &GpuPlane, pattern: CfaPattern) -> GpuRgb {
        let out = self.rgb(mosaic.w, mosaic.h);
        let c = |x, y| pattern.color_at(x, y) as u32;
        let p = Params { i: [c(0, 0), c(1, 0), c(0, 1), c(1, 1)], ..params(mosaic.w, mosaic.h) };
        self.run(kernel!("demosaic"), p, &[&mosaic.buf, &out.buf], mosaic.w, mosaic.h);
        out
    }
    fn warp(&self, src: &GpuRgb, t: &Affine, rect: Rect) -> GpuRgb {
        let out = self.rgb(rect.width, rect.height);
        let m = t.m;
        let p = Params {
            sw: src.w as u32,
            sh: src.h as u32,
            i: [rect.x as u32, rect.y as u32, 0, 0],
            f: [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], 0.0, 0.0].map(|v| v as f32),
            ..params(rect.width, rect.height)
        };
        self.run(kernel!("warp"), p, &[&src.buf, &out.buf], rect.width, rect.height);
        out
    }
    fn luminance(&self, rgb: &GpuRgb) -> GpuPlane {
        let out = self.plane(rgb.w, rgb.h);
        self.run(kernel!("luminance"), params(rgb.w, rgb.h), &[&rgb.buf, &out.buf], rgb.w, rgb.h);
        out
    }
    fn convolve(&self, src: &GpuPlane, kernel: &[f32]) -> GpuPlane {
        let taps = Pooled(Some(self.ctx.upload(kernel)));
        let p = Params { i: [(kernel.len() / 2) as u32, 0, 0, 0], ..params(src.w, src.h) };
        let tmp = self.plane(src.w, src.h);
        self.run(kernel!("conv_h"), p, &[&src.buf, &*taps, &tmp.buf], src.w, src.h);
        let out = self.plane(src.w, src.h);
        self.run(kernel!("conv_v"), p, &[&tmp.buf, &*taps, &out.buf], src.w, src.h);
        out
    }
    fn laplacian(&self, src: &GpuPlane, square: bool) -> GpuPlane {
        let out = self.plane(src.w, src.h);
        let p = Params { i: [square as u32, 0, 0, 0], ..params(src.w, src.h) };
        self.run(kernel!("laplacian"), p, &[&src.buf, &out.buf], src.w, src.h);
        out
    }
    fn reduce(&self, src: &GpuPlane) -> GpuPlane {
        let out = self.plane(src.w.div_ceil(2), src.h.div_ceil(2));
        let p = Params { sw: src.w as u32, sh: src.h as u32, ..params(out.w, out.h) };
        self.run(kernel!("reduce"), p, &[&src.buf, &out.buf], out.w, out.h);
        out
    }
    fn expand(&self, src: &GpuPlane, w: usize, h: usize) -> GpuPlane {
        let out = self.plane(w, h);
        let p = Params { sw: src.w as u32, sh: src.h as u32, ..params(w, h) };
        self.run(kernel!("expand"), p, &[&src.buf, &out.buf], w, h);
        out
    }
    fn downsample_box(&self, src: &GpuPlane, factor: usize) -> GpuPlane {
        let out = self.plane(src.w / factor, src.h / factor);
        let p = Params { sw: src.w as u32, i: [factor as u32, 0, 0, 0], ..params(out.w, out.h) };
        self.run(kernel!("downbox"), p, &[&src.buf, &out.buf], out.w, out.h);
        out
    }
    fn downsample_rgb(&self, src: &GpuRgb, factor: usize) -> GpuRgb {
        let out = self.rgb(src.w / factor, src.h / factor);
        let p = Params { sw: src.w as u32, i: [factor as u32, 0, 0, 0], ..params(out.w, out.h) };
        self.run(kernel!("downbox_rgb"), p, &[&src.buf, &out.buf], out.w, out.h);
        out
    }
    fn sub(&self, a: &GpuPlane, b: &GpuPlane) -> GpuPlane {
        self.binary(0, a, b)
    }
    fn add_assign(&self, a: &mut GpuPlane, b: &GpuPlane) {
        self.run(kernel!("accumulate"), params(a.w, a.h), &[&a.buf, &b.buf], a.w, a.h);
    }
    fn max_assign(&self, a: &mut GpuPlane, b: &GpuPlane) {
        let p = Params { i: [1, 0, 0, 0], ..params(a.w, a.h) };
        self.run(kernel!("accumulate"), p, &[&a.buf, &b.buf], a.w, a.h);
    }
    fn scale(&self, a: &mut GpuPlane, f: f32) {
        self.unary(&a.buf, a.w, a.h, 0, f);
    }
    fn clamp_min_rgb(&self, a: &mut GpuRgb, min: f32) {
        self.unary(&a.buf, a.w * 3, a.h, 1, min);
    }
    fn log_rgb(&self, a: &mut GpuRgb, offset: f32) {
        self.unary(&a.buf, a.w * 3, a.h, 3, offset);
    }
    fn exp_rgb(&self, a: &mut GpuRgb, offset: f32) {
        self.unary(&a.buf, a.w * 3, a.h, 4, offset);
    }
    fn split(&self, rgb: &GpuRgb) -> [GpuPlane; 3] {
        let out = [(); 3].map(|_| self.plane(rgb.w, rgb.h));
        self.run(
            kernel!("split"),
            params(rgb.w, rgb.h),
            &[&rgb.buf, &out[0].buf, &out[1].buf, &out[2].buf],
            rgb.w,
            rgb.h,
        );
        out
    }
    fn merge(&self, planes: &[GpuPlane; 3]) -> GpuRgb {
        let (w, h) = (planes[0].w, planes[0].h);
        let out = self.rgb(w, h);
        self.run(kernel!("merge"), params(w, h), &[&planes[0].buf, &planes[1].buf, &planes[2].buf, &out.buf], w, h);
        out
    }
    fn sum_squares(&self, planes: &[GpuPlane; 3]) -> GpuPlane {
        let (w, h) = (planes[0].w, planes[0].h);
        let out = self.plane(w, h);
        self.run(kernel!("sumsq3"), params(w, h), &[&planes[0].buf, &planes[1].buf, &planes[2].buf, &out.buf], w, h);
        out
    }
    fn argmax_update(
        &self,
        best: &mut GpuPlane,
        sum: &mut GpuPlane,
        depth: &mut GpuPlane,
        focus: &GpuPlane,
        index: usize,
    ) {
        let p = Params { i: [index as u32, 0, 0, 0], ..params(best.w, best.h) };
        self.run(kernel!("argmax"), p, &[&best.buf, &sum.buf, &depth.buf, &focus.buf], best.w, best.h);
    }
    fn fuse(
        &self,
        fused: &mut [GpuPlane; 3],
        fused_energy: &mut GpuPlane,
        level: &[GpuPlane; 3],
        energy: &GpuPlane,
        source_map: Option<&mut GpuPlane>,
        index: usize,
    ) {
        let (w, h) = (energy.w, energy.h);
        let mask = self.plane(w, h);
        let dummy;
        let (map, flag) = match source_map {
            Some(m) => (&*m.buf, 1),
            None => {
                dummy = Pooled(Some(self.ctx.storage(1)));
                (&*dummy, 0)
            }
        };
        let p = Params { i: [index as u32, flag, 0, 0], ..params(w, h) };
        self.run(kernel!("fuse_energy"), p, &[&fused_energy.buf, &energy.buf, &mask.buf, map], w, h);
        for (dst, src) in fused.iter_mut().zip(level) {
            self.run(kernel!("select"), params(w, h), &[&dst.buf, &src.buf, &mask.buf], w, h);
        }
    }
    fn blend(&self, out: &mut GpuRgb, frame: &GpuRgb, depth: &GpuPlane, index: usize) {
        let p = Params { i: [index as u32, 0, 0, 0], ..params(out.w, out.h) };
        self.run(kernel!("blend"), p, &[&out.buf, &frame.buf, &depth.buf], out.w, out.h);
    }
}

/// Each GPU kernel against the CPU reference. Skipped when no GPU is available.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::{Cpu, box_blur, gaussian_blur};
    use crate::filter::{merge_channels, tests::noise};

    fn gpu_or_skip() -> Option<&'static Gpu> {
        let g = gpu();
        if g.is_none() {
            eprintln!("no GPU available, skipping");
        }
        g
    }

    fn assert_close(a: &[f32], b: &[f32], tol: f32, what: &str) {
        assert_eq!(a.len(), b.len(), "{what}: length");
        let max = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
        assert!(max <= tol, "{what}: max difference {max}");
    }

    fn rgb(w: usize, h: usize, seed: u32) -> RgbImage {
        merge_channels(&[noise(w, h, seed), noise(w, h, seed + 1), noise(w, h, seed + 2)])
    }

    #[test]
    fn kernels_match_cpu() {
        let Some(g) = gpu_or_skip() else { return };
        let (w, h) = (67, 45);
        let plane = noise(w, h, 7);
        let other = noise(w, h, 9);
        let img = rgb(w, h, 11);
        let (gp, go, gi) = (g.upload_plane(&plane), g.upload_plane(&other), g.upload_rgb(&img));
        let dp = |p: &GpuPlane| g.download_plane(p).data().to_vec();

        assert_close(g.download_rgb(&gi).data(), img.data(), 0.0, "roundtrip");
        for pattern in ["RGGB", "GBRG"] {
            let pat = CfaPattern::from_name(pattern).unwrap();
            assert_close(
                g.download_rgb(&g.demosaic(&gp, pat)).data(),
                Cpu.demosaic(&plane, pat).data(),
                1e-5,
                "demosaic",
            );
        }
        let t = Affine::similarity((33.0, 22.0), 0.97, 0.02, (1.3, -0.6));
        let rect = Rect::new(3, 2, 50, 30);
        assert_close(g.download_rgb(&g.warp(&gi, &t, rect)).data(), Cpu.warp(&img, &t, rect).data(), 1e-3, "warp");
        assert_close(&dp(&g.luminance(&gi)), Cpu.luminance(&img).data(), 1e-6, "luminance");
        assert_close(&dp(&gaussian_blur(g, &gp, 1.5)), gaussian_blur(&Cpu, &plane, 1.5).data(), 1e-5, "gaussian");
        assert_close(&dp(&box_blur(g, &gp, 4)), box_blur(&Cpu, &plane, 4).data(), 1e-5, "box");
        for square in [false, true] {
            assert_close(&dp(&g.laplacian(&gp, square)), Cpu.laplacian(&plane, square).data(), 1e-5, "laplacian");
        }
        let reduced = Cpu.reduce(&plane);
        assert_close(&dp(&g.reduce(&gp)), reduced.data(), 1e-5, "reduce");
        let gr = g.upload_plane(&reduced);
        assert_close(&dp(&g.expand(&gr, w, h)), Cpu.expand(&reduced, w, h).data(), 1e-5, "expand");
        assert_close(&dp(&g.downsample_box(&gp, 3)), Cpu.downsample_box(&plane, 3).data(), 1e-5, "downsample");
        assert_close(&dp(&g.sub(&gp, &go)), Cpu.sub(&plane, &other).data(), 1e-6, "sub");

        let (mut ga, mut ca) = (g.upload_plane(&plane), plane.clone());
        g.add_assign(&mut ga, &go);
        Cpu.add_assign(&mut ca, &other);
        g.scale(&mut ga, 0.5);
        Cpu.scale(&mut ca, 0.5);
        assert_close(&dp(&ga), ca.data(), 1e-6, "add/scale");
        g.max_assign(&mut ga, &go);
        Cpu.max_assign(&mut ca, &other);
        assert_close(&dp(&ga), ca.data(), 1e-6, "max");

        let shifted = RgbImage::from_vec(w, h, img.data().iter().map(|v| v - 0.5).collect());
        let (mut gs, mut cs) = (g.upload_rgb(&shifted), shifted.clone());
        g.clamp_min_rgb(&mut gs, 0.0);
        Cpu.clamp_min_rgb(&mut cs, 0.0);
        assert_close(g.download_rgb(&gs).data(), cs.data(), 0.0, "clamp");
        g.log_rgb(&mut gs, 1e-3);
        Cpu.log_rgb(&mut cs, 1e-3);
        assert_close(g.download_rgb(&gs).data(), cs.data(), 1e-5, "log");
        g.exp_rgb(&mut gs, 1e-3);
        Cpu.exp_rgb(&mut cs, 1e-3);
        assert_close(g.download_rgb(&gs).data(), cs.data(), 1e-5, "exp");

        let split = g.split(&gi);
        let cpu_split = Cpu.split(&img);
        for c in 0..3 {
            assert_close(&dp(&split[c]), cpu_split[c].data(), 0.0, "split");
        }
        assert_close(g.download_rgb(&g.merge(&split)).data(), img.data(), 0.0, "merge");
        assert_close(&dp(&g.sum_squares(&split)), Cpu.sum_squares(&cpu_split).data(), 1e-5, "sum_squares");
    }

    #[test]
    fn accumulating_kernels_match_cpu() {
        let Some(g) = gpu_or_skip() else { return };
        let (w, h) = (40, 30);
        let focus: Vec<Plane> = (0..3).map(|i| noise(w, h, 20 + i)).collect();
        let (mut cb, mut cs, mut cd) = (focus[0].clone(), focus[0].clone(), Plane::new(w, h));
        let (mut gb, mut gs, mut gd) = (g.upload_plane(&focus[0]), g.upload_plane(&focus[0]), g.zeros_plane(w, h));
        for (i, f) in focus.iter().enumerate().skip(1) {
            Cpu.argmax_update(&mut cb, &mut cs, &mut cd, f, i);
            g.argmax_update(&mut gb, &mut gs, &mut gd, &g.upload_plane(f), i);
        }
        assert_close(g.download_plane(&gb).data(), cb.data(), 0.0, "argmax best");
        assert_close(g.download_plane(&gs).data(), cs.data(), 1e-6, "argmax sum");
        assert_close(g.download_plane(&gd).data(), cd.data(), 0.0, "argmax depth");

        let level = Cpu.split(&rgb(w, h, 30));
        let energy = noise(w, h, 40);
        let mut c_fused = Cpu.split(&rgb(w, h, 50));
        let mut c_energy = noise(w, h, 60);
        let mut c_map = Plane::new(w, h);
        let mut g_fused = c_fused.each_ref().map(|p| g.upload_plane(p));
        let mut g_energy = g.upload_plane(&c_energy);
        let mut g_map = g.zeros_plane(w, h);
        Cpu.fuse(&mut c_fused, &mut c_energy, &level, &energy, Some(&mut c_map), 5);
        let g_level = level.each_ref().map(|p| g.upload_plane(p));
        g.fuse(&mut g_fused, &mut g_energy, &g_level, &g.upload_plane(&energy), Some(&mut g_map), 5);
        for c in 0..3 {
            assert_close(g.download_plane(&g_fused[c]).data(), c_fused[c].data(), 0.0, "fuse level");
        }
        assert_close(g.download_plane(&g_energy).data(), c_energy.data(), 0.0, "fuse energy");
        assert_close(g.download_plane(&g_map).data(), c_map.data(), 0.0, "fuse map");

        let depth = Plane::from_vec(w, h, noise(w, h, 70).data().iter().map(|v| v * 3.0).collect());
        let frame = rgb(w, h, 80);
        let (mut co, mut go) = (RgbImage::new(w, h), g.zeros_rgb(w, h));
        for i in 0..4 {
            Cpu.blend(&mut co, &frame, &depth, i);
            g.blend(&mut go, &g.upload_rgb(&frame), &g.upload_plane(&depth), i);
        }
        assert_close(g.download_rgb(&go).data(), co.data(), 1e-6, "blend");
    }

    #[test]
    fn stacking_on_gpu_matches_cpu() {
        use crate::stack::{CancelToken, Method, StackSettings, stack, tests::banded_stack};
        if gpu_or_skip().is_none() {
            return;
        }
        let (_, frames) = banded_stack(96, 64, 3);
        for method in [Method::DEFAULT_DEPTH_MAP, Method::DEFAULT_PYRAMID] {
            let run = |use_gpu| {
                let settings = StackSettings { method, align: false, use_gpu, bokeh: None };
                stack(&frames, &settings, &|_| {}, &CancelToken::default()).unwrap()
            };
            let (cpu, gpu) = (run(false), run(true));
            assert!(gpu.backend.starts_with("GPU"), "{}", gpu.backend);
            assert_close(gpu.image.data(), cpu.image.data(), 1e-4, &format!("{method:?} image"));
            assert_close(gpu.depth_map.data(), cpu.depth_map.data(), 1e-3, &format!("{method:?} depth"));
        }
    }
}
