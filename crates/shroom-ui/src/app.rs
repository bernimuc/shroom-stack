//! Application state and the workspaces.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use eframe::egui::{self, Color32, RichText, Vec2};
use shroom_core::render::{ColorInfo, render_depth_map, render_display};
use shroom_core::retouch::{Brush, Retouch};
use shroom_core::stack::{Method, Progress, StackSettings, Stage};
use shroom_core::{Orientation, RgbImage};

use crate::dialogs::{Dialogs, Picked};
use crate::frames::{FrameList, Issue, Severity, preflight};
use crate::loader::{FullRequest, Loaded, Loader};
use crate::processing::{Finished, Job, JobError, JobEvent, overall_progress};
use crate::viewer::{self, Pane, TiledImage, ViewState};

const ERROR_COLOR: Color32 = Color32::from_rgb(255, 110, 110);
const WARNING_COLOR: Color32 = Color32::from_rgb(240, 200, 90);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workspace {
    Preparation,
    Processing,
    Postprocessing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MethodKind {
    DepthMap,
    Pyramid,
}

/// Stacking parameters as edited in the toolbar; values of the other method are kept.
struct Parameters {
    kind: MethodKind,
    radius: u32,
    depth_smoothing: u32,
    pyramid_smoothing: u32,
    align: bool,
}

impl Default for Parameters {
    fn default() -> Self {
        let Method::DepthMap { radius, smoothing } = Method::DEFAULT_DEPTH_MAP else { unreachable!() };
        let Method::Pyramid { smoothing: pyramid_smoothing } = Method::DEFAULT_PYRAMID else { unreachable!() };
        Self { kind: MethodKind::DepthMap, radius, depth_smoothing: smoothing, pyramid_smoothing, align: true }
    }
}

impl Parameters {
    fn method(&self) -> Method {
        match self.kind {
            MethodKind::DepthMap => Method::DepthMap { radius: self.radius, smoothing: self.depth_smoothing },
            MethodKind::Pyramid => Method::Pyramid { smoothing: self.pyramid_smoothing },
        }
    }
}

/// A running stack job and its latest live images.
struct Running {
    job: Job,
    settings: StackSettings,
    progress: Option<Progress>,
    frame: Option<TiledImage>,
    map: Option<TiledImage>,
    /// Full-resolution size of what the live images show.
    size: Vec2,
    stage: Stage,
}

/// A finished stack shown in Postprocessing.
struct Done {
    finished: Box<Finished>,
    /// The result image (moved out of `finished`) with its retouching history.
    retouch: Retouch,
    color: ColorInfo,
    image: TiledImage,
    map: TiledImage,
}

impl Done {
    /// Re-renders `rect` of the result into its texture after retouching.
    fn refresh(&mut self, rect: shroom_core::Rect) {
        let patch = render_display(&self.retouch.image().crop(rect), &self.color, None);
        self.image.update(rect.x, rect.y, &patch);
    }
}

/// Brush settings and the stroke in progress.
struct BrushTool {
    active: bool,
    brush: Brush,
    /// Aligned linear image of the frame painted from.
    source: Option<(PathBuf, Arc<RgbImage>)>,
    /// Position of the previous dab of the current stroke.
    last: Option<(f32, f32)>,
}

/// Questions that need a confirmation before results are discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    DiscardResult,
}

pub struct ShroomApp {
    workspace: Workspace,
    frames: FrameList,
    selected: Option<usize>,
    /// Embedded previews (about 1024 px) as textures, by file.
    thumbs: HashMap<PathBuf, TiledImage>,
    /// Full-resolution rendering of the selected frame (aligned to the result in
    /// Postprocessing).
    full: Option<(FullRequest, TiledImage)>,
    view: ViewState,
    params: Parameters,
    issues: Option<Vec<Issue>>,
    /// Outcome of the last action, shown in the toolbar (`true` = error).
    status: Option<(bool, String)>,
    running: Option<Running>,
    done: Option<Done>,
    confirm: Option<Confirm>,
    brush: BrushTool,
    /// Show the depth/source map instead of the result in Postprocessing.
    show_map: bool,
    /// Orientation of the stack: from the first frame's EXIF, unless set by the user.
    orientation: Orientation,
    orientation_manual: bool,
    saving: Option<Receiver<Result<PathBuf, String>>>,
    dialogs: Dialogs,
    loader: Loader,
}

impl ShroomApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            workspace: Workspace::Preparation,
            frames: FrameList::default(),
            selected: None,
            thumbs: HashMap::new(),
            full: None,
            view: ViewState::default(),
            params: Parameters::default(),
            issues: None,
            status: None,
            running: None,
            done: None,
            confirm: None,
            brush: BrushTool { active: false, brush: Brush { radius: 60.0, softness: 0.5 }, source: None, last: None },
            show_map: false,
            orientation: Orientation::Normal,
            orientation_manual: false,
            saving: None,
            dialogs: Dialogs::new(cc.egui_ctx.clone()),
            loader: Loader::new(cc.egui_ctx.clone()),
        };
        app.add_files(files);
        app
    }

    /// True when no background loading is pending (all file infos and the selected
    /// frame's full-resolution preview have arrived).
    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        !self.frames.is_loading()
            && self.running.is_none()
            && self.saving.is_none()
            && (self.selected.is_none()
                || self.full.as_ref().is_some_and(|(r, _)| Some(r) == self.full_request().as_ref()))
    }

    #[cfg(test)]
    pub fn view(&self) -> ViewState {
        self.view
    }

    #[cfg(test)]
    pub fn workspace(&self) -> Workspace {
        self.workspace
    }

    /// Whether the running job has delivered live images for both panes.
    #[cfg(test)]
    pub fn has_live_map(&self) -> bool {
        self.running.as_ref().is_some_and(|r| r.frame.is_some() && r.map.is_some())
    }

    /// Adds DNG files; folders are searched (one level deep).
    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let (found, skipped) = crate::frames::collect_raw_files(paths);
        let added = self.frames.add(found);
        self.status = match (added.is_empty(), skipped) {
            (true, 0) => None,
            (true, n) => Some((
                true,
                format!(
                    "No raw files added ({n} other files ignored; only raw formats like DNG or CR3 are supported)."
                ),
            )),
            (false, 0) => None,
            (false, n) => Some((false, format!("{n} files ignored: only raw formats like DNG or CR3 are supported."))),
        };
        for path in added {
            self.loader.request_info(path);
        }
        if self.selected.is_none() && !self.frames.frames.is_empty() {
            self.select(Some(0));
        }
        self.issues = None;
    }

    fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if let Some(request) = self.full_request()
            && self.full.as_ref().is_none_or(|(r, _)| *r != request)
        {
            self.loader.request_full(request);
        }
    }

    /// What the preview of the selected frame should show: the frame as is, or aligned
    /// to the result in Postprocessing.
    fn full_request(&self) -> Option<FullRequest> {
        let index = self.selected?;
        let path = self.frames.frames.get(index)?.path.clone();
        let aligned = match (&self.done, self.workspace) {
            (Some(done), Workspace::Postprocessing) => {
                let result = &done.finished.result;
                let i = done.finished.paths.iter().position(|p| *p == path)?;
                Some(match &result.alignment {
                    Some(a) => (a.transforms[i], a.crop),
                    None => (
                        shroom_core::align::Affine::IDENTITY,
                        shroom_core::Rect::new(0, 0, done.retouch.image().width(), done.retouch.image().height()),
                    ),
                })
            }
            _ => None,
        };
        Some(FullRequest { path, aligned })
    }

    fn set_workspace(&mut self, workspace: Workspace) {
        self.workspace = workspace;
        self.view = ViewState::default();
        self.select(self.selected);
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected.and_then(|i| self.frames.frames.get(i)).map(|f| f.path.clone())
    }

    fn receive(&mut self, ctx: &egui::Context) {
        for loaded in self.loader.poll() {
            match loaded {
                Loaded::Info(path, result) => {
                    if let Ok(info) = &result
                        && let Some(preview) = &info.preview
                    {
                        self.thumbs.insert(path.clone(), TiledImage::new(ctx, &path.to_string_lossy(), preview));
                    }
                    self.frames.set_info(&path, result);
                    // Keep the list in shooting order while files arrive.
                    let selected = self.selected_path();
                    self.frames.sort_by_capture_time();
                    self.selected = selected.and_then(|p| self.frames.frames.iter().position(|f| f.path == p));
                    self.update_auto_orientation();
                }
                Loaded::Full(request, Ok(img)) if Some(&request) == self.full_request().as_ref() => {
                    let name = format!("full:{}", request.path.display());
                    if let Some(linear) = img.linear {
                        self.brush.source = Some((request.path.clone(), linear));
                    }
                    self.full = Some((request, TiledImage::new(ctx, &name, &img.display)));
                }
                Loaded::Full(..) => {}
            }
        }
        self.receive_job(ctx);
        match self.dialogs.poll() {
            Some(Picked::Open(paths)) if self.workspace == Workspace::Preparation => self.add_files(paths),
            Some(Picked::Save(Some(path))) => self.save_to(path),
            _ => {}
        }
        if let Some(rx) = &self.saving
            && let Ok(result) = rx.try_recv()
        {
            self.status = Some(match result {
                Ok(path) => (false, format!("Saved {}", path.display())),
                Err(e) => (true, format!("Saving failed: {e}")),
            });
            self.saving = None;
        }
    }

    fn receive_job(&mut self, ctx: &egui::Context) {
        let Some(running) = &mut self.running else { return };
        let max_index = self.frames.frames.len().saturating_sub(1) as f32;
        let color = self.frames.frames.first().and_then(|f| f.info.as_ref()).map(|i| i.color.clone());
        for event in running.job.poll() {
            match event {
                JobEvent::Progress(p) => running.progress = Some(p),
                JobEvent::Snapshot(snap) => {
                    let color = color.clone().unwrap_or_default();
                    let frame = shroom_core::render::render_display(&snap.frame, &color, None);
                    running.frame = Some(TiledImage::new(ctx, "live:frame", &frame));
                    running.map =
                        snap.map.map(|m| TiledImage::new(ctx, "live:map", &render_depth_map(&m, max_index, None)));
                    running.size = Vec2::new(snap.full_size.0 as f32, snap.full_size.1 as f32);
                    if running.stage != snap.stage {
                        running.stage = snap.stage;
                        self.view = ViewState::default();
                    }
                }
                JobEvent::Done(outcome) => {
                    self.running = None;
                    match outcome {
                        Ok(mut finished) => {
                            let image = TiledImage::new(ctx, "result", &finished.display);
                            let result = std::mem::replace(&mut finished.result.image, RgbImage::new(0, 0));
                            let color =
                                self.frames.frames[0].info.as_ref().map(|i| i.color.clone()).unwrap_or_default();
                            let map = render_depth_map(&finished.result.depth_map, max_index, Some(2048));
                            let map = TiledImage::new(ctx, "result:map", &map);
                            self.status = Some((
                                false,
                                format!(
                                    "Stacked {} images in {:.1} s ({})",
                                    finished.paths.len(),
                                    finished.elapsed.as_secs_f32(),
                                    finished.result.backend
                                ),
                            ));
                            self.done = Some(Done { finished, retouch: Retouch::new(result), color, image, map });
                            self.brush.source = None;
                            self.set_workspace(Workspace::Postprocessing);
                        }
                        Err(JobError::Cancelled) => {
                            self.status = Some((false, "Stacking cancelled.".into()));
                            self.set_workspace(Workspace::Preparation);
                        }
                        Err(JobError::Failed(e)) => {
                            self.status = Some((true, format!("Stacking failed: {e}")));
                            self.set_workspace(Workspace::Preparation);
                        }
                    }
                    return;
                }
            }
        }
    }

    /// Runs the preflight checks and starts stacking if they pass.
    fn render(&mut self, ctx: &egui::Context) {
        let issues = preflight(&self.frames, crate::frames::total_memory());
        let ok = issues.iter().all(|i| i.severity != Severity::Error);
        self.issues = Some(issues);
        self.status = None;
        if !ok {
            return;
        }
        let settings = StackSettings { method: self.params.method(), align: self.params.align, use_gpu: true };
        let color = self.frames.frames[0].info.as_ref().map(|i| i.color.clone()).unwrap_or_default();
        let paths = self.frames.frames.iter().map(|f| f.path.clone()).collect();
        self.running = Some(Running {
            job: Job::start(ctx.clone(), paths, settings, color),
            settings,
            progress: None,
            frame: None,
            map: None,
            size: Vec2::ZERO,
            stage: Stage::Aligning,
        });
        self.set_workspace(Workspace::Processing);
    }

    fn save(&mut self) {
        let Some(done) = &self.done else { return };
        let first = &done.finished.paths[0];
        let name = format!("{}-stacked.dng", first.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default());
        let directory = first.parent().map(|d| d.to_path_buf());
        self.dialogs.save_dng(name, directory);
    }

    fn save_to(&mut self, path: PathBuf) {
        let Some(done) = &self.done else { return };
        let (image, template) =
            (done.retouch.image().clone(), done.finished.template.with_orientation(self.orientation));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let software = concat!("Shroom Stack ", env!("CARGO_PKG_VERSION"));
            let result = shroom_core::io::write_linear_dng(&path, &image, &template, software)
                .map(|()| path)
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.saving = Some(rx);
        self.status = Some((false, "Saving…".into()));
    }

    #[cfg(test)]
    pub fn save_for_test(&mut self, path: PathBuf) {
        self.save_to(path);
    }

    /// Follows the first frame's EXIF orientation unless the user rotated the stack.
    fn update_auto_orientation(&mut self) {
        if !self.orientation_manual {
            self.orientation =
                self.frames.frames.first().and_then(|f| f.info.as_ref()).map(|i| i.orientation).unwrap_or_default();
        }
    }

    fn rotate(&mut self, clockwise: bool) {
        self.orientation = if clockwise { self.orientation.rotated_cw() } else { self.orientation.rotated_ccw() };
        self.orientation_manual = true;
        self.view = ViewState::default();
    }

    /// "Rotate left/right" buttons.
    fn rotate_buttons(&mut self, ui: &mut egui::Ui) {
        if ui.button("Rotate left").on_hover_text("Rotate the stack 90° counter-clockwise (Cmd/Ctrl+[)").clicked() {
            self.rotate(false);
        }
        if ui.button("Rotate right").on_hover_text("Rotate the stack 90° clockwise (Cmd/Ctrl+])").clicked() {
            self.rotate(true);
        }
    }

    #[cfg(test)]
    pub fn orientation(&self) -> Orientation {
        self.orientation
    }

    fn discard_result(&mut self) {
        self.done = None;
        self.status = None;
        self.issues = None;
        self.set_workspace(Workspace::Preparation);
    }
}

impl eframe::App for ShroomApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive(&ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if !dropped.is_empty() && self.workspace == Workspace::Preparation {
            self.add_files(dropped);
        }
        // Rotation shortcuts as in Lightroom.
        if self.workspace != Workspace::Processing && self.confirm.is_none() && !ctx.egui_wants_keyboard_input() {
            let (left, right) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::OpenBracket),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::CloseBracket),
                )
            });
            if left {
                self.rotate(false);
            }
            if right {
                self.rotate(true);
            }
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("files")
            .resizable(true)
            .default_size(320.0)
            .min_size(220.0)
            .show(ui, |ui| self.file_list(ui));
        egui::CentralPanel::default().show(ui, |ui| self.preview(ui));
        self.confirm_dialog(&ctx);
    }
}

impl ShroomApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Shroom Stack");
            ui.separator();
            for (ws, label) in [
                (Workspace::Preparation, "Preparation"),
                (Workspace::Processing, "Processing"),
                (Workspace::Postprocessing, "Postprocessing"),
            ] {
                // Preparation is left only by rendering; Postprocessing needs a result.
                let enabled = match ws {
                    Workspace::Preparation => self.running.is_none(),
                    Workspace::Processing => self.running.is_some(),
                    Workspace::Postprocessing => self.done.is_some(),
                };
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.selectable_label(self.workspace == ws, label).clicked() && self.workspace != ws {
                        if ws == Workspace::Preparation && self.done.is_some() {
                            self.confirm = Some(Confirm::DiscardResult);
                        } else {
                            self.set_workspace(ws);
                        }
                    }
                });
            }
        });
        ui.add_space(2.0);
        match self.workspace {
            Workspace::Preparation => self.preparation_toolbar(ui),
            Workspace::Processing => self.processing_toolbar(ui),
            Workspace::Postprocessing => self.postprocessing_toolbar(ui),
        }
        if let Some((error, message)) = &self.status {
            ui.colored_label(if *error { ERROR_COLOR } else { Color32::LIGHT_GREEN }, message);
        }
        ui.add_space(4.0);
    }

    fn preparation_toolbar(&mut self, ui: &mut egui::Ui) {
        let mut render = false;
        ui.horizontal(|ui| {
            let method_label = ui.label("Method");
            let p = &mut self.params;
            egui::ComboBox::from_id_salt("method")
                .selected_text(match p.kind {
                    MethodKind::DepthMap => "Depth map",
                    MethodKind::Pyramid => "Pyramid",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.kind, MethodKind::DepthMap, "Depth map")
                        .on_hover_text("Takes each pixel from the sharpest frame. Best for few overlapping objects.");
                    ui.selectable_value(&mut p.kind, MethodKind::Pyramid, "Pyramid")
                        .on_hover_text("Combines detail at every scale. Best for complex, overlapping structures.");
                })
                .response
                .labelled_by(method_label.id);
            ui.separator();
            match p.kind {
                MethodKind::DepthMap => {
                    ui.label("Radius");
                    ui.add(egui::DragValue::new(&mut p.radius).range(1..=32))
                        .on_hover_text("Size of the area judged for sharpness. Larger is smoother but less precise.");
                    ui.label("Smoothing");
                    ui.add(egui::DragValue::new(&mut p.depth_smoothing).range(0..=20)).on_hover_text(
                        "How strongly the depth map is smoothed. Higher hides artefacts in smooth areas.",
                    );
                }
                MethodKind::Pyramid => {
                    ui.label("Smoothing");
                    ui.add(egui::DragValue::new(&mut p.pyramid_smoothing).range(0..=20))
                        .on_hover_text("Size of the area compared when picking detail.");
                }
            }
            ui.separator();
            ui.checkbox(&mut p.align, "Align frames")
                .on_hover_text("Compensates shifts and focus breathing between frames (recommended).");
            ui.separator();
            self.rotate_buttons(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                render = ui
                    .add(egui::Button::new(RichText::new("Render").strong()).min_size(Vec2::new(90.0, 0.0)))
                    .clicked();
            });
        });
        if render {
            self.render(ui.ctx());
        }
        if let Some(issues) = &self.issues {
            for issue in issues {
                let (kind, color) = match issue.severity {
                    Severity::Error => ("Error", ERROR_COLOR),
                    Severity::Warning => ("Warning", WARNING_COLOR),
                };
                ui.colored_label(color, format!("{kind}: {}", issue.message));
            }
        }
    }

    fn processing_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(running) = &self.running else { return };
        ui.horizontal(|ui| {
            let compositing = matches!(running.settings.method, Method::DepthMap { .. });
            let (fraction, text) = match &running.progress {
                Some(p) => overall_progress(p, running.settings.align, compositing),
                None => (0.0, "Starting…".into()),
            };
            let text = if running.job.is_cancelling() { "Cancelling…".into() } else { text };
            let cancel = ui.add_enabled(!running.job.is_cancelling(), egui::Button::new("Cancel"));
            ui.add(egui::ProgressBar::new(fraction).text(text).desired_width(ui.available_width()));
            if cancel.clicked() {
                running.job.cancel();
            }
        });
    }

    fn postprocessing_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(done) = &self.done else { return };
        let mut rotate = None;
        let mut save = false;
        let mut new_stack = false;
        ui.horizontal(|ui| {
            let s = &done.finished.settings;
            let method = match s.method {
                Method::DepthMap { radius, smoothing } => format!("Depth map (radius {radius}, smoothing {smoothing})"),
                Method::Pyramid { smoothing } => format!("Pyramid (smoothing {smoothing})"),
            };
            let img = done.retouch.image();
            let (w, h) = self.orientation.display_size(img.width() as f32, img.height() as f32);
            ui.label(format!("{method} · {} images · {w}×{h}", done.finished.paths.len()));
            ui.separator();
            ui.add_enabled(!self.brush.active, egui::Checkbox::new(&mut self.show_map, "Show depth map"))
                .on_hover_text("Which frame each part of the result comes from (blue = first, red = last).");
            ui.separator();
            if ui.button("Rotate left").on_hover_text("Rotate the result 90° counter-clockwise (Cmd/Ctrl+[)").clicked()
            {
                rotate = Some(false);
            }
            if ui.button("Rotate right").on_hover_text("Rotate the result 90° clockwise (Cmd/Ctrl+])").clicked() {
                rotate = Some(true);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                save = ui
                    .add_enabled(
                        self.saving.is_none() && !self.dialogs.is_open(),
                        egui::Button::new(RichText::new("Save…").strong()),
                    )
                    .on_hover_text("Save the result as Linear DNG")
                    .clicked();
                new_stack =
                    ui.button("New stack").on_hover_text("Back to Preparation (discards this result)").clicked();
            });
        });
        self.brush_toolbar(ui);
        if let Some(clockwise) = rotate {
            self.rotate(clockwise);
        }
        if save {
            self.save();
        }
        if new_stack {
            self.confirm = Some(Confirm::DiscardResult);
        }
    }

    fn brush_toolbar(&mut self, ui: &mut egui::Ui) {
        let Some(done) = &mut self.done else { return };
        let (mut undo, mut redo) = (false, false);
        ui.horizontal(|ui| {
            let tool = &mut self.brush;
            if ui
                .selectable_label(tool.active, "Brush")
                .on_hover_text("Paint the selected input frame into the result (B)")
                .clicked()
            {
                tool.active = !tool.active;
            }
            ui.add_enabled_ui(tool.active, |ui| {
                ui.label("Size");
                ui.add(egui::Slider::new(&mut tool.brush.radius, 2.0..=600.0).logarithmic(true).suffix(" px"))
                    .on_hover_text("Brush radius in image pixels ([ and ] change it)");
                ui.label("Softness");
                ui.add(egui::Slider::new(&mut tool.brush.softness, 0.0..=1.0))
                    .on_hover_text("0 = hard edge, 1 = soft fall-off");
            });
            ui.separator();
            undo = ui
                .add_enabled(done.retouch.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("Cmd/Ctrl+Z")
                .clicked();
            redo = ui
                .add_enabled(done.retouch.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("Shift+Cmd/Ctrl+Z")
                .clicked();
            if tool.active {
                let hint = match self.selected.and_then(|i| self.frames.frames.get(i)) {
                    None => "Select the frame to paint from in the list.".to_string(),
                    Some(f) if tool.source.as_ref().is_some_and(|(p, _)| *p == f.path) => {
                        format!("Painting from {}. Space+drag pans.", f.file_name())
                    }
                    Some(f) => format!("Loading {}…", f.file_name()),
                };
                ui.label(RichText::new(hint).weak());
            }
        });
        if self.brush.active {
            self.show_map = false;
        }
        // Keyboard shortcuts (Shift+Cmd+Z must be checked before Cmd+Z).
        let ctx = ui.ctx().clone();
        if self.confirm.is_none() && !ctx.egui_wants_keyboard_input() {
            redo |= ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z));
            undo |= ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z));
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::B) {
                    self.brush.active = !self.brush.active;
                }
                let r = &mut self.brush.brush.radius;
                if i.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket) {
                    *r = (*r / 1.25).max(2.0);
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket) {
                    *r = (*r * 1.25).min(600.0);
                }
            });
        }
        if undo && let Some(rect) = done.retouch.undo() {
            self.brush.last = None;
            done.refresh(rect);
        }
        if redo && let Some(rect) = done.retouch.redo() {
            done.refresh(rect);
        }
    }

    /// Applies brush input from the view to the result.
    fn paint(&mut self, response: viewer::ViewResponse) {
        let Some(done) = &mut self.done else { return };
        let tool = &mut self.brush;
        let selected = self.selected.and_then(|i| self.frames.frames.get(i)).map(|f| &f.path);
        let source = tool.source.as_ref().filter(|(p, _)| Some(p) == selected).map(|(_, s)| s.clone());
        match (tool.active, source, response.painting, response.hovered) {
            (true, Some(source), true, Some(pos)) => {
                let p = (pos.x, pos.y);
                let changed = match tool.last {
                    None => done.retouch.dab(&source, p.0, p.1, tool.brush),
                    Some(last) => done.retouch.line(&source, last, p, tool.brush),
                };
                tool.last = Some(p);
                if let Some(rect) = changed {
                    done.refresh(rect);
                }
            }
            _ => {
                if tool.last.take().is_some() {
                    done.retouch.end_stroke();
                }
            }
        }
    }

    /// Paints a stroke through the given image positions (for tests).
    #[cfg(test)]
    pub fn paint_for_test(&mut self, points: &[(f32, f32)]) -> bool {
        if self.brush.source.is_none() {
            return false;
        }
        self.brush.active = true;
        for &(x, y) in points {
            self.paint(viewer::ViewResponse { hovered: Some(Vec2::new(x, y)), painting: true });
        }
        self.paint(viewer::ViewResponse::default());
        true
    }

    #[cfg(test)]
    pub fn select_for_test(&mut self, index: usize) {
        self.select(Some(index));
    }

    #[cfg(test)]
    pub fn result_pixel(&self, x: usize, y: usize) -> [f32; 3] {
        self.done.as_ref().unwrap().retouch.image().pixel(x, y)
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(Confirm::DiscardResult) = self.confirm else { return };
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("confirm-discard")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Discard the result?");
            ui.label(
                "Going back to Preparation discards the stacked image and its retouching. Save it first if you \
                 want to keep it.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Discard").clicked() {
                    answer = Some(true);
                }
                if ui.button("Keep").clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(false);
        }
        match answer {
            Some(true) => {
                self.confirm = None;
                self.discard_result();
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn file_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let editable = self.workspace == Workspace::Preparation;
        let processing = self.running.as_ref().and_then(|r| r.progress.map(|p| p.frame));
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.dialogs.is_open(), egui::Button::new("Add images…")).clicked() {
                    self.dialogs.pick_raw_files();
                }
                if ui
                    .add_enabled(!self.frames.frames.is_empty(), egui::Button::new("Clear"))
                    .on_hover_text("Remove all images")
                    .clicked()
                {
                    self.frames.clear();
                    self.orientation_manual = false;
                    self.orientation = Orientation::Normal;
                    self.full = None;
                    self.issues = None;
                    self.status = None;
                    self.select(None);
                }
                let has = self.selected.is_some();
                if ui.add_enabled(has, egui::Button::new("Remove")).clicked() {
                    let i = self.selected.unwrap();
                    self.frames.remove(i);
                    self.issues = None;
                    let next =
                        if self.frames.frames.is_empty() { None } else { Some(i.min(self.frames.frames.len() - 1)) };
                    self.select(next);
                }
                if ui.add_enabled(has, egui::Button::new("Up")).on_hover_text("Move up").clicked() {
                    self.selected = Some(self.frames.shift(self.selected.unwrap(), -1));
                    self.issues = None;
                }
                if ui.add_enabled(has, egui::Button::new("Down")).on_hover_text("Move down").clicked() {
                    self.selected = Some(self.frames.shift(self.selected.unwrap(), 1));
                    self.issues = None;
                }
                if ui.button("Reverse").on_hover_text("Reverse the order").clicked() {
                    self.frames.reverse();
                    self.selected = self.selected.map(|i| self.frames.frames.len() - 1 - i);
                    self.issues = None;
                }
            })
        });
        ui.label(RichText::new(format!("{} images", self.frames.frames.len())).weak());
        ui.separator();

        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.frames.frames.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Add raw files (DNG, CR3, …) or drop files or a folder here.").weak())
                });
            }
            for (i, frame) in self.frames.frames.iter().enumerate() {
                let selected = match processing {
                    Some(current) => current == i,
                    None => self.selected == Some(i) && self.workspace != Workspace::Processing,
                };
                let row = egui::Frame::new()
                    .fill(if selected { ui.visuals().selection.bg_fill } else { Color32::TRANSPARENT })
                    .inner_margin(4.0)
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let thumb_size = Vec2::new(96.0, 64.0);
                            match self.thumbs.get(&frame.path).and_then(|t| t.single()) {
                                Some((tex, size)) => {
                                    let (w, h) = self.orientation.display_size(size.x, size.y);
                                    let scale = (thumb_size.x / w).min(thumb_size.y / h);
                                    let (rect, _) =
                                        ui.allocate_exact_size(Vec2::new(w, h) * scale, egui::Sense::hover());
                                    viewer::paint_rotated(ui.painter(), tex.id(), rect, self.orientation);
                                }
                                None => {
                                    ui.add_sized(thumb_size, egui::Spinner::new());
                                }
                            }
                            ui.vertical(|ui| {
                                ui.label(RichText::new(frame.file_name()).strong());
                                match (&frame.info, &frame.error) {
                                    (Some(info), _) => {
                                        let time =
                                            info.capture_time.map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string());
                                        ui.label(
                                            RichText::new(time.unwrap_or_else(|| "no capture time".into())).small(),
                                        );
                                        ui.label(
                                            RichText::new(format!("{} · {}×{}", info.camera, info.width, info.height))
                                                .small()
                                                .weak(),
                                        );
                                    }
                                    (None, Some(e)) => {
                                        ui.colored_label(Color32::from_rgb(255, 110, 110), RichText::new(e).small());
                                    }
                                    (None, None) => {
                                        ui.label(RichText::new("loading…").small().weak());
                                    }
                                }
                            });
                        });
                    });
                let response = row.response.interact(egui::Sense::click());
                if response.clicked() {
                    clicked = Some(i);
                }
            }
        });
        if self.workspace == Workspace::Processing {
            return;
        }
        if let Some(i) = clicked {
            self.select(Some(i));
        }
        // Keyboard navigation through the list.
        let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
        if self.confirm.is_none()
            && let Some(i) = self.selected
        {
            if up && i > 0 {
                self.select(Some(i - 1));
            } else if down && i + 1 < self.frames.frames.len() {
                self.select(Some(i + 1));
            }
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        match self.workspace {
            Workspace::Preparation => self.preparation_preview(ui),
            Workspace::Processing => self.processing_preview(ui),
            Workspace::Postprocessing => self.postprocessing_preview(ui),
        }
    }

    /// The selected frame's full-resolution texture, if loaded for the current workspace.
    fn selected_full(&self) -> Option<&TiledImage> {
        let request = self.full_request()?;
        self.full.as_ref().filter(|(r, _)| *r == request).map(|(_, t)| t)
    }

    fn preparation_preview(&mut self, ui: &mut egui::Ui) {
        let frame = self.selected.and_then(|i| self.frames.frames.get(i));
        let size = frame.and_then(|f| f.info.as_ref()).map(|i| Vec2::new(i.width as f32, i.height as f32));
        let full = self.selected_full();
        let thumb = frame.and_then(|f| self.thumbs.get(&f.path));
        let image = size.and_then(|s| full.or(thumb).map(|t| (t, s)));
        let label = match (frame, full.is_some()) {
            (Some(f), true) => f.file_name(),
            (Some(f), false) => format!("{} (loading full resolution…)", f.file_name()),
            (None, _) => String::new(),
        };
        let placeholder = if frame.is_some() { "Loading…" } else { "No image selected" };
        let mut view = self.view;
        viewer::show(ui, &mut view, &[Pane { label: &label, image, placeholder }], None, self.orientation);
        self.view = view;
    }

    fn processing_preview(&mut self, ui: &mut egui::Ui) {
        let Some(running) = &self.running else { return };
        let frame_label =
            running.progress.and_then(|p| self.frames.frames.get(p.frame)).map(|f| f.file_name()).unwrap_or_default();
        let map_label = match (running.stage, running.settings.method) {
            (Stage::Aligning, _) => "",
            (_, Method::DepthMap { .. }) => "Depth map (blue = first frame, red = last)",
            (_, Method::Pyramid { .. }) => "Detail source (blue = first frame, red = last)",
        };
        let size = running.size;
        let panes = [
            Pane { label: &frame_label, image: running.frame.as_ref().map(|t| (t, size)), placeholder: "Starting…" },
            Pane {
                label: map_label,
                image: running.map.as_ref().map(|t| (t, size)),
                placeholder: "The depth map appears after alignment.",
            },
        ];
        let mut view = self.view;
        viewer::show(ui, &mut view, &panes, None, self.orientation);
        self.view = view;
    }

    fn postprocessing_preview(&mut self, ui: &mut egui::Ui) {
        let Some(done) = &self.done else { return };
        let img = done.retouch.image();
        let size = Vec2::new(img.width() as f32, img.height() as f32);
        let frame = self.selected.and_then(|i| self.frames.frames.get(i));
        let left_label = frame.map(|f| format!("{} (aligned)", f.file_name())).unwrap_or_default();
        let left = self.selected_full();
        let panes = [
            Pane { label: &left_label, image: left.map(|t| (t, size)), placeholder: "Loading…" },
            if self.show_map {
                Pane { label: "Depth map", image: Some((&done.map, size)), placeholder: "" }
            } else {
                Pane { label: "Result", image: Some((&done.image, size)), placeholder: "" }
            },
        ];
        let brush = self.brush.active.then_some(self.brush.brush.radius);
        let mut view = self.view;
        let response = viewer::show(ui, &mut view, &panes, brush, self.orientation);
        self.view = view;
        self.paint(response);
    }
}
