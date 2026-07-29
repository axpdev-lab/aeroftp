// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Remote file from FTP server
export interface RemoteFile {
  name: string;
  path: string;
  size: number | null;
  is_dir: boolean;
  modified: string | null;
  permissions: string | null;
  metadata?: Record<string, string>;
}

export interface FileListResponse {
  files: RemoteFile[];
  current_path: string;
}

// Supported storage provider types
export type ProviderType =
  | "ftp"
  | "ftps"
  | "sftp"
  | "webdav"
  | "s3"
  | "aerocloud"
  | "googledrive"
  | "googlephotos"
  | "dropbox"
  | "onedrive"
  | "mega"
  | "box"
  | "pcloud"
  | "azure"
  | "filen"
  | "fourshared"
  | "zohoworkdrive"
  | "internxt"
  | "kdrive"
  | "jottacloud"
  | "drime"
  | "filelu"
  | "koofr"
  | "opendrive"
  | "yandexdisk"
  | "github"
  | "gitlab"
  | "swift"
  | "immich"
  | "imagekit"
  | "uploadcare"
  | "backblaze"
  | "cloudinary"
  | "peer"
  /**
   * Portable device over MTP/WPD. Session from PLACES discovery, or a saved
   * device profile matched by fingerprint (not host+password path).
   * See APPENDIX-DEVICE-PROFILES.
   */
  | "mtp";

// Check if a provider type requires OAuth2 authentication
export const isOAuthProvider = (type: ProviderType): boolean => {
  return (
    type === "googledrive" ||
    type === "googlephotos" ||
    type === "dropbox" ||
    type === "onedrive" ||
    type === "box" ||
    type === "pcloud" ||
    type === "zohoworkdrive" ||
    type === "yandexdisk"
  );
};

// Check if a provider type requires OAuth 1.0 authentication (4shared)
export const isFourSharedProvider = (type: ProviderType): boolean => {
  return type === "fourshared";
};

// Check if a provider type is AeroCloud
export const isAeroCloudProvider = (type: ProviderType): boolean => {
  return type === "aerocloud";
};

// Protocol class label shown on My Servers tiles (OAuth / API / WebDAV / E2E / FTP / SFTP / S3 / Azure / MTP)
// "Crypt" is a profile-level class (not a transport): a saved profile with an
// enabled crypt overlay reads as "Crypt" regardless of its backend. See
// getProfileProtocolClass.
export type ProtocolClass = "OAuth" | "API" | "WebDAV" | "E2E" | "FTP" | "FTPS" | "SFTP" | "S3" | "Azure" | "AeroCloud" | "Crypt" | "MTP";

export const getProtocolClass = (type: ProviderType): ProtocolClass => {
  if (isOAuthProvider(type) || isFourSharedProvider(type)) return "OAuth";
  if (isAeroCloudProvider(type)) return "AeroCloud";
  if (type === "filen" || type === "internxt" || type === "mega") return "E2E";
  if (type === "webdav") return "WebDAV";
  if (type === "ftps") return "FTPS";
  if (type === "ftp") return "FTP";
  if (type === "sftp") return "SFTP";
  if (type === "s3") return "S3";
  if (type === "azure") return "Azure";
  // Portable USB MTP/WPD (not an HTTP "API" cloud).
  if (type === "mtp") return "MTP";
  // Native API providers (Koofr, Jottacloud, OpenDrive, kDrive, Drime, FileLu, Yandex, GitHub, GitLab, Swift, Immich)
  return "API";
};

// Encryption strength (bits) for E2E providers. MEGA uses AES-128 for files;
// Filen and Internxt use AES-256 zero-knowledge encryption.
export const getE2EBits = (type: ProviderType): 128 | 256 | null => {
  if (type === "mega") return 128;
  if (type === "filen" || type === "internxt") return 256;
  return null;
};

// Check if a provider uses non-FTP backend (provider_* Tauri commands)
export const isNonFtpProvider = (type: ProviderType): boolean => {
  return [
    "googledrive",
    "dropbox",
    "onedrive",
    "s3",
    "webdav",
    "mega",
    "sftp",
    "box",
    "pcloud",
    "azure",
    "filen",
    "fourshared",
    "zohoworkdrive",
    "internxt",
    "kdrive",
    "jottacloud",
    "drime",
    "filelu",
    "koofr",
    "opendrive",
    "yandexdisk",
    "github",
    "gitlab",
    "swift",
    "immich",
    "imagekit",
    "uploadcare",
    "backblaze",
    "cloudinary",
    // AeroShare peer drives browse a local replica through the provider_*
    // command surface (protocol "peer"), so they dispatch like any non-FTP
    // provider. The replica is read-only in Phase 1.
    "peer",
    // Portable MTP/WPD sessions installed into ProviderState from PLACES.
    "mtp",
  ].includes(type);
};

// Check if a provider is a traditional FTP/FTPS connection (uses ftp_* Tauri commands)
export const isFtpProtocol = (type: ProviderType): boolean => {
  return type === "ftp" || type === "ftps";
};

// Backends where a transparent encryption overlay (AeroCrypt / rclone-crypt)
// does NOT apply and must NOT be offered. These accept only specific content
// or impose their own path/object model, so writing encrypted blobs through
// them is confusing at best and corrupts/rejects uploads at worst:
//  - Media-only APIs (images/video, transformations, flat or media-library
//    layout): Immich, ImageKit, Uploadcare, Cloudinary, Google Photos.
//  - Code / release hosting (repos, not arbitrary file storage): GitHub, GitLab.
// Google Photos is already excluded by isNonFtpProvider (OAuth media), but is
// listed for clarity. Everything else that uses a provider backend supports
// arbitrary file CRUD, so the overlay applies.
const CRYPT_OVERLAY_INCOMPATIBLE: ReadonlySet<string> = new Set([
  "immich",
  "imagekit",
  "uploadcare",
  "cloudinary",
  "googlephotos",
  "github",
  "gitlab",
  // MTP is whole-file object transfer with weak metadata; crypt overlay
  // would invent a filesystem model the device does not have.
  "mtp",
]);

// True when a transparent crypt overlay can be offered for this protocol.
// Used by both the runtime context-menu entries and the connection-form
// overlay toggle so the two surfaces stay consistent (provider-aware: the
// overlay only shows where it is actually usable).
export const providerSupportsCryptOverlay = (
  type?: ProviderType | string | null,
): boolean => {
  if (!type) return false;
  if (type === "ftp" || type === "ftps") return true;
  return (
    isNonFtpProvider(type as ProviderType) &&
    !CRYPT_OVERLAY_INCOMPATIBLE.has(type)
  );
};

// First-class native-API provider protocols. A saved profile with one of
// these protocols is authoritative: the connect path must NOT override it
// from a registry preset. Some providers expose BOTH a native API protocol
// and a WebDAV registry preset sharing the same providerId (Koofr,
// OpenDrive); the registry-preset override used to flip a native profile to
// 'webdav' and connect it over WebDAV, which 404s against the bare API host
// (issue #213). Generic protocols (ftp/ftps/sftp/s3/webdav) are deliberately
// excluded: those legitimately rely on the registry preset for dispatch.
const NATIVE_API_PROTOCOLS: ReadonlySet<string> = new Set([
  "mega", "box", "pcloud", "azure", "filen", "internxt", "kdrive", "drime",
  "filelu", "koofr", "opendrive", "yandexdisk", "googledrive", "dropbox",
  "onedrive", "fourshared", "zohoworkdrive", "github", "gitlab", "immich",
  "jottacloud", "swift",
]);

export const isNativeApiProtocol = (protocol?: string | null): boolean => {
  return !!protocol && NATIVE_API_PROTOCOLS.has(protocol);
};

// True when the backend reports its own real storage quota, so the
// manual total-storage override and the recursive used-storage scan are
// pointless noise and must be hidden in the connection form.
//
//  - Native-API providers always report quota from their account API.
//  - Koofr and OpenDrive are special even over WebDAV: their DAV servers
//    return 0 for the RFC 4331 quota properties, so the backend fetches
//    the real quota from the provider REST API instead (webdav.rs
//    storage_info special-cases app.koofr.net and webdav.opendrive.com).
//    So a Koofr/OpenDrive profile, API or WebDAV, never needs the cap.
//  - Yandex Disk over WebDAV (webdav.yandex.ru) returns real RFC 4331
//    quota via the standard PROPFIND path, so the cap and used-scan are
//    pointless noise there too (#270 comment 17195110).
//
// Raw FTP/FTPS/SFTP, generic S3/WebDAV, and USED-but-no-TOTAL backends
// (Backblaze B2) are deliberately NOT covered: they keep the override.
export const providerServesQuota = (
  protocol?: string | null,
  providerId?: string | null,
  server?: string | null,
): boolean => {
  // AeroShare friend (protocol "peer"): a read-only local replica with no quota
  // concept. Reporting "serves quota" suppresses the manual-total-bytes field
  // and the used-storage scan in the connection/edit form.
  if (protocol === "peer") return true;
  // MTP portable devices report real storage free/total from the device; the
  // manual cap and used-scan checkbox are noise on that form (live-test LT7).
  if (protocol === "mtp") return true;
  if (isNativeApiProtocol(protocol)) return true;
  if (protocol === "webdav") {
    const host = (server || "").toLowerCase();
    if (providerId === "megacmd" || providerId === "megacmd-webdav") return true;
    if (providerId === "koofr" || host.includes("koofr")) return true;
    if (
      providerId === "opendrive-webdav" ||
      host.includes("webdav.opendrive.com")
    ) {
      return true;
    }
    if (providerId === "yandexdisk-webdav" || host.includes("webdav.yandex")) {
      return true;
    }
  }
  return false;
};

// Check if a provider supports storage quota queries
export const supportsStorageQuota = (type: ProviderType): boolean => {
  return [
    "mega",
    "googledrive",
    "dropbox",
    "onedrive",
    "box",
    "pcloud",
    "filen",
    "sftp",
    "webdav",
    "fourshared",
    "zohoworkdrive",
    "internxt",
    "kdrive",
    "jottacloud",
    "drime",
    "filelu",
    "koofr",
    "opendrive",
    "yandexdisk",
    "github",
    "gitlab",
    "swift",
    "cloudinary",
    "backblaze",
  ].includes(type);
};

// Check if a provider supports native share links
export const supportsNativeShareLink = (type: ProviderType): boolean => {
  return [
    "googledrive",
    "dropbox",
    "onedrive",
    "s3",
    "mega",
    "box",
    "pcloud",
    "filen",
    "zohoworkdrive",
    "filelu",
    "koofr",
    "opendrive",
    "yandexdisk",
    "github",
    "webdav",
    "azure",
    "kdrive",
    "drime",
    "immich",
    "imagekit",
    "uploadcare",
    "cloudinary",
    "backblaze",
  ].includes(type);
};

// FTP/FTPS TLS encryption mode
export type FtpTlsMode =
  | "none"
  | "explicit"
  | "implicit"
  | "explicit_if_available";

// Provider-specific configuration options
export interface ProviderOptions {
  // S3-specific
  bucket?: string;
  region?: string;
  endpoint?: string; // For S3-compatible (MinIO, etc.)
  accountId?: string; // Cloudflare R2 account ID (used to compute endpoint)
  pathStyle?: boolean;
  storage_class?: string; // S3 default storage class for uploads
  sse_mode?: string; // S3 server-side encryption (AES256, aws:kms)
  sse_kms_key_id?: string; // S3 KMS key ARN for SSE-KMS
  sessionToken?: string; // AWS STS session token for temporary credentials (AssumeRole / SSO), issue #301
  roleArn?: string; // AWS STS AssumeRole: ARN of the role to assume, issue #301 Fase 2
  roleExternalId?: string; // AWS STS AssumeRole: ExternalId for cross-account protection
  roleSessionName?: string; // AWS STS AssumeRole: RoleSessionName audit label (default aeroftp-session)
  roleDurationSeconds?: number; // AWS STS AssumeRole: requested credential lifetime in seconds (900-43200)
  roleMfaSerial?: string; // AWS STS AssumeRole: MFA device serial/ARN (persisted identifier), issue #301
  roleMfaTokenCode?: string; // AWS STS AssumeRole: one-time MFA token code (single-use, NOT persisted), issue #301

  // WebDAV-specific
  anonymous?: boolean; // Skip auth headers for local WebDAV bridges

  // FTP/FTPS-specific
  tlsMode?: FtpTlsMode; // TLS encryption mode
  verifyCert?: boolean; // Verify server certificate (default: true)

  // WebDAV-specific scheme override.
  // "auto" (default): port 443 -> https, port 80 -> http, localhost / RFC1918
  // / *.local / Filen Desktop hostnames -> http on any port, otherwise https.
  // "http"/"https" force the scheme regardless of port.
  webdavScheme?: 'http' | 'https' | 'auto';

  // SFTP-specific
  private_key_path?: string; // Path to SSH private key
  key_passphrase?: string; // Passphrase for encrypted keys
  timeout?: number; // Connection timeout in seconds

  // OAuth-specific (for Google Drive, Dropbox, OneDrive)
  clientId?: string;
  clientSecret?: string;

  // MEGA-specific
  save_session?: boolean;
  mega_mode?: "native" | "megacmd";
  session_expires_at?: number; // Timestamp (ms)
  logout_on_disconnect?: boolean;

  // Azure Blob Storage-specific
  container?: string;
  accountName?: string;
  accessKey?: string;
  sasToken?: string;

  // pCloud-specific
  pcloudRegion?: "us" | "eu";

  // Filen-specific
  two_factor_code?: string; // Optional single-use TOTP 2FA code (NOT persisted)
  totp_secret?: string;     // Optional persisted base32 TOTP secret. When set, the backend derives the 6-digit code on every connect via totp_helper. Used by Filen and MEGA.
  filen_api_key?: string;   // Optional Filen CLI API key. When set, the backend authenticates with it and skips the /v3/login call (and therefore the 2FA TOTP window). The password is still required for E2E decryption. SECURITY (issue #230): this is a long-lived secret and is NEVER persisted on the saved profile; on save it is moved to the secure vault under filen_api_key_<profileId> and ServerProfile.hasStoredFilenApiKey is set. It only appears here transiently while the connection form is open.
  filen_auth_version?: number; // Last observed Filen authVersion (v1/v2/v3)

  // kDrive-specific
  drive_id?: string; // Infomaniak kDrive numeric ID

  // OpenDrive-specific (#252): per-account default privacy applied to newly
  // created folders and uploaded files. Unset preserves the legacy behaviour
  // (folders created Private, files left as OpenDrive assigns).
  opendriveDefaultPrivacy?: 'private' | 'public' | 'hidden';

  // GitHub-specific
  githubAuthMode?: "authorize" | "pat" | "app";
  githubAppId?: string; // GitHub App ID (for bot mode)
  githubInstallationId?: string; // GitHub App Installation ID (for bot mode)
  githubPemPath?: string; // Local PEM path used to refresh installation tokens
  githubPemStored?: boolean; // true = PEM content is stored in vault (no file needed)
  githubTokenExpiresAt?: string; // ISO timestamp returned by GitHub for installation token expiry
  githubBranch?: string; // Optional branch override for repository browsing

  // Manual storage cap (item 4a). Many backends expose no quota-total
  // endpoint (raw FTP/FTPS/SFTP, most S3/WebDAV) and a few expose USED but
  // not TOTAL (Backblaze B2). This optional override (in bytes) lets the
  // user declare the account/plan cap so the usage bar and columns render.
  // The API total always wins; this is only the fallback when it is 0.
  manualTotalBytes?: number;

  // Item 4b: per-profile opt-in. When true, the explicit recursive "used
  // storage" scan runs once automatically on connect for backends with no
  // `used` API (FTP/S3/WebDAV). Default OFF: a no-quota profile with a
  // huge tree (e.g. a web project on FTP) must not pay a full walk on
  // every connect unless the user asked for it.
  autoScanUsedOnConnect?: boolean;

  // AeroShare (protocol "peer"): the local binding that lets a saved friend
  // profile reconnect to a received drive (design doc §8, Phase 1 task 11).
  // `server`/`host` carries the friend's AeroFTP-ID and `username` the alias;
  // these four describe the drive itself. Persisted on the friend's profile
  // JSON (no separate backend table for Phase 1) and forwarded to
  // provider_connect as peer_namespace/peer_ticket/peer_local_folder/peer_role.
  peerNamespace?: string;   // iroh-docs namespace id of the received drive
  peerTicket?: string;      // DocTicket (dial addresses + namespace) from the share link
  peerLocalFolder?: string; // absolute local folder the drive replicates into
  peerRole?: string;        // "replicator" (their drive, read) in Phase 1
  peerDriveName?: string;   // human label for the drive (display only)

  // InfiniCloud-specific
  infinicloud_mode?: "webdav" | "api"; // Connection mode: standard WebDAV or REST API with auto-discovery
  apiKey?: string; // InfiniCloud developer API key (128-bit hex)
  infinicloudNode?: string; // Discovered node server FQDN (set by discovery)
  infinicloudCapacityGb?: number; // Contract capacity in GB (set by discovery)
  infinicloudIntroduceCode?: string; // Referral code (set by discovery)
}

export interface ConnectionParams {
  server: string;
  username: string;
  password: string;
  protocol?: ProviderType; // Default: 'ftp'
  port?: number; // Default based on protocol
  options?: ProviderOptions;
  displayName?: string; // Custom name for tab display
  providerId?: string; // Registry provider ID for logo display
  savedServerId?: string; // ServerProfile.id when connecting from a saved server (used by Cross-Profile Transfer)
}

export interface DownloadParams {
  remote_path: string;
  local_path: string;
  modified?: string;
  use_delta?: boolean;
}

export interface UploadParams {
  local_path: string;
  remote_path: string;
  use_delta?: boolean;
}

// Local file from filesystem (from backend)
export interface LocalFile {
  name: string;
  path: string;
  size: number | null;
  is_dir: boolean;
  modified: string | null;
}

// Transfer progress event from backend
export interface TransferProgress {
  transfer_id: string;
  filename: string;
  transferred: number;
  total: number;
  percentage: number;
  speed_bps: number;
  eta_seconds: number;
  direction: "download" | "upload";
  total_files?: number; // When set, transferred/total are file counts (folder transfer)
  path?: string; // Full path for context
}

// Per-file delta stats emitted on `event_type === 'complete'` when the
// rsync delta path serviced the transfer (SFTP + key-auth + rsync on
// the remote). Absent for classic transfers and non-SFTP providers.
// `speedup` is rsync's per-file ratio; the directory-wide
// aggregate lives in `DeltaSavingsSummary`.
export interface DeltaTransferStats {
  bytes_sent: number;
  total_size: number;
  speedup: number;
}

// Aggregated delta savings across a sync run, accumulated client-side
// from the stream of `DeltaTransferStats` carried on TransferEvent.
// `average_speedup` is recomputed as `total_size / total_bytes_sent`.
// Absent (summary never built) when no file used the delta path.
export interface DeltaSavingsSummary {
  files_using_delta: number;
  total_bytes_sent: number;
  total_size: number;
  bytes_saved: number; // total_size - total_bytes_sent (can be negative on overhead)
  average_speedup: number | null;
}

// Transfer event from backend (includes transfers and deletes)
export interface TransferEvent {
  event_type: // Transfer events
  | "start"
    | "scanning"
    | "progress"
    | "complete"
    | "error"
    | "cancelled"
    | "file_start"
    | "file_complete"
    | "file_error"
    | "file_skip"
    // Delete events
    | "delete_start"
    | "delete_complete"
    | "delete_cancelled"
    | "delete_error"
    | "delete_file_start"
    | "delete_file_complete"
    | "delete_file_error"
    | "delete_dir_complete";
  transfer_id: string;
  filename: string;
  direction: "download" | "upload" | "local" | "remote" | "cross-profile" | "delete";
  message?: string;
  progress?: TransferProgress;
  path?: string; // Full path for context (file or folder)
  delta_stats?: DeltaTransferStats; // Present only when rsync delta serviced this transfer
  fallback_reason?: string; // Present only when delta was attempted, then fell back to classic
}

// AeroCrypt overlay binding (P3). When present and enabled, connecting this
// profile unlocks an encrypted overlay and presents the standard dual-panel
// transparently (decrypted names/sizes), Filen/MEGA-style, on any provider-API
// backend. The overlay password is NEVER stored in the profile JSON: it lives
// in the OS vault under aerocrypt_overlay_pw_<id> (mirrors hasStoredFilenApiKey),
// flagged by ServerProfile.hasStoredAeroCryptPassword.
// Spec: APPENDIX-AEROVAULT-STACK master plan §3.3/§3.6/§3.8.
export interface AeroCryptOverlayBinding {
  enabled: boolean;
  kind: "aerocrypt" | "rclone-crypt"; // native (recommended) or interop (opens pre-existing rclone-crypt folders)
  remoteScope?: string; // "" / undefined = whole remote root; else a subfolder to encrypt
  localScope?: string; // bound local working folder: downloads land here, uploads source here
  localEncrypted?: boolean; // opt-in, schema-ready now, implementation deferred (P3b/P4)
  withHeader?: boolean; // true = write marker to remote; default undefined (treated as false = headerless)
  useDefaultSalt?: boolean; // opt-in public constant salt (D1); password alone opens headerless vaults (rclone parity). Entropy gate + attestation required in UI.
  filenameEncryption?: "standard" | "obfuscate" | "off"; // default "standard"
  directoryNameEncryption?: boolean; // rclone-crypt only (P3.3b): default true; native ignores it
  aead?: "auto" | "aes-256-gcm-siv" | "xchacha20-poly1305"; // native only; see master plan §5
}

/**
 * Stable identity for a saved MTP device profile (APPENDIX-DEVICE-PROFILES).
 * Prefer serial form; fall back to vid/pid (+ model) when serial is missing.
 * `canonical` is the compare/storage key (`mtp:serial=...` or `mtp:vidpid=...`).
 */
export interface DeviceFingerprint {
  kind: "mtp";
  serial?: string;
  /** USB vendor id as 4-digit uppercase hex when known. */
  vid?: string;
  /** USB product id as 4-digit uppercase hex when known. */
  pid?: string;
  model?: string;
  /** Canonical fingerprint string used for attach match. */
  canonical: string;
}

// Server profile for saved connections
export interface ServerProfile {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  password?: string; // DEPRECATED: migrated to secure credential store
  hasStoredCredential?: boolean; // true if password stored in OS keyring/vault
  hasStoredFilenApiKey?: boolean; // true if a Filen CLI API key is stored in the vault under filen_api_key_<id> (issue #230); the key itself is never persisted in options
  protocol?: ProviderType; // Default: 'ftp'
  initialPath?: string; // Initial remote directory to navigate after connection
  localInitialPath?: string; // Initial local directory for this project/server
  color?: string;
  lastConnected?: string;
  options?: ProviderOptions; // Provider-specific options (S3 bucket, etc.)
  persistModeCredentials?: boolean; // #215: remember each protocol mode's credentials across restarts (server_modes_<id>)
  aeroCryptOverlay?: AeroCryptOverlayBinding; // P3: encrypted-overlay binding (transparent dual-panel). Spec: master plan §3.3
  hasStoredAeroCryptPassword?: boolean; // true if the overlay password is stored in the vault under aerocrypt_overlay_pw_<id>
  hasStoredAeroCryptSalt?: boolean; // rclone-crypt only (P3.3b): true if the overlay salt/password2 is stored in the vault under aerocrypt_overlay_salt_<id>
  hasStoredAeroCryptKeyfilePath?: boolean; // AeroCrypt Tier 1: true if a keyfile path is stored in the vault under aerocrypt_overlay_keyfile_path_<id> (the path is a pointer, not a secret)
  providerId?: string; // Registry provider ID (e.g. 'cloudflare-r2', 'koofr')
  faviconUrl?: string; // Base64 data URL of detected project favicon
  customIconUrl?: string; // User-chosen custom icon (base64 data URL, highest priority)
  publicUrlBase?: string; // HTTP base URL for share link generation (e.g. https://www.example.com/)
  skipDeltaEligibilityPrompt?: boolean; // Suppress the classic fallback modal for this saved server
  /**
   * MTP device identity when `protocol === 'mtp'` (APPENDIX-DEVICE-PROFILES).
   * Connect is fingerprint match → `mtp_open_device`, not host+password.
   * For MTP rows, `host` may hold a human search string (model); `port` is 0;
   * username/password are unused.
   */
  deviceFingerprint?: DeviceFingerprint;
  // Last known storage quota cached after a successful connection. Used by the
  // detailed My Servers card layout to render a usage bar without requiring
  // a fresh authentication round-trip on every render. `totalSource` records
  // whether the cap came from the provider API or the manual override
  // (item 4a); `usedSource` whether `used` is an API figure or an explicit
  // recursive scan (item 4b). `used_at` timestamps the scan.
  lastQuota?: {
    used: number;
    total: number;
    fetched_at: string;
    totalSource?: "api" | "manual";
    usedSource?: "api" | "scan";
    used_at?: string;
    // Number of files counted by the last explicit scan (item 4b). Shown
    // next to the byte figure so the user can sanity-check the result.
    fileCount?: number;
    // Bytes consumed by retained file versions, when the provider reports it
    // (MEGAcmd `mega-df`). Drawn as a distinct segment on the usage bar
    // (#270 c.17207733). Undefined when unknown/unsupported.
    versioningBytes?: number;
  };
  // Aggregate compression telemetry from the last AeroVault op run against
  // this profile (Ehud #162). Feeds the optional, default-hidden "Saved"
  // and "Saved%" columns in My Servers and `aeroftp-cli profiles`.
  // `plaintext`/`compressed` are bytes; `ratio` is percent saved.
  lastCompression?: {
    plaintext: number;
    compressed: number;
    ratio: number;
    at: string;
  };
  // Outcome of the most recent connect attempt for this profile. When set
  // we display a standalone connect-failure marker on the My Servers card
  // (an alert triangle), so a closed Activity Log is no longer the only
  // feedback path for a failed login (#180 / 4486730822). This is profile
  // state, NOT a health status: `useProviderHealth` is unchanged and the
  // marker is rendered independently from the health dot/HealthRadial.
  // Cleared on the next successful connect to the same profile.
  lastConnectionError?: {
    timestamp: string;
    message: string;
  };
}

/**
 * Resolve the effective storage quota under the single rule shared by GUI
 * and CLI (item 4a). A user-set manual total is a TRUE override: when
 * present it wins even over an API-reported total. Rationale: SFTP statfs
 * (and similar) often reports the whole server disk, not the user's
 * allotment, so the explicit manual value must take precedence. Without a
 * manual value, behaviour is unchanged (API total, else nothing). `used`
 * is passed through (API value, or an explicit recursive scan: item 4b).
 */
export interface EffectiveQuota {
  used: number;
  total: number;
  totalSource: "api" | "manual" | "none";
}

export const resolveEffectiveQuota = (
  apiUsed: number,
  apiTotal: number,
  manualTotalBytes?: number,
): EffectiveQuota => {
  if (manualTotalBytes && manualTotalBytes > 0) {
    return { used: apiUsed, total: manualTotalBytes, totalSource: "manual" };
  }
  if (apiTotal > 0) {
    return { used: apiUsed, total: apiTotal, totalSource: "api" };
  }
  return { used: apiUsed, total: 0, totalSource: "none" };
};

// A manual total cap only applies to backends that do NOT serve their own
// quota; the connection form hides the field for quota-serving providers.
// When a provider later GAINS a real quota source (e.g. MEGAcmd via mega-df
// in v4.0.1), a previously-stored manual cap would wrongly override the real
// total: #275 reported a stale 1 GB cap rendering >100% red because the form
// no longer exposed the field to clear it. Ignore the stale cap for
// quota-serving providers so the API/mega-df total wins; raw FTP/SFTP/S3 and
// generic WebDAV keep the override (SFTP statfs reports whole-disk, not the
// user's allotment).
export const effectiveManualCap = (
  manualTotalBytes: number | undefined,
  protocol?: string | null,
  providerId?: string | null,
  server?: string | null,
): number | undefined =>
  providerServesQuota(protocol, providerId, server) ? undefined : manualTotalBytes;

/**
 * A profile is quota-capable for the My Servers bar/columns when its
 * protocol natively exposes a quota OR the user set a manual total
 * override. This lets no-quota backends (FTP/FTPS/SFTP/S3/WebDAV) show a
 * bar once a manual cap (and a scanned `used`) exists.
 */
export const profileHasQuota = (server: {
  protocol?: ProviderType;
  options?: ProviderOptions;
}): boolean =>
  supportsStorageQuota((server.protocol || "ftp") as ProviderType) ||
  !!(server.options?.manualTotalBytes && server.options.manualTotalBytes > 0);

// Crypt-overlay kind for a saved profile, or null when the profile has no
// enabled encrypted overlay binding. Drives the at-rest "Encrypted" markers
// in My Servers (corner shield + list badge + filter chip): 'aerocrypt' is the
// native overlay (emerald), 'rclone-crypt' is the interop lane (blue).
export type CryptOverlayKind = "aerocrypt" | "rclone-crypt";
export const getServerCryptOverlay = (
  server: Pick<ServerProfile, "aeroCryptOverlay">,
): CryptOverlayKind | null => {
  const ov = server.aeroCryptOverlay;
  return ov?.enabled ? ov.kind : null;
};

// Profile-aware protocol class. A saved profile with an enabled crypt overlay
// — native AeroCrypt OR interop rclone-crypt, at equal grade — classifies as
// "Crypt", a single shared family regardless of the transport underneath. This
// mirrors the card badge, which REPLACES the transport badge with the crypt
// identity for overlay profiles; the native/interop distinction lives in the
// badge color + cipher label, NOT in the class. Falls back to the transport
// class when no overlay is bound. Use this (not getProtocolClass) wherever a
// SAVED PROFILE is being classified for display/sort/grouping.
export const getProfileProtocolClass = (
  server: Pick<ServerProfile, "protocol" | "aeroCryptOverlay">,
): ProtocolClass => {
  if (getServerCryptOverlay(server) !== null) return "Crypt";
  return getProtocolClass((server.protocol || "ftp") as ProviderType);
};

// Session status for multi-tab management
export type SessionStatus =
  | "connected"
  | "disconnected"
  | "connecting"
  | "cached";

export interface AeroVaultOverlaySession {
  sessionId: string;
  vaultPath: string;
  source: "local" | "remote";
  remoteVaultPath?: string;
  remoteLocalPath?: string;
  mode: "browse";
  currentPath?: string;
}

// FTP Session for multi-session tabs (Hybrid Cache Architecture)
export interface FtpSession {
  id: string;
  serverId: string; // Display key (host/displayName): kept for backwards compat with favicon lookups
  savedServerId?: string; // ServerProfile.id when connecting from a saved server
  serverName: string; // Display name for tab
  status: SessionStatus;
  remotePath: string;
  localPath: string;
  remoteFiles: RemoteFile[]; // Cached file list
  localFiles: LocalFile[]; // Cached local files
  lastActivity: Date;
  connectionParams: ConnectionParams;
  providerId?: string; // Registry provider ID for logo display
  faviconUrl?: string; // Inherited from ServerProfile on connection
  customIconUrl?: string; // Inherited from ServerProfile on connection
  publicUrlBase?: string; // Inherited from ServerProfile for share link generation
  serverInitialPath?: string; // Inherited from ServerProfile for share link path resolution
  // Per-session navigation sync state
  isSyncNavigation?: boolean;
  syncBasePaths?: { remote: string; local: string } | null;
  // Per-session AeroVault overlay state (N1)
  aeroVaultOverlaySession?: AeroVaultOverlaySession | null;
  // Per-session AeroCrypt / rclone-crypt transparent overlay binding. The backend
  // keeps each unlocked vault addressable by its own vault_id, so every tab can
  // hold its own overlay independently. Stored here so switching/reconnecting to
  // a tab restores ITS overlay (the global vault-id mirror alone cannot, because
  // it only ever reflects one tab at a time).
  // `remoteScope` (CWP-20B): the plaintext-absolute folder the overlay is bound
  // to (empty/undefined => whole remote). Carried per-session so the scope-aware
  // path-bar badge and transfer routing can tell inside-scope (decrypted) from
  // outside-scope (plaintext) without leaking one tab's scope into another.
  cryptOverlay?: { vaultId: string; kind: 'rclone-crypt' | 'aerocrypt'; remoteScope?: string } | null;
  // Persistent overlay CAPABILITY of this tab (the saved profile's overlay kind),
  // set the moment an overlay tab connects and kept across lock/unlock so the
  // path-bar badge can render as a stateful toggle: grey while decrypting/locked,
  // lit when active. Distinct from `cryptOverlay` (the live unlocked vault, which
  // is cleared on lock). Cleared only on disconnect / switch-away.
  cryptOverlayKind?: 'rclone-crypt' | 'aerocrypt' | null;
  // The bound plaintext-absolute scope, kept across lock exactly like the kind
  // above ('' => whole remote). `cryptOverlay` is cleared on lock, so without
  // this the scope would fall back to '' — "the whole remote is the anchor" —
  // and a locked tab would render the in-scope toggle in every folder instead
  // of the 'Overlays Path' button. Cleared only on disconnect / switch-away.
  cryptOverlayScope?: string | null;
}

// State for managing multiple tabs
export interface TabsState {
  sessions: FtpSession[];
  activeSessionId: string | null;
}

// ============ Sync Types ============

export type SyncStatus =
  | "identical"
  | "local_newer"
  | "remote_newer"
  | "local_only"
  | "remote_only"
  | "conflict"
  | "size_mismatch";

export type SyncDirection =
  | "local_to_remote"
  | "remote_to_local"
  | "bidirectional";

export type SyncAction =
  | "upload"
  | "download"
  | "delete_local"
  | "delete_remote"
  | "skip"
  | "ask_user"
  | "keep_both";

export interface FileInfo {
  name: string;
  path: string;
  size: number;
  modified: string | null;
  is_dir: boolean;
  checksum: string | null;
  checksum_alg?: string | null;
}

export interface FileComparison {
  relative_path: string;
  status: SyncStatus;
  local_info: FileInfo | null;
  remote_info: FileInfo | null;
  is_dir: boolean;
  sync_reason: string;
  /** True if this file existed in a previous sync index (for bisync delete detection) */
  previously_synced?: boolean;
}

export type ConflictStrategy =
  | "ask"
  | "newer"
  | "older"
  | "larger"
  | "smaller"
  | "skip";

export interface CompareOptions {
  compare_timestamp: boolean;
  compare_size: boolean;
  compare_checksum: boolean;
  exclude_patterns: string[];
  error_correction?: {
    enabled: boolean;
    pct?: number;
    max_file_size?: number;
  } | null;
  direction: SyncDirection;
  delete_orphans?: boolean;
  conflict_strategy?: ConflictStrategy;
  min_size?: number;
  max_size?: number;
  min_age_secs?: number;
  max_age_secs?: number;
  versioning_strategy?: "disabled" | "trash_can" | "simple" | "staggered";
  /** Bandwidth schedule preset: off = manual limits, office = throttle 08-18, night = throttle 18-08 */
  bw_schedule?: "off" | "office" | "night";
}

export interface SyncIndexEntry {
  size: number;
  modified: string | null;
  is_dir: boolean;
}

export interface SyncIndex {
  version: number;
  last_sync: string;
  local_path: string;
  remote_path: string;
  files: Record<string, SyncIndexEntry>;
}

// ============ Sync Phase 2: Reliability Types ============

export type SyncErrorKind =
  | "network"
  | "auth"
  | "path_not_found"
  | "permission_denied"
  | "quota_exceeded"
  | "rate_limit"
  | "timeout"
  | "file_locked"
  | "disk_error"
  | "unknown";

export interface SyncErrorInfo {
  kind: SyncErrorKind;
  message: string;
  retryable: boolean;
  file_path: string | null;
}

export interface RetryPolicy {
  max_retries: number;
  base_delay_ms: number;
  max_delay_ms: number;
  timeout_ms: number;
  backoff_multiplier: number;
}

export type VerifyPolicy = "none" | "size_only" | "size_and_mtime" | "full";

export type CompressionMode = "auto" | "on" | "off";

export interface SyncProfile {
  id: string;
  name: string;
  builtin: boolean;
  direction: SyncDirection;
  compare_timestamp: boolean;
  compare_size: boolean;
  compare_checksum: boolean;
  exclude_patterns: string[];
  retry_policy: RetryPolicy;
  verify_policy: VerifyPolicy;
  delete_orphans: boolean;
  parallel_streams: number;
  compression_mode: CompressionMode;
}

/**
 * Wire format for `.aeroftp-script` export/import (issue #133). Mirrors
 * `sync_script::AerosyncScriptProfile` on the Rust side.
 */
export interface AerosyncScriptProfile {
  profile: SyncProfile;
  local_path: string;
  remote_path: string;
  connect_profile: string | null;
  connect_url: string | null;
  dry_run: boolean;
  conflict_mode: string | null;
  track_renames: boolean;
  skip_matching: boolean;
  resync: boolean;
  watch: boolean;
}

export interface AerosyncImportScriptResult {
  profile: AerosyncScriptProfile;
  unmapped_fields: string[];
  warnings: string[];
  canonical_path: string;
  resolved_from_wrapper: boolean;
}

export interface AerosyncExportScriptResult {
  canonical_path: string;
  wrapper_path: string | null;
}

// Phase 3A+: Sync Scheduler
export type Weekday = "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";

export interface TimeWindow {
  start_hour: number;
  start_minute: number;
  end_hour: number;
  end_minute: number;
  days: Weekday[];
}

export interface SyncSchedule {
  enabled: boolean;
  interval_secs: number;
  time_window: TimeWindow | null;
  paused: boolean;
  last_sync: string | null;
}

// Phase 3A+: Parallel Transfer
export type TransferAction = "upload" | "download" | "mkdir" | "delete";

export interface SyncTransferEntry {
  relative_path: string;
  action: TransferAction;
  local_path: string;
  remote_path: string;
  expected_size: number;
  is_dir: boolean;
}

export interface ParallelTransferError {
  relative_path: string;
  action: TransferAction;
  error: string;
  retryable: boolean;
}

export interface ParallelSyncResult {
  uploaded: number;
  downloaded: number;
  deleted: number;
  skipped: number;
  errors: ParallelTransferError[];
  duration_ms: number;
  streams_used: number;
}

// Phase 3A+: Watcher Status
export interface WatcherStatus {
  available: boolean;
  native_backend: string;
  inotify_capacity: {
    subdirectory_count: number;
    should_warn: boolean;
    should_fallback_to_poll: boolean;
  } | null;
}

// Transfer optimization hints (per-provider capabilities)
export interface TransferOptimizationHints {
  supports_multipart: boolean;
  multipart_threshold: number;
  multipart_part_size: number;
  multipart_max_parallel: number;
  supports_resume_download: boolean;
  supports_resume_upload: boolean;
  supports_server_checksum: boolean;
  preferred_checksum_algo: string | null;
  supports_compression: boolean;
  supports_delta_sync: boolean;
  delta_sync_eligible: boolean;
  delta_sync_active: boolean;
  delta_sync_note: string | null;
}

// Transfer capability descriptor (mirrors Rust transfer_dag::TransferCapabilities)
export type Capability =
  | "unsupported"
  | "supported"
  | "supported_after_probe"
  | "experimental";

export interface TransferCapabilities {
  file_parallel: Capability;
  session_pool: Capability;
  strict_concurrent_range_download: Capability;
  resume_download: Capability;
  resume_upload: Capability;
  multipart_upload: Capability;
  offset_upload: Capability;
  upload_session: Capability;
  server_side_copy: Capability;
  list_parallel: Capability;
  batch_list: Capability;
  server_checksum: Capability;
  atomic_rename: Capability;
  rate_limited_api: Capability;
  max_file_slots: number | null;
  max_chunk_slots: number | null;
  max_checker_slots: number | null;
  preferred_chunk_size: number | null;
}

export interface DeltaServerIdentity {
  protocol: ProviderType;
  host: string;
  port: number;
  username: string;
}

export interface DeltaEligibilityProbeResult {
  eligible: boolean;
  reason: string | null;
  server_identity: DeltaServerIdentity | null;
}

// Multi-Path Sync (#52)
export interface PathPair {
  id: string;
  name: string;
  local_path: string;
  remote_path: string;
  enabled: boolean;
  exclude_overrides: string[];
}

export interface MultiPathConfig {
  pairs: PathPair[];
  parallel_pairs: boolean;
}

// AeroCloud multiple pairs (separate store cloud_pairs.json; do not share with AeroSync PathPair)
export type CloudSyncDirection = 'bidirectional' | 'local_to_remote' | 'remote_to_local';

export interface CloudVersioningStrategy {
  type: 'disabled' | 'trash_can' | 'simple' | 'staggered';
  max_age_days?: number;
  max_copies?: number;
}

export interface CloudPathPair {
  id: string;
  name: string;
  local_path: string;
  remote_path: string;
  enabled: boolean;
  server_profile: string;
  protocol_type: string;
  connection_params: Record<string, unknown>;
  sync_direction: CloudSyncDirection;
  preserve_remote_deletes: boolean;
  compress_enabled: boolean;
  compress_level: number;
  conflict_strategy: string;
  versioning_strategy: CloudVersioningStrategy;
  excluded_folders: string[];
  exclude_patterns: string[];
  last_sync: string | null;
}

export interface CloudPairsConfig {
  pairs: CloudPathPair[];
  parallel_pairs: boolean;
}

// AeroCloud overlay stack: per-config/pair AeroCompress wraps the optional
// profile-bound AeroCrypt layer at sync time.


// Sync Script Export (T-AEROSYNC-SCRIPT-EXPORT)
export type SyncScriptFormat = "bash" | "pwsh";

export interface SyncScriptMeta {
  schema: number;
  profile_id: string;
  profile_name: string;
  local_path: string;
  remote_path: string;
  direction: SyncDirection;
  delete_orphans: boolean;
  exclude_patterns: string[];
  retries: number | null;
  retries_sleep: string | null;
}

// Sync Templates (#153)
export interface SyncTemplate {
  schema_version: number;
  name: string;
  description: string;
  created_by: string;
  path_patterns: { local: string; remote: string }[];
  profile: {
    direction: SyncDirection;
    compare_timestamp: boolean;
    compare_size: boolean;
    compare_checksum: boolean;
    delete_orphans: boolean;
    parallel_streams: number;
    compression_mode: CompressionMode;
  };
  exclude_patterns: string[];
  schedule: SyncSchedule | null;
}

// Rollback Snapshots (#154)
export interface SyncSnapshot {
  id: string;
  created_at: string;
  local_path: string;
  remote_path: string;
  files: Record<string, FileSnapshotEntry>;
}

export interface FileSnapshotEntry {
  size: number;
  modified: string | null;
  checksum: string | null;
  action_taken: string;
}

export interface RestoreSnapshotResult {
  restored_from_remote: number;
  restored_to_remote: number;
  skipped: number;
  failed: string[];
}

// Delta Sync (#155)
export interface DeltaResult {
  block_size: number;
  source_size: number;
  dest_size: number;
  copy_blocks: number;
  literal_bytes: number;
  total_delta_bytes: number;
  savings_ratio: number;
  should_use_delta: boolean;
}

export interface VerifyResult {
  path: string;
  passed: boolean;
  policy: VerifyPolicy;
  expected_size: number;
  actual_size: number | null;
  size_match: boolean;
  mtime_match: boolean | null;
  hash_match: boolean | null;
  message: string | null;
}

export type JournalEntryStatus =
  | "pending"
  | "in_progress"
  | "completed"
  | "failed"
  | "skipped"
  | "verify_failed";

export type SyncEcStatus =
  | "generated"
  | "verified"
  | "repaired"
  | "skipped_too_large"
  | "generate_failed"
  | "missing_sidecar"
  | "missing_expected_hash"
  | "verify_failed";

export interface SyncJournalEntry {
  relative_path: string;
  action: string;
  status: JournalEntryStatus;
  attempts: number;
  last_error: SyncErrorInfo | null;
  verified: boolean | null;
  bytes_transferred: number;
  ec_status?: SyncEcStatus | null;
}

export interface SyncJournal {
  id: string;
  created_at: string;
  updated_at: string;
  local_path: string;
  remote_path: string;
  direction: SyncDirection;
  retry_policy: RetryPolicy;
  verify_policy: VerifyPolicy;
  entries: SyncJournalEntry[];
  completed: boolean;
}

export interface JournalSummary {
  local_path: string;
  remote_path: string;
  created_at: string;
  updated_at: string;
  total_entries: number;
  completed_entries: number;
  completed: boolean;
}

// Archive browsing types
export interface ArchiveEntry {
  name: string;
  size: number;
  compressedSize: number;
  isDir: boolean;
  isEncrypted: boolean;
  modified: string | null;
}

export type ArchiveType = "zip" | "7z" | "tar" | "rar";

export interface AeroVaultMeta {
  version: number;
  created: string;
  modified: string;
  description: string | null;
  fileCount: number;
}
