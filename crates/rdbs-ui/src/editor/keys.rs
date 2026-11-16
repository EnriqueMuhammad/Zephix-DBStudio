//! Editor keyboard handling: two-key chords, shortcuts, auto-closing pairs and selection.

use super::*;

/// First key of a two-key shortcut (Ctrl+K, Ctrl+C), waiting for the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Chord {
    CtrlK,
    CtrlE,
    ShiftAltL,
}

pub(super) fn chord_id() -> Id {
    Id::new("editor-chord")
}

/// Whether an editor waits for the second key of a shortcut; the global shortcuts then leave
/// the keyboard to it (Ctrl+E, Ctrl+W toggles word wrap instead of closing the tab).
pub fn chord_pending(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<(Id, Chord)>(chord_id()).is_some())
}

/// Forgets a chord started in `editor_id` once that editor lost focus.
pub(super) fn clear_chord(ctx: &egui::Context, editor_id: Id) {
    ctx.data_mut(|d| {
        if d.get_temp::<(Id, Chord)>(chord_id())
            .is_some_and(|(owner, _)| owner == editor_id)
        {
            d.remove::<(Id, Chord)>(chord_id());
        }
    });
}

/// Removes a key press with exactly these modifiers from this frame's input.
pub(super) fn take_key(i: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let found = i.events.iter().position(|e| {
        matches!(e, Event::Key { key: k, pressed: true, modifiers: m, .. }
            if *k == key && m.matches_exact(modifiers))
    });
    found.map(|p| i.events.remove(p)).is_some()
}

/// Editor shortcuts, read before the text editor so it does not act on the same keys (it
/// binds Ctrl+K and Ctrl+U to line deletions).
pub(super) fn shortcut_action(ui: &mut Ui, editor_id: Id, text: &str) -> Option<EditorAction> {
    use EditorAction as A;
    const CTRL: Modifiers = Modifiers::COMMAND;
    let ctrl_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
    let ctrl_alt = Modifiers::COMMAND.plus(Modifiers::ALT);
    let shift_alt = Modifiers::SHIFT.plus(Modifiers::ALT);
    let ctx = ui.ctx().clone();
    let chord = ctx
        .data(|d| d.get_temp::<(Id, Chord)>(chord_id()))
        .filter(|(owner, _)| *owner == editor_id)
        .map(|(_, c)| c);
    let multiline = egui::text_edit::TextEditState::load(&ctx, editor_id)
        .and_then(|s| s.cursor.char_range())
        .is_some_and(|r| {
            let r = r.as_sorted_char_range();
            text.chars()
                .skip(r.start.0)
                .take(r.end.0 - r.start.0)
                .any(|c| c == '\n')
        });
    let (action, next) = ui.input_mut(|i| {
        if let Some(chord) = chord {
            let action = match chord {
                Chord::CtrlK => {
                    // Ctrl+C arrives as a copy request on most platforms.
                    if let Some(p) = i.events.iter().position(|e| matches!(e, Event::Copy)) {
                        i.events.remove(p);
                        Some(A::Comment)
                    } else if take_key(i, CTRL, Key::C) {
                        Some(A::Comment)
                    } else if take_key(i, CTRL, Key::U) {
                        Some(A::Uncomment)
                    } else if take_key(i, CTRL, Key::Backslash) {
                        Some(A::DeleteWhiteSpace)
                    } else {
                        None
                    }
                }
                Chord::CtrlE => {
                    if take_key(i, CTRL, Key::W) {
                        Some(A::ToggleWordWrap)
                    } else if take_key(i, CTRL, Key::S) {
                        Some(A::ToggleWhiteSpace)
                    } else {
                        None
                    }
                }
                Chord::ShiftAltL => {
                    if take_key(i, shift_alt, Key::S) {
                        Some(A::SortLines)
                    } else if take_key(i, shift_alt, Key::J) {
                        Some(A::JoinLines)
                    } else {
                        None
                    }
                }
            };
            // Any other key ends the chord and is handled as usual.
            let other_key = i.events.iter().any(|e| {
                matches!(
                    e,
                    Event::Key { pressed: true, .. }
                        | Event::Text(_)
                        | Event::Copy
                        | Event::Cut
                        | Event::Paste(_)
                )
            });
            let waiting = action.is_none() && !other_key;
            return (action, waiting.then_some(chord));
        }
        if take_key(i, CTRL, Key::K) {
            return (None, Some(Chord::CtrlK));
        }
        if take_key(i, CTRL, Key::E) {
            return (None, Some(Chord::CtrlE));
        }
        if take_key(i, shift_alt, Key::L) {
            return (None, Some(Chord::ShiftAltL));
        }
        let shift_ctrl_alt = ctrl_alt.plus(Modifiers::SHIFT);
        let action = if take_key(i, CTRL, Key::Slash) || take_key(i, ctrl_shift, Key::Slash) {
            Some(A::ToggleComment)
        } else if take_key(i, ctrl_shift, Key::U) {
            Some(A::Uppercase)
        } else if take_key(i, ctrl_shift, Key::L) {
            Some(A::Lowercase)
        } else if take_key(i, Modifiers::ALT, Key::ArrowUp) {
            Some(A::MoveLinesUp)
        } else if take_key(i, Modifiers::ALT, Key::ArrowDown) {
            Some(A::MoveLinesDown)
        } else if take_key(i, ctrl_alt, Key::ArrowRight) {
            Some(A::Subword {
                forward: true,
                extend: false,
            })
        } else if take_key(i, shift_ctrl_alt, Key::ArrowRight) {
            Some(A::Subword {
                forward: true,
                extend: true,
            })
        } else if take_key(i, ctrl_alt, Key::ArrowLeft) {
            Some(A::Subword {
                forward: false,
                extend: false,
            })
        } else if take_key(i, shift_ctrl_alt, Key::ArrowLeft) {
            Some(A::Subword {
                forward: false,
                extend: true,
            })
        } else if take_key(i, shift_alt, Key::E) {
            Some(A::ExpandToLine)
        } else if take_key(i, Modifiers::SHIFT, Key::Tab) {
            Some(A::DecreaseIndent)
        } else if multiline && take_key(i, Modifiers::NONE, Key::Tab) {
            Some(A::IncreaseIndent)
        } else {
            None
        };
        (action, None)
    });
    ctx.data_mut(|d| match next {
        Some(chord) => {
            d.insert_temp(chord_id(), (editor_id, chord));
        }
        None => {
            d.remove::<(Id, Chord)>(chord_id());
        }
    });
    action
}

/// Closes brackets and quotes as they are typed and deletes an empty pair with Backspace
/// ([`textops::type_char`], [`textops::backspace`]). Only the first edit of a frame is taken
/// over, so keys typed after it in the same frame still apply in order.
pub(super) fn auto_close(ui: &mut Ui, editor_id: Id, q: &mut QueryTab) {
    let ctx = ui.ctx().clone();
    let Some(sel) = current_selection(&ctx, editor_id) else {
        return;
    };
    let edit = ui.input_mut(|i| {
        let first = i.events.iter().position(|e| match e {
            Event::Text(_) | Event::Paste(_) | Event::Ime(_) | Event::Cut => true,
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => key_edits(*key, *modifiers),
            _ => false,
        })?;
        let edit = match &i.events[first] {
            Event::Text(t) => {
                let mut chars = t.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => textops::type_char(&q.sql, sel, c),
                    _ => None,
                }
            }
            Event::Key {
                key: Key::Backspace,
                pressed: true,
                modifiers,
                ..
            } if modifiers.is_none() => textops::backspace(&q.sql, sel),
            _ => None,
        }?;
        i.events.remove(first);
        Some(edit)
    });
    if let Some(edit) = edit {
        apply_edit(&ctx, editor_id, q, edit);
    }
}

/// Whether a key press moves the caret or changes the text by itself. A printable key does
/// not: the window sends its press just before the `Event::Text` with the character, and
/// the text editor only acts on the text.
pub(super) fn key_edits(key: Key, modifiers: Modifiers) -> bool {
    modifiers.ctrl
        || modifiers.command
        || modifiers.mac_cmd
        || matches!(
            key,
            Key::ArrowDown
                | Key::ArrowLeft
                | Key::ArrowRight
                | Key::ArrowUp
                | Key::Tab
                | Key::Backspace
                | Key::Enter
                | Key::Delete
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Copy
                | Key::Cut
                | Key::Paste
        )
}

/// The editor's selection in characters.
pub(super) fn current_selection(ctx: &egui::Context, editor_id: Id) -> Option<Selection> {
    let range = egui::text_edit::TextEditState::load(ctx, editor_id)?
        .cursor
        .char_range()?;
    Some(Selection::new(
        range.secondary.index.0,
        range.primary.index.0,
    ))
}

pub(super) fn set_selection(ctx: &egui::Context, editor_id: Id, sel: Selection) {
    let mut state = egui::text_edit::TextEditState::load(ctx, editor_id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(sel.anchor),
            egui::text::CCursor::new(sel.caret),
        )));
    state.store(ctx, editor_id);
}

/// Replaces the editor text with `edit`, as one undo step.
pub(super) fn apply_edit(
    ctx: &egui::Context,
    editor_id: Id,
    q: &mut QueryTab,
    edit: textops::Edit,
) {
    let mut state = egui::text_edit::TextEditState::load(ctx, editor_id).unwrap_or_default();
    if let Some(range) = state.cursor.char_range() {
        let mut undoer = state.undoer();
        undoer.add_undo(&(range, q.sql.clone()));
        state.set_undoer(undoer);
    }
    q.sql = edit.text;
    state.store(ctx, editor_id);
    set_selection(ctx, editor_id, edit.selection);
}

/// While a selection drag is held outside the editor, scroll toward the pointer and
/// keep extending the selection. `TextEdit` only follows the pointer while it is over
/// the widget, so the scroll offset and the cursor are written back after it returns.
pub(super) fn extend_selection_past_editor(
    ui: &Ui,
    q: &mut QueryTab,
    editor_id: Id,
    scrolled: &egui::containers::scroll_area::ScrollAreaOutput<
        egui::widgets::text_edit::TextEditOutput,
    >,
) {
    if !ui.ctx().is_being_dragged(editor_id) {
        return;
    }
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos().or(input.pointer.latest_pos()))
    else {
        return;
    };
    let view = scrolled.inner_rect;
    if view.width() <= 1.0 || view.height() <= 1.0 || view.contains(pointer) {
        return;
    }
    let delta = Vec2::new(
        outside_speed(pointer.x, view.left(), view.right()),
        outside_speed(pointer.y, view.top(), view.bottom()),
    );
    if delta == Vec2::ZERO {
        return;
    }

    let max_offset = (scrolled.content_size - view.size()).max(Vec2::ZERO);
    let mut scroll = scrolled.state;
    let next_offset = (scroll.offset + delta).clamp(Vec2::ZERO, max_offset);
    let moved = next_offset != scroll.offset;
    scroll.offset = next_offset;
    scroll.store(ui.ctx(), scrolled.id);

    let edge = egui::pos2(
        pointer.x.clamp(view.left(), view.right()),
        pointer.y.clamp(view.top(), view.bottom()),
    );
    let local = (edge - scrolled.inner.galley_pos) + delta;
    let primary = scrolled.inner.galley.cursor_from_pos(local);
    let mut text = scrolled.inner.state.clone();
    let secondary = text
        .cursor
        .char_range()
        .map(|range| range.secondary)
        .unwrap_or(primary);
    text.cursor.set_char_range(Some(egui::text::CCursorRange {
        primary,
        secondary,
        h_pos: None,
    }));
    if let Some(range) = text.cursor.char_range() {
        q.run.cursor = Some(
            widgets::char_range_to_bytes(&q.sql, range.primary.index.0, range.primary.index.0)
                .start,
        );
        let chars = range.as_sorted_char_range();
        q.run.selection = (chars.start.0 != chars.end.0)
            .then(|| widgets::char_range_to_bytes(&q.sql, chars.start.0, chars.end.0));
    }
    text.store(ui.ctx(), editor_id);
    if moved {
        ui.ctx().request_repaint();
    }
}

/// Pixels to scroll this frame when the pointer is outside one edge of the editor.
pub(super) fn outside_speed(pointer: f32, min: f32, max: f32) -> f32 {
    if pointer > max {
        let over = (pointer - max).clamp(0.0, 48.0);
        12.0 + over * 0.5
    } else if pointer < min {
        let over = (min - pointer).clamp(0.0, 48.0);
        -(12.0 + over * 0.5)
    } else {
        0.0
    }
}
