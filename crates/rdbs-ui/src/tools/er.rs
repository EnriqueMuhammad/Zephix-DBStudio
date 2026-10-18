//! ER diagram: tables of a database/schema as boxes with their columns, foreign keys as
//! connectors. Pan by dragging the background or scrolling, zoom with Ctrl+wheel, drag tables
//! to arrange them (positions are remembered), double-click a table to design it.

use std::collections::HashMap;

use egui::epaint::CubicBezierShape;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};
use egui_phosphor::regular as icon;
use rdbs_core::{Event, RequestId, TableSchema};
use serde::{Deserialize, Serialize};

use super::{Scope, ToolLaunch, error_box, error_text, header};
use crate::model::{StatusMessage, UiAction};
use crate::theme::color;
use crate::workspace::TabContext;

const BOX_WIDTH: f32 = 240.0;
const HEADER_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = 18.0;
const MAX_ROWS: usize = 25;
const GAP_X: f32 = 90.0;
const GAP_Y: f32 = 36.0;

/// ER diagram state; table positions are persisted.
#[derive(Serialize, Deserialize)]
pub struct Er {
    /// Database/schema shown.
    pub scope: Scope,
    /// World positions of table boxes by table key.
    #[serde(default)]
    positions: HashMap<String, [f32; 2]>,
    #[serde(skip)]
    rt: Runtime,
}

#[derive(Default)]
struct Runtime {
    started: bool,
    describe: Option<RequestId>,
    tables: Vec<TableSchema>,
    error: Option<String>,
    /// World coordinate shown at the canvas' top-left corner.
    offset: Vec2,
    zoom: f32,
    dragging: Option<String>,
    selected: Option<String>,
    filter: String,
    fit: bool,
}

fn key(t: &TableSchema) -> String {
    match &t.schema {
        Some(s) => format!("{s}.{}", t.name),
        None => t.name.clone(),
    }
}

fn shown_rows(t: &TableSchema) -> usize {
    let n = t.columns.len();
    if n > MAX_ROWS { MAX_ROWS + 1 } else { n }
}

fn box_size(t: &TableSchema) -> Vec2 {
    vec2(
        BOX_WIDTH,
        HEADER_HEIGHT + shown_rows(t) as f32 * ROW_HEIGHT + 6.0,
    )
}

/// The table a foreign key points at.
fn referenced<'a>(
    tables: &'a [TableSchema],
    from: &TableSchema,
    fk: &rdbs_core::ForeignKeyDef,
) -> Option<&'a TableSchema> {
    let schema = fk.ref_schema.as_ref().or(from.schema.as_ref());
    tables
        .iter()
        .find(|t| t.name == fk.ref_table && t.schema.as_ref() == schema)
        .or_else(|| tables.iter().find(|t| t.name == fk.ref_table))
}

/// Layered layout: referenced tables to the left of the tables that reference them; tables
/// without relationships in a grid on the right.
pub fn auto_layout(tables: &[TableSchema]) -> HashMap<String, [f32; 2]> {
    let mut level: HashMap<String, usize> = HashMap::new();
    fn depth(
        t: &TableSchema,
        tables: &[TableSchema],
        memo: &mut HashMap<String, usize>,
        guard: usize,
    ) -> usize {
        if let Some(d) = memo.get(&key(t)) {
            return *d;
        }
        if guard > tables.len() {
            return 0; // cycle
        }
        let d = t
            .foreign_keys
            .iter()
            .filter_map(|fk| referenced(tables, t, fk))
            .filter(|r| key(r) != key(t))
            .map(|r| depth(r, tables, memo, guard + 1) + 1)
            .max()
            .unwrap_or(0);
        memo.insert(key(t), d);
        d
    }
    let related: std::collections::HashSet<String> = tables
        .iter()
        .flat_map(|t| {
            t.foreign_keys
                .iter()
                .filter_map(|fk| referenced(tables, t, fk))
                .map(key)
                .chain((!t.foreign_keys.is_empty()).then(|| key(t)))
                .collect::<Vec<_>>()
        })
        .collect();
    for t in tables.iter().filter(|t| related.contains(&key(t))) {
        let d = depth(t, tables, &mut level, 0);
        level.insert(key(t), d);
    }
    let mut out = HashMap::new();
    let levels = level.values().copied().max().map_or(0, |m| m + 1);
    for l in 0..levels {
        let mut y = 0.0;
        let mut column: Vec<&TableSchema> = tables
            .iter()
            .filter(|t| level.get(&key(t)) == Some(&l))
            .collect();
        column.sort_by_key(|t| key(t));
        for t in column {
            out.insert(key(t), [l as f32 * (BOX_WIDTH + GAP_X), y]);
            y += box_size(t).y + GAP_Y;
        }
    }
    // Unrelated tables: grid to the right.
    let loose: Vec<&TableSchema> = tables
        .iter()
        .filter(|t| !related.contains(&key(t)))
        .collect();
    let per_column = (loose.len() as f32).sqrt().ceil().max(1.0) as usize;
    let base_x = levels as f32 * (BOX_WIDTH + GAP_X);
    for (i, chunk) in loose.chunks(per_column).enumerate() {
        let mut y = 0.0;
        for t in chunk {
            out.insert(key(t), [base_x + i as f32 * (BOX_WIDTH + GAP_X), y]);
            y += box_size(t).y + GAP_Y;
        }
    }
    out
}

impl Er {
    /// Diagram of `scope`.
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            positions: HashMap::new(),
            rt: Runtime::default(),
        }
    }

    /// Tab title.
    pub fn title(&self) -> String {
        let place = self
            .scope
            .schema
            .clone()
            .or_else(|| self.scope.database.clone())
            .unwrap_or_else(|| "main".into());
        format!("ER {place}")
    }

    /// Consumes this tool's events.
    pub fn on_event(&mut self, event: &Event) -> bool {
        match event {
            Event::SchemaDescribed { request_id, result }
                if self.rt.describe == Some(*request_id) =>
            {
                self.rt.describe = None;
                match result {
                    Ok(tables) => {
                        self.rt.tables = tables.clone();
                        self.rt.error = None;
                        let missing = tables.iter().any(|t| !self.positions.contains_key(&key(t)));
                        if missing {
                            let layout = auto_layout(tables);
                            for t in tables {
                                self.positions.entry(key(t)).or_insert_with(|| {
                                    layout.get(&key(t)).copied().unwrap_or([0.0, 0.0])
                                });
                            }
                        }
                        self.rt.fit = true;
                    }
                    Err(e) => self.rt.error = Some(error_text(e)),
                }
                true
            }
            _ => false,
        }
    }

    fn rect_of(&self, t: &TableSchema) -> Rect {
        let p = self.positions.get(&key(t)).copied().unwrap_or([0.0, 0.0]);
        Rect::from_min_size(pos2(p[0], p[1]), box_size(t))
    }

    fn bounds(&self) -> Option<Rect> {
        self.rt
            .tables
            .iter()
            .map(|t| self.rect_of(t))
            .reduce(|a, b| a.union(b))
    }

    /// Renders the tool.
    pub fn show(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        if !self.rt.started {
            self.rt.started = true;
            self.rt.zoom = 1.0;
            self.rt.describe = super::describe(cx, &self.scope);
        }
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    header(ui, icon::GRAPH, "ER diagram", &self.scope.label(cx));
                    ui.separator();
                    if ui
                        .button(format!("{}  Refresh", icon::ARROWS_CLOCKWISE))
                        .clicked()
                    {
                        self.rt.describe = super::describe(cx, &self.scope);
                    }
                    if ui.button(format!("{}  Auto Layout", icon::TREE_STRUCTURE)).clicked() {
                        self.positions = auto_layout(&self.rt.tables);
                        self.rt.fit = true;
                    }
                    if ui.button(icon::MAGNIFYING_GLASS_MINUS).on_hover_text("Zoom out").clicked() {
                        self.rt.zoom = (self.rt.zoom / 1.2).max(0.2);
                    }
                    ui.label(format!("{:.0}%", self.rt.zoom * 100.0));
                    if ui.button(icon::MAGNIFYING_GLASS_PLUS).on_hover_text("Zoom in").clicked() {
                        self.rt.zoom = (self.rt.zoom * 1.2).min(3.0);
                    }
                    if ui.button(format!("{}  Fit", icon::ARROWS_OUT)).clicked() {
                        self.rt.fit = true;
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rt.filter)
                            .hint_text("highlight table…")
                            .desired_width(140.0),
                    );
                    if ui
                        .add_enabled(!self.rt.tables.is_empty(), egui::Button::new(format!("{}  Export SVG", icon::EXPORT)))
                        .clicked()
                    {
                        self.export_svg(cx);
                    }
                    if self.rt.describe.is_some() {
                        ui.spinner();
                    }
                });
                if let Some(e) = &self.rt.error {
                    error_box(ui, e);
                }
                ui.label(
                    RichText::new(format!(
                        "{} table(s)  ·  drag to move  ·  Ctrl+wheel to zoom  ·  double-click to design",
                        self.rt.tables.len()
                    ))
                    .small()
                    .color(color::TEXT_FAINT),
                );
                self.canvas(ui, cx);
            });
    }

    fn canvas(&mut self, ui: &mut Ui, cx: &mut TabContext<'_>) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, color::EDITOR);

        if self.rt.zoom <= 0.0 {
            self.rt.zoom = 1.0;
        }
        if std::mem::take(&mut self.rt.fit)
            && let Some(bounds) = self.bounds()
        {
            let margin = 40.0;
            let zx = (rect.width() - margin * 2.0) / bounds.width().max(1.0);
            let zy = (rect.height() - margin * 2.0) / bounds.height().max(1.0);
            self.rt.zoom = zx.min(zy).clamp(0.2, 1.2);
            self.rt.offset = bounds.min.to_vec2() - Vec2::splat(margin / self.rt.zoom);
        }
        let zoom = self.rt.zoom;
        let to_world = |p: Pos2| pos2(0.0, 0.0) + (p - rect.min) / zoom + self.rt.offset;

        // Zoom around the pointer, pan with the wheel.
        if response.hovered() {
            let (zoom_delta, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if (zoom_delta - 1.0).abs() > f32::EPSILON
                && let Some(pointer) = response.hover_pos()
            {
                let before = to_world(pointer);
                self.rt.zoom = (self.rt.zoom * zoom_delta).clamp(0.2, 3.0);
                self.rt.offset = before.to_vec2() - (pointer - rect.min) / self.rt.zoom;
            } else if scroll != Vec2::ZERO {
                self.rt.offset -= scroll / self.rt.zoom;
            }
        }
        let zoom = self.rt.zoom;

        // Hit test (topmost = last drawn).
        let hit = |pos: Pos2, er: &Er| -> Option<String> {
            let world = pos2(0.0, 0.0) + (pos - rect.min) / zoom + er.rt.offset;
            er.rt
                .tables
                .iter()
                .rev()
                .find(|t| er.rect_of(t).contains(world))
                .map(key)
        };
        if response.drag_started()
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.rt.dragging = hit(pos, self);
            if let Some(k) = &self.rt.dragging {
                self.rt.selected = Some(k.clone());
            }
        }
        if response.dragged() {
            let delta = response.drag_delta() / zoom;
            match &self.rt.dragging {
                Some(k) => {
                    if let Some(p) = self.positions.get_mut(k) {
                        p[0] += delta.x;
                        p[1] += delta.y;
                    }
                }
                None => self.rt.offset -= delta,
            }
        }
        if response.drag_stopped() {
            self.rt.dragging = None;
        }
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.rt.selected = hit(pos, self);
        }
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(k) = hit(pos, self)
            && let Some(t) = self.rt.tables.iter().find(|t| key(t) == k)
        {
            cx.actions.push(UiAction::OpenTool(ToolLaunch::Designer {
                scope: Scope {
                    schema: t.schema.clone().or(self.scope.schema.clone()),
                    ..self.scope.clone()
                },
                table: Some(t.name.clone()),
            }));
        }

        let offset = self.rt.offset;
        let to_screen = move |p: Pos2| rect.min + (p.to_vec2() - offset) * zoom;
        let filter = self.rt.filter.trim().to_lowercase();
        let focus = self.rt.selected.clone();
        let involved = |t: &TableSchema, other: &TableSchema| {
            focus
                .as_ref()
                .is_some_and(|f| *f == key(t) || *f == key(other))
        };

        // Relationships.
        for t in &self.rt.tables {
            for fk in &t.foreign_keys {
                let Some(parent) = referenced(&self.rt.tables, t, fk) else {
                    continue;
                };
                let child_rect = self.rect_of(t);
                let parent_rect = self.rect_of(parent);
                let row_y = |table: &TableSchema, r: Rect, col: Option<&String>| {
                    let i = col
                        .and_then(|c| table.columns.iter().position(|x| &x.name == c))
                        .filter(|i| *i < MAX_ROWS);
                    match i {
                        Some(i) => r.top() + HEADER_HEIGHT + (i as f32 + 0.5) * ROW_HEIGHT,
                        None => r.top() + HEADER_HEIGHT / 2.0,
                    }
                };
                let cy = row_y(t, child_rect, fk.columns.first());
                let py = row_y(parent, parent_rect, fk.ref_columns.first());
                let highlight = involved(t, parent);
                let stroke = Stroke::new(
                    if highlight { 2.0 } else { 1.2 },
                    if highlight {
                        color::ACCENT
                    } else {
                        color::TEXT_FAINT
                    },
                );
                let (start, end, out_dir, in_dir) = if key(t) == key(parent) {
                    let x = child_rect.right();
                    (pos2(x, cy), pos2(x, py), 1.0, 1.0)
                } else if child_rect.center().x >= parent_rect.center().x {
                    (
                        pos2(child_rect.left(), cy),
                        pos2(parent_rect.right(), py),
                        -1.0,
                        1.0,
                    )
                } else {
                    (
                        pos2(child_rect.right(), cy),
                        pos2(parent_rect.left(), py),
                        1.0,
                        -1.0,
                    )
                };
                let bend = 60.0 + if key(t) == key(parent) { 30.0 } else { 0.0 };
                let points = [
                    to_screen(start),
                    to_screen(start + vec2(out_dir * bend, 0.0)),
                    to_screen(end + vec2(in_dir * bend, 0.0)),
                    to_screen(end),
                ];
                painter.add(CubicBezierShape::from_points_stroke(
                    points,
                    false,
                    Color32::TRANSPARENT,
                    stroke,
                ));
                // Crow's foot (many) at the child, bar (one) at the parent.
                let s = to_screen(start);
                let d = out_dir * 10.0 * zoom;
                for dy in [-5.0 * zoom, 0.0, 5.0 * zoom] {
                    painter.line_segment([pos2(s.x + d, s.y), pos2(s.x, s.y + dy)], stroke);
                }
                let e = to_screen(end);
                let bar_x = e.x + in_dir * 8.0 * zoom;
                painter.line_segment(
                    [pos2(bar_x, e.y - 5.0 * zoom), pos2(bar_x, e.y + 5.0 * zoom)],
                    stroke,
                );
            }
        }

        // Tables.
        let text = |size: f32| FontId::proportional(size * zoom);
        for t in &self.rt.tables {
            let world = self.rect_of(t);
            let r = Rect::from_min_max(to_screen(world.min), to_screen(world.max));
            if !r.intersects(rect) {
                continue;
            }
            let k = key(t);
            let selected = focus.as_ref() == Some(&k);
            let matched = !filter.is_empty() && t.name.to_lowercase().contains(&filter);
            let border = if selected {
                color::ACCENT
            } else if matched {
                color::WARNING
            } else {
                color::BORDER
            };
            painter.rect_filled(r, 6.0 * zoom, color::SURFACE);
            painter.rect_stroke(
                r,
                6.0 * zoom,
                Stroke::new(if selected || matched { 2.0 } else { 1.0 }, border),
                egui::StrokeKind::Inside,
            );
            let head = Rect::from_min_size(r.min, vec2(r.width(), HEADER_HEIGHT * zoom));
            painter.rect_filled(
                head,
                egui::CornerRadius {
                    nw: (6.0 * zoom) as u8,
                    ne: (6.0 * zoom) as u8,
                    sw: 0,
                    se: 0,
                },
                color::ACCENT.gamma_multiply(if selected { 0.55 } else { 0.3 }),
            );
            painter.text(
                head.left_center() + vec2(8.0 * zoom, 0.0),
                Align2::LEFT_CENTER,
                format!("{}  {}", icon::TABLE, t.name),
                text(13.0),
                color::TEXT,
            );
            if zoom < 0.45 {
                continue;
            }
            for (i, c) in t.columns.iter().take(MAX_ROWS).enumerate() {
                let y = r.top() + (HEADER_HEIGHT + (i as f32 + 0.5) * ROW_HEIGHT) * zoom;
                let is_pk = t.primary_key.contains(&c.name);
                let is_fk = t.foreign_keys.iter().any(|fk| fk.columns.contains(&c.name));
                let (glyph, glyph_color) = if is_pk {
                    (icon::KEY, color::WARNING)
                } else if is_fk {
                    (icon::LINK, color::ACCENT)
                } else {
                    (icon::DOT_OUTLINE, color::TEXT_FAINT)
                };
                painter.text(
                    pos2(r.left() + 10.0 * zoom, y),
                    Align2::LEFT_CENTER,
                    glyph,
                    text(11.0),
                    glyph_color,
                );
                painter.text(
                    pos2(r.left() + 26.0 * zoom, y),
                    Align2::LEFT_CENTER,
                    &c.name,
                    text(12.0),
                    color::TEXT,
                );
                let ty: String = c.data_type.chars().take(22).collect();
                painter.text(
                    pos2(r.right() - 8.0 * zoom, y),
                    Align2::RIGHT_CENTER,
                    ty,
                    text(11.0),
                    color::TEXT_FAINT,
                );
            }
            if t.columns.len() > MAX_ROWS {
                let y = r.top() + (HEADER_HEIGHT + (MAX_ROWS as f32 + 0.5) * ROW_HEIGHT) * zoom;
                painter.text(
                    pos2(r.left() + 26.0 * zoom, y),
                    Align2::LEFT_CENTER,
                    format!("… {} more", t.columns.len() - MAX_ROWS),
                    text(11.0),
                    color::TEXT_FAINT,
                );
            }
        }
        if self.rt.tables.is_empty() && self.rt.describe.is_none() && self.rt.error.is_none() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No tables in this schema",
                FontId::proportional(14.0),
                color::TEXT_DIM,
            );
        }
    }

    /// The diagram as an SVG document (world coordinates).
    pub fn svg(&self) -> String {
        let esc = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
        };
        let bounds = self
            .bounds()
            .unwrap_or(Rect::from_min_size(Pos2::ZERO, vec2(100.0, 100.0)))
            .expand(40.0);
        let mut out = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\" \
             font-family=\"Segoe UI, Helvetica, Arial, sans-serif\">\n\
             <rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#ffffff\"/>\n",
            bounds.min.x,
            bounds.min.y,
            bounds.width(),
            bounds.height(),
            bounds.min.x,
            bounds.min.y,
            bounds.width(),
            bounds.height()
        );
        for t in &self.rt.tables {
            for fk in &t.foreign_keys {
                let Some(parent) = referenced(&self.rt.tables, t, fk) else {
                    continue;
                };
                let (c, p) = (self.rect_of(t), self.rect_of(parent));
                let (sx, ex, d) = if c.center().x >= p.center().x {
                    (c.left(), p.right(), -1.0)
                } else {
                    (c.right(), p.left(), 1.0)
                };
                let (sy, ey) = (c.top() + HEADER_HEIGHT / 2.0, p.top() + HEADER_HEIGHT / 2.0);
                out.push_str(&format!(
                    "<path d=\"M{sx},{sy} C{},{sy} {},{ey} {ex},{ey}\" stroke=\"#7a8494\" fill=\"none\"/>\n",
                    sx + d * 60.0,
                    ex - d * 60.0
                ));
            }
        }
        for t in &self.rt.tables {
            let r = self.rect_of(t);
            out.push_str(&format!(
                "<g><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"6\" fill=\"#f7f9fc\" stroke=\"#9aa4b2\"/>\n\
                 <rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{HEADER_HEIGHT}\" rx=\"6\" fill=\"#d6e6ff\"/>\n\
                 <text x=\"{}\" y=\"{}\" font-size=\"13\" font-weight=\"bold\">{}</text>\n",
                r.min.x, r.min.y, r.width(), r.height(),
                r.min.x, r.min.y, r.width(),
                r.min.x + 8.0, r.min.y + 17.0, esc(&t.name)
            ));
            for (i, c) in t.columns.iter().take(MAX_ROWS).enumerate() {
                let y = r.min.y + HEADER_HEIGHT + (i as f32 + 0.5) * ROW_HEIGHT + 4.0;
                let marker = if t.primary_key.contains(&c.name) {
                    "PK "
                } else {
                    ""
                };
                out.push_str(&format!(
                    "<text x=\"{}\" y=\"{y}\" font-size=\"12\">{marker}{}</text>\
                     <text x=\"{}\" y=\"{y}\" font-size=\"11\" fill=\"#6b7480\" text-anchor=\"end\">{}</text>\n",
                    r.min.x + 10.0,
                    esc(&c.name),
                    r.max.x - 8.0,
                    esc(&c.data_type)
                ));
            }
            out.push_str("</g>\n");
        }
        out.push_str("</svg>\n");
        out
    }

    fn export_svg(&self, cx: &mut TabContext<'_>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("SVG image", &["svg"])
            .set_file_name(format!("{}.svg", self.title().replace(' ', "_")))
            .save_file()
        else {
            return;
        };
        let status = match std::fs::write(&path, self.svg()) {
            Ok(()) => StatusMessage::info(format!("Saved diagram to {}", path.display())),
            Err(e) => StatusMessage::error(format!("Could not save {}: {e}", path.display())),
        };
        cx.actions.push(UiAction::Status(status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdbs_core::{ColumnDef, ForeignKeyDef, ProfileId};

    fn t(name: &str, refs: &[&str]) -> TableSchema {
        TableSchema {
            schema: Some("public".into()),
            name: name.into(),
            columns: vec![ColumnDef::new("id", "integer")],
            primary_key: vec!["id".into()],
            foreign_keys: refs
                .iter()
                .map(|r| ForeignKeyDef {
                    name: format!("fk_{name}_{r}"),
                    columns: vec!["id".into()],
                    ref_schema: None,
                    ref_table: (*r).into(),
                    ref_columns: vec!["id".into()],
                })
                .collect(),
            ..TableSchema::default()
        }
    }

    #[test]
    fn referenced_tables_are_left_of_referencing_ones() {
        let tables = vec![
            t("order_items", &["orders", "products"]),
            t("orders", &["customers"]),
            t("customers", &[]),
            t("products", &[]),
            t("audit_log", &[]),
        ];
        let layout = auto_layout(&tables);
        let x = |n: &str| layout[&format!("public.{n}")][0];
        assert!(x("customers") < x("orders"));
        assert!(x("orders") < x("order_items"));
        assert!(x("products") < x("order_items"));
        assert!(
            x("audit_log") > x("order_items"),
            "unrelated tables go right"
        );
    }

    #[test]
    fn cycles_do_not_hang_and_svg_escapes_names() {
        let tables = vec![t("a", &["b"]), t("b", &["a"]), t("c&d", &["c&d"])];
        let layout = auto_layout(&tables);
        assert_eq!(layout.len(), 3);
        let mut er = Er::new(Scope {
            profile_id: ProfileId::new(),
            database: None,
            schema: Some("public".into()),
        });
        er.rt.tables = tables;
        er.positions = layout;
        let svg = er.svg();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("c&amp;d"));
        assert_eq!(svg.matches("<path").count(), 3);
    }
}
