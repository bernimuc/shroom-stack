//! Zoomable, pannable image view; single or split (both halves share zoom and position).

use eframe::egui::{self, Color32, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use shroom_core::{DisplayImage, Orientation, Plane};

/// Zoom and position shared by all panes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    /// Screen points per image pixel.
    pub zoom: f32,
    /// Image position (pixels) shown at the pane centre.
    pub center: Vec2,
    /// Keep the whole image fitted to the pane (until the user zooms or pans).
    pub fit: bool,
    /// The pane shown alone, filling the screen (toggled with F over a pane).
    pub fullscreen: Option<Fullscreen>,
    /// The pane last shown full screen.
    last_fullscreen: Option<usize>,
}

/// A pane shown full screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fullscreen {
    pub pane: usize,
    /// Whether the window was put into full screen for it (and goes back afterwards).
    restore_window: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self { zoom: 1.0, center: Vec2::ZERO, fit: true, fullscreen: None, last_fullscreen: None }
    }
}

/// Wheel zoom beyond 1:1 goes up to this factor (useful for retouching).
const MAX_ZOOM_FACTOR: f32 = 4.0;
/// Zoom factor of one step of the +/− buttons and keys.
const ZOOM_STEP: f32 = 1.25;

impl ViewState {
    pub fn fit_zoom(pane: Vec2, image: Vec2) -> f32 {
        (pane.x / image.x).min(pane.y / image.y).max(1e-6)
    }

    /// Zoom where one image pixel covers one physical screen pixel.
    pub fn one_to_one(pixels_per_point: f32) -> f32 {
        1.0 / pixels_per_point
    }

    /// Fits the image if `fit` is set; keeps zoom and centre in range otherwise.
    pub fn update(&mut self, pane: Vec2, image: Vec2, pixels_per_point: f32) {
        let fit = Self::fit_zoom(pane, image);
        if self.fit {
            self.zoom = fit;
            self.center = image / 2.0;
            return;
        }
        let max = Self::one_to_one(pixels_per_point) * MAX_ZOOM_FACTOR;
        self.zoom = self.zoom.clamp(fit.min(max), max);
        self.center = self.center.clamp(Vec2::ZERO, image);
    }

    /// Changes the zoom keeping the image point under `anchor` (offset from the pane
    /// centre, in points) in place.
    pub fn zoom_to(&mut self, zoom: f32, anchor: Vec2) {
        let under_anchor = self.center + anchor / self.zoom;
        self.zoom = zoom;
        self.center = under_anchor - anchor / zoom;
        self.fit = false;
    }

    /// Switches between fit and 1:1; zooming in keeps the image point at `anchor` (offset
    /// from the pane centre) under the pointer.
    pub fn toggle_one_to_one(&mut self, pixels_per_point: f32, anchor: Vec2) {
        let one = Self::one_to_one(pixels_per_point);
        if !self.fit && (self.zoom - one).abs() <= one * 0.01 {
            self.fit = true;
        } else {
            self.zoom_to(one, anchor);
        }
    }

    /// One zoom step in (`steps` > 0) or out, keeping the image point at `anchor` (offset
    /// from the pane centre) in place. Zooming out stops at fit or 1:1, whichever is
    /// smaller; zooming in at [`MAX_ZOOM_FACTOR`] × 1:1.
    pub fn zoom_step(&mut self, steps: i32, pane: Vec2, image: Vec2, pixels_per_point: f32, anchor: Vec2) {
        let one = Self::one_to_one(pixels_per_point);
        let min = Self::fit_zoom(pane, image).min(one);
        let zoom = (self.zoom * ZOOM_STEP.powi(steps)).clamp(min, one * MAX_ZOOM_FACTOR);
        self.zoom_to(zoom, anchor);
    }

    pub fn pan(&mut self, screen_delta: Vec2) {
        self.center -= screen_delta / self.zoom;
        self.fit = false;
    }

    pub fn image_to_screen(&self, pane: Rect, p: Vec2) -> Pos2 {
        pane.center() + (p - self.center) * self.zoom
    }

    pub fn screen_to_image(&self, pane: Rect, p: Pos2) -> Vec2 {
        self.center + (p - pane.center()) / self.zoom
    }

    pub fn percent(&self, pixels_per_point: f32) -> f32 {
        self.zoom * pixels_per_point * 100.0
    }

    /// Shows `pane` alone, putting the window into full screen.
    fn enter_fullscreen(&mut self, ctx: &egui::Context, pane: usize) {
        let restore_window = !ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if restore_window {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        self.fullscreen = Some(Fullscreen { pane, restore_window });
    }

    /// Leaves the full screen view, restoring the window.
    pub fn leave_fullscreen(&mut self, ctx: &egui::Context) {
        let Some(fs) = self.fullscreen.take() else { return };
        self.last_fullscreen = Some(fs.pane);
        if fs.restore_window {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }
}

/// An image as GPU textures, split into tiles below the maximum texture size.
pub struct TiledImage {
    tiles: Vec<(Rect, TextureHandle)>,
    /// Size of the texture data in pixels (may be smaller than the image it shows).
    size: Vec2,
}

impl TiledImage {
    pub fn new(ctx: &egui::Context, name: &str, img: &DisplayImage) -> Self {
        Self::from_rows(ctx, name, img.width, img.height, |y, x0, w, rgba| {
            for px in img.data[(y * img.width + x0) * 3..(y * img.width + x0 + w) * 3].chunks(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        })
    }

    /// A translucent overlay in `color`, opaque where `mask` is 1. The mask may have a
    /// lower resolution than the image it covers.
    pub fn overlay(ctx: &egui::Context, name: &str, mask: &Plane, color: Color32) -> Self {
        Self::from_rows(ctx, name, mask.width(), mask.height(), |y, x0, w, rgba| {
            for m in &mask.data()[y * mask.width() + x0..y * mask.width() + x0 + w] {
                rgba.extend_from_slice(&[
                    color.r(),
                    color.g(),
                    color.b(),
                    (m.clamp(0.0, 1.0) * color.a() as f32) as u8,
                ]);
            }
        })
    }

    /// Builds the tiles; `row(y, x0, w, rgba)` appends `w` RGBA pixels of row `y` from `x0`.
    fn from_rows(
        ctx: &egui::Context,
        name: &str,
        width: usize,
        height: usize,
        row: impl Fn(usize, usize, usize, &mut Vec<u8>),
    ) -> Self {
        let max = ctx.input(|i| i.max_texture_side).min(4096);
        let mut tiles = Vec::new();
        for y0 in (0..height).step_by(max) {
            for x0 in (0..width).step_by(max) {
                let (w, h) = ((width - x0).min(max), (height - y0).min(max));
                let mut rgba = Vec::with_capacity(w * h * 4);
                for y in y0..y0 + h {
                    row(y, x0, w, &mut rgba);
                }
                let color = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                let texture = ctx.load_texture(format!("{name}@{x0},{y0}"), color, TextureOptions::LINEAR);
                let rect = Rect::from_min_size(Pos2::new(x0 as f32, y0 as f32), Vec2::new(w as f32, h as f32));
                tiles.push((rect, texture));
            }
        }
        Self { tiles, size: Vec2::new(width as f32, height as f32) }
    }

    /// Replaces the pixels at `(x, y)` (texture pixels) with `img`.
    pub fn update(&mut self, x: usize, y: usize, img: &DisplayImage) {
        let patch = Rect::from_min_size(Pos2::new(x as f32, y as f32), Vec2::new(img.width as f32, img.height as f32));
        for (rect, texture) in &mut self.tiles {
            let overlap = rect.intersect(patch);
            if overlap.width() <= 0.0 || overlap.height() <= 0.0 {
                continue;
            }
            let (ox, oy) = ((overlap.min.x - patch.min.x) as usize, (overlap.min.y - patch.min.y) as usize);
            let (w, h) = (overlap.width() as usize, overlap.height() as usize);
            let mut rgba = Vec::with_capacity(w * h * 4);
            for row in oy..oy + h {
                let start = (row * img.width + ox) * 3;
                for px in img.data[start..start + w * 3].chunks(3) {
                    rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            let pos = [(overlap.min.x - rect.min.x) as usize, (overlap.min.y - rect.min.y) as usize];
            texture.set_partial(pos, egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba), TextureOptions::LINEAR);
        }
    }

    /// The texture and its size, if the image fits into a single tile.
    pub fn single(&self) -> Option<(&TextureHandle, Vec2)> {
        match self.tiles.as_slice() {
            [(_, texture)] => Some((texture, self.size)),
            _ => None,
        }
    }

    /// Paints the image, which covers `image_size` stored-image pixels, into `pane`,
    /// rotated by `orientation` (the view works in display coordinates).
    fn paint(&self, painter: &egui::Painter, view: &ViewState, pane: Rect, image_size: Vec2, orientation: Orientation) {
        let scale = image_size / self.size;
        for (rect, texture) in &self.tiles {
            let corner = |p: Pos2| {
                let stored = p.to_vec2() * scale;
                let (x, y) = orientation.to_display((stored.x, stored.y), (image_size.x, image_size.y));
                view.image_to_screen(pane, Vec2::new(x, y))
            };
            let quad = [
                corner(rect.left_top()),
                corner(rect.right_top()),
                corner(rect.right_bottom()),
                corner(rect.left_bottom()),
            ];
            if Rect::from_points(&quad).intersects(pane) {
                painter.add(textured_quad(texture.id(), quad));
            }
        }
    }
}

/// A texture drawn onto four screen points: its top-left, top-right, bottom-right and
/// bottom-left corners, in that order (which rotates it as needed).
fn textured_quad(texture: egui::TextureId, corners: [Pos2; 4]) -> egui::Shape {
    let mut mesh = egui::Mesh::with_texture(texture);
    let uvs = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
    for (pos, uv) in corners.into_iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: Color32::WHITE });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    egui::Shape::mesh(mesh)
}

/// Paints a whole texture into `rect` (sized for the displayed orientation), rotated by
/// `orientation`. Used for thumbnails.
pub fn paint_rotated(painter: &egui::Painter, texture: egui::TextureId, rect: Rect, orientation: Orientation) {
    let corner = |x: f32, y: f32| {
        let (u, v) = orientation.to_display((x, y), (1.0, 1.0));
        rect.min + Vec2::new(u, v) * rect.size()
    };
    painter.add(textured_quad(texture, [corner(0.0, 0.0), corner(1.0, 0.0), corner(1.0, 1.0), corner(0.0, 1.0)]));
}

/// One half of the view.
pub struct Pane<'a> {
    pub label: &'a str,
    /// Image to show and the size (in image pixels) it covers.
    pub image: Option<(&'a TiledImage, Vec2)>,
    /// Text shown instead of an image (e.g. while loading).
    pub placeholder: &'a str,
    /// Drawn over the image, covering the same area.
    pub overlay: Option<&'a TiledImage>,
}

/// What happened in the view this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewResponse {
    /// Stored-image position (sensor orientation) under the pointer.
    pub hovered: Option<Vec2>,
    /// The primary button is held over the image (and the view is not being panned).
    pub painting: bool,
}

/// Shows one or more panes side by side with shared zoom/pan, plus the zoom controls.
/// Images are given in stored (sensor) orientation and shown rotated by `orientation`;
/// the view state works in display coordinates. `brush` (radius in image pixels) draws a
/// brush outline at the pointer in every pane. `options` adds display options to the
/// controls below the panes. F over a pane shows it alone, filling the screen without any
/// other controls; F or Esc goes back.
pub fn show(
    ui: &mut egui::Ui,
    view: &mut ViewState,
    panes: &[Pane],
    brush: Option<f32>,
    orientation: Orientation,
    options: impl FnOnce(&mut egui::Ui),
) -> ViewResponse {
    let ctx = ui.ctx().clone();
    let keys = !ctx.egui_wants_keyboard_input();
    if let Some(fs) = view.fullscreen {
        let close = keys
            && ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::F)
                    | i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
            });
        match panes.get(fs.pane) {
            Some(pane) if !close => {
                // Keep the workspace's layout underneath, but leave it empty.
                ui.allocate_rect(ui.available_rect_before_wrap(), Sense::hover());
                let screen = ctx.content_rect();
                let area = egui::Area::new(egui::Id::unique("viewer-fullscreen"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(screen.min)
                    .show(&ctx, |ui| {
                        let (response, _) =
                            panes_view(ui, view, std::slice::from_ref(pane), screen, brush, orientation, false);
                        response
                    });
                return area.inner;
            }
            _ => view.leave_fullscreen(&ctx),
        }
    }

    let controls_height = 28.0;
    let area = ui.available_rect_before_wrap();
    let views_rect = Rect::from_min_max(area.min, Pos2::new(area.max.x, area.max.y - controls_height));
    let (response, pointer_pane) = panes_view(ui, view, panes, views_rect, brush, orientation, true);
    // Resizing the window into and out of full screen makes the pointer position unknown
    // until the mouse moves; then F goes back to the pane last shown full screen.
    let pointer_known = ctx.input(|i| i.pointer.latest_pos().is_some());
    let pane = if pointer_known { pointer_pane } else { view.last_fullscreen.filter(|&p| p < panes.len()) };
    if keys
        && let Some(pane) = pane
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F))
    {
        view.enter_fullscreen(&ctx, pane);
    }

    // Controls below the panes.
    let ppp = ctx.pixels_per_point();
    let image_size = image_size(panes, orientation);
    let pane_size = Vec2::new(views_rect.width() / panes.len().max(1) as f32, views_rect.height());
    let controls = Rect::from_min_max(Pos2::new(area.min.x, views_rect.max.y), area.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(controls), |ui| {
        ui.horizontal_centered(|ui| {
            controls_bar(ui, view, image_size, pane_size, ppp);
            ui.separator();
            options(ui);
            let hint = if brush.is_some() {
                "Wheel: zoom · Space+drag: pan · Drag: paint"
            } else {
                "Wheel: zoom · Drag: pan · Double-click: fit / 1:1 · F: full screen"
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(hint).weak()).truncate());
            });
        });
    });
    response
}

/// Size of the panes' image in display orientation.
fn image_size(panes: &[Pane], orientation: Orientation) -> Option<Vec2> {
    panes.iter().find_map(|p| p.image.map(|(_, s)| s)).map(|s| {
        let (w, h) = orientation.display_size(s.x, s.y);
        Vec2::new(w, h)
    })
}

/// The panes side by side in `views_rect`, with zoom, pan and brush input. Returns what
/// happened and the index of the pane under the pointer.
fn panes_view(
    ui: &mut egui::Ui,
    view: &mut ViewState,
    panes: &[Pane],
    views_rect: Rect,
    brush: Option<f32>,
    orientation: Orientation,
    labels: bool,
) -> (ViewResponse, Option<usize>) {
    let ppp = ui.ctx().pixels_per_point();
    let stored_size = panes.iter().find_map(|p| p.image.map(|(_, s)| s));
    let image_size = image_size(panes, orientation);
    let pane_width = views_rect.width() / panes.len().max(1) as f32;
    let pane_rects: Vec<Rect> = (0..panes.len())
        .map(|i| {
            Rect::from_min_size(
                Pos2::new(views_rect.min.x + i as f32 * pane_width, views_rect.min.y),
                Vec2::new(pane_width, views_rect.height()),
            )
        })
        .collect();

    if let Some(size) = image_size {
        view.update(pane_rects[0].size(), size, ppp);
    }
    let response = ui.allocate_rect(views_rect, Sense::click_and_drag());
    let mut hovered = None;
    let pointer_index = response.hover_pos().and_then(|pos| pane_rects.iter().position(|r| r.contains(pos)));
    let pointer_pane = response.hover_pos().map(|pos| (pos, pane_rects[pointer_index.unwrap_or(0)]));
    // Double-click toggles fit / 1:1 (not while painting, where clicks are dabs).
    if image_size.is_some()
        && brush.is_none()
        && response.double_clicked()
        && let Some((pos, pane)) = pointer_pane
    {
        view.toggle_one_to_one(ppp, pos - pane.center());
    }
    if let (Some(size), Some((pos, pane))) = (image_size, pointer_pane) {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = (scroll * 0.002).exp() * pinch;
        if factor != 1.0 {
            let fit = ViewState::fit_zoom(pane.size(), size);
            let max = ViewState::one_to_one(ppp) * MAX_ZOOM_FACTOR;
            view.zoom_to((view.zoom * factor).clamp(fit.min(max), max), pos - pane.center());
        }
        hovered = Some(view.screen_to_image(pane, pos));
    }
    // + and − zoom in steps, at the pointer if it is over a pane.
    if let Some(size) = image_size
        && !ui.ctx().egui_wants_keyboard_input()
    {
        let steps: i32 = ui.input_mut(|i| {
            let zoom_in = i.consume_key(egui::Modifiers::NONE, egui::Key::Plus)
                | i.consume_key(egui::Modifiers::NONE, egui::Key::Equals);
            let zoom_out = i.consume_key(egui::Modifiers::NONE, egui::Key::Minus);
            zoom_in as i32 - zoom_out as i32
        });
        if steps != 0 {
            let (pane, anchor) = match pointer_pane {
                Some((pos, pane)) => (pane, pos - pane.center()),
                None => (pane_rects[0], Vec2::ZERO),
            };
            view.zoom_step(steps, pane.size(), size, ppp, anchor);
        }
    }
    // Dragging pans, except with the brush, where Space+drag or middle-drag pans.
    let space = ui.input(|i| i.key_down(egui::Key::Space));
    let pan_mode = brush.is_none() || space || response.dragged_by(egui::PointerButton::Middle);
    if pan_mode && response.dragged() {
        view.pan(response.drag_delta());
    }
    let painting = brush.is_some()
        && hovered.is_some()
        && !pan_mode
        && ui.input(|i| i.pointer.primary_down())
        && response.is_pointer_button_down_on();
    if hovered.is_some() || response.dragged() {
        let cursor = if pan_mode {
            if response.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab }
        } else {
            egui::CursorIcon::Crosshair
        };
        ui.ctx().set_cursor_icon(cursor);
    }
    if let Some(size) = image_size {
        view.update(pane_rects[0].size(), size, ppp);
    }

    let painter = ui.painter_at(views_rect);
    painter.rect_filled(views_rect, 0.0, Color32::from_gray(24));
    for (pane, rect) in panes.iter().zip(&pane_rects) {
        let clipped = painter.with_clip_rect(*rect);
        match pane.image {
            Some((img, size)) => {
                img.paint(&clipped, view, *rect, size, orientation);
                if let Some(overlay) = pane.overlay {
                    overlay.paint(&clipped, view, *rect, size, orientation);
                }
            }
            None => {
                clipped.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    pane.placeholder,
                    egui::FontId::proportional(16.0),
                    Color32::GRAY,
                );
            }
        }
        if labels && !pane.label.is_empty() {
            let galley_pos = rect.min + Vec2::new(8.0, 8.0);
            clipped.text(
                galley_pos,
                egui::Align2::LEFT_TOP,
                pane.label,
                egui::FontId::proportional(13.0),
                Color32::from_gray(220),
            );
        }
    }
    for rect in pane_rects.iter().skip(1) {
        painter.vline(rect.min.x, rect.y_range(), egui::Stroke::new(1.0, Color32::from_gray(90)));
    }
    if let (Some(radius), Some(pos), false) = (brush, hovered, pan_mode) {
        for rect in &pane_rects {
            let centre = view.image_to_screen(*rect, pos);
            let clipped = painter.with_clip_rect(*rect);
            let r = radius * view.zoom;
            clipped.circle_stroke(centre, r, egui::Stroke::new(2.5, Color32::from_black_alpha(160)));
            clipped.circle_stroke(centre, r, egui::Stroke::new(1.0, Color32::WHITE));
        }
    }

    // Report the pointer in stored-image coordinates.
    let hovered = hovered.zip(stored_size).map(|(d, s)| {
        let (x, y) = orientation.to_image((d.x, d.y), (s.x, s.y));
        Vec2::new(x, y)
    });
    (ViewResponse { hovered, painting }, pointer_index)
}

fn controls_bar(ui: &mut egui::Ui, view: &mut ViewState, image: Option<Vec2>, pane: Vec2, ppp: f32) {
    let Some(image) = image else {
        ui.add_enabled(false, egui::Label::new("–"));
        return;
    };
    let fit = ViewState::fit_zoom(pane, image);
    let one = ViewState::one_to_one(ppp);
    ui.label(format!("{:.0} %", view.percent(ppp))).on_hover_text("Current zoom level");
    if ui.button("Fit").on_hover_text("Fit the whole image").clicked() {
        view.fit = true;
    }
    if ui.button("1:1").on_hover_text("One image pixel per screen pixel").clicked() {
        view.zoom_to(one, Vec2::ZERO);
    }
    if ui.button("-").on_hover_text("Zoom out (−)").clicked() {
        view.zoom_step(-1, pane, image, ppp, Vec2::ZERO);
    }
    if ui.button("+").on_hover_text("Zoom in (+)").clicked() {
        view.zoom_step(1, pane, image, ppp, Vec2::ZERO);
    }
    // Logarithmic slider from "fit" to 1:1.
    let (lo, hi) = (fit.min(one).ln(), one.max(fit).ln());
    let mut t = if hi > lo { ((view.zoom.ln() - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 1.0 };
    let slider = ui.add(egui::Slider::new(&mut t, 0.0..=1.0).show_value(false)).on_hover_text("Zoom from fit to 1:1");
    if slider.changed() {
        view.zoom_to((lo + t * (hi - lo)).exp(), Vec2::ZERO);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_stop_at_fit_and_max() {
        let (pane, image, ppp) = (Vec2::new(400.0, 300.0), Vec2::new(8000.0, 6000.0), 2.0);
        let mut v = ViewState::default();
        v.update(pane, image, ppp);
        assert_eq!(v.zoom, 0.05);
        v.zoom_step(1, pane, image, ppp, Vec2::ZERO);
        assert!((v.zoom - 0.0625).abs() < 1e-6 && !v.fit);
        v.zoom_step(-3, pane, image, ppp, Vec2::ZERO);
        assert_eq!(v.zoom, 0.05, "not below fit");
        v.zoom_step(50, pane, image, ppp, Vec2::ZERO);
        assert_eq!(v.zoom, 0.5 * MAX_ZOOM_FACTOR, "not beyond the maximum");
    }

    #[test]
    fn fit_centres_whole_image() {
        let mut v = ViewState::default();
        v.update(Vec2::new(400.0, 300.0), Vec2::new(8000.0, 4000.0), 2.0);
        assert_eq!(v.zoom, 0.05);
        assert_eq!(v.center, Vec2::new(4000.0, 2000.0));
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let pane = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let mut v = ViewState { zoom: 0.1, center: Vec2::new(1000.0, 800.0), fit: false, ..Default::default() };
        let cursor = Pos2::new(300.0, 50.0);
        let before = v.screen_to_image(pane, cursor);
        v.zoom_to(0.5, cursor - pane.center());
        let after = v.screen_to_image(pane, cursor);
        assert!((before - after).length() < 1e-3, "{before:?} vs {after:?}");
        assert!(!v.fit);
    }

    #[test]
    fn screen_image_roundtrip_and_pan() {
        let pane = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(400.0, 300.0));
        let mut v = ViewState { zoom: 0.25, center: Vec2::new(500.0, 500.0), fit: false, ..Default::default() };
        let p = Vec2::new(123.0, 456.0);
        assert!((v.screen_to_image(pane, v.image_to_screen(pane, p)) - p).length() < 1e-3);
        v.pan(Vec2::new(10.0, -5.0));
        assert_eq!(v.center, Vec2::new(460.0, 520.0));
    }

    #[test]
    fn double_click_toggles_fit_and_one_to_one() {
        let pane = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let mut v = ViewState::default();
        v.update(pane.size(), Vec2::new(4000.0, 3000.0), 2.0);
        // From fit: 1:1 with the clicked image point staying under the pointer.
        let click = Pos2::new(300.0, 100.0);
        let target = v.screen_to_image(pane, click);
        v.toggle_one_to_one(2.0, click - pane.center());
        assert_eq!(v.zoom, 0.5);
        assert!((v.screen_to_image(pane, click) - target).length() < 1e-3);
        // From 1:1: back to fit.
        v.toggle_one_to_one(2.0, Vec2::ZERO);
        assert!(v.fit);
        // From another zoom level: to 1:1.
        v = ViewState { zoom: 1.3, center: Vec2::new(10.0, 10.0), fit: false, ..Default::default() };
        v.toggle_one_to_one(2.0, Vec2::ZERO);
        assert_eq!(v.zoom, 0.5);
    }

    #[test]
    fn update_clamps_zoom_and_centre() {
        let (pane, image) = (Vec2::new(400.0, 300.0), Vec2::new(4000.0, 3000.0));
        let mut v = ViewState { zoom: 100.0, center: Vec2::new(-50.0, 9999.0), fit: false, ..Default::default() };
        v.update(pane, image, 2.0);
        assert_eq!(v.zoom, 0.5 * MAX_ZOOM_FACTOR);
        assert_eq!(v.center, Vec2::new(0.0, 3000.0));
        v.zoom = 0.0001;
        v.update(pane, image, 2.0);
        assert_eq!(v.zoom, 0.1);
        assert_eq!(v.percent(2.0), 20.0);
    }
}
