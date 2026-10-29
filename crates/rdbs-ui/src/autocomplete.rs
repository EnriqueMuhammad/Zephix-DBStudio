//! Autocomplete popup for the SQL editor. Suggestions come from
//! [`rdbs_core::complete::complete`]; this module handles keys, rendering and insertion.

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditOutput;
use egui::{Id, Key, KeyboardShortcut, Modifiers, Order, Pos2, RichText, Ui};
use egui_phosphor::regular as icon;
use rdbs_core::complete::{Catalog, Completion, Item, ItemKind, complete};
use rdbs_core::{DatabaseKind, DbObject};

use crate::theme::color;
use crate::widgets::char_range_to_bytes;

const SC_COMPLETE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Space);

/// Open popup.
pub struct Popup {
    completion: Completion,
    selected: usize,
    anchor: Pos2,
    /// Scroll the selected row into view on the next draw.
    scroll_to_selected: bool,
}

/// Per-tab autocomplete state.
#[derive(Default)]
pub struct AutocompleteState {
    popup: Option<Popup>,
    /// Ctrl+Space pressed: complete even with an empty prefix.
    forced: bool,
}

/// An item the user accepted, to be applied to the editor text.
pub struct Accepted {
    replace: std::ops::Range<usize>,
    insert: String,
    /// Byte offset in `insert` for the caret (templates); `None` is after the insertion.
    caret: Option<usize>,
}

impl Accepted {
    fn new(replace: std::ops::Range<usize>, item: Item) -> Self {
        Self {
            replace,
            insert: item.insert,
            caret: item.caret,
        }
    }
}

impl AutocompleteState {
    /// Whether the popup is visible.
    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Handles navigation keys before the text editor sees them. Returns an accepted item.
    pub fn handle_keys(&mut self, ui: &mut Ui, editor_has_focus: bool) -> Option<Accepted> {
        if !editor_has_focus {
            // Focus moves to the popup while an item is being clicked; `show` closes the
            // popup on clicks elsewhere.
            return None;
        }
        if ui.input_mut(|i| i.consume_shortcut(&SC_COMPLETE)) {
            self.forced = true;
        }
        let popup = self.popup.as_mut()?;
        let count = popup.completion.items.len();
        let (down, up, escape, accept) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::Enter)
                    || i.consume_key(Modifiers::NONE, Key::Tab),
            )
        });
        if down {
            popup.selected = (popup.selected + 1) % count;
            popup.scroll_to_selected = true;
        }
        if up {
            popup.selected = (popup.selected + count - 1) % count;
            popup.scroll_to_selected = true;
        }
        if escape {
            self.popup = None;
            return None;
        }
        if accept {
            let item = popup.completion.items.get(popup.selected)?.clone();
            let replace = popup.completion.replace.clone();
            self.popup = None;
            return Some(Accepted::new(replace, item));
        }
        None
    }

    /// Recomputes suggestions after the editor ran: on text changes, after Ctrl+Space, and
    /// closes the popup when the cursor left the completed word.
    pub fn update(
        &mut self,
        output: &TextEditOutput,
        sql: &str,
        objects: &[DbObject],
        kind: DatabaseKind,
    ) {
        let Some(cursor) = output.cursor_range.map(|r| r.primary) else {
            self.popup = None;
            return;
        };
        let has_selection = output
            .cursor_range
            .is_some_and(|r| r.primary.index != r.secondary.index);
        let cursor_byte = char_range_to_bytes(sql, cursor.index.0, cursor.index.0).start;
        let changed = output.response.response.changed();
        let forced = std::mem::take(&mut self.forced);

        if (changed || forced) && !has_selection {
            let catalog = Catalog::from_objects(objects);
            self.popup = complete(sql, cursor_byte, &catalog, kind, forced).map(|completion| {
                let caret = output.galley.pos_from_cursor(cursor);
                Popup {
                    completion,
                    selected: 0,
                    anchor: output.galley_pos + caret.left_bottom().to_vec2(),
                    scroll_to_selected: true,
                }
            });
        } else if self
            .popup
            .as_ref()
            .is_some_and(|p| p.completion.replace.end != cursor_byte || has_selection)
        {
            self.popup = None;
        }
    }

    /// Draws the popup; returns an item clicked with the mouse.
    pub fn show(&mut self, ui: &Ui, id: Id) -> Option<Accepted> {
        let popup = self.popup.as_mut()?;
        let mut clicked = None;
        let area = egui::Area::new(id)
            .order(Order::Foreground)
            .fixed_pos(popup.anchor)
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let columns = Columns::measure(ui, &popup.completion.items);
                    ui.set_width(columns.width());
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, item) in popup.completion.items.iter().enumerate() {
                                let response = row(ui, &columns, item, i == popup.selected);
                                if i == popup.selected && popup.scroll_to_selected {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                    ui.add_space(4.0);
                    // Arrows from the icon font: the text fonts have no arrow glyphs.
                    ui.label(
                        RichText::new(format!(
                            "{}{} select · Enter/Tab insert · Esc close",
                            icon::ARROW_UP,
                            icon::ARROW_DOWN
                        ))
                        .size(10.5)
                        .color(color::text_faint()),
                    );
                });
            });
        popup.scroll_to_selected = false;
        let pressed_outside = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p))
        });
        if clicked.is_none() && pressed_outside {
            self.popup = None;
            return None;
        }
        let index = clicked?;
        let item = popup.completion.items.get(index)?.clone();
        let replace = popup.completion.replace.clone();
        self.popup = None;
        Some(Accepted::new(replace, item))
    }
}

const ROW_HEIGHT: f32 = 22.0;
const PAD: f32 = 8.0;
const ICON_WIDTH: f32 = 22.0;

/// Widths of the popup's two columns: the name on the left, and where it comes from and what
/// it is on the right, behind a vertical divider so the details line up.
struct Columns {
    label: f32,
    detail: f32,
}

impl Columns {
    fn measure(ui: &Ui, items: &[Item]) -> Self {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let width = |text: String, size: f32| {
            let font = egui::FontId::new(size, font.family.clone());
            ui.fonts_mut(|f| f.layout_no_wrap(text, font, egui::Color32::WHITE).size().x)
        };
        let label = items
            .iter()
            .map(|i| width(i.label.clone(), font.size))
            .fold(0.0, f32::max);
        let detail = items
            .iter()
            .map(|i| width(detail_text(i), font.size * DETAIL_SCALE))
            .fold(0.0, f32::max);
        Self {
            label: label.clamp(140.0, 420.0),
            detail: detail.clamp(56.0, 240.0),
        }
    }

    fn width(&self) -> f32 {
        PAD + ICON_WIDTH + self.label + 2.0 * PAD + self.detail + PAD
    }
}

const DETAIL_SCALE: f32 = 0.9;

/// Right column text: the owning schema or table, then the kind of item.
fn detail_text(item: &Item) -> String {
    let kind = match item.kind {
        ItemKind::Snippet => None,
        ItemKind::Column => Some("column"),
        ItemKind::Table => Some("table"),
        ItemKind::View => Some("view"),
        ItemKind::Schema => Some("schema"),
        ItemKind::Keyword => Some("keyword"),
    };
    match (&item.detail, kind) {
        (Some(d), Some(k)) => format!("{d} · {k}"),
        (Some(d), None) => d.clone(),
        (None, k) => k.unwrap_or_default().to_owned(),
    }
}

fn row(ui: &mut Ui, columns: &Columns, item: &Item, selected: bool) -> egui::Response {
    let (glyph, c) = match item.kind {
        ItemKind::Column => (icon::COLUMNS, color::text_dim()),
        kind => {
            let (glyph, c) = match kind {
                ItemKind::Table => (icon::TABLE, egui::Color32::from_rgb(0x5E, 0xA2, 0xFF)),
                ItemKind::View => (icon::EYE, egui::Color32::from_rgb(0x4F, 0xC1, 0xB0)),
                ItemKind::Schema => (
                    icon::SQUARES_FOUR,
                    egui::Color32::from_rgb(0xB3, 0x8B, 0xF2),
                ),
                ItemKind::Snippet => (
                    icon::BRACKETS_CURLY,
                    egui::Color32::from_rgb(0xE5, 0xB5, 0x67),
                ),
                _ => (icon::TEXT_T, egui::Color32::from_rgb(0x6F, 0xA8, 0xFF)),
            };
            (glyph, crate::theme::icon_tint(c))
        }
    };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        egui::Sense::click(),
    );
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 4, ui.visuals().selection.bg_fill);
    } else if response.hovered() {
        painter.rect_filled(rect, 4, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let y = rect.center().y;
    let (glyph_color, label_color, detail_color) = if selected {
        let white = egui::Color32::WHITE;
        (white, white, white.gamma_multiply(0.75))
    } else {
        (c, c.lerp_to_gamma(color::text(), 0.5), color::text_faint())
    };
    painter.text(
        egui::pos2(rect.left() + PAD, y),
        egui::Align2::LEFT_CENTER,
        glyph,
        font.clone(),
        glyph_color,
    );
    painter.text(
        egui::pos2(rect.left() + PAD + ICON_WIDTH, y),
        egui::Align2::LEFT_CENTER,
        &item.label,
        font.clone(),
        label_color,
    );
    let divider = rect.right() - 2.0 * PAD - columns.detail;
    painter.vline(
        divider,
        rect.shrink2(egui::vec2(0.0, 4.0)).y_range(),
        egui::Stroke::new(1.0, color::text_faint().gamma_multiply(0.5)),
    );
    painter.text(
        egui::pos2(rect.right() - PAD, y),
        egui::Align2::RIGHT_CENTER,
        detail_text(item),
        egui::FontId::new(font.size * DETAIL_SCALE, font.family),
        detail_color,
    );
    response
}

/// Replaces the accepted range in `text` and moves the editor cursor after the insertion.
pub fn apply(accepted: Accepted, text: &mut String, ctx: &egui::Context, editor_id: Id) {
    let Accepted {
        replace,
        insert,
        caret,
    } = accepted;
    if replace.end > text.len()
        || !text.is_char_boundary(replace.start)
        || !text.is_char_boundary(replace.end)
    {
        return;
    }
    text.replace_range(replace.clone(), &insert);
    let caret = caret
        .filter(|&c| insert.is_char_boundary(c))
        .unwrap_or(insert.len());
    let cursor_chars = text[..replace.start + caret].chars().count();
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, editor_id) {
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(cursor_chars))));
        state.store(ctx, editor_id);
    }
    ctx.memory_mut(|m| m.request_focus(editor_id));
}
