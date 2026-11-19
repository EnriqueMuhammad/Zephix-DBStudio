//! Keyboard shortcuts, closing tabs, saving query files, quitting and running tabs.

use super::*;

impl StudioApp {
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<UiAction>) {
        if self.dialog.is_some()
            || self.confirm.is_some()
            || self.show_about
            || self.update.dialog_open()
            || self.ai.dialog_open()
            || self.fonts.dialog_open()
            || self.prompt.is_some()
            || self.exec.is_some()
            || !self.scripts.close_queue.is_empty()
            || self.scripts.quit_prompt
        {
            return; // Modals own the keyboard.
        }
        if crate::editor::chord_pending(ctx) {
            return; // The second key of an editor shortcut such as Ctrl+E, Ctrl+W.
        }
        let pressed = |sc: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(sc));
        let new_connection = pressed(&SC_NEW_CONNECTION);
        let new_query = pressed(&SC_NEW_QUERY);
        let open_query = pressed(&SC_OPEN_QUERY);
        // Before Ctrl+S, which would also match Ctrl+Shift+S.
        let save_query_as = pressed(&SC_SAVE_QUERY_AS);
        let save_query = pressed(&SC_SAVE_QUERY);
        // Before Ctrl+W, which would also match Ctrl+Shift+W.
        let close_all_tabs = pressed(&SC_CLOSE_ALL_TABS);
        let close_tab = pressed(&SC_CLOSE_TAB);
        let sidebar = pressed(&SC_SIDEBAR);
        let filter = pressed(&SC_FILTER);
        let replace = pressed(&SC_REPLACE);
        // Before F3, which would also match Shift+F3.
        let find_previous = pressed(&SC_FIND_PREVIOUS);
        let find_next = pressed(&SC_FIND_NEXT);
        if pressed(&SC_AI_PANEL) {
            self.ai.toggle();
        }
        let f5 = pressed(&SC_F5);
        let f5_runs = f5 && self.workspace.active_query_tab().is_some();
        let run = pressed(&SC_RUN) | pressed(&SC_RUN_ALT) | f5_runs;
        let refresh = f5 && !f5_runs;

        if new_connection {
            actions.push(UiAction::NewConnection);
        }
        if open_query {
            actions.push(UiAction::OpenQueryFile);
        }
        if save_query || save_query_as {
            actions.push(UiAction::SaveQuery {
                tab_id: None,
                save_as: save_query_as,
            });
        }
        if new_query {
            actions.push(UiAction::NewQuery {
                profile_id: None,
                database: None,
                sql: None,
                title: None,
                run: false,
            });
        }
        if close_all_tabs {
            actions.push(UiAction::CloseTabs(TabClose::All));
        }
        if close_tab {
            self.close_active_tab();
        }
        if sidebar {
            actions.push(UiAction::ToggleSidebar);
        }
        // Ctrl+F searches the script of the active query tab, else filters the explorer.
        let editor_tab = self
            .workspace
            .active_query_tab()
            .is_some_and(|q| q.data_view.is_none());
        if filter && !editor_tab {
            self.workspace.sidebar_visible = true;
            self.explorer.focus_filter = true;
        }
        if editor_tab {
            use editor::EditorAction as A;
            for (pressed, action) in [
                (filter, A::Find),
                (replace, A::Replace),
                (find_next, A::FindNext),
                (find_previous, A::FindPrevious),
            ] {
                if pressed {
                    self.apply_editor_menu(action, actions);
                }
            }
        }
        if run {
            self.run_active_tab(actions);
        }
        if refresh && let Some(id) = self.explorer.selected_profile {
            actions.push(UiAction::Refresh(id));
        }
    }

    pub(super) fn close_active_tab(&mut self) {
        if let Some(key) = self
            .workspace
            .dock
            .find_active_focused()
            .map(|(_, t)| t.key())
        {
            self.request_close(TabClose::One(key));
        }
    }

    /// Closes the tabs `which` selects. Those with unsaved changes stay open and are queued;
    /// the first one asks whether to save.
    pub(super) fn request_close(&mut self, which: TabClose) {
        let dirty: Vec<u64> = self
            .workspace
            .dock
            .iter_all_tabs()
            .filter(|(_, t)| which.closes(t.key()) && tab_is_dirty(t))
            .map(|(_, t)| t.key())
            .collect();
        let closed = self.workspace.close_where(which, |t| !tab_is_dirty(t));
        self.cancel_closed(closed);
        for key in dirty {
            if !self.scripts.close_queue.contains(&key) {
                self.scripts.close_queue.push_back(key);
            }
        }
        if let Some(&key) = self.scripts.close_queue.front() {
            self.workspace.focus_tab(key);
        }
    }

    /// Closes one tab without asking (its changes were saved or discarded).
    pub(super) fn close_now(&mut self, key: u64) {
        let closed = self.workspace.close_tabs(TabClose::One(key));
        self.cancel_closed(closed);
        self.scripts.close_queue.retain(|k| *k != key);
        if let Some(&next) = self.scripts.close_queue.front() {
            self.workspace.focus_tab(next);
        }
    }

    /// Saves a query tab to its file, or asks for one (`save_as`, or a tab without a file).
    /// `then_close` closes it afterwards, as part of closing tabs.
    pub(super) fn save_query(&mut self, tab_id: u64, save_as: bool, then_close: bool) {
        let Some(q) = self.workspace.query_tab_mut(tab_id) else {
            return;
        };
        if q.data_view.is_some() {
            return;
        }
        match q.path.clone().filter(|_| !save_as) {
            Some(path) => self.write_query(tab_id, path, then_close),
            None => {
                let (path, title) = (q.path.clone(), q.title.clone());
                let ctx = self.egui_ctx.clone();
                if let Err(e) =
                    self.scripts
                        .pick_save(&ctx, tab_id, path.as_deref(), &title, then_close)
                {
                    self.status = Some(StatusMessage::error(e));
                    self.scripts.abort();
                }
            }
        }
    }

    pub(super) fn write_query(&mut self, tab_id: u64, path: std::path::PathBuf, then_close: bool) {
        let Some(q) = self.workspace.query_tab_mut(tab_id) else {
            return;
        };
        match crate::scripts::write_script(&path, &q.sql) {
            Ok(()) => {
                self.scripts.last_dir = path.parent().map(std::path::Path::to_path_buf);
                self.status = Some(StatusMessage::info(format!("Saved {}", path.display())));
                q.mark_saved(path);
                if then_close {
                    self.close_now(tab_id);
                }
            }
            Err(e) => {
                self.status = Some(StatusMessage::error(format!(
                    "Could not save {}: {e}",
                    path.display()
                )));
                self.scripts.abort();
            }
        }
    }

    /// Applies the result of a script file dialog.
    pub(super) fn apply_picked(&mut self, picked: crate::scripts::Picked) {
        use crate::scripts::Picked;
        match picked {
            Picked::Open { path, text } => {
                self.scripts.last_dir = path.parent().map(std::path::Path::to_path_buf);
                if let Some(id) = self.workspace.tab_for_path(&path) {
                    self.workspace.focus_tab(id);
                    return;
                }
                let title = path.file_name().map(|n| n.to_string_lossy().into_owned());
                let id = self
                    .workspace
                    .open_query_tab(self.explorer.selected_profile, text, title);
                if let Some(q) = self.workspace.query_tab_mut(id) {
                    q.mark_saved(path);
                }
            }
            Picked::SaveTo {
                tab_id,
                path,
                then_close,
            } => self.write_query(tab_id, path, then_close),
            Picked::Cancelled { then_close } => {
                if then_close {
                    self.scripts.abort();
                }
            }
            Picked::Failed(message) => self.status = Some(StatusMessage::error(message)),
        }
    }

    /// Holds back closing Zephix while scripts have unsaved changes, and asks what to do.
    pub(super) fn guard_quit(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.scripts.allow_quit {
            return;
        }
        if self.workspace.unsaved_tabs().is_empty() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if !self.scripts.quitting {
            self.scripts.quit_prompt = true;
        }
    }

    /// The quit prompt, and the "Save All" run that ends in closing Zephix.
    pub(super) fn show_quit_prompt(&mut self, ctx: &egui::Context) {
        use crate::scripts::QuitChoice;
        if self.scripts.quit_prompt {
            let titles: Vec<String> = self
                .workspace
                .unsaved_tabs()
                .into_iter()
                .map(|(_, title)| title)
                .collect();
            match crate::scripts::quit_prompt(ctx, &titles) {
                Some(QuitChoice::SaveAll) => {
                    self.scripts.quit_prompt = false;
                    self.scripts.quitting = true;
                    self.scripts.close_queue = self
                        .workspace
                        .unsaved_tabs()
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                }
                Some(QuitChoice::Discard) => {
                    self.scripts.quit_prompt = false;
                    let dropped = self.workspace.close_where(TabClose::All, tab_is_dirty);
                    self.cancel_closed(dropped);
                    self.quit(ctx);
                }
                Some(QuitChoice::Cancel) => self.scripts.quit_prompt = false,
                None => {}
            }
            return;
        }
        if !self.scripts.quitting || self.scripts.dialog_open {
            return;
        }
        match self.scripts.close_queue.front().copied() {
            Some(key) if self.workspace.query_tab_mut(key).is_none() => {
                self.scripts.close_queue.pop_front();
            }
            // Saved tabs leave the queue; a file dialog or an error stops here.
            Some(key) => self.save_query(key, false, true),
            None => self.quit(ctx),
        }
    }

    pub(super) fn quit(&mut self, ctx: &egui::Context) {
        self.scripts.quitting = false;
        self.scripts.allow_quit = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Asks about the first queued tab with unsaved changes.
    pub(super) fn show_close_prompt(&mut self, ctx: &egui::Context) {
        use crate::scripts::CloseChoice;
        if self.scripts.dialog_open || self.scripts.quitting || self.scripts.quit_prompt {
            return;
        }
        let Some(&key) = self.scripts.close_queue.front() else {
            return;
        };
        let Some(q) = self.workspace.query_tab_mut(key) else {
            self.scripts.close_queue.pop_front();
            return;
        };
        let (title, path) = (q.title.clone(), q.path.clone());
        match crate::scripts::close_prompt(ctx, &title, path.as_deref()) {
            Some(CloseChoice::Save) => self.save_query(key, false, true),
            Some(CloseChoice::Discard) => self.close_now(key),
            Some(CloseChoice::Cancel) => self.scripts.abort(),
            None => {}
        }
    }

    /// Cancels the queries still running in closed tabs.
    pub(super) fn cancel_closed(&self, tabs: impl IntoIterator<Item = Tab>) {
        for tab in tabs {
            if let Tab::Query(q) = tab
                && let Some(r) = q.run.running
            {
                self.services.send(Command::Cancel {
                    request_id: r.request_id,
                });
            }
        }
    }

    pub(super) fn run_active_tab(&mut self, actions: &mut Vec<UiAction>) {
        let Self {
            workspace,
            profiles,
            connections,
            services,
            session_passwords,
            history,
            backups,
            ..
        } = self;
        if let Some(q) = workspace.active_query_tab() {
            let mut cx = TabContext {
                profiles,
                connections,
                services,
                passwords: session_passwords,
                history,
                backups,
                actions,
                tab_text_px: None,
                egui: None,
                button_font: None,
            };
            editor::request_run(q, &mut cx);
        }
    }

    pub(super) fn run_tab(&mut self, tab_id: u64) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let Self {
            workspace,
            profiles,
            connections,
            services,
            session_passwords,
            history,
            backups,
            ..
        } = self;
        if let Some(q) = workspace.query_tab_mut(tab_id) {
            let mut cx = TabContext {
                profiles,
                connections,
                services,
                passwords: session_passwords,
                history,
                backups,
                actions: &mut actions,
                tab_text_px: None,
                egui: None,
                button_font: None,
            };
            editor::request_run(q, &mut cx);
        }
        actions
    }
}
