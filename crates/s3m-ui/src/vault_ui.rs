//! Master password: the Unlock prompt and Tools > Master password.

use egui::{Button, Color32, Id, Key, Modal, RichText, TextEdit};
use egui_phosphor::regular as icon;
use s3m_core::vault;
use uuid::Uuid;

use crate::app::App;
use crate::theme;
use crate::widgets::muted;

/// Unlock prompt state.
#[derive(Default)]
pub struct UnlockState {
    /// Password typed.
    pub password: String,
    /// Wrong password message.
    pub error: String,
}

/// Tools > Master password dialog state.
#[derive(Default)]
pub struct MasterDialog {
    /// Current master password (when one is set).
    pub current: String,
    /// New password.
    pub new1: String,
    /// New password again.
    pub new2: String,
    /// Validation message.
    pub error: String,
}

impl App {
    /// Every secret id that the master password protects.
    fn secret_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.account_list.iter().map(|a| a.id).collect();
        ids.push(s3m_storage::PROXY_SECRET_ID);
        ids
    }

    pub(crate) fn lock(&mut self) {
        self.vault.set_key(None);
        self.storage.clients().invalidate();
        self.log("Locked: secrets need the master password again.", false);
        self.unlock = Some(UnlockState::default());
        self.load_buckets();
    }

    pub(crate) fn open_master_dialog(&mut self) {
        self.master_dialog = Some(MasterDialog::default());
    }

    pub(crate) fn unlock_window(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.unlock.take() else {
            return;
        };
        let mut submit = false;
        let mut later = false;
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        Modal::new(Id::new("unlock-window")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(420.0);
            ui.heading(format!("{} Unlock {}", icon::LOCK_KEY, s3m_core::APP_NAME));
            ui.label("The secret keys are protected with a master password.");
            let r = ui.add(
                TextEdit::singleline(&mut state.password)
                    .password(true)
                    .hint_text("Master password")
                    .desired_width(f32::INFINITY),
            );
            if !r.has_focus() && state.password.is_empty() {
                r.request_focus();
            }
            if !state.error.is_empty() {
                ui.label(RichText::new(state.error.as_str()).color(p.danger));
            }
            crate::dialogs::button_row(ui, |ui| {
                if ui
                    .button("Later")
                    .on_hover_text("Browse without connecting; unlock from Tools")
                    .clicked()
                {
                    later = true;
                }
                if ui
                    .add_enabled(
                        !state.password.is_empty(),
                        Button::new(RichText::new("Unlock").color(Color32::WHITE))
                            .fill(p.accent_fill),
                    )
                    .clicked()
                    || (enter && !state.password.is_empty())
                {
                    submit = true;
                }
            });
        });
        if later {
            return;
        }
        if submit {
            let Some(config) = self.settings.master_password.clone() else {
                return;
            };
            match vault::unlock(&config, &state.password) {
                Ok(key) => {
                    self.vault.set_key(Some(key));
                    self.storage.clients().invalidate();
                    let proxy = self.proxy_settings(&self.settings);
                    self.storage.clients().set_proxy(proxy);
                    self.log("Unlocked.", false);
                    self.load_buckets();
                    return;
                }
                Err(e) => {
                    state.error = e.0;
                    state.password.clear();
                }
            }
        }
        self.unlock = Some(state);
    }

    pub(crate) fn master_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.master_dialog.take() else {
            return;
        };
        let configured = self.settings.master_password.is_some();
        enum Choice {
            None,
            Close,
            Save,
            Remove,
        }
        let mut choice = Choice::None;
        Modal::new(Id::new("master-window")).show(ctx, |ui| {
            let p = theme::palette(ui);
            ui.set_width(460.0);
            ui.heading(format!("{} Master password", icon::LOCK_KEY));
            muted(
                ui,
                "With a master password, secret keys are encrypted again with a key derived from it before they go to the Windows Credential Manager. It is asked for every time the app starts. If you forget it, the secret keys must be entered again.",
            );
            ui.add_space(6.0);
            egui::Grid::new("master-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                if configured {
                    ui.label("Current password:");
                    ui.add(TextEdit::singleline(&mut d.current).password(true).desired_width(240.0));
                    ui.end_row();
                }
                ui.label(if configured { "New password:" } else { "Password:" });
                ui.add(TextEdit::singleline(&mut d.new1).password(true).desired_width(240.0));
                ui.end_row();
                ui.label("Repeat:");
                ui.add(TextEdit::singleline(&mut d.new2).password(true).desired_width(240.0));
                ui.end_row();
            });
            if !d.error.is_empty() {
                ui.label(RichText::new(d.error.as_str()).color(p.danger));
            }
            crate::dialogs::button_row(ui, |ui| {
                if ui.button("Close").clicked() {
                    choice = Choice::Close;
                }
                let label = if configured { "Change password" } else { "Set password" };
                if ui
                    .add(Button::new(RichText::new(label).color(Color32::WHITE)).fill(p.accent_fill))
                    .clicked()
                {
                    choice = Choice::Save;
                }
                if configured && ui.button("Remove master password").clicked() {
                    choice = Choice::Remove;
                }
            });
        });
        match choice {
            Choice::Close => return,
            Choice::None => {}
            Choice::Save => {
                if let Err(e) = self.change_master(&d, configured) {
                    d.error = e;
                } else {
                    return;
                }
            }
            Choice::Remove => {
                if let Err(e) = self.remove_master(&d) {
                    d.error = e;
                } else {
                    return;
                }
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            return;
        }
        self.master_dialog = Some(d);
    }

    fn change_master(&mut self, d: &MasterDialog, configured: bool) -> Result<(), String> {
        if configured {
            let config = self
                .settings
                .master_password
                .clone()
                .ok_or("No master password is set.")?;
            vault::unlock(&config, &d.current).map_err(|e| e.0)?;
        }
        if d.new1 != d.new2 {
            return Err("The new passwords do not match.".into());
        }
        let (config, key) = vault::create(&d.new1).map_err(|e| e.0)?;
        self.vault
            .rekey(&self.secret_ids(), Some(key))
            .map_err(|e| e.0)?;
        let mut settings = self.settings.clone();
        settings.master_password = Some(config);
        self.persist_settings(settings)?;
        self.storage.clients().invalidate();
        self.log(
            if configured {
                "Master password changed."
            } else {
                "Master password set."
            },
            false,
        );
        Ok(())
    }

    fn remove_master(&mut self, d: &MasterDialog) -> Result<(), String> {
        let config = self
            .settings
            .master_password
            .clone()
            .ok_or("No master password is set.")?;
        vault::unlock(&config, &d.current).map_err(|e| e.0)?;
        self.vault
            .rekey(&self.secret_ids(), None)
            .map_err(|e| e.0)?;
        let mut settings = self.settings.clone();
        settings.master_password = None;
        self.persist_settings(settings)?;
        self.storage.clients().invalidate();
        self.log("Master password removed.", false);
        Ok(())
    }
}
