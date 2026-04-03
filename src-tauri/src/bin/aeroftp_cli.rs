//! AeroFTP CLI — Production multi-protocol file transfer client
//!
//! Usage:
//!   aeroftp connect <url>                     Test connection
//!   aeroftp ls <url> [path] [-l]              List files
//!   aeroftp get <url> <remote> [local] [-r] [--segments N]  Download file(s)
//!   aeroftp put <url> <local> [remote] [-r]   Upload file(s) (glob: "*.csv")
//!   aeroftp mkdir <url> <path>                Create directory
//!   aeroftp rm <url> <path> [-rf]             Delete file/directory
//!   aeroftp mv <url> <from> <to>              Rename/move
//!   aeroftp cp <url> <from> <to>              Server-side copy when supported
//!   aeroftp link <url> <path>                 Create a share link when supported
//!   aeroftp edit <url> <path> <find> <replace> Replace text in a remote UTF-8 file
//!   aeroftp cat <url> <path>                  Print to stdout
//!   aeroftp head <url> <path> [-n 20]         Print first N lines
//!   aeroftp tail <url> <path> [-n 20]         Print last N lines
//!   aeroftp touch <url> <path>                Create empty file or update timestamp
//!   aeroftp hashsum <algo> <url> <path>       Compute file hash (md5/sha1/sha256/sha512/blake3)
//!   aeroftp check <url> <local> <remote>      Verify local/remote directories match
//!   aeroftp stat <url> <path>                 File metadata
//!   aeroftp find <url> <path> <pattern>       Search files
//!   aeroftp df <url>                          Storage quota
//!   aeroftp tree <url> [path] [-d depth]      Directory tree
//!   aeroftp about <url>                       Server info and storage
//!   aeroftp dedupe <url> [path]               Find duplicate files
//!   aeroftp sync <url> <local> <remote>       Sync directories
//!   aeroftp batch <file>                      Execute .aeroftp script
//!   aeroftp rcat <url> <remote>               Upload stdin directly to remote file
//!   aeroftp serve http <url> [path]           Serve remote files over local HTTP
//!   aeroftp serve webdav <url> [path]          Serve remote files over local WebDAV (read-write)
//!
//! URL format: protocol://user:pass@host:port/path
//! Add --json for machine-readable output.
//!
//! Exit codes:
//!   0  Success
//!   1  Connection/network error
//!   2  Not found
//!   3  Permission denied
//!   4  Transfer failed / partial
//!   5  Invalid config / usage error
//!   6  Authentication failed
//!   7  Not supported
//!   8  Timeout
//!   99 Unknown error

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use base64::Engine as _;
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path as AxumPath, State},
    http::{
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE},
        HeaderMap, HeaderValue, Method, StatusCode,
    },
    response::{Html, IntoResponse, Response},
    routing::{any, get},
    Router,
};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use ftp_client_gui_lib::providers::{
    ProviderConfig, ProviderError, ProviderFactory, ProviderType, RemoteEntry, StorageProvider,
    ShareLinkOptions, MAX_DOWNLOAD_TO_BYTES,
};
use futures_util::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, IsTerminal, Read, Write as IoWrite};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tempfile::NamedTempFile;
use tokio::sync::Mutex as AsyncMutex;

// ── CLI Argument Parsing ───────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "aeroftp",
    about = "AeroFTP CLI — Multi-protocol file transfer client",
    version,
    long_about = "Direct URL schemes: FTP, FTPS, SFTP, WebDAV(S), S3, MEGA, Azure, Filen, Internxt, Jottacloud, FileLu, Koofr, OpenDrive, Yandex Disk, GitHub.\nSaved profiles additionally cover Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, 4shared, and Drime.\n\nConnect via saved profiles (--profile) or URL (protocol://user@host:port/path).\nAI agents: run 'aeroftp agent-info --json' for structured capability discovery.",
    after_help = "EXAMPLES (profiles — no credentials needed):\n  aeroftp-cli profiles                                    List saved servers\n  aeroftp-cli ls --profile \"My Server\" /var/www/ -l        List files\n  aeroftp-cli put --profile \"Production\" ./app.js /www/    Upload file\n  aeroftp-cli get --profile \"NAS\" /backups/db.sql ./       Download file\n  aeroftp-cli sync --profile \"Staging\" ./build/ /www/ --dry-run\n  aeroftp-cli agent-info --json                            AI agent discovery\n\nEXAMPLES (URL mode):\n  aeroftp-cli connect sftp://user@myserver.com\n  aeroftp-cli ls sftp://user@myserver.com /var/www/ -l\n  aeroftp-cli get sftp://user@host \"/data/*.csv\"\n  aeroftp-cli cat sftp://user@host /config.ini | grep DB_HOST\n  aeroftp-cli batch deploy.aeroftp\n\nEXIT CODES:\n  0  Success                    5  Invalid config/usage\n  1  Connection/network error   6  Authentication failed\n  2  Not found                  7  Not supported\n  3  Permission denied          8  Timeout\n  4  Transfer failed/partial   99  Unknown error"
)]
struct Cli {
    /// Output format
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Shorthand for --format json
    #[arg(long, global = true)]
    json: bool,

    /// Restrict JSON output fields (comma-separated, e.g. name,size,modified)
    #[arg(long, global = true)]
    json_fields: Option<String>,

    /// Read password from stdin (pipe: echo "pass" | aeroftp ...)
    #[arg(long, global = true)]
    password_stdin: bool,

    /// SSH private key path for SFTP
    #[arg(long, global = true)]
    key: Option<String>,

    /// SSH key passphrase
    #[arg(long, global = true)]
    key_passphrase: Option<String>,

    /// S3 bucket name
    #[arg(long, global = true)]
    bucket: Option<String>,

    /// S3/Azure region
    #[arg(long, global = true)]
    region: Option<String>,

    /// Azure container name
    #[arg(long, global = true)]
    container: Option<String>,

    /// Bearer/API token (kDrive, Jottacloud, FileLu)
    #[arg(long, global = true, env = "AEROFTP_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// FTP TLS mode: none, explicit, implicit, explicit_if_available
    #[arg(long, global = true)]
    tls: Option<String>,

    /// Skip TLS certificate verification
    #[arg(long, global = true)]
    insecure: bool,

    /// Trust unknown SSH host keys (skip TOFU verification)
    #[arg(long, global = true)]
    trust_host_key: bool,

    /// 2FA code (Filen, Internxt)
    #[arg(long, global = true, env = "AEROFTP_2FA", hide_env_values = true)]
    two_factor: Option<String>,

    /// Use a saved server profile instead of URL (name or ID)
    #[arg(long, short = 'P', global = true)]
    profile: Option<String>,

    /// Master password for encrypted vault (or set AEROFTP_MASTER_PASSWORD)
    #[arg(long, global = true, env = "AEROFTP_MASTER_PASSWORD", hide_env_values = true)]
    master_password: Option<String>,

    /// Verbose output (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Quiet mode (errors only)
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Speed limit (e.g., "1M", "500K")
    #[arg(long, global = true)]
    limit_rate: Option<String>,

    /// Bandwidth schedule (e.g., "08:00,512k 12:00,10M 18:00,off")
    #[arg(long, global = true)]
    bwlimit: Option<String>,

    /// Number of parallel transfer workers (default: 4, max: 32)
    #[arg(long, global = true, default_value_t = 4)]
    parallel: usize,

    /// Resume interrupted transfers using partial files or remote offsets when supported
    #[arg(long, global = true)]
    partial: bool,

    // ── Filter flags (apply to ls, get, put, sync, find, rm) ──

    /// Include only files matching glob pattern (repeatable)
    #[arg(long, global = true)]
    include: Vec<String>,

    /// Exclude files matching glob pattern (repeatable)
    #[arg(long, global = true)]
    exclude_global: Vec<String>,

    /// Read include patterns from file (one per line, # comments)
    #[arg(long, global = true)]
    include_from: Option<String>,

    /// Read exclude patterns from file (one per line, # comments)
    #[arg(long, global = true)]
    exclude_from: Option<String>,

    /// Minimum file size (e.g., "100k", "1M", "1G")
    #[arg(long, global = true)]
    min_size: Option<String>,

    /// Maximum file size (e.g., "100k", "1M", "1G")
    #[arg(long, global = true)]
    max_size: Option<String>,

    /// Skip files newer than duration (e.g., "7d", "24h", "2w")
    #[arg(long, global = true)]
    min_age: Option<String>,

    /// Skip files older than duration (e.g., "7d", "24h", "2w")
    #[arg(long, global = true)]
    max_age: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

impl Cli {
    fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
    Blake3,
}

#[derive(Subcommand)]
enum Commands {
    /// Test connection to a remote server
    Connect {
        /// Server URL (e.g., sftp://user@host:22). Omit when using --profile.
        #[arg(default_value = "_")]
        url: String,
    },
    /// List files on a remote server
    Ls {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote path (default: /)
        #[arg(default_value = "/")]
        path: String,
        /// Long listing format (permissions, size, date)
        #[arg(short, long)]
        long: bool,
        /// Sort by: name, size, date
        #[arg(short, long, default_value = "name")]
        sort: String,
        /// Reverse sort order
        #[arg(short, long)]
        reverse: bool,
        /// Show all files (including hidden)
        #[arg(short, long)]
        all: bool,
    },
    /// Download file(s) from remote server
    Get {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path (supports glob patterns like "*.csv")
        #[arg(default_value = "")]
        remote: String,
        /// Local destination (default: current filename)
        local: Option<String>,
        /// Recursive download (directories)
        #[arg(short, long)]
        recursive: bool,
        /// Segmented parallel download: split file into N chunks (2-16, default: 1 = off)
        #[arg(long, default_value_t = 1)]
        segments: usize,
    },
    /// Upload file(s) to remote server (supports glob patterns like "*.csv")
    Put {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Local file path (supports glob patterns like "*.csv")
        #[arg(default_value = "")]
        local: String,
        /// Remote destination path
        remote: Option<String>,
        /// Recursive upload (directories)
        #[arg(short, long)]
        recursive: bool,
    },
    /// Create a remote directory
    Mkdir {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote directory path
        #[arg(default_value = "")]
        path: String,
    },
    /// Delete a remote file or directory
    Rm {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote path to delete
        #[arg(default_value = "")]
        path: String,
        /// Recursive delete (directories)
        #[arg(short, long)]
        recursive: bool,
        /// Force (no confirmation for recursive)
        #[arg(short, long)]
        force: bool,
    },
    /// Rename/move a remote file
    Mv {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Source path
        #[arg(default_value = "")]
        from: String,
        /// Destination path
        #[arg(default_value = "")]
        to: String,
    },
    /// Copy a remote file on the server side when supported
    Cp {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Source path
        #[arg(default_value = "")]
        from: String,
        /// Destination path
        #[arg(default_value = "")]
        to: String,
    },
    /// Create a share link for a remote file when supported
    Link {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote path
        #[arg(default_value = "")]
        path: String,
        /// Link expiration (e.g. 1h, 24h, 7d, 30d, or seconds)
        #[arg(long)]
        expires: Option<String>,
        /// Password-protect the link (provider support required)
        #[arg(long)]
        password: Option<String>,
        /// Permission level: view, edit, comment (provider support required)
        #[arg(long, default_value = "view")]
        permissions: String,
    },
    /// Find and replace text in a remote UTF-8 file
    Edit {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
        /// Exact text to find
        #[arg(default_value = "")]
        find: String,
        /// Replacement text
        #[arg(default_value = "")]
        replace: String,
        /// Replace only the first occurrence
        #[arg(long)]
        first: bool,
    },
    /// Print remote file to stdout (for piping)
    Cat {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
    },
    /// Print first N lines of a remote file
    Head {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
        /// Number of lines to print (default: 20)
        #[arg(short = 'n', long, default_value = "20")]
        lines: usize,
    },
    /// Print last N lines of a remote file
    Tail {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
        /// Number of lines to print (default: 20)
        #[arg(short = 'n', long, default_value = "20")]
        lines: usize,
    },
    /// Create empty file or update timestamp
    Touch {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
        /// Timestamp override (ISO 8601)
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Compute hash of remote file(s)
    Hashsum {
        /// Hash algorithm
        #[arg(value_enum)]
        algorithm: HashAlgorithm,
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
        /// Download and hash locally
        #[arg(long)]
        download: bool,
    },
    /// Verify local and remote directories are identical
    Check {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Local directory
        #[arg(default_value = ".")]
        local: String,
        /// Remote directory
        #[arg(default_value = "/")]
        remote: String,
        /// Use checksums instead of size/mtime
        #[arg(long)]
        checksum: bool,
        /// Only check files present locally
        #[arg(long)]
        one_way: bool,
    },
    /// Show file/directory metadata
    Stat {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote path
        #[arg(default_value = "")]
        path: String,
    },
    /// Search for files by pattern
    Find {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Base path to search from
        #[arg(default_value = "/")]
        path: String,
        /// Search pattern (glob-style)
        #[arg(default_value = "*")]
        pattern: String,
    },
    /// Show storage quota/usage
    Df {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
    },
    /// Show detailed server info, account, and storage quota
    About {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
    },
    /// Measure upload/download throughput against a writable remote
    Speed {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Test file size (e.g. 1M, 8M, 64M)
        #[arg(long, default_value = "8M")]
        test_size: String,
        /// Number of upload/download iterations
        #[arg(long, default_value = "1")]
        iterations: u32,
        /// Remote path override for the temporary benchmark file
        #[arg(long)]
        remote_path: Option<String>,
    },
    /// Find and resolve duplicate files by content hash
    Dedupe {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote path to scan
        #[arg(default_value = "/")]
        path: String,
        /// Resolution mode: interactive, skip, newest, oldest, largest, smallest
        #[arg(long, default_value = "skip")]
        mode: String,
        /// Preview only (don't delete)
        #[arg(long)]
        dry_run: bool,
    },
    /// Synchronize local and remote directories
    Sync {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Local directory path
        #[arg(default_value = ".")]
        local: String,
        /// Remote directory path
        #[arg(default_value = "/")]
        remote: String,
        /// Sync direction: upload, download, both
        #[arg(long, default_value = "both")]
        direction: String,
        /// Dry run (show what would happen without executing)
        #[arg(long)]
        dry_run: bool,
        /// Delete orphaned files on destination
        #[arg(long)]
        delete: bool,
        /// Exclude patterns (can repeat: --exclude "*.tmp" --exclude ".git")
        #[arg(long, short)]
        exclude: Vec<String>,
        /// Detect renamed files by hash to avoid re-upload
        #[arg(long)]
        track_renames: bool,
        /// Safety limit: abort if more than N files (or N%) would be deleted
        #[arg(long)]
        max_delete: Option<String>,
        /// Move overwritten/deleted files to backup directory
        #[arg(long)]
        backup_dir: Option<String>,
        /// Suffix for backup files (e.g., ".bak")
        #[arg(long, default_value = "")]
        backup_suffix: String,
    },
    /// Display remote directory tree
    Tree {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote path (default: /)
        #[arg(default_value = "/")]
        path: String,
        /// Maximum depth (default: 3)
        #[arg(short = 'd', long, default_value = "3")]
        max_depth: usize,
    },
    /// Execute commands from a batch script (.aeroftp file)
    Batch {
        /// Path to .aeroftp script file
        file: String,
    },
    /// Upload stdin directly to a remote file
    Rcat {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote destination path
        #[arg(default_value = "")]
        remote: String,
    },
    /// Serve a remote over a local protocol
    Serve {
        #[command(subcommand)]
        command: ServeCommands,
    },
    /// Manage CLI aliases stored in config.toml
    Alias {
        #[command(subcommand)]
        command: AliasCommands,
    },
    /// AeroAgent — AI-powered interactive agent with tool execution
    Agent {
        /// One-shot message (run and exit)
        #[arg(short, long)]
        message: Option<String>,

        /// AI provider (anthropic, openai, gemini, ollama, etc.)
        #[arg(short, long)]
        provider: Option<String>,

        /// Model override
        #[arg(long)]
        model: Option<String>,

        /// Pre-connect to server on startup
        #[arg(short, long)]
        connect: Option<String>,

        /// Auto-approve tool calls: safe, medium, high, all
        #[arg(long, default_value = "safe")]
        auto_approve: String,

        /// Max autonomous steps (default: 10)
        #[arg(long, default_value = "10")]
        max_steps: u32,

        /// Orchestration mode (JSON-RPC 2.0 over stdin/stdout)
        #[arg(long)]
        orchestrate: bool,

        /// MCP server mode (Model Context Protocol)
        #[arg(long)]
        mcp: bool,

        /// Read message from stdin
        #[arg(long)]
        stdin: bool,

        /// Auto-approve all tools (equivalent to --auto-approve all)
        #[arg(long, short = 'y')]
        yes: bool,

        /// Plan only — show execution plan without running
        #[arg(long)]
        plan_only: bool,

        /// Cost limit in USD (stop when exceeded)
        #[arg(long)]
        cost_limit: Option<f64>,

        /// Custom system prompt (or @file.txt to load from file)
        #[arg(long)]
        system: Option<String>,
    },
    /// Generate shell completions (bash, zsh, fish, elvish, powershell)
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// List saved server profiles from the encrypted vault
    Profiles,
    /// List configured AI providers and models from the encrypted vault
    AiModels,
    /// Show CLI capabilities for AI agent discovery (always JSON)
    AgentInfo,
}

#[derive(Subcommand)]
enum AliasCommands {
    /// Set or update an alias
    Set {
        /// Alias name
        name: String,
        /// Command tokens that the alias expands to
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Remove an alias
    Remove {
        /// Alias name
        name: String,
    },
    /// Show one alias
    Show {
        /// Alias name
        name: String,
    },
    /// List all aliases
    List,
}

#[derive(Subcommand)]
enum ServeCommands {
    /// Serve a remote over local HTTP (read-only)
    Http {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote base path to expose (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Local bind address
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
    },
    /// Serve a remote over local WebDAV (read-write)
    #[command(name = "webdav")]
    WebDav {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_")]
        url: String,
        /// Remote base path to expose (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Local bind address
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
    },
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CliConfigFile {
    #[serde(default)]
    defaults: CliDefaults,
    #[serde(default)]
    aliases: HashMap<String, Vec<String>>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CliDefaults {
    format: Option<String>,
    json: Option<bool>,
    profile: Option<String>,
    parallel: Option<usize>,
    partial: Option<bool>,
    quiet: Option<bool>,
    verbose: Option<u8>,
    limit_rate: Option<String>,
    bwlimit: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CliUpdateCache {
    checked_at: Option<String>,
    latest_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseInfo {
    tag_name: String,
}

// ── Serializable Output Types ──────────────────────────────────────

#[derive(Serialize)]
struct CliConnectResult {
    status: &'static str,
    protocol: String,
    host: String,
    port: u16,
    username: String,
    server_info: Option<String>,
    elapsed_ms: u64,
}

#[derive(Serialize)]
struct CliFileEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
    modified: Option<String>,
    permissions: Option<String>,
    owner: Option<String>,
}

#[derive(Serialize)]
struct CliTransferResult {
    status: &'static str,
    operation: String,
    path: String,
    bytes: u64,
    elapsed_secs: f64,
    speed_bps: u64,
}

#[derive(Serialize)]
struct CliSpeedResult {
    status: &'static str,
    remote_path: String,
    test_size: u64,
    iterations: u32,
    upload_speed_bps: u64,
    download_speed_bps: u64,
    elapsed_secs: f64,
}

#[derive(Serialize)]
struct CliStorageResult {
    status: &'static str,
    used: u64,
    total: u64,
    free: u64,
    used_percent: f64,
}

#[derive(Serialize)]
struct CliSyncResult {
    status: &'static str,
    uploaded: u32,
    downloaded: u32,
    deleted: u32,
    skipped: u32,
    errors: Vec<String>,
    elapsed_secs: f64,
}

#[derive(Serialize)]
struct CliError {
    status: &'static str,
    error: String,
    code: i32,
}

#[derive(Serialize)]
struct CliOk {
    status: &'static str,
    message: String,
}

#[derive(Serialize)]
struct CliHashResult {
    status: &'static str,
    algorithm: String,
    hash: String,
    path: String,
    size: u64,
}

#[derive(Serialize)]
struct CliCheckResult {
    status: &'static str,
    match_count: u32,
    differ_count: u32,
    missing_local: u32,
    missing_remote: u32,
    elapsed_secs: f64,
    details: Vec<CliCheckEntry>,
}

#[derive(Serialize)]
struct CliCheckEntry {
    path: String,
    status: String,
    local_size: Option<u64>,
    remote_size: Option<u64>,
}

// ── Helpers ────────────────────────────────────────────────────────

fn cli_config_path() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "Cannot resolve config directory".to_string())?;
    Ok(base.join("aeroftp").join("config.toml"))
}

fn cli_state_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "Cannot resolve config directory".to_string())?;
    Ok(base.join("aeroftp"))
}

fn cli_update_cache_path() -> Result<PathBuf, String> {
    Ok(cli_state_dir()?.join("update-check.toml"))
}

fn load_cli_config() -> Result<CliConfigFile, String> {
    let path = cli_config_path()?;
    if !path.exists() {
        return Ok(CliConfigFile::default());
    }

    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("Cannot read config '{}': {}", path.display(), e))?;
    toml::from_str(&raw)
        .map_err(|e| format!("Invalid config '{}': {}", path.display(), e))
}

fn save_cli_config(config: &CliConfigFile) -> Result<PathBuf, String> {
    let path = cli_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create config directory '{}': {}", parent.display(), e))?;
    }

    let content = toml::to_string_pretty(config)
        .map_err(|e| format!("Cannot serialize config: {}", e))?;
    std::fs::write(&path, content)
        .map_err(|e| format!("Cannot write config '{}': {}", path.display(), e))?;
    Ok(path)
}

fn load_update_cache() -> CliUpdateCache {
    let Ok(path) = cli_update_cache_path() else {
        return CliUpdateCache::default();
    };
    if !path.exists() {
        return CliUpdateCache::default();
    }

    let Ok(raw) = std::fs::read_to_string(&path) else {
        return CliUpdateCache::default();
    };

    toml::from_str(&raw).unwrap_or_default()
}

fn save_update_cache(cache: &CliUpdateCache) -> Result<PathBuf, String> {
    let path = cli_update_cache_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create config directory '{}': {}", parent.display(), e))?;
    }

    let content = toml::to_string_pretty(cache)
        .map_err(|e| format!("Cannot serialize update cache: {}", e))?;
    std::fs::write(&path, content)
        .map_err(|e| format!("Cannot write update cache '{}': {}", path.display(), e))?;
    Ok(path)
}

fn parse_json_fields(cli: &Cli) -> Option<std::collections::HashSet<String>> {
    cli.json_fields.as_ref().and_then(|raw| {
        let fields: std::collections::HashSet<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(|field| field.to_string())
            .collect();
        if fields.is_empty() {
            None
        } else {
            Some(fields)
        }
    })
}

fn filter_json_object_fields(
    mut object: serde_json::Map<String, serde_json::Value>,
    allowed: &std::collections::HashSet<String>,
    preserve: &[&str],
) -> serde_json::Value {
    object.retain(|key, _| allowed.contains(key) || preserve.contains(&key.as_str()));
    serde_json::Value::Object(object)
}

fn cli_entry_to_filtered_json(entry: &CliFileEntry, cli: &Cli) -> serde_json::Value {
    let value = serde_json::to_value(entry).unwrap_or_else(|_| serde_json::json!({}));
    let Some(allowed) = parse_json_fields(cli) else {
        return value;
    };
    match value {
        serde_json::Value::Object(object) => filter_json_object_fields(object, &allowed, &[]),
        other => other,
    }
}

fn remote_entry_to_filtered_json(entry: &RemoteEntry, cli: &Cli) -> serde_json::Value {
    cli_entry_to_filtered_json(&remote_entry_to_cli(entry), cli)
}

fn apply_top_level_json_field_filter(
    value: serde_json::Value,
    cli: &Cli,
    preserve: &[&str],
) -> serde_json::Value {
    let Some(allowed) = parse_json_fields(cli) else {
        return value;
    };
    if let serde_json::Value::Object(object) = value {
        filter_json_object_fields(object, &allowed, preserve)
    } else {
        value
    }
}

fn estimate_ai_cost_usd(provider: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let (input_per_million, output_per_million) = match provider {
        "anthropic" => (3.0, 15.0),
        "openai" => (5.0, 15.0),
        "gemini" | "google" => (0.35, 1.05),
        "xai" => (5.0, 15.0),
        "groq" => (0.59, 0.79),
        "mistral" => (2.0, 6.0),
        "deepseek" => (0.27, 1.10),
        "perplexity" => (1.0, 1.0),
        "cohere" => (3.0, 15.0),
        "together" => (0.88, 0.88),
        "fireworks" => (0.90, 0.90),
        "cerebras" => (0.85, 1.20),
        "sambanova" => (0.90, 0.90),
        "openrouter" => (5.0, 15.0),
        "kimi" | "moonshot" => (2.0, 10.0),
        "qwen" => (0.60, 0.60),
        "ai21" => (2.0, 8.0),
        "ollama" => (0.0, 0.0),
        _ => (5.0, 15.0),
    };
    (input_tokens as f64 / 1_000_000.0) * input_per_million
        + (output_tokens as f64 / 1_000_000.0) * output_per_million
}

fn normalize_release_version(raw: &str) -> Option<Version> {
    let normalized = raw.trim().trim_start_matches(['v', 'V']);
    if normalized.is_empty() {
        return None;
    }
    Version::parse(normalized).ok()
}

fn is_newer_release(latest: &str, current: &str) -> bool {
    match (normalize_release_version(latest), normalize_release_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn update_check_due(cache: &CliUpdateCache, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Some(checked_at) = &cache.checked_at else {
        return true;
    };

    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(checked_at) else {
        return true;
    };

    now.signed_duration_since(parsed.with_timezone(&chrono::Utc)) >= chrono::Duration::hours(24)
}

async fn fetch_latest_release_version() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("Cannot build HTTP client: {}", e))?;

    let release = client
        .get("https://api.github.com/repos/axpdev-lab/aeroftp/releases/latest")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(
            reqwest::header::USER_AGENT,
            format!("aeroftp-cli/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| format!("Cannot query releases API: {}", e))?
        .error_for_status()
        .map_err(|e| format!("Releases API returned error: {}", e))?
        .json::<GitHubReleaseInfo>()
        .await
        .map_err(|e| format!("Invalid releases API response: {}", e))?;

    normalize_release_version(&release.tag_name)
        .map(|version| version.to_string())
        .ok_or_else(|| format!("Invalid release tag '{}'", release.tag_name))
}

async fn maybe_check_for_updates(cli: &Cli) {
    if cli.quiet {
        return;
    }

    let now = chrono::Utc::now();
    let mut cache = load_update_cache();
    if !update_check_due(&cache, now) {
        return;
    }

    cache.checked_at = Some(now.to_rfc3339());
    match fetch_latest_release_version().await {
        Ok(latest_version) => {
            cache.latest_version = Some(latest_version.clone());
            let _ = save_update_cache(&cache);
            if is_newer_release(&latest_version, env!("CARGO_PKG_VERSION")) {
                eprintln!(
                    "Update available: v{} -> v{} (download the latest release from GitHub)",
                    env!("CARGO_PKG_VERSION"),
                    latest_version
                );
            }
        }
        Err(_) => {
            let _ = save_update_cache(&cache);
        }
    }
}

fn arg_present(args: &[String], long: &str, short: Option<&str>) -> bool {
    args.iter().skip(1).any(|arg| {
        arg == long
            || short.is_some_and(|short_flag| arg == short_flag)
            || arg.starts_with(&format!("{}=", long))
            || short.is_some_and(|short_flag| arg.starts_with(&format!("{}=", short_flag)))
    })
}

fn verbose_present(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| {
        arg == "--verbose"
            || (arg.starts_with('-')
                && arg.len() > 1
                && arg.chars().skip(1).all(|ch| ch == 'v'))
    })
}

fn apply_config_defaults(args: &[String], config: &CliConfigFile) -> Vec<String> {
    let mut merged = vec![args[0].clone()];

    if let Some(profile) = &config.defaults.profile {
        if !arg_present(args, "--profile", Some("-P")) {
            merged.push("--profile".to_string());
            merged.push(profile.clone());
        }
    }
    if let Some(format) = &config.defaults.format {
        if !arg_present(args, "--format", None) && !arg_present(args, "--json", None) {
            merged.push("--format".to_string());
            merged.push(format.clone());
        }
    }
    if config.defaults.json.unwrap_or(false) && !arg_present(args, "--json", None) && !arg_present(args, "--format", None) {
        merged.push("--json".to_string());
    }
    if let Some(parallel) = config.defaults.parallel {
        if !arg_present(args, "--parallel", None) {
            merged.push("--parallel".to_string());
            merged.push(parallel.to_string());
        }
    }
    if config.defaults.partial.unwrap_or(false) && !arg_present(args, "--partial", None) {
        merged.push("--partial".to_string());
    }
    if config.defaults.quiet.unwrap_or(false) && !arg_present(args, "--quiet", Some("-q")) {
        merged.push("--quiet".to_string());
    }
    if let Some(verbose) = config.defaults.verbose {
        if verbose > 0 && !verbose_present(args) {
            merged.push(format!("-{}", "v".repeat(verbose.min(3) as usize)));
        }
    }
    if let Some(limit_rate) = &config.defaults.limit_rate {
        if !arg_present(args, "--limit-rate", None) {
            merged.push("--limit-rate".to_string());
            merged.push(limit_rate.clone());
        }
    }
    if let Some(bwlimit) = &config.defaults.bwlimit {
        if !arg_present(args, "--bwlimit", None) {
            merged.push("--bwlimit".to_string());
            merged.push(bwlimit.clone());
        }
    }

    merged.extend(args.iter().skip(1).cloned());
    merged
}

fn first_command_index(args: &[String]) -> Option<usize> {
    let mut idx = 1;
    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--" {
            return (idx + 1 < args.len()).then_some(idx + 1);
        }
        if !arg.starts_with('-') || arg == "-" {
            return Some(idx);
        }

        let takes_value = matches!(
            arg.as_str(),
            "--format"
                | "--json-fields"
                | "--key"
                | "--key-passphrase"
                | "--bucket"
                | "--region"
                | "--container"
                | "--token"
                | "--tls"
                | "--two-factor"
                | "--profile"
                | "-P"
                | "--master-password"
                | "--limit-rate"
                | "--bwlimit"
                | "--include"
                | "--exclude-global"
                | "--include-from"
                | "--exclude-from"
                | "--min-size"
                | "--max-size"
                | "--min-age"
                | "--max-age"
                | "--parallel"
        );

        idx += 1;
        if takes_value && !arg.contains('=') && idx < args.len() {
            idx += 1;
        }
    }
    None
}

fn expand_aliases(args: &[String], config: &CliConfigFile) -> Result<Vec<String>, String> {
    let mut expanded = args.to_vec();
    let mut seen = std::collections::HashSet::new();

    for _ in 0..8 {
        let Some(cmd_idx) = first_command_index(&expanded) else {
            return Ok(expanded);
        };
        let command = expanded[cmd_idx].clone();
        if command == "alias" {
            return Ok(expanded);
        }
        let Some(alias_tokens) = config.aliases.get(&command) else {
            return Ok(expanded);
        };
        if !seen.insert(command.clone()) {
            return Err(format!("Alias cycle detected for '{}'", command));
        }

        let mut next = expanded[..cmd_idx].to_vec();
        next.extend(alias_tokens.iter().cloned());
        next.extend(expanded.iter().skip(cmd_idx + 1).cloned());
        expanded = next;
    }

    Err("Alias expansion exceeded maximum depth (8)".to_string())
}

fn prepare_cli_args(args: Vec<String>) -> Result<Vec<String>, String> {
    let config = load_cli_config()?;
    let with_defaults = apply_config_defaults(&args, &config);
    expand_aliases(&with_defaults, &config)
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{}", json),
        Err(e) => eprintln!("Error: failed to serialize JSON: {}", e),
    }
}

fn print_error(format: OutputFormat, msg: &str, code: i32) {
    match format {
        OutputFormat::Text => eprintln!("Error: {}", msg),
        OutputFormat::Json => {
            // JSON errors go to stderr so stdout remains clean for piping
            eprintln!("{}", serde_json::to_string(&CliError {
                status: "error",
                error: msg.to_string(),
                code,
            }).unwrap());
        }
    }
}

fn provider_error_to_exit_code(err: &ProviderError) -> i32 {
    match err {
        ProviderError::ConnectionFailed(_) | ProviderError::NotConnected | ProviderError::NetworkError(_) => 1,
        ProviderError::NotFound(_) => 2,
        ProviderError::PermissionDenied(_) => 3,
        ProviderError::TransferFailed(_) | ProviderError::Cancelled => 4,
        ProviderError::InvalidConfig(_) | ProviderError::InvalidPath(_) => 5,
        ProviderError::AuthenticationFailed(_) => 6,
        ProviderError::NotSupported(_) => 7,
        ProviderError::Timeout => 8,
        _ => 99,
    }
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

fn format_speed(bps: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bps >= MB {
        format!("{:.1} MB/s", bps as f64 / MB as f64)
    } else if bps >= KB {
        format!("{:.1} KB/s", bps as f64 / KB as f64)
    } else {
        format!("{} B/s", bps)
    }
}

fn parse_speed_limit(s: &str) -> Result<u64, String> {
    let s = s.trim().to_uppercase();
    if let Some(n) = s.strip_suffix('M') {
        n.parse::<u64>().map(|v| v * 1024 * 1024).map_err(|e| e.to_string())
    } else if let Some(n) = s.strip_suffix('K') {
        n.parse::<u64>().map(|v| v * 1024).map_err(|e| e.to_string())
    } else {
        s.parse::<u64>().map_err(|e| e.to_string())
    }
}

fn remote_entry_to_cli(e: &RemoteEntry) -> CliFileEntry {
    CliFileEntry {
        name: e.name.clone(),
        path: e.path.clone(),
        is_dir: e.is_dir,
        size: e.size,
        modified: e.modified.clone(),
        permissions: e.permissions.clone(),
        owner: e.owner.clone(),
    }
}

/// Maximum BFS scan depth for recursive operations (find, get -r, tree).
const MAX_SCAN_DEPTH: usize = 100;
/// Maximum entries to collect during BFS scan to prevent OOM.
const MAX_SCAN_ENTRIES: usize = 500_000;

/// Validate a relative path component is safe (no path traversal).
/// Returns the sanitized path or None if it contains traversal attempts.
fn validate_relative_path(relative: &str) -> Option<&str> {
    // Reject null bytes
    if relative.contains('\0') {
        return None;
    }
    let trimmed = relative.trim_start_matches('/');
    // Reject path traversal at component level (allows filenames like "file..backup.txt")
    for component in trimmed.split('/') {
        if component == ".." {
            return None;
        }
    }
    // Also check backslash-separated components for Windows paths
    for component in trimmed.split('\\') {
        if component == ".." {
            return None;
        }
    }
    // Reject absolute Windows paths (drive letters, UNC)
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        return None;
    }
    if trimmed.starts_with("\\\\") {
        return None;
    }
    Some(trimmed)
}

/// Verify that a resolved path stays within the expected root directory.
/// Prevents symlink escape attacks where a pre-existing symlink in the destination
/// tree could redirect writes outside the intended root.
fn verify_path_within_root(path: &std::path::Path, root: &std::path::Path) -> Result<(), String> {
    // Canonicalize parent (must exist for the check to work)
    let parent = path.parent().unwrap_or(path);
    if parent.exists() {
        if let Ok(canonical_parent) = parent.canonicalize() {
            if let Ok(canonical_root) = root.canonicalize() {
                if !canonical_parent.starts_with(&canonical_root) {
                    return Err(format!(
                        "Path escapes destination root via symlink: {} resolves to {}",
                        path.display(), canonical_parent.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Sanitize a filename for terminal display — strip ANSI escape sequences.
fn sanitize_filename(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ESC [ ... (letter) sequence
            if let Some(next) = chars.next() {
                if next == '[' {
                    // CSI sequence — consume until a letter
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                // else: skip single ESC + char
            }
        } else if ch.is_control() && ch != '\t' {
            // Skip control characters (except tab)
            continue;
        } else {
            result.push(ch);
        }
    }
    result
}

/// Check if we should use color in output (respects NO_COLOR env var and TTY detection).
fn use_color() -> bool {
    // NO_COLOR (https://no-color.org/) takes priority
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    // CLICOLOR_FORCE=1 forces color even on non-TTY
    if std::env::var("CLICOLOR_FORCE").ok().is_some_and(|v| v != "0") {
        return true;
    }
    // CLICOLOR=0 disables color
    if std::env::var("CLICOLOR").ok().is_some_and(|v| v == "0") {
        return false;
    }
    // Default: color if stderr is a terminal
    std::io::stderr().is_terminal()
}

const CLI_DENIED_SYSTEM_PREFIXES: &[&str] = &[
    "/proc", "/sys", "/dev", "/boot", "/root",
    "/etc/shadow", "/etc/passwd", "/etc/ssh", "/etc/sudoers",
];

const CLI_DENIED_HOME_RELATIVE_PREFIXES: &[&str] = &[
    ".ssh", ".gnupg", ".aws", ".kube", ".docker", ".config/gcloud", ".config/aeroftp", ".vault-token",
];

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{}/", prefix))
}

// ── Filter System ─────────────────────────────────────────────────

/// Parse a size string like "100k", "1M", "2G" into bytes.
fn parse_size_filter(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty size".into());
    }
    let (num_str, multiplier) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 1024u64),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1u64),
    };
    num_str
        .trim()
        .parse::<f64>()
        .map(|n| (n * multiplier as f64) as u64)
        .map_err(|e| format!("Invalid size '{}': {}", s, e))
}

/// Parse an age/duration string like "7d", "24h", "2w" into seconds.
fn parse_age_filter(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty duration".into());
    }
    let (num_str, multiplier) = match s.as_bytes().last() {
        Some(b's') => (&s[..s.len() - 1], 1u64),
        Some(b'm') => (&s[..s.len() - 1], 60u64),
        Some(b'h') => (&s[..s.len() - 1], 3600u64),
        Some(b'd') => (&s[..s.len() - 1], 86400u64),
        Some(b'w') => (&s[..s.len() - 1], 604800u64),
        Some(b'M') => (&s[..s.len() - 1], 2592000u64), // 30 days
        Some(b'y') => (&s[..s.len() - 1], 31536000u64), // 365 days
        _ => (s, 86400u64), // default: days
    };
    num_str
        .trim()
        .parse::<f64>()
        .map(|n| (n * multiplier as f64) as u64)
        .map_err(|e| format!("Invalid duration '{}': {}", s, e))
}

/// Load patterns from a file (one per line, # comments, blank lines skipped).
fn load_patterns_from_file(path: &str) -> Result<Vec<String>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read filter file '{}': {}", path, e))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

/// Build a filter function from CLI global flags.
/// Returns a closure that takes (name, size, modified_timestamp) and returns true if the entry passes.
/// Filter predicate: (filename, size_bytes, modified_timestamp_secs) -> passes
type FilterFn = Box<dyn Fn(&str, u64, Option<u64>) -> bool + Send + Sync>;

fn build_filter(cli: &Cli) -> FilterFn {
    use globset::{Glob, GlobSetBuilder};

    // Collect include patterns
    let mut include_patterns = cli.include.clone();
    if let Some(ref path) = cli.include_from {
        if let Ok(patterns) = load_patterns_from_file(path) {
            include_patterns.extend(patterns);
        }
    }

    // Collect exclude patterns (merge global + per-command)
    let mut exclude_patterns = cli.exclude_global.clone();
    if let Some(ref path) = cli.exclude_from {
        if let Ok(patterns) = load_patterns_from_file(path) {
            exclude_patterns.extend(patterns);
        }
    }

    // Build glob sets
    let include_set = if include_patterns.is_empty() {
        None
    } else {
        let mut builder = GlobSetBuilder::new();
        for p in &include_patterns {
            if let Ok(g) = Glob::new(p) {
                builder.add(g);
            }
        }
        builder.build().ok()
    };

    let exclude_set = if exclude_patterns.is_empty() {
        None
    } else {
        let mut builder = GlobSetBuilder::new();
        for p in &exclude_patterns {
            if let Ok(g) = Glob::new(p) {
                builder.add(g);
            }
        }
        builder.build().ok()
    };

    // Parse size limits
    let min_size = cli.min_size.as_ref().and_then(|s| parse_size_filter(s).ok());
    let max_size = cli.max_size.as_ref().and_then(|s| parse_size_filter(s).ok());

    // Parse age limits (convert to threshold timestamps)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let min_age_ts = cli.min_age.as_ref().and_then(|s| parse_age_filter(s).ok()).map(|secs| now - secs);
    let max_age_ts = cli.max_age.as_ref().and_then(|s| parse_age_filter(s).ok()).map(|secs| now - secs);

    Box::new(move |name: &str, size: u64, modified: Option<u64>| {
        // Include filter: if set, file must match at least one include pattern
        if let Some(ref set) = include_set {
            if !set.is_match(name) {
                return false;
            }
        }
        // Exclude filter: if file matches any exclude pattern, skip it
        if let Some(ref set) = exclude_set {
            if set.is_match(name) {
                return false;
            }
        }
        // Size filters
        if let Some(min) = min_size {
            if size < min {
                return false;
            }
        }
        if let Some(max) = max_size {
            if size > max {
                return false;
            }
        }
        // Age filters: min_age means "older than", max_age means "newer than"
        if let Some(ts) = modified {
            if let Some(threshold) = min_age_ts {
                if ts > threshold {
                    return false; // File is too new
                }
            }
            if let Some(threshold) = max_age_ts {
                if ts < threshold {
                    return false; // File is too old
                }
            }
        }
        true
    })
}

/// Resolve the current bandwidth limit from a time-based schedule.
/// Format: "08:00,512k 12:00,10M 18:00,off" — space-separated entries.
/// Returns the active rate in bytes/sec, or None if unlimited ("off").
fn resolve_bwlimit_schedule(schedule: &str) -> Option<u64> {
    let now = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Extract HH:MM from epoch seconds (local time approximation)
        let local_secs = secs % 86400; // seconds since midnight UTC
        // For proper local time we'd need chrono, but UTC is good enough for scheduling
        (local_secs / 3600, (local_secs % 3600) / 60) // (hour, minute)
    };

    let mut entries: Vec<(u32, Option<u64>)> = Vec::new(); // (minutes_since_midnight, rate)
    for part in schedule.split_whitespace() {
        if let Some((time_str, rate_str)) = part.split_once(',') {
            let time_parts: Vec<&str> = time_str.split(':').collect();
            if time_parts.len() == 2 {
                if let (Ok(h), Ok(m)) = (time_parts[0].parse::<u32>(), time_parts[1].parse::<u32>()) {
                    let minutes = h * 60 + m;
                    let rate = if rate_str == "off" || rate_str == "0" {
                        None
                    } else {
                        parse_size_filter(rate_str).ok()
                    };
                    entries.push((minutes, rate));
                }
            }
        }
    }

    if entries.is_empty() {
        return parse_size_filter(schedule).ok(); // Treat as simple rate
    }

    entries.sort_by_key(|(m, _)| *m);
    let now_minutes = now.0 as u32 * 60 + now.1 as u32;

    // Find the last entry whose time <= now
    let mut active_rate: Option<u64> = None;
    for (minutes, rate) in &entries {
        if *minutes <= now_minutes {
            active_rate = *rate;
        }
    }
    // If no entry matched (before first entry), use the last entry (wrap around midnight)
    if active_rate.is_none() && !entries.is_empty() {
        active_rate = entries.last().unwrap().1;
    }

    active_rate
}

/// Check if any filter flags are active.
fn has_filters(cli: &Cli) -> bool {
    !cli.include.is_empty()
        || !cli.exclude_global.is_empty()
        || cli.include_from.is_some()
        || cli.exclude_from.is_some()
        || cli.min_size.is_some()
        || cli.max_size.is_some()
        || cli.min_age.is_some()
        || cli.max_age.is_some()
}

fn create_progress_bar(filename: &str, total: u64) -> ProgressBar {
    if !use_color() {
        let pb = ProgressBar::hidden();
        return pb;
    }
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg}  [{bar:40.cyan/blue}] {percent}%  {bytes}/{total_bytes}  {bytes_per_sec}  ETA {eta}")
            .unwrap()
            .progress_chars("━╸─"),
    );
    pb.set_message(filename.to_string());
    pb
}

fn create_spinner(msg: &str) -> ProgressBar {
    if !use_color() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

fn effective_parallel_workers(cli: &Cli) -> usize {
    cli.parallel.clamp(1, 32)
}

fn create_overall_progress_bar(total_files: usize, total_bytes: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_bytes);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("Overall [{bar:30.green/dim}] {bytes}/{total_bytes} {bytes_per_sec} ETA {eta} ({pos}/{len} bytes)")
            .unwrap()
            .progress_chars("━╸─"),
    );
    pb.set_message(format!("{} files", total_files));
    pb
}

fn make_aggregate_progress_cb(
    aggregate: Arc<AtomicU64>,
    overall_pb: Option<ProgressBar>,
) -> Box<dyn Fn(u64, u64) + Send> {
    let last_seen = Arc::new(AtomicU64::new(0));
    Box::new(move |transferred, _total| {
        let previous = last_seen.swap(transferred, Ordering::Relaxed);
        let delta = transferred.saturating_sub(previous);
        if delta == 0 {
            return;
        }
        let current = aggregate.fetch_add(delta, Ordering::Relaxed) + delta;
        if let Some(ref pb) = overall_pb {
            pb.set_position(current);
        }
    })
}

async fn download_with_resume(
    provider: &mut dyn StorageProvider,
    remote_path: &str,
    local_path: &str,
    cli: &Cli,
    progress_cb: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    if cli.partial && provider.supports_resume() {
        let offset = std::fs::metadata(local_path).map(|meta| meta.len()).unwrap_or(0);
        if offset > 0 {
            return provider.resume_download(remote_path, local_path, offset, progress_cb).await;
        }
    }
    provider.download(remote_path, local_path, progress_cb).await
}

async fn upload_with_resume(
    provider: &mut dyn StorageProvider,
    local_path: &str,
    remote_path: &str,
    cli: &Cli,
    progress_cb: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    if cli.partial && provider.supports_resume() {
        let local_size = std::fs::metadata(local_path).map(|meta| meta.len()).unwrap_or(0);
        if let Ok(remote_size) = provider.size(remote_path).await {
            if remote_size > 0 && remote_size < local_size {
                return provider.resume_upload(local_path, remote_path, remote_size, progress_cb).await;
            }
        }
    }
    provider.upload(local_path, remote_path, progress_cb).await
}

async fn download_transfer_task(
    url: &str,
    remote_path: String,
    local_path: String,
    cli: &Cli,
    format: OutputFormat,
    aggregate: Option<Arc<AtomicU64>>,
    overall_pb: Option<ProgressBar>,
) -> Result<(), String> {
    let (mut provider, _) = create_and_connect(url, cli, format)
        .await
        .map_err(|code| format!("connection failed with exit code {}", code))?;

    let progress_cb = aggregate.map(|aggregate| make_aggregate_progress_cb(aggregate, overall_pb));
    let result = download_with_resume(&mut *provider, &remote_path, &local_path, cli, progress_cb)
        .await
        .map_err(|e| e.to_string());
    let _ = provider.disconnect().await;
    result
}

async fn upload_transfer_task(
    url: &str,
    local_path: String,
    remote_path: String,
    cli: &Cli,
    format: OutputFormat,
    aggregate: Option<Arc<AtomicU64>>,
    overall_pb: Option<ProgressBar>,
) -> Result<(), String> {
    let (mut provider, _) = create_and_connect(url, cli, format)
        .await
        .map_err(|code| format!("connection failed with exit code {}", code))?;

    if let Some(parent) = Path::new(&remote_path).parent() {
        let _ = provider.mkdir(&parent.to_string_lossy()).await;
    }

    let progress_cb = aggregate.map(|aggregate| make_aggregate_progress_cb(aggregate, overall_pb));
    let result = upload_with_resume(&mut *provider, &local_path, &remote_path, cli, progress_cb)
        .await
        .map_err(|e| e.to_string());
    let _ = provider.disconnect().await;
    result
}

// ── URL Parsing → ProviderConfig ───────────────────────────────────

fn resolve_password(
    url_obj: &url::Url,
    provider_type: &ProviderType,
    cli: &Cli,
) -> Result<String, String> {
    // 1. --token flag (API key providers)
    if let Some(ref token) = cli.token {
        return Ok(token.clone());
    }

    // 2. --password-stdin (limit to 4 KB to prevent abuse)
    if cli.password_stdin {
        let mut password = String::new();
        io::stdin()
            .read_line(&mut password)
            .map_err(|e| format!("Failed to read password from stdin: {}", e))?;
        if password.len() > 4096 {
            return Err("Password too long (max 4 KB)".to_string());
        }
        return Ok(password.trim().to_string());
    }

    // 3. Environment variable (protocol-specific, then generic)
    let env_vars = match provider_type {
        ProviderType::Ftp | ProviderType::Ftps => vec!["FTP_PASSWORD", "AEROFTP_PASSWORD"],
        ProviderType::Sftp => vec!["SFTP_PASSWORD", "AEROFTP_PASSWORD"],
        ProviderType::WebDav => vec!["WEBDAV_PASSWORD", "AEROFTP_PASSWORD"],
        ProviderType::S3 => vec!["AWS_SECRET_ACCESS_KEY", "AEROFTP_PASSWORD"],
        _ => vec!["AEROFTP_PASSWORD"],
    };
    for var in env_vars {
        if let Ok(pass) = std::env::var(var) {
            return Ok(pass);
        }
    }

    // 4. URL-embedded password
    if let Some(pass) = url_obj.password() {
        if !cfg!(test) {
            eprintln!(
                "Warning: password in URL is insecure. Use --password-stdin or env var instead."
            );
        }
        return Ok(urlencoding::decode(pass)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| pass.to_string()));
    }

    // 5. Interactive prompt (only if terminal)
    if std::io::stdin().is_terminal() {
        eprint!("Password: ");
        let _ = io::stderr().flush();
        let pass = rpassword::read_password()
            .map_err(|e| format!("Failed to read password: {}", e))?;
        return Ok(pass);
    }

    // 6. No password (FTP anonymous, etc.)
    Ok(String::new())
}

fn url_to_provider_config(url: &str, cli: &Cli) -> Result<(ProviderConfig, String), String> {
    let url_obj = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;

    let scheme = url_obj.scheme().to_lowercase();
    let host_str = url_obj
        .host_str()
        .ok_or("Missing host in URL")?
        .to_string();

    let (provider_type, effective_host) = match scheme.as_str() {
        "ftp" => (ProviderType::Ftp, host_str.clone()),
        "ftps" => (ProviderType::Ftps, host_str.clone()),
        "sftp" | "ssh" => (ProviderType::Sftp, host_str.clone()),
        "webdav" | "http" => {
            let port_str = url_obj
                .port()
                .map(|p| format!(":{}", p))
                .unwrap_or_default();
            let path = url_obj.path();
            (
                ProviderType::WebDav,
                format!("http://{}{}{}", host_str, port_str, path),
            )
        }
        "webdavs" | "https" => {
            let port_str = url_obj
                .port()
                .map(|p| format!(":{}", p))
                .unwrap_or_default();
            let path = url_obj.path();
            (
                ProviderType::WebDav,
                format!("https://{}{}{}", host_str, port_str, path),
            )
        }
        "s3" => (ProviderType::S3, host_str.clone()),
        "mega" => (ProviderType::Mega, "mega.nz".to_string()),
        "azure" => (ProviderType::Azure, host_str.clone()),
        "filen" => (ProviderType::Filen, "gateway.filen.io".to_string()),
        "internxt" => (ProviderType::Internxt, "drive.internxt.com".to_string()),
        "jottacloud" => (ProviderType::Jottacloud, "jfs.jottacloud.com".to_string()),
        "filelu" => (ProviderType::FileLu, "filelu.com".to_string()),
        "koofr" => (ProviderType::Koofr, "app.koofr.net".to_string()),
        "opendrive" => (ProviderType::OpenDrive, "dev.opendrive.com".to_string()),
        "yandexdisk" => (ProviderType::YandexDisk, "cloud-api.yandex.net".to_string()),
        "github" => {
            let (github_host, github_branch) = parse_github_target(&url_obj)?;
            let mut extra = HashMap::new();
            if let Some(branch_name) = github_branch {
                extra.insert("branch".to_string(), branch_name);
            }

            let username = if url_obj.username().is_empty() {
                String::new()
            } else {
                urlencoding::decode(url_obj.username())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| url_obj.username().to_string())
            };
            let password = resolve_password(&url_obj, &ProviderType::GitHub, cli)?;

            let config = ProviderConfig {
                name: "GitHub CLI".to_string(),
                provider_type: ProviderType::GitHub,
                host: github_host,
                port: url_obj.port(),
                username: Some(username),
                password: Some(password),
                initial_path: Some("/".to_string()),
                extra,
            };

            return Ok((config, "/".to_string()));
        }
        "gitlab" => {
            // gitlab://gitlab.com/owner/repo or gitlab://self-hosted.com/owner/repo
            let host = url_obj.host_str().unwrap_or("gitlab.com");
            let path = url_obj.path().trim_matches('/');
            let gitlab_host = if path.is_empty() {
                host.to_string()
            } else {
                format!("{}/{}", host, path)
            };

            let password = resolve_password(&url_obj, &ProviderType::GitLab, cli)?;

            let config = ProviderConfig {
                name: "GitLab CLI".to_string(),
                provider_type: ProviderType::GitLab,
                host: gitlab_host,
                port: url_obj.port(),
                username: None,
                password: Some(password),
                initial_path: Some("/".to_string()),
                extra: HashMap::new(),
            };

            return Ok((config, "/".to_string()));
        }
        _ => return Err(format!("Unsupported protocol: {}. Supported: ftp, ftps, sftp, webdav, webdavs, s3, mega, azure, filen, internxt, jottacloud, filelu, koofr, opendrive, yandexdisk, github, gitlab", scheme)),
    };

    let username = if url_obj.username().is_empty() {
        match provider_type {
            ProviderType::Ftp | ProviderType::Ftps => "anonymous".to_string(),
            _ => String::new(),
        }
    } else {
        urlencoding::decode(url_obj.username())
            .map(|s| s.to_string())
            .unwrap_or_else(|_| url_obj.username().to_string())
    };

    let password = resolve_password(&url_obj, &provider_type, cli)?;

    let port = url_obj.port();

    // For WebDAV/GitHub, the URL path is part of the host — initial_path is always /
    let url_path = match provider_type {
        ProviderType::WebDav | ProviderType::GitHub | ProviderType::GitLab => "/".to_string(),
        _ => {
            if url_obj.path().is_empty() || url_obj.path() == "/" {
                "/".to_string()
            } else {
                url_obj.path().to_string()
            }
        }
    };

    // Build extra HashMap from CLI flags
    let mut extra = HashMap::new();

    // SFTP
    if let Some(ref key) = cli.key {
        extra.insert("private_key_path".to_string(), key.clone());
    }
    if let Some(ref kp) = cli.key_passphrase {
        extra.insert("key_passphrase".to_string(), kp.clone());
    }
    if cli.trust_host_key {
        extra.insert("trust_unknown_hosts".to_string(), "true".to_string());
    }

    // Warn about secrets on command line (visible in process list)
    if cli.token.is_some() && std::env::var("AEROFTP_TOKEN").is_err() && !cli.quiet {
        eprintln!("Warning: --token on command line is visible in process list. Use AEROFTP_TOKEN env var instead.");
    }
    if cli.key_passphrase.is_some() && !cli.quiet {
        eprintln!("Warning: --key-passphrase on command line is visible in process list. Consider using ssh-agent instead.");
    }

    // FTP TLS
    if let Some(ref tls) = cli.tls {
        extra.insert("tls_mode".to_string(), tls.clone());
    } else if provider_type == ProviderType::Ftps {
        extra.insert("tls_mode".to_string(), "implicit".to_string());
    }

    // TLS cert verification
    if cli.insecure {
        extra.insert("verify_cert".to_string(), "false".to_string());
        if cli.verbose > 0 || !cli.quiet {
            eprintln!("Warning: TLS certificate verification disabled (--insecure)");
        }
    }

    // S3
    if let Some(ref bucket) = cli.bucket {
        extra.insert("bucket".to_string(), bucket.clone());
    }
    if let Some(ref region) = cli.region {
        extra.insert("region".to_string(), region.clone());
    }

    // Azure
    if let Some(ref container) = cli.container {
        extra.insert("container".to_string(), container.clone());
    }

    // 2FA
    if let Some(ref code) = cli.two_factor {
        extra.insert("two_factor_code".to_string(), code.clone());
    }

    let config = ProviderConfig {
        name: format!("{} CLI", provider_type),
        provider_type,
        host: effective_host,
        port,
        username: Some(username),
        password: Some(password),
        initial_path: Some(url_path.clone()),
        extra,
    };

    Ok((config, url_path))
}

fn parse_github_target(url_obj: &url::Url) -> Result<(String, Option<String>), String> {
    let host = url_obj
        .host_str()
        .ok_or_else(|| "Missing GitHub owner/repository in URL".to_string())?;

    let segments: Vec<&str> = url_obj
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();

    let (owner, repo_with_branch) = if host.eq_ignore_ascii_case("github.com") {
        if segments.len() < 2 {
            return Err(
                "GitHub URL must be github://owner/repo or github://owner/repo@branch".to_string(),
            );
        }
        (segments[0], segments[1])
    } else {
        let repo_segment = segments.first().copied().ok_or_else(|| {
            "GitHub URL must be github://owner/repo or github://owner/repo@branch".to_string()
        })?;
        (host, repo_segment)
    };

    let (repo, branch) = match repo_with_branch.rsplit_once('@') {
        Some((repo_name, branch_name)) if !repo_name.is_empty() && !branch_name.is_empty() => {
            (repo_name, Some(branch_name.to_string()))
        }
        _ => (repo_with_branch, None),
    };

    if owner.is_empty() || repo.is_empty() {
        return Err(
            "GitHub URL must be github://owner/repo or github://owner/repo@branch".to_string(),
        );
    }

    Ok((format!("{}/{}", owner, repo), branch))
}

// ── Vault Profile Support ─────────────────────────────────────────

fn open_vault(cli: &Cli) -> Result<ftp_client_gui_lib::credential_store::CredentialStore, String> {
    use ftp_client_gui_lib::credential_store::CredentialStore;
    use zeroize::Zeroize;

    // Try to init vault (auto mode unlocks automatically)
    match CredentialStore::init() {
        Ok(status) if status == "MASTER_PASSWORD_REQUIRED" => {
            // Need master password
            // WARNING: --master-password flag is visible in /proc/*/cmdline and `ps` output.
            // Prefer AEROFTP_MASTER_PASSWORD env var or interactive prompt (rpassword).
            if let Some(ref mp) = cli.master_password {
                if std::env::var("AEROFTP_MASTER_PASSWORD").is_err() {
                    eprintln!("Warning: --master-password on command line is visible in process list. Use AEROFTP_MASTER_PASSWORD env var instead.");
                }
                // VER-006: Clone to allow zeroization after use (original in Cli struct cannot be mutated)
                let mut mp_copy = mp.clone();
                let result = CredentialStore::unlock_with_master(&mp_copy)
                    .map_err(|e| format!("Failed to unlock vault: {}", e));
                mp_copy.zeroize();
                result?;
            } else if std::io::stdin().is_terminal() {
                // Interactive: prompt for master password (hidden input)
                eprint!("Master password: ");
                let _ = io::stderr().flush();
                let mut mp = rpassword::read_password()
                    .map_err(|e| format!("Failed to read master password: {}", e))?;
                let result = CredentialStore::unlock_with_master(mp.trim())
                    .map_err(|e| format!("Failed to unlock vault: {}", e));
                mp.zeroize();
                result?;
            } else {
                return Err("Vault is locked. Use --master-password or set AEROFTP_MASTER_PASSWORD".to_string());
            }
        }
        Ok(_) => {} // Auto mode — already open
        Err(e) => return Err(format!("Failed to open vault: {}", e)),
    }

    CredentialStore::from_cache()
        .ok_or_else(|| "Vault not available after init".to_string())
}

fn list_vault_profiles(cli: &Cli, format: OutputFormat) -> i32 {
    let store = match open_vault(cli) {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    let profiles_json = match store.get("config_server_profiles") {
        Ok(json) => json,
        Err(_) => {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            } else {
                println!("No saved profiles found.");
            }
            return 0;
        }
    };

    let profiles: Vec<serde_json::Value> = match serde_json::from_str(&profiles_json) {
        Ok(p) => p,
        Err(e) => {
            print_error(format, &format!("Failed to parse profiles: {}", e), 5);
            return 5;
        }
    };

    if profiles.is_empty() {
        if matches!(format, OutputFormat::Json) {
            println!("[]");
        } else {
            println!("No saved profiles found.");
        }
        return 0;
    }

    if matches!(format, OutputFormat::Json) {
        // JSON: output array with safe fields only (no credentials)
        let safe: Vec<serde_json::Value> = profiles.iter().map(|p| {
            serde_json::json!({
                "id": p.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed"),
                "protocol": p.get("protocol").and_then(|v| v.as_str()).unwrap_or(""),
                "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                "port": p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
                "username": p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&safe).unwrap_or_default());
    } else {
        // Text: formatted table
        println!("  {:<4} {:<30} {:<8} {:<35} Path", "#", "Name", "Proto", "Host");
        println!("  {}", "\u{2500}".repeat(90));
        for (i, p) in profiles.iter().enumerate() {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
            let proto = p.get("protocol").and_then(|v| v.as_str()).unwrap_or("?");
            let host = p.get("host").and_then(|v| v.as_str()).unwrap_or("");
            let port = p.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
            let path = p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/");
            let host_port = if port > 0 && port != 21 && port != 22 && port != 443 && port != 80 {
                format!("{}:{}", host, port)
            } else {
                host.to_string()
            };
            println!("  {:<4} {:<30} {:<8} {:<35} {}", i + 1, name, proto.to_uppercase(), host_port, path);
        }
        eprintln!("\n{} profile(s). Use: aeroftp-cli ls --profile \"Name\" [path]", profiles.len());
    }

    0
}

fn list_ai_models(cli: &Cli, format: OutputFormat) -> i32 {
    let store = match open_vault(cli) {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    // Map provider type to env var name
    let env_var_for = |ptype: &str| -> &str {
        match ptype {
            "openai" => "OPENAI_API_KEY",
            "anthropic" => "ANTHROPIC_API_KEY",
            "google" => "GEMINI_API_KEY",
            "xai" => "XAI_API_KEY",
            "openrouter" => "OPENROUTER_API_KEY",
            "ollama" => "OLLAMA_HOST",
            "kimi" => "KIMI_API_KEY",
            "qwen" => "QWEN_API_KEY",
            "deepseek" => "DEEPSEEK_API_KEY",
            "mistral" => "MISTRAL_API_KEY",
            "groq" => "GROQ_API_KEY",
            "perplexity" => "PERPLEXITY_API_KEY",
            "cohere" => "COHERE_API_KEY",
            "together" => "TOGETHER_API_KEY",
            _ => "",
        }
    };

    let mut configured: Vec<serde_json::Value> = Vec::new();
    let mut seen_types: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Read ai_settings from vault (saved from the desktop app)
    if let Ok(settings_json) = store.get("ai_settings") {
        if let Ok(settings) = serde_json::from_str::<serde_json::Value>(&settings_json) {
            if let Some(providers) = settings.get("providers").and_then(|v| v.as_array()) {
                for p in providers {
                    let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let ptype = p.get("type").and_then(|v| v.as_str()).unwrap_or(id);
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or(ptype);
                    let enabled = p.get("isEnabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    let base_url = p.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");

                    if id.is_empty() { continue; }

                    // Check if API key exists for this provider
                    let vault_key = format!("ai_apikey_{}", id);
                    let has_vault_key = store.get(&vault_key).map(|v| !v.is_empty()).unwrap_or(false);
                    let env_name = env_var_for(ptype);
                    let has_env_key = if env_name.is_empty() {
                        false
                    } else {
                        std::env::var(env_name).map(|v| !v.is_empty()).unwrap_or(false)
                    };
                    // Ollama doesn't need a key
                    let is_ollama = ptype == "ollama";

                    if !has_vault_key && !has_env_key && !is_ollama { continue; }

                    let source = if has_vault_key && has_env_key {
                        "vault+env"
                    } else if has_vault_key {
                        "vault"
                    } else if is_ollama {
                        "local"
                    } else {
                        "env"
                    };

                    // Find the active model from settings
                    let active_model = settings.get("models").and_then(|m| m.as_array()).and_then(|models| {
                        models.iter().find(|m| {
                            m.get("providerId").and_then(|v| v.as_str()) == Some(id)
                                && m.get("isActive").and_then(|v| v.as_bool()).unwrap_or(false)
                        })
                    });
                    let model_name = active_model
                        .and_then(|m| m.get("name").and_then(|v| v.as_str()))
                        .unwrap_or_else(|| default_model(ptype));

                    seen_types.insert(ptype.to_string());
                    configured.push(serde_json::json!({
                        "id": id,
                        "provider": ptype,
                        "name": name,
                        "model": model_name,
                        "source": source,
                        "enabled": enabled,
                        "baseUrl": base_url,
                    }));
                }
            }
        }
    }

    // 2. Also pick up env-only providers not in vault settings
    let env_providers = [
        ("openai", "OpenAI", "OPENAI_API_KEY"),
        ("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
        ("google", "Gemini", "GEMINI_API_KEY"),
        ("xai", "xAI", "XAI_API_KEY"),
        ("openrouter", "OpenRouter", "OPENROUTER_API_KEY"),
        ("deepseek", "DeepSeek", "DEEPSEEK_API_KEY"),
        ("mistral", "Mistral", "MISTRAL_API_KEY"),
        ("groq", "Groq", "GROQ_API_KEY"),
        ("perplexity", "Perplexity", "PERPLEXITY_API_KEY"),
        ("cohere", "Cohere", "COHERE_API_KEY"),
        ("together", "Together", "TOGETHER_API_KEY"),
    ];
    for (ptype, label, env_key) in &env_providers {
        if seen_types.contains(*ptype) { continue; }
        if std::env::var(env_key).map(|v| !v.is_empty()).unwrap_or(false) {
            configured.push(serde_json::json!({
                "id": ptype,
                "provider": ptype,
                "name": label,
                "model": default_model(ptype),
                "source": "env",
                "enabled": true,
                "baseUrl": "",
            }));
        }
    }

    if configured.is_empty() {
        if matches!(format, OutputFormat::Json) {
            println!("[]");
        } else {
            println!("No AI providers configured. Set API keys in the AeroFTP desktop app or via environment variables.");
        }
        return 0;
    }

    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&configured).unwrap_or_default());
    } else {
        println!("  {:<4} {:<16} {:<14} {:<40} {:<10} Source", "#", "Name", "Provider", "Active Model", "Enabled");
        println!("  {}", "\u{2500}".repeat(95));
        for (i, p) in configured.iter().enumerate() {
            let enabled_str = if p["enabled"].as_bool().unwrap_or(true) { "yes" } else { "no" };
            println!(
                "  {:<4} {:<16} {:<14} {:<40} {:<10} {}",
                i + 1,
                truncate_str(p["name"].as_str().unwrap_or(""), 15),
                p["provider"].as_str().unwrap_or(""),
                truncate_str(p["model"].as_str().unwrap_or(""), 39),
                enabled_str,
                p["source"].as_str().unwrap_or(""),
            );
        }
        eprintln!("\n{} AI provider(s). Use: aeroftp-cli agent --provider <name> --model <model>", configured.len());
    }

    0
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max.saturating_sub(3)]) }
}

fn safe_vault_profiles(cli: &Cli) -> Result<Vec<serde_json::Value>, String> {
    let store = open_vault(cli)?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read saved profiles: {}", e))?;
    let profiles = serde_json::from_str::<Vec<serde_json::Value>>(&profiles_json)
        .map_err(|e| format!("Failed to parse saved profiles: {}", e))?;

    Ok(profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed"),
                "protocol": p.get("protocol").and_then(|v| v.as_str()).unwrap_or(""),
                "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                "port": p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
                "username": p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
            })
        })
        .collect())
}

/// Variant of safe_vault_profiles that works without a Cli reference (for agent tool context).
/// Uses the cached vault (already opened by the agent startup flow).
fn safe_vault_profiles_for_agent() -> Result<Vec<serde_json::Value>, String> {
    let store = ftp_client_gui_lib::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not open. Cannot list server profiles.".to_string())?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {}", e))?;
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse profiles: {}", e))?;

    Ok(profiles.iter().map(|p| {
        serde_json::json!({
            "id": p.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            "name": p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed"),
            "protocol": p.get("protocol").and_then(|v| v.as_str()).unwrap_or(""),
            "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
            "port": p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
            "username": p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
            "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
        })
    }).collect())
}

/// Create a provider connection from a server profile name (for agent tool context).
/// Uses the cached vault and existing profile resolution.
async fn create_and_connect_for_agent(
    server_query: &str,
) -> Result<(Box<dyn ftp_client_gui_lib::providers::StorageProvider>, String), String> {
    let store = ftp_client_gui_lib::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not open. Cannot connect to server.".to_string())?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {}", e))?;
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse profiles: {}", e))?;

    // Find matching profile (case-insensitive name, ID, or substring)
    let query_lower = server_query.to_lowercase();
    let matched = profiles
        .iter()
        .find(|p| {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
            name == query_lower || id == server_query
        })
        .or_else(|| {
            profiles.iter().find(|p| {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                name.contains(&query_lower)
            })
        })
        .ok_or_else(|| format!("Server '{}' not found in saved profiles", server_query))?;

    let profile_id = matched.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let profile_name = matched.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    let protocol = matched.get("protocol").and_then(|v| v.as_str()).unwrap_or("");
    let host = matched.get("host").and_then(|v| v.as_str()).unwrap_or("");
    let port = matched.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let username = matched.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let initial_path = matched.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/");

    // Resolve password from vault
    let password = store.get(&format!("server_{}", profile_id)).unwrap_or_default();

    // Build provider config
    let provider_type = match protocol.to_uppercase().as_str() {
        "FTP" => ftp_client_gui_lib::providers::ProviderType::Ftp,
        "FTPS" => ftp_client_gui_lib::providers::ProviderType::Ftps,
        "SFTP" => ftp_client_gui_lib::providers::ProviderType::Sftp,
        "WEBDAV" | "WEBDAVS" => ftp_client_gui_lib::providers::ProviderType::WebDav,
        "S3" => ftp_client_gui_lib::providers::ProviderType::S3,
        "GITHUB" => ftp_client_gui_lib::providers::ProviderType::GitHub,
        "GITLAB" => ftp_client_gui_lib::providers::ProviderType::GitLab,
        other => return Err(format!("Protocol '{}' on server '{}' is not supported for agent server_exec. Supported: FTP, FTPS, SFTP, WebDAV, S3, GitHub, GitLab.", other, profile_name)),
    };

    let config = ftp_client_gui_lib::providers::ProviderConfig {
        name: profile_name.to_string(),
        provider_type,
        host: host.to_string(),
        port: if port > 0 { Some(port) } else { None },
        username: if username.is_empty() { None } else { Some(username.to_string()) },
        password: if password.is_empty() { None } else { Some(password) },
        initial_path: Some(initial_path.to_string()),
        extra: std::collections::HashMap::new(),
    };

    let mut provider = ftp_client_gui_lib::providers::ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider for '{}': {}", profile_name, e))?;

    provider.connect().await
        .map_err(|e| format!("Connection to '{}' failed: {}", profile_name, e))?;

    Ok((provider, initial_path.to_string()))
}

fn cmd_agent_info(cli: &Cli) -> i32 {
    let (profiles, profiles_error) = match safe_vault_profiles(cli) {
        Ok(profiles) => (profiles, None),
        Err(error) => (vec![], Some(error)),
    };
    let profiles = profiles.into_iter()
        .map(|p| {
            serde_json::json!({
                "name": p.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "protocol": p.get("protocol").and_then(|v| v.as_str()).unwrap_or(""),
                "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
            })
        })
        .collect::<Vec<_>>();

    let info = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "description": "AeroFTP CLI — multi-protocol file transfer with encrypted vault profiles",
        "usage": "aeroftp-cli <command> --profile \"Server Name\" [args]",
        "credential_model": "Use --profile to connect via encrypted vault. Never pass passwords directly.",
        "profiles": {
            "status": if profiles_error.is_some() { "unavailable" } else { "ok" },
            "count": profiles.len(),
            "list_command": "aeroftp-cli profiles --json",
            "servers": profiles,
            "error": profiles_error,
        },
        "commands": {
            "safe": [
                {"name": "ls", "syntax": "aeroftp-cli ls --profile NAME /path/ [-l] [--json]", "description": "List directory"},
                {"name": "cat", "syntax": "aeroftp-cli cat --profile NAME /path/file", "description": "Print file to stdout"},
                {"name": "stat", "syntax": "aeroftp-cli stat --profile NAME /path/ [--json]", "description": "File metadata"},
                {"name": "find", "syntax": "aeroftp-cli find --profile NAME /path/ \"*.ext\" [--json]", "description": "Search files"},
                {"name": "tree", "syntax": "aeroftp-cli tree --profile NAME /path/ [-d N] [--json]", "description": "Directory tree"},
                {"name": "df", "syntax": "aeroftp-cli df --profile NAME [--json]", "description": "Storage quota"},
                {"name": "connect", "syntax": "aeroftp-cli connect --profile NAME", "description": "Test connection"},
                {"name": "profiles", "syntax": "aeroftp-cli profiles [--json]", "description": "List saved servers"},
                {"name": "get", "syntax": "aeroftp-cli get --profile NAME /remote/file [./local]", "description": "Download file"},
                {"name": "get -r", "syntax": "aeroftp-cli get --profile NAME /remote/dir/ ./local/ -r", "description": "Download directory"},
            ],
            "modify": [
                {"name": "put", "syntax": "aeroftp-cli put --profile NAME ./local /remote/path", "description": "Upload file"},
                {"name": "put -r", "syntax": "aeroftp-cli put --profile NAME ./local/ /remote/ -r", "description": "Upload directory"},
                {"name": "mkdir", "syntax": "aeroftp-cli mkdir --profile NAME /remote/dir", "description": "Create directory"},
                {"name": "mv", "syntax": "aeroftp-cli mv --profile NAME /old /new", "description": "Move/rename"},
                {"name": "cp", "syntax": "aeroftp-cli cp --profile NAME /old /new", "description": "Server-side copy when supported"},
                {"name": "link", "syntax": "aeroftp-cli link --profile NAME /path/file", "description": "Create share link when supported"},
                {"name": "edit", "syntax": "aeroftp-cli edit --profile NAME /path/file \"find\" \"replace\" [--first]", "description": "Replace text in a remote UTF-8 file"},
                {"name": "sync", "syntax": "aeroftp-cli sync --profile NAME ./local/ /remote/ [--dry-run]", "description": "Sync directories"},
            ],
            "destructive": [
                {"name": "rm", "syntax": "aeroftp-cli rm --profile NAME /path", "description": "Delete file (confirm with user)"},
                {"name": "rm -rf", "syntax": "aeroftp-cli rm --profile NAME /dir/ -rf", "description": "Delete directory (always confirm)"},
                {"name": "sync --delete", "syntax": "aeroftp-cli sync --profile NAME ./local/ /remote/ --delete", "description": "Sync with orphan deletion (always confirm)"},
            ],
        },
        "output": {
            "json_flag": "--json",
            "stdout": "data only (file listings, file content, JSON)",
            "stderr": "status messages, progress, warnings",
            "tip": "Use --json 2>/dev/null for clean machine-readable output"
        },
        "exit_codes": {
            "0": "success",
            "1": "connection error",
            "2": "not found",
            "3": "permission denied",
            "4": "transfer failed",
            "5": "invalid usage",
            "6": "auth failed",
            "7": "not supported",
            "8": "timeout",
            "99": "unknown"
        },
        "protocols": [
            "ftp", "ftps", "sftp", "webdav", "webdavs", "s3",
            "mega", "filen", "internxt", "kdrive", "koofr",
            "jottacloud", "filelu", "opendrive", "yandexdisk", "azure",
            "github", "gitlab", "googledrive", "dropbox", "onedrive", "box",
            "pcloud", "zohoworkdrive", "fourshared", "drime"
        ],
        "safety_rules": [
            "Always use --profile instead of passwords in URLs",
            "Use --dry-run before sync operations",
            "Confirm with user before rm, rm -rf, or sync --delete",
            "Use --json for all programmatic parsing"
        ]
    });

    println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
    0
}

fn resolve_url_or_profile(url: &str, cli: &Cli, format: OutputFormat) -> Result<(ProviderConfig, String), i32> {
    // UX-002: Reject ambiguous invocations where both --profile and a URL are given
    if cli.profile.is_some() && url.contains("://") {
        print_error(
            format,
            "Cannot specify both --profile and URL. Use either --profile <NAME> or a URL, not both.",
            5,
        );
        return Err(5);
    }

    // If --profile is set, the "url" field is actually the first positional arg (path)
    // But if we have a profile, we ignore the url and use the profile
    if let Some(ref profile_name) = cli.profile {
        return profile_to_provider_config(profile_name, cli, format);
    }

    // Normal URL path
    match url_to_provider_config(url, cli) {
        Ok(v) => Ok(v),
        Err(e) => {
            print_error(format, &e, 5);
            Err(5)
        }
    }
}

fn normalize_profile_option_key(key: &str) -> &str {
    match key {
        "tlsMode" => "tls_mode",
        "verifyCert" => "verify_cert",
        "pathStyle" => "path_style",
        "accountName" => "account_name",
        "accessKey" => "access_key",
        "sasToken" => "sas_token",
        "pcloudRegion" => "region",
        other => other,
    }
}

fn insert_profile_option(extra: &mut HashMap<String, String>, key: &str, value: &serde_json::Value) {
    let normalized_key = normalize_profile_option_key(key).to_string();

    if let Some(string_value) = value.as_str() {
        extra.insert(normalized_key, string_value.to_string());
    } else if let Some(bool_value) = value.as_bool() {
        extra.insert(normalized_key, bool_value.to_string());
    } else if let Some(number_value) = value.as_i64() {
        extra.insert(normalized_key, number_value.to_string());
    } else if let Some(number_value) = value.as_u64() {
        extra.insert(normalized_key, number_value.to_string());
    } else if let Some(number_value) = value.as_f64() {
        extra.insert(normalized_key, number_value.to_string());
    }
}

fn profile_to_provider_config(profile_name: &str, cli: &Cli, format: OutputFormat) -> Result<(ProviderConfig, String), i32> {
    let store = match open_vault(cli) {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &e, 5);
            return Err(5);
        }
    };

    let profiles_json = match store.get("config_server_profiles") {
        Ok(json) => json,
        Err(_) => {
            print_error(format, "No saved profiles found in vault", 5);
            return Err(5);
        }
    };

    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| { print_error(format, &format!("Failed to parse profiles: {}", e), 5); 5 })?;

    // Match by index, exact name, ID, or substring (with disambiguation)
    let profile_lower = profile_name.to_lowercase();
    let matched = if let Ok(idx) = profile_name.parse::<usize>() {
        profiles.get(idx.saturating_sub(1))
    } else {
        // 1. Exact name match (case-insensitive)
        let exact = profiles.iter().find(|p| {
            p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase() == profile_lower
        });
        if exact.is_some() {
            exact
        } else {
            // 2. Exact ID match
            let by_id = profiles.iter().find(|p| {
                p.get("id").and_then(|v| v.as_str()).unwrap_or("") == profile_name
            });
            if by_id.is_some() {
                by_id
            } else {
                // 3. Substring match with disambiguation
                let matches: Vec<_> = profiles.iter().filter(|p| {
                    p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase().contains(&profile_lower)
                }).collect();
                match matches.len() {
                    0 => None,
                    1 => Some(matches[0]),
                    _ => {
                        print_error(format, &format!(
                            "Ambiguous profile '{}'. Matches: {}. Use exact name or index number.",
                            profile_name,
                            matches.iter().filter_map(|p| p.get("name").and_then(|v| v.as_str())).collect::<Vec<_>>().join(", ")
                        ), 5);
                        return Err(5);
                    }
                }
            }
        }
    };

    let profile = match matched {
        Some(p) => p,
        None => {
            print_error(format, &format!("Profile not found: '{}'. Run 'aeroftp-cli profiles' to list.", profile_name), 5);
            return Err(5);
        }
    };

    let id = profile.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    let host = profile.get("host").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let port = profile.get("port").and_then(|v| v.as_u64()).map(|p| p as u16);
    let username = profile.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let protocol = profile.get("protocol").and_then(|v| v.as_str()).unwrap_or("ftp");
    let initial_path = profile.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/").to_string();

    // Load credentials from vault
    // Password is stored as a raw string (not JSON) in server_{id}
    let (cred_user, cred_pass) = if !id.is_empty() {
        match store.get(&format!("server_{}", id)) {
            Ok(password_str) => {
                // The vault stores just the password as a plain string
                // Try to parse as JSON first (legacy format), fall back to raw string
                if let Ok(cred) = serde_json::from_str::<serde_json::Value>(&password_str) {
                    if let Some(obj) = cred.as_object() {
                        let u = obj.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let p = obj.get("password").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        (if u.is_empty() { username.clone() } else { u }, p)
                    } else {
                        // JSON but not an object — treat as raw password string
                        let raw = password_str.trim_matches('"').to_string();
                        (username.clone(), raw)
                    }
                } else {
                    // Raw password string (current format from GUI)
                    (username.clone(), password_str)
                }
            }
            Err(_) => (username.clone(), String::new()),
        }
    } else {
        (username.clone(), String::new())
    };

    let provider_type = match protocol {
        "ftp" => ProviderType::Ftp,
        "ftps" => ProviderType::Ftps,
        "sftp" => ProviderType::Sftp,
        "webdav" => ProviderType::WebDav,
        "s3" => ProviderType::S3,
        "mega" => ProviderType::Mega,
        "azure" => ProviderType::Azure,
        "filen" => ProviderType::Filen,
        "internxt" => ProviderType::Internxt,
        "jottacloud" => ProviderType::Jottacloud,
        "filelu" => ProviderType::FileLu,
        "koofr" => ProviderType::Koofr,
        "opendrive" => ProviderType::OpenDrive,
        "kdrive" => ProviderType::KDrive,
        "github" => ProviderType::GitHub,
        "gitlab" => ProviderType::GitLab,
        "swift" => ProviderType::Swift,
        "yandexdisk" => ProviderType::YandexDisk,
        "googledrive" => ProviderType::GoogleDrive,
        "dropbox" => ProviderType::Dropbox,
        "onedrive" => ProviderType::OneDrive,
        "box" => ProviderType::Box,
        "pcloud" => ProviderType::PCloud,
        "zohoworkdrive" => ProviderType::ZohoWorkdrive,
        "fourshared" => ProviderType::FourShared,
        "drime" => ProviderType::DrimeCloud,
        _ => {
            print_error(format, &format!("Unsupported protocol in profile: {}", protocol), 7);
            return Err(7);
        }
    };

    // Build extra from profile options and CLI overrides
    let mut extra = HashMap::new();

    // Load provider-specific options from profile
    if let Some(opts) = profile.get("options").and_then(|v| v.as_object()) {
        for (k, v) in opts {
            insert_profile_option(&mut extra, k, v);
        }
    }

    // CLI overrides take precedence
    if let Some(ref key) = cli.key {
        extra.insert("private_key_path".to_string(), key.clone());
    }
    if let Some(ref kp) = cli.key_passphrase {
        extra.insert("key_passphrase".to_string(), kp.clone());
    }
    if cli.trust_host_key {
        extra.insert("trust_unknown_hosts".to_string(), "true".to_string());
    }
    if let Some(ref tls) = cli.tls {
        extra.insert("tls_mode".to_string(), tls.clone());
    }
    if cli.insecure {
        extra.insert("verify_cert".to_string(), "false".to_string());
    }
    if let Some(ref bucket) = cli.bucket {
        extra.insert("bucket".to_string(), bucket.clone());
    }
    if let Some(ref region) = cli.region {
        extra.insert("region".to_string(), region.clone());
    }
    if let Some(ref container) = cli.container {
        extra.insert("container".to_string(), container.clone());
    }

    // Azure: GUI stores container as "bucket" in options; map to "container" for provider
    if provider_type == ProviderType::Azure && !extra.contains_key("container") {
        if let Some(bucket) = extra.remove("bucket") {
            extra.insert("container".to_string(), bucket);
        }
    }

    if !cli.quiet {
        eprintln!("Using profile: {} ({} → {})", name, protocol.to_uppercase(), host);
    }

    let config = ProviderConfig {
        name: name.to_string(),
        provider_type,
        host,
        port,
        username: Some(cred_user),
        password: Some(cred_pass),
        initial_path: Some(initial_path.clone()),
        extra,
    };

    Ok((config, initial_path))
}

/// Run OAuth2 browser authorization flow from CLI.
/// Opens the browser for the user to authorize, waits for the callback, saves tokens to vault.
async fn cli_oauth_browser_auth(protocol: &str, store: &CredentialStore) -> Result<(), String> {
    use ftp_client_gui_lib::providers::{
        OAuth2Manager, OAuthConfig,
        oauth2::{bind_callback_listener, bind_callback_listener_on_port, wait_for_callback},
    };

    let oauth_settings = load_oauth_client_config(store, protocol);
    if oauth_settings.0.is_empty() {
        return Err(format!("No OAuth client credentials found for '{}'. Configure Client ID and Client Secret in AeroFTP GUI Settings > Cloud Providers.", protocol));
    }

    // Provider-specific fixed ports (must match registered redirect URIs)
    let fixed_port: u16 = match protocol {
        "box" => 9484,
        "dropbox" => 17548,
        "onedrive" => 27154,
        "pcloud" => 17384,
        "zohoworkdrive" => 18765,
        "yandexdisk" => 19847,
        _ => 0,
    };

    let (listener, port) = if fixed_port > 0 {
        bind_callback_listener_on_port(fixed_port).await
    } else {
        bind_callback_listener().await
    }.map_err(|e| format!("Failed to bind callback listener on port {}: {}", fixed_port, e))?;

    let config = match protocol {
        "googledrive" => OAuthConfig::google_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "dropbox" => OAuthConfig::dropbox_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "onedrive" => OAuthConfig::onedrive_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "box" => OAuthConfig::box_cloud_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "pcloud" => {
            let region = store.get("oauth_pcloud_region").unwrap_or_else(|_| "us".to_string());
            OAuthConfig::pcloud_with_port(&oauth_settings.0, &oauth_settings.1, port, &region)
        }
        "zohoworkdrive" => {
            let region = store.get("oauth_zohoworkdrive_region").unwrap_or_else(|_| "us".to_string());
            OAuthConfig::zoho_with_port(&oauth_settings.0, &oauth_settings.1, port, &region)
        }
        "yandexdisk" => OAuthConfig::yandex_disk_with_port(&oauth_settings.0, &oauth_settings.1, port),
        other => return Err(format!("OAuth not supported for: {}", other)),
    };

    let manager = OAuth2Manager::new();
    let (auth_url, expected_state) = manager.start_auth_flow(&config).await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Try to open browser automatically
    eprintln!("\nAuthorize in your browser:");
    eprintln!("  {}\n", auth_url);
    if open::that(&auth_url).is_err() {
        eprintln!("Could not open browser automatically. Please open the URL above manually.");
    }
    eprintln!("Waiting for authorization... (press Ctrl+C to cancel)");

    // Wait for callback with 5-minute timeout
    let callback_handle = tokio::spawn(async move {
        wait_for_callback(listener).await
    });
    let (code, state) = tokio::time::timeout(
        tokio::time::Duration::from_secs(300),
        callback_handle
    ).await
        .map_err(|_| "Timeout: no response within 5 minutes".to_string())?
        .map_err(|e| format!("Callback error: {}", e))?
        .map_err(|e| format!("Callback error: {}", e))?;

    if state != expected_state {
        return Err("OAuth state mismatch — possible CSRF attack".to_string());
    }

    // Exchange code for tokens
    manager.complete_auth_flow(&config, &code, &expected_state).await
        .map_err(|e| format!("Token exchange failed: {}", e))?;

    Ok(())
}

/// Create an OAuth provider by protocol name (used for retry after re-authorization)
fn create_oauth_provider_by_protocol(protocol: &str, store: &CredentialStore) -> Result<Box<dyn StorageProvider>, String> {
    use ftp_client_gui_lib::providers::{
        OAuth2Manager, OAuthProvider,
        GoogleDriveProvider, DropboxProvider, OneDriveProvider, BoxProvider, PCloudProvider,
        ZohoWorkdriveProvider, YandexDiskProvider,
        google_drive::GoogleDriveConfig, dropbox::DropboxConfig,
        onedrive::OneDriveConfig, types::BoxConfig, types::PCloudConfig,
        zoho_workdrive::ZohoWorkdriveConfig,
    };

    let oauth_settings = load_oauth_client_config(store, protocol);
    match protocol {
        "googledrive" => Ok(Box::new(GoogleDriveProvider::new(GoogleDriveConfig::new(&oauth_settings.0, &oauth_settings.1)))),
        "dropbox" => Ok(Box::new(DropboxProvider::new(DropboxConfig::new(&oauth_settings.0, &oauth_settings.1)))),
        "onedrive" => Ok(Box::new(OneDriveProvider::new(OneDriveConfig::new(&oauth_settings.0, &oauth_settings.1)))),
        "box" => Ok(Box::new(BoxProvider::new(BoxConfig { client_id: oauth_settings.0, client_secret: oauth_settings.1 }))),
        "pcloud" => {
            let region = store.get("oauth_pcloud_region").unwrap_or_else(|_| "us".to_string());
            Ok(Box::new(PCloudProvider::new(PCloudConfig { client_id: oauth_settings.0, client_secret: oauth_settings.1, region })))
        }
        "zohoworkdrive" => {
            let region = store.get("oauth_zohoworkdrive_region").unwrap_or_else(|_| "us".to_string());
            Ok(Box::new(ZohoWorkdriveProvider::new(ZohoWorkdriveConfig::new(&oauth_settings.0, &oauth_settings.1, &region))))
        }
        "yandexdisk" => {
            let manager = OAuth2Manager::new();
            let tokens = manager.load_tokens(OAuthProvider::YandexDisk)
                .map_err(|e| format!("No Yandex tokens: {}", e))?;
            Ok(Box::new(YandexDiskProvider::new(tokens.access_token.clone(), None)))
        }
        "fourshared" => {
            use ftp_client_gui_lib::providers::{
                fourshared::FourSharedProvider, types::FourSharedConfig,
            };

            // Read consumer key/secret — try individual keys first (GUI format), then legacy JSON
            let (ck, cs) = if let (Ok(k), Ok(s)) = (
                store.get("oauth_fourshared_client_id"),
                store.get("oauth_fourshared_client_secret"),
            ) {
                (k, s)
            } else {
                let json = store.get("fourshared_oauth_settings")
                    .map_err(|e| format!("No 4shared OAuth settings in vault: {}", e))?;
                #[derive(serde::Deserialize)]
                struct Fs { consumer_key: String, consumer_secret: String }
                let fs: Fs = serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse 4shared settings: {}", e))?;
                (fs.consumer_key, fs.consumer_secret)
            };
            let (at, ats) = {
                let data = store.get("oauth_fourshared")
                    .map_err(|_| "No 4shared access tokens in vault. Authorize from GUI first.".to_string())?;
                data.split_once(':')
                    .map(|(t, s)| (t.to_string(), s.to_string()))
                    .ok_or_else(|| "Invalid 4shared token format".to_string())?
            };

            let fs_config = FourSharedConfig {
                consumer_key: ck,
                consumer_secret: secrecy::SecretString::from(cs),
                access_token: secrecy::SecretString::from(at),
                access_token_secret: secrecy::SecretString::from(ats),
            };

            Ok(Box::new(FourSharedProvider::new(fs_config)))
        }
        other => Err(format!("Unknown OAuth protocol: {}", other)),
    }
}

/// Try to create an OAuth provider directly from vault tokens (for --profile with OAuth providers)
async fn try_create_oauth_provider(
    protocol: &str,
    profile_name: &str,
    initial_path: &str,
    store: &ftp_client_gui_lib::credential_store::CredentialStore,
    quiet: bool,
) -> Option<Result<(Box<dyn StorageProvider>, String), i32>> {
    use ftp_client_gui_lib::providers::{
        OAuth2Manager, OAuthProvider,
        GoogleDriveProvider, DropboxProvider, OneDriveProvider, BoxProvider, PCloudProvider,
        ZohoWorkdriveProvider,
        google_drive::GoogleDriveConfig, dropbox::DropboxConfig,
        onedrive::OneDriveConfig, types::BoxConfig, types::PCloudConfig,
        zoho_workdrive::ZohoWorkdriveConfig,
    };

    type OAuthCreateFn = Box<dyn FnOnce(&CredentialStore) -> Result<Box<dyn StorageProvider>, String>>;
    let (oauth_provider, create_fn): (OAuthProvider, OAuthCreateFn) = match protocol {
        "googledrive" => {
            let oauth_settings = load_oauth_client_config(store, "googledrive");
            (OAuthProvider::Google, Box::new(move |_| {
                let config = GoogleDriveConfig::new(&oauth_settings.0, &oauth_settings.1);
                Ok(Box::new(GoogleDriveProvider::new(config)) as Box<dyn StorageProvider>)
            }))
        }
        "dropbox" => {
            let oauth_settings = load_oauth_client_config(store, "dropbox");
            (OAuthProvider::Dropbox, Box::new(move |_| {
                let config = DropboxConfig::new(&oauth_settings.0, &oauth_settings.1);
                Ok(Box::new(DropboxProvider::new(config)) as Box<dyn StorageProvider>)
            }))
        }
        "onedrive" => {
            let oauth_settings = load_oauth_client_config(store, "onedrive");
            (OAuthProvider::OneDrive, Box::new(move |_| {
                let config = OneDriveConfig::new(&oauth_settings.0, &oauth_settings.1);
                Ok(Box::new(OneDriveProvider::new(config)) as Box<dyn StorageProvider>)
            }))
        }
        "box" => {
            let oauth_settings = load_oauth_client_config(store, "box");
            (OAuthProvider::Box, Box::new(move |_| {
                let config = BoxConfig { client_id: oauth_settings.0, client_secret: oauth_settings.1 };
                Ok(Box::new(BoxProvider::new(config)) as Box<dyn StorageProvider>)
            }))
        }
        "pcloud" => {
            let oauth_settings = load_oauth_client_config(store, "pcloud");
            let region = store.get("oauth_pcloud_region").unwrap_or_else(|_| "us".to_string());
            (OAuthProvider::PCloud, Box::new(move |_| {
                let config = PCloudConfig { client_id: oauth_settings.0, client_secret: oauth_settings.1, region };
                Ok(Box::new(PCloudProvider::new(config)) as Box<dyn StorageProvider>)
            }))
        }
        "zohoworkdrive" => {
            let oauth_settings = load_oauth_client_config(store, "zohoworkdrive");
            let region = store.get("oauth_zohoworkdrive_region").unwrap_or_else(|_| "us".to_string());
            (OAuthProvider::ZohoWorkdrive, Box::new(move |_| {
                let config = ZohoWorkdriveConfig::new(&oauth_settings.0, &oauth_settings.1, &region);
                Ok(Box::new(ZohoWorkdriveProvider::new(config)) as Box<dyn StorageProvider>)
            }))
        }
        "yandexdisk" => {
            // Yandex uses OAuth2 but creates provider with raw token
            (OAuthProvider::YandexDisk, Box::new(move |_| {
                let manager = OAuth2Manager::new();
                let tokens = manager.load_tokens(OAuthProvider::YandexDisk)
                    .map_err(|e| format!("No Yandex Disk tokens: {}", e))?;
                Ok(Box::new(ftp_client_gui_lib::providers::YandexDiskProvider::new(
                    tokens.access_token.clone(), None
                )) as Box<dyn StorageProvider>)
            }))
        }
        "fourshared" => {
            // 4shared uses OAuth1 — handle separately from the OAuth2 flow
            use ftp_client_gui_lib::providers::{
                fourshared::FourSharedProvider, types::FourSharedConfig,
            };

            // GUI stores 4shared credentials as individual vault keys:
            //   oauth_fourshared_client_id, oauth_fourshared_client_secret (consumer)
            //   fourshared_access_token, fourshared_access_token_secret (tokens)
            // Also support legacy JSON format: fourshared_oauth_settings, fourshared_oauth_tokens
            let (consumer_key, consumer_secret) = if let (Ok(k), Ok(s)) = (
                store.get("oauth_fourshared_client_id"),
                store.get("oauth_fourshared_client_secret"),
            ) {
                (k, s)
            } else if let Ok(json) = store.get("fourshared_oauth_settings") {
                #[derive(serde::Deserialize)]
                struct FsSettings { consumer_key: String, consumer_secret: String }
                match serde_json::from_str::<FsSettings>(&json) {
                    Ok(s) => (s.consumer_key, s.consumer_secret),
                    Err(_) => {
                        eprintln!("Error: No 4shared OAuth settings found in vault. Configure consumer_key/consumer_secret in AeroFTP GUI first.");
                        return Some(Err(6));
                    }
                }
            } else {
                eprintln!("Error: No 4shared OAuth settings found in vault. Configure consumer_key/consumer_secret in AeroFTP GUI first.");
                return Some(Err(6));
            };

            // Tokens stored as "token:token_secret" in key "oauth_fourshared"
            let (access_token, access_secret) = if let Ok(data) = store.get("oauth_fourshared") {
                if let Some((t, s)) = data.split_once(':') {
                    (t.to_string(), s.to_string())
                } else {
                    eprintln!("Error: Invalid 4shared token format in vault.");
                    return Some(Err(6));
                }
            } else {
                eprintln!("Error: No 4shared access tokens found in vault. Authorize 4shared from AeroFTP GUI first.");
                return Some(Err(6));
            };

            let fs_config = FourSharedConfig {
                consumer_key,
                consumer_secret: secrecy::SecretString::from(consumer_secret),
                access_token: secrecy::SecretString::from(access_token),
                access_token_secret: secrecy::SecretString::from(access_secret),
            };

            let mut provider = FourSharedProvider::new(fs_config);
            if let Err(e) = provider.connect().await {
                eprintln!("Error: 4shared connection failed: {}", e);
                return Some(Err(6));
            }
            if !quiet {
                eprintln!("Using profile: {} (4SHARED via OAuth1)", profile_name);
            }
            return Some(Ok((Box::new(provider) as Box<dyn StorageProvider>, initial_path.to_string())));
        }
        _ => return None,
    };

    // Check if tokens exist — if not, offer browser authorization
    let manager = OAuth2Manager::new();
    let needs_auth = !manager.has_tokens(oauth_provider);

    if needs_auth {
        if !std::io::stdin().is_terminal() {
            eprintln!("Error: No OAuth tokens for {}. Run interactively to authorize, or authorize from AeroFTP GUI.", profile_name);
            return Some(Err(6));
        }
        eprintln!("No OAuth tokens found for {}. Starting browser authorization...", profile_name);
        match cli_oauth_browser_auth(protocol, store).await {
            Ok(()) => eprintln!("Authorization successful!"),
            Err(e) => {
                eprintln!("Error: Authorization failed: {}", e);
                return Some(Err(6));
            }
        }
    }

    // Create provider
    let mut provider = match create_fn(store) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: Failed to create provider: {}", e);
            return Some(Err(5));
        }
    };

    if !quiet {
        eprintln!("Using profile: {} ({} via OAuth)", profile_name, protocol.to_uppercase());
    }

    // Connect — if token expired, offer re-authorization
    if let Err(e) = provider.connect().await {
        if !std::io::stdin().is_terminal() {
            eprintln!("Error: OAuth connection failed: {}. Run interactively to re-authorize.", e);
            return Some(Err(6));
        }
        eprintln!("Token expired or invalid. Starting browser re-authorization...");
        match cli_oauth_browser_auth(protocol, store).await {
            Ok(()) => {
                eprintln!("Re-authorization successful! Reconnecting...");
                // Recreate provider with fresh tokens
                // We need to recreate since create_fn was consumed — rebuild inline
                let mut retry_provider = match create_oauth_provider_by_protocol(protocol, store) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("Error: Failed to recreate provider: {}", e);
                        return Some(Err(5));
                    }
                };
                if let Err(e2) = retry_provider.connect().await {
                    eprintln!("Error: Connection failed after re-authorization: {}", e2);
                    return Some(Err(6));
                }
                return Some(Ok((retry_provider, initial_path.to_string())));
            }
            Err(e2) => {
                eprintln!("Error: Re-authorization failed: {}", e2);
                return Some(Err(6));
            }
        }
    }

    Some(Ok((provider, initial_path.to_string())))
}

/// Load OAuth client_id and client_secret from vault settings
fn load_oauth_client_config(store: &CredentialStore, provider: &str) -> (String, String) {
    // Format 1: Individual vault keys (current SettingsPanel format)
    let cid_key = format!("oauth_{}_client_id", provider);
    let csec_key = format!("oauth_{}_client_secret", provider);
    if let Ok(cid) = store.get(&cid_key) {
        if !cid.is_empty() {
            let csec = store.get(&csec_key).unwrap_or_default();
            return (cid, csec);
        }
    }

    // Format 2: Structured JSON (legacy migration / config_oauth_clients)
    for key in &["config_oauth_clients", "config_aeroftp_oauth_settings"] {
        if let Ok(json) = store.get(key) {
            if let Ok(settings) = serde_json::from_str::<serde_json::Value>(&json) {
                if let Some(p) = settings.get(provider) {
                    let cid = p.get("clientId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let csec = p.get("clientSecret").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if !cid.is_empty() {
                        return (cid, csec);
                    }
                }
            }
        }
    }
    (String::new(), String::new())
}

use ftp_client_gui_lib::credential_store::CredentialStore;

async fn create_and_connect(
    url: &str,
    cli: &Cli,
    format: OutputFormat,
) -> Result<(Box<dyn StorageProvider>, String), i32> {
    // Check if --profile points to an OAuth provider — handle separately
    // Uses the same strict matching as profile_to_provider_config (exact → ID → disambiguated substring)
    if let Some(ref profile_name) = cli.profile {
        if let Ok(store) = open_vault(cli) {
            if let Ok(profiles_json) = store.get("config_server_profiles") {
                if let Ok(profiles) = serde_json::from_str::<Vec<serde_json::Value>>(&profiles_json) {
                    let profile_lower = profile_name.to_lowercase();
                    let matched = if let Ok(idx) = profile_name.parse::<usize>() {
                        profiles.get(idx.saturating_sub(1)).cloned()
                    } else {
                        // Exact name → exact ID → disambiguated substring (same as profile_to_provider_config)
                        let exact = profiles.iter().find(|p| {
                            p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase() == profile_lower
                        });
                        if exact.is_some() {
                            exact.cloned()
                        } else {
                            let by_id = profiles.iter().find(|p| {
                                p.get("id").and_then(|v| v.as_str()).unwrap_or("") == profile_name.as_str()
                            });
                            if by_id.is_some() {
                                by_id.cloned()
                            } else {
                                let matches: Vec<_> = profiles.iter().filter(|p| {
                                    p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase().contains(&profile_lower)
                                }).collect();
                                match matches.len() {
                                    1 => Some(matches[0].clone()),
                                    _ => None, // 0 or ambiguous — let profile_to_provider_config handle the error
                                }
                            }
                        }
                    };
                    if let Some(profile) = matched {
                        let protocol = profile.get("protocol").and_then(|v| v.as_str()).unwrap_or("");
                        let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
                        let initial_path = profile.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/");
                        if let Some(result) = try_create_oauth_provider(protocol, name, initial_path, &store, cli.quiet).await {
                            return result;
                        }
                    }
                }
            }
        }
    }

    let (config, path) = resolve_url_or_profile(url, cli, format)?;

    let mut provider = match ProviderFactory::create(&config) {
        Ok(p) => p,
        Err(e) => {
            print_error(format, &format!("Failed to create provider: {}", e), provider_error_to_exit_code(&e));
            return Err(provider_error_to_exit_code(&e));
        }
    };

    if let Err(e) = provider.connect().await {
        print_error(format, &format!("Connection failed: {}", e), provider_error_to_exit_code(&e));
        return Err(provider_error_to_exit_code(&e));
    }

    // Apply speed limit if set
    if let Some(ref rate) = cli.limit_rate {
        match parse_speed_limit(rate) {
            Ok(bps) => {
                let kb = bps / 1024;
                let _ = provider.set_speed_limit(kb, kb).await;
            }
            Err(e) => {
                if cli.verbose > 0 {
                    eprintln!("Warning: invalid --limit-rate '{}': {}", rate, e);
                }
            }
        }
    }
    // Apply bandwidth schedule if set (--bwlimit "08:00,512k 18:00,off")
    if let Some(ref schedule) = cli.bwlimit {
        if let Some(rate) = resolve_bwlimit_schedule(schedule) {
            let kb = rate / 1024;
            let _ = provider.set_speed_limit(kb, kb).await;
            if cli.verbose > 0 {
                eprintln!("Bandwidth limit: {} (from schedule)", format_size(rate));
            }
        }
    }

    Ok((provider, path))
}

// ── Command Handlers ───────────────────────────────────────────────

#[derive(Clone)]
struct ServeHttpState {
    provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
    provider_label: String,
    base_path: String,
}

fn serve_effective_base_path(path: &str, url_path: &str) -> String {
    if path == "/" && url_path != "/" {
        normalize_remote_path(url_path)
    } else {
        normalize_remote_path(path)
    }
}

fn normalize_remote_path(path: &str) -> String {
    let mut normalized = String::from("/");
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();

    if !segments.is_empty() {
        normalized.push_str(&segments.join("/"));
    }

    normalized
}

fn sanitize_served_relative_path(path: &str) -> Result<String, StatusCode> {
    let decoded = urlencoding::decode(path)
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .into_owned();

    if decoded.contains('\0') {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut parts = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(StatusCode::FORBIDDEN),
            other => parts.push(other),
        }
    }

    Ok(parts.join("/"))
}

fn build_served_remote_path(base_path: &str, relative_path: &str) -> String {
    if relative_path.is_empty() {
        return normalize_remote_path(base_path);
    }

    let normalized_base = normalize_remote_path(base_path);
    if normalized_base == "/" {
        format!("/{}", relative_path)
    } else {
        format!("{}/{}", normalized_base.trim_end_matches('/'), relative_path)
    }
}

fn encode_request_path(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }

    let encoded_segments: Vec<String> = trimmed
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect();

    let mut encoded = format!("/{}", encoded_segments.join("/"));
    if path.ends_with('/') {
        encoded.push('/');
    }
    encoded
}

fn child_request_path(current_relative_path: &str, name: &str, is_dir: bool) -> String {
    let mut path = current_relative_path.trim_matches('/').to_string();
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(name);
    if is_dir {
        path.push('/');
    }
    path
}

fn parent_request_path(current_relative_path: &str) -> Option<String> {
    let mut segments: Vec<&str> = current_relative_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    if segments.is_empty() {
        return None;
    }

    segments.pop();
    if segments.is_empty() {
        Some("/".to_string())
    } else {
        Some(format!("/{}/", segments.join("/")))
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn provider_error_to_status_code(error: &ProviderError) -> StatusCode {
    match error {
        ProviderError::NotFound(_) => StatusCode::NOT_FOUND,
        ProviderError::PermissionDenied(_) => StatusCode::FORBIDDEN,
        ProviderError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
        ProviderError::NotSupported(_) => StatusCode::NOT_IMPLEMENTED,
        ProviderError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        ProviderError::Cancelled => StatusCode::REQUEST_TIMEOUT,
        ProviderError::InvalidPath(_) | ProviderError::InvalidConfig(_) | ProviderError::ParseError(_) => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn serve_error_response(status: StatusCode, message: &str) -> Response {
    let body = Html(format!(
        "<!doctype html><html><body><h1>{}</h1><p>{}</p></body></html>",
        status,
        escape_html(message)
    ));
    (status, body).into_response()
}

fn build_html_response(status: StatusCode, html: String, head_only: bool) -> Response {
    let content_length = html.len().to_string();
    let mut response = if head_only {
        Response::new(Body::empty())
    } else {
        Response::new(Body::from(html))
    };
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(length) = HeaderValue::from_str(&content_length) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
}

fn guess_content_type(entry: &RemoteEntry) -> String {
    entry
        .mime_type
        .as_deref()
        .map(str::to_string)
        .or_else(|| {
            mime_guess::from_path(&entry.name)
                .first_raw()
                .map(|mime| mime.to_string())
        })
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn build_file_head_response(entry: &RemoteEntry) -> Response {
    let content_type = guess_content_type(entry);
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    if let Ok(length) = HeaderValue::from_str(&entry.size.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
        .headers_mut()
        .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response
}

fn build_file_get_response(
    entry: &RemoteEntry,
    bytes: Vec<u8>,
    range: Option<&HeaderValue>,
) -> Response {
    let content_type = guess_content_type(entry);
    let total = bytes.len();

    if let Some(range_val) = range {
        if let Some((start, end)) = parse_range_header(range_val.to_str().unwrap_or(""), total) {
            let sliced = bytes[start..=end].to_vec();
            let content_range = format!("bytes {}-{}/{}", start, end, total);
            let mut response = Response::new(Body::from(sliced));
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            if let Ok(value) = HeaderValue::from_str(&content_type) {
                response.headers_mut().insert(CONTENT_TYPE, value);
            }
            if let Ok(cl) = HeaderValue::from_str(&(end - start + 1).to_string()) {
                response.headers_mut().insert(CONTENT_LENGTH, cl);
            }
            if let Ok(cr) = HeaderValue::from_str(&content_range) {
                response.headers_mut().insert(CONTENT_RANGE, cr);
            }
            response
                .headers_mut()
                .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return response;
        }
    }

    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    if let Ok(length) = HeaderValue::from_str(&total.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, length);
    }
    response
        .headers_mut()
        .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response
}

/// Parse an HTTP Range header value like "bytes=0-499" or "bytes=-500" or "bytes=9500-".
fn parse_range_header(value: &str, file_size: usize) -> Option<(usize, usize)> {
    let bytes_spec = value.strip_prefix("bytes=")?;

    if file_size == 0 {
        return None;
    }

    let (start_str, end_str) = bytes_spec.split_once('-')?;

    if start_str.is_empty() {
        // Suffix range: "-500" means last 500 bytes
        let suffix_len: usize = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = file_size.saturating_sub(suffix_len);
        return Some((start, file_size - 1));
    }

    let start: usize = start_str.parse().ok()?;
    let end = if end_str.is_empty() {
        file_size - 1
    } else {
        end_str.parse::<usize>().ok()?
    };

    if start > end || start >= file_size {
        return None;
    }
    Some((start, end.min(file_size - 1)))
}

fn render_directory_listing(
    provider_label: &str,
    current_relative_path: &str,
    remote_path: &str,
    entries: &[RemoteEntry],
) -> String {
    let title = if current_relative_path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", current_relative_path.trim_matches('/'))
    };

    let mut html = String::new();
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    html.push_str(&format!("<title>{}</title>", escape_html(&title)));
    html.push_str("<style>body{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;background:#f6f6f2;color:#171717;margin:2rem;}h1{font-size:1.4rem;margin-bottom:.25rem;}p{color:#555;}table{width:100%;border-collapse:collapse;margin-top:1.5rem;background:#fff;}th,td{text-align:left;padding:.7rem .85rem;border-bottom:1px solid #ece7df;}th{font-size:.85rem;letter-spacing:.04em;text-transform:uppercase;color:#6a6257;}a{color:#004f59;text-decoration:none;}a:hover{text-decoration:underline;}.dir a{font-weight:700;}</style>");
    html.push_str("</head><body>");
    html.push_str(&format!("<h1>{}</h1>", escape_html(&title)));
    html.push_str(&format!("<p>{} · base remote: {}</p>", escape_html(provider_label), escape_html(remote_path)));
    html.push_str("<table><thead><tr><th>Name</th><th>Size</th><th>Modified</th></tr></thead><tbody>");

    if let Some(parent) = parent_request_path(current_relative_path) {
        html.push_str(&format!(
            "<tr class=\"dir\"><td><a href=\"{}\">../</a></td><td>-</td><td>-</td></tr>",
            encode_request_path(&parent)
        ));
    }

    for entry in entries {
        let request_path = child_request_path(current_relative_path, &entry.name, entry.is_dir);
        let size = if entry.is_dir {
            "-".to_string()
        } else {
            format_size(entry.size)
        };
        let modified = entry.modified.as_deref().unwrap_or("-");
        let label = if entry.is_dir {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };
        let row_class = if entry.is_dir { "dir" } else { "file" };
        html.push_str(&format!(
            "<tr class=\"{}\"><td><a href=\"{}\">{}</a></td><td>{}</td><td>{}</td></tr>",
            row_class,
            encode_request_path(&request_path),
            escape_html(&label),
            escape_html(&size),
            escape_html(modified)
        ));
    }

    html.push_str("</tbody></table></body></html>");
    html
}

async fn serve_http_response(
    state: ServeHttpState,
    relative_path: String,
    head_only: bool,
    range: Option<&HeaderValue>,
) -> Response {
    let relative_path = match sanitize_served_relative_path(&relative_path) {
        Ok(path) => path,
        Err(status) => return serve_error_response(status, "Invalid request path"),
    };

    let remote_path = build_served_remote_path(&state.base_path, &relative_path);
    let mut provider = state.provider.lock().await;

    let stat_result = if relative_path.is_empty() {
        Ok(RemoteEntry::directory("/".to_string(), remote_path.clone()))
    } else {
        provider.stat(&remote_path).await
    };

    match stat_result {
        Ok(entry) if entry.is_dir => {
            let mut entries = match provider.list(&remote_path).await {
                Ok(entries) => entries,
                Err(error) => {
                    return serve_error_response(
                        provider_error_to_status_code(&error),
                        &error.to_string(),
                    )
                }
            };
            entries.sort_by(|left, right| match (left.is_dir, right.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
            });
            let html = render_directory_listing(
                &state.provider_label,
                &relative_path,
                &remote_path,
                &entries,
            );
            build_html_response(StatusCode::OK, html, head_only)
        }
        Ok(entry) => {
            if head_only {
                return build_file_head_response(&entry);
            }

            if entry.size > MAX_DOWNLOAD_TO_BYTES {
                return serve_error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!(
                        "File too large for serve http MVP ({} > {} bytes)",
                        entry.size, MAX_DOWNLOAD_TO_BYTES
                    ),
                );
            }

            let bytes = match provider.download_to_bytes(&remote_path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return serve_error_response(
                        provider_error_to_status_code(&error),
                        &error.to_string(),
                    )
                }
            };

            build_file_get_response(&entry, bytes, range)
        }
        Err(ProviderError::NotFound(_)) => match provider.list(&remote_path).await {
            Ok(mut entries) => {
                entries.sort_by(|left, right| match (left.is_dir, right.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                });
                let html = render_directory_listing(
                    &state.provider_label,
                    &relative_path,
                    &remote_path,
                    &entries,
                );
                build_html_response(StatusCode::OK, html, head_only)
            }
            Err(error) => serve_error_response(provider_error_to_status_code(&error), &error.to_string()),
        },
        Err(error) => serve_error_response(provider_error_to_status_code(&error), &error.to_string()),
    }
}

async fn serve_http_root(
    State(state): State<ServeHttpState>,
    headers: HeaderMap,
) -> Response {
    let range = headers.get(axum::http::header::RANGE);
    serve_http_response(state, String::new(), false, range).await
}

async fn serve_http_root_head(State(state): State<ServeHttpState>) -> Response {
    serve_http_response(state, String::new(), true, None).await
}

async fn serve_http_path(
    State(state): State<ServeHttpState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let range = headers.get(axum::http::header::RANGE);
    serve_http_response(state, path, false, range).await
}

async fn serve_http_path_head(
    State(state): State<ServeHttpState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    serve_http_response(state, path, true, None).await
}

async fn cmd_serve_http(url: &str, path: &str, addr: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(value) => value,
        Err(code) => return code,
    };

    let bind_addr = match addr.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(error) => {
            print_error(format, &format!("Invalid --addr '{}': {}", addr, error), 5);
            return 5;
        }
    };

    let base_path = serve_effective_base_path(path, &url_path);
    let provider_label = if let Some(profile) = &cli.profile {
        format!("profile {}", profile)
    } else {
        provider.display_name()
    };

    let state = ServeHttpState {
        provider: Arc::new(AsyncMutex::new(provider)),
        provider_label,
        base_path: base_path.clone(),
    };

    let app = Router::new()
        .route("/", get(serve_http_root).head(serve_http_root_head))
        .route("/{*path}", get(serve_http_path).head(serve_http_path_head))
        .with_state(state.clone());

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            print_error(format, &format!("Failed to bind {}: {}", addr, error), 1);
            return 1;
        }
    };

    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({
                "status": "serving",
                "protocol": "http",
                "addr": addr,
                "base_path": base_path,
            })
        );
    } else if !cli.quiet {
        eprintln!("Serving HTTP on http://{}", addr);
        eprintln!("Remote base path: {}", state.base_path);
        eprintln!("Press Ctrl+C to stop.");
    }

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;

    let mut provider = state.provider.lock().await;
    let _ = provider.disconnect().await;

    match result {
        Ok(()) => 0,
        Err(error) => {
            print_error(format, &format!("HTTP server failed: {}", error), 1);
            1
        }
    }
}

// ── WebDAV serve ──────────────────────────────────────────────────

/// Maximum upload size for WebDAV PUT (512 MB).
const WEBDAV_MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;

fn webdav_xml_entry(href: &str, entry: &RemoteEntry) -> String {
    let mut props = String::new();
    if entry.is_dir {
        props.push_str("<D:resourcetype><D:collection/></D:resourcetype>");
    } else {
        props.push_str("<D:resourcetype/>");
        props.push_str(&format!(
            "<D:getcontentlength>{}</D:getcontentlength>",
            entry.size
        ));
    }
    props.push_str(&format!(
        "<D:displayname>{}</D:displayname>",
        escape_html(&entry.name)
    ));
    if let Some(ref modified) = entry.modified {
        props.push_str(&format!(
            "<D:getlastmodified>{}</D:getlastmodified>",
            escape_html(modified)
        ));
    }
    if !entry.is_dir {
        let ct = guess_content_type(entry);
        props.push_str(&format!("<D:getcontenttype>{}</D:getcontenttype>", ct));
    }
    format!(
        "<D:response><D:href>{}</D:href><D:propstat><D:prop>{}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        escape_html(href),
        props
    )
}

fn build_propfind_xml(
    base_path: &str,
    relative_path: &str,
    self_entry: &RemoteEntry,
    children: Option<&[RemoteEntry]>,
) -> String {
    let self_href = if relative_path.is_empty() {
        "/".to_string()
    } else {
        encode_request_path(&format!("/{}", relative_path.trim_matches('/')))
    };

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n",
    );
    xml.push_str(&webdav_xml_entry(
        &if self_entry.is_dir {
            format!("{}/", self_href.trim_end_matches('/'))
        } else {
            self_href.clone()
        },
        self_entry,
    ));
    xml.push('\n');

    if let Some(entries) = children {
        let _ = base_path; // used for context, href is relative to serve root
        for entry in entries {
            let child = child_request_path(relative_path, &entry.name, entry.is_dir);
            let child_href = encode_request_path(&format!("/{}", child.trim_start_matches('/')));
            xml.push_str(&webdav_xml_entry(&child_href, entry));
            xml.push('\n');
        }
    }

    xml.push_str("</D:multistatus>\n");
    xml
}

fn webdav_multistatus_response(xml: String) -> Response {
    let mut response = Response::new(Body::from(xml));
    *response.status_mut() = StatusCode::MULTI_STATUS;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    response
}

fn extract_destination_relative(headers: &HeaderMap) -> Result<String, StatusCode> {
    let dest = headers
        .get("Destination")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    // Destination is a full URI like http://127.0.0.1:8080/path
    // Extract the path portion
    let path_part = if let Some(idx) = dest.find("://") {
        let after_scheme = &dest[idx + 3..];
        after_scheme.find('/').map_or("/", |i| &after_scheme[i..])
    } else {
        dest
    };

    sanitize_served_relative_path(path_part).map_err(|_| StatusCode::BAD_REQUEST)
}

async fn webdav_dispatch(
    state: ServeHttpState,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let relative_path = match sanitize_served_relative_path(&path) {
        Ok(p) => p,
        Err(status) => return serve_error_response(status, "Invalid path"),
    };
    let remote_path = build_served_remote_path(&state.base_path, &relative_path);

    match method.as_str() {
        "OPTIONS" => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().insert(
                "Allow",
                HeaderValue::from_static(
                    "OPTIONS, GET, HEAD, PUT, DELETE, MKCOL, MOVE, COPY, PROPFIND",
                ),
            );
            response.headers_mut().insert(
                "DAV",
                HeaderValue::from_static("1"),
            );
            response
        }

        "PROPFIND" => {
            let depth = headers
                .get("Depth")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("1");

            let mut provider = state.provider.lock().await;

            // Stat the target
            let self_entry = if relative_path.is_empty() {
                RemoteEntry::directory("/".to_string(), remote_path.clone())
            } else {
                match provider.stat(&remote_path).await {
                    Ok(e) => e,
                    Err(ProviderError::NotFound(_)) => {
                        // Might be a directory that doesn't support stat
                        RemoteEntry::directory(
                            remote_path
                                .rsplit('/')
                                .next()
                                .unwrap_or("")
                                .to_string(),
                            remote_path.clone(),
                        )
                    }
                    Err(e) => {
                        return serve_error_response(
                            provider_error_to_status_code(&e),
                            &e.to_string(),
                        )
                    }
                }
            };

            let children = if self_entry.is_dir && depth != "0" {
                match provider.list(&remote_path).await {
                    Ok(mut entries) => {
                        entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                            (true, false) => std::cmp::Ordering::Less,
                            (false, true) => std::cmp::Ordering::Greater,
                            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                        });
                        Some(entries)
                    }
                    Err(_) => None,
                }
            } else {
                None
            };

            let xml = build_propfind_xml(
                &state.base_path,
                &relative_path,
                &self_entry,
                children.as_deref(),
            );
            webdav_multistatus_response(xml)
        }

        "GET" => {
            let range = headers.get(axum::http::header::RANGE);
            serve_http_response(state, path, false, range).await
        }

        "HEAD" => serve_http_response(state, path, true, None).await,

        "PUT" => {
            if body.len() > WEBDAV_MAX_UPLOAD_BYTES {
                return serve_error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!(
                        "Upload too large ({} > {} bytes)",
                        body.len(),
                        WEBDAV_MAX_UPLOAD_BYTES
                    ),
                );
            }

            // Write body to temp file, then upload
            let temp = match NamedTempFile::new() {
                Ok(t) => t,
                Err(e) => {
                    return serve_error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("Cannot create temp file: {}", e),
                    )
                }
            };
            if let Err(e) = std::fs::write(temp.path(), &body) {
                return serve_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("Cannot write temp file: {}", e),
                );
            }

            let mut provider = state.provider.lock().await;
            match provider
                .upload(&temp.path().to_string_lossy(), &remote_path, None)
                .await
            {
                Ok(()) => {
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = StatusCode::CREATED;
                    response
                }
                Err(e) => serve_error_response(provider_error_to_status_code(&e), &e.to_string()),
            }
        }

        "MKCOL" => {
            let mut provider = state.provider.lock().await;
            match provider.mkdir(&remote_path).await {
                Ok(()) => {
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = StatusCode::CREATED;
                    response
                }
                Err(e) => serve_error_response(provider_error_to_status_code(&e), &e.to_string()),
            }
        }

        "DELETE" => {
            let mut provider = state.provider.lock().await;
            // Try file delete first; on any failure try rmdir (target may be a directory)
            match provider.delete(&remote_path).await {
                Ok(()) => {
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = StatusCode::NO_CONTENT;
                    response
                }
                Err(_file_err) => match provider.rmdir(&remote_path).await {
                    Ok(()) => {
                        let mut response = Response::new(Body::empty());
                        *response.status_mut() = StatusCode::NO_CONTENT;
                        response
                    }
                    Err(_) => match provider.rmdir_recursive(&remote_path).await {
                        Ok(()) => {
                            let mut response = Response::new(Body::empty());
                            *response.status_mut() = StatusCode::NO_CONTENT;
                            response
                        }
                        Err(e) => serve_error_response(
                            provider_error_to_status_code(&e),
                            &e.to_string(),
                        ),
                    },
                },
            }
        }

        "MOVE" => {
            let dest_relative = match extract_destination_relative(&headers) {
                Ok(d) => d,
                Err(status) => {
                    return serve_error_response(status, "Invalid or missing Destination header")
                }
            };
            let dest_remote = build_served_remote_path(&state.base_path, &dest_relative);
            let mut provider = state.provider.lock().await;
            match provider.rename(&remote_path, &dest_remote).await {
                Ok(()) => {
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = StatusCode::NO_CONTENT;
                    response
                }
                Err(e) => serve_error_response(provider_error_to_status_code(&e), &e.to_string()),
            }
        }

        "COPY" => {
            let dest_relative = match extract_destination_relative(&headers) {
                Ok(d) => d,
                Err(status) => {
                    return serve_error_response(status, "Invalid or missing Destination header")
                }
            };
            let dest_remote = build_served_remote_path(&state.base_path, &dest_relative);
            let mut provider = state.provider.lock().await;
            if provider.supports_server_copy() {
                match provider.server_copy(&remote_path, &dest_remote).await {
                    Ok(()) => {
                        let mut response = Response::new(Body::empty());
                        *response.status_mut() = StatusCode::CREATED;
                        response
                    }
                    Err(e) => {
                        serve_error_response(provider_error_to_status_code(&e), &e.to_string())
                    }
                }
            } else {
                // Fallback: download then upload
                match provider.download_to_bytes(&remote_path).await {
                    Ok(data) => {
                        let temp = match NamedTempFile::new() {
                            Ok(t) => t,
                            Err(e) => {
                                return serve_error_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    &format!("Temp file error: {}", e),
                                )
                            }
                        };
                        if let Err(e) = std::fs::write(temp.path(), &data) {
                            return serve_error_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                &format!("Write error: {}", e),
                            );
                        }
                        match provider
                            .upload(&temp.path().to_string_lossy(), &dest_remote, None)
                            .await
                        {
                            Ok(()) => {
                                let mut response = Response::new(Body::empty());
                                *response.status_mut() = StatusCode::CREATED;
                                response
                            }
                            Err(e) => serve_error_response(
                                provider_error_to_status_code(&e),
                                &e.to_string(),
                            ),
                        }
                    }
                    Err(e) => {
                        serve_error_response(provider_error_to_status_code(&e), &e.to_string())
                    }
                }
            }
        }

        _ => serve_error_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

async fn webdav_root_handler(
    State(state): State<ServeHttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    webdav_dispatch(state, method, String::new(), headers, body).await
}

async fn webdav_path_handler(
    State(state): State<ServeHttpState>,
    method: Method,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    webdav_dispatch(state, method, path, headers, body).await
}

async fn cmd_serve_webdav(
    url: &str,
    path: &str,
    addr: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(value) => value,
        Err(code) => return code,
    };

    let bind_addr = match addr.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(error) => {
            print_error(
                format,
                &format!("Invalid --addr '{}': {}", addr, error),
                5,
            );
            return 5;
        }
    };

    let base_path = serve_effective_base_path(path, &url_path);
    let provider_label = if let Some(profile) = &cli.profile {
        format!("profile {}", profile)
    } else {
        provider.display_name()
    };

    let state = ServeHttpState {
        provider: Arc::new(AsyncMutex::new(provider)),
        provider_label,
        base_path: base_path.clone(),
    };

    let app = Router::new()
        .route("/", any(webdav_root_handler))
        .route("/{*path}", any(webdav_path_handler))
        .layer(DefaultBodyLimit::max(WEBDAV_MAX_UPLOAD_BYTES))
        .with_state(state.clone());

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            print_error(format, &format!("Failed to bind {}: {}", addr, error), 1);
            return 1;
        }
    };

    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({
                "status": "serving",
                "protocol": "webdav",
                "addr": addr,
                "base_path": base_path,
            })
        );
    } else if !cli.quiet {
        eprintln!("Serving WebDAV on http://{}", addr);
        eprintln!("Remote base path: {}", state.base_path);
        eprintln!("Read-write mode. Press Ctrl+C to stop.");
    }

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;

    let mut provider = state.provider.lock().await;
    let _ = provider.disconnect().await;

    match result {
        Ok(()) => 0,
        Err(error) => {
            print_error(
                format,
                &format!("WebDAV server failed: {}", error),
                1,
            );
            1
        }
    }
}

async fn cmd_connect(url: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let start = Instant::now();
    let spinner = if matches!(format, OutputFormat::Text) && !cli.quiet {
        Some(create_spinner("Connecting..."))
    } else {
        None
    };

    let (mut provider, _path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => {
            if let Some(sp) = spinner { sp.finish_and_clear(); }
            return code;
        }
    };

    let elapsed = start.elapsed();
    let server_info = provider.server_info().await.ok();
    let pt = provider.provider_type();
    let host = provider.display_name();
    let port = display_port_for_provider(&pt, server_info.as_deref());
    let user = String::new();

    if let Some(sp) = spinner { sp.finish_and_clear(); }

    match format {
        OutputFormat::Text => {
            eprintln!("Connected to {} ({})", host, pt);
            eprintln!("  User:     {}", user);
            eprintln!("  Port:     {}", port);
            eprintln!("  Protocol: {}", pt);
            if let Some(ref info) = server_info {
                if !info.is_empty() {
                    eprintln!("  Server:   {}", info);
                }
            }
            eprintln!("  Time:     {:.0}ms", elapsed.as_millis());

            // Try to show storage info
            if let Ok(storage) = provider.storage_info().await {
                if storage.total > 0 {
                    let pct = (storage.used as f64 / storage.total as f64) * 100.0;
                    eprintln!(
                        "  Storage:  {} / {} ({:.1}% used)",
                        format_size(storage.used),
                        format_size(storage.total),
                        pct
                    );
                }
            }
        }
        OutputFormat::Json => {
            print_json(&CliConnectResult {
                status: "ok",
                protocol: pt.to_string(),
                host,
                port,
                username: user,
                server_info,
                elapsed_ms: elapsed.as_millis() as u64,
            });
        }
    }

    let _ = provider.disconnect().await;
    0
}

#[allow(clippy::too_many_arguments)]
async fn cmd_ls(
    url: &str,
    path: &str,
    long: bool,
    sort: &str,
    reverse: bool,
    all: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let effective_path = if path == "/" && url_path != "/" {
        &url_path
    } else {
        path
    };

    let entries = match provider.list(effective_path).await {
        Ok(e) => e,
        Err(e) => {
            print_error(format, &format!("ls failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    // Filter hidden files
    let mut entries: Vec<RemoteEntry> = if all {
        entries
    } else {
        entries.into_iter().filter(|e| !e.name.starts_with('.')).collect()
    };

    // Apply global filters (--include, --exclude, --min-size, --max-size, --min-age, --max-age)
    if has_filters(cli) {
        let filter = build_filter(cli);
        entries.retain(|e| {
            if e.is_dir { return true; } // Don't filter directories in ls
            filter(&e.name, e.size, None)
        });
    }

    // Sort
    match sort {
        "size" => entries.sort_by(|a, b| a.size.cmp(&b.size)),
        "date" => entries.sort_by(|a, b| a.modified.cmp(&b.modified)),
        _ => entries.sort_by(|a, b| {
            // Directories first, then alphabetical
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        }),
    }
    if reverse {
        entries.reverse();
    }

    // Summary
    let file_count = entries.iter().filter(|e| !e.is_dir).count();
    let dir_count = entries.iter().filter(|e| e.is_dir).count();
    let total_bytes: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();

    match format {
        OutputFormat::Text => {
            if entries.is_empty() {
                if !cli.quiet {
                    println!("(empty directory)");
                }
            } else if long {
                // Long format: permissions  size  date  name
                for e in &entries {
                    let perms = e.permissions.as_deref().unwrap_or(if e.is_dir { "drwxr-xr-x" } else { "-rw-r--r--" });
                    let size_str = if e.is_dir {
                        "       -".to_string()
                    } else {
                        format!("{:>8}", format_size(e.size))
                    };
                    let date = e.modified.as_deref().unwrap_or("-");
                    // Truncate date to first 16 chars (YYYY-MM-DD HH:MM), safely on char boundary
                    let date_short = if date.len() > 16 {
                        date.get(..16).unwrap_or(date)
                    } else {
                        date
                    };
                    let safe_name = sanitize_filename(&e.name);
                    let name = if e.is_dir {
                        format!("{}/", safe_name)
                    } else {
                        safe_name
                    };
                    println!("{}  {}  {}  {}", perms, size_str, date_short, name);
                }
            } else {
                // Short format: just names
                for e in &entries {
                    let safe_name = sanitize_filename(&e.name);
                    if e.is_dir {
                        println!("{}/", safe_name);
                    } else {
                        println!("{}", safe_name);
                    }
                }
            }

            if !cli.quiet {
                eprintln!(
                    "\n{} items ({} directories, {} files) — {} total",
                    entries.len(),
                    dir_count,
                    file_count,
                    format_size(total_bytes)
                );
            }
        }
        OutputFormat::Json => {
            let entries_json: Vec<serde_json::Value> = entries
                .iter()
                .map(|entry| remote_entry_to_filtered_json(entry, cli))
                .collect();
            print_json(&serde_json::json!({
                "status": "ok",
                "path": effective_path,
                "entries": entries_json,
                "summary": {
                    "total": entries.len(),
                    "files": file_count,
                    "dirs": dir_count,
                    "total_bytes": total_bytes,
                }
            }));
        }
    }

    let _ = provider.disconnect().await;
    0
}

#[allow(clippy::too_many_arguments)]
async fn cmd_get(
    url: &str,
    remote: &str,
    local: Option<&str>,
    recursive: bool,
    segments: usize,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    if remote.trim().is_empty() {
        print_error(format, "Missing remote path for get", 5);
        return 5;
    }

    if recursive {
        return cmd_get_recursive(url, remote, local, cli, format, cancelled).await;
    }

    // Check for glob patterns
    if remote.contains('*') || remote.contains('?') {
        return cmd_get_glob(url, remote, local, cli, format, cancelled).await;
    }

    let (mut provider, _url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let filename = remote.rsplit('/').next().unwrap_or("download");
    let local_path = local.unwrap_or(filename);
    let start = Instant::now();

    // Get file size for progress bar
    let total_size = provider.size(remote).await.unwrap_or(0);

    // ── Segmented parallel download (pget) ──
    let segments = segments.clamp(1, 16);
    if segments > 1 && total_size > 0 {
        let hints = provider.transfer_optimization_hints();
        let quiet = cli.quiet || matches!(format, OutputFormat::Json);
        if hints.supports_range_download && total_size >= PGET_MIN_FILE_SIZE {
            let _ = provider.disconnect().await;
            return pget_segmented_download(url, remote, local_path, segments, total_size, cli, format).await;
        } else if !quiet {
            let reason = if !hints.supports_range_download {
                "provider does not support range downloads"
            } else {
                &format!("file too small (< {})", format_size(PGET_MIN_FILE_SIZE))
            };
            eprintln!("pget: falling back to single download ({})", reason);
        }
    }

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let pb = if !quiet && total_size > 0 {
        Some(create_progress_bar(filename, total_size))
    } else if !quiet {
        Some(create_spinner(&format!("Downloading {}...", filename)))
    } else {
        None
    };

    let pb_clone = pb.clone();
    let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = if pb_clone.is_some() {
        Some(Box::new(move |transferred, total| {
            if let Some(ref pb) = pb_clone {
                if total > 0 {
                    pb.set_length(total);
                }
                pb.set_position(transferred);
            }
        }))
    } else {
        None
    };

    match download_with_resume(&mut *provider, remote, local_path, cli, progress_cb).await {
        Ok(()) => {
            let elapsed = start.elapsed();
            let file_size = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
            let speed = if elapsed.as_secs_f64() > 0.0 {
                (file_size as f64 / elapsed.as_secs_f64()) as u64
            } else {
                0
            };

            if let Some(pb) = pb {
                pb.finish_and_clear();
            }

            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        println!(
                            "{} → {} ({}, {}, {:.1}s)",
                            remote,
                            local_path,
                            format_size(file_size),
                            format_speed(speed),
                            elapsed.as_secs_f64()
                        );
                    }
                }
                OutputFormat::Json => {
                    print_json(&CliTransferResult {
                        status: "ok",
                        operation: "download".to_string(),
                        path: remote.to_string(),
                        bytes: file_size,
                        elapsed_secs: elapsed.as_secs_f64(),
                        speed_bps: speed,
                    });
                }
            }

            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            if let Some(pb) = pb {
                pb.finish_and_clear();
            }
            if !cli.partial {
                let _ = std::fs::remove_file(local_path);
            }
            print_error(format, &format!("Download failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

// ── Segmented Parallel Download (pget) ────────────────────────────────

const PGET_MIN_FILE_SIZE: u64 = 4 * 1024 * 1024; // 4 MB minimum for segmented download
const PGET_MIN_CHUNK_SIZE: u64 = 1024 * 1024; // 1 MB minimum per chunk
const PGET_SUB_READ_SIZE: u64 = 64 * 1024 * 1024; // 64 MB max per read_range call

struct PgetChunk {
    index: usize,
    offset: u64,
    length: u64,
}

fn plan_pget_chunks(file_size: u64, segments: usize) -> Vec<PgetChunk> {
    if file_size == 0 || segments == 0 {
        return Vec::new();
    }

    let segments = segments.clamp(1, 16);

    // Reduce segment count if chunks would be too small
    let actual_segments = {
        let chunk = file_size / segments as u64;
        if chunk < PGET_MIN_CHUNK_SIZE {
            (file_size / PGET_MIN_CHUNK_SIZE).max(1) as usize
        } else {
            segments
        }
    };

    let base = file_size / actual_segments as u64;
    let remainder = file_size % actual_segments as u64;

    let mut chunks = Vec::with_capacity(actual_segments);
    let mut offset = 0u64;
    for i in 0..actual_segments {
        let length = base + if (i as u64) < remainder { 1 } else { 0 };
        chunks.push(PgetChunk { index: i, offset, length });
        offset += length;
    }
    chunks
}

/// RAII guard that removes a temp directory on drop
struct PgetTempGuard(String);
impl Drop for PgetTempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn pget_segmented_download(
    url: &str,
    remote_path: &str,
    local_path: &str,
    segments: usize,
    file_size: u64,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let chunks = plan_pget_chunks(file_size, segments);
    let actual_segments = chunks.len();

    if actual_segments <= 1 {
        // Degenerate: only 1 segment, fall through to normal download would be redundant
        // but we already disconnected the initial provider, so reconnect and do single download
        return pget_fallback_single(url, remote_path, local_path, cli, format).await;
    }

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    if !quiet {
        eprintln!(
            "pget: {} in {} segments ({} each)",
            format_size(file_size),
            actual_segments,
            format_size(file_size / actual_segments as u64),
        );
    }

    // Create temp directory for chunk files
    let temp_dir = format!("{}.aeroftp-pget-{}", local_path, uuid::Uuid::new_v4());
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        print_error(format, &format!("pget: failed to create temp dir: {}", e), 4);
        return 4;
    }
    let _temp_guard = PgetTempGuard(temp_dir.clone());

    // Progress bar
    let filename = remote_path.rsplit('/').next().unwrap_or("download");
    let pb = if !quiet {
        Some(create_progress_bar(&format!("{} (pget x{})", filename, actual_segments), file_size))
    } else {
        None
    };
    let aggregate = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    // Download all chunks concurrently, each with its own connection
    let workers = effective_parallel_workers(cli).min(actual_segments);
    let results: Vec<Result<(), String>> = futures_util::stream::iter(chunks.iter().map(|chunk| {
        let url = url.to_string();
        let remote = remote_path.to_string();
        let temp_dir = temp_dir.clone();
        let aggregate = aggregate.clone();
        let pb = pb.clone();
        let offset = chunk.offset;
        let length = chunk.length;
        let idx = chunk.index;
        async move {
            pget_download_chunk(&url, &remote, &temp_dir, idx, offset, length, aggregate, pb, cli, format).await
        }
    }))
    .buffer_unordered(workers)
    .collect()
    .await;

    // Check for chunk errors
    let errors: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    if !errors.is_empty() {
        if let Some(ref pb) = pb { pb.finish_and_clear(); }
        for err in &errors {
            print_error(format, &format!("pget: {}", err), 4);
        }
        // _temp_guard cleans up
        return 4;
    }

    // Assemble chunks into final file
    if let Err(e) = pget_assemble_chunks(&temp_dir, local_path, actual_segments).await {
        if let Some(ref pb) = pb { pb.finish_and_clear(); }
        print_error(format, &format!("pget assembly failed: {}", e), 4);
        return 4;
    }

    if let Some(pb) = pb { pb.finish_and_clear(); }

    let elapsed = start.elapsed();
    let speed = if elapsed.as_secs_f64() > 0.0 {
        (file_size as f64 / elapsed.as_secs_f64()) as u64
    } else {
        0
    };

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "{} → {} ({}, {}, {:.1}s, {} segments)",
                    remote_path, local_path,
                    format_size(file_size), format_speed(speed),
                    elapsed.as_secs_f64(), actual_segments,
                );
            }
        }
        OutputFormat::Json => {
            print_json(&CliTransferResult {
                status: "ok",
                operation: "download".to_string(),
                path: remote_path.to_string(),
                bytes: file_size,
                elapsed_secs: elapsed.as_secs_f64(),
                speed_bps: speed,
            });
        }
    }

    0
}

#[allow(clippy::too_many_arguments)]
async fn pget_download_chunk(
    url: &str,
    remote_path: &str,
    temp_dir: &str,
    chunk_index: usize,
    offset: u64,
    length: u64,
    aggregate: Arc<AtomicU64>,
    pb: Option<ProgressBar>,
    cli: &Cli,
    format: OutputFormat,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let (mut provider, _) = create_and_connect(url, cli, format)
        .await
        .map_err(|code| format!("chunk {}: connection failed (exit code {})", chunk_index, code))?;

    let chunk_path = format!("{}/chunk_{:04}", temp_dir, chunk_index);
    let mut file = tokio::fs::File::create(&chunk_path)
        .await
        .map_err(|e| format!("chunk {}: create file failed: {}", chunk_index, e))?;

    // Stream range data in sub-reads to bound memory usage
    let mut downloaded = 0u64;
    while downloaded < length {
        let remaining = length - downloaded;
        let read_size = remaining.min(PGET_SUB_READ_SIZE);
        let data = provider.read_range(remote_path, offset + downloaded, read_size)
            .await
            .map_err(|e| format!("chunk {}: read_range at offset {} failed: {}", chunk_index, offset + downloaded, e))?;

        if data.is_empty() {
            break;
        }

        file.write_all(&data)
            .await
            .map_err(|e| format!("chunk {}: write failed: {}", chunk_index, e))?;

        downloaded += data.len() as u64;
        let new_total = aggregate.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64;
        if let Some(ref pb) = pb {
            pb.set_position(new_total);
        }
    }

    file.flush().await.map_err(|e| format!("chunk {}: flush failed: {}", chunk_index, e))?;
    let _ = provider.disconnect().await;
    Ok(())
}

async fn pget_assemble_chunks(temp_dir: &str, dest_path: &str, num_chunks: usize) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let temp_dest = format!("{}.aeroftp-assemble-{}.tmp", dest_path, uuid::Uuid::new_v4());
    let mut dest = tokio::fs::File::create(&temp_dest)
        .await
        .map_err(|e| format!("failed to create destination temp file: {}", e))?;

    let mut buf = vec![0u8; 256 * 1024]; // 256 KB copy buffer

    for i in 0..num_chunks {
        let chunk_path = format!("{}/chunk_{:04}", temp_dir, i);
        let mut src = tokio::fs::File::open(&chunk_path)
            .await
            .map_err(|e| {
                let _ = std::fs::remove_file(&temp_dest);
                format!("failed to open chunk {}: {}", i, e)
            })?;

        loop {
            let n = src.read(&mut buf)
                .await
                .map_err(|e| {
                    let _ = std::fs::remove_file(&temp_dest);
                    format!("failed to read chunk {}: {}", i, e)
                })?;
            if n == 0 { break; }
            dest.write_all(&buf[..n])
                .await
                .map_err(|e| {
                    let _ = std::fs::remove_file(&temp_dest);
                    format!("failed to write chunk {}: {}", i, e)
                })?;
        }
    }

    dest.flush().await.map_err(|e| {
        let _ = std::fs::remove_file(&temp_dest);
        format!("failed to flush destination: {}", e)
    })?;
    drop(dest);

    if Path::new(dest_path).exists() {
        std::fs::remove_file(dest_path).map_err(|e| {
            let _ = std::fs::remove_file(&temp_dest);
            format!("failed to replace destination: {}", e)
        })?;
    }
    std::fs::rename(&temp_dest, dest_path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_dest);
        format!("failed to finalize destination: {}", e)
    })?;
    Ok(())
}

/// Fallback to a normal single-stream download (used when pget degrades)
async fn pget_fallback_single(
    url: &str,
    remote_path: &str,
    local_path: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    if !quiet {
        eprintln!("pget: using single download (only 1 effective segment)");
    }

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let filename = remote_path.rsplit('/').next().unwrap_or("download");
    let total_size = provider.size(remote_path).await.unwrap_or(0);
    let start = Instant::now();

    let pb = if !quiet && total_size > 0 {
        Some(create_progress_bar(filename, total_size))
    } else {
        None
    };

    let pb_clone = pb.clone();
    let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = pb_clone.map(|pb| {
        Box::new(move |transferred: u64, total: u64| {
            if total > 0 { pb.set_length(total); }
            pb.set_position(transferred);
        }) as Box<dyn Fn(u64, u64) + Send>
    });

    match download_with_resume(&mut *provider, remote_path, local_path, cli, progress_cb).await {
        Ok(()) => {
            let elapsed = start.elapsed();
            let file_size = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
            let speed = if elapsed.as_secs_f64() > 0.0 { (file_size as f64 / elapsed.as_secs_f64()) as u64 } else { 0 };
            if let Some(pb) = pb { pb.finish_and_clear(); }
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        println!("{} → {} ({}, {}, {:.1}s)", remote_path, local_path, format_size(file_size), format_speed(speed), elapsed.as_secs_f64());
                    }
                }
                OutputFormat::Json => {
                    print_json(&CliTransferResult {
                        status: "ok", operation: "download".to_string(), path: remote_path.to_string(),
                        bytes: file_size, elapsed_secs: elapsed.as_secs_f64(), speed_bps: speed,
                    });
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            if let Some(pb) = pb { pb.finish_and_clear(); }
            if !cli.partial { let _ = std::fs::remove_file(local_path); }
            let code = provider_error_to_exit_code(&e);
            print_error(format, &format!("Download failed: {}", e), code);
            let _ = provider.disconnect().await;
            code
        }
    }
}

async fn cmd_get_recursive(
    url: &str,
    remote_dir: &str,
    local_base: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let local_base = local_base.unwrap_or(".");
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let mp = MultiProgress::new();

    let spinner = if !quiet {
        Some(mp.add(create_spinner("Scanning remote directory...")))
    } else {
        None
    };

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let mut queue: Vec<(String, usize)> = vec![(remote_dir.to_string(), 0)];
    let mut files: Vec<(String, String, u64)> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();

    while let Some((dir, depth)) = queue.pop() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        if depth >= MAX_SCAN_DEPTH {
            if !quiet {
                eprintln!("Warning: max depth {} reached at {}", MAX_SCAN_DEPTH, dir);
            }
            continue;
        }
        if files.len() + dirs.len() >= MAX_SCAN_ENTRIES {
            if !quiet {
                eprintln!("Warning: max entries {} reached, stopping scan", MAX_SCAN_ENTRIES);
            }
            break;
        }
        match provider.list(&dir).await {
            Ok(entries) => {
                for e in entries {
                    if e.is_dir {
                        queue.push((e.path.clone(), depth + 1));
                        dirs.push(e.path);
                    } else {
                        let relative = e.path.strip_prefix(remote_dir).unwrap_or(&e.path).trim_start_matches('/');
                        let Some(relative) = validate_relative_path(relative) else {
                            continue;
                        };
                        let local_path_buf = Path::new(local_base).join(relative);
                        if verify_path_within_root(&local_path_buf, Path::new(local_base)).is_ok() {
                            files.push((e.path, local_path_buf.to_string_lossy().to_string(), e.size));
                        }
                    }
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("Warning: cannot list {}: {}", dir, e);
                }
            }
        }
    }

    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

    for dir in &dirs {
        let relative = dir.strip_prefix(remote_dir).unwrap_or(dir).trim_start_matches('/');
        let Some(relative) = validate_relative_path(relative) else {
            if !quiet {
                eprintln!("Warning: skipping unsafe directory path: {}", dir);
            }
            continue;
        };
        let local_dir = Path::new(local_base).join(relative);
        if verify_path_within_root(&local_dir, Path::new(local_base)).is_ok() {
            let _ = std::fs::create_dir_all(&local_dir);
        }
    }

    let _ = provider.disconnect().await;

    let total_bytes: u64 = files.iter().map(|(_, _, size)| *size).sum();
    let total_files = files.len();
    if !quiet {
        eprintln!(
            "Found {} files ({}) in {} directories using {} workers",
            total_files,
            format_size(total_bytes),
            dirs.len() + 1,
            effective_parallel_workers(cli)
        );
    }

    let start = Instant::now();
    let aggregate = Arc::new(AtomicU64::new(0));
    let overall_pb = if !quiet && total_bytes > 0 {
        Some(mp.add(create_overall_progress_bar(total_files, total_bytes)))
    } else {
        None
    };

    let results = futures_util::stream::iter(files.into_iter().map(|(remote_path, local_path, _size)| {
        let cancelled = cancelled.clone();
        let aggregate = aggregate.clone();
        let overall_pb = overall_pb.clone();
        async move {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled by user".to_string());
            }
            if let Some(parent) = Path::new(&local_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let result = download_transfer_task(url, remote_path.clone(), local_path, cli, format, Some(aggregate), overall_pb).await;
            result.map(|_| remote_path)
        }
    }))
    .buffer_unordered(effective_parallel_workers(cli))
    .collect::<Vec<_>>()
    .await;

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }

    let mut downloaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(_) => downloaded += 1,
            Err(err) => errors.push(err),
        }
    }

    let elapsed = start.elapsed();
    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "Downloaded {}/{} files ({}) in {:.1}s",
                    downloaded,
                    total_files,
                    format_size(total_bytes),
                    elapsed.as_secs_f64()
                );
                for err in &errors {
                    eprintln!("  Error: {}", err);
                }
            }
        }
        OutputFormat::Json => {
            print_json(&CliSyncResult {
                status: if errors.is_empty() { "ok" } else { "partial" },
                uploaded: 0,
                downloaded,
                deleted: 0,
                skipped: (total_files as u32).saturating_sub(downloaded).saturating_sub(errors.len() as u32),
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
            });
        }
    }

    if downloaded == total_files as u32 { 0 } else { 4 }
}

async fn cmd_get_glob(
    url: &str,
    pattern: &str,
    local_base: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let local_base = local_base.unwrap_or(".");

    // Split pattern into directory + glob
    let (dir, glob_pattern) = if let Some(pos) = pattern.rfind('/') {
        (&pattern[..pos], &pattern[pos + 1..])
    } else {
        ("/", pattern)
    };
    let dir = if dir.is_empty() { "/" } else { dir };

    let matcher = match globset::Glob::new(glob_pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            print_error(format, &format!("Invalid glob pattern: {}", e), 5);
            return 5;
        }
    };

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let entries = match provider.list(dir).await {
        Ok(e) => e,
        Err(e) => {
            print_error(format, &format!("ls failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    let matched: Vec<&RemoteEntry> = entries
        .iter()
        .filter(|e| !e.is_dir && matcher.is_match(&e.name))
        .collect();

    if matched.is_empty() {
        if !cli.quiet {
            match format {
                OutputFormat::Text => eprintln!("No files matching '{}'", glob_pattern),
                OutputFormat::Json => print_json(&CliOk {
                    status: "ok",
                    message: format!("No files matching '{}'", glob_pattern),
                }),
            }
        }
        let _ = provider.disconnect().await;
        return 0;
    }

    let _ = provider.disconnect().await;

    let start = Instant::now();
    let total = matched.len();
    let total_bytes: u64 = matched.iter().map(|entry| entry.size).sum();
    let mut downloaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    let aggregate = Arc::new(AtomicU64::new(0));
    let overall_pb = if !cli.quiet && matches!(format, OutputFormat::Text) && total_bytes > 0 {
        Some(create_overall_progress_bar(total, total_bytes))
    } else {
        None
    };

    let _ = std::fs::create_dir_all(local_base);

    let results = futures_util::stream::iter(matched.into_iter().map(|entry| {
        let cancelled = cancelled.clone();
        let aggregate = aggregate.clone();
        let overall_pb = overall_pb.clone();
        let local_path = format!("{}/{}", local_base, entry.name);
        async move {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled by user".to_string());
            }
            if validate_relative_path(&entry.name).is_none() {
                return Err(format!("{}: unsafe path (traversal rejected)", entry.name));
            }
            download_transfer_task(url, entry.path.clone(), local_path.clone(), cli, format, Some(aggregate), overall_pb)
                .await
                .map(|_| entry.name.clone())
        }
    }))
    .buffer_unordered(effective_parallel_workers(cli))
    .collect::<Vec<_>>()
    .await;

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }

    for result in results {
        match result {
            Ok(_) => downloaded += 1,
            Err(err) => errors.push(err),
        }
    }

    let elapsed = start.elapsed();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "\n{}/{} files downloaded in {:.1}s",
                    downloaded, total, elapsed.as_secs_f64()
                );
            }
        }
        OutputFormat::Json => {
            print_json(&CliSyncResult {
                status: if errors.is_empty() { "ok" } else { "partial" },
                uploaded: 0,
                downloaded,
                deleted: 0,
                skipped: 0,
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
            });
        }
    }

    if downloaded == total as u32 { 0 } else { 4 }
}

async fn cmd_put(
    url: &str,
    local: &str,
    remote: Option<&str>,
    recursive: bool,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    if local.trim().is_empty() {
        print_error(format, "Missing local path for put", 5);
        return 5;
    }

    if recursive {
        return cmd_put_recursive(url, local, remote, cli, format, cancelled).await;
    }

    // Check for glob patterns in local path
    if local.contains('*') || local.contains('?') {
        return cmd_put_glob(url, local, remote, cli, format, cancelled).await;
    }

    let (mut provider, _url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let filename = Path::new(local)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| local.to_string());
    let remote_path = remote.unwrap_or(&filename);

    let file_size = match std::fs::metadata(local) {
        Ok(m) => m.len(),
        Err(e) => {
            print_error(format, &format!("Cannot read local file '{}': {}", local, e), 2);
            return 2;
        }
    };

    let start = Instant::now();
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);

    let pb = if !quiet && file_size > 0 {
        Some(create_progress_bar(&filename, file_size))
    } else {
        None
    };

    let pb_clone = pb.clone();
    let progress_cb: Option<Box<dyn Fn(u64, u64) + Send>> = if pb_clone.is_some() {
        Some(Box::new(move |transferred, total| {
            if let Some(ref pb) = pb_clone {
                if total > 0 {
                    pb.set_length(total);
                }
                pb.set_position(transferred);
            }
        }))
    } else {
        None
    };

    match upload_with_resume(&mut *provider, local, remote_path, cli, progress_cb).await {
        Ok(()) => {
            let elapsed = start.elapsed();
            let speed = if elapsed.as_secs_f64() > 0.0 {
                (file_size as f64 / elapsed.as_secs_f64()) as u64
            } else {
                0
            };

            if let Some(pb) = pb {
                pb.finish_and_clear();
            }

            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        println!(
                            "{} → {} ({}, {}, {:.1}s)",
                            local,
                            remote_path,
                            format_size(file_size),
                            format_speed(speed),
                            elapsed.as_secs_f64()
                        );
                    }
                }
                OutputFormat::Json => {
                    print_json(&CliTransferResult {
                        status: "ok",
                        operation: "upload".to_string(),
                        path: remote_path.to_string(),
                        bytes: file_size,
                        elapsed_secs: elapsed.as_secs_f64(),
                        speed_bps: speed,
                    });
                }
            }

            // Allow SSH transport to flush in-flight write data before closing.
            // russh 0.57 buffers SFTP writes; disconnect before flush produces 0-byte files.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            if let Some(pb) = pb {
                pb.finish_and_clear();
            }
            print_error(format, &format!("Upload failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_put_recursive(
    url: &str,
    local_dir: &str,
    remote_base: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let remote_base = match remote_base.unwrap_or("/").trim_end_matches('/') {
        "" => "/".to_string(),
        value => value.to_string(),
    };
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);

    // Walk local directory (bounded: max 100 levels deep, 500K entries)
    const MAX_SCAN_DEPTH_PUT: usize = 100;
    const MAX_SCAN_ENTRIES_PUT: usize = 500_000;
    let walker = walkdir::WalkDir::new(local_dir).follow_links(false).max_depth(MAX_SCAN_DEPTH_PUT);
    let mut files: Vec<(String, String, u64)> = Vec::new(); // (local, remote, size)
    let mut dirs: Vec<String> = Vec::new();

    for entry in walker {
        if files.len() + dirs.len() >= MAX_SCAN_ENTRIES_PUT {
            eprintln!("Warning: scan capped at {} entries", MAX_SCAN_ENTRIES_PUT);
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                if !quiet {
                    eprintln!("Warning: {}", e);
                }
                continue;
            }
        };

        let relative = entry
            .path()
            .strip_prefix(local_dir)
            .unwrap_or(entry.path());
        let relative_str = relative.to_string_lossy().replace('\\', "/");
        if relative_str.is_empty() {
            continue;
        }

        let remote_path = if remote_base == "/" {
            format!("/{}", relative_str)
        } else {
            format!("{}/{}", remote_base, relative_str)
        };

        if entry.file_type().is_dir() {
            dirs.push(remote_path);
        } else if entry.file_type().is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push((entry.path().to_string_lossy().to_string(), remote_path, size));
        }
    }

    let total_bytes: u64 = files.iter().map(|(_, _, s)| *s).sum();
    let total_files = files.len();

    if remote_base != "/" {
        dirs.push(remote_base.clone());
    }
    dirs.sort_by(|left, right| {
        let left_depth = left.matches('/').count();
        let right_depth = right.matches('/').count();
        left_depth.cmp(&right_depth).then_with(|| left.cmp(right))
    });
    dirs.dedup();

    if !quiet {
        eprintln!(
            "Found {} files ({}) in {} directories",
            total_files,
            format_size(total_bytes),
            dirs.len()
        );
    }

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    for dir in &dirs {
        let _ = provider.mkdir(dir).await;
    }
    let _ = provider.disconnect().await;

    // Upload files
    let start = Instant::now();
    let aggregate = Arc::new(AtomicU64::new(0));
    let overall_pb = if !quiet && total_bytes > 0 {
        Some(create_overall_progress_bar(total_files, total_bytes))
    } else {
        None
    };

    let results = futures_util::stream::iter(files.into_iter().map(|(local_path, remote_path, _size)| {
        let cancelled = cancelled.clone();
        let aggregate = aggregate.clone();
        let overall_pb = overall_pb.clone();
        async move {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled by user".to_string());
            }
            upload_transfer_task(url, local_path.clone(), remote_path.clone(), cli, format, Some(aggregate), overall_pb)
                .await
                .map(|_| local_path)
        }
    }))
    .buffer_unordered(effective_parallel_workers(cli))
    .collect::<Vec<_>>()
    .await;

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }

    let mut uploaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(_) => uploaded += 1,
            Err(err) => errors.push(err),
        }
    }

    let elapsed = start.elapsed();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "\nUploaded {}/{} files ({}) in {:.1}s",
                    uploaded, total_files, format_size(total_bytes), elapsed.as_secs_f64()
                );
                for err in &errors {
                    eprintln!("  Error: {}", err);
                }
            }
        }
        OutputFormat::Json => {
            print_json(&CliSyncResult {
                status: if errors.is_empty() { "ok" } else { "partial" },
                uploaded,
                downloaded: 0,
                deleted: 0,
                skipped: 0,
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
            });
        }
    }
    if uploaded == total_files as u32 { 0 } else { 4 }
}

async fn cmd_mkdir(url: &str, path: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    match provider.mkdir(path).await {
        Ok(()) => {
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        eprintln!("Created directory: {}", path);
                    }
                }
                OutputFormat::Json => print_json(&CliOk {
                    status: "ok",
                    message: format!("Created directory: {}", path),
                }),
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("mkdir failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_rm(
    url: &str,
    path: &str,
    recursive: bool,
    force: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Confirmation for recursive delete (unless --force)
    if recursive && !force && std::io::stdin().is_terminal() {
        eprint!("Recursively delete '{}'? [y/N]: ", path);
        let _ = io::stderr().flush();
        let mut input = String::new();
        let _ = io::stdin().read_line(&mut input);
        if !input.trim().eq_ignore_ascii_case("y") {
            if !cli.quiet {
                eprintln!("Aborted.");
            }
            let _ = provider.disconnect().await;
            return 0;
        }
    }

    let result = if recursive {
        provider.rmdir_recursive(path).await
    } else {
        // Try file delete first, then directory
        match provider.delete(path).await {
            Ok(()) => Ok(()),
            Err(_) => provider.rmdir(path).await,
        }
    };

    match result {
        Ok(()) => {
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        eprintln!("Deleted: {}", path);
                    }
                }
                OutputFormat::Json => print_json(&CliOk {
                    status: "ok",
                    message: format!("Deleted: {}", path),
                }),
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("rm failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_mv(url: &str, from: &str, to: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    match provider.rename(from, to).await {
        Ok(()) => {
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        eprintln!("{} → {}", from, to);
                    }
                }
                OutputFormat::Json => print_json(&CliOk {
                    status: "ok",
                    message: format!("{} → {}", from, to),
                }),
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("mv failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_cp(url: &str, from: &str, to: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    if !provider.supports_server_copy() {
        print_error(format, "Server-side copy is not supported by this provider", 7);
        let _ = provider.disconnect().await;
        return 7;
    }

    match provider.server_copy(from, to).await {
        Ok(()) => {
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        eprintln!("{} ⇒ {}", from, to);
                    }
                }
                OutputFormat::Json => print_json(&CliOk {
                    status: "ok",
                    message: format!("{} ⇒ {}", from, to),
                }),
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("cp failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

/// Parse a human-friendly duration string into seconds.
/// Accepts: "1h", "24h", "7d", "30d", or raw seconds like "3600".
fn parse_expires(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_suffix('h') {
        h.parse::<u64>().ok().map(|v| v * 3600)
    } else if let Some(d) = s.strip_suffix('d') {
        d.parse::<u64>().ok().map(|v| v * 86400)
    } else {
        s.parse::<u64>().ok()
    }
}

async fn cmd_link(url: &str, path: &str, expires: Option<&str>, password: Option<&str>, permissions: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    if !provider.supports_share_links() {
        print_error(format, "Share links are not supported by this provider", 7);
        let _ = provider.disconnect().await;
        return 7;
    }

    let expires_in_secs = expires.and_then(parse_expires);
    if expires.is_some() && expires_in_secs.is_none() {
        print_error(format, "Invalid --expires format. Use: 1h, 24h, 7d, 30d, or seconds", 1);
        let _ = provider.disconnect().await;
        return 1;
    }

    let options = ShareLinkOptions {
        expires_in_secs,
        password: password.map(|s| s.to_string()),
        permissions: if permissions == "view" { None } else { Some(permissions.to_string()) },
    };

    match provider.create_share_link(path, options).await {
        Ok(result) => {
            match format {
                OutputFormat::Text => {
                    println!("{}", result.url);
                    if let Some(ref pw) = result.password {
                        eprintln!("Password: {}", pw);
                    }
                    if let Some(ref exp) = result.expires_at {
                        eprintln!("Expires: {}", exp);
                    }
                }
                OutputFormat::Json => {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "path": path,
                        "url": result.url,
                        "password": result.password,
                        "expires_at": result.expires_at,
                    }));
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("link failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_edit(
    url: &str,
    path: &str,
    find: &str,
    replace: &str,
    replace_all: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    if path.is_empty() {
        print_error(format, "edit requires a remote path", 5);
        return 5;
    }
    if find.is_empty() {
        print_error(format, "edit requires a non-empty find string", 5);
        return 5;
    }

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let data = match provider.download_to_bytes(path).await {
        Ok(data) => data,
        Err(e) => {
            print_error(format, &format!("edit failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    let mut content = match String::from_utf8(data) {
        Ok(content) => content,
        Err(_) => {
            print_error(format, "edit supports only UTF-8 text files", 5);
            let _ = provider.disconnect().await;
            return 5;
        }
    };
    content = content.strip_prefix('\u{FEFF}').unwrap_or(&content).to_string();

    let occurrences = content.matches(find).count();
    if occurrences == 0 {
        match format {
            OutputFormat::Text => {
                if !cli.quiet {
                    eprintln!("No matches found in {}", path);
                }
            }
            OutputFormat::Json => {
                print_json(&serde_json::json!({
                    "status": "ok",
                    "path": path,
                    "occurrences": 0,
                    "replaced": 0,
                    "message": "No matches found",
                }));
            }
        }
        let _ = provider.disconnect().await;
        return 0;
    }

    let new_content = if replace_all {
        content.replace(find, replace)
    } else {
        content.replacen(find, replace, 1)
    };
    let replaced = if replace_all { occurrences } else { 1 };

    let mut temp_file = match NamedTempFile::new() {
        Ok(file) => file,
        Err(e) => {
            print_error(format, &format!("edit failed: cannot create temp file: {}", e), 99);
            let _ = provider.disconnect().await;
            return 99;
        }
    };

    if let Err(e) = temp_file.write_all(new_content.as_bytes()) {
        print_error(format, &format!("edit failed: cannot write temp file: {}", e), 99);
        let _ = provider.disconnect().await;
        return 99;
    }

    let temp_path = temp_file.path().to_string_lossy().to_string();
    match provider.upload(&temp_path, path, None).await {
        Ok(()) => {
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        eprintln!("Replaced {} occurrence(s) in {}", replaced, path);
                    }
                }
                OutputFormat::Json => {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "path": path,
                        "occurrences": occurrences,
                        "replaced": replaced,
                        "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
                    }));
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("edit failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_cat(url: &str, path: &str, cli: &Cli, format: OutputFormat) -> i32 {
    const MAX_CAT_SIZE: u64 = 256 * 1024 * 1024; // 256 MB

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Guard: reject files larger than MAX_CAT_SIZE to prevent OOM
    if let Ok(size) = provider.size(path).await {
        if size > MAX_CAT_SIZE {
            print_error(
                format,
                &format!("File too large for cat ({}). Use 'get' instead.", format_size(size)),
                5,
            );
            let _ = provider.disconnect().await;
            return 5;
        }
    }

    match provider.download_to_bytes(path).await {
        Ok(data) => {
            match format {
                OutputFormat::Text => {
                    // Warn if binary content is being sent to a terminal
                    if io::stdout().is_terminal()
                        && data
                            .iter()
                            .take(8192)
                            .any(|&b| b == 0 || (b < 32 && b != b'\n' && b != b'\r' && b != b'\t'))
                    {
                        eprintln!("Warning: binary content detected. Pipe to file: aeroftp-cli cat ... > output.bin");
                    }
                    let stdout = io::stdout();
                    let mut handle = stdout.lock();
                    let _ = handle.write_all(&data);
                    let _ = handle.flush();
                }
                OutputFormat::Json => {
                    // For JSON, encode as UTF-8 string or base64 for binary
                    if let Ok(text) = String::from_utf8(data.clone()) {
                        print_json(&CliOk {
                            status: "ok",
                            message: text,
                        });
                    } else {
                        #[derive(Serialize)]
                        struct CatBinaryResult {
                            status: &'static str,
                            content: String,
                            encoding: &'static str,
                            size: usize,
                        }
                        print_json(&CatBinaryResult {
                            status: "ok",
                            content: base64::engine::general_purpose::STANDARD.encode(&data),
                            encoding: "base64",
                            size: data.len(),
                        });
                    }
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("cat failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_rcat(url: &str, remote: &str, cli: &Cli, format: OutputFormat) -> i32 {
    if remote.trim().is_empty() {
        print_error(format, "Missing remote path for rcat", 5);
        return 5;
    }

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let mut temp = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(e) => {
            print_error(format, &format!("Cannot create temporary file for stdin upload: {}", e), 5);
            let _ = provider.disconnect().await;
            return 5;
        }
    };

    let bytes = match io::copy(&mut io::stdin().lock(), &mut temp) {
        Ok(bytes) => bytes,
        Err(e) => {
            print_error(format, &format!("Cannot read stdin: {}", e), 2);
            let _ = provider.disconnect().await;
            return 2;
        }
    };

    if let Err(e) = temp.flush() {
        print_error(format, &format!("Cannot flush temporary stdin file: {}", e), 5);
        let _ = provider.disconnect().await;
        return 5;
    }

    let start = Instant::now();
    match provider.upload(temp.path().to_string_lossy().as_ref(), remote, None).await {
        Ok(()) => {
            let elapsed = start.elapsed();
            let speed = if elapsed.as_secs_f64() > 0.0 {
                (bytes as f64 / elapsed.as_secs_f64()) as u64
            } else {
                0
            };
            match format {
                OutputFormat::Text => {
                    if !cli.quiet {
                        println!(
                            "stdin → {} ({}, {}, {:.1}s)",
                            remote,
                            format_size(bytes),
                            format_speed(speed),
                            elapsed.as_secs_f64()
                        );
                    }
                }
                OutputFormat::Json => {
                    print_json(&CliTransferResult {
                        status: "ok",
                        operation: "upload-stdin".to_string(),
                        path: remote.to_string(),
                        bytes,
                        elapsed_secs: elapsed.as_secs_f64(),
                        speed_bps: speed,
                    });
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("stdin upload failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

fn cmd_alias(command: &AliasCommands, format: OutputFormat) -> i32 {
    let mut config = match load_cli_config() {
        Ok(config) => config,
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    match command {
        AliasCommands::Set { name, command } => {
            config.aliases.insert(name.clone(), command.clone());
            match save_cli_config(&config) {
                Ok(path) => match format {
                    OutputFormat::Text => {
                        println!("Alias '{}' saved in {}", name, path.display());
                    }
                    OutputFormat::Json => {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "alias": name,
                            "command": command,
                            "config": path.display().to_string(),
                        }));
                    }
                },
                Err(e) => {
                    print_error(format, &e, 5);
                    return 5;
                }
            }
            0
        }
        AliasCommands::Remove { name } => {
            if config.aliases.remove(name).is_none() {
                print_error(format, &format!("Alias not found: {}", name), 2);
                return 2;
            }
            match save_cli_config(&config) {
                Ok(path) => match format {
                    OutputFormat::Text => {
                        println!("Alias '{}' removed from {}", name, path.display());
                    }
                    OutputFormat::Json => {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "alias": name,
                            "removed": true,
                            "config": path.display().to_string(),
                        }));
                    }
                },
                Err(e) => {
                    print_error(format, &e, 5);
                    return 5;
                }
            }
            0
        }
        AliasCommands::Show { name } => {
            let Some(alias) = config.aliases.get(name) else {
                print_error(format, &format!("Alias not found: {}", name), 2);
                return 2;
            };
            match format {
                OutputFormat::Text => println!("{} = {}", name, alias.join(" ")),
                OutputFormat::Json => {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "alias": name,
                        "command": alias,
                    }));
                }
            }
            0
        }
        AliasCommands::List => {
            let mut aliases: Vec<_> = config.aliases.iter().collect();
            aliases.sort_by(|(left, _), (right, _)| left.cmp(right));
            match format {
                OutputFormat::Text => {
                    for (name, command) in aliases {
                        println!("{} = {}", name, command.join(" "));
                    }
                }
                OutputFormat::Json => {
                    let aliases_json: Vec<_> = aliases
                        .into_iter()
                        .map(|(name, command)| {
                            serde_json::json!({
                                "name": name,
                                "command": command,
                            })
                        })
                        .collect();
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "aliases": aliases_json,
                    }));
                }
            }
            0
        }
    }
}

async fn cmd_stat(url: &str, path: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    match provider.stat(path).await {
        Ok(entry) => {
            match format {
                OutputFormat::Text => {
                    println!("  Name:        {}", entry.name);
                    println!("  Path:        {}", entry.path);
                    println!(
                        "  Type:        {}",
                        if entry.is_dir { "directory" } else { "file" }
                    );
                    if !entry.is_dir {
                        println!("  Size:        {} ({} bytes)", format_size(entry.size), entry.size);
                    }
                    if let Some(ref m) = entry.modified {
                        println!("  Modified:    {}", m);
                    }
                    if let Some(ref p) = entry.permissions {
                        println!("  Permissions: {}", p);
                    }
                    if let Some(ref o) = entry.owner {
                        println!("  Owner:       {}", o);
                    }
                    if let Some(ref g) = entry.group {
                        println!("  Group:       {}", g);
                    }
                    if entry.is_symlink {
                        if let Some(ref t) = entry.link_target {
                            println!("  Link target: {}", t);
                        }
                    }
                }
                OutputFormat::Json => {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "entry": remote_entry_to_filtered_json(&entry, cli),
                    }));
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("stat failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_find(
    url: &str,
    path: &str,
    pattern: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Try provider.find() first, fallback to recursive list + glob
    let results = match provider.find(path, pattern).await {
        Ok(entries) => entries,
        Err(ProviderError::NotSupported(_)) => {
            // Fallback: recursive list + glob filter
            let matcher = match globset::Glob::new(pattern) {
                Ok(g) => g.compile_matcher(),
                Err(e) => {
                    print_error(format, &format!("Invalid pattern: {}", e), 5);
                    let _ = provider.disconnect().await;
                    return 5;
                }
            };

            let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];
            let mut found = Vec::new();
            let mut scanned: usize = 0;

            while let Some((dir, depth)) = queue.pop() {
                if depth >= MAX_SCAN_DEPTH {
                    continue;
                }
                if let Ok(entries) = provider.list(&dir).await {
                    for e in entries {
                        scanned += 1;
                        if scanned >= MAX_SCAN_ENTRIES {
                            if !cli.quiet {
                                eprintln!("Warning: scan limit reached ({} entries), results may be incomplete", MAX_SCAN_ENTRIES);
                            }
                            break;
                        }
                        if e.is_dir {
                            queue.push((e.path.clone(), depth + 1));
                        }
                        if matcher.is_match(&e.name) {
                            found.push(e);
                        }
                    }
                }
                if scanned >= MAX_SCAN_ENTRIES {
                    break;
                }
            }
            found
        }
        Err(e) => {
            print_error(format, &format!("find failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    match format {
        OutputFormat::Text => {
            for e in &results {
                println!("{}", sanitize_filename(&e.path));
            }
            if !cli.quiet {
                eprintln!("\n{} matches", results.len());
            }
        }
        OutputFormat::Json => {
            let file_count = results.iter().filter(|e| !e.is_dir).count();
            let dir_count = results.iter().filter(|e| e.is_dir).count();
            let total_bytes: u64 = results.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let entries_json: Vec<serde_json::Value> = results
                .iter()
                .map(|entry| remote_entry_to_filtered_json(entry, cli))
                .collect();
            print_json(&serde_json::json!({
                "status": "ok",
                "path": path,
                "entries": entries_json,
                "summary": {
                    "total": results.len(),
                    "files": file_count,
                    "dirs": dir_count,
                    "total_bytes": total_bytes,
                }
            }));
        }
    }

    let _ = provider.disconnect().await;
    0
}

async fn cmd_df(url: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    match provider.storage_info().await {
        Ok(info) => {
            let pct = if info.total > 0 {
                (info.used as f64 / info.total as f64) * 100.0
            } else {
                0.0
            };

            match format {
                OutputFormat::Text => {
                    println!("Storage usage:");
                    println!("  Used:  {} ({:.1}%)", format_size(info.used), pct);
                    println!("  Free:  {}", format_size(info.free));
                    println!("  Total: {}", format_size(info.total));

                    // Visual bar
                    let bar_width: usize = 40;
                    let filled = (((pct.min(100.0)) / 100.0) * bar_width as f64) as usize;
                    let empty = bar_width.saturating_sub(filled);
                    println!(
                        "  [{}{}] {:.1}%",
                        "━".repeat(filled),
                        "─".repeat(empty),
                        pct
                    );
                }
                OutputFormat::Json => {
                    print_json(&CliStorageResult {
                        status: "ok",
                        used: info.used,
                        total: info.total,
                        free: info.free,
                        used_percent: pct,
                    });
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(format, &format!("df failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_about(url: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let provider_name = provider.display_name();
    let provider_type = provider.provider_type().to_string();
    let server_info = provider.server_info().await.ok().unwrap_or_default();
    let storage = provider.storage_info().await.ok();

    match format {
        OutputFormat::Text => {
            eprintln!("Provider:  {} ({})", provider_name, provider_type);
            if !server_info.is_empty() {
                eprintln!("Server:    {}", server_info);
            }
            if let Some(ref info) = storage {
                let pct = if info.total > 0 {
                    (info.used as f64 / info.total as f64) * 100.0
                } else { 0.0 };
                eprintln!("Used:      {} ({:.1}%)", format_size(info.used), pct);
                eprintln!("Free:      {}", format_size(info.free));
                eprintln!("Total:     {}", format_size(info.total));
            }
        }
        OutputFormat::Json => {
            let mut result = serde_json::json!({
                "status": "ok",
                "provider": provider_type,
                "display_name": provider_name,
                "server_info": server_info,
            });
            if let Some(info) = storage {
                result["used"] = serde_json::json!(info.used);
                result["total"] = serde_json::json!(info.total);
                result["free"] = serde_json::json!(info.free);
                result["used_percent"] = serde_json::json!(
                    if info.total > 0 { (info.used as f64 / info.total as f64) * 100.0 } else { 0.0 }
                );
            }
            print_json(&apply_top_level_json_field_filter(result, cli, &["status"]));
        }
    }
    let _ = provider.disconnect().await;
    0
}

fn write_speed_test_file(path: &Path, size: u64) -> Result<(), String> {
    let mut file = std::fs::File::create(path)
        .map_err(|e| format!("Cannot create speed test payload: {}", e))?;
    let chunk = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let next = remaining.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..next])
            .map_err(|e| format!("Cannot write speed test payload: {}", e))?;
        remaining -= next as u64;
    }
    file.flush()
        .map_err(|e| format!("Cannot flush speed test payload: {}", e))?;
    Ok(())
}

async fn cmd_speed(
    url: &str,
    test_size: &str,
    iterations: u32,
    remote_path: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let iterations = iterations.clamp(1, 10);
    let size = match parse_size_filter(test_size) {
        Ok(size) if size > 0 => size,
        Ok(_) => {
            print_error(format, "Speed test size must be greater than zero", 5);
            return 5;
        }
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    let remote_test_path = remote_path
        .map(|path| path.to_string())
        .unwrap_or_else(|| format!("/aeroftp_speed_test_{}.bin", uuid::Uuid::new_v4()));

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let local_upload = match NamedTempFile::new() {
        Ok(file) => file,
        Err(e) => {
            print_error(format, &format!("Cannot create temp file: {}", e), 5);
            let _ = provider.disconnect().await;
            return 5;
        }
    };
    if let Err(e) = write_speed_test_file(local_upload.path(), size) {
        print_error(format, &e, 5);
        let _ = provider.disconnect().await;
        return 5;
    }

    let mut upload_total = 0u64;
    let mut download_total = 0u64;
    let start = Instant::now();

    for iteration in 0..iterations {
        let upload_start = Instant::now();
        if let Err(e) = provider
            .upload(
                local_upload.path().to_string_lossy().as_ref(),
                &remote_test_path,
                None,
            )
            .await
        {
            print_error(
                format,
                &format!("speed test upload failed on iteration {}: {}", iteration + 1, e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.delete(&remote_test_path).await;
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
        let upload_elapsed = upload_start.elapsed().as_secs_f64();
        if upload_elapsed > 0.0 {
            upload_total += (size as f64 / upload_elapsed) as u64;
        }

        let local_download = match NamedTempFile::new() {
            Ok(file) => file,
            Err(e) => {
                print_error(format, &format!("Cannot create temp file: {}", e), 5);
                let _ = provider.delete(&remote_test_path).await;
                let _ = provider.disconnect().await;
                return 5;
            }
        };

        let download_start = Instant::now();
        if let Err(e) = provider
            .download(
                &remote_test_path,
                local_download.path().to_string_lossy().as_ref(),
                None,
            )
            .await
        {
            print_error(
                format,
                &format!("speed test download failed on iteration {}: {}", iteration + 1, e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.delete(&remote_test_path).await;
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
        let download_elapsed = download_start.elapsed().as_secs_f64();
        if download_elapsed > 0.0 {
            download_total += (size as f64 / download_elapsed) as u64;
        }
    }

    let _ = provider.delete(&remote_test_path).await;
    let _ = provider.disconnect().await;

    let upload_speed = upload_total / iterations as u64;
    let download_speed = download_total / iterations as u64;
    let elapsed = start.elapsed().as_secs_f64();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!("Speed test complete ({} iteration(s), {})", iterations, format_size(size));
                println!("  Upload:   {}", format_speed(upload_speed));
                println!("  Download: {}", format_speed(download_speed));
                println!("  Remote:   {}", remote_test_path);
            }
        }
        OutputFormat::Json => {
            print_json(&CliSpeedResult {
                status: "ok",
                remote_path: remote_test_path,
                test_size: size,
                iterations,
                upload_speed_bps: upload_speed,
                download_speed_bps: download_speed,
                elapsed_secs: elapsed,
            });
        }
    }

    0
}

async fn cmd_dedupe(
    url: &str, path: &str, mode: &str, dry_run: bool,
    cli: &Cli, format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    if !quiet { eprintln!("Scanning {} for duplicates...", path); }

    // BFS scan to collect all files with sizes
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut dirs = vec![path.to_string()];
    let max_entries = 100_000usize;

    while let Some(dir) = dirs.pop() {
        if files.len() >= max_entries { break; }
        match provider.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    if entry.is_dir {
                        dirs.push(entry.path.clone());
                    } else {
                        files.push((entry.path.clone(), entry.size));
                    }
                }
            }
            Err(_) => continue,
        }
    }

    if !quiet { eprintln!("Scanned {} files. Grouping by size...", files.len()); }

    // Group by size (fast pre-filter)
    let mut size_groups: std::collections::HashMap<u64, Vec<String>> = std::collections::HashMap::new();
    for (path, size) in &files {
        if *size > 0 { // Skip empty files
            size_groups.entry(*size).or_default().push(path.clone());
        }
    }

    // Filter to groups with >1 file (potential duplicates)
    let candidate_groups: Vec<(u64, Vec<String>)> = size_groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();

    if candidate_groups.is_empty() {
        if !quiet { eprintln!("No potential duplicates found."); }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({"status": "ok", "groups": 0, "duplicates": 0}));
        }
        let _ = provider.disconnect().await;
        return 0;
    }

    if !quiet {
        eprintln!("{} size groups with potential duplicates. Hashing...", candidate_groups.len());
    }

    // Hash files within each group to confirm duplicates
    let mut duplicate_groups: Vec<Vec<String>> = Vec::new();
    let mut total_duplicates = 0u32;
    let mut wasted_bytes = 0u64;

    for (size, paths) in &candidate_groups {
        let mut hash_map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for p in paths {
            match provider.download_to_bytes(p).await {
                Ok(data) => {
                    use sha2::Digest;
                    let hash = format!("{:x}", sha2::Sha256::digest(&data));
                    hash_map.entry(hash).or_default().push(p.clone());
                }
                Err(_) => continue,
            }
        }
        for (_, group) in hash_map {
            if group.len() > 1 {
                let dupes = group.len() as u32 - 1;
                total_duplicates += dupes;
                wasted_bytes += size * dupes as u64;
                duplicate_groups.push(group);
            }
        }
    }

    if duplicate_groups.is_empty() {
        if !quiet { eprintln!("No duplicates found (same size but different content)."); }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({"status": "ok", "groups": 0, "duplicates": 0}));
        }
        let _ = provider.disconnect().await;
        return 0;
    }

    // Report duplicates
    match format {
        OutputFormat::Text => {
            eprintln!("\nFound {} duplicate group(s), {} duplicate file(s), {} wasted",
                duplicate_groups.len(), total_duplicates, format_size(wasted_bytes));

            for (i, group) in duplicate_groups.iter().enumerate() {
                eprintln!("\n  Group {} ({} files):", i + 1, group.len());
                for (j, p) in group.iter().enumerate() {
                    let marker = if j == 0 { "KEEP" } else {
                        match mode {
                            "skip" => "DUPE",
                            _ => "DELETE",
                        }
                    };
                    eprintln!("    [{}] {}", marker, p);
                }
            }

            if !dry_run && mode != "skip" {
                // Delete duplicates (keep first in each group)
                let mut deleted = 0u32;
                for group in &duplicate_groups {
                    for p in group.iter().skip(1) {
                        match provider.delete(p).await {
                            Ok(()) => { deleted += 1; }
                            Err(e) => { eprintln!("  Failed to delete {}: {}", p, e); }
                        }
                    }
                }
                eprintln!("\nDeleted {} duplicate file(s).", deleted);
            } else if dry_run {
                eprintln!("\n(dry run — no files deleted)");
            }
        }
        OutputFormat::Json => {
            let groups_json: Vec<serde_json::Value> = duplicate_groups.iter().map(|g| {
                serde_json::json!({
                    "files": g,
                    "keep": g[0],
                    "duplicates": &g[1..],
                })
            }).collect();
            print_json(&serde_json::json!({
                "status": "ok",
                "groups": duplicate_groups.len(),
                "duplicates": total_duplicates,
                "wasted_bytes": wasted_bytes,
                "mode": mode,
                "dry_run": dry_run,
                "details": groups_json,
            }));
        }
    }

    let _ = provider.disconnect().await;
    0
}

#[allow(clippy::too_many_arguments)]
async fn cmd_sync(
    url: &str,
    local: &str,
    remote: &str,
    direction: &str,
    dry_run: bool,
    delete: bool,
    exclude: &[String],
    track_renames: bool,
    max_delete: Option<&str>,
    _backup_dir: Option<&str>,
    _backup_suffix: &str,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let start = Instant::now();

    if !quiet {
        eprintln!("Scanning local: {}", local);
        eprintln!("Scanning remote: {}", remote);
    }

    // Pre-compile exclude matchers (avoids O(n*m) recompilation)
    let exclude_matchers: Vec<globset::GlobMatcher> = exclude
        .iter()
        .filter_map(|pat| globset::Glob::new(pat).ok().map(|g| g.compile_matcher()))
        .collect();

    // Scan local files (bounded: max 100 levels, 500K entries)
    let local_entries: Vec<(String, u64, Option<String>)> = {
        let walker = walkdir::WalkDir::new(local).follow_links(false).max_depth(100);
        let mut entries = Vec::new();
        for entry in walker {
            if entries.len() >= 500_000 {
                eprintln!("Warning: local scan capped at 500,000 entries");
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().is_dir() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(local)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            if relative.is_empty() {
                continue;
            }

            // Check excludes (pre-compiled matchers)
            let fname = entry.file_name().to_string_lossy();
            let fname_ref: &str = fname.as_ref();
            if exclude_matchers
                .iter()
                .any(|m| m.is_match(&relative) || m.is_match(fname_ref))
            {
                continue;
            }

            let meta = entry.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta.and_then(|m| {
                m.modified().ok().map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
                })
            });
            entries.push((relative, size, mtime));
        }
        entries
    };

    // Scan remote files (recursive, depth and entry limited)
    let mut remote_entries: Vec<(String, u64, Option<String>)> = Vec::new();
    {
        let mut queue: Vec<(String, usize)> = vec![(remote.to_string(), 0)];
        while let Some((dir, depth)) = queue.pop() {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            if depth >= MAX_SCAN_DEPTH {
                if !quiet {
                    eprintln!("Warning: max scan depth reached at {}", dir);
                }
                continue;
            }
            if remote_entries.len() >= MAX_SCAN_ENTRIES {
                if !quiet {
                    eprintln!("Warning: max entries reached during remote scan");
                }
                break;
            }
            match provider.list(&dir).await {
                Ok(entries) => {
                    for e in entries {
                        if e.is_dir {
                            queue.push((e.path.clone(), depth + 1));
                        } else {
                            let relative = e
                                .path
                                .strip_prefix(remote)
                                .unwrap_or(&e.path)
                                .trim_start_matches('/')
                                .to_string();
                            if !relative.is_empty() {
                                // Apply exclude patterns to remote entries too
                                if exclude_matchers
                                    .iter()
                                    .any(|m| m.is_match(&relative) || m.is_match(&e.name))
                                {
                                    continue;
                                }
                                remote_entries.push((relative, e.size, e.modified));
                            }
                        }
                    }
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("Warning: cannot scan {}: {}", dir, e);
                    }
                }
            }
        }
    }

    // Build comparison
    let local_map: HashMap<&str, (u64, Option<&str>)> = local_entries
        .iter()
        .map(|(p, s, m)| (p.as_str(), (*s, m.as_deref())))
        .collect();
    let remote_map: HashMap<&str, (u64, Option<&str>)> = remote_entries
        .iter()
        .map(|(p, s, m)| (p.as_str(), (*s, m.as_deref())))
        .collect();

    let mut to_upload: Vec<&str> = Vec::new();
    let mut to_download: Vec<&str> = Vec::new();
    let mut to_delete_remote: Vec<&str> = Vec::new();
    let mut to_delete_local: Vec<&str> = Vec::new();
    let mut skipped: u32 = 0;

    // Files to upload (local → remote)
    if direction == "upload" || direction == "both" {
        for (path, (size, _mtime)) in &local_map {
            if let Some((rsize, _rmtime)) = remote_map.get(path) {
                if size == rsize {
                    skipped += 1;
                } else {
                    to_upload.push(path);
                }
            } else {
                to_upload.push(path);
            }
        }
    }

    // Files to download (remote → local)
    if direction == "download" || direction == "both" {
        for (path, (size, _mtime)) in &remote_map {
            if let Some((lsize, _lmtime)) = local_map.get(path) {
                if size == lsize {
                    // Already counted in upload skipped
                    if direction == "download" {
                        skipped += 1;
                    }
                } else if direction == "download" {
                    to_download.push(path);
                }
            } else {
                to_download.push(path);
            }
        }
    }

    // Orphan deletion
    if delete {
        if direction == "upload" || direction == "both" {
            for path in remote_map.keys() {
                if !local_map.contains_key(path) {
                    to_delete_remote.push(path);
                }
            }
        }
        if direction == "download" || direction == "both" {
            for path in local_map.keys() {
                if !remote_map.contains_key(path) {
                    to_delete_local.push(path);
                }
            }
        }
    }

    // --track-renames: detect files that were renamed (same hash, different path)
    let mut renames: Vec<(String, String)> = Vec::new(); // (old_remote, new_local)
    if track_renames && !to_upload.is_empty() && !to_delete_remote.is_empty() {
        if !quiet { eprintln!("Checking for renamed files..."); }
        // Build hash map of files to upload (local side)
        let mut upload_hashes: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for up_path in &to_upload {
            let local_file = std::path::Path::new(local).join(up_path);
            if let Ok(data) = std::fs::read(&local_file) {
                use sha2::Digest;
                let hash = format!("{:x}", sha2::Sha256::digest(&data));
                upload_hashes.entry(hash).or_default().push(up_path.to_string());
            }
        }
        // For each file to delete, check if its hash matches an upload candidate
        let mut matched_uploads: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut matched_deletes: std::collections::HashSet<String> = std::collections::HashSet::new();
        for del_path in &to_delete_remote {
            let remote_full = if remote.ends_with('/') {
                format!("{}{}", remote, del_path)
            } else {
                format!("{}/{}", remote, del_path)
            };
            if let Ok(data) = provider.download_to_bytes(&remote_full).await {
                use sha2::Digest;
                let hash = format!("{:x}", sha2::Sha256::digest(&data));
                if let Some(upload_paths) = upload_hashes.get(&hash) {
                    if let Some(up) = upload_paths.first() {
                        if !matched_uploads.contains(up) {
                            renames.push((del_path.to_string(), up.clone()));
                            matched_uploads.insert(up.clone());
                            matched_deletes.insert(del_path.to_string());
                        }
                    }
                }
            }
        }
        // Remove matched items from upload/delete lists
        if !renames.is_empty() {
            to_upload.retain(|p| !matched_uploads.contains(*p));
            to_delete_remote.retain(|p| !matched_deletes.contains(*p));
            if !quiet {
                eprintln!("  {} rename(s) detected — will rename instead of delete+upload", renames.len());
            }
        }
    }

    if !quiet {
        eprintln!(
            "\nSync plan: {} upload, {} download, {} delete, {} rename, {} skipped",
            to_upload.len(),
            to_download.len(),
            to_delete_remote.len() + to_delete_local.len(),
            renames.len(),
            skipped
        );
    }

    // --max-delete safety check
    if let Some(max_del) = max_delete {
        let delete_count = to_delete_remote.len() + to_delete_local.len();
        let total_files = local_map.len() + remote_map.len();
        let limit = if max_del.ends_with('%') {
            let pct: f64 = max_del.trim_end_matches('%').parse().unwrap_or(100.0);
            ((pct / 100.0) * total_files as f64).ceil() as usize
        } else {
            max_del.parse::<usize>().unwrap_or(usize::MAX)
        };
        if delete_count > limit {
            let msg = format!(
                "Safety abort: {} files would be deleted (limit: {}). Increase --max-delete or remove the flag.",
                delete_count, max_del
            );
            print_error(format, &msg, 4);
            let _ = provider.disconnect().await;
            return 4;
        }
    }

    if dry_run {
        match format {
            OutputFormat::Text => {
                for p in &to_upload {
                    println!("  UPLOAD  {}", p);
                }
                for p in &to_download {
                    println!("  DOWNLOAD  {}", p);
                }
                for p in &to_delete_remote {
                    println!("  DELETE (remote)  {}", p);
                }
                for p in &to_delete_local {
                    println!("  DELETE (local)  {}", p);
                }
                println!("\n(dry run — no changes made)");
            }
            OutputFormat::Json => {
                print_json(&CliSyncResult {
                    status: "dry_run",
                    uploaded: to_upload.len() as u32,
                    downloaded: to_download.len() as u32,
                    deleted: (to_delete_remote.len() + to_delete_local.len()) as u32,
                    skipped,
                    errors: vec![],
                    elapsed_secs: start.elapsed().as_secs_f64(),
                });
            }
        }
        let _ = provider.disconnect().await;
        return 0;
    }

    // Execute sync transfers
    let mut uploaded: u32 = 0;
    let mut downloaded: u32 = 0;
    let mut deleted: u32 = 0;
    let mut errors: Vec<String> = Vec::new();

    let upload_jobs: Vec<(String, String, String, u64)> = to_upload
        .iter()
        .map(|path| {
            let relative = (*path).to_string();
            let local_path = Path::new(local).join(path).to_string_lossy().to_string();
            let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
            let size = local_map.get(*path).map(|(size, _)| *size).unwrap_or(0);
            (relative, local_path, remote_path, size)
        })
        .collect();

    let mut download_jobs: Vec<(String, String, String, u64)> = Vec::new();
    for path in &to_download {
        if validate_relative_path(path).is_none() {
            errors.push(format!("download {}: unsafe path (traversal rejected)", path));
            continue;
        }
        let relative = (*path).to_string();
        let local_path = Path::new(local).join(path).to_string_lossy().to_string();
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
        let size = remote_map.get(*path).map(|(size, _)| *size).unwrap_or(0);
        download_jobs.push((relative, local_path, remote_path, size));
    }

    let total_transfer_files = upload_jobs.len() + download_jobs.len();
    let total_transfer_bytes: u64 = upload_jobs
        .iter()
        .map(|(_, _, _, size)| *size)
        .sum::<u64>()
        + download_jobs.iter().map(|(_, _, _, size)| *size).sum::<u64>();

    if !quiet && total_transfer_files > 0 {
        eprintln!(
            "Executing {} transfer(s) with {} workers",
            total_transfer_files,
            effective_parallel_workers(cli)
        );
    }

    let mut upload_dirs: Vec<String> = upload_jobs
        .iter()
        .filter_map(|(_, _, remote_path, _)| {
            Path::new(remote_path)
                .parent()
                .map(|parent| parent.to_string_lossy().to_string())
        })
        .filter(|dir| !dir.is_empty() && dir != "/")
        .collect();
    upload_dirs.sort_by(|left, right| {
        let left_depth = left.matches('/').count();
        let right_depth = right.matches('/').count();
        left_depth.cmp(&right_depth).then_with(|| left.cmp(right))
    });
    upload_dirs.dedup();
    for dir in &upload_dirs {
        let _ = provider.mkdir(dir).await;
    }

    let aggregate = Arc::new(AtomicU64::new(0));
    let overall_pb = if !quiet && total_transfer_bytes > 0 {
        Some(create_overall_progress_bar(total_transfer_files, total_transfer_bytes))
    } else {
        None
    };

    let upload_results = futures_util::stream::iter(upload_jobs.into_iter().map(
        |(path, local_path, remote_path, _size)| {
            let cancelled = cancelled.clone();
            let aggregate = aggregate.clone();
            let overall_pb = overall_pb.clone();
            async move {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(format!("upload {}: cancelled", path));
                }
                match upload_transfer_task(
                    url,
                    local_path,
                    remote_path,
                    cli,
                    format,
                    Some(aggregate),
                    overall_pb,
                )
                .await
                {
                    Ok(()) => Ok(path),
                    Err(err) => Err(format!("upload {}: {}", path, err)),
                }
            }
        },
    ))
    .buffer_unordered(effective_parallel_workers(cli))
    .collect::<Vec<_>>()
    .await;

    for result in upload_results {
        match result {
            Ok(_) => uploaded += 1,
            Err(err) => errors.push(err),
        }
    }

    let download_results = futures_util::stream::iter(download_jobs.into_iter().map(
        |(path, local_path, remote_path, _size)| {
            let cancelled = cancelled.clone();
            let aggregate = aggregate.clone();
            let overall_pb = overall_pb.clone();
            async move {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(format!("download {}: cancelled", path));
                }
                if let Some(parent) = Path::new(&local_path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match download_transfer_task(
                    url,
                    remote_path,
                    local_path,
                    cli,
                    format,
                    Some(aggregate),
                    overall_pb,
                )
                .await
                {
                    Ok(()) => Ok(path),
                    Err(err) => Err(format!("download {}: {}", path, err)),
                }
            }
        },
    ))
    .buffer_unordered(effective_parallel_workers(cli))
    .collect::<Vec<_>>()
    .await;

    for result in download_results {
        match result {
            Ok(_) => downloaded += 1,
            Err(err) => errors.push(err),
        }
    }

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }

    // Execute renames (--track-renames)
    let mut renamed = 0u32;
    for (old_remote, new_local) in &renames {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let old_path = format!("{}/{}", remote.trim_end_matches('/'), old_remote);
        let new_path = format!("{}/{}", remote.trim_end_matches('/'), new_local);
        if !dry_run {
            match provider.rename(&old_path, &new_path).await {
                Ok(()) => {
                    renamed += 1;
                    if !quiet { eprintln!("  RENAME {} → {}", old_remote, new_local); }
                }
                Err(e) => errors.push(format!("rename {} → {}: {}", old_remote, new_local, e)),
            }
        } else {
            renamed += 1;
        }
    }

    for path in &to_delete_remote {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        if validate_relative_path(path).is_none() {
            errors.push(format!("delete remote {}: unsafe path (traversal rejected)", path));
            continue;
        }
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
        match provider.delete(&remote_path).await {
            Ok(()) => deleted += 1,
            Err(e) => errors.push(format!("delete remote {}: {}", path, e)),
        }
    }

    for path in &to_delete_local {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        if validate_relative_path(path).is_none() {
            errors.push(format!("delete local {}: unsafe path (traversal rejected)", path));
            continue;
        }
        let local_path = format!("{}/{}", local, path);
        match std::fs::remove_file(&local_path) {
            Ok(()) => deleted += 1,
            Err(e) => errors.push(format!("delete local {}: {}", path, e)),
        }
    }

    let elapsed = start.elapsed();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "\nSync complete: {} uploaded, {} downloaded, {} deleted, {} renamed in {:.1}s",
                    uploaded,
                    downloaded,
                    deleted,
                    renamed,
                    elapsed.as_secs_f64()
                );
                for err in &errors {
                    eprintln!("  Error: {}", err);
                }
            }
        }
        OutputFormat::Json => {
            print_json(&CliSyncResult {
                status: if errors.is_empty() { "ok" } else { "partial" },
                uploaded,
                downloaded,
                deleted,
                skipped,
                errors: errors.clone(),
                elapsed_secs: elapsed.as_secs_f64(),
            });
        }
    }

    let _ = provider.disconnect().await;
    if errors.is_empty() { 0 } else { 4 }
}

async fn cmd_tree(
    url: &str,
    path: &str,
    max_depth: usize,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let effective_path = if path == "/" && url_path != "/" {
        &url_path
    } else {
        path
    };

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    if !quiet {
        println!("{}", effective_path);
    }

    #[derive(Serialize)]
    struct TreeNode {
        name: String,
        path: String,
        is_dir: bool,
        size: u64,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        children: Vec<TreeNode>,
    }

    // BFS with depth tracking
    struct QueueItem {
        path: String,
        name: String,
        depth: usize,
        prefix: String,
    }

    let mut file_count: usize = 0;
    let mut dir_count: usize = 0;

    // For JSON output, build tree recursively with entry limit
    async fn build_tree(
        provider: &mut dyn StorageProvider,
        path: &str,
        depth: usize,
        max_depth: usize,
        entry_count: &mut usize,
        visited: &mut std::collections::HashSet<String>,
    ) -> Vec<TreeNode> {
        if depth >= max_depth || *entry_count >= MAX_SCAN_ENTRIES {
            return Vec::new();
        }
        // Symlink loop detection: skip already-visited paths
        if !visited.insert(path.to_string()) {
            return Vec::new();
        }
        let entries = match provider.list(path).await {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut nodes = Vec::new();
        for e in entries {
            if *entry_count >= MAX_SCAN_ENTRIES {
                break;
            }
            *entry_count += 1;
            let children = if e.is_dir {
                Box::pin(build_tree(provider, &e.path, depth + 1, max_depth, entry_count, visited)).await
            } else {
                Vec::new()
            };
            nodes.push(TreeNode {
                name: e.name,
                path: e.path,
                is_dir: e.is_dir,
                size: e.size,
                children,
            });
        }
        nodes.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        nodes
    }

    match format {
        OutputFormat::Json => {
            let mut tree_entry_count: usize = 0;
            let mut tree_visited = std::collections::HashSet::new();
            let root_children = build_tree(&mut *provider, effective_path, 0, max_depth, &mut tree_entry_count, &mut tree_visited).await;
            fn count_nodes(nodes: &[TreeNode]) -> (usize, usize) {
                let mut files = 0;
                let mut dirs = 0;
                for n in nodes {
                    if n.is_dir {
                        dirs += 1;
                    } else {
                        files += 1;
                    }
                    let (f, d) = count_nodes(&n.children);
                    files += f;
                    dirs += d;
                }
                (files, dirs)
            }
            let (f, d) = count_nodes(&root_children);
            file_count = f;
            dir_count = d;

            #[derive(Serialize)]
            struct TreeResult {
                status: &'static str,
                root: String,
                tree: Vec<TreeNode>,
                summary: TreeSummary,
            }
            #[derive(Serialize)]
            struct TreeSummary {
                directories: usize,
                files: usize,
            }
            print_json(&TreeResult {
                status: "ok",
                root: effective_path.to_string(),
                tree: root_children,
                summary: TreeSummary {
                    directories: dir_count,
                    files: file_count,
                },
            });
        }
        OutputFormat::Text => {
            // Iterative DFS with prefix tracking for tree drawing
            let mut stack: Vec<QueueItem> = Vec::new();
            let mut tree_entry_count: usize = 0;
            let mut tree_visited: std::collections::HashSet<String> = std::collections::HashSet::new();

            // Load root entries
            let root_entries = match provider.list(effective_path).await {
                Ok(e) => e,
                Err(e) => {
                    print_error(format, &format!("tree failed: {}", e), provider_error_to_exit_code(&e));
                    let _ = provider.disconnect().await;
                    return provider_error_to_exit_code(&e);
                }
            };

            let mut sorted: Vec<_> = root_entries.into_iter().collect();
            sorted.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            // Push in reverse so first item is processed first
            for (i, e) in sorted.iter().enumerate().rev() {
                let is_last = i == sorted.len() - 1;
                let connector = if is_last { "\u{2514}\u{2500}\u{2500} " } else { "\u{251c}\u{2500}\u{2500} " };
                let child_prefix = if is_last { "    " } else { "\u{2502}   " };
                stack.push(QueueItem {
                    path: e.path.clone(),
                    name: format!(
                        "{}{}{}",
                        connector,
                        sanitize_filename(&e.name),
                        if e.is_dir { "/" } else { "" }
                    ),
                    depth: 1,
                    prefix: child_prefix.to_string(),
                });
                if e.is_dir { dir_count += 1; } else { file_count += 1; }
            }

            while let Some(item) = stack.pop() {
                if tree_entry_count >= MAX_SCAN_ENTRIES {
                    eprintln!("Warning: max entries {} reached, tree output truncated", MAX_SCAN_ENTRIES);
                    break;
                }
                tree_entry_count += 1;
                println!("{}", item.name);

                if item.depth < max_depth {
                    // Check if this is a directory by checking the trailing /
                    if item.name.ends_with('/') && tree_visited.insert(item.path.clone()) {
                        if let Ok(children) = provider.list(&item.path).await {
                            let mut sorted: Vec<_> = children.into_iter().collect();
                            sorted.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                                (true, false) => std::cmp::Ordering::Less,
                                (false, true) => std::cmp::Ordering::Greater,
                                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                            });

                            for (i, e) in sorted.iter().enumerate().rev() {
                                let is_last = i == sorted.len() - 1;
                                let connector = if is_last { "\u{2514}\u{2500}\u{2500} " } else { "\u{251c}\u{2500}\u{2500} " };
                                let child_prefix = if is_last { "    " } else { "\u{2502}   " };
                                stack.push(QueueItem {
                                    path: e.path.clone(),
                                    name: format!(
                                        "{}{}{}{}",
                                        item.prefix,
                                        connector,
                                        sanitize_filename(&e.name),
                                        if e.is_dir { "/" } else { "" }
                                    ),
                                    depth: item.depth + 1,
                                    prefix: format!("{}{}", item.prefix, child_prefix),
                                });
                                if e.is_dir { dir_count += 1; } else { file_count += 1; }
                            }
                        }
                    }
                }
            }

            if !cli.quiet {
                println!(
                    "\n{} directories, {} files",
                    dir_count, file_count
                );
            }
        }
    }

    let _ = provider.disconnect().await;
    0
}

async fn cmd_put_glob(
    url: &str,
    local_pattern: &str,
    remote_base: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let remote_base = remote_base.unwrap_or("/");

    // Split pattern into directory + glob
    let pattern_path = Path::new(local_pattern);
    let (dir, glob_pattern) = if let Some(parent) = pattern_path.parent() {
        let parent_str = parent.to_string_lossy();
        let parent_dir = if parent_str.is_empty() { "." } else { &*parent_str };
        (
            parent_dir.to_string(),
            pattern_path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
        )
    } else {
        (".".to_string(), local_pattern.to_string())
    };

    let matcher = match globset::Glob::new(&glob_pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            print_error(format, &format!("Invalid glob pattern: {}", e), 5);
            return 5;
        }
    };

    // Read local directory and match
    let read_dir = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            print_error(format, &format!("Cannot read directory '{}': {}", dir, e), 2);
            return 2;
        }
    };

    let mut matched: Vec<(String, String, u64)> = Vec::new(); // (local_path, filename, size)
    for entry in read_dir.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if matcher.is_match(&name) {
            matched.push((entry.path().to_string_lossy().to_string(), name, meta.len()));
        }
    }

    if matched.is_empty() {
        if !cli.quiet {
            match format {
                OutputFormat::Text => eprintln!("No files matching '{}'", glob_pattern),
                OutputFormat::Json => print_json(&CliOk {
                    status: "ok",
                    message: format!("No files matching '{}'", glob_pattern),
                }),
            }
        }
        return 0;
    }

    matched.sort_by(|a, b| a.1.cmp(&b.1));

    let start = Instant::now();
    let total = matched.len();
    let total_bytes: u64 = matched.iter().map(|(_, _, size)| *size).sum();
    let aggregate = Arc::new(AtomicU64::new(0));
    let overall_pb = if !cli.quiet && matches!(format, OutputFormat::Text) && total_bytes > 0 {
        Some(create_overall_progress_bar(total, total_bytes))
    } else {
        None
    };

    let results = futures_util::stream::iter(matched.into_iter().map(|(local_path, filename, _size)| {
        let cancelled = cancelled.clone();
        let aggregate = aggregate.clone();
        let overall_pb = overall_pb.clone();
        let remote_path = format!("{}/{}", remote_base.trim_end_matches('/'), filename);
        async move {
            if cancelled.load(Ordering::Relaxed) {
                return Err("Cancelled by user".to_string());
            }
            upload_transfer_task(url, local_path, remote_path, cli, format, Some(aggregate), overall_pb)
                .await
                .map(|_| filename)
        }
    }))
    .buffer_unordered(effective_parallel_workers(cli))
    .collect::<Vec<_>>()
    .await;

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
    }

    let mut uploaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(_) => uploaded += 1,
            Err(err) => errors.push(err),
        }
    }

    let elapsed = start.elapsed();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "\n{}/{} files uploaded in {:.1}s",
                    uploaded, total, elapsed.as_secs_f64()
                );
            }
        }
        OutputFormat::Json => {
            print_json(&CliSyncResult {
                status: if errors.is_empty() { "ok" } else { "partial" },
                uploaded,
                downloaded: 0,
                deleted: 0,
                skipped: 0,
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
            });
        }
    }
    if uploaded == total as u32 { 0 } else { 4 }
}

// ── Head / Tail / Touch / Hashsum / Check ─────────────────────────

async fn cmd_head(
    url: &str,
    path: &str,
    lines: usize,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    match provider.download_to_bytes(path).await {
        Ok(data) => {
            match String::from_utf8(data) {
                Ok(text) => {
                    let result: Vec<&str> = text.lines().take(lines).collect();
                    let output = result.join("\n");
                    if matches!(format, OutputFormat::Json) {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "path": path,
                            "lines": result.len(),
                            "content": output,
                        }));
                    } else {
                        println!("{}", output);
                    }
                    let _ = provider.disconnect().await;
                    0
                }
                Err(_) => {
                    print_error(format, "File is not valid UTF-8 text", 5);
                    let _ = provider.disconnect().await;
                    5
                }
            }
        }
        Err(e) => {
            let code = provider_error_to_exit_code(&e);
            print_error(format, &format!("head failed: {}", e), code);
            let _ = provider.disconnect().await;
            code
        }
    }
}

async fn cmd_tail(
    url: &str,
    path: &str,
    lines: usize,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    match provider.download_to_bytes(path).await {
        Ok(data) => {
            match String::from_utf8(data) {
                Ok(text) => {
                    let all_lines: Vec<&str> = text.lines().collect();
                    let start = all_lines.len().saturating_sub(lines);
                    let result = &all_lines[start..];
                    let output = result.join("\n");
                    if matches!(format, OutputFormat::Json) {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "path": path,
                            "lines": result.len(),
                            "content": output,
                        }));
                    } else {
                        println!("{}", output);
                    }
                    let _ = provider.disconnect().await;
                    0
                }
                Err(_) => {
                    print_error(format, "File is not valid UTF-8 text", 5);
                    let _ = provider.disconnect().await;
                    5
                }
            }
        }
        Err(e) => {
            let code = provider_error_to_exit_code(&e);
            print_error(format, &format!("tail failed: {}", e), code);
            let _ = provider.disconnect().await;
            code
        }
    }
}

async fn cmd_touch(
    url: &str,
    path: &str,
    _timestamp: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    // Check if file exists
    match provider.stat(path).await {
        Ok(_) => {
            // File exists — touch is a no-op for most providers (mtime update not widely supported)
            if matches!(format, OutputFormat::Json) {
                print_json(&serde_json::json!({"status": "ok", "path": path, "action": "exists"}));
            } else {
                eprintln!("File exists: {}", path);
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(_) => {
            // File doesn't exist — create empty file
            let tmp = std::env::temp_dir().join(format!("aeroftp_touch_{}", uuid::Uuid::new_v4()));
            if let Err(e) = std::fs::write(&tmp, b"") {
                print_error(format, &format!("Failed to create temp file: {}", e), 4);
                let _ = provider.disconnect().await;
                return 4;
            }
            let result = provider.upload(tmp.to_str().unwrap_or(""), path, None).await;
            let _ = std::fs::remove_file(&tmp);
            match result {
                Ok(()) => {
                    if matches!(format, OutputFormat::Json) {
                        print_json(&serde_json::json!({"status": "ok", "path": path, "action": "created"}));
                    } else {
                        eprintln!("Created: {}", path);
                    }
                    let _ = provider.disconnect().await;
                    0
                }
                Err(e) => {
                    let code = provider_error_to_exit_code(&e);
                    print_error(format, &format!("touch failed: {}", e), code);
                    let _ = provider.disconnect().await;
                    code
                }
            }
        }
    }
}

fn display_port_for_provider(provider_type: &ProviderType, server_info: Option<&str>) -> u16 {
    if let Some(info) = server_info {
        if let Some((_, port_str)) = info.rsplit_once(':') {
            if !port_str.is_empty() && port_str.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(port) = port_str.parse::<u16>() {
                    return port;
                }
            }
        }
    }

    match provider_type {
        ProviderType::Ftp | ProviderType::Ftps => 21,
        ProviderType::Sftp => 22,
        _ => 443,
    }
}

async fn cmd_hashsum(
    algorithm: HashAlgorithm,
    url: &str,
    path: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    match provider.download_to_bytes(path).await {
        Ok(data) => {
            let hash = match algorithm {
                HashAlgorithm::Md5 => {
                    use md5::Digest;
                    format!("{:x}", md5::Md5::digest(&data))
                }
                HashAlgorithm::Sha1 => {
                    use sha1::Digest;
                    format!("{:x}", sha1::Sha1::digest(&data))
                }
                HashAlgorithm::Sha256 => {
                    use sha2::Digest;
                    format!("{:x}", sha2::Sha256::digest(&data))
                }
                HashAlgorithm::Sha512 => {
                    use sha2::Digest;
                    format!("{:x}", sha2::Sha512::digest(&data))
                }
                HashAlgorithm::Blake3 => {
                    blake3::hash(&data).to_hex().to_string()
                }
            };
            let algo_name = match algorithm {
                HashAlgorithm::Md5 => "md5",
                HashAlgorithm::Sha1 => "sha1",
                HashAlgorithm::Sha256 => "sha256",
                HashAlgorithm::Sha512 => "sha512",
                HashAlgorithm::Blake3 => "blake3",
            };
            if matches!(format, OutputFormat::Json) {
                print_json(&CliHashResult {
                    status: "ok",
                    algorithm: algo_name.to_string(),
                    hash: hash.clone(),
                    path: path.to_string(),
                    size: data.len() as u64,
                });
            } else {
                println!("{}  {}", hash, path);
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            let code = provider_error_to_exit_code(&e);
            print_error(format, &format!("hashsum failed: {}", e), code);
            let _ = provider.disconnect().await;
            code
        }
    }
}

async fn cmd_check(
    url: &str,
    local_path: &str,
    remote_path: &str,
    checksum: bool,
    one_way: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let start = Instant::now();
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Scan local files
    let local_dir = Path::new(local_path);
    if !local_dir.is_dir() {
        print_error(format, &format!("Local path is not a directory: {}", local_path), 5);
        let _ = provider.disconnect().await;
        return 5;
    }
    let mut local_files: HashMap<String, (u64, Option<String>)> = HashMap::new();
    for entry in walkdir::WalkDir::new(local_dir)
        .max_depth(MAX_SCAN_DEPTH)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            if let Ok(rel) = entry.path().strip_prefix(local_dir) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let hash = if checksum {
                    use sha2::Digest;
                    match std::fs::read(entry.path()) {
                        Ok(data) => Some(format!("{:x}", sha2::Sha256::digest(&data))),
                        Err(_) => None,
                    }
                } else {
                    None
                };
                local_files.insert(rel_str, (size, hash));
            }
        }
    }

    // Scan remote files (BFS)
    let mut remote_files: HashMap<String, u64> = HashMap::new();
    let mut dirs_to_scan = vec![remote_path.to_string()];
    let remote_prefix = if remote_path.ends_with('/') {
        remote_path.to_string()
    } else {
        format!("{}/", remote_path)
    };
    while let Some(dir) = dirs_to_scan.pop() {
        match provider.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    let rel = entry
                        .path
                        .strip_prefix(&remote_prefix)
                        .unwrap_or(&entry.path)
                        .to_string();
                    if entry.is_dir {
                        dirs_to_scan.push(entry.path.clone());
                    } else {
                        remote_files.insert(rel, entry.size);
                    }
                }
            }
            Err(e) => {
                if !matches!(format, OutputFormat::Json) {
                    eprintln!("Warning: failed to scan {}: {}", dir, e);
                }
            }
        }
    }

    // Compare
    let mut match_count: u32 = 0;
    let mut differ_count: u32 = 0;
    let mut missing_local: u32 = 0;
    let mut missing_remote: u32 = 0;
    let mut details: Vec<CliCheckEntry> = Vec::new();

    for (rel, (local_size, _local_hash)) in &local_files {
        match remote_files.get(rel) {
            Some(&remote_size) => {
                if *local_size == remote_size {
                    match_count += 1;
                } else {
                    differ_count += 1;
                    details.push(CliCheckEntry {
                        path: rel.clone(),
                        status: "differ".to_string(),
                        local_size: Some(*local_size),
                        remote_size: Some(remote_size),
                    });
                }
            }
            None => {
                missing_remote += 1;
                details.push(CliCheckEntry {
                    path: rel.clone(),
                    status: "missing_remote".to_string(),
                    local_size: Some(*local_size),
                    remote_size: None,
                });
            }
        }
    }

    if !one_way {
        for (rel, &remote_size) in &remote_files {
            if !local_files.contains_key(rel) {
                missing_local += 1;
                details.push(CliCheckEntry {
                    path: rel.clone(),
                    status: "missing_local".to_string(),
                    local_size: None,
                    remote_size: Some(remote_size),
                });
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();

    if matches!(format, OutputFormat::Json) {
        print_json(&CliCheckResult {
            status: if differ_count == 0 && missing_local == 0 && missing_remote == 0 {
                "ok"
            } else {
                "differences_found"
            },
            match_count,
            differ_count,
            missing_local,
            missing_remote,
            elapsed_secs: elapsed,
            details,
        });
    } else {
        eprintln!(
            "\n  Match: {}  Differ: {}  Missing local: {}  Missing remote: {}  ({:.1}s)",
            match_count, differ_count, missing_local, missing_remote, elapsed
        );
        for d in &details {
            let icon = match d.status.as_str() {
                "differ" => "~",
                "missing_local" => "-",
                "missing_remote" => "+",
                _ => "?",
            };
            eprintln!("  {} {}", icon, d.path);
        }
    }

    let _ = provider.disconnect().await;
    if differ_count > 0 || missing_local > 0 || missing_remote > 0 {
        4
    } else {
        0
    }
}

async fn cmd_batch(file: &str, cli: &Cli, format: OutputFormat, cancelled: Arc<AtomicBool>) -> i32 {
    let content = if file == "-" {
        let mut stdin = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut stdin) {
            print_error(format, &format!("Cannot read batch script from stdin: {}", e), 2);
            return 2;
        }
        stdin
    } else {
        match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => {
                print_error(format, &format!("Cannot read batch file '{}': {}", file, e), 2);
                return 2;
            }
        }
    };

    // Limit batch file size to 1 MB
    if content.len() > 1_048_576 {
        print_error(format, "Batch file exceeds 1 MB limit", 5);
        return 5;
    }

    let mut variables: HashMap<String, String> = HashMap::new();
    let mut current_url: Option<String> = None;
    let mut exit_code = 0;
    let mut on_error_continue = false;
    let mut total_commands: u32 = 0;
    let mut failed_commands: u32 = 0;

    /// Check exit code and handle ON_ERROR policy.
    /// Returns Some(exit_code) if batch should abort, None to continue.
    fn check_exit(
        code: i32,
        line_num: usize,
        cmd: &str,
        on_error_continue: bool,
        failed_commands: &mut u32,
    ) -> Option<i32> {
        if code != 0 {
            if on_error_continue {
                eprintln!(
                    "Warning: line {} ({}) failed with exit code {} (continuing)",
                    line_num + 1,
                    cmd,
                    code
                );
                *failed_commands += 1;
                None
            } else {
                eprintln!(
                    "Batch failed at line {} ({}): exit code {}",
                    line_num + 1,
                    cmd,
                    code
                );
                Some(code)
            }
        } else {
            None
        }
    }

    /// Require an active connection URL, or return error.
    fn require_url(current_url: &Option<String>, line_num: usize) -> Result<String, i32> {
        match current_url {
            Some(u) => Ok(u.clone()),
            None => {
                eprintln!(
                    "Line {}: No active connection. Use CONNECT first.",
                    line_num + 1
                );
                Err(5)
            }
        }
    }

    for (line_num, raw_line) in content.lines().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            eprintln!("Batch interrupted at line {}", line_num + 1);
            return 4;
        }

        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Single-pass variable substitution (prevents recursive expansion)
        // Uses char indices for proper UTF-8 handling
        let expanded = {
            let mut result = String::with_capacity(line.len());
            let chars: Vec<(usize, char)> = line.char_indices().collect();
            let mut ci = 0;
            while ci < chars.len() {
                let (byte_idx, ch) = chars[ci];
                if ch == '$' && ci + 1 < chars.len() {
                    let (_, next_ch) = chars[ci + 1];
                    if next_ch == '$' {
                        // $$ escape → literal $
                        result.push('$');
                        ci += 2;
                        continue;
                    } else if next_ch == '{' {
                        // ${VAR} syntax
                        let start_byte = chars[ci + 2..].first().map(|(b, _)| *b).unwrap_or(line.len());
                        if let Some(close_pos) = line[start_byte..].find('}') {
                            let key = &line[start_byte..start_byte + close_pos];
                            if let Some(val) = variables.get(key) {
                                result.push_str(val);
                            } else {
                                let end_byte = start_byte + close_pos + 1;
                                result.push_str(&line[byte_idx..end_byte]);
                            }
                            // Skip past the closing }
                            let end_byte = start_byte + close_pos + 1;
                            ci = chars.iter().position(|(b, _)| *b >= end_byte).unwrap_or(chars.len());
                            continue;
                        }
                    } else if next_ch.is_ascii_alphabetic() || next_ch == '_' {
                        // $VAR syntax
                        let start = ci + 1;
                        let mut end = start;
                        while end < chars.len() && (chars[end].1.is_ascii_alphanumeric() || chars[end].1 == '_') {
                            end += 1;
                        }
                        let key_start = chars[start].0;
                        let key_end = if end < chars.len() { chars[end].0 } else { line.len() };
                        let key = &line[key_start..key_end];
                        if let Some(val) = variables.get(key) {
                            result.push_str(val);
                        } else {
                            result.push_str(&line[byte_idx..key_end]);
                        }
                        ci = end;
                        continue;
                    }
                }
                result.push(ch);
                ci += 1;
            }
            result
        };

        // Shell-like splitting that respects double quotes for paths with spaces
        let parts_owned: Vec<String> = {
            let mut parts = Vec::new();
            let mut current = String::new();
            let mut in_quotes = false;
            for ch in expanded.chars() {
                match ch {
                    '"' => in_quotes = !in_quotes,
                    ' ' | '\t' if !in_quotes => {
                        if !current.is_empty() {
                            parts.push(std::mem::take(&mut current));
                        }
                    }
                    _ => current.push(ch),
                }
            }
            if in_quotes {
                eprintln!("Warning: line {}: unmatched quote", line_num + 1);
            }
            if !current.is_empty() {
                parts.push(current);
            }
            parts
        };
        if parts_owned.is_empty() {
            continue;
        }
        let parts: Vec<&str> = parts_owned.iter().map(|s| s.as_str()).collect();

        let cmd = parts[0].to_uppercase();
        total_commands += 1;

        match cmd.as_str() {
            "SET" => {
                if expanded.len() > 3 {
                    let rest = expanded[3..].trim();
                    if let Some(eq_pos) = rest.find('=') {
                        let key = rest[..eq_pos].trim().to_string();
                        let value = rest[eq_pos + 1..].trim().to_string();
                        // Limit variable value size to 64 KB
                        if value.len() > 65_536 {
                            eprintln!("Line {}: variable value too large (max 64 KB)", line_num + 1);
                            return 5;
                        }
                        // Validate variable name: [A-Za-z_][A-Za-z0-9_]*
                        if !key.is_empty()
                            && key.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        {
                            if variables.len() >= 256 && !variables.contains_key(&key) {
                                eprintln!("Line {}: too many variables (max 256)", line_num + 1);
                                return 5;
                            }
                            variables.insert(key, value);
                        } else {
                            eprintln!("Line {}: invalid variable name '{}' (must match [A-Za-z_][A-Za-z0-9_]*)", line_num + 1, key);
                            return 5;
                        }
                    } else {
                        eprintln!("Line {}: SET requires KEY=VALUE syntax", line_num + 1);
                        return 5;
                    }
                } else {
                    eprintln!("Line {}: SET requires KEY=VALUE syntax", line_num + 1);
                    return 5;
                }
            }
            "ECHO" => {
                // ECHO <message> — print to stderr for logging
                let msg = if expanded.len() > 4 {
                    expanded[4..].trim()
                } else {
                    ""
                };
                eprintln!("{}", msg);
            }
            "ON_ERROR" => {
                // ON_ERROR CONTINUE | ON_ERROR FAIL
                if parts.len() >= 2 {
                    match parts[1].to_uppercase().as_str() {
                        "CONTINUE" => on_error_continue = true,
                        "FAIL" => on_error_continue = false,
                        other => {
                            eprintln!(
                                "Line {}: ON_ERROR expects CONTINUE or FAIL, got '{}'",
                                line_num + 1,
                                other
                            );
                            return 5;
                        }
                    }
                }
            }
            "CONNECT" => {
                if parts.len() < 2 {
                    eprintln!("Line {}: CONNECT requires a URL", line_num + 1);
                    return 5;
                }
                // Clear previous URL before attempting new connection
                // Prevents stale URL reuse if CONNECT fails with ON_ERROR CONTINUE
                current_url = None;
                exit_code = cmd_connect(parts[1], cli, format).await;
                if exit_code == 0 {
                    current_url = Some(parts[1].to_string());
                } else if let Some(code) = check_exit(exit_code, line_num, "CONNECT", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "DISCONNECT" => {
                current_url = None;
            }
            "GET" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: GET requires a remote path", line_num + 1);
                    return 5;
                }
                let local = if parts.len() > 2 { Some(parts[2]) } else { None };
                exit_code = cmd_get(&url, parts[1], local, false, 1, cli, format, cancelled.clone()).await;
                if let Some(code) = check_exit(exit_code, line_num, "GET", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "PUT" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: PUT requires a local path", line_num + 1);
                    return 5;
                }
                let remote = if parts.len() > 2 { Some(parts[2]) } else { None };
                exit_code = cmd_put(&url, parts[1], remote, false, cli, format, cancelled.clone()).await;
                if let Some(code) = check_exit(exit_code, line_num, "PUT", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "RM" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: RM requires a path", line_num + 1);
                    return 5;
                }
                let recursive = parts.contains(&"-r") || parts.contains(&"-rf");
                exit_code = cmd_rm(&url, parts[1], recursive, true, cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "RM", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "MV" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 3 {
                    eprintln!("Line {}: MV requires <from> <to>", line_num + 1);
                    return 5;
                }
                exit_code = cmd_mv(&url, parts[1], parts[2], cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "MV", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "LS" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                let path = if parts.len() > 1 { parts[1] } else { "/" };
                let long = parts.contains(&"-l");
                exit_code = cmd_ls(&url, path, long, "name", false, true, cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "LS", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "CAT" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: CAT requires a path", line_num + 1);
                    return 5;
                }
                exit_code = cmd_cat(&url, parts[1], cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "CAT", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "STAT" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: STAT requires a path", line_num + 1);
                    return 5;
                }
                exit_code = cmd_stat(&url, parts[1], cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "STAT", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "FIND" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 3 {
                    eprintln!("Line {}: FIND requires <path> <pattern>", line_num + 1);
                    return 5;
                }
                exit_code = cmd_find(&url, parts[1], parts[2], cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "FIND", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "DF" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                exit_code = cmd_df(&url, cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "DF", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "MKDIR" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: MKDIR requires a path", line_num + 1);
                    return 5;
                }
                exit_code = cmd_mkdir(&url, parts[1], cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "MKDIR", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "TREE" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                let path = if parts.len() > 1 { parts[1] } else { "/" };
                exit_code = cmd_tree(&url, path, 3, cli, format).await;
                if let Some(code) = check_exit(exit_code, line_num, "TREE", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            "SYNC" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                if parts.len() < 3 {
                    eprintln!("Line {}: SYNC requires <local> <remote>", line_num + 1);
                    return 5;
                }
                exit_code = cmd_sync(
                    &url,
                    parts[1],
                    parts[2],
                    "both",
                    false,
                    false,
                    &[],
                    false,
                    None,
                    None,
                    "",
                    cli,
                    format,
                    cancelled.clone(),
                )
                .await;
                if let Some(code) = check_exit(exit_code, line_num, "SYNC", on_error_continue, &mut failed_commands) {
                    return code;
                }
            }
            _ => {
                print_error(
                    format,
                    &format!("Line {}: Unknown command '{}'. Supported: SET, ECHO, ON_ERROR, CONNECT, DISCONNECT, GET, PUT, RM, MV, LS, CAT, STAT, FIND, DF, MKDIR, TREE, SYNC", line_num + 1, cmd),
                    5,
                );
                if !on_error_continue {
                    return 5;
                }
                failed_commands += 1;
            }
        }
    }

    if failed_commands > 0 {
        eprintln!(
            "\nBatch completed: {}/{} commands succeeded, {} failed",
            total_commands - failed_commands,
            total_commands,
            failed_commands
        );
        4
    } else {
        exit_code
    }
}

// ── Agent Command ──────────────────────────────────────────────────

/// Detect AI provider from environment variables
fn detect_ai_provider() -> Option<(String, String, String)> {
    // Returns (provider_name, api_key, base_url)
    let providers = [
        ("anthropic", "ANTHROPIC_API_KEY", "https://api.anthropic.com"),
        ("openai", "OPENAI_API_KEY", "https://api.openai.com/v1"),
        ("gemini", "GEMINI_API_KEY", "https://generativelanguage.googleapis.com"),
        ("xai", "XAI_API_KEY", "https://api.x.ai/v1"),
        ("groq", "GROQ_API_KEY", "https://api.groq.com/openai/v1"),
        ("mistral", "MISTRAL_API_KEY", "https://api.mistral.ai/v1"),
        ("perplexity", "PERPLEXITY_API_KEY", "https://api.perplexity.ai"),
        ("deepseek", "DEEPSEEK_API_KEY", "https://api.deepseek.com"),
        ("together", "TOGETHER_API_KEY", "https://api.together.xyz/v1"),
        ("fireworks", "FIREWORKS_API_KEY", "https://api.fireworks.ai/inference/v1"),
        ("cerebras", "CEREBRAS_API_KEY", "https://api.cerebras.ai/v1"),
        ("sambanova", "SAMBANOVA_API_KEY", "https://api.sambanova.ai/v1"),
    ];
    for (name, env_key, base_url) in &providers {
        if let Ok(key) = std::env::var(env_key) {
            if !key.is_empty() {
                return Some((name.to_string(), key, base_url.to_string()));
            }
        }
    }
    // Check Ollama (no API key needed)
    if std::env::var("OLLAMA_HOST").is_ok() || std::path::Path::new("/usr/local/bin/ollama").exists() {
        return Some(("ollama".to_string(), String::new(), "http://localhost:11434".to_string()));
    }
    None
}

/// Detect AI provider from the encrypted vault (desktop app configuration).
/// Falls back here when no environment variable is set.
fn detect_ai_provider_from_vault(cli: &Cli) -> Option<(String, String, String)> {
    let store = open_vault(cli).ok()?;
    // Priority order: anthropic first (best tool use), then openai, gemini, etc.
    let providers = [
        ("anthropic", "https://api.anthropic.com"),
        ("openai", "https://api.openai.com/v1"),
        ("gemini", "https://generativelanguage.googleapis.com"),
        ("xai", "https://api.x.ai/v1"),
        ("openrouter", "https://openrouter.ai/api/v1"),
        ("deepseek", "https://api.deepseek.com"),
        ("mistral", "https://api.mistral.ai/v1"),
        ("groq", "https://api.groq.com/openai/v1"),
        ("perplexity", "https://api.perplexity.ai"),
        ("cohere", "https://api.cohere.com/compatibility"),
        ("together", "https://api.together.xyz/v1"),
        ("kimi", "https://api.moonshot.cn/v1"),
        ("qwen", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
    ];
    for (name, base_url) in &providers {
        let vault_key = format!("ai_apikey_{}", name);
        if let Ok(key) = store.get(&vault_key) {
            if !key.is_empty() {
                eprintln!("Using AI provider '{}' from AeroFTP vault.", name);
                return Some((name.to_string(), key, base_url.to_string()));
            }
        }
    }
    None
}

/// Get default model for a provider
fn default_model(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-sonnet-4-20250514",
        "openai" => "gpt-4o",
        "gemini" => "gemini-2.0-flash",
        "xai" => "grok-3",
        "ollama" => "llama3.1",
        "groq" => "llama-3.3-70b-versatile",
        "mistral" => "mistral-large-latest",
        "perplexity" => "sonar-pro",
        "deepseek" => "deepseek-chat",
        "together" => "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        "fireworks" => "accounts/fireworks/models/llama-v3p1-70b-instruct",
        "cerebras" => "llama-3.3-70b",
        "sambanova" => "Meta-Llama-3.1-70B-Instruct",
        _ => "gpt-4o",
    }
}

/// Map provider name to AIProviderType
fn provider_type_from_name(name: &str) -> ftp_client_gui_lib::ai::AIProviderType {
    use ftp_client_gui_lib::ai::AIProviderType;
    match name {
        "anthropic" => AIProviderType::Anthropic,
        "openai" => AIProviderType::OpenAI,
        "gemini" | "google" => AIProviderType::Google,
        "xai" => AIProviderType::Xai,
        "ollama" => AIProviderType::Ollama,
        "groq" => AIProviderType::Groq,
        "mistral" => AIProviderType::Mistral,
        "perplexity" => AIProviderType::Perplexity,
        "deepseek" => AIProviderType::DeepSeek,
        "together" => AIProviderType::Together,
        "fireworks" => AIProviderType::Fireworks,
        "cerebras" => AIProviderType::Cerebras,
        "sambanova" => AIProviderType::SambaNova,
        "cohere" => AIProviderType::Cohere,
        "kimi" | "moonshot" => AIProviderType::Kimi,
        "qwen" => AIProviderType::Qwen,
        "ai21" => AIProviderType::Ai21,
        "openrouter" => AIProviderType::OpenRouter,
        _ => AIProviderType::Custom,
    }
}

/// Build system prompt with context
fn build_agent_system_prompt(custom_system: &Option<String>) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let base = if let Some(ref s) = custom_system {
        if let Some(file) = s.strip_prefix('@') {
            std::fs::read_to_string(file).unwrap_or_else(|_| s.clone())
        } else {
            s.clone()
        }
    } else {
        "You are AeroAgent, an AI-powered file management assistant in AeroFTP CLI.\n\
         You have access to tools for local file management, shell execution, archive \
         handling, code search, and more. Use them to perform actions directly.\n\n\
         Rules:\n\
         1. Use tools to perform actions — don't just describe what to do.\n\
         2. Be concise and direct. Explain briefly what you did after executing tools.\n\
         3. For destructive operations (delete, overwrite), confirm with the user first.\n\
         4. Resolve relative paths against the working directory.".to_string()
    };

    format!("{}\n\n## Current Context\n- Working directory: {}\n- Platform: {}\n- Time: {}",
        base, cwd, std::env::consts::OS,
        chrono::Local::now().format("%Y-%m-%d %H:%M"))
}

/// Parse auto-approve level
fn parse_approve_level(s: &str) -> u8 {
    match s.to_lowercase().as_str() {
        "all" => 3,
        "high" => 3,
        "medium" => 2,
        "safe" | "low" => 1,
        "none" => 0,
        _ => 1,
    }
}

/// Get tool danger level (0=safe, 1=medium, 2=high)
fn tool_danger_level(tool: &str) -> u8 {
    match tool {
        // Safe — read-only
        "local_list" | "local_read" | "remote_list" | "remote_read" | "remote_info"
        | "remote_search" | "local_search" | "local_grep" | "local_head" | "local_tail"
        | "local_stat_batch" | "local_diff" | "local_tree" | "local_file_info"
        | "local_disk_usage" | "local_find_duplicates" | "clipboard_read" | "app_info"
        | "server_list_saved" | "rag_search" | "rag_index" | "preview_edit"
        | "generate_transfer_plan"
        | "vault_peek" | "hash_file" => 0,
        // Medium — local writes
        "local_write" | "local_mkdir" | "local_edit" | "local_rename" | "local_copy_files"
        | "local_move_files" | "local_batch_rename" | "remote_upload" | "remote_mkdir"
        | "remote_rename" | "remote_edit" | "upload_files" | "download_files"
        | "remote_download" | "archive_compress" | "archive_decompress" | "clipboard_write"
        | "agent_memory_write" | "sync_preview" | "set_theme" => 1,
        // High — destructive or remote writes
        "local_delete" | "local_trash" | "remote_delete" | "shell_execute"
        | "server_exec" | "sync_control" => 2,
        _ => 2, // Unknown tools default to high
    }
}

/// Build native tool definitions for the AI API (JSON Schema format).
/// These are sent as `tools` in the AIRequest so the model can generate tool_calls.
fn cli_tool_definitions() -> Vec<ftp_client_gui_lib::ai::AIToolDefinition> {
    use ftp_client_gui_lib::ai::AIToolDefinition;
    use serde_json::json;

    // Helper to build a tool definition with JSON Schema parameters
    macro_rules! tool {
        ($name:expr, $desc:expr, { $($pname:expr => ($ptype:expr, $pdesc:expr, $req:expr)),* $(,)? }) => {
            {
                #[allow(unused_mut)]
                let mut props = serde_json::Map::new();
                #[allow(unused_mut)]
                let mut required: Vec<serde_json::Value> = Vec::new();
                $(
                    let mut prop = serde_json::Map::new();
                    if $ptype == "array" {
                        prop.insert("type".into(), json!("array"));
                        prop.insert("items".into(), json!({"type": "string"}));
                    } else {
                        prop.insert("type".into(), json!($ptype));
                    }
                    prop.insert("description".into(), json!($pdesc));
                    props.insert($pname.into(), serde_json::Value::Object(prop));
                    if $req {
                        required.push(json!($pname));
                    }
                )*
                AIToolDefinition {
                    name: $name.to_string(),
                    description: $desc.to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": serde_json::Value::Object(props),
                        "required": required,
                    }),
                }
            }
        };
    }

    vec![
        // === Safe (read-only) ===
        tool!("local_list", "List files and folders in a local directory", {
            "path" => ("string", "Local directory path", true)
        }),
        tool!("local_read", "Read a local text file (max 5KB)", {
            "path" => ("string", "Local file path", true)
        }),
        tool!("local_search", "Search for files by name pattern in a local directory", {
            "path" => ("string", "Directory to search", true),
            "pattern" => ("string", "Search pattern (e.g. \"*.txt\")", true)
        }),
        tool!("local_file_info", "Get detailed file properties: size, permissions, timestamps", {
            "path" => ("string", "File or directory path", true)
        }),
        tool!("local_disk_usage", "Calculate total size of a directory (recursive)", {
            "path" => ("string", "Directory path", true)
        }),
        tool!("local_find_duplicates", "Find duplicate files using MD5 hash comparison", {
            "path" => ("string", "Directory to scan", true),
            "min_size" => ("number", "Minimum file size in bytes (default: 1024)", false)
        }),
        tool!("local_diff", "Compare two local files and show unified diff", {
            "path_a" => ("string", "First file path", true),
            "path_b" => ("string", "Second file path", true),
            "context_lines" => ("number", "Lines of context (default: 3)", false)
        }),
        tool!("local_grep", "Search file contents using regex pattern recursively", {
            "path" => ("string", "Directory to search in", true),
            "pattern" => ("string", "Regex pattern to search for", true),
            "glob" => ("string", "File filter pattern (e.g. \"*.ts\")", false),
            "max_results" => ("number", "Maximum matches (default: 50)", false),
            "context_lines" => ("number", "Lines of context (default: 2)", false),
            "case_sensitive" => ("boolean", "Case-sensitive (default: true)", false)
        }),
        tool!("local_head", "Read the first N lines of a file (default: 20)", {
            "path" => ("string", "File path", true),
            "lines" => ("number", "Number of lines (default: 20, max: 500)", false)
        }),
        tool!("local_tail", "Read the last N lines of a file (default: 20)", {
            "path" => ("string", "File path", true),
            "lines" => ("number", "Number of lines (default: 20, max: 500)", false)
        }),
        tool!("local_stat_batch", "Get file metadata for multiple paths at once", {
            "paths" => ("array", "Array of file/directory paths to stat (max 100)", true)
        }),
        tool!("local_tree", "Display a recursive directory tree with file sizes", {
            "path" => ("string", "Root directory path", true),
            "max_depth" => ("number", "Maximum depth (default: 3, max: 10)", false),
            "show_hidden" => ("boolean", "Show hidden files (default: false)", false),
            "glob" => ("string", "File filter pattern", false)
        }),
        tool!("hash_file", "Compute cryptographic hash (MD5, SHA-1, SHA-256, SHA-512, BLAKE3)", {
            "path" => ("string", "File path to hash", true),
            "algorithm" => ("string", "Hash algorithm (default: sha256)", false)
        }),
        tool!("app_info", "Get CLI application state: version, platform, working directory", {}),
        // === Medium (local writes) ===
        tool!("local_write", "Write content to a local text file", {
            "path" => ("string", "Local file path", true),
            "content" => ("string", "File content", true)
        }),
        tool!("local_mkdir", "Create a local directory (including parents)", {
            "path" => ("string", "Directory path to create", true)
        }),
        tool!("local_edit", "Find and replace text in a local file", {
            "path" => ("string", "Local file path", true),
            "find" => ("string", "Exact text to find", true),
            "replace" => ("string", "Replacement text", true),
            "replace_all" => ("boolean", "Replace all occurrences (default: true)", false)
        }),
        tool!("local_rename", "Rename/move a local file or folder", {
            "from" => ("string", "Current path", true),
            "to" => ("string", "New path", true)
        }),
        tool!("local_move_files", "Move multiple local files into a destination directory", {
            "paths" => ("array", "Array of source file paths", true),
            "destination" => ("string", "Destination directory path", true)
        }),
        tool!("local_copy_files", "Copy multiple local files into a destination directory", {
            "paths" => ("array", "Array of source file paths", true),
            "destination" => ("string", "Destination directory path", true)
        }),
        tool!("local_batch_rename", "Rename multiple files using patterns", {
            "paths" => ("array", "Array of file paths to rename", true),
            "mode" => ("string", "Rename mode: find_replace, add_prefix, add_suffix, sequential", true),
            "find" => ("string", "Text to find (find_replace only)", false),
            "replace" => ("string", "Replacement text (find_replace only)", false),
            "prefix" => ("string", "Prefix to add", false),
            "suffix" => ("string", "Suffix to add before extension", false),
            "base_name" => ("string", "Base name for sequential (default: file)", false),
            "start_number" => ("number", "Start number for sequential (default: 1)", false)
        }),
        tool!("archive_compress", "Compress files into an archive (ZIP, 7z, TAR, etc.)", {
            "paths" => ("array", "Array of file/folder paths to compress", true),
            "output_path" => ("string", "Output archive file path", true),
            "format" => ("string", "Archive format: zip, 7z, tar, tar.gz, tar.bz2, tar.xz", false),
            "password" => ("string", "Encryption password", false),
            "compression_level" => ("number", "Compression level 0-9 (default: 6)", false)
        }),
        tool!("archive_decompress", "Extract an archive", {
            "archive_path" => ("string", "Path to the archive file", true),
            "output_dir" => ("string", "Output directory", true),
            "password" => ("string", "Decryption password", false)
        }),
        tool!("clipboard_write", "Write text to the system clipboard", {
            "content" => ("string", "Text to copy", true)
        }),
        tool!("agent_memory_write", "Save a note to persistent project memory", {
            "entry" => ("string", "Content to remember", true),
            "category" => ("string", "Category: convention, preference, issue, pattern", false)
        }),
        // === High (destructive) ===
        tool!("local_delete", "Delete a local file or directory", {
            "path" => ("string", "Path to delete", true)
        }),
        tool!("local_trash", "Move files to system trash (safe alternative to delete)", {
            "paths" => ("array", "Array of file paths to trash", true)
        }),
        tool!("shell_execute", "Execute a shell command and capture output", {
            "command" => ("string", "Shell command to execute", true),
            "working_dir" => ("string", "Working directory (default: cwd)", false),
            "timeout_secs" => ("number", "Timeout in seconds (default: 30, max: 120)", false)
        }),
        // === Server operations (vault-backed) ===
        tool!("server_list_saved", "List all saved server profiles from the encrypted vault. Returns names, protocols, hosts. Passwords are never exposed. Use this to discover which servers are available before using server_exec.", {}),
        tool!("remote_list", "List files on a saved remote server profile.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "path" => ("string", "Remote path (default: /)", false)
        }),
        tool!("remote_read", "Read a remote text file from a saved server profile (truncated for safety).", {
            "server" => ("string", "Server name from server_list_saved", true),
            "path" => ("string", "Remote file path", true)
        }),
        tool!("remote_info", "Get metadata for a remote file or directory from a saved server profile.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "path" => ("string", "Remote path", true)
        }),
        tool!("remote_search", "Search a saved remote server profile by path and pattern.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "path" => ("string", "Base remote path", false),
            "pattern" => ("string", "Search pattern (default: *)", false)
        }),
        tool!("remote_upload", "Upload content or a local file to a saved remote server profile.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "remote_path" => ("string", "Destination remote path", true),
            "local_path" => ("string", "Optional local file path to upload", false),
            "content" => ("string", "Optional inline text content to upload", false)
        }),
        tool!("remote_download", "Download a file from a saved remote server profile to a local path.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "remote_path" => ("string", "Source remote path", true),
            "local_path" => ("string", "Destination local path", true)
        }),
        tool!("remote_mkdir", "Create a directory on a saved remote server profile.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "path" => ("string", "Remote directory path", true)
        }),
        tool!("remote_delete", "Delete a file or directory on a saved remote server profile.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "path" => ("string", "Remote path to delete", true)
        }),
        tool!("remote_rename", "Rename or move a path on a saved remote server profile.", {
            "server" => ("string", "Server name from server_list_saved", true),
            "from" => ("string", "Current remote path", true),
            "to" => ("string", "New remote path", true)
        }),
        tool!("server_exec", "Execute a file operation on a saved server. Creates a temporary connection using credentials from the vault, executes the operation, then disconnects. Passwords are resolved internally and never exposed. Operations: ls (list files), cat (read file content), stat (file metadata), find (search by pattern), df (disk usage/quota).", {
            "server" => ("string", "Server name from server_list_saved (exact or partial match)", true),
            "operation" => ("string", "Operation: ls, cat, stat, find, df", true),
            "path" => ("string", "Remote path for the operation (default: /)", false),
            "pattern" => ("string", "Search pattern (required for find)", false)
        }),
    ]
}

/// Execute a CLI tool locally (no Tauri dependency).
/// Returns JSON result or error string.
async fn execute_cli_tool(tool_name: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    use serde_json::json;

    // Helper to extract string argument
    let get_str = |key: &str| -> Result<String, String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("Missing required argument: {}", key))
    };

    let get_str_opt = |key: &str| -> Option<String> {
        args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };

    // Validate local path — deny sensitive paths (mirrors ai_tools.rs::validate_path)
    let validate_path = |path: &str, param: &str| -> Result<(), String> {
        if path.len() > 4096 {
            return Err(format!("{}: path exceeds 4096 characters", param));
        }
        if path.contains('\0') {
            return Err(format!("{}: path contains null bytes", param));
        }
        let normalized = path.replace('\\', "/");
        for component in normalized.split('/') {
            if component == ".." {
                return Err(format!("{}: path traversal ('..') not allowed", param));
            }
        }
        let resolved = std::fs::canonicalize(path).or_else(|_| {
            std::path::Path::new(path)
                .parent()
                .map(std::fs::canonicalize)
                .unwrap_or(Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no parent")))
        });
        if let Ok(canonical) = resolved {
            let s = canonical.to_string_lossy();
            if CLI_DENIED_SYSTEM_PREFIXES.iter().any(|d| path_matches_prefix(&s, d)) {
                return Err(format!("{}: access to system path denied: {}", param, s));
            }
            if let Ok(home) = std::env::var("HOME") {
                for sensitive in CLI_DENIED_HOME_RELATIVE_PREFIXES {
                    if path_matches_prefix(&s, &format!("{}/{}", home, sensitive)) {
                        return Err(format!("{}: access to sensitive path denied: {}", param, s));
                    }
                }
            }
            if path_matches_prefix(&s, "/run/secrets") {
                return Err(format!("{}: access to system path denied: {}", param, s));
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            for sensitive in CLI_DENIED_HOME_RELATIVE_PREFIXES {
                let full = format!("{}/{}", home, sensitive);
                if path_matches_prefix(path, &full) {
                    return Err(format!("{}: access to sensitive path denied: {}", param, path));
                }
            }
        }

        Ok(())
    };

    // Resolve relative path against cwd
    let resolve_path = |path: &str| -> String {
        if std::path::Path::new(path).is_absolute() {
            path.to_string()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path).to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string())
        }
    };

    match tool_name {
        "local_list" => {
            let path = resolve_path(&get_str("path")?);
            validate_path(&path, "path")?;
            let entries: Vec<serde_json::Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
                .filter_map(|e| e.ok())
                .take(100)
                .map(|e| {
                    let meta = e.metadata().ok();
                    json!({
                        "name": e.file_name().to_string_lossy(),
                        "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    })
                })
                .collect();
            Ok(json!({ "entries": entries }))
        }

        "local_read" => {
            let path = resolve_path(&get_str("path")?);
            validate_path(&path, "path")?;
            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Failed to stat file: {}", e))?;
            if meta.len() > 10_485_760 {
                return Err(format!("File too large: {:.1} MB (max 10 MB)", meta.len() as f64 / 1_048_576.0));
            }
            let max_bytes: usize = 5120;
            let file_size = meta.len() as usize;
            let read_size = std::cmp::min(file_size, max_bytes);
            let mut file = std::fs::File::open(&path)
                .map_err(|e| format!("Failed to open file: {}", e))?;
            let mut buf = vec![0u8; read_size];
            use std::io::Read as _;
            file.read_exact(&mut buf)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let truncated = file_size > max_bytes;
            let content = String::from_utf8_lossy(&buf).to_string();
            Ok(json!({ "content": content, "size": file_size, "truncated": truncated }))
        }

        "local_search" => {
            let path = resolve_path(&get_str("path")?);
            let pattern = get_str("pattern")?;
            validate_path(&path, "path")?;
            let pattern_lower = pattern.to_lowercase();
            let matcher: Box<dyn Fn(&str) -> bool> = if let Some(suffix) = pattern_lower.strip_prefix('*') {
                let s = suffix.to_string();
                Box::new(move |name: &str| name.ends_with(&s))
            } else if let Some(prefix) = pattern_lower.strip_suffix('*') {
                let p = prefix.to_string();
                Box::new(move |name: &str| name.starts_with(&p))
            } else {
                let pat = pattern_lower.clone();
                Box::new(move |name: &str| name.contains(&pat))
            };
            let results: Vec<serde_json::Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| matcher(&e.file_name().to_string_lossy().to_lowercase()))
                .take(100)
                .map(|e| {
                    let meta = e.metadata().ok();
                    json!({
                        "name": e.file_name().to_string_lossy(),
                        "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    })
                })
                .collect();
            let total = results.len();
            Ok(json!({ "results": results, "total": total }))
        }

        "local_write" => {
            let path = resolve_path(&get_str("path")?);
            let content = get_str("content")?;
            validate_path(&path, "path")?;
            std::fs::write(&path, &content)
                .map_err(|e| format!("Failed to write file: {}", e))?;
            Ok(json!({ "success": true, "message": format!("Written {} bytes to {}", content.len(), path) }))
        }

        "local_mkdir" => {
            let path = resolve_path(&get_str("path")?);
            validate_path(&path, "path")?;
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
            Ok(json!({ "success": true, "message": format!("Created directory {}", path) }))
        }

        "local_delete" => {
            let path = resolve_path(&get_str("path")?);
            validate_path(&path, "path")?;
            let home_dir = std::env::var("HOME").unwrap_or_default();
            let normalized = path.trim_end_matches('/').trim_end_matches('\\');
            if normalized.is_empty() || normalized == "/" || normalized == "~" || normalized == "." || normalized == ".." || normalized == home_dir {
                return Err(format!("Refusing to delete dangerous path: {}", path));
            }
            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Path not found: {}", e))?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("Failed to delete directory: {}", e))?;
            } else {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete file: {}", e))?;
            }
            Ok(json!({ "success": true, "message": format!("Deleted {}", path) }))
        }

        "local_rename" => {
            let from = resolve_path(&get_str("from")?);
            let to = resolve_path(&get_str("to")?);
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;
            std::fs::rename(&from, &to)
                .map_err(|e| format!("Failed to rename: {}", e))?;
            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "local_edit" => {
            let path = resolve_path(&get_str("path")?);
            let find = get_str("find")?;
            let replace = get_str("replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&path, "path")?;
            if find.is_empty() {
                return Err("'find' parameter cannot be empty".to_string());
            }
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let new_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };
            if content == new_content {
                return Ok(json!({ "success": false, "message": "No matches found" }));
            }
            std::fs::write(&path, &new_content)
                .map_err(|e| format!("Failed to write file: {}", e))?;
            let count = if replace_all { content.matches(&find).count() } else { 1 };
            Ok(json!({ "success": true, "message": format!("Replaced {} occurrence(s) in {}", count, path) }))
        }

        "local_move_files" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(&resolve_path)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let destination = resolve_path(&get_str("destination")?);
            validate_path(&destination, "destination")?;
            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }
            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("Failed to create destination: {}", e))?;
            let mut moved = 0u32;
            let mut errors = Vec::new();
            for source in &paths {
                if let Err(e) = validate_path(source, "path") {
                    errors.push(format!("{}: {}", source, e));
                    continue;
                }
                let filename = std::path::Path::new(source)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let dest_path = format!("{}/{}", destination.trim_end_matches('/'), filename);
                match std::fs::rename(source, &dest_path) {
                    Ok(_) => moved += 1,
                    Err(_) => {
                        match std::fs::copy(source, &dest_path).and_then(|_| std::fs::remove_file(source)) {
                            Ok(_) => moved += 1,
                            Err(e) => errors.push(format!("{}: {}", filename, e)),
                        }
                    }
                }
            }
            Ok(json!({ "moved": moved, "errors": errors }))
        }

        "local_copy_files" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(&resolve_path)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let destination = resolve_path(&get_str("destination")?);
            validate_path(&destination, "destination")?;
            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("Failed to create destination: {}", e))?;
            let mut copied = 0u32;
            let mut errors = Vec::new();
            for source in &paths {
                if let Err(e) = validate_path(source, "path") {
                    errors.push(format!("{}: {}", source, e));
                    continue;
                }
                let filename = std::path::Path::new(source)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let dest_path = format!("{}/{}", destination.trim_end_matches('/'), filename);
                match std::fs::copy(source, &dest_path) {
                    Ok(_) => copied += 1,
                    Err(e) => errors.push(format!("{}: {}", filename, e)),
                }
            }
            Ok(json!({ "copied": copied, "errors": errors }))
        }

        "local_batch_rename" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(&resolve_path)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let mode = get_str("mode")?;
            let mut renamed = 0u32;
            let mut errors = Vec::new();
            for (idx, source) in paths.iter().enumerate() {
                if let Err(e) = validate_path(source, "paths[]") {
                    errors.push(format!("{}: {}", source, e));
                    continue;
                }
                let p = std::path::Path::new(source);
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                let parent = p.parent().map(|pp| pp.to_string_lossy().to_string()).unwrap_or_else(|| ".".to_string());
                let new_name = match mode.as_str() {
                    "find_replace" => {
                        let find = get_str("find").unwrap_or_default();
                        let replace = get_str("replace").unwrap_or_default();
                        format!("{}{}", stem.replace(&find, &replace), ext)
                    }
                    "add_prefix" => {
                        let prefix = get_str_opt("prefix").unwrap_or_default();
                        format!("{}{}{}", prefix, stem, ext)
                    }
                    "add_suffix" => {
                        let suffix = get_str_opt("suffix").unwrap_or_default();
                        format!("{}{}{}", stem, suffix, ext)
                    }
                    "sequential" => {
                        let base = get_str_opt("base_name").unwrap_or_else(|| "file".to_string());
                        let start = args.get("start_number").and_then(|v| v.as_u64()).unwrap_or(1);
                        format!("{}_{:03}{}", base, start + idx as u64, ext)
                    }
                    _ => { errors.push(format!("Unknown mode: {}", mode)); continue; }
                };
                let dest = format!("{}/{}", parent, new_name);
                if let Err(e) = validate_path(&dest, "destination") {
                    errors.push(format!("{} -> {}: {}", source, dest, e));
                    continue;
                }
                match std::fs::rename(source, &dest) {
                    Ok(_) => renamed += 1,
                    Err(e) => errors.push(format!("{}: {}", source, e)),
                }
            }
            Ok(json!({ "renamed": renamed, "errors": errors }))
        }

        "local_trash" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(&resolve_path)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let mut trashed = 0u32;
            let mut errors = Vec::new();
            for source in &paths {
                if let Err(e) = validate_path(source, "paths[]") {
                    errors.push(format!("{}: {}", source, e));
                    continue;
                }
                match trash::delete(source) {
                    Ok(_) => trashed += 1,
                    Err(e) => errors.push(format!("{}: {}", source, e)),
                }
            }
            Ok(json!({ "trashed": trashed, "errors": errors }))
        }

        "local_file_info" => {
            let path = resolve_path(&get_str("path")?);
            validate_path(&path, "path")?;
            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Failed to stat: {}", e))?;
            let mut info = json!({
                "path": path,
                "size": meta.len(),
                "is_dir": meta.is_dir(),
                "is_file": meta.is_file(),
                "is_symlink": meta.is_symlink(),
                "readonly": meta.permissions().readonly(),
            });
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                    info["modified_unix"] = json!(dur.as_secs());
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                info["mode"] = json!(format!("{:o}", meta.permissions().mode()));
            }
            Ok(info)
        }

        "local_disk_usage" => {
            let path = resolve_path(&get_str("path")?);
            validate_path(&path, "path")?;
            fn dir_size(p: &std::path::Path) -> (u64, u64, u64) {
                let mut total_bytes = 0u64;
                let mut file_count = 0u64;
                let mut dir_count = 0u64;
                if let Ok(entries) = std::fs::read_dir(p) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        if let Ok(meta) = entry.metadata() {
                            if meta.is_dir() {
                                dir_count += 1;
                                let (b, f, d) = dir_size(&entry.path());
                                total_bytes += b;
                                file_count += f;
                                dir_count += d;
                            } else {
                                file_count += 1;
                                total_bytes += meta.len();
                            }
                        }
                    }
                }
                (total_bytes, file_count, dir_count)
            }
            let (bytes, files, dirs) = dir_size(std::path::Path::new(&path));
            Ok(json!({
                "total_bytes": bytes,
                "file_count": files,
                "directory_count": dirs,
                "human_readable": format!("{:.1} MB", bytes as f64 / 1_048_576.0),
            }))
        }

        "local_find_duplicates" => {
            let path = resolve_path(&get_str("path")?);
            let min_size = args.get("min_size").and_then(|v| v.as_u64()).unwrap_or(1024);
            validate_path(&path, "path")?;
            use std::collections::HashMap;
            let mut size_map: HashMap<u64, Vec<String>> = HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() && meta.len() >= min_size {
                            size_map.entry(meta.len()).or_default()
                                .push(entry.path().to_string_lossy().to_string());
                        }
                    }
                }
            }
            // Only hash files with same size
            let mut duplicates = Vec::new();
            for paths in size_map.values() {
                if paths.len() < 2 { continue; }
                let mut hash_map: HashMap<String, Vec<String>> = HashMap::new();
                for p in paths {
                    if let Ok(data) = std::fs::read(p) {
                        let digest = {
                                        use md5::Digest;
                                        let mut hasher = md5::Md5::new();
                                        hasher.update(&data);
                                        format!("{:x}", hasher.finalize())
                                    };
                        hash_map.entry(digest).or_default().push(p.clone());
                    }
                }
                for (hash, files) in hash_map {
                    if files.len() >= 2 {
                        duplicates.push(json!({ "hash": hash, "files": files }));
                    }
                }
            }
            Ok(json!({ "duplicates": duplicates, "groups": duplicates.len() }))
        }

        "local_grep" => {
            let path = resolve_path(&get_str("path")?);
            let pattern = get_str("pattern")?;
            let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let context_lines = args.get("context_lines").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let case_sensitive = args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(true);
            let glob_filter = get_str_opt("glob");
            validate_path(&path, "path")?;
            let re = if case_sensitive {
                regex::Regex::new(&pattern).map_err(|e| format!("Invalid regex: {}", e))?
            } else {
                regex::RegexBuilder::new(&pattern).case_insensitive(true).build()
                    .map_err(|e| format!("Invalid regex: {}", e))?
            };
            let mut results = Vec::new();
            fn walk_grep(dir: &std::path::Path, re: &regex::Regex, glob_filter: &Option<String>,
                        ctx: usize, results: &mut Vec<serde_json::Value>, max: usize) {
                if results.len() >= max { return; }
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        if results.len() >= max { return; }
                        let p = entry.path();
                        if p.is_dir() {
                            walk_grep(&p, re, glob_filter, ctx, results, max);
                        } else if p.is_file() {
                            if let Some(ref glob) = glob_filter {
                                let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                                let pattern = glob.trim_start_matches('*').to_lowercase();
                                if !name.ends_with(&pattern) { continue; }
                            }
                            if let Ok(content) = std::fs::read_to_string(&p) {
                                let lines: Vec<&str> = content.lines().collect();
                                for (i, line) in lines.iter().enumerate() {
                                    if results.len() >= max { return; }
                                    if re.is_match(line) {
                                        let start = i.saturating_sub(ctx);
                                        let end = (i + ctx + 1).min(lines.len());
                                        let context: Vec<String> = lines[start..end].iter().map(|l| l.to_string()).collect();
                                        results.push(serde_json::json!({
                                            "file": p.to_string_lossy(),
                                            "line": i + 1,
                                            "match": line,
                                            "context": context,
                                        }));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            walk_grep(std::path::Path::new(&path), &re, &glob_filter, context_lines, &mut results, max_results);
            let total = results.len();
            Ok(json!({ "results": results, "total": total }))
        }

        "local_head" => {
            let path = resolve_path(&get_str("path")?);
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(20).min(500) as usize;
            validate_path(&path, "path")?;
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let result: String = content.lines().take(lines).collect::<Vec<_>>().join("\n");
            let total_lines = content.lines().count();
            Ok(json!({ "content": result, "lines_shown": lines.min(total_lines), "total_lines": total_lines }))
        }

        "local_tail" => {
            let path = resolve_path(&get_str("path")?);
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(20).min(500) as usize;
            validate_path(&path, "path")?;
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let all_lines: Vec<&str> = content.lines().collect();
            let start = all_lines.len().saturating_sub(lines);
            let result = all_lines[start..].join("\n");
            Ok(json!({ "content": result, "lines_shown": all_lines.len() - start, "total_lines": all_lines.len() }))
        }

        "local_stat_batch" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(&resolve_path)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            if paths.len() > 100 {
                return Err("Maximum 100 paths allowed".to_string());
            }
            let stats: Vec<serde_json::Value> = paths.iter().map(|p| {
                if let Err(error) = validate_path(p, "paths[]") {
                    return json!({ "path": p, "exists": false, "error": error });
                }
                match std::fs::metadata(p) {
                    Ok(meta) => json!({
                        "path": p,
                        "exists": true,
                        "size": meta.len(),
                        "is_dir": meta.is_dir(),
                        "is_file": meta.is_file(),
                        "readonly": meta.permissions().readonly(),
                    }),
                    Err(e) => json!({ "path": p, "exists": false, "error": e.to_string() }),
                }
            }).collect();
            Ok(json!({ "stats": stats }))
        }

        "local_tree" => {
            let path = resolve_path(&get_str("path")?);
            let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3).min(10) as usize;
            let show_hidden = args.get("show_hidden").and_then(|v| v.as_bool()).unwrap_or(false);
            let glob_filter = get_str_opt("glob");
            validate_path(&path, "path")?;
            fn build_tree(dir: &std::path::Path, depth: usize, max_depth: usize,
                         show_hidden: bool, glob_filter: &Option<String>) -> Vec<serde_json::Value> {
                if depth >= max_depth { return vec![]; }
                let mut items = Vec::new();
                if let Ok(entries) = std::fs::read_dir(dir) {
                    let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                    sorted.sort_by_key(|e| e.file_name());
                    for entry in sorted {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !show_hidden && name.starts_with('.') { continue; }
                        let meta = entry.metadata().ok();
                        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                        if !is_dir {
                            if let Some(ref glob) = glob_filter {
                                let pattern = glob.trim_start_matches('*').to_lowercase();
                                if !name.to_lowercase().ends_with(&pattern) { continue; }
                            }
                        }
                        let mut node = serde_json::json!({
                            "name": name,
                            "is_dir": is_dir,
                            "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        });
                        if is_dir {
                            let children = build_tree(&entry.path(), depth + 1, max_depth, show_hidden, glob_filter);
                            node["children"] = serde_json::json!(children);
                        }
                        items.push(node);
                    }
                }
                items
            }
            let tree = build_tree(std::path::Path::new(&path), 0, max_depth, show_hidden, &glob_filter);
            Ok(json!({ "tree": tree }))
        }

        "local_diff" => {
            let path_a = resolve_path(&get_str("path_a")?);
            let path_b = resolve_path(&get_str("path_b")?);
            validate_path(&path_a, "path_a")?;
            validate_path(&path_b, "path_b")?;
            let content_a = std::fs::read_to_string(&path_a)
                .map_err(|e| format!("Failed to read {}: {}", path_a, e))?;
            let content_b = std::fs::read_to_string(&path_b)
                .map_err(|e| format!("Failed to read {}: {}", path_b, e))?;
            use similar::TextDiff;
            let diff = TextDiff::from_lines(&content_a, &content_b);
            let unified = diff.unified_diff().header(&path_a, &path_b).to_string();
            Ok(json!({ "diff": unified, "has_changes": !unified.is_empty() }))
        }

        "hash_file" => {
            let path = resolve_path(&get_str("path")?);
            let algorithm = get_str_opt("algorithm").unwrap_or_else(|| "sha256".to_string());
            validate_path(&path, "path")?;
            let data = std::fs::read(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let hash = match algorithm.to_lowercase().as_str() {
                "md5" => {
                                        use md5::Digest;
                                        let mut hasher = md5::Md5::new();
                                        hasher.update(&data);
                                        format!("{:x}", hasher.finalize())
                                    },
                "sha256" => {
                    use sha2::Digest;
                    format!("{:x}", sha2::Sha256::digest(&data))
                }
                "sha512" => {
                    use sha2::Digest;
                    format!("{:x}", sha2::Sha512::digest(&data))
                }
                "blake3" => blake3::hash(&data).to_hex().to_string(),
                other => return Err(format!("Unsupported algorithm: {}", other)),
            };
            Ok(json!({ "path": path, "algorithm": algorithm, "hash": hash }))
        }

        "shell_execute" => {
            let command = get_str("command")?;
            let working_dir = get_str_opt("working_dir");
            let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(30).min(120);

            // Defense-in-depth: reject shell meta-characters that enable denylist bypass
            // (pipes, subshells, backticks, semicolons, eval chains, etc.)
            // Mirrors ai_tools.rs shell_execute meta-char filter
            const SHELL_META: &[char] = &['|', ';', '`', '$', '&', '(', ')', '{', '}', '\n', '\r'];
            if SHELL_META.iter().any(|c| command.contains(*c)) {
                return Err(
                    "Command contains shell meta-characters (|;&`$(){}\\n\\r). Use simple commands only."
                        .to_string(),
                );
            }

            // Denylist (mirrors ai_tools.rs DENIED_COMMAND_PATTERNS)
            static DENIED: &[&str] = &[
                "rm -rf /", "rm -rf /*", "mkfs", "dd if=", ":(){", "fork bomb",
                "chmod -R 777 /", "chmod 777 /", "chown ",
                "wget|sh", "curl|sh", "curl|bash", "wget|bash",
                "> /dev/sda", "shutdown", "reboot", "halt",
                "init 0", "init 6",
                "kill -9 1", "killall", "pkill -9",
                "python -c", "python3 -c", "eval ",
                "base64 -d", "base64 --decode",
                "truncate", "shred",
                "crontab", "nohup", "systemctl", "service ",
                "mount ", "umount ", "fdisk", "parted",
                "iptables", "useradd", "userdel", "passwd", "sudo ",
            ];
            let cmd_lower = command.to_lowercase();
            for pattern in DENIED {
                if cmd_lower.contains(pattern) {
                    return Err(format!("Command denied for safety: contains '{}'", pattern));
                }
            }

            // Validate working_dir against deny-list
            if let Some(ref wd) = working_dir {
                validate_path(wd, "working_dir")?;
            }

            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(&command)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(ref wd) = working_dir {
                cmd.current_dir(wd);
            }
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                cmd.output(),
            ).await
                .map_err(|_| format!("Command timed out after {}s", timeout_secs))?
                .map_err(|e| format!("Failed to execute: {}", e))?;

            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            // Truncate output to 10KB
            let max_out = 10240;
            Ok(json!({
                "exit_code": result.status.code().unwrap_or(-1),
                "stdout": if stdout.len() > max_out { &stdout[..max_out] } else { &stdout },
                "stderr": if stderr.len() > max_out { &stderr[..max_out] } else { &stderr },
            }))
        }

        "archive_compress" => {
            // Safe: spawn tools directly with .arg() — no shell interpolation
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(&resolve_path)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let output_path = resolve_path(&get_str("output_path")?);
            let format = get_str_opt("format").unwrap_or_else(|| "zip".to_string());
            validate_path(&output_path, "output_path")?;
            for p in &paths { validate_path(p, "paths[]")?; }
            let mut cmd = match format.as_str() {
                "zip" => {
                    let mut c = tokio::process::Command::new("zip");
                    c.arg("-r").arg(&output_path);
                    for p in &paths { c.arg(p); }
                    c
                }
                "tar.gz" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("czf").arg(&output_path);
                    for p in &paths { c.arg(p); }
                    c
                }
                "tar.bz2" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("cjf").arg(&output_path);
                    for p in &paths { c.arg(p); }
                    c
                }
                "tar.xz" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("cJf").arg(&output_path);
                    for p in &paths { c.arg(p); }
                    c
                }
                "tar" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("cf").arg(&output_path);
                    for p in &paths { c.arg(p); }
                    c
                }
                "7z" => {
                    let mut c = tokio::process::Command::new("7z");
                    c.arg("a").arg(&output_path);
                    for p in &paths { c.arg(p); }
                    c
                }
                _ => return Err(format!("Unsupported format: {}", format)),
            };
            let output = cmd.output().await.map_err(|e| format!("Failed: {}", e))?;
            if output.status.success() {
                Ok(json!({ "success": true, "output": output_path, "format": format }))
            } else {
                Err(String::from_utf8_lossy(&output.stderr).to_string())
            }
        }

        "archive_decompress" => {
            let archive_path = resolve_path(&get_str("archive_path")?);
            let output_dir = resolve_path(&get_str("output_dir")?);
            validate_path(&archive_path, "archive_path")?;
            validate_path(&output_dir, "output_dir")?;
            std::fs::create_dir_all(&output_dir).ok();
            let ext = archive_path.to_lowercase();
            let mut cmd = if ext.ends_with(".zip") {
                let mut c = tokio::process::Command::new("unzip");
                c.arg("-o").arg(&archive_path).arg("-d").arg(&output_dir);
                c
            } else if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
                let mut c = tokio::process::Command::new("tar");
                c.arg("xzf").arg(&archive_path).arg("-C").arg(&output_dir);
                c
            } else if ext.ends_with(".tar.bz2") {
                let mut c = tokio::process::Command::new("tar");
                c.arg("xjf").arg(&archive_path).arg("-C").arg(&output_dir);
                c
            } else if ext.ends_with(".tar.xz") {
                let mut c = tokio::process::Command::new("tar");
                c.arg("xJf").arg(&archive_path).arg("-C").arg(&output_dir);
                c
            } else if ext.ends_with(".tar") {
                let mut c = tokio::process::Command::new("tar");
                c.arg("xf").arg(&archive_path).arg("-C").arg(&output_dir);
                c
            } else if ext.ends_with(".7z") {
                let mut c = tokio::process::Command::new("7z");
                c.arg("x").arg(&archive_path).arg(format!("-o{}", output_dir));
                c
            } else {
                return Err(format!("Unsupported archive format: {}", archive_path));
            };
            let output = cmd.output().await.map_err(|e| format!("Failed: {}", e))?;
            if output.status.success() {
                Ok(json!({ "success": true, "output_dir": output_dir }))
            } else {
                Err(String::from_utf8_lossy(&output.stderr).to_string())
            }
        }

        "clipboard_write" => {
            let content = get_str("content")?;
            // Use xclip/xsel on Linux
            let mut child = tokio::process::Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|_| "xclip not found. Install: sudo apt install xclip".to_string())?;
            if let Some(ref mut stdin) = child.stdin {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(content.as_bytes()).await
                    .map_err(|e| format!("Failed to write to clipboard: {}", e))?;
            }
            child.wait().await.map_err(|e| format!("xclip failed: {}", e))?;
            Ok(json!({ "success": true, "message": format!("Copied {} chars to clipboard", content.len()) }))
        }

        "agent_memory_write" => {
            let entry = get_str("entry")?;
            let category = get_str_opt("category").unwrap_or_else(|| "general".to_string());
            let project_path = std::env::current_dir()
                .map(|cwd| cwd.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            let stored = ftp_client_gui_lib::agent_memory_db::store_memory_entry_cli(
                &project_path,
                &category,
                &entry,
                None,
            )?;
            Ok(json!({ "success": true, "message": format!("Saved memory entry {}", stored.id) }))
        }

        "app_info" => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "?".to_string());
            Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "platform": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "working_directory": cwd,
                "mode": "cli",
            }))
        }

        "server_list_saved" => {
            let profiles = safe_vault_profiles_for_agent()?;
            Ok(json!({
                "servers": profiles,
                "count": profiles.len(),
            }))
        }

        "remote_list" => {
            let server_query = get_str("server")?;
            let path = get_str_opt("path").unwrap_or_else(|| "/".to_string());
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let entries = provider.list(&path).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let entries = entries?;
            let items: Vec<serde_json::Value> = entries.iter().take(200).map(|e| json!({
                "name": e.name,
                "path": e.path,
                "is_dir": e.is_dir,
                "size": e.size,
                "modified": e.modified,
            })).collect();
            Ok(json!({
                "server": server_query,
                "path": path,
                "entries": items,
                "total": entries.len(),
                "truncated": entries.len() > 200,
            }))
        }

        "remote_read" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let data = provider.download_to_bytes(&path).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let data = data?;
            let truncated = data.len() > 5 * 1024;
            let preview = if truncated { &data[..5 * 1024] } else { &data };
            let content = String::from_utf8_lossy(preview).to_string();
            Ok(json!({
                "server": server_query,
                "path": path,
                "content": content,
                "size": data.len(),
                "truncated": truncated,
            }))
        }

        "remote_info" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let entry = provider.stat(&path).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let entry = entry?;
            Ok(json!({
                "server": server_query,
                "path": path,
                "name": entry.name,
                "is_dir": entry.is_dir,
                "size": entry.size,
                "modified": entry.modified,
                "permissions": entry.permissions,
                "owner": entry.owner,
            }))
        }

        "remote_search" => {
            let server_query = get_str("server")?;
            let path = get_str_opt("path").unwrap_or_else(|| "/".to_string());
            let pattern = get_str_opt("pattern").unwrap_or_else(|| "*".to_string());
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let entries = provider.find(&path, &pattern).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let entries = entries?;
            let items: Vec<serde_json::Value> = entries.iter().take(100).map(|e| json!({
                "name": e.name,
                "path": e.path,
                "is_dir": e.is_dir,
                "size": e.size,
            })).collect();
            Ok(json!({
                "server": server_query,
                "path": path,
                "pattern": pattern,
                "results": items,
                "total": entries.len(),
                "truncated": entries.len() > 100,
            }))
        }

        "remote_upload" => {
            let server_query = get_str("server")?;
            let remote_path = get_str("remote_path")?;
            if remote_path.contains('\0') {
                return Err("remote_path contains null bytes".to_string());
            }
            let local_path = get_str_opt("local_path");
            let content = get_str_opt("content");
            if local_path.is_none() && content.is_none() {
                return Err("Provide either 'local_path' or 'content'".to_string());
            }

            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let upload_source = if let Some(local_path) = local_path {
                let resolved = resolve_path(&local_path);
                validate_path(&resolved, "local_path")?;
                resolved
            } else {
                let mut temp = NamedTempFile::new()
                    .map_err(|e| format!("Cannot create temp upload file: {}", e))?;
                temp.write_all(content.as_deref().unwrap_or_default().as_bytes())
                    .map_err(|e| format!("Cannot write temp upload file: {}", e))?;
                temp.flush()
                    .map_err(|e| format!("Cannot flush temp upload file: {}", e))?;
                let temp_path = temp.path().to_string_lossy().to_string();
                match provider.upload(&temp_path, &remote_path, None).await {
                    Ok(()) => {
                        let _ = provider.disconnect().await;
                        return Ok(json!({
                            "server": server_query,
                            "remote_path": remote_path,
                            "uploaded": true,
                            "bytes": content.as_deref().unwrap_or_default().len(),
                        }));
                    }
                    Err(e) => {
                        let _ = provider.disconnect().await;
                        return Err(e.to_string());
                    }
                }
            };

            let bytes = std::fs::metadata(&upload_source).map(|m| m.len()).unwrap_or(0);
            let result = provider.upload(&upload_source, &remote_path, None).await;
            let _ = provider.disconnect().await;
            result.map_err(|e| e.to_string())?;
            Ok(json!({
                "server": server_query,
                "remote_path": remote_path,
                "uploaded": true,
                "bytes": bytes,
            }))
        }

        "remote_download" => {
            let server_query = get_str("server")?;
            let remote_path = get_str("remote_path")?;
            let local_path = resolve_path(&get_str("local_path")?);
            validate_path(&local_path, "local_path")?;
            if let Some(parent) = Path::new(&local_path).parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create local parent directory: {}", e))?;
            }
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let result = provider
                .download(&remote_path, &local_path, None)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({
                "server": server_query,
                "remote_path": remote_path,
                "local_path": local_path,
                "downloaded": true,
            }))
        }

        "remote_mkdir" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            if path.contains('\0') {
                return Err("path contains null bytes".to_string());
            }
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let result = provider.mkdir(&path).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({ "server": server_query, "path": path, "created": true }))
        }

        "remote_delete" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            if path.contains('\0') {
                return Err("path contains null bytes".to_string());
            }
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let result = provider.delete(&path).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({ "server": server_query, "path": path, "deleted": true }))
        }

        "remote_rename" => {
            let server_query = get_str("server")?;
            let from = get_str("from")?;
            let to = get_str("to")?;
            if from.contains('\0') || to.contains('\0') {
                return Err("remote path contains null bytes".to_string());
            }
            let (mut provider, _) = create_and_connect_for_agent(&server_query).await?;
            let result = provider.rename(&from, &to).await.map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({ "server": server_query, "from": from, "to": to, "renamed": true }))
        }

        "server_exec" => {
            let server_query = get_str("server")?;
            let operation = get_str("operation")?;
            let path = get_str_opt("path").unwrap_or_else(|| "/".to_string());
            let pattern = get_str_opt("pattern");

            let valid_ops = ["ls", "cat", "stat", "find", "df"];
            if !valid_ops.contains(&operation.as_str()) {
                return Err(format!(
                    "Invalid operation '{}'. CLI agent supports: {}. Mutative operations (put, rm, mv, mkdir) require explicit CLI commands.",
                    operation, valid_ops.join(", ")
                ));
            }

            // Validate path
            if path.contains('\0') {
                return Err("Path contains null bytes".to_string());
            }

            let (mut provider, _initial_path) = create_and_connect_for_agent(&server_query).await?;

            let result = match operation.as_str() {
                "ls" => {
                    let entries = provider.list(&path).await.map_err(|e| e.to_string())?;
                    let items: Vec<serde_json::Value> = entries.iter().take(200).map(|e| json!({
                        "name": e.name,
                        "path": e.path,
                        "is_dir": e.is_dir,
                        "size": e.size,
                        "modified": e.modified,
                    })).collect();
                    json!({
                        "operation": "ls",
                        "server": server_query,
                        "path": path,
                        "entries": items,
                        "total": entries.len(),
                        "truncated": entries.len() > 200,
                    })
                }
                "cat" => {
                    let data = provider.download_to_bytes(&path).await.map_err(|e| e.to_string())?;
                    if data.len() > 5 * 1024 {
                        let preview = String::from_utf8_lossy(&data[..5 * 1024]);
                        json!({
                            "operation": "cat",
                            "server": server_query,
                            "path": path,
                            "content": preview,
                            "size": data.len(),
                            "truncated": true,
                        })
                    } else {
                        let content = String::from_utf8_lossy(&data);
                        json!({
                            "operation": "cat",
                            "server": server_query,
                            "path": path,
                            "content": content,
                            "size": data.len(),
                            "truncated": false,
                        })
                    }
                }
                "stat" => {
                    let entry = provider.stat(&path).await.map_err(|e| e.to_string())?;
                    json!({
                        "operation": "stat",
                        "server": server_query,
                        "path": path,
                        "name": entry.name,
                        "is_dir": entry.is_dir,
                        "size": entry.size,
                        "modified": entry.modified,
                        "permissions": entry.permissions,
                    })
                }
                "find" => {
                    let pat = pattern.unwrap_or_else(|| "*".to_string());
                    let entries = provider.find(&path, &pat).await.map_err(|e| e.to_string())?;
                    let items: Vec<serde_json::Value> = entries.iter().take(100).map(|e| json!({
                        "name": e.name,
                        "path": e.path,
                        "is_dir": e.is_dir,
                        "size": e.size,
                    })).collect();
                    json!({
                        "operation": "find",
                        "server": server_query,
                        "path": path,
                        "pattern": pat,
                        "results": items,
                        "total": entries.len(),
                        "truncated": entries.len() > 100,
                    })
                }
                "df" => {
                    let info = provider.storage_info().await.map_err(|e| e.to_string())?;
                    json!({
                        "operation": "df",
                        "server": server_query,
                        "used_bytes": info.used,
                        "total_bytes": info.total,
                        "free_bytes": info.free,
                    })
                }
                _ => unreachable!(),
            };

            let _ = provider.disconnect().await;
            Ok(result)
        }

        _ => Err(format!("Tool '{}' is not available in CLI mode", tool_name)),
    }
}

/// Ask for interactive tool approval. Returns true if approved.
fn prompt_tool_approval(tool_name: &str, args: &serde_json::Value) -> bool {
    let danger = tool_danger_level(tool_name);
    let level_str = match danger {
        0 => "\x1b[32mSAFE\x1b[0m",
        1 => "\x1b[33mMEDIUM\x1b[0m",
        _ => "\x1b[1;31mHIGH\x1b[0m",
    };
    eprintln!();
    eprintln!("  \x1b[1m🔧 Tool Call:\x1b[0m {} [{}]", tool_name, level_str);
    // Show key arguments
    if let Some(obj) = args.as_object() {
        for (key, val) in obj {
            let display = match val {
                serde_json::Value::String(s) => {
                    if s.len() > 80 { format!("{}...", s.get(..77).unwrap_or(s)) } else { s.clone() }
                }
                other => other.to_string(),
            };
            eprintln!("    {}: {}", key, display);
        }
    }
    eprint!("  \x1b[1mApprove? [Y/n]\x1b[0m ");
    io::stderr().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let trimmed = input.trim().to_lowercase();
        trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
    } else {
        false
    }
}

/// Check if a tool call should be auto-approved based on approve_level.
/// approve_level: 0=none(ask all), 1=auto-approve safe, 2=auto-approve safe+medium, 3=auto-approve all
fn is_auto_approved(tool_name: &str, approve_level: u8) -> bool {
    let danger = tool_danger_level(tool_name);
    match approve_level {
        0 => false,
        1 => danger == 0,
        2 => danger <= 1,
        3 => true,
        _ => false,
    }
}

/// Multi-step agent tool execution loop.
/// Sends tool definitions to the model, processes tool_calls, executes tools,
/// re-injects results, and loops until the model responds with text only.
async fn agent_tool_loop(
    cfg: &AgentConfig,
    messages: &mut Vec<ftp_client_gui_lib::ai::ChatMessage>,
    is_tty: bool,
) -> Result<String, String> {
    use ftp_client_gui_lib::ai::{AIRequest, AIResponse, AIToolResult, ChatMessage};

    let tools = cli_tool_definitions();
    let mut steps = 0u32;
    // Pending tool results from previous iteration (passed via native tool_results field)
    let mut pending_tool_results: Option<Vec<AIToolResult>> = None;

    loop {
        // Build request with tool definitions
        let mut all_messages = vec![ChatMessage {
            role: "system".to_string(),
            content: cfg.system.clone(),
            images: None,
        }];
        all_messages.extend_from_slice(messages);

        let request = AIRequest {
            provider_type: cfg.provider_type.clone(),
            model: cfg.model.clone(),
            api_key: Some(cfg.api_key.clone()),
            base_url: cfg.base_url.clone(),
            messages: all_messages,
            max_tokens: Some(4096),
            temperature: Some(0.3),
            tools: Some(tools.clone()),
            tool_results: pending_tool_results.take(),
            thinking_budget: None,
            top_p: None,
            top_k: None,
            cached_content: None,
            web_search: None,
        };

        let response: AIResponse = ftp_client_gui_lib::ai::call_ai(request)
            .await
            .map_err(|e| e.to_string())?;

        {
            let mut usage = cfg.usage.lock().map_err(|_| "Agent usage lock poisoned".to_string())?;
            usage.input_tokens += response.input_tokens.unwrap_or(0) as u64;
            usage.output_tokens += response.output_tokens.unwrap_or(0) as u64;
            usage.total_tokens += response.tokens_used.unwrap_or(0) as u64;

            if let Some(limit) = cfg.cost_limit {
                let estimated = estimate_ai_cost_usd(
                    &cfg.provider_name,
                    usage.input_tokens,
                    usage.output_tokens,
                );
                if estimated > limit {
                    return Err(format!(
                        "Estimated AI cost limit exceeded: ${:.4} > ${:.4} (input tokens: {}, output tokens: {})",
                        estimated,
                        limit,
                        usage.input_tokens,
                        usage.output_tokens,
                    ));
                }
            }
        }

        // Check if model wants to call tools
        let tool_calls = response.tool_calls.as_ref()
            .filter(|tc| !tc.is_empty());

        match tool_calls {
            None => {
                // No tool calls — return the text response
                return Ok(response.content);
            }
            Some(calls) => {
                if cfg.plan_only {
                    let plan_lines: Vec<String> = calls.iter().map(|tc| {
                        format!(
                            "- {} {}",
                            tc.name,
                            serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string())
                        )
                    }).collect();
                    let mut output = response.content.clone();
                    if !output.is_empty() {
                        output.push_str("\n\n");
                    }
                    output.push_str("Planned tool calls:\n");
                    output.push_str(&plan_lines.join("\n"));
                    return Ok(output);
                }

                steps += 1;
                if steps > cfg.max_steps {
                    if !response.content.is_empty() {
                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: response.content.clone(),
                            images: None,
                        });
                    }
                    return Ok(format!("{}\n\n[Reached max steps limit ({}).]", response.content, cfg.max_steps));
                }

                // Show assistant text if any
                if !response.content.is_empty() && is_tty {
                    eprintln!("\n{}", response.content);
                }

                // Add assistant message with tool call markers
                let mut assistant_text = response.content.clone();
                let tool_call_desc: Vec<String> = calls.iter().map(|tc| {
                    format!("[Tool call: {} ({})]", tc.name, tc.id)
                }).collect();
                if !assistant_text.is_empty() {
                    assistant_text.push('\n');
                }
                assistant_text.push_str(&tool_call_desc.join("\n"));

                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: assistant_text,
                    images: None,
                });

                // Execute each tool call and collect native tool results
                let mut native_results: Vec<AIToolResult> = Vec::new();
                for tc in calls {
                    // Check approval
                    let approved = if is_auto_approved(&tc.name, cfg.approve_level) {
                        if is_tty {
                            eprintln!("  \x1b[32m✓\x1b[0m Auto-approved: {} (safe)", tc.name);
                        }
                        true
                    } else if is_tty && io::stdin().is_terminal() {
                        prompt_tool_approval(&tc.name, &tc.arguments)
                    } else {
                        cfg.approve_level >= 3
                    };

                    let result_content = if approved {
                        if is_tty {
                            eprint!("  \x1b[2m⠙ Executing {}...\x1b[0m", tc.name);
                            io::stderr().flush().ok();
                        }
                        match execute_cli_tool(&tc.name, &tc.arguments).await {
                            Ok(val) => {
                                if is_tty {
                                    eprint!("\r                                        \r");
                                    eprintln!("  \x1b[32m✓\x1b[0m {} completed", tc.name);
                                }
                                let s = val.to_string();
                                if s.len() > 8192 {
                                    format!("{}... [truncated, {} bytes total]", s.get(..8192).unwrap_or(&s), s.len())
                                } else {
                                    s
                                }
                            }
                            Err(e) => {
                                if is_tty {
                                    eprint!("\r                                        \r");
                                    eprintln!("  \x1b[31m✗\x1b[0m {} failed: {}", tc.name, e);
                                }
                                format!("Error: {}", e)
                            }
                        }
                    } else {
                        if is_tty {
                            eprintln!("  \x1b[33m⊘\x1b[0m {} denied by user", tc.name);
                        }
                        "Tool call denied by user.".to_string()
                    };

                    native_results.push(AIToolResult {
                        tool_call_id: tc.id.clone(),
                        content: result_content,
                    });
                }

                // Store native tool results for next iteration
                pending_tool_results = Some(native_results);

                if is_tty {
                    eprint!("\n  \x1b[2m⠙ Thinking...\x1b[0m");
                    io::stderr().flush().ok();
                }
            }
        }
    }
}

/// Run the agent in interactive REPL, one-shot, or orchestration mode
#[allow(clippy::too_many_arguments)]
async fn cmd_agent(
    message: Option<String>,
    provider_name: Option<String>,
    model_override: Option<String>,
    connect_url: Option<String>,
    auto_approve: String,
    max_steps: u32,
    orchestrate: bool,
    mcp: bool,
    stdin_mode: bool,
    plan_only: bool,
    cost_limit: Option<f64>,
    system_prompt: Option<String>,
    _cli: &Cli,
    format: OutputFormat,
    _cancelled: Arc<AtomicBool>,
) -> i32 {
    // Detect provider
    let (prov_name, api_key, base_url) = if let Some(ref name) = provider_name {
        let env_key = format!("{}_API_KEY", name.to_uppercase());
        let key = std::env::var(&env_key).unwrap_or_default();
        let url = match name.as_str() {
            "anthropic" => "https://api.anthropic.com",
            "openai" => "https://api.openai.com/v1",
            "gemini" | "google" => "https://generativelanguage.googleapis.com",
            "ollama" => "http://localhost:11434",
            "groq" => "https://api.groq.com/openai/v1",
            "mistral" => "https://api.mistral.ai/v1",
            "deepseek" => "https://api.deepseek.com",
            "xai" => "https://api.x.ai/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "together" => "https://api.together.xyz/v1",
            "fireworks" => "https://api.fireworks.ai/inference/v1",
            "cerebras" => "https://api.cerebras.ai/v1",
            "sambanova" => "https://api.sambanova.ai/v1",
            "perplexity" => "https://api.perplexity.ai",
            "cohere" => "https://api.cohere.com/compatibility",
            "ai21" => "https://api.ai21.com/studio/v1",
            "kimi" | "moonshot" => "https://api.moonshot.cn/v1",
            "qwen" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
            _ => "https://api.openai.com/v1",
        };
        // Ollama doesn't require API key; all others do
        if key.is_empty() && name != "ollama" {
            eprintln!("Error: {} is not set or empty.", env_key);
            eprintln!("Set it: export {}=your-api-key", env_key);
            return 5;
        }
        (name.clone(), key, url.to_string())
    } else if let Some(detected) = detect_ai_provider() {
        detected
    } else if let Some(vault_detected) = detect_ai_provider_from_vault(_cli) {
        vault_detected
    } else {
        eprintln!("Error: No AI provider configured.");
        eprintln!("Set an API key environment variable:");
        eprintln!("  export ANTHROPIC_API_KEY=sk-ant-...");
        eprintln!("  export OPENAI_API_KEY=sk-...");
        eprintln!("Or configure a provider in AeroFTP desktop (Settings > AI).");
        eprintln!("Or specify: aeroftp agent --provider anthropic");
        return 5;
    };

    // Ensure vault is open for server_list_saved/server_exec tools (even when provider came from env)
    let _ = open_vault(_cli);

    let model = model_override.unwrap_or_else(|| default_model(&prov_name).to_string());
    let provider_type = provider_type_from_name(&prov_name);
    let approve_level = parse_approve_level(&auto_approve);
    let mut system = build_agent_system_prompt(&system_prompt);

    if let Some(target) = connect_url {
        let summary = if target.contains("://") {
            match create_and_connect(&target, _cli, format).await {
                Ok((mut provider, initial_path)) => {
                    let provider_label = provider.provider_type().to_string();
                    let display = provider.display_name();
                    let _ = provider.disconnect().await;
                    format!("Pre-validated remote target: {} ({}) path {}", display, provider_label, initial_path)
                }
                Err(code) => {
                    print_error(format, &format!("agent pre-connect failed for '{}'", target), code);
                    return code;
                }
            }
        } else {
            match create_and_connect_for_agent(&target).await {
                Ok((mut provider, initial_path)) => {
                    let provider_label = provider.provider_type().to_string();
                    let display = provider.display_name();
                    let _ = provider.disconnect().await;
                    format!("Pre-validated saved server: {} ({}) path {}", display, provider_label, initial_path)
                }
                Err(e) => {
                    print_error(format, &format!("agent pre-connect failed: {}", e), 6);
                    return 6;
                }
            }
        };
        system.push_str("\n- ");
        system.push_str(&summary);
    }

    let cfg = AgentConfig {
        provider_name: prov_name,
        api_key,
        base_url,
        model,
        provider_type,
        approve_level,
        max_steps,
        system,
        plan_only,
        cost_limit,
        usage: Arc::new(Mutex::new(AgentUsage::default())),
    };

    // MCP server mode
    if mcp {
        return cmd_agent_mcp(&cfg.provider_name, _cli).await;
    }

    // Orchestration mode (JSON-RPC 2.0 over stdio)
    if orchestrate {
        return cmd_agent_orchestrate(&cfg).await;
    }

    // One-shot mode: -m "message" or --stdin
    let one_shot_message = if let Some(msg) = message {
        Some(msg)
    } else if stdin_mode {
        let mut buf = String::new();
        // CL-004: Limit stdin to 1 MB to prevent OOM
        let mut limited = io::Read::take(io::stdin(), 1_048_576);
        if let Ok(n) = io::Read::read_to_string(&mut limited, &mut buf) {
            if n > 0 { Some(buf) } else { None }
        } else {
            None
        }
    } else {
        None
    };

    if let Some(msg) = one_shot_message {
        return cmd_agent_oneshot(&msg, &cfg, format).await;
    }

    // Interactive REPL mode
    cmd_agent_repl(&cfg).await
}

/// Shared agent configuration — avoids passing too many arguments
#[allow(dead_code)]
struct AgentConfig {
    provider_name: String,
    api_key: String,
    base_url: String,
    model: String,
    provider_type: ftp_client_gui_lib::ai::AIProviderType,
    approve_level: u8,
    max_steps: u32,
    system: String,
    plan_only: bool,
    cost_limit: Option<f64>,
    usage: Arc<Mutex<AgentUsage>>,
}

#[derive(Default)]
struct AgentUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

/// One-shot agent mode
async fn cmd_agent_oneshot(
    message: &str,
    cfg: &AgentConfig,
    format: OutputFormat,
) -> i32 {
    use ftp_client_gui_lib::ai::ChatMessage;

    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: message.to_string(),
        images: None,
    }];

    let is_tty = io::stdin().is_terminal();

    match agent_tool_loop(cfg, &mut messages, is_tty).await {
        Ok(response) => {
            match format {
                OutputFormat::Json => {
                    println!("{}", serde_json::json!({
                        "status": "ok",
                        "response": response,
                    }));
                }
                OutputFormat::Text => {
                    println!("{}", response);
                }
            }
            0
        }
        Err(e) => {
            match format {
                OutputFormat::Json => {
                    println!("{}", serde_json::json!({
                        "status": "error",
                        "error": e,
                    }));
                }
                OutputFormat::Text => {
                    eprintln!("Error: {}", e);
                }
            }
            1
        }
    }
}

/// Interactive REPL agent mode
async fn cmd_agent_repl(cfg: &AgentConfig) -> i32 {
    use ftp_client_gui_lib::ai::ChatMessage;
    use std::io::BufRead;

    let cli_tool_count = cli_tool_definitions().len();

    // Banner
    eprintln!();
    eprintln!("  \x1b[1m╭─────────────────────────────────────────────╮\x1b[0m");
    eprintln!("  \x1b[1m│           AeroAgent Interactive              │\x1b[0m");
    eprintln!("  \x1b[1m│       AI-Powered File Operations Shell       │\x1b[0m");
    eprintln!("  \x1b[1m│                                              │\x1b[0m");
    eprintln!("  \x1b[1m│  {} tools · 19 AI providers · tool execution  │\x1b[0m", cli_tool_count);
    eprintln!("  \x1b[1m╰─────────────────────────────────────────────╯\x1b[0m");
    eprintln!();
    eprintln!("  \x1b[36mProvider:\x1b[0m  {} ({})", cfg.provider_name, cfg.model);
    let mode_str = match cfg.approve_level {
        0 => "Manual (approve all tools)",
        1 => "Safe (auto-approve safe tools)",
        2 => "Medium (auto-approve safe + medium)",
        3 => "Auto (auto-approve all tools)",
        _ => "Unknown",
    };
    eprintln!("  \x1b[36mMode:\x1b[0m      {}", mode_str);
    eprintln!("  \x1b[36mMax steps:\x1b[0m {}", cfg.max_steps);
    eprintln!();
    eprintln!("  Type \x1b[1;32m/help\x1b[0m for commands, or ask anything.");
    eprintln!("  Press \x1b[1mCtrl+D\x1b[0m to exit.\n");

    let mut conversation: Vec<ChatMessage> = Vec::new();
    let stdin = io::stdin();
    let is_tty = stdin.is_terminal();

    loop {
        // Print prompt
        if is_tty {
            eprint!("\x1b[1;37m> \x1b[0m");
            io::stderr().flush().ok();
        }

        // Read input
        let reader = stdin.lock();
        let input = match reader.lines().next() {
            Some(Ok(line)) => line,
            _ => break, // EOF (Ctrl+D)
        };

        let input = input.trim().to_string();
        if input.is_empty() {
            continue;
        }

        // Handle slash commands
        if input.starts_with('/') {
            match input.as_str() {
                "/help" => {
                    eprintln!("\n  \x1b[1mAeroAgent Commands:\x1b[0m");
                    eprintln!("  /help          Show this help");
                    eprintln!("  /tools         List available tools");
                    eprintln!("  /context       Show current context");
                    eprintln!("  /clear         Clear conversation");
                    eprintln!("  /cost          Show token usage");
                    eprintln!("  /quit          Exit\n");
                }
                "/tools" => {
                    let tools = cli_tool_definitions();
                    eprintln!("\n  \x1b[1mAvailable Tools ({}):\x1b[0m\n", tools.len());
                    for t in &tools {
                        let danger = tool_danger_level(&t.name);
                        let label = match danger {
                            0 => "\x1b[32mSAFE\x1b[0m",
                            1 => "\x1b[33mMEDIUM\x1b[0m",
                            _ => "\x1b[1;31mHIGH\x1b[0m",
                        };
                        eprintln!("  [{}] \x1b[1m{}\x1b[0m — {}", label, t.name, t.description);
                    }
                    eprintln!();
                }
                "/context" => {
                    let cwd = std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "?".to_string());
                    eprintln!("\n  \x1b[1mContext:\x1b[0m");
                    eprintln!("  CWD:      {}", cwd);
                    eprintln!("  Provider: {} ({})", cfg.provider_name, cfg.model);
                    eprintln!("  Messages: {}\n", conversation.len());
                }
                "/clear" => {
                    conversation.clear();
                    eprintln!("  Conversation cleared.\n");
                }
                "/cost" => {
                    match cfg.usage.lock() {
                        Ok(usage) => {
                            let estimated = estimate_ai_cost_usd(
                                &cfg.provider_name,
                                usage.input_tokens,
                                usage.output_tokens,
                            );
                            eprintln!("  Messages:      {}", conversation.len());
                            eprintln!("  Input tokens:  {}", usage.input_tokens);
                            eprintln!("  Output tokens: {}", usage.output_tokens);
                            eprintln!("  Total tokens:  {}", usage.total_tokens);
                            eprintln!("  Est. cost:     ${:.4}\n", estimated);
                        }
                        Err(_) => eprintln!("  Token usage unavailable.\n"),
                    }
                }
                "/quit" | "/exit" | "/q" => break,
                other => {
                    eprintln!("  Unknown command: {}", other);
                    eprintln!("  Type /help for available commands.\n");
                }
            }
            continue;
        }

        // Add user message
        conversation.push(ChatMessage {
            role: "user".to_string(),
            content: input,
            images: None,
        });

        // Show thinking indicator
        if is_tty {
            eprint!("\n  \x1b[2m⠙ Thinking...\x1b[0m");
            io::stderr().flush().ok();
        }

        // Call AI with tool execution loop
        match agent_tool_loop(cfg, &mut conversation, is_tty).await {
            Ok(response) => {
                if is_tty {
                    eprint!("\r                    \r"); // Clear "Thinking..."
                }
                println!("\n{}\n", response);
                conversation.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: response,
                    images: None,
                });
                // Sliding window: keep last 40 messages to avoid context overflow
                const MAX_CONVERSATION_MESSAGES: usize = 40;
                if conversation.len() > MAX_CONVERSATION_MESSAGES {
                    let drain_count = conversation.len() - MAX_CONVERSATION_MESSAGES;
                    conversation.drain(..drain_count);
                }
            }
            Err(e) => {
                if is_tty {
                    eprint!("\r                    \r");
                }
                eprintln!("\n  \x1b[1;31mError:\x1b[0m {}\n", e);
                // Remove the failed user message
                conversation.pop();
            }
        }
    }

    if is_tty {
        eprintln!("\n  Goodbye!\n");
    }
    0
}

/// Orchestration mode — JSON-RPC 2.0 over stdin/stdout
async fn cmd_agent_orchestrate(cfg: &AgentConfig) -> i32 {
    use ftp_client_gui_lib::ai::ChatMessage;
    use std::io::BufRead;

    let mut conversation: Vec<ChatMessage> = Vec::new();

    // Emit ready notification with actual CLI tool count
    let cli_tools = cli_tool_definitions();
    let cli_tool_count = cli_tools.len();
    println!("{}", serde_json::json!({
        "jsonrpc": "2.0",
        "method": "agent/ready",
        "params": {
            "version": env!("CARGO_PKG_VERSION"),
            "tools": cli_tool_count,
        }
    }));

    const ORCH_MAX_LINE_BYTES: usize = 1_048_576; // 1 MB
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.len() > ORCH_MAX_LINE_BYTES {
            println!("{}", serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32600, "message": "Line exceeds 1 MB limit" }
            }));
            continue;
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Parse JSON-RPC request
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                }));
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(serde_json::json!({}));

        match method {
            "agent/chat" | "agent.chat" => {
                let msg = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
                if msg.is_empty() {
                    println!("{}", serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32602, "message": "Missing 'message' parameter" }
                    }));
                    continue;
                }

                conversation.push(ChatMessage {
                    role: "user".to_string(),
                    content: msg.to_string(),
                    images: None,
                });

                // Emit thinking notification
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "stream/thinking",
                    "params": { "content": "Processing..." }
                }));

                match agent_tool_loop(cfg, &mut conversation, false).await {
                    Ok(response) => {
                        conversation.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: response.clone(),
                            images: None,
                        });
                        // Sliding window: keep last 40 messages
                        if conversation.len() > 40 {
                            conversation.drain(..conversation.len() - 40);
                        }
                        println!("{}", serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": {
                                "status": "ok",
                                "response": response,
                                "messages": conversation.len(),
                            }
                        }));
                    }
                    Err(e) => {
                        conversation.pop(); // Remove failed user message
                        println!("{}", serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32000, "message": e }
                        }));
                    }
                }
            }

            "session/status" | "agent.status" => {
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "status": "ok",
                        "messages": conversation.len(),
                        "tools": cli_tool_count,
                    }
                }));
            }

            "session/clear" | "agent.clear" => {
                conversation.clear();
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "status": "ok" }
                }));
            }

            "session/close" | "agent.close" => {
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "status": "ok" }
                }));
                break;
            }

            "tool/list" | "agent.tools" => {
                // Expose only tools actually implemented in CLI executor
                let tool_entries: Vec<serde_json::Value> = cli_tools.iter().map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "danger": match tool_danger_level(&t.name) {
                            0 => "safe",
                            1 => "medium",
                            _ => "high",
                        }
                    })
                }).collect();
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": { "tools": tool_entries }
                }));
            }

            _ => {
                println!("{}", serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("Method not found: {}", method) }
                }));
            }
        }
    }

    0
}

async fn cmd_agent_mcp(_provider_name: &str, _cli: &Cli) -> i32 {
    // Delegate to the new MCP server module (async stdio, connection pooling,
    // curated tool set, rate limiting, audit logging).
    let config = ftp_client_gui_lib::mcp::McpConfig::default();
    let server = ftp_client_gui_lib::mcp::McpServer::new(config);
    server.run().await
}

// ── Main ───────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Reset SIGPIPE to default behavior (exit silently on broken pipe)
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Show banner when help is displayed (no args, --help, or -h)
    let raw_args: Vec<String> = std::env::args().collect();
    let show_help = raw_args.len() <= 1
        || raw_args.iter().any(|a| a == "--help" || a == "-h" || a == "help");
    if show_help {
        if use_color() {
            eprintln!("\x1b[38;2;0;255;255m  ___    __________  ____  __________ ______  ______\x1b[0m");
            eprintln!("\x1b[38;2;0;220;255m /   |  / ____/ __ \\/ __ \\/ ____/ __ /_  __/ / ____/\x1b[0m");
            eprintln!("\x1b[38;2;80;180;255m/ /| | / __/ / /_/ / / / / /_  / /_/ / / /   / /    \x1b[0m");
            eprintln!("\x1b[38;2;180;120;255m/ ___ |/ /___/ _, _/ /_/ / __/ / ____/ / /   / /___  \x1b[0m");
            eprintln!("\x1b[38;2;255;80;220m/_/  |_/_____/_/ |_|\\____/_/   /_/     /_/    \\____/  \x1b[0m");
            eprintln!(
                "\x1b[38;2;120;255;180m  v{}  |  23 protocols  |  pget  |  mcp  |  ai agent  |  vault profiles\x1b[0m",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("\x1b[38;2;170;170;190m  transfer engine for operators, shell users, and terminal obsessives\x1b[0m");
        } else {
            eprintln!();
            eprintln!("  ___    __________  ____  __________ ______  ______");
            eprintln!(" /   |  / ____/ __ \\/ __ \\/ ____/ __ /_  __/ / ____/");
            eprintln!("/ /| | / __/ / /_/ / / / / /_  / /_/ / / /   / /    ");
            eprintln!("/ ___ |/ /___/ _, _/ /_/ / __/ / ____/ / /   / /___  ");
            eprintln!("/_/  |_/_____/_/ |_|\\____/_/   /_/     /_/    \\____/  ");
            eprintln!(
                "  v{}  |  23 protocols  |  pget  |  mcp  |  ai agent  |  vault profiles",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("  transfer engine for operators, shell users, and terminal obsessives");
            eprintln!();
        }
    }

    let args = match prepare_cli_args(raw_args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(5);
        }
    };

    let cli = Cli::parse_from(args);
    let format = cli.output_format();

    // Setup tracing based on verbosity
    if cli.verbose >= 2 {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_target(false)
            .init();
    } else if cli.verbose == 1 {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_target(false)
            .init();
    }

    // Setup Ctrl+C handler (double Ctrl+C forces immediate exit with code 130)
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let _ = ctrlc::set_handler(move || {
        if cancelled_clone.load(Ordering::Relaxed) {
            // Second Ctrl+C — force exit immediately
            std::process::exit(130);
        }
        eprintln!("\nInterrupted (Ctrl+C) — press again to force quit");
        cancelled_clone.store(true, Ordering::Relaxed);
    });

    maybe_check_for_updates(&cli).await;

    let exit_code = match &cli.command {
        Commands::Connect { url } => cmd_connect(url, &cli, format).await,
        // Profile-aware positional shift: when --profile is set, the "url" positional
        // is actually the first real argument (path, remote, etc.). We detect this by
        // checking if url doesn't look like a URL (no "://") and shift args accordingly.
        Commands::Ls {
            url,
            path,
            long,
            sort,
            reverse,
            all,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_ls(u, p, *long, sort, *reverse, *all, &cli, format).await
        }
        Commands::Get {
            url,
            remote,
            local,
            recursive,
            segments,
        } => {
            let (u, r, l) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), Some(remote.as_str()))
            } else {
                (url.as_str(), remote.as_str(), local.as_deref())
            };
            cmd_get(u, r, l, *recursive, *segments, &cli, format, cancelled).await
        }
        Commands::Put {
            url,
            local,
            remote,
            recursive,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), Some(local.as_str()))
            } else {
                (url.as_str(), local.as_str(), remote.as_deref())
            };
            cmd_put(u, l, r, *recursive, &cli, format, cancelled).await
        }
        Commands::Mkdir { url, path } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_mkdir(u, p, &cli, format).await
        }
        Commands::Rm {
            url,
            path,
            recursive,
            force,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_rm(u, p, *recursive, *force, &cli, format).await
        }
        Commands::Mv { url, from, to } => {
            let (u, f, t) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), from.as_str())
            } else {
                (url.as_str(), from.as_str(), to.as_str())
            };
            cmd_mv(u, f, t, &cli, format).await
        }
        Commands::Cp { url, from, to } => {
            let (u, f, t) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), from.as_str())
            } else {
                (url.as_str(), from.as_str(), to.as_str())
            };
            cmd_cp(u, f, t, &cli, format).await
        }
        Commands::Link { url, path, expires, password, permissions } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_link(u, p, expires.as_deref(), password.as_deref(), permissions.as_str(), &cli, format).await
        }
        Commands::Edit {
            url,
            path,
            find,
            replace,
            first,
        } => {
            let (u, p, f, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), path.as_str(), find.as_str())
            } else {
                (url.as_str(), path.as_str(), find.as_str(), replace.as_str())
            };
            cmd_edit(u, p, f, r, !first, &cli, format).await
        }
        Commands::Cat { url, path } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_cat(u, p, &cli, format).await
        }
        Commands::Rcat { url, remote } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), remote.as_str())
            };
            cmd_rcat(u, p, &cli, format).await
        }
        Commands::Serve { command } => match command {
            ServeCommands::Http { url, path, addr } => {
                let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                    ("_", url.as_str())
                } else {
                    (url.as_str(), path.as_str())
                };
                cmd_serve_http(u, p, addr, &cli, format).await
            }
            ServeCommands::WebDav { url, path, addr } => {
                let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                    ("_", url.as_str())
                } else {
                    (url.as_str(), path.as_str())
                };
                cmd_serve_webdav(u, p, addr, &cli, format).await
            }
        },
        Commands::Head { url, path, lines } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_head(u, p, *lines, &cli, format).await
        }
        Commands::Tail { url, path, lines } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_tail(u, p, *lines, &cli, format).await
        }
        Commands::Touch { url, path, timestamp } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_touch(u, p, timestamp.as_deref(), &cli, format).await
        }
        Commands::Hashsum {
            algorithm,
            url,
            path,
            download: _,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_hashsum(*algorithm, u, p, &cli, format).await
        }
        Commands::Check {
            url,
            local,
            remote,
            checksum,
            one_way,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), local.as_str())
            } else {
                (url.as_str(), local.as_str(), remote.as_str())
            };
            cmd_check(u, l, r, *checksum, *one_way, &cli, format).await
        }
        Commands::Stat { url, path } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_stat(u, p, &cli, format).await
        }
        Commands::Find {
            url,
            path,
            pattern,
        } => {
            let (u, p, pat) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), path.as_str())
            } else {
                (url.as_str(), path.as_str(), pattern.as_str())
            };
            cmd_find(u, p, pat, &cli, format).await
        }
        Commands::Df { url } => cmd_df(url, &cli, format).await,
        Commands::Tree {
            url,
            path,
            max_depth,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_tree(u, p, *max_depth, &cli, format).await
        }
        Commands::Sync {
            url,
            local,
            remote,
            direction,
            dry_run,
            delete,
            exclude,
            track_renames,
            max_delete,
            backup_dir,
            backup_suffix,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), local.as_str())
            } else {
                (url.as_str(), local.as_str(), remote.as_str())
            };
            cmd_sync(
                u, l, r, direction, *dry_run, *delete, exclude,
                *track_renames, max_delete.as_deref(), backup_dir.as_deref(), backup_suffix,
                &cli, format, cancelled,
            )
            .await
        }
        Commands::About { url } => {
            let u = if cli.profile.is_some() && !url.contains("://") && url != "_" { "_" } else { url };
            cmd_about(u, &cli, format).await
        }
        Commands::Speed {
            url,
            test_size,
            iterations,
            remote_path,
        } => {
            let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                "_"
            } else {
                url.as_str()
            };
            cmd_speed(u, test_size, *iterations, remote_path.as_deref(), &cli, format).await
        }
        Commands::Dedupe { url, path, mode, dry_run } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_dedupe(u, p, mode, *dry_run, &cli, format).await
        }
        Commands::Completions { shell } => {
            match std::panic::catch_unwind(|| {
                let mut cmd = Cli::command();
                clap_complete::generate(*shell, &mut cmd, "aeroftp", &mut std::io::stdout());
            }) {
                Ok(()) => 0,
                Err(_) => {
                    print_error(
                        format,
                        "Shell completion generation failed because the current CLI positional layout is incompatible with clap completion metadata.",
                        7,
                    );
                    7
                }
            }
        }
        Commands::Profiles => list_vault_profiles(&cli, format),
        Commands::AiModels => list_ai_models(&cli, format),
        Commands::AgentInfo => cmd_agent_info(&cli),
        Commands::Batch { file } => cmd_batch(file, &cli, format, cancelled).await,
        Commands::Alias { command } => cmd_alias(command, format),
        Commands::Agent {
            message,
            provider,
            model,
            connect,
            auto_approve,
            max_steps,
            orchestrate,
            mcp,
            stdin,
            yes,
            plan_only,
            cost_limit,
            system,
        } => {
            cmd_agent(
                message.clone(),
                provider.clone(),
                model.clone(),
                connect.clone(),
                if *yes { "all".to_string() } else { auto_approve.clone() },
                *max_steps,
                *orchestrate,
                *mcp,
                *stdin,
                *plan_only,
                *cost_limit,
                system.clone(),
                &cli,
                format,
                cancelled,
            )
            .await
        }
    };

    std::process::exit(exit_code);
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_cli() -> Cli {
        Cli {
            format: OutputFormat::Text,
            json: false,
            json_fields: None,
            password_stdin: false,
            key: None,
            key_passphrase: None,
            bucket: None,
            region: None,
            container: None,
            token: None,
            tls: None,
            insecure: false,
            trust_host_key: false,
            two_factor: None,
            profile: None,
            master_password: None,
            verbose: 0,
            quiet: false,
            limit_rate: None,
            bwlimit: None,
            parallel: 4,
            partial: false,
            include: Vec::new(),
            exclude_global: Vec::new(),
            include_from: None,
            exclude_from: None,
            min_size: None,
            max_size: None,
            min_age: None,
            max_age: None,
            command: Commands::Profiles,
        }
    }

    #[test]
    fn test_parse_speed_limit_megabytes() {
        assert_eq!(parse_speed_limit("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_speed_limit("10M").unwrap(), 10 * 1024 * 1024);
    }

    #[test]
    fn test_parse_speed_limit_kilobytes() {
        assert_eq!(parse_speed_limit("500K").unwrap(), 500 * 1024);
        assert_eq!(parse_speed_limit("1K").unwrap(), 1024);
    }

    #[test]
    fn test_parse_speed_limit_bytes() {
        assert_eq!(parse_speed_limit("1024").unwrap(), 1024);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1048576), "1.0 MB");
        assert_eq!(format_size(1073741824), "1.0 GB");
    }

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(512), "512 B/s");
        assert_eq!(format_speed(1024), "1.0 KB/s");
        assert_eq!(format_speed(1048576), "1.0 MB/s");
    }

    #[test]
    fn test_provider_error_exit_codes() {
        assert_eq!(
            provider_error_to_exit_code(&ProviderError::ConnectionFailed("test".into())),
            1
        );
        assert_eq!(
            provider_error_to_exit_code(&ProviderError::NotFound("test".into())),
            2
        );
        assert_eq!(
            provider_error_to_exit_code(&ProviderError::PermissionDenied("test".into())),
            3
        );
        assert_eq!(
            provider_error_to_exit_code(&ProviderError::TransferFailed("test".into())),
            4
        );
        assert_eq!(
            provider_error_to_exit_code(&ProviderError::AuthenticationFailed("test".into())),
            6
        );
        assert_eq!(
            provider_error_to_exit_code(&ProviderError::NotSupported("test".into())),
            7
        );
        assert_eq!(provider_error_to_exit_code(&ProviderError::Timeout), 8);
    }

    #[test]
    fn test_url_parsing_ftp() {
        let cli = test_cli();
        let (config, path) = url_to_provider_config("ftp://anonymous:test@ftp.example.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Ftp);
        assert_eq!(config.host, "ftp.example.com");
        assert_eq!(config.username.as_deref(), Some("anonymous"));
        assert_eq!(path, "/");
    }

    #[test]
    fn test_url_parsing_sftp_with_port() {
        let cli = test_cli();
        let (config, path) = url_to_provider_config("sftp://admin:test@server.com:2222/home", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Sftp);
        assert_eq!(config.host, "server.com");
        assert_eq!(config.port, Some(2222));
        assert_eq!(config.username.as_deref(), Some("admin"));
        assert_eq!(path, "/home");
    }

    #[test]
    fn test_url_parsing_webdavs() {
        let cli = test_cli();
        let (config, _path) = url_to_provider_config("webdavs://user:test@cloud.example.com/dav", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::WebDav);
        assert!(config.host.starts_with("https://"));
    }

    #[test]
    fn test_url_parsing_s3() {
        let mut cli = test_cli();
        cli.bucket = Some("mybucket".to_string());
        cli.region = Some("eu-west-1".to_string());
        let (config, _path) = url_to_provider_config("s3://AKID:secret@s3.amazonaws.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::S3);
        assert_eq!(config.extra.get("bucket").map(|s| s.as_str()), Some("mybucket"));
        assert_eq!(config.extra.get("region").map(|s| s.as_str()), Some("eu-west-1"));
    }

    #[test]
    fn test_insert_profile_option_normalizes_tencent_path_style() {
        let mut extra = HashMap::new();

        insert_profile_option(&mut extra, "pathStyle", &json!(false));
        insert_profile_option(&mut extra, "endpoint", &json!("https://cos.ap-guangzhou.myqcloud.com"));
        insert_profile_option(&mut extra, "bucket", &json!("mybucket-1250000000"));
        insert_profile_option(&mut extra, "region", &json!("ap-guangzhou"));

        assert_eq!(extra.get("path_style").map(|s| s.as_str()), Some("false"));
        assert_eq!(extra.get("endpoint").map(|s| s.as_str()), Some("https://cos.ap-guangzhou.myqcloud.com"));
        assert_eq!(extra.get("bucket").map(|s| s.as_str()), Some("mybucket-1250000000"));
        assert_eq!(extra.get("region").map(|s| s.as_str()), Some("ap-guangzhou"));
    }

    #[test]
    fn test_insert_profile_option_normalizes_common_camel_case_keys() {
        let mut extra = HashMap::new();

        insert_profile_option(&mut extra, "verifyCert", &json!(false));
        insert_profile_option(&mut extra, "tlsMode", &json!("implicit"));
        insert_profile_option(&mut extra, "sasToken", &json!("sig"));
        insert_profile_option(&mut extra, "accountName", &json!("acct"));

        assert_eq!(extra.get("verify_cert").map(|s| s.as_str()), Some("false"));
        assert_eq!(extra.get("tls_mode").map(|s| s.as_str()), Some("implicit"));
        assert_eq!(extra.get("sas_token").map(|s| s.as_str()), Some("sig"));
        assert_eq!(extra.get("account_name").map(|s| s.as_str()), Some("acct"));
    }

    #[test]
    fn test_display_port_for_provider_parses_server_info() {
        let port = display_port_for_provider(&ProviderType::Ftp, Some("FTP Server: ftp.axpdev.it:21"));
        assert_eq!(port, 21);
    }

    #[test]
    fn test_display_port_for_provider_falls_back_by_protocol() {
        assert_eq!(display_port_for_provider(&ProviderType::Sftp, None), 22);
        assert_eq!(display_port_for_provider(&ProviderType::Mega, None), 443);
    }

    #[test]
    fn test_url_parsing_unsupported() {
        let cli = test_cli();
        assert!(url_to_provider_config("gopher://host", &cli).is_err());
    }

    #[test]
    fn test_url_parsing_mega() {
        let cli = test_cli();
        let (config, _) = url_to_provider_config("mega://user:test@mega.nz", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Mega);
    }

    #[test]
    fn test_url_parsing_koofr() {
        let cli = test_cli();
        let (config, _) = url_to_provider_config("koofr://user:test@koofr.net", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Koofr);
        assert_eq!(config.host, "app.koofr.net");
    }

    #[test]
    fn test_url_parsing_opendrive() {
        let cli = test_cli();
        let (config, _) = url_to_provider_config("opendrive://user:test@dev.opendrive.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::OpenDrive);
        assert_eq!(config.host, "dev.opendrive.com");
    }

    #[test]
    fn test_url_parsing_github_repo() {
        let cli = test_cli();
        let (config, path) = url_to_provider_config(
            "github://token:secret@axpdev-lab/aeroftp-test-playground",
            &cli,
        )
        .unwrap();
        assert_eq!(config.provider_type, ProviderType::GitHub);
        assert_eq!(config.host, "axpdev-lab/aeroftp-test-playground");
        assert_eq!(config.extra.get("branch"), None);
        assert_eq!(path, "/");
    }

    #[test]
    fn test_url_parsing_github_branch_suffix() {
        let cli = test_cli();
        let (config, _) = url_to_provider_config(
            "github://token:secret@axpdev-lab/aeroftp-test-playground@main",
            &cli,
        )
        .unwrap();
        assert_eq!(config.host, "axpdev-lab/aeroftp-test-playground");
        assert_eq!(config.extra.get("branch").map(|s| s.as_str()), Some("main"));
    }

    #[test]
    fn test_url_parsing_github_token_placeholder() {
        let cli = test_cli();
        let (config, _) = url_to_provider_config(
            "github://token:secret@axpdev-lab/aeroftp-test-playground",
            &cli,
        )
        .unwrap();
        assert_eq!(config.provider_type, ProviderType::GitHub);
        assert_eq!(config.host, "axpdev-lab/aeroftp-test-playground");
    }

    // ── validate_relative_path tests ──────────────────────────────────

    #[test]
    fn test_validate_relative_path_normal() {
        assert_eq!(validate_relative_path("file.txt"), Some("file.txt"));
        assert_eq!(validate_relative_path("dir/file.txt"), Some("dir/file.txt"));
        assert_eq!(validate_relative_path("/dir/file.txt"), Some("dir/file.txt"));
    }

    #[test]
    fn test_validate_relative_path_traversal() {
        assert_eq!(validate_relative_path("../etc/passwd"), None);
        assert_eq!(validate_relative_path("dir/../../etc/passwd"), None);
        assert_eq!(validate_relative_path("..\\windows\\system32"), None);
    }

    #[test]
    fn test_validate_relative_path_null_bytes() {
        assert_eq!(validate_relative_path("file\0.txt"), None);
    }

    #[test]
    fn test_validate_relative_path_windows_drive() {
        assert_eq!(validate_relative_path("C:\\Windows"), None);
        assert_eq!(validate_relative_path("\\\\server\\share"), None);
    }

    #[test]
    fn test_validate_relative_path_safe_dots() {
        // Single dot and dotfiles should be fine
        assert_eq!(validate_relative_path(".hidden"), Some(".hidden"));
        assert_eq!(validate_relative_path("dir/.file"), Some("dir/.file"));
        assert_eq!(validate_relative_path("..."), Some("..."));
    }

    // ── parse_speed_limit edge case tests ─────────────────────────────

    #[test]
    fn test_parse_speed_limit_invalid() {
        assert!(parse_speed_limit("abc").is_err());
        assert!(parse_speed_limit("").is_err());
        assert!(parse_speed_limit("0M").is_ok()); // 0 is valid (no limit)
    }

    #[test]
    fn test_parse_speed_limit_case_insensitive() {
        assert_eq!(parse_speed_limit("1m").unwrap(), 1024 * 1024);
        assert_eq!(parse_speed_limit("500k").unwrap(), 500 * 1024);
    }

    // ── sanitize_filename tests ───────────────────────────────────────

    #[test]
    fn test_sanitize_filename_normal() {
        assert_eq!(sanitize_filename("normal.txt"), "normal.txt");
        assert_eq!(sanitize_filename("file with spaces.doc"), "file with spaces.doc");
    }

    #[test]
    fn test_sanitize_filename_ansi_escape() {
        assert_eq!(sanitize_filename("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(sanitize_filename("before\x1b[1;32mgreen\x1b[0mafter"), "beforegreenafter");
    }

    #[test]
    fn test_sanitize_filename_control_chars() {
        assert_eq!(sanitize_filename("file\x07name"), "filename");
        assert_eq!(sanitize_filename("file\ttab"), "file\ttab"); // tab preserved
    }

    #[test]
    fn test_normalize_release_version_accepts_prefixed_tags() {
        assert_eq!(
            normalize_release_version("v3.3.5").map(|version| version.to_string()),
            Some("3.3.5".to_string())
        );
        assert_eq!(
            normalize_release_version("3.3.5").map(|version| version.to_string()),
            Some("3.3.5".to_string())
        );
    }

    #[test]
    fn test_is_newer_release_uses_semver_ordering() {
        assert!(is_newer_release("v3.3.5", "3.3.4"));
        assert!(!is_newer_release("3.3.4", "3.3.4"));
        assert!(!is_newer_release("invalid", "3.3.4"));
    }

    #[test]
    fn test_update_check_due_after_24_hours() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-03T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let due_cache = CliUpdateCache {
            checked_at: Some("2026-04-02T10:59:59Z".to_string()),
            latest_version: Some("3.3.5".to_string()),
        };
        let fresh_cache = CliUpdateCache {
            checked_at: Some("2026-04-02T12:00:00Z".to_string()),
            latest_version: Some("3.3.5".to_string()),
        };

        assert!(update_check_due(&due_cache, now));
        assert!(!update_check_due(&fresh_cache, now));
    }

    #[test]
    fn test_update_check_due_on_invalid_timestamp() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-03T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let cache = CliUpdateCache {
            checked_at: Some("not-a-timestamp".to_string()),
            latest_version: None,
        };

        assert!(update_check_due(&cache, now));
    }

    // ── pget chunk planning tests ──────────────────────────────────

    #[test]
    fn test_pget_plan_basic() {
        let chunks = plan_pget_chunks(100 * 1024 * 1024, 4); // 100 MB, 4 segments
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].length, 25 * 1024 * 1024);
        assert_eq!(chunks[1].offset, 25 * 1024 * 1024);
        assert_eq!(chunks[3].offset, 75 * 1024 * 1024);
        // Sum of all lengths equals file size
        let total: u64 = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, 100 * 1024 * 1024);
    }

    #[test]
    fn test_pget_plan_uneven_division() {
        let chunks = plan_pget_chunks(10_000_003, 4); // not evenly divisible
        assert_eq!(chunks.len(), 4);
        let total: u64 = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, 10_000_003);
        // Offsets are contiguous
        for i in 1..chunks.len() {
            assert_eq!(chunks[i].offset, chunks[i - 1].offset + chunks[i - 1].length);
        }
    }

    #[test]
    fn test_pget_plan_reduces_segments_for_small_files() {
        // 3 MB file with 16 segments requested — each chunk would be < PGET_MIN_CHUNK_SIZE (1 MB)
        let chunks = plan_pget_chunks(3 * 1024 * 1024, 16);
        assert_eq!(chunks.len(), 3); // reduced to 3 segments
        let total: u64 = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, 3 * 1024 * 1024);
    }

    #[test]
    fn test_pget_plan_single_segment() {
        let chunks = plan_pget_chunks(500_000, 1);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].length, 500_000);
    }

    #[test]
    fn test_pget_plan_zero_size() {
        let chunks = plan_pget_chunks(0, 4);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_pget_plan_clamps_segments() {
        let chunks = plan_pget_chunks(100 * 1024 * 1024, 100); // 100 > max 16
        assert!(chunks.len() <= 16);
        let total: u64 = chunks.iter().map(|c| c.length).sum();
        assert_eq!(total, 100 * 1024 * 1024);
    }

    #[test]
    fn test_pget_plan_tiny_file() {
        // File smaller than PGET_MIN_CHUNK_SIZE — should get 1 segment
        let chunks = plan_pget_chunks(500_000, 8);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].length, 500_000);
    }

    #[tokio::test]
    async fn test_pget_assemble_chunks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_path = temp_dir.path().to_str().unwrap();

        // Write 3 chunk files
        tokio::fs::write(format!("{}/chunk_0000", temp_path), b"Hello, ").await.unwrap();
        tokio::fs::write(format!("{}/chunk_0001", temp_path), b"segmented ").await.unwrap();
        tokio::fs::write(format!("{}/chunk_0002", temp_path), b"world!").await.unwrap();

        let dest = temp_dir.path().join("assembled.bin");
        let dest_str = dest.to_str().unwrap();

        pget_assemble_chunks(temp_path, dest_str, 3).await.unwrap();

        let result = tokio::fs::read(dest_str).await.unwrap();
        assert_eq!(result, b"Hello, segmented world!");
    }

    #[tokio::test]
    async fn test_pget_assemble_binary_integrity() {
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_path = temp_dir.path().to_str().unwrap();

        // Write binary chunk data
        let chunk0: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        let chunk1: Vec<u8> = (1024..2048).map(|i| (i % 256) as u8).collect();
        tokio::fs::write(format!("{}/chunk_0000", temp_path), &chunk0).await.unwrap();
        tokio::fs::write(format!("{}/chunk_0001", temp_path), &chunk1).await.unwrap();

        let dest = temp_dir.path().join("assembled.bin");
        let dest_str = dest.to_str().unwrap();

        pget_assemble_chunks(temp_path, dest_str, 2).await.unwrap();

        let result = tokio::fs::read(dest_str).await.unwrap();
        let expected: Vec<u8> = (0..2048).map(|i| (i % 256) as u8).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_pget_temp_guard_cleanup() {
        let temp_dir = tempfile::tempdir().unwrap();
        let guard_path = temp_dir.path().join("pget-guard-test");
        std::fs::create_dir_all(&guard_path).unwrap();
        std::fs::write(guard_path.join("chunk_0000"), b"data").unwrap();

        let guard_str = guard_path.to_string_lossy().to_string();
        assert!(guard_path.exists());

        {
            let _guard = PgetTempGuard(guard_str);
            // guard goes out of scope here
        }

        assert!(!guard_path.exists(), "temp dir should be cleaned up by PgetTempGuard");
    }

    // ── serve http helper tests ──────────────────────────────────────

    #[test]
    fn test_normalize_remote_path() {
        assert_eq!(normalize_remote_path("/"), "/");
        assert_eq!(normalize_remote_path(""), "/");
        assert_eq!(normalize_remote_path("/foo/bar"), "/foo/bar");
        assert_eq!(normalize_remote_path("foo/bar/"), "/foo/bar");
        assert_eq!(normalize_remote_path("//foo///bar//"), "/foo/bar");
        assert_eq!(normalize_remote_path("/./foo/./bar"), "/foo/bar");
    }

    #[test]
    fn test_sanitize_served_relative_path_valid() {
        assert_eq!(sanitize_served_relative_path("foo/bar").unwrap(), "foo/bar");
        assert_eq!(sanitize_served_relative_path("/foo/bar").unwrap(), "foo/bar");
        assert_eq!(sanitize_served_relative_path("").unwrap(), "");
        assert_eq!(sanitize_served_relative_path("/").unwrap(), "");
        assert_eq!(sanitize_served_relative_path("./foo").unwrap(), "foo");
        assert_eq!(sanitize_served_relative_path("a%20b").unwrap(), "a b");
    }

    #[test]
    fn test_sanitize_served_relative_path_traversal() {
        assert!(sanitize_served_relative_path("..").is_err());
        assert!(sanitize_served_relative_path("../etc/passwd").is_err());
        assert!(sanitize_served_relative_path("foo/../../etc").is_err());
        assert!(sanitize_served_relative_path("foo%2F..%2F..%2Fetc").is_err());
    }

    #[test]
    fn test_sanitize_served_relative_path_null() {
        assert!(sanitize_served_relative_path("foo%00bar").is_err());
    }

    #[test]
    fn test_build_served_remote_path() {
        assert_eq!(build_served_remote_path("/data", ""), "/data");
        assert_eq!(build_served_remote_path("/data", "sub/file.txt"), "/data/sub/file.txt");
        assert_eq!(build_served_remote_path("/", "file.txt"), "/file.txt");
        assert_eq!(build_served_remote_path("/", ""), "/");
    }

    #[test]
    fn test_serve_effective_base_path() {
        assert_eq!(serve_effective_base_path("/", "/home/user"), "/home/user");
        assert_eq!(serve_effective_base_path("/custom", "/home/user"), "/custom");
        assert_eq!(serve_effective_base_path("/", "/"), "/");
    }

    #[test]
    fn test_encode_request_path() {
        assert_eq!(encode_request_path("/"), "/");
        assert_eq!(encode_request_path(""), "/");
        assert_eq!(encode_request_path("foo/bar"), "/foo/bar");
        assert_eq!(encode_request_path("a b/c d"), "/a%20b/c%20d");
        assert_eq!(encode_request_path("foo/bar/"), "/foo/bar/");
    }

    #[test]
    fn test_child_request_path() {
        assert_eq!(child_request_path("", "docs", true), "docs/");
        assert_eq!(child_request_path("", "file.txt", false), "file.txt");
        assert_eq!(child_request_path("sub", "file.txt", false), "sub/file.txt");
        assert_eq!(child_request_path("a/b", "c", true), "a/b/c/");
    }

    #[test]
    fn test_parent_request_path() {
        assert_eq!(parent_request_path(""), None);
        assert_eq!(parent_request_path("/"), None);
        assert_eq!(parent_request_path("foo"), Some("/".to_string()));
        assert_eq!(parent_request_path("foo/bar"), Some("/foo/".to_string()));
        assert_eq!(parent_request_path("a/b/c"), Some("/a/b/".to_string()));
    }

    #[test]
    fn test_escape_html_serve() {
        assert_eq!(escape_html("<script>alert('xss')</script>"), "&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;");
        assert_eq!(escape_html("a&b"), "a&amp;b");
        assert_eq!(escape_html("\"quoted\""), "&quot;quoted&quot;");
    }

    // ── range request tests ─────────────────────────────────────────

    #[test]
    fn test_parse_range_header_normal() {
        assert_eq!(parse_range_header("bytes=0-499", 1000), Some((0, 499)));
        assert_eq!(parse_range_header("bytes=500-999", 1000), Some((500, 999)));
    }

    #[test]
    fn test_parse_range_header_open_end() {
        assert_eq!(parse_range_header("bytes=500-", 1000), Some((500, 999)));
    }

    #[test]
    fn test_parse_range_header_suffix() {
        assert_eq!(parse_range_header("bytes=-200", 1000), Some((800, 999)));
    }

    #[test]
    fn test_parse_range_header_clamped() {
        // End beyond file size is clamped
        assert_eq!(parse_range_header("bytes=0-9999", 1000), Some((0, 999)));
    }

    #[test]
    fn test_parse_range_header_invalid() {
        assert_eq!(parse_range_header("bytes=999-0", 1000), None); // start > end
        assert_eq!(parse_range_header("bytes=1000-", 1000), None); // start >= size
        assert_eq!(parse_range_header("chars=0-10", 1000), None); // wrong prefix
        assert_eq!(parse_range_header("bytes=0-10", 0), None); // empty file
    }

    // ── webdav helper tests ─────────────────────────────────────────

    #[test]
    fn test_webdav_xml_entry_file() {
        let mut entry = RemoteEntry::file("test.txt".to_string(), "/test.txt".to_string(), 1234);
        entry.modified = Some("2026-04-03".to_string());
        entry.mime_type = Some("text/plain".to_string());
        let xml = webdav_xml_entry("/test.txt", &entry);
        assert!(xml.contains("<D:href>/test.txt</D:href>"));
        assert!(xml.contains("<D:resourcetype/>"));
        assert!(xml.contains("<D:getcontentlength>1234</D:getcontentlength>"));
        assert!(xml.contains("<D:displayname>test.txt</D:displayname>"));
        assert!(xml.contains("<D:getlastmodified>2026-04-03</D:getlastmodified>"));
    }

    #[test]
    fn test_webdav_xml_entry_directory() {
        let entry = RemoteEntry::directory("docs".to_string(), "/docs".to_string());
        let xml = webdav_xml_entry("/docs/", &entry);
        assert!(xml.contains("<D:collection/>"));
        assert!(xml.contains("<D:displayname>docs</D:displayname>"));
        assert!(!xml.contains("<D:getcontentlength>"));
    }

    #[test]
    fn test_build_propfind_xml_structure() {
        let root = RemoteEntry::directory("/".to_string(), "/".to_string());
        let children = vec![
            RemoteEntry::directory("sub".to_string(), "/sub".to_string()),
            RemoteEntry::file("file.txt".to_string(), "/file.txt".to_string(), 100),
        ];
        let xml = build_propfind_xml("/", "", &root, Some(&children));
        assert!(xml.contains("<D:multistatus"));
        assert!(xml.contains("</D:multistatus>"));
        // Root + 2 children = 3 responses
        assert_eq!(xml.matches("<D:response>").count(), 3);
    }

    #[test]
    fn test_extract_destination_relative() {
        let mut headers = HeaderMap::new();
        headers.insert("Destination", HeaderValue::from_static("http://127.0.0.1:8080/new/path"));
        assert_eq!(extract_destination_relative(&headers).unwrap(), "new/path");
    }

    #[test]
    fn test_extract_destination_relative_missing() {
        let headers = HeaderMap::new();
        assert!(extract_destination_relative(&headers).is_err());
    }

    #[test]
    fn test_extract_destination_relative_traversal() {
        let mut headers = HeaderMap::new();
        headers.insert("Destination", HeaderValue::from_static("http://host/../etc/passwd"));
        assert!(extract_destination_relative(&headers).is_err());
    }
}
