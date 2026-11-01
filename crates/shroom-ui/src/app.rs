//! Application state and the workspaces.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText, Vec2};
use shroom_core::stack::Method;

use crate::frames::{FrameList, Issue, Severity, preflight};
use crate::loader::{Loaded, Loader};
use crate::viewer::{self, Pane, TiledImage, ViewState};

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
        let Method::DepthMap { radius, smoothing } = Method::DEFAULT_DEPTH_MAP else {
            unreachable!()
        };
        let Method::Pyramid {
            smoothing: pyramid_smoothing,
        } = Method::DEFAULT_PYRAMID
        else {
            unreachable!()
        };
        Self {
            kind: MethodKind::DepthMap,
            radius,
            depth_smoothing: smoothing,
            pyramid_smoothing,
            align: true,
        }
    }
}

impl Parameters {
    #[allow(dead_code)] // used once processing is wired up
    fn method(&self) -> Method {
        match self.kind {
            MethodKind::DepthMap => Method::DepthMap {
                radius: self.radius,
                smoothing: self.depth_smoothing,
            },
            MethodKind::Pyramid => Method::Pyramid {
                smoothing: self.pyramid_smoothing,
            },
        }
    }
}

pub struct ShroomApp {
    workspace: Workspace,
    frames: FrameList,
    selected: Option<usize>,
    /// Embedded previews (about 1024 px) as textures, by file.
    thumbs: HashMap<PathBuf, TiledImage>,
    /// Full-resolution rendering of the selected frame.
    full: Option<(PathBuf, TiledImage)>,
    view: ViewState,
    params: Parameters,
    issues: Option<Vec<Issue>>,
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
            loader: Loader::new(cc.egui_ctx.clone()),
        };
        app.add_files(files);
        app
    }

    /// True when no background loading is pending (all file infos and the selected
    /// frame's full-resolution preview have arrived).
    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        !self.frames.is_loading() && (self.selected.is_none() || self.full.is_some())
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let dngs = paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")));
        for path in self.frames.add(dngs) {
            self.loader.request_info(path);
        }
        if self.selected.is_none() && !self.frames.frames.is_empty() {
            self.select(Some(0));
        }
        self.issues = None;
    }

    fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        if let Some(path) = self.selected_path()
            && self.full.as_ref().is_none_or(|(p, _)| *p != path)
        {
            self.loader.request_full(path);
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected
            .and_then(|i| self.frames.frames.get(i))
            .map(|f| f.path.clone())
    }

    fn receive(&mut self, ctx: &egui::Context) {
        for loaded in self.loader.poll() {
            match loaded {
                Loaded::Info(path, result) => {
                    if let Ok(info) = &result
                        && let Some(preview) = &info.preview
                    {
                        self.thumbs
                            .insert(path.clone(), TiledImage::new(ctx, &path.to_string_lossy(), preview));
                    }
                    self.frames.set_info(&path, result);
                    // Keep the list in shooting order while files arrive.
                    let selected = self.selected_path();
                    self.frames.sort_by_capture_time();
                    self.selected = selected.and_then(|p| self.frames.frames.iter().position(|f| f.path == p));
                }
                Loaded::Full(path, Ok(img)) if Some(&path) == self.selected_path().as_ref() => {
                    self.full = Some((
                        path.clone(),
                        TiledImage::new(ctx, &format!("full:{}", path.display()), &img),
                    ));
                }
                Loaded::Full(..) => {}
            }
        }
    }
}

impl eframe::App for ShroomApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive(&ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_files(dropped);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("files")
            .resizable(true)
            .default_size(320.0)
            .min_size(220.0)
            .show(ui, |ui| self.file_list(ui));
        egui::CentralPanel::default().show(ui, |ui| self.preview(ui));
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
                // Only Preparation is reachable until a stack has been processed.
                let enabled = ws == Workspace::Preparation;
                ui.add_enabled_ui(enabled, |ui| {
                    if ui.selectable_label(self.workspace == ws, label).clicked() {
                        self.workspace = ws;
                    }
                });
            }
        });
        ui.add_space(2.0);
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
                    ui.add(egui::DragValue::new(&mut p.depth_smoothing).range(0..=20))
                        .on_hover_text(
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let render = ui.add(egui::Button::new(RichText::new("Render").strong()).min_size(Vec2::new(90.0, 0.0)));
                if render.clicked() {
                    self.issues = Some(preflight(&self.frames));
                }
            });
        });
        if let Some(issues) = &self.issues {
            ui.add_space(2.0);
            if issues.iter().all(|i| i.severity != Severity::Error) {
                ui.colored_label(
                    Color32::LIGHT_GREEN,
                    "Ready to stack. (Processing is the next milestone.)",
                );
            }
            for issue in issues {
                let (kind, color) = match issue.severity {
                    Severity::Error => ("Error", Color32::from_rgb(255, 110, 110)),
                    Severity::Warning => ("Warning", Color32::from_rgb(240, 200, 90)),
                };
                ui.colored_label(color, format!("{kind}: {}", issue.message));
            }
        }
        ui.add_space(4.0);
    }

    fn file_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add images…").clicked()
                && let Some(paths) = rfd::FileDialog::new().add_filter("DNG", &["dng", "DNG"]).pick_files()
            {
                self.add_files(paths);
            }
            let has = self.selected.is_some();
            if ui.add_enabled(has, egui::Button::new("Remove")).clicked() {
                let i = self.selected.unwrap();
                self.frames.remove(i);
                self.issues = None;
                let next = if self.frames.frames.is_empty() {
                    None
                } else {
                    Some(i.min(self.frames.frames.len() - 1))
                };
                self.select(next);
            }
            if ui
                .add_enabled(has, egui::Button::new("Up"))
                .on_hover_text("Move up")
                .clicked()
            {
                self.selected = Some(self.frames.shift(self.selected.unwrap(), -1));
                self.issues = None;
            }
            if ui
                .add_enabled(has, egui::Button::new("Down"))
                .on_hover_text("Move down")
                .clicked()
            {
                self.selected = Some(self.frames.shift(self.selected.unwrap(), 1));
                self.issues = None;
            }
            if ui.button("Reverse").on_hover_text("Reverse the order").clicked() {
                self.frames.reverse();
                self.selected = self.selected.map(|i| self.frames.frames.len() - 1 - i);
                self.issues = None;
            }
        });
        ui.label(RichText::new(format!("{} images", self.frames.frames.len())).weak());
        ui.separator();

        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.frames.frames.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Add DNG files or drop them here.").weak()));
            }
            for (i, frame) in self.frames.frames.iter().enumerate() {
                let selected = self.selected == Some(i);
                let row = egui::Frame::new()
                    .fill(if selected {
                        ui.visuals().selection.bg_fill
                    } else {
                        Color32::TRANSPARENT
                    })
                    .inner_margin(4.0)
                    .corner_radius(4.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let thumb_size = Vec2::new(96.0, 64.0);
                            match self.thumbs.get(&frame.path).and_then(|t| t.single()) {
                                Some((tex, size)) => {
                                    let scale = (thumb_size.x / size.x).min(thumb_size.y / size.y);
                                    ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), size * scale)));
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
        if let Some(i) = clicked {
            self.select(Some(i));
        }
        // Keyboard navigation through the list.
        let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
        if let Some(i) = self.selected {
            if up && i > 0 {
                self.select(Some(i - 1));
            } else if down && i + 1 < self.frames.frames.len() {
                self.select(Some(i + 1));
            }
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let frame = self.selected.and_then(|i| self.frames.frames.get(i));
        let size = frame
            .and_then(|f| f.info.as_ref())
            .map(|i| Vec2::new(i.width as f32, i.height as f32));
        let full = self
            .full
            .as_ref()
            .filter(|(p, _)| Some(p) == frame.map(|f| &f.path))
            .map(|(_, t)| t);
        let thumb = frame.and_then(|f| self.thumbs.get(&f.path));
        let image = size.and_then(|s| full.or(thumb).map(|t| (t, s)));
        let label = match (frame, full.is_some()) {
            (Some(f), true) => f.file_name(),
            (Some(f), false) => format!("{} (loading full resolution…)", f.file_name()),
            (None, _) => String::new(),
        };
        let placeholder = if frame.is_some() {
            "Loading…"
        } else {
            "No image selected"
        };
        viewer::show(
            ui,
            &mut self.view,
            &[Pane {
                label: &label,
                image,
                placeholder,
            }],
        );
    }
}
