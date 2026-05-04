//! Data models for the GitLab REST API v4
//!
//! Only the fields AeroFTP actually uses are deserialized.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

/// Authenticated user (from `GET /user`)
#[derive(Debug, Deserialize)]
pub struct GitLabUser {
    pub username: String,
    pub name: Option<String>,
    pub email: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub state: String,
}

/// Project metadata (from `GET /projects/:id`)
#[derive(Debug, Deserialize)]
pub struct GitLabProject {
    pub id: u64,
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub path_with_namespace: String,
    pub default_branch: Option<String>,
    pub visibility: String,
    /// Repository size in bytes (GitLab reports bytes, unlike GitHub's KB)
    #[serde(default)]
    pub repository_size: Option<u64>,
    #[allow(dead_code)]
    pub namespace: Option<GitLabNamespace>,
}

/// Project namespace
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitLabNamespace {
    pub full_path: String,
}

/// A tree entry (from `GET /projects/:id/repository/tree`)
#[derive(Debug, Deserialize)]
pub struct GitLabTreeEntry {
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    /// `"blob"` or `"tree"`
    #[serde(rename = "type")]
    pub entry_type: String,
    pub path: String,
    #[allow(dead_code)]
    pub mode: String,
}

/// File metadata (from `GET /projects/:id/repository/files/:path`)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitLabFileInfo {
    pub file_name: String,
    pub file_path: String,
    pub size: u64,
    pub encoding: String,
    pub blob_id: String,
    pub commit_id: String,
    pub last_commit_id: String,
    #[serde(default)]
    pub content_sha256: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

/// Commit response (from `POST /projects/:id/repository/commits`)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitLabCommit {
    pub id: String,
    pub short_id: String,
    pub title: String,
    pub message: String,
    pub author_name: String,
    pub created_at: String,
    pub web_url: Option<String>,
}

/// Branch info (from `GET /projects/:id/repository/branches/:name`)
#[derive(Debug, Deserialize)]
pub struct GitLabBranch {
    pub name: String,
    #[serde(rename = "protected")]
    pub is_protected: bool,
    #[serde(rename = "default")]
    pub is_default: bool,
    pub can_push: bool,
}

// ── Releases ───────────────────────────────────────────────────────

/// Release (from `GET /projects/:id/releases`)
#[derive(Debug, Deserialize, Clone)]
pub struct GitLabRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
    pub released_at: Option<String>,
    pub author: GitLabReleaseAuthor,
    pub assets: GitLabReleaseAssets,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GitLabReleaseAuthor {
    pub username: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GitLabReleaseAssets {
    pub count: u32,
    #[serde(default)]
    pub sources: Vec<GitLabReleaseSource>,
    #[serde(default)]
    pub links: Vec<GitLabReleaseLink>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GitLabReleaseSource {
    pub format: String,
    pub url: String,
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct GitLabReleaseLink {
    pub id: u64,
    pub name: String,
    pub url: String,
    pub direct_asset_url: Option<String>,
    pub link_type: String,
    #[serde(default)]
    pub external: bool,
}

// ── Merge Requests ─────────────────────────────────────────────────

/// Merge Request (from `POST /projects/:id/merge_requests`)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitLabMergeRequest {
    pub iid: u64,
    pub title: String,
    pub state: String,
    pub web_url: String,
    pub source_branch: String,
    pub target_branch: String,
}
