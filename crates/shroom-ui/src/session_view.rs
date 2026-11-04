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
use shroom_core::stack::{BokehFrame, Method, StackSettings};

use crate::batch::{Batch, BatchEvent};
use crate::dng_converter::{self, Conversion};
use crate::loader::{FullImage, FullRequest, Loader};
use crate::processing::overall_progress;
use crate::toolbar::Icon;
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
    /// These files were moved to the trash.
    Deleted(Vec<PathBuf>),
}

/// A photo of the session.
struct Photo {
    shot: Shot,
    file_name: String,
    thumb: Option<TextureHandle>,
    /// Why the image data can't be decoded (listed from its embedded preview).
    unsupported: Option<String>,
}

/// A stacking result in the folder.
struct ResultPhoto {
    file: ResultFile,
    file_name: String,
    orientation: Orientation,
    size: Vec2,
    /// How it was made, if recorded in the file.
    settings: Option<StackSettings>,
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
    SetExcluded(usize, usize, bool),
    /// Moves the selected photos into a stack, or a new stack if `None`.
    MoveSelected(Option<usize>),
    SetSelectedExcluded(bool),
    KeepBoundary(usize),
}

/// A conversion to DNG of the photos of a stack.
struct RunningConversion {
    conversion: Conversion,
    /// The first converted photo, identifying the stack (indices change with edits).
    source: PathBuf,
    /// Files converted so far, of all.
    progress: (usize, usize),
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
    /// Stacking results (Linear DNGs, JPEGs recording their sources) in the folder.
    results: HashMap<PathBuf, ResultPhoto>,
    /// Results saved during this session whose information is still being read.
    pending_results: HashSet<PathBuf>,
    unreadable: usize,
    /// Why the first unreadable file could not be read.
    unreadable_reason: Option<String>,
    stacks: Vec<Stack>,
    checked: Vec<bool>,
    status: HashMap<usize, Status>,
    /// Selected photos and results.
    selected: HashSet<PathBuf>,
    /// Where a Shift-click range starts.
    anchor: Option<PathBuf>,
    /// Files to move to the trash once the user confirms.
    confirm_delete: Option<Vec<PathBuf>>,
    batch: Option<RunningBatch>,
    preview: Option<Preview>,
    conversion: Option<RunningConversion>,
    /// DNGs from a finished conversion whose information is still being read.
    pending_converted: HashSet<PathBuf>,
    /// Thumbnail height above [`THUMB_HEIGHT_MIN`], in points (so the default is the
    /// smallest size).
    thumb_extra: f32,
    /// Outcome of the last action (`true` = error).
    pub message: Option<(bool, String)>,
}

impl SessionView {
    /// Starts reading the raw files and JPEGs of `folder`; grouping follows once all are read.
    pub fn open(&mut self, folder: PathBuf, loader: &Loader) {
        if self.batch.is_some() || self.conversion.is_some() {
            return;
        }
        let (files, _) = crate::frames::collect_input_files([folder.clone()]);
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
            self.message = Some((true, "No raw files or JPEGs in this folder.".into()));
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
            || self.pending_converted.contains(path)
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
    fn open_preview(&mut self, path: &Path, view: ViewState, loader: &Loader) {
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
            view,
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
            settings: info.settings,
            file_name: info.file_name,
            orientation: info.orientation,
            file: ResultFile { path: info.path, sources: info.sources },
        };
        let photo = |info: &FrameInfo| Photo {
            shot: Shot::from_info(info),
            file_name: info.file_name.clone(),
            thumb: thumb(info),
            unsupported: info.unsupported.clone(),
        };
        if self.pending_converted.remove(path) {
            // Already in the stack created for the conversion.
            match result {
                Ok(info) => {
                    self.photos.insert(path.to_path_buf(), photo(&info));
                }
                Err(e) => self.message = Some((true, e)),
            }
            return;
        }
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
            Ok(info) if info.is_result() => {
                self.results.insert(path.to_path_buf(), result_photo(info));
            }
            Ok(info) => {
                self.photos.insert(path.to_path_buf(), photo(&info));
            }
            Err(e) => {
                self.unreadable += 1;
                // The error names the file; keep only the reason.
                let reason = e.split_once(&format!("{}: ", path.display())).map_or(e.as_str(), |(_, r)| r);
                self.unreadable_reason.get_or_insert_with(|| reason.to_string());
            }
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
        let stacks =
            if self.stacks.len() == 1 { "1 stack".to_string() } else { format!("{} stacks", self.stacks.len()) };
        let mut text = format!("{} in {stacks}", photo_count(shots.len()));
        if uncertain > 0 {
            text += &format!(", {uncertain} to check");
        }
        if unmatched > 0 {
            let plural = if unmatched == 1 { "" } else { "s" };
            text += &format!(" ({unmatched} other stacking result{plural})");
        }
        let unsupported: Vec<&str> = self.photos.values().filter_map(|p| p.unsupported.as_deref()).collect();
        if let Some(reason) = unsupported.first() {
            // The first sentence; the thumbnails' tooltips have the whole reason.
            let short = reason.split_once(". ").map_or(*reason, |(s, _)| s);
            text += &format!(", {} can't be stacked: {short}", unsupported.len());
        }
        if self.unreadable > 0 {
            text += &format!(", {} files could not be read", self.unreadable);
            if let Some(reason) = &self.unreadable_reason {
                text += &format!(": {reason}");
            }
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

    /// Converts the stack's photos that can't be decoded to DNGs with Adobe DNG Converter;
    /// the DNGs become a new stack after it.
    fn start_conversion(&mut self, ctx: &egui::Context, i: usize) {
        let Some(converter) = dng_converter::find() else { return };
        let files: Vec<PathBuf> = self.stacks[i]
            .included()
            .into_iter()
            .filter(|p| self.photos.get(p).is_some_and(|p| p.unsupported.is_some()))
            .collect();
        if files.is_empty() || self.conversion.is_some() {
            return;
        }
        let progress = (0, files.len());
        let source = files[0].clone();
        let conversion = Conversion::start(ctx.clone(), converter, files);
        self.conversion = Some(RunningConversion { conversion, source, progress });
        self.message = None;
    }

    pub fn is_converting(&self) -> bool {
        self.conversion.is_some()
    }

    #[cfg(test)]
    pub fn is_reading_converted(&self) -> bool {
        !self.pending_converted.is_empty()
    }

    pub fn poll_conversion(&mut self, loader: &Loader) {
        let Some(c) = &mut self.conversion else { return };
        let mut finished = None;
        for event in c.conversion.poll() {
            match event {
                dng_converter::Event::Progress(done, total) => c.progress = (done, total),
                dng_converter::Event::Finished(result) => finished = Some(result),
            }
        }
        let Some(result) = finished else { return };
        let source = self.conversion.take().expect("running").source;
        match result {
            Ok(dngs) => {
                self.message = Some((false, format!("Converted {} to DNG.", photo_count(dngs.len()))));
                self.add_converted_stack(&source, dngs, loader);
            }
            Err(e) => self.message = Some((true, e)),
        }
    }

    /// Puts `dngs` into a new stack after the one holding `source` (taking them out of any
    /// other stack, e.g. from an earlier conversion) and loads their thumbnails.
    fn add_converted_stack(&mut self, source: &Path, dngs: Vec<PathBuf>, loader: &Loader) {
        for s in &mut self.stacks {
            s.frames.retain(|p| !dngs.contains(p));
            for p in &dngs {
                s.excluded.remove(p);
            }
        }
        self.stacks.retain(|s| !s.frames.is_empty());
        let at =
            self.stacks.iter().position(|s| s.frames.iter().any(|p| p == source)).map_or(self.stacks.len(), |i| i + 1);
        let new: Vec<PathBuf> = dngs.iter().filter(|p| !self.photos.contains_key(*p)).cloned().collect();
        self.stacks.insert(at, Stack { frames: dngs, boundary: 1.0, ..Default::default() });
        self.checked = vec![false; self.stacks.len()];
        self.status.clear();
        self.pending_converted.extend(new.iter().cloned());
        if !new.is_empty() {
            loader.request_infos(new);
        }
        self.save();
    }

    pub fn checked_count(&self) -> usize {
        self.checked.iter().filter(|c| **c).count()
    }

    /// Stacks the checked stacks one after another.
    /// "Convert to DNG" for a stack with photos that can't be decoded, or the progress of
    /// its conversion.
    fn conversion_controls(&mut self, ui: &mut egui::Ui, i: usize, editable: bool) {
        let stack = &self.stacks[i];
        if let Some(c) = &self.conversion
            && stack.frames.contains(&c.source)
        {
            if ui.add_enabled(!c.conversion.is_cancelling(), egui::Button::new("Cancel")).clicked() {
                c.conversion.cancel();
            }
            let (done, total) = c.progress;
            let text = format!("Converting {done} of {total}");
            ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).text(text).desired_width(160.0));
            return;
        }
        let converter = dng_converter::find();
        if converter.is_none() && !dng_converter::AVAILABLE_ON_PLATFORM {
            return;
        }
        let enabled = editable && converter.is_some() && self.conversion.is_none();
        let button = ui
            .add_enabled(enabled, egui::Button::new("Convert to DNG"))
            .on_hover_text("Convert these photos with Adobe DNG Converter; the DNGs become a new stack")
            .on_disabled_hover_text(match converter {
                None => "Install Adobe DNG Converter (free from Adobe) to convert these photos.",
                Some(_) => "Another conversion or a batch is running.",
            });
        if button.clicked() {
            self.start_conversion(ui.ctx(), i);
        }
    }

    /// Why a stack can't be stacked: the reason of the first included photo whose image
    /// data can't be decoded.
    fn blocked(&self, stack: &Stack) -> Option<&str> {
        stack.included().iter().find_map(|p| self.photos.get(p)?.unsupported.as_deref())
    }

    pub fn start_batch(&mut self, ctx: &egui::Context, settings: StackSettings, format: shroom_core::io::ResultFormat) {
        let stacks: Vec<usize> = (0..self.stacks.len())
            .filter(|&i| {
                let s = &self.stacks[i];
                self.checked[i] && s.included().len() >= 2 && self.blocked(s).is_none()
            })
            .collect();
        if stacks.is_empty() || self.batch.is_some() {
            return;
        }
        self.status = stacks.iter().map(|&i| (i, Status::Queued)).collect();
        let frames = stacks.iter().map(|&i| self.stacks[i].included()).collect();
        let batch = Batch::start(ctx.clone(), frames, settings, format);
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
            self.show_preview(ui, loader);
            return None;
        }
        let (mut edit, mut action, mut open) = (None, None, None);
        let editable = self.batch.is_none();
        // Delete (or Backspace) moves the selected files to the trash, after asking.
        let keys = editable && self.confirm_delete.is_none() && !ui.ctx().egui_wants_keyboard_input();
        if keys
            && !self.selected.is_empty()
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    | i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
            })
        {
            self.confirm_delete = Some(self.selected_files());
        }
        // Esc clears the selection, unless it closes a menu or the question.
        if self.confirm_delete.is_none()
            && !egui::Popup::is_any_open(ui.ctx())
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.selected.clear();
        }
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
            self.open_preview(&path, ViewState::default(), loader);
        }
        if let Some(deleted) = self.confirm_delete_dialog(ui.ctx()) {
            action = Some(Action::Deleted(deleted));
        }
        action
    }

    /// Asks before moving files to the trash; returns the files once moved.
    fn confirm_delete_dialog(&mut self, ctx: &egui::Context) -> Option<Vec<PathBuf>> {
        let paths = self.confirm_delete.clone()?;
        let results = paths.iter().filter(|p| self.results.contains_key(*p)).count();
        let photos = paths.len() - results;
        let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let what = match (photos, results) {
            (_, 0) => count(photos, "photo"),
            (0, _) => count(results, "stacking result"),
            _ => format!("{} and {}", count(photos, "photo"), count(results, "stacking result")),
        };
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Move to Trash?");
            ui.label(format!("{what} will be moved to the Trash. You can restore them from there."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let trash = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE))
                    .fill(crate::toolbar::DANGER_COLOR);
                if ui.add(trash).clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(false);
        }
        match answer? {
            false => {
                self.confirm_delete = None;
                None
            }
            true => {
                self.confirm_delete = None;
                match move_to_trash(&paths) {
                    Ok(()) => {
                        self.forget(&paths);
                        self.message = Some((false, format!("Moved {what} to the Trash.")));
                        Some(paths)
                    }
                    Err(e) => {
                        self.message = Some((true, format!("Could not move to the Trash: {e}")));
                        None
                    }
                }
            }
        }
    }

    /// Removes deleted files from the stacks; stacks without photos left are removed.
    fn forget(&mut self, paths: &[PathBuf]) {
        for s in &mut self.stacks {
            s.frames.retain(|p| !paths.contains(p));
            s.results.retain(|p| !paths.contains(p));
            for p in paths {
                s.excluded.remove(p);
            }
        }
        self.stacks.retain(|s| !s.frames.is_empty());
        for p in paths {
            self.photos.remove(p);
            self.results.remove(p);
            self.selected.remove(p);
        }
        if self.preview.as_ref().is_some_and(|p| paths.contains(&p.path)) {
            self.preview = None;
        }
        self.checked = vec![false; self.stacks.len()];
        self.status.clear();
        self.save();
    }

    /// The selected photos and results, in list order.
    fn selected_files(&self) -> Vec<PathBuf> {
        self.preview_order().into_iter().filter(|p| self.selected.contains(p)).collect()
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
    fn show_preview(&mut self, ui: &mut egui::Ui, loader: &Loader) {
        let Some(preview) = &self.preview else { return };
        let path = preview.path.clone();
        // In full screen, Esc belongs to the view (it leaves full screen first).
        let fullscreen = preview.view.fullscreen.is_some();
        let keys = !ui.ctx().egui_wants_keyboard_input();
        let (mut back, step, include) = ui.input_mut(|i| {
            let mut key = |k| keys && i.consume_key(egui::Modifiers::NONE, k);
            let back = !fullscreen && key(egui::Key::Escape);
            let step = key(egui::Key::ArrowRight) as i32 - key(egui::Key::ArrowLeft) as i32;
            let include = match (key(egui::Key::P), key(egui::Key::X)) {
                (true, false) => Some(true),
                (false, true) => Some(false),
                _ => None,
            };
            (back, step, include)
        });
        let frame = self.frame_position(&path);
        if let (Some(include), Some((i, j)), true) = (include, frame, self.batch.is_none()) {
            self.apply(Edit::SetExcluded(i, j, !include));
        }
        let excluded = frame.is_some_and(|(i, _)| self.stacks[i].excluded.contains(&path));

        let Some(preview) = &mut self.preview else { return };
        let mut label = match (&preview.error, preview.full.is_some()) {
            (Some(e), _) => format!("{} (full resolution failed: {e})", preview.file_name),
            (None, false) => format!("{} (loading full resolution…)", preview.file_name),
            (None, true) => preview.file_name.clone(),
        };
        if excluded {
            label += " · excluded from stacking";
        }
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
            .on_hover_text("Back to the stack list (Esc)\n← →: previous / next file\nP / X: include / exclude")
            .clicked();
        if back {
            self.preview = None;
        } else if step != 0 {
            let view = preview.view;
            let order = self.preview_order();
            let next = order
                .iter()
                .position(|p| *p == path)
                .and_then(|k| k.checked_add_signed(step as isize))
                .and_then(|k| order.get(k));
            if let Some(next) = next {
                // Same zoom and position, to compare a detail across files.
                self.open_preview(&next.clone(), view, loader);
            }
        }
    }

    /// Stack and frame index of an input photo.
    fn frame_position(&self, path: &Path) -> Option<(usize, usize)> {
        self.stacks.iter().enumerate().find_map(|(i, s)| s.frames.iter().position(|p| p == path).map(|j| (i, j)))
    }

    /// The files the preview steps through: each stack's photos, then its results.
    fn preview_order(&self) -> Vec<PathBuf> {
        self.stacks
            .iter()
            .flat_map(|s| {
                let photos = s.frames.iter().filter(|p| self.photos.contains_key(*p));
                photos.chain(s.results.iter().filter(|p| self.results.contains_key(*p)))
            })
            .cloned()
            .collect()
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
        let blocked = self.blocked(stack).map(|r| format!("Contains photos that can't be stacked: {r}"));
        let stackable = included >= 2 && blocked.is_none();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(editable && stackable, |ui| {
                let checkbox = ui
                    .checkbox(&mut self.checked[i], RichText::new(format!("Stack {}", i + 1)).strong())
                    .on_hover_text("Include in “Render selected”");
                if let Some(reason) = &blocked {
                    checkbox.on_disabled_hover_text(reason);
                }
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
                        .add_enabled(stackable, egui::Button::new("Open"))
                        .on_hover_text("Stack it in Preparation")
                        .on_disabled_hover_text(blocked.as_deref().unwrap_or("Needs at least two photos"))
                        .clicked()
                    {
                        *action = Some(Action::OpenStack(stack.included()));
                    }
                    if i > 0 && ui.button("Merge with previous").clicked() {
                        *edit = Some(Edit::MergeWithPrevious(i));
                    }
                    let name = format!("Delete stack {}", i + 1);
                    if crate::toolbar::danger_button(
                        ui,
                        Icon::Trash,
                        &name,
                        "Move all photos and results of this stack to the Trash",
                    )
                    .clicked()
                    {
                        self.confirm_delete = Some(stack.frames.iter().chain(&stack.results).cloned().collect());
                    }
                });
                if blocked.is_some() {
                    self.conversion_controls(ui, i, editable);
                }
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
            0 => photo_count(stack.frames.len()),
            n => format!("{}, {n} excluded", photo_count(stack.frames.len())),
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
                if self.selected.contains(path) {
                    paint_selection(ui, response.rect);
                }
                if excluded {
                    let r = response.rect;
                    ui.painter().line_segment([r.left_top(), r.right_bottom()], egui::Stroke::new(2.0, ERROR_COLOR));
                }
                if photo.unsupported.is_some() {
                    ui.painter().rect_filled(response.rect, 0.0, Color32::from_black_alpha(140));
                    paint_badge(ui.painter(), response.rect, crate::toolbar::Icon::Blocked, ERROR_COLOR);
                }
                let mut hover = format!("{}\n{}", photo.file_name, exposure_text(&photo.shot.exposure));
                if let Some(reason) = &photo.unsupported {
                    hover += &format!("\nCan't be stacked: {reason}");
                }
                hover += "\nDouble-click to view";
                if let Some(t) = photo.shot.capture_time {
                    hover += &format!("\n{}", t.format("%Y-%m-%d %H:%M:%S%.3f"));
                }
                if excluded {
                    hover += "\nExcluded from stacking";
                }
                let response = response.on_hover_text(hover);
                if response.clicked() {
                    let modifiers = ui.input(|i| i.modifiers);
                    self.click(path, modifiers);
                }
                if response.double_clicked() {
                    *open = Some(path.clone());
                }
                if editable {
                    response.context_menu(|ui| {
                        if !self.selected.contains(path) {
                            self.selected = HashSet::from([path.clone()]);
                            self.anchor = Some(path.clone());
                        }
                        let photos = self.selected_photos().len();
                        if photos > 1 {
                            ui.menu_button(format!("Move {photos} photos to"), |ui| {
                                for k in 0..=last_stack {
                                    if ui.button(format!("Stack {}", k + 1)).clicked() {
                                        *edit = Some(Edit::MoveSelected(Some(k)));
                                    }
                                }
                                ui.separator();
                                if ui.button("New stack").clicked() {
                                    *edit = Some(Edit::MoveSelected(None));
                                }
                            });
                            ui.separator();
                            if ui.button(format!("Exclude {photos} photos from stacking")).clicked() {
                                *edit = Some(Edit::SetSelectedExcluded(true));
                            }
                            if ui.button(format!("Include {photos} photos in stacking")).clicked() {
                                *edit = Some(Edit::SetSelectedExcluded(false));
                            }
                            ui.separator();
                            if ui.button(format!("Move {} files to Trash…", self.selected.len())).clicked() {
                                self.confirm_delete = Some(self.selected_files());
                            }
                            return;
                        }
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
                            *edit = Some(Edit::SetExcluded(i, j, !excluded));
                        }
                        ui.separator();
                        if ui.button("Move to Trash…").clicked() {
                            self.confirm_delete = Some(self.selected_files());
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
    fn result_thumbnails(&mut self, ui: &mut egui::Ui, stack: &Stack, open: &mut Option<PathBuf>) {
        let thumb_height = self.thumb_height();
        let editable = self.batch.is_none();
        let mut clicked = None;
        let mut trash = None;
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
                let settings = photo.and_then(|p| p.settings);
                if let Some(s) = &settings {
                    paint_method_badge(ui.painter(), response.rect, s.method);
                }
                if self.selected.contains(path) {
                    paint_selection(ui, response.rect);
                }
                if response.clicked() {
                    clicked = Some((path.clone(), ui.input(|i| i.modifiers)));
                }
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
                let details = settings.as_ref().map(settings_text).unwrap_or_else(|| "Settings not recorded".into());
                let response = response.on_hover_text(format!("{name}\nStacking result\n{details}"));
                if response.double_clicked() {
                    *open = Some(path.clone());
                }
                if editable {
                    response.context_menu(|ui| {
                        if ui.button("Move to Trash…").clicked() {
                            trash = Some(path.clone());
                        }
                    });
                }
            }
        });
        let mut rect = row.response.rect;
        rect.max.x = ui.max_rect().max.x;
        painter.set(background, egui::Shape::rect_filled(rect.expand(RESULT_MARGIN), 4.0, RESULT_FILL));
        if let Some((path, modifiers)) = clicked {
            self.click(&path, modifiers);
        }
        if let Some(path) = trash {
            if !self.selected.contains(&path) {
                self.selected = HashSet::from([path.clone()]);
                self.anchor = Some(path);
            }
            self.confirm_delete = Some(self.selected_files());
        }
    }

    /// Selects on a click: only this file; with Cmd/Ctrl adds or removes it; with Shift
    /// selects the files from the last clicked one to this, in list order.
    fn click(&mut self, path: &Path, modifiers: egui::Modifiers) {
        if modifiers.shift
            && let Some(anchor) = &self.anchor
        {
            let order = self.preview_order();
            let (a, b) = (order.iter().position(|p| p == anchor), order.iter().position(|p| p == path));
            if let (Some(a), Some(b)) = (a, b) {
                self.selected.extend(order[a.min(b)..=a.max(b)].iter().cloned());
                return;
            }
        }
        if modifiers.command {
            if !self.selected.remove(path) {
                self.selected.insert(path.to_path_buf());
            }
        } else {
            self.selected = HashSet::from([path.to_path_buf()]);
        }
        self.anchor = Some(path.to_path_buf());
    }

    /// The selected input photos, in list order.
    fn selected_photos(&self) -> Vec<PathBuf> {
        self.stacks.iter().flat_map(|s| &s.frames).filter(|p| self.selected.contains(*p)).cloned().collect()
    }

    #[cfg(test)]
    pub fn selected_for_test(&self) -> Vec<PathBuf> {
        self.preview_order().into_iter().filter(|p| self.selected.contains(p)).collect()
    }

    #[cfg(test)]
    pub fn move_selected_for_test(&mut self, to: Option<usize>) {
        self.apply(Edit::MoveSelected(to));
    }

    fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::MoveSelected(to) => {
                let photos = self.selected_photos();
                session::move_frames(&mut self.stacks, &photos, to);
                // Stacks may have been added, removed or reordered.
                self.checked = vec![false; self.stacks.len()];
                self.status.clear();
                self.save();
                return;
            }
            Edit::SetSelectedExcluded(excluded) => {
                let photos = self.selected_photos();
                for s in &mut self.stacks {
                    for p in s.frames.iter().filter(|p| photos.contains(p)) {
                        if excluded {
                            s.excluded.insert(p.clone());
                        } else {
                            s.excluded.remove(p);
                        }
                    }
                }
                for (i, c) in self.checked.iter_mut().enumerate() {
                    *c &= self.stacks[i].included().len() >= 2;
                }
                self.save();
                return;
            }
            _ => {}
        }
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
            Edit::SetExcluded(i, j, excluded) => {
                let s = &mut stacks[i];
                let path = s.frames[j].clone();
                if excluded {
                    s.excluded.insert(path);
                } else {
                    s.excluded.remove(&path);
                }
                i
            }
            Edit::KeepBoundary(i) => {
                stacks[i].boundary = 1.0;
                i
            }
            Edit::MoveSelected(_) | Edit::SetSelectedExcluded(_) => unreachable!("handled above"),
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

fn photo_count(n: usize) -> String {
    if n == 1 { "1 photo".into() } else { format!("{n} photos") }
}

/// Moves files to the system trash (in tests: deletes the temporary test files).
fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    #[cfg(not(test))]
    return trash::delete_all(paths).map_err(|e| e.to_string());
    #[cfg(test)]
    paths.iter().try_for_each(std::fs::remove_file).map_err(|e| e.to_string())
}

/// The outline marking a selected thumbnail.
fn paint_selection(ui: &egui::Ui, rect: egui::Rect) {
    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
    ui.painter().rect_stroke(rect.expand(1.5), 2.0, stroke, egui::StrokeKind::Outside);
}

/// The stacking method as a small icon in the top left corner of a result thumbnail.
fn paint_method_badge(painter: &egui::Painter, thumb: egui::Rect, method: Method) {
    let icon = match method {
        Method::DepthMap { .. } => crate::toolbar::Icon::DepthMap,
        Method::Pyramid { .. } => crate::toolbar::Icon::Pyramid,
    };
    paint_badge(painter, thumb, icon, Color32::from_gray(230));
}

/// A small icon on a dark square in the top left corner of a thumbnail.
fn paint_badge(painter: &egui::Painter, thumb: egui::Rect, icon: crate::toolbar::Icon, color: Color32) {
    let badge = egui::Rect::from_min_size(thumb.min + Vec2::splat(4.0), Vec2::splat(20.0));
    let fill = Color32::from_gray(28);
    painter.rect_filled(badge, 4.0, fill);
    crate::toolbar::paint_icon(painter, icon, badge.shrink(3.0), color, fill);
}

/// The settings of a result, one aspect per line.
fn settings_text(s: &StackSettings) -> String {
    let mut lines = vec![match s.method {
        Method::DepthMap { radius, smoothing } => format!("Depth map: radius {radius}, smoothing {smoothing}"),
        Method::Pyramid { smoothing } => format!("Pyramid: smoothing {smoothing}"),
    }];
    lines.push(if s.align { "Frames aligned" } else { "Frames not aligned" }.into());
    if let Some(b) = s.bokeh {
        let frame = match b.frame {
            BokehFrame::MostBlurred => "most blurred frame",
            BokehFrame::NearestFocus => "nearest focus",
        };
        lines.push(format!("Soft bokeh: {frame}, threshold {}", b.threshold));
    }
    lines.join("\n")
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
