//! Menu bar, toolbar and status bar.

use super::*;

impl StudioApp {
    pub(super) fn menu_bar(
        &mut self,
        ui: &mut Ui,
        actions: &mut Vec<UiAction>,
    ) -> Option<editor::EditorAction> {
        let (editor_open, has_selection) = match self.workspace.active_query_tab() {
            Some(query) if query.data_view.is_none() => (true, query.run.selection.is_some()),
            _ => (false, false),
        };
        let (word_wrap, white_space) = editor::editor_preferences(ui.ctx());
        let mut edit_action = None;
        let ctx = ui.ctx().clone();
        let item = |label: String, sc: &KeyboardShortcut| {
            Button::new(label).shortcut_text(ctx.format_shortcut(sc))
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add(item(
                        format!("{}  New Connection…", icon::PLUS),
                        &SC_NEW_CONNECTION,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::NewConnection);
                }
                if ui
                    .add(item(
                        format!("{}  New Query", icon::FILE_SQL),
                        &SC_NEW_QUERY,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::NewQuery {
                        profile_id: None,
                        database: None,
                        sql: None,
                        title: None,
                        run: false,
                    });
                }
                if ui
                    .add(item(
                        format!("{}  Open Query…", icon::FOLDER_OPEN),
                        &SC_OPEN_QUERY,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::OpenQueryFile);
                }
                let has_script = self
                    .workspace
                    .active_query_tab()
                    .is_some_and(|q| q.data_view.is_none());
                if ui
                    .add_enabled(
                        has_script,
                        item(format!("{}  Save Query", icon::FLOPPY_DISK), &SC_SAVE_QUERY),
                    )
                    .clicked()
                {
                    actions.push(UiAction::SaveQuery {
                        tab_id: None,
                        save_as: false,
                    });
                }
                if ui
                    .add_enabled(
                        has_script,
                        item(
                            format!("{}  Save Query As…", icon::FLOPPY_DISK_BACK),
                            &SC_SAVE_QUERY_AS,
                        ),
                    )
                    .clicked()
                {
                    actions.push(UiAction::SaveQuery {
                        tab_id: None,
                        save_as: true,
                    });
                }
                ui.separator();
                if ui
                    .add(item(format!("{}  Close Tab", icon::X), &SC_CLOSE_TAB))
                    .clicked()
                {
                    self.close_active_tab();
                }
                if ui
                    .add(item(
                        format!("{}  Close All Tabs", icon::X_SQUARE),
                        &SC_CLOSE_ALL_TABS,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::CloseTabs(TabClose::All));
                }
                ui.separator();
                if ui.button(format!("{}  Exit", icon::SIGN_OUT)).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if let Some(action) =
                    editor::show_edit_menu(ui, editor_open, has_selection, word_wrap, white_space)
                {
                    edit_action = Some(action);
                }
            });
            ui.menu_button("View", |ui| {
                if ui
                    .add(item(
                        format!("{}  Toggle Sidebar", icon::SIDEBAR),
                        &SC_SIDEBAR,
                    ))
                    .clicked()
                {
                    actions.push(UiAction::ToggleSidebar);
                }
                if ui
                    .add(item(
                        format!("{}  Filter Explorer", icon::MAGNIFYING_GLASS),
                        &SC_FILTER,
                    ))
                    .clicked()
                {
                    self.workspace.sidebar_visible = true;
                    self.explorer.focus_filter = true;
                }
                if ui
                    .button(format!("{}  Welcome Page", icon::HOUSE))
                    .clicked()
                {
                    self.workspace.show_welcome();
                }
                if ui
                    .add(
                        item(format!("{}  AI Assistant", icon::SPARKLE), &SC_AI_PANEL)
                            .selected(self.ai.visible),
                    )
                    .clicked()
                {
                    self.ai.toggle();
                }
                ui.separator();
                ui.menu_button(format!("{}  Theme", icon::PALETTE), |ui| {
                    let current = theme::mode();
                    for (mode, label) in theme::ThemeMode::ALL {
                        if ui.radio(current == mode, label).clicked() {
                            theme::set_mode(ui.ctx(), mode);
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui
                    .button(format!("{}  Reset Layout", icon::ARROWS_CLOCKWISE))
                    .clicked()
                {
                    self.workspace = Workspace::default();
                }
            });
            ui.menu_button("Query", |ui| {
                if ui
                    .add(item(format!("{}  Run", icon::PLAY), &SC_RUN))
                    .clicked()
                {
                    self.run_active_tab(actions);
                }
                ui.separator();
                let (_, cx) = self.ai_context(false);
                ui.menu_button(format!("{}  Ask AI", icon::SPARKLE), |ui| {
                    for q in rdbs_core::ai::QuickAsk::ALL {
                        let enabled =
                            cx.has_sql && (!q.needs_error() || cx.has_error) && !self.ai.is_busy();
                        if ui
                            .add_enabled(enabled, Button::new(q.label()))
                            .on_hover_text(q.prompt())
                            .clicked()
                        {
                            self.ai_ask(q.prompt().to_owned());
                        }
                    }
                    ui.separator();
                    if ui.button("AI Providers…").clicked() {
                        self.ai.open_settings();
                    }
                });
            });
            ui.menu_button("Tools", |ui| {
                self.tools_menu(ui, actions);
                ui.separator();
                if ui
                    .button(format!("{}  Fonts…", icon::TEXT_AA))
                    .on_hover_text(
                        "Font and size of the interface, the SQL editor and the results grid",
                    )
                    .clicked()
                {
                    self.fonts.open_dialog();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui
                    .button(format!("{}  Keyboard Shortcuts", icon::KEYBOARD))
                    .clicked()
                {
                    self.workspace.show_welcome();
                }
                ui.separator();
                if let Some(crate::update::UpdateAction::CheckNow) = self.update.menu(ui) {
                    self.check_for_updates(true);
                }
                ui.separator();
                if ui
                    .button(format!(
                        "{}  About {}",
                        icon::INFO,
                        rdbs_core::brand::APP_NAME
                    ))
                    .clicked()
                {
                    self.show_about = true;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let light = theme::is_light();
                let (glyph, mut tip) = if light {
                    (icon::MOON, "Switch to the dark theme".to_owned())
                } else {
                    (icon::SUN, "Switch to the light theme".to_owned())
                };
                if theme::mode() == theme::ThemeMode::System {
                    tip.push_str(" (stops following the system; View > Theme)");
                }
                if ui
                    .add(Button::new(glyph).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    theme::set_light(ui.ctx(), !light);
                }
            });
        });
        edit_action
    }

    /// Applies an Edit-menu command to the focused query tab. The script tab stays open.
    pub(super) fn apply_editor_menu(
        &mut self,
        action: editor::EditorAction,
        actions: &mut Vec<UiAction>,
    ) {
        let Self {
            workspace,
            profiles,
            connections,
            services,
            session_passwords,
            history,
            backups,
            egui_ctx,
            ..
        } = self;
        let Some(query) = workspace.active_query_tab() else {
            return;
        };
        if query.data_view.is_some() {
            return;
        }
        let editor_id = query.editor_id();
        let mut cx = TabContext {
            profiles,
            connections,
            services,
            passwords: session_passwords,
            history,
            backups,
            actions,
            tab_text_px: None,
            egui: Some(egui_ctx.clone()),
            button_font: None,
        };
        editor::apply_editor_action(egui_ctx, editor_id, action, query, &mut cx);
    }

    /// Entries of the Tools menu (scope = explorer selection).
    pub(super) fn tools_menu(&self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        use crate::tools::ToolLaunch;
        let selected = self.selected_scope();
        let needs_selection = "Select a connection in the explorer first";
        if ui
            .button(format!("{}  Schema Diff…", icon::GIT_DIFF))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Diff {
                source: selected.clone(),
            }));
        }
        if ui.button(format!("{}  Backups…", icon::ARCHIVE)).clicked() {
            actions.push(UiAction::OpenTool(ToolLaunch::Backup { scope: None }));
        }
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  Query Builder", icon::PUZZLE_PIECE)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Builder { scope }));
        }
        if ui
            .button(format!("{}  Data Transfer…", icon::FLOW_ARROW))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Transfer {
                source: selected.clone(),
            }));
        }
        if ui
            .button(format!("{}  Table Sync…", icon::ARROWS_LEFT_RIGHT))
            .clicked()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Sync {
                source: selected.clone(),
            }));
        }
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  ER Diagram", icon::GRAPH)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Er { scope }));
        }
        ui.separator();
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  New Table…", icon::PLUS_SQUARE)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Designer {
                scope,
                table: None,
            }));
        }
        if ui
            .add_enabled(
                selected.is_some(),
                Button::new(format!("{}  Import Data…", icon::DOWNLOAD_SIMPLE)),
            )
            .on_disabled_hover_text(needs_selection)
            .clicked()
            && let Some(scope) = selected.clone()
        {
            actions.push(UiAction::OpenTool(ToolLaunch::Import {
                scope,
                table: None,
            }));
        }
        ui.separator();
        if ui
            .button(format!("{}  Query History", icon::CLOCK))
            .clicked()
        {
            actions.push(UiAction::ShowHistory);
        }
    }

    /// Scope of the explorer selection: the selected object's database/schema, or the
    /// selected connection's default database.
    pub(super) fn selected_scope(&self) -> Option<crate::tools::Scope> {
        let profile = self
            .profile(self.explorer.selected_profile?)
            .filter(|p| p.kind.is_sql())?;
        let (database, schema) = match &self.explorer.selected_object {
            Some(o) if o.profile_id == profile.id => (o.database.clone(), o.schema.clone()),
            _ => (
                self.connections
                    .get(&profile.id)
                    .and_then(|c| c.default_database.clone())
                    .or_else(|| (!profile.database.is_empty()).then(|| profile.database.clone())),
                crate::tools::picker::default_schema(profile.kind),
            ),
        };
        Some(crate::tools::scope(
            profile.kind,
            profile.id,
            database,
            schema,
        ))
    }

    pub(super) fn toolbar(&mut self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        let can_export = self
            .workspace
            .active_query_tab()
            .is_some_and(|q| q.run.active_result_set().is_some());
        let selected_profile = self.explorer.selected_profile;
        let selected_connected = selected_profile
            .and_then(|id| self.connections.get(&id))
            .is_some_and(ConnState::is_connected);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if ui
                .add(ToolbarButton::new(icon::PLUGS_CONNECTED, "Connection"))
                .on_hover_text("New connection (Ctrl+N)")
                .clicked()
            {
                actions.push(UiAction::NewConnection);
            }
            if ui
                .add(ToolbarButton::new(icon::FILE_SQL, "New Query"))
                .on_hover_text("New query tab (Ctrl+T)")
                .clicked()
            {
                actions.push(UiAction::NewQuery {
                    profile_id: None,
                    database: None,
                    sql: None,
                    title: None,
                    run: false,
                });
            }
            if ui
                .add(ToolbarButton::new(icon::FOLDER_OPEN, "Open"))
                .on_hover_text("Open a .sql file (Ctrl+O)")
                .clicked()
            {
                actions.push(UiAction::OpenQueryFile);
            }
            let has_script = self
                .workspace
                .active_query_tab()
                .is_some_and(|q| q.data_view.is_none());
            if ui
                .add(ToolbarButton::new(icon::FLOPPY_DISK, "Save").enabled(has_script))
                .on_hover_text("Save the query to a file (Ctrl+S)")
                .clicked()
            {
                actions.push(UiAction::SaveQuery {
                    tab_id: None,
                    save_as: false,
                });
            }
            ui.separator();
            let selected_object = self.explorer.selected_object.clone();
            if ui
                .add(
                    ToolbarButton::new(icon::TABLE, "Open Table")
                        .enabled(selected_object.is_some()),
                )
                .on_hover_text("Open the selected table or view")
                .clicked()
                && let Some(obj) = selected_object
            {
                actions.push(UiAction::OpenObject(obj));
            }
            if ui
                .add(
                    ToolbarButton::new(icon::ARROWS_CLOCKWISE, "Refresh")
                        .enabled(selected_profile.is_some()),
                )
                .on_hover_text("Refresh selected connection (F5 when no query tab is active)")
                .clicked()
                && let Some(id) = selected_profile
            {
                actions.push(UiAction::Refresh(id));
            }
            if ui
                .add(ToolbarButton::new(icon::PLUGS, "Disconnect").enabled(selected_connected))
                .clicked()
                && let Some(id) = selected_profile
            {
                actions.push(UiAction::Disconnect(id));
            }
            ui.separator();
            if ui
                .add(ToolbarButton::new(icon::EXPORT, "Export").enabled(can_export))
                .on_hover_text("Save the visible result as CSV (more formats in the result header)")
                .clicked()
            {
                actions.push(UiAction::ExportActive);
            }
            let scope = self.selected_scope();
            if ui
                .add(ToolbarButton::new(icon::GRAPH, "Model").enabled(scope.is_some()))
                .on_hover_text("ER diagram of the selected database / schema")
                .clicked()
                && let Some(scope) = scope
            {
                actions.push(UiAction::OpenTool(crate::tools::ToolLaunch::Er { scope }));
            }
            if ui
                .add(ToolbarButton::new(icon::CLOCK, "History"))
                .on_hover_text("Query history")
                .clicked()
            {
                actions.push(UiAction::ShowHistory);
            }
            if ui
                .add(ToolbarButton::new(icon::SPARKLE, "AI"))
                .on_hover_text("AI assistant (Ctrl+Shift+I)")
                .clicked()
            {
                self.ai.toggle();
            }
            ui.separator();
            if ui
                .add(ToolbarButton::new(icon::SIDEBAR, "Sidebar"))
                .on_hover_text("Toggle sidebar (Ctrl+B)")
                .clicked()
            {
                actions.push(UiAction::ToggleSidebar);
            }
        });
    }

    pub(super) fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            // The summary on the right gets its room first; the message takes what is left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connected = self
                    .connections
                    .values()
                    .filter(|c| c.is_connected())
                    .count();
                ui.label(
                    RichText::new(format!(
                        "v{}  ·  {}  ·  {connected}/{} connected",
                        env!("CARGO_PKG_VERSION"),
                        if self.demo {
                            "Demo mode (mock data)"
                        } else {
                            "Live"
                        },
                        self.profiles.len()
                    ))
                    .size(12.0)
                    .color(color::text_faint()),
                );
                ui.add_space(16.0);
                self.update.status_item(ui);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let Some(msg) = &self.status else {
                        ui.label(RichText::new("Ready").size(12.0).color(color::text_dim()));
                        return;
                    };
                    let (glyph, c) = match msg.severity {
                        Severity::Info => (icon::INFO, color::text_dim()),
                        Severity::Error => (icon::WARNING, color::danger()),
                    };
                    // Server versions and errors can span several lines: one line here, all of
                    // it on hover.
                    let one_line = msg.text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let label = ui.add(
                        egui::Label::new(
                            RichText::new(format!("{glyph}  {one_line}"))
                                .size(12.0)
                                .color(c),
                        )
                        .truncate(),
                    );
                    if one_line != msg.text || label.rect.width() >= ui.available_width() {
                        label.on_hover_text(msg.text.as_str());
                    }
                });
            });
        });
    }
}
