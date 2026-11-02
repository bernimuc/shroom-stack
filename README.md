# Shroom Stack

Focus stacking for macro photography: combines a series of raw frames (DNG, CR3 and
other camera raw formats) with different focal planes into one image that is sharp throughout, and writes it as a Linear DNG
that stays fully editable in Lightroom.

- **Two methods:** *depth map* (each pixel from its sharpest frame, parameters radius and
  smoothing) and *pyramid* (detail combined at every scale, parameter smoothing).
- **Alignment** compensates shifts and focus breathing (frames growing or shrinking
  between shots); the output is cropped to the area all frames cover.
- **GPU accelerated** via wgpu (Metal, Vulkan, DirectX 12) with an identical CPU
  fallback; frames are processed one at a time, so stack size doesn't limit memory.
- **Soft bokeh** (optional): backgrounds that are out of focus in every frame are taken
  from the most blurred (or nearest-focus) frame, keeping the large-aperture look.
  Expects frames ordered from near to far focus, as focus bracketing shoots them.
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

On Linux the usual windowing libraries are needed (e.g. on Debian/Ubuntu:[Shroom Stack session.json](../stack-samples/pilze-20261031/100CANON/Shroom%20Stack%20session.json)
`libxkbcommon-dev libwayland-dev libx11-dev libxi-dev libgl1-mesa-dev`).

## Using the app

1. **Manage stacks** (where the app starts): "Open folder…" (or drop a folder) groups
   its photos into stacks, using pauses between shots, changed camera settings and
   changed pictures. Uncertain boundaries are marked "Check: new stack?" with the
   reasons.
   - Right-click a thumbnail to start a new stack there, move photos to the previous or
     next stack, or exclude them; "Merge with previous" joins stacks.
   - Select several files with Cmd/Ctrl-click and Shift-click (Esc clears the selection);
     right-click to move the selected photos to any stack or a new one, or to exclude or
     include them.
   - **Delete** (or Backspace), "Move to Trash…" in the context menu and the red trash
     button of a stack move files to the system trash, after asking.
   - Stacking results appear in a green row below their stack's photos, with the method
     as an icon and the settings in the tooltip. A result is matched to its stack by the
     source frames recorded in it (older results by session file or file name).
   - Double-click a thumbnail to view it on its own: ← → step through the files, P and X
     include or exclude the photo, Esc or "Back to stacks" returns to the list.
   - The slider below the list sets the thumbnail size.
   - **Open** loads a stack into Preparation; **Render selected** stacks the checked
     stacks one after another and saves each result next to its originals
     (`<first photo>-stacked.dng`, numbered instead of overwriting).

   The grouping is kept in `Shroom Stack session.json` in the folder and restored when it
   is opened again.
2. **Preparation:** add raw files ("Add images…", drag & drop of files or a folder).
   They are sorted by capture time; reorder with Up/Down/Reverse if needed. Choose the
   method and parameters (optionally **Soft bokeh** with its source frame and
   threshold), then **Render**. Checks run first (frame count, sizes,
   cameras, shooting order, memory).
3. **Postprocessing:** while stacking runs, the progress and a live view of the current
   frame and the depth map (**Cancel** goes back to Preparation). Then compare any input
   frame (aligned) with the result or the depth map; **Show soft bokeh area** tints what
   soft bokeh replaced. **Brush** (B) paints the selected frame into the result; size
   `[` `]`, undo Cmd/Ctrl+Z, redo Shift+Cmd/Ctrl+Z. **Save…** writes a Linear DNG (a free
   name is suggested).

Results record the source frames (`xmpMM:Ingredients`) and the stacking settings in their
XMP metadata.

Orientation: taken from the camera (EXIF); **Rotate left/right** (Cmd/Ctrl+[ / ]) in
Preparation and Postprocessing override it for the whole stack and for the saved file.

Viewer: mouse wheel and + / − zoom at the cursor, dragging pans (with the brush:
Space+drag), double-click switches between fit and 1:1 at the clicked point; Fit, 1:1,
−, + and the slider are below the image. **F** over an image shows it full screen without
any other controls; F or Esc goes back.

## Command line

`shroom-cli` (binary `shroom`) exposes the same core, mainly for development:

```sh
cargo run --release -p shroom-cli -- stack frames/*.dng -o out.dng --method pyramid --preview out.jpg
cargo run --release -p shroom-cli -- info frames/*.dng
cargo run --release -p shroom-cli -- sharpness a.dng b.dng   # compare detail energy
```

`stack` options: `--method depth-map|pyramid`, `--radius`, `--smoothing`, `--no-align`,
`--cpu`, `--preview`, `--depth-map`, `--bokeh most-blurred|nearest-focus`,
`--bokeh-threshold`, `--bokeh-mask` (preview with the soft bokeh areas tinted).

## Development

```sh
cargo test --workspace                           # unit and UI tests
cargo test --release --workspace -- --ignored    # + tests on examples/ (if present)
cargo clippy --workspace --all-targets && cargo fmt
```

The ignored UI tests write screenshots to `target/ui-screenshots/`. The example stacks
in `examples/` are not part of the repository.

`python3 scripts/third-party-licenses.py -o THIRD-PARTY-LICENSES.txt [--target <triple>]`
collects the licenses of all crates linked into the binaries; CI adds this file and the
license files to each downloadable build.

| Crate | Purpose |
|---|---|
| `shroom-core` | DNG I/O, demosaicing, alignment, stacking (CPU and GPU), retouching |
| `shroom-ui` | egui desktop application |
| `shroom-cli` | command-line front end |

Design notes and the history of each milestone are in [PLAN.md](PLAN.md).

## License

Shroom Stack is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.

### Third-party code

Raw decoding uses [rawler](https://github.com/dnglab/dnglab) (part of dnglab), licensed
under the GNU Lesser General Public License, version 2.1. It is linked into the
application binaries; its source code is available from the dnglab repository and from
[crates.io](https://crates.io/crates/rawler). Since Shroom Stack's own source is public, you
can rebuild the application against a modified rawler with Cargo.

The other dependencies are under permissive licenses (mostly MIT and/or Apache-2.0; also
BSD, Zlib, ISC, Unicode, MPL-2.0 for unmodified crates, and the SIL Open Font License and
Ubuntu Font Licence for the fonts embedded by egui).
