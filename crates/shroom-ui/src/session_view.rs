//! The Session workspace: the photos of a folder grouped into stacks, reviewed and edited
//! by the user, and rendered in a batch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use eframe::egui::{self, Color32, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use shroom_core::DisplayImage;
use shroom_core::io::{Exposure, FrameInfo};
use shroom_core::session::{self, SessionFile, Shot, Stack};
use shroom_core::stack::{Method, StackSettings};

use crate::batch::{Batch, BatchEvent};
use crate::loader::Loader;
use crate::processing::overall_progress;

/// Longer side of the thumbnail textures, in pixels.
const THUMB_SIZE: usize = 160;
/// Height of thumbnails in the stack rows, in points.
const THUMB_HEIGHT: f32 = 72.0;
const WARNING_COLOR: Color32 = Color32::from_rgb(240, 200, 90);
const ERROR_COLOR: Color32 = Color32::from_rgb(255, 110, 110);

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
    /// Linear DNGs (stacking results) in the folder, not grouped.
    results: usize,
    unreadable: usize,
    stacks: Vec<Stack>,
    checked: Vec<bool>,
    status: HashMap<usize, Status>,
    selected: Option<PathBuf>,
    batch: Option<RunningBatch>,
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
    }

    pub fn is_loading(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn is_batch_running(&self) -> bool {
        self.batch.is_some()
    }

    #[cfg(test)]
    pub fn stacks(&self) -> &[Stack] {
        &self.stacks
    }

    pub fn set_info(&mut self, ctx: &egui::Context, path: &Path, result: Result<FrameInfo, String>) {
        if !self.pending.remove(path) {
            return;
        }
        match result {
            Ok(info) if !info.mosaic => self.results += 1,
            Ok(info) => {
                let thumb = info.preview.as_ref().map(|p| {
                    let small = downscale(p, THUMB_SIZE);
                    let image = egui::ColorImage::from_rgb([small.width, small.height], &small.data);
                    ctx.load_texture(format!("session:{}", path.display()), image, TextureOptions::LINEAR)
                });
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
        self.checked = vec![false; self.stacks.len()];
        let uncertain = self.stacks.iter().filter(|s| s.is_uncertain()).count();
        let mut text = format!("{} photos in {} stacks", shots.len(), self.stacks.len());
        if uncertain > 0 {
            text += &format!(", {uncertain} to check");
        }
        if self.results > 0 {
            text += &format!(" ({} stacking results not listed)", self.results);
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

    pub fn poll_batch(&mut self) {
        let Some(b) = &mut self.batch else { return };
        let mut done = false;
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
                            self.stacks[i].result = Some(path);
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
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Action> {
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
        let (mut edit, mut action) = (None, None);
        let editable = self.batch.is_none();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for i in 0..self.stacks.len() {
                ui.add_space(6.0);
                self.stack_header(ui, i, editable, &mut edit, &mut action);
                self.thumbnails(ui, i, editable, &mut edit);
                ui.add_space(4.0);
                ui.separator();
            }
        });
        if let Some(edit) = edit {
            self.apply(edit);
        }
        action
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
            if let Some(result) = &stack.result {
                let name = result.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(RichText::new(format!("Result: {name}")).weak());
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

    fn thumbnails(&mut self, ui: &mut egui::Ui, i: usize, editable: bool, edit: &mut Option<Edit>) {
        let stack = self.stacks[i].clone();
        let stack = &stack;
        let last_stack = self.stacks.len() - 1;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(4.0);
            for (j, path) in stack.frames.iter().enumerate() {
                let Some(photo) = self.photos.get(path) else { continue };
                let excluded = stack.excluded.contains(path);
                let response = match &photo.thumb {
                    Some(tex) => {
                        let orientation = photo.shot.orientation;
                        let (w, h) = orientation.display_size(tex.size_vec2().x, tex.size_vec2().y);
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(w, h) * (THUMB_HEIGHT / h), Sense::click());
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

/// Box-filtered copy with the longer side at most `max` pixels.
fn downscale(img: &DisplayImage, max: usize) -> DisplayImage {
    let factor = img.width.max(img.height).div_ceil(max).max(1);
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
        assert_eq!((small.width, small.height), (146, 97));
        assert!(small.data.iter().all(|&v| v == 100));
    }

    #[test]
    fn exposure_description() {
        let e = Exposure { time: Some(0.04), f_number: Some(6.3), iso: Some(1600), focal_length: Some(35.0) };
        assert_eq!(exposure_text(&e), "f/6.3 · 1/25 s · ISO 1600 · 35 mm");
    }
}
