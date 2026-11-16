//! Modal dialogs: connection, password, routine execute, confirmations and About.

use super::*;

impl StudioApp {
    pub(super) fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        match dialog.show(ctx) {
            DialogAction::None => {}
            DialogAction::Cancel => self.dialog = None,
            DialogAction::Save(profile, password) => self.save_profile(profile, password),
            DialogAction::Test(profile, password) => {
                let request_id = self.services.next_request_id();
                self.services.send(Command::TestConnection {
                    request_id,
                    profile,
                    password,
                });
                dialog.test = TestState::Running(request_id);
            }
        }
    }

    pub(super) fn show_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let Some(profile) = self
            .profiles
            .iter()
            .find(|p| p.id == prompt.profile_id)
            .cloned()
        else {
            self.prompt = None;
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let response = egui::Modal::new(Id::new("password-prompt")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                widgets::kind_badge(ui, profile.kind, 22.0);
                let verb = if prompt.then == AfterPassword::Reconnect {
                    "Reconnect to"
                } else {
                    "Connect to"
                };
                ui.label(
                    RichText::new(format!("{verb} {}", profile.name))
                        .size(16.0)
                        .strong(),
                );
            });
            ui.label(
                RichText::new(format!("{}@{}", profile.username, profile.endpoint()))
                    .color(color::text_dim()),
            );
            if let Some(reason) = &prompt.reason {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("{}  {reason}", icon::WARNING)).color(color::danger()),
                );
            }
            ui.add_space(8.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut prompt.password)
                    .password(true)
                    .hint_text("Password")
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut prompt.focus) {
                field.request_focus();
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                submit = true;
            }
            ui.checkbox(&mut prompt.remember, "Save password in OS keychain");
            ui.add_space(8.0);
            let verb = if prompt.then == AfterPassword::Reconnect {
                "Reconnect"
            } else {
                "Connect"
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connect = Button::new(RichText::new(verb).color(Color32::WHITE).strong())
                    .fill(color::accent())
                    .min_size(Vec2::new(96.0, 26.0));
                if ui.add(connect).clicked() {
                    submit = true;
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        if cancel || (!submit && response.should_close()) {
            if matches!(
                self.prompt.as_ref().map(|prompt| prompt.then),
                Some(AfterPassword::Reconnect)
            ) {
                self.reconnect = None;
            }
            self.prompt = None;
            return;
        }
        if !submit {
            return;
        }
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        if prompt.remember {
            let mut updated = profile.clone();
            updated.save_password = true;
            self.save_profile(updated, Some(prompt.password));
        } else {
            self.session_passwords.insert(profile.id, prompt.password);
        }
        match prompt.then {
            AfterPassword::Connect => self.connect(profile.id),
            AfterPassword::RunTab(tab_id) => {
                let follow_up = self.run_tab(tab_id);
                self.apply(follow_up);
            }
            AfterPassword::Reconnect => self.send_reconnect(&profile),
        }
    }

    pub(super) fn show_exec_dialog(&mut self, ctx: &egui::Context) {
        let Some(exec) = &mut self.exec else {
            return;
        };
        let mut run = false;
        let mut cancel = false;
        let what = if exec.target.routine == rdbs_core::ObjectKind::Function {
            "function"
        } else {
            "procedure"
        };
        let response = egui::Modal::new(Id::new("exec-routine")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(
                RichText::new(format!(
                    "{}  Execute {what} {}",
                    icon::PLAY,
                    exec.target.name
                ))
                .size(16.0)
                .strong(),
            );
            ui.label(
                RichText::new("Text is quoted for you; numbers are passed as they are.")
                    .color(color::text_dim()),
            );
            ui.add_space(8.0);
            egui::Grid::new("exec-params")
                .num_columns(4)
                .spacing([10.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for h in ["Parameter", "Type", "Value", "NULL"] {
                        ui.label(RichText::new(h).color(color::text_dim()));
                    }
                    ui.end_row();
                    for (i, p) in exec.signature.params.iter().enumerate() {
                        let name = if p.name.is_empty() {
                            format!("${}", i + 1)
                        } else {
                            p.name.clone()
                        };
                        ui.label(RichText::new(name).strong());
                        ui.label(
                            RichText::new(format!("{} · {}", p.data_type, p.mode.label()))
                                .color(color::text_dim()),
                        );
                        if p.mode.takes_value() {
                            let null = exec.nulls[i];
                            ui.add_enabled_ui(!null, |ui| {
                                ui.add_sized(
                                    [240.0, ui.spacing().interact_size.y],
                                    egui::TextEdit::singleline(&mut exec.values[i])
                                        .hint_text(if null { "NULL" } else { "" }),
                                )
                            });
                            ui.checkbox(&mut exec.nulls[i], "");
                        } else {
                            ui.label(RichText::new("returned").color(color::text_faint()));
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let execute = Button::new(
                    RichText::new(format!("{}  Execute", icon::PLAY))
                        .color(Color32::WHITE)
                        .strong(),
                )
                .fill(color::accent())
                .min_size(Vec2::new(110.0, 26.0));
                if ui.add(execute).clicked() {
                    run = true;
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    cancel = true;
                }
            });
        });
        if cancel || (!run && response.should_close()) {
            self.exec = None;
            return;
        }
        if !run {
            return;
        }
        let Some(exec) = self.exec.take() else {
            return;
        };
        let args: Vec<rdbs_core::routine::ArgValue> = exec
            .values
            .into_iter()
            .zip(exec.nulls)
            .map(|(value, null)| {
                if null {
                    rdbs_core::routine::ArgValue::Null
                } else {
                    rdbs_core::routine::ArgValue::Text(value)
                }
            })
            .collect();
        let sql = rdbs_core::routine::call_statement(
            exec.target.kind,
            &exec.target.qualified,
            exec.target.routine,
            &exec.signature,
            &args,
        );
        self.run_routine(&exec.target, sql);
    }

    pub(super) fn show_confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else { return };
        let mut decision: Option<bool> = None;
        let profiles = &self.profiles;
        let name_of = |id: ProfileId| {
            profiles
                .iter()
                .find(|p| p.id == id)
                .map_or_else(String::new, |p| p.name.clone())
        };

        let response = egui::Modal::new(Id::new("confirm-dialog")).show(ctx, |ui| {
            ui.set_width(480.0);
            let agent_sql = match confirm {
                Confirmation::AgentJob {
                    job_name, action, ..
                } => Some(rdbs_core::agent::action_sql(job_name, action)),
                _ => None,
            };
            let (title, body, confirm_label, preview) = match confirm {
                Confirmation::Execute {
                    profile_id,
                    sql,
                    kind,
                    ..
                } => (
                    format!("{}  Run on production?", icon::SHIELD_WARNING),
                    format!(
                        "This script contains a {} statement and will run on “{}” ({}).",
                        kind.label(),
                        name_of(*profile_id),
                        Environment::Production.label()
                    ),
                    "Run on production".to_owned(),
                    Some(sql.as_str()),
                ),
                Confirmation::SaveEdits { rows, .. } => (
                    format!("{}  Save changes on production?", icon::SHIELD_WARNING),
                    format!(
                        "{rows} row(s) will be changed in a single transaction on a {} connection.",
                        Environment::Production.label()
                    ),
                    "Save on production".to_owned(),
                    None,
                ),
                Confirmation::ClearHistory => (
                    format!("{}  Clear query history?", icon::TRASH),
                    "All recorded queries will be deleted. This cannot be undone.".to_owned(),
                    "Clear history".to_owned(),
                    None,
                ),
                Confirmation::DeleteProfile(id) => (
                    format!("{}  Delete connection?", icon::TRASH),
                    format!(
                        "“{}” will be removed from your saved connections. This cannot be undone.",
                        name_of(*id)
                    ),
                    "Delete".to_owned(),
                    None,
                ),
                Confirmation::AgentJob {
                    profile_id,
                    job_name,
                    action,
                } => {
                    let production = profiles.iter().any(|p| {
                        p.id == *profile_id && p.environment.requires_write_confirmation()
                    });
                    let prompt = rdbs_core::agent::action_prompt(
                        job_name,
                        action,
                        &name_of(*profile_id),
                        production,
                    );
                    let glyph = if production {
                        icon::SHIELD_WARNING
                    } else {
                        icon::STOP
                    };
                    (
                        format!("{glyph}  {}", prompt.title),
                        prompt.body,
                        prompt.confirm_label,
                        agent_sql.as_deref(),
                    )
                }
            };
            ui.label(
                RichText::new(title)
                    .size(16.0)
                    .strong()
                    .color(color::warning()),
            );
            ui.add_space(6.0);
            ui.label(body);
            if let Some(sql) = preview {
                ui.add_space(6.0);
                Frame::new()
                    .fill(color::editor())
                    .stroke(Stroke::new(1.0, color::border()))
                    .corner_radius(4)
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                let shown: String = sql.chars().take(2_000).collect();
                                ui.label(widgets::highlight_sql(&shown, &theme::mono()));
                            });
                    });
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let danger =
                    Button::new(RichText::new(confirm_label).color(Color32::WHITE).strong())
                        .fill(color::danger())
                        .min_size(Vec2::new(120.0, 26.0));
                if ui.add(danger).clicked() {
                    decision = Some(true);
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    decision = Some(false);
                }
            });
        });
        if decision.is_none() && response.should_close() {
            decision = Some(false);
        }

        let Some(accepted) = decision else { return };
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        if !accepted {
            return;
        }
        match confirm {
            Confirmation::Execute {
                tab_id,
                profile_id,
                sql,
                ..
            } => {
                let Some(profile) = self.profile(profile_id).cloned() else {
                    return;
                };
                let password = self.session_passwords.get(&profile_id).cloned();
                if let Some(q) = self.workspace.query_tab_mut(tab_id) {
                    editor::start_query(
                        q,
                        &profile,
                        sql,
                        &self.services,
                        password,
                        &self.connections,
                    );
                }
            }
            Confirmation::DeleteProfile(id) => self.delete_profile(id),
            Confirmation::AgentJob {
                profile_id,
                job_name,
                action,
            } => self.send_agent_job(profile_id, job_name, action),
            Confirmation::SaveEdits { tab_id, .. } => self.save_edits(tab_id, true),
            Confirmation::ClearHistory => {
                if let Err(e) = self.store.clear_history() {
                    self.status = Some(StatusMessage::error(format!(
                        "Could not clear history: {e}"
                    )));
                }
                self.reload_history();
            }
        }
    }

    pub(super) fn about_modal(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let response = egui::Modal::new(Id::new("about-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(icon::DATABASE)
                        .size(36.0)
                        .color(color::accent()),
                );
                ui.label(
                    RichText::new(rdbs_core::brand::APP_NAME)
                        .size(22.0)
                        .strong(),
                );
                ui.label(RichText::new(rdbs_core::brand::APP_TAGLINE).color(color::text()));
                ui.label(
                    RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                        .color(color::text_dim()),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Author & creator")
                        .small()
                        .color(color::text_faint()),
                );
                ui.label(RichText::new(rdbs_core::brand::APP_AUTHOR).strong());
                ui.add_space(6.0);
                ui.label("Native, clean-room database IDE written in Rust with egui.");
                ui.label(
                    RichText::new(format!(
                        "© 2026 {}. All rights reserved.",
                        rdbs_core::brand::APP_AUTHOR
                    ))
                    .small()
                    .color(color::text_faint()),
                );
                ui.add_space(8.0);
                ui.button("Close").clicked()
            })
            .inner
        });
        if response.inner || response.should_close() {
            self.show_about = false;
        }
    }
}
