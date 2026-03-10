//! CredentialProvider trait — abstracts credential/vault access
//!
//! Tauri implementation reads from vault.db (AES-256-GCM + Argon2id).
//! CLI implementation reads from vault cache (if open) or env vars
//! (AEROFTP_HOST / AEROFTP_USER / AEROFTP_PASS).

/// Server profile metadata (no secrets).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// Server credentials (secrets).
#[derive(Clone)]
pub struct ServerCredentials {
    pub server: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ServerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerCredentials")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Extra provider-specific options (region, bucket, endpoint, etc.)
pub type ProviderExtraOptions = std::collections::HashMap<String, String>;

/// Abstraction over credential storage.
pub trait CredentialProvider: Send + Sync {
    /// List all saved server profiles (no passwords).
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String>;

    /// Load credentials for a specific server (by ID or fuzzy name match).
    fn get_credentials(&self, server_id: &str) -> Result<ServerCredentials, String>;

    /// Load provider-specific extra options for a server.
    fn get_extra_options(&self, server_id: &str) -> Result<ProviderExtraOptions, String>;
}
