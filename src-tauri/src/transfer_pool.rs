// AeroSync Parallel Transfer Pool
// Semaphore-bounded parallel transfer engine with compression support

use serde::{Deserialize, Serialize};

/// Configuration for parallel transfer streams
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelTransferConfig {
    /// Number of concurrent transfer streams (1-8, default: 3)
    pub max_streams: u8,
    /// Timeout in ms to acquire a semaphore permit (0 = no timeout)
    pub acquire_timeout_ms: u64,
}

impl Default for ParallelTransferConfig {
    fn default() -> Self {
        Self {
            max_streams: 3,
            acquire_timeout_ms: 30000,
        }
    }
}

/// Transfer action type for parallel sync entries
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TransferAction {
    Upload,
    Download,
    Mkdir,
    Delete,
}

/// A single transfer entry to be processed in parallel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncTransferEntry {
    pub relative_path: String,
    pub action: TransferAction,
    pub local_path: String,
    pub remote_path: String,
    pub expected_size: u64,
    pub is_dir: bool,
}

/// Result of a parallel sync execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelSyncResult {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub skipped: u32,
    pub errors: Vec<ParallelTransferError>,
    pub duration_ms: u64,
    pub streams_used: u8,
}

impl ParallelSyncResult {
    pub fn new() -> Self {
        Self {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            errors: Vec::new(),
            duration_ms: 0,
            streams_used: 0,
        }
    }
}

impl Default for ParallelSyncResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Error for a single transfer in the parallel batch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParallelTransferError {
    pub relative_path: String,
    pub action: TransferAction,
    pub error: String,
    pub retryable: bool,
}

/// Trigger types for the sync engine
#[derive(Debug, Clone)]
pub enum SyncTrigger {
    /// Periodic poll (full directory scan)
    Scheduled,
    /// Filesystem watcher detected changes (incremental sync)
    FileChanged(Vec<std::path::PathBuf>),
    /// User clicked "Sync Now" button
    Manual,
    /// Stop the sync engine
    Stop,
}

/// Compression mode for transfers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionMode {
    /// Smart detection: skip already-compressed files
    Auto,
    /// Always compress
    On,
    /// Never compress (default)
    Off,
}

impl Default for CompressionMode {
    fn default() -> Self {
        Self::Off
    }
}

/// File extensions that are already compressed (compression counterproductive)
const PRECOMPRESSED_EXTENSIONS: &[&str] = &[
    "zip", "gz", "bz2", "xz", "7z", "rar", "zst", "lz4", "br", "jpg", "jpeg", "png", "gif",
    "webp", "avif", "heic", "heif", "mp3", "mp4", "mkv", "avi", "mov", "flac", "aac", "ogg",
    "opus", "m4a", "m4v", "webm", "pdf", "docx", "xlsx", "pptx", "wasm", "woff", "woff2",
    "aerovault",
];

/// Validate parallel transfer config, clamping values to safe ranges
pub fn validate_config(config: &mut ParallelTransferConfig) {
    // Clamp max_streams to 1-8
    if config.max_streams < 1 {
        config.max_streams = 1;
    } else if config.max_streams > 8 {
        config.max_streams = 8;
    }

    // Set default timeout if 0
    if config.acquire_timeout_ms == 0 {
        config.acquire_timeout_ms = 30000;
    }
}

/// Check if a file should be compressed based on its extension.
/// Returns false for already-compressed formats (zip, gz, jpg, mp4, etc.)
pub fn should_compress(filename: &str) -> bool {
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase());

    match extension {
        Some(ext) => !PRECOMPRESSED_EXTENSIONS.contains(&ext.as_str()),
        None => true, // No extension → allow compression
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_config_clamp_high() {
        let mut config = ParallelTransferConfig {
            max_streams: 10,
            acquire_timeout_ms: 5000,
        };
        validate_config(&mut config);
        assert_eq!(config.max_streams, 8);
        assert_eq!(config.acquire_timeout_ms, 5000);
    }

    #[test]
    fn test_validate_config_clamp_low() {
        let mut config = ParallelTransferConfig {
            max_streams: 0,
            acquire_timeout_ms: 1000,
        };
        validate_config(&mut config);
        assert_eq!(config.max_streams, 1);
        assert_eq!(config.acquire_timeout_ms, 1000);
    }

    #[test]
    fn test_should_compress_text() {
        assert!(should_compress("readme.txt"));
    }

    #[test]
    fn test_should_compress_zip() {
        assert!(!should_compress("archive.zip"));
    }

    #[test]
    fn test_should_compress_image() {
        assert!(!should_compress("photo.jpg"));
    }

    #[test]
    fn test_should_compress_no_extension() {
        assert!(should_compress("Makefile"));
    }

    #[test]
    fn test_should_compress_case_insensitive() {
        assert!(!should_compress("data.ZIP"));
    }

    #[test]
    fn test_parallel_sync_result_new() {
        let result = ParallelSyncResult::new();
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.downloaded, 0);
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.errors.len(), 0);
        assert_eq!(result.duration_ms, 0);
        assert_eq!(result.streams_used, 0);
    }

    #[test]
    fn test_compression_mode_serde() {
        let modes = vec![
            CompressionMode::Auto,
            CompressionMode::On,
            CompressionMode::Off,
        ];

        for mode in modes {
            let serialized = serde_json::to_string(&mode).unwrap();
            let deserialized: CompressionMode = serde_json::from_str(&serialized).unwrap();
            assert_eq!(mode, deserialized);
        }
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = ParallelTransferConfig {
            max_streams: 5,
            acquire_timeout_ms: 15000,
        };

        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: ParallelTransferConfig = serde_json::from_str(&serialized).unwrap();

        assert_eq!(config.max_streams, deserialized.max_streams);
        assert_eq!(
            config.acquire_timeout_ms,
            deserialized.acquire_timeout_ms
        );
    }
}
