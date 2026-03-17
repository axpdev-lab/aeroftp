//! Repository-mode operations
//!
//! Extended operations on repository contents: tree listing via the Git Trees API
//! (for large directories), branch management, and commit history.
//! The core CRUD (list, download, upload, delete) is in `mod.rs` via the
//! StorageProvider trait implementation.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;

use crate::providers::{ProviderError, RemoteEntry, StorageProvider};

use super::model::{GitHubContent, GitHubContentDelete, GitHubContentUpdate};
use super::releases_mode::delete_release;
use super::GitHubVirtualPath;

/// Maximum file size GitHub allows via the Contents API (100 MiB)
const MAX_GITHUB_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Warning threshold for large files (50 MiB)
const WARN_GITHUB_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Contents API truncation threshold — switch to Trees API above this
const CONTENTS_API_MAX_ENTRIES: usize = 1000;

const API_BASE: &str = "https://api.github.com";

// ─── Git Trees API response types ───

/// A node in the recursive tree listing
#[derive(Debug, Deserialize)]
struct GitTreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    size: Option<u64>,
    sha: String,
}

/// Response from `GET /repos/{owner}/{repo}/git/trees/{sha}?recursive=1`
#[derive(Debug, Deserialize)]
struct GitTreeResponse {
    #[allow(dead_code)]
    sha: String,
    tree: Vec<GitTreeEntry>,
    /// `true` if the tree was truncated (>100k entries)
    #[serde(default)]
    truncated: bool,
}

/// Response from `GET /repos/{owner}/{repo}/git/ref/heads/{branch}`
#[derive(Debug, Deserialize)]
struct GitRefResponse {
    object: GitRefObject,
}

#[derive(Debug, Deserialize)]
struct GitRefObject {
    sha: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    object_type: String,
}

/// Response wrapper for Contents API PUT/DELETE operations
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ContentMutationResponse {
    content: Option<ContentMutationFile>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ContentMutationFile {
    sha: String,
}

// ─── Debug logging ───

#[cfg(debug_assertions)]
fn gh_log(msg: &str) {
    eprintln!("[github/repo] {}", msg);
}

#[cfg(not(debug_assertions))]
fn gh_log(_msg: &str) {}

// ─── Path utilities ───

/// Normalize a repository path: strip leading `/`, collapse double slashes.
fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    let mut result = String::with_capacity(trimmed.len());
    let mut prev_slash = false;
    for ch in trimmed.chars() {
        if ch == '/' {
            if prev_slash {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        result.push(ch);
    }
    result.trim_end_matches('/').to_string()
}

/// Extract filename from a path
fn filename_from_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// ─── Core implementation ───

use super::GitHubProvider;

impl GitHubProvider {
    // ── Internal helpers ──

    /// Build the Contents API URL for a path
    fn contents_url(&self, path: &str) -> String {
        let norm = normalize_path(path);
        if norm.is_empty() {
            format!(
                "{}/repos/{}/{}/contents?ref={}",
                API_BASE, self.owner, self.repo, self.branch
            )
        } else {
            {
                let encoded_path = norm
                    .split('/')
                    .map(|seg| urlencoding::encode(seg).into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                format!(
                    "{}/repos/{}/{}/contents/{}?ref={}",
                    API_BASE,
                    self.owner,
                    self.repo,
                    encoded_path,
                    self.branch
                )
            }
        }
    }

    /// Build the raw download URL for a file
    fn raw_url(&self, path: &str) -> String {
        let norm = normalize_path(path);
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            self.owner, self.repo, self.branch, norm
        )
    }

    /// Convert a GitHubContent entry to a RemoteEntry
    fn content_to_entry(item: &GitHubContent) -> Option<RemoteEntry> {
        let is_dir = match item.content_type.as_str() {
            "dir" => true,
            "file" => false,
            "symlink" | "submodule" => return None,
            _ => false,
        };

        let mut metadata = HashMap::new();
        metadata.insert("sha".to_string(), item.sha.clone());
        if let Some(ref url) = item.html_url {
            metadata.insert("html_url".to_string(), url.clone());
        }

        Some(RemoteEntry {
            name: item.name.clone(),
            path: format!("/{}", item.path),
            is_dir,
            size: item.size.unwrap_or(0),
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata,
        })
    }

    /// Convert a Git Tree entry to a RemoteEntry
    fn tree_entry_to_remote(entry: &GitTreeEntry) -> Option<RemoteEntry> {
        let is_dir = match entry.entry_type.as_str() {
            "tree" => true,
            "blob" => false,
            _ => return None,
        };

        let name = filename_from_path(&entry.path).to_string();
        let mut metadata = HashMap::new();
        metadata.insert("sha".to_string(), entry.sha.clone());
        metadata.insert("mode".to_string(), entry.mode.clone());

        Some(RemoteEntry {
            name,
            path: format!("/{}", entry.path),
            is_dir,
            size: entry.size.unwrap_or(0),
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata,
        })
    }

    /// Fetch the full recursive tree for the current branch.
    async fn fetch_full_tree(&mut self) -> Result<Vec<GitTreeEntry>, ProviderError> {
        let url = format!(
            "{}/repos/{}/{}/git/trees/{}?recursive=1",
            API_BASE, self.owner, self.repo, self.branch
        );

        gh_log(&format!("Fetching full tree: {}", url));

        let tree_resp: GitTreeResponse = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        if tree_resp.truncated {
            gh_log("Warning: tree response was truncated (>100k entries)");
        }

        // Populate SHA cache from tree entries
        for entry in &tree_resp.tree {
            self.cache_sha(&entry.path, &entry.sha);
        }

        Ok(tree_resp.tree)
    }

    /// Resolve the SHA for a file, checking cache first, then fetching via stat.
    async fn resolve_sha(&mut self, path: &str) -> Result<String, ProviderError> {
        if let Some(sha) = self.get_cached_sha(path).cloned() {
            return Ok(sha);
        }
        let entry = self.stat_file(path).await?;
        entry
            .metadata
            .get("sha")
            .cloned()
            .ok_or_else(|| ProviderError::Other("No SHA in stat response".into()))
    }

    // ── Public API ──

    /// List the contents of a directory in the repository.
    ///
    /// Uses the Contents API for directories with <=1000 entries.
    /// Falls back to the Git Trees API for larger directories.
    pub async fn list_contents(
        &mut self,
        path: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let url = self.contents_url(path);
        gh_log(&format!("list_contents: {}", url));

        // Try as directory listing (returns Vec)
        match self.client.get_json::<Vec<GitHubContent>>(&url).await {
            Ok(items) => {
                // Cache SHAs
                for item in &items {
                    self.cache_sha(&item.path, &item.sha);
                }

                // If at/near the Contents API limit, fall back to Trees API
                if items.len() >= CONTENTS_API_MAX_ENTRIES {
                    gh_log("Contents API returned >=1000 entries, falling back to Trees API");
                    return self.list_contents_via_tree(path).await;
                }

                let entries: Vec<RemoteEntry> = items
                    .iter()
                    .filter_map(Self::content_to_entry)
                    .collect();

                Ok(entries)
            }
            Err(e) => {
                // Might be a single file (returns object, not array).
                // Try to parse as a single item.
                match self.client.get_json::<GitHubContent>(&url).await {
                    Ok(item) => {
                        self.cache_sha(&item.path, &item.sha);
                        match Self::content_to_entry(&item) {
                            Some(entry) => Ok(vec![entry]),
                            None => Ok(vec![]),
                        }
                    }
                    Err(_) => Err(ProviderError::from(e)),
                }
            }
        }
    }

    /// Fall back to the Git Trees API for large directories.
    async fn list_contents_via_tree(
        &mut self,
        path: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let norm = normalize_path(path);
        let tree = self.fetch_full_tree().await?;

        let prefix = if norm.is_empty() {
            String::new()
        } else {
            format!("{}/", norm)
        };

        let entries: Vec<RemoteEntry> = tree
            .iter()
            .filter(|e| {
                if norm.is_empty() {
                    !e.path.contains('/')
                } else if let Some(rest) = e.path.strip_prefix(&prefix) {
                    !rest.contains('/')
                } else {
                    false
                }
            })
            .filter_map(Self::tree_entry_to_remote)
            .collect();

        Ok(entries)
    }

    /// Download a file from the repository to a local path.
    ///
    /// Uses the raw.githubusercontent.com endpoint for streaming downloads.
    pub async fn download_file(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let entry = self.stat_file(remote_path).await?;
        let file_size = entry.size;

        if file_size > MAX_GITHUB_FILE_SIZE {
            return Err(ProviderError::TransferFailed(format!(
                "File too large ({:.1} MiB). GitHub Contents API has a 100 MiB limit.",
                file_size as f64 / 1_048_576.0
            )));
        }

        let url = self.raw_url(remote_path);
        gh_log(&format!("download_file: {} -> {}", url, local_path));

        let resp = self
            .client
            .get_raw(&url)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let total_size = resp.content_length().unwrap_or(file_size);
        let mut stream = resp.bytes_stream();

        let mut file = tokio::fs::File::create(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut downloaded: u64 = 0;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result
                .map_err(|e| ProviderError::TransferFailed(format!("Download stream error: {}", e)))?;

            file.write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;

            downloaded += chunk.len() as u64;

            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }

        file.flush().await.map_err(ProviderError::IoError)?;

        gh_log(&format!("download_file complete: {} bytes", downloaded));

        Ok(())
    }

    /// Upload a local file to the repository.
    ///
    /// Uses the Contents API (`PUT /repos/{owner}/{repo}/contents/{path}`).
    pub async fn upload_file(
        &mut self,
        local_path: &str,
        remote_path: &str,
        commit_message: Option<&str>,
    ) -> Result<(), ProviderError> {
        let norm = normalize_path(remote_path);
        let filename = filename_from_path(&norm);

        let data = tokio::fs::read(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let file_size = data.len() as u64;

        if file_size > MAX_GITHUB_FILE_SIZE {
            return Err(ProviderError::TransferFailed(format!(
                "File too large ({:.1} MiB). GitHub has a 100 MiB file size limit.",
                file_size as f64 / 1_048_576.0
            )));
        }

        if file_size > WARN_GITHUB_FILE_SIZE {
            gh_log(&format!(
                "Warning: uploading large file ({:.1} MiB). Consider using Git LFS for files >50 MiB.",
                file_size as f64 / 1_048_576.0
            ));
        }

        let encoded = BASE64.encode(&data);

        let existing_sha = match self.resolve_sha(remote_path).await {
            Ok(sha) => Some(sha),
            Err(ProviderError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };

        let default_msg = format!("Update {} via AeroFTP", filename);
        let message = commit_message.unwrap_or(&default_msg).to_string();

        let body = GitHubContentUpdate {
            message,
            content: encoded,
            sha: existing_sha,
            branch: Some(self.branch.clone()),
            committer: self.content_committer(),
        };

        let encoded_path = norm
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/repos/{}/{}/contents/{}",
            API_BASE,
            self.owner,
            self.repo,
            encoded_path
        );

        gh_log(&format!("upload_file: {} ({} bytes)", url, file_size));

        let body_json = serde_json::to_value(&body)
            .map_err(|e| ProviderError::Other(format!("Serialize error: {}", e)))?;

        let resp_json = self
            .client
            .put_json(&url, &body_json)
            .await
            .map_err(ProviderError::from)?;

        // Update SHA cache from response
        if let Some(content) = resp_json.get("content") {
            if let Some(sha) = content.get("sha").and_then(|s| s.as_str()) {
                self.cache_sha(&norm, sha);
            }
        }

        gh_log(&format!("upload_file complete: {}", norm));
        Ok(())
    }

    /// Delete a file from the repository.
    pub async fn delete_file(
        &mut self,
        remote_path: &str,
        commit_message: Option<&str>,
    ) -> Result<(), ProviderError> {
        let norm = normalize_path(remote_path);
        let filename = filename_from_path(&norm);

        let sha = self.resolve_sha(remote_path).await?;

        let default_msg = format!("Delete {} via AeroFTP", filename);
        let message = commit_message.unwrap_or(&default_msg).to_string();

        let body = GitHubContentDelete {
            message,
            sha,
            branch: Some(self.branch.clone()),
            committer: self.content_committer(),
        };

        let encoded_path = norm
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/repos/{}/{}/contents/{}",
            API_BASE,
            self.owner,
            self.repo,
            encoded_path
        );

        gh_log(&format!("delete_file: {}", url));

        let body_json = serde_json::to_value(&body)
            .map_err(|e| ProviderError::Other(format!("Serialize error: {}", e)))?;

        self.client
            .delete_json(&url, &body_json)
            .await
            .map_err(ProviderError::from)?;

        // Remove from cache
        self.sha_cache.remove(&(self.branch.clone(), norm.clone()));

        gh_log(&format!("delete_file complete: {}", norm));
        Ok(())
    }

    /// Create a logical directory in the repository by writing a `.gitkeep` placeholder.
    pub async fn create_directory(
        &mut self,
        path: &str,
        commit_message: Option<&str>,
    ) -> Result<(), ProviderError> {
        let norm = normalize_path(path);

        if self.parse_virtual_path(&norm).is_some() {
            return Err(ProviderError::NotSupported(
                "Creating GitHub releases via mkdir is not supported".to_string(),
            ));
        }

        let gitkeep_path = format!("{}/.gitkeep", norm);
        let default_msg = format!("Create directory {} via AeroFTP", norm);
        let message = commit_message.unwrap_or(&default_msg).to_string();

        let body = GitHubContentUpdate {
            message,
            content: String::new(),
            sha: None,
            branch: Some(self.content_branch().to_string()),
            committer: self.content_committer(),
        };

        let encoded_path = gitkeep_path
            .split('/')
            .map(|seg| urlencoding::encode(seg).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{}/repos/{}/{}/contents/{}",
            API_BASE,
            self.owner,
            self.repo,
            encoded_path
        );

        let body_json = serde_json::to_value(&body)
            .map_err(|e| ProviderError::Other(format!("Serialize error: {}", e)))?;

        self.client
            .put_json(&url, &body_json)
            .await
            .map_err(ProviderError::from)?;

        Ok(())
    }

    /// Delete a logical directory by removing all contained files with the same commit message.
    pub async fn delete_directory_recursive(
        &mut self,
        path: &str,
        commit_message: Option<&str>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);

        if let Some(virtual_path) = self.parse_virtual_path(&resolved) {
            return match virtual_path {
                GitHubVirtualPath::ReleasesRoot => Err(ProviderError::NotSupported(
                    "Recursive deletion of all releases is not supported".to_string(),
                )),
                GitHubVirtualPath::ReleaseTag(tag) => {
                    delete_release(&mut self.client, &self.owner, &self.repo, &tag).await
                }
                GitHubVirtualPath::ReleaseAsset { .. } => self.delete_file(path, commit_message).await,
            };
        }

        let entries = self.list(path).await?;
        for entry in entries {
            if entry.is_dir {
                Box::pin(self.delete_directory_recursive(&entry.path, commit_message)).await?;
            } else {
                self.delete_file(&entry.path, commit_message).await?;
            }
        }

        Ok(())
    }

    /// Get metadata for a single file or directory.
    pub async fn stat_file(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let url = self.contents_url(path);
        gh_log(&format!("stat_file: {}", url));

        // Try as single file first
        match self.client.get_json::<GitHubContent>(&url).await {
            Ok(item) => {
                self.cache_sha(&item.path, &item.sha);
                Self::content_to_entry(&item).ok_or_else(|| {
                    ProviderError::NotSupported(format!(
                        "Unsupported content type: {}",
                        item.content_type
                    ))
                })
            }
            Err(e) => {
                // Might be a directory (returns array). If so, synthesize a dir entry.
                let norm = normalize_path(path);
                match self.client.get_json::<Vec<GitHubContent>>(&url).await {
                    Ok(_items) => {
                        let name = filename_from_path(&norm).to_string();
                        Ok(RemoteEntry {
                            name: if name.is_empty() {
                                "/".to_string()
                            } else {
                                name
                            },
                            path: format!("/{}", norm),
                            is_dir: true,
                            size: 0,
                            modified: None,
                            permissions: None,
                            owner: None,
                            group: None,
                            is_symlink: false,
                            link_target: None,
                            mime_type: None,
                            metadata: HashMap::new(),
                        })
                    }
                    Err(_) => Err(ProviderError::from(e)),
                }
            }
        }
    }

    /// Check whether a file or directory exists at the given path.
    pub async fn file_exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat_file(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Search for files matching a glob pattern across the entire repository.
    pub async fn search_files(
        &mut self,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let norm = normalize_path(path);

        let glob = globset::GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| {
                ProviderError::InvalidPath(format!("Invalid search pattern '{}': {}", pattern, e))
            })?;
        let matcher = glob.compile_matcher();

        gh_log(&format!(
            "search_files: pattern='{}' path='{}'",
            pattern, norm
        ));

        let tree = self.fetch_full_tree().await?;

        let prefix = if norm.is_empty() {
            String::new()
        } else {
            format!("{}/", norm)
        };

        let entries: Vec<RemoteEntry> = tree
            .iter()
            .filter(|e| {
                if !norm.is_empty() && !e.path.starts_with(&prefix) && e.path != norm {
                    return false;
                }
                let match_target = if !prefix.is_empty() {
                    e.path.strip_prefix(&prefix).unwrap_or(&e.path)
                } else {
                    &e.path
                };
                matcher.is_match(match_target)
                    || matcher.is_match(filename_from_path(&e.path))
            })
            .filter_map(Self::tree_entry_to_remote)
            .collect();

        gh_log(&format!("search_files: {} matches", entries.len()));

        Ok(entries)
    }

    /// Generate a shareable link for a file.
    pub fn create_share_link_for(&self, path: &str, is_private: bool) -> String {
        let norm = normalize_path(path);
        if is_private {
            format!(
                "https://github.com/{}/{}/blob/{}/{}",
                self.owner, self.repo, self.branch, norm
            )
        } else {
            format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                self.owner, self.repo, self.branch, norm
            )
        }
    }

    /// Get the HEAD commit SHA for the current branch.
    pub async fn get_head_sha(&mut self) -> Result<String, ProviderError> {
        let url = format!(
            "{}/repos/{}/{}/git/ref/heads/{}",
            API_BASE, self.owner, self.repo, self.branch
        );

        gh_log(&format!("get_head_sha: {}", url));

        let ref_resp: GitRefResponse = self
            .client
            .get_json(&url)
            .await
            .map_err(ProviderError::from)?;

        gh_log(&format!(
            "HEAD sha for {}: {}",
            self.branch, ref_resp.object.sha
        ));

        Ok(ref_resp.object.sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("/"), "");
        assert_eq!(normalize_path("/src/main.rs"), "src/main.rs");
        assert_eq!(normalize_path("src/main.rs"), "src/main.rs");
        assert_eq!(normalize_path("//src//lib.rs//"), "src/lib.rs");
        assert_eq!(normalize_path(""), "");
        assert_eq!(normalize_path("/README.md"), "README.md");
    }

    #[test]
    fn test_filename_from_path() {
        assert_eq!(filename_from_path("src/main.rs"), "main.rs");
        assert_eq!(filename_from_path("README.md"), "README.md");
        assert_eq!(filename_from_path("a/b/c/d.txt"), "d.txt");
        assert_eq!(filename_from_path(""), "");
    }
}
