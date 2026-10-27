//! Left bucket panel and the file list with its path bar.

use egui::{Align, Button, Frame, Layout, Margin, RichText, ScrollArea, Sense, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use s3m_core::{browse, format};

use crate::app::{Action, App};
use crate::model::SortColumn;
use crate::theme;
use crate::widgets::{entry_icon, list_row, muted, section_title};

impl App {
    pub(crate) fn buckets_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        let has_account = self.current_account.is_some();
        ui.horizontal(|ui| {
            section_title(ui, "Buckets");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(has_account, Button::new(icon::ARROWS_CLOCKWISE).small())
                    .on_hover_text("Refresh (F5)")
                    .clicked()
                {
                    actions.push(Action::Refresh);
                }
                if ui
                    .add_enabled(has_account, Button::new(icon::PLUS).small())
                    .on_hover_text("Add bucket by name")
                    .clicked()
                {
                    actions.push(Action::AddBucketPrompt);
                }
                if self.buckets_loading {
                    ui.spinner();
                }
            });
        });
        ui.separator();
        if self.restricted {
            Frame::new()
                .fill(p.banner)
                .inner_margin(Margin::same(6))
                .corner_radius(4)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Limited key. This key can't list buckets, so only buckets added by name are shown.")
                            .size(12.0)
                            .color(p.text),
                    );
                });
            ui.add_space(4.0);
        }
        Frame::new()
            .fill(p.list)
            .stroke(egui::Stroke::new(1.0, p.border))
            .inner_margin(Margin::same(2))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    if !has_account {
                        muted(ui, "No account selected.");
                    } else if self.buckets.is_empty() && !self.buckets_loading {
                        muted(ui, "No buckets. Use + to open a bucket by name.");
                    }
                    for bucket in &self.buckets {
                        let selected = self.current_bucket.as_deref() == Some(bucket.name.as_str());
                        ui.horizontal(|ui| {
                            let width = ui.available_width() - if bucket.manual { 70.0 } else { 0.0 };
                            let r = list_row(ui, selected, icon::ARCHIVE, p.accent, &bucket.name, width.max(40.0));
                            let r = match bucket.creation_date {
                                Some(d) => r.on_hover_text(format!("Created {}", d.format("%Y-%m-%d"))),
                                None => r,
                            };
                            if r.clicked() {
                                actions.push(Action::SelectBucket(bucket.name.clone()));
                            }
                            r.context_menu(|ui| {
                                if ui.button("Open").clicked() {
                                    actions.push(Action::SelectBucket(bucket.name.clone()));
                                }
                                if ui.button("Refresh").clicked() {
                                    actions.push(Action::Refresh);
                                }
                                if ui.button("Add bucket by name…").clicked() {
                                    actions.push(Action::AddBucketPrompt);
                                }
                                if ui.add_enabled(bucket.manual, Button::new("Remove from list")).clicked() {
                                    actions.push(Action::RemoveBucket(bucket.name.clone()));
                                }
                                if ui.button("Copy bucket name").clicked() {
                                    actions.push(Action::CopyText(bucket.name.clone()));
                                }
                                ui.separator();
                                if ui.button("Create new bucket…").clicked() {
                                    actions.push(Action::CreateBucketPrompt);
                                }
                                if ui.button("Delete bucket…").clicked() {
                                    actions.push(Action::DeleteBucket(bucket.name.clone()));
                                }
                                if ui.button("Properties").clicked() {
                                    actions.push(Action::BucketProperties(bucket.name.clone()));
                                }
                                if ui.button("Settings (versioning, policy, CORS, lifecycle)…").clicked() {
                                    actions.push(Action::BucketSettings(bucket.name.clone()));
                                }
                            });
                            if bucket.manual {
                                ui.label(RichText::new("added").size(10.5).color(p.dim))
                                    .on_hover_text("Added by name for this account");
                                if ui
                                    .add(Button::new(icon::X).small().frame(false))
                                    .on_hover_text("Remove from list (the bucket on the server is not touched)")
                                    .clicked()
                                {
                                    actions.push(Action::RemoveBucket(bucket.name.clone()));
                                }
                            }
                        });
                    }
                });
            });
    }

    pub(crate) fn files_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        self.path_bar(ui, actions);
        ui.add_space(2.0);

        if let Some(banner) = self.banner.clone() {
            Frame::new()
                .fill(p.banner)
                .stroke(egui::Stroke::new(1.0, p.warning.gamma_multiply(0.5)))
                .corner_radius(4)
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icon::WARNING_CIRCLE)
                                .size(20.0)
                                .color(p.warning),
                        );
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&banner.title).strong().color(p.text));
                            ui.label(RichText::new(&banner.detail).color(p.text));
                            ui.horizontal(|ui| {
                                if ui
                                    .button(format!("{} Retry", icon::ARROWS_CLOCKWISE))
                                    .clicked()
                                {
                                    actions.push(Action::Refresh);
                                }
                                if banner.offers_remove_bucket
                                    && let Some(b) = self.current_bucket.clone()
                                    && ui
                                        .button(format!("{} Remove from list", icon::TRASH))
                                        .clicked()
                                {
                                    actions.push(Action::RemoveBucket(b));
                                }
                                if (banner.offers_edit_account || banner.offers_remove_bucket)
                                    && ui
                                        .button(format!("{} Edit account", icon::PENCIL_SIMPLE))
                                        .clicked()
                                {
                                    actions.push(Action::EditAccount(self.current_account));
                                }
                                if ui.button("Dismiss").clicked() {
                                    actions.push(Action::DismissBanner);
                                }
                            });
                        });
                    });
                });
            ui.add_space(4.0);
        }

        if self.next_token.is_some() {
            egui::Panel::bottom(egui::Id::new("load-more"))
                .frame(Frame::new())
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.objects_loading,
                                Button::new(format!("{} Load next page", icon::CARET_DOWN)),
                            )
                            .clicked()
                        {
                            actions.push(Action::LoadMore);
                        }
                        muted(
                            ui,
                            "Only the first 1,000 items of each page are loaded at a time.",
                        );
                    });
                });
        }

        if let Some((title, detail, offer_add)) = self.empty_state() {
            Frame::new()
                .fill(p.list)
                .stroke(egui::Stroke::new(1.0, p.border))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ui.vertical_centered(|ui| {
                        ui.add_space((ui.available_height() / 3.0).max(20.0));
                        ui.label(RichText::new(title).heading().color(p.text));
                        ui.label(RichText::new(detail).color(p.dim));
                        if offer_add {
                            ui.add_space(8.0);
                            if ui.button(format!("{} Add account", icon::PLUS)).clicked() {
                                actions.push(Action::EditAccount(None));
                            }
                        }
                    });
                });
            return;
        }

        self.refresh_view();
        let response = Frame::new()
            .fill(p.list)
            .stroke(egui::Stroke::new(1.0, p.border))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.file_table(ui, actions);
            })
            .response;
        let _ = response;
    }

    fn path_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            let can_up = self.current_bucket.is_some() && !self.prefix.is_empty();
            if ui
                .add_enabled(can_up, Button::new(icon::ARROW_U_UP_LEFT))
                .on_hover_text("Up one level (Backspace)")
                .clicked()
            {
                actions.push(Action::Up);
            }
            let filter_width = 240.0_f32.min(ui.available_width() * 0.4);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let edit = TextEdit::singleline(&mut self.filter)
                    .hint_text(format!(
                        "{} Filter this folder (Ctrl+F)",
                        icon::MAGNIFYING_GLASS
                    ))
                    .desired_width(filter_width);
                let r = ui.add_enabled(self.current_bucket.is_some(), edit);
                if self.focus_filter {
                    r.request_focus();
                    self.focus_filter = false;
                }
                if self.objects_loading {
                    ui.spinner();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ScrollArea::horizontal()
                        .id_salt("breadcrumbs")
                        .stick_to_right(true)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let Some(bucket) = self.current_bucket.clone() else {
                                    ui.label(RichText::new("Select a bucket").color(p.dim));
                                    return;
                                };
                                if ui
                                    .add(
                                        Button::new(
                                            RichText::new(format!("{} {bucket}", icon::ARCHIVE))
                                                .strong(),
                                        )
                                        .frame(false),
                                    )
                                    .clicked()
                                {
                                    actions.push(Action::Navigate(String::new()));
                                }
                                for (name, prefix) in browse::breadcrumbs(&self.prefix) {
                                    ui.label(RichText::new(" / ").color(p.faint));
                                    if ui.add(Button::new(name).frame(false)).clicked() {
                                        actions.push(Action::Navigate(prefix));
                                    }
                                }
                            });
                        });
                });
            });
        });
    }

    fn empty_state(&self) -> Option<(String, String, bool)> {
        if self.current_account.is_none() {
            return Some((
                "Add your first storage account".into(),
                "Accounts hold the endpoint and keys. You can add as many as you need, including several keys for the same server.".into(),
                true,
            ));
        }
        if self.current_bucket.is_none() {
            if self.banner.is_some() {
                return Some((String::new(), String::new(), false));
            }
            return Some(if self.buckets.is_empty() && !self.buckets_loading {
                (
                    "No buckets to show".into(),
                    "This key can't list buckets, or there are none yet. Use Add bucket to open a bucket by name.".into(),
                    false,
                )
            } else {
                (
                    "Select a bucket".into(),
                    "Choose a bucket on the left to see its files.".into(),
                    false,
                )
            });
        }
        if self.banner.is_some() || !self.objects_loaded {
            return Some((String::new(), String::new(), false));
        }
        if self.objects.is_empty() {
            return Some((
                "This folder is empty".into(),
                "Drag files here from Explorer, or use Upload.".into(),
                false,
            ));
        }
        let needle = self.filter.trim().to_lowercase();
        if !needle.is_empty()
            && !self
                .objects
                .iter()
                .any(|o| o.name.to_lowercase().contains(&needle))
        {
            return Some((
                "No matches".into(),
                format!(
                    "Nothing in this folder matches \"{}\". Only loaded items are filtered.",
                    self.filter.trim()
                ),
                false,
            ));
        }
        None
    }

    fn file_table(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        let (sort_col, ascending) = self.sort;
        let header = |ui: &mut Ui, label: &str, col: SortColumn, actions: &mut Vec<Action>| {
            let arrow = if sort_col == col {
                if ascending {
                    icon::CARET_UP
                } else {
                    icon::CARET_DOWN
                }
            } else {
                ""
            };
            let r = ui.add(
                egui::Label::new(RichText::new(format!("{label} {arrow}")).strong())
                    .sense(Sense::click())
                    .selectable(false),
            );
            if r.clicked() {
                actions.push(Action::SortBy(col));
            }
        };
        let modifiers = ui.input(|i| i.modifiers);
        let view = &self.view;
        let objects = &self.objects;
        let selection = &self.selection;
        let can_paste = self.clipboard.is_some() && self.current_bucket.is_some();
        TableBuilder::new(ui)
            .id_salt("file-table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .auto_shrink(false)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::initial(90.0).at_least(60.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(140.0).at_least(80.0))
            .column(Column::initial(110.0).at_least(60.0).clip(true))
            .header(24.0, |mut row| {
                row.col(|ui| header(ui, "Name", SortColumn::Name, actions));
                row.col(|ui| header(ui, "Size", SortColumn::Size, actions));
                row.col(|ui| header(ui, "Type", SortColumn::Type, actions));
                row.col(|ui| header(ui, "Modified", SortColumn::Modified, actions));
                row.col(|ui| header(ui, "Storage class", SortColumn::Class, actions));
            })
            .body(|body| {
                body.rows(22.0, view.len(), |mut row| {
                    let vi = row.index();
                    let entry = &objects[view[vi]];
                    row.set_selected(selection.contains(&entry.key));
                    row.col(|ui| {
                        let (glyph, color) = entry_icon(&entry.name, entry.is_folder, p);
                        ui.label(RichText::new(glyph).color(color).size(15.0));
                        ui.add(egui::Label::new(&entry.name).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        if !entry.is_folder {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new(format::size(entry.size)).selectable(false),
                                );
                            });
                        }
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(entry.type_label()).color(p.dim))
                                .selectable(false),
                        );
                    });
                    row.col(|ui| {
                        let text = entry
                            .last_modified
                            .map(|t| {
                                t.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.add(
                            egui::Label::new(RichText::new(text).color(p.dim)).selectable(false),
                        );
                    });
                    row.col(|ui| {
                        let class = entry.storage_class.clone().unwrap_or_default();
                        ui.add(
                            egui::Label::new(RichText::new(class).color(p.dim)).selectable(false),
                        );
                    });
                    let response = row.response();
                    if response.double_clicked() {
                        if entry.is_folder {
                            actions.push(Action::Open(entry.key.clone()));
                        } else {
                            actions.push(Action::ClickRow {
                                view_index: vi,
                                ctrl: false,
                                shift: false,
                            });
                            actions.push(Action::Download);
                        }
                    } else if response.clicked() {
                        actions.push(Action::ClickRow {
                            view_index: vi,
                            ctrl: modifiers.command,
                            shift: modifiers.shift,
                        });
                    } else if response.secondary_clicked() {
                        actions.push(Action::ContextRow(vi));
                    }
                    let is_folder = entry.is_folder;
                    let key = entry.key.clone();
                    response.context_menu(|ui| {
                        let single = selection.len() <= 1;
                        let single_file = single && !is_folder;
                        if is_folder && ui.button(format!("{} Open", icon::FOLDER_OPEN)).clicked() {
                            actions.push(Action::Open(key.clone()));
                        }
                        if ui
                            .add_enabled(
                                true,
                                Button::new(format!("{} Download…", icon::DOWNLOAD_SIMPLE))
                                    .shortcut_text("Ctrl+D"),
                            )
                            .clicked()
                        {
                            actions.push(Action::Download);
                        }
                        if ui
                            .button(format!("{} Upload files here…", icon::UPLOAD_SIMPLE))
                            .clicked()
                        {
                            actions.push(Action::UploadFiles);
                        }
                        if ui
                            .button(format!("{} Upload folder here…", icon::UPLOAD_SIMPLE))
                            .clicked()
                        {
                            actions.push(Action::UploadFolder);
                        }
                        ui.separator();
                        if ui
                            .add(
                                Button::new(format!("{} New folder…", icon::FOLDER_PLUS))
                                    .shortcut_text("Ctrl+N"),
                            )
                            .clicked()
                        {
                            actions.push(Action::NewFolderPrompt);
                        }
                        if ui
                            .add_enabled(
                                single,
                                Button::new(format!("{} Rename…", icon::PENCIL_LINE))
                                    .shortcut_text("F2"),
                            )
                            .clicked()
                        {
                            actions.push(Action::RenamePrompt);
                        }
                        if ui
                            .add(
                                Button::new(format!("{} Delete", icon::TRASH)).shortcut_text("Del"),
                            )
                            .clicked()
                        {
                            actions.push(Action::DeleteSelected);
                        }
                        ui.separator();
                        if ui
                            .add(
                                Button::new(format!("{} Copy", icon::COPY)).shortcut_text("Ctrl+C"),
                            )
                            .clicked()
                        {
                            actions.push(Action::Copy);
                        }
                        if ui
                            .add(
                                Button::new(format!("{} Cut", icon::SCISSORS))
                                    .shortcut_text("Ctrl+X"),
                            )
                            .clicked()
                        {
                            actions.push(Action::Cut);
                        }
                        if ui
                            .add_enabled(
                                can_paste,
                                Button::new(format!("{} Paste", icon::CLIPBOARD))
                                    .shortcut_text("Ctrl+V"),
                            )
                            .clicked()
                        {
                            actions.push(Action::Paste);
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                single_file,
                                Button::new(format!("{} Web URL…", icon::LINK_SIMPLE)),
                            )
                            .clicked()
                        {
                            actions.push(Action::WebUrl);
                        }
                        if ui
                            .add_enabled(
                                single_file,
                                Button::new(format!("{} Edit headers…", icon::TAG)),
                            )
                            .clicked()
                        {
                            actions.push(Action::EditHeaders);
                        }
                        if ui
                            .button(format!("{} Copy path (s3://)", icon::CLIPBOARD_TEXT))
                            .clicked()
                        {
                            actions.push(Action::CopyPath);
                        }
                        if ui.button(format!("{} Copy key", icon::COPY)).clicked() {
                            actions.push(Action::CopyText(key.clone()));
                        }
                        if ui
                            .add(
                                Button::new(format!("{} Refresh", icon::ARROWS_CLOCKWISE))
                                    .shortcut_text("F5"),
                            )
                            .clicked()
                        {
                            actions.push(Action::Refresh);
                        }
                    });
                });
            });
    }
}
