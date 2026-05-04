//! Filen Encrypted Notes
//!
//! End-to-end encrypted notes with per-note AES-256-GCM keys.
//! Each note has a unique symmetric key stored encrypted with the user's master key.
//! Titles, content, and previews are encrypted with the per-note key.
//! Tag names are encrypted with the user's master key.
//!
//! Encryption format (v002):
//! - Note key: `encrypt_metadata(JSON({"key": noteKey}))` with master key
//! - Title:    `encrypt_metadata_with_key(JSON({"title": "..."}), noteKey)`
//! - Content:  `encrypt_metadata_with_key(JSON({"content": "..."}), noteKey)`
//! - Preview:  `encrypt_metadata_with_key(JSON({"preview": "..."}), noteKey)`
//! - Tag name: `encrypt_metadata(JSON({"name": "..."}))` with master key

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::{FilenProvider, GATEWAY};
use crate::providers::ProviderError;

/// Maximum encrypted content size (1 MB - 1 byte, per Filen SDK)
const MAX_NOTE_CIPHERTEXT_SIZE: usize = 1_048_575;

/// Maximum preview length (plain text characters before encryption)
const MAX_PREVIEW_LENGTH: usize = 128;

/// Bounded note key cache to prevent unbounded memory growth
const NOTE_KEY_CACHE_MAX: usize = 500;

// ─── Public types ───

/// Supported note content types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NoteType {
    Text,
    Md,
    Code,
    Rich,
    Checklist,
}

impl std::fmt::Display for NoteType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoteType::Text => write!(f, "text"),
            NoteType::Md => write!(f, "md"),
            NoteType::Code => write!(f, "code"),
            NoteType::Rich => write!(f, "rich"),
            NoteType::Checklist => write!(f, "checklist"),
        }
    }
}

/// A decrypted Filen note (list item)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilenNote {
    pub uuid: String,
    pub title: String,
    pub preview: String,
    pub note_type: NoteType,
    pub favorite: bool,
    pub pinned: bool,
    pub trash: bool,
    pub archive: bool,
    pub created_timestamp: u64,
    pub edited_timestamp: u64,
    pub tags: Vec<FilenNoteTagRef>,
    pub participants: Vec<FilenNoteParticipant>,
}

/// Tag reference on a note (just UUID, name resolved separately)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilenNoteTagRef {
    pub uuid: String,
}

/// Note participant info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilenNoteParticipant {
    pub user_id: u64,
    pub is_owner: bool,
    pub email: String,
    pub permissions_write: bool,
}

/// Decrypted note content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilenNoteContent {
    pub content: String,
    pub preview: String,
    pub note_type: NoteType,
    pub edited_timestamp: u64,
    pub editor_id: u64,
}

/// Decrypted note history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilenNoteHistoryEntry {
    pub id: u64,
    pub content: String,
    pub preview: String,
    pub note_type: NoteType,
    pub edited_timestamp: u64,
    pub editor_id: u64,
}

/// A decrypted note tag
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilenNoteTag {
    pub uuid: String,
    pub name: String,
    pub favorite: bool,
    pub created_timestamp: u64,
    pub edited_timestamp: u64,
}

// ─── API response types (private) ───

#[derive(Debug, Deserialize)]
struct NotesListResponse {
    status: bool,
    data: Option<Vec<RawNote>>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNote {
    uuid: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    metadata: String,
    #[serde(rename = "type", default = "default_note_type")]
    note_type: String,
    #[serde(default)]
    favorite: bool,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    trash: bool,
    #[serde(default)]
    archive: bool,
    created_timestamp: u64,
    edited_timestamp: u64,
    #[serde(default)]
    tags: Vec<RawNoteTag>,
    #[serde(default)]
    participants: Vec<RawNoteParticipant>,
}

fn default_note_type() -> String {
    "text".to_string()
}

#[derive(Debug, Deserialize)]
struct RawNoteTag {
    uuid: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNoteParticipant {
    #[serde(default)]
    user_id: u64,
    #[serde(default)]
    is_owner: bool,
    #[serde(default)]
    email: String,
    #[serde(default)]
    permissions_write: bool,
    /// Encrypted note key for this participant
    #[serde(default)]
    metadata: String,
}

#[derive(Debug, Deserialize)]
struct NoteContentResponse {
    status: bool,
    data: Option<RawNoteContent>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNoteContent {
    #[serde(default)]
    content: String,
    #[serde(default)]
    preview: String,
    #[serde(rename = "type", default = "default_note_type")]
    note_type: String,
    edited_timestamp: u64,
    editor_id: u64,
}

#[derive(Debug, Deserialize)]
struct NoteHistoryResponse {
    status: bool,
    data: Option<Vec<RawNoteHistoryEntry>>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNoteHistoryEntry {
    id: u64,
    #[serde(default)]
    content: String,
    #[serde(default)]
    preview: String,
    #[serde(rename = "type", default = "default_note_type")]
    note_type: String,
    edited_timestamp: u64,
    editor_id: u64,
}

#[derive(Debug, Deserialize)]
struct TagsListResponse {
    status: bool,
    data: Option<Vec<RawTagItem>>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTagItem {
    uuid: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    favorite: bool,
    created_timestamp: u64,
    edited_timestamp: u64,
}

#[derive(Debug, Deserialize)]
struct GenericNoteResponse {
    status: bool,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateTagResponse {
    status: bool,
    data: Option<CreateTagData>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateTagData {
    uuid: String,
}

// ─── Note key cache ───

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

/// Cache for decrypted note keys (note UUID → plaintext key).
/// Uses a std::sync::Mutex (not tokio) because operations are CPU-bound lookups.
static NOTE_KEY_CACHE: std::sync::LazyLock<StdMutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| StdMutex::new(HashMap::new()));

fn cache_note_key(uuid: &str, key: &str) {
    if let Ok(mut cache) = NOTE_KEY_CACHE.lock() {
        if cache.len() >= NOTE_KEY_CACHE_MAX {
            cache.clear();
        }
        cache.insert(uuid.to_string(), key.to_string());
    }
}

fn get_cached_note_key(uuid: &str) -> Option<String> {
    NOTE_KEY_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(uuid).cloned())
}

// ─── Helpers ───

pub fn parse_note_type(s: &str) -> NoteType {
    match s {
        "md" => NoteType::Md,
        "code" => NoteType::Code,
        "rich" => NoteType::Rich,
        "checklist" => NoteType::Checklist,
        _ => NoteType::Text,
    }
}

/// Strip HTML tags and truncate to produce a plain-text preview.
/// Uses character counting (not byte length) to safely truncate multi-byte content.
fn make_preview(content: &str) -> String {
    let mut result = String::with_capacity(content.len().min(MAX_PREVIEW_LENGTH * 4));
    let mut in_tag = false;
    let mut char_count = 0usize;
    for ch in content.chars() {
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if !in_tag {
            result.push(ch);
            char_count += 1;
            if char_count >= MAX_PREVIEW_LENGTH {
                break;
            }
        }
    }
    result
}

/// Generate a random 32-character key from the base64 charset (Filen v2 format).
fn generate_note_key() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

// ─── Core implementation ───

impl FilenProvider {
    // ── Note key management ──

    /// Decrypt the per-note key from the note's `metadata` field.
    /// The metadata is `encrypt(JSON({"key":"..."}), masterKey)`.
    fn decrypt_note_key(&self, metadata: &str) -> Option<String> {
        let decrypted = self.decrypt_metadata(metadata)?;
        let parsed: serde_json::Value = serde_json::from_str(&decrypted).ok()?;
        parsed.get("key").and_then(|v| v.as_str()).map(String::from)
    }

    /// Resolve the note key for a given note, checking cache first,
    /// then trying the owner's metadata, then participant metadata.
    fn resolve_note_key(&self, note: &RawNote) -> Option<String> {
        // Check cache
        if let Some(cached) = get_cached_note_key(&note.uuid) {
            return Some(cached);
        }

        // Try owner metadata (encrypted with master key)
        if let Some(key) = self.decrypt_note_key(&note.metadata) {
            cache_note_key(&note.uuid, &key);
            return Some(key);
        }

        // Try participant metadata (also encrypted with master key for us)
        for p in &note.participants {
            if let Some(key) = self.decrypt_note_key(&p.metadata) {
                cache_note_key(&note.uuid, &key);
                return Some(key);
            }
        }

        None
    }

    /// Decrypt a field (title/content/preview) using a per-note key.
    /// The encrypted value is `encrypt_metadata_with_key(JSON({"field":"..."}), noteKey)`.
    fn decrypt_note_field(encrypted: &str, note_key: &str, field: &str) -> Option<String> {
        let decrypted = Self::try_decrypt_aes_gcm(encrypted, note_key)?;
        let parsed: serde_json::Value = serde_json::from_str(&decrypted).ok()?;
        parsed.get(field).and_then(|v| v.as_str()).map(String::from)
    }

    /// Encrypt a field (title/content/preview) with a per-note key.
    fn encrypt_note_field(
        value: &str,
        note_key: &str,
        field: &str,
    ) -> Result<String, ProviderError> {
        let json = serde_json::json!({ field: value }).to_string();
        Self::encrypt_metadata_with_key(&json, note_key)
    }

    /// Build an authenticated GET request to the Filen gateway.
    fn notes_get(&self, endpoint: &str) -> reqwest::RequestBuilder {
        use secrecy::ExposeSecret;
        self.client.get(format!("{}{}", GATEWAY, endpoint)).header(
            "Authorization",
            format!("Bearer {}", self.api_key.expose_secret()),
        )
    }

    /// Build an authenticated POST request to the Filen gateway.
    fn notes_post(&self, endpoint: &str) -> reqwest::RequestBuilder {
        use secrecy::ExposeSecret;
        self.client.post(format!("{}{}", GATEWAY, endpoint)).header(
            "Authorization",
            format!("Bearer {}", self.api_key.expose_secret()),
        )
    }

    /// Send a notes POST request with retry and parse the response.
    async fn notes_request<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
    ) -> Result<T, ProviderError> {
        let request = self
            .notes_post(endpoint)
            .json(body)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp = self.send_retry(request).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    /// Send a notes GET request with retry and parse the response.
    async fn notes_get_request<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
    ) -> Result<T, ProviderError> {
        let request = self
            .notes_get(endpoint)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp = self.send_retry(request).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    // ── Notes CRUD ──

    /// List all notes (decrypted titles and previews).
    pub async fn list_notes(&self) -> Result<Vec<FilenNote>, ProviderError> {
        let api_resp: NotesListResponse = self.notes_get_request("/v3/notes").await?;

        if !api_resp.status {
            return Err(ProviderError::Other(
                api_resp
                    .message
                    .unwrap_or_else(|| "Failed to list notes".into()),
            ));
        }

        let raw_notes = api_resp.data.unwrap_or_default();
        let mut notes = Vec::with_capacity(raw_notes.len());

        for raw in &raw_notes {
            let note_key = match self.resolve_note_key(raw) {
                Some(k) => k,
                None => {
                    debug!(target: "filen_notes", "Skipping note {}: cannot decrypt key", raw.uuid);
                    continue;
                }
            };

            let title =
                Self::decrypt_note_field(&raw.title, &note_key, "title").unwrap_or_default();
            let preview =
                Self::decrypt_note_field(&raw.preview, &note_key, "preview").unwrap_or_default();

            notes.push(FilenNote {
                uuid: raw.uuid.clone(),
                title,
                preview,
                note_type: parse_note_type(&raw.note_type),
                favorite: raw.favorite,
                pinned: raw.pinned,
                trash: raw.trash,
                archive: raw.archive,
                created_timestamp: raw.created_timestamp,
                edited_timestamp: raw.edited_timestamp,
                tags: raw
                    .tags
                    .iter()
                    .map(|t| FilenNoteTagRef {
                        uuid: t.uuid.clone(),
                    })
                    .collect(),
                participants: raw
                    .participants
                    .iter()
                    .map(|p| FilenNoteParticipant {
                        user_id: p.user_id,
                        is_owner: p.is_owner,
                        email: p.email.clone(),
                        permissions_write: p.permissions_write,
                    })
                    .collect(),
            });
        }

        Ok(notes)
    }

    /// Create a new encrypted note. Returns the note UUID.
    pub async fn create_note(
        &self,
        title: &str,
        content: &str,
        note_type: &NoteType,
    ) -> Result<String, ProviderError> {
        let uuid = uuid::Uuid::new_v4().to_string();
        let note_key = generate_note_key();

        // Encrypt the note key with master key
        let key_json = serde_json::json!({ "key": note_key }).to_string();
        let encrypted_metadata = self.encrypt_metadata(&key_json)?;

        // Encrypt title with note key
        let encrypted_title = Self::encrypt_note_field(title, &note_key, "title")?;

        // Create the note
        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/create",
                &serde_json::json!({
                    "uuid": uuid,
                    "title": encrypted_title,
                    "metadata": encrypted_metadata,
                }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to create note".into()),
            ));
        }

        // Cache the key
        cache_note_key(&uuid, &note_key);

        // Add creator as participant (required for the note to appear in the list).
        // Without this step, the Filen API does not return the note in GET /v3/notes.
        // The participant metadata is the note key encrypted with the master key.
        // Best-effort: try to add creator as participant.
        // The server may auto-assign ownership on create; if not, participant/add
        // registers the creator so the note appears in GET /v3/notes.
        if !self.user_uuid.is_empty() {
            match self
                .notes_request::<GenericNoteResponse>(
                    "/v3/notes/participants/add",
                    &serde_json::json!({
                        "uuid": uuid,
                        "contactUUID": self.user_uuid,
                        "metadata": encrypted_metadata,
                        "permissionsWrite": true,
                    }),
                )
                .await
            {
                Ok(resp) if !resp.status => {
                    debug!(
                        target: "filen_notes",
                        "participants/add returned false: {} (non-fatal)",
                        resp.message.unwrap_or_default()
                    );
                }
                Err(e) => {
                    debug!(target: "filen_notes", "participants/add error: {} (non-fatal)", e);
                }
                _ => {}
            }
        }

        // If content is not empty, write it
        if !content.is_empty() {
            self.edit_note_content(&uuid, content, note_type).await?;
        }

        Ok(uuid)
    }

    /// Fetch and decrypt note content.
    pub async fn get_note_content(&self, uuid: &str) -> Result<FilenNoteContent, ProviderError> {
        let note_key = self.get_or_fetch_note_key(uuid).await?;

        let resp: NoteContentResponse = self
            .notes_request("/v3/notes/content", &serde_json::json!({ "uuid": uuid }))
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to get note content".into()),
            ));
        }

        let raw = resp
            .data
            .ok_or_else(|| ProviderError::Other("Note content response missing data".into()))?;

        let content =
            Self::decrypt_note_field(&raw.content, &note_key, "content").unwrap_or_default();
        let preview =
            Self::decrypt_note_field(&raw.preview, &note_key, "preview").unwrap_or_default();

        Ok(FilenNoteContent {
            content,
            preview,
            note_type: parse_note_type(&raw.note_type),
            edited_timestamp: raw.edited_timestamp,
            editor_id: raw.editor_id,
        })
    }

    /// Edit (replace) note content.
    pub async fn edit_note_content(
        &self,
        uuid: &str,
        content: &str,
        note_type: &NoteType,
    ) -> Result<(), ProviderError> {
        let note_key = self.get_or_fetch_note_key(uuid).await?;

        let encrypted_content = Self::encrypt_note_field(content, &note_key, "content")?;

        // Validate ciphertext size
        if encrypted_content.len() > MAX_NOTE_CIPHERTEXT_SIZE {
            return Err(ProviderError::Other(format!(
                "Note content too large ({} bytes, max {})",
                encrypted_content.len(),
                MAX_NOTE_CIPHERTEXT_SIZE
            )));
        }

        let preview_text = make_preview(content);
        let encrypted_preview = Self::encrypt_note_field(&preview_text, &note_key, "preview")?;

        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/content/edit",
                &serde_json::json!({
                    "uuid": uuid,
                    "content": encrypted_content,
                    "preview": encrypted_preview,
                    "type": note_type.to_string(),
                }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to edit note content".into()),
            ));
        }

        Ok(())
    }

    /// Edit note title.
    pub async fn edit_note_title(&self, uuid: &str, title: &str) -> Result<(), ProviderError> {
        let note_key = self.get_or_fetch_note_key(uuid).await?;
        let encrypted_title = Self::encrypt_note_field(title, &note_key, "title")?;

        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/title/edit",
                &serde_json::json!({
                    "uuid": uuid,
                    "title": encrypted_title,
                }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to edit note title".into()),
            ));
        }

        Ok(())
    }

    /// Change note type (e.g., text → markdown), re-encrypting content.
    pub async fn change_note_type(
        &self,
        uuid: &str,
        new_type: &NoteType,
    ) -> Result<(), ProviderError> {
        let note_key = self.get_or_fetch_note_key(uuid).await?;

        // Fetch current content to re-encrypt with the type change
        let current = self.get_note_content(uuid).await?;

        let encrypted_content = Self::encrypt_note_field(&current.content, &note_key, "content")?;
        let encrypted_preview = Self::encrypt_note_field(&current.preview, &note_key, "preview")?;

        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/type/change",
                &serde_json::json!({
                    "uuid": uuid,
                    "type": new_type.to_string(),
                    "content": encrypted_content,
                    "preview": encrypted_preview,
                }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to change note type".into()),
            ));
        }

        Ok(())
    }

    /// Move note to trash.
    pub async fn trash_note(&self, uuid: &str) -> Result<(), ProviderError> {
        self.note_simple_action("/v3/notes/trash", uuid).await
    }

    /// Archive a note.
    pub async fn archive_note(&self, uuid: &str) -> Result<(), ProviderError> {
        self.note_simple_action("/v3/notes/archive", uuid).await
    }

    /// Restore a note from trash or archive.
    pub async fn restore_note(&self, uuid: &str) -> Result<(), ProviderError> {
        self.note_simple_action("/v3/notes/restore", uuid).await
    }

    /// Permanently delete a note.
    pub async fn delete_note(&self, uuid: &str) -> Result<(), ProviderError> {
        self.note_simple_action("/v3/notes/delete", uuid).await
    }

    /// Toggle note favorite.
    pub async fn toggle_note_favorite(
        &self,
        uuid: &str,
        favorite: bool,
    ) -> Result<(), ProviderError> {
        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/favorite",
                &serde_json::json!({ "uuid": uuid, "favorite": favorite }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to toggle favorite".into()),
            ));
        }
        Ok(())
    }

    /// Toggle note pinned.
    pub async fn toggle_note_pinned(&self, uuid: &str, pinned: bool) -> Result<(), ProviderError> {
        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/pinned",
                &serde_json::json!({ "uuid": uuid, "pinned": pinned }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to toggle pinned".into()),
            ));
        }
        Ok(())
    }

    // ── Note history ──

    /// Get version history for a note.
    pub async fn get_note_history(
        &self,
        uuid: &str,
    ) -> Result<Vec<FilenNoteHistoryEntry>, ProviderError> {
        let note_key = self.get_or_fetch_note_key(uuid).await?;

        let resp: NoteHistoryResponse = self
            .notes_request("/v3/notes/history", &serde_json::json!({ "uuid": uuid }))
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to get note history".into()),
            ));
        }

        let raw_entries = resp.data.unwrap_or_default();
        let mut entries = Vec::with_capacity(raw_entries.len());

        for raw in &raw_entries {
            let content =
                Self::decrypt_note_field(&raw.content, &note_key, "content").unwrap_or_default();
            let preview =
                Self::decrypt_note_field(&raw.preview, &note_key, "preview").unwrap_or_default();

            entries.push(FilenNoteHistoryEntry {
                id: raw.id,
                content,
                preview,
                note_type: parse_note_type(&raw.note_type),
                edited_timestamp: raw.edited_timestamp,
                editor_id: raw.editor_id,
            });
        }

        Ok(entries)
    }

    /// Restore a specific history version.
    pub async fn restore_note_history(
        &self,
        uuid: &str,
        history_id: u64,
    ) -> Result<(), ProviderError> {
        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/history/restore",
                &serde_json::json!({ "uuid": uuid, "id": history_id }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to restore note history".into()),
            ));
        }
        Ok(())
    }

    // ── Tags ──

    /// List all note tags (decrypted names).
    pub async fn list_note_tags(&self) -> Result<Vec<FilenNoteTag>, ProviderError> {
        let api_resp: TagsListResponse = self.notes_get_request("/v3/notes/tags").await?;

        if !api_resp.status {
            return Err(ProviderError::Other(
                api_resp
                    .message
                    .unwrap_or_else(|| "Failed to list tags".into()),
            ));
        }

        let raw_tags = api_resp.data.unwrap_or_default();
        let mut tags = Vec::with_capacity(raw_tags.len());

        for raw in &raw_tags {
            let name = self.decrypt_tag_name(&raw.name).unwrap_or_default();
            if name.is_empty() {
                debug!(target: "filen_notes", "Skipping tag {}: cannot decrypt name", raw.uuid);
                continue;
            }
            tags.push(FilenNoteTag {
                uuid: raw.uuid.clone(),
                name,
                favorite: raw.favorite,
                created_timestamp: raw.created_timestamp,
                edited_timestamp: raw.edited_timestamp,
            });
        }

        Ok(tags)
    }

    /// Create a new tag. Returns the tag UUID.
    pub async fn create_note_tag(&self, name: &str) -> Result<String, ProviderError> {
        let encrypted_name = self.encrypt_tag_name(name)?;

        let resp: CreateTagResponse = self
            .notes_request(
                "/v3/notes/tags/create",
                &serde_json::json!({ "name": encrypted_name }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to create tag".into()),
            ));
        }

        resp.data
            .map(|d| d.uuid)
            .ok_or_else(|| ProviderError::Other("Create tag response missing UUID".into()))
    }

    /// Rename an existing tag.
    pub async fn rename_note_tag(&self, tag_uuid: &str, name: &str) -> Result<(), ProviderError> {
        let encrypted_name = self.encrypt_tag_name(name)?;

        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/tags/rename",
                &serde_json::json!({ "uuid": tag_uuid, "name": encrypted_name }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to rename tag".into()),
            ));
        }
        Ok(())
    }

    /// Delete a tag.
    pub async fn delete_note_tag(&self, tag_uuid: &str) -> Result<(), ProviderError> {
        self.note_simple_action("/v3/notes/tags/delete", tag_uuid)
            .await
    }

    /// Assign a tag to a note.
    pub async fn tag_note(&self, note_uuid: &str, tag_uuid: &str) -> Result<(), ProviderError> {
        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/tag",
                &serde_json::json!({ "uuid": note_uuid, "tag": tag_uuid }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message.unwrap_or_else(|| "Failed to tag note".into()),
            ));
        }
        Ok(())
    }

    /// Remove a tag from a note.
    pub async fn untag_note(&self, note_uuid: &str, tag_uuid: &str) -> Result<(), ProviderError> {
        let resp: GenericNoteResponse = self
            .notes_request(
                "/v3/notes/untag",
                &serde_json::json!({ "uuid": note_uuid, "tag": tag_uuid }),
            )
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to untag note".into()),
            ));
        }
        Ok(())
    }

    // ── Private helpers ──

    /// Decrypt a tag name (encrypted with master key).
    fn decrypt_tag_name(&self, encrypted: &str) -> Option<String> {
        let decrypted = self.decrypt_metadata(encrypted)?;
        let parsed: serde_json::Value = serde_json::from_str(&decrypted).ok()?;
        parsed
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Encrypt a tag name with master key.
    fn encrypt_tag_name(&self, name: &str) -> Result<String, ProviderError> {
        let json = serde_json::json!({ "name": name }).to_string();
        self.encrypt_metadata(&json)
    }

    /// Send a simple action (trash, archive, restore, delete) that only takes a UUID.
    async fn note_simple_action(&self, endpoint: &str, uuid: &str) -> Result<(), ProviderError> {
        let resp: GenericNoteResponse = self
            .notes_request(endpoint, &serde_json::json!({ "uuid": uuid }))
            .await?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| format!("Action failed: {}", endpoint)),
            ));
        }
        Ok(())
    }

    /// Get the note key, checking cache first, then fetching the notes list.
    async fn get_or_fetch_note_key(&self, uuid: &str) -> Result<String, ProviderError> {
        if let Some(key) = get_cached_note_key(uuid) {
            return Ok(key);
        }

        // Fetch notes list to populate key cache
        let api_resp: NotesListResponse = self.notes_get_request("/v3/notes").await?;

        if let Some(notes) = api_resp.data {
            for note in &notes {
                if let Some(key) = self.resolve_note_key(note) {
                    if note.uuid == uuid {
                        return Ok(key);
                    }
                }
            }
        }

        Err(ProviderError::Other(format!(
            "Cannot decrypt note key for {}",
            uuid
        )))
    }
}
