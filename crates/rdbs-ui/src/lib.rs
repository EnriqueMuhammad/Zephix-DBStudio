//! egui user interface for Zephix - Database Management Studio.
//!
//! The UI never performs database I/O itself: it sends typed commands to the background
//! worker in `rdbs-services` and renders the events it receives (see ADR 0003).

mod app;
mod autocomplete;
mod dialog;
mod editor;
mod explorer;
mod grid;
mod history;
mod model;
mod plan;
mod scripts;
pub mod theme;
mod tools;
mod welcome;
mod widgets;
mod window;
mod workspace;

pub use app::{AppConfig, InitError, StudioApp};
pub use window::{MIN_SIZE, PREFERRED, strip_window_position};
