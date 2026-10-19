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
            return Some(Accepted {
                replace,
                insert: item.insert,
            });
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
                    ui.set_min_width(300.0);
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (i, item) in popup.completion.items.iter().enumerate() {
                                let response = row(ui, item, i == popup.selected);
                                if i == popup.selected && popup.scroll_to_selected {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                    ui.label(
                        RichText::new("↑↓ select · Enter/Tab insert · Esc close")
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
        Some(Accepted {
            replace,
            insert: item.insert,
        })
    }
}

fn row(ui: &mut Ui, item: &Item, selected: bool) -> egui::Response {
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
                _ => (icon::TEXT_T, egui::Color32::from_rgb(0x6F, 0xA8, 0xFF)),
            };
            (glyph, crate::theme::icon_tint(c))
        }
    };
    let text = match &item.detail {
        Some(d) => format!("{glyph}  {}   {d}", item.label),
        None => format!("{glyph}  {}", item.label),
    };
    let label = egui::Button::selectable(
        selected,
        RichText::new(text).color(if selected {
            egui::Color32::WHITE
        } else {
            c.lerp_to_gamma(color::text(), 0.5)
        }),
    );
    ui.add_sized([ui.available_width(), 20.0], label)
}

/// Replaces the accepted range in `text` and moves the editor cursor after the insertion.
pub fn apply(accepted: Accepted, text: &mut String, ctx: &egui::Context, editor_id: Id) {
    let Accepted { replace, insert } = accepted;
    if replace.end > text.len()
        || !text.is_char_boundary(replace.start)
        || !text.is_char_boundary(replace.end)
    {
        return;
    }
    text.replace_range(replace.clone(), &insert);
    let cursor_chars = text[..replace.start + insert.len()].chars().count();
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, editor_id) {
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(cursor_chars))));
        state.store(ctx, editor_id);
    }
    ctx.memory_mut(|m| m.request_focus(editor_id));
}
