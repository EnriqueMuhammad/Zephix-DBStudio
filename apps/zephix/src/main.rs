//! Zephix - Database Management Studio, desktop entry point.
//! Author and creator: Enrique Muhammad Ilham.
//!
//! `zephix` connects to real servers. `zephix --demo` (or `RDBS_DEMO=1`) runs
//! against the built-in mock engine with sample connections and keeps nothing on disk.

// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use eframe::egui;
use rdbs_core::MemoryCredentials;
use rdbs_ui::AppConfig;
use tracing_subscriber::EnvFilter;

fn demo_requested() -> bool {
    std::env::args().any(|a| a == "--demo")
        || std::env::var("RDBS_DEMO").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn app_config() -> AppConfig {
    if demo_requested() {
        AppConfig {
            factory: Arc::new(rdbs_adapter_mock::MockAdapterFactory::new()),
            credentials: Arc::new(MemoryCredentials::default()),
            demo_profiles: Some(rdbs_adapter_mock::sample_profiles()),
        }
    } else {
        let credentials: Arc<dyn rdbs_core::CredentialStore> =
            Arc::new(rdbs_storage::KeyringCredentials);
        let known_hosts = rdbs_storage::default_known_hosts_path()
            .unwrap_or_else(|_| std::env::temp_dir().join("rustdb-studio-known_hosts"));
        AppConfig {
            factory: Arc::new(rdbs_drivers::LiveFactory::new(known_hosts)),
            credentials,
            demo_profiles: None,
        }
    }
}

/// Removes the window position that Zephix 0.2.0 and earlier saved; eframe restores it off
/// screen when the monitors use different scale factors (see `rdbs_ui::strip_window_position`).
fn forget_window_position() {
    let Some(path) = eframe::storage_dir("zephix").map(|dir| dir.join("app.ron")) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    if let Some(fixed) = rdbs_ui::strip_window_position(&text) {
        match std::fs::write(&path, fixed) {
            Ok(()) => tracing::info!("removed the saved window position"),
            Err(e) => tracing::warn!("could not update {}: {e}", path.display()),
        }
    }
}

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,egui_glow=warn")),
        )
        .init();

    forget_window_position();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(rdbs_core::brand::APP_TITLE)
        .with_app_id("zephix");
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/zephix-256.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => tracing::warn!("could not load the window icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size(rdbs_ui::PREFERRED)
            .with_min_inner_size(rdbs_ui::MIN_SIZE),
        // eframe would save the window position and restore it wrongly when the monitors use
        // different scale factors (off screen, twice the size). The app saves the size and
        // maximized state without a position instead; the window opens centered.
        persist_window: false,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        rdbs_core::brand::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(rdbs_ui::StudioApp::new(cc, app_config())?))),
    )
}
