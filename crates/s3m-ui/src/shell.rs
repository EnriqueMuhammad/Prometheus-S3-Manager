//! Main window: menu bar, toolbar, status bar, panels, shortcuts and drag-and-drop.

use egui::{
    Align, Align2, Button, CentralPanel, Color32, FontId, Frame, Id, Key, KeyboardShortcut, Layout,
    Margin, Modifiers, RichText, Ui,
};
use egui_phosphor::regular as icon;
use s3m_core::format;

use crate::app::{Action, App};
use crate::theme;
use crate::widgets::{toolbar_button, toolbar_separator};

const SC_REFRESH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);
const SC_NEW_FOLDER: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SC_UPLOAD: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::U);
const SC_DOWNLOAD: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::D);
const SC_FILTER: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SC_SELECT_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::A);
const SC_RENAME: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F2);
const SC_DELETE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Delete);
const SC_UP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Backspace);
const SC_ABOUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F1);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.window_fit.update(&ctx);
        self.pump();
        let mut actions = Vec::new();
        self.shortcuts(&ctx, &mut actions);
        self.dropped_files(&ctx, &mut actions);

        let p = theme::palette(ui);
        let bar = |m: Margin| Frame::new().fill(p.panel).inner_margin(m);
        egui::Panel::top(Id::new("menu-bar"))
            .frame(bar(Margin::symmetric(6, 2)))
            .show(ui, |ui| self.menu_bar(ui, &mut actions));
        egui::Panel::top(Id::new("toolbar"))
            .frame(bar(Margin::symmetric(8, 2)))
            .show(ui, |ui| self.toolbar(ui, &mut actions));
        egui::Panel::bottom(Id::new("status-bar"))
            .exact_size(24.0)
            .frame(bar(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom(Id::new("bottom-panel"))
            .resizable(true)
            .default_size(210.0)
            .size_range(90.0..=600.0)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(6, 4)),
            )
            .show(ui, |ui| self.bottom_panel(ui, &mut actions));
        egui::Panel::left(Id::new("buckets"))
            .resizable(true)
            .default_size(250.0)
            .size_range(160.0..=520.0)
            .frame(Frame::new().fill(p.panel).inner_margin(Margin::same(6)))
            .show(ui, |ui| self.buckets_panel(ui, &mut actions));
        CentralPanel::default()
            .frame(Frame::new().fill(p.panel).inner_margin(Margin::same(6)))
            .show(ui, |ui| self.files_panel(ui, &mut actions));

        self.drop_overlay(&ctx);
        self.accounts_window(&ctx, &mut actions);
        self.editor_window(&ctx);
        self.options_window(&ctx);
        self.bucket_settings_window(&ctx);
        self.access_keys_window(&ctx);
        self.key_editor_window(&ctx);
        self.created_key_window(&ctx);
        self.sync_window(&ctx);
        self.master_window(&ctx);
        self.unlock_window(&ctx);
        self.about_window(&ctx, &mut actions);
        self.update_window(&ctx);
        self.modal_window(&ctx);
        self.apply(&ctx, actions);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(state) = self.window_fit.state() {
            eframe::set_value(storage, crate::window::EFRAME_WINDOW_KEY, &state);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

impl App {
    fn dialogs_open(&self) -> bool {
        self.modal.is_some()
            || self.editor.is_some()
            || self.accounts_window.is_some()
            || self.options.is_some()
            || self.bucket_settings.is_some()
            || self.access_keys.is_some()
            || self.key_editor.is_some()
            || self.created_key.is_some()
            || self.sync.is_some()
            || self.unlock.is_some()
            || self.master_dialog.is_some()
            || self.update.is_some()
            || self.show_about
    }

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.dialogs_open() {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        ctx.input_mut(|i| {
            if i.consume_shortcut(&SC_REFRESH) {
                actions.push(Action::Refresh);
            }
            if i.consume_shortcut(&SC_ABOUT) {
                actions.push(Action::About);
            }
            if i.consume_shortcut(&SC_NEW_FOLDER) {
                actions.push(Action::NewFolderPrompt);
            }
            if i.consume_shortcut(&SC_UPLOAD) {
                actions.push(Action::UploadFiles);
            }
            if i.consume_shortcut(&SC_DOWNLOAD) {
                actions.push(Action::Download);
            }
            if i.consume_shortcut(&SC_FILTER) {
                self.focus_filter = true;
            }
            if typing {
                return;
            }
            for event in &i.events {
                match event {
                    egui::Event::Copy => actions.push(Action::Copy),
                    egui::Event::Cut => actions.push(Action::Cut),
                    egui::Event::Paste(_) => actions.push(Action::Paste),
                    _ => {}
                }
            }
            if i.consume_shortcut(&SC_SELECT_ALL) {
                actions.push(Action::SelectAll);
            }
            if i.consume_shortcut(&SC_RENAME) {
                actions.push(Action::RenamePrompt);
            }
            if i.consume_shortcut(&SC_DELETE) {
                actions.push(Action::DeleteSelected);
            }
            if i.consume_shortcut(&SC_UP) {
                actions.push(Action::Up);
            }
            if i.key_pressed(Key::Enter)
                && let Some(key) = self.focused.clone().filter(|k| k.ends_with('/'))
            {
                actions.push(Action::Open(key));
            }
        });
    }

    fn dropped_files(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let dropped: Vec<_> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if !dropped.is_empty() && !self.dialogs_open() {
            actions.push(Action::UploadDropped(dropped));
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let text = match &self.current_bucket {
            Some(b) => format!(
                "{}  Drop to upload to {b}/{}",
                icon::UPLOAD_SIMPLE,
                self.prefix
            ),
            None => "Select a bucket first".to_owned(),
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::new("drop-overlay"),
        ));
        let rect = ctx.content_rect();
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(140));
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(22.0),
            Color32::WHITE,
        );
    }

    fn menu_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let has_account = self.current_account.is_some();
        let has_bucket = self.current_bucket.is_some();
        let selection = self.selected_entries();
        let single = selection.len() == 1;
        let single_file = single && !selection[0].is_folder;
        let has_selection = !selection.is_empty();
        let can_paste = has_bucket && self.clipboard.is_some();
        let manual_bucket = self
            .current_bucket
            .as_ref()
            .is_some_and(|b| self.buckets.iter().any(|x| &x.name == b && x.manual));
        let data_dir = self.data_dir.clone();
        let dark = self.settings.dark_mode;
        let item = |ui: &mut Ui, text: &str, shortcut: &str, enabled: bool| {
            ui.add_enabled(enabled, Button::new(text).shortcut_text(shortcut))
                .clicked()
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Accounts", |ui| {
                if item(ui, "Add account…", "", true) {
                    actions.push(Action::EditAccount(None));
                }
                if item(ui, "Manage accounts…", "", true) {
                    actions.push(Action::ManageAccounts);
                }
                if item(ui, "Edit current account…", "", has_account) {
                    actions.push(Action::EditAccount(self.current_account));
                }
                if item(
                    ui,
                    "Test connection",
                    "",
                    has_account && !self.toolbar_testing,
                ) {
                    actions.push(Action::TestConnection);
                }
                if item(ui, "Access keys…", "", has_account) {
                    actions.push(Action::AccessKeys);
                }
                ui.separator();
                if item(ui, "Import accounts…", "", true) {
                    actions.push(Action::ImportAccounts);
                }
                if item(ui, "Export accounts…", "", !self.account_list.is_empty()) {
                    actions.push(Action::ExportAccounts);
                }
                ui.separator();
                if item(ui, "Exit", "Alt+F4", true) {
                    actions.push(Action::Exit);
                }
            });
            ui.menu_button("Buckets", |ui| {
                if item(ui, "Refresh", "F5", has_account) {
                    actions.push(Action::Refresh);
                }
                if item(ui, "Add bucket by name…", "", has_account) {
                    actions.push(Action::AddBucketPrompt);
                }
                if item(ui, "Remove selected bucket from list", "", manual_bucket)
                    && let Some(b) = self.current_bucket.clone()
                {
                    actions.push(Action::RemoveBucket(b));
                }
                if item(ui, "Copy bucket name", "", has_bucket)
                    && let Some(b) = self.current_bucket.clone()
                {
                    actions.push(Action::CopyText(b));
                }
                ui.separator();
                if item(ui, "Create new bucket…", "", has_account) {
                    actions.push(Action::CreateBucketPrompt);
                }
                if item(ui, "Delete bucket…", "", has_bucket)
                    && let Some(b) = self.current_bucket.clone()
                {
                    actions.push(Action::DeleteBucket(b));
                }
                if item(ui, "Bucket properties", "", has_bucket)
                    && let Some(b) = self.current_bucket.clone()
                {
                    actions.push(Action::BucketProperties(b));
                }
                if item(ui, "Bucket settings…", "", has_bucket)
                    && let Some(b) = self.current_bucket.clone()
                {
                    actions.push(Action::BucketSettings(b));
                }
            });
            ui.menu_button("Files", |ui| {
                if item(ui, "Upload files…", "Ctrl+U", has_bucket) {
                    actions.push(Action::UploadFiles);
                }
                if item(ui, "Upload folder…", "", has_bucket) {
                    actions.push(Action::UploadFolder);
                }
                if item(ui, "Download…", "Ctrl+D", has_selection) {
                    actions.push(Action::Download);
                }
                ui.separator();
                if item(ui, "Copy", "Ctrl+C", has_selection) {
                    actions.push(Action::Copy);
                }
                if item(ui, "Cut", "Ctrl+X", has_selection) {
                    actions.push(Action::Cut);
                }
                if item(ui, "Paste", "Ctrl+V", can_paste) {
                    actions.push(Action::Paste);
                }
                ui.separator();
                if item(ui, "New folder…", "Ctrl+N", has_bucket) {
                    actions.push(Action::NewFolderPrompt);
                }
                if item(ui, "Rename…", "F2", single) {
                    actions.push(Action::RenamePrompt);
                }
                if item(ui, "Delete", "Del", has_selection) {
                    actions.push(Action::DeleteSelected);
                }
                ui.separator();
                if item(ui, "Select all", "Ctrl+A", has_bucket) {
                    actions.push(Action::SelectAll);
                }
                if item(ui, "Web URL…", "", single_file) {
                    actions.push(Action::WebUrl);
                }
                if item(ui, "Edit headers…", "", single_file) {
                    actions.push(Action::EditHeaders);
                }
                if item(ui, "Copy path (s3://)", "", has_selection) {
                    actions.push(Action::CopyPath);
                }
                if item(ui, "Load next page", "", self.next_token.is_some()) {
                    actions.push(Action::LoadMore);
                }
            });
            ui.menu_button("Tools", |ui| {
                if item(ui, "Sync folder…", "", has_bucket) {
                    actions.push(Action::Sync);
                }
                ui.separator();
                if item(ui, "Options…", "", true) {
                    actions.push(Action::Options);
                }
                if ui.checkbox(&mut dark.clone(), "Dark theme").clicked() {
                    actions.push(Action::ToggleTheme);
                }
                if item(ui, "Clear finished tasks", "", true) {
                    actions.push(Action::ClearFinished);
                }
                ui.separator();
                if item(ui, "Master password…", "", !self.vault.is_locked()) {
                    actions.push(Action::MasterPassword);
                }
                if self.vault.is_locked() {
                    if item(ui, "Unlock…", "", true) {
                        actions.push(Action::Unlock);
                    }
                } else if item(ui, "Lock now", "", self.vault.is_required()) {
                    actions.push(Action::Lock);
                }
                ui.separator();
                if item(ui, "Open data folder", "", true) {
                    actions.push(Action::OpenFolder(data_dir.clone()));
                }
                if item(ui, "Open logs folder", "", true) {
                    actions.push(Action::OpenFolder(data_dir.join("logs")));
                }
            });
            ui.menu_button("Help", |ui| {
                if item(ui, "Check for updates…", "", !self.update_checking) {
                    actions.push(Action::CheckUpdates);
                }
                ui.separator();
                if item(ui, &format!("About {}", s3m_core::APP_NAME), "F1", true) {
                    actions.push(Action::About);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (glyph, tip) = if dark {
                    (icon::SUN, "Switch to the light theme")
                } else {
                    (icon::MOON, "Switch to the dark theme")
                };
                if ui
                    .add(Button::new(glyph).frame(false))
                    .on_hover_text(tip)
                    .clicked()
                {
                    actions.push(Action::ToggleTheme);
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let has_account = self.current_account.is_some();
        let has_bucket = self.current_bucket.is_some();
        let selection = self.selected_entries();
        let single = selection.len() == 1;
        let single_file = single && !selection[0].is_folder;
        let has_selection = !selection.is_empty();
        drop(selection);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("Account").size(11.5));
                let current = self
                    .current()
                    .map_or_else(|| "No account".to_owned(), |a| a.name.clone());
                egui::ComboBox::from_id_salt("account-combo")
                    .width(200.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        // Grouped by folder: top level first, then folders alphabetically.
                        let mut folders: Vec<&str> = self
                            .account_list
                            .iter()
                            .map(|a| a.folder.as_str())
                            .collect();
                        folders.sort_unstable();
                        folders.dedup();
                        for folder in folders {
                            if !folder.is_empty() {
                                ui.label(
                                    RichText::new(format!("{} {folder}", icon::FOLDER))
                                        .size(11.5)
                                        .strong(),
                                );
                            }
                            for a in self.account_list.iter().filter(|a| a.folder == folder) {
                                let label = if folder.is_empty() {
                                    a.name.clone()
                                } else {
                                    format!("    {}", a.name)
                                };
                                if ui
                                    .selectable_label(Some(a.id) == self.current_account, label)
                                    .on_hover_text(a.endpoint_url())
                                    .clicked()
                                {
                                    actions.push(Action::SelectAccount(a.id));
                                }
                            }
                        }
                        if self.account_list.is_empty() {
                            ui.label("No accounts yet");
                        }
                    });
            });
            ui.add_space(4.0);
            if toolbar_button(ui, icon::USERS, "Accounts", true, "Manage storage accounts")
                .clicked()
            {
                actions.push(Action::ManageAccounts);
            }
            if toolbar_button(
                ui,
                icon::PENCIL_SIMPLE,
                "Edit",
                has_account,
                "Edit this account (keys, endpoint, registered buckets)",
            )
            .clicked()
            {
                actions.push(Action::EditAccount(self.current_account));
            }
            let testing = self.toolbar_testing;
            if toolbar_button(
                ui,
                icon::PLUGS_CONNECTED,
                if testing { "Testing…" } else { "Test" },
                has_account && !testing,
                "Test connection",
            )
            .clicked()
            {
                actions.push(Action::TestConnection);
            }
            if toolbar_button(
                ui,
                icon::KEY,
                "Keys",
                has_account,
                "Access keys of this account's user (MinIO)",
            )
            .clicked()
            {
                actions.push(Action::AccessKeys);
            }
            toolbar_separator(ui);
            if toolbar_button(
                ui,
                icon::ARROWS_CLOCKWISE,
                "Refresh",
                has_account,
                "Refresh (F5)",
            )
            .clicked()
            {
                actions.push(Action::Refresh);
            }
            if toolbar_button(
                ui,
                icon::PLUS,
                "Add bucket",
                has_account,
                "Add a bucket by name",
            )
            .clicked()
            {
                actions.push(Action::AddBucketPrompt);
            }
            toolbar_separator(ui);
            let upload_label = format!("Upload {}", icon::CARET_DOWN);
            let upload = toolbar_button(
                ui,
                icon::UPLOAD_SIMPLE,
                &upload_label,
                has_bucket,
                "Upload files or a folder (Ctrl+U)",
            );
            egui::Popup::menu(&upload).show(|ui| {
                if ui.button("Upload files…").clicked() {
                    actions.push(Action::UploadFiles);
                }
                if ui.button("Upload folder…").clicked() {
                    actions.push(Action::UploadFolder);
                }
            });
            if toolbar_button(
                ui,
                icon::DOWNLOAD_SIMPLE,
                "Download",
                has_selection,
                "Download selected files and folders (Ctrl+D)",
            )
            .clicked()
            {
                actions.push(Action::Download);
            }
            toolbar_separator(ui);
            if toolbar_button(ui, icon::FOLDER_PLUS, "New folder", has_bucket, "Ctrl+N").clicked() {
                actions.push(Action::NewFolderPrompt);
            }
            if toolbar_button(ui, icon::PENCIL_LINE, "Rename", single, "F2").clicked() {
                actions.push(Action::RenamePrompt);
            }
            if toolbar_button(ui, icon::TRASH, "Delete", has_selection, "Del").clicked() {
                actions.push(Action::DeleteSelected);
            }
            toolbar_separator(ui);
            if toolbar_button(
                ui,
                icon::LINK_SIMPLE,
                "Web URL",
                single_file,
                "Generate a link that works without keys (presigned URL)",
            )
            .clicked()
            {
                actions.push(Action::WebUrl);
            }
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let p = theme::palette(ui);
        ui.horizontal_centered(|ui| {
            match self.current() {
                Some(a) => {
                    ui.label(RichText::new(&a.name).strong());
                    ui.label(RichText::new("·").color(p.faint));
                    ui.label(RichText::new(&a.endpoint_host).color(p.dim));
                    ui.label(RichText::new("·").color(p.faint));
                    if a.use_tls {
                        ui.label(
                            RichText::new(format!("{} TLS", icon::LOCK_SIMPLE)).color(p.success),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("{} HTTP · private network only", icon::WARNING))
                                .color(p.warning),
                        );
                    }
                }
                None => {
                    ui.label(RichText::new("No account").color(p.dim));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let active = self.queue.active_count();
                if active > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{} {active} transfer(s) running",
                            icon::ARROWS_CLOCKWISE
                        ))
                        .color(p.accent),
                    );
                } else {
                    ui.label(RichText::new("No active transfers").color(p.dim));
                }
                if self.current_bucket.is_some() {
                    ui.label(RichText::new("·").color(p.faint));
                    let more = if self.next_token.is_some() {
                        " (more available)"
                    } else {
                        ""
                    };
                    ui.label(
                        RichText::new(format!(
                            "{} item(s){more}",
                            format::count(self.objects.len() as u64)
                        ))
                        .color(p.dim),
                    );
                }
                let selection = self.selected_entries();
                if !selection.is_empty() {
                    ui.label(RichText::new("·").color(p.faint));
                    let text = if selection.len() == 1 {
                        "1 selected".to_owned()
                    } else {
                        format!(
                            "{} selected · {}",
                            selection.len(),
                            format::size(selection.iter().map(|o| o.size).sum())
                        )
                    };
                    ui.label(RichText::new(text).color(p.dim));
                }
            });
        });
    }
}
