//! Tools > Sync folder: compare a local folder with the current bucket folder, show the plan,
//! then run it.

use std::path::PathBuf;

use egui::{Button, Id, Modal, RichText, ScrollArea, TextEdit};
use egui_phosphor::regular as icon;
use s3m_core::format;
use s3m_core::sync::{self, SyncDirection, SyncPlan};

use crate::app::App;
use crate::model::{Msg, Reload};
use crate::theme;
use crate::widgets::muted;

/// Sync dialog state.
pub struct SyncDialog {
    /// Bucket being synchronised.
    pub bucket: String,
    /// Bucket folder being synchronised.
    pub prefix: String,
    /// Local folder.
    pub local: String,
    /// Direction.
    pub direction: SyncDirection,
    /// Delete remote files missing locally (upload only).
    pub delete_extra: bool,
    /// Comparison result.
    pub plan: Option<SyncPlan>,
    /// Comparison in flight.
    pub comparing: bool,
    /// Error message.
    pub error: String,
    /// Distinguishes results of earlier comparisons.
    pub nonce: u64,
}

impl App {
    pub(crate) fn open_sync(&mut self) {
        let Some(bucket) = self.current_bucket.clone() else {
            self.show_info(
                "Sync folder",
                "Open the bucket folder to synchronise first.",
            );
            return;
        };
        self.sync = Some(SyncDialog {
            bucket,
            prefix: self.prefix.clone(),
            local: String::new(),
            direction: SyncDirection::Upload,
            delete_extra: false,
            plan: None,
            comparing: false,
            error: String::new(),
            nonce: 0,
        });
    }

    fn compare(&mut self) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        let Some(d) = self.sync.as_mut() else { return };
        let root = PathBuf::from(d.local.trim());
        if d.local.trim().is_empty() || !root.is_dir() {
            d.error = "Choose an existing local folder.".into();
            return;
        }
        d.error.clear();
        d.plan = None;
        d.comparing = true;
        d.nonce += 1;
        let (nonce, bucket, prefix, direction, delete_extra) = (
            d.nonce,
            d.bucket.clone(),
            d.prefix.clone(),
            d.direction,
            d.delete_extra,
        );
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = async {
                let scan_root = root.clone();
                let local = tokio::task::spawn_blocking(move || sync::scan_local(&scan_root))
                    .await
                    .map_err(|e| e.to_string())?;
                let objects = storage
                    .list_all_with_times(&account, &bucket, &prefix)
                    .await
                    .map_err(|e| e.display())?;
                let remote = sync::remote_files(&prefix, &objects);
                Ok::<_, String>(sync::plan(
                    account.id,
                    &bucket,
                    &prefix,
                    &root,
                    direction,
                    delete_extra && direction == SyncDirection::Upload,
                    &local,
                    &remote,
                ))
            }
            .await;
            Msg::SyncPlan { nonce, result }
        });
    }

    pub(crate) fn handle_sync_plan(&mut self, nonce: u64, result: Result<SyncPlan, String>) {
        let Some(d) = self.sync.as_mut().filter(|d| d.nonce == nonce) else {
            return;
        };
        d.comparing = false;
        match result {
            Ok(p) => d.plan = Some(p),
            Err(e) => d.error = e,
        }
    }

    fn start_sync(&mut self) {
        let Some(d) = self.sync.take() else { return };
        let Some(plan) = d.plan else { return };
        let Some(account) = self.current().cloned() else {
            return;
        };
        self.log(
            format!(
                "Sync {} {}/{}: {} transfer(s), {} delete(s), {} unchanged",
                d.local,
                d.bucket,
                d.prefix,
                plan.transfers.len(),
                plan.deletes.len(),
                plan.unchanged
            ),
            false,
        );
        self.enqueue(plan.transfers);
        if !plan.deletes.is_empty() {
            let storage = self.storage.clone();
            let (bucket, keys) = (d.bucket, plan.deletes);
            self.spawn(async move {
                let result = storage
                    .delete_objects(&account, &bucket, &keys)
                    .await
                    .map(|n| format!("Sync deleted {n} remote file(s) from {bucket}"));
                Msg::Done {
                    title: "Couldn't delete remote files during sync".into(),
                    result,
                    reload: Reload::Objects,
                }
            });
        }
    }

    pub(crate) fn sync_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.sync.take() else {
            return;
        };
        let mut close = false;
        let mut compare = false;
        let mut start = false;
        Modal::new(Id::new("sync-window")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(680.0);
            ui.heading(format!("{} Sync folder", icon::ARROWS_LEFT_RIGHT));
            egui::Grid::new("sync-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Local folder:");
                ui.horizontal(|ui| {
                    if ui.add(TextEdit::singleline(&mut d.local).desired_width(430.0)).changed() {
                        d.plan = None;
                    }
                    if ui.button("Browse…").clicked()
                        && let Some(folder) = rfd::FileDialog::new().set_title("Local folder to sync").pick_folder()
                    {
                        d.local = folder.display().to_string();
                        d.plan = None;
                    }
                });
                ui.end_row();
                ui.label("Bucket folder:");
                ui.label(RichText::new(format!("{}/{}", d.bucket, d.prefix)).strong());
                ui.end_row();
                ui.label("Direction:");
                ui.vertical(|ui| {
                    let up = format!("Local {} bucket: upload new and changed files", icon::ARROW_RIGHT);
                    let down = format!("Bucket {} local: download new files", icon::ARROW_RIGHT);
                    if ui.radio_value(&mut d.direction, SyncDirection::Upload, up).changed()
                        | ui.radio_value(&mut d.direction, SyncDirection::Download, down).changed()
                    {
                        d.plan = None;
                    }
                });
                ui.end_row();
                ui.label("");
                ui.add_enabled_ui(d.direction == SyncDirection::Upload, |ui| {
                    if ui
                        .checkbox(&mut d.delete_extra, "Delete files in the bucket folder that do not exist locally")
                        .changed()
                    {
                        d.plan = None;
                    }
                });
                ui.end_row();
            });
            muted(ui, "Files are compared by size and modification time. Nothing changes until you press Start.");
            if !d.error.is_empty() {
                ui.label(RichText::new(d.error.as_str()).color(p.danger));
            }
            if d.comparing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Comparing…");
                });
            }
            if let Some(plan) = &d.plan {
                ui.separator();
                let verb = if d.direction == SyncDirection::Upload { "upload" } else { "download" };
                ui.label(
                    RichText::new(format!(
                        "{} file(s) to {verb} ({}), {} to delete, {} unchanged, {} skipped",
                        plan.transfers.len(),
                        format::size(plan.bytes),
                        plan.deletes.len(),
                        plan.unchanged,
                        plan.skipped.len()
                    ))
                    .strong(),
                );
                ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for t in plan.transfers.iter().take(300) {
                        let what = if d.direction == SyncDirection::Upload { icon::UPLOAD_SIMPLE } else { icon::DOWNLOAD_SIMPLE };
                        ui.label(format!("{what}  {}", t.key));
                    }
                    for k in plan.deletes.iter().take(300) {
                        ui.label(RichText::new(format!("{}  {k}", icon::TRASH)).color(p.danger));
                    }
                    for (rel, why) in plan.skipped.iter().take(300) {
                        ui.label(RichText::new(format!("{}  {rel}: {why}", icon::PROHIBIT)).color(p.dim));
                    }
                });
                if !plan.deletes.is_empty() {
                    ui.label(
                        RichText::new(format!("{} {} file(s) in the bucket will be permanently deleted.", icon::WARNING, plan.deletes.len()))
                            .color(p.danger),
                    );
                }
            }
            crate::dialogs::button_row(ui, |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
                let can_start = d.plan.as_ref().is_some_and(|p| !p.transfers.is_empty() || !p.deletes.is_empty());
                if ui.add_enabled(can_start, Button::new(format!("{} Start", icon::PLAY))).clicked() {
                    start = true;
                }
                if ui.add_enabled(!d.comparing, Button::new(format!("{} Compare", icon::MAGNIFYING_GLASS))).clicked() {
                    compare = true;
                }
            });
        });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return;
        }
        self.sync = Some(d);
        if compare {
            self.compare();
        }
        if start {
            self.start_sync();
        }
    }
}
