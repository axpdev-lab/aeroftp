//! GitHub Storage Provider
//!
//! Dual-mode GitHub integration:
//! - **Repo mode** (default): Browse repository contents via the Contents/Trees API,
//!   upload/download files, manage branches.
//! - **Releases mode**: Browse releases as directories, download/upload release assets.
//!
//! Authentication uses a GitHub Personal Access Token (classic or fine-grained).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub(crate) mod actions;
pub mod auth;
mod client;
mod errors;
mod graphql;
mod model;
pub(crate) mod pages;
mod rate_limit;
mod releases_mode;
mod repo_mode;

pub use self::model::*;

use self::client::GitHubHttpClient;
use self::errors::GitHubError;
use self::releases_mode::{
    asset_to_entry, create_release, delete_release, delete_release_asset, download_release_asset,
    get_release_info, list_release_assets, list_releases, release_to_entry, upload_release_asset,
    CreateReleaseParams, VIRTUAL_RELEASES_DIR,
};
use crate::providers::HttpResultExt;

use super::{
    ProviderConfig, ProviderError, ProviderType, RemoteEntry, ShareLinkOptions, ShareLinkResult,
    StorageProvider,
};
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
    /// User has push access and branch is not protected: direct commits allowed.
    DirectWrite,
    /// Branch is protected but user/app has bypass: direct commits allowed.
    /// Falls back to BranchWorkflow if the first write is rejected; the
    /// fallback reads the base's tip then, not at connect (S3b).
    DirectWriteProtected {},
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

/// How long a working branch's classification answers for the writes that
/// follow it, while they succeed (`refuse_stale_write`, S2 b).
const BRANCH_CHECK_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// How many working-branch names a walk tries (`name`, `name-2` ...).
const WORKING_BRANCH_CANDIDATES: u32 = 9;

/// What a working branch holds against its base
/// ([`GitHubProvider::classify_working_branch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkingBranchState {
    /// No such branch.
    Missing,
    /// Nothing the base lacks (merged with a merge commit, or empty). Such
    /// a branch IS reused (R2): its history is already in the base, and it
    /// is fast-forwarded to the base before a write.
    NotAhead,
    /// Work the base does not hold yet.
    Pending,
    /// Squash- or rebase-merged, its tip the merged head: never written
    /// again (its commits are not in the base, so a write would revive
    /// them). This is the only kind of merged branch that is not reused.
    Merged,
}

/// Raises the session's poison when dropped, unless the write it guards was
/// refused by an answer (`refused`): the fallback write (S2 d).
struct PoisonUnlessRefused(Option<std::sync::Arc<std::sync::atomic::AtomicBool>>);

impl PoisonUnlessRefused {
    fn refused(mut self) {
        self.0 = None;
    }
}

impl Drop for PoisonUnlessRefused {
    fn drop(&mut self) {
        if let Some(poison) = &self.0 {
            poison.store(true, std::sync::atomic::Ordering::SeqCst);
        }
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
    /// `(branch, path) -> SHA`: used to supply the required `sha` on updates/deletes.
    sha_cache: HashMap<(String, String), String>,
    account_email: Option<String>,
    account_name: Option<String>,
    /// The authenticated login (`GitHubUser.login`, `aeroftp[bot]` for an
    /// installation token): unique per account, unlike the display name in
    /// `account_name`. It names the account's working branch (P7).
    user_login: Option<String>,
    /// Repository size in bytes (converted from KB on connect).
    repo_size: Option<u64>,
    repo_private: bool,
    is_bot_token: bool,
    default_branch: String,
    /// Raised by the working-branch fallback; see
    /// `StorageProvider::target_poison`. Never lowered: a new connection is a
    /// new provider.
    poison: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When the working branch was last classified as still writable
    /// (`refuse_stale_write`).
    last_branch_check: Option<std::time::Instant>,
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
    /// QA-GH-004: Returns Result: HTTP client construction can fail.
    pub fn new(config: GitHubConfig) -> Result<Self, ProviderError> {
        let token = SecretString::from(config.token);
        let current_path = Self::normalise_path(config.initial_path.as_deref().unwrap_or(""));
        let poison = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut client = GitHubHttpClient::new(token).map_err(ProviderError::from)?;
        client.poison = Some(poison.clone());
        Ok(Self {
            client,
            owner: config.owner,
            repo: config.repo,
            branch: config.branch,
            current_path,
            connected: false,
            write_mode: GitHubWriteMode::Unknown,
            sha_cache: HashMap::new(),
            account_email: None,
            account_name: None,
            user_login: None,
            repo_size: None,
            repo_private: false,
            is_bot_token: false,
            default_branch: String::from("main"),
            poison,
            last_branch_check: None,
        })
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
    /// Mutable reference to the HTTP client (for GraphQL calls from commands).
    pub fn client_mut(&mut self) -> &mut client::GitHubHttpClient {
        &mut self.client
    }

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
    pub async fn delete_asset(&mut self, tag: &str, asset_name: &str) -> Result<(), ProviderError> {
        delete_release_asset(&mut self.client, &self.owner, &self.repo, tag, asset_name).await
    }

    /// Download a release asset to a local file path.
    pub async fn download_asset(
        &mut self,
        tag: &str,
        asset_name: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        download_release_asset(
            &mut self.client,
            &self.owner,
            &self.repo,
            tag,
            asset_name,
            local_path,
        )
        .await
    }

    /// Atomic batch upload of binary files via GraphQL `createCommitOnBranch`.
    ///
    /// Unlike `batch_commit()` which takes `(String, String)` text content,
    /// this accepts raw `Vec<u8>` for binary files. Uses the content branch
    /// (working branch if branch-workflow is active) and appends co-author
    /// trailer for bot tokens.
    pub async fn batch_upload(
        &mut self,
        message: &str,
        additions: &[(String, Vec<u8>)],
        deletions: &[String],
    ) -> Result<String, ProviderError> {
        self.refuse_stale_write().await?;
        let branch = self.content_branch().to_string();
        let head_oid = graphql::get_head_sha(&mut self.client, &self.owner, &self.repo, &branch)
            .await
            .map_err(ProviderError::from)?;

        let full_message = self.with_co_author(message);

        let params = graphql::BatchCommitParams {
            owner: &self.owner,
            repo: &self.repo,
            branch: &branch,
            head_oid: &head_oid,
            message: &full_message,
            additions,
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

    /// The branch every content read and write of the session uses: the
    /// working branch in the branch workflow, else the active one (S3a).
    /// The base (`active_branch`) is read only for what concerns the base
    /// itself: its protection, pull requests into it, the working-branch
    /// name and classification, and the session summary
    /// (`a_content_read_never_names_the_base_branch` keeps it so).
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
            // Encode each path segment individually: do NOT encode the slashes
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
        format!("{}/contents/{}", self.repo_url(), encoded_path,)
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

    /// A connected provider whose branch `main` is protected, its API at
    /// `api_origin`: the state `connect` leaves before a working branch is
    /// needed.
    #[cfg(test)]
    pub(crate) fn protected_for_test(api_origin: String) -> Self {
        let mut extra = HashMap::new();
        extra.insert("branch".to_string(), "main".to_string());
        let config = ProviderConfig {
            name: "repo".to_string(),
            provider_type: ProviderType::GitHub,
            host: "o/r".to_string(),
            port: Some(443),
            username: None,
            password: Some("token".to_string()),
            initial_path: Some("/".to_string()),
            extra,
        };
        let mut provider = Self::connected_for_test(&config, "alice", "main");
        provider.client.api_origin_override = Some(api_origin);
        provider.write_mode = GitHubWriteMode::DirectWriteProtected {};
        provider
    }

    /// A provider in the state `connect` leaves it in, without the network:
    /// the account and the repository's default branch as the API reported
    /// them, the configured branch (or that default) active.
    #[cfg(test)]
    pub(crate) fn connected_for_test(
        config: &ProviderConfig,
        account: &str,
        default_branch: &str,
    ) -> Self {
        let mut provider = Self::new(GitHubConfig::from_provider_config(config).unwrap()).unwrap();
        provider.account_email = Some(account.to_string());
        provider.user_login = Some(account.to_string());
        provider.default_branch = default_branch.to_string();
        if provider.branch.is_empty() {
            provider.branch = provider.default_branch.clone();
        }
        provider.connected = true;
        provider
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

    /// Before every tree write (S2 b): the poison, then, on the working
    /// branch, whether it is still there and not merged. A pull request
    /// merged while the session stayed open (or the branch deleted) would
    /// take the next write onto a branch whose work is done, reviving its
    /// commits: the session is poisoned and refuses, and the next connection
    /// starts afresh. Only in the branch workflow.
    ///
    /// The bound. The check is the full classification, because nothing
    /// cheaper sees a merge: a squash or rebase merge moves the BASE, never
    /// the working branch's own ref. It costs one request for a branch that
    /// is only ahead (the comparison), and for one that diverged the
    /// comparison, its tip and one request per 100 pull requests. A write
    /// skips it when the classification is younger than
    /// [`BRANCH_CHECK_WINDOW`] AND the session's last tree-write request got
    /// a success answer (`last_tree_write_ok`: a failed or unanswered write
    /// forces the next check). So while writes succeed, a run of any length
    /// adds at most one classification per window, whatever its number of
    /// files, and nothing between.
    ///
    /// What the window cannot see: a merge or deletion inside it, after the
    /// check. Writes until the window ends can land on that branch (a
    /// deleted one fails them, which ends the window); the first write after
    /// it is refused. Below the window, a merge in the instant between a
    /// check and its write cannot be excluded either: the contents API takes
    /// no expected branch head.
    async fn refuse_stale_write(&mut self) -> Result<(), ProviderError> {
        Self::refuse_when_poisoned(self)?;
        let GitHubWriteMode::BranchWorkflow { branch } = &self.write_mode else {
            return Ok(());
        };
        let recent = self.client.last_tree_write_ok
            && self
                .last_branch_check
                .is_some_and(|at| at.elapsed() < BRANCH_CHECK_WINDOW);
        if recent {
            return Ok(());
        }
        let branch = branch.clone();
        let base = self.active_branch().to_string();
        match self.classify_working_branch(&base, &branch).await? {
            WorkingBranchState::Pending | WorkingBranchState::NotAhead => {
                self.last_branch_check = Some(std::time::Instant::now());
                Ok(())
            }
            WorkingBranchState::Merged | WorkingBranchState::Missing => {
                self.last_branch_check = None;
                self.poison.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(ProviderError::TargetChanged)
            }
        }
    }

    /// A write of a poisoned session (see `StorageProvider::target_poison`)
    /// is refused here, before any request, its own preparing reads
    /// included. The client refuses any request that is not a read as well.
    pub(crate) fn refuse_when_poisoned(&self) -> Result<(), ProviderError> {
        if self.poison.load(std::sync::atomic::Ordering::SeqCst) {
            Err(ProviderError::TargetChanged)
        } else {
            Ok(())
        }
    }

    /// The working branch the fallback writes to for this account and branch:
    /// the authenticated login's, never the display name's (two accounts can
    /// share one, or have none). Before `connect` there is no login.
    fn own_working_branch(&self) -> Result<String, ProviderError> {
        let login = self
            .user_login
            .as_deref()
            .ok_or(ProviderError::NotConnected)?;
        Ok(Self::workflow_branch_name(login, self.active_branch()))
    }

    /// How far `branch` is ahead of and behind `base` (a branch or a SHA);
    /// `None` when the API answers 404 (the branch is not there).
    async fn compare_with(
        &mut self,
        base: &str,
        branch: &str,
    ) -> Result<Option<GitHubCompare>, ProviderError> {
        let url = format!(
            "{}/compare/{}...{}",
            self.repo_url(),
            urlencoding::encode(base),
            urlencoding::encode(branch)
        );
        match self.client.get_json::<GitHubCompare>(&url).await {
            Ok(compare) => Ok(Some(compare)),
            Err(
                GitHubError::PathNotFound(_)
                | GitHubError::RepoNotFound
                | GitHubError::BranchNotFound(_)
                | GitHubError::NotFound(_),
            ) => Ok(None),
            Err(e) => Err(ProviderError::from(e)),
        }
    }

    /// What a working branch holds against `base` (Q1, Q4).
    ///
    /// Ancestry says "pending" only when the branch is ahead and not behind:
    /// a merge of any kind puts a commit on the base the branch lacks. A
    /// branch that is ahead AND behind is either squash- or rebase-merged
    /// and kept, or pending work whose base moved on, and ancestry cannot
    /// tell them apart; a merged pull request can. The branch is MERGED only
    /// when a pull request from it into `base` was merged with the branch's
    /// current tip as its head (nothing written since). Anything else (an
    /// open one, none, a closed unmerged one, new commits after the merge) is
    /// pending. Only a 404 says the branch is not there; any other answer of
    /// any lookup is an error, never a guess.
    async fn classify_working_branch(
        &mut self,
        base: &str,
        branch: &str,
    ) -> Result<WorkingBranchState, ProviderError> {
        // `base` is a branch name or a tip SHA (S3b); the pull requests are
        // always looked up against the base's name.
        let Some(compare) = self.compare_with(base, branch).await? else {
            return Ok(WorkingBranchState::Missing);
        };
        if compare.ahead_by == 0 {
            return Ok(WorkingBranchState::NotAhead);
        }
        if compare.behind_by == 0 {
            return Ok(WorkingBranchState::Pending);
        }
        let tip = self
            .client
            .get_json::<GitHubBranch>(&self.branch_api_url(branch))
            .await
            .map_err(ProviderError::from)?
            .commit
            .sha;
        // Every page (S2 a): a merged pull request on a later page is as
        // merged as one on the first. A page shorter than asked is the last.
        const PER_PAGE: usize = 100;
        for page in 1.. {
            let url = format!(
                "{}/pulls?state=all&head={}&base={}&per_page={PER_PAGE}&page={page}",
                self.repo_url(),
                urlencoding::encode(&format!("{}:{}", self.owner, branch)),
                urlencoding::encode(self.active_branch()),
            );
            let pulls: Vec<GitHubPullRequest> = self
                .client
                .get_json(&url)
                .await
                .map_err(ProviderError::from)?;
            if pulls
                .iter()
                .any(|pr| pr.merged_at.is_some() && pr.head.sha == tip)
            {
                return Ok(WorkingBranchState::Merged);
            }
            if pulls.len() < PER_PAGE {
                break;
            }
        }
        Ok(WorkingBranchState::Pending)
    }

    /// The candidate working branches that exist: one
    /// `GET /git/matching-refs/heads/<name>` lists every branch whose name
    /// starts with the account's working-branch name, `-2` to `-9` included,
    /// so no candidate needs a request of its own to say it is missing.
    async fn existing_working_branches(
        &mut self,
        own: &str,
    ) -> Result<std::collections::HashSet<String>, ProviderError> {
        #[derive(serde::Deserialize)]
        struct MatchingRef {
            #[serde(rename = "ref")]
            name: String,
        }
        let url = format!("{}/git/matching-refs/heads/{own}", self.repo_url());
        let refs: Vec<MatchingRef> = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;
        Ok(refs
            .into_iter()
            .filter_map(|r| r.name.strip_prefix("refs/heads/").map(str::to_string))
            .collect())
    }

    /// The working branch for this account and base, and what it holds (Q1,
    /// Q4, B). Every candidate name (`workflow_branch_name`, then `-2` to
    /// `-9`) is looked at, not only up to the first gap: a merged original
    /// deleted on GitHub must not hide the pending work of a later one. The
    /// rule: the first PENDING candidate, if any (connect adopts it, a
    /// refused write goes there); with none pending, the first empty one
    /// (fast-forwarded), else the first missing name (created from the base).
    /// A MERGED one (squash- or rebase-merged) is never written again; one
    /// merged with a merge commit is NOT AHEAD and is reused, fast-forwarded
    /// first (R2). The cost is one listing, plus
    /// one compare per candidate that exists (and the pull-request lookup
    /// only for a diverged one): one request when there is none.
    ///
    /// `None` when every name is merged: only a refused write needing a new
    /// branch reports that ([`Self::allocate_working_branch`]); discovery
    /// at connect just finds nothing pending (C1).
    async fn current_working_branch(
        &mut self,
        base: &str,
    ) -> Result<Option<(String, WorkingBranchState)>, ProviderError> {
        let own = self.own_working_branch()?;
        let existing = self.existing_working_branches(&own).await?;
        let mut states = Vec::new();
        for n in 1..=WORKING_BRANCH_CANDIDATES {
            let name = if n == 1 {
                own.clone()
            } else {
                format!("{own}-{n}")
            };
            let state = if existing.contains(&name) {
                self.classify_working_branch(base, &name).await?
            } else {
                WorkingBranchState::Missing
            };
            if state == WorkingBranchState::Pending {
                return Ok(Some((name, state)));
            }
            states.push((name, state));
        }
        for wanted in [WorkingBranchState::NotAhead, WorkingBranchState::Missing] {
            if let Some(found) = states.iter().find(|(_, state)| *state == wanted) {
                return Ok(Some(found.clone()));
            }
        }
        Ok(None)
    }

    /// The branch a refused write goes to ([`Self::current_working_branch`]),
    /// or the refusal when every name is merged: the one place that is
    /// reported (C1).
    async fn allocate_working_branch(&mut self, base_tip: &str) -> Result<String, ProviderError> {
        match self.current_working_branch(base_tip).await? {
            Some((branch, _)) => Ok(branch),
            None => {
                let own = self.own_working_branch()?;
                Err(ProviderError::InvalidConfig(format!(
                    "The working branches {own} to {own}-{WORKING_BRANCH_CANDIDATES} were all \
                     merged and kept: delete the merged ones on GitHub to write again"
                )))
            }
        }
    }

    /// One decision, at connect (A1, point 1): a protected branch whose
    /// working branch holds PENDING work ([`Self::classify_working_branch`])
    /// starts the session on that working branch. The listing, the target
    /// identity and every write are the working branch's from the first
    /// request, so no session switches tree half way, and a sync of many
    /// files to a protected repository is not stopped after its first one
    /// at every connection. A merged, empty or missing one leaves the
    /// session on the protected branch (R2, Q1, Q4). Any failed lookup fails
    /// the connect rather than guess.
    async fn start_on_existing_working_branch(&mut self) -> Result<(), ProviderError> {
        // Nothing pending (every name merged included): the session stays
        // on the protected branch; browsing and reads work (C1).
        let base = self.active_branch().to_string();
        if let Some((branch, WorkingBranchState::Pending)) =
            self.current_working_branch(&base).await?
        {
            self.write_mode = GitHubWriteMode::BranchWorkflow { branch };
        }
        Ok(())
    }

    /// The one place a working branch is named (P7, P8):
    /// `aeroftp/<login>/<base>-<tag>`. The login is the authenticated one
    /// (GitHub logins are unique and case-insensitive, so lowercasing keeps
    /// them apart). The readable login and base are sanitised, which maps
    /// `release/x`, `release-x` and `Release-X` to one string, and a GitHub
    /// App's `x[bot]` to a user's `x-bot`, so `<tag>`, the first 16 hex
    /// digits of the SHA-256 of the login (lowercased) and the EXACT base
    /// name (S2 e), tells them apart.
    /// A branch named under the earlier scheme (display name, no tag) is
    /// neither adopted nor written again.
    fn workflow_branch_name(user_login: &str, base_branch: &str) -> String {
        use sha2::Digest;
        // The login AND the exact base (S2 e): a GitHub App's
        // `x[bot]` and a user `x-bot` sanitise to one readable login, and
        // the tag still tells them apart. A NUL cannot occur in either.
        // The login lowercased: GitHub logins are case-insensitive, so
        // `Alice` and `alice` are one account and one branch.
        let mut hasher = sha2::Sha256::new();
        hasher.update(user_login.to_ascii_lowercase().as_bytes());
        hasher.update([0]);
        hasher.update(base_branch.as_bytes());
        let digest = hasher.finalize();
        let tag: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!(
            "aeroftp/{}/{}-{}",
            Self::sanitise_branch_component(user_login),
            Self::sanitise_branch_component(base_branch),
            tag,
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
            Ok(_) => return self.fast_forward_if_behind(branch, base_sha).await,
            Err(
                GitHubError::PathNotFound(_)
                | GitHubError::RepoNotFound
                | GitHubError::BranchNotFound(_)
                | GitHubError::NotFound(_),
            ) => {}
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
            // GitHub refuses a ref that already exists with a 422, which the
            // client maps to `ApiError { status: 422 }`: the arm that read
            // "reference already exists" off `Unprocessable` never matched,
            // and a branch created meanwhile (another device, another tab)
            // failed the write. The branch is looked up again instead of
            // reading the message. #1506 row 164.
            Err(
                refused @ (GitHubError::Unprocessable(_)
                | GitHubError::ApiError { status: 422, .. }),
            ) => match self
                .client
                .get_json::<GitHubBranch>(&self.branch_api_url(branch))
                .await
            {
                Ok(_) => Ok(()),
                // Not there: the refusal stands. Any other failure of the
                // lookup (a timeout, a login) is the caller's to handle, not
                // hidden behind the refusal (CodeRabbit, #1520).
                Err(
                    GitHubError::PathNotFound(_)
                    | GitHubError::RepoNotFound
                    | GitHubError::BranchNotFound(_)
                    | GitHubError::NotFound(_),
                ) => Err(ProviderError::from(refused)),
                Err(lookup) => Err(ProviderError::from(lookup)),
            },
            Err(e) => Err(ProviderError::from(e)),
        }
    }

    /// A working branch that exists but holds nothing the base does not (its
    /// pull request merged, the branch kept) is behind the base: a write
    /// there would land on an older tree. It is moved up to `base_sha` first,
    /// a plain update with no force, which GitHub accepts only as a fast
    /// forward (R2). A branch ahead of the base keeps its own commits.
    async fn fast_forward_if_behind(
        &mut self,
        branch: &str,
        base_sha: &str,
    ) -> Result<(), ProviderError> {
        let Some(compare) = self.compare_with(base_sha, branch).await? else {
            return Err(ProviderError::from(GitHubError::BranchNotFound(
                branch.to_string(),
            )));
        };
        if compare.ahead_by > 0 || compare.behind_by == 0 {
            return Ok(());
        }
        let url = format!(
            "{}/git/refs/heads/{}",
            self.repo_url(),
            urlencoding::encode(branch)
        );
        self.client
            .patch_json(&url, &serde_json::json!({"sha": base_sha, "force": false}))
            .await
            .map(drop)
            .map_err(ProviderError::from)
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

        log::info!(
            "GitHub: creating PR '{}' ({} -> {})",
            title,
            working_branch,
            self.active_branch()
        );

        let payload = serde_json::json!({
            "title": title,
            "head": format!("{}:{}", self.owner, working_branch),
            "base": self.active_branch(),
            "body": body.unwrap_or_default(),
            "draft": draft,
            "maintainer_can_modify": true,
        });

        let pr = self
            .client
            .post_json::<GitHubPullRequest>(&format!("{}/pulls", self.repo_url()), &payload)
            .await
            .map_err(ProviderError::from)?;

        log::info!("GitHub: PR #{} created: {}", pr.number, pr.html_url);
        Ok(pr)
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
            log::info!("GitHub: found existing PR #{}: {}", pr.number, pr.html_url);
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
            // "/" means root (empty internal path), "." or "" means current dir
            if is_absolute {
                return String::new();
            }
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
            &mut self.client,
            &self.owner,
            &self.repo,
            cname,
            https_enforced,
            source_branch,
            source_path,
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
        pages::create_pages_site(
            &mut self.client,
            &self.owner,
            &self.repo,
            branch,
            path,
            build_type,
        )
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

    fn target_poison(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        Some(self.poison.clone())
    }

    /// The account, the repository and the branch the listing reads
    /// (`content_branch`: the configured branch, the repository's default
    /// one resolved at connect, or the workflow branch). Another branch of
    /// the same repository is another tree, so a sync index of one never
    /// serves the other as a baseline.
    fn sync_namespace_identity(&self) -> Option<String> {
        let branch = self.content_branch();
        // Before connect the default branch is unknown: no identity.
        (self.connected && !branch.is_empty()).then(|| {
            serde_json::json!(["github", self.account_email, self.repo_slug(), branch]).to_string()
        })
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // 1. Validate token: try GET /user first (PAT/Device Flow),
        //    fallback to GET /app for installation tokens
        let user_login: String;
        match self
            .client
            .get_json::<GitHubUser>(&format!("{}/user", self.client.api_base()))
            .await
        {
            Ok(user) => {
                user_login = user.login.clone();
                self.user_login = Some(user.login.clone());
                self.account_name = user.name.clone();
                self.account_email = user.email.clone().or(Some(user.login.clone()));
            }
            Err(e) => {
                // Distinguish: 401 Unauthorized → likely installation token
                // Other errors (network, rate limit) → propagate
                let is_auth_error = matches!(
                    &e,
                    GitHubError::Unauthorized | GitHubError::InsufficientPermissions(_)
                );
                if !is_auth_error {
                    // Network error, rate limit, etc.: not a token type issue
                    return Err(ProviderError::from(e));
                }
                // Installation token: GET /user returns 401, but repo access works
                log::info!("GitHub: user endpoint returned 401, treating as installation token");
                user_login = "aeroftp[bot]".to_string();
                self.user_login = Some(user_login.clone());
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
        // Installation tokens report push:false even with contents:write: override for bot tokens
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
            match self.client.get_json::<GitHubBranch>(&branch_url).await {
                Ok(branch_info) => {
                    if branch_info.protected {
                        // Branch is protected, but user/app might have bypass.
                        // Use DirectWriteProtected: attempts direct push first,
                        // falls back to BranchWorkflow if the write is rejected.
                        log::info!(
                            "GitHub: branch '{}' is protected: will attempt direct write (bypass), fallback to working branch if rejected",
                            self.active_branch()
                        );
                        self.write_mode = GitHubWriteMode::DirectWriteProtected {};
                        self.start_on_existing_working_branch().await?;
                    } else {
                        self.write_mode = GitHubWriteMode::DirectWrite;
                    }
                }
                Err(GitHubError::PathNotFound(_) | GitHubError::RepoNotFound) => {
                    // Branch doesn't exist yet: we'll create it on first write.
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
                GitHubVirtualPath::ReleasesRoot => {
                    list_releases(&mut self.client, &self.owner, &self.repo).await
                }
                GitHubVirtualPath::ReleaseTag(tag) => {
                    list_release_assets(&mut self.client, &self.owner, &self.repo, &tag).await
                }
                GitHubVirtualPath::ReleaseAsset { tag, asset_name } => {
                    let release =
                        get_release_info(&mut self.client, &self.owner, &self.repo, &tag).await?;
                    let asset = release
                        .assets
                        .iter()
                        .find(|asset| asset.name == asset_name)
                        .ok_or_else(|| {
                            ProviderError::NotFound(format!(
                                "Asset '{}' not found in release '{}'",
                                asset_name, tag
                            ))
                        })?;
                    Ok(vec![asset_to_entry(asset, &tag)])
                }
            };
        }

        let url = self.contents_query_url(&resolved);

        let items: Vec<GitHubContent> = self
            .client
            .get_json_at(&url, &resolved)
            .await
            .map_err(ProviderError::from)?;

        if items.len() >= 1000 {
            // Not a ParseError, which a search skips as that folder's own: the
            // matches left out would read as a complete result.
            return Err(ProviderError::Other(
                "GitHub Contents listing reached the 1000-entry cap; completeness cannot be verified".into()
            ));
        }
        let mut entries = Vec::with_capacity(items.len());
        for item in &items {
            let is_dir = item.content_type == "dir";
            // Absolute: a relative one would be read from the current folder.
            let entry_path = if resolved.is_empty() {
                format!("/{}", item.name)
            } else {
                format!("/{}/{}", resolved, item.name)
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

        if resolved.is_empty()
            && !entries
                .iter()
                .any(|entry| entry.name == VIRTUAL_RELEASES_DIR)
        {
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
                .get_json_at(&url, &resolved)
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

        if let Some(GitHubVirtualPath::ReleaseAsset { tag, asset_name }) =
            self.parse_virtual_path(&resolved)
        {
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
            .get_json_at(&url, &resolved)
            .await
            .map_err(ProviderError::from)?;

        let download_url = content.download_url.ok_or_else(|| {
            ProviderError::TransferFailed(format!(
                "No download URL for '{}' (might be a directory or submodule)",
                resolved,
            ))
        })?;

        // QA-GH-002: Stream to disk instead of buffering entire file in memory
        let total_size = content.size.unwrap_or(0);
        let resp = self
            .client
            .get_raw(&download_url)
            .await
            .map_err(ProviderError::from)?;

        let content_len = resp.content_length().unwrap_or(total_size);
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        let mut atomic = crate::providers::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut downloaded: u64 = 0;

        use futures_util::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_http_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {}", e))
            })?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, content_len);
            }
        }

        atomic.commit().await.map_err(ProviderError::IoError)?;
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
            .get_json_at(&url, &resolved)
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
            ProviderError::TransferFailed(format!("No download URL for '{}'", resolved,))
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
        if let Some(GitHubVirtualPath::ReleaseAsset { tag, asset_name }) =
            self.parse_virtual_path(&resolved)
        {
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
        // A release (an asset, a tag) is not the session's tree: it was
        // dispatched above, and only a tree write is refused after a
        // fallback (Q2), as the client's own check allows it.
        self.refuse_stale_write().await?;

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
        drop(data); // QA-GH-003: free raw buffer immediately: only base64 remains in memory

        // Check if the file already exists (need its SHA for update).
        let sha = self.get_cached_sha(&resolved).cloned();
        let sha = if sha.is_some() {
            sha
        } else {
            // Try to fetch the file to get its SHA.
            let url = self.contents_query_url(&resolved);
            match self
                .client
                .get_json_at::<GitHubContent>(&url, &resolved)
                .await
            {
                Ok(existing) => {
                    self.cache_sha(&resolved, &existing.sha);
                    Some(existing.sha)
                }
                Err(GitHubError::PathNotFound(_) | GitHubError::RepoNotFound) => None,
                Err(e) => return Err(ProviderError::from(e)),
            }
        };

        let file_name = resolved.rsplit('/').next().unwrap_or(&resolved);
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
        let mut body = serde_json::to_value(&update)
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let result = self.client.put_json(&url, &body).await;

        // QA-GH-009: Structured error matching instead of string parsing
        let resp = match result {
            Err(ref e)
                if matches!(
                    self.write_mode,
                    GitHubWriteMode::DirectWriteProtected { .. }
                ) =>
            {
                let is_protected_rejection = matches!(
                    e,
                    GitHubError::ProtectedBranch(_)
                        | GitHubError::InsufficientPermissions(_)
                        | GitHubError::RequiredPullRequest
                        | GitHubError::ApiError {
                            status: 403 | 422,
                            ..
                        }
                );
                if is_protected_rejection {
                    log::info!("GitHub: direct push rejected on protected branch: falling back to working branch");
                    // One fresh tip of the base, read now, for the
                    // classification and the preparation alike (S3b): the
                    // base can have moved since connect, and a branch
                    // compared with one tip and prepared from another would
                    // take the write onto an older tree.
                    let base_sha = self
                        .client
                        .get_json::<GitHubBranch>(&self.branch_api_url(self.active_branch()))
                        .await
                        .map_err(ProviderError::from)?
                        .commit
                        .sha;
                    // The branch the walk names: a pending one as it is, an
                    // empty one moved up to the base, a missing one created
                    // from it; never a merged one's stale tip (Q1, Q4).
                    let workflow_branch = self.allocate_working_branch(&base_sha).await?;
                    self.ensure_branch_exists_from_sha(&workflow_branch, &base_sha)
                        .await
                        .map_err(|e2| {
                            crate::providers::keep_timeout(e2, |e2| {
                                ProviderError::TransferFailed(format!(
                                    "Cannot create working branch '{}': {}",
                                    workflow_branch, e2
                                ))
                            })
                        })?;
                    // QA-GH-019: Mutate the existing body in-place: no clone of the
                    // large base64 content. `body` is already a serde_json::Value on the stack.
                    // The upload goes to the working branch by name; the
                    // session is still on the protected one while it waits.
                    if let Some(obj) = body.as_object_mut() {
                        obj.insert("branch".to_string(), serde_json::json!(workflow_branch));
                    }
                    // Until the answer says the write was refused, it may
                    // have landed (S2 d): a lost or unreadable answer, a
                    // 5xx, or this future dropped while it waits leaves the
                    // session poisoned, as a landed write does.
                    let pending = PoisonUnlessRefused(Some(self.poison.clone()));
                    let written = match self.client.put_json(&url, &body).await {
                        Ok(written) => written,
                        Err(e) => {
                            if !matches!(
                                e,
                                GitHubError::NetworkError(_)
                                    | GitHubError::ServerError(_)
                                    | GitHubError::ParseError(_)
                            ) {
                                pending.refused();
                            }
                            return Err(ProviderError::from(e));
                        }
                    };
                    // Written: the session is on another branch from here,
                    // another tree than the one any plan of it was made
                    // against, so the switch poisons it in the same step (no
                    // await between): every later write of this session is
                    // refused, and the next connection starts on the working
                    // branch. Only an upload refused by its answer leaves the
                    // session wholly on the protected branch.
                    self.write_mode = GitHubWriteMode::BranchWorkflow {
                        branch: workflow_branch,
                    };
                    self.poison.store(true, std::sync::atomic::Ordering::SeqCst);
                    drop(pending);
                    written
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
        self.refuse_stale_write().await?;
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

        // GitHub has no directory concept: create a `.gitkeep` placeholder.
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
        // A release (an asset, a tag) is not the session's tree: it was
        // dispatched above, and only a tree write is refused after a
        // fallback (Q2), as the client's own check allows it.
        self.refuse_stale_write().await?;

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
                .get_json_at(&url, &resolved)
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
        let body =
            serde_json::to_value(&del).map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        self.client
            .delete_json(&url, &body)
            .await
            .map_err(ProviderError::from)?;

        // Remove from cache.
        self.sha_cache
            .remove(&(self.content_branch().to_string(), resolved));

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        // The virtual paths are dispatched first: an asset is a file, and a
        // release is judged by its own assets before the release goes.
        let resolved = self.resolve_path(path);
        if let Some(virtual_path) = self.parse_virtual_path(&resolved) {
            return match virtual_path {
                GitHubVirtualPath::ReleasesRoot => Err(ProviderError::NotSupported(
                    "Deleting all releases in one operation is not supported".to_string(),
                )),
                GitHubVirtualPath::ReleaseTag(tag) => {
                    self.refuse_non_empty_dir(path).await?;
                    delete_release(&mut self.client, &self.owner, &self.repo, &tag).await
                }
                GitHubVirtualPath::ReleaseAsset { .. } => self.delete(path).await,
            };
        }
        // A release (an asset, a tag) is not the session's tree: it was
        // dispatched above, and only a tree write is refused after a
        // fallback (Q2), as the client's own check allows it.
        self.refuse_stale_write().await?;
        self.refuse_non_empty_dir(path).await?;

        // GitHub has no empty-dir concept; delete all contents recursively.
        self.rmdir_recursive(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        match self.parse_virtual_path(&resolved) {
            Some(GitHubVirtualPath::ReleasesRoot) => {
                return Err(ProviderError::NotSupported(
                    "Recursive deletion of all releases is not supported".to_string(),
                ));
            }
            // Deleting a release takes its assets with it: the release goes
            // whole, rather than being emptied and left behind.
            Some(GitHubVirtualPath::ReleaseTag(tag)) => {
                return delete_release(&mut self.client, &self.owner, &self.repo, &tag).await;
            }
            // An asset lists as itself: it is deleted, not walked.
            Some(GitHubVirtualPath::ReleaseAsset { .. }) => return self.delete(path).await,
            None => {}
        }
        // A release (an asset, a tag) is not the session's tree: it was
        // dispatched above, and only a tree write is refused after a
        // fallback (Q2), as the client's own check allows it.
        self.refuse_stale_write().await?;
        // Each child's path is built from a name the API gave: every one
        // must be inside the folder before any of them is deleted.
        let entries = crate::sync_core::listing_to_remove(&resolved, self.list(path).await?)?;
        for entry in entries {
            if entry.is_walkable_dir() {
                // Box the recursive future to avoid infinite type.
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.refuse_stale_write().await?;
        if self.parse_virtual_path(&self.resolve_path(from)).is_some()
            || self.parse_virtual_path(&self.resolve_path(to)).is_some()
        {
            return Err(ProviderError::NotSupported(
                "Renaming GitHub releases or release assets is not supported".to_string(),
            ));
        }

        let resolved_to = self.resolve_path(to);
        if self.resolve_path(from) == resolved_to {
            return Ok(());
        }
        // The Contents API has no rename: download + upload + delete. The
        // upload refuses an existing path (422, AlreadyExists), but after the
        // download: look first, so a taken destination costs no transfer.
        super::refuse_occupied_destination(self, from, to).await?;
        let data = self.download_to_bytes(from).await?;

        // Upload to new location.
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
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

    /// No: the Contents API creates a file only where there is none, so a
    /// rename has no one-step replace, and the callers that need one refuse
    /// before they write anything (a staged temporary would be a commit).
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
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
                    let release =
                        get_release_info(&mut self.client, &self.owner, &self.repo, &tag).await?;
                    Ok(release_to_entry(&release))
                }
                GitHubVirtualPath::ReleaseAsset { tag, asset_name } => {
                    let release =
                        get_release_info(&mut self.client, &self.owner, &self.repo, &tag).await?;
                    let asset = release
                        .assets
                        .iter()
                        .find(|asset| asset.name == asset_name)
                        .ok_or_else(|| {
                            ProviderError::NotFound(format!(
                                "Asset '{}' not found in release '{}'",
                                asset_name, tag
                            ))
                        })?;
                    Ok(asset_to_entry(asset, &tag))
                }
            };
        }

        let url = self.contents_query_url(&resolved);

        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Contents {
            File(Box<GitHubContent>),
            Directory(Vec<GitHubContent>),
        }
        let response: Contents = self
            .client
            .get_json_at(&url, &resolved)
            .await
            .map_err(ProviderError::from)?;
        let content = match response {
            Contents::File(content) => content,
            Contents::Directory(children) => {
                // The successful directory response proves the directory itself,
                // even when its children exceed the Contents listing cap.
                drop(children);
                let name = resolved
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("/");
                return Ok(RemoteEntry::directory(
                    name.to_string(),
                    format!("/{resolved}"),
                ));
            }
        };

        self.cache_sha(&content.path, &content.sha);

        let mut entry = RemoteEntry {
            name: content.name.clone(),
            path: format!("/{}", content.path),
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
        };

        // Fetch last-commit date for this single entry
        self.enrich_entries_with_dates(std::slice::from_mut(&mut entry))
            .await;

        Ok(entry)
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
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let _ = &options;
        let resolved = self.resolve_path(path);
        if let Some(GitHubVirtualPath::ReleaseAsset { .. }) = self.parse_virtual_path(&resolved) {
            let entry = self.stat(path).await?;
            if let Some(download_url) = entry.metadata.get("browser_download_url") {
                return Ok(ShareLinkResult {
                    url: download_url.clone(),
                    password: None,
                    expires_at: None,
                });
            }
        }
        // Return the html_url for the file on GitHub.
        Ok(ShareLinkResult {
            url: format!(
                "https://github.com/{}/{}/blob/{}/{}",
                self.owner,
                self.repo,
                self.content_branch(),
                resolved,
            ),
            password: None,
            expires_at: None,
        })
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
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

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Use Git Trees API with recursive flag: lists all files in repo
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            self.owner,
            self.repo,
            // S3a: the tree the writers write, not the base.
            urlencoding::encode(self.content_branch())
        );

        #[derive(serde::Deserialize)]
        struct TreeResponse {
            tree: Vec<TreeItem>,
            /// The tree was too large for one answer: the search walks instead.
            #[serde(default)]
            truncated: bool,
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

        if response.truncated {
            return crate::providers::find_by_walk(self, path, pattern).await;
        }

        // The tree covers the whole repository: keep what is under the folder.
        let under = match self.resolve_path(path) {
            folder if folder.is_empty() => String::new(),
            folder => format!("{folder}/"),
        };
        let entries = response
            .tree
            .into_iter()
            .filter(|item| item.path.starts_with(&under))
            .filter(|item| {
                // Match filename or path against pattern (glob or substring)
                let name = item.path.rsplit('/').next().unwrap_or(&item.path);
                crate::providers::matches_find_pattern(name, pattern)
                    || crate::providers::matches_find_pattern(&item.path, pattern)
            })
            .take(crate::providers::FIND_RESULT_LIMIT)
            .map(|item| {
                let name = item
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&item.path)
                    .to_string();
                RemoteEntry {
                    name,
                    path: format!("/{}", item.path),
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

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T09): intentionally NotSupported
        // by design on GitHub.
        //
        // The GitHub provider operates in three modes, none of which
        // expose a native chunked upload primitive:
        //
        //   - **Repo / Contents mode**: writes go through
        //     `PUT /repos/{owner}/{repo}/contents/{path}` which takes the
        //     entire file Base64-encoded inside the JSON request body.
        //     There is no documented chunked variant and the endpoint
        //     hard-caps payloads at 100 MiB. The contents API trades
        //     parallelism for atomic commit semantics by design.
        //   - **Release Assets mode**: writes go through
        //     `POST https://uploads.github.com/repos/{owner}/{repo}/releases/{id}/assets`
        //     as a single-shot binary POST against `uploads.github.com`.
        //     The asset upload endpoint requires `Content-Type: application/octet-stream`
        //     and a precomputed `Content-Length`; no resumable session
        //     URL is exposed.
        //   - **GitHub Pages mode**: a virtual path layer over Repo
        //     mode, inheriting the same contents-API constraints.
        //
        // A fake-multipart wrapper would either buffer the whole
        // Base64-encoded blob (defeats streaming, doubles RAM via
        // Base64 overhead) or serialise per-chunk PUTs against the
        // same JSON body (impossible: contents API is non-resumable).
        // We leave `supports_multipart=false` and let the runner pick
        // the legacy single-stream path with provider-side commit
        // semantics intact.
        super::TransferOptimizationHints::default()
    }
}

#[cfg(test)]
mod tests {
    /// A1, point 1, as one group: what `connect` decides for a protected
    /// branch, from the API's comparison of this account's working branch
    /// with it; and the fallback's handling of a working branch left behind.
    mod start_on_existing_working_branch {
        use super::super::*;
        use std::sync::{Arc, Mutex};

        /// A GitHub API that answers the comparison with `compare` (a status
        /// and a body), refuses direct pushes to `main`, accepts the working
        /// branch, and logs every write with its body.
        async fn api(
            compare: (u16, serde_json::Value),
        ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
            let log = Arc::new(Mutex::new(Vec::new()));
            let seen = log.clone();
            let app = axum::Router::new().fallback(axum::routing::any(
                move |method: axum::http::Method, uri: axum::http::Uri, body: String| {
                    let (seen, compare) = (seen.clone(), compare.clone());
                    async move {
                        use axum::response::IntoResponse;
                        let path = uri.path().to_string();
                        let reply = |status: u16, json: serde_json::Value| {
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                axum::Json(json),
                            )
                                .into_response()
                        };
                        if method != axum::http::Method::GET {
                            seen.lock().unwrap().push(format!("{method} {path} {body}"));
                        }
                        if path.contains("/compare/") {
                            return reply(compare.0, compare.1);
                        }
                        // The listing names the branch whenever the
                        // comparison does not say it is missing.
                        if let Some((_, prefix)) = path.split_once("/git/matching-refs/heads/") {
                            let refs = if compare.0 == 404 {
                                serde_json::json!([])
                            } else {
                                serde_json::json!([{"ref": format!("refs/heads/{prefix}")}])
                            };
                            return reply(200, refs);
                        }
                        if method == axum::http::Method::GET && path.ends_with("/pulls") {
                            return reply(200, serde_json::json!([]));
                        }
                        if path.contains("/branches/") {
                            return reply(
                                200,
                                serde_json::json!({
                                    "name": "aeroftp/aeroftp/main", "protected": false,
                                    "commit": {"sha": "fedcba9876543210fedcba9876543210fedcba98"}
                                }),
                            );
                        }
                        if method == axum::http::Method::PUT && body.contains("\"branch\":\"main\"")
                        {
                            return reply(
                                403,
                                serde_json::json!({"message": "Protected branch update failed"}),
                            );
                        }
                        if method == axum::http::Method::PUT || method == axum::http::Method::PATCH
                        {
                            return reply(200, serde_json::json!({"content": {"sha": "abc123"}}));
                        }
                        reply(404, serde_json::json!({"message": "Not Found"}))
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (format!("http://{addr}"), log, server)
        }

        /// One branch as [`api_per_branch`] serves it.
        #[derive(Clone)]
        struct Branch {
            ahead: u64,
            behind: u64,
            tip: &'static str,
            /// `(merged, head sha)` of each pull request from it into `main`.
            pulls: Vec<(bool, &'static str)>,
        }

        /// A GitHub API that answers each branch by name (`branches`; any
        /// other is a 404), `pulls_status` for the pull-request lookup,
        /// refuses direct pushes to `main` and logs every write.
        async fn api_per_branch(
            branches: Vec<(String, Branch)>,
            pulls_status: u16,
        ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
            api_per_branch_with(branches, pulls_status, false).await
        }

        /// [`api_per_branch`], also serving what `connect` reads (the user
        /// `alice`, the repository `o/r` with push, `main` protected) and a
        /// root listing; with `bypass`, a direct write to `main` succeeds.
        async fn api_per_branch_with(
            branches: Vec<(String, Branch)>,
            pulls_status: u16,
            bypass: bool,
        ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
            let log = Arc::new(Mutex::new(Vec::new()));
            let seen = log.clone();
            let branches = Arc::new(branches);
            let app = axum::Router::new().fallback(axum::routing::any(
                move |method: axum::http::Method, uri: axum::http::Uri, body: String| {
                    let (seen, branches) = (seen.clone(), branches.clone());
                    async move {
                        use axum::response::IntoResponse;
                        let path = urlencoding::decode(uri.path()).unwrap().into_owned();
                        let query =
                            urlencoding::decode(uri.query().unwrap_or("")).unwrap().into_owned();
                        let reply = |status: u16, json: serde_json::Value| {
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                axum::Json(json),
                            )
                                .into_response()
                        };
                        if method != axum::http::Method::GET {
                            seen.lock().unwrap().push(format!("{method} {path} {body}"));
                        }
                        let named = |name: &str| {
                            branches
                                .iter()
                                .find(|(branch, _)| branch == name)
                                .map(|(_, b)| b.clone())
                        };
                        let not_found = serde_json::json!({"message": "Not Found"});
                        if method == axum::http::Method::GET {
                            if path.ends_with("/user") {
                                return reply(
                                    200,
                                    serde_json::json!({"login": "alice", "email": null, "name": "Same Name"}),
                                );
                            }
                            if path.ends_with("/repos/o/r") {
                                return reply(
                                    200,
                                    serde_json::json!({
                                        "full_name": "o/r", "private": false, "default_branch": "main",
                                        "size": 1, "permissions": {"push": true, "pull": true, "admin": false}
                                    }),
                                );
                            }
                            if path.ends_with("/branches/main") {
                                return reply(
                                    200,
                                    serde_json::json!({
                                        "name": "main", "protected": true,
                                        "commit": {"sha": "0123456789abcdef0123456789abcdef01234567"}
                                    }),
                                );
                            }
                            // The root listing only: a file's own lookup (an
                            // upload's existence check) stays a 404.
                            if path.ends_with("/contents") || path.ends_with("/contents/") {
                                return reply(200, serde_json::json!([]));
                            }
                        }
                        if let Some((_, prefix)) = path.split_once("/git/matching-refs/heads/") {
                            let refs: Vec<_> = branches
                                .iter()
                                .filter(|(name, _)| name.starts_with(prefix))
                                .map(|(name, _)| serde_json::json!({"ref": format!("refs/heads/{name}")}))
                                .collect();
                            return reply(200, serde_json::json!(refs));
                        }
                        // Against `main` or its head SHA alike (the fallback's
                        // fast-forward check compares with the SHA).
                        if let Some((_, head)) = path
                            .split_once("/compare/")
                            .and_then(|(_, range)| range.split_once("..."))
                        {
                            return match named(head) {
                                Some(b) => reply(
                                    200,
                                    serde_json::json!({"ahead_by": b.ahead, "behind_by": b.behind}),
                                ),
                                None => reply(404, not_found),
                            };
                        }
                        if method == axum::http::Method::GET && path.ends_with("/pulls") {
                            if pulls_status != 200 {
                                return reply(pulls_status, serde_json::json!({"message": "x"}));
                            }
                            let head = query
                                .split('&')
                                .find_map(|kv| kv.strip_prefix("head=o:"))
                                .unwrap_or("");
                            // 100 to a page, as the lookup asks (S2 a).
                            let page: usize = query
                                .split('&')
                                .find_map(|kv| kv.strip_prefix("page="))
                                .and_then(|p| p.parse().ok())
                                .unwrap_or(1);
                            let pulls: Vec<_> = named(head)
                                .map(|b| b.pulls)
                                .unwrap_or_default()
                                .into_iter()
                                .enumerate()
                                .skip((page - 1) * 100)
                                .take(100)
                                .map(|(n, (merged, sha))| {
                                    serde_json::json!({
                                        "number": n + 1, "title": "t",
                                        "state": if merged { "closed" } else { "open" },
                                        "html_url": "u", "created_at": "c", "mergeable": null,
                                        "merged_at": if merged { serde_json::json!("2026-11-01T00:00:00Z") } else { serde_json::Value::Null },
                                        "head": {"ref": head, "sha": sha},
                                        "base": {"ref": "main", "sha": "b"},
                                        "user": {"login": "alice", "email": null, "name": null},
                                    })
                                })
                                .collect();
                            return reply(200, serde_json::json!(pulls));
                        }
                        if let Some((_, name)) = path.split_once("/branches/") {
                            return match named(name) {
                                Some(b) => reply(
                                    200,
                                    serde_json::json!({
                                        "name": name, "protected": false,
                                        "commit": {"sha": b.tip}
                                    }),
                                ),
                                None => reply(404, not_found),
                            };
                        }
                        if !bypass
                            && method == axum::http::Method::PUT
                            && body.contains("\"branch\":\"main\"")
                        {
                            return reply(
                                403,
                                serde_json::json!({"message": "Protected branch update failed"}),
                            );
                        }
                        if method == axum::http::Method::POST && path.ends_with("/git/refs") {
                            return reply(201, serde_json::json!({"ref": "r"}));
                        }
                        if method == axum::http::Method::PUT || method == axum::http::Method::PATCH
                        {
                            return reply(200, serde_json::json!({"content": {"sha": "abc123"}}));
                        }
                        reply(404, not_found)
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (format!("http://{addr}"), log, server)
        }

        const TIP: &str = "1111111111111111111111111111111111111111";

        /// Q1, Q4: a working branch squash-merged and kept, then `main`
        /// moved on (ahead AND behind; its merged pull request's head is the
        /// tip). The session stays on `main`, and a refused write goes to a
        /// fresh branch (`-2`) created from `main`, never onto the stale tip.
        #[tokio::test]
        async fn a_squash_merged_kept_branch_is_not_adopted_and_a_write_takes_the_next_name() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let merged = Branch {
                ahead: 3,
                behind: 2,
                tip: TIP,
                pulls: vec![(true, TIP)],
            };
            let (origin, log, server) = api_per_branch(vec![(own.clone(), merged)], 200).await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            provider.start_on_existing_working_branch().await.unwrap();
            assert_eq!(provider.working_branch(), None);
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.txt");
            std::fs::write(&file, b"a").unwrap();
            provider
                .upload(file.to_str().unwrap(), "/a.txt", None)
                .await
                .unwrap();
            let fresh = format!("{own}-2");
            let writes = log.lock().unwrap().clone();
            assert!(
                writes.iter().any(|w| w.starts_with("POST ")
                    && w.contains("/git/refs")
                    && w.contains(&format!("refs/heads/{fresh}"))
                    && w.contains("0123456789abcdef0123456789abcdef01234567")),
                "the fresh branch is made from main: {writes:?}"
            );
            assert!(
                writes.iter().any(
                    |w| w.starts_with("PUT ") && w.contains(&format!("\"branch\":\"{fresh}\""))
                ),
                "{writes:?}"
            );
            assert!(
                !writes
                    .iter()
                    .any(|w| w.contains(&format!("\"branch\":\"{own}\""))),
                "nothing on the merged branch: {writes:?}"
            );
            server.abort();
        }

        /// Q1, Q4: pending work on a diverged branch is adopted: a commit
        /// after its merged pull request (head is not the tip), no pull
        /// request at all while `main` moved on, an open pull request. And
        /// the walk goes past a merged branch to a pending `-2`.
        #[tokio::test]
        async fn pending_work_on_a_diverged_branch_is_adopted() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            for pulls in [
                vec![(true, "2222222222222222222222222222222222222222")],
                vec![],
                vec![(false, TIP)],
            ] {
                let pending = Branch {
                    ahead: 3,
                    behind: 2,
                    tip: TIP,
                    pulls: pulls.clone(),
                };
                let (origin, _log, server) =
                    api_per_branch(vec![(own.clone(), pending)], 200).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                provider.start_on_existing_working_branch().await.unwrap();
                assert_eq!(provider.working_branch(), Some(own.as_str()), "{pulls:?}");
                server.abort();
            }
            let merged = Branch {
                ahead: 3,
                behind: 2,
                tip: TIP,
                pulls: vec![(true, TIP)],
            };
            let pending = Branch {
                ahead: 1,
                behind: 0,
                tip: TIP,
                pulls: vec![],
            };
            let fresh = format!("{own}-2");
            let (origin, _log, server) =
                api_per_branch(vec![(own.clone(), merged), (fresh.clone(), pending)], 200).await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            provider.start_on_existing_working_branch().await.unwrap();
            assert_eq!(provider.working_branch(), Some(fresh.as_str()));
            server.abort();
        }

        /// B: the merged original was deleted on GitHub and `-2` holds
        /// pending work: connect adopts `-2`, and a refused write of a
        /// session that did not adopt it goes there too, never to a fresh
        /// original. Deleted original, merged `-2`, pending `-3`: `-3`.
        #[tokio::test]
        async fn pending_work_behind_a_deleted_or_merged_candidate_is_found() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let pending = Branch {
                ahead: 1,
                behind: 0,
                tip: TIP,
                pulls: vec![],
            };
            let merged = Branch {
                ahead: 3,
                behind: 2,
                tip: TIP,
                pulls: vec![(true, TIP)],
            };
            let second = format!("{own}-2");
            let third = format!("{own}-3");
            for (branches, expected) in [
                (vec![(second.clone(), pending.clone())], second.clone()),
                (
                    vec![
                        (second.clone(), merged.clone()),
                        (third.clone(), pending.clone()),
                    ],
                    third.clone(),
                ),
            ] {
                let (origin, _log, server) = api_per_branch(branches.clone(), 200).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                provider.start_on_existing_working_branch().await.unwrap();
                assert_eq!(provider.working_branch(), Some(expected.as_str()));
                server.abort();

                let (origin, log, server) = api_per_branch(branches, 200).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                let dir = tempfile::tempdir().unwrap();
                let file = dir.path().join("a.txt");
                std::fs::write(&file, b"a").unwrap();
                provider
                    .upload(file.to_str().unwrap(), "/a.txt", None)
                    .await
                    .unwrap();
                let writes = log.lock().unwrap().clone();
                assert!(
                    writes.iter().any(|w| w.starts_with("PUT ")
                        && w.contains(&format!("\"branch\":\"{expected}\""))),
                    "{writes:?}"
                );
                assert!(
                    !writes
                        .iter()
                        .any(|w| w.starts_with("POST ") && w.contains("/git/refs")),
                    "no branch is created: {writes:?}"
                );
                server.abort();
            }
        }

        /// C1, D3: nine merged candidates. The real `connect` succeeds on the
        /// protected branch, which reads through the provider; a refused
        /// write that needs a new working branch is refused with the clear
        /// error, nothing written; a user who may write the protected
        /// branch (bypass) writes there and never needs a name.
        #[tokio::test]
        async fn nine_merged_candidates_fail_only_the_write_that_needs_a_new_branch() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let merged = Branch {
                ahead: 3,
                behind: 2,
                tip: TIP,
                pulls: vec![(true, TIP)],
            };
            let branches: Vec<_> = (1..=WORKING_BRANCH_CANDIDATES)
                .map(|n| {
                    let name = if n == 1 {
                        own.clone()
                    } else {
                        format!("{own}-{n}")
                    };
                    (name, merged.clone())
                })
                .collect();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.txt");
            std::fs::write(&file, b"a").unwrap();
            for bypass in [false, true] {
                let (origin, log, server) =
                    api_per_branch_with(branches.clone(), 200, bypass).await;
                let mut extra = HashMap::new();
                extra.insert("branch".to_string(), "main".to_string());
                let config = ProviderConfig {
                    name: "repo".to_string(),
                    provider_type: ProviderType::GitHub,
                    host: "o/r".to_string(),
                    port: Some(443),
                    username: None,
                    password: Some("token".to_string()),
                    initial_path: Some("/".to_string()),
                    extra,
                };
                let mut provider =
                    GitHubProvider::new(GitHubConfig::from_provider_config(&config).unwrap())
                        .unwrap();
                provider.client.api_origin_override = Some(origin);
                provider.connect().await.expect("connect succeeds");
                assert_eq!(provider.working_branch(), None);
                provider
                    .list("/")
                    .await
                    .expect("the protected branch reads");
                let outcome = provider
                    .upload(file.to_str().unwrap(), "/a.txt", None)
                    .await;
                let writes = log.lock().unwrap().clone();
                assert!(
                    !writes
                        .iter()
                        .any(|w| w.starts_with("POST ") && w.contains("/git/refs")),
                    "no working branch is made: {writes:?}"
                );
                if bypass {
                    outcome.expect("the bypass write lands on main");
                    assert!(
                        writes
                            .iter()
                            .any(|w| w.starts_with("PUT ") && w.contains("\"branch\":\"main\"")),
                        "{writes:?}"
                    );
                } else {
                    assert!(
                        matches!(&outcome, Err(ProviderError::InvalidConfig(m)) if m.contains("were all")),
                        "{outcome:?}"
                    );
                    assert!(
                        !writes
                            .iter()
                            .any(|w| w.starts_with("PUT ") && w.contains(&own)),
                        "nothing on a merged branch: {writes:?}"
                    );
                }
                server.abort();
            }
        }

        /// Q1, Q4: a pull-request lookup that fails fails the connect, never
        /// a guess.
        #[tokio::test]
        async fn a_failed_pull_request_lookup_fails_the_connect() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let diverged = Branch {
                ahead: 3,
                behind: 2,
                tip: TIP,
                pulls: vec![],
            };
            let (origin, _log, server) = api_per_branch(vec![(own, diverged)], 500).await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            assert!(provider.start_on_existing_working_branch().await.is_err());
            assert_eq!(provider.working_branch(), None);
            server.abort();
        }

        fn compared(ahead_by: u64, behind_by: u64) -> (u16, serde_json::Value) {
            (
                200,
                serde_json::json!({"ahead_by": ahead_by, "behind_by": behind_by, "status": "x"}),
            )
        }

        /// The working branch has commits `main` does not: the session starts
        /// on it, so its listing, its target identity and its writes are that
        /// branch's from the first request.
        #[tokio::test]
        async fn a_working_branch_ahead_is_where_the_session_starts() {
            let (origin, _log, server) = api(compared(2, 0)).await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            let identity_before = provider.sync_namespace_identity();
            provider.start_on_existing_working_branch().await.unwrap();
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            assert_eq!(provider.working_branch(), Some(own.as_str()));
            assert_ne!(provider.sync_namespace_identity(), identity_before);
            server.abort();
        }

        /// R2: merged and not deleted, the working branch is behind `main`
        /// and holds an older tree: the session stays on `main`. A 404 (no
        /// working branch) does the same.
        #[tokio::test]
        async fn a_merged_or_missing_working_branch_keeps_the_protected_branch() {
            for answer in [
                compared(0, 3),
                compared(0, 0),
                (404, serde_json::json!({"message": "Not Found"})),
            ] {
                let (origin, _log, server) = api(answer).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                provider.start_on_existing_working_branch().await.unwrap();
                assert_eq!(provider.working_branch(), None);
                server.abort();
            }
        }

        /// Any other answer is not "absent": the connect fails rather than
        /// guess which branch the session is on.
        #[tokio::test]
        async fn another_answer_fails_the_connect() {
            for status in [500, 403, 401] {
                let (origin, _log, server) =
                    api((status, serde_json::json!({"message": "answer"}))).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                assert!(
                    provider.start_on_existing_working_branch().await.is_err(),
                    "{status}"
                );
                assert_eq!(provider.working_branch(), None);
                server.abort();
            }
        }

        /// R2, the fallback: a working branch left behind `main` is moved up
        /// to `main`'s tip with a plain update (no force) before the write,
        /// so the write never lands on the older tree. One ahead keeps its
        /// commits and gets no update.
        #[tokio::test]
        async fn the_fallback_fast_forwards_a_working_branch_left_behind() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.txt");
            std::fs::write(&file, b"a").unwrap();
            for (answer, moved) in [(compared(0, 2), true), (compared(1, 2), false)] {
                let (origin, log, server) = api(answer).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                provider
                    .upload(file.to_str().unwrap(), "/a.txt", None)
                    .await
                    .unwrap();
                let writes = log.lock().unwrap().clone();
                let patch = writes
                    .iter()
                    .position(|w| w.starts_with("PATCH ") && w.contains("/git/refs/heads/"));
                let written = writes
                    .iter()
                    .position(|w| {
                        w.starts_with("PUT ")
                            && w.contains(&GitHubProvider::workflow_branch_name("alice", "main"))
                    })
                    .expect("the write to the working branch");
                if moved {
                    let patch = patch.expect("the working branch is fast-forwarded");
                    assert!(patch < written, "{writes:?}");
                    assert!(writes[patch].contains("\"force\":false"), "{writes:?}");
                    // The base's tip read at the write (S3b), which this API
                    // serves for every branch.
                    assert!(
                        writes[patch].contains("fedcba9876543210fedcba9876543210fedcba98"),
                        "{writes:?}"
                    );
                } else {
                    assert_eq!(patch, None, "{writes:?}");
                }
                server.abort();
            }
        }

        /// S3b: the base moved after connect (T1 then, T2 now). The fallback
        /// reads the tip at the write and uses it for the classification and
        /// the preparation alike: a working branch level with T1 but behind
        /// T2 is fast-forwarded to T2 before the write, a missing one is
        /// created from T2. Never from the tip read at connect.
        #[tokio::test]
        async fn the_fallback_prepares_its_branch_from_the_base_tip_at_the_write() {
            const T1: &str = "0123456789abcdef0123456789abcdef01234567";
            const T2: &str = "2222222222222222222222222222222222222222";
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            for exists in [true, false] {
                let log = Arc::new(Mutex::new(Vec::<String>::new()));
                let seen = log.clone();
                let own_name = own.clone();
                let app = axum::Router::new().fallback(axum::routing::any(
                    move |method: axum::http::Method, uri: axum::http::Uri, body: String| {
                        let (seen, own) = (seen.clone(), own_name.clone());
                        async move {
                            use axum::response::IntoResponse;
                            let path = urlencoding::decode(uri.path()).unwrap().into_owned();
                            let reply = |status: u16, json: serde_json::Value| {
                                (axum::http::StatusCode::from_u16(status).unwrap(), axum::Json(json))
                                    .into_response()
                            };
                            if method != axum::http::Method::GET {
                                seen.lock().unwrap().push(format!("{method} {path} {body}"));
                            }
                            if path.ends_with("/branches/main") {
                                return reply(200, serde_json::json!({
                                    "name": "main", "protected": true, "commit": {"sha": T2}
                                }));
                            }
                            if let Some((_, matching)) = path.split_once("/git/matching-refs/heads/") {
                                let refs = if exists {
                                    serde_json::json!([{"ref": format!("refs/heads/{matching}")}])
                                } else {
                                    serde_json::json!([])
                                };
                                return reply(200, refs);
                            }
                            if path.ends_with(&format!("/branches/{own}")) {
                                return if exists {
                                    reply(200, serde_json::json!({
                                        "name": own, "protected": false, "commit": {"sha": T1}
                                    }))
                                } else {
                                    reply(404, serde_json::json!({"message": "Branch not found"}))
                                };
                            }
                            // Level with T1, one behind T2 (or the name `main`).
                            if let Some((_, range)) = path.split_once("/compare/") {
                                let base = range.split_once("...").map(|(b, _)| b).unwrap_or("");
                                if !exists {
                                    return reply(404, serde_json::json!({"message": "Not Found"}));
                                }
                                let behind = u64::from(base != T1);
                                return reply(200, serde_json::json!({"ahead_by": 0, "behind_by": behind}));
                            }
                            if method == axum::http::Method::PUT && body.contains("\"branch\":\"main\"") {
                                return reply(403, serde_json::json!({"message": "Protected branch update failed"}));
                            }
                            if method == axum::http::Method::POST && path.ends_with("/git/refs") {
                                return reply(201, serde_json::json!({"ref": "r"}));
                            }
                            if method == axum::http::Method::PUT || method == axum::http::Method::PATCH {
                                return reply(200, serde_json::json!({"content": {"sha": "abc123"}}));
                            }
                            reply(404, serde_json::json!({"message": "Not Found"}))
                        }
                    },
                ));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                let mut provider = GitHubProvider::protected_for_test(format!("http://{addr}"));
                let dir = tempfile::tempdir().unwrap();
                let file = dir.path().join("a.txt");
                std::fs::write(&file, b"a").unwrap();
                provider
                    .upload(file.to_str().unwrap(), "/a.txt", None)
                    .await
                    .unwrap();
                let writes = log.lock().unwrap().clone();
                let written = writes
                    .iter()
                    .position(|w| {
                        w.starts_with("PUT ") && w.contains(&format!("\"branch\":\"{own}\""))
                    })
                    .expect("the write to the working branch");
                let prepared = writes
                    .iter()
                    .position(|w| {
                        if exists {
                            w.starts_with("PATCH ") && w.contains(&format!("/git/refs/heads/{own}"))
                        } else {
                            w.starts_with("POST ") && w.contains("/git/refs")
                        }
                    })
                    .unwrap_or_else(|| {
                        panic!("exists={exists}: the branch is prepared: {writes:?}")
                    });
                assert!(prepared < written, "{writes:?}");
                assert!(writes[prepared].contains(T2), "exists={exists}: {writes:?}");
                assert!(
                    !writes.iter().any(|w| w.contains(T1)),
                    "exists={exists}: {writes:?}"
                );
                server.abort();
            }
        }

        /// The S2 API: `main` protected at its tip; the working branch `own`
        /// (when `exists`) at `TIP`, `ahead`/`behind` of `main`, with one
        /// pull request merged at `TIP` when `merged_at_tip`; the PUT to
        /// `own` answered with `put_own` (status, raw body); every write
        /// logged.
        async fn scripted(
            own: String,
            exists: bool,
            (ahead, behind): (u64, u64),
            merged_at_tip: bool,
            put_own: (u16, &'static str),
        ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
            let log = Arc::new(Mutex::new(Vec::new()));
            let seen = log.clone();
            let app = axum::Router::new().fallback(axum::routing::any(
                move |method: axum::http::Method, uri: axum::http::Uri, body: String| {
                    let (seen, own) = (seen.clone(), own.clone());
                    async move {
                        use axum::response::IntoResponse;
                        let path = urlencoding::decode(uri.path()).unwrap().into_owned();
                        let raw = |status: u16, body: String| {
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                [(axum::http::header::CONTENT_TYPE, "application/json")],
                                body,
                            )
                                .into_response()
                        };
                        let reply =
                            |status: u16, json: serde_json::Value| raw(status, json.to_string());
                        if method != axum::http::Method::GET {
                            seen.lock().unwrap().push(format!("{method} {path} {body}"));
                        }
                        let not_found = serde_json::json!({"message": "Not Found"});
                        if path.ends_with("/branches/main") {
                            return reply(
                                200,
                                serde_json::json!({
                                    "name": "main", "protected": true,
                                    "commit": {"sha": "0123456789abcdef0123456789abcdef01234567"}
                                }),
                            );
                        }
                        if path.contains("/git/matching-refs/heads/") {
                            let refs = if exists {
                                serde_json::json!([{"ref": format!("refs/heads/{own}")}])
                            } else {
                                serde_json::json!([])
                            };
                            return reply(200, refs);
                        }
                        if path.ends_with(&format!("/branches/{own}")) {
                            return if exists {
                                reply(
                                    200,
                                    serde_json::json!({
                                        "name": own, "protected": false, "commit": {"sha": TIP}
                                    }),
                                )
                            } else {
                                reply(404, not_found)
                            };
                        }
                        if path.contains("/compare/") {
                            return if exists {
                                reply(
                                    200,
                                    serde_json::json!({"ahead_by": ahead, "behind_by": behind}),
                                )
                            } else {
                                reply(404, not_found)
                            };
                        }
                        if method == axum::http::Method::GET && path.ends_with("/pulls") {
                            let pulls = if merged_at_tip {
                                serde_json::json!([{
                                    "number": 1, "title": "t", "state": "closed",
                                    "html_url": "u", "created_at": "c", "mergeable": null,
                                    "merged_at": "2026-11-01T00:00:00Z",
                                    "head": {"ref": own, "sha": TIP},
                                    "base": {"ref": "main", "sha": "b"},
                                    "user": {"login": "alice", "email": null, "name": null},
                                }])
                            } else {
                                serde_json::json!([])
                            };
                            return reply(200, pulls);
                        }
                        if method == axum::http::Method::PUT && body.contains("\"branch\":\"main\"")
                        {
                            return reply(
                                403,
                                serde_json::json!({"message": "Protected branch update failed"}),
                            );
                        }
                        if method == axum::http::Method::PUT
                            && body.contains(&format!("\"branch\":\"{own}\""))
                        {
                            return raw(put_own.0, put_own.1.to_string());
                        }
                        if method == axum::http::Method::POST && path.ends_with("/git/refs") {
                            return reply(201, serde_json::json!({"ref": "r"}));
                        }
                        if method == axum::http::Method::PATCH {
                            return reply(200, serde_json::json!({"ref": "r"}));
                        }
                        reply(404, not_found)
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (format!("http://{addr}"), log, server)
        }

        const WRITTEN: &str = r#"{"content": {"sha": "abc123"}}"#;

        fn a_file() -> (tempfile::TempDir, String) {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.txt");
            std::fs::write(&file, b"a").unwrap();
            let path = file.to_str().unwrap().to_string();
            (dir, path)
        }

        /// S2 (b): a working branch adopted at connect, then merged (its pull
        /// request merged at its tip) or deleted while the session stays
        /// open: the next write is refused before any request writes, and
        /// the session is poisoned. Still pending, the write goes on.
        #[tokio::test]
        async fn a_working_branch_merged_or_deleted_while_the_session_is_open_refuses_the_next_write(
        ) {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let (_dir, file) = a_file();
            for (case, exists, compare, merged, refused) in [
                ("merged", true, (3, 2), true, true),
                ("deleted", false, (0, 0), false, true),
                ("pending", true, (1, 0), false, false),
            ] {
                let (origin, log, server) =
                    scripted(own.clone(), exists, compare, merged, (200, WRITTEN)).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                provider.write_mode = GitHubWriteMode::BranchWorkflow {
                    branch: own.clone(),
                };
                let answer = provider.upload(&file, "/a.txt", None).await;
                let writes = log.lock().unwrap().clone();
                if refused {
                    assert!(
                        matches!(answer, Err(ProviderError::TargetChanged)),
                        "{case}: {answer:?}"
                    );
                    assert!(writes.is_empty(), "{case}: {writes:?}");
                    assert!(provider.refuse_when_poisoned().is_err(), "{case}: poisoned");
                    // And the next write sends nothing at all.
                    assert!(provider.upload(&file, "/b.txt", None).await.is_err());
                    assert!(log.lock().unwrap().is_empty(), "{case}");
                } else {
                    answer.unwrap();
                    assert!(
                        writes
                            .iter()
                            .any(|w| w.starts_with("PUT ") && w.contains(&own)),
                        "{case}: {writes:?}"
                    );
                }
                server.abort();
            }
        }

        /// S2 (b), the bound: N writes in a row on an unchanged pending
        /// branch cost N PUTs and ONE classification (one comparison), not N.
        /// A merge between two writes farther apart than the window is
        /// refused at the next write; one inside the window is refused at the
        /// latest by the first write after the window. A failed write forces
        /// the next check.
        #[tokio::test]
        async fn the_working_branch_check_costs_one_classification_per_window() {
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let merged = Arc::new(AtomicBool::new(false));
            let fail_put = Arc::new(AtomicBool::new(false));
            let (compares, puts) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let state = (
                merged.clone(),
                fail_put.clone(),
                compares.clone(),
                puts.clone(),
                own.clone(),
            );
            let app = axum::Router::new().fallback(axum::routing::any(
                move |method: axum::http::Method, uri: axum::http::Uri| {
                    let (merged, fail_put, compares, puts, own) = state.clone();
                    async move {
                        use axum::response::IntoResponse;
                        let path = urlencoding::decode(uri.path()).unwrap().into_owned();
                        let reply = |status: u16, json: serde_json::Value| {
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                axum::Json(json),
                            )
                                .into_response()
                        };
                        let merged = merged.load(SeqCst);
                        if path.contains("/compare/") {
                            compares.fetch_add(1, SeqCst);
                            let (ahead, behind) = if merged { (3, 2) } else { (1, 0) };
                            return reply(
                                200,
                                serde_json::json!({"ahead_by": ahead, "behind_by": behind}),
                            );
                        }
                        if path.ends_with(&format!("/branches/{own}")) {
                            return reply(
                                200,
                                serde_json::json!({
                                    "name": own, "protected": false, "commit": {"sha": TIP}
                                }),
                            );
                        }
                        if method == axum::http::Method::GET && path.ends_with("/pulls") {
                            return reply(
                                200,
                                serde_json::json!([{
                                    "number": 1, "title": "t", "state": "closed",
                                    "html_url": "u", "created_at": "c", "mergeable": null,
                                    "merged_at": "2026-11-01T00:00:00Z",
                                    "head": {"ref": own, "sha": TIP},
                                    "base": {"ref": "main", "sha": "b"},
                                    "user": {"login": "alice", "email": null, "name": null},
                                }]),
                            );
                        }
                        if method == axum::http::Method::PUT {
                            puts.fetch_add(1, SeqCst);
                            if fail_put.load(SeqCst) {
                                return reply(409, serde_json::json!({"message": "conflict"}));
                            }
                            return reply(200, serde_json::json!({"content": {"sha": "abc123"}}));
                        }
                        reply(404, serde_json::json!({"message": "Not Found"}))
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut provider = GitHubProvider::protected_for_test(format!("http://{addr}"));
            provider.write_mode = GitHubWriteMode::BranchWorkflow {
                branch: own.clone(),
            };
            let (_dir, file) = a_file();
            // The window ran out: as if the last check were older than it.
            let age = |provider: &mut GitHubProvider| {
                provider.last_branch_check = provider.last_branch_check.map(|at| {
                    at.checked_sub(BRANCH_CHECK_WINDOW + std::time::Duration::from_secs(1))
                        .unwrap()
                });
            };

            const N: usize = 20;
            for n in 0..N {
                provider
                    .upload(&file, &format!("/f{n}.txt"), None)
                    .await
                    .unwrap();
            }
            assert_eq!((puts.load(SeqCst), compares.load(SeqCst)), (N, 1));

            // Unchanged after the window: one more classification, then quiet.
            age(&mut provider);
            provider.upload(&file, "/g.txt", None).await.unwrap();
            provider.upload(&file, "/h.txt", None).await.unwrap();
            assert_eq!((puts.load(SeqCst), compares.load(SeqCst)), (N + 2, 2));

            // A failed write forces the next check, inside the window too.
            fail_put.store(true, SeqCst);
            assert!(provider.upload(&file, "/x.txt", None).await.is_err());
            fail_put.store(false, SeqCst);
            provider.upload(&file, "/y.txt", None).await.unwrap();
            assert_eq!(compares.load(SeqCst), 3);

            // Merged inside the window: that window's writes can land, and
            // the first write after it is refused, with nothing sent.
            merged.store(true, SeqCst);
            provider.upload(&file, "/inside.txt", None).await.unwrap();
            let landed = puts.load(SeqCst);
            age(&mut provider);
            assert!(matches!(
                provider.upload(&file, "/after.txt", None).await,
                Err(ProviderError::TargetChanged)
            ));
            assert_eq!(
                puts.load(SeqCst),
                landed,
                "nothing written after the window"
            );

            // Merged between two writes farther apart than the window: the
            // next write is refused.
            let mut later = GitHubProvider::protected_for_test(format!("http://{addr}"));
            later.write_mode = GitHubWriteMode::BranchWorkflow {
                branch: own.clone(),
            };
            merged.store(false, SeqCst);
            later.upload(&file, "/a.txt", None).await.unwrap();
            merged.store(true, SeqCst);
            age(&mut later);
            let before = puts.load(SeqCst);
            assert!(matches!(
                later.upload(&file, "/b.txt", None).await,
                Err(ProviderError::TargetChanged)
            ));
            assert_eq!(puts.load(SeqCst), before);
            server.abort();
        }

        /// S2 (c): the branch deleted (or replaced, or merged) in the instant
        /// between the check and the PUT. The contents API takes no expected
        /// head, so this cannot be excluded; what holds is that the write
        /// fails alone: no other branch is created or written.
        #[tokio::test]
        async fn a_working_branch_gone_between_the_check_and_the_write_fails_that_write_only() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let (_dir, file) = a_file();
            let (origin, log, server) = scripted(
                own.clone(),
                true,
                (1, 0),
                false,
                (404, r#"{"message": "Branch not found"}"#),
            )
            .await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            provider.write_mode = GitHubWriteMode::BranchWorkflow {
                branch: own.clone(),
            };
            assert!(provider.upload(&file, "/a.txt", None).await.is_err());
            let writes = log.lock().unwrap().clone();
            assert_eq!(writes.len(), 1, "{writes:?}");
            assert!(
                writes[0].starts_with("PUT ") && writes[0].contains(&own),
                "{writes:?}"
            );
            server.abort();
        }

        /// S2 (d): the fallback write landed but its answer is lost
        /// (unreadable): the session is poisoned all the same, and the next
        /// write sends nothing. A write refused by its answer leaves the
        /// session on the protected branch, unpoisoned.
        #[tokio::test]
        async fn a_fallback_write_whose_answer_is_lost_poisons_the_session() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let (_dir, file) = a_file();
            for (case, put_own, poisoned) in [
                ("lost answer", (200, "garbled, not json"), true),
                ("server error", (502, r#"{"message": "Bad gateway"}"#), true),
                ("refused", (422, r#"{"message": "Invalid request"}"#), false),
            ] {
                let (origin, log, server) =
                    scripted(own.clone(), false, (0, 0), false, put_own).await;
                let mut provider = GitHubProvider::protected_for_test(origin);
                assert!(
                    provider.upload(&file, "/a.txt", None).await.is_err(),
                    "{case}"
                );
                assert_eq!(provider.refuse_when_poisoned().is_err(), poisoned, "{case}");
                if poisoned {
                    let before = log.lock().unwrap().len();
                    assert!(
                        provider.upload(&file, "/b.txt", None).await.is_err(),
                        "{case}"
                    );
                    assert_eq!(
                        log.lock().unwrap().len(),
                        before,
                        "{case}: nothing more sent"
                    );
                }
                server.abort();
            }
            // A dropped write (its future gone while it waits) poisons too.
            let poison = Arc::new(std::sync::atomic::AtomicBool::new(false));
            drop(PoisonUnlessRefused(Some(poison.clone())));
            assert!(poison.load(std::sync::atomic::Ordering::SeqCst));
            let poison = Arc::new(std::sync::atomic::AtomicBool::new(false));
            PoisonUnlessRefused(Some(poison.clone())).refused();
            assert!(!poison.load(std::sync::atomic::Ordering::SeqCst));
        }

        /// S2 (a): a merged pull request on the second page of the lookup
        /// is found: 100 unmerged ones first, then the one merged at the tip.
        /// The branch is not adopted, and a refused write takes the next name.
        #[tokio::test]
        async fn a_merged_pull_request_beyond_the_first_page_is_found() {
            let own = GitHubProvider::workflow_branch_name("alice", "main");
            let mut pulls = vec![(false, "2222222222222222222222222222222222222222"); 100];
            pulls.push((true, TIP));
            let merged = Branch {
                ahead: 3,
                behind: 2,
                tip: TIP,
                pulls,
            };
            let (origin, log, server) = api_per_branch(vec![(own.clone(), merged)], 200).await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            provider.start_on_existing_working_branch().await.unwrap();
            assert_eq!(provider.working_branch(), None);
            let (_dir, file) = a_file();
            provider.upload(&file, "/a.txt", None).await.unwrap();
            let writes = log.lock().unwrap().clone();
            assert!(
                writes.iter().any(
                    |w| w.starts_with("PUT ") && w.contains(&format!("\"branch\":\"{own}-2\""))
                ),
                "{writes:?}"
            );
            assert!(
                !writes
                    .iter()
                    .any(|w| w.contains(&format!("\"branch\":\"{own}\""))),
                "{writes:?}"
            );
            server.abort();
        }

        /// S2 (e): logins that sanitise alike (a GitHub App's `x[bot]` and a
        /// user `x-bot`) get different working branches on the same base,
        /// as two bases that sanitise alike do.
        #[test]
        fn logins_and_bases_that_sanitise_alike_get_different_working_branches() {
            let app = GitHubProvider::workflow_branch_name("x[bot]", "main");
            let user = GitHubProvider::workflow_branch_name("x-bot", "main");
            assert!(app.starts_with("aeroftp/x-bot/main-"), "{app}");
            assert!(user.starts_with("aeroftp/x-bot/main-"), "{user}");
            assert_ne!(app, user);
            assert_ne!(
                GitHubProvider::workflow_branch_name("alice", "release/x"),
                GitHubProvider::workflow_branch_name("alice", "release-x")
            );
        }

        /// S2 (b), the class: every tree write goes through the one check.
        #[test]
        fn every_tree_write_checks_the_working_branch_first() {
            for (source, writes) in [
                (include_str!("mod.rs"), 7),
                (include_str!("repo_mode.rs"), 4),
            ] {
                let code = source.split("#[cfg(test)]\nmod tests").next().unwrap();
                assert_eq!(code.matches("self.refuse_when_poisoned()").count(), 0);
                assert_eq!(
                    code.matches("self.refuse_stale_write().await?").count(),
                    writes
                );
            }
        }

        /// R1: a poisoned session refuses what writes the tree (contents,
        /// git data, GraphQL commits) before any request, and lets through
        /// what names its own branch or tag (a pull request, a release, a
        /// Pages build, an Actions run).
        #[tokio::test]
        async fn a_poisoned_session_refuses_tree_writes_only() {
            let (origin, log, server) = api(compared(0, 0)).await;
            let mut provider = GitHubProvider::protected_for_test(origin);
            provider
                .poison
                .store(true, std::sync::atomic::Ordering::SeqCst);
            for url in [
                "/repos/o/r/contents/a.txt",
                "/repos/o/r/git/refs/heads/x",
                "/graphql",
                // P6: the resource after an owner or a repository named
                // like one.
                "/repos/contents/git/contents/a.txt",
                "/repos/git/contents/git/refs/heads/x",
            ] {
                let refused = provider.client.put_json(url, &serde_json::json!({})).await;
                assert!(matches!(refused, Err(GitHubError::TargetChanged)), "{url}");
            }
            assert!(matches!(
                provider
                    .client
                    .graphql_raw(&serde_json::json!({"query": "x"}))
                    .await,
                Err(GitHubError::TargetChanged)
            ));
            assert!(log.lock().unwrap().is_empty());
            let allowed = [
                "/repos/o/r/pulls",
                "/repos/o/r/releases",
                "/repos/o/r/pages/builds",
                // P6: an owner or a repository named `contents`, `git` or
                // `graphql` is not the resource.
                "/repos/acme/git/releases",
                "/repos/contents/r/releases",
                "/repos/o/contents/releases",
                "/repos/git/graphql/pulls",
            ];
            for url in allowed {
                let _ = provider
                    .client
                    .post_json::<serde_json::Value>(url, &serde_json::json!({}))
                    .await;
            }
            let sent = log.lock().unwrap().clone();
            assert_eq!(sent.len(), allowed.len(), "{sent:?}");
            // The provider's own tree writes are refused before any request.
            assert!(matches!(
                provider.delete("/a.txt").await,
                Err(ProviderError::TargetChanged)
            ));
            assert!(matches!(
                provider
                    .batch_upload("m", &[("a.txt".into(), b"a".to_vec())], &[])
                    .await,
                Err(ProviderError::TargetChanged)
            ));
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("a.txt");
            std::fs::write(&file, b"a").unwrap();
            for refused in [
                provider
                    .upload(file.to_str().unwrap(), "/docs/a.txt", None)
                    .await,
                provider.rmdir("/docs").await,
                provider.rmdir_recursive("/docs").await,
            ] {
                assert!(
                    matches!(refused, Err(ProviderError::TargetChanged)),
                    "{refused:?}"
                );
            }
            assert_eq!(log.lock().unwrap().len(), allowed.len());
            // Q2: a release (an asset uploaded or deleted, a tag deleted) is
            // not the tree: it reaches the API, whatever this mock answers.
            let releases = super::VIRTUAL_RELEASES_DIR;
            let release_operations = [
                provider
                    .upload(
                        file.to_str().unwrap(),
                        &format!("/{releases}/v1/a.txt"),
                        None,
                    )
                    .await,
                provider.delete(&format!("/{releases}/v1/a.txt")).await,
                provider.rmdir(&format!("/{releases}/v1")).await,
                provider.rmdir_recursive(&format!("/{releases}/v1")).await,
            ];
            // Each one looked its release up (a GET, which this mock answers
            // 404 and does not log): reached, not refused by the poison.
            for (n, outcome) in release_operations.iter().enumerate() {
                assert!(
                    !matches!(outcome, Err(ProviderError::TargetChanged)),
                    "release operation {n}: {outcome:?}"
                );
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn probe_contract_github_directory_metadata_is_not_a_parse_error() {
        use axum::response::IntoResponse;
        let app = axum::Router::new().fallback(axum::routing::get(|uri: axum::http::Uri| async move {
            if uri.path().ends_with("/missing") {
                (axum::http::StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"message":"Not Found"}))).into_response()
            } else if uri.path().ends_with("/denied") {
                (axum::http::StatusCode::FORBIDDEN, axum::Json(serde_json::json!({"message":"Forbidden"}))).into_response()
            } else if uri.path().ends_with("/file") {
                axum::Json(serde_json::json!({"name":"file","path":"file","type":"file","sha":"s","size":3})).into_response()
            } else { axum::Json(serde_json::json!([])).into_response() }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/"))).unwrap(),
        )
        .unwrap();
        provider.connected = true;
        provider.client.api_origin_override = Some(format!("http://{addr}"));
        assert!(provider.stat("/dir").await.unwrap().is_dir);
        assert!(provider.stat("/").await.unwrap().is_dir);
        assert!(!provider.stat("/file").await.unwrap().is_dir);
        assert!(!provider.exists("/missing").await.unwrap());
        assert!(provider.exists("/denied").await.is_err());
        server.abort();
    }

    /// #1506 row 164: a spent primary quota is read from
    /// `x-ratelimit-remaining: 0`, whatever the message says; a 403 without
    /// it stays a permission refusal.
    #[tokio::test]
    async fn a_spent_quota_is_read_from_the_header_not_the_message() {
        use axum::response::IntoResponse;
        let app =
            axum::Router::new().fallback(axum::routing::get(|uri: axum::http::Uri| async move {
                let message = if uri.path().ends_with("/secondary") {
                    "You have exceeded a secondary rate limit"
                } else {
                    "Kontingent für API-Anfragen erschöpft"
                };
                // GitHub answers a spent primary quota with 403 or 429.
                let status = if uri.path().ends_with("/spent429") {
                    axum::http::StatusCode::TOO_MANY_REQUESTS
                } else {
                    axum::http::StatusCode::FORBIDDEN
                };
                let mut response = (
                    status,
                    axum::Json(serde_json::json!({ "message": message })),
                )
                    .into_response();
                let remaining =
                    if uri.path().ends_with("/spent") || uri.path().ends_with("/spent429") {
                        Some("0")
                    } else if uri.path().ends_with("/secondary") {
                        Some("4999")
                    } else {
                        None
                    };
                if let Some(remaining) = remaining {
                    response.headers_mut().insert(
                        "x-ratelimit-remaining",
                        axum::http::HeaderValue::from_static(remaining),
                    );
                }
                response
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/"))).unwrap(),
        )
        .unwrap();
        provider.client.api_origin_override = Some(format!("http://{addr}"));
        let base = provider.client.api_base();
        let spent = provider
            .client
            .get_json::<serde_json::Value>(&format!("{base}/spent"))
            .await;
        let refused = provider
            .client
            .get_json::<serde_json::Value>(&format!("{base}/refused"))
            .await;
        // A 429 with the quota spent is the primary limit too (CodeRabbit,
        // #1520). `execute` directly: the retry wrapper waits on a 429.
        let spent429_request = provider
            .client
            .request(reqwest::Method::GET, &format!("http://{addr}/spent429"));
        let spent429 = provider.client.execute(spent429_request, None).await;
        assert!(
            matches!(spent429, Err(GitHubError::PrimaryRateLimit { .. })),
            "{:?}",
            spent429.err()
        );
        // Quota left and "rate limit" in the message: the secondary limit,
        // retried after a wait (Greptile, #1520). `execute` directly, since
        // `get_json` would sleep for the retry.
        let secondary_request = provider
            .client
            .request(reqwest::Method::GET, &format!("http://{addr}/secondary"));
        let secondary = provider.client.execute(secondary_request, None).await;
        server.abort();
        assert!(
            matches!(secondary, Err(GitHubError::SecondaryRateLimit { .. })),
            "{:?}",
            secondary.err()
        );
        assert!(
            matches!(spent, Err(GitHubError::PrimaryRateLimit { .. })),
            "{spent:?}"
        );
        assert!(
            matches!(refused, Err(GitHubError::InsufficientPermissions(_))),
            "{refused:?}"
        );
    }

    /// #1506 row 164: a branch that appears between the lookup and the
    /// create (422 from git/refs) is found by a second lookup, not by the
    /// message; a 422 for any other reason still fails.
    #[tokio::test]
    async fn a_branch_created_meanwhile_is_found_again_not_read_from_the_refusal() {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        // The second lookup: the branch is there; it is not (404, the refusal
        // stands); it fails otherwise (401, that failure reaches the caller,
        // CodeRabbit #1520).
        for (second, expect_ok) in [("found", true), ("absent", false), ("unauthorized", false)] {
            let lookups = Arc::new(AtomicUsize::new(0));
            let seen = lookups.clone();
            let app = axum::Router::new().fallback(axum::routing::any(
                move |method: axum::http::Method, uri: axum::http::Uri| {
                    let seen = seen.clone();
                    async move {
                        if method == axum::http::Method::POST && uri.path().ends_with("/git/refs") {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                axum::Json(
                                    serde_json::json!({"message": "Riferimento già esistente"}),
                                ),
                            )
                                .into_response();
                        }
                        if uri.path().contains("/branches/") {
                            let n = seen.fetch_add(1, Ordering::SeqCst);
                            if n > 0 && second == "found" {
                                return axum::Json(serde_json::json!({
                                    "name": "aeroftp/u/main", "protected": false,
                                    "commit": {"sha": "0123456789abcdef0123456789abcdef01234567"}
                                }))
                                .into_response();
                            }
                            if n > 0 && second == "unauthorized" {
                                return (
                                    axum::http::StatusCode::UNAUTHORIZED,
                                    axum::Json(serde_json::json!({"message": "Bad credentials"})),
                                )
                                    .into_response();
                            }
                        }
                        (
                            axum::http::StatusCode::NOT_FOUND,
                            axum::Json(serde_json::json!({"message": "Not Found"})),
                        )
                            .into_response()
                    }
                },
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut provider = GitHubProvider::new(
                GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/")))
                    .unwrap(),
            )
            .unwrap();
            provider.connected = true;
            provider.client.api_origin_override = Some(format!("http://{addr}"));
            let outcome = provider
                .ensure_branch_exists_from_sha(
                    "aeroftp/u/main",
                    "0123456789abcdef0123456789abcdef01234567",
                )
                .await;
            server.abort();
            assert_eq!(outcome.is_ok(), expect_ok, "{second}: {outcome:?}");
            if second == "unauthorized" {
                assert!(
                    matches!(outcome, Err(ProviderError::AuthenticationFailed(_))),
                    "the lookup's own failure, not the 422: {outcome:?}"
                );
            }
            assert_eq!(
                lookups.load(Ordering::SeqCst),
                2,
                "one lookup before, one after the 422"
            );
        }
    }

    /// The folder of a search reads as `list` reads it: a relative one from the
    /// current folder and `..` resolved, whether or not the tree came whole.
    #[tokio::test]
    async fn a_search_reads_its_folder_as_a_listing_does() {
        let app = axum::Router::new().fallback(axum::routing::get(|| async {
            let tree: Vec<_> = ["a.txt", "docs/a.txt", "src/a.txt", "src/docs/a.txt"]
                .iter()
                .map(
                    |path| serde_json::json!({"path": path, "type": "blob", "sha": "s", "size": 1}),
                )
                .collect();
            axum::Json(serde_json::json!({"tree": tree, "truncated": false}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/src")))
                .unwrap(),
        )
        .unwrap();
        provider.connected = true;
        provider.client.api_origin_override = Some(format!("http://{addr}"));
        for (folder, expected) in [
            (".", &["/src/a.txt", "/src/docs/a.txt"][..]),
            ("docs", &["/src/docs/a.txt"][..]),
            ("docs/../docs", &["/src/docs/a.txt"][..]),
            ("/docs", &["/docs/a.txt"][..]),
            (
                "/",
                &["/a.txt", "/docs/a.txt", "/src/a.txt", "/src/docs/a.txt"][..],
            ),
        ] {
            let mut found: Vec<String> = provider
                .find(folder, "a.txt")
                .await
                .unwrap()
                .into_iter()
                .map(|entry| entry.path)
                .collect();
            found.sort();
            assert_eq!(found, expected, "{folder}");
        }
        server.abort();
    }

    /// A tree too large for one answer is walked, and the walk follows the
    /// paths the listing gives, from any current folder.
    #[tokio::test]
    async fn a_walked_search_follows_the_listing_from_the_current_folder() {
        let entry = |name: &str, path: &str, kind: &str| serde_json::json!({"name": name, "path": path, "type": kind, "sha": "s", "size": 1});
        let src = serde_json::json!([
            entry("a.txt", "src/a.txt", "file"),
            entry("docs", "src/docs", "dir")
        ]);
        let docs = serde_json::json!([entry("a.txt", "src/docs/a.txt", "file")]);
        let app = axum::Router::new().fallback(axum::routing::get(move |uri: axum::http::Uri| {
            let (src, docs) = (src.clone(), docs.clone());
            async move {
                use axum::response::IntoResponse;
                if uri.path().contains("/git/trees/") {
                    axum::Json(serde_json::json!({"tree": [], "truncated": true})).into_response()
                } else if uri.path().ends_with("/contents/src") {
                    axum::Json(src).into_response()
                } else if uri.path().ends_with("/contents/src/docs") {
                    axum::Json(docs).into_response()
                } else {
                    axum::http::StatusCode::NOT_FOUND.into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/src")))
                .unwrap(),
        )
        .unwrap();
        provider.connected = true;
        provider.client.api_origin_override = Some(format!("http://{addr}"));
        let mut found: Vec<String> = provider
            .find(".", "a.txt")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        found.sort();
        assert_eq!(found, ["/src/a.txt", "/src/docs/a.txt"]);
        server.abort();
    }

    #[tokio::test]
    async fn probe_contract_github_full_contents_page_does_not_prove_absence() {
        let app = axum::Router::new().fallback(axum::routing::get(|| async {
            let items: Vec<_> = (0..1000).map(|i| serde_json::json!({"name":format!("f{i}"),"path":format!("f{i}"),"type":"file","sha":"s","size":1})).collect();
            axum::Json(serde_json::json!(items))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/"))).unwrap(),
        )
        .unwrap();
        provider.connected = true;
        provider.client.api_origin_override = Some(format!("http://{addr}"));
        let outcome = provider.list("/").await;
        server.abort();
        assert!(
            matches!(&outcome, Err(ProviderError::Other(m)) if m.contains("1000-entry cap")),
            "a full capped page is incomplete"
        );
    }
    use super::*;

    fn provider_config(
        host: &str,
        branch: Option<&str>,
        initial_path: Option<&str>,
    ) -> ProviderConfig {
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

    /// The Contents API creates a file only where there is none, so there is no
    /// one-step replace. The answer is no, so the callers that need one (CLI
    /// `edit`, MCP `remote_edit`, the crypt marker paths) refuse before they
    /// write.
    #[tokio::test]
    async fn github_does_not_claim_an_atomic_replace() {
        let mut p = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config("o/r", None, Some("/"))).unwrap(),
        )
        .unwrap();
        assert!(!p.supports_atomic_replace().await.unwrap());
        assert!(!p.replace_sets_aside());
    }

    /// The Contents API has no rename: download, upload, delete. Onto its
    /// own path that read the file and then failed to create it again. It is
    /// a no-op everywhere else, and here no request is made at all (the
    /// provider is not even connected).
    #[tokio::test]
    async fn a_rename_onto_its_own_path_makes_no_request() {
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                None,
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();
        provider
            .rename("/docs/a.md", "/docs/a.md")
            .await
            .expect("a no-op");
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
        let provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                None,
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(provider.current_path, "");
    }

    #[test]
    fn test_resolve_path_honors_absolute_paths() {
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                Some("main"),
                Some("/docs"),
            ))
            .unwrap(),
        )
        .unwrap();
        provider.current_path = "docs/guides".to_string();
        assert_eq!(provider.resolve_path("/README.md"), "README.md");
        assert_eq!(provider.resolve_path("guide.md"), "docs/guides/guide.md");
    }

    /// The tag `workflow_branch_name` appends: the first 16 hex digits of
    /// the SHA-256 of the lowercased login, a NUL and the exact base name.
    fn base_tag(login: &str, base: &str) -> String {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(login.to_ascii_lowercase().as_bytes());
        hasher.update([0]);
        hasher.update(base.as_bytes());
        hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn test_workflow_branch_name_is_stable_and_sanitized() {
        assert_eq!(
            GitHubProvider::workflow_branch_name("AxPNet", "release/2026.03"),
            format!(
                "aeroftp/axpnet/release-2026.03-{}",
                base_tag("AxPNet", "release/2026.03")
            )
        );
        assert_eq!(base_tag("alice", "main").len(), 16);
    }

    /// P7, P8: one working branch per authenticated login and EXACT base.
    /// The login decides (same login in another case: the same account),
    /// never the display name; bases that sanitise alike stay apart; an
    /// installation token has its own.
    #[test]
    fn a_working_branch_is_the_logins_and_the_exact_bases() {
        let name = GitHubProvider::workflow_branch_name;
        assert_eq!(name("Alice", "main"), name("alice", "main"));
        assert_ne!(name("alice", "main"), name("bob", "main"));
        let bases = [
            name("alice", "release/x"),
            name("alice", "release-x"),
            name("alice", "Release-X"),
        ];
        assert_ne!(bases[0], bases[1]);
        assert_ne!(bases[1], bases[2]);
        assert_ne!(bases[0], bases[2]);
        assert!(bases
            .iter()
            .all(|b| b.starts_with("aeroftp/alice/release-x-")));
        assert_eq!(
            name("aeroftp[bot]", "main"),
            format!(
                "aeroftp/aeroftp-bot/main-{}",
                base_tag("aeroftp[bot]", "main")
            )
        );
        // Two accounts with the same display name, or none: their logins.
        let config = provider_config("o/r", Some("main"), Some("/"));
        let mut alice = GitHubProvider::connected_for_test(&config, "alice", "main");
        let mut bob = GitHubProvider::connected_for_test(&config, "bob", "main");
        for display in [Some("Same Name".to_string()), None] {
            alice.account_name = display.clone();
            bob.account_name = display;
            assert_ne!(
                alice.own_working_branch().unwrap(),
                bob.own_working_branch().unwrap()
            );
            assert_eq!(alice.own_working_branch().unwrap(), name("alice", "main"));
        }
        // Before connect there is no login, so no working branch.
        let fresh =
            GitHubProvider::new(GitHubConfig::from_provider_config(&config).unwrap()).unwrap();
        assert!(matches!(
            fresh.own_working_branch(),
            Err(ProviderError::NotConnected)
        ));
    }

    #[test]
    fn test_content_branch_uses_working_branch_when_available() {
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                Some("main"),
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();
        provider.write_mode = GitHubWriteMode::BranchWorkflow {
            branch: "aeroftp/tester/main".to_string(),
        };

        assert_eq!(provider.active_branch(), "main");
        assert_eq!(provider.content_branch(), "aeroftp/tester/main");
    }

    #[test]
    fn test_parse_virtual_release_paths() {
        let provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                Some("main"),
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();

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

    // W4: Write mode detection tests
    #[test]
    fn test_write_mode_direct_allows_content_branch() {
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                Some("main"),
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();
        provider.write_mode = GitHubWriteMode::DirectWrite;
        assert_eq!(provider.content_branch(), "main");
    }

    #[test]
    fn test_write_mode_branch_workflow_uses_working_branch() {
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                Some("main"),
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();
        provider.write_mode = GitHubWriteMode::BranchWorkflow {
            branch: "aeroftp/tester/main".to_string(),
        };
        assert_eq!(provider.content_branch(), "aeroftp/tester/main");
        assert_eq!(provider.active_branch(), "main");
    }

    #[test]
    fn test_write_mode_readonly_preserves_branch() {
        let mut provider = GitHubProvider::new(
            GitHubConfig::from_provider_config(&provider_config(
                "axpnet/aeroftp-test-playground",
                Some("develop"),
                Some("/"),
            ))
            .unwrap(),
        )
        .unwrap();
        provider.write_mode = GitHubWriteMode::ReadOnly {
            reason: "No push access".to_string(),
        };
        assert_eq!(provider.active_branch(), "develop");
        assert_eq!(provider.content_branch(), "develop");
    }

    // W4: Branch sanitization edge cases
    #[test]
    fn test_workflow_branch_sanitizes_special_chars() {
        assert_eq!(
            GitHubProvider::workflow_branch_name("user@email", "feat/new thing!"),
            format!(
                "aeroftp/user-email/feat-new-thing-{}",
                base_tag("user@email", "feat/new thing!")
            )
        );
    }

    #[test]
    fn test_workflow_branch_preserves_consecutive_hyphens() {
        // The sanitizer replaces invalid chars with hyphens but does not collapse
        // consecutive valid chars. Hyphens are valid in branch names.
        assert_eq!(
            GitHubProvider::workflow_branch_name("user", "release---v2"),
            format!(
                "aeroftp/user/release---v2-{}",
                base_tag("user", "release---v2")
            )
        );
    }

    // W4: Path normalization edge cases
    #[test]
    fn test_normalise_path_traversal() {
        assert_eq!(
            GitHubProvider::normalise_path("src/../README.md"),
            "README.md"
        );
        assert_eq!(GitHubProvider::normalise_path("/a/b/../c"), "a/c");
    }

    #[test]
    fn test_normalise_path_double_slash() {
        assert_eq!(
            GitHubProvider::normalise_path("//src//main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn test_normalise_path_above_root() {
        assert_eq!(
            GitHubProvider::normalise_path("../../etc/passwd"),
            "etc/passwd"
        );
    }

    /// S3a: a session on its working branch reads the branch it writes.
    /// `main` and the working branch `work` hold different versions of
    /// `a.txt` (their SHA and body name the branch; `main`'s tree lacks the
    /// file). The repo-mode listing, stat and download, the update's SHA,
    /// and `find` all get the working branch's.
    #[tokio::test]
    async fn a_session_on_its_working_branch_reads_that_branch() {
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = log.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri, body: String| {
                let seen = seen.clone();
                async move {
                    use axum::response::IntoResponse;
                    let path = urlencoding::decode(uri.path()).unwrap().into_owned();
                    let query = urlencoding::decode(uri.query().unwrap_or(""))
                        .unwrap()
                        .into_owned();
                    let json = |v: serde_json::Value| axum::Json(v).into_response();
                    if method != axum::http::Method::GET {
                        seen.lock().unwrap().push(format!("{method} {path} {body}"));
                        return json(serde_json::json!({"content": {"sha": "new"}}));
                    }
                    let file = |b: &str| {
                        serde_json::json!({
                            "name": "a.txt", "path": "a.txt", "type": "file", "size": 12,
                            "sha": format!("sha-{b}"), "content": null, "html_url": null,
                            "download_url": format!("https://raw.githubusercontent.com/o/r/{b}/a.txt"),
                        })
                    };
                    let branch = query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("ref="))
                        .map(str::to_string);
                    match (path.as_str(), branch) {
                        ("/repos/o/r/contents/a.txt", Some(b)) => return json(file(&b)),
                        ("/repos/o/r/contents" | "/repos/o/r/contents/", Some(b)) => {
                            return json(serde_json::json!([file(&b)]))
                        }
                        _ => {}
                    }
                    // The working branch is pending: every write checks it.
                    if path.contains("/compare/") {
                        return json(serde_json::json!({"ahead_by": 1, "behind_by": 0}));
                    }
                    if let Some(b) = path.strip_prefix("/repos/o/r/git/trees/") {
                        let tree = if b == "main" {
                            serde_json::json!([])
                        } else {
                            serde_json::json!([{
                                "path": "a.txt", "mode": "100644", "type": "blob",
                                "size": 12, "sha": format!("sha-{b}"),
                            }])
                        };
                        return json(serde_json::json!({"sha": "t", "truncated": false, "tree": tree}));
                    }
                    if let Some(rest) = path.strip_prefix("/o/r/") {
                        let b = rest.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
                        return format!("content-{b}").into_response();
                    }
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        axum::Json(serde_json::json!({"message": "Not Found"})),
                    )
                        .into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = GitHubProvider::protected_for_test(format!("http://{addr}"));
        provider.write_mode = GitHubWriteMode::BranchWorkflow {
            branch: "work".to_string(),
        };
        let sha = |entry: &RemoteEntry| entry.metadata.get("sha").cloned();

        let listed = provider.list_contents("/").await.unwrap();
        assert_eq!(
            listed.iter().map(sha).collect::<Vec<_>>(),
            [Some("sha-work".to_string())]
        );
        provider.sha_cache.clear();
        let stat = provider.stat_file("/a.txt").await.unwrap();
        assert_eq!(sha(&stat).as_deref(), Some("sha-work"));
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.txt");
        provider
            .download_file("/a.txt", local.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "content-work");
        let found = provider.find("/", "a.txt").await.unwrap();
        assert_eq!(
            found.len(),
            1,
            "find reads the working branch's tree: {found:?}"
        );

        // The update of an existing file names the working branch's SHA.
        provider.sha_cache.clear();
        std::fs::write(&local, b"new body").unwrap();
        provider
            .upload_file(local.to_str().unwrap(), "/a.txt", Some("update"))
            .await
            .unwrap();
        let writes = log.lock().unwrap().clone();
        assert!(
            writes.iter().any(|w| w.starts_with("PUT ")
                && w.contains("\"sha\":\"sha-work\"")
                && w.contains("\"branch\":\"work\"")),
            "{writes:?}"
        );
        assert!(!writes.iter().any(|w| w.contains("sha-main")), "{writes:?}");
        server.abort();
    }

    /// S3a, the class: a content read never names the base branch. In
    /// repository mode nothing reads `self.branch` or `active_branch()`; in
    /// the provider, `active_branch()` is read only where the base itself is
    /// meant (its protection, the pull requests into it, the working
    /// branch's name and classification, the session summary), and
    /// `self.branch` only where the configured branch is defined.
    #[test]
    fn a_content_read_never_names_the_base_branch() {
        /// The functions of `source`, its tests left out, whose code names
        /// `needle`.
        fn functions_naming(source: &str, needle: &str) -> std::collections::BTreeSet<String> {
            let code = source.split("#[cfg(test)]\nmod tests").next().unwrap();
            let mut current = String::new();
            let mut found = std::collections::BTreeSet::new();
            for line in code.lines() {
                let text = line.trim_start();
                if text.starts_with("//") {
                    continue;
                }
                if let Some((_, after)) = text.split_once("fn ") {
                    if text.starts_with("fn ")
                        || text.starts_with("pub")
                        || text.starts_with("async ")
                    {
                        current = after.split(['(', '<']).next().unwrap_or("").to_string();
                    }
                }
                // `needle` as a whole name: `self.branch_api_url` is not
                // `self.branch`.
                let named = line.match_indices(needle).any(|(at, _)| {
                    !line[at + needle.len()..]
                        .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                });
                if named {
                    found.insert(current.clone());
                }
            }
            found
        }
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
        let repo_mode = include_str!("repo_mode.rs");
        assert_eq!(functions_naming(repo_mode, "self.branch"), set(&[]));
        assert_eq!(functions_naming(repo_mode, "active_branch()"), set(&[]));
        let provider = include_str!("mod.rs");
        assert_eq!(
            functions_naming(provider, "self.branch"),
            set(&["active_branch", "connect", "fmt"])
        );
        assert_eq!(
            functions_naming(provider, "active_branch()"),
            set(&ACTIVE_BRANCH_READERS)
        );
    }

    /// Where the provider reads the base branch, and why: see
    /// `a_content_read_never_names_the_base_branch`. A new entry is a
    /// reader of the base; anything that reads content uses
    /// `content_branch()`.
    const ACTIVE_BRANCH_READERS: [&str; 11] = [
        // Its protection and the write mode it gives.
        "connect",
        "encoded_branch_for_path",
        // The branch content is read from when no working branch is active.
        "content_branch",
        // Pull requests into the base, the merged one looked up by name.
        "classify_working_branch",
        "create_pull_request",
        "ensure_pull_request",
        // The working branch's name, and the base it is compared with at
        // connect, before every write (S2 b) and at the fallback (whose tip
        // `upload` reads, S3b).
        "own_working_branch",
        "refuse_stale_write",
        "start_on_existing_working_branch",
        "upload",
        // The session summary names both.
        "server_info",
    ];
}
