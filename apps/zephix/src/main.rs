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

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,egui_glow=warn")),
        )
        .init();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(rdbs_core::brand::APP_TITLE)
        .with_app_id("zephix");
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/zephix-256.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => tracing::warn!("could not load the window icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([900.0, 560.0]),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        rdbs_core::brand::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(rdbs_ui::StudioApp::new(cc, app_config())?))),
    )
}
