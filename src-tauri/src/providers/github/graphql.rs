//! GraphQL operations for the GitHub provider
//!
//! Uses the `createCommitOnBranch` mutation to perform atomic multi-file
//! commits in a single API call.  GitHub GPG-signs these commits automatically
//! on behalf of the authenticated user.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};

use super::client::GitHubHttpClient;
use super::errors::GitHubError;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GraphQLResponse {
    data: Option<CreateCommitData>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct CreateCommitData {
    #[serde(rename = "createCommitOnBranch")]
    create_commit: Option<CreateCommitPayload>,
}

#[derive(Debug, Deserialize)]
struct CreateCommitPayload {
    commit: Option<CommitResult>,
}

#[derive(Debug, Deserialize)]
struct CommitResult {
    oid: String,
    #[allow(dead_code)]
    url: String,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    #[serde(rename = "type")]
    error_type: Option<String>,
    message: String,
    #[allow(dead_code)]
    path: Option<Vec<Value>>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Batch commit multiple file changes in a single atomic commit.
///
/// Uses the GraphQL `createCommitOnBranch` mutation.
///
/// # Returns
///
/// The new commit SHA on success.
/// Parameters for a batch commit operation.
pub struct BatchCommitParams<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
    pub head_oid: &'a str,
    pub message: &'a str,
    pub additions: &'a [(String, Vec<u8>)],
    pub deletions: &'a [String],
}

pub async fn batch_commit(
    client: &mut GitHubHttpClient,
    params: &BatchCommitParams<'_>,
) -> Result<String, GitHubError> {
    let BatchCommitParams {
        owner,
        repo,
        branch,
        head_oid,
        message,
        additions,
        deletions,
    } = params;
    if additions.is_empty() && deletions.is_empty() {
        return Err(GitHubError::InvalidInput(
            "batch_commit requires at least one addition or deletion".into(),
        ));
    }

    let mutation = r#"
        mutation($input: CreateCommitOnBranchInput!) {
            createCommitOnBranch(input: $input) {
                commit {
                    oid
                    url
                }
            }
        }
    "#;

    let additions_json: Vec<Value> = additions
        .iter()
        .map(|(path, content)| {
            json!({
                "path": path,
                "contents": B64.encode(content)
            })
        })
        .collect();

    let deletions_json: Vec<Value> = deletions
        .iter()
        .map(|path| json!({ "path": path }))
        .collect();

    let mut file_changes = json!({});
    if !additions_json.is_empty() {
        file_changes["additions"] = Value::Array(additions_json);
    }
    if !deletions_json.is_empty() {
        file_changes["deletions"] = Value::Array(deletions_json);
    }

    let variables = json!({
        "input": {
            "branch": {
                "repositoryNameWithOwner": format!("{}/{}", owner, repo),
                "branchName": branch
            },
            "expectedHeadOid": head_oid,
            "message": {
                "headline": message
            },
            "fileChanges": file_changes
        }
    });

    let body = json!({
        "query": mutation,
        "variables": variables
    });

    let resp = client.graphql_raw(&body).await?;
    let gql: GraphQLResponse = serde_json::from_value(resp)
        .map_err(|e| GitHubError::ParseError(format!("Failed to parse GraphQL response: {e}")))?;

    if let Some(errors) = gql.errors {
        if let Some(err) = errors.first() {
            return Err(classify_graphql_error(err));
        }
    }

    let oid = gql
        .data
        .and_then(|d| d.create_commit)
        .and_then(|c| c.commit)
        .map(|c| c.oid)
        .ok_or_else(|| GitHubError::ParseError("GraphQL response missing commit OID".into()))?;

    Ok(oid)
}

/// Retrieve the current HEAD SHA of a branch.
pub async fn get_head_sha(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    branch: &str,
) -> Result<String, GitHubError> {
    let path = head_ref_path(owner, repo, branch);
    let resp: Value = client.get_json(&path).await?;

    resp.get("object")
        .and_then(|o| o.get("sha"))
        .and_then(|s| s.as_str())
        .map(String::from)
        .ok_or_else(|| {
            GitHubError::ParseError(format!("Could not resolve HEAD SHA for branch '{branch}'"))
        })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn head_ref_path(owner: &str, repo: &str, branch: &str) -> String {
    format!(
        "/repos/{owner}/{repo}/git/ref/heads/{}",
        urlencoding::encode(branch)
    )
}

fn classify_graphql_error(err: &GraphQLError) -> GitHubError {
    let etype = err.error_type.as_deref().unwrap_or("");
    let msg = &err.message;

    match etype {
        "PROTECTED_BRANCH" => GitHubError::ProtectedBranch(msg.clone()),
        "STALE_DATA" => GitHubError::StaleObject {
            path: msg.clone(),
            expected_sha: String::new(),
        },
        "UNPROCESSABLE" => classify_unprocessable(msg),
        "FORBIDDEN" | "INSUFFICIENT_SCOPES" => GitHubError::PermissionDenied(msg.clone()),
        _ => GitHubError::GraphQLError {
            error_type: etype.to_string(),
            message: msg.clone(),
        },
    }
}

fn classify_unprocessable(message: &str) -> GitHubError {
    let lower = message.to_lowercase();

    if lower.contains("empty commit") || lower.contains("no file changes") {
        GitHubError::InvalidInput("No effective file changes in commit".into())
    } else if lower.contains("branch does not exist") || lower.contains("could not resolve") {
        GitHubError::NotFound(format!("Branch not found: {message}"))
    } else if lower.contains("too large") || lower.contains("exceeds") {
        GitHubError::PayloadTooLarge(message.to_string())
    } else {
        GitHubError::Unprocessable(message.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_protected_branch() {
        let err = GraphQLError {
            error_type: Some("PROTECTED_BRANCH".into()),
            message: "Cannot push to protected branch".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::ProtectedBranch(_)
        ));
    }

    #[test]
    fn test_classify_stale_data() {
        let err = GraphQLError {
            error_type: Some("STALE_DATA".into()),
            message: "Expected HEAD OID does not match".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::StaleObject { .. }
        ));
    }

    #[test]
    fn test_classify_unprocessable_empty() {
        let err = GraphQLError {
            error_type: Some("UNPROCESSABLE".into()),
            message: "Empty commit: no file changes provided".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::InvalidInput(_)
        ));
    }

    #[test]
    fn test_classify_unprocessable_branch_not_found() {
        let err = GraphQLError {
            error_type: Some("UNPROCESSABLE".into()),
            message: "Could not resolve to a node: branch does not exist".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::NotFound(_)
        ));
    }

    #[test]
    fn test_classify_unprocessable_too_large() {
        let err = GraphQLError {
            error_type: Some("UNPROCESSABLE".into()),
            message: "Content exceeds maximum size of 100 MB".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::PayloadTooLarge(_)
        ));
    }

    #[test]
    fn test_classify_forbidden() {
        let err = GraphQLError {
            error_type: Some("FORBIDDEN".into()),
            message: "Resource not accessible by personal access token".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::PermissionDenied(_)
        ));
    }

    #[test]
    fn test_classify_unknown_type() {
        let err = GraphQLError {
            error_type: Some("SOMETHING_NEW".into()),
            message: "Unexpected error".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::GraphQLError { .. }
        ));
    }

    #[test]
    fn test_head_ref_path_encodes_branch_name() {
        assert_eq!(
            head_ref_path("axpnet", "aeroftp", "feature/github-audit"),
            "/repos/axpnet/aeroftp/git/ref/heads/feature%2Fgithub-audit"
        );
    }

    // W4: Additional GraphQL error edge cases
    #[test]
    fn test_classify_insufficient_scopes() {
        let err = GraphQLError {
            error_type: Some("INSUFFICIENT_SCOPES".into()),
            message: "Your token has not been granted the required scopes".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::PermissionDenied(_)
        ));
    }

    #[test]
    fn test_classify_none_type_falls_back_to_generic() {
        let err = GraphQLError {
            error_type: None,
            message: "Something went wrong".into(),
            path: None,
        };
        assert!(matches!(
            classify_graphql_error(&err),
            GitHubError::GraphQLError { .. }
        ));
    }

    #[test]
    fn test_classify_stale_data_with_oid_hint() {
        let err = GraphQLError {
            error_type: Some("STALE_DATA".into()),
            message: "The expected head OID did not match the actual head OID".into(),
            path: None,
        };
        let result = classify_graphql_error(&err);
        assert!(matches!(result, GitHubError::StaleObject { .. }));
    }
}
