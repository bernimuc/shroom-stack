//! Zoomable, pannable image view; single or split (both halves share zoom and position).

use eframe::egui::{self, Color32, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use shroom_core::DisplayImage;

/// Zoom and position shared by all panes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    /// Screen points per image pixel.
    pub zoom: f32,
    /// Image position (pixels) shown at the pane centre.
    pub center: Vec2,
    /// Keep the whole image fitted to the pane (until the user zooms or pans).
    pub fit: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self { zoom: 1.0, center: Vec2::ZERO, fit: true }
    }
}

/// Wheel zoom beyond 1:1 goes up to this factor (useful for retouching).
const MAX_ZOOM_FACTOR: f32 = 4.0;

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
}

/// An image as GPU textures, split into tiles below the maximum texture size.
pub struct TiledImage {
    tiles: Vec<(Rect, TextureHandle)>,
    /// Size of the texture data in pixels (may be smaller than the image it shows).
    size: Vec2,
}

impl TiledImage {
    pub fn new(ctx: &egui::Context, name: &str, img: &DisplayImage) -> Self {
        let max = ctx.input(|i| i.max_texture_side).min(4096);
        let mut tiles = Vec::new();
        for y0 in (0..img.height).step_by(max) {
            for x0 in (0..img.width).step_by(max) {
                let (w, h) = ((img.width - x0).min(max), (img.height - y0).min(max));
                let mut rgba = Vec::with_capacity(w * h * 4);
                for y in y0..y0 + h {
                    for px in img.data[(y * img.width + x0) * 3..(y * img.width + x0 + w) * 3].chunks(3) {
                        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                    }
                }
                let color = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                let texture = ctx.load_texture(format!("{name}@{x0},{y0}"), color, TextureOptions::LINEAR);
                let rect = Rect::from_min_size(Pos2::new(x0 as f32, y0 as f32), Vec2::new(w as f32, h as f32));
                tiles.push((rect, texture));
            }
        }
        Self { tiles, size: Vec2::new(img.width as f32, img.height as f32) }
    }

    /// The texture and its size, if the image fits into a single tile.
    pub fn single(&self) -> Option<(&TextureHandle, Vec2)> {
        match self.tiles.as_slice() {
            [(_, texture)] => Some((texture, self.size)),
            _ => None,
        }
    }

    /// Paints the image, which covers `image_size` image pixels, into `pane`.
    fn paint(&self, painter: &egui::Painter, view: &ViewState, pane: Rect, image_size: Vec2) {
        let scale = image_size / self.size;
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for (rect, texture) in &self.tiles {
            let min = view.image_to_screen(pane, rect.min.to_vec2() * scale);
            let max = view.image_to_screen(pane, rect.max.to_vec2() * scale);
            let screen = Rect::from_min_max(min, max);
            if screen.intersects(pane) {
                painter.image(texture.id(), screen, uv, Color32::WHITE);
            }
        }
    }
}

/// One half of the view.
pub struct Pane<'a> {
    pub label: &'a str,
    /// Image to show and the size (in image pixels) it covers.
    pub image: Option<(&'a TiledImage, Vec2)>,
    /// Text shown instead of an image (e.g. while loading).
    pub placeholder: &'a str,
}

/// Shows one or more panes side by side with shared zoom/pan, plus the zoom controls.
/// Returns the hovered image position, if any.
pub fn show(ui: &mut egui::Ui, view: &mut ViewState, panes: &[Pane]) -> Option<Vec2> {
    let ppp = ui.ctx().pixels_per_point();
    let image_size = panes.iter().find_map(|p| p.image.map(|(_, s)| s));
    let controls_height = 28.0;
    let area = ui.available_rect_before_wrap();
    let views_rect = Rect::from_min_max(area.min, Pos2::new(area.max.x, area.max.y - controls_height));
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
    if let (Some(size), Some(pos)) = (image_size, response.hover_pos()) {
        let pane = pane_rects.iter().find(|r| r.contains(pos)).copied().unwrap_or(pane_rects[0]);
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = (scroll * 0.002).exp() * pinch;
        if factor != 1.0 {
            let fit = ViewState::fit_zoom(pane.size(), size);
            let max = ViewState::one_to_one(ppp) * MAX_ZOOM_FACTOR;
            view.zoom_to((view.zoom * factor).clamp(fit.min(max), max), pos - pane.center());
        }
        hovered = Some(view.screen_to_image(pane, pos));
    }
    let panning = ui.input(|i| i.key_down(egui::Key::Space)) || response.dragged_by(egui::PointerButton::Middle);
    if panning && response.dragged() {
        view.pan(response.drag_delta());
    }
    if let Some(size) = image_size {
        view.update(pane_rects[0].size(), size, ppp);
    }

    let painter = ui.painter_at(views_rect);
    painter.rect_filled(views_rect, 0.0, Color32::from_gray(24));
    for (pane, rect) in panes.iter().zip(&pane_rects) {
        let clipped = painter.with_clip_rect(*rect);
        match pane.image {
            Some((img, size)) => img.paint(&clipped, view, *rect, size),
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
        if !pane.label.is_empty() {
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

    // Controls below the panes.
    let controls = Rect::from_min_max(Pos2::new(area.min.x, views_rect.max.y), area.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(controls), |ui| {
        ui.horizontal_centered(|ui| controls_bar(ui, view, image_size, pane_rects[0].size(), ppp));
    });
    hovered
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
    if ui.button("-").on_hover_text("Zoom out").clicked() {
        view.zoom_to((view.zoom / 1.25).max(fit.min(one)), Vec2::ZERO);
    }
    if ui.button("+").on_hover_text("Zoom in").clicked() {
        view.zoom_to((view.zoom * 1.25).min(one * MAX_ZOOM_FACTOR), Vec2::ZERO);
    }
    // Logarithmic slider from "fit" to 1:1.
    let (lo, hi) = (fit.min(one).ln(), one.max(fit).ln());
    let mut t = if hi > lo { ((view.zoom.ln() - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 1.0 };
    let slider = ui.add(egui::Slider::new(&mut t, 0.0..=1.0).show_value(false)).on_hover_text("Zoom from fit to 1:1");
    if slider.changed() {
        view.zoom_to((lo + t * (hi - lo)).exp(), Vec2::ZERO);
    }
    ui.label(egui::RichText::new("Wheel: zoom · Space+drag: pan").weak());
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut v = ViewState { zoom: 0.1, center: Vec2::new(1000.0, 800.0), fit: false };
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
        let mut v = ViewState { zoom: 0.25, center: Vec2::new(500.0, 500.0), fit: false };
        let p = Vec2::new(123.0, 456.0);
        assert!((v.screen_to_image(pane, v.image_to_screen(pane, p)) - p).length() < 1e-3);
        v.pan(Vec2::new(10.0, -5.0));
        assert_eq!(v.center, Vec2::new(460.0, 520.0));
    }

    #[test]
    fn update_clamps_zoom_and_centre() {
        let (pane, image) = (Vec2::new(400.0, 300.0), Vec2::new(4000.0, 3000.0));
        let mut v = ViewState { zoom: 100.0, center: Vec2::new(-50.0, 9999.0), fit: false };
        v.update(pane, image, 2.0);
        assert_eq!(v.zoom, 0.5 * MAX_ZOOM_FACTOR);
        assert_eq!(v.center, Vec2::new(0.0, 3000.0));
        v.zoom = 0.0001;
        v.update(pane, image, 2.0);
        assert_eq!(v.zoom, 0.1);
        assert_eq!(v.percent(2.0), 20.0);
    }
}
