//! Shared data types for the GitHub provider
//!
//! API response models, request payloads, and internal types used across
//! the GitHub provider submodules.

use serde::{Deserialize, Serialize};

/// GitHub authenticated user
#[derive(Debug, Deserialize)]
pub struct GitHubUser {
    pub login: String,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// GitHub repository metadata
#[derive(Debug, Deserialize)]
pub struct GitHubRepo {
    pub full_name: String,
    pub private: bool,
    pub default_branch: String,
    /// Repository size in KB (as reported by GitHub)
    pub size: u64,
    pub permissions: Option<GitHubRepoPermissions>,
}

/// Repository-level permissions for the authenticated user
#[derive(Debug, Deserialize)]
pub struct GitHubRepoPermissions {
    pub push: bool,
    pub pull: bool,
    pub admin: bool,
}

/// A file or directory entry from the Contents API
#[derive(Debug, Deserialize)]
pub struct GitHubContent {
    pub name: String,
    pub path: String,
    /// `"file"`, `"dir"`, `"symlink"`, or `"submodule"`
    #[serde(rename = "type")]
    pub content_type: String,
    pub size: Option<u64>,
    pub sha: String,
    /// Base64-encoded content (only present for files < 1 MB via single-file GET)
    pub content: Option<String>,
    pub download_url: Option<String>,
    pub html_url: Option<String>,
}

/// A GitHub release
#[derive(Debug, Deserialize)]
pub struct GitHubRelease {
    pub id: u64,
    pub tag_name: String,
    pub name: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub created_at: String,
    pub published_at: Option<String>,
    pub assets: Vec<GitHubAsset>,
    pub upload_url: String,
    pub body: Option<String>,
}

/// A release asset (binary attachment)
#[derive(Debug, Deserialize)]
pub struct GitHubAsset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub download_count: u64,
    pub browser_download_url: String,
    pub content_type: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A branch reference
#[derive(Debug, Deserialize)]
pub struct GitHubBranch {
    pub name: String,
    pub protected: bool,
    pub commit: GitHubCommitRef,
}

/// Minimal commit reference (SHA only)
#[derive(Debug, Deserialize)]
pub struct GitHubCommitRef {
    pub sha: String,
}

/// A pull request
#[derive(Debug, Deserialize)]
pub struct GitHubPullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub head: GitHubPrRef,
    pub base: GitHubPrRef,
    pub user: GitHubUser,
    pub created_at: String,
    pub mergeable: Option<bool>,
}

/// Head/base reference in a pull request
#[derive(Debug, Deserialize)]
pub struct GitHubPrRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

/// An issue
#[derive(Debug, Deserialize)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub user: GitHubUser,
    pub labels: Vec<GitHubLabel>,
    pub created_at: String,
    pub body: Option<String>,
}

/// A label on an issue or PR
#[derive(Debug, Deserialize)]
pub struct GitHubLabel {
    pub name: String,
    pub color: String,
}

/// A workflow definition
#[derive(Debug, Deserialize)]
pub struct GitHubWorkflow {
    pub id: u64,
    pub name: String,
    pub state: String,
    pub path: String,
}

/// A workflow run
#[derive(Debug, Deserialize)]
pub struct GitHubWorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    /// `queued`, `in_progress`, `completed`
    pub status: String,
    /// `success`, `failure`, `cancelled`, etc.
    pub conclusion: Option<String>,
    pub html_url: String,
    pub created_at: String,
    pub head_branch: Option<String>,
}

/// A CI/CD artifact
#[derive(Debug, Deserialize)]
pub struct GitHubArtifact {
    pub id: u64,
    pub name: String,
    pub size_in_bytes: u64,
    pub archive_download_url: String,
    pub created_at: String,
    pub expired: bool,
}

/// Git committer/author identity for AeroFTP-signed commits
#[derive(Debug, Serialize, Clone)]
pub struct GitHubCommitter {
    pub name: String,
    pub email: String,
}

impl Default for GitHubCommitter {
    fn default() -> Self {
        Self {
            name: "aeroftp[bot]".to_string(),
            email: "3115847+aeroftp[bot]@users.noreply.github.com".to_string(),
        }
    }
}

/// Request body for creating/updating a file via the Contents API
#[derive(Debug, Serialize)]
pub struct GitHubContentUpdate {
    pub message: String,
    /// Base64-encoded file content
    pub content: String,
    /// Required for updates (current SHA); absent for creates
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Committer identity — defaults to AeroFTP
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committer: Option<GitHubCommitter>,
}

/// Request body for deleting a file via the Contents API
#[derive(Debug, Serialize)]
pub struct GitHubContentDelete {
    pub message: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Committer identity — defaults to AeroFTP
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committer: Option<GitHubCommitter>,
}
