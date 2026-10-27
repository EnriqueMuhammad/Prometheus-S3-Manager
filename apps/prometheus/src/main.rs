//! Prometheus S3Manager, desktop entry point.
//! Author and creator: Enrique Muhammad Ilham.
//!
//! `s3manager [--data-dir <path>]`. The data folder can also be set with `S3MANAGER_DATA_DIR`.

// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use s3m_core::vault::ProtectedSecrets;
use s3m_core::{AccountService, TransferStore};
use s3m_storage::{
    AppPaths, JsonAccountRepository, KeyringSecrets, LogWriter, SettingsStore, SqliteTransferStore,
};
use s3m_ui::{App, AppConfig};
use tracing_subscriber::EnvFilter;

fn data_dir_arg() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            return args.next().map(PathBuf::from);
        }
        if let Some(v) = arg.strip_prefix("--data-dir=") {
            return Some(PathBuf::from(v));
        }
    }
    None
}

fn config(paths: &AppPaths) -> AppConfig {
    let settings_store = SettingsStore::new(paths.settings());
    let master = settings_store.load().master_password.is_some();
    let vault = Arc::new(ProtectedSecrets::new(Arc::new(KeyringSecrets), master));
    let accounts = Arc::new(AccountService::new(
        Box::new(JsonAccountRepository::new(paths.accounts())),
        vault.clone(),
    ));
    let mut startup_warning = None;
    let transfer_store: Arc<dyn TransferStore> = match SqliteTransferStore::open(&paths.transfers())
    {
        Ok(store) => Arc::new(store),
        Err(e) => {
            startup_warning = Some(format!(
                "The transfer history ({}) could not be opened: {e}. Transfers still work but are not remembered.",
                paths.transfers().display()
            ));
            Arc::new(s3m_core::transfer::MemoryTransferStore::default())
        }
    };
    if let Err(e) = accounts.list() {
        startup_warning = Some(format!(
            "{e}\n\nFix or remove the file, then restart. Nothing has been overwritten."
        ));
    }
    AppConfig {
        accounts,
        transfer_store,
        settings_store,
        vault,
        data_dir: paths.root.clone(),
        startup_warning,
        // Set by release builds (CI passes the GitHub repository); empty or unset = no updates.
        update_repo: option_env!("S3M_UPDATE_REPO")
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_owned),
    }
}

/// eframe's app id: names its storage folder (window state).
const APP_ID: &str = "prometheus-s3manager";

/// Removes the window position that earlier builds saved; eframe restores it off screen when the
/// monitors use different scale factors (see `s3m_ui::strip_window_position`).
fn forget_window_position() {
    let Some(path) = eframe::storage_dir(APP_ID).map(|dir| dir.join("app.ron")) else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    if let Some(fixed) = s3m_ui::strip_window_position(&text) {
        match std::fs::write(&path, fixed) {
            Ok(()) => tracing::info!("removed the saved window position"),
            Err(e) => tracing::warn!("could not update {}: {e}", path.display()),
        }
    }
}

fn main() -> eframe::Result {
    let paths = AppPaths::resolve(data_dir_arg().as_deref());
    let writer = LogWriter::new(paths.logs());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,egui_glow=warn,aws_smithy_runtime=warn,aws_config=warn,hyper=warn")
        }))
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();
    tracing::info!(
        "starting {} {} (data: {})",
        s3m_core::APP_NAME,
        env!("CARGO_PKG_VERSION"),
        paths.root.display()
    );

    forget_window_position();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(s3m_core::APP_NAME)
        .with_app_id(APP_ID)
        .with_drag_and_drop(true);
    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => tracing::warn!("could not load the window icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size(s3m_ui::PREFERRED)
            .with_min_inner_size(s3m_ui::MIN_SIZE),
        // eframe would save the window position and restore it wrongly when the monitors use
        // different scale factors (off screen, twice the size). The app saves the size and
        // maximized state without a position instead; the window opens centered.
        persist_window: false,
        centered: true,
        ..Default::default()
    };
    let app_config = config(&paths);
    eframe::run_native(
        "Prometheus S3Manager",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, app_config)?))),
    )
}
