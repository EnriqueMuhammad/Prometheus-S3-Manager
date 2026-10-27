//! Phase 3 UI: Versions and Permissions tabs, and the Bucket settings window
//! (versioning, default encryption, policy, CORS, lifecycle, bucket ACL).

use egui::{Align, Button, Id, Layout, Modal, RichText, ScrollArea, TextEdit, Ui};
use egui_extras::{Column, TableBuilder};
use egui_phosphor::regular as icon;
use s3m_core::admin::{self, Acl, AclGrant, AclMatrix, Encryption};
use s3m_core::{ObjectVersion, TransferDirection, TransferRequest, format};
use s3m_s3::{AclTarget, StorageError};

use crate::app::{Action, App};
use crate::model::{ConfirmAction, Modal as AppModal, Msg};
use crate::theme;
use crate::widgets::muted;

/// Loading state of something fetched for the focused object.
pub enum Fetch<T> {
    /// Request in flight.
    Loading,
    /// Loaded.
    Ready(T),
    /// Failed; the message.
    Failed(String),
}

/// ACL being edited (object Permissions tab or bucket settings).
pub struct AclEdit {
    /// Owner id.
    pub owner_id: String,
    /// Owner name.
    pub owner_name: Option<String>,
    /// Matrix being edited.
    pub matrix: AclMatrix,
    /// Grants outside the matrix (kept as they are).
    pub others: Vec<AclGrant>,
    /// Matrix as loaded, to know whether there are changes.
    pub original: AclMatrix,
}

impl AclEdit {
    fn new(acl: &Acl) -> Self {
        let (matrix, others) = acl.matrix();
        Self {
            owner_id: acl.owner_id.clone(),
            owner_name: acl.owner_name.clone(),
            matrix,
            others,
            original: matrix,
        }
    }

    fn to_acl(&self) -> Acl {
        Acl::from_matrix(
            &self.owner_id,
            self.owner_name.clone(),
            &self.matrix,
            &self.others,
        )
    }
}

/// Tabs of the Bucket settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    /// Versioning and default encryption.
    General,
    /// Bucket policy JSON.
    Policy,
    /// CORS rules JSON.
    Cors,
    /// Lifecycle rules JSON.
    Lifecycle,
    /// Bucket ACL.
    Permissions,
}

/// Everything loaded for the Bucket settings window.
pub struct SettingsData {
    /// Versioning status.
    pub versioning: Result<Option<String>, String>,
    /// Default encryption.
    pub encryption: Result<Encryption, String>,
    /// Policy (None = no policy).
    pub policy: Result<Option<String>, String>,
    /// CORS rules.
    pub cors: Result<Vec<admin::CorsRule>, String>,
    /// Lifecycle rules.
    pub lifecycle: Result<Vec<admin::LifecycleRule>, String>,
    /// Bucket ACL.
    pub acl: Result<Acl, String>,
}

/// Bucket settings window state.
pub struct BucketSettings {
    /// Bucket.
    pub bucket: String,
    /// Open tab.
    pub tab: SettingsTab,
    /// Loaded data (None while loading).
    pub loaded: bool,
    /// Versioning status as reported.
    pub versioning: Result<Option<String>, String>,
    /// Encryption choice: 0 none, 1 SSE-S3, 2 SSE-KMS.
    pub encryption_kind: usize,
    /// KMS key id.
    pub kms_key: String,
    /// Encryption load error.
    pub encryption_error: Option<String>,
    /// Policy text.
    pub policy: String,
    /// Policy load error.
    pub policy_error: Option<String>,
    /// CORS JSON.
    pub cors: String,
    /// CORS load error.
    pub cors_error: Option<String>,
    /// Lifecycle JSON.
    pub lifecycle: String,
    /// Lifecycle load error.
    pub lifecycle_error: Option<String>,
    /// Bucket ACL.
    pub acl: Fetch<AclEdit>,
    /// Validation or save result message.
    pub notice: String,
    /// The notice is an error.
    pub notice_error: bool,
    /// A save is in flight.
    pub saving: bool,
}

/// What a bucket settings save changes.
pub enum SettingsSave {
    /// Enable or suspend versioning.
    Versioning(bool),
    /// Default encryption.
    Encryption(Encryption),
    /// Policy (None deletes it).
    Policy(Option<String>),
    /// CORS rules (empty deletes them).
    Cors(Vec<admin::CorsRule>),
    /// Lifecycle rules (empty deletes them).
    Lifecycle(Vec<admin::LifecycleRule>),
    /// Bucket ACL.
    Acl(Acl),
}

fn err(e: &StorageError) -> String {
    e.display()
}

fn version_file_name(key: &str, version: &str) -> String {
    let name = s3m_core::browse::leaf_name(key);
    let short: String = version.chars().take(8).collect();
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} (version {short}).{ext}"),
        _ => format!("{name} (version {short})"),
    }
}

impl App {
    // ------------------------------------------------------------------ versions

    pub(crate) fn ensure_versions(&mut self) {
        let Some(key) = self.focused.clone().filter(|k| !k.ends_with('/')) else {
            self.versions = None;
            return;
        };
        if self.versions.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        self.versions = Some((key.clone(), Fetch::Loading));
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Versions {
                result: storage.list_versions(&account, &bucket, &key).await,
                key,
            }
        });
    }

    pub(crate) fn handle_versions(
        &mut self,
        key: String,
        result: Result<Vec<ObjectVersion>, StorageError>,
    ) {
        if self.versions.as_ref().is_some_and(|(k, _)| *k == key) {
            self.versions = Some((
                key,
                match result {
                    Ok(v) => Fetch::Ready(v),
                    Err(e) => Fetch::Failed(err(&e)),
                },
            ));
        }
    }

    pub(crate) fn versions_tab(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        let Some((key, state)) = &self.versions else {
            ui.label(RichText::new("Select a file to see its versions.").color(p.dim));
            return;
        };
        let versions = match state {
            Fetch::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading versions…");
                });
                return;
            }
            Fetch::Failed(e) => {
                ui.label(RichText::new(e).color(p.danger));
                return;
            }
            Fetch::Ready(v) => v,
        };
        if versions.len() <= 1 && versions.iter().all(|v| v.version_id == "null") {
            muted(
                ui,
                "Versioning is not enabled for this bucket, so there is only the current version. Enable it in Buckets > Bucket settings.",
            );
        }
        TableBuilder::new(ui)
            .id_salt("versions-table")
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(300.0).clip(true))
            .column(Column::initial(150.0))
            .column(Column::initial(90.0))
            .column(Column::initial(110.0))
            .column(Column::remainder().at_least(220.0))
            .header(22.0, |mut row| {
                for h in ["Version", "Modified", "Size", "State", ""] {
                    row.col(|ui| {
                        ui.label(RichText::new(h).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, versions.len(), |mut row| {
                    let v = &versions[row.index()];
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&v.version_id).monospace()).truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.label(
                            v.last_modified
                                .map(|t| {
                                    t.with_timezone(&chrono::Local)
                                        .format("%Y-%m-%d %H:%M:%S")
                                        .to_string()
                                })
                                .unwrap_or_default(),
                        );
                    });
                    row.col(|ui| {
                        if !v.delete_marker {
                            ui.label(format::size(v.size));
                        }
                    });
                    row.col(|ui| {
                        let (text, color) = match (v.is_latest, v.delete_marker) {
                            (true, true) => ("Deleted (latest)", p.danger),
                            (false, true) => ("Delete marker", p.dim),
                            (true, false) => ("Current", p.success),
                            (false, false) => ("Previous", p.dim),
                        };
                        ui.label(RichText::new(text).color(color));
                    });
                    row.col(|ui| {
                        if !v.delete_marker {
                            if ui
                                .small_button(format!("{} Download", icon::DOWNLOAD_SIMPLE))
                                .clicked()
                            {
                                actions.push(Action::DownloadVersion(
                                    key.clone(),
                                    v.version_id.clone(),
                                ));
                            }
                            if !v.is_latest
                                && ui
                                    .small_button(format!("{} Restore", icon::ARROW_U_UP_LEFT))
                                    .clicked()
                            {
                                actions.push(Action::RestoreVersion(
                                    key.clone(),
                                    v.version_id.clone(),
                                ));
                            }
                        }
                        let label = if v.delete_marker {
                            "Remove marker"
                        } else {
                            "Delete"
                        };
                        if ui
                            .small_button(format!("{} {label}", icon::TRASH))
                            .clicked()
                        {
                            actions.push(Action::DeleteVersion(
                                key.clone(),
                                v.version_id.clone(),
                                v.delete_marker,
                            ));
                        }
                    });
                });
            });
    }

    pub(crate) fn download_version(&mut self, key: String, version: String) {
        let (Some(account_id), Some(bucket)) = (self.current_account, self.current_bucket.clone())
        else {
            return;
        };
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Download version to")
            .pick_folder()
        else {
            return;
        };
        let local_path = folder.join(version_file_name(&key, &version));
        let size = match &self.versions {
            Some((_, Fetch::Ready(v))) => v
                .iter()
                .find(|x| x.version_id == version)
                .map_or(0, |x| x.size),
            _ => 0,
        };
        self.enqueue(vec![TransferRequest {
            account_id,
            direction: TransferDirection::Download,
            bucket,
            key,
            local_path,
            size_hint: size,
            source: None,
            version_id: (version != "null").then_some(version),
        }]);
    }

    pub(crate) fn restore_version(&mut self, key: String, version: String) {
        let (Some(account_id), Some(bucket)) = (self.current_account, self.current_bucket.clone())
        else {
            return;
        };
        self.log(format!("Restoring {key} to version {version}"), false);
        self.enqueue(vec![TransferRequest {
            account_id,
            direction: TransferDirection::Copy,
            bucket: bucket.clone(),
            key: key.clone(),
            local_path: std::path::PathBuf::new(),
            size_hint: 0,
            source: Some(s3m_core::ObjectRef {
                bucket,
                key,
                account_id: None,
            }),
            version_id: Some(version),
        }]);
        self.versions = None;
    }

    pub(crate) fn confirm_delete_version(&mut self, key: String, version: String, marker: bool) {
        let body = if marker {
            format!(
                "Remove this delete marker of \"{key}\"?\n\nThe previous version becomes the current one again."
            )
        } else {
            format!(
                "Permanently delete version {version} of \"{key}\"?\n\nThis version cannot be recovered afterwards."
            )
        };
        self.modal = Some(AppModal::Confirm {
            title: if marker {
                "Remove delete marker"
            } else {
                "Delete version"
            }
            .into(),
            body,
            ok: if marker { "Remove" } else { "Delete version" }.into(),
            alternative: None,
            action: ConfirmAction::DeleteVersion { key, version },
            danger: !marker,
        });
    }

    pub(crate) fn delete_version(&mut self, key: String, version: String) {
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        self.versions = None;
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = storage
                .delete_version(&account, &bucket, &key, &version)
                .await
                .map(|()| format!("Deleted version {version} of {key}"));
            Msg::Done {
                title: "Couldn't delete the version".into(),
                result,
                reload: crate::model::Reload::Objects,
            }
        });
    }

    // ------------------------------------------------------------------ object permissions

    pub(crate) fn ensure_acl(&mut self) {
        let Some(key) = self.focused.clone().filter(|k| !k.ends_with('/')) else {
            self.acl = None;
            return;
        };
        if self.acl.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        self.acl = Some((key.clone(), Fetch::Loading));
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Acl {
                result: storage
                    .get_acl(&account, &bucket, AclTarget::Object(&key))
                    .await,
                key,
            }
        });
    }

    pub(crate) fn handle_acl(&mut self, key: String, result: Result<Acl, StorageError>) {
        if self.acl.as_ref().is_some_and(|(k, _)| *k == key) {
            self.acl = Some((
                key,
                match result {
                    Ok(a) => Fetch::Ready(AclEdit::new(&a)),
                    Err(e) => Fetch::Failed(err(&e)),
                },
            ));
        }
    }

    pub(crate) fn permissions_tab(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let p = theme::palette(ui);
        let Some((key, state)) = self.acl.as_mut() else {
            ui.label(RichText::new("Select a file to see who may access it.").color(p.dim));
            return;
        };
        match state {
            Fetch::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading permissions…");
                });
            }
            Fetch::Failed(e) => {
                ui.label(RichText::new(e.as_str()).color(p.danger));
                muted(
                    ui,
                    "Buckets with Object Ownership \"bucket owner enforced\" and many S3-compatible servers do not use ACLs; use a bucket policy instead.",
                );
            }
            Fetch::Ready(edit) => {
                if acl_matrix_ui(ui, edit, "object-acl") {
                    actions.push(Action::SaveObjectAcl(key.clone()));
                }
            }
        }
    }

    pub(crate) fn save_object_acl(&mut self, key: String) {
        let Some((k, Fetch::Ready(edit))) = &self.acl else {
            return;
        };
        if *k != key {
            return;
        }
        let acl = edit.to_acl();
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        self.acl = None;
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = storage
                .set_acl(&account, &bucket, AclTarget::Object(&key), &acl)
                .await
                .map(|()| format!("Updated permissions of {key}"));
            Msg::Done {
                title: "Couldn't update the permissions".into(),
                result,
                reload: crate::model::Reload::Objects,
            }
        });
    }

    // ------------------------------------------------------------------ bucket settings

    pub(crate) fn open_bucket_settings(&mut self, bucket: String) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        self.bucket_settings = Some(BucketSettings {
            bucket: bucket.clone(),
            tab: SettingsTab::General,
            loaded: false,
            versioning: Ok(None),
            encryption_kind: 0,
            kms_key: String::new(),
            encryption_error: None,
            policy: String::new(),
            policy_error: None,
            cors: String::new(),
            cors_error: None,
            lifecycle: String::new(),
            lifecycle_error: None,
            acl: Fetch::Loading,
            notice: String::new(),
            notice_error: false,
            saving: false,
        });
        let storage = self.storage.clone();
        self.spawn(async move {
            let versioning = storage
                .bucket_info(&account, &bucket)
                .await
                .map(|i| i.versioning)
                .map_err(|e| err(&e));
            let data = SettingsData {
                versioning,
                encryption: storage
                    .get_encryption(&account, &bucket)
                    .await
                    .map_err(|e| err(&e)),
                policy: storage
                    .get_policy(&account, &bucket)
                    .await
                    .map_err(|e| err(&e)),
                cors: storage
                    .get_cors(&account, &bucket)
                    .await
                    .map_err(|e| err(&e)),
                lifecycle: storage
                    .get_lifecycle(&account, &bucket)
                    .await
                    .map_err(|e| err(&e)),
                acl: storage
                    .get_acl(&account, &bucket, AclTarget::Bucket)
                    .await
                    .map_err(|e| err(&e)),
            };
            Msg::BucketSettings {
                bucket,
                data: Box::new(data),
            }
        });
    }

    pub(crate) fn handle_bucket_settings(&mut self, bucket: &str, data: SettingsData) {
        let Some(w) = self.bucket_settings.as_mut().filter(|w| w.bucket == bucket) else {
            return;
        };
        w.loaded = true;
        w.versioning = data.versioning;
        match data.encryption {
            Ok(Encryption::None) => w.encryption_kind = 0,
            Ok(Encryption::Aes256) => w.encryption_kind = 1,
            Ok(Encryption::Kms(k)) => {
                w.encryption_kind = 2;
                w.kms_key = k;
            }
            Err(e) => w.encryption_error = Some(e),
        }
        match data.policy {
            Ok(Some(p)) => w.policy = admin::check_policy(&p).unwrap_or(p),
            Ok(None) => w.policy = String::new(),
            Err(e) => w.policy_error = Some(e),
        }
        match data.cors {
            Ok(r) => {
                w.cors = if r.is_empty() {
                    String::new()
                } else {
                    admin::to_json(&r)
                }
            }
            Err(e) => w.cors_error = Some(e),
        }
        match data.lifecycle {
            Ok(r) => {
                w.lifecycle = if r.is_empty() {
                    String::new()
                } else {
                    admin::to_json(&r)
                }
            }
            Err(e) => w.lifecycle_error = Some(e),
        }
        w.acl = match data.acl {
            Ok(a) => Fetch::Ready(AclEdit::new(&a)),
            Err(e) => Fetch::Failed(e),
        };
    }

    fn save_bucket_setting(&mut self, save: SettingsSave) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        let Some(w) = self.bucket_settings.as_mut() else {
            return;
        };
        w.saving = true;
        w.notice.clear();
        let bucket = w.bucket.clone();
        let storage = self.storage.clone();
        self.spawn(async move {
            let (what, result) = match &save {
                SettingsSave::Versioning(on) => (
                    if *on {
                        "Versioning enabled"
                    } else {
                        "Versioning suspended"
                    },
                    storage.set_versioning(&account, &bucket, *on).await,
                ),
                SettingsSave::Encryption(e) => (
                    "Default encryption saved",
                    storage.set_encryption(&account, &bucket, e).await,
                ),
                SettingsSave::Policy(p) => (
                    if p.is_some() {
                        "Bucket policy saved"
                    } else {
                        "Bucket policy deleted"
                    },
                    storage.set_policy(&account, &bucket, p.as_deref()).await,
                ),
                SettingsSave::Cors(r) => (
                    if r.is_empty() {
                        "CORS rules deleted"
                    } else {
                        "CORS rules saved"
                    },
                    storage.set_cors(&account, &bucket, r).await,
                ),
                SettingsSave::Lifecycle(r) => (
                    if r.is_empty() {
                        "Lifecycle rules deleted"
                    } else {
                        "Lifecycle rules saved"
                    },
                    storage.set_lifecycle(&account, &bucket, r).await,
                ),
                SettingsSave::Acl(a) => (
                    "Bucket permissions saved",
                    storage
                        .set_acl(&account, &bucket, AclTarget::Bucket, a)
                        .await,
                ),
            };
            Msg::SettingsSaved {
                bucket,
                result: result.map(|()| what.to_owned()).map_err(|e| err(&e)),
                reload: matches!(save, SettingsSave::Versioning(_)),
            }
        });
    }

    pub(crate) fn handle_settings_saved(
        &mut self,
        bucket: &str,
        result: Result<String, String>,
        reload: bool,
    ) {
        let ok = result.is_ok();
        match &result {
            Ok(m) => self.log(format!("{bucket}: {m}"), false),
            Err(e) => self.log(format!("{bucket}: {e}"), true),
        }
        if let Some(w) = self.bucket_settings.as_mut().filter(|w| w.bucket == bucket) {
            w.saving = false;
            w.notice_error = !ok;
            w.notice = match result {
                Ok(m) => m,
                Err(e) => e,
            };
        }
        if ok && reload {
            self.open_bucket_settings(bucket.to_owned());
        }
    }

    pub(crate) fn bucket_settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.bucket_settings.take() else {
            return;
        };
        let mut close = false;
        let mut save: Option<SettingsSave> = None;
        Modal::new(Id::new("bucket-settings")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(720.0);
            ui.heading(format!("{} Bucket settings: {}", icon::GEAR, w.bucket));
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (SettingsTab::General, "Versioning & encryption"),
                    (SettingsTab::Policy, "Policy"),
                    (SettingsTab::Cors, "CORS"),
                    (SettingsTab::Lifecycle, "Lifecycle"),
                    (SettingsTab::Permissions, "Permissions"),
                ] {
                    if ui.selectable_label(w.tab == tab, label).clicked() {
                        w.tab = tab;
                        w.notice.clear();
                    }
                }
            });
            ui.separator();
            if !w.loaded {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading bucket settings…");
                });
            } else {
                ui.set_min_height(320.0);
                match w.tab {
                    SettingsTab::General => {
                        ui.label(RichText::new("Versioning").strong());
                        match &w.versioning {
                            Ok(status) => {
                                let status_text = status.clone().unwrap_or_else(|| "Unknown".into());
                                ui.label(format!("Status: {status_text}"));
                                ui.horizontal(|ui| {
                                    let enabled = status.as_deref() == Some("Enabled");
                                    if ui.add_enabled(!enabled && !w.saving, Button::new("Enable versioning")).clicked() {
                                        save = Some(SettingsSave::Versioning(true));
                                    }
                                    if ui.add_enabled(enabled && !w.saving, Button::new("Suspend versioning")).clicked() {
                                        save = Some(SettingsSave::Versioning(false));
                                    }
                                });
                                muted(ui, "With versioning, overwritten and deleted files keep their previous versions (see the Versions tab). Versioning can be suspended but never fully turned off.");
                            }
                            Err(e) => {
                                ui.label(RichText::new(e.as_str()).color(p.danger));
                            }
                        }
                        ui.add_space(10.0);
                        ui.label(RichText::new("Default encryption").strong());
                        if let Some(e) = &w.encryption_error {
                            ui.label(RichText::new(e.as_str()).color(p.danger));
                        }
                        ui.radio_value(&mut w.encryption_kind, 0, "None");
                        ui.radio_value(&mut w.encryption_kind, 1, "SSE-S3 (AES-256, keys managed by the server)");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut w.encryption_kind, 2, "SSE-KMS, key:");
                            ui.add_enabled(
                                w.encryption_kind == 2,
                                TextEdit::singleline(&mut w.kms_key).hint_text("key id or ARN (empty = default key)").desired_width(320.0),
                            );
                        });
                        if ui.add_enabled(!w.saving, Button::new("Save encryption")).clicked() {
                            save = Some(SettingsSave::Encryption(match w.encryption_kind {
                                1 => Encryption::Aes256,
                                2 => Encryption::Kms(w.kms_key.clone()),
                                _ => Encryption::None,
                            }));
                        }
                    }
                    SettingsTab::Policy => {
                        if let Some(e) = &w.policy_error {
                            ui.label(RichText::new(e.as_str()).color(p.danger));
                        }
                        muted(ui, "JSON bucket policy. Leave empty and save to delete the policy.");
                        json_editor(ui, &mut w.policy, "policy-text");
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!w.saving, Button::new("Save policy")).clicked() {
                                if w.policy.trim().is_empty() {
                                    save = Some(SettingsSave::Policy(None));
                                } else {
                                    match admin::check_policy(&w.policy) {
                                        Ok(text) => save = Some(SettingsSave::Policy(Some(text))),
                                        Err(e) => {
                                            w.notice = e;
                                            w.notice_error = true;
                                        }
                                    }
                                }
                            }
                            if ui.button("Insert example").clicked() {
                                w.policy = admin::policy_template(&w.bucket);
                            }
                        });
                    }
                    SettingsTab::Cors => {
                        if let Some(e) = &w.cors_error {
                            ui.label(RichText::new(e.as_str()).color(p.danger));
                        }
                        muted(ui, "JSON array of CORS rules (AllowedOrigins, AllowedMethods, AllowedHeaders, ExposeHeaders, MaxAgeSeconds). Leave empty and save to delete them.");
                        json_editor(ui, &mut w.cors, "cors-text");
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!w.saving, Button::new("Save CORS")).clicked() {
                                let text = if w.cors.trim().is_empty() { "[]" } else { w.cors.as_str() };
                                match admin::parse_cors(text) {
                                    Ok(r) => save = Some(SettingsSave::Cors(r)),
                                    Err(e) => {
                                        w.notice = e;
                                        w.notice_error = true;
                                    }
                                }
                            }
                            if ui.button("Insert example").clicked() {
                                w.cors = admin::CORS_TEMPLATE.into();
                            }
                        });
                    }
                    SettingsTab::Lifecycle => {
                        if let Some(e) = &w.lifecycle_error {
                            ui.label(RichText::new(e.as_str()).color(p.danger));
                        }
                        muted(ui, "JSON array of rules (Id, Enabled, Prefix, ExpirationDays, NoncurrentExpirationDays, AbortIncompleteUploadDays, TransitionDays + TransitionStorageClass). Leave empty and save to delete them.");
                        json_editor(ui, &mut w.lifecycle, "lifecycle-text");
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!w.saving, Button::new("Save lifecycle")).clicked() {
                                let text = if w.lifecycle.trim().is_empty() { "[]" } else { w.lifecycle.as_str() };
                                match admin::parse_lifecycle(text) {
                                    Ok(r) => save = Some(SettingsSave::Lifecycle(r)),
                                    Err(e) => {
                                        w.notice = e;
                                        w.notice_error = true;
                                    }
                                }
                            }
                            if ui.button("Insert example").clicked() {
                                w.lifecycle = admin::LIFECYCLE_TEMPLATE.into();
                            }
                        });
                    }
                    SettingsTab::Permissions => match &mut w.acl {
                        Fetch::Loading => {
                            ui.spinner();
                        }
                        Fetch::Failed(e) => {
                            ui.label(RichText::new(e.as_str()).color(p.danger));
                            muted(ui, "This bucket or server does not use ACLs; use the bucket policy instead.");
                        }
                        Fetch::Ready(edit) => {
                            if acl_matrix_ui(ui, edit, "bucket-acl") {
                                save = Some(SettingsSave::Acl(edit.to_acl()));
                            }
                        }
                    },
                }
            }
            if w.saving {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving…");
                });
            }
            if !w.notice.is_empty() {
                let color = if w.notice_error { p.danger } else { p.success };
                ui.add(egui::Label::new(RichText::new(w.notice.as_str()).color(color)).wrap());
            }
            crate::dialogs::button_row(ui, |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if !close && !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.bucket_settings = Some(w);
        }
        if let Some(save) = save {
            self.save_bucket_setting(save);
        }
    }
}

fn json_editor(ui: &mut Ui, text: &mut String, id: &str) {
    ScrollArea::vertical()
        .id_salt(id)
        .max_height(260.0)
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(text)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(14)
                    .desired_width(f32::INFINITY)
                    .code_editor(),
            );
        });
}

/// The permission matrix; returns `true` when Save was clicked.
fn acl_matrix_ui(ui: &mut Ui, edit: &mut AclEdit, id: &str) -> bool {
    let p = theme::palette(ui);
    let owner = edit
        .owner_name
        .clone()
        .unwrap_or_else(|| edit.owner_id.clone());
    egui::Grid::new(id)
        .num_columns(5)
        .striped(true)
        .spacing([18.0, 6.0])
        .show(ui, |ui| {
            ui.label("");
            for h in ["Read", "Write", "Read permissions", "Write permissions"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for (label, row) in [
                (format!("Owner ({owner})"), &mut edit.matrix.owner),
                (
                    "Authenticated users (any AWS account)".to_owned(),
                    &mut edit.matrix.authenticated,
                ),
                ("All users (public)".to_owned(), &mut edit.matrix.everyone),
            ] {
                ui.label(label);
                for cell in row.iter_mut() {
                    ui.checkbox(cell, "");
                }
                ui.end_row();
            }
        });
    if edit.matrix.everyone.iter().any(|x| *x) {
        ui.label(
            RichText::new(format!(
                "{} Everyone on the internet gets these rights.",
                icon::WARNING
            ))
            .color(p.warning),
        );
    }
    if !edit.others.is_empty() {
        ui.label(RichText::new("Other grants (kept unchanged):").strong());
        for g in &edit.others {
            ui.label(format!(
                "{} {} — {}",
                g.kind,
                g.display_name.clone().unwrap_or_else(|| g.grantee.clone()),
                g.permission
            ));
        }
    }
    let changed = edit.matrix != edit.original;
    ui.add_enabled(changed, Button::new("Save permissions"))
        .clicked()
}
