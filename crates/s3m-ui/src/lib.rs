//! egui user interface for Prometheus S3Manager, laid out like a classic file manager: accounts
//! and buckets on the left, the file list in the middle, Tasks / Properties / Event log at the bottom.
//!
//! The UI never blocks on I/O: S3 calls run on the app's Tokio runtime and report back through
//! a channel that is drained at the start of every frame.

mod access_keys_ui;
mod admin_ui;
mod app;
mod bottom;
mod dialogs;
mod explorer;
mod model;
mod ops;
mod shell;
mod sync_ui;
pub mod theme;
mod update_ui;
mod vault_ui;
mod widgets;
mod window;

pub use app::{App, AppConfig, InitError};
pub use window::{MIN_SIZE, PREFERRED, strip_window_position};
