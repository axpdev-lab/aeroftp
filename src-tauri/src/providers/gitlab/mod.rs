//! GitLab Storage Provider
//!
//! Browse repository contents via the GitLab REST API v4.
//! Supports both gitlab.com and self-hosted instances.
//!
//! Key differences from GitHub provider:
//! - Auth: `PRIVATE-TOKEN` header (not Bearer)
//! - API base: configurable (self-hosted support)
//! - Batch commit: native REST endpoint (no GraphQL needed)
//! - Pagination: `x-next-page` header (not Link header)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

mod client;
mod model;

use self::client::GitLabHttpClient;
use self::model::*;

use super::{ProviderConfig, ProviderError, ProviderType, RemoteEntry, StorageProvider};
use async_trait::async_trait;
use secrecy::SecretString;

/// Configuration for connecting to a GitLab repository.
#[derive(Debug, Clone)]
pub struct GitLabConfig {
    /// Personal/Project access token.
    pub token: String,
    /// API base URL (e.g. `https://gitlab.com/api/v4`).
    pub api_base: String,
    /// Project ID (numeric) or URL-encoded path (e.g. `group%2Fproject`).
    pub project_path: String,
    /// Branch to browse (empty = default branch).
    pub branch: String,
    /// Initial path within the repo.
    pub initial_path: Option<String>,
}

impl GitLabConfig {
    /// Build a [`GitLabConfig`] from the generic [`ProviderConfig`].
    ///
    /// Expects:
    /// - `host`: `"owner/repo"`, `"gitlab.com/owner/repo"`, or `"self-hosted.com/owner/repo"`
    /// - `password`: the access token
    /// - `extra["branch"]`: optional branch override
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config
            .password
            .clone()
            .ok_or_else(|| {
                ProviderError::InvalidConfig("GitLab access token is required".into())
            })?;

        let host = config.host.trim().to_string();

        // Determine API base and project path from the host field.
        // Supported formats:
        //   "owner/repo"                    -> gitlab.com, project = owner/repo
        //   "gitlab.com/owner/repo"         -> gitlab.com, project = owner/repo
        //   "self-hosted.com/owner/repo"    -> self-hosted, project = owner/repo
        //   "https://gitlab.example.com/group/subgroup/project"
        let (api_base, project_path) = parse_host_field(&host)?;

        // Optional branch from extra or host@branch syntax
        let (clean_project, host_branch) = match project_path.rsplit_once('@') {
            Some((proj, branch)) if !proj.is_empty() && !branch.is_empty() => {
                (proj.to_string(), Some(branch.to_string()))
            }
            _ => (project_path, None),
        };

        let branch = config
            .extra
            .get("branch")
            .cloned()
            .or(host_branch)
            .unwrap_or_default();

        Ok(Self {
            token,
            api_base,
            project_path: clean_project,
            branch,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Parse the host field into (api_base, project_path).
fn parse_host_field(host: &str) -> Result<(String, String), ProviderError> {
    let stripped = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);

    // Split into domain and path parts
    let (domain, path) = match stripped.split_once('/') {
        Some((d, p)) => (d, p.trim_matches('/')),
        None => {
            return Err(ProviderError::InvalidConfig(
                "Host must be 'owner/repo' or 'gitlab.example.com/owner/repo'".into(),
            ));
        }
    };

    if path.is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Project path is required (e.g. 'owner/repo')".into(),
        ));
    }

    // If domain looks like a GitLab instance (contains a dot), use it as the base
    if domain.contains('.') {
        let api_base = format!("https://{}/api/v4", domain);
        Ok((api_base, path.to_string()))
    } else {
        // "owner/repo" format — domain is actually the owner
        let api_base = "https://gitlab.com/api/v4".to_string();
        Ok((api_base, format!("{}/{}", domain, path)))
    }
}

/// URL-encode a project path for GitLab API.
/// `group/project` → `group%2Fproject`
fn encode_project_path(path: &str) -> String {
    path.replace('/', "%2F")
}

/// URL-encode a file path for the files API.
/// `src/main.rs` → `src%2Fmain.rs`
fn encode_file_path(path: &str) -> String {
    path.replace('/', "%2F")
}

/// GitLab storage provider implementing [`StorageProvider`].
pub struct GitLabProvider {
    client: GitLabHttpClient,
    project_path: String,
    project_id: Option<u64>,
    branch: String,
    current_path: String,
    connected: bool,
    account_name: Option<String>,
    account_email: Option<String>,
    default_branch: String,
    project_visibility: String,
    repo_size: Option<u64>,
    can_push: bool,
}

impl std::fmt::Debug for GitLabProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitLabProvider")
            .field("project_path", &self.project_path)
            .field("branch", &self.branch)
            .field("current_path", &self.current_path)
            .field("connected", &self.connected)
            .finish()
    }
}

impl GitLabProvider {
    /// Create a new provider from a parsed config.
    pub fn new(config: GitLabConfig) -> Result<Self, ProviderError> {
        let token = SecretString::from(config.token);
        let current_path = normalise_path(config.initial_path.as_deref().unwrap_or(""));

        Ok(Self {
            client: GitLabHttpClient::new(token, config.api_base)?,
            project_path: config.project_path,
            project_id: None,
            branch: config.branch,
            current_path,
            connected: false,
            account_name: None,
            account_email: None,
            default_branch: String::from("main"),
            project_visibility: String::new(),
            repo_size: None,
            can_push: false,
        })
    }

    // ── Public accessors for Tauri commands ──────────────────────────

    /// Project path (e.g. "group/project").
    pub fn project_path(&self) -> &str {
        &self.project_path
    }

    /// Whether the repository is private.
    pub fn is_private(&self) -> bool {
        self.project_visibility == "private"
    }

    /// Whether the user has push access.
    pub fn can_push(&self) -> bool {
        self.can_push
    }

    /// The active branch.
    pub fn active_branch_name(&self) -> &str {
        self.active_branch()
    }

    /// List all branches.
    pub async fn list_branches(&mut self) -> Result<Vec<model::GitLabBranch>, ProviderError> {
        let path = format!("{}/repository/branches", self.project_api());
        self.client.get_paginated(&path, 100).await
    }

    /// Switch to a different branch.
    pub async fn switch_branch(&mut self, branch: &str) -> Result<(), ProviderError> {
        // Verify the branch exists
        let branch_url = format!(
            "{}/repository/branches/{}",
            self.project_api(),
            urlencoding::encode(branch),
        );
        let branch_info: model::GitLabBranch = self.client.get_json(&branch_url).await?;
        self.branch = branch_info.name;
        self.can_push = branch_info.can_push;
        self.current_path = String::new();
        Ok(())
    }

    /// Project API base path: `/projects/{encoded_id}`
    fn project_api(&self) -> String {
        if let Some(id) = self.project_id {
            format!("/projects/{}", id)
        } else {
            format!("/projects/{}", encode_project_path(&self.project_path))
        }
    }

    /// The active branch for browsing.
    fn active_branch(&self) -> &str {
        if self.branch.is_empty() {
            &self.default_branch
        } else {
            &self.branch
        }
    }

    /// Resolve a path relative to current_path.
    fn resolve_path(&self, path: &str) -> String {
        let p = normalise_path(path);
        if p.is_empty() || p == "/" {
            self.current_path.clone()
        } else if p.starts_with('/') {
            normalise_path(&p)
        } else if self.current_path.is_empty() {
            p
        } else {
            normalise_path(&format!("{}/{}", self.current_path, p))
        }
    }

    /// Build a commit with batch actions (public for Tauri commands).
    pub async fn commit_actions_pub(
        &mut self,
        message: &str,
        actions: Vec<serde_json::Value>,
    ) -> Result<GitLabCommit, ProviderError> {
        self.commit_actions(message, actions).await
    }

    /// Build a commit with batch actions.
    async fn commit_actions(
        &mut self,
        message: &str,
        actions: Vec<serde_json::Value>,
    ) -> Result<GitLabCommit, ProviderError> {
        let body = serde_json::json!({
            "branch": self.active_branch(),
            "commit_message": message,
            "actions": actions,
        });
        let path = format!("{}/repository/commits", self.project_api());
        self.client.post_json(&path, &body).await
    }
}

/// Normalise a path: strip leading/trailing slashes, collapse duplicates.
fn normalise_path(p: &str) -> String {
    let trimmed = p.trim_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        String::new()
    } else {
        trimmed.to_string()
    }
}

#[async_trait]
impl StorageProvider for GitLabProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::GitLab
    }

    fn display_name(&self) -> String {
        format!("GitLab ({})", self.project_path)
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // 1. Validate token — GET /user
        match self.client.get_json::<GitLabUser>("/user").await {
            Ok(user) => {
                self.account_name = user.name.clone().or(Some(user.username.clone()));
                self.account_email = user.email.clone().or(Some(format!("{}@gitlab", user.username)));
            }
            Err(ProviderError::AuthenticationFailed(_)) => {
                // Project access tokens return a bot user, still works
                self.account_name = Some("GitLab Token".to_string());
                self.account_email = None;
            }
            Err(e) => return Err(e),
        }

        // 2. Resolve project — GET /projects/:id
        let project_url = format!(
            "/projects/{}",
            encode_project_path(&self.project_path)
        );
        let project: GitLabProject = self.client.get_json(&project_url).await?;

        self.project_id = Some(project.id);
        self.default_branch = project.default_branch.unwrap_or_else(|| "main".to_string());
        self.project_visibility = project.visibility;
        self.repo_size = project.repository_size;

        // If no branch was specified, use the project default
        if self.branch.is_empty() {
            self.branch = self.default_branch.clone();
        }

        // 3. Check write access via branch info
        let branch_url = format!(
            "{}/repository/branches/{}",
            self.project_api(),
            urlencoding::encode(self.active_branch()),
        );
        match self.client.get_json::<GitLabBranch>(&branch_url).await {
            Ok(branch_info) => {
                self.can_push = branch_info.can_push;
            }
            Err(_) => {
                // Branch might not exist yet, assume write access
                self.can_push = true;
            }
        }

        self.connected = true;

        log::info!(
            "GitLab: connected to {} (branch: {}, visibility: {}, can_push: {})",
            self.project_path,
            self.active_branch(),
            self.project_visibility,
            self.can_push,
        );

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

        let tree_path = if resolved.is_empty() {
            format!(
                "{}/repository/tree?ref={}",
                self.project_api(),
                urlencoding::encode(self.active_branch()),
            )
        } else {
            format!(
                "{}/repository/tree?ref={}&path={}",
                self.project_api(),
                urlencoding::encode(self.active_branch()),
                urlencoding::encode(&resolved),
            )
        };

        let items: Vec<GitLabTreeEntry> = self.client.get_paginated(&tree_path, 100).await?;

        let entries = items
            .iter()
            .map(|item| {
                let is_dir = item.entry_type == "tree";
                let entry_path = if resolved.is_empty() {
                    item.name.clone()
                } else {
                    format!("{}/{}", resolved, item.name)
                };

                RemoteEntry {
                    name: item.name.clone(),
                    path: entry_path,
                    is_dir,
                    size: 0, // Tree API doesn't return sizes
                    modified: None,
                    permissions: None,
                    owner: self.account_name.clone(),
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: Default::default(),
                }
            })
            .collect();

        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_path.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_path))
        }
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);

        if resolved.is_empty() {
            self.current_path = String::new();
            return Ok(());
        }

        // Handle ".." navigation
        if path == ".." {
            return self.cd_up().await;
        }

        // Verify directory exists by listing it
        let tree_path = format!(
            "{}/repository/tree?ref={}&path={}&per_page=1",
            self.project_api(),
            urlencoding::encode(self.active_branch()),
            urlencoding::encode(&resolved),
        );
        self.client.get_json::<Vec<GitLabTreeEntry>>(&tree_path).await?;

        self.current_path = resolved;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if let Some(pos) = self.current_path.rfind('/') {
            self.current_path = self.current_path[..pos].to_string();
        } else {
            self.current_path = String::new();
        }
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
        let resolved = self.resolve_path(remote_path);
        let encoded = encode_file_path(&resolved);

        let url = format!(
            "{}/repository/files/{}/raw?ref={}",
            self.project_api(),
            encoded,
            urlencoding::encode(self.active_branch()),
        );

        let resp = self.client.get_raw_response(&url).await?;
        let total_size = resp
            .content_length()
            .unwrap_or(0);

        let bytes = resp.bytes().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Download failed: {}", e))
        })?;

        if let Some(ref progress) = on_progress {
            progress(total_size, total_size);
        }

        tokio::fs::write(local_path, &bytes).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to write file: {}", e))
        })?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(remote_path);
        let encoded = encode_file_path(&resolved);

        let url = format!(
            "{}/repository/files/{}/raw?ref={}",
            self.project_api(),
            encoded,
            urlencoding::encode(self.active_branch()),
        );

        self.client.get_raw_bytes(&url).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(remote_path);

        let data = tokio::fs::read(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to read local file: {}", e))
        })?;
        let content_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &data,
        );

        // Check if file exists to determine create vs update action
        let encoded = encode_file_path(&resolved);
        let file_url = format!(
            "{}/repository/files/{}?ref={}",
            self.project_api(),
            encoded,
            urlencoding::encode(self.active_branch()),
        );
        let action = if self.client.exists(&file_url).await? {
            "update"
        } else {
            "create"
        };

        let file_name = std::path::Path::new(&resolved)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| resolved.clone());

        self.commit_actions(
            &format!("Upload {} via AeroFTP", file_name),
            vec![serde_json::json!({
                "action": action,
                "file_path": resolved,
                "content": content_b64,
                "encoding": "base64",
            })],
        )
        .await?;

        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let gitkeep_path = format!("{}/.gitkeep", resolved);

        self.commit_actions(
            &format!("Create directory {} via AeroFTP", resolved),
            vec![serde_json::json!({
                "action": "create",
                "file_path": gitkeep_path,
                "content": "",
            })],
        )
        .await?;

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);

        self.commit_actions(
            &format!("Delete {} via AeroFTP", resolved),
            vec![serde_json::json!({
                "action": "delete",
                "file_path": resolved,
            })],
        )
        .await?;

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // GitLab: delete all files in the directory
        self.rmdir_recursive(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);

        // List all files recursively
        let tree_path = format!(
            "{}/repository/tree?ref={}&path={}&recursive=true&per_page=100",
            self.project_api(),
            urlencoding::encode(self.active_branch()),
            urlencoding::encode(&resolved),
        );
        let items: Vec<GitLabTreeEntry> = self.client.get_paginated(&tree_path, 100).await?;

        let delete_actions: Vec<serde_json::Value> = items
            .iter()
            .filter(|item| item.entry_type == "blob")
            .map(|item| {
                serde_json::json!({
                    "action": "delete",
                    "file_path": item.path,
                })
            })
            .collect();

        if delete_actions.is_empty() {
            return Err(ProviderError::NotFound(format!(
                "Directory '{}' is empty or does not exist",
                resolved
            )));
        }

        self.commit_actions(
            &format!("Delete directory {} via AeroFTP", resolved),
            delete_actions,
        )
        .await?;

        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);

        self.commit_actions(
            &format!("Rename {} to {} via AeroFTP", resolved_from, resolved_to),
            vec![serde_json::json!({
                "action": "move",
                "file_path": resolved_to,
                "previous_path": resolved_from,
            })],
        )
        .await?;

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let encoded = encode_file_path(&resolved);

        let file_url = format!(
            "{}/repository/files/{}?ref={}",
            self.project_api(),
            encoded,
            urlencoding::encode(self.active_branch()),
        );

        let file_info: GitLabFileInfo = self.client.get_json(&file_url).await?;

        let name = std::path::Path::new(&resolved)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| resolved.clone());

        Ok(RemoteEntry {
            name,
            path: resolved,
            is_dir: false,
            size: file_info.size,
            modified: None,
            permissions: None,
            owner: self.account_name.clone(),
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let encoded = encode_file_path(&resolved);

        let file_url = format!(
            "{}/repository/files/{}?ref={}",
            self.project_api(),
            encoded,
            urlencoding::encode(self.active_branch()),
        );

        self.client.exists(&file_url).await
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(()) // REST API — no persistent connection
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let mut info = format!(
            "GitLab Repository: {}\nBranch: {}\nVisibility: {}",
            self.project_path,
            self.active_branch(),
            self.project_visibility,
        );
        if let Some(size) = self.repo_size {
            info.push_str(&format!("\nRepository Size: {} bytes", size));
        }
        info.push_str(&format!("\nWrite Access: {}", self.can_push));
        if let Some(ref name) = self.account_name {
            info.push_str(&format!("\nAuthenticated as: {}", name));
        }
        Ok(info)
    }

    fn supports_chmod(&self) -> bool {
        false
    }

    fn supports_symlinks(&self) -> bool {
        false
    }
}
