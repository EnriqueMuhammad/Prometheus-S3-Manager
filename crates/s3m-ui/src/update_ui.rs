//! Help > Check for updates, the automatic check at start and the update dialog.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Id, Modal, ProgressBar, RichText, ScrollArea};
use egui_phosphor::regular as icon;
use s3m_core::update::{self, Release};
use s3m_core::{APP_NAME, format};
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::dialogs::{button_row, primary_button};
use crate::model::Msg;
use crate::theme;
use crate::widgets::muted;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Where the update dialog is.
pub(crate) enum Stage {
    /// Release shown, nothing downloaded.
    Available,
    /// Installer download running.
    Downloading {
        done: u64,
        total: u64,
        cancel: CancellationToken,
    },
    /// Installer downloaded and verified.
    Ready { msi: PathBuf, note: String },
    /// Download, verification or start of the installer failed.
    Failed(String),
}

/// The update dialog.
pub(crate) struct UpdateDialog {
    release: Release,
    stage: Stage,
    /// This copy runs from the MSI install folder and can replace itself.
    installed: bool,
}

impl App {
    /// Starts an update check; `manual` reports "up to date" and errors in a dialog.
    pub(crate) fn check_for_updates(&mut self, manual: bool) {
        let Some(repo) = self.update_repo.clone() else {
            if manual {
                self.show_info(
                    "Check for updates",
                    "This build has no update source. Builds published on GitHub Releases check for updates; development builds do not.",
                );
            }
            return;
        };
        if self.update_checking {
            return;
        }
        let updater = match self.updater(&repo) {
            Ok(u) => u,
            Err(e) => {
                if manual {
                    self.show_error("Check for updates", e);
                }
                return;
            }
        };
        self.update_checking = true;
        self.spawn(async move {
            Msg::UpdateChecked {
                manual,
                result: updater.latest().await,
            }
        });
    }

    fn updater(&self, repo: &str) -> Result<s3m_update::Updater, String> {
        let proxy = self.proxy_settings(&self.settings);
        match std::env::var("S3MANAGER_UPDATE_API") {
            Ok(api) if !api.trim().is_empty() => s3m_update::Updater::with_api(repo, &proxy, &api),
            _ => s3m_update::Updater::new(repo, &proxy),
        }
    }

    pub(crate) fn handle_update_checked(&mut self, manual: bool, result: Result<Release, String>) {
        self.update_checking = false;
        let release = match result {
            Ok(r) => r,
            Err(e) => {
                self.log(format!("Update check failed: {e}"), true);
                if manual {
                    self.show_error("Check for updates", e);
                }
                return;
            }
        };
        let mut settings = self.settings.clone();
        settings.last_update_check = Some(chrono::Utc::now());
        if let Err(e) = self.persist_settings(settings) {
            self.log(e, true);
        }
        if !update::is_newer(&release, CURRENT) {
            self.log(format!("{APP_NAME} {CURRENT} is up to date."), false);
            if manual {
                self.show_info(
                    "Check for updates",
                    format!("You have the latest version ({CURRENT})."),
                );
            }
            return;
        }
        let version = release.version.to_string();
        if !manual && self.settings.skipped_version == version {
            self.log(format!("Version {version} is available (skipped)."), false);
            return;
        }
        self.log(format!("Version {version} is available."), false);
        let installed = std::env::current_exe().is_ok_and(|exe| s3m_update::is_installed(&exe));
        self.update = Some(UpdateDialog {
            release,
            stage: Stage::Available,
            installed,
        });
    }

    fn start_update_download(&mut self) {
        let Some(repo) = self.update_repo.clone() else {
            return;
        };
        let Some(dialog) = self.update.as_mut() else {
            return;
        };
        let release = dialog.release.clone();
        let cancel = CancellationToken::new();
        dialog.stage = Stage::Downloading {
            done: 0,
            total: release.installer.as_ref().map_or(0, |a| a.size),
            cancel: cancel.clone(),
        };
        let updater = match self.updater(&repo) {
            Ok(u) => u,
            Err(e) => {
                if let Some(d) = self.update.as_mut() {
                    d.stage = Stage::Failed(e);
                }
                return;
            }
        };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let last = std::sync::Mutex::new(Instant::now());
        let progress = move |done: u64, total: u64| {
            let mut last = last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if done == total || last.elapsed() >= Duration::from_millis(150) {
                *last = Instant::now();
                let _ = tx.send(Msg::UpdateProgress { done, total });
                ctx.request_repaint();
            }
        };
        self.spawn(async move {
            let result = async {
                let msi = updater
                    .download(&release, &s3m_update::download_dir(), &progress, &cancel)
                    .await?;
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let check = s3m_update::check_signatures(&exe, &msi).await?;
                if check.accepted {
                    Ok((msi, check.message))
                } else {
                    let _ = tokio::fs::remove_file(&msi).await;
                    Err(check.message)
                }
            }
            .await;
            Msg::UpdateDownloaded { result }
        });
    }

    pub(crate) fn handle_update_progress(&mut self, done: u64, total: u64) {
        if let Some(UpdateDialog {
            stage: Stage::Downloading {
                done: d, total: t, ..
            },
            ..
        }) = self.update.as_mut()
        {
            *d = done;
            *t = total;
        }
    }

    pub(crate) fn handle_update_downloaded(&mut self, result: Result<(PathBuf, String), String>) {
        let Some(dialog) = self.update.as_mut() else {
            return;
        };
        if !matches!(dialog.stage, Stage::Downloading { .. }) {
            return;
        }
        let version = dialog.release.version.to_string();
        match result {
            Ok((msi, note)) => {
                self.log(
                    format!("Update {version} downloaded and verified. {note}"),
                    false,
                );
                if let Some(d) = self.update.as_mut() {
                    d.stage = Stage::Ready { msi, note };
                }
            }
            Err(e) => {
                self.log(format!("Update {version}: {e}"), true);
                if let Some(d) = self.update.as_mut() {
                    d.stage = Stage::Failed(e);
                }
            }
        }
    }

    fn install_update(&mut self, msi: PathBuf) {
        let version = self
            .update
            .as_ref()
            .map(|d| d.release.version.to_string())
            .unwrap_or_default();
        let result = std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| {
                let log = self
                    .data_dir
                    .join("logs")
                    .join(format!("update-{version}.log"));
                s3m_update::start_install(&msi, &exe, &log)
            });
        match result {
            Ok(()) => {
                self.log(
                    format!("Installing update {version}; the app restarts."),
                    false,
                );
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                self.log(format!("Update {version}: {e}"), true);
                if let Some(d) = self.update.as_mut() {
                    d.stage = Stage::Failed(e);
                }
            }
        }
    }

    pub(crate) fn update_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.update.as_ref() else {
            return;
        };
        enum Choice {
            Download,
            Install(PathBuf),
            Skip,
            Cancel,
            Close,
        }
        let mut choice = None;
        let mut open_page = false;
        let active = self.queue.active_count();
        let release = &dialog.release;
        let version = release.version.to_string();
        let page = release.page.clone();
        Modal::new(Id::new("update-window")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(520.0);
            ui.heading(format!("{} Update available", icon::ARROW_CIRCLE_UP));
            ui.add_space(4.0);
            ui.label(format!(
                "{APP_NAME} {} is available. You have {CURRENT}.",
                release.version
            ));
            if let Some(published) = release.published {
                muted(
                    ui,
                    format!("Published {}", published.format("%Y-%m-%d")),
                );
            }
            ui.add_space(6.0);
            ui.label(RichText::new(&release.name).strong());
            egui::Frame::new()
                .fill(p.list)
                .stroke(egui::Stroke::new(1.0, p.border))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let notes = release.notes.trim();
                            if notes.is_empty() {
                                muted(ui, "No release notes.");
                            } else {
                                ui.add(egui::Label::new(notes).wrap());
                            }
                        });
                });
            if ui.link("Open the release page").clicked() {
                open_page = true;
            }
            ui.add_space(6.0);
            match &dialog.stage {
                Stage::Available => {
                    let can_install = dialog.installed && release.installer.is_some();
                    if !dialog.installed {
                        muted(
                            ui,
                            "This copy was not installed with the MSI installer, so it cannot update itself. Download the new version from the release page.",
                        );
                    } else if release.installer.is_none() {
                        muted(ui, "This release has no Windows installer.");
                    }
                    button_row(ui, |ui| {
                        if ui.button("Later").clicked() {
                            choice = Some(Choice::Close);
                        }
                        if ui.button("Skip this version").clicked() {
                            choice = Some(Choice::Skip);
                        }
                        if can_install {
                            if primary_button(ui, "Download and install", true) {
                                choice = Some(Choice::Download);
                            }
                        } else if primary_button(ui, "Open download page", true) {
                            open_page = true;
                            choice = Some(Choice::Close);
                        }
                    });
                }
                Stage::Downloading { done, total, .. } => {
                    #[allow(clippy::cast_precision_loss)]
                    let fraction = if *total > 0 {
                        (*done as f64 / *total as f64) as f32
                    } else {
                        0.0
                    };
                    ui.add(ProgressBar::new(fraction).text(format!(
                        "Downloading {} of {}",
                        format::size(*done),
                        format::size(*total)
                    )));
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() {
                            choice = Some(Choice::Cancel);
                        }
                    });
                }
                Stage::Ready { msi, note } => {
                    ui.label(
                        RichText::new(format!("{} Downloaded and verified. {note}", icon::CHECK_CIRCLE))
                            .color(p.success),
                    );
                    ui.add(
                        egui::Label::new(
                            "Installing closes the app, runs the installer (no administrator rights needed) and starts the new version.",
                        )
                        .wrap(),
                    );
                    if active > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{} {active} transfer(s) are running. They are paused and wait for Resume after the update.",
                                icon::WARNING
                            ))
                            .color(p.warning),
                        );
                    }
                    let msi = msi.clone();
                    button_row(ui, |ui| {
                        if ui.button("Later").clicked() {
                            choice = Some(Choice::Close);
                        }
                        if primary_button(ui, "Install and restart", true) {
                            choice = Some(Choice::Install(msi));
                        }
                    });
                }
                Stage::Failed(error) => {
                    ui.add(egui::Label::new(RichText::new(error).color(p.danger)).wrap());
                    button_row(ui, |ui| {
                        if ui.button("Close").clicked() {
                            choice = Some(Choice::Close);
                        }
                        if ui.button("Try again").clicked() {
                            choice = Some(Choice::Download);
                        }
                        if primary_button(ui, "Open download page", true) {
                            open_page = true;
                        }
                    });
                }
            }
        });
        if open_page {
            ctx.open_url(egui::OpenUrl::new_tab(page));
        }
        let downloading = matches!(dialog.stage, Stage::Downloading { .. });
        if choice.is_none() && !downloading && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            choice = Some(Choice::Close);
        }
        match choice {
            Some(Choice::Download) => self.start_update_download(),
            Some(Choice::Install(msi)) => self.install_update(msi),
            Some(Choice::Skip) => {
                let mut settings = self.settings.clone();
                settings.skipped_version.clone_from(&version);
                if let Err(e) = self.persist_settings(settings) {
                    self.log(e, true);
                }
                self.log(format!("Version {version} skipped."), false);
                self.update = None;
            }
            Some(Choice::Cancel) => {
                if let Some(UpdateDialog {
                    stage: Stage::Downloading { cancel, .. },
                    ..
                }) = self.update.as_ref()
                {
                    cancel.cancel();
                }
                if let Some(d) = self.update.as_mut() {
                    d.stage = Stage::Available;
                }
            }
            Some(Choice::Close) => self.update = None,
            None => {}
        }
    }
}
