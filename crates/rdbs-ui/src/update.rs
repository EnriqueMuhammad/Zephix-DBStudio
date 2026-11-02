//! Update check: Help > Check for Updates, the start-up check, its toast and the status-bar
//! notice (ADR 0013). The worker does the HTTP request; this module only shows the answer.

use egui::{Align2, Id, RichText, Ui, Vec2};
use egui_phosphor::regular as icon;
use rdbs_core::update::{RELEASES_PAGE_URL, Release, Version, newer_release};

use crate::theme::color;

/// Persisted: check for a new version when the app starts (`true` unless turned off).
pub(crate) const CHECK_ON_START_KEY: &str = "rdbs.update.check_on_start";
/// Persisted: the version the user chose to skip (no toast or status-bar notice for it).
pub(crate) const SKIPPED_VERSION_KEY: &str = "rdbs.update.skipped_version";

/// How long the start-up toast stays (it waits while the pointer is over it).
const TOAST_SECONDS: f64 = 15.0;

/// What the update dialog shows.
enum Dialog {
    Checking,
    UpToDate(Option<Version>),
    Available(Release),
    Failed(String),
}

/// Update-check state owned by the app.
pub(crate) struct UpdateState {
    /// Check once at start-up.
    pub check_on_start: bool,
    /// Version (`X.Y.Z`) the user skipped.
    pub skipped: Option<String>,
    /// The running version. A field so tests can pretend to be older.
    current: Version,
    /// A newer release than [`Self::current`], from the last successful check.
    available: Option<Release>,
    /// A request is in flight.
    checking: bool,
    dialog: Option<Dialog>,
    /// When the toast appeared (`egui` time), while it is shown.
    toast_since: Option<f64>,
}

/// Something the update UI asks the app to do.
pub(crate) enum UpdateAction {
    /// Send `Command::CheckForUpdates { manual: true }`.
    CheckNow,
}

impl UpdateState {
    pub fn new(check_on_start: bool, skipped: Option<String>) -> Self {
        let current = Version::current();
        // Debug builds can pretend to be another version to try the update notice.
        #[cfg(debug_assertions)]
        let current = std::env::var("ZEPHIX_PRETEND_VERSION")
            .ok()
            .and_then(|v| Version::parse(&v))
            .unwrap_or(current);
        Self {
            check_on_start,
            skipped,
            current,
            available: None,
            checking: false,
            dialog: None,
            toast_since: None,
        }
    }

    /// Marks a request as sent. A manual one opens the dialog on "Checking…".
    pub fn started(&mut self, manual: bool) {
        self.checking = true;
        if manual {
            self.dialog = Some(Dialog::Checking);
        }
    }

    /// Whether a check is already running (the menu item is disabled meanwhile).
    pub fn is_checking(&self) -> bool {
        self.checking
    }

    /// The update dialog is open (it owns the keyboard).
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Handles `Event::UpdateChecked`.
    pub fn finished(&mut self, manual: bool, result: Result<Option<Release>, String>) {
        self.checking = false;
        match result {
            Ok(latest) => {
                let latest_version = latest.as_ref().map(|r| r.version);
                self.available = newer_release(self.current, latest);
                let manual_dialog = manual || matches!(self.dialog, Some(Dialog::Checking));
                if manual_dialog {
                    self.dialog = Some(match &self.available {
                        Some(release) => Dialog::Available(release.clone()),
                        None => Dialog::UpToDate(latest_version),
                    });
                } else if self.notice().is_some() {
                    // Shown on the next frame, which has the clock.
                    self.toast_since = Some(f64::NAN);
                }
            }
            Err(message) => {
                tracing::info!("update check failed: {message}");
                if manual || matches!(self.dialog, Some(Dialog::Checking)) {
                    self.dialog = Some(Dialog::Failed(message));
                }
            }
        }
    }

    /// The newer release to point at in the status bar and toast, unless it was skipped.
    fn notice(&self) -> Option<&Release> {
        self.available
            .as_ref()
            .filter(|r| self.skipped.as_deref() != Some(r.version.to_string().as_str()))
    }

    fn skip(&mut self, release: &Release) {
        self.skipped = Some(release.version.to_string());
        self.toast_since = None;
    }

    /// Help-menu entries. Returns an action for the app.
    pub fn menu(&mut self, ui: &mut Ui) -> Option<UpdateAction> {
        let mut action = None;
        let label = if self.checking {
            format!("{}  Checking for Updates…", icon::ARROWS_CLOCKWISE)
        } else {
            format!("{}  Check for Updates…", icon::ARROWS_CLOCKWISE)
        };
        if ui
            .add_enabled(!self.checking, egui::Button::new(label))
            .clicked()
        {
            action = Some(UpdateAction::CheckNow);
        }
        ui.checkbox(&mut self.check_on_start, "Check for Updates at Start-up");
        action
    }

    /// The clickable "New version available" item in the status bar (right-to-left layout).
    pub fn status_item(&mut self, ui: &mut Ui) {
        let Some(release) = self.notice() else {
            return;
        };
        let text = format!(
            "{}  New version {} available",
            icon::ARROW_CIRCLE_UP,
            release.version
        );
        let clicked = ui
            .add(
                egui::Label::new(RichText::new(text).size(12.0).color(color::accent()))
                    .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Show what is new and where to download it")
            .clicked();
        if clicked {
            self.dialog = Some(Dialog::Available(release.clone()));
            self.toast_since = None;
        }
        ui.add_space(16.0);
    }

    /// The start-up toast in the bottom-right corner, above the status bar.
    pub fn toast(&mut self, ctx: &egui::Context) {
        let Some(since) = self.toast_since else {
            return;
        };
        let Some(release) = self.notice().cloned() else {
            self.toast_since = None;
            return;
        };
        let now = ctx.input(|i| i.time);
        let since = if since.is_nan() {
            self.toast_since = Some(now);
            now
        } else {
            since
        };
        let mut close = false;
        let mut view = false;
        let mut skip = false;
        let area = egui::Area::new(Id::new("update-toast"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-14.0, -34.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icon::ARROW_CIRCLE_UP)
                                .size(20.0)
                                .color(color::accent()),
                        );
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} {} is available",
                                    rdbs_core::brand::APP_NAME,
                                    release.version
                                ))
                                .strong(),
                            );
                            ui.label(
                                RichText::new(format!("You have {}.", self.current))
                                    .small()
                                    .color(color::text_dim()),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            close = ui
                                .add(egui::Button::new(icon::X).frame(false))
                                .on_hover_text("Close")
                                .clicked();
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        view = ui
                            .button(format!("{}  View Release", icon::ARROW_SQUARE_OUT))
                            .clicked();
                        skip = ui
                            .button("Skip This Version")
                            .on_hover_text("No reminder until a later version is released")
                            .clicked();
                    });
                });
            });
        if view {
            ctx.open_url(egui::OpenUrl::new_tab(&release.url));
        }
        if skip {
            self.skip(&release);
        } else if close || view {
            self.toast_since = None;
        } else if area.response.contains_pointer() {
            // Restart the clock while the pointer rests on the toast.
            self.toast_since = Some(now);
        } else if now - since >= TOAST_SECONDS {
            self.toast_since = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                TOAST_SECONDS - (now - since),
            ));
        }
    }

    /// The update dialog (manual check, or the status-bar item).
    pub fn dialog(&mut self, ctx: &egui::Context) -> Option<UpdateAction> {
        let dialog = self.dialog.as_ref()?;
        let mut close = false;
        let mut action = None;
        let mut skip = None;
        let current = self.current;
        let skipped = self.skipped.clone();
        let response = egui::Modal::new(Id::new("update-dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Check for Updates");
            ui.add_space(6.0);
            match dialog {
                Dialog::Checking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label("Looking for the latest release…");
                    });
                }
                Dialog::UpToDate(latest) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::CHECK_CIRCLE).color(color::success()));
                        ui.label(format!(
                            "{} {current} is up to date.",
                            rdbs_core::brand::APP_NAME
                        ));
                    });
                    match latest {
                        Some(v) if *v < current => {
                            ui.label(
                                RichText::new(format!("The latest published release is {v}."))
                                    .small()
                                    .color(color::text_dim()),
                            );
                        }
                        None => {
                            ui.label(
                                RichText::new("No release has been published yet.")
                                    .small()
                                    .color(color::text_dim()),
                            );
                        }
                        _ => {}
                    }
                }
                Dialog::Available(release) => {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icon::ARROW_CIRCLE_UP)
                                .size(18.0)
                                .color(color::accent()),
                        );
                        ui.label(
                            RichText::new(format!("New version {} available", release.version))
                                .strong(),
                        );
                    });
                    ui.label(
                        RichText::new(format!("You have {current}.")).color(color::text_dim()),
                    );
                    ui.label(
                        RichText::new(
                            "The release page has the installer, the portable executable \
                             and their SHA-256 checksums.",
                        )
                        .small()
                        .color(color::text_faint()),
                    );
                    if skipped.as_deref() == Some(release.version.to_string().as_str()) {
                        ui.label(
                            RichText::new("You skipped this version.")
                                .small()
                                .color(color::text_faint()),
                        );
                    }
                }
                Dialog::Failed(message) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::WARNING).color(color::danger()));
                        ui.label("Could not check for updates.");
                    });
                    ui.label(RichText::new(message).small().color(color::text_dim()));
                }
            }
            ui.add_space(8.0);
            ui.checkbox(&mut self.check_on_start, "Check for updates at start-up");
            ui.add_space(8.0);
            ui.horizontal(|ui| match dialog {
                Dialog::Available(release) => {
                    if ui
                        .button(format!("{}  Open Release Page", icon::ARROW_SQUARE_OUT))
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&release.url));
                        close = true;
                    }
                    if skipped.as_deref() != Some(release.version.to_string().as_str())
                        && ui
                            .button("Skip This Version")
                            .on_hover_text("No reminder until a later version is released")
                            .clicked()
                    {
                        skip = Some(release.clone());
                        close = true;
                    }
                    close |= ui.button("Later").clicked();
                }
                Dialog::Failed(_) => {
                    if ui.button("Try Again").clicked() {
                        action = Some(UpdateAction::CheckNow);
                    }
                    if ui.button("Releases Page").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(RELEASES_PAGE_URL));
                    }
                    close |= ui.button("Close").clicked();
                }
                Dialog::Checking | Dialog::UpToDate(_) => {
                    close |= ui.button("Close").clicked();
                }
            });
        });
        if let Some(release) = skip {
            self.skip(&release);
        }
        if close || response.should_close() {
            self.dialog = None;
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> Release {
        Release {
            version: Version::parse(version).unwrap_or(Version {
                major: 0,
                minor: 0,
                patch: 0,
            }),
            tag: format!("v{version}"),
            url: format!("https://github.com/x/y/releases/tag/v{version}"),
            name: format!("Zephix {version}"),
        }
    }

    fn state(current: &str) -> UpdateState {
        let mut s = UpdateState::new(true, None);
        s.current = Version::parse(current).unwrap_or(s.current);
        s
    }

    #[test]
    fn start_up_check_shows_toast_and_notice_for_newer_release() {
        let mut s = state("0.3.2");
        s.started(false);
        assert!(s.is_checking() && !s.dialog_open());
        s.finished(false, Ok(Some(release("0.4.0"))));
        assert!(!s.is_checking());
        assert!(!s.dialog_open(), "a start-up check never opens the dialog");
        assert_eq!(s.notice().map(|r| r.tag.as_str()), Some("v0.4.0"));
        assert!(s.toast_since.is_some());
    }

    #[test]
    fn start_up_check_stays_quiet_when_current_or_failed() {
        let mut s = state("0.3.2");
        s.finished(false, Ok(Some(release("0.3.2"))));
        assert!(s.notice().is_none() && s.toast_since.is_none() && !s.dialog_open());
        s.finished(false, Err("offline".into()));
        assert!(s.notice().is_none() && s.toast_since.is_none() && !s.dialog_open());
    }

    #[test]
    fn manual_check_reports_every_outcome() {
        let mut s = state("0.3.2");
        s.started(true);
        assert!(matches!(s.dialog, Some(Dialog::Checking)));
        s.finished(true, Ok(Some(release("0.3.2"))));
        assert!(matches!(s.dialog, Some(Dialog::UpToDate(Some(_)))));
        s.finished(true, Ok(Some(release("0.3.3"))));
        assert!(matches!(&s.dialog, Some(Dialog::Available(r)) if r.tag == "v0.3.3"));
        assert!(s.toast_since.is_none(), "the dialog replaces the toast");
        s.finished(true, Err("HTTP 500".into()));
        assert!(matches!(&s.dialog, Some(Dialog::Failed(m)) if m == "HTTP 500"));
    }

    #[test]
    fn skipping_hides_that_version_only() {
        let mut s = state("0.3.2");
        s.finished(false, Ok(Some(release("0.4.0"))));
        s.skip(&release("0.4.0"));
        assert_eq!(s.skipped.as_deref(), Some("0.4.0"));
        assert!(s.notice().is_none() && s.toast_since.is_none());
        s.finished(false, Ok(Some(release("0.4.0"))));
        assert!(s.toast_since.is_none(), "no toast for a skipped version");
        s.finished(false, Ok(Some(release("0.4.1"))));
        assert!(s.notice().is_some() && s.toast_since.is_some());
    }
}
