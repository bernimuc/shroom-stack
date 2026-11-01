# Shroom Stack – Roadmap for the ideas in IDEAS.md

Status: **agreed plan**, nothing implemented yet. Each feature has a feasibility
rating, a proposed approach, dependencies and open questions (marked **Q**). Answers so
far are recorded as **A**.

## Overview

| # | Feature | Feasibility | Effort | Risk | Depends on |
|---|---|---|---|---|---|
| 1 | Auto rotate | high | small | low | – |
| 2 | Camera RAW support | high (reading already works) | medium | medium (colour look in Lightroom) | – |
| 3 | Soft bokeh | high | medium | low–medium (tuning) | – |
| 4 | Stack management (+ batch) | high | medium–large | medium (grouping heuristics) | 1, 2 helpful |
| 5 | Lightroom plugin | high | medium | medium (needs testing in Lightroom) | 2 helpful, 4 for colour labels |
| 6 | Generative fill of occlusion gaps | partial | large | high | detection shares work with 3 |

```
1 Auto rotate ──┐
2 RAW support ──┼──► 4 Stack management ──► 5 Lightroom plugin (labels, batch)
                │                         ▲
                └─────────────────────────┘ (plugin can export originals)
3 Soft bokeh ──► (focus/confidence analysis) ──► 6 Generative fill (problem zones)
```

**Proposed order:** 1 → 2 → 3 → 4 → 5 → 6 (6 starts with a research spike).

---

## 1. Auto rotate

**Status:** implemented (orientation from EXIF, per-stack override, display, thumbnails,
brush and saved DNG); writing the override back to Lightroom comes with the plugin (5).

**What the data offers:** cameras store an EXIF orientation (0/90/180/270°, mirrored
variants are not used by cameras). It comes from the camera's orientation sensor, which
is unreliable when shooting straight up/down – the cases mentioned in IDEAS.md.

**Approach**
- Processing stays in sensor orientation (alignment, stacking, retouching unchanged).
  Orientation is applied only for display and written into the output DNG's
  `Orientation` tag – lossless, and Lightroom shows it correctly.
- Per stack an effective orientation: default from the first frame's EXIF, overridable
  with "rotate left/right" buttons (and shortcut). The viewer, file list thumbnails,
  live view and brush map coordinates through the rotation.
- Preflight warns if frames of one stack have different EXIF orientations (sensor flipped
  mid-stack) – the override then decides.

**Q1.1** Is one orientation per stack enough (no per-frame override)?
**A:** yes – the orientation doesn't change within a stack.
**Q1.2** Should the override also be remembered for the source files (e.g. written back
to Lightroom via the plugin), or only affect the result?
**A:** both – the result and the source photos. For the sources this goes through the
Lightroom plugin (to verify: whether the SDK can rotate photos in the catalogue).

## 2. Camera RAW support (CR3 etc.)

**Finding:** rawler already decodes CR3 (and NEF, ARW, RAF, ORF, RW2, …); a probe on
Stack 5 (CR3 originals of Stack 1) converted fine. What differs from Lightroom's DNGs:

| | Lightroom DNG | Direct from CR3 |
|---|---|---|
| Colour matrices, white balance | Adobe's | rawler's own (different values) |
| ForwardMatrix, camera calibration, Adobe Standard profile tables | yes | **missing** |
| BaselineExposure | yes | **missing** (result looks darker) |
| Usable image area | 8224×5490 | 8214×5478 (slightly different crop) |
| Embedded preview | 1024 px | **8192 px JPEG** (slow for thumbnails) |

**Approach**
- Accept all rawler formats in file dialogs, drag & drop and the preflight; mixed
  formats in one stack are rejected.
- Thumbnails: use the smallest embedded preview (CR3 also has a small one) or decode the
  large JPEG downscaled.
- Output stays Linear DNG. Colour: write what is available (matrices, as-shot white
  balance, `UniqueCameraModel`). Whether Lightroom then offers its own "Adobe Standard"
  profile for this camera, or only "Embedded", must be tested – this decides how close the
  result looks to Lightroom's rendering of the CR3.
- Fallback if needed: let the user point to a matching Lightroom DNG / DCP profile once
  per camera, and copy its profile tags (we already copy them from DNG sources).
- Regression check: Stack 5 (CR3) vs Stack 1 (DNG) must give the same geometry and
  equivalent results.

**Q2.1** Which formats matter to you – Canon CR3 only, or others too?
**A:** CR3 now (only format available for testing); other cameras later, test stacks can
be provided.
**Q2.2** How close must the colour be to Lightroom's own rendering of the CR3? (I would
prepare a test DNG from Stack 5 for you to compare in Lightroom first.)
**A:** agreed – start with a test DNG from Stack 5 compared in Lightroom.

## 3. Soft bokeh

**Idea restated:** in areas that are out of focus in *every* frame (background bokeh),
don't take the "least blurred" frame (furthest focus) but the most blurred one, so the
large-aperture look is preserved.

**Approach** (both methods)
- Detection: we already compute, per pixel, how much the best frame stands out from the
  average (the depth map's confidence). Bokeh regions = large connected areas where even
  the best frame has no real detail (confidence near the noise floor), with a
  size/threshold control and a soft (feathered) edge.
- Bokeh frame: the frame where the background is most blurred – usually the one focused
  nearest. Determined per region by the *lowest* focus energy (robust, no assumption about
  focus direction); the near-focus frame as an alternative.
- Composition: depth map method – these regions get the bokeh frame's index instead of
  the interpolated depth. Pyramid method – blend the bokeh frame in with a pyramid of the
  feathered mask (seamless multi-scale blending); requires one extra analysis pass.
- UI: checkbox "Soft bokeh" with a strength/threshold slider, the detected area shown as
  an overlay in Postprocessing; the brush remains for corrections.

**Q3.1** Most blurred frame or nearest-focus frame – or a choice between both?
**A:** offer both.
**Q3.2** Should the bokeh mask be editable (paint include/exclude), or is the brush
enough?
*Clarification:* the existing retouch brush paints pixels of a chosen frame into the
finished result. A *mask brush* would instead edit the detected bokeh area itself (shown
as an overlay; add/remove areas), after which the result is recomputed. Open: is the
retouch brush enough, or is the mask brush wanted as well?
**A:** the retouch brush is enough for now; revisit after trying soft bokeh.

## 4. Stack management

**Shooting practice (A):** mostly in-camera focus bracketing, some manual stacks (manual
focus lens); 10–60 frames per stack. Rule: setting up a new stack always takes longer
than the gap between shots of a stack. Grouping should look for patterns, not fixed
thresholds.

**Grouping without fixed thresholds**
- Gaps relative to their surroundings: a gap is a boundary candidate when it is clearly
  larger than the typical gaps before and after it (factor of the local median, not
  seconds). Additionally the distribution of all gaps of a session splits into "within
  stack" and "between stacks" (two clusters on a log scale).
- Further evidence per boundary: changed aperture, exposure time, ISO or focal length;
  focus distance reversing after running monotonically; image content change between
  neighbouring thumbnails (alignment residual on small previews); orientation change.
- Each boundary gets a confidence; uncertain ones are highlighted for review (manual
  stacks).
- To check: whether Canon's focus bracketing writes a sequence marker into the maker
  notes – that would make automatic stacks near-certain.

**UI:** workspace "Session" – stacks as rows of thumbnails; split, merge, move or exclude
frames; open a stack in Preparation; later batch processing.

**Q4.1** How are stacks shot? **A:** see "Shooting practice" above.
**Q4.2** *Batch processing:* after a session is split into stacks, either open each stack
in Preparation and render/save it individually, or select several stacks, choose method
and parameters once, and let the app process them unattended, saving each result
automatically. Is batch processing wanted – and where do its results go (next to the
originals or a chosen folder), named e.g. `<first file>-stacked.dng`?
**A:** yes – batch processing of stacks the user selects; results always next to the
originals.
**Q4.3** Colour labels via plugin or XMP? **A:** via the plugin (options 1 and 3, below).

**Getting groups into Lightroom – options**

1. *Lightroom first:* a Library-menu plugin action passes the selected photos' original
   paths and metadata to the app (nothing is copied). The app groups, the user adjusts,
   the plugin sets colour labels in the catalogue. Robust: no file changes.
2. *App standalone, XMP files:* the app writes labels into XMP (sidecars for raw files,
   embedded for DNGs). Fragile with Lightroom: changes are only flagged; "Read Metadata
   from Files" applies the whole XMP and can reset develop settings if a sidecar is
   missing or incomplete; DNG originals get modified; label names are language-specific.
   Only useful as an option for use without Lightroom.
3. *App standalone, session file:* the app writes the groups to a small JSON file next to
   the photos; a plugin action "Apply Shroom Stack groups" reads it and sets the labels
   in the catalogue. Same safety as 1, no need to start from Lightroom.

Options 1 and 3 share the grouping view, the file format and the plugin's labelling
step; proposal: build both, XMP writing (2) only as an option without Lightroom.

**A:** decided – options 1 and 3; no XMP writing for now.

**Q4.4** Which colour scheme: cycle through the five label colours so neighbouring stacks
differ? What about photos that already have a label (your files carry "Blau")?
**A:** cycle through all five colours. Whether existing labels are overwritten or kept is
a user choice (option when applying the groups).
**Q4.5** Besides labels: also a keyword or a collection per stack (e.g. a collection set
per session with one collection per stack)?
**A:** an automatically created keyword per stack, to filter the library down to one
stack. Proposal: hierarchical keyword under a parent "Shroom Stack", child named after
the stack's first capture time and size, e.g. `Shroom Stack > 2026-10-17 16:53 (34)`,
not included on export.
**Q4.6** Should the app remember sessions (reopen a folder later with the groups as
edited)?
**A:** yes – reopening a folder picks up its session file.

## 5. Lightroom (Classic) plugin

**Feasibility:** Lightroom Classic has a Lua plugin SDK with export-service plugins. The
cloud-based "Lightroom" has no comparable plugin interface – so this targets Classic.

**Approach**
- Export service "Shroom Stack": exports the selected photos (originals once 2 is done,
  otherwise as DNG) to a temporary folder and starts Shroom Stack with them.
- App: a "handoff" mode via command-line options – files to load, where to save, and a
  result file the plugin reads (path of the saved DNG, or "cancelled"). In this mode
  "Save" can offer "Save & return to Lightroom".
- Plugin waits for the app, then imports the result into the catalogue next to the
  sources (optional: same folder, collection, keyword/label).
- With 4: a library-menu action "Detect stacks and label" – the plugin passes the
  selection to the app's grouping (no UI), receives groups, and sets colour labels.
- I can write the plugin, but I cannot run Lightroom here: testing has to happen on your
  side.

**Q5.1** Lightroom Classic, correct? Which version?
**A:** Lightroom Classic only. Plugin actions planned: "Stack in Shroom Stack" (pass the
selected originals, import the result – needs 2 for CR3) and the stack-management
actions from 4 (paths only, no export).
**Q5.2** Where should results go – next to the originals, or a fixed folder? Should the
result be grouped with its sources in Lightroom (Lightroom's SDK can't create stacks;
alternatives: same label, keyword, collection)?
**A:** the result gets the same colour label and stack keyword as its frames. Location
next to the originals (as for batch results, Q4.2).

## 6. Generative fill of occlusion gaps

**Feasibility:** split in two very different parts.

- **Detecting the problem zones: feasible.** These are bands along depth-map
  discontinuities (near object in front of a far, sharp background) where no frame has
  detail: the near-focused frames show the background blurred, and in the far-focused
  frames the foreground's blur covers it. The same focus/confidence analysis as in 3 finds
  them; the result is a mask overlay that the user can refine with the brush (the
  "paint, then remove what already has detail" variant from IDEAS.md).
- **Generating the content: possible, but the riskiest feature.**
  - Image generators work on 8-bit sRGB images of about 1 megapixel; our data is
    47-megapixel linear camera RGB. Needs tiling, colour/tone conversion there and back,
    and seamless blending – and the generated detail is invented, not photographed.
  - Options: a cloud service (inpainting API: needs an account/API key, costs per image,
    uploads image crops) or a local model (several GB download, slow without a large GPU,
    complex dependency).
  - Recommendation: first build detection + mask (useful on its own: shows where to
    retouch), then a time-boxed research spike with one provider on real crops before
    committing to the full feature.

**Q6.1** Cloud service (which one, if you have a preference) or local only?
**A:** cloud service.
**Q6.2** Is invented detail acceptable for your use, if clearly limited to these zones?
**A:** yes, limited to the small problem zones.
**Q6.3** Start with detection + mask only?
**A:** yes.
