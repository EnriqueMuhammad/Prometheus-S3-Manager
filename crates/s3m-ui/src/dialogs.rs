//! Dialogs: message/prompt/confirm/password, Storage Accounts, account editor, Options, About.

use egui::{Align, Button, Color32, Id, Key, Layout, Modal, RichText, ScrollArea, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use s3m_core::{APP_NAME, AccountType, export};

use crate::app::{Action, App};
use crate::model::{AccountRow, ConfirmAction, Modal as AppModal};
use crate::theme;
use crate::widgets::muted;

/// Right-aligned row of dialog buttons.
/// Right-aligned row of dialog buttons. The row gets a fixed height: a plain
/// `with_layout` would take all remaining height (sized from the previous frame)
/// and keep a modal from shrinking when its content gets shorter.
pub(crate) fn button_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(6.0);
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), add);
}

fn danger_button(ui: &mut Ui, label: &str) -> bool {
    ui.add(
        Button::new(RichText::new(label).color(Color32::WHITE))
            .fill(theme::palette(ui).danger_fill),
    )
    .clicked()
}

pub(crate) fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> bool {
    let p = theme::palette(ui);
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(label).color(Color32::WHITE)).fill(p.accent_fill),
    )
    .clicked()
}

impl App {
    pub(crate) fn modal_window(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));
        enum Outcome {
            Keep,
            Close,
            Prompt,
            Confirm(bool),
            Password,
            CreateBucket,
            DeleteBucket,
            WebUrl,
            Headers,
        }
        let mut outcome = Outcome::Keep;
        Modal::new(Id::new(("app-modal", std::mem::discriminant(&modal)))).show(ctx, |ui| {
            let p = theme::palette(ui);
            match &mut modal {
                AppModal::Message { title, body, error } => {
                    ui.set_max_width(520.0);
                    ui.horizontal(|ui| {
                        let (glyph, color) = if *error {
                            (icon::X_CIRCLE, p.danger)
                        } else {
                            (icon::INFO, p.accent)
                        };
                        ui.label(RichText::new(glyph).size(26.0).color(color));
                        ui.vertical(|ui| {
                            ui.heading(title.as_str());
                            ui.add(egui::Label::new(body.as_str()).wrap());
                        });
                    });
                    button_row(ui, |ui| {
                        if primary_button(ui, "OK", true) || enter || escape {
                            outcome = Outcome::Close;
                        }
                    });
                }
                AppModal::Prompt {
                    title,
                    label,
                    text,
                    focus,
                    ..
                } => {
                    ui.set_width(420.0);
                    ui.heading(title.as_str());
                    ui.add(egui::Label::new(label.as_str()).wrap());
                    let r = ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    let submit = r.lost_focus() && enter;
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if primary_button(ui, "OK", !text.trim().is_empty())
                            || (submit && !text.trim().is_empty())
                        {
                            outcome = Outcome::Prompt;
                        }
                    });
                }
                AppModal::Confirm {
                    title,
                    body,
                    ok,
                    alternative,
                    danger,
                    ..
                } => {
                    ui.set_max_width(560.0);
                    ui.heading(title.as_str());
                    ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        ui.add(egui::Label::new(body.as_str()).wrap());
                    });
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if let Some(alt) = alternative
                            && ui.button(alt.as_str()).clicked()
                        {
                            outcome = Outcome::Confirm(true);
                        }
                        let clicked = if *danger {
                            danger_button(ui, ok)
                        } else {
                            primary_button(ui, ok, true)
                        };
                        if clicked {
                            outcome = Outcome::Confirm(false);
                        }
                    });
                }
                AppModal::CreateBucket { name, region, error, .. } => {
                    ui.set_width(440.0);
                    ui.heading(format!("{} Create new bucket", icon::ARCHIVE));
                    ui.add_space(4.0);
                    egui::Grid::new("create-bucket").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Bucket name:");
                        let r = ui.add(TextEdit::singleline(name).hint_text("e.g. po-archive-2026").desired_width(260.0));
                        if name.is_empty() && !r.has_focus() {
                            r.request_focus();
                        }
                        ui.end_row();
                        ui.label("Region:");
                        ui.add(TextEdit::singleline(region).hint_text("server default").desired_width(160.0));
                        ui.end_row();
                    });
                    muted(ui, "3 to 63 lowercase letters, digits, '.' or '-'. Bucket names are global on Amazon S3.");
                    let normalized = s3m_core::bucket::normalize(name);
                    if !name.is_empty() && !s3m_core::bucket::is_valid(&normalized) {
                        *error = "This is not a valid bucket name.".into();
                    } else {
                        error.clear();
                    }
                    if !error.is_empty() {
                        ui.label(RichText::new(error.as_str()).color(p.danger));
                    }
                    let ok = !name.is_empty() && error.is_empty();
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if primary_button(ui, "Create", ok) || (enter && ok) {
                            outcome = Outcome::CreateBucket;
                        }
                    });
                }
                AppModal::DeleteBucket { name, contents, typed } => {
                    ui.set_width(460.0);
                    ui.heading(format!("{} Delete bucket", icon::TRASH));
                    ui.add(egui::Label::new(format!("Delete the bucket \"{name}\" from the server?")).wrap());
                    ui.checkbox(contents, "Also delete every object in the bucket");
                    if *contents {
                        ui.label(
                            RichText::new("All files in the bucket are permanently deleted. Type the bucket name to confirm:")
                                .color(p.danger),
                        );
                        ui.add(TextEdit::singleline(typed).hint_text(name.as_str()).desired_width(f32::INFINITY));
                    } else {
                        muted(ui, "Only an empty bucket can be deleted this way.");
                    }
                    let ok = !*contents || typed.trim() == name.as_str();
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if ui
                            .add_enabled(
                                ok,
                                Button::new(RichText::new("Delete bucket").color(Color32::WHITE))
                                    .fill(theme::palette(ui).danger_fill),
                            )
                            .clicked()
                        {
                            outcome = Outcome::DeleteBucket;
                        }
                    });
                }
                AppModal::WebUrl { bucket, key, minutes, url, error, busy, .. } => {
                    ui.set_width(560.0);
                    ui.heading(format!("{} Web URL", icon::LINK_SIMPLE));
                    ui.add(egui::Label::new(RichText::new(format!("{bucket}/{key}")).color(p.dim)).wrap());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Valid for:");
                        let before = *minutes;
                        for (label, m) in [
                            ("15 minutes", 15),
                            ("1 hour", 60),
                            ("12 hours", 720),
                            ("1 day", 1440),
                            ("7 days", 10080),
                        ] {
                            ui.selectable_value(minutes, m, label);
                        }
                        if *minutes != before {
                            outcome = Outcome::WebUrl;
                        }
                    });
                    if *busy {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Generating...");
                        });
                    }
                    if !url.is_empty() {
                        let mut shown = url.clone();
                        ui.add(
                            TextEdit::multiline(&mut shown)
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                        if url.starts_with("http://") {
                            ui.label(
                                RichText::new("This account uses plain HTTP: the link only works inside the private network and is not encrypted.")
                                    .color(p.warning),
                            );
                        }
                        muted(ui, "Anyone with this link can download the file until it expires. The link stops working earlier if the access key is disabled.");
                    }
                    if !error.is_empty() {
                        ui.label(RichText::new(error.as_str()).color(p.danger));
                    }
                    button_row(ui, |ui| {
                        if ui.button("Close").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if primary_button(ui, &format!("{} Copy URL", icon::COPY), !url.is_empty()) {
                            ui.ctx().copy_text(url.clone());
                        }
                    });
                }
                AppModal::Headers { bucket, key, edit, error } => {
                    ui.set_width(600.0);
                    ui.heading(format!("{} Edit headers", icon::TAG));
                    ui.add(egui::Label::new(RichText::new(format!("{bucket}/{key}")).color(p.dim)).wrap());
                    ui.add_space(4.0);
                    egui::Grid::new("headers-edit").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        for (label, value, hint) in [
                            ("Content-Type", &mut edit.content_type, "application/octet-stream"),
                            ("Cache-Control", &mut edit.cache_control, "max-age=3600"),
                            ("Content-Disposition", &mut edit.content_disposition, "attachment"),
                            ("Content-Encoding", &mut edit.content_encoding, "gzip"),
                            ("Content-Language", &mut edit.content_language, "id-ID"),
                        ] {
                            ui.label(label);
                            ui.add(TextEdit::singleline(value).hint_text(hint).desired_width(380.0));
                            ui.end_row();
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(RichText::new("User metadata (x-amz-meta-*)").strong());
                    let mut remove = None;
                    for (i, (k, v)) in edit.metadata.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(TextEdit::singleline(k).hint_text("name").desired_width(170.0));
                            ui.add(TextEdit::singleline(v).hint_text("value").desired_width(320.0));
                            if ui.small_button(icon::X).on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        edit.metadata.remove(i);
                    }
                    if ui.small_button(format!("{} Add metadata", icon::PLUS)).clicked() {
                        edit.metadata.push((String::new(), String::new()));
                    }
                    muted(ui, "Saving copies the object onto itself with the new headers; the content does not change.");
                    if !error.is_empty() {
                        ui.label(RichText::new(error.as_str()).color(p.danger));
                    }
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if primary_button(ui, "Save", true) {
                            let mut check = edit.clone();
                            check.metadata.retain(|(k, _)| !k.trim().is_empty());
                            match check.validate() {
                                Ok(()) => outcome = Outcome::Headers,
                                Err(e) => *error = e,
                            }
                        }
                    });
                }
                AppModal::Password {
                    title,
                    body,
                    confirm,
                    first,
                    second,
                    error,
                    ..
                } => {
                    ui.set_width(420.0);
                    ui.heading(title.as_str());
                    ui.add(egui::Label::new(body.as_str()).wrap());
                    let r1 = ui.add(
                        TextEdit::singleline(first)
                            .password(true)
                            .hint_text("Password")
                            .desired_width(f32::INFINITY),
                    );
                    if first.is_empty() && !r1.has_focus() && second.is_empty() {
                        r1.request_focus();
                    }
                    if *confirm {
                        ui.add(
                            TextEdit::singleline(second)
                                .password(true)
                                .hint_text("Repeat password")
                                .desired_width(f32::INFINITY),
                        );
                    }
                    if !error.is_empty() {
                        ui.label(RichText::new(error.as_str()).color(p.danger));
                    }
                    button_row(ui, |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            outcome = Outcome::Close;
                        }
                        if primary_button(ui, "OK", !first.is_empty())
                            || (enter && !first.is_empty())
                        {
                            if *confirm && first.chars().count() < export::MIN_PASSWORD_LENGTH {
                                *error = format!(
                                    "Use at least {} characters.",
                                    export::MIN_PASSWORD_LENGTH
                                );
                            } else if *confirm && first != second {
                                *error = "The passwords do not match.".into();
                            } else {
                                outcome = Outcome::Password;
                            }
                        }
                    });
                }
            }
        });
        match outcome {
            Outcome::Keep => self.modal = Some(modal),
            Outcome::Close => {}
            Outcome::Prompt => {
                if let AppModal::Prompt { action, text, .. } = modal {
                    self.submit_prompt(action, text);
                }
            }
            Outcome::Confirm(alternative) => {
                if let AppModal::Confirm { action, .. } = modal {
                    self.submit_confirm(action, alternative);
                }
            }
            Outcome::CreateBucket => {
                if let AppModal::CreateBucket { name, region, .. } = modal {
                    self.create_bucket(name, region);
                }
            }
            Outcome::DeleteBucket => {
                if let AppModal::DeleteBucket { name, contents, .. } = modal {
                    self.delete_bucket(name, contents);
                }
            }
            Outcome::WebUrl => {
                self.modal = Some(modal);
                self.request_web_url();
            }
            Outcome::Headers => {
                if let AppModal::Headers {
                    bucket,
                    key,
                    mut edit,
                    ..
                } = modal
                {
                    edit.metadata.retain(|(k, _)| !k.trim().is_empty());
                    self.save_headers(bucket, key, edit);
                }
            }
            Outcome::Password => {
                if let AppModal::Password { action, first, .. } = modal {
                    self.submit_password(action, first);
                }
            }
        }
    }

    pub(crate) fn accounts_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut window) = self.accounts_window.take() else {
            return;
        };
        if window.revision != self.accounts.revision()
            || window.rows.len() != self.account_list.len()
        {
            window.revision = self.accounts.revision();
            window.rows = self
                .account_list
                .iter()
                .map(|a| AccountRow {
                    id: a.id,
                    name: a.name.clone(),
                    type_label: a.kind.label(),
                    endpoint: a.endpoint_url(),
                    has_secret: self.accounts.has_secret(a.id),
                    folder: a.folder.clone(),
                })
                .collect();
            if window
                .selected
                .is_none_or(|s| !window.rows.iter().any(|r| r.id == s))
            {
                window.selected = window.rows.first().map(|r| r.id);
            }
        }
        let active = self.current_account;
        let mut close = false;
        let mut local: Vec<Action> = Vec::new();
        let mut delete: Option<uuid::Uuid> = None;
        let mut moves: Option<(uuid::Uuid, i32)> = None;
        let busy_ids: Vec<uuid::Uuid> = window
            .rows
            .iter()
            .filter(|r| self.queue.has_active_jobs(r.id))
            .map(|r| r.id)
            .collect();
        Modal::new(Id::new("accounts-window")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(760.0);
            ui.heading(format!("{} Storage Accounts", icon::USERS));
            muted(ui, "Accounts are listed in the order of the toolbar's account box. Secret keys are kept in the Windows Credential Manager.");
            ui.add_space(4.0);
            let selected_index = window.selected.and_then(|s| window.rows.iter().position(|r| r.id == s));
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(600.0);
                    ui.set_height(300.0);
                    // The folder column only appears once an account is put in a folder.
                    let show_folder = window.rows.iter().any(|r| !r.folder.is_empty());
                    egui::Frame::new()
                        .fill(p.list)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            // Plain labels: a click on the text selects the row instead of the text.
                            ui.style_mut().interaction.selectable_labels = false;
                            let mut table = TableBuilder::new(ui)
                                .id_salt(("accounts-table", show_folder))
                                .striped(true)
                                .sense(egui::Sense::click())
                                .auto_shrink(false)
                                .cell_layout(Layout::left_to_right(Align::Center));
                            if show_folder {
                                table = table.column(Column::initial(100.0).clip(true));
                            }
                            let mut headers = vec!["Account name", "Account type", "Endpoint"];
                            if show_folder {
                                headers.insert(0, "Folder");
                            }
                            table
                                .column(Column::initial(190.0).clip(true))
                                .column(Column::initial(150.0).clip(true))
                                .column(Column::remainder().clip(true))
                                .header(22.0, |mut row| {
                                    for h in headers {
                                        row.col(|ui| {
                                            ui.add(egui::Label::new(RichText::new(h).strong()).truncate());
                                        });
                                    }
                                })
                                .body(|body| {
                                    body.rows(24.0, window.rows.len(), |mut row| {
                                        let r = &window.rows[row.index()];
                                        row.set_selected(window.selected == Some(r.id));
                                        if show_folder {
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(RichText::new(&r.folder).color(p.dim)).truncate());
                                            });
                                        }
                                        row.col(|ui| {
                                            ui.label(&r.name);
                                            if active == Some(r.id) {
                                                ui.label(RichText::new("active").size(10.5).color(p.success));
                                            }
                                            if !r.has_secret {
                                                ui.label(RichText::new("no secret").size(10.5).color(p.warning));
                                            }
                                        });
                                        row.col(|ui| {
                                            ui.add(egui::Label::new(r.type_label).truncate());
                                        });
                                        row.col(|ui| {
                                            // Elided when too long; the full URL shows on hover.
                                            ui.add(egui::Label::new(RichText::new(&r.endpoint).color(p.dim)).truncate());
                                        });
                                        let resp = row.response();
                                        if resp.clicked() {
                                            window.selected = Some(r.id);
                                        }
                                        if resp.double_clicked() {
                                            local.push(Action::EditAccount(Some(r.id)));
                                        }
                                    });
                                });
                        });
                });
                ui.vertical(|ui| {
                    let has = window.selected.is_some();
                    let w = [130.0, 24.0];
                    if ui.add_sized(w, Button::new(format!("{} Add…", icon::PLUS))).clicked() {
                        local.push(Action::EditAccount(None));
                    }
                    if ui.add_enabled_ui(has, |ui| ui.add_sized(w, Button::new(format!("{} Edit…", icon::PENCIL_SIMPLE)))).inner.clicked() {
                        local.push(Action::EditAccount(window.selected));
                    }
                    if ui.add_enabled_ui(has, |ui| ui.add_sized(w, Button::new(format!("{} Delete", icon::TRASH)))).inner.clicked() {
                        delete = window.selected;
                    }
                    ui.separator();
                    let can_up = selected_index.is_some_and(|i| i > 0);
                    let can_down = selected_index.is_some_and(|i| i + 1 < window.rows.len());
                    if ui.add_enabled_ui(can_up, |ui| ui.add_sized(w, Button::new(format!("{} Move up", icon::ARROW_UP)))).inner.clicked() {
                        moves = window.selected.map(|id| (id, -1));
                    }
                    if ui.add_enabled_ui(can_down, |ui| ui.add_sized(w, Button::new(format!("{} Move down", icon::ARROW_DOWN)))).inner.clicked() {
                        moves = window.selected.map(|id| (id, 1));
                    }
                    ui.separator();
                    if ui.add_enabled_ui(has, |ui| ui.add_sized(w, Button::new(format!("{} Switch to", icon::CHECK)))).inner.clicked()
                        && let Some(id) = window.selected
                    {
                        local.push(Action::SelectAccount(id));
                        close = true;
                    }
                    ui.separator();
                    if ui.add_sized(w, Button::new(format!("{} Import…", icon::TRAY_ARROW_DOWN))).clicked() {
                        local.push(Action::ImportAccounts);
                    }
                    if ui.add_enabled_ui(!window.rows.is_empty(), |ui| ui.add_sized(w, Button::new(format!("{} Export…", icon::EXPORT)))).inner.clicked() {
                        local.push(Action::ExportAccounts);
                    }
                });
            });
            if !window.notice.is_empty() {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(&window.notice).color(p.dim)).wrap());
            }
            button_row(ui, |ui| {
                if primary_button(ui, "Close", true) {
                    close = true;
                }
            });
        });
        if let Some(id) = delete {
            if busy_ids.contains(&id) {
                self.show_error(
                    "Delete account",
                    "Cancel this account's running transfers first.",
                );
            } else {
                let name = window
                    .rows
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.name.clone())
                    .unwrap_or_default();
                self.modal = Some(AppModal::Confirm {
                    title: "Delete account".into(),
                    body: format!(
                        "Delete account \"{name}\"? Its stored secret key is removed from this PC."
                    ),
                    ok: "Delete".into(),
                    alternative: None,
                    action: ConfirmAction::DeleteAccount { id },
                    danger: true,
                });
            }
        }
        if let Some((id, direction)) = moves
            && let Err(e) = self.accounts.move_by(id, direction)
        {
            self.show_error("Storage Accounts", e.to_string());
        }
        let escape = ctx.input(|i| i.key_pressed(Key::Escape))
            && self.modal.is_none()
            && self.editor.is_none();
        if !close && !escape {
            self.accounts_window = Some(window);
        }
        actions.extend(local);
    }

    pub(crate) fn editor_window(&mut self, ctx: &egui::Context) {
        if self.editor.is_none() {
            return;
        }
        let mut save = false;
        let mut test = false;
        let mut cancel = false;
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        if let Some(editor) = self.editor.as_mut() {
            let editing = editor.draft.id.is_some();
            Modal::new(Id::new("account-editor")).show(ctx, |ui| {
                let p = theme::palette(ui);
                ui.set_width(560.0);
                ui.heading(if editing { "Edit Account" } else { "Add New Account" });
                muted(ui, if editing { "Edit account details and click Save changes." } else { "Enter account details and click Add new account." });
                ui.add_space(6.0);
                let d = &mut editor.draft;
                egui::Grid::new("editor-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .min_col_width(140.0)
                    .show(ui, |ui| {
                        ui.label("Display name:");
                        ui.add(TextEdit::singleline(&mut d.name).hint_text("e.g. Digital PO S3").desired_width(f32::INFINITY));
                        ui.end_row();

                        ui.label("Preset:");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("account-preset")
                                .width(260.0)
                                .selected_text("Fill in for a provider…")
                                .show_ui(ui, |ui| {
                                    for preset in s3m_core::presets::PRESETS {
                                        if ui.selectable_label(false, preset.name).clicked() {
                                            d.kind = preset.kind;
                                            d.endpoint_host = preset.endpoint.into();
                                            d.region = preset.region.into();
                                            d.force_path_style = preset.path_style;
                                            d.use_tls = preset.tls;
                                            editor.error = preset.hint.into();
                                        }
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("Folder:");
                        ui.add(
                            TextEdit::singleline(&mut d.folder)
                                .hint_text("optional, e.g. Office/Production")
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label("Account type:");
                        let before = d.kind;
                        egui::ComboBox::from_id_salt("account-type")
                            .width(260.0)
                            .selected_text(d.kind.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut d.kind, AccountType::AmazonS3, AccountType::AmazonS3.label());
                                ui.selectable_value(&mut d.kind, AccountType::S3Compatible, AccountType::S3Compatible.label());
                            });
                        if before != d.kind {
                            if d.kind == AccountType::S3Compatible && d.endpoint_host == "s3.amazonaws.com" {
                                d.endpoint_host.clear();
                                d.force_path_style = true;
                            }
                            if d.kind == AccountType::AmazonS3 && d.endpoint_host.is_empty() {
                                d.endpoint_host = "s3.amazonaws.com".into();
                                d.force_path_style = false;
                            }
                        }
                        ui.end_row();

                        ui.label("API endpoint:");
                        let r = ui.add(TextEdit::singleline(&mut d.endpoint_host).hint_text("store.example.net or 10.0.0.5:9000").desired_width(f32::INFINITY));
                        if r.lost_focus() {
                            let f = s3m_core::endpoint::normalize(&d.endpoint_host, d.use_tls);
                            d.endpoint_host = f.host;
                            d.use_tls = f.use_tls;
                        }
                        ui.end_row();

                        ui.label("Access Key ID:");
                        ui.add(TextEdit::singleline(&mut d.access_key_id).desired_width(f32::INFINITY));
                        ui.end_row();

                        ui.label("Secret Access Key:");
                        ui.vertical(|ui| {
                            ui.add(TextEdit::singleline(&mut editor.secret).password(true).desired_width(f32::INFINITY));
                            muted(ui, if editor.has_secret { "Leave blank to keep the saved key." } else { "Required." });
                        });
                        ui.end_row();

                        ui.label("");
                        ui.vertical(|ui| {
                            ui.checkbox(&mut d.use_tls, "Use secure transfer (SSL/TLS)");
                            let hint = if d.use_tls {
                                "All communication goes through an encrypted TLS channel. Company CAs installed in Windows are trusted automatically."
                            } else {
                                "Plain HTTP sends data unencrypted and is only allowed to private-network addresses (checked on every connection)."
                            };
                            ui.add(egui::Label::new(RichText::new(hint).size(12.0).color(if d.use_tls { p.dim } else { p.warning })).wrap());
                        });
                        ui.end_row();
                    });
                ui.add_space(4.0);
                egui::CollapsingHeader::new("Advanced settings")
                    .id_salt("editor-advanced")
                    .default_open(editor.advanced)
                    .show(ui, |ui| {
                        egui::Grid::new("editor-advanced-grid")
                            .num_columns(2)
                            .spacing([12.0, 8.0])
                            .min_col_width(140.0)
                            .show(ui, |ui| {
                                ui.label("Region:");
                                ui.add(TextEdit::singleline(&mut d.region).desired_width(200.0));
                                ui.end_row();
                                ui.label("");
                                ui.checkbox(&mut d.force_path_style, "Use path-style requests (most S3-compatible servers)");
                                ui.end_row();
                                ui.label("Registered buckets:").on_hover_text("For keys that may open a bucket but not list all buckets. One per line.");
                                ui.add(TextEdit::multiline(&mut editor.buckets_text).desired_rows(3).hint_text("one bucket name per line").desired_width(f32::INFINITY));
                                ui.end_row();
                                ui.label("Custom CA (PEM):").on_hover_text("Only needed when the company root CA is not installed in Windows.");
                                ScrollArea::vertical().id_salt("ca-scroll").max_height(110.0).show(ui, |ui| {
                                    ui.add(
                                        TextEdit::multiline(&mut d.custom_ca_pem)
                                            .font(egui::TextStyle::Monospace)
                                            .desired_rows(4)
                                            .hint_text("-----BEGIN CERTIFICATE-----")
                                            .desired_width(f32::INFINITY),
                                    );
                                });
                                ui.end_row();
                                ui.label("Notes:");
                                ui.add(TextEdit::multiline(&mut d.notes).desired_rows(2).desired_width(f32::INFINITY));
                                ui.end_row();
                            });
                    });
                if !editor.error.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("{} {}", icon::WARNING_CIRCLE, editor.error)).color(p.danger));
                }
                if editor.testing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Testing connection…");
                    });
                } else if let Some(t) = &editor.test_result {
                    let (glyph, color) = if t.ok { (icon::CHECK_CIRCLE, p.success) } else { (icon::X_CIRCLE, p.danger) };
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{glyph} {} ({} ms)", t.message, t.latency.as_millis())).color(color),
                        )
                        .wrap(),
                    );
                }
                button_row(ui, |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if primary_button(ui, if editing { "Save changes" } else { "Add new account" }, true) {
                        save = true;
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        if ui
                            .add_enabled(!editor.testing, Button::new(format!("{} Test connection", icon::PLUGS_CONNECTED)))
                            .clicked()
                        {
                            test = true;
                        }
                    });
                });
            });
            let _ = enter;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) && self.modal.is_none() {
            cancel = true;
        }
        if cancel {
            self.editor = None;
        } else if save {
            self.save_editor();
        } else if test {
            self.test_from_editor();
        }
    }

    pub(crate) fn options_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.options.take() else {
            return;
        };
        let mut done: Option<bool> = None;
        Modal::new(Id::new("options-window")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(format!("{} Options", icon::GEAR));
            ui.add_space(6.0);
            let s = &mut state.settings;
            egui::Grid::new("options-grid")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Transfers at once:");
                    ui.add(egui::Slider::new(&mut s.transfer_concurrency, 1..=16));
                    ui.end_row();
                    ui.label("Attempts per transfer:");
                    ui.add(egui::Slider::new(&mut s.transfer_max_attempts, 1..=10));
                    ui.end_row();
                    ui.label("Bandwidth limit:");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut s.bandwidth_limit_kbps)
                                .range(0..=10_000_000)
                                .speed(64),
                        );
                        ui.label("KB/s for all transfers (0 = unlimited)");
                    });
                    ui.end_row();
                    ui.label("Ask before uploading more than:");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut s.confirm_upload_over).range(0..=100_000));
                        ui.label("files (0 = never ask)");
                    });
                    ui.end_row();
                    ui.label("Theme:");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut s.dark_mode, false, "Light");
                        ui.selectable_value(&mut s.dark_mode, true, "Dark");
                    });
                    ui.end_row();
                    ui.label("Updates:");
                    ui.checkbox(&mut s.check_updates, "Check for a new version when the app starts");
                    ui.end_row();
                });
            muted(
                ui,
                "Changes to transfers at once apply to transfers that start afterwards.",
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("Proxy for TLS accounts (plain-HTTP accounts on the private network never use it)")
                    .strong(),
            );
            let mode = &mut state.settings.proxy_mode;
            ui.horizontal(|ui| {
                ui.radio_value(mode, "none".to_owned(), "No proxy");
                ui.radio_value(mode, "system".to_owned(), "Windows settings / environment");
                ui.radio_value(mode, "manual".to_owned(), "Manual");
            });
            if state.settings.proxy_mode == "manual" {
                egui::Grid::new("proxy-grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Proxy URL:");
                        ui.add(
                            TextEdit::singleline(&mut state.settings.proxy_url)
                                .hint_text("http://proxy.corp.local:8080")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                        ui.label("User:");
                        ui.add(
                            TextEdit::singleline(&mut state.settings.proxy_user)
                                .hint_text("optional")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                        ui.label("Password:");
                        ui.add(
                            TextEdit::singleline(&mut state.proxy_password)
                                .password(true)
                                .hint_text("leave blank to keep the saved one")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                        ui.label("Bypass for:");
                        ui.add(
                            TextEdit::singleline(&mut state.settings.proxy_no_proxy)
                                .hint_text("*.corp.local, 10.0.0.0/8")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    });
            }
            if !state.error.is_empty() {
                ui.label(RichText::new(state.error.as_str()).color(theme::palette(ui).danger));
            }
            button_row(ui, |ui| {
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
                if primary_button(ui, "Save", true) {
                    done = Some(true);
                }
            });
        });
        if done == Some(true)
            && state.settings.proxy_mode == "manual"
            && let Err(e) = s3m_s3::validate_proxy_url(&state.settings.proxy_url)
        {
            state.error = e;
            done = None;
        }
        match done {
            Some(true) => self.save_options(state.settings, state.proxy_password),
            Some(false) => {}
            None => self.options = Some(state),
        }
    }

    pub(crate) fn about_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_about {
            return;
        }
        let mut close = false;
        Modal::new(Id::new("about-window")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(440.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::CLOUD).size(40.0).color(p.accent));
                ui.vertical(|ui| {
                    ui.heading(APP_NAME);
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                });
            });
            ui.add_space(6.0);
            ui.add(egui::Label::new("Multi-account desktop file manager for Amazon S3 and S3-compatible storage. Native Rust/egui build.").wrap());
            ui.add_space(4.0);
            ui.label("Created by Enrique Muhammad Ilham.");
            ui.label(RichText::new("© 2026 Enrique Muhammad Ilham").color(p.dim));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Data folder:").color(p.dim));
                if ui.link(self.data_dir.display().to_string()).clicked() {
                    actions.push(Action::OpenFolder(self.data_dir.clone()));
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Updates:").color(p.dim));
                match &self.update_repo {
                    Some(repo) => {
                        ui.hyperlink_to(
                            format!("github.com/{repo}"),
                            format!("https://github.com/{repo}/releases"),
                        );
                    }
                    None => {
                        ui.label("not available in this build");
                    }
                }
            });
            button_row(ui, |ui| {
                if primary_button(ui, "OK", true) {
                    close = true;
                }
            });
        });
        if close || ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter)) {
            self.show_about = false;
        }
    }
}
