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
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
    /// Accept invalid/self-signed TLS certificates (for self-hosted instances).
    pub accept_invalid_certs: bool,
}

impl GitLabConfig {
    /// Build a [`GitLabConfig`] from the generic [`ProviderConfig`].
    ///
    /// Expects:
    /// - `host`: `"owner/repo"`, `"gitlab.com/owner/repo"`, or `"self-hosted.com/owner/repo"`
    /// - `password`: the access token
    /// - `extra["branch"]`: optional branch override
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone().ok_or_else(|| {
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

        let accept_invalid_certs = config
            .extra
            .get("verify_cert")
            .map(|v| v == "false")
            .unwrap_or(false);

        Ok(Self {
            token,
            api_base,
            project_path: clean_project,
            branch,
            initial_path: config.initial_path.clone(),
            accept_invalid_certs,
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
        // "owner/repo" format: domain is actually the owner
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
            client: GitLabHttpClient::new(token, config.api_base, config.accept_invalid_certs)?,
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

    /// The default branch of the project.
    pub fn default_branch_name(&self) -> &str {
        &self.default_branch
    }

    /// List all branches.
    pub async fn list_branches(&mut self) -> Result<Vec<model::GitLabBranch>, ProviderError> {
        let path = format!("{}/repository/branches", self.project_api());
        self.client.get_paginated(&path, 100).await
    }

    /// Switch to a different branch.
    pub async fn switch_branch(&mut self, branch: &str) -> Result<(), ProviderError> {
        log::info!("GitLab: switching branch to '{}'", branch);
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
        log::info!(
            "GitLab: switched to branch '{}' (can_push: {})",
            self.branch,
            self.can_push
        );
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

    // ── Releases ────────────────────────────────────────────────────

    /// List all releases.
    pub async fn list_releases(&mut self) -> Result<Vec<model::GitLabRelease>, ProviderError> {
        log::info!("GitLab: listing releases for {}", self.project_path);
        let path = format!("{}/releases", self.project_api());
        let releases: Vec<model::GitLabRelease> = self.client.get_paginated(&path, 100).await?;
        log::info!("GitLab: found {} releases", releases.len());
        Ok(releases)
    }

    /// Create a release.
    pub async fn create_release(
        &mut self,
        tag: &str,
        name: &str,
        description: &str,
    ) -> Result<model::GitLabRelease, ProviderError> {
        log::info!(
            "GitLab: creating release '{}' (tag: {}) on branch '{}'",
            name,
            tag,
            self.active_branch()
        );
        let body = serde_json::json!({
            "tag_name": tag,
            "name": name,
            "description": description,
            "ref": self.active_branch(),
        });
        let path = format!("{}/releases", self.project_api());
        let release = self.client.post_json(&path, &body).await?;
        log::info!("GitLab: release '{}' created successfully", tag);
        Ok(release)
    }

    /// Delete a release (preserves the git tag).
    pub async fn delete_release(&mut self, tag: &str) -> Result<(), ProviderError> {
        log::info!("GitLab: deleting release '{}'", tag);
        let path = format!(
            "{}/releases/{}",
            self.project_api(),
            urlencoding::encode(tag),
        );
        self.client.delete(&path).await?;
        log::info!("GitLab: release '{}' deleted", tag);
        Ok(())
    }

    /// List asset links for a release.
    pub async fn list_release_links(
        &mut self,
        tag: &str,
    ) -> Result<Vec<model::GitLabReleaseLink>, ProviderError> {
        log::info!("GitLab: listing asset links for release '{}'", tag);
        let path = format!(
            "{}/releases/{}/assets/links",
            self.project_api(),
            urlencoding::encode(tag),
        );
        let links: Vec<model::GitLabReleaseLink> = self.client.get_paginated(&path, 100).await?;
        log::info!(
            "GitLab: found {} asset links for release '{}'",
            links.len(),
            tag
        );
        Ok(links)
    }

    /// Upload a file as release asset via Generic Packages, then link it.
    ///
    /// `link_type` values: "other" (default), "package", "image", "runbook".
    pub async fn upload_release_asset(
        &mut self,
        tag: &str,
        local_path: &str,
        asset_name: &str,
        link_type: Option<&str>,
    ) -> Result<model::GitLabReleaseLink, ProviderError> {
        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to read file: {}", e)))?;

        let content_type = mime_guess::from_path(asset_name)
            .first_or_octet_stream()
            .to_string();

        // Determine link_type: explicit > auto-detect from extension > "other"
        let resolved_type = link_type.unwrap_or_else(|| {
            let lower = asset_name.to_lowercase();
            if lower.ends_with(".deb")
                || lower.ends_with(".rpm")
                || lower.ends_with(".msi")
                || lower.ends_with(".exe")
                || lower.ends_with(".dmg")
                || lower.ends_with(".pkg")
                || lower.ends_with(".snap")
                || lower.ends_with(".flatpak")
                || lower.ends_with(".appimage")
                || lower.ends_with(".whl")
                || lower.ends_with(".gem")
                || lower.ends_with(".nupkg")
                || lower.ends_with(".zip")
                || lower.ends_with(".tar.gz")
                || lower.ends_with(".tar.bz2")
                || lower.ends_with(".tar.xz")
                || lower.ends_with(".tar.zst")
                || lower.ends_with(".7z")
                || lower.ends_with(".rar")
                || lower.ends_with(".jar")
                || lower.ends_with(".apk")
                || lower.ends_with(".aab")
                || lower.ends_with(".ipa")
            {
                "package"
            } else if lower.ends_with(".png")
                || lower.ends_with(".jpg")
                || lower.ends_with(".jpeg")
                || lower.ends_with(".gif")
                || lower.ends_with(".svg")
                || lower.ends_with(".webp")
                || lower.ends_with(".ico")
                || lower.ends_with(".bmp")
                || lower.ends_with(".iso")
                || lower.ends_with(".img")
                || lower.ends_with(".qcow2")
                || lower.ends_with(".vmdk")
                || lower.ends_with(".ova")
                || lower.ends_with(".vhd")
                || lower.ends_with(".vhdx")
            {
                "image"
            } else {
                "other"
            }
        });

        log::info!(
            "GitLab: uploading release asset '{}' to tag '{}' (type: {}, size: {} bytes)",
            asset_name,
            tag,
            resolved_type,
            bytes.len()
        );

        // Step 1: Upload to Generic Packages
        let pkg_path = format!(
            "{}/packages/generic/release-assets/{}/{}",
            self.project_api(),
            urlencoding::encode(tag),
            urlencoding::encode(asset_name),
        );
        self.client
            .put_bytes(&pkg_path, bytes, &content_type)
            .await?;

        // Use the API download URL which works for both public and private projects
        let download_url = format!(
            "{}{}/packages/generic/release-assets/{}/{}",
            self.client.api_base(),
            self.project_api(),
            urlencoding::encode(tag),
            urlencoding::encode(asset_name),
        );

        // Step 2: Link to release
        let link_body = serde_json::json!({
            "name": asset_name,
            "url": download_url,
            "link_type": resolved_type,
        });
        let link_path = format!(
            "{}/releases/{}/assets/links",
            self.project_api(),
            urlencoding::encode(tag),
        );
        self.client.post_json(&link_path, &link_body).await
    }

    /// Delete a release asset link.
    pub async fn delete_release_link(
        &mut self,
        tag: &str,
        link_id: u64,
    ) -> Result<(), ProviderError> {
        log::info!(
            "GitLab: deleting asset link {} from release '{}'",
            link_id,
            tag
        );
        let path = format!(
            "{}/releases/{}/assets/links/{}",
            self.project_api(),
            urlencoding::encode(tag),
            link_id,
        );
        self.client.delete(&path).await?;
        log::info!(
            "GitLab: asset link {} deleted from release '{}'",
            link_id,
            tag
        );
        Ok(())
    }

    /// Download a release asset to a local file with authentication.
    pub async fn download_release_asset(
        &mut self,
        url: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        log::info!("GitLab: downloading release asset to '{}'", local_path);
        let resp = self.client.get_raw_response(url).await?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Download failed: {}", e)))?;
        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to write file: {}", e)))?;
        log::info!(
            "GitLab: asset downloaded ({} bytes) to '{}'",
            bytes.len(),
            local_path
        );
        Ok(())
    }

    /// Read a file's content as UTF-8 string from the repo root (for CHANGELOG import).
    /// Always reads relative to root, regardless of current_path.
    pub async fn read_file_content(&mut self, path: &str) -> Result<String, ProviderError> {
        let clean = normalise_path(path);
        let encoded = encode_file_path(&clean);
        let url = format!(
            "{}/repository/files/{}/raw?ref={}",
            self.project_api(),
            encoded,
            urlencoding::encode(self.active_branch()),
        );
        let bytes = self.client.get_raw_bytes(&url).await?;
        String::from_utf8(bytes)
            .map_err(|e| ProviderError::Other(format!("File is not valid UTF-8: {}", e)))
    }

    // ── Merge Requests ─────────────────────────────────────────────

    /// Create a merge request (or return existing one).
    pub async fn create_merge_request(
        &mut self,
        title: &str,
        body: &str,
    ) -> Result<String, ProviderError> {
        let source = self.active_branch().to_string();
        let target = self.default_branch.clone();

        if source == target {
            return Err(ProviderError::Other(
                "Cannot create merge request: source and target branch are the same. Switch to a different branch first.".into(),
            ));
        }

        log::info!(
            "GitLab: creating merge request '{}' ({} -> {})",
            title,
            source,
            target
        );

        // Check for existing open MR with same source branch
        let search_path = format!(
            "{}/merge_requests?state=opened&source_branch={}",
            self.project_api(),
            urlencoding::encode(&source),
        );
        let existing: Vec<model::GitLabMergeRequest> =
            self.client.get_json(&search_path).await.unwrap_or_default();

        if let Some(mr) = existing.first() {
            log::info!("GitLab: found existing MR {}", mr.web_url);
            return Ok(mr.web_url.clone());
        }

        // Create new MR
        let mr_body = serde_json::json!({
            "source_branch": source,
            "target_branch": target,
            "title": title,
            "description": body,
            "remove_source_branch": true,
        });
        let mr_path = format!("{}/merge_requests", self.project_api());
        let mr: model::GitLabMergeRequest = self.client.post_json(&mr_path, &mr_body).await?;
        log::info!("GitLab: merge request created: {}", mr.web_url);
        Ok(mr.web_url)
    }

    // ── Web URLs ───────────────────────────────────────────────────

    /// Build web URL for a file or directory on GitLab.
    pub fn web_url(&self, path: &str, is_dir: bool) -> String {
        let base = self.client.api_base().replace("/api/v4", "");
        let kind = if is_dir { "tree" } else { "blob" };
        let clean = path.trim_matches('/');
        if clean.is_empty() {
            format!("{}/{}", base, self.project_path)
        } else {
            // Encode branch (may contain /) and path segments for valid browser URL
            let encoded_branch = urlencoding::encode(self.active_branch());
            let encoded_path = clean
                .split('/')
                .map(|seg| urlencoding::encode(seg).into_owned())
                .collect::<Vec<_>>()
                .join("/");
            format!(
                "{}/{}/-/{}/{}/{}",
                base, self.project_path, kind, encoded_branch, encoded_path,
            )
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
        log::info!(
            "GitLab: committing {} actions on branch '{}': {}",
            actions.len(),
            self.active_branch(),
            message
        );
        let body = serde_json::json!({
            "branch": self.active_branch(),
            "commit_message": message,
            "actions": actions,
        });
        let path = format!("{}/repository/commits", self.project_api());
        let commit: GitLabCommit = self.client.post_json(&path, &body).await?;
        log::info!("GitLab: commit {} created successfully", commit.id);
        Ok(commit)
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
        // 1. Validate token: GET /user
        match self.client.get_json::<GitLabUser>("/user").await {
            Ok(user) => {
                self.account_name = user.name.clone().or(Some(user.username.clone()));
                self.account_email = user
                    .email
                    .clone()
                    .or(Some(format!("{}@gitlab", user.username)));
            }
            Err(ProviderError::AuthenticationFailed(_)) => {
                // Project access tokens return a bot user, still works
                self.account_name = Some("GitLab Token".to_string());
                self.account_email = None;
            }
            Err(e) => return Err(e),
        }

        // 2. Resolve project: GET /projects/:id
        let project_url = format!("/projects/{}", encode_project_path(&self.project_path));
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
                // Conservative: don't assume write access if we can't verify
                self.can_push = false;
                log::warn!("GitLab: could not verify branch permissions, defaulting to read-only");
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
        self.client
            .get_json::<Vec<GitLabTreeEntry>>(&tree_path)
            .await?;

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
        let total_size = resp.content_length().unwrap_or(0);

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Download failed: {}", e)))?;

        if let Some(ref progress) = on_progress {
            progress(total_size, total_size);
        }

        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to write file: {}", e)))?;

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
        let content_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

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
        Ok(()) // REST API: no persistent connection
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_field_accepts_owner_repo_shorthand() {
        let (api, path) = parse_host_field("owner/repo").unwrap();
        assert_eq!(api, "https://gitlab.com/api/v4");
        assert_eq!(path, "owner/repo");
    }

    #[test]
    fn parse_host_field_accepts_self_hosted_instance_url() {
        let (api, path) = parse_host_field("https://gitlab.example.com/group/project").unwrap();
        assert_eq!(api, "https://gitlab.example.com/api/v4");
        assert_eq!(path, "group/project");
    }

    #[test]
    fn parse_host_field_strips_scheme_and_upgrades_to_https() {
        // The helper accepts both http:// and https:// prefixes but the
        // computed api_base is always https://: input scheme is dropped as a
        // defense-in-depth measure. Test asserts the actual behavior so any
        // future change is intentional.
        let (api, path) = parse_host_field("http://gitlab.local/team/svc/").unwrap();
        assert_eq!(api, "https://gitlab.local/api/v4");
        assert_eq!(path, "team/svc");

        // https input works identically
        let (api2, _) = parse_host_field("https://gitlab.local/team/svc").unwrap();
        assert_eq!(api2, "https://gitlab.local/api/v4");
    }

    #[test]
    fn parse_host_field_handles_nested_groups() {
        let (api, path) = parse_host_field("https://gitlab.com/team/sub/project").unwrap();
        assert_eq!(api, "https://gitlab.com/api/v4");
        assert_eq!(path, "team/sub/project");
    }

    #[test]
    fn parse_host_field_rejects_missing_project_path() {
        // Only a domain, no project segment
        assert!(parse_host_field("gitlab.com").is_err());
        assert!(parse_host_field("gitlab.com/").is_err());
        // Empty input
        assert!(parse_host_field("").is_err());
    }

    #[test]
    fn encode_project_path_percent_encodes_slashes_only() {
        assert_eq!(encode_project_path("group/project"), "group%2Fproject");
        assert_eq!(
            encode_project_path("team/sub/project"),
            "team%2Fsub%2Fproject"
        );
        // No slash → unchanged
        assert_eq!(encode_project_path("solo"), "solo");
        // Other characters stay as-is (they go through urlencode elsewhere)
        assert_eq!(encode_project_path("team-1/svc.api"), "team-1%2Fsvc.api");
    }

    #[test]
    fn encode_file_path_percent_encodes_slashes() {
        assert_eq!(encode_file_path("src/main.rs"), "src%2Fmain.rs");
        assert_eq!(encode_file_path("a/b/c"), "a%2Fb%2Fc");
        assert_eq!(encode_file_path("Cargo.toml"), "Cargo.toml");
    }

    #[test]
    fn normalise_path_strips_slashes_and_dot() {
        assert_eq!(normalise_path(""), "");
        assert_eq!(normalise_path("/"), "");
        assert_eq!(normalise_path("."), "");
        assert_eq!(normalise_path("/src/main.rs/"), "src/main.rs");
        assert_eq!(normalise_path("///a///"), "a");
        assert_eq!(normalise_path("src/main.rs"), "src/main.rs");
    }
}
