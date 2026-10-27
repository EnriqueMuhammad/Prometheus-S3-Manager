//! Bottom panel: Tasks, Properties and Event log tabs.

use egui::{Align, Button, Layout, ProgressBar, RichText, ScrollArea, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use s3m_core::{TransferDirection, TransferState};

use crate::app::{Action, App};
use crate::model::{BottomTab, PreviewState};
use crate::theme;

impl App {
    pub(crate) fn bottom_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let active = self.queue.active_count();
        let total = self.queue.with_jobs(<[_]>::len);
        // Keep the user-chosen height instead of shrinking to the content.
        ui.set_min_height(ui.available_height());
        ui.horizontal(|ui| {
            let tasks = format!("{} Tasks ({active} active)", icon::LIST);
            for (tab, label) in [
                (BottomTab::Tasks, tasks),
                (BottomTab::Properties, format!("{} Properties", icon::INFO)),
                (BottomTab::Headers, format!("{} Headers", icon::TAG)),
                (BottomTab::Preview, format!("{} Preview", icon::EYE)),
                (
                    BottomTab::Versions,
                    format!("{} Versions", icon::CLOCK_COUNTER_CLOCKWISE),
                ),
                (
                    BottomTab::Permissions,
                    format!("{} Permissions", icon::LOCK_KEY),
                ),
                (BottomTab::EventLog, format!("{} Event log", icon::CLOCK)),
            ] {
                if ui.selectable_label(self.bottom_tab == tab, label).clicked() {
                    self.bottom_tab = tab;
                }
            }
            if self.bottom_tab == BottomTab::Tasks {
                let paused = self.queue.with_jobs(|jobs| {
                    jobs.iter()
                        .filter(|j| j.state == TransferState::Paused)
                        .count()
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            total > active + paused,
                            Button::new(format!("{} Clear finished", icon::BROOM)),
                        )
                        .clicked()
                    {
                        actions.push(Action::ClearFinished);
                    }
                    if ui
                        .add_enabled(
                            paused > 0,
                            Button::new(format!("{} Resume all", icon::PLAY)),
                        )
                        .clicked()
                    {
                        actions.push(Action::ResumeAll);
                    }
                    if ui
                        .add_enabled(
                            active > 0,
                            Button::new(format!("{} Pause all", icon::PAUSE)),
                        )
                        .clicked()
                    {
                        actions.push(Action::PauseAll);
                    }
                    let limit = self.settings.bandwidth_limit_kbps;
                    if limit > 0 {
                        ui.label(
                            RichText::new(format!("{} limit {} KB/s", icon::GAUGE, limit))
                                .color(theme::palette(ui).warning),
                        )
                        .on_hover_text("Bandwidth limit (Tools > Options)");
                    }
                });
            }
        });
        ui.separator();
        match self.bottom_tab {
            BottomTab::Tasks => self.tasks_tab(ui, actions),
            BottomTab::Properties => self.properties_tab(ui, actions),
            BottomTab::Headers => self.headers_tab(ui, actions),
            BottomTab::Preview => {
                self.ensure_preview();
                self.preview_tab(ui);
            }
            BottomTab::Versions => {
                self.ensure_versions();
                self.versions_tab(ui, actions);
            }
            BottomTab::Permissions => {
                self.ensure_acl();
                self.permissions_tab(ui, actions);
            }
            BottomTab::EventLog => self.log_tab(ui),
        }
    }

    fn tasks_tab(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        self.queue.with_jobs(|jobs| {
            if jobs.is_empty() {
                ui.label(
                    RichText::new("No transfers yet. Uploads and downloads appear here.")
                        .color(p.dim),
                );
                return;
            }
            TableBuilder::new(ui)
                .id_salt("tasks-table")
                .striped(true)
                .resizable(true)
                .auto_shrink(false)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(24.0))
                .column(Column::initial(180.0).at_least(80.0).clip(true))
                .column(Column::initial(220.0).at_least(80.0).clip(true))
                .column(Column::initial(200.0).at_least(80.0).clip(true))
                .column(Column::initial(130.0).at_least(80.0))
                .column(Column::remainder().at_least(120.0).clip(true))
                .column(Column::exact(150.0))
                .header(22.0, |mut row| {
                    for h in [
                        "",
                        "File",
                        "Bucket / key",
                        "Source / local path",
                        "Progress",
                        "Status",
                        "",
                    ] {
                        row.col(|ui| {
                            ui.label(RichText::new(h).strong());
                        });
                    }
                })
                .body(|body| {
                    // Newest first.
                    body.rows(22.0, jobs.len(), |mut row| {
                        let job = &jobs[jobs.len() - 1 - row.index()];
                        row.col(|ui| {
                            let (glyph, color) = match job.direction {
                                TransferDirection::Upload => (icon::ARROW_FAT_UP, p.accent),
                                TransferDirection::Download => (icon::ARROW_LINE_DOWN, p.success),
                                TransferDirection::Copy => (icon::COPY, p.dim),
                                TransferDirection::Move => (icon::ARROW_RIGHT, p.warning),
                            };
                            ui.label(RichText::new(glyph).color(color))
                                .on_hover_text(job.direction.label());
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(job.name()).truncate());
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(format!("{}/{}", job.bucket, job.key)).truncate(),
                            );
                        });
                        row.col(|ui| {
                            let from = match &job.source {
                                Some(s) => format!("from {}/{}", s.bucket, s.key),
                                None => job.local_path.display().to_string(),
                            };
                            ui.add(egui::Label::new(from).truncate());
                        });
                        row.col(|ui| {
                            let fill = match job.state {
                                TransferState::Failed => p.danger,
                                TransferState::Canceled => p.faint,
                                TransferState::Paused => p.warning,
                                TransferState::Completed => p.success,
                                _ => p.accent,
                            };
                            ui.add(
                                ProgressBar::new(job.percent() / 100.0)
                                    .desired_width(120.0)
                                    .desired_height(12.0)
                                    .fill(fill),
                            );
                        });
                        row.col(|ui| {
                            let color = match job.state {
                                TransferState::Failed => p.danger,
                                TransferState::Retrying => p.warning,
                                _ => p.text,
                            };
                            let summary = job.summary();
                            ui.add(
                                egui::Label::new(RichText::new(&summary).color(color)).truncate(),
                            )
                            .on_hover_text(summary);
                        });
                        row.col(|ui| {
                            if job.is_active() {
                                if ui.small_button("Pause").clicked() {
                                    actions.push(Action::PauseJob(job.id));
                                }
                                if ui.small_button("Cancel").clicked() {
                                    actions.push(Action::CancelJob(job.id));
                                }
                            } else if job.state == TransferState::Paused {
                                if ui.small_button("Resume").clicked() {
                                    actions.push(Action::RetryJob(job.id));
                                }
                                if ui.small_button("Cancel").clicked() {
                                    actions.push(Action::CancelJob(job.id));
                                }
                            } else if matches!(
                                job.state,
                                TransferState::Failed | TransferState::Canceled
                            ) && ui.small_button("Retry").clicked()
                            {
                                actions.push(Action::RetryJob(job.id));
                            }
                        });
                    });
                });
        });
    }

    fn properties_tab(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        if self.properties.is_empty() {
            ui.label(RichText::new("Select a file to see its properties.").color(p.dim));
            return;
        }
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("properties-grid")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for (name, value) in &self.properties {
                        ui.label(RichText::new(name).color(p.dim));
                        let r = ui.add(egui::Label::new(value).wrap());
                        r.context_menu(|ui| {
                            if ui.button("Copy value").clicked() {
                                actions.push(Action::CopyText(value.clone()));
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    }

    fn headers_tab(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        let Some(d) = &self.details else {
            ui.label(
                RichText::new("Select a file to see its HTTP headers and metadata.").color(p.dim),
            );
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&d.key).strong());
            if ui
                .button(format!("{} Edit headers…", icon::PENCIL_SIMPLE))
                .clicked()
            {
                actions.push(Action::EditHeaders);
            }
        });
        let rows: Vec<(String, String)> = [
            ("Content-Type", d.content_type.clone()),
            ("Cache-Control", d.cache_control.clone()),
            ("Content-Disposition", d.content_disposition.clone()),
            ("Content-Encoding", d.content_encoding.clone()),
            ("Content-Language", d.content_language.clone()),
            ("ETag", d.etag.clone()),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v)))
        .chain(
            d.metadata
                .iter()
                .map(|(k, v)| (format!("x-amz-meta-{k}"), v.clone())),
        )
        .collect();
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("headers-grid")
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    for (name, value) in &rows {
                        ui.label(RichText::new(name).color(p.dim));
                        ui.add(egui::Label::new(value).wrap());
                        ui.end_row();
                    }
                });
        });
    }

    fn preview_tab(&self, ui: &mut Ui) {
        let p = theme::palette(ui);
        match &self.preview {
            None => {
                ui.label(RichText::new("Select a file to preview it.").color(p.dim));
            }
            Some((_, PreviewState::Loading)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading preview…");
                });
            }
            Some((_, PreviewState::Unavailable(reason))) => {
                ui.label(RichText::new(reason).color(p.dim));
            }
            Some((_, PreviewState::Text { text, truncated })) => {
                if *truncated {
                    ui.label(
                        RichText::new("Showing the first 256 KB.")
                            .size(11.5)
                            .color(p.dim),
                    );
                }
                // Only the visible lines are laid out and painted: a 256 KB file as one label
                // would be tessellated in full every frame.
                let lines: Vec<&str> = text.lines().collect();
                let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                ScrollArea::both().auto_shrink(false).show_rows(
                    ui,
                    row_height,
                    lines.len(),
                    |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for line in &lines[range] {
                            ui.add(
                                egui::Label::new(RichText::new(*line).monospace())
                                    .extend()
                                    .selectable(true),
                            );
                        }
                    },
                );
            }
            Some((_, PreviewState::Image { uri, bytes })) => {
                ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    ui.add(
                        egui::Image::from_bytes(
                            uri.clone(),
                            egui::load::Bytes::Shared(bytes.clone()),
                        )
                        .max_size(ui.available_size())
                        .maintain_aspect_ratio(true),
                    );
                });
            }
        }
    }

    fn log_tab(&self, ui: &mut Ui) {
        let p = theme::palette(ui);
        if self.log.is_empty() {
            ui.label(RichText::new("Nothing has happened yet.").color(p.dim));
            return;
        }
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for entry in &self.log {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(entry.time.format("%H:%M:%S").to_string())
                            .color(p.faint)
                            .monospace(),
                    );
                    let color = if entry.is_error { p.danger } else { p.text };
                    ui.add(egui::Label::new(RichText::new(&entry.message).color(color)).wrap());
                });
            }
        });
    }
}
