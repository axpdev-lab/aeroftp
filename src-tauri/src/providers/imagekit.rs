//! ImageKit Storage Provider
//!
//! Implements StorageProvider for ImageKit's REST APIs.
//! Authentication: HTTP Basic with the private API key as username and an empty password.
//! API: https://api.imagekit.io/v1 and https://upload.imagekit.io/api/v1/files/upload

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
    pub fn new(config: ImageKitConfig) -> Self {
        let current_path = normalize_path(config.initial_path.as_deref().unwrap_or("/"));
        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
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

    fn resolve_path(&self, path: &str) -> String {
        if path.trim().is_empty() {
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
            s if s.is_client_error() => ProviderError::InvalidConfig(msg),
            s if s.is_server_error() => ProviderError::ServerError(msg),
            _ => ProviderError::Other(format!("HTTP {}: {}", status, msg)),
        }
    }

    async fn list_raw(&self, path: &str) -> Result<Vec<IkFile>, ProviderError> {
        validate_path(path)?;
        // ImageKit `/files` accepts only `file | file-version | folder | all`
        // for the `type` query parameter. Sending `file-and-folder` (an
        // earlier guess from the API docs) gets rejected at runtime with
        // `Invalid configuration: Your request contains invalid value for
        // type parameter ...`. `all` returns folders + files in one call,
        // which is what the rest of this provider already expects.
        let url = format!(
            "{}/files?path={}&type=all&limit=1000&skip=0",
            self.api_base(),
            urlencoding::encode(path)
        );
        let resp = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        resp.json::<Vec<IkFile>>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    async fn find_entry(&self, path: &str) -> Result<IkFile, ProviderError> {
        let resolved = normalize_path(path);
        if resolved == "/" {
            return Ok(IkFile {
                file_id: String::new(),
                name: "/".to_string(),
                file_path: "/".to_string(),
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
            .ok_or(ProviderError::NotFound(resolved))
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
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn copy_file(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        let source = normalize_path(from);
        let dest_parent = folder_path(&parent_path(&normalize_path(to)));
        let resp = self
            .auth(self.client.post(format!("{}/files/copy", self.api_base())))
            .json(&CopyFileRequest {
                source_file_path: &source,
                destination_path: &dest_parent,
                include_file_versions: false,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
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
        // run after the first.
        if let Some(outcome) = self
            .resume_folder_job("moveFolder", &source, &parent_path(&target))
            .await
        {
            return outcome;
        }
        let entry = self.stat(&source).await?;
        // The look reads the listing (`GET /v1/files`), the one lookup by
        // path the API has: there is no folder-details endpoint, and a
        // folder created a moment earlier is not in that index yet, so for
        // that window its name reads free and a file lands beside it (live,
        // 2026-09-27: `mkdir` then `mv` onto the new folder). Declared, not
        // closed: nothing in the API answers sooner.
        let occupant = match self.find_entry(&target).await {
            Ok(found) => Some(file_to_entry(&found)),
            Err(ProviderError::NotFound(_)) => None,
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
            refuse_rewritten_folder_name(dest_name)?;
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
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }
        let job = resp
            .json::<IkBulkJob>()
            .await
            .map_err(|e| ProviderError::ParseError(format!("bulk job response: {e}")))?;
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
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        // The POST answers as soon as the job is queued. Returning then told
        // the caller a folder had moved while it still sat at the source.
        let job = resp
            .json::<IkBulkJob>()
            .await
            .map_err(|e| ProviderError::ParseError(format!("bulk job response: {e}")))?;
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
                Err(e) => return unknown(ProviderError::NetworkError(e.to_string())),
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
                    return unknown(ProviderError::ParseError(format!("bulk job status: {e}")))
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
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

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
        let url = entry
            .metadata
            .get("url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("ImageKit URL missing".to_string()))?;
        validate_download_url(&url)?;

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
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
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
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
        let url = entry
            .metadata
            .get("url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("ImageKit URL missing".to_string()))?;
        validate_download_url(&url)?;
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
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
        let url = entry
            .metadata
            .get("url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("ImageKit URL missing".to_string()))?;
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
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
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
        let bytes = response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await?;
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
            .text("overwriteFile", "true");

        let resp = self
            .auth(self.client.post(UPLOAD_URL))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let uploaded = resp
            .json::<IkUploadResponse>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
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

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let name = basename(&resolved);
        if name.is_empty() {
            return Ok(());
        }
        let parent = folder_path(&parent_path(&resolved));

        let resp = self
            .auth(self.client.post(format!("{}/folder/", self.api_base())))
            .json(&FolderRequest {
                folder_name: name,
                parent_folder_path: &parent,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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
            let entries = self.list_raw(&dir).await?;
            for entry in entries {
                if entry.entry_type == "folder" {
                    stack.push(normalize_path(&entry.file_path));
                } else {
                    self.delete_file_by_path(&entry.file_path).await?;
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
            .ok_or_else(|| ProviderError::NotFound("No ImageKit thumbnail URL".to_string()))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let root = self.resolve_path(path);
        let mut stack = vec![root];
        let mut matches = Vec::new();
        while let Some(dir) = stack.pop() {
            for item in self.list_raw(&dir).await? {
                if item.entry_type == "folder" {
                    stack.push(normalize_path(&item.file_path));
                }
                if super::matches_find_pattern(&item.name, pattern) {
                    matches.push(file_to_entry(&item));
                }
            }
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

/// `renameFolder` replaces every character of the new name that is not a
/// letter, a digit or `-` with `_` (official SDK, `FolderRenameParams`). A
/// name it would rewrite is refused before anything changes, rather than
/// leaving the folder under a name nobody asked for.
fn refuse_rewritten_folder_name(name: &str) -> Result<(), ProviderError> {
    let rewritten: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if rewritten == name {
        return Ok(());
    }
    Err(ProviderError::NotSupported(format!(
        "ImageKit would rename the folder to \"{rewritten}\", not \"{name}\": a folder name \
         there keeps only letters, digits and '-', and every other character becomes '_'. \
         Nothing was changed"
    )))
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
                            "fileId": "", "name": "photos", "filePath": "/src/photos", "type": "folder",
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
                            "fileId": "", "name": "photos", "filePath": "/src/photos", "type": "folder",
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

    /// A folder rename was refused as not supported, while ImageKit renames a
    /// folder with the `renameFolder` job. In one folder it is that job.
    #[tokio::test]
    async fn a_folder_renames_in_place_with_the_rename_folder_job() {
        let (mut provider, posts) = provider_recording_folder_jobs().await;
        provider
            .rename("/src/photos", "/src/albums")
            .await
            .expect("folder rename");
        let posts = posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(posts[0].0, "/bulkJobs/renameFolder");
        assert_eq!(
            posts[0].1,
            serde_json::json!({
                "folderPath": "/src/photos", "newFolderName": "albums", "purgeCache": false,
            })
        );
    }

    /// A new name and a new parent: renamed in place, then moved.
    #[tokio::test]
    async fn a_folder_renamed_into_another_folder_is_renamed_then_moved() {
        let (mut provider, posts) = provider_recording_folder_jobs().await;
        provider
            .rename("/src/photos", "/dst/albums")
            .await
            .expect("folder rename and move");
        let jobs: Vec<String> = posts
            .lock()
            .unwrap()
            .iter()
            .map(|(job, _)| job.clone())
            .collect();
        assert_eq!(jobs, ["/bulkJobs/renameFolder", "/bulkJobs/moveFolder"]);
        assert_eq!(
            posts.lock().unwrap()[1].1["sourceFolderPath"],
            "/src/albums/"
        );
    }

    /// ImageKit turns every character of a new folder name other than a
    /// letter, a digit or `-` into `_`: such a name is refused before any job
    /// is queued, rather than renaming the folder to something else.
    #[tokio::test]
    async fn a_folder_name_imagekit_would_rewrite_is_refused() {
        let (mut provider, posts) = provider_recording_folder_jobs().await;
        let outcome = provider.rename("/src/photos", "/src/my albums").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotSupported(ref m)) if m.contains("my_albums")),
            "{outcome:?}"
        );
        assert!(posts.lock().unwrap().is_empty());
        assert!(refuse_rewritten_folder_name("albums_2026-10").is_ok());
        assert!(refuse_rewritten_folder_name("fotografía").is_ok());
    }

    #[tokio::test]
    async fn folder_move_returns_only_once_the_bulk_job_completed() {
        let (mut provider, polls) = provider_with_folder_job(1).await;
        provider
            .rename("/src/photos", "/dst/photos")
            .await
            .expect("move");
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
                                    let kind = if name.contains('.') { "file" } else { "folder" };
                                    serde_json::json!({
                                        "fileId": format!("id-{name}"), "name": name,
                                        "filePath": p, "type": kind,
                                    })
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
    async fn a_move_whose_destination_holds_the_old_name_renames_first() {
        let (mut provider, calls) = provider_on_tree(&["/src/a.jpg", "/dst/a.jpg"], &[]).await;
        provider
            .rename("/src/a.jpg", "/dst/b.jpg")
            .await
            .expect("rename then move");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].starts_with("PUT /files/rename"), "{calls:?}");
        assert!(calls[0].contains(r#""filePath":"/src/a.jpg""#), "{calls:?}");
        assert!(calls[1].starts_with("PUT /files/move"), "{calls:?}");
        assert!(
            calls[1].contains(r#""sourceFilePath":"/src/b.jpg""#),
            "{calls:?}"
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
        let first = provider.rename("/src/photos", "/dst/photos").await;
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

    /// A job ImageKit no longer knows (404) is over: remembering it made
    /// every later move of that folder poll the dead job again, so the folder
    /// could not be moved for the rest of the session. The next attempt
    /// queues a job of its own.
    #[tokio::test]
    async fn a_job_imagekit_does_not_know_is_forgotten() {
        let (mut provider, calls) = provider_on_tree(&["/src", "/src/photos"], &[404]).await;
        let first = provider.rename("/src/photos", "/dst/photos").await;
        assert!(first.is_err(), "{first:?}");
        provider
            .rename("/src/photos", "/dst/photos")
            .await
            .expect("a new job, which completes");
        let posts = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("POST /bulkJobs/moveFolder"))
            .count();
        assert_eq!(posts, 2, "{:?}", calls.lock().unwrap());
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
}
