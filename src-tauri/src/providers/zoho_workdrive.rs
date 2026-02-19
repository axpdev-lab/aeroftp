//! Zoho WorkDrive Storage Provider
//!
//! Implements StorageProvider for Zoho WorkDrive using the WorkDrive API v1.
//! Uses OAuth2 for authentication. API follows JSON:API specification.
//!
//! Zoho WorkDrive hierarchy:
//!   Team → Workspaces → Team Folders → Files/Folders
//!
//! For personal use, we navigate the user's "privatespace" (My Folders).

use async_trait::async_trait;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{info, debug};

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo,
    sanitize_api_error,
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
};

/// Zoho WorkDrive provider configuration
#[derive(Debug, Clone)]
pub struct ZohoWorkdriveConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Zoho region: "us", "eu", "in", "au", "jp", "uk", "ca", "sa"
    pub region: String,
}

impl ZohoWorkdriveConfig {
    pub fn new(client_id: &str, client_secret: &str, region: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            region: region.to_string(),
        }
    }

    /// Zoho API domain for this region (different from web domain!)
    /// Web: workdrive.zoho.{tld} — API: www.zohoapis.{ext}
    fn api_domain(&self) -> &str {
        match self.region.as_str() {
            "eu" => "www.zohoapis.eu",
            "in" => "www.zohoapis.in",
            "au" => "www.zohoapis.com.au",
            "jp" => "www.zohoapis.jp",
            "uk" => "www.zohoapis.uk",
            "ca" => "www.zohoapis.ca",
            "sa" => "www.zohoapis.sa",
            "cn" => "www.zohoapis.com.cn",
            "ae" => "www.zohoapis.ae",
            _ => "www.zohoapis.com", // US default
        }
    }

    /// Download domain for this region (dedicated download servers).
    /// Different from API domain! API: www.zohoapis.{ext} — Download: download.zoho.{tld}
    fn download_domain(&self) -> &str {
        match self.region.as_str() {
            "eu" => "download.zoho.eu",
            "in" => "download.zoho.in",
            "au" => "download.zoho.com.au",
            "jp" => "download.zoho.jp",
            "cn" => "download.zoho.com.cn",
            "ae" => "files.zoho.ae",
            "ca" => "download.zohocloud.ca",
            "sa" => "files.zoho.sa",
            "uk" => "download.zoho.uk",
            _ => "download.zoho.com", // US default
        }
    }

    /// API base URL for this region: www.zohoapis.{ext}/workdrive/api/v1
    /// Used for metadata operations (list, mkdir, delete, rename).
    pub fn api_base(&self) -> String {
        format!("https://{}/workdrive/api/v1", self.api_domain())
    }
}

// ── JSON:API response structures ──────────────────────────────────────────

/// JSON:API wrapper for a single resource
#[derive(Debug, Deserialize)]
struct JsonApiResponse<T> {
    data: T,
}

/// JSON:API wrapper for a list of resources
#[derive(Debug, Deserialize)]
struct JsonApiListResponse<T> {
    data: Vec<T>,
}

/// Team resource
#[derive(Debug, Deserialize)]
struct TeamResource {
    id: String,
    attributes: TeamAttributes,
}

#[derive(Debug, Deserialize)]
struct TeamAttributes {
    name: String,
    #[serde(default)]
    storage_used: Option<u64>,
    #[serde(default)]
    storage_quota: Option<u64>,
}

/// Team current user info (for getting teamUserID)
#[derive(Debug, Deserialize)]
struct TeamCurrentUserResource {
    id: String,
}

/// Privatespace / workspace info
#[derive(Debug, Deserialize)]
struct WorkspaceResource {
    id: String,
}

/// Workspace list resource (for GET /teams/{id}/workspaces)
#[derive(Debug, Deserialize)]
struct WorkspaceListResource {
    id: String,
    #[allow(dead_code)]
    attributes: WorkspaceListAttributes,
}

#[derive(Debug, Deserialize)]
struct WorkspaceListAttributes {
    #[allow(dead_code)]
    name: String,
}

/// Team Folder resource (inside a workspace)
#[derive(Debug, Deserialize)]
struct TeamFolderResource {
    id: String,
    attributes: TeamFolderAttributes,
}

#[derive(Debug, Deserialize)]
struct TeamFolderAttributes {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    modified_time: Option<String>,
}

/// File/Folder resource from WorkDrive API
#[derive(Debug, Deserialize)]
struct FileResource {
    id: String,
    attributes: FileAttributes,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FileAttributes {
    #[serde(rename = "name")]
    name: String,
    /// "file" or "folder" (Zoho may use either "type" or "is_folder")
    #[serde(rename = "type", default)]
    file_type: String,
    /// Fallback: boolean is_folder field
    #[serde(default)]
    is_folder: Option<bool>,
    #[serde(rename = "storage_info", default)]
    storage_info: Option<StorageInfoAttr>,
    #[serde(rename = "modified_time", default)]
    modified_time: Option<String>,
    #[serde(rename = "modified_time_i18", default)]
    modified_time_i18: Option<String>,
    /// File extension (e.g. "pdf", "docx")
    #[serde(default)]
    extn: Option<String>,
    /// Direct download URL provided by Zoho (e.g. "https://download-accl.zoho.com/v1/workdrive/download/{id}")
    #[serde(default)]
    download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StorageInfoAttr {
    /// Human-readable size string (e.g. "6.35 KB") — not used for parsing
    #[serde(default)]
    #[allow(dead_code)]
    size: Option<String>,
    /// Numeric size in bytes
    #[serde(default)]
    size_in_bytes: Option<u64>,
}

/// User info from /users/me
#[derive(Debug, Deserialize)]
struct UserResource {
    #[allow(dead_code)]
    id: String,
    attributes: UserAttributes,
}

#[derive(Debug, Deserialize)]
struct UserAttributes {
    email_id: Option<String>,
    #[allow(dead_code)]
    display_name: Option<String>,
}

// ── Provider implementation ───────────────────────────────────────────────

/// Zoho WorkDrive Storage Provider
pub struct ZohoWorkdriveProvider {
    config: ZohoWorkdriveConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    /// Current folder ID (starts with privatespace root)
    current_folder_id: String,
    /// Human-readable current path
    current_path: String,
    /// Cache: path → folder_id
    folder_cache: HashMap<String, String>,
    /// Team ID (discovered on connect)
    team_id: Option<String>,
    /// Privatespace (workspace) ID — needs different list endpoint than folder IDs
    privatespace_id: Option<String>,
    /// Authenticated user email
    account_email: Option<String>,
    /// Team folders discovered during connect: (id, name)
    team_folders: Vec<(String, String)>,
}

impl ZohoWorkdriveProvider {
    pub fn new(config: ZohoWorkdriveConfig) -> Self {
        // Build HTTP client with default Accept header required by Zoho JSON:API
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.api+json"),
        );
        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client,
            connected: false,
            current_folder_id: String::new(),
            current_path: "/".to_string(),
            folder_cache: HashMap::new(),
            team_id: None,
            privatespace_id: None,
            account_email: None,
            team_folders: Vec::new(),
        }
    }

    /// Get OAuth config for token operations
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::zoho(&self.config.client_id, &self.config.client_secret, &self.config.region)
    }

    /// Get authorization header with valid token
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        use secrecy::ExposeSecret;
        let token = self.oauth_manager.get_valid_token(&self.oauth_config()).await?;
        // Zoho uses "Zoho-oauthtoken" format, but "Bearer" also works with newer APIs
        HeaderValue::from_str(&format!("Zoho-oauthtoken {}", token.expose_secret()))
            .map_err(|_| ProviderError::Other("Invalid token characters".into()))
    }

    /// API base URL
    fn api_base(&self) -> String {
        self.config.api_base()
    }

    // ── Zoho-specific trash management ─────────────────────────────────

    /// Helper: PATCH a single file/folder status (1=active, 51=trash, 61=delete)
    async fn patch_file_status(&self, file_id: &str, status: &str) -> Result<(), ProviderError> {
        let url = format!("{}/files/{}", self.api_base(), file_id);
        let body = serde_json::json!({
            "data": {
                "attributes": { "status": status },
                "type": "files"
            }
        });

        let resp = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
            .body(body.to_string())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status_code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "PATCH status {} failed ({}): {}", status, status_code, sanitize_api_error(&text)
            )));
        }
        Ok(())
    }

    /// Helper: PATCH multiple files/folders status in batch (max 200 per Zoho API)
    async fn patch_files_status_batch(&self, file_ids: &[String], status: &str) -> Result<(), ProviderError> {
        if file_ids.is_empty() {
            return Ok(());
        }

        // Zoho allows max 200 objects per batch
        for chunk in file_ids.chunks(200) {
            let items: Vec<serde_json::Value> = chunk.iter().map(|id| {
                serde_json::json!({
                    "attributes": { "status": status },
                    "id": id,
                    "type": "files"
                })
            }).collect();

            let body = serde_json::json!({ "data": items });
            let url = format!("{}/files", self.api_base());

            let resp = self.client
                .patch(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
                .body(body.to_string())
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !resp.status().is_success() {
                let status_code = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Batch PATCH status {} failed ({}): {}", status, status_code, sanitize_api_error(&text)
                )));
            }
        }
        Ok(())
    }

    /// Permanently delete a file/folder (must be in trash first)
    pub async fn permanent_delete(&self, file_id: &str) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive permanent delete: {}", file_id);
        self.patch_file_status(file_id, "61").await?;
        info!("Permanently deleted: {}", file_id);
        Ok(())
    }

    /// Permanently delete multiple files/folders in batch (max 200 per request)
    pub async fn permanent_delete_batch(&self, file_ids: &[String]) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive permanent delete batch: {} items", file_ids.len());
        self.patch_files_status_batch(file_ids, "61").await?;
        info!("Permanently deleted {} items", file_ids.len());
        Ok(())
    }

    /// Restore a file/folder from trash to its original location
    pub async fn restore_from_trash(&self, file_id: &str) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive restore from trash: {}", file_id);
        self.patch_file_status(file_id, "1").await?;
        info!("Restored from trash: {}", file_id);
        Ok(())
    }

    /// Restore multiple files/folders from trash in batch (max 200 per request)
    pub async fn restore_from_trash_batch(&self, file_ids: &[String]) -> Result<(), ProviderError> {
        info!("Zoho WorkDrive restore batch: {} items", file_ids.len());
        self.patch_files_status_batch(file_ids, "1").await?;
        info!("Restored {} items from trash", file_ids.len());
        Ok(())
    }

    /// List trashed files from privatespace and team folders (with pagination)
    pub async fn list_trash(&self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut results = Vec::new();
        let page_limit = 50;

        // List privatespace trash (paginated)
        if let Some(ref ps_id) = self.privatespace_id {
            let mut offset = 0;
            loop {
                let url = format!(
                    "{}/privatespace/{}/trashedfiles?page%5Blimit%5D={}&page%5Boffset%5D={}",
                    self.api_base(), ps_id, page_limit, offset
                );
                info!("Zoho WorkDrive list trash: GET {}", url);

                let resp = self.client
                    .get(&url)
                    .header(AUTHORIZATION, self.auth_header().await?)
                    .send().await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                if !resp.status().is_success() { break; }

                let body = resp.text().await.unwrap_or_default();
                if let Ok(parsed) = serde_json::from_str::<JsonApiListResponse<FileResource>>(&body) {
                    let count = parsed.data.len();
                    info!("Zoho WorkDrive trash (privatespace): {} items at offset {}", count, offset);
                    for file in &parsed.data {
                        results.push(self.to_remote_entry(file, "[Trash]"));
                    }
                    if count < page_limit {
                        break; // Last page
                    }
                    offset += count;
                } else {
                    break;
                }
            }
        }

        // List team folder trash (paginated)
        for tf in &self.team_folders.clone() {
            let mut offset = 0;
            loop {
                let url = format!(
                    "{}/teamfolders/{}/trashedfiles?page%5Blimit%5D={}&page%5Boffset%5D={}",
                    self.api_base(), tf.0, page_limit, offset
                );

                let resp = self.client
                    .get(&url)
                    .header(AUTHORIZATION, self.auth_header().await?)
                    .send().await;

                let resp = match resp {
                    Ok(r) if r.status().is_success() => r,
                    _ => break,
                };

                let body = resp.text().await.unwrap_or_default();
                if let Ok(parsed) = serde_json::from_str::<JsonApiListResponse<FileResource>>(&body) {
                    let count = parsed.data.len();
                    info!("Zoho WorkDrive trash (team folder {}): {} items at offset {}", tf.1, count, offset);
                    for file in &parsed.data {
                        results.push(self.to_remote_entry(file, &format!("[Trash/{}]", tf.1)));
                    }
                    if count < page_limit {
                        break; // Last page
                    }
                    offset += count;
                } else {
                    break;
                }
            }
        }

        info!("Zoho WorkDrive total trash items: {}", results.len());
        Ok(results)
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.oauth_manager.has_tokens(OAuthProvider::ZohoWorkdrive)
    }

    /// Discover team ID and privatespace root folder
    async fn discover_team(&mut self) -> Result<(), ProviderError> {
        // Step 1: Get user info and user ID
        let user_url = format!("{}/users/me", self.api_base());
        info!("Zoho WorkDrive: GET {}", user_url);
        let resp = self.client
            .get(&user_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let mut user_id: Option<String> = None;
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<JsonApiResponse<UserResource>>().await {
                self.account_email = body.data.attributes.email_id.clone();
                user_id = Some(body.data.id.clone());
                info!("Zoho WorkDrive user: {} ({})", body.data.attributes.email_id.as_deref().unwrap_or("?"), body.data.id);
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /users/me failed ({}): {}", status, text);
        }

        // Step 2: Get teams — try /teams first, then /users/{id}/teams as fallback
        let teams_url = format!("{}/teams", self.api_base());
        info!("Zoho WorkDrive: GET {}", teams_url);
        let resp = self.client
            .get(&teams_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let team = if resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /teams response: {}", &body[..body.len().min(500)]);
            match serde_json::from_str::<JsonApiListResponse<TeamResource>>(&body) {
                Ok(teams) if !teams.data.is_empty() => {
                    teams.data.into_iter().next()
                }
                Ok(_) => {
                    info!("Zoho WorkDrive /teams returned empty list, trying user-specific endpoint");
                    None
                }
                Err(e) => {
                    info!("Zoho WorkDrive /teams parse error: {}", e);
                    None
                }
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /teams failed ({}): {}", status, &text[..text.len().min(500)]);
            None
        };

        // Fallback: try /users/{user_id}/teams
        let team: Option<TeamResource> = if team.is_some() {
            team
        } else if let Some(ref uid) = user_id {
            let user_teams_url = format!("{}/users/{}/teams", self.api_base(), uid);
            info!("Zoho WorkDrive: GET {} (fallback)", user_teams_url);
            let resp = self.client
                .get(&user_teams_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                debug!("Zoho WorkDrive /users/{}/teams response: {}", uid, &body[..body.len().min(500)]);
                match serde_json::from_str::<JsonApiListResponse<TeamResource>>(&body) {
                    Ok(teams) => teams.data.into_iter().next(),
                    Err(e) => {
                        info!("Zoho WorkDrive /users/{}/teams parse error: {}", uid, e);
                        None
                    }
                }
            } else {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                debug!("Zoho WorkDrive /users/{}/teams failed ({}): {}", uid, status, &text[..text.len().min(500)]);
                None
            }
        } else {
            None
        };

        let team = team.ok_or_else(|| ProviderError::Other(
            "No teams found in Zoho WorkDrive. Create a team at workdrive.zoho.eu (or your region's URL).".to_string()
        ))?;

        self.team_id = Some(team.id.clone());
        info!("Zoho WorkDrive team: {} ({})", team.attributes.name, team.id);

        // Step 3: Get team-specific user ID (different from ZUID)
        // rclone flow: /teams/{teamID}/currentuser → teamUserID
        let cu_url = format!("{}/teams/{}/currentuser", self.api_base(), team.id);
        info!("Zoho WorkDrive: GET {}", cu_url);
        let resp = self.client
            .get(&cu_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let team_user_id = if resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /currentuser response: {}", &body_text[..body_text.len().min(500)]);
            match serde_json::from_str::<JsonApiResponse<TeamCurrentUserResource>>(&body_text) {
                Ok(cu) => {
                    info!("Zoho WorkDrive teamUserID: {}", cu.data.id);
                    Some(cu.data.id)
                }
                Err(e) => {
                    info!("Zoho WorkDrive /currentuser parse error: {}", e);
                    None
                }
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /currentuser failed ({}): {}", status, &text[..text.len().min(300)]);
            None
        };

        // Step 4: Get privatespace (My Folders) root using teamUserID
        // rclone flow: /users/{teamUserID}/privatespace → root folder ID
        let ps_user_id = team_user_id.as_deref().unwrap_or(
            user_id.as_deref().unwrap_or(&team.id)
        );
        let private_url = format!("{}/users/{}/privatespace", self.api_base(), ps_user_id);
        info!("Zoho WorkDrive: GET {}", private_url);
        let resp = self.client
            .get(&private_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive privatespace response: {}", &body_text[..body_text.len().min(500)]);
            // Zoho returns {"data": [...]} (array), try array first, then single object
            let ps_id = if let Ok(body) = serde_json::from_str::<JsonApiListResponse<WorkspaceResource>>(&body_text) {
                body.data.into_iter().next().map(|r| r.id)
            } else if let Ok(body) = serde_json::from_str::<JsonApiResponse<WorkspaceResource>>(&body_text) {
                Some(body.data.id)
            } else {
                info!("Zoho WorkDrive privatespace parse failed for both array and object formats");
                None
            };

            if let Some(id) = ps_id {
                self.privatespace_id = Some(id.clone());
                self.current_folder_id = id.clone();
                self.folder_cache.insert("/".to_string(), id);
                info!("Zoho WorkDrive privatespace root: {}", self.current_folder_id);
                self.discover_team_folders(&team.id).await;
                return Ok(());
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive /users/{}/privatespace failed ({}): {}", ps_user_id, status, &text[..text.len().min(300)]);
        }

        // Last fallback: use team ID as root
        self.current_folder_id = team.id.clone();
        self.folder_cache.insert("/".to_string(), team.id.clone());

        // Continue to fetch team folders (don't return early)
        self.discover_team_folders(&team.id).await;

        Ok(())
    }

    /// Discover team folders.
    /// Primary: GET /teams/{teamID}/teamfolders (documented endpoint)
    /// Fallback: GET /teams/{teamID}/workspaces (workspaces as virtual folders)
    async fn discover_team_folders(&mut self, team_id: &str) {
        // Primary: GET /teams/{teamId}/teamfolders
        let tf_url = format!("{}/teams/{}/teamfolders", self.api_base(), team_id);
        info!("Zoho WorkDrive: GET {}", tf_url);

        let auth = match self.auth_header().await {
            Ok(h) => h,
            Err(e) => {
                info!("Zoho WorkDrive: auth error for team folders: {}", e);
                return;
            }
        };

        let resp = match self.client.get(&tf_url).header(AUTHORIZATION, auth).send().await {
            Ok(r) => r,
            Err(e) => {
                info!("Zoho WorkDrive: team folders request failed: {}", e);
                return;
            }
        };

        if resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive teamfolders response: {}", &body[..body.len().min(500)]);

            if let Ok(tfs) = serde_json::from_str::<JsonApiListResponse<TeamFolderResource>>(&body) {
                for tf in tfs.data {
                    info!("Zoho WorkDrive team folder: {} ({})", tf.attributes.name, tf.id);
                    self.folder_cache.insert(format!("/{}", tf.attributes.name), tf.id.clone());
                    self.team_folders.push((tf.id, tf.attributes.name));
                }
                if !self.team_folders.is_empty() {
                    info!("Zoho WorkDrive discovered {} team folders", self.team_folders.len());
                    return;
                }
            }
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive teamfolders failed ({}): {}", status, &text[..text.len().min(300)]);
        }

        // Fallback: use workspaces as team folders (they appear as "Cartelle Team" in the UI)
        let ws_url = format!("{}/teams/{}/workspaces", self.api_base(), team_id);
        info!("Zoho WorkDrive: GET {} (fallback — treating workspaces as team folders)", ws_url);

        let auth = match self.auth_header().await {
            Ok(h) => h,
            Err(_) => return,
        };

        let resp = match self.client.get(&ws_url).header(AUTHORIZATION, auth).send().await {
            Ok(r) => r,
            Err(e) => {
                info!("Zoho WorkDrive: workspaces request failed: {}", e);
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive workspaces failed ({}): {}", status, &text[..text.len().min(300)]);
            return;
        }

        let body = resp.text().await.unwrap_or_default();
        debug!("Zoho WorkDrive workspaces response: {}", &body[..body.len().min(500)]);

        if let Ok(ws_list) = serde_json::from_str::<JsonApiListResponse<WorkspaceListResource>>(&body) {
            for ws in ws_list.data {
                info!("Zoho WorkDrive workspace as team folder: {} ({})", ws.attributes.name, ws.id);
                self.folder_cache.insert(format!("/{}", ws.attributes.name), ws.id.clone());
                self.team_folders.push((ws.id, ws.attributes.name));
            }
        }

        info!("Zoho WorkDrive discovered {} team folders (via workspaces)", self.team_folders.len());
    }

    /// List files in a folder by ID.
    /// - Privatespace root: GET /privatespace/{id}/files
    /// - All other folders:  GET /files/{id}/files (universal endpoint)
    async fn list_folder(&self, folder_id: &str) -> Result<Vec<FileResource>, ProviderError> {
        let mut all_files = Vec::new();
        let mut offset: usize = 0;
        let limit: usize = 50;

        // Privatespace root needs its own endpoint; everything else uses /files/{id}/files
        let is_privatespace = self.privatespace_id.as_deref() == Some(folder_id);

        loop {
            let url = if is_privatespace {
                format!(
                    "{}/privatespace/{}/files?page%5Blimit%5D={}&page%5Boffset%5D={}",
                    self.api_base(), folder_id, limit, offset
                )
            } else {
                format!(
                    "{}/files/{}/files?page%5Blimit%5D={}&page%5Boffset%5D={}",
                    self.api_base(), folder_id, limit, offset
                )
            };

            info!("Zoho WorkDrive list: GET {}", url);

            let resp = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();

            if !status.is_success() {
                debug!("Zoho WorkDrive list error ({}): {}", status, &body_text[..body_text.len().min(300)]);
                return Err(ProviderError::Other(
                    format!("List files error {} (folder_id={}): {}", status, folder_id, sanitize_api_error(&body_text))
                ));
            }

            debug!("Zoho WorkDrive list response ({} bytes): {}", body_text.len(), &body_text[..body_text.len().min(500)]);

            let list: JsonApiListResponse<FileResource> = serde_json::from_str(&body_text)
                .map_err(|e| ProviderError::Other(format!("Parse files error: {} — body: {}", e, sanitize_api_error(&body_text))))?;

            let count = list.data.len();
            info!("Zoho WorkDrive list parsed {} items", count);
            all_files.extend(list.data);

            if count < limit {
                break;
            }
            offset += limit;
        }

        Ok(all_files)
    }

    /// Convert a FileResource to RemoteEntry
    fn to_remote_entry(&self, file: &FileResource, parent_path: &str) -> RemoteEntry {
        let is_dir = file.attributes.file_type == "folder"
            || file.attributes.is_folder.unwrap_or(false);
        let size = file.attributes.storage_info
            .as_ref()
            .and_then(|s| s.size_in_bytes)
            .unwrap_or(0);

        let path = if parent_path == "/" {
            format!("/{}", file.attributes.name)
        } else {
            format!("{}/{}", parent_path.trim_end_matches('/'), file.attributes.name)
        };

        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), file.id.clone());

        RemoteEntry {
            name: file.attributes.name.clone(),
            path,
            is_dir,
            size,
            modified: file.attributes.modified_time_i18.clone()
                .or_else(|| file.attributes.modified_time.clone()),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata,
        }
    }

    /// List root directory: team folders (as directories) + privatespace files
    async fn list_root(&self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut entries = Vec::new();

        // 1. Team folders as virtual directory entries
        for (id, name) in &self.team_folders {
            let mut metadata = HashMap::new();
            metadata.insert("id".to_string(), id.clone());
            metadata.insert("resource_type".to_string(), "teamfolder".to_string());
            entries.push(RemoteEntry {
                name: name.clone(),
                path: format!("/{}", name),
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata,
            });
        }

        // 2. Privatespace files (My Folders contents) — merge at root level
        if let Some(ref ps_id) = self.privatespace_id {
            match self.list_folder(ps_id).await {
                Ok(files) => {
                    for f in &files {
                        entries.push(self.to_remote_entry(f, "/"));
                    }
                    info!("Zoho WorkDrive root: {} team folders + {} privatespace items", self.team_folders.len(), files.len());
                }
                Err(e) => {
                    info!("Zoho WorkDrive privatespace list error: {}", e);
                    // If no team folders either, propagate the error
                    if entries.is_empty() {
                        return Err(e);
                    }
                }
            }
        } else {
            info!("Zoho WorkDrive: no privatespace_id set");
        }

        if entries.is_empty() {
            info!("Zoho WorkDrive root listing is empty — no team folders and no privatespace files");
        }

        Ok(entries)
    }

    /// Find a file/folder by name in a parent folder
    async fn find_by_name(&self, name: &str, parent_id: &str) -> Result<Option<FileResource>, ProviderError> {
        let files = self.list_folder(parent_id).await?;
        Ok(files.into_iter().find(|f| f.attributes.name == name))
    }

    /// Resolve a path like "/docs/file.txt" to a folder/file ID.
    /// First path component is checked against team folder names, then privatespace.
    async fn resolve_path(&mut self, path: &str) -> Result<String, ProviderError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok(self.folder_cache.get("/")
                .cloned()
                .unwrap_or_else(|| self.current_folder_id.clone()));
        }

        // Check cache
        let full_path = format!("/{}", path);
        if let Some(id) = self.folder_cache.get(&full_path) {
            return Ok(id.clone());
        }

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        // First component: check team folders by name, then privatespace contents
        let first = parts[0];
        let mut current_path = format!("/{}", first);

        let mut current_id = if let Some(cached) = self.folder_cache.get(&current_path) {
            cached.clone()
        } else if let Some((tf_id, _)) = self.team_folders.iter().find(|(_, name)| name.as_str() == first) {
            let id = tf_id.clone();
            self.folder_cache.insert(current_path.clone(), id.clone());
            id
        } else {
            // Try privatespace listing
            let root_id = self.privatespace_id.as_deref()
                .unwrap_or(&self.current_folder_id);
            let file = self.find_by_name(first, root_id).await?
                .ok_or_else(|| ProviderError::NotFound(current_path.clone()))?;
            self.folder_cache.insert(current_path.clone(), file.id.clone());
            file.id
        };

        // Remaining components: normal walk via /files/{id}/files listing
        for part in &parts[1..] {
            current_path = format!("{}/{}", current_path, part);

            if let Some(cached) = self.folder_cache.get(&current_path) {
                current_id = cached.clone();
                continue;
            }

            let file = self.find_by_name(part, &current_id).await?
                .ok_or_else(|| ProviderError::NotFound(current_path.clone()))?;

            current_id = file.id.clone();
            self.folder_cache.insert(current_path.clone(), file.id);
        }

        Ok(current_id)
    }
}

#[async_trait]
impl StorageProvider for ZohoWorkdriveProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::ZohoWorkdrive
    }

    fn display_name(&self) -> String {
        "Zoho WorkDrive".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if !self.is_authenticated() {
            return Err(ProviderError::AuthenticationFailed(
                "Not authenticated. Complete OAuth2 flow first.".to_string()
            ));
        }

        // Discover team and privatespace
        self.discover_team().await?;

        self.connected = true;
        self.current_path = "/".to_string();
        info!("Connected to Zoho WorkDrive ({})", self.account_email.as_deref().unwrap_or("unknown"));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.folder_cache.clear();
        self.team_id = None;
        self.privatespace_id = None;
        self.team_folders.clear();
        info!("Disconnected from Zoho WorkDrive");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // Determine effective path
        let effective_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else {
            path.to_string()
        };

        // Root listing: team folders + privatespace files combined
        if effective_path == "/" {
            return self.list_root().await;
        }

        let folder_id = if path == "." || path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(path).await?
        };

        let files = self.list_folder(&folder_id).await?;

        Ok(files.iter()
            .map(|f| self.to_remote_entry(f, &effective_path))
            .collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            path.to_string()
        } else if path == ".." {
            let mut parts: Vec<&str> = self.current_path.split('/').filter(|s| !s.is_empty()).collect();
            parts.pop();
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        let folder_id = self.resolve_path(&new_path).await?;
        self.current_folder_id = folder_id;
        self.current_path = if new_path.is_empty() { "/".to_string() } else { new_path };
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        // Zoho uses a DEDICATED download domain: download.zoho.{tld}
        // Path: /v1/workdrive/download/{resource_id}
        // Clean client (no default Accept: application/vnd.api+json header)
        let download_url = format!(
            "https://{}/v1/workdrive/download/{}",
            self.config.download_domain(),
            file.id
        );
        info!("Zoho WorkDrive download: GET {}", download_url);

        let auth = self.auth_header().await?;

        // Reuse self.client (connection pool) but override Accept header for binary download
        let resp = self.client
            .get(&download_url)
            .header(AUTHORIZATION, auth)
            .header(ACCEPT, HeaderValue::from_static("*/*"))
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let final_url = resp.url().to_string();
        debug!("Zoho WorkDrive download response: {} (content-length: {:?}, final_url: {})", status, resp.content_length(), final_url);

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive download error body: {}", &text[..text.len().min(1000)]);
            return Err(ProviderError::Other(format!("Download failed ({}): {}", status, sanitize_api_error(&text))));
        }

        // Streaming download: write chunks to file as they arrive
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let total_size = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();
        let mut file = tokio::fs::File::create(local_path).await
            .map_err(|e| ProviderError::Other(format!("Create file error: {}", e)))?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            file.write_all(&chunk).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }

        info!("Downloaded {} to {} ({} bytes)", remote_path, local_path, downloaded);
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;

        // Zoho dedicated download domain: download.zoho.{tld}/v1/workdrive/download/{id}
        let download_url = format!(
            "https://{}/v1/workdrive/download/{}",
            self.config.download_domain(),
            file.id
        );
        info!("Zoho WorkDrive download_to_bytes: GET {}", download_url);

        // Reuse self.client (connection pool) but override Accept header for binary download
        let resp = self.client
            .get(&download_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(ACCEPT, HeaderValue::from_static("*/*"))
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            debug!("Zoho WorkDrive download_to_bytes error: {} — {}", status, &text[..text.len().min(500)]);
            return Err(ProviderError::Other(format!("Download failed ({}): {}", status, sanitize_api_error(&text))));
        }

        Ok(resp.bytes().await
            .map_err(|e| ProviderError::Other(format!("Download read error: {}", e)))?
            .to_vec())
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let path = remote_path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        // Streaming upload: read file as a stream instead of loading into memory
        let file_meta = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::Other(format!("Metadata error: {}", e)))?;
        let total_size = file_meta.len();
        info!("Zoho WorkDrive upload: parent_id={}, filename={}, size={}", parent_id, file_name, total_size);

        let file = tokio::fs::File::open(local_path).await
            .map_err(|e| ProviderError::Other(format!("Open file error: {}", e)))?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        // Zoho WorkDrive upload uses multipart/form-data
        // parent_id and override-name-exist go in the form body (per Zoho docs)
        let url = format!("{}/upload", self.api_base());

        let file_part = reqwest::multipart::Part::stream(body)
            .file_name(file_name.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| ProviderError::Other(format!("MIME error: {}", e)))?;

        let form = reqwest::multipart::Form::new()
            .text("parent_id", parent_id.clone())
            .text("filename", file_name.to_string())
            .text("override-name-exist", "true")
            .part("content", file_part);

        // Override Accept header: upload response is NOT JSON:API format
        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .multipart(form)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Upload failed ({}): {}", status, sanitize_api_error(&text))));
        }

        let resp_text = resp.text().await.unwrap_or_default();
        debug!("Zoho WorkDrive upload response ({}): {}", status, &resp_text[..resp_text.len().min(300)]);

        if let Some(cb) = on_progress {
            cb(total_size, total_size);
        }

        info!("Uploaded {} to {} (parent_id={})", local_path, remote_path, parent_id);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let path = path.trim_matches('/');
        let (parent_path, folder_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        // JSON:API format for folder creation
        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "name": folder_name,
                    "parent_id": parent_id
                },
                "type": "files"
            }
        });

        let url = format!("{}/files", self.api_base());
        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
            .body(body.to_string())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("mkdir failed: {}", sanitize_api_error(&text))));
        }

        info!("Created folder: {}", path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // Move to trash via PATCH with status "51" (Zoho JSON:API convention)
        let url = format!("{}/files/{}", self.api_base(), file.id);
        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "status": "51"
                },
                "type": "files"
            }
        });

        info!("Zoho WorkDrive move to trash: PATCH {} id={}", url, file.id);
        let resp = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
            .body(body.to_string())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Delete failed ({}): {}", status, sanitize_api_error(&text))));
        }

        info!("Moved to trash: {}", path);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Zoho trash handles recursive deletion
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = from.trim_matches('/');
        let (from_parent_path, file_name) = if let Some(pos) = from_path.rfind('/') {
            (&from_path[..pos], &from_path[pos + 1..])
        } else {
            ("", from_path)
        };

        let from_parent_id = if from_parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(from_parent_path).await?
        };

        let file = self.find_by_name(file_name, &from_parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        let to_path = to.trim_matches('/');
        let (to_parent_path, new_name) = if let Some(pos) = to_path.rfind('/') {
            (&to_path[..pos], &to_path[pos + 1..])
        } else {
            ("", to_path)
        };

        let to_parent_id = if to_parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(to_parent_path).await?
        };

        let is_cross_folder = from_parent_id != to_parent_id;
        let is_rename = file_name != new_name;

        // Step 1: Move to new folder if cross-folder operation
        if is_cross_folder {
            let move_body = serde_json::json!({
                "data": {
                    "attributes": {
                        "parent_id": to_parent_id
                    },
                    "type": "files"
                }
            });

            let url = format!("{}/files/{}/move", self.api_base(), file.id);
            let resp = self.client
                .patch(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
                .body(move_body.to_string())
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Move failed ({}): {}", status, sanitize_api_error(&text)
                )));
            }
            info!("Moved {} to folder {}", from, to_parent_path);
        }

        // Step 2: Rename if the name changed
        if is_rename {
            let rename_body = serde_json::json!({
                "data": {
                    "attributes": {
                        "name": new_name
                    },
                    "type": "files"
                }
            });

            let url = format!("{}/files/{}", self.api_base(), file.id);
            let resp = self.client
                .patch(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
                .body(rename_body.to_string())
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(format!("Rename failed: {}", resp.status())));
            }
        }

        info!("Renamed {} to {}", from, to);
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let path_str = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path_str.rfind('/') {
            (&path_str[..pos], &path_str[pos + 1..])
        } else {
            ("", path_str)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path_str.to_string()))?;

        Ok(self.to_remote_entry(&file, parent_path))
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
        // Zoho REST API doesn't need keep-alive
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let team_name = if let Some(ref team_id) = self.team_id {
            let url = format!("{}/teams/{}", self.api_base(), team_id);
            let resp = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send().await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if let Ok(body) = resp.json::<JsonApiResponse<TeamResource>>().await {
                body.data.attributes.name
            } else {
                "Unknown".to_string()
            }
        } else {
            "Unknown".to_string()
        };

        Ok(format!("Zoho WorkDrive — Team: {} — Region: {}", team_name, self.config.region.to_uppercase()))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = from.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = from_path.rfind('/') {
            (&from_path[..pos], &from_path[pos + 1..])
        } else {
            ("", from_path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(from.to_string()))?;

        // Resolve destination folder
        let to_path = to.trim_matches('/');
        let dest_parent = if let Some(pos) = to_path.rfind('/') {
            &to_path[..pos]
        } else {
            ""
        };

        let dest_id = if dest_parent.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(dest_parent).await?
        };

        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "parent_id": dest_id
                },
                "type": "files"
            }
        });

        let url = format!("{}/files/{}/copy", self.api_base(), file.id);
        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
            .body(body.to_string())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Copy failed: {}", resp.status())));
        }

        info!("Copied {} to {}", from, to);
        Ok(())
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        _expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        // Resolve path to file/folder ID
        let path = path.trim_matches('/');
        let (parent_path, file_name) = if let Some(pos) = path.rfind('/') {
            (&path[..pos], &path[pos + 1..])
        } else {
            ("", path)
        };

        let parent_id = if parent_path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_path(parent_path).await?
        };

        let file = self.find_by_name(file_name, &parent_id).await?
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // Create a download link via Zoho WorkDrive Links API
        // POST /api/v1/links
        let body = serde_json::json!({
            "data": {
                "attributes": {
                    "resource_id": file.id,
                    "link_name": file_name,
                    "request_user_data": false,
                    "allow_download": true,
                    "role_id": "34",
                    "link_type": "download"
                },
                "type": "links"
            }
        });

        let url = format!("{}/links", self.api_base());
        info!("Zoho WorkDrive create share link: POST {} for resource {}", url, file.id);

        let resp = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/vnd.api+json"))
            .body(body.to_string())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let resp_text = resp.text().await
            .map_err(|e| ProviderError::Other(format!("Read response: {}", e)))?;

        if !status.is_success() {
            info!("Zoho WorkDrive create share link failed: {} - {}", status, resp_text);
            return Err(ProviderError::Other(format!("Create share link failed: {} - {}", status, sanitize_api_error(&resp_text))));
        }

        debug!("Zoho WorkDrive create share link response: {}", resp_text);

        // Parse response to extract the link URL
        // Response shape: { "data": { "attributes": { "link": "https://..." } } }
        let parsed: serde_json::Value = serde_json::from_str(&resp_text)
            .map_err(|e| ProviderError::Other(format!("Parse share link response: {}", e)))?;

        let link = parsed["data"]["attributes"]["link"]
            .as_str()
            .or_else(|| parsed["data"]["attributes"]["permalink"].as_str())
            .ok_or_else(|| ProviderError::Other(format!(
                "No link URL in response: {}", sanitize_api_error(&resp_text)
            )))?;

        info!("Zoho WorkDrive share link created: {}", link);
        Ok(link.to_string())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let team_id = self.team_id.as_ref()
            .ok_or_else(|| ProviderError::Other("Not connected to a team".to_string()))?;

        let url = format!("{}/teams/{}", self.api_base(), team_id);
        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Storage info failed: {}", resp.status())));
        }

        let team: JsonApiResponse<TeamResource> = resp.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let used = team.data.attributes.storage_used.unwrap_or(0);
        let total = team.data.attributes.storage_quota.unwrap_or(0);
        let free = total.saturating_sub(used);

        Ok(StorageInfo { used, total, free })
    }

    fn supports_find(&self) -> bool { true }

    async fn find(&mut self, _path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let team_id = self.team_id.as_ref()
            .ok_or_else(|| ProviderError::Other("Not connected to a team".to_string()))?
            .clone();

        let encoded = urlencoding::encode(pattern);
        let url = format!(
            "{}/files/search?search_string={}&team_id={}",
            self.api_base(), encoded, team_id
        );
        info!("Zoho WorkDrive find: GET {}", url);

        let resp = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "Search failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        debug!("Zoho WorkDrive find response ({} bytes): {}", body.len(), &body[..body.len().min(500)]);

        let list: JsonApiListResponse<FileResource> = serde_json::from_str(&body)
            .map_err(|e| ProviderError::ParseError(format!(
                "Parse search results: {} — body: {}", e, sanitize_api_error(&body)
            )))?;

        let entries = list.data.iter()
            .map(|f| self.to_remote_entry(f, "/"))
            .collect();

        info!("Zoho WorkDrive find '{}': {} results", pattern, list.data.len());
        Ok(entries)
    }
}
