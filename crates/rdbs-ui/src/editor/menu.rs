//! Edit menu and context menu entries, and the editor actions they run.

use super::*;

/// Commands of the editor's context menu, Edit menu and shortcuts.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    Run,
    Tabify,
    Untabify,
    Uppercase,
    Lowercase,
    MoveLinesUp,
    MoveLinesDown,
    DeleteWhiteSpace,
    ToggleWhiteSpace,
    ToggleWordWrap,
    SortLines,
    JoinLines,
    Subword { forward: bool, extend: bool },
    ExpandToLine,
    Comment,
    Uncomment,
    ToggleComment,
    IncreaseIndent,
    DecreaseIndent,
    Find,
    Replace,
    FindNext,
    FindPrevious,
}

/// Where an editor command is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuGlyph {
    /// Fixed icon, or a blank of the same width.
    Fixed(&'static str),
    /// Check mark follows the word-wrap preference.
    WordWrap,
    /// Check mark follows the white-space preference.
    WhiteSpace,
}

/// One row of the editor menus. The same list feeds the top Edit menu and the context menu.
#[derive(Clone, Copy)]
pub(super) struct MenuEntry {
    action: EditorAction,
    label: &'static str,
    shortcut: &'static str,
    glyph: MenuGlyph,
    /// `0` clipboard, `1` select all, `5` find, `2` comments, `3` the old Advanced group,
    /// `4` run.
    section: u8,
    /// Shown in the top Edit menu.
    edit: bool,
    /// Shown in the context menu itself. Advanced rows are `false` and live in the submenu.
    context_root: bool,
    needs_selection: bool,
}

/// Editor commands, in menu order. Advanced rows are also in the top Edit menu.
pub(super) fn menu_entries() -> &'static [MenuEntry] {
    use EditorAction as A;
    use MenuGlyph::Fixed;
    const ADVANCED: u8 = 3;
    const FIND: u8 = 5;
    &[
        MenuEntry {
            action: A::Cut,
            label: "Cut",
            shortcut: "Ctrl+X",
            glyph: Fixed(icon::SCISSORS),
            section: 0,
            edit: true,
            context_root: true,
            needs_selection: true,
        },
        MenuEntry {
            action: A::Copy,
            label: "Copy",
            shortcut: "Ctrl+C",
            glyph: Fixed(icon::COPY),
            section: 0,
            edit: true,
            context_root: true,
            needs_selection: true,
        },
        MenuEntry {
            action: A::Paste,
            label: "Paste",
            shortcut: "Ctrl+V",
            glyph: Fixed(icon::CLIPBOARD_TEXT),
            section: 0,
            edit: true,
            context_root: true,
            needs_selection: false,
        },
        MenuEntry {
            action: A::SelectAll,
            label: "Select All",
            shortcut: "Ctrl+A",
            glyph: Fixed(icon::SELECTION_ALL),
            section: 1,
            edit: true,
            context_root: true,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Find,
            label: "Find…",
            shortcut: "Ctrl+F",
            glyph: Fixed(icon::MAGNIFYING_GLASS),
            section: FIND,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Replace,
            label: "Replace…",
            shortcut: "Ctrl+H",
            glyph: Fixed(icon::SWAP),
            section: FIND,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::FindNext,
            label: "Find Next",
            shortcut: "F3",
            glyph: Fixed(icon::ARROW_DOWN),
            section: FIND,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::FindPrevious,
            label: "Find Previous",
            shortcut: "Shift+F3",
            glyph: Fixed(icon::ARROW_UP),
            section: FIND,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Comment,
            label: "Comment Selection",
            shortcut: "Ctrl+K, Ctrl+C",
            glyph: Fixed(icon::CHAT_CENTERED_SLASH),
            section: 2,
            edit: true,
            context_root: true,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Uncomment,
            label: "Uncomment Selection",
            shortcut: "Ctrl+K, Ctrl+U",
            glyph: Fixed(icon::CHAT_CENTERED),
            section: 2,
            edit: true,
            context_root: true,
            needs_selection: false,
        },
        MenuEntry {
            action: A::ToggleComment,
            label: "Toggle Line Comment",
            shortcut: "Ctrl+/",
            glyph: Fixed(icon::CHAT_CENTERED_DOTS),
            section: 2,
            edit: true,
            context_root: true,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Tabify,
            label: "Tabify Selected Lines",
            shortcut: "",
            glyph: Fixed(icon::ARROW_LINE_RIGHT),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Untabify,
            label: "Untabify Selected Lines",
            shortcut: "",
            glyph: Fixed(icon::ARROW_LINE_LEFT),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Uppercase,
            label: "Make Uppercase",
            shortcut: "Ctrl+Shift+U",
            glyph: Fixed(icon::TEXT_AA),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Lowercase,
            label: "Make Lowercase",
            shortcut: "Ctrl+Shift+L",
            glyph: Fixed(" "),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::MoveLinesUp,
            label: "Move Selected Lines Up",
            shortcut: "Alt+Up Arrow",
            glyph: Fixed(icon::ARROW_UP),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::MoveLinesDown,
            label: "Move Selected Lines Down",
            shortcut: "Alt+Down Arrow",
            glyph: Fixed(icon::ARROW_DOWN),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::DeleteWhiteSpace,
            label: "Delete Horizontal White Space",
            shortcut: "Ctrl+K, Ctrl+\\",
            glyph: Fixed(" "),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::ToggleWhiteSpace,
            label: "View White Space",
            shortcut: "Ctrl+E, Ctrl+S",
            glyph: MenuGlyph::WhiteSpace,
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::ToggleWordWrap,
            label: "Word Wrap",
            shortcut: "Ctrl+E, Ctrl+W",
            glyph: MenuGlyph::WordWrap,
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::SortLines,
            label: "Sort Lines",
            shortcut: "Shift+Alt+L, Shift+Alt+S",
            glyph: Fixed(icon::SORT_ASCENDING),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::JoinLines,
            label: "Join Lines",
            shortcut: "Shift+Alt+L, Shift+Alt+J",
            glyph: Fixed(" "),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Subword {
                forward: true,
                extend: false,
            },
            label: "Next Subword",
            shortcut: "Ctrl+Alt+Right Arrow",
            glyph: Fixed(" "),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Subword {
                forward: false,
                extend: false,
            },
            label: "Previous Subword",
            shortcut: "Ctrl+Alt+Left Arrow",
            glyph: Fixed(" "),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::ExpandToLine,
            label: "Expand Selection to Line",
            shortcut: "Shift+Alt+E",
            glyph: Fixed(" "),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::IncreaseIndent,
            label: "Increase Line Indent",
            shortcut: "Tab",
            glyph: Fixed(icon::TEXT_INDENT),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::DecreaseIndent,
            label: "Decrease Line Indent",
            shortcut: "Shift+Tab",
            glyph: Fixed(icon::TEXT_OUTDENT),
            section: ADVANCED,
            edit: true,
            context_root: false,
            needs_selection: false,
        },
        MenuEntry {
            action: A::Run,
            label: "Run",
            shortcut: "Ctrl+Enter / F5",
            glyph: Fixed(icon::PLAY),
            section: 4,
            edit: false,
            context_root: true,
            needs_selection: false,
        },
    ]
}

pub(super) fn entry_glyph(entry: &MenuEntry, word_wrap: bool, white_space: bool) -> &'static str {
    match entry.glyph {
        MenuGlyph::Fixed(icon) => icon,
        MenuGlyph::WordWrap => {
            if word_wrap {
                icon::CHECK
            } else {
                " "
            }
        }
        MenuGlyph::WhiteSpace => {
            if white_space {
                icon::CHECK
            } else {
                " "
            }
        }
    }
}

/// Draws `entries` and returns the one that was clicked. A separator sits between sections.
pub(super) fn show_menu_entries(
    ui: &mut Ui,
    entries: impl Iterator<Item = &'static MenuEntry>,
    editor_open: bool,
    has_selection: bool,
    word_wrap: bool,
    white_space: bool,
) -> Option<EditorAction> {
    let mut action = None;
    let mut last_section = None;
    for entry in entries {
        if last_section.is_some_and(|section| section != entry.section) {
            ui.separator();
        }
        last_section = Some(entry.section);
        let label = if entry.action == EditorAction::Run && has_selection {
            "Run Selection"
        } else {
            entry.label
        };
        let enabled = editor_open && (!entry.needs_selection || has_selection);
        let button = Button::new(format!(
            "{}  {label}",
            entry_glyph(entry, word_wrap, white_space)
        ))
        .shortcut_text(entry.shortcut);
        let mut response = ui.add_enabled(enabled, button);
        if !editor_open {
            response = response.on_disabled_hover_text("Open a query tab to edit its script");
        }
        if response.clicked() {
            action = Some(entry.action);
        }
    }
    action
}

/// Word wrap and visible white space, shared by every query tab.
pub(crate) fn editor_preferences(ctx: &egui::Context) -> (bool, bool) {
    (pref(ctx, WORD_WRAP), pref(ctx, WHITE_SPACE))
}

/// Top Edit menu: clipboard, comments, and every command that used to live under Advanced.
pub(crate) fn show_edit_menu(
    ui: &mut Ui,
    editor_open: bool,
    has_selection: bool,
    word_wrap: bool,
    white_space: bool,
) -> Option<EditorAction> {
    show_menu_entries(
        ui,
        menu_entries().iter().filter(|entry| entry.edit),
        editor_open,
        has_selection,
        word_wrap,
        white_space,
    )
}

pub(super) fn editor_menu(
    ui: &mut Ui,
    has_selection: bool,
    word_wrap: bool,
    white_space: bool,
) -> Option<EditorAction> {
    let mut action = show_menu_entries(
        ui,
        menu_entries()
            .iter()
            .filter(|entry| entry.context_root && entry.section < 3),
        true,
        has_selection,
        word_wrap,
        white_space,
    );
    ui.separator();
    ui.menu_button(format!("{}  Advanced", icon::TEXT_AA), |ui| {
        if let Some(chosen) = show_menu_entries(
            ui,
            menu_entries().iter().filter(|entry| entry.section == 3),
            true,
            has_selection,
            word_wrap,
            white_space,
        ) {
            action = Some(chosen);
        }
    });
    ui.separator();
    if let Some(chosen) = show_menu_entries(
        ui,
        menu_entries().iter().filter(|entry| entry.section == 4),
        true,
        has_selection,
        word_wrap,
        white_space,
    ) {
        action = Some(chosen);
    }
    action
}

/// Carries out a context-menu or shortcut command. Clipboard commands go through the platform
/// clipboard (the backend turns them into the same events as the keyboard shortcuts).
pub(crate) fn apply_editor_action(
    ctx: &egui::Context,
    editor_id: Id,
    action: EditorAction,
    q: &mut QueryTab,
    cx: &mut TabContext<'_>,
) {
    use EditorAction as A;
    // The find commands leave the keyboard where they put it (the find bar, or the editor).
    match action {
        A::Find | A::Replace => return find::open(ctx, editor_id, q, action == A::Replace),
        A::FindNext | A::FindPrevious => {
            return find::step(ctx, editor_id, q, action == A::FindNext);
        }
        _ => {}
    }
    ctx.memory_mut(|m| m.request_focus(editor_id));
    let sel = current_selection(ctx, editor_id).unwrap_or(Selection::caret(0));
    let text = q.sql.as_str();
    let edit = match action {
        A::Cut => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut);
            None
        }
        A::Copy => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
            None
        }
        A::Paste => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            None
        }
        A::SelectAll => {
            set_selection(ctx, editor_id, Selection::new(0, text.chars().count()));
            None
        }
        A::Run => {
            request_run(q, cx);
            None
        }
        A::ToggleWhiteSpace => {
            toggle_pref(ctx, WHITE_SPACE);
            None
        }
        A::ToggleWordWrap => {
            toggle_pref(ctx, WORD_WRAP);
            None
        }
        A::Subword { forward, extend } => {
            let caret = textops::subword(text, sel.caret, forward);
            let anchor = if extend { sel.anchor } else { caret };
            set_selection(ctx, editor_id, Selection::new(anchor, caret));
            None
        }
        A::ExpandToLine => {
            set_selection(ctx, editor_id, textops::expand_to_line(text, sel));
            None
        }
        A::Tabify => textops::retab(text, sel, true),
        A::Untabify => textops::retab(text, sel, false),
        A::Uppercase => textops::change_case(text, sel, true),
        A::Lowercase => textops::change_case(text, sel, false),
        A::MoveLinesUp => textops::move_lines(text, sel, true),
        A::MoveLinesDown => textops::move_lines(text, sel, false),
        A::DeleteWhiteSpace => textops::delete_horizontal_whitespace(text, sel),
        A::SortLines => textops::sort_lines(text, sel),
        A::JoinLines => textops::join_lines(text, sel),
        A::Comment => textops::toggle_comment(text, sel, true),
        A::Uncomment => textops::toggle_comment(text, sel, false),
        A::ToggleComment => textops::toggle_line_comment(text, sel),
        A::IncreaseIndent => textops::indent_lines(text, sel, true),
        A::DecreaseIndent => textops::indent_lines(text, sel, false),
        A::Find | A::Replace | A::FindNext | A::FindPrevious => None,
    };
    if let Some(edit) = edit {
        apply_edit(ctx, editor_id, q, edit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_menu_includes_the_advanced_editor_options() {
        use EditorAction as A;
        let edit: Vec<EditorAction> = menu_entries()
            .iter()
            .filter(|entry| entry.edit)
            .map(|entry| entry.action)
            .collect();
        for action in [
            A::Cut,
            A::Copy,
            A::Paste,
            A::SelectAll,
            A::Find,
            A::Replace,
            A::FindNext,
            A::FindPrevious,
            A::Comment,
            A::Uncomment,
            A::Tabify,
            A::Untabify,
            A::Uppercase,
            A::Lowercase,
            A::MoveLinesUp,
            A::MoveLinesDown,
            A::DeleteWhiteSpace,
            A::ToggleWhiteSpace,
            A::ToggleWordWrap,
            A::SortLines,
            A::JoinLines,
            A::Subword {
                forward: true,
                extend: false,
            },
            A::Subword {
                forward: false,
                extend: false,
            },
            A::ExpandToLine,
            A::IncreaseIndent,
            A::DecreaseIndent,
        ] {
            assert!(edit.contains(&action), "{action:?} is missing from Edit");
        }
        assert!(!edit.contains(&A::Run));
        assert!(menu_entries().iter().any(|entry| {
            entry.action == A::ToggleWordWrap && entry.edit && !entry.context_root
        }));
    }
}
