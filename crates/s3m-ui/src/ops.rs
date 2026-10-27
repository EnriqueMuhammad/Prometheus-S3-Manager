//! Phase 2 file operations: folder download/delete/rename, copy/cut/paste, buckets,
//! Web URL, headers and preview.

use std::sync::Arc;
use std::time::Duration;

use s3m_core::plan::{self, Clipboard, ListedObject};
use s3m_core::{HeaderEdit, browse, format};
use s3m_s3::StorageError;

use crate::app::App;
use crate::model::{
    BottomTab, ConfirmAction, Expand, Modal, Msg, PreviewState, PromptAction, Reload,
};

/// Largest image shown in the Preview tab.
const MAX_IMAGE_PREVIEW: u64 = 10 * 1024 * 1024;
/// Bytes of a text file shown in the Preview tab.
const TEXT_PREVIEW: u64 = 256 * 1024;

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico"];

fn extension(key: &str) -> String {
    browse::leaf_name(key)
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

impl App {
    /// Lists every object under the selected folders (selected files are included as they are).
    fn expand(&mut self, keys: Vec<String>, purpose: Expand) {
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        let known: Vec<ListedObject> = self
            .objects
            .iter()
            .filter(|o| !o.is_folder && keys.contains(&o.key))
            .map(|o| ListedObject {
                key: o.key.clone(),
                size: o.size,
            })
            .collect();
        let storage = self.storage.clone();
        self.busy_ops += 1;
        self.log(format!("Listing {} selected item(s)…", keys.len()), false);
        self.spawn(async move {
            let result = async {
                let mut out = known;
                for key in keys.iter().filter(|k| k.ends_with('/')) {
                    out.extend(storage.list_all(&account, &bucket, key).await?);
                }
                // Files that were not in the current listing (clipboard from another folder).
                for key in keys.iter().filter(|k| !k.ends_with('/')) {
                    if !out.iter().any(|o| &o.key == key) {
                        out.extend(
                            storage
                                .list_all(&account, &bucket, key)
                                .await?
                                .into_iter()
                                .filter(|o| &o.key == key),
                        );
                    }
                }
                Ok::<_, StorageError>(out)
            }
            .await;
            Msg::Expanded { purpose, result }
        });
    }

    /// Clipboard source bucket must be listed with the clipboard's bucket, not the current one.
    fn expand_clipboard(&mut self, clipboard: Clipboard, bucket: String, prefix: String) {
        let Some(account) = self.account(clipboard.account_id).cloned() else {
            self.show_error("Paste", "The account of the copied items no longer exists.");
            return;
        };
        let storage = self.storage.clone();
        self.busy_ops += 1;
        self.log(
            format!("Listing {} copied item(s)…", clipboard.keys.len()),
            false,
        );
        self.spawn(async move {
            let result = async {
                let mut out = Vec::new();
                for key in &clipboard.keys {
                    let found = storage.list_all(&account, &clipboard.bucket, key).await?;
                    if key.ends_with('/') {
                        out.extend(found);
                    } else {
                        out.extend(found.into_iter().filter(|o| &o.key == key));
                    }
                }
                Ok::<_, StorageError>(out)
            }
            .await;
            Msg::Expanded {
                purpose: Expand::Paste {
                    clipboard,
                    bucket,
                    prefix,
                },
                result,
            }
        });
    }

    pub(crate) fn handle_expanded(
        &mut self,
        purpose: Expand,
        result: Result<Vec<ListedObject>, StorageError>,
    ) {
        self.busy_ops = self.busy_ops.saturating_sub(1);
        let objects = match result {
            Ok(o) => o,
            Err(e) => {
                self.log(e.display(), true);
                self.show_error(APP, e.display());
                return;
            }
        };
        let Some(account_id) = self.current_account else {
            return;
        };
        let Some(bucket) = self.current_bucket.clone() else {
            return;
        };
        match purpose {
            Expand::Download { dest, strip } => {
                let plan = plan::download_plan(account_id, &bucket, &dest, &strip, &objects);
                for dir in &plan.folders {
                    if let Err(e) = std::fs::create_dir_all(dir) {
                        self.log(format!("Cannot create {}: {e}", dir.display()), true);
                    }
                }
                if !plan.rejected.is_empty() {
                    self.show_error("Download", plan.rejected.join("\n"));
                }
                if plan.requests.is_empty() && plan.rejected.is_empty() {
                    self.log(
                        "Nothing to download: the selected folders are empty.",
                        false,
                    );
                }
                self.enqueue(plan.requests);
            }
            Expand::Delete => {
                if objects.is_empty() && self.selection.is_empty() {
                    return;
                }
                let mut keys: Vec<String> = objects.iter().map(|o| o.key.clone()).collect();
                // Folder markers of the selected folders themselves.
                for key in self.selection.iter().filter(|k| k.ends_with('/')) {
                    if !keys.contains(key) {
                        keys.push(key.clone());
                    }
                }
                let size: u64 = objects.iter().map(|o| o.size).sum();
                let files = objects.iter().filter(|o| !o.key.ends_with('/')).count();
                let mut preview = keys.iter().take(10).cloned().collect::<Vec<_>>().join("\n");
                if keys.len() > 10 {
                    preview.push_str(&format!("\n… and {} more", keys.len() - 10));
                }
                self.modal = Some(Modal::Confirm {
                    title: "Delete folders".into(),
                    body: format!(
                        "Permanently delete {} file(s) ({}) and the selected folders from \"{bucket}\"?\n\nThis cannot be undone.\n\n{preview}",
                        files,
                        format::size(size)
                    ),
                    ok: "Delete".into(),
                    alternative: None,
                    action: ConfirmAction::DeleteMany { keys },
                    danger: true,
                });
            }
            Expand::Paste {
                clipboard,
                bucket: dest_bucket,
                prefix,
            } => {
                let requests =
                    plan::paste_requests(&clipboard, &objects, account_id, &dest_bucket, &prefix);
                if requests.is_empty() {
                    self.show_info("Paste", "The copied items no longer exist.");
                    return;
                }
                if clipboard.cut {
                    self.clipboard = None;
                }
                // Overwrite check against the folder on screen.
                let here = self.current_bucket.as_deref() == Some(dest_bucket.as_str())
                    && self.prefix == prefix;
                let conflicts = if here {
                    requests
                        .iter()
                        .filter(|r| self.objects.iter().any(|o| !o.is_folder && o.key == r.key))
                        .count()
                } else {
                    0
                };
                if clipboard.cut && conflicts == 0 {
                    let roots: Vec<String> = clipboard
                        .keys
                        .iter()
                        .filter(|k| k.ends_with('/'))
                        .cloned()
                        .collect();
                    let keys: Vec<String> = objects.iter().map(|o| o.key.clone()).collect();
                    self.enqueue_moves(
                        requests,
                        clipboard.account_id,
                        &clipboard.bucket,
                        &keys,
                        &roots,
                    );
                    return;
                }
                if conflicts > 0 {
                    self.modal = Some(Modal::Confirm {
                        title: "Paste".into(),
                        body: format!(
                            "{conflicts} file(s) in \"{dest_bucket}/{prefix}\" have the same name and will be overwritten. Continue?"
                        ),
                        ok: "Overwrite".into(),
                        alternative: None,
                        action: ConfirmAction::Enqueue { requests },
                        danger: true,
                    });
                } else {
                    self.enqueue(requests);
                }
            }
            Expand::RenameFolder { old, new } => {
                let requests =
                    plan::rename_folder_requests(account_id, &bucket, &old, &new, &objects);
                if requests.is_empty() {
                    self.show_info("Rename folder", "The folder is empty; nothing to rename.");
                    return;
                }
                self.log(
                    format!(
                        "Renaming folder {old} -> {new} ({} object(s))",
                        requests.len()
                    ),
                    false,
                );
                let keys: Vec<String> = objects.iter().map(|o| o.key.clone()).collect();
                self.enqueue_moves(requests, account_id, &bucket, &keys, &[old]);
            }
        }
    }

    /// Enqueues moves and remembers the source folders to remove once every move succeeded.
    fn enqueue_moves(
        &mut self,
        requests: Vec<s3m_core::TransferRequest>,
        account_id: uuid::Uuid,
        bucket: &str,
        source_keys: &[String],
        roots: &[String],
    ) {
        let folders = plan::folder_prefixes(source_keys, roots);
        let count = requests.len();
        let jobs = self.queue.enqueue(requests);
        self.bottom_tab = BottomTab::Tasks;
        self.log(format!("Queued {count} move(s)"), false);
        if !folders.is_empty() {
            self.cleanups.push(crate::model::FolderCleanup {
                account_id,
                bucket: bucket.to_owned(),
                folders,
                remaining: jobs.iter().map(|j| j.id).collect(),
                failed: false,
            });
        }
    }

    /// Called when a job reaches a final state.
    pub(crate) fn cleanup_after(&mut self, job: &s3m_core::TransferJob) {
        let mut ready = Vec::new();
        self.cleanups.retain_mut(|c| {
            if c.remaining.remove(&job.id) && job.state != s3m_core::TransferState::Completed {
                c.failed = true;
            }
            if c.remaining.is_empty() {
                if !c.failed {
                    ready.push((
                        c.account_id,
                        c.bucket.clone(),
                        std::mem::take(&mut c.folders),
                    ));
                }
                return false;
            }
            true
        });
        for (account_id, bucket, folders) in ready {
            let Some(account) = self.account(account_id).cloned() else {
                continue;
            };
            let storage = self.storage.clone();
            self.spawn(async move {
                let result = storage
                    .delete_tree(&account, &bucket, &[], &folders)
                    .await
                    .map(|_| format!("Removed {} emptied folder(s) in {bucket}", folders.len()));
                Msg::Done {
                    title: "Couldn't remove the emptied folders".into(),
                    result,
                    reload: Reload::Objects,
                }
            });
        }
    }

    // ------------------------------------------------------------------ selection actions

    pub(crate) fn download_selection(&mut self) {
        let entries: Vec<_> = self.selected_entries().into_iter().cloned().collect();
        if entries.is_empty() {
            return;
        }
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Download to")
            .pick_folder()
        else {
            return;
        };
        let keys: Vec<String> = entries.iter().map(|e| e.key.clone()).collect();
        if entries.iter().any(|e| e.is_folder) {
            self.expand(
                keys,
                Expand::Download {
                    dest,
                    strip: self.prefix.clone(),
                },
            );
        } else {
            let objects: Vec<ListedObject> = entries
                .iter()
                .map(|e| ListedObject {
                    key: e.key.clone(),
                    size: e.size,
                })
                .collect();
            self.busy_ops += 1;
            self.handle_expanded(
                Expand::Download {
                    dest,
                    strip: self.prefix.clone(),
                },
                Ok(objects),
            );
        }
    }

    pub(crate) fn delete_selection(&mut self) {
        let Some(bucket) = self.current_bucket.clone() else {
            return;
        };
        let entries: Vec<_> = self.selected_entries().into_iter().cloned().collect();
        if entries.is_empty() {
            return;
        }
        let keys: Vec<String> = entries.iter().map(|e| e.key.clone()).collect();
        if entries.iter().any(|e| e.is_folder) {
            self.expand(keys, Expand::Delete);
            return;
        }
        let mut preview = keys.iter().take(10).cloned().collect::<Vec<_>>().join("\n");
        if keys.len() > 10 {
            preview.push_str(&format!("\n… and {} more", keys.len() - 10));
        }
        self.modal = Some(Modal::Confirm {
            title: "Delete files".into(),
            body: format!(
                "Permanently delete {} file(s) from \"{bucket}\"?\n\n{preview}",
                keys.len()
            ),
            ok: "Delete".into(),
            alternative: None,
            action: ConfirmAction::Delete { keys },
            danger: true,
        });
    }

    pub(crate) fn delete_keys(&mut self, keys: Vec<String>) {
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        let roots: Vec<String> = keys.iter().filter(|k| k.ends_with('/')).cloned().collect();
        let folders = plan::folder_prefixes(&keys, &roots);
        let files: Vec<String> = keys.into_iter().filter(|k| !k.ends_with('/')).collect();
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = storage
                .delete_tree(&account, &bucket, &files, &folders)
                .await
                .map(|n| {
                    if folders.is_empty() {
                        format!("Deleted {n} file(s) from {bucket}")
                    } else {
                        format!(
                            "Deleted {n} file(s) and {} folder(s) from {bucket}",
                            folders.len()
                        )
                    }
                });
            Msg::Done {
                title: "Couldn't delete the selected items".into(),
                result,
                reload: Reload::Objects,
            }
        });
    }

    pub(crate) fn rename_prompt(&mut self) {
        let selected = self.selected_entries();
        let [entry] = selected.as_slice() else { return };
        let (key, name, folder) = (entry.key.clone(), entry.name.clone(), entry.is_folder);
        self.modal = Some(Modal::Prompt {
            title: if folder { "Rename folder" } else { "Rename" }.into(),
            label: if folder {
                "New folder name (every object in the folder is copied, then the originals are deleted):"
            } else {
                "New name (the object is copied, then the original is deleted):"
            }
            .into(),
            text: name,
            action: if folder {
                PromptAction::RenameFolder { key }
            } else {
                PromptAction::Rename { key }
            },
            focus: true,
        });
    }

    pub(crate) fn rename_folder(&mut self, key: String, name: String) {
        if let Err(e) = plan::check_folder_name(&name) {
            self.show_error("Rename folder", e);
            return;
        }
        let new = format!("{}{}/", self.prefix, name.trim());
        if new == key {
            return;
        }
        if self.objects.iter().any(|o| o.key == new) {
            self.show_error(
                "Rename folder",
                format!("A folder named \"{}\" already exists here.", name.trim()),
            );
            return;
        }
        self.expand(vec![key.clone()], Expand::RenameFolder { old: key, new });
    }

    pub(crate) fn copy_to_clipboard(&mut self, cut: bool) {
        let (Some(account_id), Some(bucket)) = (self.current_account, self.current_bucket.clone())
        else {
            return;
        };
        let keys: Vec<String> = self
            .selected_entries()
            .iter()
            .map(|o| o.key.clone())
            .collect();
        if keys.is_empty() {
            return;
        }
        self.log(
            format!(
                "{} {} item(s) — open a folder and Paste (Ctrl+V)",
                if cut { "Cut" } else { "Copied" },
                keys.len()
            ),
            false,
        );
        self.clipboard = Some(Clipboard {
            account_id,
            bucket,
            keys,
            cut,
        });
    }

    pub(crate) fn paste(&mut self) {
        let Some(clipboard) = self.clipboard.clone() else {
            return;
        };
        let Some(bucket) = self.current_bucket.clone() else {
            self.show_info("Paste", "Open a bucket or folder to paste into.");
            return;
        };
        let Some(dest_account) = self.current_account else {
            return;
        };
        if clipboard.account_id == dest_account {
            for key in &clipboard.keys {
                if let Err(e) = plan::check_paste(&clipboard.bucket, key, &bucket, &self.prefix) {
                    self.show_error("Paste", e);
                    return;
                }
            }
        } else {
            let from = self
                .account(clipboard.account_id)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            self.log(
                format!("Copying from account \"{from}\": objects are streamed through this PC."),
                false,
            );
        }
        let prefix = self.prefix.clone();
        self.expand_clipboard(clipboard, bucket, prefix);
    }

    pub(crate) fn copy_paths(&mut self, ctx: &egui::Context) {
        let Some(bucket) = self.current_bucket.clone() else {
            return;
        };
        let paths: Vec<String> = self
            .selected_entries()
            .iter()
            .map(|o| format!("s3://{bucket}/{}", o.key))
            .collect();
        if !paths.is_empty() {
            ctx.copy_text(paths.join("\n"));
            self.log(format!("Copied {} path(s)", paths.len()), false);
        }
    }

    // ------------------------------------------------------------------ Web URL

    pub(crate) fn open_web_url(&mut self) {
        let Some(bucket) = self.current_bucket.clone() else {
            return;
        };
        let selected = self.selected_entries();
        let [entry] = selected.as_slice() else {
            self.show_info("Web URL", "Select one file.");
            return;
        };
        if entry.is_folder {
            self.show_info(
                "Web URL",
                "Web URLs can be generated for files, not folders.",
            );
            return;
        }
        let key = entry.key.clone();
        self.web_nonce += 1;
        self.modal = Some(Modal::WebUrl {
            bucket,
            key,
            minutes: 60,
            url: String::new(),
            error: String::new(),
            nonce: self.web_nonce,
            busy: false,
        });
        self.request_web_url();
    }

    pub(crate) fn request_web_url(&mut self) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        let Some(Modal::WebUrl {
            bucket,
            key,
            minutes,
            nonce,
            busy,
            error,
            ..
        }) = self.modal.as_mut()
        else {
            return;
        };
        *busy = true;
        error.clear();
        let (bucket, key, expires, nonce) = (
            bucket.clone(),
            key.clone(),
            Duration::from_secs(u64::from(*minutes) * 60),
            *nonce,
        );
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Presigned {
                nonce,
                result: storage.presign_get(&account, &bucket, &key, expires).await,
            }
        });
    }

    // ------------------------------------------------------------------ buckets

    pub(crate) fn create_bucket_prompt(&mut self) {
        let Some(account) = self.current() else {
            return;
        };
        let region = if account.kind == s3m_core::AccountType::AmazonS3 {
            account.region.clone()
        } else {
            String::new()
        };
        self.modal = Some(Modal::CreateBucket {
            name: String::new(),
            region,
            error: String::new(),
        });
    }

    pub(crate) fn create_bucket(&mut self, name: String, region: String) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        let storage = self.storage.clone();
        let name = s3m_core::bucket::normalize(&name);
        self.pending_bucket = Some(name.clone());
        self.spawn(async move {
            let result = storage
                .create_bucket(&account, &name, &region)
                .await
                .map(|()| format!("Created bucket {name}"));
            Msg::Done {
                title: format!("Couldn't create bucket \"{name}\""),
                result,
                reload: Reload::Buckets,
            }
        });
    }

    pub(crate) fn delete_bucket(&mut self, name: String, contents: bool) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        if self.current_bucket.as_deref() == Some(name.as_str()) {
            self.current_bucket = None;
            self.prefix.clear();
        }
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = storage
                .delete_bucket(&account, &name, contents)
                .await
                .map(|n| {
                    if contents {
                        format!("Deleted bucket {name} and {n} object(s)")
                    } else {
                        format!("Deleted bucket {name}")
                    }
                });
            Msg::Done {
                title: format!("Couldn't delete bucket \"{name}\""),
                result,
                reload: Reload::Buckets,
            }
        });
    }

    pub(crate) fn bucket_properties(&mut self, name: String) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        self.focused = None;
        self.details = None;
        self.properties = vec![
            ("Bucket".into(), name.clone()),
            ("Status".into(), "Loading…".into()),
        ];
        self.bottom_tab = BottomTab::Properties;
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::BucketInfo {
                result: storage.bucket_info(&account, &name).await,
            }
        });
    }

    // ------------------------------------------------------------------ headers

    pub(crate) fn edit_headers(&mut self) {
        let (Some(bucket), Some(details)) = (self.current_bucket.clone(), self.details.clone())
        else {
            self.show_info(
                "Edit headers",
                "Select a file and wait for its properties to load.",
            );
            return;
        };
        self.modal = Some(Modal::Headers {
            bucket,
            key: details.key.clone(),
            edit: HeaderEdit::from_details(&details),
            error: String::new(),
        });
    }

    pub(crate) fn save_headers(&mut self, bucket: String, key: String, edit: HeaderEdit) {
        let Some(account) = self.current().cloned() else {
            return;
        };
        let storage = self.storage.clone();
        self.spawn(async move {
            let result = storage
                .update_headers(&account, &bucket, &key, &edit)
                .await
                .map(|()| format!("Updated headers of {key}"));
            Msg::Done {
                title: "Couldn't update the headers".into(),
                result,
                reload: Reload::Objects,
            }
        });
    }

    // ------------------------------------------------------------------ preview

    /// Starts loading the preview of the focused file when the Preview tab is shown.
    pub(crate) fn ensure_preview(&mut self) {
        let Some(key) = self.focused.clone() else {
            self.preview = None;
            return;
        };
        if self.preview.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        if let Some((_, PreviewState::Image { uri, .. })) = &self.preview {
            self.ctx.forget_image(uri);
        }
        let Some(entry) = self.objects.iter().find(|o| o.key == key).cloned() else {
            return;
        };
        if entry.is_folder {
            self.preview = Some((
                key,
                PreviewState::Unavailable("Select a file to preview it.".into()),
            ));
            return;
        }
        let image = IMAGE_EXTENSIONS.contains(&extension(&key).as_str());
        if image && entry.size > MAX_IMAGE_PREVIEW {
            self.preview = Some((
                key,
                PreviewState::Unavailable(format!(
                    "Images larger than {} are not previewed.",
                    format::size(MAX_IMAGE_PREVIEW)
                )),
            ));
            return;
        }
        if entry.size == 0 {
            self.preview = Some((
                key,
                PreviewState::Text {
                    text: String::new(),
                    truncated: false,
                },
            ));
            return;
        }
        let (Some(account), Some(bucket)) = (self.current().cloned(), self.current_bucket.clone())
        else {
            return;
        };
        let limit = if image { entry.size } else { TEXT_PREVIEW };
        self.preview = Some((key.clone(), PreviewState::Loading));
        let storage = self.storage.clone();
        self.spawn(async move {
            Msg::Preview {
                result: storage.preview(&account, &bucket, &key, limit).await,
                key,
            }
        });
    }

    pub(crate) fn handle_preview(
        &mut self,
        key: String,
        result: Result<s3m_s3::PreviewData, StorageError>,
    ) {
        if self.preview.as_ref().is_none_or(|(k, _)| *k != key) {
            return;
        }
        let state = match result {
            Err(e) => PreviewState::Unavailable(e.display()),
            Ok(data) => {
                if IMAGE_EXTENSIONS.contains(&extension(&key).as_str()) {
                    PreviewState::Image {
                        uri: format!("bytes://preview/{}/{key}", self.web_nonce),
                        bytes: Arc::from(data.bytes),
                    }
                } else if data.bytes.iter().take(8192).any(|&b| b == 0) {
                    PreviewState::Unavailable("Binary file; no text preview.".into())
                } else {
                    PreviewState::Text {
                        truncated: data.total > data.bytes.len() as u64,
                        text: String::from_utf8_lossy(&data.bytes).into_owned(),
                    }
                }
            }
        };
        self.preview = Some((key, state));
    }
}

const APP: &str = s3m_core::APP_NAME;
