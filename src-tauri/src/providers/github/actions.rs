//! GitHub Actions API integration
//!
//! Provides workflow runs listing, status checking, re-run, and cancel
//! for GitHub Actions CI/CD pipelines.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::client::GitHubHttpClient;
use super::errors::GitHubError;
use serde::{Deserialize, Serialize};

// ── Response Models ──────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkflowRunsResponse {
    pub total_count: u64,
    pub workflow_runs: Vec<WorkflowRun>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: Option<String>,
    pub status: Option<String>, // queued, in_progress, completed, waiting
    pub conclusion: Option<String>, // success, failure, cancelled, skipped, timed_out
    pub workflow_id: Option<u64>,
    pub run_number: Option<u64>,
    pub run_attempt: Option<u64>,
    pub event: Option<String>, // push, pull_request, schedule, workflow_dispatch
    pub display_title: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub run_started_at: Option<String>,
    pub html_url: Option<String>,
    pub actor: Option<ActionsUser>,
    pub triggering_actor: Option<ActionsUser>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ActionsUser {
    pub login: Option<String>,
    pub avatar_url: Option<String>,
}

// ── Serializable output (sent to frontend) ───────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct WorkflowRunInfo {
    pub id: u64,
    pub name: String,
    pub branch: String,
    pub sha: String,
    pub status: String,
    pub conclusion: String,
    pub event: String,
    pub run_number: u64,
    pub display_title: String,
    pub created_at: String,
    pub updated_at: String,
    pub duration_seconds: u64,
    pub html_url: String,
    pub actor_login: String,
    pub actor_avatar: String,
}

// ── API Functions ────────────────────────────────────────────────

/// List recent workflow runs (most recent first, max 30).
pub async fn list_workflow_runs(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    branch: Option<&str>,
    per_page: u8,
) -> Result<Vec<WorkflowRunInfo>, GitHubError> {
    let mut url = format!(
        "https://api.github.com/repos/{}/{}/actions/runs?per_page={}",
        owner,
        repo,
        per_page.min(30)
    );
    if let Some(b) = branch {
        url.push_str(&format!("&branch={}", urlencoding::encode(b)));
    }

    let response: WorkflowRunsResponse = client.get_json(&url).await?;

    let runs = response
        .workflow_runs
        .into_iter()
        .map(|r| {
            let created = r.created_at.clone().unwrap_or_default();
            let updated = r.updated_at.clone().unwrap_or_default();
            let duration = compute_duration(&created, &updated);

            WorkflowRunInfo {
                id: r.id,
                name: r.name.unwrap_or_else(|| "Workflow".to_string()),
                branch: r.head_branch.unwrap_or_default(),
                sha: r.head_sha.unwrap_or_default(),
                status: r.status.unwrap_or_else(|| "unknown".to_string()),
                conclusion: r.conclusion.unwrap_or_default(),
                event: r.event.unwrap_or_default(),
                run_number: r.run_number.unwrap_or(0),
                display_title: r.display_title.unwrap_or_default(),
                created_at: created,
                updated_at: updated,
                duration_seconds: duration,
                html_url: r.html_url.unwrap_or_default(),
                actor_login: r
                    .actor
                    .as_ref()
                    .and_then(|a| a.login.clone())
                    .unwrap_or_default(),
                actor_avatar: r
                    .actor
                    .as_ref()
                    .and_then(|a| a.avatar_url.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();

    Ok(runs)
}

/// Re-run a failed or completed workflow run.
pub async fn rerun_workflow(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), GitHubError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/actions/runs/{}/rerun",
        owner, repo, run_id
    );
    client.post_empty(&url).await
}

/// Re-run only failed jobs in a workflow run.
pub async fn rerun_failed_jobs(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), GitHubError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/actions/runs/{}/rerun-failed-jobs",
        owner, repo, run_id
    );
    client.post_empty(&url).await
}

/// Cancel an in-progress workflow run.
pub async fn cancel_workflow_run(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    run_id: u64,
) -> Result<(), GitHubError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/actions/runs/{}/cancel",
        owner, repo, run_id
    );
    client.post_empty(&url).await
}

// ── Helpers ──────────────────────────────────────────────────────

fn compute_duration(created: &str, updated: &str) -> u64 {
    let parse = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.timestamp() as u64)
    };
    match (parse(created), parse(updated)) {
        (Some(c), Some(u)) if u > c => u - c,
        _ => 0,
    }
}
