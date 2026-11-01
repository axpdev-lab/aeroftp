//! ImageKit Storage Provider
//!
//! Implements StorageProvider for ImageKit's REST APIs.
//! Authentication: HTTP Basic with the private API key as username and an empty password.
//! API: https://api.imagekit.io/v1 and https://upload.imagekit.io/api/v1/files/upload

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::HttpResultExt;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::io::ReaderStream;

/// Deserialize a JSON value that may be `null` into a typed default.
///
/// `#[serde(default)]` only fires when a field is *missing*; an explicit
/// `null` still goes through the type's own `Deserialize` impl and trips
/// for non-`Option` targets like `u64` or `Vec<T>`. ImageKit's `/files`
/// listing with `type=all` mixes file and folder entries: folder rows
/// emit `null` for fields that only make sense on files (`size`, `tags`,
/// `mime`, `width`, `height`, ...), so the response would refuse to
/// parse against `Vec<IkFile>` until we tolerate `null` here.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

use super::{
    response_bytes_with_limit, sanitize_api_error, ProviderConfig, ProviderError, ProviderType,
    RemoteEntry, StorageProvider, TransferOptimizationHints, AEROFTP_USER_AGENT,
    MAX_DOWNLOAD_TO_BYTES,
};

const API_BASE: &str = "https://api.imagekit.io/v1";
const UPLOAD_URL: &str = "https://upload.imagekit.io/api/v1/files/upload";

#[derive(Debug, Clone)]
pub struct ImageKitConfig {
    pub imagekit_id: String,
    pub private_key: SecretString,
    pub initial_path: Option<String>,
}

impl ImageKitConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let imagekit_id = config
            .extra
            .get("imagekit_id")
            .cloned()
            .or_else(|| config.username.clone())
            .ok_or_else(|| {
                ProviderError::InvalidConfig("ImageKit URL endpoint ID is required".to_string())
            })?;

        let private_key = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("ImageKit private API key is required".to_string())
        })?;

        Ok(Self {
            imagekit_id: imagekit_id.trim().trim_matches('/').to_string(),
            private_key: SecretString::from(private_key),
            initial_path: config.initial_path.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IkFile {
    #[serde(default, deserialize_with = "null_to_default")]
    file_id: String,
    #[serde(default, deserialize_with = "null_to_default")]
    name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    file_path: String,
    /// A folder carries `folderPath` (and `folderId`), never `filePath`.
    /// `list_raw` folds it into `file_path`, so the rest of the provider
    /// reads one field.
    #[serde(default, deserialize_with = "null_to_default")]
    folder_path: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    thumbnail_url: Option<String>,
    #[serde(default, rename = "type", deserialize_with = "null_to_default")]
    entry_type: String,
    #[serde(default)]
    file_type: Option<String>,
    #[serde(default)]
    mime: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    size: u64,
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    width: Option<u64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    is_private_file: Option<bool>,
    #[serde(default, deserialize_with = "null_to_default")]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IkUploadResponse {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    thumbnail_url: Option<String>,
    #[serde(default)]
    file_type: Option<String>,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct IkError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    error: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IkBulkJob {
    #[serde(default)]
    job_id: String,
}

/// `GET /bulkJobs/{jobId}`: `status` is `Pending` or `Completed`.
#[derive(Debug, Deserialize)]
struct IkBulkJobStatus {
    #[serde(default)]
    status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenameFileRequest<'a> {
    file_path: &'a str,
    new_file_name: &'a str,
    purge_cache: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MoveFileRequest<'a> {
    source_file_path: &'a str,
    destination_path: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CopyFileRequest<'a> {
    source_file_path: &'a str,
    destination_path: &'a str,
    include_file_versions: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderRequest<'a> {
    folder_name: &'a str,
    parent_folder_path: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFolderRequest<'a> {
    folder_path: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkFolderRequest<'a> {
    source_folder_path: &'a str,
    destination_path: &'a str,
    include_file_versions: bool,
}

/// `POST /bulkJobs/renameFolder` (official SDK, `folders.rename`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenameFolderRequest<'a> {
    folder_path: &'a str,
    new_folder_name: &'a str,
    purge_cache: bool,
}

/// How the wait for a folder job ended.
#[derive(Debug)]
enum FolderJob {
    Completed,
    /// Still pending, or not seen: it may complete, and asking again for the
    /// same move waits for it.
    Unfinished(ProviderError),
    /// Over for good without completing, as far as this session can tell.
    Over(ProviderError),
}

pub struct ImageKitProvider {
    config: ImageKitConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Folder jobs a call queued and could not see finish, by
    /// `folder_job_key`: a later call for the same move waits for that job
    /// instead of queueing a second one.
    unfinished_folder_jobs: std::sync::Mutex<HashMap<String, String>>,
    #[cfg(test)]
    api_base_override: Option<String>,
}

/// How long a folder move or copy may stay a pending bulk job before the
/// call gives up waiting and says so. The job keeps running server side, and
/// the wait holds the provider (the trait gives `rename` no way to be
/// cancelled), so it is kept short: a later call for the same move picks the
/// same job up again instead of queueing another.
const FOLDER_JOB_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const FOLDER_JOB_FIRST_POLL: std::time::Duration = std::time::Duration::from_millis(250);
const FOLDER_JOB_MAX_POLL: std::time::Duration = std::time::Duration::from_secs(5);

impl ImageKitProvider {
    /// Build the provider and its HTTP client, with the limits of [`crate::providers::http_limits`].
    /// Nothing is sent until the first request.
    pub fn new(config: ImageKitConfig) -> Self {
        let current_path = normalize_path(config.initial_path.as_deref().unwrap_or("/"));
        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
            .connect_timeout(crate::providers::http_limits::limit(
                std::time::Duration::from_secs(30),
            ))
            .read_timeout(crate::providers::http_limits::limit(
                crate::providers::http_limits::READ_TIMEOUT,
            ))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            connected: false,
            current_path,
            unfinished_folder_jobs: std::sync::Mutex::default(),
            #[cfg(test)]
            api_base_override: None,
        }
    }

    fn api_base(&self) -> String {
        #[cfg(test)]
        if let Some(base) = &self.api_base_override {
            return base.clone();
        }
        API_BASE.to_string()
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.basic_auth(self.config.private_key.expose_secret(), Some(""))
    }

    fn upload_url(&self) -> String {
        #[cfg(test)]
        if let Some(base) = &self.api_base_override {
            return format!("{base}/upload");
        }
        UPLOAD_URL.to_string()
    }

    fn resolve_path(&self, path: &str) -> String {
        // A name of spaces is a name, not the current folder.
        if path.is_empty() {
            return self.current_path.clone();
        }
        if path.starts_with('/') {
            normalize_path(path)
        } else {
            normalize_path(&format!(
                "{}/{}",
                self.current_path.trim_end_matches('/'),
                path
            ))
        }
    }

    async fn parse_error(&self, resp: reqwest::Response) -> ProviderError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<IkError>(&body).ok();
        let msg = parsed
            .as_ref()
            .map(|e| {
                if !e.message.trim().is_empty() {
                    e.message.clone()
                } else {
                    e.error.clone()
                }
            })
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| sanitize_api_error(&body));

        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                ProviderError::AuthenticationFailed(msg)
            }
            StatusCode::NOT_FOUND => ProviderError::NotFound(msg),
            StatusCode::CONFLICT => ProviderError::AlreadyExists(msg),
            StatusCode::BAD_REQUEST if parsed.as_ref().is_some_and(|error|
                error.message == "A file with the same name already exists at the exact location. We could not overwrite it because both overwriteFile and useUniqueFileName are set to false."
            ) => ProviderError::AlreadyExists(msg),
            s if s.is_client_error() => ProviderError::InvalidConfig(msg),
            s if s.is_server_error() => ProviderError::ServerError(msg),
            _ => ProviderError::Other(format!("HTTP {}: {}", status, msg)),
        }
    }

    async fn list_raw(&self, path: &str) -> Result<Vec<IkFile>, ProviderError> {
        validate_path(path)?;
        let dir = normalize_path(path);
        let mut skip = 0usize;
        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        loop {
            // `all` includes files and folders. A full page cannot prove
            // absence; continue with the documented skip offset.
            let url = format!(
                "{}/files?path={}&type=all&limit=1000&skip={skip}",
                self.api_base(),
                urlencoding::encode(path)
            );
            let response = self
                .auth(self.client.get(url))
                .send()
                .await
                .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;
            if !response.status().is_success() {
                return Err(self.parse_error(response).await);
            }
            let mut page = response
                .json::<Vec<IkFile>>()
                .await
                .map_http_err(|e| ProviderError::ParseError(e.to_string()))?;
            let count = page.len();
            for item in &mut page {
                item.file_path = listed_entry_path(item, &dir)?;
                if !seen.insert((
                    item.file_id.clone(),
                    item.file_path.clone(),
                    item.entry_type.clone(),
                )) {
                    return Err(ProviderError::ParseError(
                        "ImageKit lookup listing repeated an entry".into(),
                    ));
                }
            }
            all.extend(page);
            if count < 1000 {
                return Ok(all);
            }
            skip = skip.checked_add(count).ok_or_else(|| {
                ProviderError::ParseError("ImageKit listing offset overflow".into())
            })?;
        }
    }

    async fn upload_file(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        overwrite: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let target = self.resolve_path(remote_path);
        let file_name = if target.ends_with('/') {
            Path::new(local_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .ok_or_else(|| {
                    ProviderError::InvalidPath("Upload path must include a filename".to_string())
                })?
        } else {
            basename(&target).to_string()
        };
        refuse_rewritten_name(&file_name, NameUse::Upload)?;
        let folder = folder_path(&parent_path(&target));
        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total = meta.len();
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut uploaded = 0u64;
        let progress_cb = on_progress;
        let stream = crate::transfer_dag::throttle::throttle_stream(
            ReaderStream::with_capacity(file, 64 * 1024),
            crate::transfer_dag::governor::TransferDirection::Upload,
        )
        .map(move |chunk| {
            if let Ok(bytes) = &chunk {
                uploaded += bytes.len() as u64;
                if let Some(ref cb) = progress_cb {
                    cb(uploaded, total);
                }
            }
            chunk
        });

        let body = reqwest::Body::wrap_stream(stream);
        let mime = mime_guess::from_path(&file_name).first_or_octet_stream();
        let file_part = multipart::Part::stream_with_length(body, total)
            .file_name(file_name.clone())
            .mime_str(mime.as_ref())
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;

        let form = multipart::Form::new()
            .part("file", file_part)
            .text("fileName", file_name)
            .text("folder", folder)
            .text("useUniqueFileName", "false")
            .text("overwriteFile", if overwrite { "true" } else { "false" });

        let resp = self
            .auth(self.client.post(self.upload_url()))
            .multipart(form)
            .send()
            .await
            .map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let uploaded = resp
            .json::<IkUploadResponse>()
            .await
            .map_http_err(|e| ProviderError::ParseError(e.to_string()))?;
        let _ = (
            uploaded.file_id,
            uploaded.name,
            uploaded.file_path,
            uploaded.url,
            uploaded.thumbnail_url,
            uploaded.file_type,
            uploaded.size,
        );
        Ok(())
    }

    async fn find_entry(&self, path: &str) -> Result<IkFile, ProviderError> {
        let resolved = normalize_path(path);
        if resolved == "/" {
            return Ok(IkFile {
                file_id: String::new(),
                name: "/".to_string(),
                file_path: "/".to_string(),
                folder_path: String::new(),
                url: None,
                thumbnail_url: None,
                entry_type: "folder".to_string(),
                file_type: None,
                mime: None,
                size: 0,
                height: None,
                width: None,
                created_at: None,
                updated_at: None,
                is_private_file: None,
                tags: Vec::new(),
            });
        }

        let parent = parent_path(&resolved);
        let name = basename(&resolved);
        let items = self.list_raw(&parent).await?;
        items
            .into_iter()
            .find(|item| normalize_path(&item.file_path) == resolved || item.name == name)
            .ok_or_else(|| ProviderError::NotSupported(format!(
                "ImageKit cannot verify whether {resolved} exists: its listing can omit stored files; absence is not proven"
            )))
    }

    async fn delete_file_by_path(&self, path: &str) -> Result<(), ProviderError> {
        let item = self.find_entry(path).await?;
        if item.file_id.is_empty() {
            return Err(ProviderError::InvalidPath(format!(
                "No file id for {}",
                path
            )));
        }

        let resp = self
            .auth(
                self.client
                    .delete(format!("{}/files/{}", self.api_base(), item.file_id)),
            )
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn delete_folder_by_path(&self, path: &str) -> Result<(), ProviderError> {
        let folder_path = folder_path(path);
        let resp = self
            .auth(self.client.delete(format!("{}/folder/", self.api_base())))
            .json(&DeleteFolderRequest {
                folder_path: &folder_path,
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn copy_file(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        let source = normalize_path(from);
        let dest_parent = folder_path(&parent_path(&normalize_path(to)));
        // The copy keeps the name and a rename follows: refuse the new name
        // before the copy, not after it.
        if basename(&source) != basename(&normalize_path(to)) {
            refuse_rewritten_name(basename(&normalize_path(to)), NameUse::RenameFile)?;
        }
        let resp = self
            .auth(self.client.post(format!("{}/files/copy", self.api_base())))
            .json(&CopyFileRequest {
                source_file_path: &source,
                destination_path: &dest_parent,
                include_file_versions: false,
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let src_name = basename(&source);
        let target = normalize_path(to);
        let dest_name = basename(&target);
        if src_name != dest_name {
            let copied = format!("{}/{}", dest_parent.trim_end_matches('/'), src_name);
            self.rename_file(&copied, dest_name).await?;
        }
        Ok(())
    }

    async fn rename_file(&self, from: &str, new_name: &str) -> Result<(), ProviderError> {
        refuse_rewritten_name(new_name, NameUse::RenameFile)?;
        let source = normalize_path(from);
        let resp = self
            .auth(self.client.put(format!("{}/files/rename", self.api_base())))
            .json(&RenameFileRequest {
                file_path: &source,
                new_file_name: new_name,
                purge_cache: true,
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    /// Whether `path` names a file or folder.
    async fn path_exists(&self, path: &str) -> Result<bool, ProviderError> {
        match self.find_entry(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `PUT /files/move`: move the file at `source` into `dest_parent`
    /// under its name. A same-named file there does not stop it: the moved
    /// file becomes that file's newest version.
    async fn files_move(&self, source: &str, dest_parent: &str) -> Result<(), ProviderError> {
        let resp = self
            .auth(self.client.put(format!("{}/files/move", self.api_base())))
            .json(&MoveFileRequest {
                source_file_path: source,
                destination_path: &folder_path(dest_parent),
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    /// Move and/or rename the file `from` to `to`. ImageKit moves and renames
    /// with two calls: the move keeps the name and lands on top of a
    /// same-named file (as its newest version), the rename refuses a taken
    /// name (409). So the rename goes first, in the source folder, when the
    /// destination folder holds the old name, or when `overwrite` must put
    /// the file on top of the one at `to`.
    async fn move_file(&self, from: &str, to: &str, overwrite: bool) -> Result<(), ProviderError> {
        let source = normalize_path(from);
        let target = normalize_path(to);
        let src_parent = parent_path(&source);
        let dest_parent = parent_path(&target);
        let src_name = basename(&source);
        let dest_name = basename(&target);
        let moves = folder_path(&src_parent) != folder_path(&dest_parent);
        // A move and a rename are two calls: refuse the new name before the
        // first one.
        if src_name != dest_name {
            refuse_rewritten_name(dest_name, NameUse::RenameFile)?;
        }
        if !moves {
            return if src_name == dest_name {
                Ok(())
            } else {
                self.rename_file(&source, dest_name).await
            };
        }
        if src_name == dest_name {
            return self.files_move(&source, &dest_parent).await;
        }
        let intermediate = format!("{}/{src_name}", dest_parent.trim_end_matches('/'));
        let rename_first = self.path_exists(&intermediate).await?
            || (overwrite && self.path_exists(&target).await?);
        if !rename_first {
            self.files_move(&source, &dest_parent).await?;
            return self
                .rename_file(&intermediate, dest_name)
                .await
                .map_err(|e| {
                    ProviderError::Other(format!(
                        "moved {source} to {intermediate}, but renaming it to {dest_name} \
                         failed, so it is still there: {e}"
                    ))
                });
        }
        let renamed = format!("{}/{dest_name}", src_parent.trim_end_matches('/'));
        if self.path_exists(&renamed).await? {
            return Err(ProviderError::Other(format!(
                "Cannot move {source} to {target} in two steps without two items sharing a \
                 name: {intermediate} and {renamed} both exist"
            )));
        }
        self.rename_file(&source, dest_name).await?;
        self.files_move(&renamed, &dest_parent).await.map_err(|e| {
            ProviderError::Other(format!(
                "renamed {source} to {renamed}, but moving it to {dest_parent} failed, so it \
                 is still there: {e}"
            ))
        })
    }

    /// Rename or replace. With `overwrite` false an occupied destination is
    /// refused before anything changes (the `rename` contract): `files/move`
    /// would put the file on top of a same-named one as its newest version,
    /// and `moveFolder` would merge into an existing folder. With it true
    /// (the `replace` contract) a file lands on top of the one at `to`, which
    /// keeps its version history; a replace across file and folder, and a
    /// folder onto a folder (which `moveFolder` would merge), are refused
    /// before anything changes.
    async fn move_entry(
        &mut self,
        from: &str,
        to: &str,
        overwrite: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let source = self.resolve_path(from);
        let target = self.resolve_path(to);
        if source == target {
            return Ok(());
        }
        // The same folder move queued earlier and not seen to finish: wait for
        // that job. Its source may already be gone, and a second job would
        // run after the first. A moveFolder job keeps the folder's name, so a
        // move to another name in that folder is another move, not this one.
        if basename(&source) == basename(&target) {
            if let Some(outcome) = self
                .resume_folder_job("moveFolder", &source, &parent_path(&target))
                .await
            {
                return outcome;
            }
        }
        let entry = self.stat(&source).await?;
        // A new name ImageKit would store as another one is refused before
        // anything is asked of the destination or changed.
        if basename(&source) != basename(&target) {
            let use_ = if entry.is_dir {
                NameUse::RenameFolder
            } else {
                NameUse::RenameFile
            };
            refuse_rewritten_name(basename(&target), use_)?;
        }
        // The listing can omit stored entries (including newly created
        // folders). find_entry therefore refuses an unverified destination
        // rather than treating an unlisted name as free. Positive matches
        // still allow an explicitly requested compatible file replacement.
        let occupant = match self.find_entry(&target).await {
            Ok(found) => Some(file_to_entry(&found)),
            Err(ProviderError::NotFound(_)) => None,
            Err(ProviderError::NotSupported(_))
                if !overwrite && parent_path(&source) == parent_path(&target) =>
            {
                // Only same-folder rename has a documented collision409.
                // Move APIs append versions/merge and have no such guard.
                if entry.is_dir {
                    return self.rename_folder_job(&source, basename(&target)).await;
                }
                return self.rename_file(&source, basename(&target)).await;
            }
            Err(e) => return Err(e),
        };
        if let Some(occupant) = occupant {
            if !overwrite {
                return Err(ProviderError::AlreadyExists(to.to_string()));
            }
            super::refuse_replace_across_types(to, entry.is_dir, occupant.is_dir)?;
            if entry.is_dir {
                return Err(ProviderError::NotSupported(format!(
                    "{to} is a folder, and ImageKit's moveFolder would merge {from} into it \
                     instead of replacing it: nothing was changed"
                )));
            }
        }
        if entry.is_dir {
            let src_name = basename(&source);
            let dest_name = basename(&target);
            if src_name == dest_name {
                return self
                    .start_folder_job("moveFolder", &source, &parent_path(&target), false)
                    .await;
            }
            let src_parent = parent_path(&source);
            let dest_parent = parent_path(&target);
            if folder_path(&src_parent) == folder_path(&dest_parent) {
                return self.rename_folder_job(&source, dest_name).await;
            }
            // A rename and a move are two jobs: rename in place, then move.
            // The renamed folder must not meet a folder of that name first.
            let renamed = format!("{}/{dest_name}", src_parent.trim_end_matches('/'));
            if self.path_exists(&renamed).await? {
                return Err(ProviderError::Other(format!(
                    "Cannot move {source} to {target} in two steps: {renamed} already exists, \
                     so the folder cannot be renamed in place first"
                )));
            }
            self.rename_folder_job(&source, dest_name).await?;
            self.start_folder_job("moveFolder", &renamed, &dest_parent, false)
                .await
                .map_err(|e| {
                    // The move job may only be unfinished, not failed: say
                    // where the folder is until it completes, and that a
                    // retry of the original rename cannot find it any more.
                    ProviderError::Other(format!(
                        "renamed {source} to {renamed}, but moving it to {dest_parent} did not \
                         complete, so until it does the folder is at {renamed}; move it from \
                         there, since {source} no longer exists: {e}"
                    ))
                })
        } else {
            self.move_file(&source, &target, overwrite).await
        }
    }

    /// Rename the folder `source` in place to `new_name` with the
    /// `renameFolder` job, and wait for it like the other folder jobs. The
    /// CDN cache of the old URLs is not purged (it would count against the
    /// account's monthly purge quota).
    async fn rename_folder_job(&self, source: &str, new_name: &str) -> Result<(), ProviderError> {
        let renamed = format!("{}/{new_name}", parent_path(source).trim_end_matches('/'));
        if let Some(outcome) = self
            .resume_folder_job("renameFolder", source, &renamed)
            .await
        {
            return outcome;
        }
        let folder = normalize_path(source);
        let resp = self
            .auth(
                self.client
                    .post(format!("{}/bulkJobs/renameFolder", self.api_base())),
            )
            .json(&RenameFolderRequest {
                folder_path: folder.trim_end_matches('/'),
                new_folder_name: new_name,
                purge_cache: false,
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }
        let job = resp
            .json::<IkBulkJob>()
            .await
            .map_http_err(|e| ProviderError::ParseError(format!("bulk job response: {e}")))?;
        if job.job_id.is_empty() {
            return Err(ProviderError::ParseError(
                "ImageKit queued the folder rename without a jobId, so its outcome cannot be \
                 checked"
                    .to_string(),
            ));
        }
        tracing::debug!("ImageKit folder rename job started: {}", job.job_id);
        self.wait_remembering(
            &Self::folder_job_key("renameFolder", source, &renamed),
            &job.job_id,
        )
        .await
    }

    fn folder_job_key(endpoint: &str, source: &str, destination: &str) -> String {
        format!(
            "{endpoint} {} {}",
            folder_path(source),
            folder_path(destination)
        )
    }

    /// Wait for the job an earlier call queued for this same folder job and
    /// could not see finish, if there is one.
    async fn resume_folder_job(
        &self,
        endpoint: &str,
        source: &str,
        destination: &str,
    ) -> Option<Result<(), ProviderError>> {
        let key = Self::folder_job_key(endpoint, source, destination);
        let job_id = self
            .unfinished_folder_jobs
            .lock()
            .ok()?
            .get(&key)
            .cloned()?;
        Some(self.wait_remembering(&key, &job_id).await)
    }

    /// Wait for `job_id`. Remember it under `key` only while it may still
    /// complete (still pending, or a poll that failed in transport or with a
    /// server error); forget it once it completed or is over for good (a job
    /// ImageKit does not know, a status it does not document), so a later
    /// call queues a job of its own instead of polling a dead one forever.
    async fn wait_remembering(&self, key: &str, job_id: &str) -> Result<(), ProviderError> {
        let outcome = self.wait_for_folder_job(job_id, FOLDER_JOB_WAIT).await;
        if let Ok(mut jobs) = self.unfinished_folder_jobs.lock() {
            match &outcome {
                FolderJob::Unfinished(_) => {
                    jobs.insert(key.to_string(), job_id.to_string());
                }
                FolderJob::Completed | FolderJob::Over(_) => {
                    jobs.remove(key);
                }
            }
        }
        match outcome {
            FolderJob::Completed => Ok(()),
            FolderJob::Unfinished(e) | FolderJob::Over(e) => Err(e),
        }
    }

    async fn start_folder_job(
        &self,
        endpoint: &str,
        from: &str,
        to: &str,
        include_versions: bool,
    ) -> Result<(), ProviderError> {
        if let Some(outcome) = self.resume_folder_job(endpoint, from, to).await {
            return outcome;
        }
        let source = folder_path(from);
        let destination = folder_path(to);
        let resp = self
            .auth(
                self.client
                    .post(format!("{}/bulkJobs/{}", self.api_base(), endpoint)),
            )
            .json(&BulkFolderRequest {
                source_folder_path: &source,
                destination_path: &destination,
                include_file_versions: include_versions,
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        // The POST answers as soon as the job is queued. Returning then told
        // the caller a folder had moved while it still sat at the source.
        let job = resp
            .json::<IkBulkJob>()
            .await
            .map_http_err(|e| ProviderError::ParseError(format!("bulk job response: {e}")))?;
        if job.job_id.is_empty() {
            return Err(ProviderError::ParseError(
                "ImageKit queued the folder job without a jobId, so its outcome cannot be checked"
                    .to_string(),
            ));
        }
        tracing::debug!("ImageKit folder job started: {}", job.job_id);
        self.wait_remembering(&Self::folder_job_key(endpoint, from, to), &job.job_id)
            .await
    }

    /// Poll `GET /bulkJobs/{jobId}` (official SDK, `folders/job.ts`) until
    /// the job is `Completed`. A job still `Pending` after `budget` is an
    /// error that says so: the folder may yet finish moving, and the caller
    /// must not take it as done.
    async fn wait_for_folder_job(&self, job_id: &str, budget: std::time::Duration) -> FolderJob {
        let started = std::time::Instant::now();
        let mut interval = FOLDER_JOB_FIRST_POLL;
        // The job is queued before the first poll: a poll that fails in
        // transport or with a server error says nothing about the job, which
        // may still complete.
        let unknown = |e: ProviderError| {
            FolderJob::Unfinished(ProviderError::Other(format!(
                "ImageKit folder job {job_id} was queued, but checking on it failed ({e}); it \
                 may still complete, check the destination before retrying"
            )))
        };
        loop {
            let resp = match self
                .auth(self.client.get(format!(
                    "{}/bulkJobs/{}",
                    self.api_base(),
                    urlencoding::encode(job_id)
                )))
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    return unknown(crate::providers::http_send_error(e, |e| {
                        ProviderError::NetworkError(e.to_string())
                    }))
                }
            };
            let status = resp.status();
            if !status.is_success() {
                // A refusal to show the job (401, 403: a key that expired or
                // lost a permission) says nothing about the job itself, which
                // may still be running: like a server error, it is unknown.
                let unknown_status = status.is_server_error()
                    || status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::UNAUTHORIZED
                    || status == StatusCode::FORBIDDEN;
                let error = self.parse_error(resp).await;
                if unknown_status {
                    return unknown(error);
                }
                // A job ImageKit does not know (404) is over for this
                // session: nothing is left to wait for.
                return FolderJob::Over(ProviderError::Other(format!(
                    "ImageKit no longer reports folder job {job_id} ({error}); check the \
                     destination to see whether it ran"
                )));
            }
            let job = match resp.json::<IkBulkJobStatus>().await {
                Ok(job) => job,
                Err(e) => {
                    return unknown(crate::providers::http_send_error(e, |e| {
                        ProviderError::ParseError(format!("bulk job status: {e}"))
                    }))
                }
            };
            match job.status.as_str() {
                "Completed" => return FolderJob::Completed,
                "Pending" if started.elapsed() < budget => {}
                "Pending" => {
                    return FolderJob::Unfinished(ProviderError::Other(format!(
                        "ImageKit folder job {job_id} is still pending after {} s; \
                         it may complete later, check the destination before retrying",
                        budget.as_secs()
                    )))
                }
                other => {
                    return FolderJob::Over(ProviderError::ServerError(format!(
                        "ImageKit folder job {job_id} reports the unknown status {other:?}"
                    )))
                }
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(FOLDER_JOB_MAX_POLL);
        }
    }
}

#[async_trait]
impl StorageProvider for ImageKitProvider {
    fn sync_namespace_identity(&self) -> Option<String> {
        use sha2::Digest;
        let key = self.config.private_key.expose_secret();
        if !self.connected || key.is_empty() {
            return None;
        }
        // ImageKit's URL id is not verified by API authentication. Bind to the
        // high-entropy API credential actually used instead; rotation revokes
        // old sync authority. Neither raw key nor a user password is persisted.
        let mut digest = sha2::Sha256::new();
        digest.update(b"aeroftp/imagekit/sync-credential/v1\0");
        digest.update(key.as_bytes());
        Some(format!("{:x}", digest.finalize()))
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::ImageKit
    }

    fn display_name(&self) -> String {
        "ImageKit".to_string()
    }

    fn listing_is_authoritative(&self) -> bool {
        // Live verification on 2026-08-01: upload returned a valid fileId and
        // CDN URL (the exact bytes were served), while every Media Library
        // List query omitted the object. Absence from this listing therefore
        // cannot authorise deleting a local file during sync.
        false
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let resp = self
            .auth(
                self.client
                    .get(format!("{}/files?limit=1&skip=0", self.api_base())),
            )
            .send()
            .await
            .map_http_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let items = self.list_raw(&resolved).await?;
        Ok(items.iter().map(file_to_entry).collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let entry = self.stat(&resolved).await?;
        if !entry.is_dir {
            return Err(ProviderError::InvalidPath(format!(
                "'{}' is not a directory",
                resolved
            )));
        }
        self.current_path = resolved;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.current_path = parent_path(&self.current_path);
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let entry = self.stat(remote_path).await?;
        if entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Cannot download a directory as a file".to_string(),
            ));
        }
        let url = entry.metadata.get("url").cloned().ok_or_else(|| {
            ProviderError::ParseError("ImageKit file metadata has no download URL".to_string())
        })?;
        validate_download_url(&url)?;

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        let total = resp.content_length().unwrap_or(entry.size);
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut downloaded = 0u64;
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total);
            }
        }
        atomic.commit().await.map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let entry = self.stat(remote_path).await?;
        if entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Cannot download a directory as bytes".to_string(),
            ));
        }
        let url = entry.metadata.get("url").cloned().ok_or_else(|| {
            ProviderError::ParseError("ImageKit file metadata has no download URL".to_string())
        })?;
        validate_download_url(&url)?;
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }
        response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await
    }

    /// Ranged read for remote archive/encryption surfacing (header/tail windows).
    /// The ImageKit delivery CDN serves byte ranges over a plain HTTP `Range`
    /// header; if a range is ever ignored (200 OK), slice the full body locally.
    async fn read_range(
        &mut self,
        remote_path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        let entry = self.stat(remote_path).await?;
        if entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Cannot read a range from a directory".to_string(),
            ));
        }
        let url = entry.metadata.get("url").cloned().ok_or_else(|| {
            ProviderError::ParseError("ImageKit file metadata has no download URL".to_string())
        })?;
        validate_download_url(&url)?;
        // offset + len - 1 must not wrap: a wrapped end makes an invalid Range
        // header that servers ignore, turning a bounded probe into a full-body
        // download. (len == 0 already returned above.)
        let end = offset
            .checked_add(len - 1)
            .ok_or_else(|| ProviderError::Other("read_range end overflows u64".to_string()))?;
        let resp = self
            .client
            .get(&url)
            .header("Range", format!("bytes={}-{}", offset, end))
            .send()
            .await
            .map_err(super::multi_thread::range_read_error)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Range download failed: HTTP {}",
                status
            )));
        }
        let answered = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        let bytes = super::range_window_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await?;
        // A 206 that does not name the window it carries is written at this
        // offset just the same, so it has to say which range it is.
        match super::multi_thread::ranged_answer(
            status,
            answered.as_deref(),
            bytes.len() as u64,
            offset,
            end,
        ) {
            Ok(super::multi_thread::RangedAnswer::Window) => Ok(bytes),
            Ok(super::multi_thread::RangedAnswer::WholeObject) => {
                Ok(super::multi_thread::slice_whole_object(&bytes, offset, len))
            }
            Err(why) => Err(ProviderError::ParallelRefused(
                super::multi_thread::parallel_refused("ImageKit range read", remote_path, &why),
            )),
        }
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.upload_file(local_path, remote_path, on_progress, true)
            .await
    }

    fn supports_upload_if_absent(&self) -> bool {
        true
    }

    async fn upload_if_absent(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.upload_file(local_path, remote_path, on_progress, false)
            .await
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let name = basename(&resolved);
        if name.is_empty() {
            return Ok(());
        }
        refuse_rewritten_name(name, NameUse::CreateFolder)?;
        let parent = folder_path(&parent_path(&resolved));

        let resp = self
            .auth(self.client.post(format!("{}/folder/", self.api_base())))
            .json(&FolderRequest {
                folder_name: name,
                parent_folder_path: &parent,
            })
            .send()
            .await
            .map_http_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let entry = self.stat(&resolved).await?;
        if entry.is_dir {
            self.rmdir(&resolved).await
        } else {
            self.delete_file_by_path(&resolved).await
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        self.refuse_non_empty_dir(path).await?;
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        self.delete_folder_by_path(&resolved).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let mut stack = vec![resolved.clone()];
        let mut dirs = Vec::new();

        while let Some(dir) = stack.pop() {
            let listed: Vec<RemoteEntry> = self
                .list_raw(&dir)
                .await?
                .iter()
                .map(file_to_entry)
                .collect();
            // Every entry the listing names must be inside `dir` before any
            // of it is deleted: a folder this API spelled `/` sent the walk
            // back to the account root, which it emptied and never left.
            for entry in crate::sync_core::listing_to_remove(&dir, listed)? {
                if entry.is_dir {
                    stack.push(entry.path);
                } else {
                    self.delete_file_by_path(&entry.path).await?;
                }
            }
            dirs.push(dir);
        }

        for dir in dirs.into_iter().rev() {
            self.delete_folder_by_path(&dir).await?;
        }
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.move_entry(from, to, false).await
    }

    /// A file moved into another folder lands on top of the one at `to` as
    /// its newest version, the version history kept: see `move_entry`. In
    /// one folder ImageKit only renames, and its rename refuses a taken name
    /// (409, AlreadyExists), so a replace there is refused, as it was before.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.move_entry(from, to, true).await
    }

    /// No. The callers that need atomicity (CLI `edit`, MCP `remote_edit`,
    /// the crypt marker paths) stage their temporary next to the target, and
    /// in one folder ImageKit only renames, which refuses a taken name: the
    /// edit uploaded its temporary and then failed on the replace (found
    /// live on 2026-09-26). Answering no makes them refuse before they write
    /// anything. A replace across folders still lands in one step.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        self.find_entry(&resolved)
            .await
            .map(|item| file_to_entry(&item))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!("ImageKit endpoint: {}", self.config.imagekit_id))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `copyFile` / `copyFolder` implementation lives
        // on `server_side_copy`.
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let source = self.resolve_path(from);
        let target = self.resolve_path(to);
        let entry = self.stat(&source).await?;
        if entry.is_dir {
            let destination_parent = folder_copy_destination(&source, &target)?;
            self.start_folder_job("copyFolder", &source, &destination_parent, false)
                .await
        } else {
            self.copy_file(&source, &target).await
        }
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let entry = self.stat(path).await?;
        entry
            .metadata
            .get("thumbnail_url")
            .or_else(|| entry.metadata.get("url"))
            .cloned()
            .ok_or_else(|| {
                ProviderError::ParseError(
                    "ImageKit file metadata has no thumbnail or download URL".to_string(),
                )
            })
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let root = self.resolve_path(path);
        let mut walk = crate::sync_core::FindWalk::new(&root);
        let mut stack = vec![root.clone()];
        let mut matches = Vec::new();
        while let Some(dir) = stack.pop() {
            let listed = self
                .list_raw(&dir)
                .await?
                .iter()
                .map(file_to_entry)
                .collect();
            let sorted = walk.sort(&dir, listed);
            // A listing error fails the search, and so does a folder it
            // cannot follow: either way the search did not see the folder.
            if let Some((folder, step)) = sorted.refused.first() {
                return Err(crate::sync_core::refused_search(&root, &dir, folder, *step));
            }
            stack.extend(sorted.follow);
            matches.extend(
                sorted
                    .entries
                    .into_iter()
                    .filter(|entry| super::matches_find_pattern(&entry.name, pattern)),
            );
        }
        Ok(matches)
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T06): intentionally not advertised.
        //
        // ImageKit's upload API is a single-POST `multipart/form-data` against
        // `/api/v1/files/upload` carrying the file part, the per-call
        // `signature`/`token`/`expire` triplet, and the optional folder/tags
        // metadata. The API does not document a chunked append/commit flow:
        // every upload is one atomic POST whose body is the whole file.
        //
        // A fake-multipart wrapper buffering chunks in memory would defeat
        // streaming uploads without producing any per-chunk retry, so the
        // trait is left as the `NotSupported` default and the runner falls
        // back to the legacy `upload()` path (which already streams the body).
        TransferOptimizationHints {
            supports_range_download: true,
            supports_server_checksum: false,
            supports_resume_download: true,
            ..TransferOptimizationHints::default()
        }
    }
}

fn file_to_entry(item: &IkFile) -> RemoteEntry {
    let is_dir = item.entry_type == "folder";
    let mut metadata = HashMap::new();
    if !item.file_id.is_empty() {
        metadata.insert("file_id".to_string(), item.file_id.clone());
    }
    if let Some(url) = &item.url {
        metadata.insert("url".to_string(), url.clone());
    }
    if let Some(url) = &item.thumbnail_url {
        metadata.insert("thumbnail_url".to_string(), url.clone());
    }
    if let Some(kind) = &item.file_type {
        metadata.insert("file_type".to_string(), kind.clone());
    }
    if let Some(height) = item.height {
        metadata.insert("height".to_string(), height.to_string());
    }
    if let Some(width) = item.width {
        metadata.insert("width".to_string(), width.to_string());
    }
    if let Some(is_private) = item.is_private_file {
        metadata.insert("is_private_file".to_string(), is_private.to_string());
    }
    if !item.tags.is_empty() {
        metadata.insert("tags".to_string(), item.tags.join(","));
    }

    RemoteEntry {
        name: item.name.clone(),
        path: normalize_path(&item.file_path),
        is_dir,
        size: if is_dir { 0 } else { item.size },
        modified: item.updated_at.clone().or_else(|| item.created_at.clone()),
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: item.mime.clone(),
        metadata,
    }
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    if path.contains('\0') {
        return Err(ProviderError::InvalidPath(
            "Path contains null byte".to_string(),
        ));
    }
    for component in path.split('/') {
        if component == ".." {
            return Err(ProviderError::InvalidPath(
                "Path traversal (..) not allowed".to_string(),
            ));
        }
    }
    Ok(())
}

fn normalize_path(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// copyFolder preserves the source leaf; reject requests it cannot honor
/// before starting a job that would write to a different destination.
fn folder_copy_destination(source: &str, target: &str) -> Result<String, ProviderError> {
    if basename(source) != basename(target) {
        return Err(ProviderError::NotSupported(
            "ImageKit folder copy preserves the source folder name; copying to a different folder name is not supported".to_string(),
        ));
    }
    Ok(parent_path(target))
}

/// What ImageKit does with a name it is given: it keeps the letters, marks
/// and digits of the Basic Multilingual Plane, `-`, `_` and, in a file name,
/// `.`, and stores every other character as `_`, one for each. Measured on the
/// API on 2026-10-31, 68 characters one by one:
///
/// - kept: `é` (composed or with a combining accent), `ß`, `日`, `한`, `Ա`,
///   `कि`, `ｱ`, `Ａ`, `ﬁ`, `ǅ`, `ʰ`, `ª`, `Ω` (U+2126), `١`, `²`, `½`, `①`,
///   `Ⅷ`, `〇`, and the marks U+0345, U+20DD and U+0E31;
/// - `_`: space, no-break space, `(`, `&`, `'`, `’`, `·`, the circled letters
///   `Ⓐ` and `ⓐ` (symbols that Unicode counts as alphabetic), and every
///   character above U+FFFF, letters and digits included (`𝔸`, `𐐀`, `𠀀`,
///   `𝟎`, `𑁦`, an emoji): `a𝔸𝔸b` became `a__b`;
/// - a folder named `a.b` was created as `a_b`.
///
/// The upload documentation lists only a-z, A-Z, 0-9, `.` and `-`, fewer than
/// it keeps.
fn imagekit_stored_name(name: &str, folder: bool) -> String {
    name.chars()
        .map(|c| if imagekit_keeps(c, folder) { c } else { '_' })
        .collect()
}

/// Whether ImageKit keeps `c` in a name (see `imagekit_stored_name`). Of the
/// characters of the Basic Multilingual Plane that Rust's `is_alphanumeric`
/// accepts, the circled Latin letters U+24B6 to U+24E9 are the only ones that
/// are not a letter, a mark or a digit.
fn imagekit_keeps(c: char, folder: bool) -> bool {
    let letter_mark_or_digit = u32::from(c) <= 0xFFFF
        && !('\u{24B6}'..='\u{24E9}').contains(&c)
        && (c.is_alphanumeric() || unicode_normalization::char::is_combining_mark(c));
    letter_mark_or_digit || c == '-' || c == '_' || (c == '.' && !folder)
}

/// What a name is given to ImageKit for.
#[derive(Clone, Copy)]
enum NameUse {
    Upload,
    CreateFolder,
    RenameFile,
    RenameFolder,
}

/// A name ImageKit would store as another one is refused before anything is
/// sent. Sent as it is, `a b.txt` was uploaded as `a_b.txt` and, the upload
/// overwriting, replaced the file of that name; a folder or a file renamed
/// to such a name was left under a name nobody asked for.
fn refuse_rewritten_name(name: &str, use_: NameUse) -> Result<(), ProviderError> {
    let folder = matches!(use_, NameUse::CreateFolder | NameUse::RenameFolder);
    let stored = imagekit_stored_name(name, folder);
    if stored == name {
        return Ok(());
    }
    let (would, nothing) = match use_ {
        NameUse::Upload => ("upload the file as", "Nothing was uploaded"),
        NameUse::CreateFolder => ("create the folder as", "Nothing was created"),
        NameUse::RenameFile => ("rename the file to", "Nothing was changed"),
        NameUse::RenameFolder => ("rename the folder to", "Nothing was changed"),
    };
    let keeps = if folder {
        "a folder name there keeps only letters, marks and digits up to U+FFFF, '-' and '_'"
    } else {
        "a file name there keeps only letters, marks and digits up to U+FFFF, '.', '-' and '_'"
    };
    Err(ProviderError::NotSupported(format!(
        "ImageKit would {would} \"{stored}\", not \"{name}\": {keeps}, and every other \
         character becomes '_'. {nothing}"
    )))
}

/// Where a listed entry lives. A file names itself with `filePath`, a folder
/// with `folderPath`; an entry with neither goes under the listed folder by
/// its name. Reading `filePath` alone gave every folder an empty path, which
/// normalizes to `/`: a recursive delete then walked the account root,
/// deleting its files, and never ended. A listing holds the listed folder's
/// own children, so a path that is not one of them is refused here, where
/// every walk of this provider (delete, find, the shared walker through
/// `list`) takes its next folder from.
fn listed_entry_path(item: &IkFile, dir: &str) -> Result<String, ProviderError> {
    let dir = normalize_path(dir);
    let reported = if item.file_path.is_empty() {
        &item.folder_path
    } else {
        &item.file_path
    };
    let path = if reported.is_empty() {
        let name = item.name.as_str();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(ProviderError::ParseError(format!(
                "ImageKit listed an entry under {dir} with no path and no usable name"
            )));
        }
        normalize_path(&format!("{}/{name}", dir.trim_end_matches('/')))
    } else {
        normalize_path(reported)
    };
    if path == dir || parent_path(&path) != dir {
        return Err(ProviderError::ParseError(format!(
            "ImageKit listed {path} under {dir}, which is not one of its children"
        )));
    }
    Ok(path)
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

fn parent_path(path: &str) -> String {
    let normalized = normalize_path(path);
    if normalized == "/" {
        return "/".to_string();
    }
    match normalized.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(idx) => normalized[..idx].to_string(),
    }
}

fn folder_path(path: &str) -> String {
    let normalized = normalize_path(path);
    if normalized == "/" {
        "/".to_string()
    } else {
        format!("{}/", normalized.trim_end_matches('/'))
    }
}

fn validate_download_url(url: &str) -> Result<(), ProviderError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ProviderError::ServerError(format!("Invalid ImageKit URL: {}", e)))?;
    if parsed.scheme() != "https" {
        return Err(ProviderError::ServerError(
            "ImageKit download URL must use https".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn sync_namespace_identity_binds_the_authenticated_api_key_not_display_url() {
        let mut provider = empty_provider();
        assert!(provider.sync_namespace_identity().is_none());
        provider.connected = true;
        let initial = provider.sync_namespace_identity().unwrap();
        assert_eq!(initial.len(), 64);
        provider.config.imagekit_id = "other-display-url".into();
        assert_eq!(provider.sync_namespace_identity().unwrap(), initial);
        provider.config.private_key =
            SecretString::from("rotated-high-entropy-api-key".to_string());
        assert_ne!(provider.sync_namespace_identity().unwrap(), initial);
        provider.config.private_key = SecretString::from(String::new());
        assert!(provider.sync_namespace_identity().is_none());
    }

    #[test]
    fn a_name_keeps_its_leading_and_trailing_spaces() {
        // A name with a leading or trailing space is a name of its own, and a
        // name of spaces only is not the current folder.
        assert_eq!(super::normalize_path("/ a"), "/ a");
        assert_eq!(super::normalize_path("/a /b "), "/a /b ");
        let mut p = super::ImageKitProvider::new(super::ImageKitConfig {
            imagekit_id: "test".into(),
            private_key: secrecy::SecretString::from("test".to_string()),
            initial_path: None,
        });
        p.current_path = "/Cloud".to_string();
        assert_eq!(p.resolve_path("   "), "/Cloud/   ");
        assert_eq!(p.resolve_path(" a"), "/Cloud/ a");
        assert_eq!(p.resolve_path("/a "), "/a ");
    }

    #[tokio::test]
    async fn probe_contract_imagekit_create_only_upload_uses_the_native_guard() {
        use axum::response::IntoResponse;
        let app = axum::Router::new().fallback(axum::routing::post(|request: axum::extract::Request| async move {
            let body = axum::body::to_bytes(request.into_body(), 1 << 20).await.unwrap();
            let text = String::from_utf8_lossy(&body);
            assert!(text.contains("name=\"useUniqueFileName\"\r\n\r\nfalse"), "{text}");
            assert!(text.contains("name=\"overwriteFile\"\r\n\r\nfalse"), "{text}");
            if text.contains("name=\"fileName\"\r\n\r\ntaken") {
                (axum::http::StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"message":"A file with the same name already exists at the exact location. We could not overwrite it because both overwriteFile and useUniqueFileName are set to false."}))).into_response()
            } else if text.contains("name=\"fileName\"\r\n\r\nbad") {
                (axum::http::StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"message":"Your request is missing file parameter."}))).into_response()
            } else { axum::Json(serde_json::json!({"fileId":"new"})).into_response() }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{address}"));
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"payload").unwrap();
        assert!(provider.supports_upload_if_absent());
        assert!(provider
            .upload_if_absent(file.path().to_str().unwrap(), "/fresh", None)
            .await
            .is_ok());
        assert!(matches!(
            provider
                .upload_if_absent(file.path().to_str().unwrap(), "/taken", None)
                .await,
            Err(ProviderError::AlreadyExists(_))
        ));
        assert!(matches!(
            provider
                .upload_if_absent(file.path().to_str().unwrap(), "/bad", None)
                .await,
            Err(ProviderError::InvalidConfig(_))
        ));
        server.abort();
    }

    #[tokio::test]
    async fn probe_contract_imagekit_same_folder_file_rename_lets_the_server_decide() {
        use axum::response::IntoResponse;
        let app = axum::Router::new().fallback(axum::routing::any(|request: axum::extract::Request| async move {
            if request.method() == "GET" {
                axum::Json(serde_json::json!([{"fileId":"source", "name":"source", "filePath":"/source", "type":"file", "size":3},{"folderId":"f","name":"folder","folderPath":"/folder","type":"folder"}])).into_response()
            } else {
                let body = axum::body::to_bytes(request.into_body(), 1 << 20).await.unwrap();
                let request:serde_json::Value=serde_json::from_slice(&body).unwrap();
                if request["newFileName"] == "taken" || request["newFolderName"] == "taken" {
                    (axum::http::StatusCode::CONFLICT,axum::Json(serde_json::json!({"reason":"FILE_ALREADY_EXISTS","message":"File with name taken already exists at the same location."}))).into_response()
                } else { axum::Json(serde_json::json!({})).into_response() }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{address}"));
        assert!(matches!(
            provider.rename("/source", "/taken").await,
            Err(ProviderError::AlreadyExists(_))
        ));
        provider.rename("/source", "/fresh").await.unwrap();
        assert!(matches!(
            provider.rename("/folder", "/taken").await,
            Err(ProviderError::AlreadyExists(_))
        ));
        server.abort();
    }

    #[tokio::test]
    async fn probe_contract_imagekit_empty_partial_list_cannot_authorize_rmdir() {
        let (mut provider, deletes) = provider_on_listings(&[("/dir", "[]")]).await;
        let outcome = provider.rmdir("/dir").await;
        assert!(
            deletes.lock().unwrap().is_empty(),
            "an omitted child must not be deleted"
        );
        assert!(
            matches!(outcome, Err(ProviderError::NotSupported(_))),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn probe_contract_imagekit_unlisted_name_is_not_proven_absent() {
        let (mut provider, _) = provider_on_listings(&[("/", "[]")]).await;
        assert!(!provider.listing_is_authoritative());
        assert!(matches!(
            provider.exists("/unlisted").await,
            Err(ProviderError::NotSupported(_))
        ));
        assert!(matches!(
            provider.stat("/unlisted").await,
            Err(ProviderError::NotSupported(_))
        ));
    }

    #[tokio::test]
    async fn probe_contract_imagekit_missing_url_is_a_metadata_error() {
        let (mut provider, _) = provider_on_listings(&[(
            "/",
            r#"[{"fileId":"f","name":"present","filePath":"/present","type":"file","size":3}]"#,
        )])
        .await;
        assert!(provider.stat("/present").await.is_ok());
        let outcome = provider.read_range("/present", 0, 1).await;
        assert!(
            matches!(outcome, Err(ProviderError::ParseError(_))),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn probe_contract_imagekit_gui_upload_executor_sends_every_plain_folder_file() {
        use crate::provider_transfer_executor::{
            ProviderExecutorSessionModel, ProviderUploadExecutor,
        };
        use crate::transfer_orchestrator::TransferExecutor;
        use std::sync::{Arc, Mutex};
        type RequestLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
        let requests: RequestLog = Arc::default();
        let seen = requests.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let seen = seen.clone();
                async move {
                    let path = request.uri().path().to_string();
                    let body = axum::body::to_bytes(request.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    seen.lock().unwrap().push((path.clone(), body.to_vec()));
                    if path == "/files" {
                        axum::Json(serde_json::json!([]))
                    } else {
                        axum::Json(serde_json::json!({"fileId":"uploaded"}))
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        provider.mkdir("/target").await.unwrap();
        provider.mkdir("/target/sub").await.unwrap();
        let capabilities = provider.transfer_capabilities();
        let executor = ProviderUploadExecutor::new(
            Arc::new(crate::transfer_event_sink::NoopTransferSink),
            Arc::new(tokio::sync::Mutex::new(Some(
                Box::new(provider) as Box<dyn StorageProvider>
            ))),
            crate::transfer_settings::ResolvedTransferSettings {
                requested_max_concurrent: 1,
                max_concurrent: 1,
                retry_count: 0,
                timeout_seconds: 10,
                download_segments: crate::transfer_settings::ResolvedDownloadSegments::explicit(1),
                sftp_download_preset: None,
            },
            None,
            tokio_util::sync::CancellationToken::new(),
            ProviderExecutorSessionModel::LockedSingle {
                provider_type: Some(ProviderType::ImageKit),
            },
            capabilities,
        );
        let temp = tempfile::tempdir().unwrap();
        for (index, remote, bytes) in [
            (0, "/target/a.txt", b"first payload".as_slice()),
            (1, "/target/sub/b.txt", b"second payload".as_slice()),
        ] {
            let local = temp.path().join(format!("file-{index}"));
            std::fs::write(&local, bytes).unwrap();
            let result = executor
                .execute(crate::transfer_domain::TransferEntry {
                    id: index.to_string(),
                    display_name: format!("file-{index}"),
                    local_path: local.to_string_lossy().into_owned(),
                    remote_path: remote.into(),
                    size: bytes.len() as u64,
                    modified: None,
                })
                .await;
            assert!(
                matches!(result, crate::transfer_domain::TransferOutcome::Success),
                "{result:?}"
            );
        }
        server.abort();
        let requests = requests.lock().unwrap();
        let uploads: Vec<_> = requests
            .iter()
            .filter(|(path, _)| path == "/upload")
            .collect();
        assert_eq!(uploads.len(), 2);
        assert!(uploads[0]
            .1
            .windows(b"first payload".len())
            .any(|part| part == b"first payload"));
        assert!(uploads[1]
            .1
            .windows(b"second payload".len())
            .any(|part| part == b"second payload"));
        assert!(
            requests.iter().all(|(path, _)| path != "/files"),
            "plain GUI upload must not require a missing-name proof"
        );
    }
    #[tokio::test]
    async fn probe_contract_imagekit_gui_skip_uses_native_guard() {
        use crate::provider_transfer_executor::{
            ProviderExecutorSessionModel, ProviderUploadExecutor,
        };
        use crate::transfer_orchestrator::TransferExecutor;
        use std::sync::{Arc, Mutex};
        type RequestLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
        let requests: RequestLog = Arc::default();
        let seen = requests.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let seen = seen.clone();
                async move {
                    let path = request.uri().path().to_string();
                    let body = axum::body::to_bytes(request.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    seen.lock().unwrap().push((path.clone(), body.to_vec()));
                    use axum::response::IntoResponse;
                    if path == "/upload" && String::from_utf8_lossy(&body).contains("name=\"fileName\"\r\n\r\nb.txt") {
                        (axum::http::StatusCode::BAD_REQUEST,axum::Json(serde_json::json!({"message":"A file with the same name already exists at the exact location. We could not overwrite it because both overwriteFile and useUniqueFileName are set to false."}))).into_response()
                    } else if path == "/files" {
                        axum::Json(serde_json::json!([])).into_response()
                    } else {
                        axum::Json(serde_json::json!({"fileId":"uploaded"})).into_response()
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        provider.mkdir("/target").await.unwrap();
        provider.mkdir("/target/sub").await.unwrap();
        let capabilities = provider.transfer_capabilities();
        let executor = ProviderUploadExecutor::new(
            Arc::new(crate::transfer_event_sink::NoopTransferSink),
            Arc::new(tokio::sync::Mutex::new(Some(
                Box::new(provider) as Box<dyn StorageProvider>
            ))),
            crate::transfer_settings::ResolvedTransferSettings {
                requested_max_concurrent: 1,
                max_concurrent: 1,
                retry_count: 0,
                timeout_seconds: 10,
                download_segments: crate::transfer_settings::ResolvedDownloadSegments::explicit(1),
                sftp_download_preset: None,
            },
            None,
            tokio_util::sync::CancellationToken::new(),
            ProviderExecutorSessionModel::LockedSingle {
                provider_type: Some(ProviderType::ImageKit),
            },
            capabilities,
        )
        .with_no_clobber(true);
        let temp = tempfile::tempdir().unwrap();
        for (index, remote, bytes) in [
            (0, "/target/a.txt", b"first payload".as_slice()),
            (1, "/target/sub/b.txt", b"second payload".as_slice()),
        ] {
            let local = temp.path().join(format!("file-{index}"));
            std::fs::write(&local, bytes).unwrap();
            let result = executor
                .execute(crate::transfer_domain::TransferEntry {
                    id: index.to_string(),
                    display_name: format!("file-{index}"),
                    local_path: local.to_string_lossy().into_owned(),
                    remote_path: remote.into(),
                    size: bytes.len() as u64,
                    modified: None,
                })
                .await;
            assert!(
                matches!(
                    (&index, &result),
                    (0, crate::transfer_domain::TransferOutcome::Success)
                        | (1, crate::transfer_domain::TransferOutcome::Skipped { .. })
                ),
                "{result:?}"
            );
        }
        server.abort();
        let requests = requests.lock().unwrap();
        let uploads: Vec<_> = requests
            .iter()
            .filter(|(path, _)| path == "/upload")
            .collect();
        assert_eq!(uploads.len(), 2);
        for (_, body) in &uploads {
            assert!(String::from_utf8_lossy(body).contains("name=\"overwriteFile\"\r\n\r\nfalse"));
        }
        assert!(uploads[0]
            .1
            .windows(b"first payload".len())
            .any(|part| part == b"first payload"));
        assert!(uploads[1]
            .1
            .windows(b"second payload".len())
            .any(|part| part == b"second payload"));
        assert!(
            requests.iter().all(|(path, _)| path != "/files"),
            "plain GUI upload must not require a missing-name proof"
        );
    }
    #[tokio::test]
    async fn probe_contract_imagekit_searches_beyond_the_first_page() {
        let app =
            axum::Router::new().fallback(axum::routing::get(|uri: axum::http::Uri| async move {
                let second = uri
                    .query()
                    .unwrap_or("")
                    .split('&')
                    .any(|q| q == "skip=1000");
                let names: Vec<_> = if second {
                    vec!["target".to_string()]
                } else {
                    (0..1000).map(|i| format!("first-{i}")).collect()
                };
                axum::Json(names.into_iter().map(|name| serde_json::json!({
                    "fileId":name, "filePath":format!("/{name}"), "name":name,"type":"file"
                })).collect::<Vec<_>>())
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut p = ImageKitProvider::new(ImageKitConfig {
            imagekit_id: "test".into(),
            private_key: SecretString::from("test".to_string()),
            initial_path: None,
        });
        p.api_base_override = Some(format!("http://{addr}"));
        p.connected = true;
        let outcome = p.exists("/target").await;
        server.abort();
        assert!(
            outcome.unwrap(),
            "the first page is not the whole directory"
        );
    }

    use super::*;

    #[test]
    fn folder_copy_rejects_a_different_destination_leaf() {
        assert!(matches!(
            folder_copy_destination("/src/photos", "/dst/renamed"),
            Err(ProviderError::NotSupported(_))
        ));
        assert_eq!(
            folder_copy_destination("/src/photos", "/dst/photos").unwrap(),
            "/dst"
        );
    }

    /// An ImageKit double that answers the listing of each folder in
    /// `listings` with the JSON array given for it (any other folder lists
    /// empty) and records every DELETE, as `DELETE path body`.
    async fn provider_on_listings(
        listings: &'static [(&'static str, &'static str)],
    ) -> (
        ImageKitProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use std::sync::{Arc, Mutex};
        let deletes: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&deletes);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let method = req.method().as_str().to_string();
                    let url = reqwest::Url::parse(&format!("http://h{}", req.uri())).unwrap();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    let reply = if method == "GET" && url.path() == "/files" {
                        let folder = url
                            .query_pairs()
                            .find(|(k, _)| k == "path")
                            .map(|(_, v)| normalize_path(&v))
                            .unwrap_or_default();
                        listings
                            .iter()
                            .find(|(listed, _)| *listed == folder)
                            .map_or("[]", |(_, json)| *json)
                            .to_string()
                    } else {
                        if method == "DELETE" {
                            seen.lock().unwrap().push(format!(
                                "DELETE {} {}",
                                url.path(),
                                String::from_utf8_lossy(&body)
                            ));
                        }
                        "{}".to_string()
                    };
                    axum::response::Response::builder()
                        .status(200)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(reply))
                        .unwrap()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, deletes)
    }

    /// The account root holds `keep.png`; the listing of `/tree` names a
    /// folder spelled `/` (the old reading of a folder without `filePath`),
    /// one spelled as `/tree` itself, and one outside it.
    const STRAY_FOLDERS: &[(&str, &str)] = &[
        (
            "/",
            r#"[{"fileId":"k","name":"keep.png","filePath":"/keep.png","type":"file"},
                {"name":"tree","filePath":"/tree","type":"folder"}]"#,
        ),
        (
            "/tree",
            r#"[{"fileId":"a","name":"a.png","filePath":"/tree/a.png","type":"file"},
                {"name":"sub","filePath":"/","type":"folder"},
                {"name":"tree","filePath":"/tree","type":"folder"},
                {"name":"other","filePath":"/other","type":"folder"}]"#,
        ),
        (
            "/other",
            r#"[{"fileId":"o","name":"o.png","filePath":"/other/o.png","type":"file"}]"#,
        ),
    ];

    /// A recursive delete deletes only inside its folder, and ends, whatever
    /// folder paths the listing gives. On a listing that names a folder
    /// outside it, it fails rather than report a partial delete as done.
    #[tokio::test]
    async fn a_recursive_delete_never_follows_a_folder_outside_it() {
        let (mut provider, deletes) = provider_on_listings(STRAY_FOLDERS).await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.rmdir_recursive("/tree"),
        )
        .await
        .expect("the delete ends");
        let deletes = deletes.lock().unwrap().clone();
        assert!(
            deletes
                .iter()
                .all(|call| !call.contains("/files/k") && !call.contains("/files/o")),
            "{outcome:?} {deletes:?}"
        );
        assert!(
            deletes
                .iter()
                .all(|call| !call.contains(r#""folderPath":"/""#) && !call.contains("/other/")),
            "{outcome:?} {deletes:?}"
        );
        assert!(outcome.is_err(), "{deletes:?}");
    }

    /// A search ends, never reports what lies outside the folder searched,
    /// and fails on a listing that names a folder it cannot follow, as on
    /// one it cannot list.
    #[tokio::test]
    async fn a_find_never_follows_a_folder_outside_it() {
        let (mut provider, _) = provider_on_listings(STRAY_FOLDERS).await;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.find("/tree", "*.png"),
        )
        .await
        .expect("the search ends");
        assert!(outcome.is_err(), "{outcome:?}");
    }

    /// An ImageKit double holding the folder `/src/photos` (listed only
    /// under `/src`, as the API lists a folder's children). A folder move
    /// queues job `J`, whose status reads `Pending` for the first
    /// `pending_polls` polls and `Completed` after. Returns the provider and
    /// the number of status polls.
    async fn provider_with_folder_job(
        pending_polls: usize,
    ) -> (
        ImageKitProvider,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let polls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&polls);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let listing_src = req.uri().query().unwrap_or("").contains("path=%2Fsrc&");
                    let body = match (req.method().as_str(), req.uri().path()) {
                        ("GET", "/files") if listing_src => serde_json::json!([{
                            "folderId": "F", "name": "photos", "folderPath": "/src/photos", "type": "folder",
                        }]),
                        ("GET", "/files") => serde_json::json!([]),
                        ("POST", "/bulkJobs/moveFolder") => serde_json::json!({ "jobId": "J" }),
                        ("GET", "/bulkJobs/J") => {
                            let done = seen.fetch_add(1, Ordering::SeqCst) >= pending_polls;
                            serde_json::json!({
                                "jobId": "J",
                                "type": "MOVE_FOLDER",
                                "status": if done { "Completed" } else { "Pending" },
                            })
                        }
                        _ => serde_json::json!({ "message": "unexpected" }),
                    };
                    axum::Json(body)
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, polls)
    }

    /// An ImageKit double holding the folder `/src/photos`, which completes
    /// every folder job at once and records each `POST` to `/bulkJobs` with
    /// its body.
    async fn provider_recording_folder_jobs() -> (
        ImageKitProvider,
        std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    ) {
        use std::sync::Arc;
        let posts: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>> = Arc::default();
        let seen = Arc::clone(&posts);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let listing_src = req.uri().query().unwrap_or("").contains("path=%2Fsrc&");
                    let method = req.method().as_str().to_string();
                    let path = req.uri().path().to_string();
                    let body: serde_json::Value = serde_json::from_slice(
                        &axum::body::to_bytes(req.into_body(), 64 * 1024).await.unwrap(),
                    )
                    .unwrap_or_default();
                    let reply = match (method.as_str(), path.as_str()) {
                        ("GET", "/files") if listing_src => serde_json::json!([{
                            "folderId": "F", "name": "photos", "folderPath": "/src/photos", "type": "folder",
                        }]),
                        ("GET", "/files") => serde_json::json!([]),
                        ("POST", job) if job.starts_with("/bulkJobs/") => {
                            seen.lock().unwrap().push((job.to_string(), body));
                            serde_json::json!({ "jobId": "J" })
                        }
                        ("GET", "/bulkJobs/J") => serde_json::json!({
                            "jobId": "J", "type": "RENAME_FOLDER", "status": "Completed",
                        }),
                        _ => serde_json::json!({ "message": "unexpected" }),
                    };
                    axum::Json(reply)
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, posts)
    }

    /// A native rename job is available, but a public rename must first
    /// verify a free destination; ImageKit's index cannot prove one.
    #[tokio::test]
    async fn native_folder_rename_job_uses_the_documented_request() {
        let (provider, posts) = provider_recording_folder_jobs().await;
        provider
            .rename_folder_job("/src/photos", "albums")
            .await
            .unwrap();
        let posts = posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "/bulkJobs/renameFolder");
        assert_eq!(
            posts[0].1,
            serde_json::json!({"folderPath":"/src/photos", "newFolderName":"albums", "purgeCache":false})
        );
    }

    #[tokio::test]
    async fn probe_contract_imagekit_same_folder_rename_uses_native_collision_check() {
        let (mut provider, posts) = provider_recording_folder_jobs().await;
        let outcome = provider.rename("/src/photos", "/src/albums").await;
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(posts.lock().unwrap().len(), 1);
    }

    /// An unknown final name must not start the first of two folder jobs.
    #[tokio::test]
    async fn a_cross_folder_rename_refuses_before_its_first_job() {
        let (mut provider, posts) = provider_recording_folder_jobs().await;
        let outcome = provider.rename("/src/photos", "/dst/albums").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotSupported(_))),
            "{outcome:?}"
        );
        assert!(posts.lock().unwrap().is_empty());
    }

    /// ImageKit turns every character of a new folder name other than a
    /// letter, a digit or `-` into `_`: such a name is refused before any job
    /// is queued, rather than renaming the folder to something else.
    #[tokio::test]
    async fn a_folder_name_imagekit_would_rewrite_is_refused() {
        let (mut provider, posts) = provider_recording_folder_jobs().await;
        let outcome = provider.rename("/src/photos", "/src/my albums").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotSupported(_))),
            "{outcome:?}"
        );
        assert!(posts.lock().unwrap().is_empty());
        assert!(refuse_rewritten_name("my albums", NameUse::RenameFolder)
            .unwrap_err()
            .to_string()
            .contains("my_albums"));
        assert!(refuse_rewritten_name("albums_2026-10", NameUse::RenameFolder).is_ok());
        assert!(refuse_rewritten_name("fotografía", NameUse::RenameFolder).is_ok());
    }

    /// What ImageKit keeps of a name, as measured on its API: letters, marks
    /// and digits of the Basic Multilingual Plane, `-`, `_`, and `.` in a file
    /// name only. Every character measured is here, so a rule that keeps one
    /// ImageKit rewrote (every Unicode alphanumeric did: `Ⓐ`, `𝔸`) fails.
    #[test]
    fn imagekit_keeps_letters_marks_digits_and_three_signs() {
        for kept in [
            "a_b-c.txt",
            "aéb.txt",
            "ae\u{301}b.txt",
            "aßb.txt",
            "a日b.txt",
            "a\u{661}b.txt",
            "a²b.txt",
            // Each letter case, modifier letters, compatibility forms, other
            // scripts, the three kinds of number and the three kinds of mark.
            "a\u{2167}\u{3007}\u{bd}\u{2460}\u{663}b.txt",
            "a\u{2b0}\u{1c5}\u{1fbc}\u{aa}\u{fb01}\u{640}b.txt",
            "a\u{531}\u{915}\u{93f}\u{ff71}\u{ff21}\u{d55c}\u{130}b.txt",
            "a\u{2126}\u{212b}b.txt",
            "a\u{345}\u{20dd}\u{e31}b.txt",
        ] {
            assert_eq!(imagekit_stored_name(kept, false), kept);
        }
        for (sent, stored) in [
            ("a b.txt", "a_b.txt"),
            (" lead.txt", "_lead.txt"),
            ("   ", "___"),
            ("a(1).txt", "a_1_.txt"),
            ("a&b'c.txt", "a_b_c.txt"),
            ("a\u{a0}b\u{2019}c\u{b7}d.txt", "a_b_c_d.txt"),
            ("a\u{1F600}.txt", "a_.txt"),
            // Symbols Unicode counts as alphabetic: the circled letters.
            ("a\u{24b6}\u{24d0}\u{24e9}b.txt", "a___b.txt"),
            ("a\u{1F130}b.txt", "a_b.txt"),
            // Above U+FFFF, letters, digits and marks too, one `_` each.
            ("a\u{1d538}\u{1d538}b.txt", "a__b.txt"),
            ("\u{915}\u{1d538}.txt", "\u{915}_.txt"),
            (
                "a\u{20000}\u{1d7ce}\u{10400}\u{10330}\u{11066}\u{1d165}b.txt",
                "a______b.txt",
            ),
        ] {
            assert_eq!(imagekit_stored_name(sent, false), stored, "{sent:?}");
        }
        // The circled digit after the circled letters is a digit, as `①` is.
        assert_eq!(imagekit_stored_name("\u{24ea}", true), "\u{24ea}");
        assert_eq!(imagekit_stored_name("a.b", true), "a_b");
        assert_eq!(imagekit_stored_name("a-b_c\u{e9}", true), "a-b_c\u{e9}");
    }

    /// Every write, as `METHOD path name`: the name is the upload's
    /// `fileName`, the folder's `folderName`, or the rename's `newFileName`.
    async fn provider_recording_writes() -> (
        ImageKitProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use std::sync::Arc;
        let writes: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&writes);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let method = req.method().as_str().to_string();
                    let path = req.uri().path().to_string();
                    let query = req.uri().query().unwrap_or("").to_string();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    if method == "GET" {
                        // The source file, and nothing else, in every listing.
                        let listed = if query.contains("path=%2Fsrc&") {
                            serde_json::json!([{
                                "fileId": "A", "name": "a.txt", "filePath": "/src/a.txt",
                                "type": "file", "size": 3,
                            }])
                        } else {
                            serde_json::json!([])
                        };
                        return axum::Json(listed);
                    }
                    let text = String::from_utf8_lossy(&body).to_string();
                    let name = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| {
                            ["newFileName", "folderName", "newFolderName"]
                                .iter()
                                .find_map(|k| v[*k].as_str().map(str::to_string))
                        })
                        .or_else(|| {
                            text.split("name=\"fileName\"\r\n\r\n")
                                .nth(1)
                                .and_then(|rest| rest.split("\r\n").next())
                                .map(str::to_string)
                        })
                        .unwrap_or_default();
                    seen.lock().unwrap().push(format!("{method} {path} {name}"));
                    axum::Json(serde_json::json!({ "jobId": "J" }))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, writes)
    }

    /// Sent as it was, `a b.txt` was uploaded as `a_b.txt` and replaced the
    /// file of that name (measured on the API). A name ImageKit would store
    /// as another one is refused before any request; a name it keeps goes.
    #[tokio::test]
    async fn a_name_imagekit_would_rewrite_is_refused_before_any_request() {
        let local = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(local.path(), b"abc").unwrap();
        let local = local.path().to_str().unwrap();
        let (mut provider, writes) = provider_recording_writes().await;

        let refused = [
            ("upload", provider.upload(local, "/src/a b.txt", None).await),
            (
                "upload",
                provider.upload(local, "/src/ lead.txt", None).await,
            ),
            // A letter above U+FFFF: ImageKit stores it as `_`.
            (
                "upload",
                provider.upload(local, "/src/a\u{1d538}.txt", None).await,
            ),
            ("mkdir", provider.mkdir("/src/my albums").await),
            ("mkdir", provider.mkdir("/src/a.b").await),
            (
                "rename",
                provider.rename("/src/a.txt", "/src/a b.txt").await,
            ),
            ("move", provider.rename("/src/a.txt", "/dst/a b.txt").await),
            (
                "copy",
                provider.server_copy("/src/a.txt", "/dst/a&b.txt").await,
            ),
        ];
        for (what, outcome) in &refused {
            let message = match outcome {
                Err(ProviderError::NotSupported(message)) => message,
                other => panic!("{what}: {other:?}"),
            };
            assert!(
                message.contains("every other character becomes '_'"),
                "{message}"
            );
        }
        assert!(
            writes.lock().unwrap().is_empty(),
            "{:?}",
            writes.lock().unwrap()
        );
        let upload = refused[0].1.as_ref().unwrap_err().to_string();
        assert!(upload.contains("\"a_b.txt\", not \"a b.txt\""), "{upload}");
        let astral = refused[2].1.as_ref().unwrap_err().to_string();
        assert!(astral.contains("\"a_.txt\""), "{astral}");

        provider
            .upload(local, "/src/aéb-1_2.txt", None)
            .await
            .unwrap();
        provider.mkdir("/src/fotografía-2026").await.unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            ["POST /upload aéb-1_2.txt", "POST /folder/ fotografía-2026"]
        );
    }

    #[tokio::test]
    async fn folder_move_returns_only_once_the_bulk_job_completed() {
        let (provider, polls) = provider_with_folder_job(1).await;
        provider
            .start_folder_job("moveFolder", "/src/photos", "/dst", false)
            .await
            .expect("native move job completes");
        assert_eq!(
            polls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one Pending, then Completed"
        );
    }

    #[tokio::test]
    async fn a_folder_job_still_pending_after_the_wait_is_an_error() {
        let (provider, _) = provider_with_folder_job(usize::MAX).await;
        let outcome = provider
            .wait_for_folder_job("J", std::time::Duration::ZERO)
            .await;
        assert!(
            matches!(outcome, FolderJob::Unfinished(ProviderError::Other(ref m)) if m.contains("still pending")),
            "{outcome:?}"
        );
    }

    /// A poll refused with 403 (a key that lost a permission) says nothing
    /// about the job, which may still run: it was forgotten as over, and a
    /// retry queued a second job. It is unfinished, and remembered.
    #[tokio::test]
    async fn a_job_poll_refused_by_permissions_is_unfinished() {
        let (provider, _) = provider_on_tree(&[], &[403]).await;
        let outcome = provider
            .wait_for_folder_job("J", std::time::Duration::ZERO)
            .await;
        assert!(matches!(outcome, FolderJob::Unfinished(_)), "{outcome:?}");
    }

    /// A file moved onto a folder at the destination is refused, like a move
    /// onto a file: ImageKit's rename would put a file `d` beside the folder.
    #[tokio::test]
    async fn a_file_move_onto_an_existing_folder_is_refused() {
        let (mut provider, calls) = provider_on_tree(&["/D", "/D/c.txt", "/D/d"], &[]).await;
        let outcome = provider.rename("/D/c.txt", "/D/d").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    /// An ImageKit double holding `tree` (paths; a name without a dot is a
    /// folder), listed per folder as the API does. Moves, renames and folder
    /// jobs succeed without changing the tree; the status polls of job `J`
    /// answer `job_polls` in turn (an HTTP status, 200 meaning `Completed`).
    /// Returns the provider and every mutating call, as `METHOD path body`.
    async fn provider_on_tree(
        tree: &'static [&'static str],
        job_polls: &'static [u16],
    ) -> (
        ImageKitProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let polls = Arc::new(AtomicUsize::new(0));
        let (seen, polled) = (Arc::clone(&calls), Arc::clone(&polls));
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let (seen, polled) = (Arc::clone(&seen), Arc::clone(&polled));
                async move {
                    let method = req.method().as_str().to_string();
                    let url = reqwest::Url::parse(&format!("http://h{}", req.uri())).unwrap();
                    let path = url.path().to_string();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    let json = |status: u16, value: serde_json::Value| {
                        axum::response::Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(value.to_string()))
                            .unwrap()
                    };
                    match (method.as_str(), path.as_str()) {
                        ("GET", "/files") => {
                            let folder = url
                                .query_pairs()
                                .find(|(k, _)| k == "path")
                                .map(|(_, v)| v.to_string())
                                .unwrap_or_default();
                            let items: Vec<serde_json::Value> = tree
                                .iter()
                                .filter(|p| parent_path(p) == normalize_path(&folder))
                                .map(|p| {
                                    let name = basename(p);
                                    // The API's shapes: a folder has `folderPath`
                                    // and `folderId`, no `filePath`.
                                    if name.contains('.') {
                                        serde_json::json!({
                                            "fileId": format!("id-{name}"), "name": name,
                                            "filePath": p, "type": "file",
                                        })
                                    } else {
                                        serde_json::json!({
                                            "folderId": format!("id-{name}"), "name": name,
                                            "folderPath": p, "type": "folder",
                                        })
                                    }
                                })
                                .collect();
                            json(200, serde_json::json!(items))
                        }
                        ("GET", "/bulkJobs/J") => {
                            let n = polled.fetch_add(1, Ordering::SeqCst);
                            match job_polls.get(n).copied().unwrap_or(200) {
                                200 => json(
                                    200,
                                    serde_json::json!({ "jobId": "J", "status": "Completed" }),
                                ),
                                status => json(status, serde_json::json!({ "message": "busy" })),
                            }
                        }
                        _ => {
                            seen.lock().unwrap().push(format!(
                                "{method} {path} {}",
                                String::from_utf8_lossy(&body)
                            ));
                            json(200, serde_json::json!({ "jobId": "J" }))
                        }
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = empty_provider();
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, calls)
    }

    /// `files/move` puts the moved file on top of a same-named one at the
    /// destination, as its newest version: a rename must refuse first.
    #[tokio::test]
    async fn a_file_move_onto_an_existing_file_is_refused() {
        let (mut provider, calls) = provider_on_tree(&["/src/a.jpg", "/dst/a.jpg"], &[]).await;
        let outcome = provider.rename("/src/a.jpg", "/dst/a.jpg").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    /// `moveFolder` merges into an existing folder of the same name.
    #[tokio::test]
    async fn a_folder_move_onto_an_existing_folder_is_refused() {
        let (mut provider, calls) =
            provider_on_tree(&["/src", "/dst", "/src/photos", "/dst/photos"], &[]).await;
        let outcome = provider.rename("/src/photos", "/dst/photos").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    /// Moving first would land `a.jpg` on top of `/dst/a.jpg` (as its newest
    /// version) and then rename that file, with the other's history, to
    /// `b.jpg`: the rename goes first, in `/src`.
    #[tokio::test]
    async fn a_file_move_refuses_an_unverified_destination_before_renaming() {
        let (mut provider, calls) = provider_on_tree(&["/src/a.jpg", "/dst/a.jpg"], &[]).await;
        let outcome = provider.rename("/src/a.jpg", "/dst/b.jpg").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotSupported(_))),
            "{outcome:?}"
        );
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|call| !call.starts_with("PUT ") && !call.starts_with("POST ")),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    /// `replace` puts the file on top of the one at the destination, which
    /// keeps the old content as a previous version.
    #[tokio::test]
    async fn replace_moves_the_file_on_top_of_the_existing_one() {
        let (mut provider, calls) = provider_on_tree(&["/src/a.jpg", "/dst/a.jpg"], &[]).await;
        provider
            .replace("/src/a.jpg", "/dst/a.jpg")
            .await
            .expect("replace");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with("PUT /files/move"), "{calls:?}");
        assert!(
            calls[0].contains(r#""destinationPath":"/dst/""#),
            "{calls:?}"
        );
    }

    /// With `overwrite` the destination was not looked at: a file replaced
    /// onto a folder was moved in beside it under its name, and a folder
    /// replaced onto a folder queued a `moveFolder` that merges the two
    /// trees. Both are refused before any call.
    #[tokio::test]
    async fn a_replace_across_types_or_onto_a_folder_is_refused() {
        let (mut provider, calls) =
            provider_on_tree(&["/src", "/dst", "/src/a.jpg", "/dst/photos"], &[]).await;
        let across = provider.replace("/src/a.jpg", "/dst/photos").await;
        assert!(
            matches!(across, Err(ProviderError::AlreadyExists(_))),
            "{across:?}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
        let (mut provider, calls) =
            provider_on_tree(&["/src", "/dst", "/src/photos", "/dst/photos"], &[]).await;
        let merge = provider.replace("/src/photos", "/dst/photos").await;
        assert!(
            matches!(merge, Err(ProviderError::NotSupported(_))),
            "{merge:?}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    /// A poll that fails says nothing about the job, which was queued and may
    /// still complete; retrying the move waits for that job instead of
    /// queueing a second one.
    #[tokio::test]
    async fn a_failed_poll_says_the_job_may_complete_and_a_retry_does_not_requeue() {
        let (mut provider, calls) = provider_on_tree(&["/src", "/src/photos"], &[500, 200]).await;
        // A job already submitted before this probe-policy change may finish.
        let first = provider
            .start_folder_job("moveFolder", "/src/photos", "/dst", false)
            .await;
        assert!(
            matches!(first, Err(ProviderError::Other(ref m)) if m.contains("may still complete")),
            "{first:?}"
        );
        provider
            .rename("/src/photos", "/dst/photos")
            .await
            .expect("the retry sees the first job complete");
        let posts = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("POST /bulkJobs/moveFolder"))
            .count();
        assert_eq!(posts, 1, "{:?}", calls.lock().unwrap());
    }

    /// A moveFolder job keeps the folder's name. A move to another name in
    /// the same folder found the job queued for the first move and answered
    /// with its outcome: `/dst/other` was reported moved while the folder
    /// went to `/dst/photos`.
    #[tokio::test]
    async fn a_queued_folder_move_answers_only_a_move_that_keeps_its_name() {
        let (mut provider, calls) = provider_on_tree(&["/src", "/src/photos"], &[500, 200]).await;
        let first = provider
            .start_folder_job("moveFolder", "/src/photos", "/dst", false)
            .await;
        assert!(first.is_err(), "{first:?}");
        let other = provider.rename("/src/photos", "/dst/other").await;
        assert!(other.is_err(), "another name is another move: {other:?}");
        assert_eq!(
            provider.unfinished_folder_jobs.lock().unwrap().len(),
            1,
            "the first move is still waited for"
        );
        provider
            .rename("/src/photos", "/dst/photos")
            .await
            .expect("the same move waits for its job");
        let posts = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("POST /bulkJobs/"))
            .count();
        assert_eq!(posts, 1, "{:?}", calls.lock().unwrap());
    }

    /// A job ImageKit no longer knows (404) is over: remembering it made
    /// every later move of that folder poll the dead job again, so the folder
    /// could not be moved for the rest of the session. A later fresh attempt
    /// runs the new destination checks and refuses an unverified target.
    #[tokio::test]
    async fn a_job_imagekit_does_not_know_is_forgotten() {
        let (mut provider, calls) = provider_on_tree(&["/src", "/src/photos"], &[404]).await;
        // A job already submitted before this probe-policy change may finish.
        let first = provider
            .start_folder_job("moveFolder", "/src/photos", "/dst", false)
            .await;
        assert!(first.is_err(), "{first:?}");
        let retry = provider.rename("/src/photos", "/dst/photos").await;
        assert!(
            matches!(retry, Err(ProviderError::NotSupported(_))),
            "a fresh unverified destination is refused: {retry:?}"
        );
        assert!(
            provider.unfinished_folder_jobs.lock().unwrap().is_empty(),
            "the forgotten job must be cleared"
        );
        let posts = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("POST /bulkJobs/moveFolder"))
            .count();
        assert_eq!(posts, 1, "no fresh job: {:?}", calls.lock().unwrap());
    }

    /// Every caller that needs atomicity stages its temporary next to the
    /// target, and in one folder ImageKit can only rename, which refuses a
    /// taken name (409): the edit uploaded its temporary and then failed on
    /// the replace (found live on 2026-09-26, the temporary left behind). The
    /// answer is no, so those callers refuse before writing anything.
    #[tokio::test]
    async fn imagekit_does_not_claim_an_atomic_replace() {
        let mut provider = empty_provider();
        assert!(!provider.supports_atomic_replace().await.unwrap());
        assert!(!provider.replace_sets_aside());
    }

    fn empty_provider() -> ImageKitProvider {
        ImageKitProvider::new(ImageKitConfig {
            imagekit_id: "demo".to_string(),
            private_key: SecretString::from("k".to_string()),
            initial_path: None,
        })
    }

    /// SG-T09 gate: ImageKit advertises the server_side_copy capability
    /// under both the legacy and the new slot, so the DAG capability
    /// builder picks it up regardless of which name the wiring uses.
    #[test]
    fn imagekit_advertises_server_side_copy_capability() {
        let p = empty_provider();
        assert!(p.supports_server_copy());
        assert!(p.supports_server_side_copy());
    }

    #[test]
    fn imagekit_listing_cannot_authorize_sync_deletes() {
        let p = empty_provider();
        assert!(!p.listing_is_authoritative());
    }

    /// SG-T09 gate: both the trait-level entry point and the legacy
    /// delegate fail fast on the connection check before issuing a
    /// `copyFile` / `copyFolder` API call.
    #[tokio::test]
    async fn imagekit_server_side_copy_requires_connection() {
        let mut p = empty_provider();
        let direct =
            StorageProvider::server_side_copy(&mut p, "/src/file.png", "/dst/file.png").await;
        assert!(matches!(direct, Err(ProviderError::NotConnected)));

        let via_legacy = p.server_copy("/src/file.png", "/dst/file.png").await;
        assert!(matches!(via_legacy, Err(ProviderError::NotConnected)));
    }

    /// The API names a folder with `folderPath`: a recursive delete of a
    /// folder holding a subfolder walked `/` instead of the subfolder, deleted
    /// the files at the account root and never ended (live, 2026-10-15).
    #[tokio::test]
    async fn a_recursive_delete_stays_inside_its_folder_and_ends() {
        let (mut provider, calls) = provider_on_tree(
            &[
                "/keep.png",
                "/tree",
                "/tree/a.png",
                "/tree/sub",
                "/tree/sub/b.png",
            ],
            &[],
        )
        .await;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.rmdir_recursive("/tree"),
        )
        .await
        .expect("the walk ends")
        .expect("recursive delete");
        let calls = calls.lock().unwrap().clone();
        assert!(calls.iter().all(|c| !c.contains("keep.png")), "{calls:?}");
        assert_eq!(
            calls,
            [
                "DELETE /files/id-a.png ",
                "DELETE /files/id-b.png ",
                r#"DELETE /folder/ {"folderPath":"/tree/sub/"}"#,
                r#"DELETE /folder/ {"folderPath":"/tree/"}"#,
            ]
        );
    }

    /// The same path reaches every reader: a listed folder, its `stat` and a
    /// `find` that descends through it.
    #[tokio::test]
    async fn a_listed_folder_carries_its_own_path() {
        let (mut provider, _) =
            provider_on_tree(&["/tree", "/tree/sub", "/tree/sub/b.png"], &[]).await;
        let listed = provider.list("/tree").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (listed[0].path.as_str(), listed[0].is_dir),
            ("/tree/sub", true)
        );
        let stat = provider.stat("/tree/sub").await.expect("stat");
        assert_eq!(stat.path, "/tree/sub");
        let found = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            provider.find("/tree", "*.png"),
        )
        .await
        .expect("the walk ends")
        .expect("find");
        let paths: Vec<_> = found.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/tree/sub/b.png"]);
    }

    fn listed(json: serde_json::Value) -> IkFile {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_listed_entry_path_comes_from_the_listing() {
        let folder = listed(serde_json::json!({
            "type": "folder", "name": "sub", "folderPath": "/tree/sub/", "folderId": "F",
        }));
        assert_eq!(listed_entry_path(&folder, "/tree").unwrap(), "/tree/sub");
        let file = listed(serde_json::json!({
            "type": "file", "name": "a.png", "filePath": "/tree/a.png", "fileId": "A",
        }));
        assert_eq!(listed_entry_path(&file, "/tree/").unwrap(), "/tree/a.png");
        let bare = listed(serde_json::json!({ "type": "folder", "name": "sub", "filePath": null }));
        assert_eq!(listed_entry_path(&bare, "/").unwrap(), "/sub");
    }

    /// A path that is not a child of the listed folder is refused, whatever
    /// field carries it, and so is an entry with no path and no usable name:
    /// walking either would leave the folder the walk was given.
    #[test]
    fn a_listed_entry_outside_the_listed_folder_is_refused() {
        for json in [
            serde_json::json!({ "type": "folder", "name": "x", "folderPath": "/" }),
            serde_json::json!({ "type": "folder", "name": "x", "folderPath": "/tree" }),
            serde_json::json!({ "type": "folder", "name": "x", "folderPath": "/other/x" }),
            serde_json::json!({ "type": "file", "name": "a.png", "filePath": "/tree/sub/a.png" }),
            serde_json::json!({ "type": "folder", "name": ".." }),
            serde_json::json!({ "type": "folder", "name": "" }),
            serde_json::json!({ "type": "folder", "name": "a/b" }),
        ] {
            let item = listed(json.clone());
            assert!(
                matches!(
                    listed_entry_path(&item, "/tree"),
                    Err(ProviderError::ParseError(_))
                ),
                "{json}"
            );
        }
    }
}
