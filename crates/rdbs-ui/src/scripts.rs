//! Opening and saving query scripts (`.sql` files), and asking what to do with unsaved
//! changes when a tab closes.
//!
//! The native file dialogs are modal and block their thread, so they run on a helper thread;
//! the choice comes back through a channel and is applied at the start of a frame.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Context, Id, RichText};
use egui_phosphor::regular as icon;

use crate::theme::color;

/// Storage key of the folder last used to open or save a script.
pub const SCRIPT_DIR_KEY: &str = "rdbs.script_dir";

/// What a file dialog came back with.
#[derive(Debug)]
pub enum Picked {
    /// A script to open, already read.
    Open {
        /// File.
        path: PathBuf,
        /// Contents.
        text: String,
    },
    /// Where to save a tab.
    SaveTo {
        /// Tab.
        tab_id: u64,
        /// Chosen file.
        path: PathBuf,
        /// Close the tab once saved (it was being closed).
        then_close: bool,
    },
    /// The dialog was dismissed.
    Cancelled {
        /// A tab close was waiting on this dialog.
        then_close: bool,
    },
    /// The file could not be read.
    Failed(String),
}

/// The choice in the unsaved-changes prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    /// Save, then close.
    Save,
    /// Close without saving.
    Discard,
    /// Keep the tab open and stop closing tabs.
    Cancel,
}

/// The choice in the prompt shown when Zephix closes with unsaved scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitChoice {
    /// Save every unsaved script (asking for file names where needed), then quit.
    SaveAll,
    /// Drop the unsaved scripts and quit.
    Discard,
    /// Keep Zephix open.
    Cancel,
}

/// File dialogs in flight, the last folder, and the tabs waiting for a save decision.
pub struct Scripts {
    tx: Sender<Picked>,
    rx: Receiver<Picked>,
    /// Folder last used to open or save a script.
    pub last_dir: Option<PathBuf>,
    /// Tabs with unsaved changes that are being closed, in order; the first is asked about.
    pub close_queue: VecDeque<u64>,
    /// A file dialog is open (the prompt waits for it).
    pub dialog_open: bool,
    /// Zephix is closing and asks what to do with the unsaved scripts.
    pub quit_prompt: bool,
    /// "Save All" is saving the queued tabs one by one; Zephix closes when they are done.
    pub quitting: bool,
    /// The close may go ahead (the unsaved scripts were saved or dropped).
    pub allow_quit: bool,
}

impl Scripts {
    /// No dialog open, nothing queued.
    pub fn new(last_dir: Option<PathBuf>) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            last_dir,
            close_queue: VecDeque::new(),
            dialog_open: false,
            quit_prompt: false,
            quitting: false,
            allow_quit: false,
        }
    }

    /// Stops closing tabs (and quitting): a save was cancelled or failed.
    pub fn abort(&mut self) {
        self.close_queue.clear();
        self.quitting = false;
    }

    /// The next dialog result, if one arrived.
    pub fn poll(&mut self) -> Option<Picked> {
        let picked = self.rx.try_recv().ok()?;
        self.dialog_open = false;
        Some(picked)
    }

    /// Folder the dialogs start in: the last one used, else `Documents\Zephix`.
    pub fn start_dir(&self) -> Option<PathBuf> {
        start_dir(self.last_dir.as_deref(), documents_dir().as_deref())
    }

    /// Asks for a `.sql` file to open, on a helper thread.
    pub fn pick_open(&mut self, ctx: &Context) -> Result<(), String> {
        let dir = self.start_dir();
        self.spawn(ctx, move || {
            let mut dialog = rfd::FileDialog::new()
                .add_filter("SQL script", &["sql"])
                .add_filter("All files", &["*"]);
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                dialog = dialog.set_directory(dir);
            }
            match dialog.pick_file() {
                None => Picked::Cancelled { then_close: false },
                Some(path) => match read_script(&path) {
                    Ok(text) => Picked::Open { path, text },
                    Err(e) => Picked::Failed(format!("Could not open {}: {e}", path.display())),
                },
            }
        })
    }

    /// Asks where to save tab `tab_id`, on a helper thread. The folder is created first when
    /// it is the default one.
    pub fn pick_save(
        &mut self,
        ctx: &Context,
        tab_id: u64,
        current: Option<&Path>,
        title: &str,
        then_close: bool,
    ) -> Result<(), String> {
        let dir = current
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.start_dir());
        let name = current
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_name_for(title));
        self.spawn(ctx, move || {
            let mut dialog = rfd::FileDialog::new()
                .set_file_name(&name)
                .add_filter("SQL script", &["sql"]);
            if let Some(dir) = dir {
                let _ = std::fs::create_dir_all(&dir);
                dialog = dialog.set_directory(dir);
            }
            match dialog.save_file() {
                Some(path) => Picked::SaveTo {
                    tab_id,
                    path: with_sql_extension(path),
                    then_close,
                },
                None => Picked::Cancelled { then_close },
            }
        })
    }

    fn spawn(
        &mut self,
        ctx: &Context,
        job: impl FnOnce() -> Picked + Send + 'static,
    ) -> Result<(), String> {
        if self.dialog_open {
            return Err("A file dialog is already open".into());
        }
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("rdbs-script-dialog".into())
            .spawn(move || {
                let _ = tx.send(job());
                ctx.request_repaint();
            })
            .map_err(|e| format!("Could not open the file dialog: {e}"))?;
        self.dialog_open = true;
        Ok(())
    }
}

/// `Documents\Zephix` (the platform's documents folder).
fn documents_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|d| d.document_dir().map(|p| p.join("Zephix")))
}

/// The last folder when it still exists, else the documents folder.
fn start_dir(last: Option<&Path>, documents: Option<&Path>) -> Option<PathBuf> {
    last.filter(|d| d.is_dir())
        .or(documents)
        .map(Path::to_path_buf)
}

/// A file name for a tab title: characters Windows does not allow become `_`, and `.sql` is
/// added.
pub fn file_name_for(title: &str) -> String {
    let base: String = title
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let base = base.trim_end_matches(['.', ' ']);
    let base = if base.is_empty() { "query" } else { base };
    if base.to_lowercase().ends_with(".sql") {
        base.to_owned()
    } else {
        format!("{base}.sql")
    }
}

/// Adds `.sql` to a file name without an extension.
fn with_sql_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension("sql")
    }
}

/// Reads a script as text: UTF-8 (a byte-order mark is dropped), invalid bytes replaced.
pub fn read_script(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// Writes a script as UTF-8, creating its folder when needed.
pub fn write_script(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text.as_bytes())
}

/// The unsaved-changes prompt for one tab. `file` is the script's file, when it has one.
pub fn close_prompt(ctx: &Context, title: &str, file: Option<&Path>) -> Option<CloseChoice> {
    let mut choice = None;
    let response = egui::Modal::new(Id::new("unsaved-script")).show(ctx, |ui| {
        ui.set_width(440.0);
        let (heading, body) = match file {
            Some(path) => (
                format!("Save changes to “{title}”?"),
                format!(
                    "The script was changed since it was opened or saved.\n{}",
                    path.display()
                ),
            ),
            None => (
                format!("Save “{title}” to a file?"),
                "This query is not saved in a file yet. Save it before closing the tab?".to_owned(),
            ),
        };
        ui.label(
            RichText::new(format!("{}  {heading}", icon::FLOPPY_DISK))
                .size(16.0)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(RichText::new(body).color(color::text_dim()));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let save = egui::Button::new(
                RichText::new(format!("{}  Save", icon::FLOPPY_DISK)).color(egui::Color32::WHITE),
            )
            .fill(color::accent());
            if ui.add(save).clicked() {
                choice = Some(CloseChoice::Save);
            }
            if ui.button("Don't Save").clicked() {
                choice = Some(CloseChoice::Discard);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(CloseChoice::Cancel);
            }
        });
    });
    if choice.is_none() && response.should_close() {
        choice = Some(CloseChoice::Cancel);
    }
    choice
}

/// The prompt shown when Zephix closes while scripts have unsaved changes.
pub fn quit_prompt(ctx: &Context, unsaved: &[String]) -> Option<QuitChoice> {
    let mut choice = None;
    let response = egui::Modal::new(Id::new("unsaved-on-quit")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.label(
            RichText::new(format!(
                "{}  Save changes before closing Zephix?",
                icon::FLOPPY_DISK
            ))
            .size(16.0)
            .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "{} quer{} not saved:",
                unsaved.len(),
                if unsaved.len() == 1 {
                    "y is"
                } else {
                    "ies are"
                }
            ))
            .color(color::text_dim()),
        );
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .show(ui, |ui| {
                for title in unsaved {
                    ui.label(format!("{}  {title}", icon::FILE_SQL));
                }
            });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Queries you do not save are not reopened next time.")
                .small()
                .color(color::text_faint()),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let save = egui::Button::new(
                RichText::new(format!("{}  Save All", icon::FLOPPY_DISK))
                    .color(egui::Color32::WHITE),
            )
            .fill(color::accent());
            if ui.add(save).clicked() {
                choice = Some(QuitChoice::SaveAll);
            }
            if ui.button("Don't Save").clicked() {
                choice = Some(QuitChoice::Discard);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(QuitChoice::Cancel);
            }
        });
    });
    if choice.is_none() && response.should_close() {
        choice = Some(QuitChoice::Cancel);
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_come_from_titles() {
        assert_eq!(file_name_for("Query 1"), "Query 1.sql");
        assert_eq!(file_name_for("report.sql"), "report.sql");
        assert_eq!(file_name_for("a/b:c?"), "a_b_c_.sql");
        assert_eq!(file_name_for("  "), "query.sql");
        assert_eq!(file_name_for("v1.2 report"), "v1.2 report.sql");
        assert_eq!(
            with_sql_extension(PathBuf::from("x")),
            PathBuf::from("x.sql")
        );
        assert_eq!(
            with_sql_extension(PathBuf::from("x.txt")),
            PathBuf::from("x.txt")
        );
    }

    #[test]
    fn dialogs_start_in_the_last_folder_that_still_exists() {
        let tmp = std::env::temp_dir();
        let docs = Path::new("C:/Users/me/Documents/Zephix");
        assert_eq!(start_dir(Some(&tmp), Some(docs)), Some(tmp.clone()));
        let gone = tmp.join("rdbs-no-such-folder-7f3a");
        assert_eq!(start_dir(Some(&gone), Some(docs)), Some(docs.to_path_buf()));
        assert_eq!(start_dir(None, None), None);
    }

    #[test]
    fn scripts_round_trip_through_files() {
        let dir = std::env::temp_dir().join(format!("rdbs-scripts-{}", std::process::id()));
        let path = dir.join("nested").join("q.sql");
        write_script(&path, "SELECT 'é';\n").expect("write");
        assert_eq!(read_script(&path).expect("read"), "SELECT 'é';\n");
        // A byte-order mark from another editor is dropped.
        std::fs::write(&path, b"\xEF\xBB\xBFSELECT 1").expect("write bom");
        assert_eq!(read_script(&path).expect("read"), "SELECT 1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
