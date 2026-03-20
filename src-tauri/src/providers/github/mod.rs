//! GitHub Storage Provider
//!
//! Dual-mode GitHub integration:
//! - **Repo mode** (default): Browse repository contents via the Contents/Trees API,
//!   upload/download files, manage branches.
//! - **Releases mode**: Browse releases as directories, download/upload release assets.
//!
//! Authentication uses a GitHub Personal Access Token (classic or fine-grained).

pub mod auth;
mod client;
mod errors;
mod graphql;
mod model;
mod rate_limit;
pub(crate) mod actions;
pub(crate) mod pages;
mod releases_mode;
mod repo_mode;

pub use self::model::*;

use self::client::GitHubHttpClient;
use self::errors::GitHubError;
use self::releases_mode::{
    asset_to_entry, create_release, delete_release, delete_release_asset,
    download_release_asset, get_release_info, list_release_assets, list_releases,
    release_to_entry, upload_release_asset, CreateReleaseParams, VIRTUAL_RELEASES_DIR,
};

use super::{ProviderConfig, ProviderError, ProviderType, RemoteEntry, StorageProvider};
use async_trait::async_trait;
use secrecy::SecretString;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitHubVirtualPath {
    ReleasesRoot,
    ReleaseTag(String),
    ReleaseAsset { tag: String, asset_name: String },
}

/// Write-mode policy detected during connect.
#[derive(Debug, Clone, PartialEq)]
pub enum GitHubWriteMode {
    /// Not yet determined (pre-connect).
    Unknown,
    /// User has push access and branch is not protected — direct commits allowed.
    DirectWrite,
    /// Branch is protected but user/app has bypass — direct commits allowed.
    /// Falls back to BranchWorkflow if the first write is rejected.
    DirectWriteProtected {
        /// SHA of the protected branch tip (for fallback branch creation).
        base_sha: String,
    },
    /// Branch is protected; provider will auto-create a working branch.
    BranchWorkflow { branch: String },
    /// Token only has read access.
    ReadOnly { reason: String },
}

/// Configuration for connecting to a GitHub repository.
#[derive(Debug, Clone)]
pub struct GitHubConfig {
    /// Personal access token.
    pub token: String,
    /// Repository owner (user or org).
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Branch to browse (empty = default branch).
    pub branch: String,
    /// Initial path within the repo.
    pub initial_path: Option<String>,
}

impl GitHubConfig {
    /// Build a [`GitHubConfig`] from the generic [`ProviderConfig`].
    ///
    /// Expects:
    /// - `host`: `"owner/repo"` or `"github.com/owner/repo"`
    /// - `password`: the PAT
    /// - `extra["branch"]`: optional branch override
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config
            .password
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("GitHub token is required".into()))?;

        let host = config.host.trim().to_string();
        let path_part = host
            .strip_prefix("https://github.com/")
            .or_else(|| host.strip_prefix("github.com/"))
            .unwrap_or(&host);

        let parts: Vec<&str> = path_part.trim_matches('/').splitn(2, '/').collect();
        if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Host must be 'owner/repo' (e.g. 'octocat/Hello-World')".into(),
            ));
        }

        let (repo, host_branch) = match parts[1].rsplit_once('@') {
            Some((repo_name, branch_name)) if !repo_name.is_empty() && !branch_name.is_empty() => {
                (repo_name.to_string(), Some(branch_name.to_string()))
            }
            _ => (parts[1].to_string(), None),
        };

        let branch = config
            .extra
            .get("branch")
            .cloned()
            .or(host_branch)
            .unwrap_or_default();

        Ok(Self {
            token,
            owner: parts[0].to_string(),
            repo,
            branch,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// GitHub storage provider implementing the [`StorageProvider`] trait.
///
/// Connects to a single repository and exposes its tree as a virtual filesystem.
pub struct GitHubProvider {
    client: GitHubHttpClient,
    owner: String,
    repo: String,
    branch: String,
    current_path: String,
    connected: bool,
    write_mode: GitHubWriteMode,
    /// `(branch, path) -> SHA` — used to supply the required `sha` on updates/deletes.
    sha_cache: HashMap<(String, String), String>,
    account_email: Option<String>,
    account_name: Option<String>,
    /// Repository size in bytes (converted from KB on connect).
    repo_size: Option<u64>,
    repo_private: bool,
    is_bot_token: bool,
    default_branch: String,
}

impl std::fmt::Debug for GitHubProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubProvider")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("branch", &self.branch)
            .field("current_path", &self.current_path)
            .field("connected", &self.connected)
            .field("write_mode", &self.write_mode)
            .finish()
    }
}

impl GitHubProvider {
    /// Create a new provider from a parsed config.
    pub fn new(config: GitHubConfig) -> Self {
        let token = SecretString::from(config.token);
        let current_path = Self::normalise_path(config.initial_path.as_deref().unwrap_or(""));
        Self {
            client: GitHubHttpClient::new(token),
            owner: config.owner,
            repo: config.repo,
            branch: config.branch,
            current_path,
            connected: false,
            write_mode: GitHubWriteMode::Unknown,
            sha_cache: HashMap::new(),
            account_email: None,
            account_name: None,
            repo_size: None,
            repo_private: false,
            is_bot_token: false,
            default_branch: String::from("main"),
        }
    }

    // ── Public accessors for Tauri commands ──────────────────────────

    /// Repository owner.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Repository name.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Whether the repository is private.
    pub fn is_private(&self) -> bool {
        self.repo_private
    }

    /// Owner identity for commits (both committer and author fields).
    /// - Installation token (.pem): returns the owner's identity so their avatar appears.
    ///   The bot is added via Co-authored-by trailer in the commit message.
    /// - PAT / Device Flow: returns None, GitHub uses the authenticated user's identity.
    fn owner_identity(&self) -> Option<GitHubCommitter> {
        if self.is_bot_token {
            let email = format!("{}@users.noreply.github.com", self.owner);
            Some(GitHubCommitter {
                name: self.owner.clone(),
                email,
            })
        } else {
            None
        }
    }

    pub fn content_committer(&self) -> Option<GitHubCommitter> {
        self.owner_identity()
    }

    pub fn content_author(&self) -> Option<GitHubCommitter> {
        self.owner_identity()
    }

    /// Append Co-authored-by trailer for bot mode.
    /// This ensures aeroftp[bot] appears as contributor on the commit.
    pub fn with_co_author(&self, message: &str) -> String {
        if self.is_bot_token {
            format!(
                "{}\n\nCo-authored-by: aeroftp[bot] <268949222+aeroftp[bot]@users.noreply.github.com>",
                message
            )
        } else {
            message.to_string()
        }
    }

    /// Current write mode.
    pub fn write_mode(&self) -> &GitHubWriteMode {
        &self.write_mode
    }

    // ── Release management ─────────────────────────────────────────

    /// List all releases as virtual directory entries.
    pub async fn list_all_releases(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        list_releases(&mut self.client, &self.owner, &self.repo).await
    }

    /// List assets belonging to a specific release tag.
    pub async fn list_assets_for_release(
        &mut self,
        tag: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        list_release_assets(&mut self.client, &self.owner, &self.repo, tag).await
    }

    /// Create a new release.
    pub async fn create_new_release(
        &mut self,
        tag: &str,
        name: &str,
        body: &str,
        draft: bool,
        prerelease: bool,
    ) -> Result<GitHubRelease, ProviderError> {
        create_release(
            &mut self.client,
            &CreateReleaseParams {
                owner: &self.owner,
                repo: &self.repo,
                tag,
                name,
                body,
                draft,
                prerelease,
            },
        )
        .await
    }

    /// Upload a local file as a release asset.
    pub async fn upload_asset(
        &mut self,
        tag: &str,
        local_path: &str,
        asset_name: &str,
    ) -> Result<(), ProviderError> {
        upload_release_asset(
            &mut self.client,
            &self.owner,
            &self.repo,
            tag,
            local_path,
            asset_name,
        )
        .await
    }

    /// Delete an entire release by tag.
    pub async fn delete_release_by_tag(&mut self, tag: &str) -> Result<(), ProviderError> {
        delete_release(&mut self.client, &self.owner, &self.repo, tag).await
    }

    /// Delete a specific asset from a release.
    pub async fn delete_asset(
        &mut self,
        tag: &str,
        asset_name: &str,
    ) -> Result<(), ProviderError> {
        delete_release_asset(&mut self.client, &self.owner, &self.repo, tag, asset_name).await
    }

    /// Get release metadata.
    pub async fn get_release(&mut self, tag: &str) -> Result<GitHubRelease, ProviderError> {
        get_release_info(&mut self.client, &self.owner, &self.repo, tag).await
    }

    /// Download a release asset to a local file path.
    pub async fn download_asset(
        &mut self,
        tag: &str,
        asset_name: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        download_release_asset(&mut self.client, &self.owner, &self.repo, tag, asset_name, local_path).await
    }

    /// Atomic multi-file commit via GraphQL `createCommitOnBranch`.
    ///
    /// Accepts UTF-8 string content for additions. The content is converted to
    /// bytes internally and base64-encoded for the GraphQL mutation.
    ///
    /// Returns the new commit SHA on success.
    pub async fn batch_commit(
        &mut self,
        branch: &str,
        message: &str,
        additions: &[(String, String)],
        deletions: &[String],
    ) -> Result<String, ProviderError> {
        let head_oid =
            graphql::get_head_sha(&mut self.client, &self.owner, &self.repo, branch)
                .await
                .map_err(ProviderError::from)?;

        let additions_bytes: Vec<(String, Vec<u8>)> = additions
            .iter()
            .map(|(path, content)| (path.clone(), content.as_bytes().to_vec()))
            .collect();

        let params = graphql::BatchCommitParams {
            owner: &self.owner,
            repo: &self.repo,
            branch,
            head_oid: &head_oid,
            message,
            additions: &additions_bytes,
            deletions,
        };

        graphql::batch_commit(&mut self.client, &params)
            .await
            .map_err(ProviderError::from)
    }

    /// List all branches of the repository.
    pub async fn list_branches(&mut self) -> Result<Vec<serde_json::Value>, ProviderError> {
        let url = format!("{}/branches?per_page=100", self.repo_url());
        let branches: Vec<serde_json::Value> = self
            .client
            .get_paginated_json_array(&url)
            .await
            .map_err(ProviderError::from)?;
        Ok(branches)
    }

    /// Full `{owner}/{repo}` slug.
    fn repo_slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// API URL for the repository root.
    fn repo_url(&self) -> String {
        format!(
            "{}/repos/{}/{}",
            self.client.api_base(),
            self.owner,
            self.repo,
        )
    }

    fn branch_api_url(&self, branch: &str) -> String {
        format!(
            "{}/branches/{}",
            self.repo_url(),
            urlencoding::encode(branch),
        )
    }

    fn encoded_branch_for_path(&self) -> String {
        urlencoding::encode(self.active_branch()).into_owned()
    }

    fn encoded_content_branch_for_query(&self) -> String {
        urlencoding::encode(self.content_branch()).into_owned()
    }

    fn content_branch(&self) -> &str {
        match &self.write_mode {
            GitHubWriteMode::BranchWorkflow { branch } => branch.as_str(),
            _ => self.active_branch(),
        }
    }

    fn contents_query_url(&self, path: &str) -> String {
        let resolved = Self::normalise_path(path);
        if resolved.is_empty() {
            format!(
                "{}/contents?ref={}",
                self.repo_url(),
                self.encoded_content_branch_for_query(),
            )
        } else {
            // Encode each path segment individually — do NOT encode the slashes
            let encoded_path = resolved
                .split('/')
                .map(|seg| urlencoding::encode(seg).into_owned())
                .collect::<Vec<_>>()
                .join("/");
            format!(
                "{}/contents/{}?ref={}",
                self.repo_url(),
                encoded_path,
                self.encoded_content_branch_for_query(),
            )
        }
    }

    fn contents_mutation_url(&self, path: &str) -> String {
        let normalised = Self::normalise_path(path);
        let encoded_path = normalised
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        format!(
            "{}/contents/{}",
            self.repo_url(),
            encoded_path,
        )
    }

    fn releases_entry(&self) -> RemoteEntry {
        RemoteEntry {
            name: VIRTUAL_RELEASES_DIR.to_string(),
            path: format!("/{}/", VIRTUAL_RELEASES_DIR),
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: self.account_name.clone(),
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: {
                let mut metadata = HashMap::new();
                metadata.insert("virtual".to_string(), "true".to_string());
                metadata.insert("kind".to_string(), "github-releases".to_string());
                metadata
            },
        }
    }

    fn parse_virtual_path(&self, path: &str) -> Option<GitHubVirtualPath> {
        let normalized = Self::normalise_path(path);
        let mut segments = normalized.split('/');
        let root = segments.next()?;
        if root != VIRTUAL_RELEASES_DIR {
            return None;
        }

        match (segments.next(), segments.next(), segments.next()) {
            (None, None, None) => Some(GitHubVirtualPath::ReleasesRoot),
            (Some(tag), None, None) if !tag.is_empty() => {
                Some(GitHubVirtualPath::ReleaseTag(tag.to_string()))
            }
            (Some(tag), Some(asset_name), None) if !tag.is_empty() && !asset_name.is_empty() => {
                Some(GitHubVirtualPath::ReleaseAsset {
                    tag: tag.to_string(),
                    asset_name: asset_name.to_string(),
                })
            }
            _ => None,
        }
    }

    /// Active branch (falls back to default_branch if none specified).
    pub fn active_branch(&self) -> &str {
        if self.branch.is_empty() {
            &self.default_branch
        } else {
            &self.branch
        }
    }

    fn sanitise_branch_component(value: &str) -> String {
        let mut output = String::with_capacity(value.len());
        let mut last_was_separator = false;

        for ch in value.chars() {
            let valid = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
            if valid {
                output.push(ch.to_ascii_lowercase());
                last_was_separator = false;
            } else if !last_was_separator {
                output.push('-');
                last_was_separator = true;
            }
        }

        let trimmed = output.trim_matches('-').trim_matches('.');
        if trimmed.is_empty() {
            "work".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn workflow_branch_name(user_login: &str, base_branch: &str) -> String {
        format!(
            "aeroftp/{}/{}",
            Self::sanitise_branch_component(user_login),
            Self::sanitise_branch_component(base_branch),
        )
    }

    async fn ensure_branch_exists_from_sha(
        &mut self,
        branch: &str,
        base_sha: &str,
    ) -> Result<(), ProviderError> {
        match self
            .client
            .get_json::<GitHubBranch>(&self.branch_api_url(branch))
            .await
        {
            Ok(_) => return Ok(()),
            Err(GitHubError::PathNotFound(_)
                | GitHubError::RepoNotFound
                | GitHubError::BranchNotFound(_)
                | GitHubError::NotFound(_)) => {}
            Err(e) => return Err(ProviderError::from(e)),
        }

        let body = serde_json::json!({
            "ref": format!("refs/heads/{}", branch),
            "sha": base_sha,
        });

        match self
            .client
            .post_json::<serde_json::Value>(&format!("{}/git/refs", self.repo_url()), &body)
            .await
        {
            Ok(_) => Ok(()),
            Err(GitHubError::Unprocessable(message))
                if message.to_lowercase().contains("reference already exists") =>
            {
                Ok(())
            }
            Err(e) => Err(ProviderError::from(e)),
        }
    }

    /// Return the current working branch when branch workflow mode is active.
    pub fn working_branch(&self) -> Option<&str> {
        match &self.write_mode {
            GitHubWriteMode::BranchWorkflow { branch } => Some(branch.as_str()),
            _ => None,
        }
    }

    /// Create a pull request from the active working branch back to the base branch.
    pub async fn create_pull_request(
        &mut self,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<GitHubPullRequest, ProviderError> {
        let working_branch = self.working_branch().ok_or_else(|| {
            ProviderError::NotSupported(
                "Pull request creation is only available in GitHub branch workflow mode"
                    .to_string(),
            )
        })?;

        let payload = serde_json::json!({
            "title": title,
            "head": format!("{}:{}", self.owner, working_branch),
            "base": self.active_branch(),
            "body": body.unwrap_or_default(),
            "draft": draft,
            "maintainer_can_modify": true,
        });

        self.client
            .post_json::<GitHubPullRequest>(&format!("{}/pulls", self.repo_url()), &payload)
            .await
            .map_err(ProviderError::from)
    }

    /// Reuse an existing open pull request for the working branch, or create one.
    pub async fn ensure_pull_request(
        &mut self,
        title: &str,
        body: Option<&str>,
        draft: bool,
    ) -> Result<GitHubPullRequest, ProviderError> {
        let working_branch = self.working_branch().ok_or_else(|| {
            ProviderError::NotSupported(
                "Pull request creation is only available in GitHub branch workflow mode"
                    .to_string(),
            )
        })?;

        let url = format!(
            "{}/pulls?state=open&head={}&base={}",
            self.repo_url(),
            urlencoding::encode(&format!("{}:{}", self.owner, working_branch)),
            urlencoding::encode(self.active_branch()),
        );

        let existing: Vec<GitHubPullRequest> = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        if let Some(pr) = existing.into_iter().next() {
            return Ok(pr);
        }

        self.create_pull_request(title, body, draft).await
    }

    /// Store a SHA in the cache for later use by update/delete operations.
    fn cache_sha(&mut self, path: &str, sha: &str) {
        self.sha_cache.insert(
            (self.content_branch().to_string(), path.to_string()),
            sha.to_string(),
        );
    }

    /// Look up a cached SHA for a path on the active branch.
    fn get_cached_sha(&self, path: &str) -> Option<&String> {
        self.sha_cache
            .get(&(self.content_branch().to_string(), path.to_string()))
    }

    /// Normalise a path: strip leading `/`, collapse `//`, resolve `..`.
    pub(crate) fn normalise_path(path: &str) -> String {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() || trimmed == "." {
            return String::new();
        }

        let mut segments: Vec<&str> = Vec::new();
        for seg in trimmed.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                s => segments.push(s),
            }
        }
        segments.join("/")
    }

    /// Resolve a potentially relative path against `current_path`.
    fn resolve_path(&self, path: &str) -> String {
        let is_absolute = path.starts_with('/');
        let clean = path.trim_matches('/');
        if clean.is_empty() || clean == "." {
            return self.current_path.clone();
        }
        if is_absolute || self.current_path.is_empty() {
            return Self::normalise_path(clean);
        }
        Self::normalise_path(&format!("{}/{}", self.current_path, clean))
    }

    // ── GitHub Pages ─────────────────────────────────────────────

    /// Get Pages site info. Returns Ok(None) if Pages is not enabled.
    pub async fn get_pages_info(&mut self) -> Result<Option<pages::PagesSite>, ProviderError> {
        match pages::get_pages_site(&mut self.client, &self.owner, &self.repo).await {
            Ok(site) => Ok(Some(site)),
            Err(GitHubError::NotFound(_))
            | Err(GitHubError::RepoNotFound)
            | Err(GitHubError::PathNotFound(_))
            | Err(GitHubError::ApiError { status: 404, .. }) => Ok(None),
            Err(e) => Err(ProviderError::from(e)),
        }
    }

    /// List recent Pages builds.
    pub async fn list_pages_builds(&mut self) -> Result<Vec<pages::PagesBuild>, ProviderError> {
        pages::list_pages_builds(&mut self.client, &self.owner, &self.repo)
            .await
            .map_err(ProviderError::from)
    }

    /// Get latest Pages build.
    pub async fn get_latest_pages_build(&mut self) -> Result<pages::PagesBuild, ProviderError> {
        pages::get_latest_build(&mut self.client, &self.owner, &self.repo)
            .await
            .map_err(ProviderError::from)
    }

    /// Trigger a Pages rebuild (legacy build_type only).
    pub async fn trigger_pages_build(&mut self) -> Result<pages::PagesBuildStatus, ProviderError> {
        pages::request_build(&mut self.client, &self.owner, &self.repo)
            .await
            .map_err(ProviderError::from)
    }

    /// Update Pages configuration.
    pub async fn update_pages_config(
        &mut self,
        cname: Option<&str>,
        https_enforced: Option<bool>,
        source_branch: Option<&str>,
        source_path: Option<&str>,
    ) -> Result<(), ProviderError> {
        pages::update_pages_config(
            &mut self.client, &self.owner, &self.repo,
            cname, https_enforced, source_branch, source_path,
        )
        .await
        .map_err(ProviderError::from)
    }

    /// Check DNS health for custom domain.
    pub async fn pages_health_check(&mut self) -> Result<pages::PagesHealthCheck, ProviderError> {
        pages::get_health_check(&mut self.client, &self.owner, &self.repo)
            .await
            .map_err(ProviderError::from)
    }

    // ── GitHub Actions ────────────────────────────────────────────

    /// List recent workflow runs.
    pub async fn list_actions_runs(
        &mut self,
        branch: Option<&str>,
        per_page: u8,
    ) -> Result<Vec<actions::WorkflowRunInfo>, ProviderError> {
        actions::list_workflow_runs(&mut self.client, &self.owner, &self.repo, branch, per_page)
            .await
            .map_err(ProviderError::from)
    }

    /// Re-run a workflow.
    pub async fn rerun_actions_workflow(&mut self, run_id: u64) -> Result<(), ProviderError> {
        actions::rerun_workflow(&mut self.client, &self.owner, &self.repo, run_id)
            .await
            .map_err(ProviderError::from)
    }

    /// Re-run only failed jobs.
    pub async fn rerun_failed_jobs(&mut self, run_id: u64) -> Result<(), ProviderError> {
        actions::rerun_failed_jobs(&mut self.client, &self.owner, &self.repo, run_id)
            .await
            .map_err(ProviderError::from)
    }

    /// Cancel an in-progress workflow run.
    pub async fn cancel_actions_run(&mut self, run_id: u64) -> Result<(), ProviderError> {
        actions::cancel_workflow_run(&mut self.client, &self.owner, &self.repo, run_id)
            .await
            .map_err(ProviderError::from)
    }

    /// Enable Pages on the repository.
    pub async fn enable_pages(
        &mut self,
        branch: &str,
        path: &str,
        build_type: &str,
    ) -> Result<pages::PagesSite, ProviderError> {
        pages::create_pages_site(&mut self.client, &self.owner, &self.repo, branch, path, build_type)
            .await
            .map_err(ProviderError::from)
    }

    /// Disable Pages on the repository.
    pub async fn disable_pages(&mut self) -> Result<(), ProviderError> {
        pages::delete_pages_site(&mut self.client, &self.owner, &self.repo)
            .await
            .map_err(ProviderError::from)
    }
}

#[async_trait]
impl StorageProvider for GitHubProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::GitHub
    }

    fn display_name(&self) -> String {
        format!("GitHub ({})", self.repo_slug())
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // 1. Validate token — try GET /user first (PAT/Device Flow),
        //    fallback to GET /app for installation tokens
        let user_login: String;
        match self
            .client
            .get_json::<GitHubUser>(&format!("{}/user", self.client.api_base()))
            .await
        {
            Ok(user) => {
                user_login = user.login.clone();
                self.account_name = user.name.clone();
                self.account_email = user.email.clone().or(Some(user.login.clone()));
            }
            Err(e) => {
                // Distinguish: 401 Unauthorized → likely installation token
                // Other errors (network, rate limit) → propagate
                let is_auth_error = matches!(&e,
                    GitHubError::Unauthorized |
                    GitHubError::InsufficientPermissions(_)
                );
                if !is_auth_error {
                    // Network error, rate limit, etc. — not a token type issue
                    return Err(ProviderError::from(e));
                }
                // Installation token — GET /user returns 401, but repo access works
                log::info!("GitHub: user endpoint returned 401, treating as installation token");
                user_login = "aeroftp[bot]".to_string();
                self.account_name = Some("AeroFTP App".to_string());
                self.account_email = Some("aeroftp[bot]@users.noreply.github.com".to_string());
            }
        }

        // 2. Resolve repo via GET /repos/{owner}/{repo}
        let repo: GitHubRepo = self
            .client
            .get_json(&self.repo_url())
            .await
            .map_err(ProviderError::from)?;

        self.default_branch = repo.default_branch;
        self.repo_private = repo.private;
        self.repo_size = Some(repo.size * 1024); // KB → bytes

        // If no branch was specified, use the repo's default.
        if self.branch.is_empty() {
            self.branch = self.default_branch.clone();
        }

        // 3. Detect write mode
        // Installation tokens report push:false even with contents:write — override for bot tokens
        let is_installation_token = user_login.ends_with("[bot]");
        self.is_bot_token = is_installation_token;
        let can_push = if is_installation_token {
            true // Installation tokens have the permissions granted to the app
        } else {
            repo.permissions.as_ref().map(|p| p.push).unwrap_or(false)
        };

        if !can_push {
            self.write_mode = GitHubWriteMode::ReadOnly {
                reason: "Token does not have push access to this repository".to_string(),
            };
        } else {
            // Check if the target branch is protected
            let branch_url = format!(
                "{}/branches/{}",
                self.repo_url(),
                self.encoded_branch_for_path(),
            );
            match self
                .client
                .get_json::<GitHubBranch>(&branch_url)
                .await
            {
                Ok(branch_info) => {
                    if branch_info.protected {
                        // Branch is protected, but user/app might have bypass.
                        // Use DirectWriteProtected: attempts direct push first,
                        // falls back to BranchWorkflow if the write is rejected.
                        log::info!(
                            "GitHub: branch '{}' is protected — will attempt direct write (bypass), fallback to working branch if rejected",
                            self.active_branch()
                        );
                        self.write_mode = GitHubWriteMode::DirectWriteProtected {
                            base_sha: branch_info.commit.sha,
                        };
                    } else {
                        self.write_mode = GitHubWriteMode::DirectWrite;
                    }
                }
                Err(GitHubError::PathNotFound(_) | GitHubError::RepoNotFound) => {
                    // Branch doesn't exist yet — we'll create it on first write.
                    self.write_mode = GitHubWriteMode::DirectWrite;
                }
                Err(e) => {
                    // Non-fatal: default to direct write, will fail on push if wrong.
                    log::warn!(
                        "GitHub: could not check branch protection for '{}': {}",
                        self.active_branch(),
                        e
                    );
                    self.write_mode = GitHubWriteMode::DirectWrite;
                }
            }
        }

        self.connected = true;

        log::info!(
            "GitHub: connected to {} as {} (branch: {}, write_mode: {:?}, size: {} KB, private: {})",
            self.repo_slug(),
            user_login,
            self.content_branch(),
            self.write_mode,
            repo.size,
            self.repo_private,
        );

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.sha_cache.clear();
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

        if let Some(virtual_path) = self.parse_virtual_path(&resolved) {
            return match virtual_path {
                GitHubVirtualPath::ReleasesRoot => list_releases(&mut self.client, &self.owner, &self.repo).await,
                GitHubVirtualPath::ReleaseTag(tag) => {
                    list_release_assets(&mut self.client, &self.owner, &self.repo, &tag).await
                }
                GitHubVirtualPath::ReleaseAsset { tag, asset_name } => {
                    let release = get_release_info(&mut self.client, &self.owner, &self.repo, &tag).await?;
                    let asset = release.assets.iter().find(|asset| asset.name == asset_name).ok_or_else(|| {
                        ProviderError::NotFound(format!("Asset '{}' not found in release '{}'", asset_name, tag))
                    })?;
                    Ok(vec![asset_to_entry(asset, &tag)])
                }
            };
        }

        let url = self.contents_query_url(&resolved);

        let items: Vec<GitHubContent> = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        let mut entries = Vec::with_capacity(items.len());
        for item in &items {
            let is_dir = item.content_type == "dir";
            let entry_path = if resolved.is_empty() {
                item.name.clone()
            } else {
                format!("{}/{}", resolved, item.name)
            };

            // Cache SHA for later update/delete operations.
            self.cache_sha(&item.path, &item.sha);

            entries.push(RemoteEntry {
                name: item.name.clone(),
                path: entry_path,
                is_dir,
                size: item.size.unwrap_or(0),
                modified: None, // Contents API doesn't return mtime
                permissions: None,
                owner: self.account_name.clone(),
                group: None,
                is_symlink: item.content_type == "symlink",
                link_target: None,
                mime_type: None,
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("sha".to_string(), item.sha.clone());
                    if let Some(ref url) = item.html_url {
                        m.insert("html_url".to_string(), url.clone());
                    }
                    if item.content_type == "submodule" {
                        m.insert("submodule".to_string(), "true".to_string());
                    }
                    m
                },
            });
        }

        if resolved.is_empty() && !entries.iter().any(|entry| entry.name == VIRTUAL_RELEASES_DIR) {
            entries.push(self.releases_entry());
        }

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

        if self.parse_virtual_path(&resolved).is_some() {
            self.list(path).await?;
            self.current_path = resolved;
            return Ok(());
        }

        // Verify the path exists and is a directory (by listing it).
        if !resolved.is_empty() {
            let url = self.contents_query_url(&resolved);
            // A successful list response means the dir exists.
            let _: Vec<GitHubContent> = self
                .client
                .get_json(&url)
                .await
                .map_err(ProviderError::from)?;
        }

        self.current_path = resolved;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path.is_empty() {
            return Ok(());
        }
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

        if let Some(GitHubVirtualPath::ReleaseAsset { tag, asset_name }) = self.parse_virtual_path(&resolved) {
            return download_release_asset(
                &mut self.client,
                &self.owner,
                &self.repo,
                &tag,
                &asset_name,
                local_path,
            )
            .await;
        }

        // Get file metadata to find the download URL.
        let url = self.contents_query_url(&resolved);
        let content: GitHubContent = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        let download_url = content.download_url.ok_or_else(|| {
            ProviderError::TransferFailed(format!(
                "No download URL for '{}' (might be a directory or submodule)",
                resolved,
            ))
        })?;

        let total_size = content.size.unwrap_or(0);
        let data = self
            .client
            .get_raw_bytes(&download_url)
            .await
            .map_err(ProviderError::from)?;

        if let Some(ref cb) = on_progress {
            cb(data.len() as u64, total_size);
        }

        tokio::fs::write(local_path, &data)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(remote_path);

        if self.parse_virtual_path(&resolved).is_some() {
            return Err(ProviderError::NotSupported(
                "Byte reads are not supported for virtual GitHub release paths".to_string(),
            ));
        }

        let url = self.contents_query_url(&resolved);
        let content: GitHubContent = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        // For files < 1 MB the content is inline as base64.
        if let Some(ref b64) = content.content {
            let clean = b64.replace('\n', "");
            return base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &clean)
                .map_err(|e| ProviderError::TransferFailed(format!("base64 decode: {}", e)));
        }

        // Otherwise use the raw download URL.
        let download_url = content.download_url.ok_or_else(|| {
            ProviderError::TransferFailed(format!(
                "No download URL for '{}'",
                resolved,
            ))
        })?;

        self.client
            .get_raw_bytes(&download_url)
            .await
            .map_err(ProviderError::from)
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

        let resolved = self.resolve_path(remote_path);
        if let Some(GitHubVirtualPath::ReleaseAsset { tag, asset_name }) = self.parse_virtual_path(&resolved) {
            return upload_release_asset(
                &mut self.client,
                &self.owner,
                &self.repo,
                &tag,
                local_path,
                &asset_name,
            )
            .await;
        }

        // Enforce write mode.
        if let GitHubWriteMode::ReadOnly { ref reason } = self.write_mode {
            return Err(ProviderError::PermissionDenied(reason.clone()));
        }

        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        let total = data.len() as u64;

        // GitHub Contents API limit: 100 MB.
        const MAX_CONTENT_SIZE: u64 = 100 * 1024 * 1024;
        if total > MAX_CONTENT_SIZE {
            return Err(ProviderError::TransferFailed(format!(
                "File size ({:.1} MB) exceeds GitHub's 100 MB limit. Use Releases for large files.",
                total as f64 / 1_048_576.0,
            )));
        }

        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

        // Check if the file already exists (need its SHA for update).
        let sha = self.get_cached_sha(&resolved).cloned();
        let sha = if sha.is_some() {
            sha
        } else {
            // Try to fetch the file to get its SHA.
            let url = self.contents_query_url(&resolved);
            match self.client.get_json::<GitHubContent>(&url).await {
                Ok(existing) => {
                    self.cache_sha(&resolved, &existing.sha);
                    Some(existing.sha)
                }
                Err(GitHubError::PathNotFound(_) | GitHubError::RepoNotFound) => None,
                Err(e) => return Err(ProviderError::from(e)),
            }
        };

        let file_name = resolved
            .rsplit('/')
            .next()
            .unwrap_or(&resolved);
        let message = if sha.is_some() {
            self.with_co_author(&format!("Update {}", file_name))
        } else {
            self.with_co_author(&format!("Create {}", file_name))
        };

        let update = GitHubContentUpdate {
            message,
            content: b64,
            sha,
            branch: Some(self.content_branch().to_string()),
            committer: self.content_committer(),
            author: self.content_author(),
        };

        let url = self.contents_mutation_url(&resolved);
        let body = serde_json::to_value(&update)
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let result = self
            .client
            .put_json(&url, &body)
            .await;

        // If DirectWriteProtected and push was rejected, fallback to BranchWorkflow
        let resp = match result {
            Err(ref e) if matches!(self.write_mode, GitHubWriteMode::DirectWriteProtected { .. }) => {
                let err_str = format!("{}", e);
                if err_str.contains("protected") || err_str.contains("403") || err_str.contains("422") {
                    log::info!("GitHub: direct push rejected on protected branch — falling back to working branch");
                    let base_sha = if let GitHubWriteMode::DirectWriteProtected { ref base_sha } = self.write_mode {
                        base_sha.clone()
                    } else {
                        unreachable!()
                    };
                    let user = self.account_name.clone().unwrap_or_else(|| "aeroftp".to_string());
                    let workflow_branch = Self::workflow_branch_name(&user, self.active_branch());
                    self.ensure_branch_exists_from_sha(&workflow_branch, &base_sha).await
                        .map_err(|e2| ProviderError::TransferFailed(format!(
                            "Cannot create working branch '{}': {}", workflow_branch, e2
                        )))?;
                    self.write_mode = GitHubWriteMode::BranchWorkflow { branch: workflow_branch };

                    // Retry with the new branch
                    let retry_update = GitHubContentUpdate {
                        message: update.message.clone(),
                        content: update.content.clone(),
                        sha: update.sha.clone(),
                        branch: Some(self.content_branch().to_string()),
                        committer: self.content_committer(),
            author: self.content_author(),
                    };
                    let retry_body = serde_json::to_value(&retry_update)
                        .map_err(|e3| ProviderError::TransferFailed(e3.to_string()))?;
                    self.client.put_json(&url, &retry_body).await.map_err(ProviderError::from)?
                } else {
                    return Err(ProviderError::from(result.unwrap_err()));
                }
            }
            Err(e) => return Err(ProviderError::from(e)),
            Ok(v) => v,
        };

        // Update SHA cache with the new file's SHA.
        if let Some(content_obj) = resp.get("content") {
            if let Some(new_sha) = content_obj.get("sha").and_then(|s| s.as_str()) {
                self.cache_sha(&resolved, new_sha);
            }
        }

        if let Some(ref cb) = on_progress {
            cb(total, total);
        }

        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        if self.parse_virtual_path(&resolved).is_some() {
            return Err(ProviderError::NotSupported(
                "Creating GitHub releases via mkdir is not supported".to_string(),
            ));
        }

        if let GitHubWriteMode::ReadOnly { ref reason } = self.write_mode {
            return Err(ProviderError::PermissionDenied(reason.clone()));
        }

        // GitHub has no directory concept — create a `.gitkeep` placeholder.
        let gitkeep_path = format!("{}/.gitkeep", resolved);

        let update = GitHubContentUpdate {
            message: self.with_co_author(&format!("Create directory {}", resolved)),
            content: String::new(), // empty file
            sha: None,
            branch: Some(self.content_branch().to_string()),
            committer: self.content_committer(),
            author: self.content_author(),
        };

        let url = self.contents_mutation_url(&gitkeep_path);
        let body = serde_json::to_value(&update)
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        self.client
            .put_json(&url, &body)
            .await
            .map_err(ProviderError::from)?;

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        if let Some(virtual_path) = self.parse_virtual_path(&resolved) {
            return match virtual_path {
                GitHubVirtualPath::ReleasesRoot => Err(ProviderError::NotSupported(
                    "Deleting the virtual releases root is not supported".to_string(),
                )),
                GitHubVirtualPath::ReleaseTag(tag) => {
                    delete_release(&mut self.client, &self.owner, &self.repo, &tag).await
                }
                GitHubVirtualPath::ReleaseAsset { tag, asset_name } => {
                    delete_release_asset(
                        &mut self.client,
                        &self.owner,
                        &self.repo,
                        &tag,
                        &asset_name,
                    )
                    .await
                }
            };
        }

        if let GitHubWriteMode::ReadOnly { ref reason } = self.write_mode {
            return Err(ProviderError::PermissionDenied(reason.clone()));
        }

        // We need the SHA. Check cache first.
        let sha = if let Some(s) = self.get_cached_sha(&resolved) {
            s.clone()
        } else {
            let url = self.contents_query_url(&resolved);
            let content: GitHubContent = self
                .client
                .get_json(&url)
                .await
                .map_err(ProviderError::from)?;
            self.cache_sha(&resolved, &content.sha);
            content.sha
        };

        let del = GitHubContentDelete {
            message: self.with_co_author(&format!(
                "Delete {}",
                resolved.rsplit('/').next().unwrap_or(&resolved)
            )),
            sha,
            branch: Some(self.content_branch().to_string()),
            committer: self.content_committer(),
            author: self.content_author(),
        };

        let url = self.contents_mutation_url(&resolved);
        let body = serde_json::to_value(&del)
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        self.client
            .delete_json(&url, &body)
            .await
            .map_err(ProviderError::from)?;

        // Remove from cache.
        self.sha_cache.remove(&(
            self.content_branch().to_string(),
            resolved,
        ));

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        if let Some(virtual_path) = self.parse_virtual_path(&resolved) {
            return match virtual_path {
                GitHubVirtualPath::ReleasesRoot => Err(ProviderError::NotSupported(
                    "Deleting all releases in one operation is not supported".to_string(),
                )),
                GitHubVirtualPath::ReleaseTag(tag) => {
                    delete_release(&mut self.client, &self.owner, &self.repo, &tag).await
                }
                GitHubVirtualPath::ReleaseAsset { .. } => self.delete(path).await,
            };
        }

        // GitHub has no empty-dir concept; delete all contents recursively.
        self.rmdir_recursive(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        if self.parse_virtual_path(&resolved) == Some(GitHubVirtualPath::ReleasesRoot) {
            return Err(ProviderError::NotSupported(
                "Recursive deletion of all releases is not supported".to_string(),
            ));
        }
        let entries = self.list(path).await?;
        for entry in entries {
            if entry.is_dir {
                // Box the recursive future to avoid infinite type.
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if self.parse_virtual_path(&self.resolve_path(from)).is_some()
            || self.parse_virtual_path(&self.resolve_path(to)).is_some()
        {
            return Err(ProviderError::NotSupported(
                "Renaming GitHub releases or release assets is not supported".to_string(),
            ));
        }

        // GitHub Contents API has no rename — download + upload + delete.
        let data = self.download_to_bytes(from).await?;
        let resolved_to = self.resolve_path(to);

        // Upload to new location.
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &data,
        );
        let update = GitHubContentUpdate {
            message: self.with_co_author(&format!(
                "Rename {} -> {}",
                from.rsplit('/').next().unwrap_or(from),
                to.rsplit('/').next().unwrap_or(to),
            )),
            content: b64,
            sha: None,
            branch: Some(self.content_branch().to_string()),
            committer: self.content_committer(),
            author: self.content_author(),
        };

        let url = self.contents_mutation_url(&resolved_to);
        let body = serde_json::to_value(&update)
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        self.client
            .put_json(&url, &body)
            .await
            .map_err(ProviderError::from)?;

        // Delete the original.
        self.delete(from).await?;

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);

        if let Some(virtual_path) = self.parse_virtual_path(&resolved) {
            return match virtual_path {
                GitHubVirtualPath::ReleasesRoot => Ok(self.releases_entry()),
                GitHubVirtualPath::ReleaseTag(tag) => {
                    let release = get_release_info(&mut self.client, &self.owner, &self.repo, &tag).await?;
                    Ok(release_to_entry(&release))
                }
                GitHubVirtualPath::ReleaseAsset { tag, asset_name } => {
                    let release = get_release_info(&mut self.client, &self.owner, &self.repo, &tag).await?;
                    let asset = release.assets.iter().find(|asset| asset.name == asset_name).ok_or_else(|| {
                        ProviderError::NotFound(format!("Asset '{}' not found in release '{}'", asset_name, tag))
                    })?;
                    Ok(asset_to_entry(asset, &tag))
                }
            };
        }

        let url = self.contents_query_url(&resolved);

        let content: GitHubContent = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        self.cache_sha(&content.path, &content.sha);

        Ok(RemoteEntry {
            name: content.name.clone(),
            path: content.path.clone(),
            is_dir: content.content_type == "dir",
            size: content.size.unwrap_or(0),
            modified: None,
            permissions: None,
            owner: self.account_name.clone(),
            group: None,
            is_symlink: content.content_type == "symlink",
            link_target: None,
            mime_type: None,
            metadata: {
                let mut m = HashMap::new();
                m.insert("sha".to_string(), content.sha.clone());
                m
            },
        })
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // Lightweight: just hit /rate_limit which doesn't cost a rate-limit point.
        let _: serde_json::Value = self
            .client
            .get_json(&format!("{}/rate_limit", self.client.api_base()))
            .await
            .map_err(ProviderError::from)?;
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let write_desc = match &self.write_mode {
            GitHubWriteMode::Unknown => "unknown",
            GitHubWriteMode::DirectWrite => "direct push",
            GitHubWriteMode::DirectWriteProtected { .. } => "direct push (bypass)",
            GitHubWriteMode::BranchWorkflow { .. } => "branch workflow (protected)",
            GitHubWriteMode::ReadOnly { .. } => "read-only",
        };

        let rl = self.client.rate_limit();
        let branch_info = match self.working_branch() {
            Some(working_branch) => format!(
                "Base: {} | Working: {}",
                self.active_branch(),
                working_branch,
            ),
            None => format!("Branch: {}", self.active_branch()),
        };

        Ok(format!(
            "GitHub {} | {} | {} | Private: {} | Size: {} KB | API: {}/{} remaining",
            self.repo_slug(),
            branch_info,
            write_desc,
            self.repo_private,
            self.repo_size.unwrap_or(0) / 1024,
            rl.remaining,
            rl.limit,
        ))
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        _expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        let resolved = self.resolve_path(path);
        if let Some(GitHubVirtualPath::ReleaseAsset { .. }) = self.parse_virtual_path(&resolved) {
            let entry = self.stat(path).await?;
            if let Some(download_url) = entry.metadata.get("browser_download_url") {
                return Ok(download_url.clone());
            }
        }
        // Return the html_url for the file on GitHub.
        Ok(format!(
            "https://github.com/{}/{}/blob/{}/{}",
            self.owner, self.repo, self.content_branch(), resolved,
        ))
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    async fn checksum(
        &mut self,
        path: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut map = HashMap::new();
        if let Some(sha) = entry.metadata.get("sha") {
            // GitHub uses its own SHA (git blob SHA), not a standard file hash.
            map.insert("git-sha1".to_string(), sha.clone());
        }
        Ok(map)
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, _path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Use Git Trees API with recursive flag — lists all files in repo
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            self.owner, self.repo, urlencoding::encode(self.active_branch())
        );

        #[derive(serde::Deserialize)]
        struct TreeResponse {
            tree: Vec<TreeItem>,
        }
        #[derive(serde::Deserialize)]
        struct TreeItem {
            path: String,
            #[serde(rename = "type")]
            item_type: String,
            sha: String,
            size: Option<u64>,
        }

        let response: TreeResponse = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        let pattern_lower = pattern.to_lowercase();

        let entries = response
            .tree
            .into_iter()
            .filter(|item| {
                // Match filename or path against pattern (case-insensitive)
                let name = item.path.rsplit('/').next().unwrap_or(&item.path);
                name.to_lowercase().contains(&pattern_lower)
                    || item.path.to_lowercase().contains(&pattern_lower)
            })
            .take(100)
            .map(|item| {
                let name = item.path.rsplit('/').next().unwrap_or(&item.path).to_string();
                RemoteEntry {
                    name,
                    path: item.path,
                    is_dir: item.item_type == "tree",
                    size: item.size.unwrap_or(0),
                    modified: None,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("sha".to_string(), item.sha);
                        m
                    },
                }
            })
            .collect();

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_config(host: &str, branch: Option<&str>, initial_path: Option<&str>) -> ProviderConfig {
        let mut extra = HashMap::new();
        if let Some(branch_name) = branch {
            extra.insert("branch".to_string(), branch_name.to_string());
        }
        ProviderConfig {
            name: "GitHub test".to_string(),
            provider_type: ProviderType::GitHub,
            host: host.to_string(),
            port: Some(443),
            username: None,
            password: Some("token".to_string()),
            initial_path: initial_path.map(|path| path.to_string()),
            extra,
        }
    }

    #[test]
    fn test_config_extracts_branch_from_host_suffix() {
        let config = GitHubConfig::from_provider_config(&provider_config(
            "axpnet/aeroftp-test-playground@main",
            None,
            Some("/"),
        ))
        .unwrap();
        assert_eq!(config.owner, "axpnet");
        assert_eq!(config.repo, "aeroftp-test-playground");
        assert_eq!(config.branch, "main");
    }

    #[test]
    fn test_new_normalizes_root_initial_path() {
        let provider = GitHubProvider::new(GitHubConfig::from_provider_config(&provider_config(
            "axpnet/aeroftp-test-playground",
            None,
            Some("/"),
        ))
        .unwrap());
        assert_eq!(provider.current_path, "");
    }

    #[test]
    fn test_resolve_path_honors_absolute_paths() {
        let mut provider = GitHubProvider::new(GitHubConfig::from_provider_config(&provider_config(
            "axpnet/aeroftp-test-playground",
            Some("main"),
            Some("/docs"),
        ))
        .unwrap());
        provider.current_path = "docs/guides".to_string();
        assert_eq!(provider.resolve_path("/README.md"), "README.md");
        assert_eq!(provider.resolve_path("guide.md"), "docs/guides/guide.md");
    }

    #[test]
    fn test_workflow_branch_name_is_stable_and_sanitized() {
        assert_eq!(
            GitHubProvider::workflow_branch_name("AxPNet", "release/2026.03"),
            "aeroftp/axpnet/release-2026.03"
        );
    }

    #[test]
    fn test_content_branch_uses_working_branch_when_available() {
        let mut provider = GitHubProvider::new(GitHubConfig::from_provider_config(&provider_config(
            "axpnet/aeroftp-test-playground",
            Some("main"),
            Some("/"),
        ))
        .unwrap());
        provider.write_mode = GitHubWriteMode::BranchWorkflow {
            branch: "aeroftp/tester/main".to_string(),
        };

        assert_eq!(provider.active_branch(), "main");
        assert_eq!(provider.content_branch(), "aeroftp/tester/main");
    }

    #[test]
    fn test_parse_virtual_release_paths() {
        let provider = GitHubProvider::new(GitHubConfig::from_provider_config(&provider_config(
            "axpnet/aeroftp-test-playground",
            Some("main"),
            Some("/"),
        ))
        .unwrap());

        assert_eq!(
            provider.parse_virtual_path("/.github-releases"),
            Some(GitHubVirtualPath::ReleasesRoot)
        );
        assert_eq!(
            provider.parse_virtual_path("/.github-releases/v1.0.0"),
            Some(GitHubVirtualPath::ReleaseTag("v1.0.0".to_string()))
        );
        assert_eq!(
            provider.parse_virtual_path("/.github-releases/v1.0.0/app.deb"),
            Some(GitHubVirtualPath::ReleaseAsset {
                tag: "v1.0.0".to_string(),
                asset_name: "app.deb".to_string(),
            })
        );
    }
}
