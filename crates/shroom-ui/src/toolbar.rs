//! Toolbar widgets: big icon buttons, small icon buttons and settings laid out in rows.

use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

/// Height of the toolbar's content: a big button, or three rows of settings.
pub const HEIGHT: f32 = 64.0;
const BIG_WIDTH: f32 = 76.0;
const SMALL: f32 = 20.0;
const ROW_SPACING: f32 = 2.0;
const ROWS: usize = 3;
const RENDER_COLOR: Color32 = Color32::from_rgb(52, 140, 72);
pub const DANGER_COLOR: Color32 = Color32::from_rgb(168, 48, 48);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    DepthMap,
    Pyramid,
    Render,
    RotateLeft,
    RotateRight,
    Brush,
    Undo,
    Redo,
    NewStack,
    Save,
    Folder,
    Trash,
    /// A file that can't be processed.
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A normal button.
    Plain,
    /// A toggle, shown pressed when selected.
    Toggle(bool),
    /// The main action, in green.
    Primary,
}

/// A big button with an icon above a short label; as tall as three settings rows.
pub fn big_button(ui: &mut Ui, icon: Icon, label: &str, kind: Kind) -> Response {
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let size = vec2(BIG_WIDTH.max(galley.size().x + 16.0), HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let selected = matches!(kind, Kind::Toggle(true));
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        let (fill, stroke, fg) = match kind {
            Kind::Primary => {
                let fill = if !ui.is_enabled() {
                    RENDER_COLOR.gamma_multiply(0.4)
                } else if response.is_pointer_button_down_on() {
                    RENDER_COLOR.gamma_multiply(0.8)
                } else if response.hovered() {
                    Color32::from_rgb(64, 162, 86)
                } else {
                    RENDER_COLOR
                };
                (fill, Stroke::new(1.0, Color32::from_rgb(110, 200, 125)), Color32::WHITE)
            }
            _ => (visuals.weak_bg_fill, visuals.bg_stroke, visuals.text_color()),
        };
        let painter = ui.painter();
        painter.rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
        let icon_rect = Rect::from_center_size(pos2(rect.center().x, rect.top() + 23.0), Vec2::splat(26.0));
        paint_icon(painter, icon, icon_rect, fg, fill);
        let text_pos = pos2(rect.center().x - galley.size().x / 2.0, rect.bottom() - galley.size().y - 7.0);
        painter.galley(text_pos, galley, fg);
    }
    response
}

/// A small square button showing only an icon, with an accessible `name` and a tooltip.
pub fn icon_button(ui: &mut Ui, icon: Icon, name: &str, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SMALL), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        painter.rect(rect, 3.0, visuals.weak_bg_fill, visuals.bg_stroke, StrokeKind::Inside);
        paint_icon(painter, icon, rect.shrink(3.0), visuals.text_color(), visuals.weak_bg_fill);
    }
    response.on_hover_text(tooltip)
}

/// A toolbar line of fixed height whose items are centred vertically.
pub fn bar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let size = vec2(ui.available_width(), HEIGHT);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(HEIGHT);
        add(ui)
    })
    .inner
}

/// One settings row: adds a label cell and a control cell.
pub type Row<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// A small square button in red, for actions that delete something.
pub fn danger_button(ui: &mut Ui, icon: Icon, name: &str, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SMALL), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    if ui.is_rect_visible(rect) {
        let fill = if !ui.is_enabled() {
            DANGER_COLOR.gamma_multiply(0.4)
        } else if response.hovered() {
            Color32::from_rgb(196, 64, 64)
        } else {
            DANGER_COLOR
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, fill);
        paint_icon(painter, icon, rect.shrink(4.0), Color32::WHITE, fill);
    }
    response.on_hover_text(tooltip)
}

/// Lays out settings rows top to bottom, starting a new column after three rows. Each
/// row adds two grid cells (label and control), so the controls line up.
pub fn rows(ui: &mut Ui, id: &str, rows: Vec<Row<'_>>) {
    let mut rows = rows.into_iter().peekable();
    let mut column = 0;
    while rows.peek().is_some() {
        column_box(ui, |ui| {
            egui::Grid::new((id, column)).num_columns(2).spacing([8.0, ROW_SPACING]).min_row_height(row_height()).show(
                ui,
                |ui| {
                    for row in rows.by_ref().take(ROWS) {
                        row(ui);
                        ui.end_row();
                    }
                },
            );
        });
        column += 1;
    }
}

/// A section of up to three free-form rows, each laid out left to right.
pub fn stack(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    column_box(ui, |ui| {
        ui.spacing_mut().item_spacing.y = ROW_SPACING;
        add(ui);
    });
}

/// A top-down area exactly as tall as the toolbar. A fixed height keeps it centred in
/// the toolbar line (egui centres by the size it is given, not by what grows inside).
fn column_box(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let size = vec2(ui.available_width(), HEIGHT);
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_height(HEIGHT);
        add(ui);
    });
}

/// One row of a [`stack`].
pub fn row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), row_height()),
        egui::Layout::left_to_right(egui::Align::Center),
        add,
    );
}

fn row_height() -> f32 {
    (HEIGHT - ROW_SPACING * (ROWS - 1) as f32) / ROWS as f32
}

/// Paints `icon` into `r`; `bg` is the button fill, used to hide overlapped strokes.
pub fn paint_icon(painter: &egui::Painter, icon: Icon, r: Rect, color: Color32, bg: Color32) {
    let s = r.width();
    let p = |x: f32, y: f32| pos2(r.left() + x * s, r.top() + y * s);
    let stroke = Stroke::new((s / 13.0).max(1.4), color);
    match icon {
        Icon::DepthMap => {
            for i in 0..3 {
                let o = i as f32 * 0.17;
                let frame = Rect::from_min_size(p(0.08 + o, 0.08 + o), Vec2::splat(0.5 * s));
                if i > 0 {
                    painter.rect_filled(frame, 1.0, bg);
                }
                painter.rect_stroke(frame, 1.0, stroke, StrokeKind::Middle);
            }
        }
        Icon::Pyramid => {
            painter.add(Shape::closed_line(vec![p(0.5, 0.12), p(0.92, 0.86), p(0.08, 0.86)], stroke));
        }
        Icon::Render => {
            painter.add(Shape::convex_polygon(vec![p(0.22, 0.1), p(0.88, 0.5), p(0.22, 0.9)], color, Stroke::NONE));
        }
        Icon::RotateLeft | Icon::RotateRight => {
            let ccw = icon == Icon::RotateLeft;
            arrow_arc(painter, r.center(), 0.36 * s, 15.0, 270.0, ccw, stroke);
        }
        Icon::Undo | Icon::Redo => {
            arrow_arc(painter, p(0.5, 0.62), 0.36 * s, 165.0, 185.0, icon == Icon::Undo, stroke);
        }
        Icon::Brush => {
            painter.line_segment([p(0.88, 0.12), p(0.42, 0.58)], Stroke::new(stroke.width * 1.6, color));
            let tip = vec![p(0.36, 0.5), p(0.5, 0.64), p(0.32, 0.86), p(0.1, 0.9), p(0.14, 0.68)];
            painter.add(Shape::convex_polygon(tip, color, Stroke::NONE));
        }
        Icon::NewStack => {
            let page = Rect::from_min_max(p(0.16, 0.06), p(0.84, 0.94));
            painter.rect_stroke(page, 2.0, stroke, StrokeKind::Middle);
            painter.line_segment([p(0.5, 0.3), p(0.5, 0.7)], stroke);
            painter.line_segment([p(0.3, 0.5), p(0.7, 0.5)], stroke);
        }
        Icon::Folder => {
            let outline = vec![p(0.06, 0.2), p(0.38, 0.2), p(0.48, 0.32), p(0.94, 0.32), p(0.94, 0.84), p(0.06, 0.84)];
            painter.add(Shape::closed_line(outline, stroke));
            painter.line_segment([p(0.06, 0.44), p(0.94, 0.44)], stroke);
        }
        Icon::Blocked => {
            painter.circle_stroke(r.center(), 0.4 * s, stroke);
            painter.line_segment([p(0.22, 0.22), p(0.78, 0.78)], stroke);
        }
        Icon::Trash => {
            painter.line_segment([p(0.1, 0.22), p(0.9, 0.22)], stroke);
            painter.add(Shape::line(vec![p(0.36, 0.22), p(0.4, 0.08), p(0.6, 0.08), p(0.64, 0.22)], stroke));
            painter.add(Shape::closed_line(vec![p(0.2, 0.3), p(0.8, 0.3), p(0.73, 0.94), p(0.27, 0.94)], stroke));
            painter.line_segment([p(0.42, 0.42), p(0.43, 0.82)], stroke);
            painter.line_segment([p(0.58, 0.42), p(0.57, 0.82)], stroke);
        }
        Icon::Save => {
            painter.line_segment([p(0.5, 0.06), p(0.5, 0.62)], stroke);
            painter.add(Shape::line(vec![p(0.28, 0.42), p(0.5, 0.64), p(0.72, 0.42)], stroke));
            painter.add(Shape::line(vec![p(0.1, 0.66), p(0.1, 0.9), p(0.9, 0.9), p(0.9, 0.66)], stroke));
        }
    }
}

/// An arc from `start` sweeping `sweep` degrees (0° = right, angles grow clockwise on
/// screen) with an arrowhead at its end; mirrored horizontally if `mirror` is set.
fn arrow_arc(painter: &egui::Painter, c: Pos2, radius: f32, start: f32, sweep: f32, mirror: bool, stroke: Stroke) {
    let flip = |v: Vec2| if mirror { vec2(-v.x, v.y) } else { v };
    let at = |deg: f32| flip(Vec2::angled(deg.to_radians()));
    let steps = 24;
    let points: Vec<Pos2> = (0..=steps).map(|i| c + at(start + sweep * i as f32 / steps as f32) * radius).collect();
    let end = *points.last().unwrap();
    painter.add(Shape::line(points, stroke));
    // Arrowhead along the tangent at the end.
    let tangent = at(start + sweep + 90.0);
    let normal = at(start + sweep);
    let head = radius * 0.75;
    let tip = end + tangent * head * 0.6;
    let a = end + normal * head * 0.5;
    let b = end - normal * head * 0.5;
    painter.add(Shape::convex_polygon(vec![tip, a, b], stroke.color, Stroke::NONE));
}
