# Shroom Stack – Implementation Plan (draft)

Status: agreed baseline (decisions in section 7). Items marked **[assumption]**
are technical choices that may be revisited during implementation.

## 1. What the example data tells us

| | Camera input (Lightroom DNG) | Helicon output DNG |
|---|---|---|
| Main image | Bayer CFA (RGGB), 16 bit, lossless JPEG (LJ92) | **Linear DNG** (`LinearRaw`), 3×16 bit RGB, LJ92 |
| Size | R7: 7128×4732 (33 MP), R5: 8352×5586 (47 MP) | slightly cropped (e.g. 6984×4660 vs 7128×4732) |
| Previews | 256 px RGB, 1024 px JPEG, 2048/512/256 px JPEG‑XL | 256 px + 1024 px JPEG |
| Stack sizes | 20–34 frames, ~40–50 MB each | |

Consequences:

* We need a **raw pipeline**: decode CFA → demosaic → camera‑space linear RGB.
  We stay in *linear camera RGB* the whole way (no white balance / tone curve
  baked in) and write a **Linear DNG** with the original color metadata
  (ColorMatrix, ForwardMatrix, AsShotNeutral, calibration, EXIF) copied from the
  first input. This is what Helicon does and keeps WB / exposure fully editable
  in Lightroom.
* The Helicon output is cropped → it **aligns** the frames (focus breathing
  changes magnification between frames) and crops to the common area.
* Memory: 34 × 47 MP × RGB × f32 ≈ 19 GB. On a 24 GB machine we cannot hold the
  whole stack in RAM; algorithms must be **streaming** (one frame at a time).
* The embedded 1024 px JPEG previews give us instant thumbnails without
  decoding the raw data.

## 2. Answers to the open questions in REQUIREMENTS.md

1. **Alignment – yes, required.** Focus breathing alone shifts and scales
   every frame by up to a few percent; without alignment edges get halos and
   ghosting. Plan: estimate a similarity transform (scale, rotation,
   translation; optionally full affine) between *consecutive* frames on a
   downscaled luminance image (pyramid ECC / phase correlation), chain the
   transforms to a reference frame, warp on the GPU, crop the result to the
   area covered by all frames. Alignment can be switched off in the toolbar.
2. **Order – auto‑detected, user can override.** Default sort by EXIF capture
   time (falls back to filename). Order matters for chained alignment and for
   the depth‑map smoothing. We can additionally *verify* the order: the
   per‑frame sharpness centroid should move monotonically through the stack;
   if it doesn’t, show a warning in the preflight check. Manual reorder via
   drag & drop in the file list, plus a "reverse order" button.

## 3. Architecture

Cargo workspace:

```
shroom-stack/
├─ crates/
│  ├─ shroom-core/   # pure library, no UI deps
│  │  ├─ io/         # DNG read (CFA + linear), DNG write (linear), metadata, previews
│  │  ├─ raw/        # black/white level, demosaic (bilinear → AMaZE/RCD later)
│  │  ├─ gpu/        # wgpu context, compute shaders (WGSL), buffer pool
│  │  ├─ align/      # transform estimation + warp
│  │  ├─ stack/      # depth-map + pyramid stackers, focus measures
│  │  ├─ retouch/    # brush masks, re-composition from source frames
│  │  └─ api.rs      # public interface (see below)
│  └─ shroom-ui/     # eframe/egui app, depends on shroom-core only via api
└─ examples/         # (git-ignored) test data
```

**Interface core ↔ UI** (sketch):

```rust
pub struct StackJob { frames: Vec<FrameRef>, method: Method, params: Params, align: AlignOpts }
pub enum Method { DepthMap { radius: u32, smoothing: u32 }, Pyramid { smoothing: u32 } }

pub fn load_frame_info(path) -> Result<FrameInfo>        // metadata + thumbnail, fast
pub fn preflight(&StackJob) -> Vec<Issue>                 // ≥2 frames, same size/camera/CFA, ...
pub fn run(job, progress: impl Fn(Progress), cancel: CancelToken) -> Result<StackResult>
pub enum Progress { Aligning{i,n}, Stacking{i,n, preview: PreviewImage /* depth map / weights */}, ... }
pub struct StackResult { image, depth_map: Option<..>, transforms, ... }
pub fn retouch(&mut StackResult, frame: usize, stroke: BrushStroke)
pub fn save_dng(&StackResult, path) -> Result<()>
```

Processing runs on a worker thread; the UI receives `Progress` messages over a
channel, so the UI never blocks and the run can be cancelled.

### Crate choices **[assumption]**

* **UI:** `eframe`/`egui` with the wgpu backend – Rust‑native, fast to iterate,
  lets us share the wgpu device so previews are GPU textures without copies.
* **Native dialogs:** `rfd` (NSSavePanel / NSOpenPanel on macOS).
* **DNG:** `rawler` (dnglab’s library: LJ92 decode, DNG reading *and* writing).
  Fallback: own TIFF/DNG writer on top of `tiff` + LJ92 encoder if rawler’s
  writer does not cover linear DNG with the metadata we need.
* **GPU:** `wgpu` compute shaders (Metal on macOS, Vulkan/DX12 elsewhere).
  Every GPU kernel gets a CPU reference implementation (`rayon`) used in unit
  tests and as fallback.

## 4. Algorithms

**Common pre‑processing (per frame, streamed):** decode → black/white level →
demosaic → linear RGB f16/f32 → align/warp → (stacker).

**Depth map** (Helicon “B”, params Radius R, Smoothing S):
1. Focus measure per pixel per frame: e.g. local energy of Laplacian / modified
   Laplacian over a window of radius R.
2. Running arg‑max over the stack → depth index map + confidence (kept, not all
   frames).
3. Regularize: smooth/median the depth map (S), fill low‑confidence areas
   (flat background) from neighbours.
4. Second pass: compose output by sampling each pixel from its depth frame,
   interpolating between adjacent frames for fractional depth. Needs a second
   streaming pass over the frames (or cached aligned frames on disk as f16).

**Pyramid** (Helicon “C”):
1. Laplacian pyramid per frame (GPU).
2. Fuse incrementally: for each level keep the coefficient with max
   (locally averaged) energy; base level averaged. Only the running fused
   pyramid + current frame are in memory.
3. Collapse pyramid → output. Clamp/handle overshoot (pyramid methods produce
   halos and can exceed white level).

**Processing visualization:** depth map in false colour (depth map method) or
per‑frame contribution/weight mask (pyramid) shown in the right half.

**Retouching:** keeps aligned source frames available (on‑disk f16 cache in a
temp dir) so the brush can copy pixels from the selected frame into the result
with a soft‑edged mask. Strokes are stored as a list → undo/redo.

## 5. Preview / display

* Display render: linear camera RGB → WB (AsShotNeutral) → camera→sRGB matrix →
  simple tone curve / gamma, done in a fragment shader. Not meant to match
  Lightroom, just a pleasant, consistent preview.
* Mip‑mapped GPU textures; zoom/pan state shared by both halves of the split
  view; controls exactly as in REQUIREMENTS (wheel zoom at cursor, Space‑drag,
  % label, 1:1, +, −, slider fit…100 %).

## 6. Milestones

Each milestone ends with tests green and something runnable.

| # | Milestone | Content |
|---|---|---|
| 0 ✅ | Setup | rustup toolchain, cargo workspace, CI‑style `cargo test`/`clippy`/`fmt`, `.gitignore` |
| 1 ✅ | DNG I/O | read metadata + embedded preview + CFA data; demosaic; write Linear DNG; round‑trip tests; open result in Lightroom (pending manual check) |
| 2 ✅ | CLI stacker (CPU) | `shroom-cli` for fast iteration: both methods on CPU, compare visually with Helicon outputs |
| 3 | Alignment | transform estimation, warp, crop; synthetic tests (known transform) |
| 4 | GPU | wgpu compute for demosaic, warp, focus measure, pyramid; CPU vs GPU equality tests; benchmarks on Stack 1 (34 × 47 MP) |
| 5 | UI shell + Preparation | egui app, file list w/ thumbnails, preview widget with zoom/pan, toolbar, preflight check |
| 6 | Processing workspace | worker thread, progress, live split view, cancel |
| 7 | Postprocessing | result split view, save dialog, brush retouching with undo |
| 8 | Polish | performance, memory limits, error handling, app bundle/icon |

Tests: unit tests per module with small synthetic images (generated in
tests, not the 4 GB examples); an opt‑in integration test suite
(`cargo test -- --ignored`) that runs against `examples/` if present.

## 7. Decisions

| Topic | Decision |
|---|---|
| Platforms | macOS (Apple Silicon) primary; Windows and Linux should work too (wgpu, egui, rfd are cross‑platform; keep CI/tests platform‑neutral, no macOS‑only APIs) |
| Output | Linear DNG like Helicon; always crop to the area covered by all aligned frames |
| Parameters | Same as Helicon: Radius + Smoothing (depth map), Smoothing (pyramid). Toolbar shows method‑specific input fields dynamically |
| Cancel | Cancelling during processing returns to *Preparation* (files stay loaded) |
| Going back | After a result exists the user can return to *Preparation* to try other settings; the app asks for confirmation before discarding the current result (incl. retouching) |
| Brush | Size, softness, undo/redo; painting works at every zoom level |
| Preview colours | Simple render (WB + colour matrix + gamma); need not match Lightroom |

## 8. Development

```bash
cargo test                                   # unit tests
cargo test --release -- --include-ignored    # + integration tests on examples/ (if present)
cargo clippy --all-targets && cargo fmt
cargo run --release -p shroom-cli -- info examples/Stack\ 3/*.dng
cargo run --release -p shroom-cli -- convert IN.dng OUT.dng   # raw → Linear DNG
cargo run --release -p shroom-cli -- preview IN.dng OUT.jpg   # simple display render
```

Notes from milestone 1:

* **rawler is LGPL‑2.1.** Fine for a personal/open‑source app. Distributing a
  closed‑source binary would require allowing users to relink (e.g. dynamic
  linking) – revisit before any public release.
* Demosaic: Malvar–He–Cutler (CPU reference, ~0.2 s for 33 MP with rayon).
* Image orientation (EXIF rotation) is passed through to the output but not yet
  applied in previews; all example files are unrotated.
* rawler truncates colour matrix values to 1/10000 when writing (e.g. −0.0786 →
  −0.0785); negligible.

Notes from milestone 2:

* Both stackers stream over the frames (depth map: 2 passes, pyramid: 1 pass).
  Stack 3 (20 × 33 MP) on M5 CPU: ~7 s, 2.4 GB (depth map) / 3.3 GB (pyramid) peak.
* Without alignment the example stacks show strong radial ghosting: focus breathing
  scales the image by ~2–3 % across the stack. Alignment (milestone 3) is required
  before results can be compared with Helicon.
