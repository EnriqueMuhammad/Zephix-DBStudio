//! Connection dialog (new / edit profile).

use egui::{Align, Button, Color32, Grid, Layout, RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icon;
use rdbs_core::profile::{Field, ValidationIssue};
use rdbs_core::{ConnectionProfile, DatabaseKind, DbError, Environment, RequestId, SslMode};

use crate::theme::{self, color};
use crate::widgets;

/// Dialog section tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Ssl,
    Ssh,
    Advanced,
}

/// State of the "Test connection" button.
#[derive(Debug, Clone)]
pub enum TestState {
    /// Not run yet (or invalidated by an edit).
    Idle,
    /// Waiting for the worker.
    Running(RequestId),
    /// Succeeded; carries the server version.
    Succeeded(String),
    /// Failed.
    Failed(DbError),
}

/// What the user did in the dialog this frame.
#[derive(Debug, Clone)]
pub enum DialogAction {
    /// Nothing yet.
    None,
    /// Close without saving.
    Cancel,
    /// Save the profile, with the password typed in the dialog (if any).
    Save(ConnectionProfile, Option<String>),
    /// Run a connection test with the draft profile.
    Test(ConnectionProfile, Option<String>),
}

/// The connection dialog.
pub struct ConnectionDialog {
    draft: ConnectionProfile,
    password: String,
    /// SSH password or key passphrase typed in the SSH section.
    ssh_secret: String,
    is_new: bool,
    section: Section,
    issues: Vec<ValidationIssue>,
    /// Connection test state; the app updates it when the worker answers.
    pub test: TestState,
}

impl ConnectionDialog {
    /// Dialog for a new profile.
    pub fn new_profile() -> Self {
        Self::with_draft(ConnectionProfile::new(DatabaseKind::PostgreSql), true)
    }

    /// Dialog editing an existing profile (works on a copy until saved).
    pub fn edit(profile: &ConnectionProfile) -> Self {
        Self::with_draft(profile.clone(), false)
    }

    fn with_draft(draft: ConnectionProfile, is_new: bool) -> Self {
        Self {
            draft,
            password: String::new(),
            ssh_secret: String::new(),
            is_new,
            section: Section::General,
            issues: Vec::new(),
            test: TestState::Idle,
        }
    }

    /// Renders the dialog as a modal window.
    pub fn show(&mut self, ctx: &egui::Context) -> DialogAction {
        let mut action = DialogAction::None;
        let modal = egui::Modal::new(egui::Id::new("connection-dialog")).show(ctx, |ui| {
            ui.set_width(760.0);
            self.header(ui);
            ui.separator();
            // Fixed body size: inside a modal the available height is unbounded, and the
            // vertical separator below would otherwise stretch to fill it.
            let body = Vec2::new(ui.available_width(), 390.0);
            ui.allocate_ui_with_layout(body, Layout::left_to_right(Align::Min), |ui| {
                ui.set_min_size(body);
                self.engine_list(ui);
                ui.separator();
                ui.vertical(|ui| {
                    self.section_tabs(ui);
                    ui.add_space(8.0);
                    match self.section {
                        Section::General => self.general(ui),
                        Section::Ssl => self.ssl(ui),
                        Section::Ssh => self.ssh(ui),
                        Section::Advanced => coming_soon(
                            ui,
                            icon::WRENCH,
                            "Advanced options",
                            "Timeouts, keep-alive, application name, read-only sessions and \
                             driver-specific parameters. Planned for Milestone 3.",
                        ),
                    }
                });
            });
            self.test_error(ui);
            ui.separator();
            action = self.footer(ui);
        });
        if matches!(action, DialogAction::None) && modal.should_close() {
            action = DialogAction::Cancel;
        }
        action
    }

    fn header(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            widgets::kind_badge(ui, self.draft.kind, 26.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let title = if self.is_new {
                    format!("New {} Connection", self.draft.kind)
                } else {
                    format!("Edit Connection — {}", self.draft.name)
                };
                ui.label(RichText::new(title).size(16.0).strong());
                ui.label(
                    RichText::new("Passwords are stored in the OS keychain, never on disk")
                        .small()
                        .color(color::text_faint()),
                );
            });
        });
    }

    fn engine_list(&mut self, ui: &mut Ui) {
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.label(
                RichText::new("ENGINE")
                    .size(11.0)
                    .strong()
                    .color(color::text_dim()),
            );
            ui.add_space(2.0);
            for kind in DatabaseKind::ALL {
                let selected = self.draft.kind == kind;
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
                if selected {
                    ui.painter().rect_filled(rect, 4, color::selection());
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 4, color::surface());
                }
                let badge = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + 16.0, rect.center().y),
                    Vec2::splat(20.0),
                );
                widgets::paint_kind_badge(ui, badge, kind);
                ui.painter().text(
                    egui::pos2(badge.right() + 8.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    kind.display_name(),
                    egui::FontId::proportional(13.5),
                    color::text(),
                );
                if response.clicked() && !selected {
                    self.draft.set_kind(kind);
                    self.on_edit();
                }
            }
        });
    }

    fn section_tabs(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (section, label) in [
                (Section::General, format!("{}  General", icon::PLUG)),
                (Section::Ssl, format!("{}  SSL", icon::LOCK)),
                (Section::Ssh, format!("{}  SSH", icon::LOCK_KEY)),
                (Section::Advanced, format!("{}  Advanced", icon::WRENCH)),
            ] {
                let enabled = self.draft.kind.is_networked() || section == Section::General;
                if ui
                    .add_enabled(enabled, Button::selectable(self.section == section, label))
                    .clicked()
                {
                    self.section = section;
                }
            }
        });
    }

    fn issue(&self, field: Field) -> Option<&'static str> {
        self.issues
            .iter()
            .find(|i| i.field == field)
            .map(|i| i.message)
    }

    fn general(&mut self, ui: &mut Ui) {
        let mut changed = false;
        let networked = self.draft.kind.is_networked();
        let name_issue = self.issue(Field::Name);
        let host_issue = self.issue(Field::Host);
        let port_issue = self.issue(Field::Port);
        let user_issue = self.issue(Field::Username);

        Grid::new("conn-general")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(110.0)
            .show(ui, |ui| {
                ui.label("Connection name");
                changed |= field(
                    ui,
                    &mut self.draft.name,
                    "e.g. Orders (staging)",
                    name_issue,
                );
                ui.end_row();

                ui.label(if networked { "Host" } else { "Database file" });
                let hint = if networked {
                    "hostname or IP"
                } else {
                    "/path/to/database.db"
                };
                if networked {
                    changed |= field(ui, &mut self.draft.host, hint, host_issue);
                } else {
                    ui.vertical(|ui| {
                        changed |= field(ui, &mut self.draft.host, hint, host_issue);
                        ui.horizontal(|ui| {
                            let filter = ["db", "sqlite", "sqlite3", "db3"];
                            if ui.button(format!("{}  Open…", icon::FOLDER_OPEN)).clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("SQLite database", &filter)
                                    .add_filter("All files", &["*"])
                                    .pick_file()
                            {
                                self.draft.host = path.display().to_string();
                                changed = true;
                            }
                            if ui
                                .button(format!("{}  New file…", icon::FILE_PLUS))
                                .on_hover_text("Create an empty SQLite database file")
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("SQLite database", &filter)
                                    .set_file_name("database.sqlite")
                                    .save_file()
                            {
                                // An empty file is a valid, empty SQLite database.
                                match std::fs::OpenOptions::new()
                                    .write(true)
                                    .create_new(true)
                                    .open(&path)
                                {
                                    Ok(_) => {}
                                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                                    Err(e) => {
                                        self.test = TestState::Failed(rdbs_core::DbError::new(
                                            rdbs_core::DbErrorKind::Internal,
                                            format!("could not create {}: {e}", path.display()),
                                        ));
                                        return;
                                    }
                                }
                                self.draft.host = path.display().to_string();
                                changed = true;
                            }
                        });
                    });
                }
                ui.end_row();

                if networked {
                    ui.label("Port");
                    ui.vertical(|ui| {
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.draft.port).range(0..=65535))
                            .changed();
                        issue_text(ui, port_issue);
                    });
                    ui.end_row();

                    let (db_label, db_hint) = match self.draft.kind {
                        DatabaseKind::Oracle => ("Service name", "FREEPDB1"),
                        DatabaseKind::Redis => ("Database index", "0"),
                        DatabaseKind::MongoDb => ("Database", "test"),
                        _ => ("Initial database", "server default"),
                    };
                    ui.label(db_label);
                    changed |= field(ui, &mut self.draft.database, db_hint, None);
                    ui.end_row();

                    ui.label("User name");
                    let user_hint = if self.draft.kind.requires_username() {
                        ""
                    } else {
                        "optional"
                    };
                    changed |= field(ui, &mut self.draft.username, user_hint, user_issue);
                    ui.end_row();

                    ui.label("Password");
                    ui.vertical(|ui| {
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.password)
                                    .password(true)
                                    .hint_text(if self.is_new || !self.draft.save_password {
                                        ""
                                    } else {
                                        "unchanged"
                                    })
                                    .desired_width(280.0),
                            )
                            .changed();
                        ui.checkbox(
                            &mut self.draft.save_password,
                            "Save password in OS keychain",
                        );
                    });
                    ui.end_row();
                }

                ui.label("Environment");
                ui.horizontal(|ui| {
                    for env in Environment::ALL {
                        let c = theme::env_color(env);
                        let text = RichText::new(format!("{}  {}", icon::CIRCLE, env.label()));
                        let text = if self.draft.environment == env {
                            text.color(c)
                        } else {
                            text
                        };
                        if ui
                            .add(Button::selectable(self.draft.environment == env, text))
                            .clicked()
                        {
                            self.draft.environment = env;
                        }
                    }
                });
                ui.end_row();
            });

        if self.draft.environment == Environment::Production {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{}  Data-modifying and schema statements will ask for confirmation.",
                    icon::SHIELD_WARNING
                ))
                .color(color::warning()),
            );
        }
        if changed {
            self.on_edit();
        }
    }

    fn ssl(&mut self, ui: &mut Ui) {
        Grid::new("conn-ssl")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(110.0)
            .show(ui, |ui| {
                ui.label("SSL mode");
                egui::ComboBox::from_id_salt("ssl-mode")
                    .selected_text(self.draft.ssl_mode.label())
                    .show_ui(ui, |ui| {
                        for mode in SslMode::ALL {
                            ui.selectable_value(&mut self.draft.ssl_mode, mode, mode.label());
                        }
                    });
                ui.end_row();
                for label in ["CA certificate", "Client certificate", "Client key"] {
                    ui.label(label);
                    ui.add_enabled(
                        false,
                        egui::TextEdit::singleline(&mut String::new())
                            .hint_text("file picker — Milestone 3")
                            .desired_width(280.0),
                    );
                    ui.end_row();
                }
            });
    }

    /// Full, wrapped error of a failed connection test.
    fn test_error(&self, ui: &mut Ui) {
        let TestState::Failed(e) = &self.test else {
            return;
        };
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(color::danger().gamma_multiply(0.08))
            .stroke(egui::Stroke::new(1.0, color::danger().gamma_multiply(0.4)))
            .corner_radius(4)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let headline = match &e.code {
                    Some(code) => format!("{}  [{code}] {}", icon::X_CIRCLE, e.message),
                    None => format!("{}  {}", icon::X_CIRCLE, e.message),
                };
                ui.add(
                    egui::Label::new(RichText::new(headline).color(color::danger()))
                        .wrap()
                        .selectable(true),
                );
                if let Some(detail) = &e.detail {
                    ui.add(
                        egui::Label::new(RichText::new(detail).color(color::text_dim()))
                            .wrap()
                            .selectable(true),
                    );
                }
            });
    }

    fn footer(&mut self, ui: &mut Ui) -> DialogAction {
        let mut action = DialogAction::None;
        ui.horizontal(|ui| {
            let testing = matches!(self.test, TestState::Running(_));
            if ui
                .add_enabled(
                    !testing,
                    Button::new(format!("{}  Test Connection", icon::PLUGS_CONNECTED)),
                )
                .clicked()
            {
                match self.draft.validate() {
                    Ok(()) => {
                        let password = (!self.password.is_empty()).then(|| self.password.clone());
                        action = DialogAction::Test(self.draft_with_secret(), password);
                    }
                    Err(issues) => self.issues = issues,
                }
            }
            match &self.test {
                TestState::Idle => {}
                TestState::Running(_) => {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new("Testing…").color(color::text_dim()));
                }
                TestState::Succeeded(version) => {
                    ui.label(
                        RichText::new(format!("{}  Connected · {version}", icon::CHECK_CIRCLE))
                            .color(color::success()),
                    );
                }
                TestState::Failed(_) => {
                    // The full message is shown above the footer (see `test_error`), so the
                    // footer keeps a short status that never collides with the buttons.
                    ui.label(
                        RichText::new(format!("{}  Connection failed", icon::X_CIRCLE))
                            .color(color::danger()),
                    );
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let save = Button::new(RichText::new("Save").color(Color32::WHITE).strong())
                    .fill(color::accent())
                    .min_size(Vec2::new(84.0, 26.0));
                if ui.add(save).clicked() {
                    match self.draft.validate() {
                        Ok(()) => {
                            let password =
                                (!self.password.is_empty()).then(|| self.password.clone());
                            action = DialogAction::Save(self.draft_with_secret(), password);
                        }
                        Err(issues) => {
                            self.issues = issues;
                            self.section = Section::General;
                        }
                    }
                }
                if ui
                    .add(Button::new("Cancel").min_size(Vec2::new(84.0, 26.0)))
                    .clicked()
                {
                    action = DialogAction::Cancel;
                }
            });
        });
        action
    }

    /// The draft with the typed SSH secret attached (it is never persisted with the profile).
    fn draft_with_secret(&self) -> ConnectionProfile {
        let mut draft = self.draft.clone();
        if let Some(ssh) = draft.ssh.as_mut()
            && !self.ssh_secret.is_empty()
        {
            ssh.secret = Some(self.ssh_secret.clone());
        }
        draft
    }

    fn ssh(&mut self, ui: &mut Ui) {
        let mut changed = false;
        let mut enabled = self.draft.ssh.is_some();
        if ui
            .checkbox(&mut enabled, "Connect through an SSH tunnel")
            .changed()
        {
            self.draft.ssh = enabled.then(rdbs_core::SshTunnel::default);
            changed = true;
        }
        let host_issue = self.issue(Field::SshHost);
        let user_issue = self.issue(Field::SshUser);
        let key_issue = self.issue(Field::SshKey);
        let is_new = self.is_new;
        let Some(ssh) = self.draft.ssh.as_mut() else {
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Use a bastion host when the database is not reachable directly. \
                     The host and port on the General tab are then resolved on the SSH server.",
                )
                .color(color::text_dim()),
            );
            if changed {
                self.on_edit();
            }
            return;
        };
        ui.add_space(6.0);
        Grid::new("conn-ssh")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .min_col_width(110.0)
            .show(ui, |ui| {
                ui.label("SSH host");
                changed |= field(ui, &mut ssh.host, "bastion.example.com", host_issue);
                ui.end_row();

                ui.label("SSH port");
                changed |= ui
                    .add(egui::DragValue::new(&mut ssh.port).range(1..=65535))
                    .changed();
                ui.end_row();

                ui.label("SSH user");
                changed |= field(ui, &mut ssh.username, "", user_issue);
                ui.end_row();

                ui.label("Authentication");
                ui.horizontal(|ui| {
                    let is_password = matches!(ssh.auth, rdbs_core::SshAuth::Password);
                    if ui
                        .add(Button::selectable(is_password, "Password"))
                        .clicked()
                        && !is_password
                    {
                        ssh.auth = rdbs_core::SshAuth::Password;
                        changed = true;
                    }
                    if ui
                        .add(Button::selectable(!is_password, "Private key"))
                        .clicked()
                        && is_password
                    {
                        ssh.auth = rdbs_core::SshAuth::PrivateKey {
                            path: String::new(),
                        };
                        changed = true;
                    }
                });
                ui.end_row();

                let secret_label = match &mut ssh.auth {
                    rdbs_core::SshAuth::Password => "SSH password",
                    rdbs_core::SshAuth::PrivateKey { path } => {
                        ui.label("Key file");
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                changed |= ui
                                    .add(
                                        egui::TextEdit::singleline(path)
                                            .hint_text("~/.ssh/id_ed25519")
                                            .desired_width(220.0),
                                    )
                                    .changed();
                                if ui
                                    .button(format!("{}  Browse…", icon::FOLDER_OPEN))
                                    .clicked()
                                    && let Some(picked) = rfd::FileDialog::new().pick_file()
                                {
                                    *path = picked.display().to_string();
                                    changed = true;
                                }
                            });
                            issue_text(ui, key_issue);
                        });
                        ui.end_row();
                        "Passphrase"
                    }
                };
                ui.label(secret_label);
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.ssh_secret)
                            .password(true)
                            .hint_text(if is_new { "" } else { "unchanged" })
                            .desired_width(280.0),
                    )
                    .changed();
                ui.end_row();
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{}  The SSH secret is stored in the OS keychain. Host keys are trusted on first \
                 use; a changed key is refused.",
                icon::INFO
            ))
            .small()
            .color(color::text_faint()),
        );
        if changed {
            self.on_edit();
        }
    }

    /// Clears stale validation and test results after an edit.
    fn on_edit(&mut self) {
        if !self.issues.is_empty() {
            self.issues = self.draft.validate().err().unwrap_or_default();
        }
        if !matches!(self.test, TestState::Running(_)) {
            self.test = TestState::Idle;
        }
    }
}

fn field(ui: &mut Ui, value: &mut String, hint: &str, issue: Option<&str>) -> bool {
    ui.vertical(|ui| {
        let changed = ui
            .add(
                egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .desired_width(280.0),
            )
            .changed();
        issue_text(ui, issue);
        changed
    })
    .inner
}

fn issue_text(ui: &mut Ui, issue: Option<&str>) {
    if let Some(msg) = issue {
        ui.label(RichText::new(msg).small().color(color::danger()));
    }
}

fn coming_soon(ui: &mut Ui, glyph: &str, title: &str, text: &str) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(glyph).size(28.0).color(color::text_faint()));
        ui.label(RichText::new(title).strong());
        ui.add(egui::Label::new(RichText::new(text).color(color::text_dim())).wrap());
    });
}
