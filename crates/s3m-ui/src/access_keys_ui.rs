//! Access keys (service accounts) of the current account's user, on MinIO servers.

use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use egui::{Align, Button, Id, Layout, Modal, RichText, ScrollArea, Sense, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use s3m_core::minio::{self, AccessKey, AccessKeyInfo, AccessKeyUpdate, NewAccessKey};
use s3m_core::{Account, AccountDraft};
use uuid::Uuid;

use crate::admin_ui::Fetch;
use crate::app::App;
use crate::dialogs::{button_row, primary_button};
use crate::model::Msg;
use crate::theme;
use crate::widgets::muted;

/// Keys of the user plus the user's own policy (prefills a new key's policy).
#[derive(Debug, Clone)]
pub struct KeyList {
    /// The keys.
    pub keys: Vec<AccessKey>,
    /// Name of the signed-in user.
    pub user: String,
    /// The user's policy, pretty JSON (empty when unknown).
    pub policy: String,
}

/// Results of access key requests.
#[derive(Debug)]
pub enum KeysMsg {
    /// The list was loaded.
    Listed {
        /// Account the list belongs to.
        account: Uuid,
        /// Keys or error.
        result: Result<KeyList, String>,
    },
    /// Details of the key being edited.
    Info {
        /// Access key.
        access_key: String,
        /// Details or error.
        result: Result<AccessKeyInfo, String>,
    },
    /// A key was created: its credentials.
    Created(Result<(String, String), String>),
    /// A key was changed or deleted; the message to show.
    Changed(Result<String, String>),
}

/// The Access keys window.
pub struct KeysWindow {
    account: Account,
    list: Fetch<KeyList>,
    selected: Option<String>,
    notice: String,
    notice_error: bool,
    busy: bool,
    confirm_delete: Option<String>,
}

/// Create or edit form.
pub struct KeyEditor {
    /// `None` = create.
    editing: Option<String>,
    access_key: String,
    secret_key: String,
    show_secret: bool,
    restrict: bool,
    policy: String,
    expires: bool,
    expiry_date: String,
    expiry_time: String,
    name: String,
    description: String,
    enabled: bool,
    /// Details of the edited key (`None` while loading).
    original: Option<AccessKeyInfo>,
    load_error: Option<String>,
    saving: bool,
    error: String,
}

/// Credentials of a key that was just created (the secret is shown once).
pub struct CreatedKey {
    access_key: String,
    secret_key: String,
    name: String,
}

/// Parses the expiry fields (local time) into UTC.
fn parse_expiry(date: &str, time: &str) -> Result<DateTime<Utc>, String> {
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map_err(|_| "Enter the expiry date as YYYY-MM-DD.".to_owned())?;
    let time = if time.trim().is_empty() {
        NaiveTime::MIN
    } else {
        NaiveTime::parse_from_str(time.trim(), "%H:%M")
            .map_err(|_| "Enter the expiry time as HH:MM (24-hour).".to_owned())?
    };
    let local = Local
        .from_local_datetime(&date.and_time(time))
        .earliest()
        .ok_or("That time does not exist in the local time zone.")?;
    let utc = local.with_timezone(&Utc);
    if utc <= Utc::now() {
        return Err("The expiry must be in the future.".into());
    }
    Ok(utc)
}

fn local_text(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

/// An on/off switch.
fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(36.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool(response.id, *on);
    let p = theme::palette(ui);
    let fill = if *on { p.accent } else { p.border };
    ui.painter().rect_filled(rect, 10.0, fill);
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), 7.0, egui::Color32::WHITE);
    response
}

/// Read-only text with a copy button.
fn copy_field(ui: &mut Ui, text: &str, masked: bool) {
    ui.horizontal(|ui| {
        let mut shown = text.to_owned();
        ui.add(
            TextEdit::singleline(&mut shown)
                .password(masked)
                .font(egui::TextStyle::Monospace)
                .desired_width(380.0)
                .interactive(true),
        );
        if ui
            .button(icon::COPY)
            .on_hover_text("Copy to the clipboard")
            .clicked()
        {
            ui.ctx().copy_text(text.to_owned());
        }
    });
}

fn err_text(e: &s3m_s3::StorageError) -> String {
    e.display()
}

const HELP: [(&str, &str); 3] = [
    (
        "What an access key is",
        "An access key is a second set of credentials for your user, meant for applications and scripts. It can do what your user can do, unless you restrict it. It cannot sign in to the server's web console.",
    ),
    (
        "Credentials",
        "Random keys are filled in for you; you may replace them with your own (access key 3-20 characters, secret key 8-40). The secret key is shown only once after creation: copy it or add it as a storage account.",
    ),
    (
        "Restricting a key",
        "Turn on \"Restrict beyond user policy\" to give the key its own JSON policy. It starts as a copy of your user's policy: remove statements, actions or resources the key should not have. A key can never get more access than your user.",
    ),
];

impl App {
    /// Opens the Access keys window for the current account.
    pub(crate) fn open_access_keys(&mut self) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        self.access_keys = Some(KeysWindow {
            account,
            list: Fetch::Loading,
            selected: None,
            notice: String::new(),
            notice_error: false,
            busy: false,
            confirm_delete: None,
        });
        self.load_access_keys();
    }

    fn load_access_keys(&mut self) {
        let Some(w) = self.access_keys.as_mut() else {
            return;
        };
        w.list = Fetch::Loading;
        let account = w.account.clone();
        let storage = self.storage.clone();
        self.spawn(async move {
            let (keys, info) = tokio::join!(
                storage.list_access_keys(&account),
                storage.account_policy(&account)
            );
            let result = keys.map_err(|e| err_text(&e)).map(|keys| {
                let (user, policy) = info.unwrap_or_default();
                KeyList { keys, user, policy }
            });
            Msg::Keys(KeysMsg::Listed {
                account: account.id,
                result,
            })
        });
    }

    pub(crate) fn handle_keys_msg(&mut self, msg: KeysMsg) {
        match msg {
            KeysMsg::Listed { account, result } => {
                if let Some(w) = self
                    .access_keys
                    .as_mut()
                    .filter(|w| w.account.id == account)
                {
                    w.list = match result {
                        Ok(list) => {
                            if w.selected
                                .as_ref()
                                .is_some_and(|s| !list.keys.iter().any(|k| &k.access_key == s))
                            {
                                w.selected = None;
                            }
                            Fetch::Ready(list)
                        }
                        Err(e) => Fetch::Failed(e),
                    };
                }
            }
            KeysMsg::Info { access_key, result } => {
                if let Some(ed) = self
                    .key_editor
                    .as_mut()
                    .filter(|e| e.editing.as_deref() == Some(access_key.as_str()))
                {
                    match result {
                        Ok(info) => {
                            ed.restrict = info.key.restricted;
                            ed.policy = info.policy.clone();
                            ed.name = info.key.name.clone();
                            ed.description = info.key.description.clone();
                            ed.enabled = info.key.enabled();
                            if let Some(t) = info.key.expiration {
                                let local = t.with_timezone(&Local);
                                ed.expires = true;
                                ed.expiry_date = local.format("%Y-%m-%d").to_string();
                                ed.expiry_time = local.format("%H:%M").to_string();
                            }
                            ed.original = Some(info);
                        }
                        Err(e) => ed.load_error = Some(e),
                    }
                }
            }
            KeysMsg::Created(result) => {
                let name = self
                    .key_editor
                    .as_ref()
                    .map(|e| e.name.trim().to_owned())
                    .unwrap_or_default();
                match result {
                    Ok((access_key, secret_key)) => {
                        self.key_editor = None;
                        self.log(format!("Created access key {access_key}"), false);
                        self.created_key = Some(CreatedKey {
                            access_key,
                            secret_key,
                            name,
                        });
                        self.load_access_keys();
                    }
                    Err(e) => {
                        if let Some(ed) = self.key_editor.as_mut() {
                            ed.saving = false;
                            ed.error = e;
                        }
                    }
                }
            }
            KeysMsg::Changed(result) => {
                let editor_open = self.key_editor.is_some();
                match &result {
                    Ok(m) => self.log(m.clone(), false),
                    Err(e) => self.log(e.clone(), true),
                }
                if editor_open {
                    match result {
                        Ok(m) => {
                            self.key_editor = None;
                            self.set_keys_notice(m, false);
                        }
                        Err(e) => {
                            if let Some(ed) = self.key_editor.as_mut() {
                                ed.saving = false;
                                ed.error = e;
                            }
                        }
                    }
                } else {
                    match result {
                        Ok(m) => self.set_keys_notice(m, false),
                        Err(e) => self.set_keys_notice(e, true),
                    }
                }
                if let Some(w) = self.access_keys.as_mut() {
                    w.busy = false;
                }
                self.load_access_keys();
            }
        }
    }

    fn set_keys_notice(&mut self, text: String, error: bool) {
        if let Some(w) = self.access_keys.as_mut() {
            w.notice = text;
            w.notice_error = error;
        }
    }

    fn open_key_editor(&mut self, editing: Option<String>) {
        let Some(w) = self.access_keys.as_ref() else {
            return;
        };
        let account = w.account.clone();
        let user_policy = match &w.list {
            Fetch::Ready(l) => l.policy.clone(),
            _ => String::new(),
        };
        let creating = editing.is_none();
        self.key_editor = Some(KeyEditor {
            access_key: editing.clone().unwrap_or_else(minio::random_access_key),
            secret_key: if creating {
                minio::random_secret_key()
            } else {
                String::new()
            },
            show_secret: false,
            restrict: false,
            policy: user_policy,
            expires: false,
            expiry_date: (Local::now() + chrono::Duration::days(365))
                .format("%Y-%m-%d")
                .to_string(),
            expiry_time: "00:00".into(),
            name: String::new(),
            description: String::new(),
            enabled: true,
            original: None,
            load_error: None,
            saving: false,
            error: String::new(),
            editing: editing.clone(),
        });
        if let Some(access_key) = editing {
            let storage = self.storage.clone();
            self.spawn(async move {
                let result = storage
                    .access_key_info(&account, &access_key)
                    .await
                    .map_err(|e| err_text(&e));
                Msg::Keys(KeysMsg::Info { access_key, result })
            });
        }
    }

    fn save_key(&mut self) {
        let Some(account) = self.access_keys.as_ref().map(|w| w.account.clone()) else {
            return;
        };
        let Some(ed) = self.key_editor.as_mut() else {
            return;
        };
        ed.error.clear();
        let policy = if ed.restrict {
            match minio::check_policy(&ed.policy) {
                Ok(p) => Some(p),
                Err(e) => {
                    ed.error = e;
                    return;
                }
            }
        } else {
            None
        };
        let expiration = if ed.expires {
            match parse_expiry(&ed.expiry_date, &ed.expiry_time) {
                Ok(t) => Some(t),
                Err(e) => {
                    ed.error = e;
                    return;
                }
            }
        } else {
            None
        };
        let storage = self.storage.clone();
        match ed.editing.clone() {
            None => {
                if let Err(e) = minio::check_credentials(ed.access_key.trim(), &ed.secret_key) {
                    ed.error = e;
                    return;
                }
                let key = NewAccessKey {
                    access_key: ed.access_key.trim().to_owned(),
                    secret_key: ed.secret_key.clone(),
                    policy,
                    name: ed.name.clone(),
                    description: ed.description.clone(),
                    expiration,
                };
                ed.saving = true;
                self.spawn(async move {
                    let result = storage
                        .create_access_key(&account, &key)
                        .await
                        .map_err(|e| err_text(&e));
                    Msg::Keys(KeysMsg::Created(result))
                });
            }
            Some(access_key) => {
                let Some(original) = ed.original.clone() else {
                    return;
                };
                if !ed.secret_key.is_empty()
                    && let Err(e) = minio::check_credentials(&access_key, &ed.secret_key)
                {
                    ed.error = e;
                    return;
                }
                if !ed.expires && original.key.expiration.is_some() {
                    ed.error =
                        "An expiry cannot be removed once set; choose a later date instead.".into();
                    return;
                }
                let old = &original.key;
                let update = AccessKeyUpdate {
                    policy: match (&policy, original.key.restricted) {
                        (None, false) => None,
                        (None, true) => Some(None),
                        (Some(p), _) if p == &original.policy => None,
                        (Some(p), _) => Some(Some(p.clone())),
                    },
                    enabled: (ed.enabled != old.enabled()).then_some(ed.enabled),
                    name: (ed.name.trim() != old.name).then(|| ed.name.clone()),
                    description: (ed.description.trim() != old.description)
                        .then(|| ed.description.clone()),
                    expiration: expiration.filter(|t| Some(*t) != old.expiration),
                    secret_key: (!ed.secret_key.is_empty()).then(|| ed.secret_key.clone()),
                };
                if update == AccessKeyUpdate::default() {
                    self.key_editor = None;
                    return;
                }
                ed.saving = true;
                self.spawn(async move {
                    let result = storage
                        .update_access_key(&account, &access_key, &update)
                        .await
                        .map(|()| format!("Saved access key {access_key}"))
                        .map_err(|e| err_text(&e));
                    Msg::Keys(KeysMsg::Changed(result))
                });
            }
        }
    }

    fn set_key_enabled(&mut self, access_key: String, enabled: bool) {
        let Some(w) = self.access_keys.as_mut() else {
            return;
        };
        w.busy = true;
        w.notice.clear();
        let account = w.account.clone();
        let storage = self.storage.clone();
        self.spawn(async move {
            let update = AccessKeyUpdate {
                enabled: Some(enabled),
                ..AccessKeyUpdate::default()
            };
            let result = storage
                .update_access_key(&account, &access_key, &update)
                .await
                .map(|()| {
                    format!(
                        "{} access key {access_key}",
                        if enabled { "Enabled" } else { "Disabled" }
                    )
                })
                .map_err(|e| err_text(&e));
            Msg::Keys(KeysMsg::Changed(result))
        });
    }

    fn delete_key(&mut self, access_key: String) {
        let Some(w) = self.access_keys.as_mut() else {
            return;
        };
        w.busy = true;
        w.notice.clear();
        let account = w.account.clone();
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = storage
                .delete_access_key(&account, &access_key)
                .await
                .map(|()| format!("Deleted access key {access_key}"))
                .map_err(|e| err_text(&e));
            Msg::Keys(KeysMsg::Changed(result))
        });
    }

    /// Opens the account editor filled with a created key's credentials.
    fn add_key_as_account(&mut self, created: &CreatedKey, base: &Account) {
        self.open_editor(None);
        if let Some(editor) = self.editor.as_mut() {
            let name = if created.name.is_empty() {
                format!("{} ({})", base.name, created.access_key)
            } else {
                created.name.clone()
            };
            editor.draft = AccountDraft {
                id: None,
                name,
                access_key_id: created.access_key.clone(),
                secret_access_key: None,
                manual_buckets: Vec::new(),
                notes: String::new(),
                ..AccountDraft::from_account(base)
            };
            editor.buckets_text.clear();
            editor.secret = created.secret_key.clone();
        }
    }

    // ------------------------------------------------------------------ windows

    pub(crate) fn access_keys_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.access_keys.take() else {
            return;
        };
        let mut close = false;
        let mut refresh = false;
        let mut create = false;
        let mut edit: Option<String> = None;
        let mut toggle_enabled: Option<(String, bool)> = None;
        let mut delete: Option<String> = None;
        Modal::new(Id::new("access-keys")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(880.0);
            ui.heading(format!("{} Access keys: {}", icon::KEY, w.account.name));
            let user = match &w.list {
                Fetch::Ready(l) if !l.user.is_empty() => format!("user \"{}\"", l.user),
                _ => "this account's user".to_owned(),
            };
            muted(ui, format!("Access keys of {user} on {}. Applications use them instead of the user's own keys; each key can be disabled, expire or be restricted with its own policy.", w.account.endpoint_host));
            ui.add_space(4.0);
            let selected_key = match &w.list {
                Fetch::Ready(l) => w
                    .selected
                    .as_ref()
                    .and_then(|s| l.keys.iter().find(|k| &k.access_key == s))
                    .cloned(),
                _ => None,
            };
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(720.0);
                    ui.set_height(320.0);
                    egui::Frame::new()
                        .fill(p.list)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            match &w.list {
                                Fetch::Loading => {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label("Loading access keys…");
                                    });
                                }
                                Fetch::Failed(e) => {
                                    ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.danger)).wrap());
                                }
                                Fetch::Ready(list) if list.keys.is_empty() => {
                                    ui.centered_and_justified(|ui| {
                                        muted(ui, "This user has no access keys yet. Click Create to add one.");
                                    });
                                }
                                Fetch::Ready(list) => {
                                    ui.style_mut().interaction.selectable_labels = false;
                                    TableBuilder::new(ui)
                                        .id_salt("access-keys-table")
                                        .striped(true)
                                        .sense(Sense::click())
                                        .auto_shrink(false)
                                        .cell_layout(Layout::left_to_right(Align::Center))
                                        .column(Column::initial(190.0).clip(true))
                                        .column(Column::initial(140.0).clip(true))
                                        .column(Column::initial(120.0).clip(true))
                                        .column(Column::initial(70.0).clip(true))
                                        .column(Column::initial(90.0).clip(true))
                                        .column(Column::remainder().clip(true))
                                        .header(22.0, |mut row| {
                                            for h in ["Access key", "Name", "Expiry", "Status", "Policy", "Description"] {
                                                row.col(|ui| {
                                                    ui.add(egui::Label::new(RichText::new(h).strong()).truncate());
                                                });
                                            }
                                        })
                                        .body(|body| {
                                            body.rows(24.0, list.keys.len(), |mut row| {
                                                let k = &list.keys[row.index()];
                                                row.set_selected(w.selected.as_ref() == Some(&k.access_key));
                                                row.col(|ui| {
                                                    ui.add(egui::Label::new(RichText::new(&k.access_key).monospace()).truncate());
                                                });
                                                row.col(|ui| {
                                                    ui.add(egui::Label::new(&k.name).truncate());
                                                });
                                                row.col(|ui| {
                                                    let text = k.expiration.map_or_else(|| "Never".to_owned(), local_text);
                                                    let expired = k.expiration.is_some_and(|t| t <= Utc::now());
                                                    let color = if expired { p.danger } else { p.dim };
                                                    ui.label(RichText::new(text).color(color));
                                                });
                                                row.col(|ui| {
                                                    if k.enabled() {
                                                        ui.label(RichText::new("Enabled").color(p.success));
                                                    } else {
                                                        ui.label(RichText::new("Disabled").color(p.warning));
                                                    }
                                                });
                                                row.col(|ui| {
                                                    let text = if k.restricted { "Own policy" } else { "User policy" };
                                                    ui.label(RichText::new(text).color(p.dim));
                                                });
                                                row.col(|ui| {
                                                    ui.add(egui::Label::new(RichText::new(&k.description).color(p.dim)).truncate());
                                                });
                                                let resp = row.response();
                                                if resp.clicked() {
                                                    w.selected = Some(k.access_key.clone());
                                                }
                                                if resp.double_clicked() {
                                                    edit = Some(k.access_key.clone());
                                                }
                                            });
                                        });
                                }
                            }
                        });
                });
                ui.vertical(|ui| {
                    let size = [130.0, 24.0];
                    let ready = matches!(w.list, Fetch::Ready(_)) && !w.busy;
                    let has = ready && selected_key.is_some();
                    if ui.add_enabled(ready, Button::new(format!("{} Create…", icon::PLUS)).min_size(size.into())).clicked() {
                        create = true;
                    }
                    if ui.add_enabled(has, Button::new(format!("{} Edit…", icon::PENCIL_SIMPLE)).min_size(size.into())).clicked() {
                        edit = w.selected.clone();
                    }
                    let (label, glyph) = match &selected_key {
                        Some(k) if !k.enabled() => ("Enable", icon::TOGGLE_RIGHT),
                        _ => ("Disable", icon::TOGGLE_LEFT),
                    };
                    if ui.add_enabled(has, Button::new(format!("{glyph} {label}")).min_size(size.into())).clicked()
                        && let Some(k) = &selected_key
                    {
                        toggle_enabled = Some((k.access_key.clone(), !k.enabled()));
                    }
                    if ui.add_enabled(has, Button::new(format!("{} Delete", icon::TRASH)).min_size(size.into())).clicked() {
                        delete = w.selected.clone();
                    }
                    ui.separator();
                    if ui.add_enabled(!w.busy, Button::new(format!("{} Refresh", icon::ARROWS_CLOCKWISE)).min_size(size.into())).clicked() {
                        refresh = true;
                    }
                });
            });
            if w.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Working…");
                });
            }
            if !w.notice.is_empty() {
                let color = if w.notice_error { p.danger } else { p.success };
                ui.add(egui::Label::new(RichText::new(w.notice.as_str()).color(color)).wrap());
            }
            button_row(ui, |ui| {
                if primary_button(ui, "Close", true) {
                    close = true;
                }
            });
        });

        // Delete confirmation.
        if let Some(access_key) = w.confirm_delete.clone() {
            let mut answer: Option<bool> = None;
            Modal::new(Id::new("access-key-delete")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Delete access key");
                ui.label(format!("Delete access key {access_key}? Applications using it lose access immediately. This cannot be undone."));
                button_row(ui, |ui| {
                    if ui
                        .add(Button::new(RichText::new("Delete").color(egui::Color32::WHITE)).fill(theme::palette(ui).danger_fill))
                        .clicked()
                    {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            });
            match answer {
                Some(true) => {
                    w.confirm_delete = None;
                    delete = Some(access_key);
                    w.selected = None;
                }
                Some(false) => w.confirm_delete = None,
                None => {}
            }
        } else if let Some(k) = delete.take() {
            w.confirm_delete = Some(k);
        }

        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape))
            && self.key_editor.is_none()
            && self.created_key.is_none()
            && w.confirm_delete.is_none();
        if !close && !escape {
            self.access_keys = Some(w);
        }
        if refresh {
            self.load_access_keys();
        }
        if create {
            self.open_key_editor(None);
        }
        if let Some(k) = edit {
            self.open_key_editor(Some(k));
        }
        if let Some((k, on)) = toggle_enabled {
            self.set_key_enabled(k, on);
        }
        if let Some(k) = delete {
            self.delete_key(k);
        }
    }

    pub(crate) fn key_editor_window(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.key_editor.take() else {
            return;
        };
        let user_policy = match self.access_keys.as_ref().map(|w| &w.list) {
            Some(Fetch::Ready(l)) => l.policy.clone(),
            _ => String::new(),
        };
        let mut cancel = false;
        let mut save = false;
        let creating = ed.editing.is_none();
        Modal::new(Id::new("access-key-editor")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(940.0);
            ui.heading(if creating {
                format!("{} Create access key", icon::KEY)
            } else {
                format!("{} Edit access key", icon::KEY)
            });
            ui.add_space(4.0);
            let loading = !creating && ed.original.is_none();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(600.0);
                    if let Some(e) = &ed.load_error {
                        ui.add(egui::Label::new(RichText::new(e.as_str()).color(p.danger)).wrap());
                        return;
                    }
                    if loading {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading the key…");
                        });
                        return;
                    }
                    ui.add_enabled_ui(!ed.saving, |ui| {
                        egui::Grid::new("access-key-form")
                            .num_columns(2)
                            .spacing([12.0, 8.0])
                            .min_col_width(110.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Access Key").strong());
                                ui.add_enabled(
                                    creating,
                                    TextEdit::singleline(&mut ed.access_key)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.end_row();
                                ui.label(RichText::new(if creating { "Secret Key" } else { "New Secret Key" }).strong());
                                ui.horizontal(|ui| {
                                    let hint = if creating { "" } else { "leave empty to keep the current secret" };
                                    ui.add(
                                        TextEdit::singleline(&mut ed.secret_key)
                                            .password(!ed.show_secret)
                                            .font(egui::TextStyle::Monospace)
                                            .hint_text(hint)
                                            .desired_width(470.0),
                                    );
                                    let eye = if ed.show_secret { icon::EYE_SLASH } else { icon::EYE };
                                    if ui.button(eye).on_hover_text("Show or hide the secret key").clicked() {
                                        ed.show_secret = !ed.show_secret;
                                    }
                                    if ui.button(icon::ARROWS_CLOCKWISE).on_hover_text("Generate a new random secret key").clicked() {
                                        ed.secret_key = minio::random_secret_key();
                                    }
                                });
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Restrict beyond user policy").strong());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(if ed.restrict { "ON" } else { "OFF" });
                                if toggle(ui, &mut ed.restrict).changed() && ed.restrict && ed.policy.trim().is_empty() {
                                    ed.policy = user_policy.clone();
                                }
                            });
                        });
                        muted(ui, "Give this key its own JSON policy to allow only part of what your user may do. A key never gets access beyond its user.");
                        if ed.restrict {
                            ui.add_space(4.0);
                            ui.label(RichText::new("Policy of this key - edit the JSON to remove permissions").strong());
                            ScrollArea::vertical()
                                .id_salt("access-key-policy")
                                .max_height(260.0)
                                .show(ui, |ui| {
                                    ui.add(
                                        TextEdit::multiline(&mut ed.policy)
                                            .font(egui::TextStyle::Monospace)
                                            .desired_rows(14)
                                            .desired_width(f32::INFINITY)
                                            .code_editor(),
                                    );
                                });
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!user_policy.is_empty(), Button::new("Reset to user policy")).clicked() {
                                    ed.policy = user_policy.clone();
                                }
                                if ui.button("Format").clicked()
                                    && let Ok(text) = minio::check_policy(&ed.policy)
                                {
                                    ed.policy = text;
                                }
                                if ui.button(format!("{} Copy", icon::COPY)).clicked() {
                                    ui.ctx().copy_text(ed.policy.clone());
                                }
                            });
                        }
                        ui.add_space(8.0);
                        egui::Grid::new("access-key-details")
                            .num_columns(2)
                            .spacing([12.0, 8.0])
                            .min_col_width(110.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Expiry").strong());
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut ed.expires, "Expires on");
                                    ui.add_enabled(ed.expires, TextEdit::singleline(&mut ed.expiry_date).hint_text("YYYY-MM-DD").desired_width(100.0));
                                    ui.add_enabled(ed.expires, TextEdit::singleline(&mut ed.expiry_time).hint_text("HH:MM").desired_width(50.0));
                                    muted(ui, "local time");
                                });
                                ui.end_row();
                                ui.label(RichText::new("Name").strong());
                                ui.add(TextEdit::singleline(&mut ed.name).hint_text("Enter a name").desired_width(f32::INFINITY));
                                ui.end_row();
                                ui.label(RichText::new("Description").strong());
                                ui.add(TextEdit::multiline(&mut ed.description).hint_text("What the key is used for").desired_rows(2).desired_width(f32::INFINITY));
                                ui.end_row();
                                if !creating {
                                    ui.label(RichText::new("Status").strong());
                                    ui.checkbox(&mut ed.enabled, "Enabled");
                                    ui.end_row();
                                }
                            });
                    });
                });
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(p.list)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(egui::Margin::same(12))
                    .corner_radius(4)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(format!("{} About access keys", icon::QUESTION)).strong().size(15.0));
                            for (title, text) in HELP {
                                ui.add_space(8.0);
                                ui.label(RichText::new(title).italics().color(p.dim));
                                ui.add(egui::Label::new(text).wrap());
                            }
                        });
                    });
            });
            if ed.saving {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving…");
                });
            }
            if !ed.error.is_empty() {
                ui.add(egui::Label::new(RichText::new(ed.error.as_str()).color(p.danger)).wrap());
            }
            button_row(ui, |ui| {
                let label = if creating { "Create" } else { "Save" };
                let can_save = !ed.saving && !loading && ed.load_error.is_none();
                if primary_button(ui, label, can_save) {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !ed.saving;
        if !cancel && !escape {
            self.key_editor = Some(ed);
        }
        if save {
            self.save_key();
        }
    }

    pub(crate) fn created_key_window(&mut self, ctx: &egui::Context) {
        let Some(created) = self.created_key.take() else {
            return;
        };
        let mut close = false;
        let mut add_account = false;
        Modal::new(Id::new("access-key-created")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(520.0);
            ui.heading(format!("{} Access key created", icon::CHECK_CIRCLE));
            ui.add_space(4.0);
            ui.label(RichText::new("Access Key").strong());
            copy_field(ui, &created.access_key, false);
            ui.label(RichText::new("Secret Key").strong());
            copy_field(ui, &created.secret_key, false);
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new("Copy the secret key now: it is not shown again. You can also add the key as a storage account in this app; its secret is then kept in the Windows Credential Manager.")
                        .color(p.warning),
                )
                .wrap(),
            );
            button_row(ui, |ui| {
                if primary_button(ui, "Close", true) {
                    close = true;
                }
                if ui.button(format!("{} Add as storage account…", icon::PLUS)).clicked() {
                    add_account = true;
                }
                if ui.button(format!("{} Copy both", icon::COPY)).clicked() {
                    ui.ctx().copy_text(format!(
                        "Access Key: {}\nSecret Key: {}",
                        created.access_key, created.secret_key
                    ));
                }
            });
        });
        if add_account {
            if let Some(base) = self.access_keys.as_ref().map(|w| w.account.clone()) {
                self.add_key_as_account(&created, &base);
                self.access_keys = None;
            }
            return;
        }
        if !close {
            self.created_key = Some(created);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_expiry;

    #[test]
    fn expiry_input() {
        assert!(parse_expiry("2999-01-31", "13:45").is_ok());
        assert!(parse_expiry("2999-01-31", "").is_ok());
        assert!(parse_expiry("2000-01-31", "10:00").is_err());
        assert!(parse_expiry("31-01-2999", "10:00").is_err());
        assert!(parse_expiry("2999-01-31", "25:00").is_err());
    }
}
