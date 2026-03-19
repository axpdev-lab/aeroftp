//! Release asset management for the GitHub provider
//!
//! Maps GitHub Releases to virtual directories and release assets to files,
//! providing a familiar file-manager experience over the Releases API.

use std::collections::HashMap;

use futures_util::StreamExt;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use super::client::GitHubHttpClient;
use super::errors::GitHubError;
use super::model::{GitHubAsset, GitHubRelease};
use crate::providers::{ProviderError, RemoteEntry};

pub(crate) const VIRTUAL_RELEASES_DIR: &str = ".github-releases";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `GitHubRelease` into a `RemoteEntry` (virtual directory).
pub(crate) fn release_to_entry(release: &GitHubRelease) -> RemoteEntry {
    let name = release.tag_name.clone();
    let path = format!("/{}/{}/", VIRTUAL_RELEASES_DIR, name);
    let modified = release
        .published_at
        .clone()
        .or_else(|| Some(release.created_at.clone()));

    let mut metadata = HashMap::new();
    metadata.insert("release_id".into(), release.id.to_string());
    if release.draft {
        metadata.insert("draft".into(), "true".into());
    }
    if release.prerelease {
        metadata.insert("prerelease".into(), "true".into());
    }
    if let Some(ref body) = release.body {
        let truncated: String = body.chars().take(512).collect();
        metadata.insert("body".into(), truncated);
    }

    RemoteEntry {
        name,
        path,
        is_dir: true,
        size: 0,
        modified,
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: None,
        metadata,
    }
}

/// Convert a `GitHubAsset` into a `RemoteEntry` (file) under a given tag.
pub(crate) fn asset_to_entry(asset: &GitHubAsset, tag: &str) -> RemoteEntry {
    let mut metadata = HashMap::new();
    metadata.insert("asset_id".into(), asset.id.to_string());
    metadata.insert("download_count".into(), asset.download_count.to_string());
    metadata.insert("content_type".into(), asset.content_type.clone());
    metadata.insert(
        "browser_download_url".into(),
        asset.browser_download_url.clone(),
    );

    RemoteEntry {
        name: asset.name.clone(),
        path: format!("/{}/{}/{}", VIRTUAL_RELEASES_DIR, tag, asset.name),
        is_dir: false,
        size: asset.size,
        modified: Some(asset.updated_at.clone()),
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: Some(asset.content_type.clone()),
        metadata,
    }
}

/// Strip the `{?name,label}` URI template suffix from `upload_url`.
fn strip_upload_template(upload_url: &str) -> String {
    if let Some(idx) = upload_url.find('{') {
        upload_url[..idx].to_string()
    } else {
        upload_url.to_string()
    }
}

/// Guess a Content-Type from the file extension.
fn guess_content_type(filename: &str) -> &'static str {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "deb" => "application/vnd.debian.binary-package",
        "rpm" => "application/x-rpm",
        "dmg" => "application/x-apple-diskimage",
        "msi" => "application/x-msi",
        "exe" => "application/vnd.microsoft.portable-executable",
        "appimage" => "application/x-executable",
        "snap" => "application/vnd.snap",
        "tar" => "application/x-tar",
        "gz" | "tgz" => "application/gzip",
        "xz" => "application/x-xz",
        "bz2" => "application/x-bzip2",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain",
        "sha256" | "sha512" => "text/plain",
        "sig" | "asc" => "application/pgp-signature",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Release operations
// ---------------------------------------------------------------------------

/// List all releases as virtual directories.
pub async fn list_releases(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
) -> Result<Vec<RemoteEntry>, ProviderError> {
    let path = format!("/repos/{owner}/{repo}/releases?per_page=100");
    let releases: Vec<GitHubRelease> = client
        .get_paginated_json_array(&path)
        .await
        .map_err(|e| ProviderError::ServerError(e.to_string()))?;

    Ok(releases.iter().map(release_to_entry).collect())
}

/// List assets belonging to the release identified by `tag`.
pub async fn list_release_assets(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
) -> Result<Vec<RemoteEntry>, ProviderError> {
    let release = get_release_by_tag(client, owner, repo, tag).await?;
    Ok(release
        .assets
        .iter()
        .map(|a| asset_to_entry(a, tag))
        .collect())
}

/// Download a release asset to a local file path.
pub async fn download_release_asset(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
    asset_name: &str,
    local_path: &str,
) -> Result<(), ProviderError> {
    let release = get_release_by_tag(client, owner, repo, tag).await?;
    let asset = find_asset(&release, asset_name)?;

    let resp = client
        .get_raw(&asset.browser_download_url)
        .await
        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(ProviderError::TransferFailed(format!(
            "Asset download returned HTTP {}",
            resp.status()
        )));
    }

    let mut file = tokio::fs::File::create(local_path)
        .await
        .map_err(ProviderError::IoError)?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        file.write_all(&bytes)
            .await
            .map_err(ProviderError::IoError)?;
    }

    file.flush().await.map_err(ProviderError::IoError)?;
    Ok(())
}

/// Upload a local file as a release asset.
///
/// On HTTP 422 (duplicate name), deletes the existing asset and retries once.
pub async fn upload_release_asset(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
    local_path: &str,
    asset_name: &str,
) -> Result<(), ProviderError> {
    let release = get_release_by_tag(client, owner, repo, tag).await?;
    let upload_base = strip_upload_template(&release.upload_url);
    let content_type = guess_content_type(asset_name);

    let body = tokio::fs::read(local_path)
        .await
        .map_err(ProviderError::IoError)?;

    match do_upload(client, &upload_base, asset_name, content_type, &body).await {
        Ok(()) => Ok(()),
        Err(GitHubError::DuplicateAsset(_)) => {
            // Delete the existing asset, then retry
            if let Some(existing) = release.assets.iter().find(|a| a.name == asset_name) {
                let delete_path =
                    format!("/repos/{owner}/{repo}/releases/assets/{}", existing.id);
                client
                    .delete(&delete_path)
                    .await
                    .map_err(|e| ProviderError::ServerError(e.to_string()))?;
            }
            do_upload(client, &upload_base, asset_name, content_type, &body)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))
        }
        Err(e) => Err(ProviderError::TransferFailed(e.to_string())),
    }
}

/// Parameters for creating a new release.
pub struct CreateReleaseParams<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub tag: &'a str,
    pub name: &'a str,
    pub body: &'a str,
    pub draft: bool,
    pub prerelease: bool,
}

/// Create a new release.
pub async fn create_release(
    client: &mut GitHubHttpClient,
    params: &CreateReleaseParams<'_>,
) -> Result<GitHubRelease, ProviderError> {
    let CreateReleaseParams { owner, repo, tag, name, body, draft, prerelease } = params;
    let path = format!("/repos/{owner}/{repo}/releases");
    let payload = json!({
        "tag_name": tag,
        "name": name,
        "body": body,
        "draft": draft,
        "prerelease": prerelease,
    });

    let release: GitHubRelease = client
        .post_json(&path, &payload)
        .await
        .map_err(|e| ProviderError::ServerError(e.to_string()))?;

    Ok(release)
}

/// Delete a single asset from a release.
pub async fn delete_release_asset(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
    asset_name: &str,
) -> Result<(), ProviderError> {
    let release = get_release_by_tag(client, owner, repo, tag).await?;
    let asset = find_asset(&release, asset_name)?;

    let path = format!("/repos/{owner}/{repo}/releases/assets/{}", asset.id);
    client
        .delete(&path)
        .await
        .map_err(|e| ProviderError::ServerError(e.to_string()))?;

    Ok(())
}

/// Delete an entire release (and implicitly all its assets).
pub async fn delete_release(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
) -> Result<(), ProviderError> {
    let release = get_release_by_tag(client, owner, repo, tag).await?;
    let path = format!("/repos/{owner}/{repo}/releases/{}", release.id);
    client
        .delete(&path)
        .await
        .map_err(|e| ProviderError::ServerError(e.to_string()))?;

    Ok(())
}

/// Get release metadata.
pub async fn get_release_info(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
) -> Result<GitHubRelease, ProviderError> {
    get_release_by_tag(client, owner, repo, tag).await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

async fn get_release_by_tag(
    client: &mut GitHubHttpClient,
    owner: &str,
    repo: &str,
    tag: &str,
) -> Result<GitHubRelease, ProviderError> {
    let path = release_by_tag_path(owner, repo, tag);
    let release: GitHubRelease = client.get_json(&path).await.map_err(|e| {
        if matches!(e, GitHubError::PathNotFound(_) | GitHubError::RepoNotFound) {
            ProviderError::NotFound(format!("Release not found: {tag}"))
        } else {
            ProviderError::ServerError(e.to_string())
        }
    })?;
    Ok(release)
}

fn release_by_tag_path(owner: &str, repo: &str, tag: &str) -> String {
    format!(
        "/repos/{owner}/{repo}/releases/tags/{}",
        urlencoding::encode(tag)
    )
}

fn find_asset<'a>(
    release: &'a GitHubRelease,
    asset_name: &str,
) -> Result<&'a GitHubAsset, ProviderError> {
    release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| {
            ProviderError::NotFound(format!(
                "Asset '{}' not found in release '{}'",
                asset_name, release.tag_name
            ))
        })
}

async fn do_upload(
    client: &mut GitHubHttpClient,
    upload_base: &str,
    asset_name: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), GitHubError> {
    let url = format!(
        "{}?name={}",
        upload_base,
        urlencoding::encode(asset_name)
    );

    let resp = client
        .upload_raw(&url, content_type, body.to_vec())
        .await?;

    let status = resp.status();

    if status.is_success() {
        return Ok(());
    }

    let error_body = resp
        .text()
        .await
        .unwrap_or_else(|_| String::from("<unreadable>"));

    if status.as_u16() == 422 {
        return Err(GitHubError::DuplicateAsset(format!(
            "Asset '{}' already exists: {}",
            asset_name, error_body
        )));
    }

    if status.as_u16() == 502 {
        return Err(GitHubError::ServerError(format!(
            "Upload returned 502 (may be partial): {}",
            error_body
        )));
    }

    Err(GitHubError::ApiError {
        status: status.as_u16(),
        message: error_body,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_upload_template_with_suffix() {
        let url = "https://uploads.github.com/repos/o/r/releases/1/assets{?name,label}";
        assert_eq!(
            strip_upload_template(url),
            "https://uploads.github.com/repos/o/r/releases/1/assets"
        );
    }

    #[test]
    fn test_strip_upload_template_without_suffix() {
        let url = "https://uploads.github.com/repos/o/r/releases/1/assets";
        assert_eq!(strip_upload_template(url), url);
    }

    #[test]
    fn test_guess_content_type_known() {
        assert_eq!(guess_content_type("app.deb"), "application/vnd.debian.binary-package");
        assert_eq!(guess_content_type("installer.msi"), "application/x-msi");
        assert_eq!(guess_content_type("app.tar.gz"), "application/gzip");
        assert_eq!(guess_content_type("archive.zip"), "application/zip");
        assert_eq!(guess_content_type("SHA256SUMS.sha256"), "text/plain");
    }

    #[test]
    fn test_guess_content_type_unknown() {
        assert_eq!(guess_content_type("mystery.xyz"), "application/octet-stream");
        assert_eq!(guess_content_type("noext"), "application/octet-stream");
    }

    #[test]
    fn test_release_to_entry() {
        let release = GitHubRelease {
            id: 42,
            tag_name: "v1.0.0".into(),
            name: Some("Release 1.0".into()),
            draft: false,
            prerelease: true,
            created_at: "2025-01-01T00:00:00Z".into(),
            published_at: Some("2025-01-02T00:00:00Z".into()),
            assets: vec![],
            upload_url: String::new(),
            body: Some("First release!".into()),
        };
        let entry = release_to_entry(&release);
        assert!(entry.is_dir);
        assert_eq!(entry.name, "v1.0.0");
        assert_eq!(entry.path, "/.github-releases/v1.0.0/");
        assert_eq!(entry.modified.as_deref(), Some("2025-01-02T00:00:00Z"));
        assert_eq!(entry.metadata.get("prerelease").map(|s| s.as_str()), Some("true"));
        assert!(!entry.metadata.contains_key("draft"));
    }

    #[test]
    fn test_asset_to_entry() {
        let asset = GitHubAsset {
            id: 99,
            name: "app.deb".into(),
            size: 1024000,
            download_count: 55,
            browser_download_url: "https://github.com/o/r/releases/download/v1/app.deb".into(),
            content_type: "application/vnd.debian.binary-package".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T12:00:00Z".into(),
        };
        let entry = asset_to_entry(&asset, "v1.0.0");
        assert!(!entry.is_dir);
        assert_eq!(entry.name, "app.deb");
        assert_eq!(entry.path, "/.github-releases/v1.0.0/app.deb");
        assert_eq!(entry.size, 1024000);
        assert_eq!(
            entry.metadata.get("download_count").map(|s| s.as_str()),
            Some("55")
        );
    }

    #[test]
    fn test_release_by_tag_path_encodes_reserved_chars() {
        assert_eq!(
            release_by_tag_path("axpnet", "aeroftp", "release/2026.03"),
            "/repos/axpnet/aeroftp/releases/tags/release%2F2026.03"
        );
    }
}
