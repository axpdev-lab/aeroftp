// Remote file from FTP server
export interface RemoteFile {
  name: string;
  path: string;
  size: number | null;
  is_dir: boolean;
  modified: string | null;
  permissions: string | null;
}

export interface FileListResponse {
  files: RemoteFile[];
  current_path: string;
}

// Supported storage provider types
export type ProviderType = 'ftp' | 'ftps' | 'sftp' | 'webdav' | 's3' | 'aerocloud' | 'googledrive' | 'dropbox' | 'onedrive' | 'mega' | 'box' | 'pcloud' | 'azure' | 'filen' | 'fourshared';

// Check if a provider type requires OAuth2 authentication
export const isOAuthProvider = (type: ProviderType): boolean => {
  return type === 'googledrive' || type === 'dropbox' || type === 'onedrive' || type === 'box' || type === 'pcloud';
};

// Check if a provider type requires OAuth 1.0 authentication (4shared)
export const isFourSharedProvider = (type: ProviderType): boolean => {
  return type === 'fourshared';
};

// Check if a provider type is AeroCloud
export const isAeroCloudProvider = (type: ProviderType): boolean => {
  return type === 'aerocloud';
};

// Check if a provider uses non-FTP backend (provider_* Tauri commands)
export const isNonFtpProvider = (type: ProviderType): boolean => {
  return ['googledrive', 'dropbox', 'onedrive', 's3', 'webdav', 'mega', 'sftp', 'box', 'pcloud', 'azure', 'filen', 'fourshared'].includes(type);
};

// Check if a provider is a traditional FTP/FTPS connection (uses ftp_* Tauri commands)
export const isFtpProtocol = (type: ProviderType): boolean => {
  return type === 'ftp' || type === 'ftps';
};

// Check if a provider supports storage quota queries
export const supportsStorageQuota = (type: ProviderType): boolean => {
  return ['mega', 'googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'filen', 'sftp', 'webdav', 'fourshared'].includes(type);
};

// Check if a provider supports native share links
export const supportsNativeShareLink = (type: ProviderType): boolean => {
  return ['googledrive', 'dropbox', 'onedrive', 's3', 'mega', 'box', 'pcloud', 'filen'].includes(type);
};

// FTP/FTPS TLS encryption mode
export type FtpTlsMode = 'none' | 'explicit' | 'implicit' | 'explicit_if_available';

// Provider-specific configuration options
export interface ProviderOptions {
  // S3-specific
  bucket?: string;
  region?: string;
  endpoint?: string;  // For S3-compatible (MinIO, etc.)
  pathStyle?: boolean;

  // WebDAV-specific
  // (no extra options needed, uses standard auth)

  // FTP/FTPS-specific
  tlsMode?: FtpTlsMode;      // TLS encryption mode
  verifyCert?: boolean;       // Verify server certificate (default: true)

  // SFTP-specific
  private_key_path?: string;  // Path to SSH private key
  key_passphrase?: string;    // Passphrase for encrypted keys
  timeout?: number;           // Connection timeout in seconds

  // OAuth-specific (for Google Drive, Dropbox, OneDrive)
  clientId?: string;
  clientSecret?: string;

  // MEGA-specific
  save_session?: boolean;
  session_expires_at?: number; // Timestamp (ms)
  logout_on_disconnect?: boolean;

  // Azure Blob Storage-specific
  container?: string;
  accountName?: string;
  accessKey?: string;
  sasToken?: string;

  // pCloud-specific
  pcloudRegion?: 'us' | 'eu';

  // Filen-specific
  two_factor_code?: string;  // Optional TOTP 2FA code
}

export interface ConnectionParams {
  server: string;
  username: string;
  password: string;
  protocol?: ProviderType;  // Default: 'ftp'
  port?: number;            // Default based on protocol
  options?: ProviderOptions;
  displayName?: string;     // Custom name for tab display
  providerId?: string;      // Registry provider ID for logo display
}

export interface DownloadParams {
  remote_path: string;
  local_path: string;
}

export interface UploadParams {
  local_path: string;
  remote_path: string;
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
  direction: 'download' | 'upload';
  total_files?: number; // When set, transferred/total are file counts (folder transfer)
  path?: string;        // Full path for context
}

// Transfer event from backend (includes transfers and deletes)
export interface TransferEvent {
  event_type:
  // Transfer events
  | 'start' | 'scanning' | 'progress' | 'complete' | 'error' | 'cancelled'
  | 'file_start' | 'file_complete' | 'file_error' | 'file_skip'
  // Delete events
  | 'delete_start' | 'delete_complete' | 'delete_error'
  | 'delete_file_start' | 'delete_file_complete' | 'delete_file_error'
  | 'delete_dir_complete';
  transfer_id: string;
  filename: string;
  direction: 'download' | 'upload' | 'local' | 'remote';
  message?: string;
  progress?: TransferProgress;
  path?: string; // Full path for context (file or folder)
}

// Server profile for saved connections
export interface ServerProfile {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  password?: string;            // DEPRECATED: migrated to secure credential store
  hasStoredCredential?: boolean; // true if password stored in OS keyring/vault
  protocol?: ProviderType;    // Default: 'ftp'
  initialPath?: string;       // Initial remote directory to navigate after connection
  localInitialPath?: string;  // Initial local directory for this project/server
  color?: string;
  lastConnected?: string;
  options?: ProviderOptions;  // Provider-specific options (S3 bucket, etc.)
  providerId?: string;        // Registry provider ID (e.g. 'cloudflare-r2', 'koofr')
}

// Session status for multi-tab management
export type SessionStatus = 'connected' | 'disconnected' | 'connecting' | 'cached';

// FTP Session for multi-session tabs (Hybrid Cache Architecture)
export interface FtpSession {
  id: string;
  serverId: string;              // Reference to ServerProfile.id
  serverName: string;            // Display name for tab
  status: SessionStatus;
  remotePath: string;
  localPath: string;
  remoteFiles: RemoteFile[];     // Cached file list
  localFiles: LocalFile[];       // Cached local files
  lastActivity: Date;
  connectionParams: ConnectionParams;
  providerId?: string;        // Registry provider ID for logo display
  // Per-session navigation sync state
  isSyncNavigation?: boolean;
  syncBasePaths?: { remote: string; local: string } | null;
}

// State for managing multiple tabs
export interface TabsState {
  sessions: FtpSession[];
  activeSessionId: string | null;
}

// ============ Sync Types ============

export type SyncStatus =
  | 'identical'
  | 'local_newer'
  | 'remote_newer'
  | 'local_only'
  | 'remote_only'
  | 'conflict'
  | 'size_mismatch';

export type SyncDirection =
  | 'local_to_remote'
  | 'remote_to_local'
  | 'bidirectional';

export type SyncAction =
  | 'upload'
  | 'download'
  | 'delete_local'
  | 'delete_remote'
  | 'skip'
  | 'ask_user';

export interface FileInfo {
  name: string;
  path: string;
  size: number;
  modified: string | null;
  is_dir: boolean;
  checksum: string | null;
}

export interface FileComparison {
  relative_path: string;
  status: SyncStatus;
  local_info: FileInfo | null;
  remote_info: FileInfo | null;
  is_dir: boolean;
}

export interface CompareOptions {
  compare_timestamp: boolean;
  compare_size: boolean;
  compare_checksum: boolean;
  exclude_patterns: string[];
  direction: SyncDirection;
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
  | 'network'
  | 'auth'
  | 'path_not_found'
  | 'permission_denied'
  | 'quota_exceeded'
  | 'rate_limit'
  | 'timeout'
  | 'file_locked'
  | 'disk_error'
  | 'unknown';

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

export type VerifyPolicy = 'none' | 'size_only' | 'size_and_mtime' | 'full';

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
  | 'pending'
  | 'in_progress'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'verify_failed';

export interface SyncJournalEntry {
  relative_path: string;
  action: string;
  status: JournalEntryStatus;
  attempts: number;
  last_error: SyncErrorInfo | null;
  verified: boolean | null;
  bytes_transferred: number;
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

// Archive browsing types
export interface ArchiveEntry {
  name: string;
  size: number;
  compressedSize: number;
  isDir: boolean;
  isEncrypted: boolean;
  modified: string | null;
}

export type ArchiveType = 'zip' | '7z' | 'tar' | 'rar';

export interface AeroVaultMeta {
  version: number;
  created: string;
  modified: string;
  description: string | null;
  fileCount: number;
}
