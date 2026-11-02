//! The Session workspace: the photos of a folder grouped into stacks, reviewed and edited
//! by the user, and rendered in a batch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use eframe::egui::{self, Color32, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use shroom_core::DisplayImage;
use shroom_core::Orientation;
use shroom_core::io::{Exposure, FrameInfo};
use shroom_core::session::{self, ResultFile, SessionFile, Shot, Stack};
use shroom_core::stack::{Method, StackSettings};

use crate::batch::{Batch, BatchEvent};
use crate::loader::{FullImage, FullRequest, Loader};
use crate::processing::overall_progress;
use crate::viewer::{self, Pane, TiledImage, ViewState};

/// Smallest longer side of the thumbnail textures, in pixels: sharp up to the largest
/// thumbnail height on a high-density (2×) display.
const THUMB_SIZE: usize = 480;
/// Range of the thumbnail height in the stack rows, in points.
const THUMB_HEIGHT_MIN: f32 = 72.0;
const THUMB_HEIGHT_MAX: f32 = 160.0;
const WARNING_COLOR: Color32 = Color32::from_rgb(240, 200, 90);
const ERROR_COLOR: Color32 = Color32::from_rgb(255, 110, 110);
/// Background of the row of stacking results below a stack's photos.
const RESULT_FILL: Color32 = Color32::from_rgb(38, 62, 44);
/// How far the background reaches beyond the result thumbnails.
const RESULT_MARGIN: f32 = 6.0;
/// Space left and right of the stack list (the results' background reaches into it).
const PADDING: i8 = 12;
/// Gap between thumbnails, for photos and results alike.
const THUMB_GAP: f32 = 4.0;

/// What the user asked for that the app has to carry out.
pub enum Action {
    /// Load these frames into Preparation.
    OpenStack(Vec<PathBuf>),
}

/// A photo of the session.
struct Photo {
    shot: Shot,
    file_name: String,
    thumb: Option<TextureHandle>,
}

/// A stacking result in the folder.
struct ResultPhoto {
    file: ResultFile,
    file_name: String,
    orientation: Orientation,
    size: Vec2,
    thumb: Option<TextureHandle>,
}

/// One photo or result shown on its own, instead of the stack list.
struct Preview {
    path: PathBuf,
    file_name: String,
    /// Full-resolution size in stored orientation.
    size: Vec2,
    orientation: Orientation,
    /// The embedded preview, shown until the full resolution is decoded.
    quick: Option<TiledImage>,
    full: Option<TiledImage>,
    error: Option<String>,
    view: ViewState,
}

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Queued,
    Running(f32),
    Saved,
    Failed(String),
}

/// An edit of the grouping, applied after drawing.
enum Edit {
    Split(usize, usize),
    MergeWithPrevious(usize),
    MoveToPrevious(usize, usize),
    MoveToNext(usize, usize),
    ToggleExcluded(usize, usize),
    KeepBoundary(usize),
}

struct RunningBatch {
    batch: Batch,
    /// Stack index of each batch item.
    stacks: Vec<usize>,
    settings: StackSettings,
    finished: usize,
    current: f32,
}

#[derive(Default)]
pub struct SessionView {
    folder: Option<PathBuf>,
    file: Option<SessionFile>,
    /// Files whose information is still being read.
    pending: HashSet<PathBuf>,
    photos: HashMap<PathBuf, Photo>,
    /// Linear DNGs (stacking results) in the folder.
    results: HashMap<PathBuf, ResultPhoto>,
    /// Results saved during this session whose information is still being read.
    pending_results: HashSet<PathBuf>,
    unreadable: usize,
    stacks: Vec<Stack>,
    checked: Vec<bool>,
    status: HashMap<usize, Status>,
    selected: Option<PathBuf>,
    batch: Option<RunningBatch>,
    preview: Option<Preview>,
    /// Thumbnail height above [`THUMB_HEIGHT_MIN`], in points (so the default is the
    /// smallest size).
    thumb_extra: f32,
    /// Outcome of the last action (`true` = error).
    pub message: Option<(bool, String)>,
}

impl SessionView {
    /// Starts reading the raw files of `folder`; grouping follows once all are read.
    pub fn open(&mut self, folder: PathBuf, loader: &Loader) {
        if self.batch.is_some() {
            return;
        }
        let (files, _) = crate::frames::collect_raw_files([folder.clone()]);
        let (file, message) = match SessionFile::load(&folder) {
            Ok(file) => (file, None),
            Err(e) => (None, Some((true, format!("Session file ignored: {e}")))),
        };
        *self = Self {
            folder: Some(folder),
            file,
            message,
            pending: files.iter().cloned().collect(),
            thumb_extra: self.thumb_extra,
            ..Default::default()
        };
        if files.is_empty() {
            self.message = Some((true, "No raw files in this folder.".into()));
        }
        loader.request_infos(files);
    }

    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// Whether the information of `path` is awaited.
    pub fn wants(&self, path: &Path) -> bool {
        self.pending.contains(path)
            || self.pending_results.contains(path)
            || self.preview.as_ref().is_some_and(|p| p.path == path && p.quick.is_none())
    }

    /// Whether `request` is the full-resolution rendering the preview waits for.
    pub fn wants_full(&self, request: &FullRequest) -> bool {
        request.aligned.is_none() && self.preview.as_ref().is_some_and(|p| p.path == request.path && p.full.is_none())
    }

    pub fn set_full(&mut self, ctx: &egui::Context, request: &FullRequest, result: Result<FullImage, String>) {
        let Some(preview) = self.preview.as_mut().filter(|p| p.path == request.path) else { return };
        match result {
            Ok(img) => {
                let name = format!("session-full:{}", request.path.display());
                preview.full = Some(TiledImage::new(ctx, &name, &img.display));
            }
            Err(e) => preview.error = Some(e),
        }
    }

    /// Shows `path` (a photo or result of this session) on its own.
    fn open_preview(&mut self, path: &Path, loader: &Loader) {
        let (file_name, size, orientation) = if let Some(p) = self.photos.get(path) {
            let (w, h) = p.shot.size;
            (p.file_name.clone(), Vec2::new(w as f32, h as f32), p.shot.orientation)
        } else if let Some(r) = self.results.get(path) {
            (r.file_name.clone(), r.size, r.orientation)
        } else {
            return;
        };
        self.preview = Some(Preview {
            path: path.to_path_buf(),
            file_name,
            size,
            orientation,
            quick: None,
            full: None,
            error: None,
            view: ViewState::default(),
        });
        // The embedded preview comes with the file information, which is quick to read.
        loader.request_info(path.to_path_buf());
        loader.request_full(FullRequest { path: path.to_path_buf(), aligned: None });
    }

    #[cfg(test)]
    pub fn preview_path(&self) -> Option<&Path> {
        self.preview.as_ref().map(|p| p.path.as_path())
    }

    #[cfg(test)]
    pub fn has_full_preview(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.full.is_some())
    }

    pub fn is_loading(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether thumbnails of results saved now are still being read.
    #[cfg(test)]
    pub fn is_reading_results(&self) -> bool {
        !self.pending_results.is_empty()
    }

    pub fn is_batch_running(&self) -> bool {
        self.batch.is_some()
    }

    #[cfg(test)]
    pub fn stacks(&self) -> &[Stack] {
        &self.stacks
    }

    pub fn set_info(&mut self, ctx: &egui::Context, path: &Path, result: Result<FrameInfo, String>) {
        let thumb = |info: &FrameInfo| {
            info.preview.as_ref().map(|p| {
                let small = downscale(p, THUMB_SIZE);
                let image = egui::ColorImage::from_rgb([small.width, small.height], &small.data);
                ctx.load_texture(format!("session:{}", path.display()), image, TextureOptions::LINEAR)
            })
        };
        let result_photo = |info: FrameInfo| ResultPhoto {
            thumb: thumb(&info),
            size: Vec2::new(info.width as f32, info.height as f32),
            file_name: info.file_name,
            orientation: info.orientation,
            file: ResultFile { path: info.path, sources: info.sources },
        };
        if let Some(preview) = self.preview.as_mut().filter(|p| p.path == path && p.quick.is_none())
            && !self.pending.contains(path)
            && !self.pending_results.contains(path)
        {
            if let Ok(Some(img)) = result.as_ref().map(|info| info.preview.as_ref()) {
                preview.quick = Some(TiledImage::new(ctx, &format!("session-quick:{}", path.display()), img));
            }
            return;
        }
        if self.pending_results.remove(path) {
            // A result saved now is already attached to its stack.
            if let Ok(info) = result {
                self.results.insert(path.to_path_buf(), result_photo(info));
            }
            return;
        }
        if !self.pending.remove(path) {
            return;
        }
        match result {
            Ok(info) if !info.mosaic => {
                self.results.insert(path.to_path_buf(), result_photo(info));
            }
            Ok(info) => {
                let thumb = thumb(&info);
                let photo = Photo { shot: Shot::from_info(&info), file_name: info.file_name.clone(), thumb };
                self.photos.insert(path.to_path_buf(), photo);
            }
            Err(_) => self.unreadable += 1,
        }
        if self.pending.is_empty() {
            self.group();
        }
    }

    fn group(&mut self) {
        let Some(folder) = self.folder.clone() else { return };
        let mut shots: Vec<Shot> = self.photos.values().map(|p| p.shot.clone()).collect();
        shots.sort_by(|a, b| a.capture_time.cmp(&b.capture_time).then_with(|| a.path.cmp(&b.path)));
        self.stacks = session::assemble(&folder, &shots, self.file.as_ref());
        let mut results: Vec<ResultFile> = self.results.values().map(|r| r.file.clone()).collect();
        results.sort_by(|a, b| a.path.cmp(&b.path));
        let unmatched = session::attach_results(&mut self.stacks, &results).len();
        self.checked = vec![false; self.stacks.len()];
        let uncertain = self.stacks.iter().filter(|s| s.is_uncertain()).count();
        let mut text = format!("{} photos in {} stacks", shots.len(), self.stacks.len());
        if uncertain > 0 {
            text += &format!(", {uncertain} to check");
        }
        if unmatched > 0 {
            let plural = if unmatched == 1 { "" } else { "s" };
            text += &format!(" ({unmatched} other stacking result{plural})");
        }
        if self.unreadable > 0 {
            text += &format!(", {} files could not be read", self.unreadable);
        }
        if self.message.is_none() {
            self.message = Some((false, text));
        }
        self.save();
    }

    /// Writes the session file, so the grouping survives and the plugin can apply it.
    fn save(&mut self) {
        let Some(folder) = &self.folder else { return };
        if self.stacks.is_empty() {
            return;
        }
        if let Err(e) = session::to_file(folder, &self.stacks).save(folder) {
            self.message = Some((true, format!("Could not save the session: {e}")));
        }
    }

    /// Adds a result saved from `frames` to the stack holding most of them, if it is
    /// in this session's folder.
    pub fn add_result(&mut self, path: PathBuf, frames: &[PathBuf], loader: &Loader) {
        if self.folder.as_deref() != path.parent() || self.is_loading() {
            return;
        }
        let best = (0..self.stacks.len())
            .map(|i| (i, self.stacks[i].frames.iter().filter(|p| frames.contains(p)).count()))
            .filter(|&(_, n)| n > 0)
            .max_by_key(|&(i, n)| (n, std::cmp::Reverse(i)));
        if let Some((i, _)) = best {
            self.attach_saved(i, path, loader);
            self.save();
        }
    }

    /// Lists a result saved now for stack `i` and loads its thumbnail.
    fn attach_saved(&mut self, i: usize, path: PathBuf, loader: &Loader) {
        let results = &mut self.stacks[i].results;
        if !results.contains(&path) {
            results.push(path.clone());
            results.sort();
        }
        self.pending_results.insert(path.clone());
        loader.request_infos(vec![path]);
    }

    pub fn checked_count(&self) -> usize {
        self.checked.iter().filter(|c| **c).count()
    }

    /// Stacks the checked stacks one after another.
    pub fn start_batch(&mut self, ctx: &egui::Context, settings: StackSettings) {
        let stacks: Vec<usize> =
            (0..self.stacks.len()).filter(|&i| self.checked[i] && self.stacks[i].included().len() >= 2).collect();
        if stacks.is_empty() || self.batch.is_some() {
            return;
        }
        self.status = stacks.iter().map(|&i| (i, Status::Queued)).collect();
        let frames = stacks.iter().map(|&i| self.stacks[i].included()).collect();
        let batch = Batch::start(ctx.clone(), frames, settings);
        self.batch = Some(RunningBatch { batch, stacks, settings, finished: 0, current: 0.0 });
        self.message = None;
    }

    pub fn cancel_batch(&self) {
        if let Some(b) = &self.batch {
            b.batch.cancel();
        }
    }

    /// Overall progress of the batch, a description, and whether it is being cancelled.
    pub fn batch_progress(&self) -> Option<(f32, String, bool)> {
        let b = self.batch.as_ref()?;
        let total = b.stacks.len();
        let fraction = (b.finished as f32 + b.current) / total as f32;
        let text = format!("Stack {} of {total}", (b.finished + 1).min(total));
        Some((fraction, text, b.batch.is_cancelling()))
    }

    pub fn poll_batch(&mut self, loader: &Loader) {
        let Some(b) = &mut self.batch else { return };
        let mut done = false;
        let mut saved = Vec::new();
        for event in b.batch.poll() {
            match event {
                BatchEvent::Progress(k, p) => {
                    let compositing =
                        matches!(b.settings.method, Method::DepthMap { .. }) || b.settings.bokeh.is_some();
                    b.current = overall_progress(&p, b.settings.align, compositing).0;
                    self.status.insert(b.stacks[k], Status::Running(b.current));
                }
                BatchEvent::Finished(k, result) => {
                    let i = b.stacks[k];
                    b.finished += 1;
                    b.current = 0.0;
                    match result {
                        Ok(path) => {
                            saved.push((i, path));
                            self.status.insert(i, Status::Saved);
                        }
                        Err(e) => {
                            self.status.insert(i, Status::Failed(e));
                        }
                    }
                }
                BatchEvent::Done => done = true,
            }
        }
        for (i, path) in saved {
            self.attach_saved(i, path, loader);
        }
        if done {
            let b = self.batch.take().expect("running");
            let failed = self.status.values().filter(|s| matches!(s, Status::Failed(_))).count();
            let saved = self.status.values().filter(|s| **s == Status::Saved).count();
            self.status.retain(|_, s| !matches!(s, Status::Queued | Status::Running(_)));
            self.message = Some(match (b.batch.is_cancelling(), failed) {
                (true, _) => (false, format!("Batch cancelled after {saved} of {} stacks.", b.stacks.len())),
                (false, 0) => (false, format!("Saved {saved} results next to the originals.")),
                (false, n) => (true, format!("Saved {saved} results; {n} stacks failed.")),
            });
            self.save();
        }
    }

    /// The stack rows.
    pub fn show(&mut self, ui: &mut egui::Ui, loader: &Loader) -> Option<Action> {
        if self.folder.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Open a folder of photos (or drop it here) to group it into stacks.").weak());
            });
            return None;
        }
        if self.is_loading() {
            ui.centered_and_justified(|ui| {
                ui.label(format!("Reading {} files…", self.pending.len()));
            });
            return None;
        }
        if self.preview.is_some() {
            self.show_preview(ui);
            return None;
        }
        let (mut edit, mut action, mut open) = (None, None, None);
        let editable = self.batch.is_none();
        egui::Panel::bottom("session_list_bar").show(ui, |ui| self.list_bar(ui));
        // Room on both sides for the results' background, and on the right for the scroll bar.
        let scroll_bar = ui.spacing().scroll.bar_width;
        let padding = egui::Margin { left: PADDING, right: PADDING + scroll_bar as i8, top: 0, bottom: 0 };
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            egui::Frame::new().inner_margin(padding).show(ui, |ui| {
                for i in 0..self.stacks.len() {
                    ui.add_space(6.0);
                    self.stack_header(ui, i, editable, &mut edit, &mut action);
                    self.thumbnails(ui, i, editable, &mut edit, &mut open);
                    ui.add_space(4.0);
                    ui.separator();
                }
            });
        });
        if let Some(edit) = edit {
            self.apply(edit);
        }
        if let Some(path) = open {
            self.open_preview(&path, loader);
        }
        action
    }

    /// Controls below the stack list.
    fn list_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.set_min_height(28.0);
            let mut height = self.thumb_height();
            let label = ui.label("Thumbnail size");
            ui.add(egui::Slider::new(&mut height, THUMB_HEIGHT_MIN..=THUMB_HEIGHT_MAX).show_value(false))
                .labelled_by(label.id)
                .on_hover_text("Height of the thumbnails");
            self.thumb_extra = height - THUMB_HEIGHT_MIN;
        });
    }

    fn thumb_height(&self) -> f32 {
        (THUMB_HEIGHT_MIN + self.thumb_extra).clamp(THUMB_HEIGHT_MIN, THUMB_HEIGHT_MAX)
    }

    #[cfg(test)]
    pub fn thumb_height_for_test(&self) -> f32 {
        self.thumb_height()
    }

    /// The previewed file filling the workspace; Esc or the button in its corner goes
    /// back to the stack list.
    fn show_preview(&mut self, ui: &mut egui::Ui) {
        let Some(preview) = &mut self.preview else { return };
        // In full screen, Esc belongs to the view (it leaves full screen first).
        let mut back = preview.view.fullscreen.is_none()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let label = match (&preview.error, preview.full.is_some()) {
            (Some(e), _) => format!("{} (full resolution failed: {e})", preview.file_name),
            (None, false) => format!("{} (loading full resolution…)", preview.file_name),
            (None, true) => preview.file_name.clone(),
        };
        let image = preview.full.as_ref().or(preview.quick.as_ref()).map(|t| (t, preview.size));
        let placeholder = if preview.error.is_some() { "No preview" } else { "Loading…" };
        let pane = Pane { label: &label, image, placeholder, overlay: None };
        let area = ui.available_rect_before_wrap();
        viewer::show(ui, &mut preview.view, &[pane], None, preview.orientation, |_| {});
        // Added after the view, so it lies on top and takes the clicks.
        let size = Vec2::new(116.0, 24.0);
        let corner = egui::Rect::from_min_size(egui::pos2(area.max.x - size.x - 8.0, area.min.y + 8.0), size);
        back |= ui
            .put(corner, egui::Button::new("Back to stacks").fill(Color32::from_black_alpha(170)))
            .on_hover_text("Back to the stack list (Esc)")
            .clicked();
        if back {
            self.preview = None;
        }
    }

    fn stack_header(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        editable: bool,
        edit: &mut Option<Edit>,
        action: &mut Option<Action>,
    ) {
        let stack = self.stacks[i].clone();
        let stack = &stack;
        let included = stack.included().len();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(editable && included >= 2, |ui| {
                ui.checkbox(&mut self.checked[i], RichText::new(format!("Stack {}", i + 1)).strong())
                    .on_hover_text("Include in “Render selected”");
            });
            ui.label(self.describe(stack));
            if stack.is_uncertain() {
                let reasons =
                    if stack.reasons.is_empty() { "No clear reason".to_string() } else { stack.reasons.join("\n") };
                ui.colored_label(WARNING_COLOR, "Check: new stack?")
                    .on_hover_text(format!("Probably a new stack ({:.0} %):\n{reasons}", stack.boundary * 100.0));
                ui.add_enabled_ui(editable, |ui| {
                    if ui.small_button("Keep").on_hover_text("This is a new stack").clicked() {
                        *edit = Some(Edit::KeepBoundary(i));
                    }
                });
            }
            match self.status.get(&i) {
                Some(Status::Queued) => {
                    ui.label(RichText::new("Queued").weak());
                }
                Some(Status::Running(f)) => {
                    ui.add(egui::ProgressBar::new(*f).desired_width(120.0).show_percentage());
                }
                Some(Status::Saved) => {
                    ui.colored_label(Color32::LIGHT_GREEN, "Saved");
                }
                Some(Status::Failed(e)) => {
                    ui.colored_label(ERROR_COLOR, "Failed").on_hover_text(e);
                }
                None => {}
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    if ui
                        .add_enabled(included >= 2, egui::Button::new("Open"))
                        .on_hover_text("Stack it in Preparation")
                        .clicked()
                    {
                        *action = Some(Action::OpenStack(stack.included()));
                    }
                    if i > 0 && ui.button("Merge with previous").clicked() {
                        *edit = Some(Edit::MergeWithPrevious(i));
                    }
                });
            });
        });
    }

    /// Capture time, size and settings of a stack.
    fn describe(&self, stack: &Stack) -> String {
        let shots: Vec<&Shot> = stack.frames.iter().filter_map(|p| self.photos.get(p)).map(|p| &p.shot).collect();
        let mut parts = Vec::new();
        let times: Vec<NaiveDateTime> = shots.iter().filter_map(|s| s.capture_time).collect();
        if let (Some(first), Some(last)) = (times.first(), times.last()) {
            let seconds = (*last - *first).num_milliseconds() as f32 / 1000.0;
            parts.push(format!("{} · {seconds:.1} s", first.format("%Y-%m-%d %H:%M:%S")));
        }
        let excluded = stack.excluded.len();
        parts.push(match excluded {
            0 => format!("{} photos", stack.frames.len()),
            n => format!("{} photos, {n} excluded", stack.frames.len()),
        });
        if let Some(s) = shots.first() {
            parts.push(exposure_text(&s.exposure));
        }
        parts.join(" · ")
    }

    fn thumbnails(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        editable: bool,
        edit: &mut Option<Edit>,
        open: &mut Option<PathBuf>,
    ) {
        let stack = self.stacks[i].clone();
        let stack = &stack;
        let thumb_height = self.thumb_height();
        let last_stack = self.stacks.len() - 1;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(THUMB_GAP);
            for (j, path) in stack.frames.iter().enumerate() {
                let Some(photo) = self.photos.get(path) else { continue };
                let excluded = stack.excluded.contains(path);
                let response = match &photo.thumb {
                    Some(tex) => {
                        let orientation = photo.shot.orientation;
                        let (w, h) = orientation.display_size(tex.size_vec2().x, tex.size_vec2().y);
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(w, h) * (thumb_height / h), Sense::click());
                        crate::viewer::paint_rotated(ui.painter(), tex.id(), rect, orientation);
                        if excluded {
                            ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(170));
                        }
                        response
                    }
                    None => ui.add(
                        egui::Label::new(RichText::new(&photo.file_name).small())
                            .sense(Sense::click())
                            .selectable(false),
                    ),
                };
                let name = photo.file_name.clone();
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, editable, &name));
                if self.selected.as_ref() == Some(path) {
                    ui.painter().rect_stroke(
                        response.rect.expand(1.5),
                        2.0,
                        egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                        egui::StrokeKind::Outside,
                    );
                }
                if excluded {
                    let r = response.rect;
                    ui.painter().line_segment([r.left_top(), r.right_bottom()], egui::Stroke::new(2.0, ERROR_COLOR));
                }
                let mut hover = format!("{}\n{}", photo.file_name, exposure_text(&photo.shot.exposure));
                hover += "\nDouble-click to view";
                if let Some(t) = photo.shot.capture_time {
                    hover += &format!("\n{}", t.format("%Y-%m-%d %H:%M:%S%.3f"));
                }
                if excluded {
                    hover += "\nExcluded from stacking";
                }
                let response = response.on_hover_text(hover);
                if response.clicked() {
                    self.selected = Some(path.clone());
                }
                if response.double_clicked() {
                    *open = Some(path.clone());
                }
                if editable {
                    response.context_menu(|ui| {
                        self.selected = Some(path.clone());
                        if ui.add_enabled(j > 0, egui::Button::new("Start new stack here")).clicked() {
                            *edit = Some(Edit::Split(i, j));
                        }
                        if ui
                            .add_enabled(i > 0, egui::Button::new("Move to previous stack"))
                            .on_hover_text("Moves this photo and the ones before it")
                            .clicked()
                        {
                            *edit = Some(Edit::MoveToPrevious(i, j));
                        }
                        if ui
                            .add_enabled(i < last_stack, egui::Button::new("Move to next stack"))
                            .on_hover_text("Moves this photo and the ones after it")
                            .clicked()
                        {
                            *edit = Some(Edit::MoveToNext(i, j));
                        }
                        ui.separator();
                        let label = if excluded { "Include in stacking" } else { "Exclude from stacking" };
                        if ui.button(label).clicked() {
                            *edit = Some(Edit::ToggleExcluded(i, j));
                        }
                    });
                }
            }
        });
        if !stack.results.is_empty() {
            ui.add_space(THUMB_GAP + RESULT_MARGIN);
            self.result_thumbnails(ui, stack, open);
            ui.add_space(RESULT_MARGIN);
        }
    }

    /// The stack's results in a row of their own (wrapping like the photos), on a green
    /// background reaching a little beyond them, so they line up with the photos above.
    fn result_thumbnails(&self, ui: &mut egui::Ui, stack: &Stack, open: &mut Option<PathBuf>) {
        let thumb_height = self.thumb_height();
        let painter = ui.painter().clone();
        let background = painter.add(egui::Shape::Noop);
        let row = ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(THUMB_GAP);
            for path in &stack.results {
                let photo = self.results.get(path);
                let name = match photo {
                    Some(p) => p.file_name.clone(),
                    None => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                };
                let response = match photo.and_then(|p| p.thumb.as_ref().map(|t| (t, p.orientation))) {
                    Some((tex, orientation)) => {
                        let (w, h) = orientation.display_size(tex.size_vec2().x, tex.size_vec2().y);
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(w, h) * (thumb_height / h), Sense::click());
                        crate::viewer::paint_rotated(ui.painter(), tex.id(), rect, orientation);
                        response
                    }
                    None => {
                        ui.add(egui::Label::new(RichText::new(&name).small()).sense(Sense::click()).selectable(false))
                    }
                };
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
                if response.on_hover_text(format!("{name}\nStacking result")).double_clicked() {
                    *open = Some(path.clone());
                }
            }
        });
        let mut rect = row.response.rect;
        rect.max.x = ui.max_rect().max.x;
        painter.set(background, egui::Shape::rect_filled(rect.expand(RESULT_MARGIN), 4.0, RESULT_FILL));
    }

    fn apply(&mut self, edit: Edit) {
        let stacks = &mut self.stacks;
        // Keep the batch selection with its stacks: count how the edit shifts them.
        let before = stacks.len();
        let at = match edit {
            Edit::Split(i, j) => {
                session::split(stacks, i, j);
                i + 1
            }
            Edit::MergeWithPrevious(i) => {
                session::merge_with_previous(stacks, i);
                i
            }
            Edit::MoveToPrevious(i, j) => {
                session::move_to_previous(stacks, i, j);
                i
            }
            Edit::MoveToNext(i, j) => {
                session::move_to_next(stacks, i, j);
                i + 1
            }
            Edit::ToggleExcluded(i, j) => {
                let s = &mut stacks[i];
                let path = s.frames[j].clone();
                if !s.excluded.remove(&path) {
                    s.excluded.insert(path);
                }
                i
            }
            Edit::KeepBoundary(i) => {
                stacks[i].boundary = 1.0;
                i
            }
        };
        match stacks.len().cmp(&before) {
            std::cmp::Ordering::Greater => self.checked.insert(at, false),
            std::cmp::Ordering::Less => {
                self.checked.remove(at);
            }
            std::cmp::Ordering::Equal => {}
        }
        for (i, c) in self.checked.iter_mut().enumerate() {
            *c &= self.stacks[i].included().len() >= 2;
        }
        self.status.clear();
        self.save();
    }

    #[cfg(test)]
    pub fn edit_for_test(&mut self, split: (usize, usize)) {
        self.apply(Edit::Split(split.0, split.1));
    }

    #[cfg(test)]
    pub fn check_all_for_test(&mut self) {
        self.checked.iter_mut().for_each(|c| *c = true);
    }
}

fn exposure_text(e: &Exposure) -> String {
    let mut parts = Vec::new();
    if let Some(f) = e.f_number {
        parts.push(format!("f/{f}"));
    }
    if let Some(t) = e.time {
        parts.push(if t >= 0.3 { format!("{t} s") } else { format!("1/{:.0} s", 1.0 / t) });
    }
    if let Some(iso) = e.iso {
        parts.push(format!("ISO {iso}"));
    }
    if let Some(fl) = e.focal_length {
        parts.push(format!("{fl} mm"));
    }
    parts.join(" · ")
}

/// Box-filtered copy reduced by a whole factor, keeping the longer side at least `min`
/// pixels (or the original size if that is smaller).
fn downscale(img: &DisplayImage, min: usize) -> DisplayImage {
    let factor = (img.width.max(img.height) / min).max(1);
    let (w, h) = (img.width / factor, img.height / factor);
    let mut data = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut sum = 0u32;
                for sy in y * factor..(y + 1) * factor {
                    for sx in x * factor..(x + 1) * factor {
                        sum += img.data[(sy * img.width + sx) * 3 + c] as u32;
                    }
                }
                data.push((sum / (factor * factor) as u32) as u8);
            }
        }
    }
    DisplayImage { width: w, height: h, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_keeps_aspect_and_averages() {
        let img = DisplayImage { width: 1024, height: 683, data: vec![100; 1024 * 683 * 3] };
        let small = downscale(&img, 160);
        assert_eq!((small.width, small.height), (170, 113));
        assert!(small.data.iter().all(|&v| v == 100));
        assert_eq!(downscale(&img, 480).width, 512);
        assert_eq!(downscale(&img, 2000).width, 1024);
    }

    #[test]
    fn exposure_description() {
        let e = Exposure { time: Some(0.04), f_number: Some(6.3), iso: Some(1600), focal_length: Some(35.0) };
        assert_eq!(exposure_text(&e), "f/6.3 · 1/25 s · ISO 1600 · 35 mm");
    }
}
