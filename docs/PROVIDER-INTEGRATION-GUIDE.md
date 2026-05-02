# AeroFTP Provider Integration Guide

> A comprehensive technical reference for implementing cloud storage providers in Rust using AeroFTP's `StorageProvider` trait architecture. Written for developers and AI agents integrating new protocols.

> **For storage providers and integrators**: this is the only public reference of its kind in the file-client space — a complete blueprint that lets a new cloud or self-hosted storage service ship a first-class native integration in AeroFTP without reverse-engineering the codebase. If you run a storage service and want a dedicated provider entry (instead of a generic preset), this guide is the contract. We're already collaborating with one provider on a native integration using exactly this document; we welcome more. Reach out via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues) and we'll review the API together.

**Version**: 3.7
**Last Updated**: 2026-05-02
**Codebase**: `src-tauri/src/providers/`

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [StorageProvider Trait](#2-storageprovider-trait)
3. [Authentication Patterns](#3-authentication-patterns)
   - 3.1 [OAuth2 PKCE](#31-oauth2-pkce)
   - 3.2 [OAuth 1.0 (HMAC-SHA1)](#32-oauth-10-hmac-sha1)
   - 3.3 [AWS Signature Version 4](#33-aws-signature-version-4)
   - 3.4 [Azure Shared Key](#34-azure-shared-key)
   - 3.5 [API Key / Bearer Token](#35-api-key--bearer-token)
   - 3.6 [HTTP Digest Authentication](#36-http-digest-authentication)
    - 3.7 [E2E Encrypted Providers](#37-e2e-encrypted-providers)
    - 3.8 [Session-Based Username/Password](#38-session-based-usernamepassword)
4. [Upload Patterns](#4-upload-patterns)
   - 4.1 [Simple Streaming Upload](#41-simple-streaming-upload)
   - 4.2 [Chunked Upload Session](#42-chunked-upload-session)
   - 4.3 [Resumable Upload](#43-resumable-upload)
   - 4.4 [Multipart Upload (S3)](#44-multipart-upload-s3)
5. [Download Patterns](#5-download-patterns)
6. [Pagination](#6-pagination)
7. [XML Parsing with quick-xml](#7-xml-parsing-with-quick-xml)
8. [Error Handling](#8-error-handling)
9. [HTTP Retry with Backoff](#9-http-retry-with-backoff)
10. [Credential Security](#10-credential-security)
11. [Transfer Optimization](#11-transfer-optimization)
12. [Provider Capability Matrix](#12-provider-capability-matrix)
13. [Step-by-Step: Adding a New Provider](#13-step-by-step-adding-a-new-provider)
14. [Testing & Audit Methodology](#14-testing--audit-methodology)
15. [Dependencies](#15-dependencies)
16. [Lessons Learned](#16-lessons-learned)

---

## 1. Architecture Overview

AeroFTP uses a trait-based abstraction layer that decouples protocol-specific logic from the application. All 25 storage backends implement the same `StorageProvider` trait, enabling uniform file operations across FTP, SFTP, WebDAV, S3, and 19 cloud APIs.

```
┌──────────────────────────────────────────────────────────────────────┐
│                    StorageProvider Trait (async)                      │
│  connect · list · upload · download · mkdir · delete · rename · ...   │
└──────────────────────────────────────────────────────────────────────┘
                                  │
     ┌──────┬───────┬──────┬──────┼──────┬────────┬─────────┬─────────┐
     ▼      ▼       ▼      ▼      ▼      ▼        ▼         ▼         ▼
  ┌─────┐┌──────┐┌──────┐┌────┐┌──────┐┌───────┐┌────────┐┌────────┐┌─────┐
  │ FTP ││ SFTP ││WebDAV││ S3 ││Google││Dropbox││OneDrive││  MEGA  ││ ... │
  └─────┘└──────┘└──────┘└────┘└──────┘└───────┘└────────┘└────────┘└─────┘
     │       │       │      │      │       │        │         │         │
  suppaftp  russh  reqwest reqwest  reqwest+OAuth2    ...     E2E crypto
```

### File Layout

```
src-tauri/src/providers/
├── mod.rs                  # StorageProvider trait + ProviderFactory
├── types.rs                # ProviderType enum, config structs, RemoteEntry, ProviderError
├── http_retry.rs           # HTTP retry with exponential backoff + jitter
├── oauth2.rs               # OAuth2 PKCE flow (8 providers)
├── oauth1.rs               # OAuth 1.0 HMAC-SHA1 (4shared)
├── ftp.rs                  # FTP/FTPS via suppaftp
├── sftp.rs                 # SFTP: russh (connection/listing/download) + ssh2/SCP (upload)
├── webdav.rs               # WebDAV with HTTP Digest/Basic auth
├── s3.rs                   # S3 + AWS SigV4 signing
├── google_drive.rs         # Google Drive API v3
├── dropbox.rs              # Dropbox API v2
├── onedrive.rs             # Microsoft Graph API
├── mega.rs                 # MEGA.nz (E2E encrypted)
├── box_provider.rs         # Box API v2
├── pcloud.rs               # pCloud API
├── azure.rs                # Azure Blob Storage
├── filen.rs                # Filen.io (E2E encrypted)
├── fourshared.rs           # 4shared (OAuth 1.0)
├── zoho_workdrive.rs       # Zoho WorkDrive API
├── internxt.rs             # Internxt Drive (E2E encrypted)
├── kdrive.rs               # Infomaniak kDrive
├── jottacloud.rs           # Jottacloud REST API
├── filelu.rs               # FileLu API
├── koofr.rs                # Koofr REST API
├── drime_cloud.rs          # Drime Cloud API
├── opendrive.rs            # OpenDrive REST API
├── yandex_disk.rs          # Yandex Disk REST API
├── immich.rs               # Immich photo management API
└── google_photos.rs        # Google Photos (standby - scope removed)
```

### Provider Registry

| Provider | File | Lines | Auth Method | Protocol |
|----------|------|-------|-------------|----------|
| FTP/FTPS | `ftp.rs` | ~1,050 | User/Pass + TLS | TCP socket |
| SFTP | `sftp.rs` | ~1,200 | User/Pass/Key | SSH (hybrid: russh + ssh2/SCP) |
| WebDAV | `webdav.rs` | ~1,450 | HTTP Basic/Digest | HTTPS |
| S3 | `s3.rs` | ~2,200 | AWS SigV4 | HTTPS |
| Google Drive | `google_drive.rs` | ~2,050 | OAuth2 PKCE | REST |
| Dropbox | `dropbox.rs` | ~1,500 | OAuth2 PKCE | REST |
| OneDrive | `onedrive.rs` | ~1,400 | OAuth2 PKCE | REST (Graph) |
| MEGA | `mega.rs` | ~900 | E2E (AES-128) | REST |
| Box | `box_provider.rs` | ~1,700 | OAuth2 PKCE | REST |
| pCloud | `pcloud.rs` | ~1,050 | OAuth2 PKCE | REST |
| Azure Blob | `azure.rs` | ~1,150 | Shared Key / SAS | REST |
| Filen | `filen.rs` | ~1,600 | E2E (AES-256-GCM) | REST |
| 4shared | `fourshared.rs` | ~1,350 | OAuth 1.0 (HMAC-SHA1) | REST |
| Zoho WorkDrive | `zoho_workdrive.rs` | ~2,100 | OAuth2 PKCE | REST |
| Internxt | `internxt.rs` | ~2,150 | E2E (XChaCha20) | REST |
| kDrive | `kdrive.rs` | ~1,300 | Bearer Token | REST |
| Jottacloud | `jottacloud.rs` | ~1,650 | Login Token | REST (XML) |
| FileLu | `filelu.rs` | ~1,500 | API Key | REST |
| Koofr | `koofr.rs` | ~1,750 | OAuth2 PKCE | REST |
| Drime Cloud | `drime_cloud.rs` | ~1,600 | Bearer Token | REST |
| OpenDrive | `opendrive.rs` | ~1,211 | Session login (user/pass) | REST |
| Yandex Disk | `yandex_disk.rs` | ~1,237 | OAuth2 token (`Authorization: OAuth`) | REST |
| Immich | `immich.rs` | ~1,427 | API Key (`x-api-key`) | REST |
| Google Photos | `google_photos.rs` | ~870 | OAuth2 PKCE | REST (standby) |

---

## 2. StorageProvider Trait

The unified trait defined in `mod.rs` provides **20 required methods** and **30+ optional methods** with default implementations that return `ProviderError::NotSupported`.

### Required Methods

```rust
#[async_trait]
pub trait StorageProvider: Send + Sync {
    // Identity
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn provider_type(&self) -> ProviderType;
    fn display_name(&self) -> String;

    // Lifecycle
    async fn connect(&mut self) -> Result<(), ProviderError>;
    async fn disconnect(&mut self) -> Result<(), ProviderError>;
    fn is_connected(&self) -> bool;

    // Navigation
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError>;
    async fn pwd(&mut self) -> Result<String, ProviderError>;
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError>;
    async fn cd_up(&mut self) -> Result<(), ProviderError>;

    // File Operations
    async fn download(&mut self, remote: &str, local: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError>;
    async fn download_to_bytes(&mut self, remote: &str) -> Result<Vec<u8>, ProviderError>;
    async fn upload(&mut self, local: &str, remote: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError>;
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError>;
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError>;

    // Directory Operations
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError>;
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError>;
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError>;

    // Maintenance
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError>;
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError>;
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError>;
    async fn keep_alive(&mut self) -> Result<(), ProviderError>;
    async fn server_info(&mut self) -> Result<String, ProviderError>;
}
```

### Optional Capabilities

Providers opt-in to additional features by overriding paired `supports_*()` + method:

| Capability | Check Method | Action Method | Providers |
|-----------|-------------|---------------|-----------|
| File permissions | `supports_chmod()` | `chmod()` | SFTP |
| Symlinks | `supports_symlinks()` | - | SFTP |
| Server-side copy | `supports_server_copy()` | `server_copy()` | S3 |
| Share links | `supports_share_links()` | `create_share_link()` | Google, Dropbox, OneDrive, Box, Zoho |
| Storage quota | - | `storage_info()` | Google, Dropbox, OneDrive, Box, pCloud, Zoho |
| Resume transfer | `supports_resume()` | `resume_download()` / `resume_upload()` | FTP, SFTP, Koofr |
| File versions | `supports_versions()` | `list_versions()` / `download_version()` | Google, OneDrive, Box, Zoho |
| File locking | `supports_locking()` | `lock_file()` / `unlock_file()` | WebDAV |
| Thumbnails | `supports_thumbnails()` | `get_thumbnail()` | Google, Dropbox, OneDrive, Box |
| Permissions | `supports_permissions()` | `list_permissions()` / `add_permission()` | Google, Box |
| Checksums | `supports_checksum()` | `checksum()` | S3 |
| Remote URL upload | `supports_remote_upload()` | `remote_upload()` | FileLu |
| Change tracking | `supports_change_tracking()` | `get_change_token()` / `list_changes()` | Google |
| Delta sync | `supports_delta_sync()` | `read_range()` | SFTP |
| Speed limits | - | `set_speed_limit()` / `get_speed_limit()` | FTP |

### RemoteEntry

The universal file/directory representation:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,     // ISO 8601 or provider-specific
    pub permissions: Option<String>,  // Unix-style "rwxr-xr-x"
    pub owner: Option<String>,
    pub group: Option<String>,
    pub is_symlink: Option<bool>,
    pub symlink_target: Option<String>,
}
```

### ProviderError

```rust
#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Not connected to server")]
    NotConnected,
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),
    #[error("Path not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Path already exists: {0}")]
    AlreadyExists(String),
    #[error("Transfer failed: {0}")]
    TransferFailed(String),
    #[error("Operation not supported: {0}")]
    NotSupported(String),
    #[error("Timeout")]
    Timeout,
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Other error: {0}")]
    Other(String),
}

impl ProviderError {
    pub fn is_recoverable(&self) -> bool {
        matches!(self,
            ProviderError::Timeout
            | ProviderError::NetworkError(_)
            | ProviderError::NotConnected
        )
    }
}
```

### ProviderFactory

The factory dispatches configuration to concrete provider constructors:

```rust
pub struct ProviderFactory;

impl ProviderFactory {
    pub fn create(config: &ProviderConfig) -> Result<Box<dyn StorageProvider>, ProviderError> {
        match config.provider_type {
            ProviderType::Ftp | ProviderType::Ftps => {
                let ftp_config = FtpConfig::from_provider_config(config)?;
                Ok(Box::new(FtpProvider::new(ftp_config)))
            }
            ProviderType::S3 => {
                let s3_config = S3Config::from_provider_config(config)?;
                Ok(Box::new(S3Provider::new(s3_config)?))
            }
            // OAuth2 providers use a separate flow:
            ProviderType::GoogleDrive | ProviderType::Dropbox | ... => {
                Err(ProviderError::NotSupported(
                    "Use oauth2_start_auth + oauth2_connect commands".into()
                ))
            }
            // ... all 22 variants handled
        }
    }
}
```

---

## 3. Authentication Patterns

### 3.1 OAuth2 PKCE

**File**: `oauth2.rs` (~1,050 lines)
**Providers**: Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, Koofr, Yandex Disk
**Crate**: `oauth2 = "5"` with PKCE S256

#### Flow

```
┌────────────┐    1. start_auth_flow()     ┌──────────────┐
│   AeroFTP   │─────────────────────────────►│  OAuth2Mgr   │
│  Frontend   │    Returns: (auth_url,       │              │
│             │             state_token)     │  Stores PKCE │
│             │◄─────────────────────────────│  verifier    │
│             │                              └──────┬───────┘
│             │    2. Open browser                   │
│             │─────────────────────────────►        │
│             │                                      │
│             │    3. User authorizes                 │
│             │                                      │
│             │    4. complete_auth_flow()            │
│             │       (code, state)                   │
│             │─────────────────────────────►┌───────┴──────┐
│             │                              │  Exchange    │
│             │    5. StoredTokens           │  code→token  │
│             │◄─────────────────────────────│  Store vault │
└────────────┘                              └──────────────┘
```

#### PKCE Challenge Generation

```rust
let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

let (auth_url, csrf_token) = client
    .authorize_url(CsrfToken::new_random)
    .set_pkce_challenge(pkce_challenge)
    .add_scope(Scope::new("https://www.googleapis.com/auth/drive".into()))
    .add_extra_param("access_type", "offline")
    .url();

// Store verifier keyed by CSRF state token
pending_verifiers.insert(csrf_token.secret().clone(), pkce_verifier);
```

#### Token Exchange

```rust
let token_result = client
    .exchange_code(AuthorizationCode::new(code.to_string()))
    .set_pkce_verifier(verifier)
    .request_async(&OAuth2HttpClient)
    .await?;

let tokens = StoredTokens {
    access_token: token_result.access_token().secret().clone(),
    refresh_token: token_result.refresh_token().map(|t| t.secret().clone()),
    expires_at: token_result.expires_in().map(|d| Utc::now().timestamp() + d.as_secs() as i64),
    token_type: "Bearer".to_string(),
    scopes: config.scopes.clone(),
};
```

#### Token Refresh with Race Protection

A `tokio::sync::Mutex` guard prevents concurrent refresh calls that would invalidate the first token (H-04 finding):

```rust
pub async fn get_valid_token(&self, config: &OAuthConfig) -> Result<SecretString, ProviderError> {
    let _guard = self.refresh_guard.lock().await;
    let mut tokens = self.load_tokens(config.provider)?;

    if tokens.is_expired() {
        if let Some(ref refresh_token) = tokens.refresh_token {
            tokens = self.refresh_tokens(config, refresh_token).await?;
        } else {
            return Err(ProviderError::AuthenticationFailed(
                "Token expired and no refresh token available".into()
            ));
        }
    }

    Ok(SecretString::from(tokens.access_token))
}
```

#### Token Expiry Check (5-minute buffer)

```rust
pub fn is_expired(&self) -> bool {
    if let Some(expires_at) = self.expires_at {
        expires_at <= chrono::Utc::now().timestamp() + 300
    } else {
        false // No expiry = assume valid
    }
}
```

#### Provider-Specific OAuth2 Configurations

| Provider | Auth URL | Token URL | Redirect | Notes |
|----------|----------|-----------|----------|-------|
| Google | `accounts.google.com/o/oauth2/v2/auth` | `oauth2.googleapis.com/token` | `http://127.0.0.1:{port}/callback` | `access_type=offline` |
| Dropbox | `www.dropbox.com/oauth2/authorize` | `api.dropboxapi.com/oauth2/token` | `http://127.0.0.1:{port}/callback` | `token_access_type=offline` |
| OneDrive | `login.microsoftonline.com/common/oauth2/v2.0/authorize` | `login.microsoftonline.com/common/oauth2/v2.0/token` | `http://localhost:{port}/callback` | **Must use `localhost`** (Entra ID) |
| Box | `account.box.com/api/oauth2/authorize` | `api.box.com/oauth2/token` | `http://127.0.0.1:{port}/callback` | No scopes needed |
| pCloud | `my.pcloud.com/oauth2/authorize` | `api.pcloud.com/oauth2_token` | `http://localhost:{port}/callback` | EU: `eapi.pcloud.com` |
| Zoho | `accounts.zoho.{tld}/oauth/v2/auth` | `accounts.zoho.{tld}/oauth/v2/token` | `http://127.0.0.1:{port}/callback` | 9 regional TLDs, 8 scopes, `prompt=consent` |
| Koofr | `app.koofr.net/oauth2/authorize` | `app.koofr.net/oauth2/token` | `http://127.0.0.1:{port}/callback` | No scopes needed |
| Yandex Disk | `oauth.yandex.com/authorize` | `oauth.yandex.com/token` | `http://127.0.0.1:{port}/callback` | API calls use `Authorization: OAuth {token}` rather than `Bearer` |

#### Callback Server

The redirect URI uses a dynamic OS-assigned port on loopback:

```rust
// Port 0 = OS assigns next available port
let listener = TcpListener::bind("127.0.0.1:0").await?;
let port = listener.local_addr()?.port();
let redirect_uri = format!("http://127.0.0.1:{}/callback", port);
```

> **OneDrive exception**: Microsoft Entra ID requires `http://localhost` (not `127.0.0.1`). This is the only provider with this constraint.

#### Token Storage Hierarchy

Tokens are stored with a 3-tier fallback:

1. **Encrypted vault** (`credential_store.rs`): AES-256-GCM + Argon2id in `vault.db`
2. **Auto-initialized vault**: If vault not open, try creating one without master password
3. **In-memory only** (`MEMORY_TOKEN_CACHE`): When vault requires master password - tokens survive the session but are never written to disk

```rust
// Priority 1: Vault
if let Some(store) = CredentialStore::from_cache() {
    store.store(&account, &json)?;
    return Ok(());
}

// Priority 2: Auto-init vault
if CredentialStore::init().is_ok() { ... }

// Priority 3: Memory-only (vault locked)
if let Ok(mut cache) = MEMORY_TOKEN_CACHE.lock() {
    let map = cache.get_or_insert_with(HashMap::new);
    map.insert(account, json);
}
```

#### Legacy Migration

If a plaintext token file from pre-vault versions is found, it is automatically migrated to the vault and the file is deleted:

```rust
let legacy_path = token_dir.join(format!("oauth2_{:?}.json", provider).to_lowercase());
if legacy_path.exists() {
    let json = std::fs::read_to_string(&legacy_path)?;
    // Migrate to vault
    store.store(&account, &json)?;
    // Delete plaintext file
    std::fs::remove_file(&legacy_path).ok();
}
```

### 3.2 OAuth 1.0 (HMAC-SHA1)

**File**: `oauth1.rs` (~320 lines)
**Provider**: 4shared
**Standard**: RFC 5849

#### Signing Process

```rust
pub fn sign(method: &str, url: &str, params: &[(String, String)],
            consumer_secret: &str, token_secret: &str) -> String {
    // 1. Sort parameters alphabetically
    let mut all_params = params.to_vec();
    all_params.sort_by(|a, b| a.0.cmp(&b.0));

    // 2. Build parameter string (percent-encoded key=value pairs)
    let param_string = all_params.iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    // 3. Build signature base string
    let base_string = format!("{}&{}&{}",
        method.to_uppercase(),
        percent_encode(url),
        percent_encode(&param_string),
    );

    // 4. HMAC-SHA1 signature
    let signing_key = format!("{}&{}", percent_encode(consumer_secret), percent_encode(token_secret));
    let mac = Hmac::<Sha1>::new_from_slice(signing_key.as_bytes()).unwrap();
    mac.update(base_string.as_bytes());
    base64::encode(mac.finalize().into_bytes())
}
```

#### 4shared Authentication (3-Legged)

```
1. POST /oauth/request_token        → oauth_token (temporary)
2. Redirect to /oauth/authorize      → User approves → oauth_verifier
3. POST /oauth/access_token          → oauth_token + oauth_token_secret (permanent)
```

> **Key lesson**: 4shared uses ID-based file paths, not string paths. Every file/folder has a numeric ID that must be resolved before operations.

### 3.3 AWS Signature Version 4

**File**: `s3.rs` (lines ~850-900)

```rust
// Canonical Request:
// METHOD\n
// CanonicalURI (URI-encoded path)\n
// CanonicalQueryString (sorted)\n
// CanonicalHeaders (lowercase, trimmed)\n
// \n
// SignedHeaders (semicolon-separated)\n
// HashedPayload (SHA-256 of body, or UNSIGNED-PAYLOAD)

// String To Sign:
// AWS4-HMAC-SHA256\n
// ISO8601 Timestamp\n
// date/region/s3/aws4_request\n
// SHA-256(CanonicalRequest)

// Signing Key derivation:
// kDate    = HMAC-SHA256("AWS4" + secret_key, date)
// kRegion  = HMAC-SHA256(kDate, region)
// kService = HMAC-SHA256(kRegion, "s3")
// kSigning = HMAC-SHA256(kService, "aws4_request")
// Signature = Hex(HMAC-SHA256(kSigning, StringToSign))
```

**Path-style vs Virtual-hosted**:

```rust
// Virtual-hosted (default): bucket.s3.region.amazonaws.com
// Path-style (MinIO, etc.): endpoint/bucket
let use_path_style = config.extra.get("path_style").map_or(false, |v| v == "true");
```

> **Gotcha**: Some S3-compatible providers (MinIO, Ceph) require path-style addressing. The `path_style` config flag controls this.

### 3.4 Azure Shared Key

**File**: `azure.rs` (lines ~114-163)

```rust
// String-to-Sign:
// METHOD\n
// \n (Content-Encoding)
// \n (Content-Language)
// Content-Length\n
// \n (Content-MD5)
// Content-Type\n
// \n (Date)
// \n (If-Modified-Since)
// \n (If-Match)
// \n (If-None-Match)
// \n (If-Unmodified-Since)
// \n (Range)
// x-ms-date:date\n
// x-ms-version:2020-10-02\n
// /account/container/blob\n

let signature = base64::encode(
    hmac_sha256(base64::decode(&access_key)?, &string_to_sign)
);
// Header: Authorization: SharedKey account:signature
```

**Alternative**: SAS (Shared Access Signature) tokens can be appended as query parameters.

### 3.5 API Key / Bearer Token

The simplest pattern - used by providers with key-based or token-based authentication:

```rust
// API Key (FileLu)
let response = self.client.get(&url)
    .header("X-API-Key", self.api_key.expose_secret())
    .send().await?;

// OAuth token header (Yandex Disk)
let response = self.client.get(&url)
    .header(AUTHORIZATION, format!("OAuth {}", self.oauth_token.expose_secret()))
    .send().await?;

// Bearer Token (kDrive, Jottacloud, Drime Cloud)
let response = self.client.get(&url)
    .header(AUTHORIZATION, format!("Bearer {}", self.token.expose_secret()))
    .send().await?;
```

| Provider | Header | Config Field |
|----------|--------|-------------|
| FileLu | `X-API-Key: {key}` | `api_key: SecretString` |
| Yandex Disk | `Authorization: OAuth {token}` | `oauth_token: SecretString` |
| kDrive | `Authorization: Bearer {token}` | `api_token: SecretString` |
| Jottacloud | `Authorization: Bearer {token}` | `login_token: SecretString` |
| Drime Cloud | `Authorization: Bearer {token}` | `api_token: SecretString` |

### 3.6 HTTP Digest Authentication

**File**: `webdav.rs` (lines ~25-114)

WebDAV servers may negotiate HTTP Digest auth (RFC 2617) instead of Basic:

```rust
// Server challenge: WWW-Authenticate: Digest realm="...", nonce="...", qop="auth"
// Client response:
//   HA1 = MD5(username:realm:password)
//   HA2 = MD5(method:uri)
//   response = MD5(HA1:nonce:nc:cnonce:qop:HA2)
//   Authorization: Digest username="...", realm="...", nonce="...",
//                         uri="...", response="...", nc=00000001,
//                         cnonce="...", qop=auth
```

> **Security note**: MD5-based Digest auth is a server-negotiated protocol limitation. When possible, prefer HTTPS with Basic auth (TLS protects the credential).

### 3.7 E2E Encrypted Providers

Three providers implement client-side encryption where the server never sees plaintext:

| Provider | Key Derivation | File Encryption | Metadata Encryption |
|----------|---------------|----------------|---------------------|
| **MEGA** | PBKDF2 → AES-128 master key | AES-128-ECB (per-file key) | AES-128 (file attributes) |
| **Filen** | PBKDF2 → master key | AES-256-GCM (per-file key) | AES-256-GCM (metadata) |
| **Internxt** | - | XChaCha20-Poly1305 | Encrypted JSON metadata |

#### MEGA Pattern

```
1. Login: email + password → PBKDF2 → master key
2. Decrypt user attributes → get master key material
3. List: GET /cs (command sequence) → encrypted node tree
4. Each node: encrypted key + encrypted attributes
5. Decrypt node key with master key → file key
6. Decrypt file attributes with file key → name, size, etc.
7. Download: stream encrypted chunks → decrypt with file key
```

#### Filen Pattern

```
1. Auth: POST /v3/auth/info → salt, derive master key
2. POST /v3/login → auth token + encrypted master keys
3. Decrypt master keys with password → folder/file keys
4. List: POST /v3/dir/content → encrypted metadata per entry
5. Decrypt each metadata JSON with per-file key
6. Download: GET /v3/file/download → decrypt chunks (AES-256-GCM)
```

> **Key lesson**: E2E providers require downloading the entire file to decrypt. No partial reads, no server-side search, no thumbnails. Every operation is fundamentally different from cleartext REST APIs.

### 3.8 Session-Based Username/Password

**File**: `opendrive.rs` (~1,211 lines)
**Provider**: OpenDrive
**Pattern**: Session bootstrap via username/password, then `SessionID` on all subsequent requests

#### Flow

```
1. POST /session/login.json
    body: username, passwd, version, partner_id
    -> returns SessionID

2. Store SessionID in provider state

3. Send session_id on all path lookup, metadata, upload, rename and delete operations

4. Optional: POST /session/logout.json on disconnect
```

#### Notes

- No OAuth app registration or API key is required for the standard OpenDrive personal flow.
- Root folder uses `folder_id = 0`.
- The live API diverges from the PDF in some cases, so robust integrations should prefer defensive fallbacks over assuming endpoint consistency.

---

## 4. Upload Patterns

### 4.1 Simple Streaming Upload

Most REST providers accept a single PUT/POST with a streaming body. The key principle: **never load the entire file into memory**.

```rust
use tokio::fs::File;
use tokio::io::AsyncReadExt;

async fn upload(&mut self, local_path: &str, remote_path: &str,
                on_progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
    let file = File::open(local_path).await
        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
    let file_size = file.metadata().await?.len();

    // Stream the file - reqwest reads chunks on demand
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = reqwest::Body::wrap_stream(stream);

    let token = self.get_valid_token().await?;
    self.client.put(&upload_url)
        .header(AUTHORIZATION, format!("Bearer {}", token.expose_secret()))
        .header(CONTENT_LENGTH, file_size)
        .body(body)
        .send().await?;

    Ok(())
}
```

**Used by**: pCloud, kDrive, FileLu, Drime Cloud, Koofr

### 4.2 Chunked Upload Session

For large files, some providers require a multi-step upload session:

```rust
// Dropbox: 3-step upload session for files > 150MB
const CHUNK_SIZE: usize = 128 * 1024 * 1024; // 128 MB

// Step 1: Start session
let start_resp = client.post("https://content.dropboxapi.com/2/files/upload_session/start")
    .header("Dropbox-API-Arg", "{\"close\":false}")
    .header(CONTENT_TYPE, "application/octet-stream")
    .body(first_chunk)
    .send().await?;
let session_id = start_resp.json::<Value>()["session_id"].as_str().unwrap();

// Step 2: Append chunks
let mut offset = first_chunk.len() as u64;
while offset < file_size {
    let chunk = read_chunk(&mut file, CHUNK_SIZE).await?;
    let arg = json!({
        "cursor": { "session_id": session_id, "offset": offset },
        "close": offset + chunk.len() as u64 >= file_size
    });
    client.post("https://content.dropboxapi.com/2/files/upload_session/append_v2")
        .header("Dropbox-API-Arg", serde_json::to_string(&arg)?)
        .body(chunk)
        .send().await?;
    offset += chunk_len;
}

// Step 3: Finish
let finish_arg = json!({
    "cursor": { "session_id": session_id, "offset": file_size },
    "commit": { "path": remote_path, "mode": "overwrite" }
});
client.post("https://content.dropboxapi.com/2/files/upload_session/finish")
    .header("Dropbox-API-Arg", serde_json::to_string(&finish_arg)?)
    .send().await?;
```

#### OpenDrive 4-step upload session

OpenDrive uses a provider-specific session flow rather than a single PUT:

```rust
// Step 1: Create or open destination file metadata
let created: CreateFileResponse = self.post_form(
    "upload/create_file.json",
    &[
        ("session_id", self.session_id.clone()),
        ("folder_id", folder_id),
        ("file_name", file_name.clone()),
        ("file_size", file_size.to_string()),
        ("file_hash", file_hash.clone()),
        ("open_if_exists", "1".to_string()),
    ],
).await?;

// Step 2: Open upload session and capture TempLocation
let opened: OpenUploadResponse = self.post_form(
    "upload/open_file_upload.json",
    &[
        ("session_id", self.session_id.clone()),
        ("file_id", file_id.clone()),
        ("file_size", file_size.to_string()),
        ("file_hash", file_hash.clone()),
    ],
).await?;

// Step 3: Upload chunk payload to upload_file_chunk2
// Server may request zlib compression via RequireCompression

// Step 4: Finalize with close_file_upload.json
```

> **OpenDrive quirks**: `RequireHashOnly=true` is not sufficient reason to skip chunk upload for non-empty files in practice, and `TempLocation` from `open_file_upload` is more reliable than assuming the value from `create_file`.

### 4.3 Resumable Upload

OneDrive uses a create-session-then-PUT-chunks pattern:

```rust
// Threshold: 4 MB
const RESUMABLE_THRESHOLD: u64 = 4 * 1024 * 1024;
const CHUNK_SIZE: u64 = 320 * 1024; // 320 KB (must be multiple of 320 KB)

if file_size <= RESUMABLE_THRESHOLD {
    // Simple upload: PUT /drive/items/{parent_id}:/{name}:/content
    client.put(&simple_url).body(bytes).send().await?;
} else {
    // Step 1: Create upload session
    let session = client.post(&create_session_url)
        .json(&json!({"item": {"@microsoft.graph.conflictBehavior": "replace"}}))
        .send().await?.json::<Value>().await?;
    let upload_url = session["uploadUrl"].as_str().unwrap();

    // Step 2: PUT byte ranges
    let mut offset = 0u64;
    while offset < file_size {
        let end = (offset + CHUNK_SIZE).min(file_size) - 1;
        let chunk = read_range(&mut file, offset, end - offset + 1).await?;

        client.put(upload_url)
            .header(CONTENT_RANGE, format!("bytes {}-{}/{}", offset, end, file_size))
            .body(chunk)
            .send().await?;

        offset = end + 1;
    }
}
```

### 4.4 Multipart Upload (S3)

```rust
// Step 1: Initiate
// POST /bucket/key?uploads → UploadId

// Step 2: Upload parts (5 MB minimum, except last part)
// PUT /bucket/key?partNumber=N&uploadId=X → ETag

// Step 3: Complete
// POST /bucket/key?uploadId=X
// Body: <CompleteMultipartUpload>
//         <Part><PartNumber>1</PartNumber><ETag>"..."</ETag></Part>
//         ...
//       </CompleteMultipartUpload>
```

### Upload Pattern Summary

| Provider | Simple Upload Limit | Large File Strategy | Chunk Size |
|----------|-------------------|-------------------|------------|
| Google Drive | 5 MB | Resumable upload session | 256 KB |
| Dropbox | 150 MB | Upload session (3-step) | 128 MB |
| OneDrive | 4 MB | Resumable upload session | 320 KB |
| Box | 50 MB | Chunked upload session | 20 MB |
| S3 | 5 GB (single PUT) | Multipart upload | 5 MB |
| Azure | 256 MB | Block upload + PutBlockList | 4 MB |
| pCloud | Unlimited | Single PUT | - |
| Filen | Unlimited | Encrypt + single PUT per chunk | 1 MB |
| OpenDrive | Unlimited | `create_file` → `open_file_upload` → `upload_file_chunk2` → `close_file_upload` | Provider-defined single/few chunks |

---

## 5. Download Patterns

### Streaming Download (Standard)

```rust
async fn download(&mut self, remote_path: &str, local_path: &str,
                  on_progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
    let response = self.client.get(&download_url)
        .header(AUTHORIZATION, format!("Bearer {}", token.expose_secret()))
        .send().await?;

    let total_size = response.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(local_path).await?;
    let mut downloaded = 0u64;
    let mut stream = response.bytes_stream();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        downloaded += chunk.len() as u64;

        if let Some(ref cb) = on_progress {
            cb(downloaded, total_size);
        }
    }

    Ok(())
}
```

### In-Memory Download (Size-Capped)

For small files (vault operations, text preview, etc.):

```rust
/// Downloads to memory with a 500 MB cap to prevent OOM
pub async fn response_bytes_with_limit(resp: Response, limit: u64) -> Result<Vec<u8>, ProviderError> {
    // Check Content-Length first
    if let Some(cl) = resp.content_length() {
        if cl > limit {
            return Err(ProviderError::TransferFailed(format!(
                "File too large ({:.1} MB). Use streaming download.", cl as f64 / 1_048_576.0
            )));
        }
    }

    // Stream with running size check
    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(ProviderError::TransferFailed("Size limit exceeded".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
```

> **Safety**: `MAX_DOWNLOAD_TO_BYTES = 500 MB`. This prevents OOM when a remote file is unexpectedly large. For larger files, always use the streaming `download()` method.

---

## 6. Pagination

Every provider that returns file listings must handle pagination. The general pattern:

```rust
let mut all_entries = Vec::new();
let mut continuation_token: Option<String> = None;

loop {
    let mut params = vec![("limit", "1000")];
    if let Some(ref token) = continuation_token {
        params.push(("page_token", token));
    }

    let response = self.client.get(&list_url)
        .query(&params)
        .send().await?;
    let result: ListResponse = response.json().await?;

    all_entries.extend(result.entries);

    match result.next_page_token {
        Some(token) if !token.is_empty() => continuation_token = Some(token),
        _ => break,
    }
}
```

### Provider-Specific Pagination

| Provider | Type | Field | Page Size | Notes |
|----------|------|-------|-----------|-------|
| Google Drive | Token | `nextPageToken` | 1000 | `fields` parameter required |
| Dropbox | Cursor | `cursor` via `/list_folder/continue` | N/A | Auto-pagination |
| OneDrive | Link | `@odata.nextLink` | 200 | Full URL for next page |
| S3 | Token | `NextContinuationToken` | 1000 | `list-type=2` required |
| Azure | Marker | `NextMarker` in XML | 5000 | Enum results XML parsing |
| Box | Offset | `offset` + `limit` | 1000 | Total count in response |
| pCloud | - | None (full list) | - | Returns all files at once |
| Zoho WorkDrive | Offset | `start_index` + `count` | 200 | Zero-based indexing |
| 4shared | ID-based | `pageNumber` | 1000 | Per-folder, not cursor |
| kDrive | Cursor | `cursor` | 500 | Opaque cursor token |
| FileLu | Offset | `pageNo` | 100 | 1-based page numbers |
| Jottacloud | - | None (XML listing) | - | Full list per folder |
| Koofr | - | None (full list) | - | Returns all files at once |
| OpenDrive | - | None (full list) | - | Returns folders/files for a folder ID in one response |

> **Lesson**: Never assume a provider returns all results in one call. Always implement pagination, even if the current test folder is small.

---

## 7. XML Parsing with quick-xml

Three providers return XML instead of JSON: **WebDAV** (PROPFIND), **S3** (ListObjectsV2), and **Azure Blob** (EnumerateBlobs). AeroFTP uses `quick-xml 0.39` with event-based parsing.

### Pattern: State Machine Parser

```rust
use quick_xml::Reader;
use quick_xml::events::Event;

pub fn parse_list_response(xml: &[u8]) -> Result<Vec<RemoteEntry>, ProviderError> {
    let mut reader = Reader::from_reader(xml);
    reader.trim_text(true); // Important for Azure whitespace

    let mut entries = Vec::new();
    let mut current = RemoteEntry::default();
    let mut in_blob = false;
    let mut current_tag = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match tag.as_str() {
                    "Blob" | "BlobPrefix" => {
                        in_blob = true;
                        current = RemoteEntry::default();
                        current.is_dir = tag == "BlobPrefix";
                    }
                    _ => current_tag = tag,
                }
            }
            Ok(Event::Text(e)) if in_blob => {
                let text = e.unescape()
                    .map_err(|e| ProviderError::Other(format!("XML parse error: {}", e)))?
                    .into_owned();
                match current_tag.as_str() {
                    "Name" => current.name = text,
                    "Content-Length" => current.size = text.parse().unwrap_or(0),
                    "Last-Modified" => current.modified = Some(text),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "Blob" || tag == "BlobPrefix" {
                    entries.push(current.clone());
                    in_blob = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(ProviderError::Other(format!("XML error: {}", e))),
            _ => {}
        }
    }

    Ok(entries)
}
```

### WebDAV PROPFIND

```xml
<!-- Request -->
PROPFIND /remote.php/dav/files/user/ HTTP/1.1
Depth: 1

<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:displayname/>
    <d:getcontentlength/>
    <d:getlastmodified/>
    <d:resourcetype/>
    <d:getcontenttype/>
  </d:prop>
</d:propfind>
```

```xml
<!-- Response -->
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/remote.php/dav/files/user/Documents/</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>Documents</d:displayname>
        <d:resourcetype><d:collection/></d:resourcetype>
        <d:getlastmodified>Mon, 10 Mar 2026 12:00:00 GMT</d:getlastmodified>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>
```

> **Migration note**: AeroFTP migrated from regex-based XML parsing to quick-xml in v2.4.0. The event-based approach is more robust against malformed XML and handles edge cases (CDATA, entities, namespaces) that regex misses.

---

## 8. Error Handling

### API Error Sanitization

Every REST provider must sanitize error responses before surfacing them to the UI:

```rust
pub fn sanitize_api_error(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("unknown error");

    // Truncate to 200 chars (UTF-8 safe)
    let truncated = if first_line.len() > 200 {
        let boundary = first_line.char_indices()
            .take_while(|&(i, _)| i <= 200)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(200);
        format!("{}...", &first_line[..boundary])
    } else {
        first_line.to_string()
    };

    // Redact credentials that might leak in error messages
    if truncated.contains("Bearer ") || truncated.contains("eyJ") {
        "API error (response contained credentials - redacted)".to_string()
    } else {
        truncated
    }
}
```

**Usage pattern** in every provider:

```rust
let response = self.client.get(&url).send().await?;
if !response.status().is_success() {
    let body = response.text().await.unwrap_or_default();
    return Err(ProviderError::Other(sanitize_api_error(&body)));
}
```

### HTTP Status Mapping

```rust
fn map_http_error(status: u16, body: &str) -> ProviderError {
    match status {
        401 | 403 => ProviderError::AuthenticationFailed(sanitize_api_error(body)),
        404      => ProviderError::NotFound(sanitize_api_error(body)),
        409      => ProviderError::AlreadyExists(sanitize_api_error(body)),
        429      => ProviderError::Other("Rate limited - try again later".into()),
        500..=599 => ProviderError::Other(format!("Server error ({})", status)),
        _        => ProviderError::Other(sanitize_api_error(body)),
    }
}
```

---

## 9. HTTP Retry with Backoff

**File**: `http_retry.rs` (~150 lines)

### Configuration

```rust
pub struct HttpRetryConfig {
    pub max_retries: u32,        // default: 3
    pub base_delay_ms: u64,      // default: 1000
    pub max_delay_ms: u64,       // default: 30000
    pub backoff_multiplier: f64, // default: 2.0
}
```

### Retryable Status Codes

```rust
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}
```

All 4xx except 429 are **not retried** (client errors won't change on retry).

### Exponential Backoff with Jitter

```rust
fn calculate_delay(attempt: u32, config: &HttpRetryConfig) -> Duration {
    let base = config.base_delay_ms as f64 * config.backoff_multiplier.powi(attempt as i32);
    let capped = base.min(config.max_delay_ms as f64);
    // 10-30% jitter prevents thundering herd
    let jitter = capped * (0.1 + rand::random::<f64>() * 0.2);
    Duration::from_millis((capped + jitter) as u64)
}
```

### Retry-After Header

```rust
fn parse_retry_after(response: &Response) -> Option<Duration> {
    let value = response.headers().get("retry-after")?.to_str().ok()?;
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(300))); // Cap at 5 minutes
    }
    None // HTTP-date format not parsed
}
```

### Usage

```rust
let request = client.get(&url)
    .header(AUTHORIZATION, auth)
    .build()?;
let response = send_with_retry(&client, request, &HttpRetryConfig::default()).await?;
```

---

## 10. Credential Security

### SecretString Pattern

All sensitive credentials use `secrecy::SecretString` to prevent accidental logging and ensure zeroization on drop:

```rust
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: secrecy::SecretString,  // Not String!
    // ...
}

// Usage: expose_secret() only when transmitting
let password = config.password.expose_secret();
```

### Explicit Zeroization

```rust
impl ProviderConfig {
    pub fn zeroize_password(&mut self) {
        use zeroize::Zeroize;
        if let Some(ref mut pwd) = self.password {
            pwd.zeroize();
        }
    }
}
```

### Token Storage

See [Section 3.1 Token Storage Hierarchy](#token-storage-hierarchy).

### Security Rules

1. **Never log tokens**: Use `tracing::info!("Authenticated as {:?}", provider)`, never log the token value
2. **Never return tokens to frontend**: Tauri commands return success/failure, not raw tokens
3. **Zeroize after use**: Call `zeroize_password()` on `ProviderConfig` after extracting to `SecretString`
4. **Sanitize errors**: Always pass API errors through `sanitize_api_error()` before returning to UI
5. **Encrypt at rest**: All tokens stored in vault.db (AES-256-GCM + Argon2id), never plaintext on disk

---

## 11. Transfer Optimization

### Optimization Hints

Each provider can advertise its transfer capabilities:

```rust
pub struct TransferOptimizationHints {
    pub supports_multipart: bool,         // S3: true
    pub multipart_threshold: u64,         // S3: 5MB
    pub multipart_part_size: u64,         // S3: 5MB
    pub multipart_max_parallel: u8,       // S3: 4
    pub supports_resume_download: bool,   // FTP, SFTP, Koofr
    pub supports_resume_upload: bool,     // FTP, SFTP
    pub supports_server_checksum: bool,   // S3
    pub preferred_checksum_algo: Option<String>,
    pub supports_compression: bool,       // SFTP (zlib)
    pub supports_delta_sync: bool,        // SFTP
}
```

### Progress Reporting

All `upload()` and `download()` methods accept an optional progress callback:

```rust
on_progress: Option<Box<dyn Fn(u64, u64) + Send>>
//                        bytes_transferred, total_bytes
```

The frontend throttles progress events to ~150ms or 2% delta to avoid IPC flooding.

### FTP Speed Limits

FTP is the only protocol with server-negotiated speed limits:

```rust
async fn set_speed_limit(&mut self, upload_kb: u64, download_kb: u64) -> Result<(), ProviderError>;
async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError>;
```

---

## 12. Provider Capability Matrix

| Provider | List | Upload | Download | Mkdir | Delete | Rename | Versions | Trash | Share | Quota | Resume | Search |
|----------|------|--------|----------|-------|--------|--------|----------|-------|-------|-------|--------|--------|
| FTP | X | X | X | X | X | X | | | | | X | |
| SFTP | X | X | X | X | X | X | | | | | X | |
| WebDAV | X | X | X | X | X | X | | | | | | |
| S3 | X | X | X | X | X | X | | | X | X | | |
| Google Drive | X | X | X | X | X | X | X | X | X | X | | X |
| Dropbox | X | X | X | X | X | X | | X | X | X | | X |
| OneDrive | X | X | X | X | X | X | X | X | X | X | | X |
| MEGA | X | X | X | X | X | X | | X | X | X | | |
| Box | X | X | X | X | X | X | X | X | X | X | | X |
| pCloud | X | X | X | X | X | X | | X | X | X | | |
| Azure | X | X | X | X | X | X | | | | X | | |
| Filen | X | X | X | X | X | X | | X | X | | | |
| 4shared | X | X | X | X | X | X | | | X | X | | X |
| Zoho | X | X | X | X | X | X | X | X | X | X | | X |
| Internxt | X | X | X | X | X | X | | X | | X | | |
| kDrive | X | X | X | X | X | X | | X | X | X | | |
| Jottacloud | X | X | X | X | X | X | | X | | X | | |
| FileLu | X | X | X | X | X | X | | X | X | | | |
| Koofr | X | X | X | X | X | X | | X | X | X | X | |
| Drime Cloud | X | X | X | X | X | X | | | X | X | | |
| OpenDrive | X | X | X | X | X | X | | | | | | |
| Yandex Disk | X | X | X | X | X | X | | X | X | X | | X |

---

## 13. Step-by-Step: Adding a New Provider

### Step 1: Create the Provider File

Create `src-tauri/src/providers/my_provider.rs`:

```rust
use async_trait::async_trait;
use reqwest::Client;
use secrecy::SecretString;
use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry,
    ProviderConfig, StorageInfo, TransferOptimizationHints,
    sanitize_api_error, response_bytes_with_limit, MAX_DOWNLOAD_TO_BYTES,
};

const BASE_URL: &str = "https://api.myprovider.com/v1";

/// Configuration for MyProvider
#[derive(Debug, Clone)]
pub struct MyProviderConfig {
    pub api_token: SecretString,
    pub initial_path: Option<String>,
}

impl MyProviderConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.as_deref()
            .ok_or_else(|| ProviderError::AuthenticationFailed("API token required".into()))?;
        Ok(Self {
            api_token: SecretString::from(token.to_string()),
            initial_path: config.initial_path.clone(),
        })
    }
}

pub struct MyProvider {
    config: MyProviderConfig,
    client: Client,
    connected: bool,
    current_path: String,
}

impl MyProvider {
    pub fn new(config: MyProviderConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            connected: false,
            current_path: "/".to_string(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.config.api_token.expose_secret())
    }
}

#[async_trait]
impl StorageProvider for MyProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn provider_type(&self) -> ProviderType { ProviderType::MyProvider }
    fn display_name(&self) -> String { "My Provider".to_string() }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Verify credentials with a lightweight API call
        let resp = self.client.get(format!("{}/account", BASE_URL))
            .header("Authorization", self.auth_header())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(sanitize_api_error(&body)));
        }

        self.connected = true;
        if let Some(ref path) = self.config.initial_path {
            self.current_path = path.clone();
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool { self.connected }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected { return Err(ProviderError::NotConnected); }

        // Implement pagination (see Section 6)
        let mut entries = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!("{}/files/list?path={}", BASE_URL, path);
            if let Some(ref token) = page_token {
                url.push_str(&format!("&page_token={}", token));
            }

            let resp = self.client.get(&url)
                .header("Authorization", self.auth_header())
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(sanitize_api_error(&body)));
            }

            let data: serde_json::Value = resp.json().await
                .map_err(|e| ProviderError::Other(e.to_string()))?;

            if let Some(items) = data["items"].as_array() {
                for item in items {
                    entries.push(RemoteEntry {
                        name: item["name"].as_str().unwrap_or("").to_string(),
                        path: item["path"].as_str().unwrap_or("").to_string(),
                        is_dir: item["type"].as_str() == Some("folder"),
                        size: item["size"].as_u64().unwrap_or(0),
                        modified: item["modified"].as_str().map(|s| s.to_string()),
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: None,
                        symlink_target: None,
                    });
                }
            }

            match data["next_page_token"].as_str() {
                Some(token) if !token.is_empty() => page_token = Some(token.to_string()),
                _ => break,
            }
        }

        Ok(entries)
    }

    // ... implement remaining 17 required methods
    // See existing providers for patterns
}
```

### Step 2: Register in mod.rs

```rust
// In src-tauri/src/providers/mod.rs
pub mod my_provider;
pub use my_provider::MyProvider;
```

### Step 3: Add ProviderType Variant

```rust
// In src-tauri/src/providers/types.rs
pub enum ProviderType {
    // ... existing variants
    MyProvider,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // ... existing arms
            ProviderType::MyProvider => write!(f, "My Provider"),
        }
    }
}

impl ProviderType {
    pub fn default_port(&self) -> u16 {
        match self {
            // ... existing arms
            ProviderType::MyProvider => 443,
        }
    }
}
```

### Step 4: Add to ProviderFactory

```rust
// In src-tauri/src/providers/mod.rs, ProviderFactory::create()
ProviderType::MyProvider => {
    let config = MyProviderConfig::from_provider_config(config)?;
    Ok(Box::new(MyProvider::new(config)))
}
```

### Step 5: Register Tauri Commands

```rust
// In src-tauri/src/lib.rs
// Add connection command
#[tauri::command]
async fn my_provider_connect(/* params */) -> Result<(), String> { ... }

// Register in generate_handler!
.invoke_handler(tauri::generate_handler![
    // ... existing commands
    my_provider_connect,
])
```

### Step 6: Frontend Integration

1. Add to `ProviderType` enum in TypeScript
2. Add provider card to `ProtocolSelector.tsx` or `ProviderSelector.tsx`
3. Add connection form fields
4. Add i18n keys (`protocol.myProvider`, `protocol.myProviderDesc`)
5. Run `npm run i18n:sync` to propagate to 47 languages

### Step 7: Test

```bash
cd src-tauri && cargo clippy --all-targets -- -D warnings
npm run build
```

---

## 14. Testing & Audit Methodology

### Multi-Auditor Approach

AeroFTP uses **independent parallel audits** by multiple AI models to maximize coverage:

| Phase | Auditors | Focus |
|-------|----------|-------|
| A: Capabilities | 3-4 Claude Opus | Feature completeness, StorageProvider compliance |
| B: Security | GPT-5.3 Codex | OWASP top 10, credential handling, injection |
| C: Integration | 2-3 Claude Opus | Cross-provider consistency, edge cases |
| D: Counter-Audit | 1-2 mixed | Verify fixes, find regressions |

### Security Regression Suite

**File**: `.github/scripts/security-regression.cjs`

Automated checks that run in CI on every push:

1. **Shell denylist**: Verify `ai_tools.rs` contains `DENIED_COMMAND_PATTERNS`
2. **No plaintext tokens**: Grep for hardcoded API keys or tokens
3. **SecretString usage**: Verify all `password` fields use `SecretString`
4. **CSP headers**: Check Tauri config for CSP directives
5. **Dependency audit**: `cargo audit` for known vulnerabilities

### Clippy Enforcement

```bash
# Exact command used in CI - zero warnings allowed
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

### Provider-Specific Test Patterns

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_provider_config() {
        let config = ProviderConfig {
            name: "test".into(),
            provider_type: ProviderType::MyProvider,
            host: "api.example.com".into(),
            port: None,
            username: None,
            password: Some("test-token".into()),
            initial_path: Some("/data".into()),
            extra: HashMap::new(),
        };
        let result = MyProviderConfig::from_provider_config(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_error_sanitization() {
        let raw = "Error: Bearer eyJhbGciOiJSUzI1NiJ9.invalid.token";
        let sanitized = sanitize_api_error(raw);
        assert_eq!(sanitized, "API error (response contained credentials - redacted)");
    }
}
```

---

## 15. Dependencies

### Core

| Crate | Version | Purpose |
|-------|---------|---------|
| `reqwest` | 0.13 | HTTP client (all REST providers) |
| `async-trait` | >=1 | Async trait methods |
| `tokio` | 1.0 | Async runtime |
| `tokio-util` | 0.7 | `ReaderStream` for streaming uploads |
| `serde` / `serde_json` | 1.0 | JSON serialization |
| `secrecy` | >=0.8 | `SecretString` for credentials |
| `zeroize` | >=1.6 | Memory clearing |

### Protocol-Specific

| Crate | Version | Purpose | Pin Note |
|-------|---------|---------|----------|
| `suppaftp` | **=8.0.1** | FTP/FTPS | **PINNED**: v8.0.2 uses `AsFd` (Unix-only), breaks Windows |
| `russh` | 0.57 | SSH/SFTP | |
| `quick-xml` | 0.39 | XML parsing (WebDAV, S3, Azure) | |
| `oauth2` | 5 | OAuth2 PKCE flow | |
| `hmac` / `sha1` / `sha2` | >=0.1 | HMAC signing (OAuth1, Azure, S3) | |
| `base64` | >=0.20 | Encoding (Azure, S3, MEGA) | |
| `chrono` | >=0.4 | Timestamp formatting | |
| `rand` | >=0.8 | Nonce generation, jitter | |

### Why No AWS SDK?

AeroFTP implements S3 signing manually (SigV4) instead of using `aws-sdk-s3` because:

1. **S3-compatible providers**: MinIO, Backblaze B2, Cloudflare R2, Wasabi, DigitalOcean Spaces all use S3 protocol but diverge from AWS-specific behavior
2. **Binary size**: The AWS SDK pulls in dozens of transitive dependencies
3. **Control**: Custom signing allows path-style vs virtual-hosted, custom endpoints, and precise error handling
4. **Consistency**: Same `reqwest` + `StorageProvider` pattern as all other REST providers

---

## 16. Lessons Learned

### Authentication

1. **OneDrive requires `localhost`**: Microsoft Entra ID rejects `127.0.0.1` in redirect URIs. This is unique among OAuth2 providers.

2. **Token refresh race condition (H-04)**: Two concurrent requests can both detect an expired token and try to refresh simultaneously, causing the second to receive `invalid_grant`. Solution: `tokio::sync::Mutex` guard around refresh.

3. **pCloud regional endpoints**: EU accounts use `eapi.pcloud.com`, US uses `api.pcloud.com`. The token endpoint and API base must match the account region.

4. **Zoho has 9 regional TLDs**: `zoho.com`, `zoho.eu`, `zoho.in`, `zoho.com.au`, `zoho.jp`, `zoho.uk`, `zohocloud.ca`, `zoho.sa`, `zoho.com.cn`. Each has its own OAuth endpoint.

5. **4shared ID-based paths**: Unlike all other providers, 4shared uses numeric IDs, not string paths. Every operation requires resolving path → ID first.

### Uploads

6. **Never buffer full files**: Early implementations used `std::fs::read()` → OOM on large files. Always use `ReaderStream` or chunk-based reading.

7. **OneDrive 320KB alignment**: Upload chunks must be multiples of 320 KB. Non-aligned chunks cause silent corruption.

8. **Dropbox session close flag**: The last `append_v2` call must set `close: true`, otherwise `finish` fails with an opaque error.

### Error Handling

9. **Sanitize everything**: API error responses can contain Bearer tokens, API keys, or session IDs. Always pass through `sanitize_api_error()`.

10. **Filen 2FA quirk**: Always send `twoFactorCode` field with default `"XXXXXX"` even when 2FA is not enabled. Omitting it causes auth failure.

### XML Parsing

11. **Azure requires `trim_text(true)`**: Azure Blob responses have significant whitespace that breaks text node parsing without trimming.

12. **WebDAV namespace prefixes vary**: Some servers use `d:`, others use `D:` or the full `DAV:` namespace. Parse by local name, not prefix.

### Platform

13. **suppaftp 8.0.2 breaks Windows**: Uses `std::os::fd::AsFd` which is Unix-only. Must pin to `=8.0.1` until upstream fixes cross-platform support.

14. **SFTP symlink detection**: NAS devices (Synology, WD MyCloud) create symlinks for shared folders. `list()` must follow symlinks via `sftp.metadata()` to detect if target is a directory.

### Security

15. **Never store tokens in localStorage**: Desktop app - use encrypted vault (AES-256-GCM + Argon2id). Only model names and base URLs go in localStorage.

16. **TLS downgrade detection**: When FTP `ExplicitIfAvailable` falls back to plaintext, set a `tls_downgraded` flag and show a security warning to the user.

### Provider-Specific Quirks

17. **Yandex Disk uses `Authorization: OAuth`**: The provider obtains a token through OAuth2, but the REST API rejects a standard Bearer header. Requests must send `Authorization: OAuth {token}`.

18. **OpenDrive file lookup is not fully reliable**: `file/idbypath.json` can fail for existing files after upload. A resilient implementation should fall back to listing the parent folder and matching the exact filename.

19. **OpenDrive file move is not fully reliable server-side**: `file/move_copy.json` can reject documented `move=true` with `Invalid value specified for \`move\`` even though `folder/move_copy.json` works correctly. A production-safe fallback is `download -> upload -> delete` for file moves between folders.

20. **OpenDrive rename parameters differ by object type**: `file/rename.json` requires `new_file_name`, while `folder/rename.json` requires `folder_name`.

---

## Acknowledgments

This guide documents patterns developed across 35+ releases and refined through 12+ independent security audits totaling 500+ findings. The architecture has been proven in production across 25 storage backends, with new patterns added as new providers join the catalog.

---

**Document Version**: 3.7
**Maintainer**: axpnet
**Project**: [github.com/axpdev-lab/aeroftp](https://github.com/axpdev-lab/aeroftp)
