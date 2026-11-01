# Shroom Stack

Focus stacking for macro photography: combines a series of DNG frames with different
focal planes into one image that is sharp throughout, and writes it as a Linear DNG
that stays fully editable in Lightroom.

- **Two methods:** *depth map* (each pixel from its sharpest frame, parameters radius and
  smoothing) and *pyramid* (detail combined at every scale, parameter smoothing).
- **Alignment** compensates shifts and focus breathing (frames growing or shrinking
  between shots); the output is cropped to the area all frames cover.
- **GPU accelerated** via wgpu (Metal, Vulkan, DirectX 12) with an identical CPU
  fallback; frames are processed one at a time, so stack size doesn't limit memory.
- **Retouching:** paint parts of any input frame into the result with a soft brush,
  with undo/redo.
- **Lightroom friendly output:** camera colour profile, white balance, EXIF and XMP
  (labels, lens) are carried over; crop settings are reset.

## Building and running

Requires a current stable Rust toolchain ([rustup](https://rustup.rs)).

```sh
cargo run --release -p shroom-ui                 # desktop app
cargo run --release -p shroom-ui -- frames/*.dng # app with files preloaded
./scripts/bundle-macos.sh                        # builds target/bundle/Shroom Stack.app
```

On Linux the usual windowing libraries are needed (e.g. on Debian/Ubuntu:
`libxkbcommon-dev libwayland-dev libx11-dev libxi-dev libgl1-mesa-dev`).

## Using the app

1. **Preparation:** add DNG files ("Add images…", drag & drop of files or a folder).
   They are sorted by capture time; reorder with Up/Down/Reverse if needed. Choose the
   method and parameters, then **Render**. Checks run first (frame count, sizes,
   cameras, shooting order, memory).
2. **Processing:** progress and a live view of the current frame and the depth map;
   **Cancel** goes back to Preparation.
3. **Postprocessing:** compare any input frame (aligned) with the result or the depth
   map. **Brush** (B) paints the selected frame into the result; size `[` `]`, undo
   Cmd/Ctrl+Z, redo Shift+Cmd/Ctrl+Z. **Save…** writes a Linear DNG.

Viewer: mouse wheel zooms at the cursor, Space+drag pans; Fit, 1:1, −, + and the slider
are below the image.

## Command line

`shroom-cli` (binary `shroom`) exposes the same core, mainly for development:

```sh
cargo run --release -p shroom-cli -- stack frames/*.dng -o out.dng --method pyramid --preview out.jpg
cargo run --release -p shroom-cli -- info frames/*.dng
cargo run --release -p shroom-cli -- sharpness a.dng b.dng   # compare detail energy
```

`stack` options: `--method depth-map|pyramid`, `--radius`, `--smoothing`, `--no-align`,
`--cpu`, `--preview`, `--depth-map`.

## Development

```sh
cargo test --workspace                           # unit and UI tests
cargo test --release --workspace -- --ignored    # + tests on examples/ (if present)
cargo clippy --workspace --all-targets && cargo fmt
```

The ignored UI tests write screenshots to `target/ui-screenshots/`. The example stacks
in `examples/` are not part of the repository.

| Crate | Purpose |
|---|---|
| `shroom-core` | DNG I/O, demosaicing, alignment, stacking (CPU and GPU), retouching |
| `shroom-ui` | egui desktop application |
| `shroom-cli` | command-line front end |

Design notes and the history of each milestone are in [PLAN.md](PLAN.md).

## Licence note

Raw decoding uses [rawler](https://github.com/dnglab/dnglab), licensed LGPL-2.1.
