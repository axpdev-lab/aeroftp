//! GitHub Pages API integration
//!
//! Provides site info, build history, rebuild trigger, and configuration
//! for GitHub Pages deployments.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::client::GitHubHttpClient;
use super::errors::GitHubError;
use serde::{Deserialize, Serialize};

// ── Response Models ──────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesSite {
    pub url: Option<String>,
    pub status: Option<String>,
    pub cname: Option<String>,
    pub html_url: Option<String>,
    pub build_type: Option<String>,
    pub source: Option<PagesSource>,
    pub https_enforced: Option<bool>,
    pub https_certificate: Option<serde_json::Value>,
    pub public: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PagesSource {
    pub branch: String,
    pub path: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesBuild {
    pub url: Option<String>,
    pub status: String,
    pub error: Option<PagesError>,
    pub pusher: Option<PagesUser>,
    pub commit: Option<String>,
    pub duration: Option<u64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesError {
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesUser {
    pub login: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesBuildStatus {
    pub url: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesHealthCheck {
    pub domain: Option<PagesDomain>,
    pub alt_domain: Option<PagesDomain>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PagesDomain {
    pub host: Option<String>,
    pub uri: Option<String>,
    pub nameservers: Option<String>,
    pub dns_resolves: Option<bool>,
    pub is_proxied: Option<bool>,
    pub is_cloudflare_ip: Option<bool>,
    pub is_a_record: Option<bool>,
    pub is_cname_to_github_user_domain: Option<bool>,
    pub is_cname_to_pages_dot_github_dot_com: Option<bool>,
    pub should_be_a_record: Option<bool>,
    pub is_valid: Option<bool>,
    pub responds_to_https: Option<bool>,
    pub enforces_https: Option<bool>,
    pub https_error: Option<String>,
    pub is_https_eligible: Option<bool>,
    pub caa_error: Option<String>,
}

// ── API Functions ────────────────────────────────────────────────

/// Get GitHub Pages site configuration and status.
/// Returns 404 if Pages is not enabled: caller should handle as "not enabled".
pub async fn get_pages_site(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<PagesSite, GitHubError> {
    let url = format!("https://api.github.com/repos/{}/{}/pages", owner, repo);
    client.get_json(&url).await
}

/// List GitHub Pages builds (most recent first).
/// Falls back to /deployments endpoint for repos using GitHub Actions.
pub async fn list_pages_builds(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<Vec<PagesBuild>, GitHubError> {
    // Try legacy pages/builds first
    let url = format!(
        "https://api.github.com/repos/{}/{}/pages/builds?per_page=20",
        owner, repo
    );
    let builds: Vec<PagesBuild> = client.get_json(&url).await.unwrap_or_default();

    if !builds.is_empty() {
        return Ok(builds);
    }

    // Fallback: GitHub Actions deployments for github-pages environment
    let deploy_url = format!(
        "https://api.github.com/repos/{}/{}/deployments?environment=github-pages&per_page=20",
        owner, repo
    );

    #[derive(Deserialize)]
    struct Deployment {
        id: Option<u64>,
        sha: Option<String>,
        created_at: Option<String>,
        updated_at: Option<String>,
        creator: Option<PagesUser>,
    }

    let deployments: Vec<Deployment> = client.get_json(&deploy_url).await.unwrap_or_default();

    // Fetch status for each deployment (first few only to avoid rate limits)
    let mut result = Vec::new();
    for deploy in deployments.into_iter().take(15) {
        let mut status = "built".to_string();

        if let Some(id) = deploy.id {
            let status_url = format!(
                "https://api.github.com/repos/{}/{}/deployments/{}/statuses?per_page=1",
                owner, repo, id
            );

            #[derive(Deserialize)]
            struct DeployStatus {
                state: Option<String>,
            }

            if let Ok(statuses) = client.get_json::<Vec<DeployStatus>>(&status_url).await {
                if let Some(s) = statuses.first() {
                    status = match s.state.as_deref() {
                        Some("success") => "built".to_string(),
                        Some("in_progress") | Some("queued") | Some("pending") => {
                            "building".to_string()
                        }
                        Some("failure") | Some("error") => "errored".to_string(),
                        _ => s.state.clone().unwrap_or_else(|| "built".to_string()),
                    };
                }
            }
        }

        result.push(PagesBuild {
            url: None,
            status,
            error: None,
            pusher: deploy.creator,
            commit: deploy
                .sha
                .as_deref()
                .map(|s| s[..7.min(s.len())].to_string()),
            duration: None,
            created_at: deploy.created_at,
            updated_at: deploy.updated_at,
        });
    }

    Ok(result)
}

/// Get the latest Pages build.
pub async fn get_latest_build(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<PagesBuild, GitHubError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pages/builds/latest",
        owner, repo
    );
    client.get_json(&url).await
}

/// Request a Pages build (only works for legacy build_type, not GitHub Actions).
pub async fn request_build(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<PagesBuildStatus, GitHubError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pages/builds",
        owner, repo
    );
    client.post_json(&url, &serde_json::json!({})).await
}

/// Update Pages site configuration (CNAME, HTTPS, source).
pub async fn update_pages_config(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    cname: Option<&str>,
    https_enforced: Option<bool>,
    source_branch: Option<&str>,
    source_path: Option<&str>,
) -> Result<(), GitHubError> {
    let url = format!("https://api.github.com/repos/{}/{}/pages", owner, repo);

    let mut body = serde_json::Map::new();
    if let Some(cn) = cname {
        body.insert(
            "cname".to_string(),
            serde_json::Value::String(cn.to_string()),
        );
    }
    if let Some(https) = https_enforced {
        body.insert("https_enforced".to_string(), serde_json::Value::Bool(https));
    }
    if source_branch.is_some() || source_path.is_some() {
        let mut source = serde_json::Map::new();
        if let Some(branch) = source_branch {
            source.insert(
                "branch".to_string(),
                serde_json::Value::String(branch.to_string()),
            );
        }
        if let Some(path) = source_path {
            source.insert(
                "path".to_string(),
                serde_json::Value::String(path.to_string()),
            );
        }
        body.insert("source".to_string(), serde_json::Value::Object(source));
    }

    client
        .put_json(&url, &serde_json::Value::Object(body))
        .await?;
    Ok(())
}

/// Check DNS health for custom domain.
pub async fn get_health_check(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<PagesHealthCheck, GitHubError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pages/health",
        owner, repo
    );
    client.get_json(&url).await
}

/// Enable GitHub Pages on a repository.
pub async fn create_pages_site(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    source_branch: &str,
    source_path: &str,
    build_type: &str,
) -> Result<PagesSite, GitHubError> {
    let url = format!("https://api.github.com/repos/{}/{}/pages", owner, repo);
    let body = serde_json::json!({
        "source": {
            "branch": source_branch,
            "path": source_path,
        },
        "build_type": build_type,
    });
    client.post_json(&url, &body).await
}

/// Disable GitHub Pages on a repository.
pub async fn delete_pages_site(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<(), GitHubError> {
    let url = format!("https://api.github.com/repos/{}/{}/pages", owner, repo);
    client.delete(&url).await?;
    Ok(())
}
