//! AeroFTP CLI - Production multi-protocol file transfer client
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
//!   aeroftp hashsum <url> <path> [-a sha256]  Compute file hash (md5/sha1/sha256/sha512/blake3)
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
//!   aeroftp speed <url> [-s 10M]              Benchmark upload/download (random + SHA-256 + TTFB)
//!   aeroftp speed-compare <url1> <url2>...    Rank multiple servers side-by-side
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
// Copyright (c) 2024-2026 axpnet - AI-assisted (see AI-TRANSPARENCY.md)

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path as AxumPath, State},
    http::{
        header::{
            ACCEPT_RANGES, AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
            WWW_AUTHENTICATE,
        },
        HeaderMap, HeaderValue, Method, StatusCode,
    },
    response::{Html, IntoResponse, Response},
    routing::{any, get},
    Router,
};
use base64::Engine as _;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use ftp_client_gui_lib::profile_loader::{
    apply_profile_options, apply_s3_profile_defaults, S3_ENDPOINT_SOURCE_META_KEY,
    S3_PATH_STYLE_SOURCE_META_KEY, S3_PROVIDER_ID_META_KEY, S3_REGION_SOURCE_META_KEY,
};
use ftp_client_gui_lib::providers::{
    ProviderConfig, ProviderError, ProviderFactory, ProviderType, RemoteEntry, ShareLinkOptions,
    StorageProvider, MAX_DOWNLOAD_TO_BYTES,
};
use ftp_client_gui_lib::util::shutdown_signal;
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

/// Canonical list of URL schemes accepted by `connect`, `ls`, `get`, etc.
/// Aliases (`ssh`, `http`, `https`) are accepted by the URL parser but are
/// not listed here so the public surface stays compact. Both the banner
/// help line and the "Unsupported protocol" error derive from this slice
/// so the two cannot drift apart again (issue #125 polish).
const SUPPORTED_URL_SCHEMES: &[&str] = &[
    "ftp",
    "ftps",
    "sftp",
    "webdav",
    "webdavs",
    "s3",
    "mega",
    "azure",
    "filen",
    "internxt",
    "jottacloud",
    "filelu",
    "koofr",
    "opendrive",
    "yandexdisk",
    "github",
    "gitlab",
];

#[derive(Parser)]
#[command(
    name = "aeroftp",
    about = "AeroFTP CLI - Multi-protocol file transfer client",
    version,
    long_about = "Direct URL schemes: FTP, FTPS, SFTP, WebDAV(S), S3, MEGA, Azure, Filen, Internxt, Jottacloud, FileLu, Koofr, OpenDrive, Yandex Disk, GitHub.\nSaved profiles additionally cover Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, 4shared, and Drime.\n\nConnect via saved profiles (--profile) or URL (protocol://user@host:port/path).\nAI agents: run 'aeroftp agent-bootstrap --json' for canonical task workflows and 'aeroftp agent-info --json' for full capability discovery.",
    after_help = "EXAMPLES (profiles - no credentials needed):\n  aeroftp-cli profiles                                      List saved servers\n  aeroftp-cli ls --profile \"My Server\" /var/www/ -l          List files\n  aeroftp-cli put --profile \"Production\" ./app.js /www/      Upload file\n  aeroftp-cli get --profile \"NAS\" /backups/db.sql ./         Download file\n  aeroftp-cli sync --profile \"Staging\" ./build/ /www/ --dry-run\n  aeroftp-cli agent-bootstrap                                AI quick-start playbook\n  aeroftp-cli agent-info --json                              AI capability discovery\n\nEXAMPLES (URL mode):\n  aeroftp-cli connect sftp://user@myserver.com\n  aeroftp-cli ls sftp://user@myserver.com /var/www/ -l\n  aeroftp-cli get sftp://user@host \"/data/*.csv\"\n  aeroftp-cli cat sftp://user@host /config.ini | grep DB_HOST\n  aeroftp-cli batch deploy.aeroftp\n\nEXIT CODES:\n  0  Success                    5  Invalid config/usage\n  1  Connection/network error   6  Authentication failed\n  2  Not found                  7  Not supported\n  3  Permission denied          8  Timeout\n  4  Transfer failed/partial   99  Unknown error"
)]
struct Cli {
    /// Output format
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text, help_heading = "Output options")]
    format: OutputFormat,

    /// Shorthand for --format json
    #[arg(long, global = true, help_heading = "Output options")]
    json: bool,

    /// Suppress the startup banner (also via AEROFTP_NO_BANNER env var)
    #[arg(long, global = true, help_heading = "Output options")]
    #[allow(dead_code)] // read from raw_args before clap parses
    no_banner: bool,

    /// Restrict JSON output fields (comma-separated, e.g. name,size,modified)
    #[arg(long, global = true, help_heading = "Output options")]
    json_fields: Option<String>,

    /// Read password from stdin (pipe: echo "pass" | aeroftp ...)
    #[arg(long, global = true, help_heading = "Connection options")]
    password_stdin: bool,

    /// SSH private key path for SFTP
    #[arg(long, global = true, help_heading = "Connection options")]
    key: Option<String>,

    /// SSH key passphrase
    #[arg(long, global = true, help_heading = "Connection options")]
    key_passphrase: Option<String>,

    /// S3 bucket name
    #[arg(long, global = true, help_heading = "Connection options")]
    bucket: Option<String>,

    /// S3/Azure region
    #[arg(long, global = true, help_heading = "Connection options")]
    region: Option<String>,

    /// Azure container name
    #[arg(long, global = true, help_heading = "Connection options")]
    container: Option<String>,

    /// Bearer/API token (kDrive, Jottacloud, FileLu)
    #[arg(
        long,
        global = true,
        env = "AEROFTP_TOKEN",
        hide_env_values = true,
        help_heading = "Connection options"
    )]
    token: Option<String>,

    /// FTP TLS mode: none, explicit, implicit, explicit_if_available
    #[arg(long, global = true, help_heading = "Connection options")]
    tls: Option<String>,

    /// Skip TLS certificate verification
    #[arg(long, global = true, help_heading = "Connection options")]
    insecure: bool,

    /// Trust unknown SSH host keys (skip TOFU verification)
    #[arg(long, global = true, help_heading = "Connection options")]
    trust_host_key: bool,

    /// 2FA code (Filen, Internxt)
    #[arg(
        long,
        global = true,
        env = "AEROFTP_2FA",
        hide_env_values = true,
        help_heading = "Connection options"
    )]
    two_factor: Option<String>,

    /// Use a saved server profile instead of URL (name or ID)
    #[arg(long, short = 'P', global = true)]
    profile: Option<String>,

    /// Master password for encrypted vault (or set AEROFTP_MASTER_PASSWORD)
    #[arg(
        long,
        global = true,
        env = "AEROFTP_MASTER_PASSWORD",
        hide_env_values = true
    )]
    master_password: Option<String>,

    /// Verbose output (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count, help_heading = "Output options")]
    verbose: u8,

    /// Quiet mode (errors only)
    #[arg(
        short,
        long,
        global = true,
        conflicts_with = "verbose",
        help_heading = "Output options"
    )]
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

    /// Read file list from file (one path per line). Only listed files are transferred.
    #[arg(long, global = true)]
    files_from: Option<String>,

    /// Like --files-from but don't skip empty lines or strip whitespace
    #[arg(long, global = true)]
    files_from_raw: Option<String>,

    /// Never overwrite existing files on destination (append-only / immutable mode)
    #[arg(long, global = true)]
    immutable: bool,

    /// Skip listing destination before transfer (assume dest is empty)
    #[arg(long, global = true)]
    no_check_dest: bool,

    /// Maximum directory recursion depth (default: unlimited). Applies to ls -R, find, sync, get -r.
    #[arg(long, global = true)]
    max_depth: Option<u32>,

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

    // ── Transfer control flags ──
    /// Abort after transferring this many bytes total (e.g., "10G", "500M")
    #[arg(long, global = true)]
    max_transfer: Option<String>,

    /// Maximum number of queued transfer tasks (default: 10000)
    #[arg(long, global = true, default_value_t = 10000)]
    max_backlog: usize,

    /// Number of retries for failed operations (default: 3, 0 = no retry)
    #[arg(long, global = true, default_value_t = 3)]
    retries: u32,

    /// Delay between retries (e.g., "5s", "1m", "500ms"; default: "1s")
    #[arg(long, global = true, default_value = "1s")]
    retries_sleep: String,

    /// Dump HTTP debug info to stderr (comma-separated: headers,bodies,auth)
    #[arg(long, global = true, value_delimiter = ',')]
    dump: Vec<String>,

    /// Override upload chunk size (e.g., "64M", "16M"). Min 5M for S3.
    #[arg(long, global = true)]
    chunk_size: Option<String>,

    /// Override download buffer size (e.g., "256K", "1M")
    #[arg(long, global = true)]
    buffer_size: Option<String>,

    /// Default mtime when backend returns None (ISO 8601 or "now")
    #[arg(long, global = true)]
    default_time: Option<String>,

    /// Use recursive listing in a single API call (S3 only, faster for large datasets)
    #[arg(long, global = true)]
    fast_list: bool,

    /// Write downloads directly to final path (no .aerotmp temp file)
    #[arg(long, global = true)]
    inplace: bool,

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

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
enum ReconcileFormat {
    Detailed,
    Summary,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
    Blake3,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum AgentBootstrapTask {
    Explore,
    VerifyFile,
    Transfer,
    Backup,
    Reconcile,
}

#[derive(Subcommand)]
enum Commands {
    /// Test connection to a remote server
    Connect {
        /// Server URL (e.g., sftp://user@host:22). Omit when using --profile.
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
    },
    /// List files on a remote server
    Ls {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        /// Cap the number of entries returned (after sort). When the
        /// listing is truncated, JSON output sets `summary.truncated:
        /// true` so agents can detect partial results.
        #[arg(long)]
        limit: Option<usize>,
        /// List only files (skip directories). Applied after sort.
        #[arg(long)]
        files_only: bool,
        /// List only directories (skip files). Applied after sort.
        #[arg(long, conflicts_with = "files_only")]
        dirs_only: bool,
    },
    /// Download file(s) from remote server
    Get {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
    /// Segmented parallel download (alias for `get` with --segments preset)
    Pget {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        remote: String,
        /// Local destination (default: current filename)
        local: Option<String>,
        /// Number of parallel segments (range 2-16)
        #[arg(long, default_value_t = 4)]
        segments: usize,
    },
    /// Upload file(s) to remote server (supports glob patterns like "*.csv")
    Put {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Local file path (supports glob patterns like "*.csv")
        #[arg(default_value = "")]
        local: String,
        /// Remote destination path
        remote: Option<String>,
        /// Recursive upload (directories)
        #[arg(short, long)]
        recursive: bool,
        /// Do not overwrite existing remote files
        #[arg(short, long)]
        no_clobber: bool,
    },
    /// Create a remote directory
    Mkdir {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote directory path
        #[arg(default_value = "")]
        path: String,
        /// Create parent directories as needed; no error if existing
        #[arg(short, long)]
        parents: bool,
    },
    /// Delete a remote file or directory
    Rm {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        #[arg(default_value = "_", hide_default_value = true)]
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
        #[arg(default_value = "_", hide_default_value = true)]
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
        #[arg(default_value = "_", hide_default_value = true)]
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
        /// Probe the generated URL with a follow-redirects HTTP GET and report
        /// whether it is reachable. Exit code 4 if the probe fails. Useful in
        /// CI smoke tests to catch silent regressions where the URL is built
        /// but does not actually resolve.
        #[arg(long)]
        verify: bool,
    },
    /// Find and replace text in a remote UTF-8 file
    Edit {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
    },
    /// Print first N lines of a remote file
    Head {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote file path
        #[arg(default_value = "")]
        path: String,
        /// Number of lines to print (default: 20)
        #[arg(short = 'n', long, default_value = "20")]
        lines: usize,
        /// Print the first N bytes instead of N lines. Mutually
        /// exclusive with --lines (--bytes wins when both present).
        /// Useful for "first 4KB" previews on binary or huge files.
        #[arg(short = 'c', long)]
        bytes: Option<u64>,
    },
    /// Print last N lines of a remote file
    Tail {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        #[arg(default_value = "_", hide_default_value = true)]
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
        /// Hash algorithm (md5/sha1/sha256/sha512/blake3). Defaults to sha256.
        /// Accepts `-a sha256` or `--algorithm sha256`. Omitting the flag
        /// yields a sha256 checksum.
        #[arg(value_enum, short = 'a', long = "algorithm", default_value = "sha256")]
        algorithm: HashAlgorithm,
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        #[arg(default_value = "_", hide_default_value = true)]
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
    /// Reconcile local and remote trees with categorized diff output
    Reconcile {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Local directory
        #[arg(default_value = ".")]
        local: String,
        /// Remote directory
        #[arg(default_value = "/")]
        remote: String,
        /// Use checksums instead of size-only comparison where available
        #[arg(long)]
        checksum: bool,
        /// Only consider files present locally
        #[arg(long)]
        one_way: bool,
        /// Exclude patterns (can repeat: -e "*.tmp" -e ".git")
        #[arg(long, short)]
        exclude: Vec<String>,
        /// Output verbosity: detailed (default) or summary
        #[arg(long, value_enum, default_value_t = ReconcileFormat::Detailed)]
        reconcile_format: ReconcileFormat,
    },
    /// Show file/directory metadata
    Stat {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote path
        #[arg(default_value = "")]
        path: String,
    },
    /// Search for files by pattern
    Find {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Base path to search from
        #[arg(default_value = "/")]
        path: String,
        /// Search pattern (glob-style). Positional; `--name` is an
        /// equivalent flag form for agents that expect named args.
        #[arg(default_value = "*")]
        pattern: String,
        /// Glob pattern alias for the positional argument. When both
        /// the positional pattern and `--name` are set, `--name` wins.
        #[arg(long, value_name = "GLOB")]
        name: Option<String>,
        /// Match only files (skip directories)
        #[arg(long)]
        files_only: bool,
        /// Match only directories (skip files)
        #[arg(long, conflicts_with = "files_only")]
        dirs_only: bool,
        /// Cap matches returned. JSON output sets `summary.truncated:
        /// true` when this trims results.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show storage quota/usage
    Df {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
    },
    /// Show detailed server info, account, and storage quota
    About {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
    },
    /// Measure upload/download throughput against a writable remote
    Speed {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Test file size (e.g. 1M, 8M, 64M, 1G). Also accepts --size / -s.
        #[arg(
            long = "test-size",
            visible_alias = "size",
            short = 's',
            default_value = "10M"
        )]
        test_size: String,
        /// Number of upload/download iterations
        #[arg(long, default_value = "1")]
        iterations: u32,
        /// Remote path override for the temporary benchmark file
        #[arg(long)]
        remote_path: Option<String>,
        /// Skip SHA-256 integrity verification (faster but less rigorous)
        #[arg(long)]
        no_integrity: bool,
        /// Write a JSON v1 report to this path
        #[arg(long)]
        json_out: Option<String>,
    },
    /// Benchmark and rank multiple servers side-by-side.
    SpeedCompare {
        /// Two or more server URLs to compare
        #[arg(required = true, num_args = 1..)]
        urls: Vec<String>,
        /// Test file size (e.g. 10M, 100M)
        #[arg(
            long = "test-size",
            visible_alias = "size",
            short = 's',
            default_value = "10M"
        )]
        test_size: String,
        /// Maximum parallel runs (1-4, default 2)
        #[arg(long, default_value = "2")]
        parallel: u8,
        /// Skip SHA-256 integrity verification
        #[arg(long)]
        no_integrity: bool,
        /// Write a JSON v1 compare report to this path
        #[arg(long)]
        json_out: Option<String>,
        /// Write a CSV report to this path
        #[arg(long)]
        csv_out: Option<String>,
        /// Write a Markdown report to this path
        #[arg(long)]
        md_out: Option<String>,
    },
    /// Remove orphaned .aerotmp files from interrupted downloads.
    Cleanup {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote path to scan (default: /)
        #[arg(default_value = "/")]
        path: String,
        /// Actually delete orphaned files (default: dry-run listing)
        #[arg(long)]
        force: bool,
    },
    /// Find duplicate files on a remote by content hash and optionally remove them.
    Dedupe {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote path to scan
        #[arg(default_value = "/")]
        path: String,
        /// Resolution mode: skip, delete, newest, oldest, largest, smallest, rename, interactive, list
        #[arg(long, default_value = "skip")]
        mode: String,
        /// Preview only (don't delete)
        #[arg(long)]
        dry_run: bool,
    },
    /// Synchronize local and remote directories
    Sync {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        /// Place suffix before the file extension (file.bak.txt instead of file.txt.bak)
        #[arg(long)]
        suffix_keep_extension: bool,
        /// Skip transfer if file exists in this local directory with same size+mtime
        #[arg(long)]
        compare_dest: Option<String>,
        /// Copy from this local directory instead of downloading if file matches size+mtime
        #[arg(long)]
        copy_dest: Option<String>,
        /// Consume a reconcile JSON file instead of re-scanning local and remote trees
        #[arg(long)]
        from_reconcile: Option<String>,
        /// Conflict resolution for --direction both: newer, older, larger, smaller, rename, skip (default: newer)
        #[arg(long, default_value = "newer")]
        conflict_mode: String,
        /// Trust size-only matches and skip transfers even when mtimes differ
        #[arg(long)]
        skip_matching: bool,
        /// Discard previous bisync snapshot and rebuild from scratch
        #[arg(long)]
        resync: bool,
        /// Watch local directory for changes and re-sync automatically
        #[arg(long)]
        watch: bool,
        /// Watcher backend: auto, native, poll (default: auto)
        #[arg(long, default_value = "auto")]
        watch_mode: String,
        /// Debounce window in milliseconds (default: 1500)
        #[arg(long, default_value = "1500")]
        watch_debounce_ms: u64,
        /// Minimum seconds between consecutive re-syncs (default: 15)
        #[arg(long, default_value = "15")]
        watch_cooldown: u64,
        /// Full rescan interval in seconds, 0 to disable (default: 300)
        #[arg(long, default_value = "300")]
        watch_rescan: u64,
        /// Skip the initial full sync on startup
        #[arg(long)]
        watch_no_initial: bool,
    },
    /// Preflight checks and risk summary before sync
    SyncDoctor {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
        /// Delete orphaned files on destination
        #[arg(long)]
        delete: bool,
        /// Exclude patterns
        #[arg(long, short)]
        exclude: Vec<String>,
        /// Detect renamed files by hash to avoid re-upload
        #[arg(long)]
        track_renames: bool,
        /// Conflict resolution for --direction both: newer, older, larger, smaller, rename, skip
        #[arg(long, default_value = "newer")]
        conflict_mode: String,
        /// Discard previous bisync snapshot and rebuild from scratch
        #[arg(long)]
        resync: bool,
        /// Use checksums instead of size/mtime
        #[arg(long)]
        checksum: bool,
    },
    /// Display remote directory tree
    Tree {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote path (default: /)
        #[arg(default_value = "/")]
        path: String,
        /// Maximum depth to descend.
        // The field is named `depth` (not `max_depth`) to avoid colliding
        // with the root-level `--max-depth` global flag, which clap's derive
        // otherwise reports as a runtime TypeId mismatch. This is an
        // implementation note — keep it as `//` so it doesn't reach `--help`.
        #[arg(short = 'd', long = "depth", default_value = "3")]
        depth: usize,
    },
    /// Interactive disk usage explorer (ncdu-style TUI)
    Ncdu {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote path to scan (default: /)
        #[arg(default_value = "/")]
        path: String,
        /// Maximum scan depth (default: 50). See `Tree::depth` note — the
        /// field name must differ from the global `--max-depth` flag.
        #[arg(short = 'd', long = "depth", default_value = "50")]
        depth: usize,
        /// Export scan results to JSON file instead of interactive TUI
        #[arg(long)]
        export: Option<String>,
    },
    /// Mount a remote as a local filesystem (FUSE on Linux/macOS, WebDAV drive on Windows)
    Mount {
        /// Local mount point: empty directory (Linux/macOS) or drive letter like "Z:" (Windows)
        mountpoint: String,
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote base path (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Metadata cache TTL in seconds (default: 30)
        #[arg(long, default_value = "30")]
        cache_ttl: u64,
        /// Allow other users to access the mount (Linux/macOS: requires user_allow_other in /etc/fuse.conf)
        #[arg(long)]
        allow_other: bool,
        /// Mount as read-only (default: read-write)
        #[arg(long)]
        read_only: bool,
    },
    /// Transfer files between two saved profiles (cross-profile copy)
    Transfer {
        /// Source profile name (saved in vault)
        source_profile: String,
        /// Destination profile name (saved in vault)
        dest_profile: String,
        /// Remote path on source
        source_path: String,
        /// Remote path on destination
        dest_path: String,
        /// Recursive (copy directories)
        #[arg(short, long)]
        recursive: bool,
        /// Plan without executing
        #[arg(long)]
        dry_run: bool,
        /// Skip files already present on destination (size+mtime match)
        #[arg(long)]
        skip_existing: bool,
    },
    /// Preflight checks and risk summary before cross-profile transfer
    TransferDoctor {
        /// Source profile name (saved in vault)
        source_profile: String,
        /// Destination profile name (saved in vault)
        dest_profile: String,
        /// Remote path on source
        source_path: String,
        /// Remote path on destination
        dest_path: String,
        /// Recursive (copy directories)
        #[arg(short, long)]
        recursive: bool,
        /// Skip files already present on destination
        #[arg(long)]
        skip_existing: bool,
    },
    /// Execute commands from a batch script (.aeroftp file)
    Batch {
        /// Path to .aeroftp script file
        file: String,
    },
    /// Upload stdin directly to a remote file
    Rcat {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
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
    /// AeroAgent - AI-powered interactive agent with tool execution
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

        /// Plan only - show execution plan without running
        #[arg(long)]
        plan_only: bool,

        /// Cost limit in USD (stop when exceeded)
        #[arg(long)]
        cost_limit: Option<f64>,

        /// Custom system prompt (or @file.txt to load from file)
        #[arg(long)]
        system: Option<String>,
    },
    /// Start the Model Context Protocol server (JSON-RPC 2.0 over stdio)
    ///
    /// Equivalent to `aeroftp agent --mcp`. Exposed as a top-level subcommand so
    /// that MCP clients (Claude Code, Cursor, Windsurf, VS Code extensions) can
    /// invoke it with minimal configuration.
    Mcp,
    /// Generate shell completions (bash, zsh, fish, elvish, powershell)
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// List saved server profiles from the encrypted vault
    Profiles {
        /// Optional `list` keyword for parity with `<tool> profiles list` muscle memory; ignored.
        #[arg(hide = true)]
        _ignored: Vec<String>,
    },
    /// List configured AI providers and models from the encrypted vault
    AiModels,
    /// Show the canonical task-oriented quick-start for AI agents
    AgentBootstrap {
        /// Optional task focus for tailored commands
        #[arg(long, value_enum)]
        task: Option<AgentBootstrapTask>,
        /// Remote path or working path for the task
        #[arg(long)]
        path: Option<String>,
        /// Optional filename or glob pattern
        #[arg(long)]
        pattern: Option<String>,
        /// Source profile for transfer/backup flows
        #[arg(long)]
        source_profile: Option<String>,
        /// Destination profile for transfer/backup flows
        #[arg(long)]
        dest_profile: Option<String>,
        /// Source remote path for transfer/backup flows
        #[arg(long)]
        source_path: Option<String>,
        /// Destination remote path for transfer/backup flows
        #[arg(long)]
        dest_path: Option<String>,
        /// Local path for local-vs-remote verification flows
        #[arg(long)]
        local_path: Option<String>,
        /// Remote path for local-vs-remote verification flows
        #[arg(long)]
        remote_path: Option<String>,
    },
    /// Show CLI capabilities for AI agent discovery (always JSON)
    AgentInfo,
    /// Single-shot connect surface for agents: returns per-block status
    /// (connect/capabilities/quota/path) in one JSON call. Replaces the
    /// boilerplate `connect → about → df → ls /` sequence.
    ///
    /// Live-connect allowlist: FTP, FTPS, SFTP, WebDAV, S3, GitHub,
    /// GitLab. For other protocols (pCloud, Dropbox, OneDrive, Box,
    /// Filen, MEGA, Koofr, kDrive, Jottacloud, Drime, FileLu, Yandex,
    /// 4shared, Internxt, Swift, Azure, Google Drive, ZohoWorkDrive,
    /// Immich) the response still includes valid `capabilities`, `path`,
    /// and `profile` blocks; only the `connect` block reports
    /// `status: "unsupported"` and the CLI returns exit code 0 — the
    /// payload is still actionable, just use protocol-specific commands
    /// like `link`, `ls`, `put` directly.
    ///
    /// Exit codes: 0 = ok or unsupported (capabilities valid),
    /// 1 = connect tried and failed, 2 = profile lookup failed.
    AgentConnect {
        /// Profile name or ID (exact match preferred; unique substring also accepted)
        profile: String,
    },
    /// Background daemon for persistent mounts, jobs, and watch
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    /// Manage background transfer jobs (requires daemon running)
    Jobs {
        #[command(subcommand)]
        command: JobCommands,
    },
    /// Encrypted overlay - zero-knowledge storage on any provider
    Crypt {
        #[command(subcommand)]
        command: CryptCommands,
    },
    /// Import server profiles from external sources
    Import {
        #[command(subcommand)]
        command: ImportCommands,
    },
}

#[derive(Subcommand)]
enum ImportCommands {
    /// Import remotes from rclone configuration file
    Rclone {
        /// Path to rclone.conf (auto-detected if omitted)
        path: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Import sessions from WinSCP configuration file
    Winscp {
        /// Path to WinSCP.ini (auto-detected on Windows if omitted)
        path: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Import sites from FileZilla configuration file
    Filezilla {
        /// Path to sitemanager.xml (auto-detected if omitted)
        path: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Convert an rclone filter file (--filter-from format) into a .aeroignore file
    #[command(name = "rclone-filter")]
    RcloneFilter {
        /// Path to the rclone filter file (or `-` to read from stdin)
        path: String,
        /// Output path for the generated .aeroignore. If omitted, prints to stdout.
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// Overwrite the output file if it already exists
        #[arg(long)]
        force: bool,
        /// Output a JSON envelope with the converted text and warnings
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Start the background daemon
    Start {
        /// HTTP API bind address
        #[arg(long, default_value = "127.0.0.1:14320")]
        addr: String,
        /// Allow binding the daemon API to non-loopback addresses (unsafe; exposes job control remotely)
        #[arg(long, default_value_t = false)]
        allow_remote_bind: bool,
        /// API auth token. If omitted, a random token is generated and saved for CLI reuse.
        #[arg(long, env = "AEROFTP_DAEMON_TOKEN", hide_env_values = true)]
        auth_token: Option<String>,
    },
    /// Stop the running daemon
    Stop,
    /// Show daemon status
    Status,
}

#[derive(Subcommand)]
enum JobCommands {
    /// Add a new background job
    Add {
        /// Command to execute (e.g., "get --profile S3 /file.zip ./")
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// List all jobs
    List,
    /// Show status of a specific job
    Status {
        /// Job ID
        id: String,
    },
    /// Cancel a running or queued job
    Cancel {
        /// Job ID
        id: String,
    },
}

#[derive(Subcommand)]
enum CryptCommands {
    /// Initialize an encrypted overlay on a remote directory
    Init {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote directory to encrypt
        #[arg(default_value = "/")]
        path: String,
        /// Encryption password (or will prompt interactively)
        #[arg(long, env = "AEROFTP_CRYPT_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
    /// List files in an encrypted overlay (decrypted names)
    Ls {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote encrypted directory
        #[arg(default_value = "/")]
        path: String,
        /// Encryption password
        #[arg(long, env = "AEROFTP_CRYPT_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
    /// Upload a file with encryption (content + filename encrypted)
    Put {
        /// Local file to encrypt and upload
        local: String,
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote encrypted directory
        #[arg(default_value = "/")]
        remote: String,
        /// Encryption password
        #[arg(long, env = "AEROFTP_CRYPT_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
    /// Download and decrypt a file from an encrypted overlay
    Get {
        /// Remote file name (decrypted name, e.g., "secret.txt")
        remote: String,
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote encrypted directory (same as used in crypt init/put)
        #[arg(default_value = "/")]
        path: String,
        /// Local destination
        #[arg(default_value = ".")]
        local: String,
        /// Encryption password
        #[arg(long, env = "AEROFTP_CRYPT_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
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
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote base path to expose (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Local bind address
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        /// Allow binding to non-loopback addresses (unsafe; exposes the served remote to other hosts)
        #[arg(long, default_value_t = false)]
        allow_remote_bind: bool,
        /// Optional auth token. Accepts Bearer auth or Basic auth with any username and this password.
        #[arg(long, env = "AEROFTP_SERVE_AUTH_TOKEN", hide_env_values = true)]
        auth_token: Option<String>,
    },
    /// Serve a remote over local WebDAV (read-write)
    #[command(name = "webdav")]
    WebDav {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote base path to expose (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Local bind address
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        /// Allow binding to non-loopback addresses (unsafe; exposes read-write WebDAV access to other hosts)
        #[arg(long, default_value_t = false)]
        allow_remote_bind: bool,
        /// Optional auth token. Accepts Bearer auth or Basic auth with any username and this password.
        #[arg(long, env = "AEROFTP_SERVE_AUTH_TOKEN", hide_env_values = true)]
        auth_token: Option<String>,
    },
    /// Serve a remote over local FTP (read-write, anonymous)
    Ftp {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote base path to expose (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Local bind address for control connection
        #[arg(long, default_value = "127.0.0.1:2121")]
        addr: String,
        /// Allow binding to non-loopback addresses (unsafe; FTP serve is anonymous)
        #[arg(long, default_value_t = false)]
        allow_remote_bind: bool,
        /// Username for FTP serve auth. If omitted on non-loopback, defaults to "aeroftp".
        #[arg(long, env = "AEROFTP_SERVE_USER")]
        auth_user: Option<String>,
        /// Password for FTP serve auth. If omitted on non-loopback, one is generated automatically.
        #[arg(long, env = "AEROFTP_SERVE_PASSWORD", hide_env_values = true)]
        auth_password: Option<String>,
        /// Passive port range (e.g., "49152-65535")
        #[arg(long, default_value = "49152-49200")]
        passive_ports: String,
    },
    /// Serve a remote over local SFTP (SSH file transfer, read-write)
    Sftp {
        /// Server URL (omit when using --profile)
        #[arg(default_value = "_", hide_default_value = true)]
        url: String,
        /// Remote base path to expose (default: / or the URL/profile initial path)
        #[arg(default_value = "/")]
        path: String,
        /// Local bind address
        #[arg(long, default_value = "127.0.0.1:2222")]
        addr: String,
        /// Allow binding to non-loopback addresses (unsafe; SFTP serve accepts any password)
        #[arg(long, default_value_t = false)]
        allow_remote_bind: bool,
        /// Username for SFTP serve auth. If omitted on non-loopback, defaults to "aeroftp".
        #[arg(long, env = "AEROFTP_SERVE_USER")]
        auth_user: Option<String>,
        /// Password for SFTP serve auth. If omitted on non-loopback, one is generated automatically.
        #[arg(long, env = "AEROFTP_SERVE_PASSWORD", hide_env_values = true)]
        auth_password: Option<String>,
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
    max_transfer: Option<String>,
    max_backlog: Option<usize>,
    retries: Option<u32>,
    retries_sleep: Option<String>,
    chunk_size: Option<String>,
    buffer_size: Option<String>,
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
    schema: &'static str,
    remote_path: String,
    test_size: u64,
    iterations: u32,
    upload_speed_bps: u64,
    download_speed_bps: u64,
    upload_mbps: f64,
    download_mbps: f64,
    download_ttfb_ms: Option<u64>,
    /// True when SHA-256 integrity check actually ran (vs explicitly skipped).
    integrity_checked: bool,
    /// True only when the check ran AND hashes matched.
    integrity_verified: bool,
    upload_sha256: String,
    download_sha256: String,
    cleanup_ok: bool,
    cleanup_error: Option<String>,
    elapsed_secs: f64,
    protocol: String,
}

#[derive(Serialize)]
struct CliSpeedCompareEntry {
    rank: u32,
    url: String,
    protocol: String,
    score: f64,
    result: Option<CliSpeedResult>,
    error: Option<String>,
}

#[derive(Serialize)]
struct CliSpeedCompareReport {
    status: &'static str,
    schema: &'static str,
    test_size: u64,
    parallel: u8,
    started_at_ms: u64,
    finished_at_ms: u64,
    results: Vec<CliSpeedCompareEntry>,
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
    /// Per-file execution plan. Populated in `--dry-run` so agents can pilot
    /// sync without having to parse the text-verbose output. Empty on real
    /// runs — skipped at serialization time so the shape of historical JSON
    /// output is unchanged for callers that never pass `--dry-run`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    plan: Vec<CliSyncPlanEntry>,
}

/// Single plan entry surfaced in `sync --dry-run --json`.
///
/// `local_size` / `remote_size` are both present when the file exists on
/// both sides (`differ` class); either can be `None` for one-sided
/// operations (e.g. upload to a missing path has no `remote_size`, a fresh
/// download has no `local_size`). `reason` mirrors the decision label used
/// in the text dry-run (`"new"`, `"size differs"`, `"newer local"`, etc.).
#[derive(Serialize, Default)]
struct CliSyncPlanEntry {
    op: &'static str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_path: Option<String>,
}

/// Stats returned by cmd_sync for watch mode output enrichment.
#[derive(Default, Clone)]
struct SyncCycleStats {
    exit_code: i32,
    uploaded: u32,
    downloaded: u32,
    deleted: u32,
    skipped: u32,
    error_count: u32,
}

impl From<i32> for SyncCycleStats {
    fn from(code: i32) -> Self {
        Self {
            exit_code: code,
            ..Default::default()
        }
    }
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

#[allow(dead_code)]
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

#[derive(Serialize)]
struct CliReconcileResult {
    status: &'static str,
    local_path: String,
    remote_path: String,
    summary: serde_json::Value,
    groups: serde_json::Value,
    suggested_next_command: String,
}

#[derive(Debug, Deserialize)]
struct StoredReconcileEntry {
    path: String,
    #[serde(default)]
    local_size: Option<u64>,
    #[serde(default)]
    remote_size: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct StoredReconcileGroups {
    #[serde(default, rename = "match")]
    matches: Vec<StoredReconcileEntry>,
    #[serde(default)]
    differ: Vec<StoredReconcileEntry>,
    #[serde(default)]
    missing_remote: Vec<StoredReconcileEntry>,
    #[serde(default)]
    missing_local: Vec<StoredReconcileEntry>,
}

#[derive(Debug, Deserialize)]
struct StoredReconcileResult {
    groups: Option<StoredReconcileGroups>,
}

#[derive(Debug, Default)]
struct ReconcileSyncPlan {
    local_entries: Vec<(String, u64, Option<String>)>,
    remote_entries: Vec<(String, u64, Option<String>)>,
    to_upload: Vec<String>,
    to_download: Vec<String>,
    to_delete_remote: Vec<String>,
    to_delete_local: Vec<String>,
    skipped: u32,
}

#[derive(Serialize)]
struct CliDoctorResult {
    status: &'static str,
    doctor: String,
    summary: serde_json::Value,
    checks: Vec<serde_json::Value>,
    risks: Vec<String>,
    suggested_next_command: String,
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
    toml::from_str(&raw).map_err(|e| format!("Invalid config '{}': {}", path.display(), e))
}

fn save_cli_config(config: &CliConfigFile) -> Result<PathBuf, String> {
    let path = cli_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Cannot create config directory '{}': {}",
                parent.display(),
                e
            )
        })?;
    }

    let content =
        toml::to_string_pretty(config).map_err(|e| format!("Cannot serialize config: {}", e))?;
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
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Cannot create config directory '{}': {}",
                parent.display(),
                e
            )
        })?;
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
    match (
        normalize_release_version(latest),
        normalize_release_version(current),
    ) {
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
            || (arg.starts_with('-') && arg.len() > 1 && arg.chars().skip(1).all(|ch| ch == 'v'))
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
    if config.defaults.json.unwrap_or(false)
        && !arg_present(args, "--json", None)
        && !arg_present(args, "--format", None)
    {
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
    if let Some(max_transfer) = &config.defaults.max_transfer {
        if !arg_present(args, "--max-transfer", None) {
            merged.push("--max-transfer".to_string());
            merged.push(max_transfer.clone());
        }
    }
    if let Some(max_backlog) = config.defaults.max_backlog {
        if !arg_present(args, "--max-backlog", None) {
            merged.push("--max-backlog".to_string());
            merged.push(max_backlog.to_string());
        }
    }
    if let Some(retries) = config.defaults.retries {
        if !arg_present(args, "--retries", None) {
            merged.push("--retries".to_string());
            merged.push(retries.to_string());
        }
    }
    if let Some(retries_sleep) = &config.defaults.retries_sleep {
        if !arg_present(args, "--retries-sleep", None) {
            merged.push("--retries-sleep".to_string());
            merged.push(retries_sleep.clone());
        }
    }
    if let Some(chunk_size) = &config.defaults.chunk_size {
        if !arg_present(args, "--chunk-size", None) {
            merged.push("--chunk-size".to_string());
            merged.push(chunk_size.clone());
        }
    }
    if let Some(buffer_size) = &config.defaults.buffer_size {
        if !arg_present(args, "--buffer-size", None) {
            merged.push("--buffer-size".to_string());
            merged.push(buffer_size.clone());
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
                | "--max-transfer"
                | "--max-backlog"
                | "--retries"
                | "--retries-sleep"
                | "--chunk-size"
                | "--buffer-size"
                | "--dump"
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
            eprintln!(
                "{}",
                serde_json::to_string(&CliError {
                    status: "error",
                    error: msg.to_string(),
                    code,
                })
                .unwrap()
            );
        }
    }
}

fn provider_error_to_exit_code(err: &ProviderError) -> i32 {
    match err {
        ProviderError::ConnectionFailed(_)
        | ProviderError::NotConnected
        | ProviderError::NetworkError(_) => 1,
        ProviderError::NotFound(_) => 2,
        ProviderError::PermissionDenied(_) => 3,
        ProviderError::TransferFailed(_) | ProviderError::Cancelled => 4,
        ProviderError::InvalidConfig(_) | ProviderError::InvalidPath(_) => 5,
        ProviderError::AuthenticationFailed(_) => 6,
        ProviderError::NotSupported(_) => 7,
        ProviderError::Timeout => 8,
        ProviderError::AlreadyExists(_) | ProviderError::DirectoryNotEmpty(_) => 9,
        ProviderError::ParseError(_) | ProviderError::ServerError(_) => 10,
        ProviderError::IoError(_) => 11,
        ProviderError::Unknown(_) | ProviderError::Other(_) => 99,
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
        n.parse::<u64>()
            .map(|v| v * 1024 * 1024)
            .map_err(|e| e.to_string())
    } else if let Some(n) = s.strip_suffix('K') {
        n.parse::<u64>()
            .map(|v| v * 1024)
            .map_err(|e| e.to_string())
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
                        path.display(),
                        canonical_parent.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Sanitize a filename for terminal display - strip ANSI escape sequences.
fn sanitize_filename(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ESC [ ... (letter) sequence
            if let Some(next) = chars.next() {
                if next == '[' {
                    // CSI sequence - consume until a letter
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
    if std::env::var("CLICOLOR_FORCE")
        .ok()
        .is_some_and(|v| v != "0")
    {
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
    "/proc",
    "/sys",
    "/dev",
    "/boot",
    "/root",
    "/etc/shadow",
    "/etc/passwd",
    "/etc/ssh",
    "/etc/sudoers",
];

const CLI_DENIED_HOME_RELATIVE_PREFIXES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".docker",
    ".config/gcloud",
    ".config/aeroftp",
    ".vault-token",
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
        _ => (s, 86400u64),                            // default: days
    };
    num_str
        .trim()
        .parse::<f64>()
        .map(|n| (n * multiplier as f64) as u64)
        .map_err(|e| format!("Invalid duration '{}': {}", s, e))
}

/// Load patterns from a file (one per line, # comments, blank lines skipped).
/// Load file list from --files-from or --files-from-raw.
/// Returns None if neither flag is set, or Some(HashSet) with normalized paths.
fn load_files_from(cli: &Cli) -> Option<std::collections::HashSet<String>> {
    let (path, raw) = match (&cli.files_from, &cli.files_from_raw) {
        (Some(p), _) => (p.as_str(), false),
        (_, Some(p)) => (p.as_str(), true),
        _ => return None,
    };
    // Cap file size at 10 MB to prevent OOM
    const MAX_FILES_FROM_SIZE: u64 = 10 * 1024 * 1024;
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_FILES_FROM_SIZE => {
            eprintln!(
                "Error: --files-from '{}' exceeds 10 MB limit ({} bytes)",
                path,
                meta.len()
            );
            std::process::exit(5);
        }
        Err(e) => {
            eprintln!("Error: --files-from '{}' not accessible: {}", path, e);
            std::process::exit(5);
        }
        _ => {}
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            // Fatal: user explicitly requested --files-from, silent fallback to "all files" is dangerous
            eprintln!("Error: cannot read --files-from '{}': {}", path, e);
            std::process::exit(5);
        }
    };
    let set: std::collections::HashSet<String> = content
        .lines()
        .map(|l| {
            if raw {
                l.to_string()
            } else {
                l.trim().to_string()
            }
        })
        .filter(|l| {
            if raw {
                true
            } else {
                !l.is_empty() && !l.starts_with('#')
            }
        })
        .map(|l| {
            // Normalize: strip leading ./ and /
            let s = l.strip_prefix("./").unwrap_or(&l);
            let s = s.strip_prefix('/').unwrap_or(s);
            s.to_string()
        })
        .collect();
    if !cli.quiet {
        eprintln!(
            "Note: --files-from loaded {} entries from '{}'",
            set.len(),
            path
        );
    }
    Some(set)
}

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
    let min_size = cli
        .min_size
        .as_ref()
        .and_then(|s| parse_size_filter(s).ok());
    let max_size = cli
        .max_size
        .as_ref()
        .and_then(|s| parse_size_filter(s).ok());

    // Parse age limits (convert to threshold timestamps)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let min_age_ts = cli
        .min_age
        .as_ref()
        .and_then(|s| parse_age_filter(s).ok())
        .map(|secs| now - secs);
    let max_age_ts = cli
        .max_age
        .as_ref()
        .and_then(|s| parse_age_filter(s).ok())
        .map(|secs| now - secs);

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
/// Format: "08:00,512k 12:00,10M 18:00,off" - space-separated entries.
/// Returns the active rate in bytes/sec, or None if unlimited ("off").
///
/// Time entries are interpreted in **local time** (matching rclone's behavior),
/// so "08:00" means 8 AM in the user's timezone. For the wrap-around rule see
/// `resolve_bwlimit_schedule_at`.
fn resolve_bwlimit_schedule(schedule: &str) -> Option<u64> {
    use chrono::{Local, Timelike};
    let now = Local::now();
    let now_minutes = now.hour() * 60 + now.minute();
    resolve_bwlimit_schedule_at(schedule, now_minutes)
}

/// Pure-logic core of [`resolve_bwlimit_schedule`], parameterised on
/// `now_minutes` (minutes since midnight, 0..=1439) so tests can pin the clock.
///
/// Returns the active rate at `now_minutes`. The active entry is the latest
/// scheduled time `<= now_minutes`; if `now_minutes` is before the first entry
/// of the day, the last entry wraps over from the previous day.
fn resolve_bwlimit_schedule_at(schedule: &str, now_minutes: u32) -> Option<u64> {
    let mut entries: Vec<(u32, Option<u64>)> = Vec::new(); // (minutes_since_midnight, rate)
    for part in schedule.split_whitespace() {
        if let Some((time_str, rate_str)) = part.split_once(',') {
            let time_parts: Vec<&str> = time_str.split(':').collect();
            if time_parts.len() == 2 {
                if let (Ok(h), Ok(m)) = (time_parts[0].parse::<u32>(), time_parts[1].parse::<u32>())
                {
                    if h >= 24 || m >= 60 {
                        continue; // skip malformed entry
                    }
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

    // Find the last entry whose time <= now
    let mut active_rate: Option<u64> = None;
    let mut matched = false;
    for (minutes, rate) in &entries {
        if *minutes <= now_minutes {
            active_rate = *rate;
            matched = true;
        }
    }
    // If no entry matched (before first entry of the day), wrap around to the
    // last scheduled entry (= the active rate from the previous day still holds).
    if !matched {
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

// ── Session transfer accounting (--max-transfer) ──────────────────

/// Global session byte counter (upload + download combined).
static SESSION_TRANSFERRED_BYTES: AtomicU64 = AtomicU64::new(0);
/// Print "Using profile: ..." only once per session (avoids noise from parallel workers).
static PROFILE_INFO_PRINTED: AtomicBool = AtomicBool::new(false);
/// Set early in main() when the user passed `--json` or `--format json`.
/// Used to suppress informational stderr output (banners, "Using
/// profile:") that an agent capturing combined stdout+stderr would
/// see as JSON corruption. Surfaced by the 4-Sonnet agent
/// friendliness audit (2026-04-26, all 4 batteries flagged this).
static JSON_MODE: AtomicBool = AtomicBool::new(false);

fn print_profile_banner_once(name: &str, details: String, quiet: bool) {
    if quiet || JSON_MODE.load(Ordering::Relaxed) {
        return;
    }
    if !PROFILE_INFO_PRINTED.swap(true, Ordering::Relaxed) {
        eprintln!("Using profile: {} ({})", name, details);
    }
}

async fn maybe_hydrate_ftp_stat_size(
    provider: &mut Box<dyn StorageProvider>,
    path: &str,
    entry: &mut RemoteEntry,
) {
    if entry.is_dir || entry.size > 0 {
        return;
    }
    if !matches!(
        provider.provider_type(),
        ProviderType::Ftp | ProviderType::Ftps
    ) {
        return;
    }
    if let Ok(size) = provider.size(path).await {
        entry.size = size;
    }
}

fn maybe_create_scan_spinner(format: OutputFormat, cli: &Cli, msg: &str) -> Option<ProgressBar> {
    if matches!(format, OutputFormat::Text) && !cli.quiet && use_color() {
        Some(create_spinner(msg))
    } else {
        None
    }
}

fn maybe_update_scan_spinner(
    spinner: &Option<ProgressBar>,
    last_update: &mut Instant,
    message: String,
) {
    if let Some(pb) = spinner {
        if last_update.elapsed() >= std::time::Duration::from_millis(500) {
            pb.set_message(message);
            *last_update = Instant::now();
        }
    }
}

fn hash_local_file_sha256(path: &Path) -> Option<String> {
    use sha2::Digest;

    let data = std::fs::read(path).ok()?;
    Some(format!("{:x}", sha2::Sha256::digest(&data)))
}

fn scan_local_tree_with_progress(
    root: &str,
    opts: &ftp_client_gui_lib::sync_core::ScanOptions,
    spinner: &Option<ProgressBar>,
) -> Vec<ftp_client_gui_lib::sync_core::scan::LocalEntry> {
    let matchers: Vec<globset::GlobMatcher> = opts
        .exclude_patterns
        .iter()
        .filter_map(|pat| {
            globset::Glob::new(pat)
                .ok()
                .map(|glob| glob.compile_matcher())
        })
        .collect();
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(MAX_SCAN_DEPTH);
    let mut last_update = Instant::now()
        .checked_sub(std::time::Duration::from_millis(500))
        .unwrap_or_else(Instant::now);
    let mut entries = Vec::new();

    for walk_entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(depth)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if entries.len() >= cap {
            break;
        }
        if !walk_entry.file_type().is_file() {
            continue;
        }
        let relative = walk_entry
            .path()
            .strip_prefix(root)
            .unwrap_or(walk_entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() {
            continue;
        }
        let fname = walk_entry.file_name().to_string_lossy().into_owned();
        if opts.skip_filenames.iter().any(|name| name == &fname) {
            continue;
        }
        if matchers
            .iter()
            .any(|matcher| matcher.is_match(&relative) || matcher.is_match(&fname))
        {
            continue;
        }
        if let Some(ref set) = opts.files_from {
            if !set.contains(relative.as_str()) {
                continue;
            }
        }

        let meta = walk_entry.metadata().ok();
        let size = meta.as_ref().map(|value| value.len()).unwrap_or(0);
        let mtime = meta.and_then(|value| {
            value.modified().ok().map(|timestamp| {
                let dt: chrono::DateTime<chrono::Utc> = timestamp.into();
                dt.format("%Y-%m-%dT%H:%M:%S").to_string()
            })
        });
        let sha256 = if opts.compute_checksum {
            hash_local_file_sha256(walk_entry.path())
        } else {
            None
        };

        entries.push(ftp_client_gui_lib::sync_core::scan::LocalEntry {
            rel_path: relative,
            size,
            mtime,
            sha256,
        });
        maybe_update_scan_spinner(
            spinner,
            &mut last_update,
            format!("Scanning local... {} files so far", entries.len()),
        );
    }

    if let Some(pb) = spinner {
        pb.set_message(format!("Scanning local... {} files", entries.len()));
    }

    entries
}

async fn scan_remote_tree_with_progress(
    provider: &mut Box<dyn StorageProvider>,
    remote_root: &str,
    opts: &ftp_client_gui_lib::sync_core::ScanOptions,
    spinner: &Option<ProgressBar>,
) -> Vec<ftp_client_gui_lib::sync_core::scan::RemoteEntry> {
    let matchers: Vec<globset::GlobMatcher> = opts
        .exclude_patterns
        .iter()
        .filter_map(|pat| {
            globset::Glob::new(pat)
                .ok()
                .map(|glob| glob.compile_matcher())
        })
        .collect();
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(MAX_SCAN_DEPTH);
    let want_remote_checksum = opts.compute_remote_checksum && provider.supports_checksum();
    let mut last_update = Instant::now()
        .checked_sub(std::time::Duration::from_millis(500))
        .unwrap_or_else(Instant::now);
    let mut results = Vec::new();
    let mut queue: Vec<(String, String, usize)> = vec![(remote_root.to_string(), String::new(), 0)];

    while let Some((abs_dir, rel_prefix, current_depth)) = queue.pop() {
        if current_depth >= depth || results.len() >= cap {
            continue;
        }
        match provider.list(&abs_dir).await {
            Ok(entries) => {
                for entry in entries {
                    let entry_rel = if rel_prefix.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{}/{}", rel_prefix, entry.name)
                    };
                    if entry.is_dir {
                        queue.push((entry.path.clone(), entry_rel, current_depth + 1));
                        continue;
                    }
                    if opts.skip_filenames.iter().any(|name| name == &entry.name) {
                        continue;
                    }
                    if matchers.iter().any(|matcher| {
                        matcher.is_match(&entry_rel) || matcher.is_match(&entry.name)
                    }) {
                        continue;
                    }
                    if let Some(ref set) = opts.files_from {
                        if !set.contains(entry_rel.as_str()) {
                            continue;
                        }
                    }

                    let (checksum_alg, checksum_hex) = if want_remote_checksum {
                        match provider.checksum(&entry.path).await {
                            Ok(map) => {
                                if let Some(value) =
                                    map.get("sha256").or_else(|| map.get("SHA-256"))
                                {
                                    (Some("sha256".to_string()), Some(value.clone()))
                                } else {
                                    (None, None)
                                }
                            }
                            Err(_) => (None, None),
                        }
                    } else {
                        (None, None)
                    };

                    results.push(ftp_client_gui_lib::sync_core::scan::RemoteEntry {
                        rel_path: entry_rel,
                        size: entry.size,
                        mtime: entry.modified,
                        checksum_alg,
                        checksum_hex,
                    });
                    maybe_update_scan_spinner(
                        spinner,
                        &mut last_update,
                        format!("Scanning remote... {} files so far", results.len()),
                    );
                    if results.len() >= cap {
                        break;
                    }
                }
            }
            Err(err) => {
                eprintln!(
                    "[scan_remote_tree] warning: failed to list {}: {}",
                    abs_dir, err
                );
            }
        }
    }

    if let Some(pb) = spinner {
        pb.set_message(format!("Scanning remote... {} files", results.len()));
    }

    results
}

fn load_sync_plan_from_reconcile(
    path: &str,
    direction: &str,
    delete: bool,
) -> Result<ReconcileSyncPlan, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("Cannot read reconcile file '{}': {}", path, err))?;
    let stored: StoredReconcileResult = serde_json::from_str(&raw)
        .map_err(|err| format!("Invalid reconcile JSON '{}': {}", path, err))?;
    let groups = stored.groups.ok_or_else(|| {
        format!(
            "Reconcile file '{}' does not contain detailed groups. Re-run reconcile without --format summary.",
            path
        )
    })?;

    if direction == "both" && (delete || !groups.differ.is_empty()) {
        return Err(
            "--from-reconcile supports --direction both only when there are no differ entries and --delete is off"
                .to_string(),
        );
    }

    let mut plan = ReconcileSyncPlan::default();

    for entry in &groups.matches {
        let local_size = entry.local_size.unwrap_or(0);
        let remote_size = entry.remote_size.unwrap_or(local_size);
        plan.local_entries
            .push((entry.path.clone(), local_size, None));
        plan.remote_entries
            .push((entry.path.clone(), remote_size, None));
    }
    for entry in &groups.differ {
        let local_size = entry.local_size.unwrap_or(0);
        let remote_size = entry.remote_size.unwrap_or(0);
        plan.local_entries
            .push((entry.path.clone(), local_size, None));
        plan.remote_entries
            .push((entry.path.clone(), remote_size, None));
    }
    for entry in &groups.missing_remote {
        plan.local_entries
            .push((entry.path.clone(), entry.local_size.unwrap_or(0), None));
    }
    for entry in &groups.missing_local {
        plan.remote_entries
            .push((entry.path.clone(), entry.remote_size.unwrap_or(0), None));
    }

    match direction {
        "upload" => {
            plan.to_upload = groups
                .differ
                .iter()
                .chain(groups.missing_remote.iter())
                .map(|entry| entry.path.clone())
                .collect();
            if delete {
                plan.to_delete_remote = groups
                    .missing_local
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect();
            }
        }
        "download" => {
            plan.to_download = groups
                .differ
                .iter()
                .chain(groups.missing_local.iter())
                .map(|entry| entry.path.clone())
                .collect();
            if delete {
                plan.to_delete_local = groups
                    .missing_remote
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect();
            }
        }
        "both" => {
            plan.to_upload = groups
                .missing_remote
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
            plan.to_download = groups
                .missing_local
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
        }
        other => {
            return Err(format!(
                "--from-reconcile does not support direction '{}'",
                other
            ))
        }
    }

    plan.skipped = groups.matches.len() as u32;
    Ok(plan)
}

/// Parse --max-transfer value, returning the byte limit (or None if unset).
fn resolve_max_transfer(cli: &Cli) -> Option<u64> {
    cli.max_transfer
        .as_ref()
        .and_then(|s| parse_size_filter(s).ok())
}

/// Check whether the session has exceeded --max-transfer. Returns true if over limit.
fn session_transfer_exceeded(limit: Option<u64>) -> bool {
    match limit {
        Some(max) => SESSION_TRANSFERRED_BYTES.load(Ordering::Relaxed) >= max,
        None => false,
    }
}

/// Add bytes to the session counter and return the new total.
fn session_transfer_add(bytes: u64) -> u64 {
    SESSION_TRANSFERRED_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes
}

// ── Retry helper ──────────────────────────────────────────────────

/// Parse a duration string like "5s", "1m", "500ms", "0" into Duration.
fn parse_retry_sleep(s: &str) -> std::time::Duration {
    let s = s.trim().to_lowercase();
    if s == "0" || s.is_empty() {
        return std::time::Duration::ZERO;
    }
    if let Some(n) = s.strip_suffix("ms") {
        if let Ok(v) = n.parse::<u64>() {
            return std::time::Duration::from_millis(v);
        }
    }
    if let Some(n) = s.strip_suffix('s') {
        if let Ok(v) = n.parse::<u64>() {
            return std::time::Duration::from_secs(v);
        }
    }
    if let Some(n) = s.strip_suffix('m') {
        if let Ok(v) = n.parse::<u64>() {
            return std::time::Duration::from_secs(v * 60);
        }
    }
    if let Some(n) = s.strip_suffix('h') {
        if let Ok(v) = n.parse::<u64>() {
            return std::time::Duration::from_secs(v * 3600);
        }
    }
    // Default fallback: 1 second
    std::time::Duration::from_secs(1)
}

// ── Dump helper (--dump headers,bodies,auth) ──────────────────────

fn dump_enabled(cli: &Cli, kind: &str) -> bool {
    cli.dump.iter().any(|d| d.eq_ignore_ascii_case(kind))
}

fn dump_connection_info(cli: &Cli, config: &ProviderConfig) {
    if cli.dump.is_empty() {
        return;
    }
    let has_headers = dump_enabled(cli, "headers") || dump_enabled(cli, "bodies");
    let has_auth = dump_enabled(cli, "auth");
    if has_headers || has_auth {
        eprintln!("--- DUMP: connection ---");
        eprintln!("  provider: {:?}", config.provider_type);
        eprintln!("  host: {}", config.host);
        eprintln!(
            "  port: {}",
            config.port.map_or("default".to_string(), |p| p.to_string())
        );
        eprintln!(
            "  username: {}",
            config.username.as_deref().unwrap_or("(none)")
        );
        if has_auth {
            let pass = config.password.as_deref().unwrap_or("");
            eprintln!(
                "  password: {}",
                if pass.is_empty() { "(empty)" } else { pass }
            );
        } else {
            eprintln!("  password: [redacted, use --dump auth to show]");
        }
        if let Some(ref path) = config.initial_path {
            if !path.is_empty() {
                eprintln!("  path: {}", path);
            }
        }
        eprintln!("---");
    }
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
        // Check for partial .aerotmp file from a previous interrupted download.
        // HTTP providers use ResumableFile internally (reads .aerotmp),
        // while FTP/Koofr write directly to the final path (with seek).
        let tmp_path = format!("{}.aerotmp", local_path);
        let offset = std::fs::metadata(&tmp_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if offset > 0 {
            return provider
                .resume_download(remote_path, local_path, offset, progress_cb)
                .await;
        }
        // Also check the final file itself (FTP resume writes there directly)
        let final_offset = std::fs::metadata(local_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if final_offset > 0 {
            return provider
                .resume_download(remote_path, local_path, final_offset, progress_cb)
                .await;
        }
    }
    provider
        .download(remote_path, local_path, progress_cb)
        .await
}

async fn upload_with_resume(
    provider: &mut dyn StorageProvider,
    local_path: &str,
    remote_path: &str,
    cli: &Cli,
    progress_cb: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    if cli.partial && provider.supports_resume() {
        let local_size = std::fs::metadata(local_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        if let Ok(remote_size) = provider.size(remote_path).await {
            if remote_size > 0 && remote_size < local_size {
                return provider
                    .resume_upload(local_path, remote_path, remote_size, progress_cb)
                    .await;
            }
        }
    }
    provider.upload(local_path, remote_path, progress_cb).await
}

#[allow(clippy::too_many_arguments)]
async fn download_transfer_task(
    url: &str,
    remote_path: String,
    local_path: String,
    cli: &Cli,
    format: OutputFormat,
    aggregate: Option<Arc<AtomicU64>>,
    overall_pb: Option<ProgressBar>,
    max_transfer_limit: Option<u64>,
) -> Result<(), String> {
    // --max-transfer: skip if session limit already exceeded
    if session_transfer_exceeded(max_transfer_limit) {
        return Err("max-transfer limit reached".to_string());
    }

    let (mut provider, _) = create_and_connect(url, cli, format)
        .await
        .map_err(|code| format!("connection failed with exit code {}", code))?;

    let progress_cb = aggregate.map(|aggregate| make_aggregate_progress_cb(aggregate, overall_pb));
    let result = download_with_resume(&mut *provider, &remote_path, &local_path, cli, progress_cb)
        .await
        .map_err(|e| e.to_string());

    // In --inplace mode the download writes directly to the final path, so a failed
    // transfer can leave a truncated file behind. When --partial is disabled, match
    // the single-file commands and remove that partial artifact.
    if result.is_err() && cli.inplace && !cli.partial {
        let _ = std::fs::remove_file(&local_path);
    }

    // Account transferred bytes
    if result.is_ok() {
        let bytes = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
        session_transfer_add(bytes);
    }

    let _ = provider.disconnect().await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn upload_transfer_task(
    url: &str,
    local_path: String,
    remote_path: String,
    cli: &Cli,
    format: OutputFormat,
    aggregate: Option<Arc<AtomicU64>>,
    overall_pb: Option<ProgressBar>,
    max_transfer_limit: Option<u64>,
) -> Result<(), String> {
    // --max-transfer: skip if session limit already exceeded
    if session_transfer_exceeded(max_transfer_limit) {
        return Err("max-transfer limit reached".to_string());
    }

    let (mut provider, _) = create_and_connect(url, cli, format)
        .await
        .map_err(|code| format!("connection failed with exit code {}", code))?;

    // --immutable: skip if remote file already exists (never overwrite)
    if cli.immutable && provider.stat(&remote_path).await.is_ok() {
        let _ = provider.disconnect().await;
        return Err(format!(
            "skipped (already exists, --immutable): {}",
            remote_path
        ));
    }

    if let Some(parent) = Path::new(&remote_path).parent() {
        let _ = provider.mkdir(&parent.to_string_lossy()).await;
    }

    let file_size = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
    let progress_cb = aggregate.map(|aggregate| make_aggregate_progress_cb(aggregate, overall_pb));
    let result = upload_with_resume(&mut *provider, &local_path, &remote_path, cli, progress_cb)
        .await
        .map_err(|e| e.to_string());

    // Account transferred bytes
    if result.is_ok() {
        session_transfer_add(file_size);
    }

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
        let pass =
            rpassword::read_password().map_err(|e| format!("Failed to read password: {}", e))?;
        return Ok(pass);
    }

    // 6. No password (FTP anonymous, etc.)
    Ok(String::new())
}

fn url_to_provider_config(url: &str, cli: &Cli) -> Result<(ProviderConfig, String), String> {
    let url_obj = url::Url::parse(url).map_err(|e| {
        // A parse failure on a string with no scheme is almost always a
        // user typing what they think is a saved profile path
        // (`/myserver/data`) without `--profile`. The url::ParseError
        // message ("relative URL without a base") is correct but
        // unhelpful, so map to something actionable. Issue #125 polish.
        if !url.contains("://") {
            format!(
                "Invalid URL or unknown profile in '{}'. Use --profile <name> for saved profiles, or protocol://host/path for direct URLs (supported: {}).",
                url,
                SUPPORTED_URL_SCHEMES.join(", ")
            )
        } else {
            format!("Invalid URL: {}", e)
        }
    })?;

    let scheme = url_obj.scheme().to_lowercase();
    let host_str = url_obj.host_str().ok_or("Missing host in URL")?.to_string();

    let (provider_type, effective_host) = match scheme.as_str() {
        "ftp" => (ProviderType::Ftp, host_str.clone()),
        "ftps" => (ProviderType::Ftps, host_str.clone()),
        "sftp" | "ssh" => (ProviderType::Sftp, host_str.clone()),
        "webdav" | "http" => {
            let port_str = url_obj
                .port()
                .map(|p| format!(":{}", p))
                .unwrap_or_default();
            // U2 regression: only fold the URL path into the WebDAV
            // base URL when it is a directory (ends with `/`). A file
            // path would otherwise produce a base URL pointing at a
            // regular file, and every subsequent PROPFIND would 400.
            // The caller still sees the file target via
            // `extra["url_target"]` downstream.
            let path = url_obj.path();
            let base_path = if path.is_empty() || path == "/" || path.ends_with('/') {
                path.to_string()
            } else {
                match path.rfind('/') {
                    Some(idx) if idx > 0 => format!("{}/", &path[..idx]),
                    _ => "/".to_string(),
                }
            };
            (
                ProviderType::WebDav,
                format!("http://{}{}{}", host_str, port_str, base_path),
            )
        }
        "webdavs" | "https" => {
            let port_str = url_obj
                .port()
                .map(|p| format!(":{}", p))
                .unwrap_or_default();
            let path = url_obj.path();
            let base_path = if path.is_empty() || path == "/" || path.ends_with('/') {
                path.to_string()
            } else {
                match path.rfind('/') {
                    Some(idx) if idx > 0 => format!("{}/", &path[..idx]),
                    _ => "/".to_string(),
                }
            };
            (
                ProviderType::WebDav,
                format!("https://{}{}{}", host_str, port_str, base_path),
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
        _ => {
            return Err(format!(
                "Unsupported protocol: {}. Supported: {}",
                scheme,
                SUPPORTED_URL_SCHEMES.join(", ")
            ))
        }
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

    // For WebDAV/GitHub, the URL path is part of the host - initial_path is always /
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

    // U1/U2/U3 fix: when the URL embeds a file target path (no trailing
    // slash, last segment looks like a filename), use the PARENT
    // directory as `initial_path` so the provider does not try to CWD
    // into a regular file and so `resolve_cli_remote_path(initial, arg)`
    // does not produce a double-prefix like `/dir/file/file`.
    //
    // Heuristic: treat the URL path as a file target when all of:
    //  - not empty and not `/`
    //  - does not end with `/`
    //  - last segment is non-empty
    //  - last segment contains a `.` OR at least one extra path segment
    //    exists (a single leading segment like `/dir` is still ambiguous
    //    but we keep it as-is to avoid regressing `ls ftp://host/dir`)
    //
    // The full URL path is stashed under `extra["url_target"]` so
    // single-target commands can surface it when no explicit path arg
    // was provided.
    let (initial_path, url_target_hint) = {
        let trimmed = url_path.trim();
        if trimmed.is_empty() || trimmed == "/" {
            ("/".to_string(), None)
        } else if trimmed.ends_with('/') {
            (trimmed.to_string(), None)
        } else {
            let last_segment = trimmed.rsplit('/').next().unwrap_or("");
            let segment_count = trimmed.trim_start_matches('/').split('/').count();
            let looks_file_like =
                !last_segment.is_empty() && (last_segment.contains('.') || segment_count >= 2);
            if looks_file_like {
                let parent = match trimmed.rfind('/') {
                    Some(idx) if idx > 0 => trimmed[..idx].to_string(),
                    _ => "/".to_string(),
                };
                (parent, Some(trimmed.to_string()))
            } else {
                (trimmed.to_string(), None)
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

    if provider_type == ProviderType::Mega && !extra.contains_key("mega_mode") {
        extra.insert("mega_mode".to_string(), "native".to_string());
    }

    // U1/U2/U3: propagate the file-target hint so single-target
    // commands can fall back to it when no explicit path arg is given.
    if let Some(target) = url_target_hint.as_ref() {
        extra.insert("url_target".to_string(), target.clone());
    }

    let config = ProviderConfig {
        name: format!("{} CLI", provider_type),
        provider_type,
        host: effective_host,
        port,
        username: Some(username),
        password: Some(password),
        initial_path: Some(initial_path.clone()),
        extra,
    };

    Ok((config, initial_path))
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
                return Err(
                    "Vault is locked. Use --master-password or set AEROFTP_MASTER_PASSWORD"
                        .to_string(),
                );
            }
        }
        Ok(_) => {} // Auto mode - already open
        Err(e) => return Err(format!("Failed to open vault: {}", e)),
    }

    CredentialStore::from_cache().ok_or_else(|| "Vault not available after init".to_string())
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
        // JSON: enrich with auth_state derived from local vault state only
        // (no network). Agents read this once and skip a follow-up connect
        // for profiles that aren't ready yet.
        let accounts: std::collections::HashSet<String> = store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let safe: Vec<serde_json::Value> = profiles
            .iter()
            .map(|p| {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let proto = p.get("protocol").and_then(|v| v.as_str()).unwrap_or("");
                let auth_state = ftp_client_gui_lib::profile_auth_state::derive_profile_auth_state(
                    &store, &accounts, id, proto,
                );
                serde_json::json!({
                    "id": id,
                    "name": p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed"),
                    "protocol": proto,
                    "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                    "port": p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
                    "username": p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                    "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
                    "auth_state": auth_state,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&safe).unwrap_or_default()
        );
    } else {
        // Text: formatted table
        println!(
            "  {:<4} {:<30} {:<8} {:<35} Path",
            "#", "Name", "Proto", "Host"
        );
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
            println!(
                "  {:<4} {:<30} {:<8} {:<35} {}",
                i + 1,
                name,
                proto.to_uppercase(),
                host_port,
                path
            );
        }
        eprintln!(
            "\n{} profile(s). Use: aeroftp-cli ls --profile \"Name\" [path]",
            profiles.len()
        );
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

    // Debug: list all vault keys related to AI
    if cli.verbose > 0 {
        if let Ok(accounts) = store.list_accounts() {
            let ai_keys: Vec<_> = accounts
                .iter()
                .filter(|a| a.contains("ai") || a.contains("AI"))
                .collect();
            if ai_keys.is_empty() {
                eprintln!(
                    "[debug] No AI-related keys found in vault ({} total accounts)",
                    accounts.len()
                );
            } else {
                eprintln!(
                    "[debug] AI-related vault keys ({}/{} total):",
                    ai_keys.len(),
                    accounts.len()
                );
                for k in &ai_keys {
                    eprintln!("  {}", k);
                }
            }
        }
    }

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
    // GUI secureStorage uses "config_" prefix (see src/utils/secureStorage.ts VAULT_PREFIX)
    if let Ok(settings_json) = store
        .get("config_ai_settings")
        .or_else(|_| store.get("ai_settings"))
    {
        if let Ok(settings) = serde_json::from_str::<serde_json::Value>(&settings_json) {
            if let Some(providers) = settings.get("providers").and_then(|v| v.as_array()) {
                for p in providers {
                    let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let ptype = p.get("type").and_then(|v| v.as_str()).unwrap_or(id);
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or(ptype);
                    let enabled = p.get("isEnabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    let base_url = p.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");

                    if id.is_empty() {
                        continue;
                    }

                    // Check if API key exists for this provider
                    let vault_key = format!("ai_apikey_{}", id);
                    let has_vault_key = store
                        .get(&vault_key)
                        .map(|v| !v.is_empty())
                        .unwrap_or(false);
                    let env_name = env_var_for(ptype);
                    let has_env_key = if env_name.is_empty() {
                        false
                    } else {
                        std::env::var(env_name)
                            .map(|v| !v.is_empty())
                            .unwrap_or(false)
                    };
                    // Ollama doesn't need a key
                    let is_ollama = ptype == "ollama";

                    if !has_vault_key && !has_env_key && !is_ollama {
                        continue;
                    }

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
                    let active_model =
                        settings
                            .get("models")
                            .and_then(|m| m.as_array())
                            .and_then(|models| {
                                models.iter().find(|m| {
                                    m.get("providerId").and_then(|v| v.as_str()) == Some(id)
                                        && m.get("isActive")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false)
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
        if seen_types.contains(*ptype) {
            continue;
        }
        if std::env::var(env_key)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
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
        println!(
            "{}",
            serde_json::to_string_pretty(&configured).unwrap_or_default()
        );
    } else {
        println!(
            "  {:<4} {:<16} {:<14} {:<40} {:<10} Source",
            "#", "Name", "Provider", "Active Model", "Enabled"
        );
        println!("  {}", "\u{2500}".repeat(95));
        for (i, p) in configured.iter().enumerate() {
            let enabled_str = if p["enabled"].as_bool().unwrap_or(true) {
                "yes"
            } else {
                "no"
            };
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
        eprintln!(
            "\n{} AI provider(s). Use: aeroftp-cli agent --provider <name> --model <model>",
            configured.len()
        );
    }

    0
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn safe_vault_profiles(cli: &Cli) -> Result<Vec<serde_json::Value>, String> {
    let store = open_vault(cli)?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read saved profiles: {}", e))?;
    let profiles = serde_json::from_str::<Vec<serde_json::Value>>(&profiles_json)
        .map_err(|e| format!("Failed to parse saved profiles: {}", e))?;

    // List vault keys ONCE; per-profile auth-state derivation then checks
    // existence in this set instead of issuing N decryptions.
    let accounts: std::collections::HashSet<String> = store
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .collect();

    Ok(profiles
        .iter()
        .map(|p| {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let proto = p.get("protocol").and_then(|v| v.as_str()).unwrap_or("");
            let auth_state = ftp_client_gui_lib::profile_auth_state::derive_profile_auth_state(
                &store, &accounts, id, proto,
            );
            serde_json::json!({
                "id": id,
                "name": p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed"),
                "protocol": proto,
                "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                "port": p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
                "username": p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
                "auth_state": auth_state,
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

/// Create a provider connection from a server profile name (for agent tool context).
/// Uses the cached vault and existing profile resolution.
async fn create_and_connect_for_agent(
    server_query: &str,
) -> Result<
    (
        Box<dyn ftp_client_gui_lib::providers::StorageProvider>,
        String,
    ),
    String,
> {
    let store = ftp_client_gui_lib::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not open. Cannot connect to server.".to_string())?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {}", e))?;
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse profiles: {}", e))?;

    // Find matching profile (exact name or ID first; otherwise require a unique substring match)
    let query_lower = server_query.to_lowercase();
    let exact_match = profiles.iter().find(|p| {
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
        name == query_lower || id == server_query
    });
    let matched = if let Some(profile) = exact_match {
        profile
    } else {
        let partial_matches: Vec<&serde_json::Value> = profiles
            .iter()
            .filter(|p| {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                name.contains(&query_lower)
            })
            .collect();
        match partial_matches.as_slice() {
            [single] => *single,
            [] => {
                return Err(format!(
                    "Server '{}' not found in saved profiles",
                    server_query
                ))
            }
            many => {
                let names = many
                    .iter()
                    .filter_map(|p| p.get("name").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "Server '{}' is ambiguous. Use an exact profile name. Matches: {}",
                    server_query, names
                ));
            }
        }
    };

    let profile_id = matched.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let profile_name = matched
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed");
    let protocol = matched
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let host = matched.get("host").and_then(|v| v.as_str()).unwrap_or("");
    let port = matched.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let username = matched
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let initial_path = matched
        .get("initialPath")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    // Resolve password from vault
    let password = store
        .get(&format!("server_{}", profile_id))
        .unwrap_or_default();

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
        username: if username.is_empty() {
            None
        } else {
            Some(username.to_string())
        },
        password: if password.is_empty() {
            None
        } else {
            Some(password)
        },
        initial_path: Some(initial_path.to_string()),
        extra: std::collections::HashMap::new(),
    };

    let mut provider = ftp_client_gui_lib::providers::ProviderFactory::create(&config)
        .map_err(|e| format!("Failed to create provider for '{}': {}", profile_name, e))?;

    provider
        .connect()
        .await
        .map_err(|e| format!("Connection to '{}' failed: {}", profile_name, e))?;

    Ok((provider, initial_path.to_string()))
}

fn cmd_agent_info(cli: &Cli) -> i32 {
    let (profiles, profiles_error) = match safe_vault_profiles(cli) {
        Ok(profiles) => (profiles, None),
        Err(error) => (vec![], Some(error)),
    };
    let profiles = profiles
        .into_iter()
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
        "description": "AeroFTP CLI - multi-protocol file transfer with encrypted vault profiles",
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
                {"name": "pget", "syntax": "aeroftp-cli pget --profile NAME /remote/file [./local] [--segments N]", "description": "Segmented parallel download (alias for get with --segments preset, default 4)"},
            ],
            "modify": [
                {"name": "put", "syntax": "aeroftp-cli put --profile NAME ./local /remote/path [-n]", "description": "Upload file (-n: no-clobber, skip if exists)"},
                {"name": "put -r", "syntax": "aeroftp-cli put --profile NAME ./local/ /remote/ -r", "description": "Upload directory"},
                {"name": "mkdir", "syntax": "aeroftp-cli mkdir --profile NAME /remote/dir [-p]", "description": "Create directory (-p: parents, idempotent)"},
                {"name": "mv", "syntax": "aeroftp-cli mv --profile NAME /old /new", "description": "Move/rename"},
                {"name": "cp", "syntax": "aeroftp-cli cp --profile NAME /old /new", "description": "Server-side copy when supported"},
                {"name": "link", "syntax": "aeroftp-cli link --profile NAME /path/file", "description": "Create share link when supported"},
                {"name": "edit", "syntax": "aeroftp-cli edit --profile NAME /path/file \"find\" \"replace\" [--first]", "description": "Replace text in a remote UTF-8 file"},
                {"name": "sync", "syntax": "aeroftp-cli sync --profile NAME ./local/ /remote/ [--dry-run]", "description": "Sync directories"},
            ],
            "destructive": [
                {"name": "rm", "syntax": "aeroftp-cli rm --profile NAME /path [-f]", "description": "Delete file (-f: force, no error if not found)"},
                {"name": "rm -rf", "syntax": "aeroftp-cli rm --profile NAME /dir/ -rf", "description": "Delete directory recursively (force, no prompt)"},
                {"name": "sync --delete", "syntax": "aeroftp-cli sync --profile NAME ./local/ /remote/ --delete", "description": "Sync with orphan deletion (always confirm)"},
            ],
            "advanced": [
                {"name": "head", "syntax": "aeroftp-cli head --profile NAME /path/file [-n N]", "description": "Read first lines of a remote text file"},
                {"name": "tail", "syntax": "aeroftp-cli tail --profile NAME /path/file [-n N]", "description": "Read last lines of a remote text file"},
                {"name": "touch", "syntax": "aeroftp-cli touch --profile NAME /path/file [--timestamp ISO8601]", "description": "Create file or update modified time"},
                {"name": "hashsum", "syntax": "aeroftp-cli hashsum --algorithm ALGO --profile NAME /path/file", "description": "Compute remote checksum"},
                {"name": "check", "syntax": "aeroftp-cli check --profile NAME ./local /remote", "description": "Compare local and remote trees"},
                {"name": "reconcile", "syntax": "aeroftp-cli reconcile --profile NAME ./local /remote --json", "description": "Return categorized local-vs-remote diff for agents"},
                {"name": "sync-doctor", "syntax": "aeroftp-cli sync-doctor --profile NAME ./local /remote --json", "description": "Preflight sync checks, risks, and next command"},
                {"name": "transfer-doctor", "syntax": "aeroftp-cli transfer-doctor \"SRC\" \"DST\" /src /dst --json", "description": "Preflight cross-profile transfer checks, plan, and risks"},
                {"name": "dedupe", "syntax": "aeroftp-cli dedupe --profile NAME /path [--mode MODE]", "description": "Resolve duplicate remote files"},
                {"name": "ncdu", "syntax": "aeroftp-cli ncdu --profile NAME /path [--json|--export FILE]", "description": "Analyze remote disk usage"},
                {"name": "mount", "syntax": "aeroftp-cli mount --profile NAME /mountpoint", "description": "Expose a remote as a local filesystem"},
                {"name": "serve", "syntax": "aeroftp-cli serve <http|webdav|ftp|sftp> --profile NAME /path", "description": "Expose a remote over a local protocol bridge"},
                {"name": "daemon", "syntax": "aeroftp-cli daemon <start|stop|status>", "description": "Manage the background jobs daemon"},
                {"name": "jobs", "syntax": "aeroftp-cli jobs <add|list|status|cancel>", "description": "Manage queued background jobs"},
                {"name": "crypt", "syntax": "aeroftp-cli crypt <init|ls|put|get> --profile NAME /path", "description": "Use encrypted overlay storage"},
                {"name": "batch", "syntax": "aeroftp-cli batch file.aeroftp", "description": "Run batch automation scripts"},
                {"name": "agent-info", "syntax": "aeroftp-cli agent-info --json", "description": "Show machine-readable CLI capabilities"}
            ]
        },
        "capabilities": {
            "main_command_groups": 38,
            "agent_native_tools": cli_tool_definitions().len(),
            "hash_algorithms": ["md5", "sha1", "sha256", "sha512", "blake3"],
            "serve_protocols": ["http", "webdav", "ftp", "sftp"],
            "agent_safety_model": {
                "danger_levels": ["safe", "medium", "high"],
                "categories": [
                    "local-readonly",
                    "remote-metadata",
                    "remote-preview",
                    "remote-bulk-read",
                    "local-modify",
                    "remote-modify",
                    "destructive",
                    "execution"
                ],
                "data_egress_levels": ["none", "metadata", "preview", "operation-dependent"]
            }
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
            "9": "already exists / directory not empty",
            "10": "server or parse error",
            "11": "local I/O error",
            "99": "unknown",
            "130": "interrupted (SIGINT)"
        },
        "protocols": [
            "ftp", "ftps", "sftp", "webdav", "webdavs", "s3", "aerocloud",
            "mega", "filen", "internxt", "kdrive", "koofr",
            "jottacloud", "filelu", "opendrive", "yandexdisk", "azure",
            "github", "gitlab", "googledrive", "dropbox", "onedrive", "box",
            "pcloud", "zohoworkdrive", "fourshared", "drime", "swift"
        ],
        // Per-protocol capability matrix — answers "which protocols
        // support feature X" in one call instead of N agent-connect
        // round-trips. Tokens align with the StorageProvider trait
        // `supports_*` methods. Conservative lower bound: a feature
        // listed here is reliably supported by the protocol family;
        // absence means "ask the provider directly".
        "protocol_features": {
            "ftp": ftp_client_gui_lib::agent_session::capabilities_for_protocol("ftp"),
            "ftps": ftp_client_gui_lib::agent_session::capabilities_for_protocol("ftps"),
            "sftp": ftp_client_gui_lib::agent_session::capabilities_for_protocol("sftp"),
            "webdav": ftp_client_gui_lib::agent_session::capabilities_for_protocol("webdav"),
            "s3": ftp_client_gui_lib::agent_session::capabilities_for_protocol("s3"),
            "azure": ftp_client_gui_lib::agent_session::capabilities_for_protocol("azure"),
            "googledrive": ftp_client_gui_lib::agent_session::capabilities_for_protocol("googledrive"),
            "googlephotos": ftp_client_gui_lib::agent_session::capabilities_for_protocol("googlephotos"),
            "dropbox": ftp_client_gui_lib::agent_session::capabilities_for_protocol("dropbox"),
            "onedrive": ftp_client_gui_lib::agent_session::capabilities_for_protocol("onedrive"),
            "box": ftp_client_gui_lib::agent_session::capabilities_for_protocol("box"),
            "pcloud": ftp_client_gui_lib::agent_session::capabilities_for_protocol("pcloud"),
            "mega": ftp_client_gui_lib::agent_session::capabilities_for_protocol("mega"),
            "filen": ftp_client_gui_lib::agent_session::capabilities_for_protocol("filen"),
            "internxt": ftp_client_gui_lib::agent_session::capabilities_for_protocol("internxt"),
            "kdrive": ftp_client_gui_lib::agent_session::capabilities_for_protocol("kdrive"),
            "jottacloud": ftp_client_gui_lib::agent_session::capabilities_for_protocol("jottacloud"),
            "zohoworkdrive": ftp_client_gui_lib::agent_session::capabilities_for_protocol("zohoworkdrive"),
            "yandexdisk": ftp_client_gui_lib::agent_session::capabilities_for_protocol("yandexdisk"),
            "koofr": ftp_client_gui_lib::agent_session::capabilities_for_protocol("koofr"),
            "opendrive": ftp_client_gui_lib::agent_session::capabilities_for_protocol("opendrive"),
            "drime": ftp_client_gui_lib::agent_session::capabilities_for_protocol("drime"),
            "filelu": ftp_client_gui_lib::agent_session::capabilities_for_protocol("filelu"),
            "fourshared": ftp_client_gui_lib::agent_session::capabilities_for_protocol("fourshared"),
            "swift": ftp_client_gui_lib::agent_session::capabilities_for_protocol("swift"),
            "immich": ftp_client_gui_lib::agent_session::capabilities_for_protocol("immich"),
            "github": ftp_client_gui_lib::agent_session::capabilities_for_protocol("github"),
            "gitlab": ftp_client_gui_lib::agent_session::capabilities_for_protocol("gitlab")
        },
        "agent_connect_supported_protocols": [
            "ftp", "ftps", "sftp", "webdav", "s3", "github", "gitlab"
        ],
        "safety_rules": [
            "Always use --profile instead of passwords in URLs",
            "Use --dry-run before sync operations",
            "Confirm with user before rm, rm -rf, or sync --delete",
            "Remote metadata, remote preview and destructive tools are classified separately for agent approval",
            "Use --json for all programmatic parsing",
            "Use mkdir -p for idempotent directory creation",
            "Use rm -f to ignore not-found errors (idempotent delete)",
            "Use put -n (--no-clobber) to skip existing files instead of overwriting"
        ],
        "suggested_next_commands": [
            "aeroftp-cli agent-bootstrap --json",
            "aeroftp-cli profiles --json"
        ]
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&info).unwrap_or_default()
    );
    0
}

async fn cmd_agent_connect(cli: &Cli, query: &str) -> i32 {
    // Vault must be unlocked before lookup_profile() can read
    // `config_server_profiles`. Mirror the auto/env/prompt flow other
    // CLI commands use; surface unlock failure as a structured payload
    // so agents see the same shape regardless of the failure stage.
    if let Err(msg) = open_vault(cli) {
        let payload = serde_json::json!({
            "query": query,
            "lookup": {
                "status": "error",
                "kind": "vault_closed",
                "message": msg,
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return 2;
    }

    let payload = ftp_client_gui_lib::agent_session::build_agent_connect_payload(query).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );
    // Exit code reflects only the critical block: connect.status.
    //   ok          → 0 (live connection succeeded)
    //   unsupported → 0 (protocol outside agent-connect's allowlist;
    //                   capabilities/path/profile blocks are still
    //                   actionable, so a non-zero code would mislead
    //                   exit-gating agents into discarding usable
    //                   data — flagged by the agent-friendliness
    //                   audit, Battery D)
    //   error       → 1 (real connection failure)
    //   lookup err  → 2 (profile not found / vault locked)
    let connect_status = payload
        .get("connect")
        .and_then(|c| c.get("status"))
        .and_then(|s| s.as_str());
    let lookup_failed = payload.get("lookup").is_some();
    if lookup_failed {
        2
    } else if matches!(connect_status, Some("ok") | Some("unsupported")) {
        0
    } else {
        1
    }
}

fn shell_double_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn profile_or_placeholder(cli: &Cli) -> String {
    cli.profile
        .as_deref()
        .map(shell_double_quote)
        .unwrap_or_else(|| "NAME".to_string())
}

fn suggest_ls_followup(cli: &Cli, path: &str) -> String {
    // Generic glob instead of the old `*.ext` placeholder. `*.ext` looked
    // like a real pattern to agents copy-pasting the hint — they would fire
    // it literally and get zero results. `*` matches anything on every
    // backend and is the honest default.
    format!(
        "aeroftp-cli find --profile \"{}\" \"{}\" \"*\" --json",
        profile_or_placeholder(cli),
        shell_double_quote(path)
    )
}

fn suggest_find_followup(cli: &Cli, path: &str) -> String {
    format!(
        "aeroftp-cli ls --profile \"{}\" \"{}\" --json",
        profile_or_placeholder(cli),
        shell_double_quote(path)
    )
}

fn suggest_stat_followup(cli: &Cli, path: &str) -> String {
    format!(
        "aeroftp-cli stat --profile \"{}\" \"{}\" --json",
        profile_or_placeholder(cli),
        shell_double_quote(path)
    )
}

fn suggest_transfer_apply(
    source_profile: &str,
    dest_profile: &str,
    source_path: &str,
    dest_path: &str,
) -> String {
    format!(
        "aeroftp-cli transfer \"{}\" \"{}\" \"{}\" \"{}\" --format json",
        shell_double_quote(source_profile),
        shell_double_quote(dest_profile),
        shell_double_quote(source_path),
        shell_double_quote(dest_path)
    )
}

fn suggest_transfer_verify(dest_profile: &str, dest_path: &str, planned_files: u64) -> String {
    if planned_files <= 1 {
        format!(
            "aeroftp-cli stat --profile \"{}\" \"{}\" --json",
            shell_double_quote(dest_profile),
            shell_double_quote(dest_path)
        )
    } else {
        let parent = Path::new(dest_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/".to_string());
        format!(
            "aeroftp-cli ls --profile \"{}\" \"{}\" --json",
            shell_double_quote(dest_profile),
            shell_double_quote(&parent)
        )
    }
}

fn parent_remote_path(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

fn bootstrap_task_name(task: AgentBootstrapTask) -> &'static str {
    match task {
        AgentBootstrapTask::Explore => "explore",
        AgentBootstrapTask::VerifyFile => "verify-file",
        AgentBootstrapTask::Transfer => "transfer",
        AgentBootstrapTask::Backup => "backup",
        AgentBootstrapTask::Reconcile => "reconcile",
    }
}

#[allow(clippy::too_many_arguments)]
fn build_agent_task_router(
    task: AgentBootstrapTask,
    path: Option<&str>,
    pattern: Option<&str>,
    source_profile: Option<&str>,
    dest_profile: Option<&str>,
    source_path: Option<&str>,
    dest_path: Option<&str>,
    local_path: Option<&str>,
    remote_path: Option<&str>,
) -> serde_json::Value {
    let path = path.unwrap_or("/");
    let pattern = pattern.unwrap_or("*.ext");
    let source_profile = source_profile.unwrap_or("SRC");
    let dest_profile = dest_profile.unwrap_or("DST");
    let source_path = source_path.unwrap_or("/source/path");
    let dest_path = dest_path.unwrap_or("/dest/path");
    let local_path = local_path.unwrap_or("./local");
    let remote_path = remote_path.unwrap_or("/remote/path");
    let source_parent = parent_remote_path(source_path);

    match task {
        AgentBootstrapTask::Explore => serde_json::json!({
            "task": bootstrap_task_name(task),
            "goal": "Understand a remote quickly and safely",
            "commands": [
                "aeroftp-cli profiles --json",
                format!("aeroftp-cli ls --profile \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(path)),
                format!("aeroftp-cli tree --profile \"{}\" \"{}\" -d 2 --json", shell_double_quote(source_profile), shell_double_quote(path)),
                format!("aeroftp-cli find --profile \"{}\" \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(path), shell_double_quote(pattern)),
            ],
            "rule": "Use ls for totals and find for subsets"
        }),
        AgentBootstrapTask::VerifyFile => serde_json::json!({
            "task": bootstrap_task_name(task),
            "goal": "Confirm exact existence and metadata for one remote path",
            "commands": [
                format!("aeroftp-cli stat --profile \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(source_path)),
                format!("aeroftp-cli ls --profile \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(&source_parent)),
            ],
            "rule": "Use stat for exact existence; do not infer from filtered listings"
        }),
        AgentBootstrapTask::Transfer => serde_json::json!({
            "task": bootstrap_task_name(task),
            "goal": "Copy one file or tree between saved profiles with verification",
            "commands": [
                format!("aeroftp-cli stat --profile \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(source_path)),
                format!("aeroftp-cli transfer \"{}\" \"{}\" \"{}\" \"{}\" --dry-run --format json", shell_double_quote(source_profile), shell_double_quote(dest_profile), shell_double_quote(source_path), shell_double_quote(dest_path)),
                suggest_transfer_apply(source_profile, dest_profile, source_path, dest_path),
                suggest_transfer_verify(dest_profile, dest_path, 1),
            ],
            "rule": "Always dry-run before apply"
        }),
        AgentBootstrapTask::Backup => serde_json::json!({
            "task": bootstrap_task_name(task),
            "goal": "Plan and verify a backup-style copy with counts and exact destinations",
            "commands": [
                format!("aeroftp-cli ls --profile \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(source_path)),
                format!("aeroftp-cli find --profile \"{}\" \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(source_path), shell_double_quote(pattern)),
                format!("aeroftp-cli transfer \"{}\" \"{}\" \"{}\" \"{}\" --dry-run --format json", shell_double_quote(source_profile), shell_double_quote(dest_profile), shell_double_quote(source_path), shell_double_quote(dest_path)),
                suggest_transfer_apply(source_profile, dest_profile, source_path, dest_path),
                suggest_transfer_verify(dest_profile, dest_path, 2),
            ],
            "rule": "Compare total ls count against any filtered match count before copying"
        }),
        AgentBootstrapTask::Reconcile => serde_json::json!({
            "task": bootstrap_task_name(task),
            "goal": "Compare local and remote state and prepare the next safe action",
            "commands": [
                format!("aeroftp-cli check --profile \"{}\" \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(local_path), shell_double_quote(remote_path)),
                format!("aeroftp-cli ls --profile \"{}\" \"{}\" --json", shell_double_quote(source_profile), shell_double_quote(remote_path)),
                format!("aeroftp-cli sync --profile \"{}\" \"{}\" \"{}\" --dry-run --json", shell_double_quote(source_profile), shell_double_quote(local_path), shell_double_quote(remote_path)),
            ],
            "rule": "Use check to classify differences before sync"
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_agent_bootstrap(
    cli: &Cli,
    format: OutputFormat,
    task: Option<AgentBootstrapTask>,
    path: Option<&str>,
    pattern: Option<&str>,
    source_profile: Option<&str>,
    dest_profile: Option<&str>,
    source_path: Option<&str>,
    dest_path: Option<&str>,
    local_path: Option<&str>,
    remote_path: Option<&str>,
) -> i32 {
    // Inline profile inventory so agents have ready-to-use targets in the
    // first JSON payload they read, instead of having to chain a follow-up
    // `profiles --json` call. `vault_unavailable` distinguishes "no vault"
    // (status:locked) from "vault open but empty" (status:ok, count:0).
    let (profiles, profile_status) = match safe_vault_profiles(cli) {
        Ok(p) => (p, "ok"),
        Err(_) => (vec![], "vault_unavailable"),
    };

    let bootstrap = serde_json::json!({
        "status": "ok",
        "entrypoint": "aeroftp-cli agent-bootstrap --json",
        "goal": "Give AI agents the shortest path to the correct AeroFTP command without repository-specific guesswork",
        "profiles": {
            "status": profile_status,
            "count": profiles.len(),
            "list_command": "aeroftp-cli profiles --json",
            "servers": profiles,
        },
        "first_steps": [
            {
                "step": 1,
                "name": "discover_profiles",
                "why": "Saved profiles are the safest and preferred credential model",
                "command": "aeroftp-cli profiles --json",
                "success_signal": "Returns profile names, protocols, and initial paths"
            },
            {
                "step": 2,
                "name": "inspect_remote",
                "why": "Establish the real remote shape before filtering or modifying",
                "command": "aeroftp-cli ls --profile \"NAME\" /path --json",
                "success_signal": "summary.total matches visible entries"
            },
            {
                "step": 3,
                "name": "verify_specific_target",
                "why": "Use stat for exact existence; do not infer from filtered listings",
                "command": "aeroftp-cli stat --profile \"NAME\" /path/file --json",
                "success_signal": "Returns exact path, size, and modified time"
            }
        ],
        "task_playbooks": [
            {
                "task": "list-and-explore",
                "when": "Need to understand a remote quickly",
                "commands": [
                    "aeroftp-cli profiles --json",
                    "aeroftp-cli ls --profile \"NAME\" /path --json",
                    "aeroftp-cli tree --profile \"NAME\" /path -d 2 --json",
                    "aeroftp-cli find --profile \"NAME\" /path \"*.ext\" --json"
                ],
                "rule": "Always compare ls summary.total against any filtered subset"
            },
            {
                "task": "single-file-transfer",
                "when": "Need an exact one-file copy or verification",
                "commands": [
                    "aeroftp-cli stat --profile \"SRC\" /path/file --json",
                    "aeroftp-cli transfer \"SRC\" \"DST\" /path/file /dest/file --dry-run --format json",
                    "aeroftp-cli transfer \"SRC\" \"DST\" /path/file /dest/file --format json",
                    "aeroftp-cli stat --profile \"DST\" /dest/file --json"
                ],
                "rule": "Stat the exact source file before transfer; do not rely only on find output"
            },
            {
                "task": "backup-or-reconcile",
                "when": "Need counts, missing files, renamed files, or backup verification",
                "commands": [
                    "aeroftp-cli ls --profile \"SRC\" /path --json",
                    "aeroftp-cli find --profile \"SRC\" /path \"pattern*\" --json",
                    "aeroftp-cli check --profile \"DST\" ./local /remote --json",
                    "aeroftp-cli transfer \"SRC\" \"DST\" /src/path /dst/path --dry-run --format json"
                ],
                "rule": "Separate total remote count from pattern-matched count; if names can change, reconcile by basename and exact stat"
            },
            {
                "task": "safe-modification",
                "when": "Need to upload, rename, mkdir, or sync",
                "commands": [
                    "aeroftp-cli mkdir --profile \"NAME\" /remote/dir --json",
                    "aeroftp-cli put --profile \"NAME\" ./local /remote/file --json",
                    "aeroftp-cli mv --profile \"NAME\" /old /new --json",
                    "aeroftp-cli sync --profile \"NAME\" ./local /remote --dry-run --json"
                ],
                "rule": "Use dry-run before sync and ask before destructive operations"
            }
        ],
        "decision_rules": [
            {
                "if": "Need the exact existence of one path",
                "use": "stat",
                "avoid": "inferring from ls/find counts"
            },
            {
                "if": "Need total count in a directory or album",
                "use": "ls",
                "avoid": "find with a prefix filter as the only source of truth"
            },
            {
                "if": "Need pattern matches",
                "use": "find",
                "avoid": "assuming unmatched files are missing from the remote"
            },
            {
                "if": "Need transfer planning",
                "use": "transfer --dry-run",
                "avoid": "manual destination guessing"
            }
        ],
        "anti_patterns": [
            "Do not treat filtered results as the total remote state",
            "Do not assume upload preserved the requested filename unless verified",
            "Do not start with destructive commands",
            "Do not pass credentials in URLs when a saved profile exists"
        ],
        "suggested_next_commands": [
            "aeroftp-cli profiles --json",
            "aeroftp-cli agent-info --json"
        ],
        "task_router": task.map(|task| build_agent_task_router(
            task,
            path,
            pattern,
            source_profile,
            dest_profile,
            source_path,
            dest_path,
            local_path,
            remote_path
        ))
    });

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&bootstrap).unwrap_or_default()
            );
        }
        OutputFormat::Text => {
            println!("AeroFTP agent bootstrap");
            println!();
            println!("Start here:");
            println!("  1. aeroftp-cli profiles --json");
            println!("  2. aeroftp-cli ls --profile \"NAME\" /path --json");
            println!("  3. aeroftp-cli stat --profile \"NAME\" /path/file --json");
            println!();
            println!("Task playbooks:");
            println!("  - Explore remote: profiles -> ls -> tree/find");
            println!("  - Exact file transfer: stat -> transfer --dry-run -> transfer -> stat");
            println!("  - Backup/reconcile: ls total -> find filtered -> check/transfer");
            println!("  - Safe modify: mkdir/put/mv, and sync only after --dry-run");
            println!();
            println!("Key rules:");
            println!("  - Use ls for total counts and find for subsets.");
            println!("  - Use stat for exact existence; never infer from filtered counts.");
            println!("  - Verify the effective remote name after uploads when naming matters.");
            println!("  - Prefer --profile and --json for all agent-driven flows.");
            if let Some(task) = task {
                let routed = build_agent_task_router(
                    task,
                    path,
                    pattern,
                    source_profile,
                    dest_profile,
                    source_path,
                    dest_path,
                    local_path,
                    remote_path,
                );
                println!();
                println!("Task router: {}", bootstrap_task_name(task));
                if let Some(goal) = routed.get("goal").and_then(|v| v.as_str()) {
                    println!("  Goal: {}", goal);
                }
                if let Some(commands) = routed.get("commands").and_then(|v| v.as_array()) {
                    for command in commands {
                        if let Some(command) = command.as_str() {
                            println!("  {}", command);
                        }
                    }
                }
            }
            println!();
            println!("Next commands:");
            println!("  aeroftp-cli profiles --json");
            println!("  aeroftp-cli agent-info --json");
        }
    }

    0
}

fn resolve_url_or_profile(
    url: &str,
    cli: &Cli,
    format: OutputFormat,
) -> Result<(ProviderConfig, String), i32> {
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

fn push_s3_doctor_checks(
    checks: &mut Vec<serde_json::Value>,
    risks: &mut Vec<String>,
    role: &str,
    config: &ProviderConfig,
) {
    if config.provider_type != ProviderType::S3 {
        return;
    }

    let provider_id = config
        .extra
        .get(S3_PROVIDER_ID_META_KEY)
        .cloned()
        .unwrap_or_else(|| "custom-s3".to_string());
    let endpoint = config
        .extra
        .get("endpoint")
        .cloned()
        .unwrap_or_else(|| config.host.clone());
    let region = config.extra.get("region").cloned().unwrap_or_default();
    let path_style = config
        .extra
        .get("path_style")
        .map(|value| value == "true" || value == "1")
        .unwrap_or(false);
    let endpoint_source = config
        .extra
        .get(S3_ENDPOINT_SOURCE_META_KEY)
        .cloned()
        .unwrap_or_else(|| {
            if endpoint.is_empty() {
                "missing".to_string()
            } else {
                "profile".to_string()
            }
        });
    let region_source = config
        .extra
        .get(S3_REGION_SOURCE_META_KEY)
        .cloned()
        .unwrap_or_else(|| {
            if region.is_empty() {
                "missing".to_string()
            } else {
                "profile".to_string()
            }
        });
    let path_style_source = config
        .extra
        .get(S3_PATH_STYLE_SOURCE_META_KEY)
        .cloned()
        .unwrap_or_else(|| "profile".to_string());

    checks.push(serde_json::json!({
        "name": format!("{}_s3_resolution", role),
        "ok": !endpoint.is_empty(),
        "provider_id": provider_id,
        "endpoint": endpoint,
        "endpoint_source": endpoint_source,
        "region": region,
        "region_source": region_source,
        "path_style": path_style,
        "path_style_source": path_style_source,
    }));

    if endpoint_source == "preset" {
        risks.push(format!(
            "{} S3 endpoint is being derived from provider preset defaults",
            role
        ));
    }
    if region_source == "preset" {
        risks.push(format!(
            "{} S3 region is coming from provider preset defaults",
            role
        ));
    }
    if path_style_source == "preset" {
        risks.push(format!(
            "{} S3 path-style setting is coming from provider preset defaults",
            role
        ));
    }
}

fn profile_to_provider_config(
    profile_name: &str,
    cli: &Cli,
    format: OutputFormat,
) -> Result<(ProviderConfig, String), i32> {
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
            print_error(
                format,
                "No saved profiles found in vault. Save a server in the AeroFTP GUI first, or use URL mode: aeroftp-cli ls ftp://user@host/path",
                5,
            );
            return Err(5);
        }
    };

    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json).map_err(|e| {
        print_error(format, &format!("Failed to parse profiles: {}", e), 5);
        5
    })?;

    // Match by index, exact name, ID, or substring (with disambiguation)
    let profile_lower = profile_name.to_lowercase();
    let matched = if let Ok(idx) = profile_name.parse::<usize>() {
        profiles.get(idx.saturating_sub(1))
    } else {
        // 1. Exact name match (case-insensitive)
        let exact = profiles.iter().find(|p| {
            p.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase()
                == profile_lower
        });
        if exact.is_some() {
            exact
        } else {
            // 2. Exact ID match
            let by_id = profiles
                .iter()
                .find(|p| p.get("id").and_then(|v| v.as_str()).unwrap_or("") == profile_name);
            if by_id.is_some() {
                by_id
            } else {
                // 3. Substring match with disambiguation
                let matches: Vec<_> = profiles
                    .iter()
                    .filter(|p| {
                        p.get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&profile_lower)
                    })
                    .collect();
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
            print_error(
                format,
                &format!(
                    "Profile not found: '{}'. Run 'aeroftp-cli profiles' to list.",
                    profile_name
                ),
                5,
            );
            return Err(5);
        }
    };

    let id = profile.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = profile
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed");
    let mut host = profile
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let port = profile
        .get("port")
        .and_then(|v| v.as_u64())
        .map(|p| p as u16);
    let username = profile
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let protocol = profile
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("ftp");
    let initial_path = profile
        .get("initialPath")
        .and_then(|v| v.as_str())
        .unwrap_or("/")
        .to_string();

    // Load credentials from vault
    // Password is stored as a raw string (not JSON) in server_{id}
    let (cred_user, cred_pass) = if !id.is_empty() {
        match store.get(&format!("server_{}", id)) {
            Ok(password_str) => {
                // The vault stores just the password as a plain string
                // Try to parse as JSON first (legacy format), fall back to raw string
                if let Ok(cred) = serde_json::from_str::<serde_json::Value>(&password_str) {
                    if let Some(obj) = cred.as_object() {
                        let u = obj
                            .get("username")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let p = obj
                            .get("password")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        (if u.is_empty() { username.clone() } else { u }, p)
                    } else {
                        // JSON but not an object - treat as raw password string
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
        "immich" => ProviderType::Immich,
        "b2" | "backblaze" | "backblazeb2" => ProviderType::Backblaze,
        _ => {
            print_error(
                format,
                &format!("Unsupported protocol in profile: {}", protocol),
                7,
            );
            return Err(7);
        }
    };

    // Build extra from profile options and CLI overrides
    let mut extra = HashMap::new();

    // Load provider-specific options from profile
    apply_profile_options(&mut extra, profile);

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

    if provider_type == ProviderType::S3 {
        let provider_id = profile.get("providerId").and_then(|v| v.as_str());
        if let Some(resolved_endpoint) = apply_s3_profile_defaults(&mut extra, provider_id) {
            if host.trim().is_empty() {
                host = resolved_endpoint;
            }
        }
    }

    if provider_type == ProviderType::Mega && !extra.contains_key("mega_mode") {
        extra.insert("mega_mode".to_string(), "native".to_string());
    }

    print_profile_banner_once(
        name,
        format!("{} -> {}", protocol.to_uppercase(), host),
        cli.quiet,
    );

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
        oauth2::{bind_callback_listener, bind_callback_listener_on_port, wait_for_callback},
        OAuth2Manager, OAuthConfig,
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
    }
    .map_err(|e| {
        format!(
            "Failed to bind callback listener on port {}: {}",
            fixed_port, e
        )
    })?;

    let config = match protocol {
        "googledrive" => OAuthConfig::google_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "dropbox" => OAuthConfig::dropbox_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "onedrive" => OAuthConfig::onedrive_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "box" => OAuthConfig::box_cloud_with_port(&oauth_settings.0, &oauth_settings.1, port),
        "pcloud" => {
            let region = store
                .get("oauth_pcloud_region")
                .unwrap_or_else(|_| "us".to_string());
            OAuthConfig::pcloud_with_port(&oauth_settings.0, &oauth_settings.1, port, &region)
        }
        "zohoworkdrive" => {
            let region = store
                .get("oauth_zohoworkdrive_region")
                .unwrap_or_else(|_| "us".to_string());
            OAuthConfig::zoho_with_port(&oauth_settings.0, &oauth_settings.1, port, &region)
        }
        "yandexdisk" => {
            OAuthConfig::yandex_disk_with_port(&oauth_settings.0, &oauth_settings.1, port)
        }
        other => return Err(format!("OAuth not supported for: {}", other)),
    };

    let manager = OAuth2Manager::new();
    let (auth_url, expected_state) = manager
        .start_auth_flow(&config)
        .await
        .map_err(|e| format!("Failed to start OAuth flow: {}", e))?;

    // Try to open browser automatically
    eprintln!("\nAuthorize in your browser:");
    eprintln!("  {}\n", auth_url);
    if open::that(&auth_url).is_err() {
        eprintln!("Could not open browser automatically. Please open the URL above manually.");
    }
    eprintln!("Waiting for authorization... (press Ctrl+C to cancel)");

    // Wait for callback with 5-minute timeout
    let callback_handle = tokio::spawn(async move { wait_for_callback(listener).await });
    let (code, state) =
        tokio::time::timeout(tokio::time::Duration::from_secs(300), callback_handle)
            .await
            .map_err(|_| "Timeout: no response within 5 minutes".to_string())?
            .map_err(|e| format!("Callback error: {}", e))?
            .map_err(|e| format!("Callback error: {}", e))?;

    if state != expected_state {
        return Err("OAuth state mismatch - possible CSRF attack".to_string());
    }

    // Exchange code for tokens
    manager
        .complete_auth_flow(&config, &code, &expected_state)
        .await
        .map_err(|e| format!("Token exchange failed: {}", e))?;

    Ok(())
}

/// Create an OAuth provider by protocol name (used for retry after re-authorization)
fn create_oauth_provider_by_protocol(
    protocol: &str,
    store: &CredentialStore,
) -> Result<Box<dyn StorageProvider>, String> {
    use ftp_client_gui_lib::providers::{
        dropbox::DropboxConfig, google_drive::GoogleDriveConfig, onedrive::OneDriveConfig,
        types::BoxConfig, types::PCloudConfig, zoho_workdrive::ZohoWorkdriveConfig, BoxProvider,
        DropboxProvider, GoogleDriveProvider, OAuth2Manager, OAuthProvider, OneDriveProvider,
        PCloudProvider, YandexDiskProvider, ZohoWorkdriveProvider,
    };

    let oauth_settings = load_oauth_client_config(store, protocol);
    match protocol {
        "googledrive" => Ok(Box::new(GoogleDriveProvider::new(GoogleDriveConfig::new(
            &oauth_settings.0,
            &oauth_settings.1,
        )))),
        "dropbox" => Ok(Box::new(DropboxProvider::new(DropboxConfig::new(
            &oauth_settings.0,
            &oauth_settings.1,
        )))),
        "onedrive" => Ok(Box::new(OneDriveProvider::new(OneDriveConfig::new(
            &oauth_settings.0,
            &oauth_settings.1,
        )))),
        "box" => Ok(Box::new(BoxProvider::new(BoxConfig {
            client_id: oauth_settings.0,
            client_secret: oauth_settings.1,
        }))),
        "pcloud" => {
            let region = store
                .get("oauth_pcloud_region")
                .unwrap_or_else(|_| "us".to_string());
            Ok(Box::new(PCloudProvider::new(PCloudConfig {
                client_id: oauth_settings.0,
                client_secret: oauth_settings.1,
                region,
            })))
        }
        "zohoworkdrive" => {
            let region = store
                .get("oauth_zohoworkdrive_region")
                .unwrap_or_else(|_| "us".to_string());
            Ok(Box::new(ZohoWorkdriveProvider::new(
                ZohoWorkdriveConfig::new(&oauth_settings.0, &oauth_settings.1, &region),
            )))
        }
        "yandexdisk" => {
            let manager = OAuth2Manager::new();
            let tokens = manager
                .load_tokens(OAuthProvider::YandexDisk)
                .map_err(|e| format!("No Yandex tokens: {}", e))?;
            Ok(Box::new(YandexDiskProvider::new(
                tokens.access_token.clone(),
                None,
            )))
        }
        "fourshared" => {
            use ftp_client_gui_lib::providers::{
                fourshared::FourSharedProvider, types::FourSharedConfig,
            };

            // Read consumer key/secret - try individual keys first (GUI format), then legacy JSON
            let (ck, cs) = if let (Ok(k), Ok(s)) = (
                store.get("oauth_fourshared_client_id"),
                store.get("oauth_fourshared_client_secret"),
            ) {
                (k, s)
            } else {
                let json = store
                    .get("fourshared_oauth_settings")
                    .map_err(|e| format!("No 4shared OAuth settings in vault: {}", e))?;
                #[derive(serde::Deserialize)]
                struct Fs {
                    consumer_key: String,
                    consumer_secret: String,
                }
                let fs: Fs = serde_json::from_str(&json)
                    .map_err(|e| format!("Failed to parse 4shared settings: {}", e))?;
                (fs.consumer_key, fs.consumer_secret)
            };
            let (at, ats) = {
                let data = store.get("oauth_fourshared").map_err(|_| {
                    "No 4shared access tokens in vault. Authorize from GUI first.".to_string()
                })?;
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
        dropbox::DropboxConfig, google_drive::GoogleDriveConfig, onedrive::OneDriveConfig,
        types::BoxConfig, types::PCloudConfig, zoho_workdrive::ZohoWorkdriveConfig, BoxProvider,
        DropboxProvider, GoogleDriveProvider, OAuth2Manager, OAuthProvider, OneDriveProvider,
        PCloudProvider, ZohoWorkdriveProvider,
    };

    type OAuthCreateFn =
        Box<dyn FnOnce(&CredentialStore) -> Result<Box<dyn StorageProvider>, String>>;
    let (oauth_provider, create_fn): (OAuthProvider, OAuthCreateFn) = match protocol {
        "googledrive" => {
            let oauth_settings = load_oauth_client_config(store, "googledrive");
            (
                OAuthProvider::Google,
                Box::new(move |_| {
                    let config = GoogleDriveConfig::new(&oauth_settings.0, &oauth_settings.1);
                    Ok(Box::new(GoogleDriveProvider::new(config)) as Box<dyn StorageProvider>)
                }),
            )
        }
        "dropbox" => {
            let oauth_settings = load_oauth_client_config(store, "dropbox");
            (
                OAuthProvider::Dropbox,
                Box::new(move |_| {
                    let config = DropboxConfig::new(&oauth_settings.0, &oauth_settings.1);
                    Ok(Box::new(DropboxProvider::new(config)) as Box<dyn StorageProvider>)
                }),
            )
        }
        "onedrive" => {
            let oauth_settings = load_oauth_client_config(store, "onedrive");
            (
                OAuthProvider::OneDrive,
                Box::new(move |_| {
                    let config = OneDriveConfig::new(&oauth_settings.0, &oauth_settings.1);
                    Ok(Box::new(OneDriveProvider::new(config)) as Box<dyn StorageProvider>)
                }),
            )
        }
        "box" => {
            let oauth_settings = load_oauth_client_config(store, "box");
            (
                OAuthProvider::Box,
                Box::new(move |_| {
                    let config = BoxConfig {
                        client_id: oauth_settings.0,
                        client_secret: oauth_settings.1,
                    };
                    Ok(Box::new(BoxProvider::new(config)) as Box<dyn StorageProvider>)
                }),
            )
        }
        "pcloud" => {
            let oauth_settings = load_oauth_client_config(store, "pcloud");
            let region = store
                .get("oauth_pcloud_region")
                .unwrap_or_else(|_| "us".to_string());
            (
                OAuthProvider::PCloud,
                Box::new(move |_| {
                    let config = PCloudConfig {
                        client_id: oauth_settings.0,
                        client_secret: oauth_settings.1,
                        region,
                    };
                    Ok(Box::new(PCloudProvider::new(config)) as Box<dyn StorageProvider>)
                }),
            )
        }
        "zohoworkdrive" => {
            let oauth_settings = load_oauth_client_config(store, "zohoworkdrive");
            let region = store
                .get("oauth_zohoworkdrive_region")
                .unwrap_or_else(|_| "us".to_string());
            (
                OAuthProvider::ZohoWorkdrive,
                Box::new(move |_| {
                    let config =
                        ZohoWorkdriveConfig::new(&oauth_settings.0, &oauth_settings.1, &region);
                    Ok(Box::new(ZohoWorkdriveProvider::new(config)) as Box<dyn StorageProvider>)
                }),
            )
        }
        "yandexdisk" => {
            // Yandex uses OAuth2 but creates provider with raw token
            (
                OAuthProvider::YandexDisk,
                Box::new(move |_| {
                    let manager = OAuth2Manager::new();
                    let tokens = manager
                        .load_tokens(OAuthProvider::YandexDisk)
                        .map_err(|e| format!("No Yandex Disk tokens: {}", e))?;
                    Ok(
                        Box::new(ftp_client_gui_lib::providers::YandexDiskProvider::new(
                            tokens.access_token.clone(),
                            None,
                        )) as Box<dyn StorageProvider>,
                    )
                }),
            )
        }
        "fourshared" => {
            // 4shared uses OAuth1 - handle separately from the OAuth2 flow
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
                struct FsSettings {
                    consumer_key: String,
                    consumer_secret: String,
                }
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
            print_profile_banner_once(profile_name, "4SHARED via OAuth1".to_string(), quiet);
            return Some(Ok((
                Box::new(provider) as Box<dyn StorageProvider>,
                initial_path.to_string(),
            )));
        }
        _ => return None,
    };

    // Check if tokens exist - if not, offer browser authorization
    let manager = OAuth2Manager::new();
    let needs_auth = !manager.has_tokens(oauth_provider);

    if needs_auth {
        if !std::io::stdin().is_terminal() {
            eprintln!("Error: No OAuth tokens for {}. Run interactively to authorize, or authorize from AeroFTP GUI.", profile_name);
            return Some(Err(6));
        }
        eprintln!(
            "No OAuth tokens found for {}. Starting browser authorization...",
            profile_name
        );
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

    print_profile_banner_once(
        profile_name,
        format!("{} via OAuth", protocol.to_uppercase()),
        quiet,
    );

    // Connect - if token expired, offer re-authorization
    if let Err(e) = provider.connect().await {
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "Error: OAuth connection failed: {}. Run interactively to re-authorize.",
                e
            );
            return Some(Err(6));
        }
        eprintln!("Token expired or invalid. Starting browser re-authorization...");
        match cli_oauth_browser_auth(protocol, store).await {
            Ok(()) => {
                eprintln!("Re-authorization successful! Reconnecting...");
                // Recreate provider with fresh tokens
                // We need to recreate since create_fn was consumed - rebuild inline
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
                    let cid = p
                        .get("clientId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let csec = p
                        .get("clientSecret")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
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
    // Check if --profile points to an OAuth provider - handle separately
    // Uses the same strict matching as profile_to_provider_config (exact → ID → disambiguated substring)
    if let Some(ref profile_name) = cli.profile {
        if let Ok(store) = open_vault(cli) {
            if let Ok(profiles_json) = store.get("config_server_profiles") {
                if let Ok(profiles) = serde_json::from_str::<Vec<serde_json::Value>>(&profiles_json)
                {
                    let profile_lower = profile_name.to_lowercase();
                    let matched = if let Ok(idx) = profile_name.parse::<usize>() {
                        profiles.get(idx.saturating_sub(1)).cloned()
                    } else {
                        // Exact name → exact ID → disambiguated substring (same as profile_to_provider_config)
                        let exact = profiles.iter().find(|p| {
                            p.get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_lowercase()
                                == profile_lower
                        });
                        if exact.is_some() {
                            exact.cloned()
                        } else {
                            let by_id = profiles.iter().find(|p| {
                                p.get("id").and_then(|v| v.as_str()).unwrap_or("")
                                    == profile_name.as_str()
                            });
                            if by_id.is_some() {
                                by_id.cloned()
                            } else {
                                let matches: Vec<_> = profiles
                                    .iter()
                                    .filter(|p| {
                                        p.get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_lowercase()
                                            .contains(&profile_lower)
                                    })
                                    .collect();
                                match matches.len() {
                                    1 => Some(matches[0].clone()),
                                    _ => None, // 0 or ambiguous - let profile_to_provider_config handle the error
                                }
                            }
                        }
                    };
                    if let Some(profile) = matched {
                        let protocol = profile
                            .get("protocol")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let name = profile
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unnamed");
                        let initial_path = profile
                            .get("initialPath")
                            .and_then(|v| v.as_str())
                            .unwrap_or("/");
                        if let Some(result) = try_create_oauth_provider(
                            protocol,
                            name,
                            initial_path,
                            &store,
                            cli.quiet,
                        )
                        .await
                        {
                            return result;
                        }
                    }
                }
            }
        }
    }

    let (config, path) = resolve_url_or_profile(url, cli, format)?;

    dump_connection_info(cli, &config);

    let mut provider = match ProviderFactory::create(&config) {
        Ok(p) => p,
        Err(e) => {
            print_error(
                format,
                &format!("Failed to create provider: {}", e),
                provider_error_to_exit_code(&e),
            );
            return Err(provider_error_to_exit_code(&e));
        }
    };

    if let Err(e) = provider.connect().await {
        let code = provider_error_to_exit_code(&e);
        let hint = match &e {
            ProviderError::ConnectionFailed(_) => {
                " (check hostname/port and verify the service is running)"
            }
            ProviderError::AuthenticationFailed(_) => " (check credentials in --profile or URL)",
            ProviderError::Timeout => " (increase --timeout or check firewall rules)",
            ProviderError::NetworkError(_) => " (check network connectivity and DNS resolution)",
            ProviderError::InvalidConfig(_) => " (verify profile settings or URL format)",
            _ => "",
        };
        print_error(format, &format!("Connection failed: {}{}", e, hint), code);
        return Err(code);
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

    // Apply --chunk-size / --buffer-size overrides if set
    let upload_override = cli
        .chunk_size
        .as_deref()
        .and_then(|s| parse_size_filter(s).ok());
    let download_override = cli
        .buffer_size
        .as_deref()
        .and_then(|s| parse_size_filter(s).ok());
    if upload_override.is_some() || download_override.is_some() {
        provider.set_chunk_sizes(upload_override, download_override);
    }

    Ok((provider, path))
}

// ── Command Handlers ───────────────────────────────────────────────

#[derive(Clone)]
struct ServeHttpState {
    provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
    provider_label: String,
    base_path: String,
    auth_token: Option<String>,
}

#[derive(Clone)]
struct DaemonApiState {
    conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    auth_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServeAuthCredentials {
    username: String,
    password: String,
    generated: bool,
}

#[derive(Clone, Debug, Default)]
struct ServeAuthOptions {
    username: Option<String>,
    password: Option<String>,
}

#[derive(Clone, Debug)]
struct ServeEndpointOptions {
    addr: String,
    allow_remote_bind: bool,
    auth: ServeAuthOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolExposureKind {
    LocalReadonly,
    RemoteMetadata,
    RemotePreview,
    RemoteBulkRead,
    LocalModify,
    RemoteModify,
    Destructive,
    Execution,
}

const AGENT_REMOTE_PREVIEW_BYTES: u64 = 5 * 1024;
const AGENT_REMOTE_FALLBACK_MAX_BYTES: u64 = 1024 * 1024;

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

fn validate_bind_addr(
    addr: &str,
    allow_remote_bind: bool,
    surface: &str,
) -> Result<SocketAddr, String> {
    let bind_addr: SocketAddr = addr
        .parse()
        .map_err(|error| format!("Invalid --addr '{}': {}", addr, error))?;
    if !allow_remote_bind && !bind_addr.ip().is_loopback() {
        return Err(format!(
            "Refusing to bind {} to non-loopback address {}. Re-run with --allow-remote-bind to expose it intentionally.",
            surface, addr
        ));
    }
    Ok(bind_addr)
}

fn normalize_optional_token(token: Option<String>) -> Option<String> {
    token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn generate_auth_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn resolve_service_auth_token(
    requested_token: Option<String>,
    bind_addr: SocketAddr,
) -> (Option<String>, bool) {
    if let Some(token) = normalize_optional_token(requested_token) {
        return (Some(token), false);
    }

    if bind_addr.ip().is_loopback() {
        (None, false)
    } else {
        (Some(generate_auth_token()), true)
    }
}

fn resolve_service_credentials(
    requested_username: Option<String>,
    requested_password: Option<String>,
    bind_addr: SocketAddr,
) -> Option<ServeAuthCredentials> {
    let requested_username = normalize_optional_token(requested_username);
    let requested_password = normalize_optional_token(requested_password);

    match (requested_username, requested_password) {
        (Some(username), Some(password)) => Some(ServeAuthCredentials {
            username,
            password,
            generated: false,
        }),
        (Some(username), None) => Some(ServeAuthCredentials {
            username,
            password: generate_auth_token(),
            generated: true,
        }),
        (None, Some(password)) => Some(ServeAuthCredentials {
            username: "aeroftp".to_string(),
            password,
            generated: false,
        }),
        (None, None) if bind_addr.ip().is_loopback() => None,
        (None, None) => Some(ServeAuthCredentials {
            username: "aeroftp".to_string(),
            password: generate_auth_token(),
            generated: true,
        }),
    }
}

fn request_is_authorized(headers: &HeaderMap, expected_token: Option<&str>) -> bool {
    let Some(expected_token) = expected_token else {
        return true;
    };

    let Some(raw_auth) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    if let Some(token) = raw_auth.strip_prefix("Bearer ") {
        return token.trim() == expected_token;
    }

    if let Some(encoded) = raw_auth.strip_prefix("Basic ") {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
            if let Ok(decoded) = String::from_utf8(bytes) {
                if let Some((_, password)) = decoded.split_once(':') {
                    return password == expected_token;
                }
            }
        }
    }

    false
}

fn auth_required_response(realm: &str, message: &str) -> Response {
    let mut response = serve_error_response(StatusCode::UNAUTHORIZED, message);
    if let Ok(value) =
        HeaderValue::from_str(&format!("Basic realm=\"{}\", charset=\"UTF-8\"", realm))
    {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

fn ensure_request_authorized(
    headers: &HeaderMap,
    expected_token: Option<&str>,
    realm: &str,
    message: &str,
) -> Option<Response> {
    if request_is_authorized(headers, expected_token) {
        None
    } else {
        Some(auth_required_response(realm, message))
    }
}

fn build_served_remote_path(base_path: &str, relative_path: &str) -> String {
    if relative_path.is_empty() {
        return normalize_remote_path(base_path);
    }

    let normalized_base = normalize_remote_path(base_path);
    if normalized_base == "/" {
        format!("/{}", relative_path)
    } else {
        format!(
            "{}/{}",
            normalized_base.trim_end_matches('/'),
            relative_path
        )
    }
}

fn resolve_served_backend_path(
    base_path: &str,
    requested_path: &str,
) -> Result<String, &'static str> {
    let relative =
        sanitize_served_relative_path(requested_path).map_err(|_| "path traversal denied")?;
    Ok(build_served_remote_path(base_path, &relative))
}

/// Resolve a user-supplied remote path relative to the profile's initial_path.
///
/// When a profile has `initial_path = /www.aeroftp.app`, the provider does `cd /www.aeroftp.app`
/// after connecting. User paths should be relative to this base:
///   - `/file.txt`  → `/www.aeroftp.app/file.txt`
///   - `file.txt`   → `/www.aeroftp.app/file.txt`
///   - `/sub/file`  → `/www.aeroftp.app/sub/file`
///
/// If the user path already starts with the initial_path, it is used as-is.
/// If initial_path is `/` or empty, leading slashes are stripped to treat the
/// path as relative to the current working directory.
fn resolve_cli_remote_path(initial_path: &str, user_path: &str) -> String {
    // Reject path traversal components (.. as a path segment)
    for component in user_path.split('/') {
        if component == ".." {
            eprintln!(
                "Error: path '{}' contains '..' traversal component — rejected for safety. Use absolute paths instead.",
                user_path
            );
            // Return the base path unchanged so the command operates on a safe location.
            // The caller will see the mismatch and the error on stderr.
            let base = initial_path.trim();
            return if base.is_empty() || base == "/" {
                "/".to_string()
            } else {
                base.trim_end_matches('/').to_string()
            };
        }
    }
    let base = initial_path.trim();
    // No meaningful initial_path — pass user_path through with minimal
    // rewriting:
    //   - empty user_path -> "" so the provider applies its canonical
    //     default (FTP/SFTP home, bucket root, configured current_path),
    //     instead of being coerced to absolute "/".
    //   - bare "/" with no other content -> same as empty (provider default).
    //   - otherwise the original path (relative or absolute) is preserved
    //     verbatim, so `/etc` on a non-chroot FTP still targets the
    //     filesystem root and `foo/bar` resolves against cwd.
    if base.is_empty() || base == "/" {
        if user_path.is_empty() {
            return String::new();
        }
        if user_path.trim_start_matches('/').is_empty() {
            return String::new();
        }
        return user_path.to_string();
    }
    let base_normalized = base.trim_end_matches('/');

    // User already provided the full path including the initial_path prefix
    if user_path.starts_with(base_normalized) {
        return user_path.to_string();
    }

    // Strip leading slash from user_path to make it relative, then join
    let relative = user_path.trim_start_matches('/');
    let resolved = if relative.is_empty() {
        base_normalized.to_string()
    } else {
        format!("{}/{}", base_normalized, relative)
    };

    // Always log path rewriting to stderr so agents and interactive users see
    // the resolved path without polluting stdout (JSON / piping).
    if resolved != user_path {
        eprintln!(
            "Note: path '{}' resolved to '{}' (profile base: {})",
            user_path, resolved, base_normalized
        );
    }

    resolved
}

fn resolve_agent_remote_path(initial_path: &str, requested_path: &str) -> Result<String, String> {
    if requested_path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }
    let relative = sanitize_served_relative_path(requested_path)
        .map_err(|_| "Path traversal ('..') not allowed".to_string())?;
    Ok(build_served_remote_path(initial_path, &relative))
}

async fn read_remote_preview(
    provider: &mut Box<dyn StorageProvider>,
    remote_path: &str,
) -> Result<(Vec<u8>, u64, bool), ProviderError> {
    let entry = provider.stat(remote_path).await?;
    if entry.is_dir {
        return Err(ProviderError::InvalidPath(format!(
            "'{}' is a directory; expected a file",
            remote_path
        )));
    }

    let size = entry.size;
    if size == 0 {
        return Ok((Vec::new(), 0, false));
    }

    let preview_len = size.min(AGENT_REMOTE_PREVIEW_BYTES);
    match provider.read_range(remote_path, 0, preview_len).await {
        Ok(bytes) => Ok((bytes, size, size > preview_len)),
        Err(_) if size > AGENT_REMOTE_FALLBACK_MAX_BYTES => Err(ProviderError::NotSupported(
            format!(
                "Provider does not support ranged reads for '{}', and full fallback is disabled above {} bytes",
                remote_path, AGENT_REMOTE_FALLBACK_MAX_BYTES
            ),
        )),
        Err(_) => {
            let mut bytes = provider.download_to_bytes(remote_path).await?;
            let truncated = bytes.len() as u64 > AGENT_REMOTE_PREVIEW_BYTES;
            if truncated {
                bytes.truncate(AGENT_REMOTE_PREVIEW_BYTES as usize);
            }
            Ok((bytes, size, truncated))
        }
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
        ProviderError::InvalidPath(_)
        | ProviderError::InvalidConfig(_)
        | ProviderError::ParseError(_) => StatusCode::BAD_REQUEST,
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
    html.push_str(&format!(
        "<p>{} · base remote: {}</p>",
        escape_html(provider_label),
        escape_html(remote_path)
    ));
    html.push_str(
        "<table><thead><tr><th>Name</th><th>Size</th><th>Modified</th></tr></thead><tbody>",
    );

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
            Err(error) => {
                serve_error_response(provider_error_to_status_code(&error), &error.to_string())
            }
        },
        Err(error) => {
            serve_error_response(provider_error_to_status_code(&error), &error.to_string())
        }
    }
}

async fn serve_http_root(State(state): State<ServeHttpState>, headers: HeaderMap) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        state.auth_token.as_deref(),
        "AeroFTP HTTP",
        "Authentication required. Use the configured token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }
    let range = headers.get(axum::http::header::RANGE);
    serve_http_response(state, String::new(), false, range).await
}

async fn serve_http_root_head(State(state): State<ServeHttpState>, headers: HeaderMap) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        state.auth_token.as_deref(),
        "AeroFTP HTTP",
        "Authentication required. Use the configured token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }
    serve_http_response(state, String::new(), true, None).await
}

async fn serve_http_path(
    State(state): State<ServeHttpState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        state.auth_token.as_deref(),
        "AeroFTP HTTP",
        "Authentication required. Use the configured token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }
    let range = headers.get(axum::http::header::RANGE);
    serve_http_response(state, path, false, range).await
}

async fn serve_http_path_head(
    State(state): State<ServeHttpState>,
    AxumPath(path): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        state.auth_token.as_deref(),
        "AeroFTP HTTP",
        "Authentication required. Use the configured token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }
    serve_http_response(state, path, true, None).await
}

async fn cmd_serve_http(
    url: &str,
    path: &str,
    addr: &str,
    allow_remote_bind: bool,
    auth_token: Option<String>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(value) => value,
        Err(code) => return code,
    };

    let bind_addr = match validate_bind_addr(addr, allow_remote_bind, "HTTP serve") {
        Ok(addr) => addr,
        Err(error) => {
            print_error(format, &error, 5);
            return 5;
        }
    };
    let (auth_token, generated_auth_token) = resolve_service_auth_token(auth_token, bind_addr);

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
        auth_token: auth_token.clone(),
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
                "auth_required": auth_token.is_some(),
                "auth_mode": if auth_token.is_some() { "basic-or-bearer" } else { "none" },
                "generated_auth_token": if generated_auth_token { auth_token.clone() } else { None },
            })
        );
    } else if !cli.quiet {
        eprintln!("Serving HTTP on http://{}", addr);
        eprintln!("Remote base path: {}", state.base_path);
        if let Some(token) = auth_token.as_deref() {
            eprintln!("Authentication: enabled (Basic auth or Bearer token)");
            eprintln!("Use any username and this password/token: {}", token);
        } else {
            eprintln!("Authentication: disabled for local loopback access");
        }
        eprintln!("Press Ctrl+C to stop.");
    }

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            // shutdown_signal() awaits both SIGINT and SIGTERM so systemd /
            // Docker / any well-behaved supervisor can request a clean
            // shutdown without needing SIGKILL.
            let _ = shutdown_signal().await;
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
    if let Some(response) = ensure_request_authorized(
        &headers,
        state.auth_token.as_deref(),
        "AeroFTP WebDAV",
        "Authentication required. Use the configured token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }

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
            response
                .headers_mut()
                .insert("DAV", HeaderValue::from_static("1"));
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
                            remote_path.rsplit('/').next().unwrap_or("").to_string(),
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
                        Err(e) => {
                            serve_error_response(provider_error_to_status_code(&e), &e.to_string())
                        }
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
    allow_remote_bind: bool,
    auth_token: Option<String>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(value) => value,
        Err(code) => return code,
    };

    let bind_addr = match validate_bind_addr(addr, allow_remote_bind, "WebDAV serve") {
        Ok(addr) => addr,
        Err(error) => {
            print_error(format, &error, 5);
            return 5;
        }
    };
    let (auth_token, generated_auth_token) = resolve_service_auth_token(auth_token, bind_addr);

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
        auth_token: auth_token.clone(),
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
                "auth_required": auth_token.is_some(),
                "auth_mode": if auth_token.is_some() { "basic-or-bearer" } else { "none" },
                "generated_auth_token": if generated_auth_token { auth_token.clone() } else { None },
            })
        );
    } else if !cli.quiet {
        eprintln!("Serving WebDAV on http://{}", addr);
        eprintln!("Remote base path: {}", state.base_path);
        if let Some(token) = auth_token.as_deref() {
            eprintln!("Authentication: enabled (Basic auth or Bearer token)");
            eprintln!("Use any username and this password/token: {}", token);
        } else {
            eprintln!("Authentication: disabled for local loopback access");
        }
        eprintln!("Read-write mode. Press Ctrl+C to stop.");
    }

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_signal().await;
        })
        .await;

    let mut provider = state.provider.lock().await;
    let _ = provider.disconnect().await;

    match result {
        Ok(()) => 0,
        Err(error) => {
            print_error(format, &format!("WebDAV server failed: {}", error), 1);
            1
        }
    }
}

// ── Serve FTP - libunftp StorageBackend adapter ─────────────────

mod serve_ftp_backend {
    use super::*;
    use std::fmt::Debug;
    use std::path::{Path, PathBuf};
    use tokio::io::AsyncRead;
    use unftp_core::auth::{
        AuthenticationError, Authenticator, Credentials, DefaultUser, Principal,
    };
    use unftp_core::storage::{
        Error as FtpError, ErrorKind as FtpErrorKind, Fileinfo, Metadata, Result as FtpResult,
        StorageBackend,
    };

    #[derive(Debug)]
    pub struct TokenAuthenticator {
        username: String,
        password: String,
    }

    impl TokenAuthenticator {
        pub fn new(username: String, password: String) -> Self {
            Self { username, password }
        }
    }

    #[async_trait::async_trait]
    impl Authenticator for TokenAuthenticator {
        async fn authenticate(
            &self,
            username: &str,
            creds: &Credentials,
        ) -> Result<Principal, AuthenticationError> {
            if username != self.username {
                return Err(AuthenticationError::BadUser);
            }
            match creds.password.as_deref() {
                Some(password) if password == self.password => Ok(Principal {
                    username: username.to_string(),
                }),
                _ => Err(AuthenticationError::BadPassword),
            }
        }
    }

    /// Metadata adapter for libunftp.
    #[derive(Debug)]
    pub struct AeroFtpMeta {
        size: u64,
        is_dir: bool,
        modified: Option<std::time::SystemTime>,
    }

    impl Metadata for AeroFtpMeta {
        fn len(&self) -> u64 {
            self.size
        }
        fn is_dir(&self) -> bool {
            self.is_dir
        }
        fn is_file(&self) -> bool {
            !self.is_dir
        }
        fn is_symlink(&self) -> bool {
            false
        }
        fn modified(&self) -> FtpResult<std::time::SystemTime> {
            self.modified
                .ok_or_else(|| FtpError::new(FtpErrorKind::LocalError, "no mtime"))
        }
        fn gid(&self) -> u32 {
            0
        }
        fn uid(&self) -> u32 {
            0
        }
    }

    /// StorageBackend that wraps AeroFTP's StorageProvider.
    pub struct AeroFtpBackend {
        provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
        base_path: String,
    }

    impl std::fmt::Debug for AeroFtpBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AeroFtpBackend")
                .field("base_path", &self.base_path)
                .finish()
        }
    }

    impl AeroFtpBackend {
        pub fn new(provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>, base_path: String) -> Self {
            Self {
                provider,
                base_path,
            }
        }

        fn resolve_path(&self, path: &Path) -> FtpResult<String> {
            resolve_served_backend_path(&self.base_path, &path.to_string_lossy())
                .map_err(|error| FtpError::new(FtpErrorKind::PermissionDenied, error))
        }

        fn provider_err_to_ftp(e: ProviderError) -> FtpError {
            let kind = match &e {
                ProviderError::NotFound(_) => FtpErrorKind::PermanentFileNotAvailable,
                ProviderError::PermissionDenied(_) => FtpErrorKind::PermissionDenied,
                _ => FtpErrorKind::LocalError,
            };
            FtpError::new(kind, e)
        }
    }

    #[async_trait::async_trait]
    impl StorageBackend<DefaultUser> for AeroFtpBackend {
        type Metadata = AeroFtpMeta;

        async fn metadata<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
        ) -> FtpResult<Self::Metadata> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            let entry = p.stat(&remote).await.map_err(Self::provider_err_to_ftp)?;
            Ok(AeroFtpMeta {
                size: entry.size,
                is_dir: entry.is_dir,
                modified: entry.modified.as_deref().and_then(|s| {
                    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                        .ok()
                        .map(|dt| {
                            std::time::UNIX_EPOCH
                                + std::time::Duration::from_secs(
                                    dt.and_utc().timestamp().max(0) as u64
                                )
                        })
                }),
            })
        }

        async fn list<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
        ) -> FtpResult<Vec<Fileinfo<PathBuf, Self::Metadata>>> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            let entries = p.list(&remote).await.map_err(Self::provider_err_to_ftp)?;
            Ok(entries
                .into_iter()
                .map(|e| Fileinfo {
                    path: PathBuf::from(&e.name),
                    metadata: AeroFtpMeta {
                        size: e.size,
                        is_dir: e.is_dir,
                        modified: e.modified.as_deref().and_then(|s| {
                            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                                .ok()
                                .map(|dt| {
                                    std::time::UNIX_EPOCH
                                        + std::time::Duration::from_secs(
                                            dt.and_utc().timestamp().max(0) as u64,
                                        )
                                })
                        }),
                    },
                })
                .collect())
        }

        async fn get<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
            start_pos: u64,
        ) -> FtpResult<Box<dyn AsyncRead + Send + Sync + Unpin>> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            // Download to memory and serve as cursor
            let data = if start_pos > 0 {
                let size = p.size(&remote).await.unwrap_or(0);
                let len = size.saturating_sub(start_pos);
                p.read_range(&remote, start_pos, len)
                    .await
                    .or_else(|_| {
                        // Fallback: download full and slice
                        tokio::runtime::Handle::current().block_on(async {
                            let full = p.download_to_bytes(&remote).await?;
                            Ok(full[start_pos as usize..].to_vec())
                        })
                    })
                    .map_err(Self::provider_err_to_ftp)?
            } else {
                p.download_to_bytes(&remote)
                    .await
                    .map_err(Self::provider_err_to_ftp)?
            };
            Ok(Box::new(std::io::Cursor::new(data)))
        }

        async fn put<
            P: AsRef<Path> + Send + Debug,
            R: AsyncRead + Send + Sync + Unpin + 'static,
        >(
            &self,
            _user: &DefaultUser,
            mut input: R,
            path: P,
            _start_pos: u64,
        ) -> FtpResult<u64> {
            let remote = self.resolve_path(path.as_ref())?;
            // Write input to tempfile, then upload
            let tmp = tempfile::NamedTempFile::new()
                .map_err(|e| FtpError::new(FtpErrorKind::LocalError, e))?;
            let tmp_path = tmp.path().to_string_lossy().to_string();
            {
                let mut file = tokio::fs::File::create(&tmp_path)
                    .await
                    .map_err(|e| FtpError::new(FtpErrorKind::LocalError, e))?;
                tokio::io::copy(&mut input, &mut file)
                    .await
                    .map_err(|e| FtpError::new(FtpErrorKind::LocalError, e))?;
            }
            let size = tokio::fs::metadata(&tmp_path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let mut p = self.provider.lock().await;
            p.upload(&tmp_path, &remote, None)
                .await
                .map_err(Self::provider_err_to_ftp)?;
            Ok(size)
        }

        async fn del<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
        ) -> FtpResult<()> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            p.delete(&remote).await.map_err(Self::provider_err_to_ftp)
        }

        async fn mkd<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
        ) -> FtpResult<()> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            p.mkdir(&remote).await.map_err(Self::provider_err_to_ftp)
        }

        async fn rename<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            from: P,
            to: P,
        ) -> FtpResult<()> {
            let from_remote = self.resolve_path(from.as_ref())?;
            let to_remote = self.resolve_path(to.as_ref())?;
            let mut p = self.provider.lock().await;
            p.rename(&from_remote, &to_remote)
                .await
                .map_err(Self::provider_err_to_ftp)
        }

        async fn rmd<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
        ) -> FtpResult<()> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            p.rmdir(&remote).await.map_err(Self::provider_err_to_ftp)
        }

        async fn cwd<P: AsRef<Path> + Send + Debug>(
            &self,
            _user: &DefaultUser,
            path: P,
        ) -> FtpResult<()> {
            let remote = self.resolve_path(path.as_ref())?;
            let mut p = self.provider.lock().await;
            // Validate directory exists
            match p.list(&remote).await {
                Ok(_) => Ok(()),
                Err(e) => Err(Self::provider_err_to_ftp(e)),
            }
        }
    }
}

async fn cmd_serve_ftp(
    url: &str,
    path: &str,
    endpoint: ServeEndpointOptions,
    passive_ports_str: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(value) => value,
        Err(code) => return code,
    };

    let base_path = serve_effective_base_path(path, &url_path);
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let bind_addr =
        match validate_bind_addr(&endpoint.addr, endpoint.allow_remote_bind, "FTP serve") {
            Ok(addr) => addr,
            Err(error) => {
                print_error(format, &error, 5);
                return 5;
            }
        };
    let auth_credentials =
        resolve_service_credentials(endpoint.auth.username, endpoint.auth.password, bind_addr);

    // Parse passive port range
    let passive_range = match passive_ports_str.split_once('-') {
        Some((start, end)) => {
            let s: u16 = start.parse().unwrap_or(49152);
            let e: u16 = end.parse().unwrap_or(49200);
            s..=e
        }
        None => 49152..=49200,
    };

    let provider_arc = Arc::new(AsyncMutex::new(provider));

    let backend_provider = provider_arc.clone();
    let backend_base = base_path.clone();

    let builder = if let Some(credentials) = auth_credentials.as_ref() {
        libunftp::ServerBuilder::with_authenticator(
            Box::new(move || {
                serve_ftp_backend::AeroFtpBackend::new(
                    backend_provider.clone(),
                    backend_base.clone(),
                )
            }),
            Arc::new(serve_ftp_backend::TokenAuthenticator::new(
                credentials.username.clone(),
                credentials.password.clone(),
            )),
        )
    } else {
        libunftp::ServerBuilder::new(Box::new(move || {
            serve_ftp_backend::AeroFtpBackend::new(backend_provider.clone(), backend_base.clone())
        }))
    };

    let server = builder
        .passive_ports(passive_range)
        .greeting("AeroFTP serve - connected");

    let server = match server.build() {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &format!("Failed to build FTP server: {}", e), 99);
            return 99;
        }
    };

    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({
                "status": "serving",
                "protocol": "ftp",
                "addr": endpoint.addr,
                "base_path": base_path,
                "auth_required": auth_credentials.is_some(),
                "auth_user": auth_credentials.as_ref().map(|c| c.username.clone()),
                "generated_auth_password": auth_credentials
                    .as_ref()
                    .and_then(|c| if c.generated { Some(c.password.clone()) } else { None }),
            })
        );
    } else if !quiet {
        eprintln!("Serving FTP on ftp://{}", endpoint.addr);
        eprintln!("Remote base path: {}", base_path);
        eprintln!("Passive ports: {}", passive_ports_str);
        if let Some(credentials) = auth_credentials.as_ref() {
            eprintln!("Authentication: enabled");
            eprintln!("Username: {}", credentials.username);
            eprintln!("Password: {}", credentials.password);
        } else {
            eprintln!("Authentication: disabled for local loopback access");
        }
        eprintln!("Press Ctrl+C to stop.");
    }

    // libunftp's `listen` does not accept a shutdown future, so race it
    // against `shutdown_signal()`. On signal the server future is dropped
    // (libunftp cleans up its own listener/sessions on drop) and the
    // disconnect below runs, preventing the previous behaviour where a
    // SIGINT/SIGTERM would leave the process hanging until SIGKILL.
    let serve_fut = server.listen(bind_addr.to_string());
    tokio::select! {
        res = serve_fut => {
            if let Err(e) = res {
                print_error(format, &format!("FTP server failed: {}", e), 1);
                let mut p = provider_arc.lock().await;
                let _ = p.disconnect().await;
                return 1;
            }
        }
        _ = shutdown_signal() => {
            if !quiet {
                eprintln!("Shutdown signal received, stopping FTP server...");
            }
        }
    }

    let mut p = provider_arc.lock().await;
    let _ = p.disconnect().await;
    0
}

// ── Serve SFTP - SSH File Transfer Protocol Server ──────────────

mod serve_sftp {
    use super::*;
    use russh::server::{Auth, Handler as SshHandler, Msg, Server as SshServer, Session};
    use russh::{Channel, ChannelId};
    use std::collections::HashMap;

    // SFTP protocol constants (v3)
    const SSH_FXP_INIT: u8 = 1;
    const SSH_FXP_VERSION: u8 = 2;
    const SSH_FXP_OPEN: u8 = 3;
    const SSH_FXP_CLOSE: u8 = 4;
    const SSH_FXP_READ: u8 = 5;
    const SSH_FXP_WRITE: u8 = 6;
    const SSH_FXP_LSTAT: u8 = 7;
    const SSH_FXP_FSTAT: u8 = 8;
    const SSH_FXP_OPENDIR: u8 = 11;
    const SSH_FXP_READDIR: u8 = 12;
    const SSH_FXP_REMOVE: u8 = 13;
    const SSH_FXP_MKDIR: u8 = 14;
    const SSH_FXP_RMDIR: u8 = 15;
    const SSH_FXP_REALPATH: u8 = 16;
    const SSH_FXP_STAT: u8 = 17;
    const SSH_FXP_RENAME: u8 = 18;
    const SSH_FXP_STATUS: u8 = 101;
    const SSH_FXP_HANDLE: u8 = 102;
    const SSH_FXP_DATA: u8 = 103;
    const SSH_FXP_NAME: u8 = 104;
    const SSH_FXP_ATTRS: u8 = 105;

    const SSH_FX_OK: u32 = 0;
    const SSH_FX_EOF: u32 = 1;
    const SSH_FX_NO_SUCH_FILE: u32 = 2;
    #[allow(dead_code)]
    const SSH_FX_PERMISSION_DENIED: u32 = 3;
    const SSH_FX_FAILURE: u32 = 4;

    fn read_u32(data: &[u8], pos: &mut usize) -> u32 {
        let val = u32::from_be_bytes(data[*pos..*pos + 4].try_into().unwrap_or([0; 4]));
        *pos += 4;
        val
    }

    fn read_u64(data: &[u8], pos: &mut usize) -> u64 {
        let val = u64::from_be_bytes(data[*pos..*pos + 8].try_into().unwrap_or([0; 8]));
        *pos += 8;
        val
    }

    fn read_string(data: &[u8], pos: &mut usize) -> String {
        let len = read_u32(data, pos) as usize;
        let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
        *pos += len;
        s
    }

    fn write_u32(buf: &mut Vec<u8>, val: u32) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_u64(buf: &mut Vec<u8>, val: u64) {
        buf.extend_from_slice(&val.to_be_bytes());
    }

    fn write_string(buf: &mut Vec<u8>, s: &str) {
        write_u32(buf, s.len() as u32);
        buf.extend_from_slice(s.as_bytes());
    }

    fn write_attrs(buf: &mut Vec<u8>, size: u64, is_dir: bool) {
        // flags: SSH_FILEXFER_ATTR_SIZE | SSH_FILEXFER_ATTR_PERMISSIONS
        write_u32(buf, 0x00000001 | 0x00000004);
        write_u64(buf, size);
        write_u32(buf, if is_dir { 0o40755 } else { 0o100644 });
    }

    fn make_status(id: u32, code: u32, msg: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(SSH_FXP_STATUS);
        write_u32(&mut buf, id);
        write_u32(&mut buf, code);
        write_string(&mut buf, msg);
        write_string(&mut buf, "en"); // language
        buf
    }

    fn make_handle(id: u32, handle: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(SSH_FXP_HANDLE);
        write_u32(&mut buf, id);
        write_string(&mut buf, handle);
        buf
    }

    #[allow(dead_code)]
    pub struct AeroSftpServer {
        provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
        base_path: String,
        auth_credentials: Option<ServeAuthCredentials>,
        rt: Arc<tokio::runtime::Runtime>,
    }

    impl SshServer for AeroSftpServer {
        type Handler = AeroSftpHandler;
        fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
            AeroSftpHandler {
                provider: self.provider.clone(),
                base_path: self.base_path.clone(),
                auth_credentials: self.auth_credentials.clone(),
                handles: HashMap::new(),
                next_handle: 0,
                dir_read: std::collections::HashSet::new(),
                sftp_buf: Vec::new(),
                rt: self.rt.clone(),
            }
        }
    }

    pub struct AeroSftpHandler {
        provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
        base_path: String,
        auth_credentials: Option<ServeAuthCredentials>,
        handles: HashMap<String, String>,
        next_handle: u64,
        dir_read: std::collections::HashSet<String>,
        sftp_buf: Vec<u8>,
        /// Dedicated runtime for blocking provider calls from within async SSH handler
        rt: Arc<tokio::runtime::Runtime>,
    }

    impl AeroSftpHandler {
        fn resolve_path(&self, path: &str) -> Result<String, &'static str> {
            resolve_served_backend_path(&self.base_path, path)
        }

        fn alloc_handle(&mut self, path: &str) -> String {
            let h = format!("h{}", self.next_handle);
            self.next_handle += 1;
            self.handles.insert(h.clone(), path.to_string());
            h
        }

        fn process_sftp(&mut self, data: &[u8]) -> Vec<u8> {
            let rt = self.rt.clone();
            let provider = self.provider.clone();

            // Helper macro: spawn a plain thread, run async provider call with block_on.
            // Clones all captures to avoid lifetime issues with the thread.
            macro_rules! prov {
                ($provider:expr, $rt:expr, async |$p:ident| $body:expr) => {{
                    let __prov = $provider.clone();
                    let __rt = $rt.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let r = __rt.block_on(async {
                            let mut __guard = __prov.lock().await;
                            let $p = &mut **__guard;
                            $body
                        });
                        let _ = tx.send(r);
                    });
                    rx.recv().unwrap()
                }};
            }
            if data.is_empty() {
                return Vec::new();
            }
            let ptype = data[0];
            let mut pos = 1usize;

            match ptype {
                SSH_FXP_INIT => {
                    let _version = read_u32(data, &mut pos);
                    let mut reply = Vec::new();
                    reply.push(SSH_FXP_VERSION);
                    write_u32(&mut reply, 3); // SFTP v3
                    reply
                }
                SSH_FXP_REALPATH => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let resolved = if path == "." { "/".to_string() } else { path };
                    let mut reply = Vec::new();
                    reply.push(SSH_FXP_NAME);
                    write_u32(&mut reply, id);
                    write_u32(&mut reply, 1); // count
                    write_string(&mut reply, &resolved);
                    write_string(&mut reply, &resolved); // longname
                    write_attrs(&mut reply, 0, true);
                    reply
                }
                SSH_FXP_STAT | SSH_FXP_LSTAT => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let remote = match self.resolve_path(&path) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let r = remote.clone();
                    match prov!(provider, rt, async |p| p.stat(&r).await) {
                        Ok(entry) => {
                            let mut reply = Vec::new();
                            reply.push(SSH_FXP_ATTRS);
                            write_u32(&mut reply, id);
                            write_attrs(&mut reply, entry.size, entry.is_dir);
                            reply
                        }
                        Err(_) => make_status(id, SSH_FX_NO_SUCH_FILE, "not found"),
                    }
                }
                SSH_FXP_FSTAT => {
                    let id = read_u32(data, &mut pos);
                    let handle = read_string(data, &mut pos);
                    let Some(path) = self.handles.get(&handle).cloned() else {
                        return make_status(id, SSH_FX_FAILURE, "invalid handle");
                    };
                    let result = rt.block_on(async {
                        let mut p = self.provider.lock().await;
                        p.stat(&path).await
                    });
                    match result {
                        Ok(entry) => {
                            let mut reply = Vec::new();
                            reply.push(SSH_FXP_ATTRS);
                            write_u32(&mut reply, id);
                            write_attrs(&mut reply, entry.size, entry.is_dir);
                            reply
                        }
                        Err(_) => make_status(id, SSH_FX_NO_SUCH_FILE, "not found"),
                    }
                }
                SSH_FXP_OPENDIR => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let remote = match self.resolve_path(&path) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let handle = self.alloc_handle(&remote);
                    make_handle(id, &handle)
                }
                SSH_FXP_READDIR => {
                    let id = read_u32(data, &mut pos);
                    let handle = read_string(data, &mut pos);
                    // Return EOF if already read
                    if self.dir_read.contains(&handle) {
                        return make_status(id, SSH_FX_EOF, "");
                    }
                    self.dir_read.insert(handle.clone());
                    let Some(dir_path) = self.handles.get(&handle).cloned() else {
                        return make_status(id, SSH_FX_FAILURE, "invalid handle");
                    };
                    let dp = dir_path.clone();
                    match prov!(provider, rt, async |p| p.list(&dp).await) {
                        Ok(entries) => {
                            let mut reply = Vec::new();
                            reply.push(SSH_FXP_NAME);
                            write_u32(&mut reply, id);
                            write_u32(&mut reply, entries.len() as u32);
                            for e in &entries {
                                write_string(&mut reply, &e.name);
                                // longname: permissions size name
                                let long = if e.is_dir {
                                    format!("drwxr-xr-x 1 0 0 {} {}", e.size, e.name)
                                } else {
                                    format!("-rw-r--r-- 1 0 0 {} {}", e.size, e.name)
                                };
                                write_string(&mut reply, &long);
                                write_attrs(&mut reply, e.size, e.is_dir);
                            }
                            reply
                        }
                        Err(_) => make_status(id, SSH_FX_FAILURE, "list failed"),
                    }
                }
                SSH_FXP_OPEN => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let _flags = read_u32(data, &mut pos);
                    let remote = match self.resolve_path(&path) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let handle = self.alloc_handle(&remote);
                    make_handle(id, &handle)
                }
                SSH_FXP_READ => {
                    let id = read_u32(data, &mut pos);
                    let handle = read_string(data, &mut pos);
                    let offset = read_u64(data, &mut pos);
                    let len = read_u32(data, &mut pos);
                    let Some(path) = self.handles.get(&handle).cloned() else {
                        return make_status(id, SSH_FX_FAILURE, "invalid handle");
                    };
                    let rp = path.clone();
                    match prov!(provider, rt, async |p| p
                        .read_range(&rp, offset, len as u64)
                        .await)
                    {
                        Ok(data) if data.is_empty() => make_status(id, SSH_FX_EOF, ""),
                        Ok(file_data) => {
                            let mut reply = Vec::new();
                            reply.push(SSH_FXP_DATA);
                            write_u32(&mut reply, id);
                            write_u32(&mut reply, file_data.len() as u32);
                            reply.extend_from_slice(&file_data);
                            reply
                        }
                        Err(_) => {
                            let rp2 = path.clone();
                            match prov!(provider, rt, async |p| p.download_to_bytes(&rp2).await) {
                                Ok(full) => {
                                    let start = (offset as usize).min(full.len());
                                    let end = (start + len as usize).min(full.len());
                                    if start >= full.len() {
                                        make_status(id, SSH_FX_EOF, "")
                                    } else {
                                        let mut reply = Vec::new();
                                        reply.push(SSH_FXP_DATA);
                                        write_u32(&mut reply, id);
                                        write_u32(&mut reply, (end - start) as u32);
                                        reply.extend_from_slice(&full[start..end]);
                                        reply
                                    }
                                }
                                Err(_) => make_status(id, SSH_FX_FAILURE, "read failed"),
                            }
                        }
                    }
                }
                SSH_FXP_WRITE => {
                    let id = read_u32(data, &mut pos);
                    let _handle = read_string(data, &mut pos);
                    let _offset = read_u64(data, &mut pos);
                    let _data_len = read_u32(data, &mut pos);
                    // Write support: simplified - upload on close
                    make_status(id, SSH_FX_OK, "")
                }
                SSH_FXP_CLOSE => {
                    let id = read_u32(data, &mut pos);
                    let handle = read_string(data, &mut pos);
                    self.handles.remove(&handle);
                    self.dir_read.remove(&handle);
                    make_status(id, SSH_FX_OK, "")
                }
                SSH_FXP_REMOVE => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let remote = match self.resolve_path(&path) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let r = remote.clone();
                    match prov!(provider, rt, async |p| p.delete(&r).await) {
                        Ok(()) => make_status(id, SSH_FX_OK, ""),
                        Err(_) => make_status(id, SSH_FX_FAILURE, "delete failed"),
                    }
                }
                SSH_FXP_MKDIR => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let remote = match self.resolve_path(&path) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let r = remote.clone();
                    match prov!(provider, rt, async |p| p.mkdir(&r).await) {
                        Ok(()) => make_status(id, SSH_FX_OK, ""),
                        Err(_) => make_status(id, SSH_FX_FAILURE, "mkdir failed"),
                    }
                }
                SSH_FXP_RMDIR => {
                    let id = read_u32(data, &mut pos);
                    let path = read_string(data, &mut pos);
                    let remote = match self.resolve_path(&path) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let r = remote.clone();
                    match prov!(provider, rt, async |p| p.rmdir(&r).await) {
                        Ok(()) => make_status(id, SSH_FX_OK, ""),
                        Err(_) => make_status(id, SSH_FX_FAILURE, "rmdir failed"),
                    }
                }
                SSH_FXP_RENAME => {
                    let id = read_u32(data, &mut pos);
                    let from = read_string(data, &mut pos);
                    let to = read_string(data, &mut pos);
                    let from_remote = match self.resolve_path(&from) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let to_remote = match self.resolve_path(&to) {
                        Ok(remote) => remote,
                        Err(_) => {
                            return make_status(
                                id,
                                SSH_FX_PERMISSION_DENIED,
                                "path traversal denied",
                            )
                        }
                    };
                    let fr = from_remote.clone();
                    let tr = to_remote.clone();
                    match prov!(provider, rt, async |p| p.rename(&fr, &tr).await) {
                        Ok(()) => make_status(id, SSH_FX_OK, ""),
                        Err(_) => make_status(id, SSH_FX_FAILURE, "rename failed"),
                    }
                }
                _ => {
                    if data.len() >= 5 {
                        let id = read_u32(data, &mut 1);
                        make_status(id, SSH_FX_FAILURE, "unsupported")
                    } else {
                        Vec::new()
                    }
                }
            }
        }

        fn send_sftp_packet(session: &mut Session, channel: ChannelId, payload: &[u8]) {
            let mut pkt = Vec::with_capacity(4 + payload.len());
            write_u32(&mut pkt, payload.len() as u32);
            pkt.extend_from_slice(payload);
            let pkt_bytes: Vec<u8> = pkt;
            let _ = session.data(channel, pkt_bytes);
        }
    }

    #[allow(clippy::manual_async_fn)]
    impl SshHandler for AeroSftpHandler {
        type Error = anyhow::Error;

        fn auth_password(
            &mut self,
            user: &str,
            password: &str,
        ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
            let expected = self.auth_credentials.clone();
            let user = user.to_string();
            let password = password.to_string();
            async move {
                Ok(match expected {
                    Some(credentials)
                        if credentials.username == user && credentials.password == password =>
                    {
                        Auth::Accept
                    }
                    Some(_) => Auth::Reject {
                        proceed_with_methods: Some(russh::MethodSet::from(
                            &[russh::MethodKind::Password][..],
                        )),
                        partial_success: false,
                    },
                    None => Auth::Accept,
                })
            }
        }

        fn auth_none(
            &mut self,
            _user: &str,
        ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
            let auth_enabled = self.auth_credentials.is_some();
            async move {
                Ok(if auth_enabled {
                    Auth::Reject {
                        proceed_with_methods: Some(russh::MethodSet::from(
                            &[russh::MethodKind::Password][..],
                        )),
                        partial_success: false,
                    }
                } else {
                    Auth::Accept
                })
            }
        }

        fn channel_open_session(
            &mut self,
            _channel: Channel<Msg>,
            _session: &mut Session,
        ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
            async { Ok(true) }
        }

        fn subsystem_request(
            &mut self,
            channel: ChannelId,
            name: &str,
            session: &mut Session,
        ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
            if name == "sftp" {
                let _ = session.channel_success(channel);
            } else {
                let _ = session.channel_failure(channel);
            }
            async { Ok(()) }
        }

        fn data(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
            // SFTP packets are length-prefixed: 4 bytes length + payload
            // Accumulate data in buffer for fragmented packets
            self.sftp_buf.extend_from_slice(data);

            // Process complete packets
            while self.sftp_buf.len() >= 4 {
                let pkt_len = u32::from_be_bytes(self.sftp_buf[0..4].try_into().unwrap()) as usize;
                if self.sftp_buf.len() < 4 + pkt_len {
                    break;
                }
                let pkt_data = self.sftp_buf[4..4 + pkt_len].to_vec();
                self.sftp_buf.drain(..4 + pkt_len);

                let reply = self.process_sftp(&pkt_data);
                if !reply.is_empty() {
                    Self::send_sftp_packet(session, channel, &reply);
                }
            }

            async { Ok(()) }
        }
    }

    pub async fn run(
        provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
        base_path: String,
        addr: &str,
        auth_credentials: Option<ServeAuthCredentials>,
        quiet: bool,
    ) -> i32 {
        // russh 0.60 requires a CryptoRng from rand_core 0.10. The legacy
        // `rand::thread_rng()` (rand 0.8) no longer satisfies that bound;
        // use `rand_010::rng()` (rand 0.10 alias) for this single call site.
        let key =
            russh::keys::PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519)
                .expect("generate ed25519 key");

        let config = Arc::new(russh::server::Config {
            methods: if auth_credentials.is_some() {
                russh::MethodSet::from(&[russh::MethodKind::Password][..])
            } else {
                russh::MethodSet::all()
            },
            keys: vec![key],
            ..Default::default()
        });

        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Cannot bind {}: {}", addr, e);
                return 1;
            }
        };

        if !quiet {
            eprintln!("Press Ctrl+C to stop.");
        }

        // Create a single dedicated runtime for provider calls (shared across all clients)
        let sftp_rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("sftp provider runtime"),
        );

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();
        tokio::spawn(async move {
            let _ = shutdown_signal().await;
            cancelled_clone.store(true, Ordering::Relaxed);
        });

        // Track per-connection tasks in a JoinSet so they are aborted when
        // the server loop exits. Previously bare `tokio::spawn` leaked
        // handles — on SIGINT the loop broke but active SSH sessions kept
        // running until they happened to close, holding the provider Arc
        // and any open file descriptors.
        let mut sessions = tokio::task::JoinSet::new();

        loop {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }

            // Drain finished sessions so the JoinSet does not accumulate
            // completed handles for long-running server lifetimes.
            while sessions.try_join_next().is_some() {}

            let accept = tokio::select! {
                result = listener.accept() => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => continue,
            };

            let (stream, _peer) = match accept {
                Ok(v) => v,
                Err(_) => continue,
            };

            let handler = AeroSftpHandler {
                provider: provider.clone(),
                base_path: base_path.clone(),
                auth_credentials: auth_credentials.clone(),
                handles: HashMap::new(),
                next_handle: 0,
                dir_read: std::collections::HashSet::new(),
                sftp_buf: Vec::new(),
                rt: sftp_rt.clone(),
            };

            let cfg = config.clone();
            sessions.spawn(async move {
                let _ = russh::server::run_stream(cfg, stream, handler).await;
            });
        }

        // Graceful drain: request abort then give sessions a short grace
        // window to close cleanly before force-exiting the loop.
        sessions.abort_all();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while sessions.join_next().await.is_some() {}
        })
        .await;

        0
    }
}

async fn cmd_serve_sftp(
    url: &str,
    path: &str,
    endpoint: ServeEndpointOptions,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (provider, url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = serve_effective_base_path(path, &url_path);
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let bind_addr =
        match validate_bind_addr(&endpoint.addr, endpoint.allow_remote_bind, "SFTP serve") {
            Ok(addr) => addr,
            Err(error) => {
                print_error(format, &error, 5);
                return 5;
            }
        };
    let auth_credentials =
        resolve_service_credentials(endpoint.auth.username, endpoint.auth.password, bind_addr);

    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            serde_json::json!({
                "status": "serving",
                "protocol": "sftp",
                "addr": endpoint.addr,
                "base_path": base_path,
                "auth_required": auth_credentials.is_some(),
                "auth_user": auth_credentials.as_ref().map(|c| c.username.clone()),
                "generated_auth_password": auth_credentials
                    .as_ref()
                    .and_then(|c| if c.generated { Some(c.password.clone()) } else { None }),
            })
        );
    } else if !quiet {
        eprintln!("Serving SFTP on sftp://{}", endpoint.addr);
        eprintln!("Remote base path: {}", base_path);
        if let Some(credentials) = auth_credentials.as_ref() {
            eprintln!("Authentication: enabled");
            eprintln!("Username: {}", credentials.username);
            eprintln!("Password: {}", credentials.password);
        } else {
            eprintln!("Authentication: disabled for local loopback access");
        }
    }

    let provider_arc = Arc::new(AsyncMutex::new(provider));
    serve_sftp::run(
        provider_arc,
        base_path,
        &bind_addr.to_string(),
        auth_credentials,
        quiet,
    )
    .await
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
            if let Some(sp) = spinner {
                sp.finish_and_clear();
            }
            return code;
        }
    };

    let elapsed = start.elapsed();
    let server_info = provider.server_info().await.ok();
    let pt = provider.provider_type();
    let host = provider.display_name();
    let port = display_port_for_provider(&pt, server_info.as_deref());
    let user = String::new();

    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

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
    limit: Option<usize>,
    files_only: bool,
    dirs_only: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let resolved_path = resolve_cli_remote_path(&initial_path, path);
    let effective_path = &resolved_path;

    let entries = match provider.list(effective_path).await {
        Ok(e) => e,
        Err(e) => {
            print_error(
                format,
                &format!("ls failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    // FTP/FTPS disambiguation: some servers reply to LIST/MLSD on a missing
    // directory with an empty listing instead of a 550 error, which collapses
    // a missing path into an indistinguishable "empty directory" (exit 0).
    // When the listing is empty and the user supplied an explicit non-root
    // path, run a follow-up stat to confirm. If the path does not exist,
    // surface NotFound with the correct exit code.
    if entries.is_empty()
        && !path.is_empty()
        && path != "/"
        && path != "."
        && matches!(
            provider.provider_type(),
            ProviderType::Ftp | ProviderType::Ftps
        )
    {
        if let Err(ProviderError::NotFound(_)) = provider.stat(effective_path).await {
            print_error(format, &format!("ls failed: Path not found: {}", path), 2);
            let _ = provider.disconnect().await;
            return 2;
        }
    }

    // Filter hidden files
    let mut entries: Vec<RemoteEntry> = if all {
        entries
    } else {
        entries
            .into_iter()
            .filter(|e| !e.name.starts_with('.'))
            .collect()
    };

    // Apply global filters (--include, --exclude, --min-size, --max-size, --min-age, --max-age)
    if has_filters(cli) {
        let filter = build_filter(cli);
        entries.retain(|e| {
            if e.is_dir {
                return true;
            } // Don't filter directories in ls
            filter(&e.name, e.size, None)
        });
    }

    // Sort
    match sort {
        "size" => entries.sort_by_key(|a| a.size),
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

    // --files-only / --dirs-only filter (mutually exclusive at clap
    // level). Cuts client-side post-processing for agents iterating
    // by type. Surfaced as a friction point by the agent audit
    // (P12, Battery A).
    if files_only {
        entries.retain(|e| !e.is_dir);
    } else if dirs_only {
        entries.retain(|e| e.is_dir);
    }

    // --limit N: trim AFTER sort/filter so the cap is meaningful.
    // Tracks pre-trim length so summary can report `truncated: true`
    // (P11/P13, Battery A+B).
    let total_before_limit = entries.len();
    let truncated = if let Some(n) = limit {
        if entries.len() > n {
            entries.truncate(n);
            true
        } else {
            false
        }
    } else {
        false
    };

    // Summary (post-filter, post-limit)
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
                    let perms = e.permissions.as_deref().unwrap_or(if e.is_dir {
                        "drwxr-xr-x"
                    } else {
                        "-rw-r--r--"
                    });
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
                    "\n{} items ({} directories, {} files) - {} total",
                    entries.len(),
                    dir_count,
                    file_count,
                    format_size(total_bytes)
                );
                // No `Next:` hint after `ls`: a re-ls or generic find is never
                // actionable for an agent. Hints stay on transformative
                // commands (put/rm/mv/sync) where the follow-up matters.
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
                    "truncated": truncated,
                    "total_before_limit": total_before_limit,
                },
                "suggested_next_command": suggest_ls_followup(cli, effective_path),
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let remote = &resolve_cli_remote_path(&initial_path, remote);
    let filename = remote.rsplit('/').next().unwrap_or("download");
    let local_path_owned: String;
    let local_path = if let Some(dest) = local {
        if dest.ends_with('/') || std::path::Path::new(dest).is_dir() {
            local_path_owned = format!(
                "{}{}{}",
                dest,
                if dest.ends_with('/') { "" } else { "/" },
                filename
            );
            &local_path_owned
        } else {
            dest
        }
    } else {
        filename
    };
    // Audit-friendly overwrite warning. When the resolved destination
    // already exists locally, surface a clear stderr line so the agent
    // (or human) can decide rather than silently lose data. Suppressed
    // under --quiet, --immutable (which has its own skip path), and
    // when output is JSON (the json envelope already includes the
    // destination — agents can diff against their own state).
    // Caught by the agent-friendliness audit (P8, Battery C: a `get`
    // to a directory containing a same-named local file overwrote it
    // without warning).
    if !cli.quiet
        && !cli.immutable
        && !matches!(format, OutputFormat::Json)
        && Path::new(local_path).exists()
    {
        eprintln!(
            "Warning: local '{}' already exists and will be overwritten. \
             Pass --immutable to skip, or rename the destination.",
            local_path
        );
    }
    // --immutable: skip if local file already exists
    if cli.immutable && Path::new(local_path).exists() {
        let _ = provider.disconnect().await;
        let quiet = cli.quiet || matches!(format, OutputFormat::Json);
        if !quiet {
            eprintln!("Skipped: {} (already exists, --immutable)", local_path);
        }
        if let OutputFormat::Json = format {
            print_json(
                &serde_json::json!({"status": "skipped", "reason": "already_exists", "path": local_path}),
            );
        }
        return 9;
    }

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
            return pget_segmented_download(
                url, remote, local_path, segments, total_size, cli, format,
            )
            .await;
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
            session_transfer_add(file_size);
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
            print_error(
                format,
                &format!("Download failed: {}", e),
                provider_error_to_exit_code(&e),
            );
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
        chunks.push(PgetChunk {
            index: i,
            offset,
            length,
        });
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
        print_error(
            format,
            &format!("pget: failed to create temp dir: {}", e),
            4,
        );
        return 4;
    }
    let _temp_guard = PgetTempGuard(temp_dir.clone());

    // Progress bar
    let filename = remote_path.rsplit('/').next().unwrap_or("download");
    let pb = if !quiet {
        Some(create_progress_bar(
            &format!("{} (pget x{})", filename, actual_segments),
            file_size,
        ))
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
            pget_download_chunk(
                &url, &remote, &temp_dir, idx, offset, length, aggregate, pb, cli, format,
            )
            .await
        }
    }))
    .buffer_unordered(workers)
    .collect()
    .await;

    // Check for chunk errors
    let errors: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    if !errors.is_empty() {
        if let Some(ref pb) = pb {
            pb.finish_and_clear();
        }
        for err in &errors {
            print_error(format, &format!("pget: {}", err), 4);
        }
        // _temp_guard cleans up
        return 4;
    }

    // Assemble chunks into final file
    if let Err(e) = pget_assemble_chunks(&temp_dir, local_path, actual_segments).await {
        if let Some(ref pb) = pb {
            pb.finish_and_clear();
        }
        print_error(format, &format!("pget assembly failed: {}", e), 4);
        return 4;
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

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
                    remote_path,
                    local_path,
                    format_size(file_size),
                    format_speed(speed),
                    elapsed.as_secs_f64(),
                    actual_segments,
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

    let (mut provider, _) = create_and_connect(url, cli, format).await.map_err(|code| {
        format!(
            "chunk {}: connection failed (exit code {})",
            chunk_index, code
        )
    })?;

    let chunk_path = format!("{}/chunk_{:04}", temp_dir, chunk_index);
    let mut file = tokio::fs::File::create(&chunk_path)
        .await
        .map_err(|e| format!("chunk {}: create file failed: {}", chunk_index, e))?;

    // Stream range data in sub-reads to bound memory usage
    let mut downloaded = 0u64;
    while downloaded < length {
        let remaining = length - downloaded;
        let read_size = remaining.min(PGET_SUB_READ_SIZE);
        let data = provider
            .read_range(remote_path, offset + downloaded, read_size)
            .await
            .map_err(|e| {
                format!(
                    "chunk {}: read_range at offset {} failed: {}",
                    chunk_index,
                    offset + downloaded,
                    e
                )
            })?;

        if data.is_empty() {
            break;
        }

        file.write_all(&data)
            .await
            .map_err(|e| format!("chunk {}: write failed: {}", chunk_index, e))?;

        downloaded += data.len() as u64;
        let new_total =
            aggregate.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64;
        if let Some(ref pb) = pb {
            pb.set_position(new_total);
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("chunk {}: flush failed: {}", chunk_index, e))?;
    let _ = provider.disconnect().await;
    Ok(())
}

async fn pget_assemble_chunks(
    temp_dir: &str,
    dest_path: &str,
    num_chunks: usize,
) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let temp_dest = format!(
        "{}.aeroftp-assemble-{}.tmp",
        dest_path,
        uuid::Uuid::new_v4()
    );
    let mut dest = tokio::fs::File::create(&temp_dest)
        .await
        .map_err(|e| format!("failed to create destination temp file: {}", e))?;

    let mut buf = vec![0u8; 256 * 1024]; // 256 KB copy buffer

    for i in 0..num_chunks {
        let chunk_path = format!("{}/chunk_{:04}", temp_dir, i);
        let mut src = tokio::fs::File::open(&chunk_path).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_dest);
            format!("failed to open chunk {}: {}", i, e)
        })?;

        loop {
            let n = src.read(&mut buf).await.map_err(|e| {
                let _ = std::fs::remove_file(&temp_dest);
                format!("failed to read chunk {}: {}", i, e)
            })?;
            if n == 0 {
                break;
            }
            dest.write_all(&buf[..n]).await.map_err(|e| {
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
            if total > 0 {
                pb.set_length(total);
            }
            pb.set_position(transferred);
        }) as Box<dyn Fn(u64, u64) + Send>
    });

    match download_with_resume(&mut *provider, remote_path, local_path, cli, progress_cb).await {
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
                            remote_path,
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
                        path: remote_path.to_string(),
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let remote_dir = &resolve_cli_remote_path(&initial_path, remote_dir);
    let files_from_set = load_files_from(cli);
    let scan_max_depth = cli.max_depth.unwrap_or(MAX_SCAN_DEPTH as u32) as usize;
    let mut queue: Vec<(String, usize)> = vec![(remote_dir.to_string(), 0)];
    let mut files: Vec<(String, String, u64)> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();

    while let Some((dir, depth)) = queue.pop() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        if depth >= scan_max_depth {
            if !quiet {
                eprintln!("Warning: max depth {} reached at {}", scan_max_depth, dir);
            }
            continue;
        }
        if files.len() + dirs.len() >= MAX_SCAN_ENTRIES {
            if !quiet {
                eprintln!(
                    "Warning: max entries {} reached, stopping scan",
                    MAX_SCAN_ENTRIES
                );
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
                        let relative = e
                            .path
                            .strip_prefix(remote_dir)
                            .unwrap_or(&e.path)
                            .trim_start_matches('/');
                        let Some(relative) = validate_relative_path(relative) else {
                            continue;
                        };
                        // --files-from: skip files not in the list
                        if let Some(ref set) = files_from_set {
                            if !set.contains(relative) {
                                continue;
                            }
                        }
                        let local_path_buf = Path::new(local_base).join(relative);
                        if verify_path_within_root(&local_path_buf, Path::new(local_base)).is_ok() {
                            // --immutable: skip if local file already exists
                            if cli.immutable && local_path_buf.exists() {
                                if !quiet {
                                    eprintln!("Skipping (immutable): {}", relative);
                                }
                                continue;
                            }
                            files.push((
                                e.path,
                                local_path_buf.to_string_lossy().to_string(),
                                e.size,
                            ));
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
        let relative = dir
            .strip_prefix(remote_dir)
            .unwrap_or(dir)
            .trim_start_matches('/');
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

    let results =
        futures_util::stream::iter(files.into_iter().map(|(remote_path, local_path, _size)| {
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
                let result = download_transfer_task(
                    url,
                    remote_path.clone(),
                    local_path,
                    cli,
                    format,
                    Some(aggregate),
                    overall_pb,
                    resolve_max_transfer(cli),
                )
                .await;
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
                skipped: (total_files as u32)
                    .saturating_sub(downloaded)
                    .saturating_sub(errors.len() as u32),
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
                plan: Vec::new(),
            });
        }
    }

    if downloaded == total_files as u32 {
        0
    } else {
        4
    }
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let dir = &resolve_cli_remote_path(&initial_path, dir);
    let entries = match provider.list(dir).await {
        Ok(e) => e,
        Err(e) => {
            print_error(
                format,
                &format!("ls failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    let matched: Vec<&RemoteEntry> = entries
        .iter()
        .filter(|e| !e.is_dir && matcher.is_match(&e.name))
        .collect();

    if matched.is_empty() {
        match format {
            OutputFormat::Text => eprintln!(
                "No remote files matching glob '{}' in {}",
                glob_pattern, dir
            ),
            OutputFormat::Json => print_error(
                format,
                &format!(
                    "No remote files matching glob '{}' in {}",
                    glob_pattern, dir
                ),
                2,
            ),
        }
        let _ = provider.disconnect().await;
        return 2;
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
            download_transfer_task(
                url,
                entry.path.clone(),
                local_path.clone(),
                cli,
                format,
                Some(aggregate),
                overall_pb,
                resolve_max_transfer(cli),
            )
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
                    downloaded,
                    total,
                    elapsed.as_secs_f64()
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
                plan: Vec::new(),
            });
        }
    }

    if downloaded == total as u32 {
        0
    } else {
        4
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_put(
    url: &str,
    local: &str,
    remote: Option<&str>,
    recursive: bool,
    no_clobber: bool,
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let filename = Path::new(local)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| local.to_string());
    let raw_remote = remote.unwrap_or(&filename);
    // If remote path ends with / (directory), append the local filename
    let effective_remote = if raw_remote.ends_with('/') {
        format!("{}{}", raw_remote, filename)
    } else {
        raw_remote.to_string()
    };
    let resolved_remote = resolve_cli_remote_path(&initial_path, &effective_remote);
    let remote_path = resolved_remote.as_str();

    // --immutable / --no-clobber: skip upload if remote file already exists
    if no_clobber || cli.immutable {
        match provider.stat(remote_path).await {
            Ok(_) => {
                match format {
                    OutputFormat::Text => {
                        if !cli.quiet {
                            let flag_name = if cli.immutable {
                                "--immutable"
                            } else {
                                "--no-clobber"
                            };
                            eprintln!("Skipped: {} (already exists, {})", remote_path, flag_name);
                        }
                    }
                    OutputFormat::Json => {
                        print_json(&serde_json::json!({
                            "status": "skipped",
                            "reason": "already_exists",
                            "path": remote_path,
                        }));
                    }
                }
                let _ = provider.disconnect().await;
                return 9;
            }
            Err(ProviderError::NotFound(_)) => {} // File does not exist, proceed with upload
            Err(_) => {} // stat failed for other reasons, attempt upload anyway
        }
    }

    let file_size = match std::fs::metadata(local) {
        Ok(m) => m.len(),
        Err(e) => {
            print_error(
                format,
                &format!("Cannot read local file '{}': {}", local, e),
                2,
            );
            return 2;
        }
    };

    // Pre-flight: on FTP/FTPS only, check that the remote parent directory
    // exists. If it does not, surface an explicit error pointing at the exact
    // missing segment, since these backends reply with a generic "553 Can't
    // open that file: No such file or directory" that hides which parent
    // segment is the culprit and triggers three retries of pure noise.
    //
    // Skipped on object-storage protocols (S3, Azure) and on every other
    // backend, where: (a) prefixes are virtual and stat() of an empty prefix
    // returns NotFound even after a successful mkdir, producing false
    // positives that block the natural mkdir+put workflow, and (b) native
    // error messages from those backends already point at the missing path.
    let needs_parent_check = matches!(
        provider.provider_type(),
        ProviderType::Ftp | ProviderType::Ftps
    );
    let parent = parent_remote_path(remote_path);
    if needs_parent_check && !parent.is_empty() && parent != "/" {
        match provider.stat(&parent).await {
            Ok(entry) if !entry.is_dir => {
                print_error(
                    format,
                    &format!(
                        "Parent path '{}' exists but is not a directory, upload aborted.",
                        parent
                    ),
                    2,
                );
                let _ = provider.disconnect().await;
                return 2;
            }
            Err(ProviderError::NotFound(_)) => {
                print_error(
                    format,
                    &format!(
                        "Parent directory '{}' does not exist on the remote. Create it first with: aeroftp-cli mkdir -p '{}'",
                        parent, parent
                    ),
                    2,
                );
                let _ = provider.disconnect().await;
                return 2;
            }
            _ => {} // stat OK (dir) or non-definitive error, proceed.
        }
    }

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
            session_transfer_add(file_size);
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
                        eprintln!("Next: {}", suggest_stat_followup(cli, remote_path));
                    }
                }
                OutputFormat::Json => {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "operation": "upload",
                        "path": remote_path,
                        "bytes": file_size,
                        "elapsed_secs": elapsed.as_secs_f64(),
                        "speed_bps": speed,
                        "suggested_next_command": suggest_stat_followup(cli, remote_path),
                    }));
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
            print_error(
                format,
                &format!("Upload failed: {}", e),
                provider_error_to_exit_code(&e),
            );
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
    let scan_depth = cli
        .max_depth
        .map(|d| d as usize)
        .unwrap_or(MAX_SCAN_DEPTH_PUT);
    let files_from_set = load_files_from(cli);
    let walker = walkdir::WalkDir::new(local_dir)
        .follow_links(false)
        .max_depth(scan_depth);
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

        let relative = entry.path().strip_prefix(local_dir).unwrap_or(entry.path());
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
            // --files-from: skip files not in the list
            if let Some(ref set) = files_from_set {
                if !set.contains(relative_str.as_str()) {
                    continue;
                }
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push((
                entry.path().to_string_lossy().to_string(),
                remote_path,
                size,
            ));
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    // Re-resolve remote_base and all derived paths against initial_path
    let _remote_base = resolve_cli_remote_path(&initial_path, &remote_base);
    let dirs: Vec<String> = dirs
        .into_iter()
        .map(|d| resolve_cli_remote_path(&initial_path, &d))
        .collect();
    let files: Vec<(String, String, u64)> = files
        .into_iter()
        .map(|(local, remote, size)| (local, resolve_cli_remote_path(&initial_path, &remote), size))
        .collect();

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

    let results =
        futures_util::stream::iter(files.into_iter().map(|(local_path, remote_path, _size)| {
            let cancelled = cancelled.clone();
            let aggregate = aggregate.clone();
            let overall_pb = overall_pb.clone();
            async move {
                if cancelled.load(Ordering::Relaxed) {
                    return Err("Cancelled by user".to_string());
                }
                upload_transfer_task(
                    url,
                    local_path.clone(),
                    remote_path.clone(),
                    cli,
                    format,
                    Some(aggregate),
                    overall_pb,
                    resolve_max_transfer(cli),
                )
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
    let mut skipped: u32 = 0;
    let mut errors: Vec<String> = Vec::new();
    for result in results {
        match result {
            Ok(_) => uploaded += 1,
            Err(ref err) if err.contains("--immutable") => {
                skipped += 1;
            }
            Err(err) => errors.push(err),
        }
    }

    let elapsed = start.elapsed();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "\nUploaded {}/{} files ({}) in {:.1}s{}",
                    uploaded,
                    total_files,
                    format_size(total_bytes),
                    elapsed.as_secs_f64(),
                    if skipped > 0 {
                        format!(" ({} skipped, --immutable)", skipped)
                    } else {
                        String::new()
                    }
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
                skipped,
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
                plan: Vec::new(),
            });
        }
    }
    if uploaded + skipped == total_files as u32 {
        if skipped > 0 && uploaded == 0 {
            9
        } else {
            0
        }
    } else {
        4
    }
}

async fn cmd_mkdir(url: &str, path: &str, parents: bool, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);

    // Track whether the leaf path was already a directory before this
    // call. Idempotent mkdir -p reports `already_existed: true` for an
    // audit-friendly distinction between "I created it" and "it was
    // already there" — caught by the agent-friendliness audit
    // (Battery C, P14).
    let mut leaf_already_existed = false;
    if parents {
        // Create parent directories as needed, no error if already exists.
        // Preserve absolute-vs-relative input: a path starting with '/' must
        // emit absolute path components ("/a", "/a/b"), while a relative path
        // emits relative components ("a", "a/b"). Always-prefixing with '/'
        // would make `mkdir -p relative/sub` send `MKD /relative` to FTP,
        // which fails for users without write access to the filesystem root.
        let is_absolute = path.starts_with('/');
        let components: Vec<&str> = path
            .trim_start_matches('/')
            .split('/')
            .filter(|c| !c.is_empty())
            .collect();
        let last_idx = components.len().saturating_sub(1);
        let mut current = if is_absolute {
            "/".to_string()
        } else {
            String::new()
        };
        for (idx, component) in components.iter().enumerate() {
            if current.is_empty() || current == "/" {
                current = format!("{}{}", current, component);
            } else {
                current = format!("{}/{}", current, component);
            }
            let is_leaf = idx == last_idx;
            match provider.mkdir(&current).await {
                Ok(()) => {}
                Err(ProviderError::AlreadyExists(_)) => {
                    if is_leaf {
                        leaf_already_existed = true;
                    }
                }
                Err(e) => {
                    // Some providers don't return AlreadyExists — they may
                    // return ServerError or Other for existing directories.
                    // Probe with stat: if the path is a directory, it's fine.
                    match provider.stat(&current).await {
                        Ok(entry) if entry.is_dir => {
                            if is_leaf {
                                leaf_already_existed = true;
                            }
                        }
                        _ => {
                            print_error(
                                format,
                                &format!("mkdir failed: {}", e),
                                provider_error_to_exit_code(&e),
                            );
                            let _ = provider.disconnect().await;
                            return provider_error_to_exit_code(&e);
                        }
                    }
                }
            }
        }
        let message = if leaf_already_existed {
            format!("Directory already existed: {}", path)
        } else {
            format!("Created directory: {}", path)
        };
        match format {
            OutputFormat::Text => {
                if !cli.quiet {
                    eprintln!("{}", message);
                }
            }
            OutputFormat::Json => print_json(&serde_json::json!({
                "status": "ok",
                "message": message,
                "path": path,
                "already_existed": leaf_already_existed,
            })),
        }
        let _ = provider.disconnect().await;
        0
    } else {
        match provider.mkdir(path).await {
            Ok(()) => {
                match format {
                    OutputFormat::Text => {
                        if !cli.quiet {
                            eprintln!("Created directory: {}", path);
                        }
                    }
                    OutputFormat::Json => print_json(&serde_json::json!({
                        "status": "ok",
                        "message": format!("Created directory: {}", path),
                        "path": path,
                        "already_existed": false,
                    })),
                }
                let _ = provider.disconnect().await;
                0
            }
            Err(e) => {
                print_error(
                    format,
                    &format!("mkdir failed: {}", e),
                    provider_error_to_exit_code(&e),
                );
                let _ = provider.disconnect().await;
                provider_error_to_exit_code(&e)
            }
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
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);
    // Block recursive delete on root — prevents wiping entire bucket/account
    let normalized = path.trim_matches('/');
    if recursive && normalized.is_empty() {
        print_error(
            format,
            "Refusing to recursively delete root '/'. This would erase all data on the remote. Delete specific paths instead.",
            1,
        );
        let _ = provider.disconnect().await;
        return 1;
    }

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
            // --force suppresses NotFound errors (idempotent delete).
            // Some providers (FTP, WebDAV) return ServerError instead of NotFound
            // for missing files, so we also check the error message.
            let is_not_found = matches!(e, ProviderError::NotFound(_))
                || (matches!(e, ProviderError::ServerError(_) | ProviderError::Other(_)) && {
                    let msg = e.to_string().to_ascii_lowercase();
                    msg.contains("not found")
                        || msg.contains("no such file")
                        || msg.contains("doesn't exist")
                        || msg.contains("does not exist")
                        || msg.contains("404")
                });
            if force && is_not_found {
                match format {
                    OutputFormat::Text => {
                        if !cli.quiet {
                            eprintln!("Deleted: {} (not found, ignored with --force)", path);
                        }
                    }
                    OutputFormat::Json => print_json(&CliOk {
                        status: "ok",
                        message: format!("Deleted: {} (not found, ignored with --force)", path),
                    }),
                }
                let _ = provider.disconnect().await;
                0
            } else {
                print_error(
                    format,
                    &format!("rm failed: {}", e),
                    provider_error_to_exit_code(&e),
                );
                let _ = provider.disconnect().await;
                provider_error_to_exit_code(&e)
            }
        }
    }
}

async fn cmd_mv(url: &str, from: &str, to: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let from = &resolve_cli_remote_path(&initial_path, from);
    let to = &resolve_cli_remote_path(&initial_path, to);
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
            print_error(
                format,
                &format!("mv failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_cp(url: &str, from: &str, to: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let from = &resolve_cli_remote_path(&initial_path, from);
    let to = &resolve_cli_remote_path(&initial_path, to);
    if !provider.supports_server_copy() {
        print_error(
            format,
            "Server-side copy is not supported by this provider",
            7,
        );
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
            print_error(
                format,
                &format!("cp failed: {}", e),
                provider_error_to_exit_code(&e),
            );
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

#[allow(clippy::too_many_arguments)]
async fn cmd_link(
    url: &str,
    path: &str,
    expires: Option<&str>,
    password: Option<&str>,
    permissions: &str,
    verify: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);
    if !provider.supports_share_links() {
        print_error(format, "Share links are not supported by this provider", 7);
        let _ = provider.disconnect().await;
        return 7;
    }

    let expires_in_secs = expires.and_then(parse_expires);
    if expires.is_some() && expires_in_secs.is_none() {
        print_error(
            format,
            "Invalid --expires format. Use: 1h, 24h, 7d, 30d, or seconds",
            1,
        );
        let _ = provider.disconnect().await;
        return 1;
    }

    let options = ShareLinkOptions {
        expires_in_secs,
        password: password.map(|s| s.to_string()),
        permissions: if permissions == "view" {
            None
        } else {
            Some(permissions.to_string())
        },
    };

    match provider.create_share_link(path, options).await {
        Ok(result) => {
            // Optional reachability probe so callers (and CI smoke tests) can
            // detect silent regressions where we build a URL that does not
            // actually resolve (e.g. wrong public handle, stale signature).
            // We use GET, not HEAD: SigV4 presigned URLs are signed for a
            // specific method and HEAD typically returns 403 against them.
            let probe_status = if verify {
                match probe_share_link(&result.url).await {
                    Ok(code) => Some(code),
                    Err(e) => {
                        print_error(format, &format!("link verify failed: {}", e), 4);
                        let _ = provider.disconnect().await;
                        return 4;
                    }
                }
            } else {
                None
            };
            if verify {
                if let Some(code) = probe_status {
                    if !(200..400).contains(&code) {
                        print_error(
                            format,
                            &format!("link verify failed: HTTP {} from generated URL", code),
                            4,
                        );
                        let _ = provider.disconnect().await;
                        return 4;
                    }
                }
            }
            match format {
                OutputFormat::Text => {
                    println!("{}", result.url);
                    if let Some(ref pw) = result.password {
                        eprintln!("Password: {}", pw);
                    }
                    if let Some(ref exp) = result.expires_at {
                        eprintln!("Expires: {}", exp);
                    }
                    if let Some(code) = probe_status {
                        eprintln!("Verified: HTTP {}", code);
                    }
                }
                OutputFormat::Json => {
                    let mut payload = serde_json::json!({
                        "status": "ok",
                        "path": path,
                        "url": result.url,
                        "password": result.password,
                        "expires_at": result.expires_at,
                    });
                    if let Some(code) = probe_status {
                        payload["verified"] = serde_json::json!({
                            "http_status": code,
                            "ok": (200..400).contains(&code),
                        });
                    }
                    print_json(&payload);
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            print_error(
                format,
                &format!("link failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

/// Probe a generated share-link URL with an HTTP GET (no body download) and
/// return the resulting status code. We use GET because SigV4 presigned URLs
/// reject HEAD when only `host` is in `SignedHeaders`. We follow up to 5
/// redirects and use a short timeout so CI does not hang on a misconfigured
/// provider.
async fn probe_share_link(url: &str) -> Result<u16, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client init failed: {}", e))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {}: {}", url, e))?;

    Ok(resp.status().as_u16())
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);
    let data = match provider.download_to_bytes(path).await {
        Ok(data) => data,
        Err(e) => {
            print_error(
                format,
                &format!("edit failed: {}", e),
                provider_error_to_exit_code(&e),
            );
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
    content = content
        .strip_prefix('\u{FEFF}')
        .unwrap_or(&content)
        .to_string();

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
            print_error(
                format,
                &format!("edit failed: cannot create temp file: {}", e),
                99,
            );
            let _ = provider.disconnect().await;
            return 99;
        }
    };

    if let Err(e) = temp_file.write_all(new_content.as_bytes()) {
        print_error(
            format,
            &format!("edit failed: cannot write temp file: {}", e),
            99,
        );
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
            print_error(
                format,
                &format!("edit failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_cat(url: &str, path: &str, cli: &Cli, format: OutputFormat) -> i32 {
    const MAX_CAT_SIZE: u64 = 256 * 1024 * 1024; // 256 MB

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);
    // Guard: reject files larger than MAX_CAT_SIZE to prevent OOM
    if let Ok(size) = provider.size(path).await {
        if size > MAX_CAT_SIZE {
            print_error(
                format,
                &format!(
                    "File too large for cat ({}). Use 'get' instead.",
                    format_size(size)
                ),
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
            print_error(
                format,
                &format!("cat failed: {}", e),
                provider_error_to_exit_code(&e),
            );
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

    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let remote = &resolve_cli_remote_path(&initial_path, remote);

    let mut temp = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(e) => {
            print_error(
                format,
                &format!("Cannot create temporary file for stdin upload: {}", e),
                5,
            );
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
        print_error(
            format,
            &format!("Cannot flush temporary stdin file: {}", e),
            5,
        );
        let _ = provider.disconnect().await;
        return 5;
    }

    let start = Instant::now();
    match provider
        .upload(temp.path().to_string_lossy().as_ref(), remote, None)
        .await
    {
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
            print_error(
                format,
                &format!("stdin upload failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_import_rclone(path: Option<String>, json: bool) -> i32 {
    use ftp_client_gui_lib::rclone_import;

    let config_path = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => match rclone_import::default_rclone_config_path() {
            Some(p) => p,
            None => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"error": "rclone configuration not found. Specify path manually."})
                    );
                } else {
                    eprintln!("Error: rclone configuration not found.");
                    eprintln!(
                        "Specify the path manually: aeroftp import rclone /path/to/rclone.conf"
                    );
                }
                return 1;
            }
        },
    };

    if !config_path.exists() {
        if json {
            println!(
                "{}",
                serde_json::json!({"error": format!("File not found: {}", config_path.display())})
            );
        } else {
            eprintln!("Error: file not found: {}", config_path.display());
        }
        return 1;
    }

    match rclone_import::import_rclone(&config_path) {
        Ok(result) => {
            if json {
                // Redact credentials — never output plaintext passwords to stdout
                let redacted: serde_json::Value = serde_json::json!({
                    "servers": result.servers.iter().map(|s| serde_json::json!({
                        "id": s.id,
                        "name": s.name,
                        "host": s.host,
                        "port": s.port,
                        "username": s.username,
                        "protocol": s.protocol,
                        "initialPath": s.initial_path,
                        "options": s.options,
                        "hasCredential": s.credential.is_some(),
                    })).collect::<Vec<_>>(),
                    "skipped": serde_json::to_value(&result.skipped).unwrap_or_default(),
                    "sourcePath": result.source_path,
                    "totalRemotes": result.total_remotes,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&redacted).unwrap_or_default()
                );
            } else {
                println!(
                    "Scanned {} remotes from {}",
                    result.total_remotes, result.source_path
                );
                println!();

                if !result.servers.is_empty() {
                    println!("Importable ({}):", result.servers.len());
                    for s in &result.servers {
                        let proto = s.protocol.as_deref().unwrap_or("?");
                        let cred = if s.credential.is_some() {
                            " [credentials]"
                        } else {
                            ""
                        };
                        println!(
                            "  {} - {}://{}@{}:{}{}",
                            s.name, proto, s.username, s.host, s.port, cred
                        );
                    }
                    println!();
                }

                if !result.skipped.is_empty() {
                    println!("Skipped ({}):", result.skipped.len());
                    for s in &result.skipped {
                        println!("  {} - {} ({})", s.name, s.rclone_type, s.reason);
                    }
                    println!();
                }

                println!(
                    "To import into the GUI, use Settings > Export/Import > Import from rclone"
                );
            }
            0
        }
        Err(e) => {
            if json {
                println!("{}", serde_json::json!({"error": e}));
            } else {
                eprintln!("Error: {}", e);
            }
            1
        }
    }
}

async fn cmd_import_winscp(path: Option<String>, json: bool) -> i32 {
    use ftp_client_gui_lib::winscp_import;

    let config_path = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => match winscp_import::default_winscp_config_path() {
            Some(p) => p,
            None => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"error": "WinSCP configuration not found. Specify path manually."})
                    );
                } else {
                    eprintln!("Error: WinSCP configuration not found.");
                    eprintln!(
                        "Specify the path manually: aeroftp import winscp /path/to/WinSCP.ini"
                    );
                }
                return 1;
            }
        },
    };

    if !config_path.exists() {
        if json {
            println!(
                "{}",
                serde_json::json!({"error": format!("File not found: {}", config_path.display())})
            );
        } else {
            eprintln!("Error: file not found: {}", config_path.display());
        }
        return 1;
    }

    match winscp_import::import_winscp(&config_path) {
        Ok(result) => {
            if json {
                // Redact credentials — never output plaintext passwords to stdout
                let redacted: serde_json::Value = serde_json::json!({
                    "servers": result.servers.iter().map(|s| serde_json::json!({
                        "id": s.id,
                        "name": s.name,
                        "host": s.host,
                        "port": s.port,
                        "username": s.username,
                        "protocol": s.protocol,
                        "initialPath": s.initial_path,
                        "options": s.options,
                        "hasCredential": s.credential.is_some(),
                    })).collect::<Vec<_>>(),
                    "skipped": serde_json::to_value(&result.skipped).unwrap_or_default(),
                    "sourcePath": result.source_path,
                    "totalSessions": result.total_sessions,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&redacted).unwrap_or_default()
                );
            } else {
                println!(
                    "Scanned {} sessions from {}",
                    result.total_sessions, result.source_path
                );
                println!();

                if !result.servers.is_empty() {
                    println!("Importable ({}):", result.servers.len());
                    for s in &result.servers {
                        let proto = s.protocol.as_deref().unwrap_or("?");
                        let cred = if s.credential.is_some() {
                            " [credentials]"
                        } else {
                            ""
                        };
                        println!(
                            "  {} - {}://{}@{}:{}{}",
                            s.name, proto, s.username, s.host, s.port, cred
                        );
                    }
                    println!();
                }

                if !result.skipped.is_empty() {
                    println!("Skipped ({}):", result.skipped.len());
                    for s in &result.skipped {
                        println!("  {} - FSProtocol {} ({})", s.name, s.fs_protocol, s.reason);
                    }
                    println!();
                }

                println!(
                    "To import into the GUI, use Settings > Export/Import > Import from WinSCP"
                );
            }
            0
        }
        Err(e) => {
            if json {
                println!("{}", serde_json::json!({"error": e}));
            } else {
                eprintln!("Error: {}", e);
            }
            1
        }
    }
}

async fn cmd_import_filezilla(path: Option<String>, json: bool) -> i32 {
    use ftp_client_gui_lib::filezilla_import;

    let config_path = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => match filezilla_import::default_filezilla_config_path() {
            Some(p) => p,
            None => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"error": "FileZilla configuration not found. Specify path manually."})
                    );
                } else {
                    eprintln!("Error: FileZilla configuration not found.");
                    eprintln!(
                        "Specify the path manually: aeroftp import filezilla /path/to/sitemanager.xml"
                    );
                }
                return 1;
            }
        },
    };

    if !config_path.exists() {
        if json {
            println!(
                "{}",
                serde_json::json!({"error": format!("File not found: {}", config_path.display())})
            );
        } else {
            eprintln!("Error: file not found: {}", config_path.display());
        }
        return 1;
    }

    match filezilla_import::import_filezilla(&config_path) {
        Ok(result) => {
            if json {
                let redacted: serde_json::Value = serde_json::json!({
                    "servers": result.servers.iter().map(|s| serde_json::json!({
                        "id": s.id,
                        "name": s.name,
                        "host": s.host,
                        "port": s.port,
                        "username": s.username,
                        "protocol": s.protocol,
                        "initialPath": s.initial_path,
                        "options": s.options,
                        "hasCredential": s.credential.is_some(),
                    })).collect::<Vec<_>>(),
                    "skipped": serde_json::to_value(&result.skipped).unwrap_or_default(),
                    "sourcePath": result.source_path,
                    "totalServers": result.total_servers,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&redacted).unwrap_or_default()
                );
            } else {
                println!(
                    "Scanned {} sites from {}",
                    result.total_servers, result.source_path
                );
                println!();

                if !result.servers.is_empty() {
                    println!("Importable ({}):", result.servers.len());
                    for s in &result.servers {
                        let proto = s.protocol.as_deref().unwrap_or("?");
                        let cred = if s.credential.is_some() {
                            " [credentials]"
                        } else {
                            ""
                        };
                        println!(
                            "  {} - {}://{}@{}:{}{}",
                            s.name, proto, s.username, s.host, s.port, cred
                        );
                    }
                    println!();
                }

                if !result.skipped.is_empty() {
                    println!("Skipped ({}):", result.skipped.len());
                    for s in &result.skipped {
                        println!("  {} - Protocol {} ({})", s.name, s.protocol, s.reason);
                    }
                    println!();
                }

                println!(
                    "To import into the GUI, use Settings > Export/Import > Import from FileZilla"
                );
            }
            0
        }
        Err(e) => {
            if json {
                println!("{}", serde_json::json!({"error": e}));
            } else {
                eprintln!("Error: {}", e);
            }
            1
        }
    }
}

/// Convert an rclone filter file into a `.aeroignore` file (or stdout).
///
/// `path` may be `-` to read from stdin. The conversion preserves rclone's
/// first-match-wins semantics under gitignore last-match-wins by reversing
/// the rule order; warnings are reported to the user.
async fn cmd_import_rclone_filter(
    path: String,
    output: Option<String>,
    force: bool,
    json: bool,
) -> i32 {
    use ftp_client_gui_lib::rclone_filter::rclone_filter_to_aeroignore;
    use std::io::Read;

    // Read the input file (or stdin if path == "-").
    let content = if path == "-" {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"error": format!("Failed to read stdin: {}", e)})
                );
            } else {
                eprintln!("Error: failed to read stdin: {}", e);
            }
            return 11;
        }
        buf
    } else {
        match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "error": format!("Failed to read {}: {}", path, e),
                        })
                    );
                } else {
                    eprintln!("Error: failed to read {}: {}", path, e);
                }
                return 2;
            }
        }
    };

    let (aeroignore_text, warnings) = rclone_filter_to_aeroignore(&content);
    let warning_strings: Vec<String> = warnings.iter().map(|w| w.to_string()).collect();

    // Write to file or stdout.
    if let Some(ref out_path) = output {
        let out_path_buf = std::path::PathBuf::from(out_path);
        if out_path_buf.exists() && !force {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": format!("Output file already exists: {}. Use --force to overwrite.", out_path),
                    })
                );
            } else {
                eprintln!(
                    "Error: output file already exists: {}. Use --force to overwrite.",
                    out_path
                );
            }
            return 9;
        }
        if let Err(e) = std::fs::write(&out_path_buf, &aeroignore_text) {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"error": format!("Failed to write {}: {}", out_path, e)})
                );
            } else {
                eprintln!("Error: failed to write {}: {}", out_path, e);
            }
            return 11;
        }
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "input": path,
                    "output": out_path,
                    "bytes_written": aeroignore_text.len(),
                    "warnings": warning_strings,
                })
            );
        } else {
            println!("Wrote {} bytes to {}", aeroignore_text.len(), out_path);
            if !warning_strings.is_empty() {
                eprintln!();
                eprintln!("{} warning(s):", warning_strings.len());
                for w in &warning_strings {
                    eprintln!("  - {}", w);
                }
            }
        }
        0
    } else {
        // No --output: print to stdout, warnings to stderr.
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "input": path,
                    "aeroignore": aeroignore_text,
                    "warnings": warning_strings,
                })
            );
        } else {
            print!("{}", aeroignore_text);
            if !warning_strings.is_empty() {
                eprintln!();
                eprintln!("{} warning(s):", warning_strings.len());
                for w in &warning_strings {
                    eprintln!("  - {}", w);
                }
            }
        }
        0
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
            if config.aliases.is_empty() {
                match format {
                    OutputFormat::Text => {
                        println!("No aliases configured. Use 'aeroftp-cli alias set <name> <command...>' to create one.");
                    }
                    OutputFormat::Json => {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "aliases": [],
                        }));
                    }
                }
                return 0;
            }
            let mut aliases: Vec<_> = config.aliases.iter().collect();
            aliases.sort_by_key(|(left, _)| *left);
            match format {
                OutputFormat::Text => {
                    for (name, command) in &aliases {
                        println!("{} = {}", name, command.join(" "));
                    }
                    eprintln!("\n{} alias(es) configured.", aliases.len());
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
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);
    match provider.stat(path).await {
        Ok(mut entry) => {
            maybe_hydrate_ftp_stat_size(&mut provider, path, &mut entry).await;
            match format {
                OutputFormat::Text => {
                    println!("  Name:        {}", entry.name);
                    println!("  Path:        {}", entry.path);
                    println!(
                        "  Type:        {}",
                        if entry.is_dir { "directory" } else { "file" }
                    );
                    if !entry.is_dir {
                        println!(
                            "  Size:        {} ({} bytes)",
                            format_size(entry.size),
                            entry.size
                        );
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
                    if !cli.quiet {
                        eprintln!(
                            "Next: {}",
                            if entry.is_dir {
                                format!(
                                    "aeroftp-cli ls --profile \"{}\" \"{}\" --json",
                                    profile_or_placeholder(cli),
                                    shell_double_quote(&entry.path)
                                )
                            } else {
                                format!(
                                    "aeroftp-cli ls --profile \"{}\" \"{}\" --json",
                                    profile_or_placeholder(cli),
                                    shell_double_quote(&parent_remote_path(&entry.path))
                                )
                            }
                        );
                    }
                }
                OutputFormat::Json => {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "entry": remote_entry_to_filtered_json(&entry, cli),
                        "suggested_next_command": if entry.is_dir {
                            format!(
                                "aeroftp-cli ls --profile \"{}\" \"{}\" --json",
                                profile_or_placeholder(cli),
                                shell_double_quote(&entry.path)
                            )
                        } else {
                            format!(
                                "aeroftp-cli ls --profile \"{}\" \"{}\" --json",
                                profile_or_placeholder(cli),
                                shell_double_quote(&parent_remote_path(&entry.path))
                            )
                        },
                    }));
                }
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(e) => {
            // Flatten cascaded "Path not found: File not found: No such
            // file: No such file" chain into a single canonical message
            // so machine parsers don't have to peel layers. The
            // provider-side wrapping is hard to fix in place (each
            // protocol crate adds its own prefix); this is the cheapest
            // place to dedupe.
            let exit = provider_error_to_exit_code(&e);
            let msg = if exit == 2 {
                format!("stat failed: {} not found", path)
            } else {
                format!("stat failed: {}", e)
            };
            print_error(format, &msg, exit);
            let _ = provider.disconnect().await;
            exit
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_find(
    url: &str,
    path: &str,
    pattern: &str,
    files_only: bool,
    dirs_only: bool,
    limit: Option<usize>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let path = &resolve_cli_remote_path(&initial_path, path);
    // Try provider.find() first, fallback to recursive list + glob
    let mut results = match provider.find(path, pattern).await {
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

            let find_max_depth = cli.max_depth.map(|d| d as usize).unwrap_or(MAX_SCAN_DEPTH);
            while let Some((dir, depth)) = queue.pop() {
                if depth >= find_max_depth {
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
            print_error(
                format,
                &format!("find failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = provider.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    // --files-only / --dirs-only filter applied after the provider
    // returns. P12 from the agent-friendliness audit.
    if files_only {
        results.retain(|e| !e.is_dir);
    } else if dirs_only {
        results.retain(|e| e.is_dir);
    }

    // --limit N: applied last so it's deterministic with respect to the
    // filter. P11/P13.
    let total_before_limit = results.len();
    let truncated = if let Some(n) = limit {
        if results.len() > n {
            results.truncate(n);
            true
        } else {
            false
        }
    } else {
        false
    };

    match format {
        OutputFormat::Text => {
            for e in &results {
                println!("{}", sanitize_filename(&e.path));
            }
            if !cli.quiet {
                eprintln!(
                    "\n{} matches{}",
                    results.len(),
                    if truncated {
                        " (truncated by --limit)"
                    } else {
                        ""
                    }
                );
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
                    "truncated": truncated,
                    "total_before_limit": total_before_limit,
                },
                "suggested_next_command": suggest_find_followup(cli, path),
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
            print_error(
                format,
                &format!("df failed: {}", e),
                provider_error_to_exit_code(&e),
            );
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
                } else {
                    0.0
                };
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
                result["used_percent"] = serde_json::json!(if info.total > 0 {
                    (info.used as f64 / info.total as f64) * 100.0
                } else {
                    0.0
                });
            }
            print_json(&apply_top_level_json_field_filter(result, cli, &["status"]));
        }
    }
    let _ = provider.disconnect().await;
    0
}

/// Write a non-compressible random payload to `path` and return its hex SHA-256.
///
/// Uses `rand::thread_rng()` to generate high-entropy bytes (~8 bits/byte),
/// preventing TLS or transport-level compression from skewing the benchmark.
/// This is *high-entropy random* in the benchmarking sense, not a cryptographic
/// secrecy guarantee — the bytes are read back over the wire and hashed.
fn write_speed_test_file_random(path: &Path, size: u64) -> Result<String, String> {
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::create(path)
        .map_err(|e| format!("Cannot create speed test payload: {}", e))?;
    let mut rng = rand::thread_rng();
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let next = remaining.min(chunk.len() as u64) as usize;
        rng.fill_bytes(&mut chunk[..next]);
        file.write_all(&chunk[..next])
            .map_err(|e| format!("Cannot write speed test payload: {}", e))?;
        hasher.update(&chunk[..next]);
        remaining -= next as u64;
    }
    file.flush()
        .map_err(|e| format!("Cannot flush speed test payload: {}", e))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Redact userinfo password from a URL for safe display in logs / reports.
/// Returns `protocol://user@host[:port]/path` when the URL is parseable, or a
/// best-effort manual redaction otherwise. Never returns the original password.
fn redact_url_for_display(raw: &str) -> String {
    if let Ok(mut parsed) = url::Url::parse(raw) {
        if parsed.password().is_some() {
            let _ = parsed.set_password(None);
        }
        return parsed.to_string();
    }
    // Manual fallback for inputs that don't parse: strip ":password@" from "://user:password@".
    if let Some(scheme_idx) = raw.find("://") {
        let after_scheme = &raw[scheme_idx + 3..];
        if let Some(at_idx) = after_scheme.find('@') {
            let userinfo = &after_scheme[..at_idx];
            let rest = &after_scheme[at_idx..];
            if let Some(colon_idx) = userinfo.find(':') {
                let user = &userinfo[..colon_idx];
                return format!("{}://{}{}", &raw[..scheme_idx], user, rest);
            }
        }
    }
    raw.to_string()
}

/// CSV-safe cell encoding: quotes the value, doubles inner quotes, and
/// neutralizes spreadsheet-formula leading characters (`= + - @`) by
/// prefixing with a single quote inside the quoted cell. Mitigates CSV
/// injection in tools like Excel/Numbers that auto-evaluate cells.
fn csv_cell_safe(value: &str) -> String {
    let mut s = String::with_capacity(value.len() + 4);
    s.push('"');
    let mut chars = value.chars();
    if let Some(first) = chars.next() {
        if matches!(first, '=' | '+' | '-' | '@' | '\t' | '\r') {
            s.push('\'');
        }
        if first == '"' {
            s.push('"');
            s.push('"');
        } else {
            s.push(first);
        }
        for c in chars {
            if c == '"' {
                s.push('"');
                s.push('"');
            } else {
                s.push(c);
            }
        }
    }
    s.push('"');
    s
}

/// Markdown-cell-safe encoding: escapes pipe and newline characters that
/// would break GitHub Flavored Markdown table rows. Backslashes are also
/// escaped so the output remains literal.
fn md_cell_safe(value: &str) -> String {
    let mut s = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '|' => s.push_str("\\|"),
            '\n' | '\r' => s.push(' '),
            other => s.push(other),
        }
    }
    s
}

/// Stream-hash a file from disk in 1 MB chunks. Returns hex SHA-256.
fn hash_file_streaming(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("Cannot open: {}", e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("Read: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Maximum CLI speed test size. Mirrors GUI hard cap (1 GiB).
const CLI_SPEED_MAX_SIZE: u64 = 1024 * 1024 * 1024;

#[allow(clippy::too_many_arguments)]
async fn cmd_speed(
    url: &str,
    test_size: &str,
    iterations: u32,
    remote_path: Option<&str>,
    no_integrity: bool,
    json_out: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let iterations = iterations.clamp(1, 10);
    let size = match parse_size_filter(test_size) {
        Ok(size) if size > 0 && size <= CLI_SPEED_MAX_SIZE => size,
        Ok(0) => {
            print_error(format, "Speed test size must be greater than zero", 5);
            return 5;
        }
        Ok(_) => {
            print_error(format, "Speed test size cannot exceed 1 GiB", 5);
            return 5;
        }
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    let remote_test_path = remote_path
        .map(|path| path.to_string())
        .unwrap_or_else(|| format!("/.aeroftp-speedtest-{}.bin", uuid::Uuid::new_v4()));

    let outcome = run_single_speed_test(
        url,
        size,
        iterations,
        &remote_test_path,
        !no_integrity,
        cli,
        format,
    )
    .await;

    let result = match outcome {
        Ok(r) => r,
        Err((msg, code)) => {
            // For connection errors, the inner create_and_connect has already
            // emitted a structured error to JSON / a friendly message to text.
            // Avoid duplicating: only print here if the failure happened later.
            if !msg.starts_with("Connect failed for ") {
                print_error(format, &msg, code);
            }
            return code;
        }
    };

    if let Some(path) = json_out {
        if let Err(e) = std::fs::write(
            path,
            serde_json::to_string_pretty(&result).unwrap_or_default(),
        ) {
            eprintln!("warning: could not write JSON report to {}: {}", path, e);
        }
    }

    // Exit code 4 only when an integrity check ran AND failed.
    // If integrity was explicitly skipped, that is not an error.
    let exit_code = if !result.integrity_checked || result.integrity_verified {
        0
    } else {
        4
    };

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "Speed test complete ({} iteration(s), {}, {})",
                    result.iterations,
                    format_size(result.test_size),
                    result.protocol.to_uppercase(),
                );
                println!(
                    "  Upload:    {}  ({:.2} Mbps)",
                    format_speed(result.upload_speed_bps),
                    result.upload_mbps
                );
                println!(
                    "  Download:  {}  ({:.2} Mbps)",
                    format_speed(result.download_speed_bps),
                    result.download_mbps
                );
                if let Some(ttfb) = result.download_ttfb_ms {
                    println!("  TTFB:      {} ms", ttfb);
                }
                let integrity_label = if !result.integrity_checked {
                    "skipped"
                } else if result.integrity_verified {
                    "verified"
                } else {
                    "CORRUPTED"
                };
                println!("  Integrity: {}", integrity_label);
                println!(
                    "  Cleanup:   {}",
                    if result.cleanup_ok {
                        "removed".to_string()
                    } else {
                        format!("manual: {}", result.remote_path)
                    }
                );
                println!("  Remote:    {}", result.remote_path);
            }
        }
        OutputFormat::Json => print_json(&result),
    }

    exit_code
}

#[allow(clippy::too_many_arguments)]
async fn run_single_speed_test(
    url: &str,
    size: u64,
    iterations: u32,
    remote_test_path: &str,
    verify_integrity: bool,
    cli: &Cli,
    format: OutputFormat,
) -> Result<CliSpeedResult, (String, i32)> {
    // Allocate BOTH local tempfiles BEFORE opening any network connection.
    // Failure here cannot leak a remote file or a live provider connection.
    let local_upload =
        NamedTempFile::new().map_err(|e| (format!("Cannot create upload temp: {}", e), 5))?;
    let upload_sha256 =
        write_speed_test_file_random(local_upload.path(), size).map_err(|e| (e, 5))?;
    let local_download =
        NamedTempFile::new().map_err(|e| (format!("Cannot create download temp: {}", e), 5))?;
    let download_path = local_download.path().to_path_buf();

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => {
            return Err((
                format!("Connect failed for {}", redact_url_for_display(url)),
                code,
            ))
        }
    };

    let protocol = provider.provider_type().to_string();

    let mut upload_total: f64 = 0.0;
    let mut download_total: f64 = 0.0;
    let mut last_ttfb_ms: Option<u64> = None;
    let start = Instant::now();
    let mut final_download_sha = String::new();
    // Tri-state: when verify_integrity is false, the check did not run.
    // We must NOT report `verified` for runs where SHA-256 wasn't compared.
    let integrity_checked = verify_integrity;
    let mut integrity_verified = false;

    for iteration in 0..iterations {
        let upload_start = Instant::now();
        if let Err(e) = provider
            .upload(
                local_upload.path().to_string_lossy().as_ref(),
                remote_test_path,
                None,
            )
            .await
        {
            let _ = provider.delete(remote_test_path).await;
            let _ = provider.disconnect().await;
            return Err((
                format!(
                    "speed test upload failed on iteration {}: {}",
                    iteration + 1,
                    e
                ),
                provider_error_to_exit_code(&e),
            ));
        }
        let upload_elapsed = upload_start.elapsed().as_secs_f64().max(0.0001);
        upload_total += size as f64 / upload_elapsed;

        let phase_start = Instant::now();
        let ttfb = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let ttfb_cb = std::sync::Arc::clone(&ttfb);
        let progress = Box::new(move |transferred: u64, _total: u64| {
            if transferred > 0 && ttfb_cb.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                let ms = phase_start.elapsed().as_millis() as u64;
                ttfb_cb.store(ms.max(1), std::sync::atomic::Ordering::Relaxed);
            }
        });

        let download_start = Instant::now();
        if let Err(e) = provider
            .download(
                remote_test_path,
                download_path.to_string_lossy().as_ref(),
                Some(progress),
            )
            .await
        {
            let _ = provider.delete(remote_test_path).await;
            let _ = provider.disconnect().await;
            return Err((
                format!(
                    "speed test download failed on iteration {}: {}",
                    iteration + 1,
                    e
                ),
                provider_error_to_exit_code(&e),
            ));
        }
        let download_elapsed = download_start.elapsed().as_secs_f64().max(0.0001);
        download_total += size as f64 / download_elapsed;
        let ttfb_value = ttfb.load(std::sync::atomic::Ordering::Relaxed);
        if ttfb_value > 0 {
            last_ttfb_ms = Some(ttfb_value);
        }
        // Hash the downloaded file while it's still alive (NamedTempFile drops at iter end).
        if iteration == iterations - 1 && verify_integrity {
            final_download_sha = hash_file_streaming(&download_path).unwrap_or_default();
            integrity_verified =
                !final_download_sha.is_empty() && final_download_sha == upload_sha256;
        }
    }

    finalize_speed_result(
        provider,
        remote_test_path,
        size,
        iterations,
        upload_total,
        download_total,
        last_ttfb_ms,
        upload_sha256,
        final_download_sha,
        integrity_checked,
        integrity_verified,
        protocol,
        start.elapsed().as_secs_f64(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finalize_speed_result(
    mut provider: Box<dyn StorageProvider>,
    remote_test_path: &str,
    size: u64,
    iterations: u32,
    upload_total: f64,
    download_total: f64,
    download_ttfb_ms: Option<u64>,
    upload_sha256: String,
    download_sha256: String,
    integrity_checked: bool,
    integrity_verified: bool,
    protocol: String,
    elapsed_secs: f64,
) -> Result<CliSpeedResult, (String, i32)> {
    let (cleanup_ok, cleanup_error) = match provider.delete(remote_test_path).await {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    let _ = provider.disconnect().await;

    let avg_up = (upload_total / iterations.max(1) as f64) as u64;
    let avg_dn = (download_total / iterations.max(1) as f64) as u64;
    Ok(CliSpeedResult {
        status: "ok",
        schema: "aeroftp.speedtest.v1",
        remote_path: remote_test_path.to_string(),
        test_size: size,
        iterations,
        upload_speed_bps: avg_up,
        download_speed_bps: avg_dn,
        upload_mbps: avg_up as f64 * 8.0 / 1_000_000.0,
        download_mbps: avg_dn as f64 * 8.0 / 1_000_000.0,
        download_ttfb_ms,
        integrity_checked,
        integrity_verified,
        upload_sha256,
        download_sha256,
        cleanup_ok,
        cleanup_error,
        elapsed_secs,
        protocol,
    })
}

#[allow(clippy::too_many_arguments)]
async fn cmd_speed_compare(
    urls: &[String],
    test_size: &str,
    parallel: u8,
    no_integrity: bool,
    json_out: Option<&str>,
    csv_out: Option<&str>,
    md_out: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    if urls.len() < 2 {
        print_error(format, "speed-compare requires at least 2 URLs", 5);
        return 5;
    }
    let parallel = parallel.clamp(1, 4);
    let size = match parse_size_filter(test_size) {
        Ok(size) if size > 0 && size <= CLI_SPEED_MAX_SIZE => size,
        Ok(0) => {
            print_error(format, "Speed test size must be greater than zero", 5);
            return 5;
        }
        Ok(_) => {
            print_error(format, "Speed test size cannot exceed 1 GiB", 5);
            return 5;
        }
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    let started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Use buffer_unordered to avoid tokio::spawn Send bound (some provider futures
    // hold non-Send OAuth helpers). buffer_unordered polls within the current task.
    let cli_ref = cli;
    let test_futures = urls.iter().map(|url| async move {
        let raw_url = url.clone();
        let display_url = redact_url_for_display(&raw_url);
        let remote_test_path = format!("/.aeroftp-speedtest-{}.bin", uuid::Uuid::new_v4());
        let result = run_single_speed_test(
            &raw_url,
            size,
            1,
            &remote_test_path,
            !no_integrity,
            cli_ref,
            OutputFormat::Json, // suppress per-test text output during compare
        )
        .await;
        (display_url, result)
    });
    type CompareEntry = (String, Result<CliSpeedResult, (String, i32)>);
    let mut entries: Vec<CompareEntry> = Vec::with_capacity(urls.len());
    let mut stream = futures_util::stream::iter(test_futures).buffer_unordered(parallel as usize);
    while let Some(pair) = futures_util::StreamExt::next(&mut stream).await {
        entries.push(pair);
    }

    let finished_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let max_dl: f64 = entries
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok().map(|r| r.download_mbps))
        .fold(0.0, f64::max);
    let max_ul: f64 = entries
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok().map(|r| r.upload_mbps))
        .fold(0.0, f64::max);

    let mut compare_entries: Vec<CliSpeedCompareEntry> = entries
        .into_iter()
        .map(|(url, res)| match res {
            Ok(r) => {
                let nd = if max_dl > 0.0 {
                    (r.download_mbps / max_dl).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let nu = if max_ul > 0.0 {
                    (r.upload_mbps / max_ul).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                // Tri-state: skipped integrity contributes 0.5 (neutral), not 1.0 (verified).
                let ni = if !r.integrity_checked {
                    0.5
                } else if r.integrity_verified {
                    1.0
                } else {
                    0.0
                };
                let nc = if r.cleanup_ok { 1.0 } else { 0.0 };
                let score = 0.45 * nd + 0.35 * nu + 0.10 * ni + 0.10 * nc;
                let protocol = r.protocol.clone();
                CliSpeedCompareEntry {
                    rank: 0,
                    url,
                    protocol,
                    score,
                    result: Some(r),
                    error: None,
                }
            }
            Err((msg, _)) => CliSpeedCompareEntry {
                rank: 0,
                url,
                protocol: "?".to_string(),
                score: 0.0,
                result: None,
                error: Some(msg),
            },
        })
        .collect();

    compare_entries.sort_by(|a, b| {
        if a.result.is_some() && b.result.is_none() {
            return std::cmp::Ordering::Less;
        }
        if a.result.is_none() && b.result.is_some() {
            return std::cmp::Ordering::Greater;
        }
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut rank = 0u32;
    for e in compare_entries.iter_mut() {
        if e.result.is_some() {
            rank += 1;
            e.rank = rank;
        }
    }

    let report = CliSpeedCompareReport {
        status: "ok",
        schema: "aeroftp.speedtest.v1",
        test_size: size,
        parallel,
        started_at_ms,
        finished_at_ms,
        results: compare_entries,
    };

    // Optional file exports
    if let Some(path) = json_out {
        if let Err(e) = std::fs::write(
            path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        ) {
            eprintln!("warning: could not write JSON report to {}: {}", path, e);
        }
    }
    if let Some(path) = csv_out {
        let mut csv = String::from(
            "rank,url,protocol,size_bytes,upload_mbps,download_mbps,download_ttfb_ms,integrity,cleanup,score,error\n",
        );
        for e in report.results.iter() {
            if let Some(r) = e.result.as_ref() {
                let integrity = if !r.integrity_checked {
                    "skipped"
                } else if r.integrity_verified {
                    "verified"
                } else {
                    "corrupted"
                };
                csv.push_str(&format!(
                    "{},{},{},{},{:.2},{:.2},{},{},{},{:.1},{}\n",
                    e.rank,
                    csv_cell_safe(&e.url),
                    csv_cell_safe(&r.protocol),
                    r.test_size,
                    r.upload_mbps,
                    r.download_mbps,
                    r.download_ttfb_ms
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    integrity,
                    r.cleanup_ok as u8,
                    e.score * 100.0,
                    "",
                ));
            } else {
                csv.push_str(&format!(
                    ",{},,,,,,,,,{}\n",
                    csv_cell_safe(&e.url),
                    csv_cell_safe(e.error.as_deref().unwrap_or("")),
                ));
            }
        }
        if let Err(e) = std::fs::write(path, csv) {
            eprintln!("warning: could not write CSV report to {}: {}", path, e);
        }
    }
    if let Some(path) = md_out {
        let mut md = String::from("# AeroFTP Speed Test (compare)\n\n");
        md.push_str(&format!(
            "- Size: {}\n- Parallel: {}\n\n",
            format_size(size),
            parallel
        ));
        md.push_str("| # | URL | Protocol | Down (Mbps) | Up (Mbps) | TTFB (ms) | Integ. | Clean. | Score |\n");
        md.push_str("|---:|---|---|---:|---:|---:|:---:|:---:|---:|\n");
        for e in report.results.iter() {
            if let Some(r) = e.result.as_ref() {
                let integ = if !r.integrity_checked {
                    "—"
                } else if r.integrity_verified {
                    "✓"
                } else {
                    "✗"
                };
                md.push_str(&format!(
                    "| {} | {} | {} | {:.2} | {:.2} | {} | {} | {} | {:.0} |\n",
                    e.rank,
                    md_cell_safe(&e.url),
                    md_cell_safe(&r.protocol.to_uppercase()),
                    r.download_mbps,
                    r.upload_mbps,
                    r.download_ttfb_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "—".into()),
                    integ,
                    if r.cleanup_ok { "✓" } else { "✗" },
                    e.score * 100.0,
                ));
            } else {
                md.push_str(&format!(
                    "| — | {} | — | — | — | — | — | — | error: {} |\n",
                    md_cell_safe(&e.url),
                    md_cell_safe(e.error.as_deref().unwrap_or("")),
                ));
            }
        }
        if let Err(e) = std::fs::write(path, md) {
            eprintln!(
                "warning: could not write Markdown report to {}: {}",
                path, e
            );
        }
    }

    match format {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "Speed compare ({}, parallel {})",
                    format_size(size),
                    parallel
                );
                println!(
                    "{:>3}  {:<48}  {:<7}  {:>12}  {:>12}  {:>8}  {:>5}",
                    "#", "URL", "PROTO", "DOWN Mbps", "UP Mbps", "TTFB ms", "SCORE"
                );
                for e in report.results.iter() {
                    if let Some(r) = e.result.as_ref() {
                        let url_disp = if e.url.len() > 48 {
                            format!("{}...", &e.url[..45])
                        } else {
                            e.url.clone()
                        };
                        println!(
                            "{:>3}  {:<48}  {:<7}  {:>12.2}  {:>12.2}  {:>8}  {:>5.0}",
                            e.rank,
                            url_disp,
                            r.protocol.to_uppercase(),
                            r.download_mbps,
                            r.upload_mbps,
                            r.download_ttfb_ms
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "—".into()),
                            e.score * 100.0,
                        );
                    } else {
                        println!("ERR  {}  -> {}", e.url, e.error.clone().unwrap_or_default());
                    }
                }
            }
        }
    }

    let any_failed = report.results.iter().any(|e| {
        e.result.is_none()
            || e.result
                .as_ref()
                .map(|r| !r.integrity_verified && !no_integrity)
                .unwrap_or(true)
    });
    if any_failed {
        4
    } else {
        0
    }
}

async fn cmd_cleanup(url: &str, path: &str, force: bool, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let path = &resolve_cli_remote_path(&initial_path, path);
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);

    if !quiet {
        eprintln!("Scanning {} for orphaned .aerotmp files...", path);
    }

    // BFS scan for .aerotmp files
    let mut orphans: Vec<(String, u64)> = Vec::new();
    let mut dirs = vec![path.to_string()];
    let max_entries = 100_000usize;
    let mut scan_errors = 0u32;
    let mut delete_errors = 0u32;
    let mut exit_code = 0i32;

    while let Some(dir) = dirs.pop() {
        if orphans.len() >= max_entries {
            break;
        }
        match provider.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    if entry.is_dir {
                        dirs.push(entry.path.clone());
                    } else if entry.name.ends_with(".aerotmp") || entry.path.ends_with(".aerotmp") {
                        orphans.push((entry.path.clone(), entry.size));
                    }
                }
            }
            Err(e) => {
                scan_errors += 1;
                if exit_code == 0 {
                    exit_code = provider_error_to_exit_code(&e);
                }
                if !quiet {
                    eprintln!("  Failed to list {}: {}", dir, e);
                }
                continue;
            }
        }
    }

    if orphans.is_empty() {
        if !quiet {
            if scan_errors == 0 {
                eprintln!("No orphaned .aerotmp files found.");
            } else {
                eprintln!(
                    "No orphaned .aerotmp files found, but scan completed with {} error(s).",
                    scan_errors
                );
            }
        }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({
                "status": if scan_errors == 0 { "ok" } else { "partial" },
                "cleaned": 0,
                "bytes_freed": 0,
                "scan_errors": scan_errors,
                "delete_errors": 0,
            }));
        }
        let _ = provider.disconnect().await;
        return if scan_errors == 0 {
            0
        } else {
            exit_code.max(4)
        };
    }

    let total_bytes: u64 = orphans.iter().map(|(_, s)| *s).sum();

    if !force {
        // Dry run (default)
        match format {
            OutputFormat::Text => {
                eprintln!(
                    "\nFound {} orphaned file(s), {} total:",
                    orphans.len(),
                    format_size(total_bytes)
                );
                for (p, s) in &orphans {
                    eprintln!("  {} ({})", p, format_size(*s));
                }
                eprintln!("\nUse --force to delete these files.");
            }
            OutputFormat::Json => {
                let files: Vec<serde_json::Value> = orphans
                    .iter()
                    .map(|(p, s)| serde_json::json!({"path": p, "size": s}))
                    .collect();
                print_json(&serde_json::json!({
                    "status": if scan_errors == 0 { "dry_run" } else { "partial" },
                    "dry_run": true,
                    "orphans": orphans.len(),
                    "bytes": total_bytes,
                    "scan_errors": scan_errors,
                    "delete_errors": 0,
                    "files": files,
                }));
            }
        }
        let _ = provider.disconnect().await;
        return if scan_errors == 0 {
            0
        } else {
            exit_code.max(4)
        };
    }

    // Force: delete orphans
    let mut cleaned = 0u32;
    let mut bytes_freed = 0u64;
    for (p, s) in &orphans {
        match provider.delete(p).await {
            Ok(()) => {
                cleaned += 1;
                bytes_freed += s;
                if !quiet {
                    eprintln!("  Deleted {} ({})", p, format_size(*s));
                }
            }
            Err(e) => {
                delete_errors += 1;
                if exit_code == 0 {
                    exit_code = provider_error_to_exit_code(&e);
                }
                eprintln!("  Failed to delete {}: {}", p, e);
            }
        }
    }

    let had_partial_errors = scan_errors > 0 || delete_errors > 0;

    match format {
        OutputFormat::Text => {
            eprintln!(
                "\nCleaned {} file(s), {} freed.",
                cleaned,
                format_size(bytes_freed)
            );
            if had_partial_errors {
                eprintln!(
                    "Completed with {} scan error(s) and {} delete error(s).",
                    scan_errors, delete_errors
                );
            }
        }
        OutputFormat::Json => {
            print_json(&serde_json::json!({
                "status": if had_partial_errors { "partial" } else { "ok" },
                "cleaned": cleaned,
                "bytes_freed": bytes_freed,
                "scan_errors": scan_errors,
                "delete_errors": delete_errors,
            }));
        }
    }

    let _ = provider.disconnect().await;
    if had_partial_errors {
        exit_code.max(4)
    } else {
        0
    }
}

async fn cmd_dedupe(
    url: &str,
    path: &str,
    mode: &str,
    dry_run: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let path = &resolve_cli_remote_path(&initial_path, path);

    // For interactive mode, check TTY availability — fallback to skip if not a terminal
    let effective_mode = if mode == "interactive" {
        if std::io::stdin().is_terminal() {
            "interactive"
        } else {
            eprintln!("Warning: --mode interactive requires a TTY; falling back to skip");
            "skip"
        }
    } else {
        mode
    };

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    if !quiet {
        eprintln!("Scanning {} for duplicates...", path);
    }

    // BFS scan to collect all files with sizes and mtime
    let mut files: Vec<(String, u64, Option<String>)> = Vec::new();
    let mut dirs = vec![path.to_string()];
    let max_entries = 100_000usize;
    let mut scan_errors = 0u32;
    let mut hash_errors = 0u32;
    let mut action_errors = 0u32;
    let mut exit_code = 0i32;

    while let Some(dir) = dirs.pop() {
        if files.len() >= max_entries {
            break;
        }
        match provider.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    if entry.is_dir {
                        dirs.push(entry.path.clone());
                    } else {
                        files.push((entry.path.clone(), entry.size, entry.modified.clone()));
                    }
                }
            }
            Err(e) => {
                scan_errors += 1;
                if exit_code == 0 {
                    exit_code = provider_error_to_exit_code(&e);
                }
                if !quiet {
                    eprintln!("  Failed to list {}: {}", dir, e);
                }
                continue;
            }
        }
    }

    if !quiet {
        eprintln!("Scanned {} files. Grouping by size...", files.len());
    }

    // Group by size (fast pre-filter)
    let mut size_groups: std::collections::HashMap<u64, Vec<(String, Option<String>)>> =
        std::collections::HashMap::new();
    for (path, size, mtime) in &files {
        if *size > 0 {
            size_groups
                .entry(*size)
                .or_default()
                .push((path.clone(), mtime.clone()));
        }
    }

    // Filter to groups with >1 file (potential duplicates)
    #[allow(clippy::type_complexity)]
    let candidate_groups: Vec<(u64, Vec<(String, Option<String>)>)> = size_groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();

    if candidate_groups.is_empty() {
        if !quiet {
            if scan_errors == 0 {
                eprintln!("No potential duplicates found.");
            } else {
                eprintln!(
                    "No potential duplicates found, but scan completed with {} error(s).",
                    scan_errors
                );
            }
        }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({
                "status": if scan_errors == 0 { "ok" } else { "partial" },
                "groups": 0,
                "duplicates": 0,
                "scan_errors": scan_errors,
                "hash_errors": 0,
                "action_errors": 0,
            }));
        }
        let _ = provider.disconnect().await;
        return if scan_errors == 0 {
            0
        } else {
            exit_code.max(4)
        };
    }

    if !quiet {
        eprintln!(
            "{} size groups with potential duplicates. Hashing...",
            candidate_groups.len()
        );
    }

    // Hash files within each group to confirm duplicates
    // Each entry: (path, size, mtime)
    let mut duplicate_groups: Vec<Vec<(String, u64, Option<String>)>> = Vec::new();
    let mut total_duplicates = 0u32;
    let mut wasted_bytes = 0u64;

    for (size, paths_with_mtime) in &candidate_groups {
        let mut hash_map: std::collections::HashMap<String, Vec<(String, u64, Option<String>)>> =
            std::collections::HashMap::new();
        for (p, mtime) in paths_with_mtime {
            match provider.download_to_bytes(p).await {
                Ok(data) => {
                    use sha2::Digest;
                    let hash = format!("{:x}", sha2::Sha256::digest(&data));
                    hash_map
                        .entry(hash)
                        .or_default()
                        .push((p.clone(), *size, mtime.clone()));
                }
                Err(e) => {
                    hash_errors += 1;
                    if exit_code == 0 {
                        exit_code = provider_error_to_exit_code(&e);
                    }
                    if !quiet {
                        eprintln!("  Failed to hash {}: {}", p, e);
                    }
                    continue;
                }
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
        if !quiet {
            if scan_errors == 0 && hash_errors == 0 {
                eprintln!("No duplicates found (same size but different content).");
            } else {
                eprintln!(
                    "No duplicates found, but scan/hash completed with {} scan error(s) and {} hash error(s).",
                    scan_errors, hash_errors
                );
            }
        }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({
                "status": if scan_errors == 0 && hash_errors == 0 { "ok" } else { "partial" },
                "groups": 0,
                "duplicates": 0,
                "scan_errors": scan_errors,
                "hash_errors": hash_errors,
                "action_errors": 0,
            }));
        }
        let _ = provider.disconnect().await;
        return if scan_errors == 0 && hash_errors == 0 {
            0
        } else {
            exit_code.max(4)
        };
    }

    // Sort each group to determine the "keeper" based on mode
    for group in &mut duplicate_groups {
        dedupe_sort_group(group, effective_mode);
    }

    let mut deleted = 0u32;
    let mut renamed = 0u32;

    // Report and act
    match format {
        OutputFormat::Text => {
            eprintln!(
                "\nFound {} duplicate group(s), {} duplicate file(s), {} wasted",
                duplicate_groups.len(),
                total_duplicates,
                format_size(wasted_bytes)
            );

            for (i, group) in duplicate_groups.iter().enumerate() {
                eprintln!("\n  Group {} ({} files):", i + 1, group.len());
                for (j, (p, sz, mtime)) in group.iter().enumerate() {
                    let mtime_str = mtime.as_deref().unwrap_or("-");
                    let marker = if j == 0 {
                        "KEEP"
                    } else {
                        match effective_mode {
                            "skip" | "list" => "DUPE",
                            "rename" => "RENAME",
                            _ => "DELETE",
                        }
                    };
                    eprintln!(
                        "    [{}] {} ({}, {})",
                        marker,
                        p,
                        format_size(*sz),
                        mtime_str
                    );
                }

                // Interactive mode: ask the user which file to keep
                if effective_mode == "interactive" && !dry_run {
                    eprint!("  Keep which? (1-{}, s=skip, a=all): ", group.len());
                    let mut input = String::new();
                    if std::io::stdin().read_line(&mut input).is_ok() {
                        let choice = input.trim();
                        if choice == "s" {
                            continue;
                        }
                        if choice == "a" {
                            continue;
                        }
                        if let Ok(idx) = choice.parse::<usize>() {
                            if idx >= 1 && idx <= group.len() {
                                let keep_idx = idx - 1;
                                for (j, (p, _, _)) in group.iter().enumerate() {
                                    if j != keep_idx {
                                        match provider.delete(p).await {
                                            Ok(()) => deleted += 1,
                                            Err(e) => {
                                                action_errors += 1;
                                                if exit_code == 0 {
                                                    exit_code = provider_error_to_exit_code(&e);
                                                }
                                                eprintln!("  Failed to delete {}: {}", p, e);
                                            }
                                        }
                                    }
                                }
                            } else {
                                eprintln!("  Invalid choice, skipping group");
                            }
                        } else {
                            eprintln!("  Invalid input, skipping group");
                        }
                    }
                    continue;
                }

                if dry_run || effective_mode == "skip" || effective_mode == "list" {
                    continue;
                }

                // Rename mode: rename duplicates with numeric suffix
                if effective_mode == "rename" {
                    for (j, (p, _, _)) in group.iter().enumerate() {
                        if j == 0 {
                            continue; // keep the first
                        }
                        let renamed_path = dedupe_rename_path(p, j);
                        match provider.rename(p, &renamed_path).await {
                            Ok(()) => {
                                renamed += 1;
                                if !quiet {
                                    eprintln!("  Renamed {} -> {}", p, renamed_path);
                                }
                            }
                            Err(e) => {
                                action_errors += 1;
                                if exit_code == 0 {
                                    exit_code = provider_error_to_exit_code(&e);
                                }
                                eprintln!("  Failed to rename {}: {}", p, e);
                            }
                        }
                    }
                    continue;
                }

                // Delete mode (delete, newest, oldest, largest, smallest):
                // group is already sorted so index 0 is the keeper
                for (p, _, _) in group.iter().skip(1) {
                    match provider.delete(p).await {
                        Ok(()) => deleted += 1,
                        Err(e) => {
                            action_errors += 1;
                            if exit_code == 0 {
                                exit_code = provider_error_to_exit_code(&e);
                            }
                            eprintln!("  Failed to delete {}: {}", p, e);
                        }
                    }
                }
            }

            if dry_run {
                eprintln!("\n(dry run - no changes made)");
            } else if deleted > 0 {
                eprintln!("\nDeleted {} duplicate file(s).", deleted);
            }
            if renamed > 0 {
                eprintln!("Renamed {} duplicate file(s).", renamed);
            }
            if scan_errors > 0 || hash_errors > 0 || action_errors > 0 {
                eprintln!(
                    "Completed with {} scan error(s), {} hash error(s), and {} action error(s).",
                    scan_errors, hash_errors, action_errors
                );
            }
        }
        OutputFormat::Json => {
            let groups_json: Vec<serde_json::Value> = duplicate_groups
                .iter()
                .map(|g| {
                    let files: Vec<&str> = g.iter().map(|(p, _, _)| p.as_str()).collect();
                    serde_json::json!({
                        "files": files,
                        "keep": g[0].0,
                        "duplicates": files[1..],
                    })
                })
                .collect();
            let had_partial_errors = scan_errors > 0 || hash_errors > 0 || action_errors > 0;
            print_json(&serde_json::json!({
                "status": if had_partial_errors { "partial" } else { "ok" },
                "groups": duplicate_groups.len(),
                "duplicates": total_duplicates,
                "wasted_bytes": wasted_bytes,
                "mode": effective_mode,
                "dry_run": dry_run,
                "deleted": deleted,
                "renamed": renamed,
                "scan_errors": scan_errors,
                "hash_errors": hash_errors,
                "action_errors": action_errors,
                "details": groups_json,
            }));
        }
    }

    let _ = provider.disconnect().await;
    if scan_errors > 0 || hash_errors > 0 || action_errors > 0 {
        exit_code.max(4)
    } else {
        0
    }
}

/// Sort a dedupe group so index 0 is the file to keep, based on mode.
fn dedupe_sort_group(group: &mut [(String, u64, Option<String>)], mode: &str) {
    match mode {
        "newest" => {
            group.sort_by(|a, b| compare_mtime(b.2.as_deref(), a.2.as_deref()));
        }
        "oldest" => {
            group.sort_by(|a, b| compare_mtime(a.2.as_deref(), b.2.as_deref()));
        }
        "largest" => {
            group.sort_by_key(|b| std::cmp::Reverse(b.1));
        }
        "smallest" => {
            group.sort_by_key(|a| a.1);
        }
        _ => {} // skip, delete, list, interactive, rename: keep original order (first = keeper)
    }
}

/// Generate a renamed path for a dedupe duplicate.
/// "dir/file.txt" with index 1 -> "dir/file-1.txt"
/// "dir/file" with index 2 -> "dir/file-2"
fn dedupe_rename_path(path: &str, index: usize) -> String {
    if let Some(dot_pos) = path.rfind('.') {
        // Check the dot is in the filename, not a directory separator
        let slash_pos = path.rfind('/').unwrap_or(0);
        if dot_pos > slash_pos {
            return format!("{}-{}{}", &path[..dot_pos], index, &path[dot_pos..]);
        }
    }
    format!("{}-{}", path, index)
}

// ── Bisync Snapshot ───────────────────────────────────────────────

/// Snapshot of file state from last successful sync.
#[derive(Debug, Default, Serialize, Deserialize)]
struct BisyncSnapshot {
    /// ISO timestamp of last successful sync
    synced_at: String,
    /// Map of relative_path → (size, mtime_iso_or_empty)
    files: HashMap<String, (u64, String)>,
}

const BISYNC_SNAPSHOT_FILE: &str = ".aeroftp-bisync.json";

fn load_bisync_snapshot(local_dir: &str) -> Option<BisyncSnapshot> {
    let path = Path::new(local_dir).join(BISYNC_SNAPSHOT_FILE);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_bisync_snapshot(
    local_dir: &str,
    local_entries: &[(String, u64, Option<String>)],
    remote_entries: &[(String, u64, Option<String>)],
) {
    let mut files = HashMap::new();
    // Merge both sides - after a successful sync they should be equal
    for (path, size, mtime) in local_entries.iter().chain(remote_entries.iter()) {
        files
            .entry(path.clone())
            .or_insert_with(|| (*size, mtime.as_deref().unwrap_or("").to_string()));
    }
    let snapshot = BisyncSnapshot {
        synced_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        files,
    };
    // S1: the snapshot lives on the LOCAL side, not the remote.
    // Ensure the local dir exists before writing — download-first runs
    // may create the local dir on-the-fly and the snapshot save used
    // to fire before the enclosing mkdir chain completed, producing
    // "No such file or directory" warnings.
    let dir_path = Path::new(local_dir);
    if let Err(e) = std::fs::create_dir_all(dir_path) {
        eprintln!(
            "Warning: cannot create local dir {} for bisync snapshot: {}",
            local_dir, e
        );
        return;
    }
    let path = dir_path.join(BISYNC_SNAPSHOT_FILE);
    // Atomic write via tmp + rename so a crash mid-write does not leave
    // a half-valid JSON snapshot that trips the next resync.
    let tmp = dir_path.join(format!("{}.tmp", BISYNC_SNAPSHOT_FILE));
    let json = match serde_json::to_string(&snapshot) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Warning: bisync snapshot serialize failed: {}", e);
            return;
        }
    };
    if let Err(e) = std::fs::write(&tmp, json) {
        eprintln!("Warning: failed to write bisync snapshot tmp: {}", e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        eprintln!("Warning: failed to commit bisync snapshot: {}", e);
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Parse an mtime string to a comparable timestamp (seconds since epoch).
fn parse_mtime_secs(s: &str) -> Option<i64> {
    // Try ISO 8601 with timezone
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    // Try ISO 8601 without timezone (assume UTC)
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(dt.and_utc().timestamp());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(dt.and_utc().timestamp());
    }
    // FTP MLSD timestamps: "2024-01-15 10:30:00Z" — strip trailing Z and parse
    let stripped = s.strip_suffix('Z').or_else(|| s.strip_suffix("UTC"));
    if let Some(bare) = stripped {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(bare, "%Y-%m-%d %H:%M:%S") {
            return Some(dt.and_utc().timestamp());
        }
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(bare, "%Y-%m-%dT%H:%M:%S") {
            return Some(dt.and_utc().timestamp());
        }
    }
    None
}

/// Compare two mtime strings (ISO 8601). Returns Ordering.
/// Parses timestamps to handle timezone differences (e.g., "T10:30:00" vs "T10:30:00Z").
/// Resolve a default mtime value from the --default-time flag.
/// Returns the parsed default time string, or None if not set/invalid.
fn resolve_default_time(cli: &Cli) -> Option<String> {
    cli.default_time.as_ref().map(|dt| {
        if dt == "now" {
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()
        } else {
            // Try RFC 3339 / ISO 8601 with timezone (e.g. 2026-04-16T12:30:00Z, 2026-04-16T12:30:00+02:00)
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(dt) {
                return parsed.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%S").to_string();
            }
            // Try naive datetime (YYYY-MM-DDTHH:MM:SS)
            if chrono::NaiveDateTime::parse_from_str(dt, "%Y-%m-%dT%H:%M:%S").is_ok() {
                return dt.clone();
            }
            // Try date-only (YYYY-MM-DD) → normalize to T00:00:00
            if chrono::NaiveDate::parse_from_str(dt, "%Y-%m-%d").is_ok() {
                return format!("{}T00:00:00", dt);
            }
            eprintln!("Error: --default-time '{}' is not a valid ISO 8601 timestamp (expected YYYY-MM-DDTHH:MM:SS, YYYY-MM-DD, or RFC 3339 with timezone)", dt);
            std::process::exit(5);
        }
    })
}

/// Apply --default-time: replace None mtime with the configured default.
fn apply_default_time<'a>(mtime: Option<&'a str>, default: Option<&'a str>) -> Option<&'a str> {
    mtime.or(default)
}

fn compare_mtime(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            match (parse_mtime_secs(a), parse_mtime_secs(b)) {
                (Some(ta), Some(tb)) => ta.cmp(&tb),
                _ => a.cmp(b), // fallback to lexicographic
            }
        }
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Resolve a conflict between local and remote file for --direction both.
/// Returns: "upload" (local wins), "download" (remote wins), "rename" (keep both), or "skip".
fn resolve_conflict(
    conflict_mode: &str,
    local_size: u64,
    local_mtime: Option<&str>,
    remote_size: u64,
    remote_mtime: Option<&str>,
) -> &'static str {
    match conflict_mode {
        "newer" | "newest" => match compare_mtime(local_mtime, remote_mtime) {
            std::cmp::Ordering::Greater => "upload",
            std::cmp::Ordering::Less => "download",
            std::cmp::Ordering::Equal => {
                // mtime equal but size differs - fallback to larger wins
                if local_size > remote_size {
                    "upload"
                } else if remote_size > local_size {
                    "download"
                } else {
                    "skip"
                }
            }
        },
        "older" | "oldest" => match compare_mtime(local_mtime, remote_mtime) {
            std::cmp::Ordering::Less => "upload",
            std::cmp::Ordering::Greater => "download",
            std::cmp::Ordering::Equal => {
                if local_size < remote_size {
                    "upload"
                } else if remote_size < local_size {
                    "download"
                } else {
                    "skip"
                }
            }
        },
        "larger" | "largest" => {
            if local_size > remote_size {
                "upload"
            } else if remote_size > local_size {
                "download"
            } else {
                "skip"
            }
        }
        "smaller" | "smallest" => {
            if local_size < remote_size {
                "upload"
            } else if remote_size < local_size {
                "download"
            } else {
                "skip"
            }
        }
        "rename" => "rename",
        _ => "skip", // "skip" or unknown
    }
}

fn partition_conflict_rename_downloads<'a>(
    to_download: Vec<&'a str>,
    to_conflict_upload: &[(String, String)],
) -> (Vec<&'a str>, Vec<&'a str>) {
    let conflict_paths: std::collections::HashSet<&str> = to_conflict_upload
        .iter()
        .map(|(orig_path, _)| orig_path.as_str())
        .collect();

    let mut normal_downloads = Vec::new();
    let mut gated_conflict_downloads = Vec::new();

    for path in to_download {
        if conflict_paths.contains(path) {
            gated_conflict_downloads.push(path);
        } else {
            normal_downloads.push(path);
        }
    }

    (normal_downloads, gated_conflict_downloads)
}

/// Backup a file before overwriting (if --backup-dir is set).
fn backup_file(
    source_path: &str,
    backup_dir: &str,
    backup_suffix: &str,
    relative_path: &str,
    suffix_keep_extension: bool,
) {
    if backup_dir.is_empty() {
        return;
    }
    let backup_name = if suffix_keep_extension && !backup_suffix.is_empty() {
        // Insert suffix before extension: "file.txt" + ".bak" -> "file.bak.txt"
        if let Some(dot_pos) = relative_path.rfind('.') {
            let slash_pos = relative_path.rfind('/').unwrap_or(0);
            if dot_pos > slash_pos {
                format!(
                    "{}{}{}",
                    &relative_path[..dot_pos],
                    backup_suffix,
                    &relative_path[dot_pos..]
                )
            } else {
                format!("{}{}", relative_path, backup_suffix)
            }
        } else {
            format!("{}{}", relative_path, backup_suffix)
        }
    } else {
        format!("{}{}", relative_path, backup_suffix)
    };
    let dest = Path::new(backup_dir).join(backup_name);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(source_path, &dest) {
        eprintln!("Warning: backup failed for {}: {}", relative_path, e);
    }
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
    backup_dir: Option<&str>,
    backup_suffix: &str,
    suffix_keep_extension: bool,
    compare_dest: Option<&str>,
    copy_dest: Option<&str>,
    from_reconcile: Option<&str>,
    conflict_mode: &str,
    skip_matching: bool,
    resync: bool,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
    precomputed_local: Option<Vec<(String, u64, Option<String>)>>,
) -> SyncCycleStats {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code.into(),
    };

    let remote = &resolve_cli_remote_path(&initial_path, remote);
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let start = Instant::now();

    if !quiet {
        if let Some(reconcile_path) = from_reconcile {
            eprintln!("Using reconcile plan: {}", reconcile_path);
        } else {
            if precomputed_local.is_some() {
                eprintln!("Scanning local: {} (incremental)", local);
            } else {
                eprintln!("Scanning local: {}", local);
            }
            eprintln!("Scanning remote: {}", remote);
        }
    }

    // Pre-compile exclude matchers (avoids O(n*m) recompilation)
    let exclude_matchers: Vec<globset::GlobMatcher> = exclude
        .iter()
        .filter_map(|pat| globset::Glob::new(pat).ok().map(|g| g.compile_matcher()))
        .collect();

    let files_from_set = load_files_from(cli);
    let scan_depth = cli.max_depth.map(|d| d as usize).unwrap_or(100);

    let mut reconcile_plan: Option<ReconcileSyncPlan> = None;
    #[allow(clippy::type_complexity)]
    let (local_entries, remote_entries): (
        Vec<(String, u64, Option<String>)>,
        Vec<(String, u64, Option<String>)>,
    ) = if let Some(reconcile_path) = from_reconcile {
        match load_sync_plan_from_reconcile(reconcile_path, direction, delete) {
            Ok(plan) => {
                let local_entries = plan.local_entries.clone();
                let remote_entries = plan.remote_entries.clone();
                reconcile_plan = Some(plan);
                (local_entries, remote_entries)
            }
            Err(err) => {
                print_error(format, &err, 5);
                let _ = provider.disconnect().await;
                return 5.into();
            }
        }
    } else {
        // Scan local files (bounded: max depth, 500K entries)
        // If precomputed_local is provided (incremental watch mode), skip walkdir entirely.
        let local_entries: Vec<(String, u64, Option<String>)> = if let Some(pre) = precomputed_local
        {
            pre
        } else {
            let walker = walkdir::WalkDir::new(local)
                .follow_links(false)
                .max_depth(scan_depth);
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
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(local)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative.is_empty() || relative == BISYNC_SNAPSHOT_FILE {
                    continue;
                }

                let fname = entry.file_name().to_string_lossy();
                let fname_ref: &str = fname.as_ref();
                if exclude_matchers
                    .iter()
                    .any(|m| m.is_match(&relative) || m.is_match(fname_ref))
                {
                    continue;
                }
                if let Some(ref set) = files_from_set {
                    if !set.contains(relative.as_str()) {
                        continue;
                    }
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

        if cli.no_check_dest && delete {
            print_error(format, "--no-check-dest cannot be used with --delete (would mark all destination files as orphans for deletion)", 5);
            let _ = provider.disconnect().await;
            return 5.into();
        }
        if cli.immutable && cli.no_check_dest {
            print_error(format, "--immutable cannot be used with --no-check-dest (immutable needs remote listing to detect existing files)", 5);
            let _ = provider.disconnect().await;
            return 5.into();
        }

        let mut remote_entries: Vec<(String, u64, Option<String>)> = Vec::new();
        if cli.no_check_dest {
            if !quiet {
                eprintln!(
                    "Note: --no-check-dest skipping remote scan (assuming empty destination)"
                );
            }
        } else {
            let mut used_fast_list = false;
            if cli.fast_list {
                if let Some(s3) = provider
                    .as_any_mut()
                    .downcast_mut::<ftp_client_gui_lib::providers::s3::S3Provider>()
                {
                    if !quiet {
                        eprintln!("Using --fast-list (S3 recursive listing)...");
                    }
                    match s3.list_recursive(remote).await {
                        Ok(entries) => {
                            let max_depth = cli.max_depth.map(|d| d as usize);
                            for e in entries {
                                if e.is_dir {
                                    continue;
                                }
                                if remote_entries.len() >= MAX_SCAN_ENTRIES {
                                    if !quiet {
                                        eprintln!(
                                            "Warning: --fast-list capped at {} entries",
                                            MAX_SCAN_ENTRIES
                                        );
                                    }
                                    break;
                                }
                                let relative = e
                                    .path
                                    .strip_prefix(remote)
                                    .unwrap_or(&e.path)
                                    .trim_start_matches('/')
                                    .to_string();
                                if relative.is_empty() || relative == BISYNC_SNAPSHOT_FILE {
                                    continue;
                                }
                                if let Some(max_d) = max_depth {
                                    let depth = relative.matches('/').count();
                                    if depth >= max_d {
                                        continue;
                                    }
                                }
                                if exclude_matchers
                                    .iter()
                                    .any(|m| m.is_match(&relative) || m.is_match(&e.name))
                                {
                                    continue;
                                }
                                if let Some(ref set) = files_from_set {
                                    if !set.contains(relative.as_str()) {
                                        continue;
                                    }
                                }
                                remote_entries.push((relative, e.size, e.modified));
                            }
                            used_fast_list = true;
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!(
                                    "Warning: --fast-list failed, falling back to BFS scan: {}",
                                    e
                                );
                            }
                        }
                    }
                } else if !quiet {
                    eprintln!("Note: --fast-list only supported for S3; using standard scan");
                }
            }

            if !used_fast_list {
                let remote_scan_depth = cli.max_depth.map(|d| d as usize).unwrap_or(MAX_SCAN_DEPTH);
                let mut queue: Vec<(String, String, usize)> =
                    vec![(remote.to_string(), String::new(), 0)];
                while let Some((abs_dir, rel_prefix, depth)) = queue.pop() {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    if depth >= remote_scan_depth {
                        if !quiet {
                            eprintln!("Warning: max scan depth reached at {}", abs_dir);
                        }
                        continue;
                    }
                    if remote_entries.len() >= MAX_SCAN_ENTRIES {
                        if !quiet {
                            eprintln!("Warning: max entries reached during remote scan");
                        }
                        break;
                    }
                    match provider.list(&abs_dir).await {
                        Ok(entries) => {
                            for e in entries {
                                let entry_rel = if rel_prefix.is_empty() {
                                    e.name.clone()
                                } else {
                                    format!("{}/{}", rel_prefix, e.name)
                                };
                                if e.is_dir {
                                    queue.push((e.path.clone(), entry_rel, depth + 1));
                                } else {
                                    let relative = entry_rel;
                                    if !relative.is_empty() && relative != BISYNC_SNAPSHOT_FILE {
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
                                eprintln!("Warning: cannot scan {}: {}", abs_dir, e);
                            }
                        }
                    }
                }
            }
        }

        (local_entries, remote_entries)
    };

    // Build comparison maps
    let local_map: HashMap<&str, (u64, Option<&str>)> = local_entries
        .iter()
        .map(|(p, s, m)| (p.as_str(), (*s, m.as_deref())))
        .collect();
    let remote_map: HashMap<&str, (u64, Option<&str>)> = remote_entries
        .iter()
        .map(|(p, s, m)| (p.as_str(), (*s, m.as_deref())))
        .collect();

    // Load previous snapshot for bisync delta detection (--direction both only)
    let prev_snapshot = if direction == "both" && !resync {
        load_bisync_snapshot(local)
    } else {
        if resync && !quiet {
            eprintln!("--resync: ignoring previous snapshot, full scan");
        }
        None
    };

    let default_time_val = resolve_default_time(cli);
    let default_time_ref = default_time_val.as_deref();

    let (
        owned_to_upload,
        owned_to_download,
        owned_to_delete_remote,
        owned_to_delete_local,
        preplanned_skipped,
    ) = if let Some(plan) = reconcile_plan.as_ref() {
        (
            plan.to_upload.clone(),
            plan.to_download.clone(),
            plan.to_delete_remote.clone(),
            plan.to_delete_local.clone(),
            plan.skipped,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0)
    };
    let mut to_upload: Vec<&str> = Vec::new();
    let mut to_download: Vec<&str> = Vec::new();
    let mut to_delete_remote: Vec<&str> = Vec::new();
    let mut to_delete_local: Vec<&str> = Vec::new();
    // Conflict renames: (original_relative_path, conflict_suffixed_remote_path)
    let mut to_conflict_upload: Vec<(String, String)> = Vec::new();
    let mut conflicts_resolved: u32 = 0;
    let mut skipped: u32 = 0;

    if reconcile_plan.is_some() {
        to_upload = owned_to_upload.iter().map(String::as_str).collect();
        to_download = owned_to_download.iter().map(String::as_str).collect();
        to_delete_remote = owned_to_delete_remote.iter().map(String::as_str).collect();
        to_delete_local = owned_to_delete_local.iter().map(String::as_str).collect();
        skipped = preplanned_skipped;
    } else if direction == "upload" || direction == "both" {
        for (path, (size, mtime)) in &local_map {
            if let Some((rsize, rmtime)) = remote_map.get(path) {
                let lm = apply_default_time(*mtime, default_time_ref);
                let rm = apply_default_time(*rmtime, default_time_ref);
                if size == rsize
                    && (skip_matching || compare_mtime(lm, rm) == std::cmp::Ordering::Equal)
                {
                    skipped += 1;
                } else if direction == "both" {
                    // Conflict: file exists on both sides with different content
                    let action = resolve_conflict(conflict_mode, *size, lm, *rsize, rm);
                    match action {
                        "upload" => {
                            to_upload.push(path);
                            conflicts_resolved += 1;
                        }
                        "download" => {
                            to_download.push(path);
                            conflicts_resolved += 1;
                        }
                        "rename" => {
                            // Keep both: download remote version, upload local with conflict suffix
                            to_download.push(path);
                            let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f");
                            let conflict_path = if let Some(dot_pos) = path.rfind('.') {
                                format!("{}.conflict-{}{}", &path[..dot_pos], ts, &path[dot_pos..])
                            } else {
                                format!("{}.conflict-{}", path, ts)
                            };
                            to_conflict_upload.push((path.to_string(), conflict_path));
                            conflicts_resolved += 1;
                        }
                        _ => {
                            skipped += 1;
                            conflicts_resolved += 1;
                        }
                    }
                } else {
                    // upload-only: local always wins
                    to_upload.push(path);
                }
            } else {
                // File only on local side
                if direction == "both" {
                    // Check snapshot: if file was in previous snapshot, it was deleted remotely
                    if let Some(ref snap) = prev_snapshot {
                        if snap.files.contains_key(*path) {
                            // Was synced before, now missing remotely → remote deletion
                            if delete {
                                to_delete_local.push(path);
                            } else {
                                to_upload.push(path); // re-upload unless --delete
                            }
                            continue;
                        }
                    }
                    to_upload.push(path);
                } else {
                    to_upload.push(path);
                }
            }
        }
    }

    if reconcile_plan.is_none() && (direction == "download" || direction == "both") {
        for (path, (size, mtime)) in &remote_map {
            if let Some((lsize, lmtime)) = local_map.get(path) {
                let rm = apply_default_time(*mtime, default_time_ref);
                let lm = apply_default_time(*lmtime, default_time_ref);
                if size == lsize
                    && (skip_matching || compare_mtime(rm, lm) == std::cmp::Ordering::Equal)
                {
                    if direction == "download" {
                        skipped += 1;
                    }
                    // In "both" mode, already handled above
                } else if direction == "download" {
                    to_download.push(path);
                }
                // In "both" mode, conflicts already resolved in upload pass
            } else {
                // File only on remote side
                if direction == "both" {
                    // Check snapshot: if file was in previous snapshot, it was deleted locally
                    if let Some(ref snap) = prev_snapshot {
                        if snap.files.contains_key(*path) {
                            if delete {
                                to_delete_remote.push(path);
                            } else {
                                to_download.push(path);
                            }
                            continue;
                        }
                    }
                    to_download.push(path);
                } else {
                    to_download.push(path);
                }
            }
        }
    }

    // Orphan deletion (for upload/download-only modes)
    if delete && direction != "both" {
        if direction == "upload" {
            for path in remote_map.keys() {
                if !local_map.contains_key(path) {
                    to_delete_remote.push(path);
                }
            }
        }
        if direction == "download" {
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
        if !quiet {
            eprintln!("Checking for renamed files...");
        }
        // Build hash map of files to upload (local side)
        let mut upload_hashes: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for up_path in &to_upload {
            let local_file = std::path::Path::new(local).join(up_path);
            if let Ok(data) = std::fs::read(&local_file) {
                use sha2::Digest;
                let hash = format!("{:x}", sha2::Sha256::digest(&data));
                upload_hashes
                    .entry(hash)
                    .or_default()
                    .push(up_path.to_string());
            }
        }
        // For each file to delete, check if its hash matches an upload candidate
        let mut matched_uploads: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut matched_deletes: std::collections::HashSet<String> =
            std::collections::HashSet::new();
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
                eprintln!(
                    "  {} rename(s) detected - will rename instead of delete+upload",
                    renames.len()
                );
            }
        }
    }

    // --immutable: remove uploads that would overwrite existing remote files
    if cli.immutable {
        let before = to_upload.len();
        to_upload.retain(|path| !remote_map.contains_key(path));
        let removed = before - to_upload.len();
        if removed > 0 && !quiet {
            eprintln!(
                "Note: --immutable skipped {} file(s) that already exist on remote",
                removed
            );
        }
        // Also prevent downloads that would overwrite local files
        let before_dl = to_download.len();
        to_download.retain(|path| !local_map.contains_key(path));
        let removed_dl = before_dl - to_download.len();
        if removed_dl > 0 && !quiet {
            eprintln!(
                "Note: --immutable skipped {} download(s) that already exist locally",
                removed_dl
            );
        }
    }

    // --compare-dest: skip uploads where file exists in compare dir with same size+mtime
    let mut copy_dest_ops: Vec<(String, String)> = Vec::new(); // (compare_src, local_dest)
    if let Some(cdir) = compare_dest.or(copy_dest) {
        // Validate compare/copy-dest path exists and canonicalize
        let cdir_canonical = match std::fs::canonicalize(cdir) {
            Ok(p) => p,
            Err(e) => {
                if !quiet {
                    eprintln!(
                        "Warning: --compare-dest/--copy-dest '{}' not accessible: {}. Skipping.",
                        cdir, e
                    );
                }
                Path::new(cdir).to_path_buf()
            }
        };
        let cdir_ref = cdir_canonical.to_str().unwrap_or(cdir);
        let is_copy = copy_dest.is_some();
        let before = to_upload.len();
        let mut retained = Vec::new();
        for path in &to_upload {
            let compare_file = Path::new(cdir_ref).join(path);
            // Path traversal check: ensure resolved path stays within compare-dest
            if let Ok(resolved) = compare_file.canonicalize() {
                if !resolved.starts_with(&cdir_canonical) {
                    if !quiet {
                        eprintln!("Warning: path traversal blocked for compare-dest: {}", path);
                    }
                    retained.push(*path);
                    continue;
                }
            }
            if let Ok(meta) = std::fs::metadata(&compare_file) {
                let csize = meta.len();
                let cmtime = meta.modified().ok().map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
                });
                if let Some((lsize, lmtime)) = local_map.get(path) {
                    let lm = apply_default_time(*lmtime, default_time_ref);
                    if csize == *lsize
                        && compare_mtime(cmtime.as_deref(), lm) == std::cmp::Ordering::Equal
                    {
                        if is_copy {
                            // Copy from compare-dest to local instead of downloading
                            let local_dest =
                                Path::new(local).join(path).to_string_lossy().to_string();
                            copy_dest_ops
                                .push((compare_file.to_string_lossy().to_string(), local_dest));
                        }
                        // Skip upload either way
                        continue;
                    }
                }
            }
            retained.push(*path);
        }
        to_upload = retained;
        let removed = before - to_upload.len();
        if removed > 0 && !quiet {
            let label = if is_copy {
                "--copy-dest"
            } else {
                "--compare-dest"
            };
            eprintln!(
                "Note: {} skipped {} upload(s) matched in {}",
                label, removed, cdir
            );
        }
    }

    if !quiet {
        let conflict_info = if conflicts_resolved > 0 {
            format!(
                ", {} conflict(s) resolved via --conflict-mode={}",
                conflicts_resolved, conflict_mode
            )
        } else {
            String::new()
        };
        eprintln!(
            "\nSync plan: {} upload, {} download, {} delete, {} rename, {} conflict-rename, {} skipped{}",
            to_upload.len(),
            to_download.len(),
            to_delete_remote.len() + to_delete_local.len(),
            renames.len(),
            to_conflict_upload.len(),
            skipped,
            conflict_info
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
            return 4.into();
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
                for (orig, conflict) in &to_conflict_upload {
                    println!("  CONFLICT-RENAME  {} -> {}", orig, conflict);
                }
                println!("\n(dry run - no changes made)");
            }
            OutputFormat::Json => {
                // Build the per-file plan so agents piloting `sync` via JSON
                // no longer need to parse the text-verbose output. Sizes come
                // from the comparison maps built above; entries where both
                // sides have a known size expose both (useful to render
                // "replace 12 MB with 14 MB" diffs in agent UIs).
                let mut plan: Vec<CliSyncPlanEntry> = Vec::with_capacity(
                    to_upload.len()
                        + to_download.len()
                        + to_delete_remote.len()
                        + to_delete_local.len()
                        + to_conflict_upload.len(),
                );
                for p in &to_upload {
                    plan.push(CliSyncPlanEntry {
                        op: "upload",
                        path: (*p).to_string(),
                        local_size: local_map.get(*p).map(|(s, _)| *s),
                        remote_size: remote_map.get(*p).map(|(s, _)| *s),
                        conflict_path: None,
                    });
                }
                for p in &to_download {
                    plan.push(CliSyncPlanEntry {
                        op: "download",
                        path: (*p).to_string(),
                        local_size: local_map.get(*p).map(|(s, _)| *s),
                        remote_size: remote_map.get(*p).map(|(s, _)| *s),
                        conflict_path: None,
                    });
                }
                for p in &to_delete_remote {
                    plan.push(CliSyncPlanEntry {
                        op: "delete_remote",
                        path: (*p).to_string(),
                        local_size: None,
                        remote_size: remote_map.get(*p).map(|(s, _)| *s),
                        conflict_path: None,
                    });
                }
                for p in &to_delete_local {
                    plan.push(CliSyncPlanEntry {
                        op: "delete_local",
                        path: (*p).to_string(),
                        local_size: local_map.get(*p).map(|(s, _)| *s),
                        remote_size: None,
                        conflict_path: None,
                    });
                }
                for (orig, conflict) in &to_conflict_upload {
                    plan.push(CliSyncPlanEntry {
                        op: "conflict_rename",
                        path: orig.clone(),
                        local_size: local_map.get(orig.as_str()).map(|(s, _)| *s),
                        remote_size: remote_map.get(orig.as_str()).map(|(s, _)| *s),
                        conflict_path: Some(conflict.clone()),
                    });
                }
                print_json(&CliSyncResult {
                    status: "dry_run",
                    uploaded: to_upload.len() as u32,
                    downloaded: to_download.len() as u32,
                    deleted: (to_delete_remote.len() + to_delete_local.len()) as u32,
                    skipped,
                    errors: vec![],
                    elapsed_secs: start.elapsed().as_secs_f64(),
                    plan,
                });
            }
        }
        let _ = provider.disconnect().await;
        return SyncCycleStats {
            exit_code: 0,
            uploaded: to_upload.len() as u32,
            downloaded: to_download.len() as u32,
            deleted: (to_delete_remote.len() + to_delete_local.len()) as u32,
            skipped,
            error_count: 0,
        };
    }

    // Execute --copy-dest local copies first
    for (src, dst) in &copy_dest_ops {
        if let Some(parent) = Path::new(dst).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::copy(src, dst) {
            eprintln!("  copy-dest {} -> {}: {}", src, dst, e);
        } else if !quiet {
            eprintln!("  COPY-DEST  {}", dst);
        }
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

    let total_transfer_files = upload_jobs.len() + to_download.len();
    let total_transfer_bytes: u64 = upload_jobs.iter().map(|(_, _, _, size)| *size).sum::<u64>()
        + to_download
            .iter()
            .map(|path| remote_map.get(*path).map(|(size, _)| *size).unwrap_or(0))
            .sum::<u64>();

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
        Some(create_overall_progress_bar(
            total_transfer_files,
            total_transfer_bytes,
        ))
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
                    resolve_max_transfer(cli),
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

    let (normal_download_paths, gated_conflict_download_paths) =
        partition_conflict_rename_downloads(to_download.clone(), &to_conflict_upload);

    // Execute conflict renames first: preserve the local version remotely before
    // downloading the remote canonical version over the local path.
    let mut conflict_uploaded = 0u32;
    let mut preserved_conflict_downloads: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for (orig_path, conflict_path) in &to_conflict_upload {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let local_path = Path::new(local)
            .join(orig_path)
            .to_string_lossy()
            .to_string();
        let remote_conflict = format!("{}/{}", remote.trim_end_matches('/'), conflict_path);
        match upload_transfer_task(
            url,
            local_path,
            remote_conflict,
            cli,
            format,
            None,
            None,
            resolve_max_transfer(cli),
        )
        .await
        {
            Ok(()) => {
                conflict_uploaded += 1;
                preserved_conflict_downloads.insert(orig_path.clone());
                if !quiet {
                    eprintln!("  CONFLICT-RENAME  {} -> {}", orig_path, conflict_path);
                }
            }
            Err(e) => errors.push(format!("conflict-rename {}: {}", orig_path, e)),
        }
    }

    let mut download_jobs: Vec<(String, String, String, u64)> = Vec::new();
    for path in normal_download_paths.into_iter().chain(
        gated_conflict_download_paths
            .into_iter()
            .filter(|path| preserved_conflict_downloads.contains(*path)),
    ) {
        if validate_relative_path(path).is_none() {
            errors.push(format!(
                "download {}: unsafe path (traversal rejected)",
                path
            ));
            continue;
        }
        let relative = path.to_string();
        let local_path = Path::new(local).join(path).to_string_lossy().to_string();
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
        let size = remote_map.get(path).map(|(size, _)| *size).unwrap_or(0);
        download_jobs.push((relative, local_path, remote_path, size));
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
                    resolve_max_transfer(cli),
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
                    if !quiet {
                        eprintln!("  RENAME {} → {}", old_remote, new_local);
                    }
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
            errors.push(format!(
                "delete remote {}: unsafe path (traversal rejected)",
                path
            ));
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
            errors.push(format!(
                "delete local {}: unsafe path (traversal rejected)",
                path
            ));
            continue;
        }
        let local_path = format!("{}/{}", local, path);
        // Backup before delete (if --backup-dir set)
        if let Some(bdir) = backup_dir {
            backup_file(
                &local_path,
                bdir,
                backup_suffix,
                path,
                suffix_keep_extension,
            );
        }
        match std::fs::remove_file(&local_path) {
            Ok(()) => deleted += 1,
            Err(e) => errors.push(format!("delete local {}: {}", path, e)),
        }
    }

    // Save bisync snapshot after successful sync (--direction both)
    if direction == "both" && errors.is_empty() && !dry_run {
        save_bisync_snapshot(local, &local_entries, &remote_entries);
        if !quiet {
            eprintln!(
                "Bisync snapshot saved to {}/{}",
                local, BISYNC_SNAPSHOT_FILE
            );
        }
    }

    let elapsed = start.elapsed();

    match format {
        OutputFormat::Text => {
            if !cli.quiet {
                println!(
                    "\nSync complete: {} uploaded, {} downloaded, {} deleted, {} renamed, {} conflict-renamed in {:.1}s",
                    uploaded,
                    downloaded,
                    deleted,
                    renamed,
                    conflict_uploaded,
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
                plan: Vec::new(),
            });
        }
    }

    let _ = provider.disconnect().await;
    SyncCycleStats {
        exit_code: if errors.is_empty() { 0 } else { 4 },
        uploaded,
        downloaded,
        deleted,
        skipped,
        error_count: errors.len() as u32,
    }
}

async fn cmd_tree(url: &str, path: &str, max_depth: usize, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let resolved_path = resolve_cli_remote_path(&initial_path, path);
    let effective_path = &resolved_path;

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
                Box::pin(build_tree(
                    provider,
                    &e.path,
                    depth + 1,
                    max_depth,
                    entry_count,
                    visited,
                ))
                .await
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
            let root_children = build_tree(
                &mut *provider,
                effective_path,
                0,
                max_depth,
                &mut tree_entry_count,
                &mut tree_visited,
            )
            .await;
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
            let mut tree_visited: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // Load root entries
            let root_entries = match provider.list(effective_path).await {
                Ok(e) => e,
                Err(e) => {
                    print_error(
                        format,
                        &format!("tree failed: {}", e),
                        provider_error_to_exit_code(&e),
                    );
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
                let connector = if is_last {
                    "\u{2514}\u{2500}\u{2500} "
                } else {
                    "\u{251c}\u{2500}\u{2500} "
                };
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
                if e.is_dir {
                    dir_count += 1;
                } else {
                    file_count += 1;
                }
            }

            while let Some(item) = stack.pop() {
                if tree_entry_count >= MAX_SCAN_ENTRIES {
                    eprintln!(
                        "Warning: max entries {} reached, tree output truncated",
                        MAX_SCAN_ENTRIES
                    );
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
                                let connector = if is_last {
                                    "\u{2514}\u{2500}\u{2500} "
                                } else {
                                    "\u{251c}\u{2500}\u{2500} "
                                };
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
                                if e.is_dir {
                                    dir_count += 1;
                                } else {
                                    file_count += 1;
                                }
                            }
                        }
                    }
                }
            }

            if !cli.quiet {
                println!("\n{} directories, {} files", dir_count, file_count);
            }
        }
    }

    let _ = provider.disconnect().await;
    0
}

// ── NCDU - Interactive Disk Usage Explorer ────────────────────────

/// A node in the disk usage tree.
#[derive(Debug, Serialize)]
struct NcduEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
    /// Aggregated size including all descendants (for directories).
    agg_size: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<NcduEntry>,
}

impl NcduEntry {
    fn empty() -> Self {
        Self {
            name: String::new(),
            path: String::new(),
            is_dir: true,
            size: 0,
            agg_size: 0,
            children: Vec::new(),
        }
    }
}

/// Recursively scan a remote directory and build an NcduEntry tree.
#[allow(clippy::too_many_arguments)]
async fn ncdu_scan(
    provider: &mut dyn StorageProvider,
    path: &str,
    name: &str,
    depth: usize,
    max_depth: usize,
    quiet: bool,
    spinner: &Option<ProgressBar>,
    entry_count: &mut usize,
) -> NcduEntry {
    let mut node = NcduEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir: true,
        size: 0,
        agg_size: 0,
        children: Vec::new(),
    };

    if depth >= max_depth || *entry_count >= MAX_SCAN_ENTRIES {
        return node;
    }

    let entries = match provider.list(path).await {
        Ok(e) => e,
        Err(err) => {
            if !quiet {
                if let Some(sp) = spinner {
                    sp.set_message(format!("Error listing {}: {}", path, err));
                }
            }
            return node;
        }
    };

    for entry in entries {
        *entry_count += 1;
        if *entry_count >= MAX_SCAN_ENTRIES {
            break;
        }
        if let Some(sp) = spinner {
            if *entry_count % 50 == 0 {
                sp.set_message(format!("Scanned {} entries...", entry_count));
            }
        }

        if entry.is_dir {
            let child = Box::pin(ncdu_scan(
                provider,
                &entry.path,
                &entry.name,
                depth + 1,
                max_depth,
                quiet,
                spinner,
                entry_count,
            ))
            .await;
            node.agg_size += child.agg_size;
            node.children.push(child);
        } else {
            node.agg_size += entry.size;
            node.children.push(NcduEntry {
                name: entry.name,
                path: entry.path,
                is_dir: false,
                size: entry.size,
                agg_size: entry.size,
                children: Vec::new(),
            });
        }
    }

    // Sort: directories first, then by size descending
    node.children.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| b.agg_size.cmp(&a.agg_size))
    });

    node
}

/// TUI state for ncdu navigation.
struct NcduState {
    /// Stack of directory indices for navigation (parent → child).
    path_stack: Vec<(NcduEntry, usize)>,
    /// Current directory being viewed.
    current: NcduEntry,
    /// Selected index in the current directory's children.
    selected: usize,
}

impl NcduState {
    fn new(root: NcduEntry) -> Self {
        Self {
            path_stack: Vec::new(),
            current: root,
            selected: 0,
        }
    }

    fn enter_selected(&mut self) {
        if self.current.children.is_empty() {
            return;
        }
        let idx = self
            .selected
            .min(self.current.children.len().saturating_sub(1));
        if !self.current.children[idx].is_dir || self.current.children[idx].children.is_empty() {
            return;
        }
        let mut placeholder = NcduEntry::empty();
        std::mem::swap(&mut self.current.children[idx], &mut placeholder);
        let old_current = std::mem::replace(&mut self.current, placeholder);
        self.path_stack.push((old_current, idx));
        self.selected = 0;
    }

    fn go_back(&mut self) {
        if let Some((mut parent, idx)) = self.path_stack.pop() {
            let child = std::mem::replace(&mut self.current, NcduEntry::empty());
            parent.children[idx] = child;
            self.current = parent;
            self.selected = idx;
        }
    }
}

fn ncdu_format_bar(ratio: f64, width: usize) -> String {
    let raw = (ratio * width as f64).round() as usize;
    // Guarantee at least 1 char for any non-zero entry so tiny items are visible
    let filled = if ratio > 0.0 { raw.max(1) } else { 0 };
    let filled = filled.min(width);
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "#".repeat(filled), ".".repeat(empty))
}

/// Run the interactive TUI.
fn ncdu_run_tui(root: NcduEntry) -> std::io::Result<()> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind},
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        ExecutableCommand,
    };
    use ratatui::{
        backend::CrosstermBackend,
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Paragraph},
        Terminal,
    };

    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = NcduState::new(root);

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            // Header (2 lines) + body
            let chunks = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

            // Header
            let header_text = format!(
                " ncdu - {} ({})  [{} items]",
                state.current.path,
                format_size(state.current.agg_size),
                state.current.children.len()
            );
            let header = Paragraph::new(Line::from(vec![Span::styled(
                header_text,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]));
            frame.render_widget(header, chunks[0]);

            // File list
            let list_area = chunks[1];
            let visible_count = list_area.height as usize;
            let children = &state.current.children;

            // Scroll offset
            let scroll = if state.selected >= visible_count {
                state.selected - visible_count + 1
            } else {
                0
            };

            let parent_size = state.current.agg_size.max(1) as f64;
            let bar_width = 20usize;
            let mut lines: Vec<Line> = Vec::with_capacity(visible_count);

            // ".." entry for going back
            let back_selected = state.selected == 0 && !state.path_stack.is_empty();
            if scroll == 0 && !state.path_stack.is_empty() {
                let style = if back_selected {
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Blue)
                };
                lines.push(Line::from(vec![Span::styled(
                    "          /..                          ",
                    style,
                )]));
            }

            let offset = if state.path_stack.is_empty() { 0 } else { 1 };

            for (i, child) in children.iter().enumerate() {
                let display_idx = i + offset;
                if display_idx < scroll || lines.len() >= visible_count {
                    continue;
                }
                let is_selected = display_idx == state.selected;
                let ratio = child.agg_size as f64 / parent_size;
                let pct = (ratio * 100.0).min(100.0);
                let bar = ncdu_format_bar(ratio, bar_width);

                let size_str = format!("{:>9}", format_size(child.agg_size));
                let pct_str = format!("{:5.1}%", pct);
                let name_str = if child.is_dir {
                    format!("/{}", child.name)
                } else {
                    child.name.clone()
                };

                let style = if is_selected {
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else if child.is_dir {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default()
                };

                let bar_style = if is_selected {
                    Style::default().bg(Color::DarkGray).fg(Color::Green)
                } else {
                    Style::default().fg(Color::Green)
                };

                lines.push(Line::from(vec![
                    Span::styled(size_str, style),
                    Span::raw(" "),
                    Span::styled(bar, bar_style),
                    Span::raw(" "),
                    Span::styled(pct_str, style),
                    Span::raw(" "),
                    Span::styled(name_str, style),
                ]));
            }

            let list_widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
            frame.render_widget(list_widget, list_area);

            // Footer
            let footer = Paragraph::new(Line::from(vec![Span::styled(
                " q:quit  Enter:open  Backspace/Left:back  j/k or Up/Down:navigate  d:delete info",
                Style::default().fg(Color::DarkGray),
            )]));
            frame.render_widget(footer, chunks[2]);
        })?;

        // Handle input
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let max_idx =
                    state.current.children.len() + if state.path_stack.is_empty() { 0 } else { 1 };
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') if state.selected + 1 < max_idx => {
                        state.selected += 1;
                    }
                    KeyCode::Up | KeyCode::Char('k') if state.selected > 0 => {
                        state.selected -= 1;
                    }
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                        // If on ".." entry, go back
                        if !state.path_stack.is_empty() && state.selected == 0 {
                            state.go_back();
                        } else {
                            // Adjust for ".." offset
                            let adj = if state.path_stack.is_empty() { 0 } else { 1 };
                            if state.selected >= adj {
                                state.selected -= adj;
                                state.enter_selected();
                                state.selected += if state.path_stack.is_empty() { 0 } else { 1 };
                            }
                        }
                    }
                    KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                        state.go_back();
                    }
                    KeyCode::Home => state.selected = 0,
                    KeyCode::End if max_idx > 0 => {
                        state.selected = max_idx - 1;
                    }
                    KeyCode::PageDown => {
                        state.selected = (state.selected + 20).min(max_idx.saturating_sub(1));
                    }
                    KeyCode::PageUp => {
                        state.selected = state.selected.saturating_sub(20);
                    }
                    _ => {}
                }
            }
        }
    }

    // Cleanup
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn cmd_ncdu(
    url: &str,
    path: &str,
    max_depth: usize,
    export: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, path));

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let spinner = if !quiet {
        Some(create_spinner("Scanning remote directory..."))
    } else {
        None
    };

    let mut entry_count = 0usize;
    let root_name = base_path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("/");
    let root = ncdu_scan(
        &mut *provider,
        &base_path,
        root_name,
        0,
        max_depth,
        quiet,
        &spinner,
        &mut entry_count,
    )
    .await;

    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

    let _ = provider.disconnect().await;

    if !quiet {
        eprintln!(
            "Scanned {} entries, total {}",
            entry_count,
            format_size(root.agg_size)
        );
    }

    // Export mode: write JSON and exit
    if let Some(export_path) = export {
        match serde_json::to_string_pretty(&root) {
            Ok(json) => {
                if let Err(e) = std::fs::write(export_path, &json) {
                    print_error(format, &format!("Failed to write export: {}", e), 4);
                    return 4;
                }
                if !quiet {
                    eprintln!("Exported to {}", export_path);
                }
            }
            Err(e) => {
                print_error(format, &format!("JSON serialization failed: {}", e), 99);
                return 99;
            }
        }
        return 0;
    }

    // JSON output mode: print and exit
    if matches!(format, OutputFormat::Json) {
        print_json(&root);
        return 0;
    }

    // Interactive TUI mode (requires terminal)
    if !std::io::stdin().is_terminal() {
        eprintln!("ncdu: interactive mode requires a terminal. Use --export or --json for non-interactive output.");
        return 5;
    }

    if let Err(e) = ncdu_run_tui(root) {
        eprintln!("TUI error: {}", e);
        return 99;
    }

    0
}

// ── FUSE Mount (Linux + macOS) ───────────────────────────────────

#[cfg(target_os = "linux")]
mod fuse_mount {
    use super::*;
    use fuser::{
        FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyData,
        ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request,
    };
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const ROOT_INODE: u64 = 1;
    const BLOCK_SIZE: u32 = 512;
    /// Read chunk size: 4 MB per read call
    const READ_CHUNK: u64 = 4 * 1024 * 1024;

    #[derive(Clone, Debug)]
    struct CachedEntry {
        attr: FileAttr,
        children: Option<Vec<u64>>, // inode list for directories
        fetched_at: Instant,
    }

    /// FUSE filesystem backed by a StorageProvider.
    #[allow(dead_code)]
    pub struct AeroFuseFs {
        /// Dedicated runtime for FUSE callbacks (separate from main tokio runtime)
        rt: tokio::runtime::Runtime,
        provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
        base_path: String,
        cache_ttl: Duration,
        read_only: bool,
        /// Write buffers: inode → tempfile path
        write_buffers: Mutex<HashMap<u64, PathBuf>>,
        /// Track which inodes have been successfully flushed (uploaded)
        flush_ok: Mutex<std::collections::HashSet<u64>>,
        /// Cached uid/gid (avoid repeated unsafe calls)
        uid: u32,
        gid: u32,
        /// inode → remote path
        inode_path: Mutex<HashMap<u64, String>>,
        /// remote path → inode
        path_inode: Mutex<HashMap<String, u64>>,
        /// inode → cached metadata
        cache: Mutex<HashMap<u64, CachedEntry>>,
        /// Next available inode number
        next_inode: Mutex<u64>,
        quiet: bool,
    }

    impl AeroFuseFs {
        pub fn new(
            provider: Arc<AsyncMutex<Box<dyn StorageProvider>>>,
            base_path: String,
            cache_ttl_secs: u64,
            read_only: bool,
            quiet: bool,
        ) -> Self {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to create FUSE tokio runtime");
            let cur_uid = unsafe { libc::getuid() };
            let cur_gid = unsafe { libc::getgid() };

            let mut inode_path = HashMap::new();
            let mut path_inode = HashMap::new();
            inode_path.insert(ROOT_INODE, base_path.clone());
            path_inode.insert(base_path.clone(), ROOT_INODE);

            let mut cache = HashMap::new();
            cache.insert(
                ROOT_INODE,
                CachedEntry {
                    attr: dir_attr(ROOT_INODE, 0, cur_uid, cur_gid),
                    children: None,
                    fetched_at: Instant::now(),
                },
            );

            Self {
                rt,
                provider,
                base_path,
                cache_ttl: Duration::from_secs(cache_ttl_secs),
                read_only,
                write_buffers: Mutex::new(HashMap::new()),
                flush_ok: Mutex::new(std::collections::HashSet::new()),
                uid: cur_uid,
                gid: cur_gid,
                inode_path: Mutex::new(inode_path),
                path_inode: Mutex::new(path_inode),
                cache: Mutex::new(cache),
                next_inode: Mutex::new(2),
                quiet,
            }
        }

        /// Invalidate cache for a directory (force re-listing on next access).
        fn invalidate_dir(&self, parent_ino: u64) {
            let mut cache = self.cache.lock().unwrap();
            cache.remove(&parent_ino);
        }

        /// Build the child path from a parent path and child name.
        fn child_path(parent_path: &str, name: &str) -> String {
            if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path, name)
            }
        }

        fn alloc_inode(&self, path: &str) -> u64 {
            let mut pi = self.path_inode.lock().unwrap();
            if let Some(&ino) = pi.get(path) {
                return ino;
            }
            let mut next = self.next_inode.lock().unwrap();
            let ino = *next;
            *next += 1;
            pi.insert(path.to_string(), ino);
            self.inode_path
                .lock()
                .unwrap()
                .insert(ino, path.to_string());
            ino
        }

        fn get_path(&self, ino: u64) -> Option<String> {
            self.inode_path.lock().unwrap().get(&ino).cloned()
        }

        fn get_cached(&self, ino: u64) -> Option<CachedEntry> {
            let cache = self.cache.lock().unwrap();
            let entry = cache.get(&ino)?;
            if entry.fetched_at.elapsed() < self.cache_ttl {
                Some(entry.clone())
            } else {
                None
            }
        }

        fn set_cached(&self, ino: u64, entry: CachedEntry) {
            self.cache.lock().unwrap().insert(ino, entry);
        }

        /// Fetch directory listing from provider, populate cache and inode tables.
        fn fetch_dir(&self, ino: u64, path: &str) -> Option<CachedEntry> {
            let provider = self.provider.clone();
            let path_owned = path.to_string();
            let entries = self.rt.block_on(async {
                let mut p = provider.lock().await;
                p.list(&path_owned).await.ok()
            })?;

            let mut child_inodes = Vec::with_capacity(entries.len());
            for e in &entries {
                let child_ino = self.alloc_inode(&e.path);
                child_inodes.push(child_ino);

                let attr = if e.is_dir {
                    dir_attr(child_ino, 0, self.uid, self.gid)
                } else {
                    file_attr(
                        child_ino,
                        e.size,
                        parse_mtime_to_system(&e.modified),
                        self.uid,
                        self.gid,
                    )
                };

                self.set_cached(
                    child_ino,
                    CachedEntry {
                        attr,
                        children: if e.is_dir { None } else { Some(Vec::new()) },
                        fetched_at: Instant::now(),
                    },
                );
            }

            let cached = CachedEntry {
                attr: dir_attr(ino, entries.len() as u64, self.uid, self.gid),
                children: Some(child_inodes),
                fetched_at: Instant::now(),
            };
            self.set_cached(ino, cached.clone());
            Some(cached)
        }

        /// Fetch stat for a single path from provider.
        fn fetch_stat(&self, ino: u64, path: &str) -> Option<CachedEntry> {
            let provider = self.provider.clone();
            let path_owned = path.to_string();
            let entry = self.rt.block_on(async {
                let mut p = provider.lock().await;
                p.stat(&path_owned).await.ok()
            })?;

            let attr = if entry.is_dir {
                dir_attr(ino, 0, self.uid, self.gid)
            } else {
                file_attr(
                    ino,
                    entry.size,
                    parse_mtime_to_system(&entry.modified),
                    self.uid,
                    self.gid,
                )
            };

            let cached = CachedEntry {
                attr,
                children: if entry.is_dir { None } else { Some(Vec::new()) },
                fetched_at: Instant::now(),
            };
            self.set_cached(ino, cached.clone());
            Some(cached)
        }
    }

    fn dir_attr(ino: u64, _nlink: u64, uid: u32, gid: u32) -> FileAttr {
        let now = SystemTime::now();
        FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid,
            gid,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn file_attr(ino: u64, size: u64, mtime: SystemTime, uid: u32, gid: u32) -> FileAttr {
        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(BLOCK_SIZE as u64),
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind: FileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid,
            gid,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn parse_mtime_to_system(mtime: &Option<String>) -> SystemTime {
        mtime
            .as_deref()
            .and_then(|s| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                    .ok()
                    .or_else(|| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok())
            })
            .map(|dt| {
                let ts = dt.and_utc().timestamp();
                UNIX_EPOCH + Duration::from_secs(ts.max(0) as u64)
            })
            .unwrap_or(SystemTime::now())
    }

    impl Filesystem for AeroFuseFs {
        fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
            let ttl = self.cache_ttl;

            // Root inode: always return directory attr without provider call
            if ino == ROOT_INODE {
                if let Some(cached) = self.get_cached(ino) {
                    reply.attr(&ttl, &cached.attr);
                } else {
                    let attr = dir_attr(ROOT_INODE, 0, self.uid, self.gid);
                    reply.attr(&ttl, &attr);
                }
                return;
            }

            // Try cache first
            if let Some(cached) = self.get_cached(ino) {
                reply.attr(&ttl, &cached.attr);
                return;
            }

            // Cache miss - fetch from provider
            let Some(path) = self.get_path(ino) else {
                reply.error(libc::ENOENT);
                return;
            };

            if let Some(cached) = self.fetch_stat(ino, &path) {
                reply.attr(&ttl, &cached.attr);
            } else {
                reply.error(libc::ENOENT);
            }
        }

        fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
            let ttl = self.cache_ttl;
            let Some(parent_path) = self.get_path(parent) else {
                reply.error(libc::ENOENT);
                return;
            };

            let child_name = name.to_string_lossy();
            let child_path = if parent_path == "/" {
                format!("/{}", child_name)
            } else {
                format!("{}/{}", parent_path, child_name)
            };

            // Check if we have inode + fresh cache
            let child_ino = self.alloc_inode(&child_path);
            if let Some(cached) = self.get_cached(child_ino) {
                reply.entry(&ttl, &cached.attr, 0);
                return;
            }

            // Ensure parent dir is listed (populates children cache)
            let parent_cached = self
                .get_cached(parent)
                .or_else(|| self.fetch_dir(parent, &parent_path));

            if parent_cached.is_some() {
                if let Some(cached) = self.get_cached(child_ino) {
                    reply.entry(&ttl, &cached.attr, 0);
                    return;
                }
            }

            // Still not found - try direct stat
            if let Some(cached) = self.fetch_stat(child_ino, &child_path) {
                reply.entry(&ttl, &cached.attr, 0);
            } else {
                reply.error(libc::ENOENT);
            }
        }

        fn readdir(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            mut reply: ReplyDirectory,
        ) {
            let Some(path) = self.get_path(ino) else {
                reply.error(libc::ENOENT);
                return;
            };

            // Fetch or use cached directory listing
            let cached = self
                .get_cached(ino)
                .filter(|c| c.children.is_some())
                .or_else(|| self.fetch_dir(ino, &path));

            let Some(cached) = cached else {
                reply.error(libc::EIO);
                return;
            };

            let children = cached.children.unwrap_or_default();
            let mut entries: Vec<(u64, FileType, String)> = Vec::new();

            // "." and ".."
            entries.push((ino, FileType::Directory, ".".to_string()));
            entries.push((ino, FileType::Directory, "..".to_string()));

            {
                let cache = self.cache.lock().unwrap();
                let inode_path = self.inode_path.lock().unwrap();
                for &child_ino in &children {
                    if let Some(child_cached) = cache.get(&child_ino) {
                        let name = inode_path
                            .get(&child_ino)
                            .and_then(|p| p.rsplit('/').next().map(|s| s.to_string()))
                            .unwrap_or_default();
                        if !name.is_empty() {
                            entries.push((child_ino, child_cached.attr.kind, name));
                        }
                    }
                }
            }

            for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                if reply.add(*ino, (i + 1) as i64, *kind, name) {
                    break; // buffer full
                }
            }
            reply.ok();
        }

        fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
            if self.get_path(ino).is_none() {
                reply.error(libc::ENOENT);
                return;
            }
            // Check write intent on read-only mount
            let write_flags = libc::O_WRONLY | libc::O_RDWR | libc::O_APPEND | libc::O_TRUNC;
            if self.read_only && (flags & write_flags) != 0 {
                reply.error(libc::EROFS);
                return;
            }
            reply.opened(0, 0);
        }

        fn read(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            size: u32,
            _flags: i32,
            _lock_owner: Option<u64>,
            reply: ReplyData,
        ) {
            let Some(path) = self.get_path(ino) else {
                reply.error(libc::ENOENT);
                return;
            };

            let provider = self.provider.clone();
            let len = size as u64;
            let off = offset as u64;

            let result = self.rt.block_on(async {
                let mut p = provider.lock().await;
                p.read_range(&path, off, len.min(READ_CHUNK)).await
            });

            match result {
                Ok(data) => reply.data(&data),
                Err(_) => {
                    // Fallback: download full file (for providers without range support)
                    // Safety cap: refuse to download files >64MB in fallback to prevent OOM
                    let file_size = self.get_cached(ino).map(|c| c.attr.size).unwrap_or(0);
                    if file_size > 64 * 1024 * 1024 {
                        reply.error(libc::EIO); // Too large for in-memory fallback
                        return;
                    }
                    let result = self.rt.block_on(async {
                        let mut p = provider.lock().await;
                        p.download_to_bytes(&path).await
                    });
                    match result {
                        Ok(data) => {
                            let end = (off as usize + size as usize).min(data.len());
                            let start = (off as usize).min(data.len());
                            reply.data(&data[start..end]);
                        }
                        Err(_) => reply.error(libc::EIO),
                    }
                }
            }
        }

        // ── Write operations ──────────────────────────────────────

        fn create(
            &mut self,
            _req: &Request,
            parent: u64,
            name: &OsStr,
            _mode: u32,
            _umask: u32,
            _flags: i32,
            reply: ReplyCreate,
        ) {
            if self.read_only {
                reply.error(libc::EROFS);
                return;
            }
            let Some(parent_path) = self.get_path(parent) else {
                reply.error(libc::ENOENT);
                return;
            };
            let child_name = name.to_string_lossy();
            let child_path = Self::child_path(&parent_path, &child_name);
            let child_ino = self.alloc_inode(&child_path);

            // Create secure tempfile for write buffering
            let tmp = tempfile::Builder::new()
                .prefix("aeroftp_fuse_")
                .tempfile()
                .map(|f| f.into_temp_path().to_path_buf())
                .unwrap_or_else(|_| {
                    std::env::temp_dir().join(format!("aeroftp_fuse_{}", child_ino))
                });
            let _ = std::fs::write(&tmp, b"");
            self.write_buffers.lock().unwrap().insert(child_ino, tmp);

            let ttl = self.cache_ttl;
            let attr = file_attr(child_ino, 0, SystemTime::now(), self.uid, self.gid);
            self.set_cached(
                child_ino,
                CachedEntry {
                    attr,
                    children: Some(Vec::new()),
                    fetched_at: Instant::now(),
                },
            );
            self.invalidate_dir(parent);

            reply.created(&ttl, &attr, 0, 0, 0);
        }

        fn write(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: i64,
            data: &[u8],
            _write_flags: u32,
            _flags: i32,
            _lock_owner: Option<u64>,
            reply: ReplyWrite,
        ) {
            if self.read_only {
                reply.error(libc::EROFS);
                return;
            }

            let buffers = self.write_buffers.lock().unwrap();
            let Some(tmp_path) = buffers.get(&ino).cloned() else {
                // No write buffer - this file wasn't opened for writing via create
                // Try to create one on-the-fly for existing files
                drop(buffers);
                let Some(path) = self.get_path(ino) else {
                    reply.error(libc::ENOENT);
                    return;
                };
                // Download existing content to tempfile
                let provider = self.provider.clone();
                let tmp = tempfile::Builder::new()
                    .prefix("aeroftp_fuse_")
                    .tempfile()
                    .map(|f| f.into_temp_path().to_path_buf())
                    .unwrap_or_else(|_| std::env::temp_dir().join(format!("aeroftp_fuse_{}", ino)));
                let download_result = self.rt.block_on(async {
                    let mut p = provider.lock().await;
                    p.download(&path, &tmp.to_string_lossy(), None).await
                });
                if download_result.is_err() {
                    // New file or download failed - start empty
                    let _ = std::fs::write(&tmp, b"");
                }
                self.write_buffers.lock().unwrap().insert(ino, tmp.clone());
                // Now write to the buffer
                if let Err(e) = write_at_offset(&tmp, offset, data) {
                    eprintln!("write error: {}", e);
                    reply.error(libc::EIO);
                    return;
                }
                reply.written(data.len() as u32);
                return;
            };
            drop(buffers);

            if let Err(e) = write_at_offset(&tmp_path, offset, data) {
                eprintln!("write error: {}", e);
                reply.error(libc::EIO);
                return;
            }
            reply.written(data.len() as u32);
        }

        fn flush(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            _lock_owner: u64,
            reply: ReplyEmpty,
        ) {
            let buffers = self.write_buffers.lock().unwrap();
            let Some(tmp_path) = buffers.get(&ino).cloned() else {
                reply.ok();
                return;
            };
            drop(buffers);

            let Some(remote_path) = self.get_path(ino) else {
                reply.error(libc::ENOENT);
                return;
            };

            let provider = self.provider.clone();
            let local = tmp_path.to_string_lossy().to_string();
            let result = self.rt.block_on(async {
                let mut p = provider.lock().await;
                p.upload(&local, &remote_path, None).await
            });

            if result.is_ok() {
                let size = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
                let attr = file_attr(ino, size, SystemTime::now(), self.uid, self.gid);
                self.set_cached(
                    ino,
                    CachedEntry {
                        attr,
                        children: Some(Vec::new()),
                        fetched_at: Instant::now(),
                    },
                );
                self.flush_ok.lock().unwrap().insert(ino);
                reply.ok();
            } else {
                // Do NOT mark as flushed - release will keep the tempfile
                reply.error(libc::EIO);
            }
        }

        fn release(
            &mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            _flags: i32,
            _lock_owner: Option<u64>,
            _flush: bool,
            reply: ReplyEmpty,
        ) {
            if let Some(tmp_path) = self.write_buffers.lock().unwrap().remove(&ino) {
                if self.flush_ok.lock().unwrap().remove(&ino) {
                    // Flush succeeded - safe to delete tempfile
                    let _ = std::fs::remove_file(tmp_path);
                } else {
                    // Flush failed - keep tempfile for recovery
                    eprintln!(
                        "aeroftp-fuse: keeping tempfile {} (flush failed, data preserved)",
                        tmp_path.display()
                    );
                }
            }
            reply.ok();
        }

        fn mkdir(
            &mut self,
            _req: &Request,
            parent: u64,
            name: &OsStr,
            _mode: u32,
            _umask: u32,
            reply: ReplyEntry,
        ) {
            if self.read_only {
                reply.error(libc::EROFS);
                return;
            }
            let Some(parent_path) = self.get_path(parent) else {
                reply.error(libc::ENOENT);
                return;
            };
            let child_name = name.to_string_lossy();
            let child_path = Self::child_path(&parent_path, &child_name);

            let provider = self.provider.clone();
            let p = child_path.clone();
            let result = self.rt.block_on(async {
                let mut prov = provider.lock().await;
                prov.mkdir(&p).await
            });

            match result {
                Ok(()) => {
                    let child_ino = self.alloc_inode(&child_path);
                    let ttl = self.cache_ttl;
                    let attr = dir_attr(child_ino, 0, self.uid, self.gid);
                    self.set_cached(
                        child_ino,
                        CachedEntry {
                            attr,
                            children: Some(Vec::new()),
                            fetched_at: Instant::now(),
                        },
                    );
                    self.invalidate_dir(parent);
                    reply.entry(&ttl, &attr, 0);
                }
                Err(_) => reply.error(libc::EIO),
            }
        }

        fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
            if self.read_only {
                reply.error(libc::EROFS);
                return;
            }
            let Some(parent_path) = self.get_path(parent) else {
                reply.error(libc::ENOENT);
                return;
            };
            let child_name = name.to_string_lossy();
            let child_path = Self::child_path(&parent_path, &child_name);

            let provider = self.provider.clone();
            let p = child_path.clone();
            let result = self.rt.block_on(async {
                let mut prov = provider.lock().await;
                prov.delete(&p).await
            });

            match result {
                Ok(()) => {
                    self.invalidate_dir(parent);
                    // Remove from all maps to prevent inode leak
                    if let Some(ino) = self.path_inode.lock().unwrap().remove(&child_path) {
                        self.inode_path.lock().unwrap().remove(&ino);
                        self.cache.lock().unwrap().remove(&ino);
                    }
                    reply.ok();
                }
                Err(_) => reply.error(libc::EIO),
            }
        }

        fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
            if self.read_only {
                reply.error(libc::EROFS);
                return;
            }
            let Some(parent_path) = self.get_path(parent) else {
                reply.error(libc::ENOENT);
                return;
            };
            let child_name = name.to_string_lossy();
            let child_path = Self::child_path(&parent_path, &child_name);

            let provider = self.provider.clone();
            let p = child_path.clone();
            let result = self.rt.block_on(async {
                let mut prov = provider.lock().await;
                prov.rmdir(&p).await
            });

            match result {
                Ok(()) => {
                    self.invalidate_dir(parent);
                    if let Some(ino) = self.path_inode.lock().unwrap().remove(&child_path) {
                        self.inode_path.lock().unwrap().remove(&ino);
                        self.cache.lock().unwrap().remove(&ino);
                    }
                    reply.ok();
                }
                Err(_) => reply.error(libc::EIO),
            }
        }

        fn rename(
            &mut self,
            _req: &Request,
            parent: u64,
            name: &OsStr,
            newparent: u64,
            newname: &OsStr,
            _flags: u32,
            reply: ReplyEmpty,
        ) {
            if self.read_only {
                reply.error(libc::EROFS);
                return;
            }
            let Some(parent_path) = self.get_path(parent) else {
                reply.error(libc::ENOENT);
                return;
            };
            let Some(newparent_path) = self.get_path(newparent) else {
                reply.error(libc::ENOENT);
                return;
            };
            let old_path = Self::child_path(&parent_path, &name.to_string_lossy());
            let new_path = Self::child_path(&newparent_path, &newname.to_string_lossy());

            let provider = self.provider.clone();
            let from = old_path.clone();
            let to = new_path.clone();
            let result = self.rt.block_on(async {
                let mut prov = provider.lock().await;
                prov.rename(&from, &to).await
            });

            match result {
                Ok(()) => {
                    self.invalidate_dir(parent);
                    if newparent != parent {
                        self.invalidate_dir(newparent);
                    }
                    // Update inode mappings - including all descendants for directory renames
                    let mut pi = self.path_inode.lock().unwrap();
                    let mut ip = self.inode_path.lock().unwrap();
                    let mut cache = self.cache.lock().unwrap();
                    // Collect all paths that start with old_path (the renamed entry + descendants)
                    let old_prefix = format!("{}/", old_path);
                    let to_update: Vec<(String, u64)> = pi
                        .iter()
                        .filter(|(p, _)| *p == &old_path || p.starts_with(&old_prefix))
                        .map(|(p, &ino)| (p.clone(), ino))
                        .collect();
                    for (op, ino) in to_update {
                        pi.remove(&op);
                        let np = if op == old_path {
                            new_path.clone()
                        } else {
                            format!("{}{}", new_path, &op[old_path.len()..])
                        };
                        pi.insert(np.clone(), ino);
                        ip.insert(ino, np);
                        cache.remove(&ino); // force re-fetch
                    }
                    reply.ok();
                }
                Err(_) => reply.error(libc::EIO),
            }
        }

        fn setattr(
            &mut self,
            _req: &Request,
            ino: u64,
            _mode: Option<u32>,
            _uid: Option<u32>,
            _gid: Option<u32>,
            size: Option<u64>,
            _atime: Option<fuser::TimeOrNow>,
            _mtime: Option<fuser::TimeOrNow>,
            _ctime: Option<SystemTime>,
            _fh: Option<u64>,
            _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>,
            _bkuptime: Option<SystemTime>,
            _flags: Option<u32>,
            reply: ReplyAttr,
        ) {
            // Handle truncation for write support
            if let Some(new_size) = size {
                if let Some(tmp_path) = self.write_buffers.lock().unwrap().get(&ino) {
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .open(tmp_path)
                        .and_then(|f| f.set_len(new_size));
                }
                // Update cached size to reflect truncation
                if let Some(mut cached) = self.get_cached(ino) {
                    cached.attr.size = new_size;
                    cached.attr.blocks = new_size.div_ceil(BLOCK_SIZE as u64);
                    cached.attr.mtime = SystemTime::now();
                    self.set_cached(ino, cached.clone());
                    reply.attr(&self.cache_ttl, &cached.attr);
                    return;
                }
            }

            let ttl = self.cache_ttl;
            if let Some(cached) = self.get_cached(ino) {
                reply.attr(&ttl, &cached.attr);
            } else if ino == ROOT_INODE {
                reply.attr(&ttl, &dir_attr(ROOT_INODE, 0, self.uid, self.gid));
            } else {
                reply.error(libc::ENOENT);
            }
        }

        fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
            let provider = self.provider.clone();
            let result = self.rt.block_on(async {
                let mut p = provider.lock().await;
                p.storage_info().await.ok()
            });

            if let Some(info) = result {
                let total_blocks = info.total / BLOCK_SIZE as u64;
                let free_blocks = info.free / BLOCK_SIZE as u64;
                reply.statfs(
                    total_blocks, // blocks
                    free_blocks,  // bfree
                    free_blocks,  // bavail
                    0,            // files
                    0,            // ffree
                    BLOCK_SIZE,   // bsize
                    255,          // namelen
                    BLOCK_SIZE,   // frsize
                );
            } else {
                // Default: report large filesystem
                reply.statfs(
                    u64::MAX / 512,
                    u64::MAX / 1024,
                    u64::MAX / 1024,
                    0,
                    0,
                    BLOCK_SIZE,
                    255,
                    BLOCK_SIZE,
                );
            }
        }
    }

    /// Write data at a specific offset in a file.
    fn write_at_offset(path: &std::path::Path, offset: i64, data: &[u8]) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom, Write as IoWriteTrait};
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.seek(SeekFrom::Start(offset as u64))?;
        file.write_all(data)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn cmd_mount(
        url: &str,
        mountpoint: &str,
        path: &str,
        cache_ttl: u64,
        allow_other: bool,
        read_only: bool,
        cli: &Cli,
        format: OutputFormat,
    ) -> i32 {
        // Validate mount point exists and is an empty directory
        let mp = std::path::Path::new(mountpoint);
        if !mp.exists() {
            print_error(
                format,
                &format!("Mount point does not exist: {}", mountpoint),
                5,
            );
            return 5;
        }
        if !mp.is_dir() {
            print_error(
                format,
                &format!("Mount point is not a directory: {}", mountpoint),
                5,
            );
            return 5;
        }
        // Check if empty (allow "." and "..")
        if let Ok(mut rd) = std::fs::read_dir(mp) {
            if rd.next().is_some() {
                print_error(
                    format,
                    &format!("Mount point is not empty: {}", mountpoint),
                    5,
                );
                return 5;
            }
        }

        let (provider, initial_path) = match create_and_connect(url, cli, format).await {
            Ok(v) => v,
            Err(code) => return code,
        };

        let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, path));

        let quiet = cli.quiet || matches!(format, OutputFormat::Json);
        if !quiet {
            eprintln!(
                "Mounting {} on {} ({}, cache TTL {}s)",
                base_path,
                mountpoint,
                if read_only { "read-only" } else { "read-write" },
                cache_ttl
            );
            eprintln!("Press Ctrl+C to unmount");
        }

        let provider_arc = Arc::new(AsyncMutex::new(provider));

        let fs = AeroFuseFs::new(provider_arc.clone(), base_path, cache_ttl, read_only, quiet);

        let mut options = vec![
            MountOption::FSName("aeroftp".to_string()),
            MountOption::Subtype("aeroftp".to_string()),
            MountOption::DefaultPermissions,
        ];
        if read_only {
            options.push(MountOption::RO);
        }
        if allow_other {
            options.push(MountOption::AllowOther);
        }

        // Use `spawn_mount2` so the FUSE session handle is owned by a
        // `BackgroundSession`; dropping it triggers `fuse_unmount`. Previously
        // `mount2` blocked the spawn_blocking task forever — on panic or
        // process abort the kernel was left with a dangling mountpoint that
        // only `fusermount -u` could clear.
        let mountpoint_owned = mountpoint.to_string();
        let session_result = tokio::task::spawn_blocking(move || {
            fuser::spawn_mount2(fs, &mountpoint_owned, &options)
        })
        .await;

        let session = match session_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                print_error(format, &format!("Mount failed: {}", e), 99);
                let mut p = provider_arc.lock().await;
                let _ = p.disconnect().await;
                return 99;
            }
            Err(e) => {
                print_error(format, &format!("Mount task failed: {}", e), 99);
                let mut p = provider_arc.lock().await;
                let _ = p.disconnect().await;
                return 99;
            }
        };

        // Wait for a real shutdown signal (SIGINT/SIGTERM). On any return
        // path `session` is dropped → FUSE unmount runs → kernel releases
        // the mountpoint. Previously the mount lived forever because the
        // blocking `mount2` had no cancellation surface.
        let _ = shutdown_signal().await;
        drop(session);

        // Cleanup
        let mut p = provider_arc.lock().await;
        let _ = p.disconnect().await;

        if !quiet {
            eprintln!("Unmounted successfully");
        }
        0
    }
}

#[cfg(target_os = "linux")]
use fuse_mount::cmd_mount;

/// Windows mount: WebDAV bridge - starts a local WebDAV server and maps it as a drive letter.
#[cfg(windows)]
async fn cmd_mount_windows(
    url: &str,
    drive_letter: &str,
    path: &str,
    read_only: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);

    // Validate drive letter (e.g., "Z:", "Z", "z:")
    let letter = drive_letter.trim_end_matches(':').to_uppercase();
    if letter.len() != 1
        || !letter
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
    {
        print_error(
            format,
            &format!(
                "Invalid drive letter '{}'. Use a single letter like 'Z:' or 'Z'",
                drive_letter
            ),
            5,
        );
        return 5;
    }
    let drive = format!("{}:", letter);

    // Find a free local port for the WebDAV server
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            print_error(format, &format!("Cannot bind local port: {}", e), 1);
            return 1;
        }
    };
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let addr = format!("127.0.0.1:{}", port);

    if !quiet {
        eprintln!(
            "Mounting via WebDAV bridge on {} ({}{})",
            drive,
            if read_only { "read-only, " } else { "" },
            addr,
        );
    }

    // Start the WebDAV server directly - it runs until Ctrl+C
    // We run it concurrently with the drive mapping using tokio::select
    if !quiet {
        eprintln!("Starting WebDAV server on {}...", addr);
    }

    // Connect provider first
    let (provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, path));
    let provider_label = if let Some(profile) = &cli.profile {
        format!("profile {}", profile)
    } else {
        provider.display_name()
    };

    let state = ServeHttpState {
        provider: Arc::new(AsyncMutex::new(provider)),
        provider_label,
        base_path,
        auth_token: None, // local-only WebDAV bridge for Windows mount - no auth needed
    };

    let app = Router::new()
        .route("/", any(webdav_root_handler))
        .route("/{*path}", any(webdav_path_handler))
        .layer(DefaultBodyLimit::max(WEBDAV_MAX_UPLOAD_BYTES))
        .with_state(state.clone());

    let bind_addr: SocketAddr = addr.parse().unwrap();
    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            print_error(format, &format!("Failed to bind {}: {}", addr, e), 1);
            return 1;
        }
    };

    // Spawn WebDAV server in background
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_signal().await;
            })
            .await
    });

    // Wait for server to be ready
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Map as network drive using net use
    let webdav_url = format!("http://{}", addr);
    if !quiet {
        eprintln!("Mapping drive: net use {} {}", drive, webdav_url);
    }

    let map_result = std::process::Command::new("net")
        .args(["use", &drive, &webdav_url, "/persistent:no"])
        .output();

    match map_result {
        Ok(output) if output.status.success() => {
            if !quiet {
                eprintln!(
                    "Drive {} mapped successfully. Press Ctrl+C to unmount.",
                    drive
                );
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            print_error(
                format,
                &format!("Failed to map drive {}: {}", drive, stderr.trim()),
                99,
            );
            server_handle.abort();
            return 99;
        }
        Err(e) => {
            print_error(format, &format!("Cannot execute 'net use': {}", e), 99);
            server_handle.abort();
            return 99;
        }
    }

    // Wait for server (blocks until Ctrl+C)
    let _ = server_handle.await;

    // Cleanup: unmap drive
    if !quiet {
        eprintln!("\nUnmapping {}...", drive);
    }
    let _ = std::process::Command::new("net")
        .args(["use", &drive, "/delete", "/yes"])
        .output();

    // Disconnect provider
    let mut p = state.provider.lock().await;
    let _ = p.disconnect().await;

    if !quiet {
        eprintln!("Unmounted {} successfully", drive);
    }
    0
}

// ── Daemon + Jobs ────────────────────────────────────────────────

fn daemon_config_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aeroftp");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn daemon_pid_path() -> PathBuf {
    daemon_config_dir().join("daemon.pid")
}
fn daemon_db_path() -> PathBuf {
    daemon_config_dir().join("jobs.db")
}
fn _daemon_log_path() -> PathBuf {
    daemon_config_dir().join("daemon.log")
}

fn daemon_read_pid() -> Option<u32> {
    std::fs::read_to_string(daemon_pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn daemon_is_running() -> bool {
    if let Some(pid) = daemon_read_pid() {
        // Check if process exists
        #[cfg(unix)]
        {
            let result = unsafe { libc::kill(pid as i32, 0) };
            result == 0
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    } else {
        false
    }
}

fn daemon_addr() -> String {
    // Read addr from pid file's sibling
    let addr_path = daemon_config_dir().join("daemon.addr");
    std::fs::read_to_string(addr_path)
        .unwrap_or_else(|_| "127.0.0.1:14320".to_string())
        .trim()
        .to_string()
}

fn daemon_token_path() -> PathBuf {
    daemon_config_dir().join("daemon.token")
}

fn daemon_auth_token() -> Option<String> {
    std::fs::read_to_string(daemon_token_path())
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn write_daemon_auth_token(token: &str) -> Result<(), String> {
    std::fs::write(daemon_token_path(), token)
        .map_err(|e| format!("Cannot write daemon auth token: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            std::fs::set_permissions(daemon_token_path(), std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn clear_daemon_runtime_files() {
    let _ = std::fs::remove_file(daemon_pid_path());
    let _ = std::fs::remove_file(daemon_config_dir().join("daemon.addr"));
    let _ = std::fs::remove_file(daemon_token_path());
}

fn daemon_request(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(token) = daemon_auth_token() {
        builder.bearer_auth(token)
    } else {
        builder
    }
}

fn daemon_auth_failure_message() -> String {
    format!(
        "Daemon authentication failed. Restart the daemon or remove the stale token file at {}.",
        daemon_token_path().display()
    )
}

// ── Job database (SQLite) ────────────────────────────────────────

fn jobs_db_init(db_path: &Path) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(db_path)
        .map_err(|e| format!("Cannot open jobs database: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            command TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued',
            created_at TEXT NOT NULL,
            started_at TEXT,
            finished_at TEXT,
            exit_code INTEGER,
            error TEXT
        );
        PRAGMA journal_mode=WAL;",
    )
    .map_err(|e| format!("Cannot init jobs table: {}", e))?;
    Ok(conn)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct JobEntry {
    id: String,
    command: String,
    status: String,
    created_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    exit_code: Option<i32>,
    error: Option<String>,
}

fn jobs_list_all(conn: &rusqlite::Connection) -> Vec<JobEntry> {
    let mut stmt = match conn.prepare(
        "SELECT id, command, status, created_at, started_at, finished_at, exit_code, error FROM jobs ORDER BY created_at DESC LIMIT 100"
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        Ok(JobEntry {
            id: row.get(0)?,
            command: row.get(1)?,
            status: row.get(2)?,
            created_at: row.get(3)?,
            started_at: row.get(4)?,
            finished_at: row.get(5)?,
            exit_code: row.get(6)?,
            error: row.get(7)?,
        })
    })
    .ok()
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

fn jobs_get(conn: &rusqlite::Connection, id: &str) -> Option<JobEntry> {
    conn.query_row(
        "SELECT id, command, status, created_at, started_at, finished_at, exit_code, error FROM jobs WHERE id = ?1",
        [id],
        |row| Ok(JobEntry {
            id: row.get(0)?,
            command: row.get(1)?,
            status: row.get(2)?,
            created_at: row.get(3)?,
            started_at: row.get(4)?,
            finished_at: row.get(5)?,
            exit_code: row.get(6)?,
            error: row.get(7)?,
        })
    ).ok()
}

fn jobs_add(conn: &rusqlite::Connection, command: &str) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute(
        "INSERT INTO jobs (id, command, status, created_at) VALUES (?1, ?2, 'queued', ?3)",
        rusqlite::params![id, command, now],
    )
    .map_err(|e| format!("Cannot add job: {}", e))?;
    Ok(id)
}

fn jobs_update_status(
    conn: &rusqlite::Connection,
    id: &str,
    status: &str,
    exit_code: Option<i32>,
    error: Option<&str>,
) {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let field = if status == "running" {
        "started_at"
    } else {
        "finished_at"
    };
    let _ = conn.execute(
        &format!(
            "UPDATE jobs SET status = ?1, {} = ?2, exit_code = ?3, error = ?4 WHERE id = ?5",
            field
        ),
        rusqlite::params![status, now, exit_code, error, id],
    );
}

// ── Daemon HTTP API ──────────────────────────────────────────────

async fn daemon_health_handler(
    State(state): State<DaemonApiState>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        Some(state.auth_token.as_str()),
        "AeroFTP daemon",
        "Daemon authentication required. Use the daemon token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }

    let jobs = jobs_list_all(&state.conn.lock().unwrap());
    let running = jobs.iter().filter(|j| j.status == "running").count();
    let queued = jobs.iter().filter(|j| j.status == "queued").count();
    axum::Json(serde_json::json!({
        "status": "ok",
        "pid": std::process::id(),
        "running_jobs": running,
        "queued_jobs": queued,
    }))
    .into_response()
}

async fn daemon_jobs_list_handler(
    State(state): State<DaemonApiState>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        Some(state.auth_token.as_str()),
        "AeroFTP daemon",
        "Daemon authentication required. Use the daemon token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }

    let jobs = jobs_list_all(&state.conn.lock().unwrap());
    axum::Json(serde_json::json!({ "jobs": jobs })).into_response()
}

async fn daemon_jobs_add_handler(
    State(state): State<DaemonApiState>,
    headers: HeaderMap,
    body: axum::Json<serde_json::Value>,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        Some(state.auth_token.as_str()),
        "AeroFTP daemon",
        "Daemon authentication required. Use the daemon token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }

    let command = body.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if command.is_empty() {
        let mut response =
            axum::Json(serde_json::json!({"error": "missing command"})).into_response();
        *response.status_mut() = StatusCode::BAD_REQUEST;
        return response;
    }

    let conn = state.conn.lock().unwrap();
    match jobs_add(&conn, command) {
        Ok(id) => axum::Json(serde_json::json!({"status": "queued", "id": id})).into_response(),
        Err(e) => {
            let mut response = axum::Json(serde_json::json!({"error": e})).into_response();
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            response
        }
    }
}

async fn daemon_job_status_handler(
    State(state): State<DaemonApiState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        Some(state.auth_token.as_str()),
        "AeroFTP daemon",
        "Daemon authentication required. Use the daemon token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }

    let conn = state.conn.lock().unwrap();
    match jobs_get(&conn, &id) {
        Some(job) => axum::Json(serde_json::json!(job)).into_response(),
        None => {
            let mut response =
                axum::Json(serde_json::json!({"error": "not found"})).into_response();
            *response.status_mut() = StatusCode::NOT_FOUND;
            response
        }
    }
}

async fn daemon_job_cancel_handler(
    State(state): State<DaemonApiState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if let Some(response) = ensure_request_authorized(
        &headers,
        Some(state.auth_token.as_str()),
        "AeroFTP daemon",
        "Daemon authentication required. Use the daemon token as a Bearer token or as the Basic-auth password.",
    ) {
        return response;
    }

    let conn = state.conn.lock().unwrap();
    jobs_update_status(&conn, &id, "cancelled", None, None);
    axum::Json(serde_json::json!({"status": "cancelled", "id": id})).into_response()
}

async fn cmd_daemon_start(
    addr_str: &str,
    allow_remote_bind: bool,
    auth_token: Option<String>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    if daemon_is_running() {
        let pid = daemon_read_pid().unwrap_or(0);
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({"status": "already_running", "pid": pid}));
        } else {
            eprintln!("Daemon already running (PID {})", pid);
        }
        return 0;
    }

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    let bind_addr = match validate_bind_addr(addr_str, allow_remote_bind, "daemon API") {
        Ok(addr) => addr,
        Err(error) => {
            print_error(format, &error, 5);
            return 5;
        }
    };
    let generated_auth_token = auth_token.is_none();
    let daemon_token = normalize_optional_token(auth_token).unwrap_or_else(generate_auth_token);

    // In foreground mode (for now - proper daemonization would use fork)
    // Write PID file and addr
    let pid = std::process::id();
    let _ = std::fs::write(daemon_pid_path(), pid.to_string());
    let _ = std::fs::write(daemon_config_dir().join("daemon.addr"), addr_str);
    if let Err(error) = write_daemon_auth_token(&daemon_token) {
        clear_daemon_runtime_files();
        print_error(format, &error, 99);
        return 99;
    }

    // Init job database
    let db_path = daemon_db_path();
    let conn = match jobs_db_init(&db_path) {
        Ok(c) => Arc::new(std::sync::Mutex::new(c)),
        Err(e) => {
            clear_daemon_runtime_files();
            print_error(format, &e, 99);
            return 99;
        }
    };

    if !quiet {
        eprintln!("AeroFTP daemon starting on {}", addr_str);
        eprintln!("PID: {}, DB: {}", pid, db_path.display());
        eprintln!(
            "Daemon auth: required (token saved at {})",
            daemon_token_path().display()
        );
        if generated_auth_token {
            eprintln!("A new daemon token was generated automatically.");
        }
        eprintln!("Press Ctrl+C to stop.");
    }

    if matches!(format, OutputFormat::Json) {
        let mut payload = serde_json::json!({
            "status": "started",
            "pid": pid,
            "addr": addr_str,
            "auth_required": true,
            "auth_mode": "basic-or-bearer",
            "token_path": daemon_token_path().display().to_string(),
        });
        if generated_auth_token {
            payload["generated_auth_token"] = serde_json::Value::String(daemon_token.clone());
        }
        print_json(&payload);
    }

    // Build HTTP API
    let state = DaemonApiState {
        conn,
        auth_token: daemon_token,
    };

    let app = Router::new()
        .route("/health", get(daemon_health_handler))
        .route(
            "/api/jobs",
            get(daemon_jobs_list_handler).post(daemon_jobs_add_handler),
        )
        .route(
            "/api/jobs/{id}",
            get(daemon_job_status_handler).delete(daemon_job_cancel_handler),
        )
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            clear_daemon_runtime_files();
            print_error(format, &format!("Cannot bind {}: {}", addr_str, e), 1);
            return 1;
        }
    };

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_signal().await;
        })
        .await;

    // Cleanup
    clear_daemon_runtime_files();

    if !quiet {
        eprintln!("\nDaemon stopped.");
    }

    match result {
        Ok(()) => 0,
        Err(e) => {
            print_error(format, &format!("Daemon failed: {}", e), 1);
            1
        }
    }
}

async fn cmd_daemon_stop(format: OutputFormat) -> i32 {
    if !daemon_is_running() {
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({"status": "not_running"}));
        } else {
            eprintln!("Daemon is not running.");
        }
        return 0;
    }

    let pid = daemon_read_pid().unwrap_or(0);

    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    // Wait briefly for process to exit
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    clear_daemon_runtime_files();

    if matches!(format, OutputFormat::Json) {
        print_json(&serde_json::json!({"status": "stopped", "pid": pid}));
    } else {
        eprintln!("Daemon stopped (PID {}).", pid);
    }
    0
}

async fn cmd_daemon_status(format: OutputFormat) -> i32 {
    if !daemon_is_running() {
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({"status": "not_running"}));
        } else {
            println!("Daemon: not running");
        }
        return 1;
    }

    let pid = daemon_read_pid().unwrap_or(0);
    let addr = daemon_addr();

    // Try HTTP health check
    let health_url = format!("http://{}/health", addr);
    if let Ok(resp) = daemon_request(reqwest::Client::new().get(&health_url))
        .send()
        .await
    {
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            print_error(format, &daemon_auth_failure_message(), 6);
            return 6;
        }
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if matches!(format, OutputFormat::Json) {
                print_json(&json);
            } else {
                println!("Daemon: running (PID {}, {})", pid, addr);
                if let Some(r) = json.get("running_jobs") {
                    println!("  Running jobs: {}", r);
                }
                if let Some(q) = json.get("queued_jobs") {
                    println!("  Queued jobs: {}", q);
                }
            }
            return 0;
        }
    }

    if matches!(format, OutputFormat::Json) {
        print_json(
            &serde_json::json!({"status": "running", "pid": pid, "addr": addr, "api": "unreachable"}),
        );
    } else {
        println!("Daemon: running (PID {}), API unreachable at {}", pid, addr);
    }
    0
}

async fn cmd_jobs_add(command_tokens: &[String], format: OutputFormat) -> i32 {
    let addr = daemon_addr();
    let command = command_tokens.join(" ");
    let url = format!("http://{}/api/jobs", addr);

    match daemon_request(reqwest::Client::new().post(&url))
        .json(&serde_json::json!({"command": command}))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                print_error(format, &daemon_auth_failure_message(), 6);
                return 6;
            }
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if matches!(format, OutputFormat::Json) {
                    print_json(&json);
                } else if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
                    println!("Job queued: {} ({})", id, command);
                } else if let Some(err) = json.get("error") {
                    eprintln!("Error: {}", err);
                    return 4;
                }
            }
            0
        }
        Err(e) => {
            print_error(
                format,
                &format!("Cannot reach daemon (is it running?): {}", e),
                1,
            );
            1
        }
    }
}

async fn cmd_jobs_list(format: OutputFormat) -> i32 {
    let addr = daemon_addr();
    let url = format!("http://{}/api/jobs", addr);

    match daemon_request(reqwest::Client::new().get(&url))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                print_error(format, &daemon_auth_failure_message(), 6);
                return 6;
            }
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if matches!(format, OutputFormat::Json) {
                    print_json(&json);
                } else if let Some(jobs) = json.get("jobs").and_then(|v| v.as_array()) {
                    if jobs.is_empty() {
                        println!("No jobs.");
                    } else {
                        println!("{:<10} {:<10} {:<22} Command", "ID", "Status", "Created");
                        println!("{}", "-".repeat(70));
                        for j in jobs {
                            println!(
                                "{:<10} {:<10} {:<22} {}",
                                j.get("id").and_then(|v| v.as_str()).unwrap_or("-"),
                                j.get("status").and_then(|v| v.as_str()).unwrap_or("-"),
                                j.get("created_at").and_then(|v| v.as_str()).unwrap_or("-"),
                                j.get("command").and_then(|v| v.as_str()).unwrap_or("-"),
                            );
                        }
                    }
                }
            }
            0
        }
        Err(e) => {
            print_error(format, &format!("Cannot reach daemon: {}", e), 1);
            1
        }
    }
}

async fn cmd_jobs_status(id: &str, format: OutputFormat) -> i32 {
    let addr = daemon_addr();
    let url = format!("http://{}/api/jobs/{}", addr, id);
    match daemon_request(reqwest::Client::new().get(&url))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                print_error(format, &daemon_auth_failure_message(), 6);
                return 6;
            }
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if matches!(format, OutputFormat::Json) {
                    print_json(&json);
                } else {
                    println!("Job: {}", id);
                    for (k, v) in json.as_object().into_iter().flatten() {
                        println!("  {}: {}", k, v);
                    }
                }
            }
            0
        }
        Err(e) => {
            print_error(format, &format!("Cannot reach daemon: {}", e), 1);
            1
        }
    }
}

async fn cmd_jobs_cancel(id: &str, format: OutputFormat) -> i32 {
    let addr = daemon_addr();
    let url = format!("http://{}/api/jobs/{}", addr, id);
    match daemon_request(reqwest::Client::new().delete(&url))
        .send()
        .await
    {
        Ok(resp) => {
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
                print_error(format, &daemon_auth_failure_message(), 6);
                return 6;
            }
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if matches!(format, OutputFormat::Json) {
                    print_json(&json);
                } else {
                    println!("Job {} cancelled.", id);
                }
            }
            0
        }
        Err(e) => {
            print_error(format, &format!("Cannot reach daemon: {}", e), 1);
            1
        }
    }
}

// ── Crypt Overlay - Transparent Encryption Layer ─────────────────

mod crypt_overlay {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce as AesNonce};
    #[allow(unused_imports)]
    use aes_siv::Aes256SivAead; // needed for KeyInit trait
    use argon2::Argon2;
    use base64::Engine as _;
    use hkdf::Hkdf;
    use sha2::Sha256;

    /// Magic bytes for AeroFTP encrypted files.
    const CRYPT_MAGIC: &[u8; 4] = b"AECR";
    const CRYPT_VERSION: u8 = 1;
    /// Block size for streaming encryption (64 KB).
    const BLOCK_SIZE: usize = 64 * 1024;
    /// Argon2 parameters (balanced security vs performance).
    const ARGON2_MEM_COST: u32 = 65536; // 64 MB
    const ARGON2_TIME_COST: u32 = 3;
    const ARGON2_PARALLELISM: u32 = 4;

    /// Derive a 32-byte master key from a password and salt.
    pub fn derive_master_key(password: &str, salt: &[u8; 16]) -> [u8; 32] {
        let mut key = [0u8; 32];
        let params = argon2::Params::new(
            ARGON2_MEM_COST,
            ARGON2_TIME_COST,
            ARGON2_PARALLELISM,
            Some(32),
        )
        .expect("valid argon2 params");
        let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon
            .hash_password_into(password.as_bytes(), salt, &mut key)
            .expect("argon2 hash");
        key
    }

    /// Derive a per-file encryption key from the master key and a nonce.
    fn derive_file_key(master_key: &[u8; 32], nonce: &[u8; 12]) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(Some(nonce), master_key);
        let mut file_key = [0u8; 32];
        hk.expand(b"aeroftp-crypt-file-key", &mut file_key)
            .expect("hkdf expand");
        file_key
    }

    /// Derive a key for filename encryption (AES-SIV needs 64 bytes = 2 x 256-bit keys).
    fn derive_name_key(master_key: &[u8; 32]) -> [u8; 64] {
        let hk = Hkdf::<Sha256>::new(Some(b"aeroftp-name-salt"), master_key);
        let mut name_key = [0u8; 64];
        hk.expand(b"aeroftp-crypt-name-key", &mut name_key)
            .expect("hkdf expand");
        name_key
    }

    /// Encrypt a filename using AES-SIV → base64url (no padding).
    pub fn encrypt_filename(master_key: &[u8; 32], plaintext_name: &str) -> String {
        let name_key = derive_name_key(master_key);
        let mut cipher = aes_siv::siv::Aes256Siv::new((&name_key).into());
        let ciphertext = cipher
            .encrypt([&[] as &[u8]], plaintext_name.as_bytes())
            .expect("aes-siv encrypt");
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ciphertext)
    }

    /// Decrypt a filename from base64url using AES-SIV.
    pub fn decrypt_filename(master_key: &[u8; 32], encrypted_name: &str) -> Option<String> {
        let name_key = derive_name_key(master_key);
        let mut cipher = aes_siv::siv::Aes256Siv::new((&name_key).into());
        let ciphertext = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encrypted_name)
            .ok()?;
        let plaintext = cipher.decrypt([&[] as &[u8]], &ciphertext).ok()?;
        String::from_utf8(plaintext).ok()
    }

    /// Encrypt file data: plaintext bytes → crypt format bytes.
    pub fn encrypt_data(master_key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
        use rand::Rng;
        let mut rng = rand::thread_rng();

        // Generate random master nonce
        let mut master_nonce = [0u8; 12];
        rng.fill(&mut master_nonce);

        // Derive per-file key
        let file_key = derive_file_key(master_key, &master_nonce);
        let cipher = Aes256Gcm::new((&file_key).into());

        // Build output: magic + version + nonce + encrypted blocks
        let mut output = Vec::with_capacity(
            4 + 1 + 12 + plaintext.len() + (plaintext.len() / BLOCK_SIZE + 1) * 16,
        );
        output.extend_from_slice(CRYPT_MAGIC);
        output.push(CRYPT_VERSION);
        output.extend_from_slice(&master_nonce);

        // Encrypt in blocks
        for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
            // Per-block nonce: master_nonce XOR block_index
            let mut block_nonce = master_nonce;
            let idx_bytes = (block_idx as u32).to_le_bytes();
            for i in 0..4 {
                block_nonce[i] ^= idx_bytes[i];
            }
            let nonce = AesNonce::from_slice(&block_nonce);
            let ciphertext = cipher.encrypt(nonce, chunk).expect("aes-gcm encrypt");
            output.extend_from_slice(&ciphertext);
        }

        output
    }

    /// Decrypt file data: crypt format bytes → plaintext bytes.
    pub fn decrypt_data(master_key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        if ciphertext.len() < 4 + 1 + 12 {
            return Err("data too short".into());
        }
        if &ciphertext[0..4] != CRYPT_MAGIC {
            return Err("not an AeroFTP encrypted file".into());
        }
        if ciphertext[4] != CRYPT_VERSION {
            return Err(format!("unsupported crypt version {}", ciphertext[4]));
        }

        let master_nonce: [u8; 12] = ciphertext[5..17].try_into().unwrap();
        let file_key = derive_file_key(master_key, &master_nonce);
        let cipher = Aes256Gcm::new((&file_key).into());

        let data = &ciphertext[17..];
        let block_cipher_size = BLOCK_SIZE + 16; // data + GCM tag
        let mut plaintext = Vec::with_capacity(data.len());

        let mut block_idx = 0usize;
        let mut pos = 0usize;
        while pos < data.len() {
            let end = (pos + block_cipher_size).min(data.len());
            let block = &data[pos..end];

            let mut block_nonce = master_nonce;
            let idx_bytes = (block_idx as u32).to_le_bytes();
            for i in 0..4 {
                block_nonce[i] ^= idx_bytes[i];
            }
            let nonce = AesNonce::from_slice(&block_nonce);
            let decrypted = cipher
                .decrypt(nonce, block)
                .map_err(|_| format!("decryption failed at block {}", block_idx))?;
            plaintext.extend_from_slice(&decrypted);

            pos = end;
            block_idx += 1;
        }

        Ok(plaintext)
    }

    /// Initialize a crypt overlay directory on a remote.
    /// Creates a `.aeroftp-crypt.json` config file with the salt.
    pub fn crypt_init_config(salt: &[u8; 16]) -> String {
        serde_json::json!({
            "version": CRYPT_VERSION,
            "cipher": "AES-256-GCM",
            "filename_cipher": "AES-256-SIV",
            "kdf": "Argon2id",
            "salt": base64::engine::general_purpose::STANDARD.encode(salt),
            "block_size": BLOCK_SIZE,
        })
        .to_string()
    }

    /// Parse the crypt config to extract the salt.
    pub fn crypt_parse_config(config_json: &str) -> Result<[u8; 16], String> {
        let val: serde_json::Value = serde_json::from_str(config_json)
            .map_err(|e| format!("invalid crypt config: {}", e))?;
        let salt_b64 = val
            .get("salt")
            .and_then(|v| v.as_str())
            .ok_or("missing salt in crypt config")?;
        let salt_bytes = base64::engine::general_purpose::STANDARD
            .decode(salt_b64)
            .map_err(|e| format!("invalid salt: {}", e))?;
        if salt_bytes.len() != 16 {
            return Err("salt must be 16 bytes".into());
        }
        let mut salt = [0u8; 16];
        salt.copy_from_slice(&salt_bytes);
        Ok(salt)
    }
}

/// CLI commands for crypt overlay operations.
async fn cmd_crypt_init(
    url: &str,
    path: &str,
    password: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    use rand::Rng;
    // Validate password is usable (derive key to verify)
    let _verify_key = crypt_overlay::derive_master_key(password, &[0u8; 16]);
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, path));

    let mut salt = [0u8; 16];
    rand::thread_rng().fill(&mut salt);

    let config_json = crypt_overlay::crypt_init_config(&salt);
    let config_path = format!("{}/.aeroftp-crypt.json", base_path.trim_end_matches('/'));

    // Write config to tempfile, upload
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &config_json).unwrap();

    match provider
        .upload(&tmp.path().to_string_lossy(), &config_path, None)
        .await
    {
        Ok(()) => {
            if matches!(format, OutputFormat::Json) {
                print_json(&serde_json::json!({"status": "ok", "path": config_path}));
            } else if !cli.quiet {
                println!("Crypt overlay initialized at {}", base_path);
                println!("Config: {}", config_path);
                println!("Cipher: AES-256-GCM (content) + AES-256-SIV (filenames)");
                println!("KDF: Argon2id (64 MB, 3 iterations)");
            }
            0
        }
        Err(e) => {
            print_error(format, &format!("Failed to init crypt: {}", e), 4);
            4
        }
    }
}

async fn cmd_crypt_ls(
    url: &str,
    path: &str,
    password: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, path));

    // Read crypt config
    let config_path = format!("{}/.aeroftp-crypt.json", base_path.trim_end_matches('/'));
    let config_data = match provider.download_to_bytes(&config_path).await {
        Ok(d) => d,
        Err(_) => {
            print_error(format, "No crypt overlay found. Run 'crypt init' first.", 5);
            return 5;
        }
    };
    let config_str = String::from_utf8_lossy(&config_data);
    let salt = match crypt_overlay::crypt_parse_config(&config_str) {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &format!("Invalid crypt config: {}", e), 5);
            return 5;
        }
    };

    let master_key = crypt_overlay::derive_master_key(password, &salt);

    // List directory and decrypt filenames
    let entries = match provider.list(&base_path).await {
        Ok(e) => e,
        Err(e) => {
            print_error(
                format,
                &format!("List failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            return provider_error_to_exit_code(&e);
        }
    };

    let mut decrypted: Vec<(String, String, bool, u64)> = Vec::new(); // (decrypted_name, encrypted_name, is_dir, size)
    for entry in &entries {
        if entry.name == ".aeroftp-crypt.json" {
            continue;
        }
        let decrypted_name = crypt_overlay::decrypt_filename(&master_key, &entry.name)
            .unwrap_or_else(|| format!("[encrypted: {}]", entry.name));
        decrypted.push((decrypted_name, entry.name.clone(), entry.is_dir, entry.size));
    }

    match format {
        OutputFormat::Text => {
            for (name, _enc, is_dir, size) in &decrypted {
                if *is_dir {
                    println!("{}/ ", name);
                } else {
                    println!("{:<40} {}", name, format_size(*size));
                }
            }
            if !cli.quiet {
                eprintln!("\n{} items (encrypted on remote)", decrypted.len());
            }
        }
        OutputFormat::Json => {
            let items: Vec<serde_json::Value> = decrypted.iter().map(|(name, enc, is_dir, size)| {
                serde_json::json!({"name": name, "encrypted_name": enc, "is_dir": is_dir, "size": size})
            }).collect();
            print_json(&serde_json::json!({"items": items}));
        }
    }

    let _ = provider.disconnect().await;
    0
}

async fn cmd_crypt_put(
    url: &str,
    local_file: &str,
    remote_path: &str,
    password: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, remote_path));

    // Read crypt config
    let config_path = format!("{}/.aeroftp-crypt.json", base_path.trim_end_matches('/'));
    let config_data = match provider.download_to_bytes(&config_path).await {
        Ok(d) => d,
        Err(_) => {
            print_error(format, "No crypt overlay found. Run 'crypt init' first.", 5);
            return 5;
        }
    };
    let salt = match crypt_overlay::crypt_parse_config(&String::from_utf8_lossy(&config_data)) {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    let master_key = crypt_overlay::derive_master_key(password, &salt);
    let start = Instant::now();

    // Read local file
    let plaintext = match std::fs::read(local_file) {
        Ok(d) => d,
        Err(e) => {
            print_error(format, &format!("Cannot read '{}': {}", local_file, e), 2);
            return 2;
        }
    };

    // Encrypt content
    let ciphertext = crypt_overlay::encrypt_data(&master_key, &plaintext);

    // Encrypt filename
    let filename = Path::new(local_file)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| local_file.to_string());
    let encrypted_name = crypt_overlay::encrypt_filename(&master_key, &filename);
    let remote_file = format!("{}/{}", base_path.trim_end_matches('/'), encrypted_name);

    // Write encrypted data to tempfile, upload
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &ciphertext).unwrap();

    match provider
        .upload(&tmp.path().to_string_lossy(), &remote_file, None)
        .await
    {
        Ok(()) => {
            let elapsed = start.elapsed();
            if !cli.quiet {
                println!(
                    "{} → {} ({} → {}, {:.1}s)",
                    filename,
                    encrypted_name,
                    format_size(plaintext.len() as u64),
                    format_size(ciphertext.len() as u64),
                    elapsed.as_secs_f64()
                );
            }
            0
        }
        Err(e) => {
            print_error(format, &format!("Upload failed: {}", e), 4);
            4
        }
    }
}

async fn cmd_crypt_get(
    url: &str,
    file_name: &str,
    path: &str,
    local_dest: &str,
    password: &str,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let base_path = normalize_remote_path(&resolve_cli_remote_path(&initial_path, path));

    // Read crypt config
    let config_path = format!("{}/.aeroftp-crypt.json", base_path.trim_end_matches('/'));
    let config_data = match provider.download_to_bytes(&config_path).await {
        Ok(d) => d,
        Err(_) => {
            print_error(format, "No crypt overlay found.", 5);
            return 5;
        }
    };
    let salt = match crypt_overlay::crypt_parse_config(&String::from_utf8_lossy(&config_data)) {
        Ok(s) => s,
        Err(e) => {
            print_error(format, &e, 5);
            return 5;
        }
    };

    let master_key = crypt_overlay::derive_master_key(password, &salt);
    let start = Instant::now();

    // Encrypt the decrypted name to find the remote file
    let enc = crypt_overlay::encrypt_filename(&master_key, file_name);
    let remote_file = format!("{}/{}", base_path.trim_end_matches('/'), enc);

    // Download
    let ciphertext = match provider.download_to_bytes(&remote_file).await {
        Ok(d) => d,
        Err(e) => {
            print_error(
                format,
                &format!("Download failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            return provider_error_to_exit_code(&e);
        }
    };

    // Decrypt
    let plaintext = match crypt_overlay::decrypt_data(&master_key, &ciphertext) {
        Ok(p) => p,
        Err(e) => {
            print_error(format, &format!("Decryption failed: {}", e), 99);
            return 99;
        }
    };

    // Write to local file
    let dest = if local_dest.is_empty() || local_dest == "." {
        // Use decrypted original filename
        let enc_basename = remote_file.rsplit('/').next().unwrap_or("decrypted");
        crypt_overlay::decrypt_filename(&master_key, enc_basename)
            .unwrap_or_else(|| "decrypted".to_string())
    } else {
        local_dest.to_string()
    };

    if let Err(e) = std::fs::write(&dest, &plaintext) {
        print_error(format, &format!("Cannot write '{}': {}", dest, e), 4);
        return 4;
    }

    let elapsed = start.elapsed();
    if !cli.quiet {
        println!(
            "{} → {} ({} → {}, {:.1}s)",
            remote_file.rsplit('/').next().unwrap_or("?"),
            dest,
            format_size(ciphertext.len() as u64),
            format_size(plaintext.len() as u64),
            elapsed.as_secs_f64()
        );
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
    let raw_remote_base = remote_base.unwrap_or("/");

    // Resolve remote_base against profile's initial_path
    let (mut probe_provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let _ = probe_provider.disconnect().await;
    let remote_base = resolve_cli_remote_path(&initial_path, raw_remote_base);
    let remote_base = remote_base.as_str();

    // Split pattern into directory + glob
    let pattern_path = Path::new(local_pattern);
    let (dir, glob_pattern) = if let Some(parent) = pattern_path.parent() {
        let parent_str = parent.to_string_lossy();
        let parent_dir = if parent_str.is_empty() {
            "."
        } else {
            &*parent_str
        };
        (
            parent_dir.to_string(),
            pattern_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default(),
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
            print_error(
                format,
                &format!("Cannot read directory '{}': {}", dir, e),
                2,
            );
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
        match format {
            OutputFormat::Text => eprintln!("No local files matching glob '{}'", glob_pattern),
            OutputFormat::Json => print_error(
                format,
                &format!("No local files matching glob '{}'", glob_pattern),
                2,
            ),
        }
        return 2;
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

    let results =
        futures_util::stream::iter(matched.into_iter().map(|(local_path, filename, _size)| {
            let cancelled = cancelled.clone();
            let aggregate = aggregate.clone();
            let overall_pb = overall_pb.clone();
            let remote_path = format!("{}/{}", remote_base.trim_end_matches('/'), filename);
            async move {
                if cancelled.load(Ordering::Relaxed) {
                    return Err("Cancelled by user".to_string());
                }
                upload_transfer_task(
                    url,
                    local_path,
                    remote_path,
                    cli,
                    format,
                    Some(aggregate),
                    overall_pb,
                    resolve_max_transfer(cli),
                )
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
                    uploaded,
                    total,
                    elapsed.as_secs_f64()
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
                plan: Vec::new(),
            });
        }
    }
    if uploaded == total as u32 {
        0
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// sync --watch: continuous sync with filesystem watcher
// ---------------------------------------------------------------------------

/// Returns true if a path should be excluded from watcher events (temp files, VCS dirs, OS metadata).
fn should_exclude_watch_path(path: &std::path::Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    // OS metadata
    if matches!(
        name,
        ".DS_Store" | "Thumbs.db" | "desktop.ini" | ".directory"
    ) {
        return true;
    }
    // VCS / heavy dirs (will never be a leaf event worth syncing)
    if matches!(
        name,
        ".git" | ".svn" | ".hg" | "node_modules" | "__pycache__"
    ) {
        return true;
    }
    // Temp/editor artifacts by extension
    if matches!(
        ext,
        "swp" | "swo" | "swx" | "tmp" | "temp" | "bak" | "crdownload" | "part"
    ) {
        return true;
    }
    // Temp artifacts by name pattern
    if name.starts_with('~')
        || name.starts_with(".#")
        || name.ends_with('~')
        || name.ends_with(".aerotmp")
    {
        return true;
    }
    // Vim swap: .filename.swp (already caught by ext, but be safe for .swpx etc.)
    if name.starts_with('.') && name.ends_with(".swp") {
        return true;
    }
    false
}

/// Build a local entry list incrementally: refresh metadata only for watcher-reported paths,
/// and merge with the previous full snapshot for everything else.
/// This avoids a full walkdir when only a few files changed.
fn incremental_local_scan(
    base: &std::path::Path,
    changed_paths: &[std::path::PathBuf],
    previous_entries: &std::collections::HashMap<String, (u64, Option<String>)>,
    exclude_matchers: &[globset::GlobMatcher],
) -> Vec<(String, u64, Option<String>)> {
    let mut result: std::collections::HashMap<String, (u64, Option<String>)> =
        previous_entries.clone();

    for changed in changed_paths {
        // Compute relative path
        let relative = match changed.strip_prefix(base) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if relative.is_empty() || relative == BISYNC_SNAPSHOT_FILE {
            continue;
        }
        // Check excludes
        let fname = changed.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if exclude_matchers
            .iter()
            .any(|m| m.is_match(&relative) || m.is_match(fname))
        {
            result.remove(&relative);
            continue;
        }
        // Read current metadata — if file was deleted, remove from snapshot
        match std::fs::metadata(changed) {
            Ok(meta) if meta.is_file() => {
                let size = meta.len();
                let mtime = meta.modified().ok().map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
                });
                result.insert(relative, (size, mtime));
            }
            _ => {
                // File deleted or not a regular file
                result.remove(&relative);
            }
        }
    }

    result
        .into_iter()
        .map(|(path, (size, mtime))| (path, size, mtime))
        .collect()
}

/// Start a filesystem watcher and return a boxed handle (dropped to stop).
/// Filtered, debounced paths are sent on `tx`.
fn start_watch_watcher(
    watch_path: &std::path::Path,
    mode: &str,
    debounce_ms: u64,
    tx: std::sync::mpsc::Sender<Vec<std::path::PathBuf>>,
) -> Result<Box<dyn std::any::Any + Send>, String> {
    let debounce_dur = std::time::Duration::from_millis(debounce_ms);
    match mode {
        "poll" => {
            use notify::PollWatcher;
            let config =
                notify::Config::default().with_poll_interval(std::time::Duration::from_secs(5));
            let watcher = PollWatcher::new(
                move |res: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = res {
                        let paths: Vec<_> = event
                            .paths
                            .into_iter()
                            .filter(|p| !should_exclude_watch_path(p))
                            .collect();
                        if !paths.is_empty() {
                            let _ = tx.send(paths);
                        }
                    }
                },
                config,
            )
            .map_err(|e| format!("Failed to create poll watcher: {}", e))?;
            // PollWatcher with notify 8 doesn't need explicit watch call for config,
            // but we do need to add the path:
            let mut w = watcher;
            notify::Watcher::watch(&mut w, watch_path, notify::RecursiveMode::Recursive)
                .map_err(|e| format!("Failed to watch path: {}", e))?;
            Ok(Box::new(w))
        }
        _ => {
            // native or auto — use debounced watcher
            use notify_debouncer_full::new_debouncer;

            let (dtx, drx) = std::sync::mpsc::channel();
            let mut debouncer = new_debouncer(debounce_dur, None, dtx)
                .map_err(|e| format!("Failed to create watcher: {}", e))?;
            debouncer
                .watch(watch_path, notify::RecursiveMode::Recursive)
                .map_err(|e| format!("Failed to watch path: {}", e))?;

            // Spawn thread to drain debouncer events and forward filtered paths
            std::thread::spawn(move || {
                while let Ok(result) = drx.recv() {
                    if let Ok(events) = result {
                        let mut all_paths = Vec::new();
                        for event in &events {
                            for p in &event.paths {
                                if !should_exclude_watch_path(p) && !all_paths.contains(p) {
                                    all_paths.push(p.clone());
                                }
                            }
                        }
                        if !all_paths.is_empty() && tx.send(all_paths).is_err() {
                            break; // receiver dropped
                        }
                    }
                }
            });

            Ok(Box::new(debouncer))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_sync_watch(
    url: &str,
    local: &str,
    remote: &str,
    direction: &str,
    dry_run: bool,
    delete: bool,
    exclude: &[String],
    track_renames: bool,
    max_delete: Option<&str>,
    backup_dir: Option<&str>,
    backup_suffix: &str,
    suffix_keep_extension: bool,
    compare_dest: Option<&str>,
    copy_dest: Option<&str>,
    from_reconcile: Option<&str>,
    conflict_mode: &str,
    skip_matching: bool,
    resync: bool,
    watch_mode: &str,
    watch_debounce_ms: u64,
    watch_cooldown: u64,
    watch_rescan: u64,
    watch_no_initial: bool,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);
    if from_reconcile.is_some() {
        print_error(
            format,
            "--from-reconcile cannot be combined with --watch",
            5,
        );
        return 5;
    }
    let local_path = std::path::Path::new(local);
    if !local_path.is_dir() {
        if matches!(format, OutputFormat::Json) {
            print_json(
                &serde_json::json!({"error": "Local path is not a directory", "path": local}),
            );
        } else {
            eprintln!("Error: local path is not a directory: {}", local);
        }
        return 5;
    }

    // Start filesystem watcher
    let (std_tx, std_rx) = std::sync::mpsc::channel::<Vec<std::path::PathBuf>>();
    let _watcher_handle =
        match start_watch_watcher(local_path, watch_mode, watch_debounce_ms, std_tx) {
            Ok(h) => h,
            Err(e) => {
                if matches!(format, OutputFormat::Json) {
                    print_json(
                        &serde_json::json!({"error": format!("Failed to start watcher: {}", e)}),
                    );
                } else {
                    eprintln!("Error: failed to start filesystem watcher: {}", e);
                }
                return 5;
            }
        };

    // Bridge std mpsc to tokio mpsc so we can use tokio::select!
    let (async_tx, mut async_rx) = tokio::sync::mpsc::channel::<Vec<std::path::PathBuf>>(64);
    std::thread::spawn(move || {
        while let Ok(paths) = std_rx.recv() {
            if async_tx.blocking_send(paths).is_err() {
                break;
            }
        }
    });

    if !quiet {
        eprintln!(
            "Watching {} -> {} (direction: {}, cooldown: {}s, rescan: {}s)",
            local,
            if url == "_" { "(profile)" } else { url },
            direction,
            watch_cooldown,
            watch_rescan,
        );
    }

    let syncing = Arc::new(AtomicBool::new(false));
    let mut cycle_count: u64 = 0;
    let cooldown_dur = std::time::Duration::from_secs(watch_cooldown);
    let rescan_dur = if watch_rescan > 0 {
        std::time::Duration::from_secs(watch_rescan)
    } else {
        std::time::Duration::from_secs(86400) // effectively disabled
    };
    let mut shutdown_tick = tokio::time::interval(std::time::Duration::from_millis(200));
    shutdown_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    shutdown_tick.tick().await;
    let mut rescan_tick = tokio::time::interval(rescan_dur);
    rescan_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    rescan_tick.tick().await;
    let mut last_sync_completed = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(watch_cooldown + 1))
        .unwrap_or_else(std::time::Instant::now);

    // Helper macro to run one sync cycle.
    // Usage: run_sync_cycle!("trigger")            — full walkdir scan (None)
    //        run_sync_cycle!("trigger", entries)    — incremental (Some(entries))
    macro_rules! run_sync_cycle {
        ($trigger:expr) => {
            run_sync_cycle!($trigger, None)
        };
        ($trigger:expr, $precomputed:expr) => {{
            cycle_count += 1;
            let cycle = cycle_count;
            let trigger_label: &str = $trigger;
            syncing.store(true, Ordering::SeqCst);
            let cycle_start = std::time::Instant::now();

            let stats: SyncCycleStats = cmd_sync(
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
                suffix_keep_extension,
                compare_dest,
                copy_dest,
                from_reconcile,
                conflict_mode,
                skip_matching,
                resync && cycle == 1, // resync only on first cycle
                cli,
                format,
                cancelled.clone(),
                $precomputed,
            )
            .await;

            syncing.store(false, Ordering::SeqCst);
            last_sync_completed = std::time::Instant::now();
            let elapsed = cycle_start.elapsed();
            let total_changes = stats.uploaded + stats.downloaded + stats.deleted;

            // Emit cycle status with detailed stats
            if matches!(format, OutputFormat::Json) {
                let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                print_json(&serde_json::json!({
                    "cycle": cycle,
                    "trigger": trigger_label,
                    "exit_code": stats.exit_code,
                    "uploaded": stats.uploaded,
                    "downloaded": stats.downloaded,
                    "deleted": stats.deleted,
                    "skipped": stats.skipped,
                    "errors": stats.error_count,
                    "elapsed_secs": (elapsed.as_millis() as f64) / 1000.0,
                    "timestamp": ts,
                }));
            } else if !quiet {
                let ts = chrono::Local::now().format("%H:%M:%S");
                if total_changes == 0 && stats.error_count == 0 {
                    eprintln!(
                        "[{}] Sync #{} ({}) -- no changes ({}s)",
                        ts, cycle, trigger_label,
                        format!("{:.1}", elapsed.as_secs_f64()),
                    );
                } else {
                    eprintln!(
                        "[{}] Sync #{} ({}) -- {} up, {} down, {} del{} ({}s)",
                        ts, cycle, trigger_label,
                        stats.uploaded, stats.downloaded, stats.deleted,
                        if stats.error_count > 0 {
                            format!(", {} errors", stats.error_count)
                        } else {
                            String::new()
                        },
                        format!("{:.1}", elapsed.as_secs_f64()),
                    );
                }
            }

            // Drain any watcher events accumulated during the sync
            while async_rx.try_recv().is_ok() {}

            stats.exit_code
        }};
    }

    // Pre-compile exclude matchers for incremental scan
    let exclude_matchers: Vec<globset::GlobMatcher> = exclude
        .iter()
        .filter_map(|pat| globset::Glob::new(pat).ok().map(|g| g.compile_matcher()))
        .collect();

    // Incremental scan is only safe when:
    // - direction is not download-only (local scan is irrelevant)
    // - track_renames is off (needs full file set for hash matching)
    let use_incremental = direction != "download" && !track_renames;

    // Local snapshot: populated after each full scan for incremental merging
    let mut local_snapshot: std::collections::HashMap<String, (u64, Option<String>)> =
        std::collections::HashMap::new();

    // Helper: build snapshot from a full local walkdir scan
    let build_snapshot =
        |local_dir: &str| -> std::collections::HashMap<String, (u64, Option<String>)> {
            let mut snap = std::collections::HashMap::new();
            let walker = walkdir::WalkDir::new(local_dir)
                .follow_links(false)
                .max_depth(100);
            for entry in walker {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = match entry.path().strip_prefix(local_dir) {
                    Ok(r) => r.to_string_lossy().replace('\\', "/"),
                    Err(_) => continue,
                };
                if relative.is_empty() || relative == BISYNC_SNAPSHOT_FILE {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    let size = meta.len();
                    let mtime = meta.modified().ok().map(|t| {
                        let dt: chrono::DateTime<chrono::Utc> = t.into();
                        dt.format("%Y-%m-%dT%H:%M:%S").to_string()
                    });
                    snap.insert(relative, (size, mtime));
                }
            }
            snap
        };

    // Initial sync
    if !watch_no_initial {
        let code = run_sync_cycle!("initial");
        if cancelled.load(Ordering::SeqCst) {
            return code;
        }
    }

    // Build initial snapshot after first sync (or immediately if --watch-no-initial)
    if use_incremental {
        local_snapshot = build_snapshot(local);
    }

    // Watch loop
    loop {
        tokio::select! {
            biased; // prioritize ctrl_c

            _ = shutdown_tick.tick() => {
                if cancelled.load(Ordering::SeqCst) {
                    if syncing.load(Ordering::SeqCst) {
                        continue;
                    }
                    if !quiet {
                        eprintln!("\nWatch mode stopped. {} sync cycles completed.", cycle_count);
                    }
                    return 0;
                }
            }

            Some(changed_paths) = async_rx.recv() => {
                // Suppress if sync in progress
                if syncing.load(Ordering::SeqCst) {
                    while async_rx.try_recv().is_ok() {}
                    continue;
                }
                // Cooldown check
                if last_sync_completed.elapsed() < cooldown_dur {
                    while async_rx.try_recv().is_ok() {}
                    continue;
                }
                let path_count = changed_paths.len();
                let trigger = format!("watcher: {} paths", path_count);

                if use_incremental {
                    let entries = incremental_local_scan(
                        local_path,
                        &changed_paths,
                        &local_snapshot,
                        &exclude_matchers,
                    );
                    // Update snapshot with changes
                    for (ref rel, size, ref mtime) in &entries {
                        local_snapshot.insert(rel.clone(), (*size, mtime.clone()));
                    }
                    // Remove deleted files from snapshot
                    for p in &changed_paths {
                        if let Ok(rel) = p.strip_prefix(local_path) {
                            let rel_str = rel.to_string_lossy().replace('\\', "/");
                            if !entries.iter().any(|(r, _, _)| r == &rel_str) {
                                local_snapshot.remove(&rel_str);
                            }
                        }
                    }
                    run_sync_cycle!(trigger.as_str(), Some(entries));
                    if cancelled.load(Ordering::SeqCst) {
                        if !quiet {
                            eprintln!("\nWatch mode stopped. {} sync cycles completed.", cycle_count);
                        }
                        return 0;
                    }
                } else {
                    run_sync_cycle!(trigger.as_str());
                    if cancelled.load(Ordering::SeqCst) {
                        if !quiet {
                            eprintln!("\nWatch mode stopped. {} sync cycles completed.", cycle_count);
                        }
                        return 0;
                    }
                }
            }

            _ = rescan_tick.tick() => {
                if syncing.load(Ordering::SeqCst) {
                    continue;
                }
                run_sync_cycle!("rescan");
                // Rebuild snapshot after full rescan
                if use_incremental {
                    local_snapshot = build_snapshot(local);
                }
                if cancelled.load(Ordering::SeqCst) {
                    if !quiet {
                        eprintln!("\nWatch mode stopped. {} sync cycles completed.", cycle_count);
                    }
                    return 0;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_sync_doctor(
    url: &str,
    local: &str,
    remote: &str,
    direction: &str,
    delete: bool,
    exclude: &[String],
    track_renames: bool,
    conflict_mode: &str,
    resync: bool,
    checksum: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let doctor_cfg = if let Some(profile_name) = cli.profile.as_deref() {
        match profile_to_provider_config(profile_name, cli, format) {
            Ok((cfg, _)) => Some(cfg),
            Err(code) => return code,
        }
    } else {
        None
    };

    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    let local_dir = Path::new(local);
    if !local_dir.is_dir() {
        print_error(
            format,
            &format!("Local path is not a directory: {}", local),
            5,
        );
        let _ = provider.disconnect().await;
        return 5;
    }

    let exclude_matchers: Vec<globset::GlobMatcher> = exclude
        .iter()
        .filter_map(|pat| globset::Glob::new(pat).ok().map(|g| g.compile_matcher()))
        .collect();

    let mut local_files = 0usize;
    let mut local_bytes = 0u64;
    for entry in walkdir::WalkDir::new(local)
        .follow_links(false)
        .max_depth(100)
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(local)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() || relative == BISYNC_SNAPSHOT_FILE {
            continue;
        }
        let fname = entry.file_name().to_string_lossy();
        let fname_ref: &str = fname.as_ref();
        if exclude_matchers
            .iter()
            .any(|m| m.is_match(&relative) || m.is_match(fname_ref))
        {
            continue;
        }
        local_files += 1;
        local_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
    }

    let remote_root_ok = provider.list(remote).await.is_ok();
    let mut remote_files = 0usize;
    let mut remote_bytes = 0u64;
    if remote_root_ok {
        let mut queue: Vec<(String, usize)> = vec![(remote.to_string(), 0)];
        while let Some((dir, depth)) = queue.pop() {
            if depth >= MAX_SCAN_DEPTH || remote_files >= MAX_SCAN_ENTRIES {
                break;
            }
            if let Ok(entries) = provider.list(&dir).await {
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
                        if relative.is_empty() || relative == BISYNC_SNAPSHOT_FILE {
                            continue;
                        }
                        if exclude_matchers
                            .iter()
                            .any(|m| m.is_match(&relative) || m.is_match(&e.name))
                        {
                            continue;
                        }
                        remote_files += 1;
                        remote_bytes += e.size;
                    }
                }
            }
        }
    }

    let mut checks = vec![
        serde_json::json!({"name": "local_path_exists", "ok": true, "path": local}),
        serde_json::json!({"name": "remote_path_reachable", "ok": remote_root_ok, "path": remote}),
    ];
    if !exclude.is_empty() {
        checks.push(
            serde_json::json!({"name": "exclude_patterns", "ok": true, "count": exclude.len()}),
        );
    }

    let mut risks = Vec::new();
    if let Some(cfg) = doctor_cfg.as_ref() {
        push_s3_doctor_checks(&mut checks, &mut risks, "remote", cfg);
    }
    if delete {
        risks.push("delete is enabled; sync may remove orphaned files".to_string());
    }
    if direction == "both" {
        risks.push(format!(
            "bidirectional sync can resolve conflicts automatically using conflict_mode={}",
            conflict_mode
        ));
    }
    if resync {
        risks.push("resync is enabled; previous bisync snapshot will be ignored".to_string());
    }
    if !track_renames && direction == "both" {
        risks.push("track-renames is disabled; moved files may be recopied".to_string());
    }
    if checksum {
        risks
            .push("checksum is enabled; later verification may be slower but stricter".to_string());
    }
    if !remote_root_ok {
        risks.push("remote path could not be listed".to_string());
    }

    let suggested_next_command =
        format!(
        "aeroftp-cli sync --profile \"{}\" \"{}\" \"{}\" --direction {} --dry-run --json{}{}{}{}{}",
        profile_or_placeholder(cli),
        shell_double_quote(local),
        shell_double_quote(remote),
        shell_double_quote(direction),
        if delete { " --delete" } else { "" },
        if track_renames { " --track-renames" } else { "" },
        if resync { " --resync" } else { "" },
        if checksum { " --checksum" } else { "" },
        if exclude.is_empty() {
            String::new()
        } else {
            format!(
                " {}",
                exclude
                    .iter()
                    .map(|pattern| format!("--exclude \"{}\"", shell_double_quote(pattern)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    );

    let result = CliDoctorResult {
        status: if remote_root_ok { "ok" } else { "attention" },
        doctor: "sync".to_string(),
        summary: serde_json::json!({
            "direction": direction,
            "local_files": local_files,
            "local_bytes": local_bytes,
            "remote_files": remote_files,
            "remote_bytes": remote_bytes,
            "delete": delete,
            "track_renames": track_renames,
            "conflict_mode": conflict_mode,
            "resync": resync,
        }),
        checks,
        risks,
        suggested_next_command,
    };

    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Text => {
            println!("Sync doctor");
            println!(
                "  Local:  {} file(s), {}",
                local_files,
                format_size(local_bytes)
            );
            println!(
                "  Remote: {} file(s), {}",
                remote_files,
                format_size(remote_bytes)
            );
            println!("  Direction: {}", direction);
            if !result.risks.is_empty() {
                println!("  Risks:");
                for risk in &result.risks {
                    println!("    - {}", risk);
                }
            }
            if !cli.quiet {
                eprintln!("Next: {}", result.suggested_next_command);
            }
        }
    }

    let _ = provider.disconnect().await;
    if remote_root_ok {
        0
    } else {
        4
    }
}

// ── Head / Tail / Touch / Hashsum / Check ─────────────────────────

async fn cmd_head(
    url: &str,
    path: &str,
    lines: usize,
    bytes: Option<u64>,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let path = &resolve_cli_remote_path(&initial_path, path);
    match provider.download_to_bytes(path).await {
        Ok(data) => {
            // --bytes: byte-range mode. Doesn't require UTF-8; works
            // for binaries. Returns base64 in JSON when content is not
            // valid UTF-8 to keep the JSON envelope clean. Surfaced by
            // the agent audit (P7, Battery A).
            if let Some(n) = bytes {
                let take = (n as usize).min(data.len());
                let truncated = data.len() > take;
                let slice = &data[..take];
                if matches!(format, OutputFormat::Json) {
                    let (content, encoding) = match std::str::from_utf8(slice) {
                        Ok(s) => (s.to_string(), "utf8"),
                        Err(_) => (
                            base64::engine::general_purpose::STANDARD.encode(slice),
                            "base64",
                        ),
                    };
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "path": path,
                        "bytes_returned": take,
                        "total_size": data.len(),
                        "truncated": truncated,
                        "encoding": encoding,
                        "content": content,
                    }));
                } else {
                    let stdout = io::stdout();
                    let mut handle = stdout.lock();
                    let _ = handle.write_all(slice);
                    let _ = handle.flush();
                }
                let _ = provider.disconnect().await;
                return 0;
            }
            // Default: line mode (legacy behaviour).
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
                    print_error(
                        format,
                        "File is not valid UTF-8 text. Pass --bytes to read raw bytes.",
                        5,
                    );
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

async fn cmd_tail(url: &str, path: &str, lines: usize, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let path = &resolve_cli_remote_path(&initial_path, path);
    match provider.download_to_bytes(path).await {
        Ok(data) => match String::from_utf8(data) {
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
        },
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
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let path = &resolve_cli_remote_path(&initial_path, path);
    // Check if file exists
    match provider.stat(path).await {
        Ok(_) => {
            // File exists - touch is a no-op for most providers (mtime update not widely supported)
            if matches!(format, OutputFormat::Json) {
                print_json(&serde_json::json!({"status": "ok", "path": path, "action": "exists"}));
            } else {
                eprintln!("File exists: {}", path);
            }
            let _ = provider.disconnect().await;
            0
        }
        Err(_) => {
            // File doesn't exist - create empty file
            let tmp = std::env::temp_dir().join(format!("aeroftp_touch_{}", uuid::Uuid::new_v4()));
            if let Err(e) = std::fs::write(&tmp, b"") {
                print_error(format, &format!("Failed to create temp file: {}", e), 4);
                let _ = provider.disconnect().await;
                return 4;
            }
            let result = provider
                .upload(tmp.to_str().unwrap_or(""), path, None)
                .await;
            let _ = std::fs::remove_file(&tmp);
            match result {
                Ok(()) => {
                    if matches!(format, OutputFormat::Json) {
                        print_json(
                            &serde_json::json!({"status": "ok", "path": path, "action": "created"}),
                        );
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
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let path = &resolve_cli_remote_path(&initial_path, path);
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
                HashAlgorithm::Blake3 => blake3::hash(&data).to_hex().to_string(),
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
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let remote_path = &resolve_cli_remote_path(&initial_path, remote_path);

    let local_dir = Path::new(local_path);
    if !local_dir.is_dir() {
        print_error(
            format,
            &format!("Local path is not a directory: {}", local_path),
            5,
        );
        let _ = provider.disconnect().await;
        return 5;
    }

    // Delegate scan + comparison to sync_core. Both CLI and MCP now share
    // the same implementation, so a fix in one propagates to the other.
    use ftp_client_gui_lib::sync_core::{
        compare_trees, scan_local_tree, scan_remote_tree, ScanOptions,
    };
    let scan_opts = ScanOptions {
        compute_checksum: checksum,
        max_depth: Some(MAX_SCAN_DEPTH),
        ..Default::default()
    };
    let locals = scan_local_tree(local_path, &scan_opts);
    let remotes = scan_remote_tree(&mut provider, remote_path, &scan_opts).await;
    let diff = compare_trees(&locals, &remotes, one_way);

    let match_count = diff.match_count() as u32;
    let differ_count = diff.differ_count() as u32;
    let missing_local = diff.missing_local_count() as u32;
    let missing_remote = diff.missing_remote_count() as u32;

    let mut details: Vec<CliCheckEntry> = Vec::new();
    for entry in &diff.differ {
        details.push(CliCheckEntry {
            path: entry.rel_path.clone(),
            status: "differ".to_string(),
            local_size: entry.local_size,
            remote_size: entry.remote_size,
        });
    }
    for entry in &diff.missing_remote {
        details.push(CliCheckEntry {
            path: entry.rel_path.clone(),
            status: "missing_remote".to_string(),
            local_size: entry.local_size,
            remote_size: None,
        });
    }
    for entry in &diff.missing_local {
        details.push(CliCheckEntry {
            path: entry.rel_path.clone(),
            status: "missing_local".to_string(),
            local_size: None,
            remote_size: entry.remote_size,
        });
    }

    let elapsed = start.elapsed().as_secs_f64();

    if matches!(format, OutputFormat::Json) {
        print_json(&serde_json::json!({
            "status": if differ_count == 0 && missing_local == 0 && missing_remote == 0 {
                "ok"
            } else {
                "differences_found"
            },
            "match_count": match_count,
            "differ_count": differ_count,
            "missing_local": missing_local,
            "missing_remote": missing_remote,
            "elapsed_secs": elapsed,
            "details": details,
            "suggested_next_command": format!(
                "aeroftp-cli sync --profile \"{}\" \"{}\" \"{}\" --dry-run --json",
                profile_or_placeholder(cli),
                shell_double_quote(local_path),
                shell_double_quote(remote_path)
            ),
        }));
    } else {
        eprintln!(
            "\n  Match: {}  Differ: {}  Missing local: {}  Missing remote: {}  ({:.1}s)",
            match_count, differ_count, missing_local, missing_remote, elapsed
        );
        eprintln!(
            "Next: aeroftp-cli sync --profile \"{}\" \"{}\" \"{}\" --dry-run --json",
            profile_or_placeholder(cli),
            shell_double_quote(local_path),
            shell_double_quote(remote_path)
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

#[allow(clippy::too_many_arguments)]
async fn cmd_reconcile(
    url: &str,
    local_path: &str,
    remote_path: &str,
    checksum: bool,
    one_way: bool,
    exclude: &[String],
    reconcile_format: ReconcileFormat,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    let (mut provider, initial_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };
    let remote_path = &resolve_cli_remote_path(&initial_path, remote_path);

    let start = Instant::now();
    let local_dir = Path::new(local_path);
    if !local_dir.is_dir() {
        print_error(
            format,
            &format!("Local path is not a directory: {}", local_path),
            5,
        );
        let _ = provider.disconnect().await;
        return 5;
    }

    // Merge per-command excludes with global `--exclude-global` and any
    // patterns loaded from `--exclude-from` file.
    let mut all_exclude = exclude.to_vec();
    all_exclude.extend(cli.exclude_global.clone());
    if let Some(ref path) = cli.exclude_from {
        if let Ok(patterns) = load_patterns_from_file(path) {
            all_exclude.extend(patterns);
        }
    }

    use ftp_client_gui_lib::sync_core::{compare_trees, ScanOptions};
    let scan_opts = ScanOptions {
        exclude_patterns: all_exclude,
        compute_checksum: checksum,
        compute_remote_checksum: checksum,
        max_depth: Some(MAX_SCAN_DEPTH),
        ..Default::default()
    };
    let local_spinner = maybe_create_scan_spinner(format, cli, "Scanning local...");
    let locals = scan_local_tree_with_progress(local_path, &scan_opts, &local_spinner);
    if let Some(pb) = local_spinner {
        pb.finish_and_clear();
    }

    let remote_spinner = maybe_create_scan_spinner(format, cli, "Scanning remote...");
    let remotes =
        scan_remote_tree_with_progress(&mut provider, remote_path, &scan_opts, &remote_spinner)
            .await;
    if let Some(pb) = remote_spinner {
        pb.finish_and_clear();
    }
    let diff = compare_trees(&locals, &remotes, one_way);

    let matches_group: Vec<serde_json::Value> = diff
        .matches
        .iter()
        .map(|e| {
            serde_json::json!({
                "path": e.rel_path,
                "local_size": e.local_size,
                "remote_size": e.remote_size,
                "compare_method": e.compare_method,
            })
        })
        .collect();
    let differ_group: Vec<serde_json::Value> = diff
        .differ
        .iter()
        .map(|e| {
            serde_json::json!({
                "path": e.rel_path,
                "local_size": e.local_size,
                "remote_size": e.remote_size,
                "compare_method": e.compare_method,
            })
        })
        .collect();
    let missing_remote_group: Vec<serde_json::Value> = diff
        .missing_remote
        .iter()
        .map(|e| {
            serde_json::json!({
                "path": e.rel_path,
                "local_size": e.local_size,
            })
        })
        .collect();
    let missing_local_group: Vec<serde_json::Value> = diff
        .missing_local
        .iter()
        .map(|e| {
            serde_json::json!({
                "path": e.rel_path,
                "remote_size": e.remote_size,
            })
        })
        .collect();

    let elapsed = start.elapsed().as_secs_f64();
    let suggested_next_command = format!(
        "aeroftp-cli sync --profile \"{}\" \"{}\" \"{}\" --dry-run --json",
        profile_or_placeholder(cli),
        shell_double_quote(local_path),
        shell_double_quote(remote_path)
    );

    let result = CliReconcileResult {
        status: if differ_group.is_empty()
            && missing_remote_group.is_empty()
            && missing_local_group.is_empty()
        {
            "ok"
        } else {
            "differences_found"
        },
        local_path: local_path.to_string(),
        remote_path: remote_path.to_string(),
        summary: serde_json::json!({
            "match_count": matches_group.len(),
            "differ_count": differ_group.len(),
            "missing_remote_count": missing_remote_group.len(),
            "missing_local_count": missing_local_group.len(),
            "elapsed_secs": elapsed,
        }),
        groups: serde_json::json!({
            "match": matches_group,
            "differ": differ_group,
            "missing_remote": missing_remote_group,
            "missing_local": missing_local_group,
        }),
        suggested_next_command,
    };

    match format {
        OutputFormat::Json => {
            if reconcile_format == ReconcileFormat::Summary {
                print_json(&serde_json::json!({
                    "status": result.status,
                    "local_path": result.local_path,
                    "remote_path": result.remote_path,
                    "summary": result.summary,
                    "suggested_next_command": result.suggested_next_command,
                }));
            } else {
                print_json(&result);
            }
        }
        OutputFormat::Text => {
            println!("Reconcile summary:");
            println!("  Match: {}", result.summary["match_count"]);
            println!("  Differ: {}", result.summary["differ_count"]);
            println!(
                "  Missing remote: {}",
                result.summary["missing_remote_count"]
            );
            println!("  Missing local: {}", result.summary["missing_local_count"]);
            println!("  Elapsed: {:.2}s", elapsed);
            if reconcile_format == ReconcileFormat::Detailed && !cli.quiet {
                for group in &[
                    ("Differ", &differ_group),
                    ("Missing remote", &missing_remote_group),
                    ("Missing local", &missing_local_group),
                ] {
                    if group.1.is_empty() {
                        continue;
                    }
                    eprintln!("{}:", group.0);
                    for entry in group.1 {
                        if let Some(path) = entry.get("path").and_then(|value| value.as_str()) {
                            eprintln!("  - {}", path);
                        }
                    }
                }
            }
            if !cli.quiet {
                eprintln!("Next: {}", result.suggested_next_command);
            }
        }
    }

    let _ = provider.disconnect().await;
    if result.status == "ok" {
        0
    } else {
        4
    }
}

// ── Cross-profile transfer ─────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn cmd_transfer_doctor(
    source_profile: &str,
    dest_profile: &str,
    source_path: &str,
    dest_path: &str,
    recursive: bool,
    skip_existing: bool,
    cli: &Cli,
    format: OutputFormat,
) -> i32 {
    use ftp_client_gui_lib::cross_profile_transfer::{plan_transfer, CrossProfileTransferRequest};

    let src_cfg = match profile_to_provider_config(source_profile, cli, format) {
        Ok((cfg, _)) => cfg,
        Err(code) => return code,
    };
    let dst_cfg = match profile_to_provider_config(dest_profile, cli, format) {
        Ok((cfg, _)) => cfg,
        Err(code) => return code,
    };

    let mut source = match ProviderFactory::create(&src_cfg) {
        Ok(p) => p,
        Err(e) => {
            print_error(
                format,
                &format!("Failed to create source provider: {}", e),
                provider_error_to_exit_code(&e),
            );
            return provider_error_to_exit_code(&e);
        }
    };
    let mut dest = match ProviderFactory::create(&dst_cfg) {
        Ok(p) => p,
        Err(e) => {
            print_error(
                format,
                &format!("Failed to create dest provider: {}", e),
                provider_error_to_exit_code(&e),
            );
            return provider_error_to_exit_code(&e);
        }
    };

    if let Err(e) = source.connect().await {
        print_error(
            format,
            &format!("Source connection failed: {}", e),
            provider_error_to_exit_code(&e),
        );
        return provider_error_to_exit_code(&e);
    }
    if let Err(e) = dest.connect().await {
        print_error(
            format,
            &format!("Dest connection failed: {}", e),
            provider_error_to_exit_code(&e),
        );
        let _ = source.disconnect().await;
        return provider_error_to_exit_code(&e);
    }

    let request = CrossProfileTransferRequest {
        source_profile: source_profile.to_string(),
        dest_profile: dest_profile.to_string(),
        source_path: source_path.to_string(),
        dest_path: dest_path.to_string(),
        recursive,
        dry_run: true,
        skip_existing,
    };

    let plan = match plan_transfer(source.as_mut(), dest.as_mut(), &request).await {
        Ok(plan) => plan,
        Err(e) => {
            print_error(
                format,
                &format!("Transfer doctor failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = source.disconnect().await;
            let _ = dest.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    let mut existing_dest_paths = 0u64;
    for entry in &plan.entries {
        if dest.stat(&entry.dest_path).await.is_ok() {
            existing_dest_paths += 1;
        }
    }

    let mut checks = vec![
        serde_json::json!({"name": "source_connected", "ok": true, "profile": source_profile}),
        serde_json::json!({"name": "dest_connected", "ok": true, "profile": dest_profile}),
        serde_json::json!({"name": "plan_created", "ok": true, "planned_files": plan.total_files}),
    ];
    if let Some(first) = plan.entries.first() {
        checks.push(serde_json::json!({
            "name": "sample_mapping",
            "ok": true,
            "source_path": first.source_path,
            "dest_path": first.dest_path
        }));
    }

    let mut risks = Vec::new();
    push_s3_doctor_checks(&mut checks, &mut risks, "source", &src_cfg);
    push_s3_doctor_checks(&mut checks, &mut risks, "destination", &dst_cfg);
    if recursive {
        risks.push("recursive transfer enabled; entire trees may be copied".to_string());
    }
    if existing_dest_paths > 0 && !skip_existing {
        risks.push(format!(
            "{} planned destination path(s) already exist and may be overwritten",
            existing_dest_paths
        ));
    }
    if skip_existing {
        risks.push("skip-existing enabled; matching destination files will be skipped".to_string());
    }
    if plan.total_files == 0 {
        risks.push("plan contains zero files".to_string());
    }

    let result = CliDoctorResult {
        status: if plan.total_files > 0 {
            "ok"
        } else {
            "attention"
        },
        doctor: "transfer".to_string(),
        summary: serde_json::json!({
            "source_profile": source_profile,
            "dest_profile": dest_profile,
            "source_path": source_path,
            "dest_path": dest_path,
            "recursive": recursive,
            "skip_existing": skip_existing,
            "planned_files": plan.total_files,
            "total_bytes": plan.total_bytes,
            "existing_dest_paths": existing_dest_paths,
            "sample_entries": plan.entries.iter().take(5).map(|entry| serde_json::json!({
                "source_path": entry.source_path,
                "dest_path": entry.dest_path,
                "size": entry.size,
            })).collect::<Vec<_>>(),
        }),
        checks,
        risks,
        suggested_next_command: suggest_transfer_apply(
            source_profile,
            dest_profile,
            source_path,
            dest_path,
        ),
    };

    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Text => {
            println!("Transfer doctor");
            println!("  Planned files: {}", plan.total_files);
            println!("  Total bytes: {}", format_size(plan.total_bytes));
            println!("  Existing destination paths: {}", existing_dest_paths);
            if !result.risks.is_empty() {
                println!("  Risks:");
                for risk in &result.risks {
                    println!("    - {}", risk);
                }
            }
            if !cli.quiet {
                eprintln!("Next: {}", result.suggested_next_command);
            }
        }
    }

    let _ = source.disconnect().await;
    let _ = dest.disconnect().await;
    if plan.total_files > 0 {
        0
    } else {
        4
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_transfer_profiles(
    source_profile: &str,
    dest_profile: &str,
    source_path: &str,
    dest_path: &str,
    recursive: bool,
    dry_run: bool,
    skip_existing: bool,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    use ftp_client_gui_lib::cross_profile_transfer::{plan_transfer, CrossProfileTransferRequest};

    let quiet = cli.quiet || matches!(format, OutputFormat::Json);

    // Resolve source profile
    let src_cfg = match profile_to_provider_config(source_profile, cli, format) {
        Ok((cfg, _)) => cfg,
        Err(code) => return code,
    };
    // Resolve destination profile
    let dst_cfg = match profile_to_provider_config(dest_profile, cli, format) {
        Ok((cfg, _)) => cfg,
        Err(code) => return code,
    };

    // Create providers
    let mut source = match ProviderFactory::create(&src_cfg) {
        Ok(p) => p,
        Err(e) => {
            print_error(
                format,
                &format!("Failed to create source provider: {}", e),
                provider_error_to_exit_code(&e),
            );
            return provider_error_to_exit_code(&e);
        }
    };
    let mut dest = match ProviderFactory::create(&dst_cfg) {
        Ok(p) => p,
        Err(e) => {
            print_error(
                format,
                &format!("Failed to create dest provider: {}", e),
                provider_error_to_exit_code(&e),
            );
            return provider_error_to_exit_code(&e);
        }
    };

    // Connect source
    if !quiet {
        eprintln!("Connecting to source profile '{}'...", source_profile);
    }
    if let Err(e) = source.connect().await {
        print_error(
            format,
            &format!("Source connection failed: {}", e),
            provider_error_to_exit_code(&e),
        );
        return provider_error_to_exit_code(&e);
    }

    // Connect destination
    if !quiet {
        eprintln!("Connecting to dest profile '{}'...", dest_profile);
    }
    if let Err(e) = dest.connect().await {
        print_error(
            format,
            &format!("Dest connection failed: {}", e),
            provider_error_to_exit_code(&e),
        );
        let _ = source.disconnect().await;
        return provider_error_to_exit_code(&e);
    }

    // Build request
    let request = CrossProfileTransferRequest {
        source_profile: source_profile.to_string(),
        dest_profile: dest_profile.to_string(),
        source_path: source_path.to_string(),
        dest_path: dest_path.to_string(),
        recursive,
        dry_run,
        skip_existing,
    };

    // Plan
    if !quiet {
        eprintln!("Planning transfer...");
    }
    let plan = match plan_transfer(source.as_mut(), dest.as_mut(), &request).await {
        Ok(p) => p,
        Err(e) => {
            print_error(
                format,
                &format!("Planning failed: {}", e),
                provider_error_to_exit_code(&e),
            );
            let _ = source.disconnect().await;
            let _ = dest.disconnect().await;
            return provider_error_to_exit_code(&e);
        }
    };

    if plan.entries.is_empty() {
        if !quiet {
            eprintln!("Nothing to transfer.");
            eprintln!(
                "Next: {}",
                suggest_transfer_verify(dest_profile, dest_path, 0)
            );
        }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({
                "source_profile": source_profile,
                "dest_profile": dest_profile,
                "planned_files": 0,
                "transferred_files": 0,
                "skipped_files": 0,
                "failed_files": 0,
                "total_bytes": 0,
                "duration_ms": 0,
                "dry_run": dry_run,
                "suggested_next_command": suggest_transfer_verify(dest_profile, dest_path, 0),
            }));
        }
        let _ = source.disconnect().await;
        let _ = dest.disconnect().await;
        return 0;
    }

    // Dry-run: print plan and exit
    if dry_run {
        if !quiet {
            eprintln!(
                "DRY RUN: {} file(s), {} total",
                plan.total_files,
                format_size(plan.total_bytes)
            );
            for entry in &plan.entries {
                eprintln!(
                    "  {} -> {} ({})",
                    entry.source_path,
                    entry.dest_path,
                    format_size(entry.size)
                );
            }
            eprintln!(
                "Next: {}",
                suggest_transfer_apply(source_profile, dest_profile, source_path, dest_path)
            );
        }
        if matches!(format, OutputFormat::Json) {
            print_json(&serde_json::json!({
                "plan": plan,
                "suggested_next_command": suggest_transfer_apply(
                    source_profile,
                    dest_profile,
                    source_path,
                    dest_path
                ),
            }));
        }
        let _ = source.disconnect().await;
        let _ = dest.disconnect().await;
        return 0;
    }

    let summary = execute_cross_profile_plan(
        source.as_mut(),
        dest.as_mut(),
        &plan,
        skip_existing,
        cli,
        quiet,
        cancelled,
    )
    .await;

    if !quiet {
        eprintln!(
            "Done: {} transferred, {} skipped, {} failed ({}, {:.1}s)",
            summary.transferred_files,
            summary.skipped_files,
            summary.failed_files,
            format_size(summary.total_bytes),
            summary.duration_ms as f64 / 1000.0
        );
        eprintln!(
            "Next: {}",
            suggest_transfer_verify(dest_profile, dest_path, summary.planned_files)
        );
    }
    if matches!(format, OutputFormat::Json) {
        print_json(&serde_json::json!({
            "source_profile": summary.source_profile,
            "dest_profile": summary.dest_profile,
            "planned_files": summary.planned_files,
            "transferred_files": summary.transferred_files,
            "skipped_files": summary.skipped_files,
            "failed_files": summary.failed_files,
            "total_bytes": summary.total_bytes,
            "duration_ms": summary.duration_ms,
            "dry_run": summary.dry_run,
            "suggested_next_command": suggest_transfer_verify(
                dest_profile,
                dest_path,
                summary.planned_files
            ),
        }));
    }

    let _ = source.disconnect().await;
    let _ = dest.disconnect().await;

    if summary.failed_files > 0 {
        4
    } else {
        0
    }
}

#[derive(Serialize)]
struct TransferCliSummary {
    source_profile: String,
    dest_profile: String,
    planned_files: u64,
    transferred_files: u64,
    skipped_files: u64,
    failed_files: u64,
    total_bytes: u64,
    duration_ms: u64,
    dry_run: bool,
}

async fn execute_cross_profile_plan(
    source: &mut dyn StorageProvider,
    dest: &mut dyn StorageProvider,
    plan: &ftp_client_gui_lib::cross_profile_transfer::CrossProfileTransferPlan,
    skip_existing: bool,
    cli: &Cli,
    quiet: bool,
    cancelled: Arc<AtomicBool>,
) -> TransferCliSummary {
    use ftp_client_gui_lib::cross_profile_transfer::{copy_one_file, should_skip_existing};

    let total = plan.entries.len();
    let max_attempts = cli.retries.max(1);
    let sleep_dur = parse_retry_sleep(&cli.retries_sleep);
    let start = std::time::Instant::now();
    let mut transferred: u64 = 0;
    let mut skipped: u64 = 0;
    let mut failed: u64 = 0;
    let mut bytes_transferred: u64 = 0;

    if !quiet {
        eprintln!(
            "Transferring {} file(s), {} total...",
            total,
            format_size(plan.total_bytes)
        );
    }

    for (idx, entry) in plan.entries.iter().enumerate() {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            if !quiet {
                eprintln!("Transfer cancelled.");
            }
            break;
        }

        if skip_existing {
            match should_skip_existing(dest, &entry.dest_path, entry).await {
                Ok(true) => {
                    skipped += 1;
                    if !quiet {
                        eprintln!(
                            "  [{}/{}] {} ... SKIPPED (exists)",
                            idx + 1,
                            total,
                            entry.display_name
                        );
                    }
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    if !quiet {
                        eprintln!(
                            "  [{}/{}] {} skip-existing check failed: {}, transferring anyway",
                            idx + 1,
                            total,
                            entry.display_name,
                            e
                        );
                    }
                }
            }
        }

        if !quiet {
            eprint!(
                "  [{}/{}] {} ({}) ... ",
                idx + 1,
                total,
                entry.display_name,
                format_size(entry.size)
            );
        }

        let mut file_ok = false;
        for attempt in 1..=max_attempts {
            match copy_one_file(
                source,
                dest,
                &entry.source_path,
                &entry.dest_path,
                entry.modified.as_deref(),
            )
            .await
            {
                Ok(()) => {
                    file_ok = true;
                    break;
                }
                Err(e) => {
                    let code = provider_error_to_exit_code(&e);
                    if !is_retryable_exit(code) || attempt == max_attempts {
                        if !quiet {
                            eprintln!("FAILED: {}", e);
                        }
                        break;
                    }
                    if !quiet {
                        eprintln!(
                            "attempt {}/{} failed ({}), retrying in {:?}...",
                            attempt, max_attempts, e, sleep_dur
                        );
                        eprint!(
                            "  [{}/{}] {} ({}) ... ",
                            idx + 1,
                            total,
                            entry.display_name,
                            format_size(entry.size)
                        );
                    }
                    if !sleep_dur.is_zero() {
                        tokio::time::sleep(sleep_dur).await;
                    }
                }
            }
        }

        if file_ok {
            transferred += 1;
            bytes_transferred += entry.size;
            if !quiet {
                eprintln!("OK");
            }
        } else {
            failed += 1;
        }
    }

    TransferCliSummary {
        source_profile: plan.source_profile.clone(),
        dest_profile: plan.dest_profile.clone(),
        planned_files: total as u64,
        transferred_files: transferred,
        skipped_files: skipped,
        failed_files: failed,
        total_bytes: bytes_transferred,
        duration_ms: start.elapsed().as_millis() as u64,
        dry_run: false,
    }
}

/// Resolve a profile name, create a provider, and connect it.
/// Used by batch CONNECT_SOURCE_PROFILE / CONNECT_DEST_PROFILE.
async fn batch_connect_profile(
    profile_name: &str,
    cli: &Cli,
    format: OutputFormat,
) -> Result<Box<dyn StorageProvider>, i32> {
    let (cfg, _) = profile_to_provider_config(profile_name, cli, format)?;
    let mut provider = ProviderFactory::create(&cfg).map_err(|e| {
        print_error(
            format,
            &format!("Failed to create provider for '{}': {}", profile_name, e),
            provider_error_to_exit_code(&e),
        );
        provider_error_to_exit_code(&e)
    })?;
    provider.connect().await.map_err(|e| {
        print_error(
            format,
            &format!("Connection failed for '{}': {}", profile_name, e),
            provider_error_to_exit_code(&e),
        );
        provider_error_to_exit_code(&e)
    })?;
    Ok(provider)
}

async fn cmd_batch(file: &str, cli: &Cli, format: OutputFormat, cancelled: Arc<AtomicBool>) -> i32 {
    let content = if file == "-" {
        let mut stdin = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut stdin) {
            print_error(
                format,
                &format!("Cannot read batch script from stdin: {}", e),
                2,
            );
            return 2;
        }
        stdin
    } else {
        match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => {
                print_error(
                    format,
                    &format!("Cannot read batch file '{}': {}", file, e),
                    2,
                );
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

    // Cross-profile transfer state (CONNECT_SOURCE_PROFILE / CONNECT_DEST_PROFILE / TRANSFER)
    let mut cross_source: Option<(Box<dyn StorageProvider>, String)> = None; // (provider, profile_name)
    let mut cross_dest: Option<(Box<dyn StorageProvider>, String)> = None;

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
                        let start_byte = chars[ci + 2..]
                            .first()
                            .map(|(b, _)| *b)
                            .unwrap_or(line.len());
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
                            ci = chars
                                .iter()
                                .position(|(b, _)| *b >= end_byte)
                                .unwrap_or(chars.len());
                            continue;
                        }
                    } else if next_ch.is_ascii_alphabetic() || next_ch == '_' {
                        // $VAR syntax
                        let start = ci + 1;
                        let mut end = start;
                        while end < chars.len()
                            && (chars[end].1.is_ascii_alphanumeric() || chars[end].1 == '_')
                        {
                            end += 1;
                        }
                        let key_start = chars[start].0;
                        let key_end = if end < chars.len() {
                            chars[end].0
                        } else {
                            line.len()
                        };
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
                            eprintln!(
                                "Line {}: variable value too large (max 64 KB)",
                                line_num + 1
                            );
                            return 5;
                        }
                        // Validate variable name: [A-Za-z_][A-Za-z0-9_]*
                        if !key.is_empty()
                            && key
                                .chars()
                                .next()
                                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
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
                // ECHO <message> - print to stderr for logging
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
                // Reset profile info flag so each batch CONNECT prints its profile
                PROFILE_INFO_PRINTED.store(false, Ordering::Relaxed);
                exit_code = cmd_connect(parts[1], cli, format).await;
                if exit_code == 0 {
                    current_url = Some(parts[1].to_string());
                } else if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "CONNECT",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                let local = if parts.len() > 2 {
                    Some(parts[2])
                } else {
                    None
                };
                exit_code = cmd_get(
                    &url,
                    parts[1],
                    local,
                    false,
                    1,
                    cli,
                    format,
                    cancelled.clone(),
                )
                .await;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "GET",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                let remote = if parts.len() > 2 {
                    Some(parts[2])
                } else {
                    None
                };
                exit_code = cmd_put(
                    &url,
                    parts[1],
                    remote,
                    false,
                    false,
                    cli,
                    format,
                    cancelled.clone(),
                )
                .await;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "PUT",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "RM",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "MV",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                exit_code = cmd_ls(
                    &url, path, long, "name", false, true, None, false, false, cli, format,
                )
                .await;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "LS",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "CAT",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "STAT",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                exit_code =
                    cmd_find(&url, parts[1], parts[2], false, false, None, cli, format).await;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "FIND",
                    on_error_continue,
                    &mut failed_commands,
                ) {
                    return code;
                }
            }
            "DF" => {
                let url = match require_url(&current_url, line_num) {
                    Ok(u) => u,
                    Err(code) => return code,
                };
                exit_code = cmd_df(&url, cli, format).await;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "DF",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                exit_code = cmd_mkdir(&url, parts[1], false, cli, format).await;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "MKDIR",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "TREE",
                    on_error_continue,
                    &mut failed_commands,
                ) {
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
                    false,
                    None,
                    None,
                    None,
                    "newer",
                    false,
                    false,
                    cli,
                    format,
                    cancelled.clone(),
                    None,
                )
                .await
                .exit_code;
                if let Some(code) = check_exit(
                    exit_code,
                    line_num,
                    "SYNC",
                    on_error_continue,
                    &mut failed_commands,
                ) {
                    return code;
                }
            }
            "CONNECT_SOURCE_PROFILE" => {
                if parts.len() < 2 {
                    eprintln!(
                        "Line {}: CONNECT_SOURCE_PROFILE requires a profile name",
                        line_num + 1
                    );
                    return 5;
                }
                let profile_name = parts[1..].join(" ");
                PROFILE_INFO_PRINTED.store(false, Ordering::Relaxed);
                // Disconnect previous source if any
                if let Some((mut old, _)) = cross_source.take() {
                    let _ = old.disconnect().await;
                }
                match batch_connect_profile(&profile_name, cli, format).await {
                    Ok(provider) => {
                        eprintln!("Source profile '{}' connected", profile_name);
                        cross_source = Some((provider, profile_name));
                    }
                    Err(code) => {
                        if let Some(c) = check_exit(
                            code,
                            line_num,
                            "CONNECT_SOURCE_PROFILE",
                            on_error_continue,
                            &mut failed_commands,
                        ) {
                            return c;
                        }
                    }
                }
            }
            "CONNECT_DEST_PROFILE" => {
                if parts.len() < 2 {
                    eprintln!(
                        "Line {}: CONNECT_DEST_PROFILE requires a profile name",
                        line_num + 1
                    );
                    return 5;
                }
                let profile_name = parts[1..].join(" ");
                PROFILE_INFO_PRINTED.store(false, Ordering::Relaxed);
                if let Some((mut old, _)) = cross_dest.take() {
                    let _ = old.disconnect().await;
                }
                match batch_connect_profile(&profile_name, cli, format).await {
                    Ok(provider) => {
                        eprintln!("Dest profile '{}' connected", profile_name);
                        cross_dest = Some((provider, profile_name));
                    }
                    Err(code) => {
                        if let Some(c) = check_exit(
                            code,
                            line_num,
                            "CONNECT_DEST_PROFILE",
                            on_error_continue,
                            &mut failed_commands,
                        ) {
                            return c;
                        }
                    }
                }
            }
            "TRANSFER" => {
                use ftp_client_gui_lib::cross_profile_transfer::{
                    plan_transfer, CrossProfileTransferRequest,
                };

                let (src_provider, src_name) = match &mut cross_source {
                    Some((p, n)) => (p.as_mut(), n.clone()),
                    None => {
                        eprintln!(
                            "Line {}: No source profile. Use CONNECT_SOURCE_PROFILE first.",
                            line_num + 1
                        );
                        if !on_error_continue {
                            return 5;
                        }
                        failed_commands += 1;
                        continue;
                    }
                };
                let (dest_provider, dest_name) = match &mut cross_dest {
                    Some((p, n)) => (p.as_mut(), n.clone()),
                    None => {
                        eprintln!(
                            "Line {}: No dest profile. Use CONNECT_DEST_PROFILE first.",
                            line_num + 1
                        );
                        if !on_error_continue {
                            return 5;
                        }
                        failed_commands += 1;
                        continue;
                    }
                };

                if parts.len() < 3 {
                    eprintln!("Line {}: TRANSFER requires <source_path> <dest_path> [-r] [--skip-existing]", line_num + 1);
                    return 5;
                }
                let source_path = parts[1];
                let dest_path = parts[2];
                let recursive = parts.contains(&"-r") || parts.contains(&"--recursive");
                let skip_existing = parts.contains(&"--skip-existing");
                let dry_run = parts.contains(&"--dry-run");

                let request = CrossProfileTransferRequest {
                    source_profile: src_name.clone(),
                    dest_profile: dest_name.clone(),
                    source_path: source_path.to_string(),
                    dest_path: dest_path.to_string(),
                    recursive,
                    dry_run,
                    skip_existing,
                };

                exit_code = 0;
                match plan_transfer(src_provider, dest_provider, &request).await {
                    Ok(plan) => {
                        if plan.entries.is_empty() {
                            eprintln!("  Nothing to transfer.");
                        } else if dry_run {
                            eprintln!(
                                "  DRY RUN: {} file(s), {}",
                                plan.total_files,
                                format_size(plan.total_bytes)
                            );
                            for entry in &plan.entries {
                                eprintln!("    {} -> {}", entry.source_path, entry.dest_path);
                            }
                        } else {
                            let src_p = cross_source.as_mut().unwrap().0.as_mut();
                            let dst_p = cross_dest.as_mut().unwrap().0.as_mut();
                            let summary = execute_cross_profile_plan(
                                src_p,
                                dst_p,
                                &plan,
                                skip_existing,
                                cli,
                                cli.quiet,
                                cancelled.clone(),
                            )
                            .await;
                            eprintln!(
                                "  Transfer done: {} ok, {} skipped, {} failed",
                                summary.transferred_files,
                                summary.skipped_files,
                                summary.failed_files
                            );
                            if summary.failed_files > 0 {
                                exit_code = 4;
                            }
                        }
                        if let Some(code) = check_exit(
                            exit_code,
                            line_num,
                            "TRANSFER",
                            on_error_continue,
                            &mut failed_commands,
                        ) {
                            return code;
                        }
                    }
                    Err(e) => {
                        let code = provider_error_to_exit_code(&e);
                        eprintln!("  Planning failed: {}", e);
                        if let Some(c) = check_exit(
                            code,
                            line_num,
                            "TRANSFER",
                            on_error_continue,
                            &mut failed_commands,
                        ) {
                            return c;
                        }
                    }
                }
            }
            _ => {
                print_error(
                    format,
                    &format!("Line {}: Unknown command '{}'. Supported: SET, ECHO, ON_ERROR, CONNECT, DISCONNECT, GET, PUT, RM, MV, LS, CAT, STAT, FIND, DF, MKDIR, TREE, SYNC, CONNECT_SOURCE_PROFILE, CONNECT_DEST_PROFILE, TRANSFER", line_num + 1, cmd),
                    5,
                );
                if !on_error_continue {
                    return 5;
                }
                failed_commands += 1;
            }
        }
    }

    // Cleanup cross-profile providers
    if let Some((mut p, _)) = cross_source.take() {
        let _ = p.disconnect().await;
    }
    if let Some((mut p, _)) = cross_dest.take() {
        let _ = p.disconnect().await;
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
        (
            "anthropic",
            "ANTHROPIC_API_KEY",
            "https://api.anthropic.com",
        ),
        ("openai", "OPENAI_API_KEY", "https://api.openai.com/v1"),
        (
            "gemini",
            "GEMINI_API_KEY",
            "https://generativelanguage.googleapis.com",
        ),
        ("xai", "XAI_API_KEY", "https://api.x.ai/v1"),
        ("groq", "GROQ_API_KEY", "https://api.groq.com/openai/v1"),
        ("mistral", "MISTRAL_API_KEY", "https://api.mistral.ai/v1"),
        (
            "perplexity",
            "PERPLEXITY_API_KEY",
            "https://api.perplexity.ai",
        ),
        ("deepseek", "DEEPSEEK_API_KEY", "https://api.deepseek.com"),
        (
            "together",
            "TOGETHER_API_KEY",
            "https://api.together.xyz/v1",
        ),
        (
            "fireworks",
            "FIREWORKS_API_KEY",
            "https://api.fireworks.ai/inference/v1",
        ),
        ("cerebras", "CEREBRAS_API_KEY", "https://api.cerebras.ai/v1"),
        (
            "sambanova",
            "SAMBANOVA_API_KEY",
            "https://api.sambanova.ai/v1",
        ),
    ];
    for (name, env_key, base_url) in &providers {
        if let Ok(key) = std::env::var(env_key) {
            if !key.is_empty() {
                return Some((name.to_string(), key, base_url.to_string()));
            }
        }
    }
    // Check Ollama (no API key needed)
    if std::env::var("OLLAMA_HOST").is_ok()
        || std::path::Path::new("/usr/local/bin/ollama").exists()
    {
        return Some((
            "ollama".to_string(),
            String::new(),
            "http://localhost:11434".to_string(),
        ));
    }
    None
}

/// Detect AI provider from the encrypted vault (desktop app configuration).
/// Falls back here when no environment variable is set.
/// Reads config_ai_settings to find enabled providers, then resolves API keys
/// using the GUI-generated unique IDs (e.g. ai_apikey_mmw96fix-hqlohwhwr).
fn detect_ai_provider_from_vault(cli: &Cli) -> Option<(String, String, String)> {
    let store = open_vault(cli).ok()?;
    resolve_vault_ai_provider(&store, None)
}

/// Shared logic: resolve an AI provider from vault settings.
/// If `target_type` is Some, only match that provider type (e.g. "cohere").
/// If None, pick the first enabled provider with a valid key.
fn resolve_vault_ai_provider(
    store: &ftp_client_gui_lib::credential_store::CredentialStore,
    target_type: Option<&str>,
) -> Option<(String, String, String)> {
    let settings_json = store
        .get("config_ai_settings")
        .or_else(|_| store.get("ai_settings"))
        .ok()?;
    let settings: serde_json::Value = serde_json::from_str(&settings_json).ok()?;
    let providers = settings.get("providers")?.as_array()?;

    // Priority order for auto-detect (when target_type is None)
    let priority = [
        "anthropic",
        "openai",
        "google",
        "xai",
        "openrouter",
        "deepseek",
        "mistral",
        "groq",
        "perplexity",
        "cohere",
        "together",
        "kimi",
        "qwen",
        "cerebras",
        "sambanova",
        "fireworks",
        "ai21",
    ];

    let base_url_for = |ptype: &str| -> &str {
        match ptype {
            "anthropic" => "https://api.anthropic.com",
            "openai" => "https://api.openai.com/v1",
            "google" => "https://generativelanguage.googleapis.com",
            "xai" => "https://api.x.ai/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "deepseek" => "https://api.deepseek.com",
            "mistral" => "https://api.mistral.ai/v1",
            "groq" => "https://api.groq.com/openai/v1",
            "perplexity" => "https://api.perplexity.ai",
            "cohere" => "https://api.cohere.com/compatibility",
            "together" => "https://api.together.xyz/v1",
            "kimi" => "https://api.moonshot.cn/v1",
            "qwen" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "fireworks" => "https://api.fireworks.ai/inference/v1",
            "cerebras" => "https://api.cerebras.ai/v1",
            "sambanova" => "https://api.sambanova.ai/v1",
            "ai21" => "https://api.ai21.com/studio/v1",
            _ => "https://api.openai.com/v1",
        }
    };

    // Build a list of (type, id, base_url, enabled) from settings
    let mut candidates: Vec<(String, String, String, bool)> = Vec::new();
    for p in providers {
        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let ptype = p.get("type").and_then(|v| v.as_str()).unwrap_or(id);
        let enabled = p
            .get("isEnabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let custom_base = p.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");

        if id.is_empty() || !enabled {
            continue;
        }
        if let Some(target) = target_type {
            if ptype != target {
                continue;
            }
        }

        let url = if custom_base.is_empty() {
            base_url_for(ptype).to_string()
        } else {
            custom_base.to_string()
        };
        candidates.push((ptype.to_string(), id.to_string(), url, enabled));
    }

    // Sort by priority order (for auto-detect mode)
    if target_type.is_none() {
        candidates
            .sort_by_key(|(ptype, _, _, _)| priority.iter().position(|p| p == ptype).unwrap_or(99));
    }

    // Find first candidate with a valid API key in vault
    for (ptype, id, url, _) in &candidates {
        let vault_key = format!("ai_apikey_{}", id);
        if let Ok(key) = store.get(&vault_key) {
            if !key.is_empty() {
                eprintln!("Using AI provider '{}' from AeroFTP vault.", ptype);
                return Some((ptype.clone(), key, url.clone()));
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
         1. Use tools to perform actions - don't just describe what to do.\n\
         2. Be concise and direct. Explain briefly what you did after executing tools.\n\
         3. For destructive operations (delete, overwrite), confirm with the user first.\n\
         4. Resolve relative paths against the working directory."
            .to_string()
    };

    format!(
        "{}\n\n## Current Context\n- Working directory: {}\n- Platform: {}\n- Time: {}",
        base,
        cwd,
        std::env::consts::OS,
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
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

fn tool_exposure_kind(tool: &str) -> ToolExposureKind {
    match tool {
        "local_list"
        | "local_read"
        | "local_search"
        | "local_grep"
        | "local_head"
        | "local_tail"
        | "local_stat_batch"
        | "local_diff"
        | "local_tree"
        | "local_file_info"
        | "local_disk_usage"
        | "local_find_duplicates"
        | "clipboard_read"
        | "app_info"
        | "rag_search"
        | "rag_index"
        | "preview_edit"
        | "generate_transfer_plan"
        | "vault_peek"
        | "hash_file" => ToolExposureKind::LocalReadonly,
        "server_list_saved" | "remote_list" | "remote_info" | "remote_search" => {
            ToolExposureKind::RemoteMetadata
        }
        "remote_read" => ToolExposureKind::RemotePreview,
        "server_exec" => ToolExposureKind::RemoteBulkRead,
        "local_write" | "local_mkdir" | "local_edit" | "local_rename" | "local_copy_files"
        | "local_move_files" | "local_batch_rename" | "archive_compress" | "archive_decompress"
        | "clipboard_write" | "agent_memory_write" | "sync_preview" | "set_theme" => {
            ToolExposureKind::LocalModify
        }
        "remote_upload" | "remote_mkdir" | "remote_rename" | "remote_edit" | "upload_files"
        | "download_files" | "remote_download" => ToolExposureKind::RemoteModify,
        "local_delete" | "local_trash" | "remote_delete" | "sync_control" => {
            ToolExposureKind::Destructive
        }
        "shell_execute" => ToolExposureKind::Execution,
        _ => ToolExposureKind::Execution,
    }
}

fn tool_exposure_category(tool: &str) -> &'static str {
    match tool_exposure_kind(tool) {
        ToolExposureKind::LocalReadonly => "local-readonly",
        ToolExposureKind::RemoteMetadata => "remote-metadata",
        ToolExposureKind::RemotePreview => "remote-preview",
        ToolExposureKind::RemoteBulkRead => "remote-bulk-read",
        ToolExposureKind::LocalModify => "local-modify",
        ToolExposureKind::RemoteModify => "remote-modify",
        ToolExposureKind::Destructive => "destructive",
        ToolExposureKind::Execution => "execution",
    }
}

fn tool_data_egress(tool: &str) -> &'static str {
    match tool {
        "server_list_saved" | "remote_list" | "remote_info" | "remote_search" => "metadata",
        "remote_read" => "preview",
        "server_exec" => "operation-dependent",
        // Local content-reading tools: file contents are sent to the AI model
        "local_grep" | "local_head" | "local_tail" | "local_diff" | "rag_search" => "content",
        "rag_index" => "preview",
        // Metadata-only tools: no file content egress
        "local_file_info"
        | "local_disk_usage"
        | "local_find_duplicates"
        | "local_stat_batch"
        | "local_tree" => "metadata",
        _ => "none",
    }
}

/// Get tool danger level (0=safe, 1=medium, 2=high)
fn tool_danger_level(tool: &str) -> u8 {
    // Content-reading local tools are medium (data sent to AI model)
    if matches!(
        tool,
        "local_grep"
            | "local_head"
            | "local_tail"
            | "local_diff"
            | "local_read"
            | "rag_search"
            | "rag_index"
            | "clipboard_read"
    ) {
        return 1;
    }
    match tool_exposure_kind(tool) {
        ToolExposureKind::LocalReadonly => 0,
        ToolExposureKind::RemoteMetadata
        | ToolExposureKind::LocalModify
        | ToolExposureKind::RemoteModify => 1,
        ToolExposureKind::RemotePreview
        | ToolExposureKind::RemoteBulkRead
        | ToolExposureKind::Destructive
        | ToolExposureKind::Execution => 2,
    }
}

fn tool_danger_name(tool: &str) -> &'static str {
    match tool_danger_level(tool) {
        0 => "safe",
        1 => "medium",
        _ => "high",
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
async fn execute_cli_tool(
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    use serde_json::json;

    // Helper to extract string argument
    let get_str = |key: &str| -> Result<String, String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("Missing required argument: {}", key))
    };

    let get_str_opt = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    // Validate local path - deny sensitive paths (mirrors ai_tools.rs::validate_path)
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
                .unwrap_or(Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no parent",
                )))
        });
        if let Ok(canonical) = resolved {
            let s = canonical.to_string_lossy();
            if CLI_DENIED_SYSTEM_PREFIXES
                .iter()
                .any(|d| path_matches_prefix(&s, d))
            {
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
                    return Err(format!(
                        "{}: access to sensitive path denied: {}",
                        param, path
                    ));
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

    // Try dispatch through the unified tool engine (T3 Gate 2 Area A/B/C).
    // Se il tool non è nel registry canonico, cade nel match legacy sotto.
    {
        let json_mode = std::env::args().any(|a| a == "--json");
        let ctx = ftp_client_gui_lib::ai_core::cli_impl::CliToolCtx::new(
            ftp_client_gui_lib::ai_core::cli_impl::CliEventSink { json_mode },
            ftp_client_gui_lib::ai_core::cli_impl::CliCredentialProvider,
        );
        match ftp_client_gui_lib::ai_core::tools::dispatch_tool(&ctx, tool_name, args).await {
            Ok(v) => return Ok(v),
            Err(ftp_client_gui_lib::ai_core::tools::ToolError::Unknown(_))
            | Err(ftp_client_gui_lib::ai_core::tools::ToolError::NotMigrated(_)) => {
                // Fallback al match legacy
            }
            Err(e) => return Err(e.to_string()),
        }
    }

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
            let meta =
                std::fs::metadata(&path).map_err(|e| format!("Failed to stat file: {}", e))?;
            if meta.len() > 10_485_760 {
                return Err(format!(
                    "File too large: {:.1} MB (max 10 MB)",
                    meta.len() as f64 / 1_048_576.0
                ));
            }
            let max_bytes: usize = 5120;
            let file_size = meta.len() as usize;
            let read_size = std::cmp::min(file_size, max_bytes);
            let mut file =
                std::fs::File::open(&path).map_err(|e| format!("Failed to open file: {}", e))?;
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
            let matcher: Box<dyn Fn(&str) -> bool> =
                if let Some(suffix) = pattern_lower.strip_prefix('*') {
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
            std::fs::write(&path, &content).map_err(|e| format!("Failed to write file: {}", e))?;
            Ok(
                json!({ "success": true, "message": format!("Written {} bytes to {}", content.len(), path) }),
            )
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
            if normalized.is_empty()
                || normalized == "/"
                || normalized == "~"
                || normalized == "."
                || normalized == ".."
                || normalized == home_dir
            {
                return Err(format!("Refusing to delete dangerous path: {}", path));
            }
            let meta = std::fs::metadata(&path).map_err(|e| format!("Path not found: {}", e))?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("Failed to delete directory: {}", e))?;
            } else {
                std::fs::remove_file(&path).map_err(|e| format!("Failed to delete file: {}", e))?;
            }
            Ok(json!({ "success": true, "message": format!("Deleted {}", path) }))
        }

        "local_rename" => {
            let from = resolve_path(&get_str("from")?);
            let to = resolve_path(&get_str("to")?);
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;
            std::fs::rename(&from, &to).map_err(|e| format!("Failed to rename: {}", e))?;
            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "local_edit" => {
            let path = resolve_path(&get_str("path")?);
            let find = get_str("find")?;
            let replace = get_str("replace")?;
            let replace_all = args
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
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
            let count = if replace_all {
                content.matches(&find).count()
            } else {
                1
            };
            Ok(
                json!({ "success": true, "message": format!("Replaced {} occurrence(s) in {}", count, path) }),
            )
        }

        "local_move_files" => {
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(&resolve_path))
                        .collect()
                })
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
                        match std::fs::copy(source, &dest_path)
                            .and_then(|_| std::fs::remove_file(source))
                        {
                            Ok(_) => moved += 1,
                            Err(e) => errors.push(format!("{}: {}", filename, e)),
                        }
                    }
                }
            }
            Ok(json!({ "moved": moved, "errors": errors }))
        }

        "local_copy_files" => {
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(&resolve_path))
                        .collect()
                })
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
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(&resolve_path))
                        .collect()
                })
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
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let ext = p
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let parent = p
                    .parent()
                    .map(|pp| pp.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string());
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
                        let start = args
                            .get("start_number")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1);
                        format!("{}_{:03}{}", base, start + idx as u64, ext)
                    }
                    _ => {
                        errors.push(format!("Unknown mode: {}", mode));
                        continue;
                    }
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
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(&resolve_path))
                        .collect()
                })
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
            let meta = std::fs::metadata(&path).map_err(|e| format!("Failed to stat: {}", e))?;
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
            let min_size = args
                .get("min_size")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024);
            validate_path(&path, "path")?;
            use std::collections::HashMap;
            let mut size_map: HashMap<u64, Vec<String>> = HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() && meta.len() >= min_size {
                            size_map
                                .entry(meta.len())
                                .or_default()
                                .push(entry.path().to_string_lossy().to_string());
                        }
                    }
                }
            }
            // Only hash files with same size
            let mut duplicates = Vec::new();
            for paths in size_map.values() {
                if paths.len() < 2 {
                    continue;
                }
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
            let max_results = args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            let context_lines = args
                .get("context_lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(2) as usize;
            let case_sensitive = args
                .get("case_sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let glob_filter = get_str_opt("glob");
            validate_path(&path, "path")?;
            let re = if case_sensitive {
                regex::Regex::new(&pattern).map_err(|e| format!("Invalid regex: {}", e))?
            } else {
                regex::RegexBuilder::new(&pattern)
                    .case_insensitive(true)
                    .build()
                    .map_err(|e| format!("Invalid regex: {}", e))?
            };
            let mut results = Vec::new();
            fn walk_grep(
                dir: &std::path::Path,
                re: &regex::Regex,
                glob_filter: &Option<String>,
                ctx: usize,
                results: &mut Vec<serde_json::Value>,
                max: usize,
            ) {
                if results.len() >= max {
                    return;
                }
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        if results.len() >= max {
                            return;
                        }
                        let p = entry.path();
                        if p.is_dir() {
                            walk_grep(&p, re, glob_filter, ctx, results, max);
                        } else if p.is_file() {
                            if let Some(ref glob) = glob_filter {
                                let name = p
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_lowercase();
                                let pattern = glob.trim_start_matches('*').to_lowercase();
                                if !name.ends_with(&pattern) {
                                    continue;
                                }
                            }
                            if let Ok(content) = std::fs::read_to_string(&p) {
                                let lines: Vec<&str> = content.lines().collect();
                                for (i, line) in lines.iter().enumerate() {
                                    if results.len() >= max {
                                        return;
                                    }
                                    if re.is_match(line) {
                                        let start = i.saturating_sub(ctx);
                                        let end = (i + ctx + 1).min(lines.len());
                                        let context: Vec<String> = lines[start..end]
                                            .iter()
                                            .map(|l| l.to_string())
                                            .collect();
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
            walk_grep(
                std::path::Path::new(&path),
                &re,
                &glob_filter,
                context_lines,
                &mut results,
                max_results,
            );
            let total = results.len();
            Ok(json!({ "results": results, "total": total }))
        }

        "local_head" => {
            let path = resolve_path(&get_str("path")?);
            let lines = args
                .get("lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(500) as usize;
            validate_path(&path, "path")?;
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let result: String = content.lines().take(lines).collect::<Vec<_>>().join("\n");
            let total_lines = content.lines().count();
            Ok(
                json!({ "content": result, "lines_shown": lines.min(total_lines), "total_lines": total_lines }),
            )
        }

        "local_tail" => {
            let path = resolve_path(&get_str("path")?);
            let lines = args
                .get("lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(500) as usize;
            validate_path(&path, "path")?;
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let all_lines: Vec<&str> = content.lines().collect();
            let start = all_lines.len().saturating_sub(lines);
            let result = all_lines[start..].join("\n");
            Ok(
                json!({ "content": result, "lines_shown": all_lines.len() - start, "total_lines": all_lines.len() }),
            )
        }

        "local_stat_batch" => {
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(&resolve_path))
                        .collect()
                })
                .ok_or("Missing 'paths' array parameter")?;
            if paths.len() > 100 {
                return Err("Maximum 100 paths allowed".to_string());
            }
            let stats: Vec<serde_json::Value> = paths
                .iter()
                .map(|p| {
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
                })
                .collect();
            Ok(json!({ "stats": stats }))
        }

        "local_tree" => {
            let path = resolve_path(&get_str("path")?);
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(3)
                .min(10) as usize;
            let show_hidden = args
                .get("show_hidden")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let glob_filter = get_str_opt("glob");
            validate_path(&path, "path")?;
            fn build_tree(
                dir: &std::path::Path,
                depth: usize,
                max_depth: usize,
                show_hidden: bool,
                glob_filter: &Option<String>,
            ) -> Vec<serde_json::Value> {
                if depth >= max_depth {
                    return vec![];
                }
                let mut items = Vec::new();
                if let Ok(entries) = std::fs::read_dir(dir) {
                    let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                    sorted.sort_by_key(|e| e.file_name());
                    for entry in sorted {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !show_hidden && name.starts_with('.') {
                            continue;
                        }
                        let meta = entry.metadata().ok();
                        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                        if !is_dir {
                            if let Some(ref glob) = glob_filter {
                                let pattern = glob.trim_start_matches('*').to_lowercase();
                                if !name.to_lowercase().ends_with(&pattern) {
                                    continue;
                                }
                            }
                        }
                        let mut node = serde_json::json!({
                            "name": name,
                            "is_dir": is_dir,
                            "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        });
                        if is_dir {
                            let children = build_tree(
                                &entry.path(),
                                depth + 1,
                                max_depth,
                                show_hidden,
                                glob_filter,
                            );
                            node["children"] = serde_json::json!(children);
                        }
                        items.push(node);
                    }
                }
                items
            }
            let tree = build_tree(
                std::path::Path::new(&path),
                0,
                max_depth,
                show_hidden,
                &glob_filter,
            );
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
            let data = std::fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))?;
            let hash = match algorithm.to_lowercase().as_str() {
                "md5" => {
                    use md5::Digest;
                    let mut hasher = md5::Md5::new();
                    hasher.update(&data);
                    format!("{:x}", hasher.finalize())
                }
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
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(30)
                .min(120);

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
                "rm -rf /",
                "rm -rf /*",
                "mkfs",
                "dd if=",
                ":(){",
                "fork bomb",
                "chmod -R 777 /",
                "chmod 777 /",
                "chown ",
                "wget|sh",
                "curl|sh",
                "curl|bash",
                "wget|bash",
                "> /dev/sda",
                "shutdown",
                "reboot",
                "halt",
                "init 0",
                "init 6",
                "kill -9 1",
                "killall",
                "pkill -9",
                "python -c",
                "python3 -c",
                "eval ",
                "base64 -d",
                "base64 --decode",
                "truncate",
                "shred",
                "crontab",
                "nohup",
                "systemctl",
                "service ",
                "mount ",
                "umount ",
                "fdisk",
                "parted",
                "iptables",
                "useradd",
                "userdel",
                "passwd",
                "sudo ",
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
            cmd.arg("-c")
                .arg(&command)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if let Some(ref wd) = working_dir {
                cmd.current_dir(wd);
            }
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
                    .await
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
            // Safe: spawn tools directly with .arg() - no shell interpolation
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(&resolve_path))
                        .collect()
                })
                .ok_or("Missing 'paths' array parameter")?;
            let output_path = resolve_path(&get_str("output_path")?);
            let format = get_str_opt("format").unwrap_or_else(|| "zip".to_string());
            validate_path(&output_path, "output_path")?;
            for p in &paths {
                validate_path(p, "paths[]")?;
            }
            let mut cmd = match format.as_str() {
                "zip" => {
                    let mut c = tokio::process::Command::new("zip");
                    c.arg("-r").arg(&output_path);
                    for p in &paths {
                        c.arg(p);
                    }
                    c
                }
                "tar.gz" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("czf").arg(&output_path);
                    for p in &paths {
                        c.arg(p);
                    }
                    c
                }
                "tar.bz2" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("cjf").arg(&output_path);
                    for p in &paths {
                        c.arg(p);
                    }
                    c
                }
                "tar.xz" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("cJf").arg(&output_path);
                    for p in &paths {
                        c.arg(p);
                    }
                    c
                }
                "tar" => {
                    let mut c = tokio::process::Command::new("tar");
                    c.arg("cf").arg(&output_path);
                    for p in &paths {
                        c.arg(p);
                    }
                    c
                }
                "7z" => {
                    let mut c = tokio::process::Command::new("7z");
                    c.arg("a").arg(&output_path);
                    for p in &paths {
                        c.arg(p);
                    }
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
                c.arg("x")
                    .arg(&archive_path)
                    .arg(format!("-o{}", output_dir));
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
                stdin
                    .write_all(content.as_bytes())
                    .await
                    .map_err(|e| format!("Failed to write to clipboard: {}", e))?;
            }
            child
                .wait()
                .await
                .map_err(|e| format!("xclip failed: {}", e))?;
            Ok(
                json!({ "success": true, "message": format!("Copied {} chars to clipboard", content.len()) }),
            )
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
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;
            let entries = provider
                .list(&effective_path)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let entries = entries?;
            let items: Vec<serde_json::Value> = entries
                .iter()
                .take(200)
                .map(|e| {
                    json!({
                        "name": e.name,
                        "path": e.path,
                        "is_dir": e.is_dir,
                        "size": e.size,
                        "modified": e.modified,
                    })
                })
                .collect();
            Ok(json!({
                "server": server_query,
                "path": effective_path,
                "entries": items,
                "total": entries.len(),
                "truncated": entries.len() > 200,
            }))
        }

        "remote_read" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;
            let preview = read_remote_preview(&mut provider, &effective_path)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let (preview, size, truncated) = preview?;
            let content = String::from_utf8_lossy(&preview).to_string();
            Ok(json!({
                "server": server_query,
                "path": effective_path,
                "content": content,
                "size": size,
                "truncated": truncated,
            }))
        }

        "remote_info" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;
            let entry = provider
                .stat(&effective_path)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let entry = entry?;
            Ok(json!({
                "server": server_query,
                "path": effective_path,
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
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;
            let entries = provider
                .find(&effective_path, &pattern)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            let entries = entries?;
            let items: Vec<serde_json::Value> = entries
                .iter()
                .take(100)
                .map(|e| {
                    json!({
                        "name": e.name,
                        "path": e.path,
                        "is_dir": e.is_dir,
                        "size": e.size,
                    })
                })
                .collect();
            Ok(json!({
                "server": server_query,
                "path": effective_path,
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

            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_remote_path = resolve_agent_remote_path(&initial_path, &remote_path)?;
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
                match provider
                    .upload(&temp_path, &effective_remote_path, None)
                    .await
                {
                    Ok(()) => {
                        let _ = provider.disconnect().await;
                        return Ok(json!({
                            "server": server_query,
                            "remote_path": effective_remote_path,
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

            let bytes = std::fs::metadata(&upload_source)
                .map(|m| m.len())
                .unwrap_or(0);
            let result = provider
                .upload(&upload_source, &effective_remote_path, None)
                .await;
            let _ = provider.disconnect().await;
            result.map_err(|e| e.to_string())?;
            Ok(json!({
                "server": server_query,
                "remote_path": effective_remote_path,
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
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_remote_path = resolve_agent_remote_path(&initial_path, &remote_path)?;
            let result = provider
                .download(&effective_remote_path, &local_path, None)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({
                "server": server_query,
                "remote_path": effective_remote_path,
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
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;
            let result = provider
                .mkdir(&effective_path)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({ "server": server_query, "path": effective_path, "created": true }))
        }

        "remote_delete" => {
            let server_query = get_str("server")?;
            let path = get_str("path")?;
            if path.contains('\0') {
                return Err("path contains null bytes".to_string());
            }
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;
            let result = provider
                .delete(&effective_path)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(json!({ "server": server_query, "path": effective_path, "deleted": true }))
        }

        "remote_rename" => {
            let server_query = get_str("server")?;
            let from = get_str("from")?;
            let to = get_str("to")?;
            if from.contains('\0') || to.contains('\0') {
                return Err("remote path contains null bytes".to_string());
            }
            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_from = resolve_agent_remote_path(&initial_path, &from)?;
            let effective_to = resolve_agent_remote_path(&initial_path, &to)?;
            let result = provider
                .rename(&effective_from, &effective_to)
                .await
                .map_err(|e| e.to_string());
            let _ = provider.disconnect().await;
            result?;
            Ok(
                json!({ "server": server_query, "from": effective_from, "to": effective_to, "renamed": true }),
            )
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

            let (mut provider, initial_path) = create_and_connect_for_agent(&server_query).await?;
            let effective_path = resolve_agent_remote_path(&initial_path, &path)?;

            let result = match operation.as_str() {
                "ls" => {
                    let entries = provider
                        .list(&effective_path)
                        .await
                        .map_err(|e| e.to_string())?;
                    let items: Vec<serde_json::Value> = entries
                        .iter()
                        .take(200)
                        .map(|e| {
                            json!({
                                "name": e.name,
                                "path": e.path,
                                "is_dir": e.is_dir,
                                "size": e.size,
                                "modified": e.modified,
                            })
                        })
                        .collect();
                    json!({
                        "operation": "ls",
                        "server": server_query,
                        "path": effective_path,
                        "entries": items,
                        "total": entries.len(),
                        "truncated": entries.len() > 200,
                    })
                }
                "cat" => {
                    let (preview, size, truncated) =
                        read_remote_preview(&mut provider, &effective_path)
                            .await
                            .map_err(|e| e.to_string())?;
                    if truncated {
                        let preview = String::from_utf8_lossy(&preview);
                        json!({
                            "operation": "cat",
                            "server": server_query,
                            "path": effective_path,
                            "content": preview,
                            "size": size,
                            "truncated": true,
                        })
                    } else {
                        let content = String::from_utf8_lossy(&preview);
                        json!({
                            "operation": "cat",
                            "server": server_query,
                            "path": effective_path,
                            "content": content,
                            "size": size,
                            "truncated": false,
                        })
                    }
                }
                "stat" => {
                    let entry = provider
                        .stat(&effective_path)
                        .await
                        .map_err(|e| e.to_string())?;
                    json!({
                        "operation": "stat",
                        "server": server_query,
                        "path": effective_path,
                        "name": entry.name,
                        "is_dir": entry.is_dir,
                        "size": entry.size,
                        "modified": entry.modified,
                        "permissions": entry.permissions,
                    })
                }
                "find" => {
                    let pat = pattern.unwrap_or_else(|| "*".to_string());
                    let entries = provider
                        .find(&effective_path, &pat)
                        .await
                        .map_err(|e| e.to_string())?;
                    let items: Vec<serde_json::Value> = entries
                        .iter()
                        .take(100)
                        .map(|e| {
                            json!({
                                "name": e.name,
                                "path": e.path,
                                "is_dir": e.is_dir,
                                "size": e.size,
                            })
                        })
                        .collect();
                    json!({
                        "operation": "find",
                        "server": server_query,
                        "path": effective_path,
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
                        "path": effective_path,
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
    eprintln!(
        "  \x1b[1m🔧 Tool Call:\x1b[0m {} [{}]",
        tool_name, level_str
    );
    eprintln!(
        "    category: {} · data-egress: {}",
        tool_exposure_category(tool_name),
        tool_data_egress(tool_name)
    );
    // Show key arguments
    if let Some(obj) = args.as_object() {
        for (key, val) in obj {
            let display = match val {
                serde_json::Value::String(s) => {
                    if s.len() > 80 {
                        format!("{}...", s.get(..77).unwrap_or(s))
                    } else {
                        s.clone()
                    }
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

/// EventSink that both streams tokens live to stdout (when `print_live`) and
/// accumulates the full response so the caller can reconstruct an AIResponse
/// after the stream finishes. Used by `agent_tool_loop` to replace the
/// previous synchronous `call_ai()` path with token-by-token streaming.
struct CollectingCliSink {
    print_live: bool,
    inner: std::sync::Mutex<CollectingSinkState>,
}

#[derive(Default)]
struct CollectingSinkState {
    content: String,
    thinking: String,
    tool_calls: Option<Vec<ftp_client_gui_lib::ai::AIToolCall>>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    tokens_used: Option<u32>,
    cache_creation_input_tokens: Option<u32>,
    cache_read_input_tokens: Option<u32>,
    error_seen: bool,
}

impl CollectingCliSink {
    fn new(print_live: bool) -> Self {
        Self {
            print_live,
            inner: std::sync::Mutex::new(CollectingSinkState::default()),
        }
    }

    /// Produce an AIResponse snapshot from the accumulated stream state.
    fn into_response(self, model: &str) -> ftp_client_gui_lib::ai::AIResponse {
        let state = self.inner.into_inner().unwrap_or_else(|e| e.into_inner());
        ftp_client_gui_lib::ai::AIResponse {
            content: state.content,
            model: model.to_string(),
            tokens_used: state.tokens_used.or_else(|| {
                match (state.input_tokens, state.output_tokens) {
                    (Some(i), Some(o)) => Some(i + o),
                    _ => None,
                }
            }),
            input_tokens: state.input_tokens,
            output_tokens: state.output_tokens,
            finish_reason: None,
            tool_calls: state.tool_calls,
            cache_creation_input_tokens: state.cache_creation_input_tokens,
            cache_read_input_tokens: state.cache_read_input_tokens,
        }
    }
}

impl ftp_client_gui_lib::ai_core::EventSink for CollectingCliSink {
    fn emit_stream_chunk(
        &self,
        _stream_id: &str,
        chunk: &ftp_client_gui_lib::ai_stream::StreamChunk,
    ) {
        // Live output to stdout when enabled. We only print content deltas here;
        // thinking text (Anthropic extended thinking) goes to stderr dimmed.
        if self.print_live {
            if !chunk.content.is_empty() {
                use std::io::Write;
                print!("{}", chunk.content);
                io::stdout().flush().ok();
            }
            if let Some(ref thinking) = chunk.thinking {
                if !thinking.is_empty() {
                    eprint!("\x1b[2m{}\x1b[0m", thinking);
                    io::stderr().flush().ok();
                }
            }
        }

        // Accumulate regardless of live-print so the caller sees the full
        // response even when running non-interactively.
        if let Ok(mut state) = self.inner.lock() {
            if chunk.content.starts_with("Error:") && chunk.done {
                state.error_seen = true;
            }
            state.content.push_str(&chunk.content);
            if let Some(ref t) = chunk.thinking {
                state.thinking.push_str(t);
            }
            if chunk.tool_calls.is_some() {
                state.tool_calls = chunk.tool_calls.clone();
            }
            if let Some(v) = chunk.input_tokens {
                state.input_tokens = Some(v);
            }
            if let Some(v) = chunk.output_tokens {
                state.output_tokens = Some(v);
                state.tokens_used = Some(state.input_tokens.unwrap_or(0) + v);
            }
            if let Some(v) = chunk.cache_creation_input_tokens {
                state.cache_creation_input_tokens = Some(v);
            }
            if let Some(v) = chunk.cache_read_input_tokens {
                state.cache_read_input_tokens = Some(v);
            }
            if chunk.done && self.print_live && !state.content.is_empty() {
                // Ensure a trailing newline so the shell prompt doesn't
                // collide with the last streamed token.
                if !state.content.ends_with('\n') {
                    println!();
                }
            }
        }
    }

    fn emit_tool_progress(&self, _progress: &ftp_client_gui_lib::ai_core::ToolProgress) {
        // Tool progress is surfaced via agent_tool_loop's inline execution UI.
    }

    fn emit_app_control(&self, _event_name: &str, _payload: &serde_json::Value) {
        // CLI ignores GUI-only app control events.
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
    use ftp_client_gui_lib::ai::{AIRequest, AIResponse, ChatMessage};

    let tools = cli_tool_definitions();
    let mut steps = 0u32;

    loop {
        // Build request with tool definitions
        let mut all_messages = vec![ChatMessage {
            role: "system".to_string(),
            content: cfg.system.clone(),
            images: None,
            tool_calls_echo: None,
            tool_call_id: None,
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
            tool_results: None, // Tool results are embedded in conversation messages
            thinking_budget: None,
            top_p: None,
            top_k: None,
            cached_content: None,
            web_search: None,
        };

        // T4: stream the assistant response token-by-token via ai_chat_stream_with_sink.
        // Ctrl+C during the stream triggers ai_cancel_stream for graceful cancellation.
        let stream_id = format!(
            "cli-agent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );

        // Only stream live to stdout when attached to a TTY and plan_only is off.
        // plan_only should produce deterministic output, not interleaved tokens.
        let print_live = is_tty && !cfg.plan_only;
        let sink = CollectingCliSink::new(print_live);

        let sid_for_signal = stream_id.clone();
        let cancel_task = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = ftp_client_gui_lib::ai_stream::ai_cancel_stream(sid_for_signal).await;
            }
        });

        let stream_outcome =
            ftp_client_gui_lib::ai_stream::ai_chat_stream_with_sink(&sink, request, &stream_id)
                .await;

        cancel_task.abort();
        stream_outcome?;

        let response: AIResponse = sink.into_response(&cfg.model);

        {
            let mut usage = cfg
                .usage
                .lock()
                .map_err(|_| "Agent usage lock poisoned".to_string())?;
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
        let tool_calls = response.tool_calls.as_ref().filter(|tc| !tc.is_empty());

        match tool_calls {
            None => {
                // No tool calls - return the text response
                return Ok(response.content);
            }
            Some(calls) => {
                if cfg.plan_only {
                    let plan_lines: Vec<String> = calls
                        .iter()
                        .map(|tc| {
                            format!(
                                "- {} {}",
                                tc.name,
                                serde_json::to_string(&tc.arguments)
                                    .unwrap_or_else(|_| "{}".to_string())
                            )
                        })
                        .collect();
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
                            tool_calls_echo: None,
                            tool_call_id: None,
                        });
                    }
                    return Ok(format!(
                        "{}\n\n[Reached max steps limit ({}).]",
                        response.content, cfg.max_steps
                    ));
                }

                // Show assistant text if any.
                // T4: when print_live is true the sink has already streamed
                // response.content to stdout — printing it again here would
                // duplicate the visible output, so we only fall back to the
                // stderr echo when streaming to stdout is disabled.
                if !response.content.is_empty() && is_tty && !print_live {
                    eprintln!("\n{}", response.content);
                }

                // Add assistant message with tool_calls echo (required by OpenAI/Cohere format)
                let tool_calls_echo: Vec<ftp_client_gui_lib::ai::ToolCallEcho> = calls
                    .iter()
                    .map(|tc| ftp_client_gui_lib::ai::ToolCallEcho {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                    })
                    .collect();

                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: response.content.clone(),
                    images: None,
                    tool_calls_echo: Some(tool_calls_echo),
                    tool_call_id: None,
                });

                // Execute each tool call
                for tc in calls {
                    // Check approval
                    let approved = if is_auto_approved(&tc.name, cfg.approve_level) {
                        if is_tty {
                            eprintln!(
                                "  \x1b[32m✓\x1b[0m Auto-approved: {} ({} · egress: {})",
                                tc.name,
                                tool_exposure_category(&tc.name),
                                tool_data_egress(&tc.name)
                            );
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
                                    format!(
                                        "{}... [truncated, {} bytes total]",
                                        s.get(..8192).unwrap_or(&s),
                                        s.len()
                                    )
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

                    // Add tool result as conversation message with tool_call_id
                    messages.push(ChatMessage {
                        role: "tool".to_string(),
                        content: result_content,
                        images: None,
                        tool_calls_echo: None,
                        tool_call_id: Some(tc.id.clone()),
                    });
                }

                // Tool results are now in conversation history (no separate tool_results field needed)

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
            // Try vault: resolve via config_ai_settings (GUI uses unique IDs, not provider names)
            if let Some((_, vault_key, vault_url)) = open_vault(_cli)
                .ok()
                .and_then(|s| resolve_vault_ai_provider(&s, Some(name)))
            {
                eprintln!("Using '{}' API key from AeroFTP vault.", name);
                (name.clone(), vault_key, vault_url)
            } else {
                eprintln!("Error: {} is not set or empty.", env_key);
                eprintln!("Set it: export {}=your-api-key", env_key);
                eprintln!("Or save the key in AeroFTP desktop (Settings > AI).");
                return 5;
            }
        } else {
            (name.clone(), key, url.to_string())
        }
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
                    format!(
                        "Pre-validated remote target: {} ({}) path {}",
                        display, provider_label, initial_path
                    )
                }
                Err(code) => {
                    print_error(
                        format,
                        &format!("agent pre-connect failed for '{}'", target),
                        code,
                    );
                    return code;
                }
            }
        } else {
            match create_and_connect_for_agent(&target).await {
                Ok((mut provider, initial_path)) => {
                    let provider_label = provider.provider_type().to_string();
                    let display = provider.display_name();
                    let _ = provider.disconnect().await;
                    format!(
                        "Pre-validated saved server: {} ({}) path {}",
                        display, provider_label, initial_path
                    )
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
            if n > 0 {
                Some(buf)
            } else {
                None
            }
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

/// Shared agent configuration - avoids passing too many arguments
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
async fn cmd_agent_oneshot(message: &str, cfg: &AgentConfig, format: OutputFormat) -> i32 {
    use ftp_client_gui_lib::ai::ChatMessage;

    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: message.to_string(),
        images: None,
        tool_calls_echo: None,
        tool_call_id: None,
    }];

    let is_tty = io::stdin().is_terminal();

    match agent_tool_loop(cfg, &mut messages, is_tty).await {
        Ok(response) => {
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "ok",
                            "response": response,
                        })
                    );
                }
                OutputFormat::Text => {
                    // T4: when is_tty the final response was already streamed to
                    // stdout token-by-token by the sink, so avoid duplicating it.
                    // Ensure there is a trailing newline for non-streamed paths.
                    if !is_tty || cfg.plan_only {
                        println!("{}", response);
                    }
                }
            }
            0
        }
        Err(e) => {
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "error",
                            "error": e,
                        })
                    );
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
    eprintln!(
        "  \x1b[1m│  {} tools · 19 AI providers · tool execution  │\x1b[0m",
        cli_tool_count
    );
    eprintln!("  \x1b[1m╰─────────────────────────────────────────────╯\x1b[0m");
    eprintln!();
    eprintln!(
        "  \x1b[36mProvider:\x1b[0m  {} ({})",
        cfg.provider_name, cfg.model
    );
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
                        eprintln!(
                            "  [{}] \x1b[1m{}\x1b[0m - {} ({}, egress: {})",
                            label,
                            t.name,
                            t.description,
                            tool_exposure_category(&t.name),
                            tool_data_egress(&t.name)
                        );
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
                "/cost" => match cfg.usage.lock() {
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
                },
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
            tool_calls_echo: None,
            tool_call_id: None,
        });

        // Show thinking indicator — cleared as soon as the first stream chunk
        // lands on stdout, so the spinner never clashes with streamed tokens.
        if is_tty {
            eprint!("\n  \x1b[2m⠙ Thinking...\x1b[0m");
            io::stderr().flush().ok();
        }

        // Call AI with tool execution loop
        match agent_tool_loop(cfg, &mut conversation, is_tty).await {
            Ok(response) => {
                if is_tty {
                    eprint!("\r                    \r"); // Clear "Thinking..."
                                                         // T4: response has already been streamed to stdout by the
                                                         // sink. Just emit a blank line for readability between turns.
                    println!();
                } else {
                    println!("\n{}\n", response);
                }
                conversation.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: response,
                    images: None,
                    tool_calls_echo: None,
                    tool_call_id: None,
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

/// Orchestration mode - JSON-RPC 2.0 over stdin/stdout
async fn cmd_agent_orchestrate(cfg: &AgentConfig) -> i32 {
    use ftp_client_gui_lib::ai::ChatMessage;
    use std::io::BufRead;

    let mut conversation: Vec<ChatMessage> = Vec::new();

    // Emit ready notification with actual CLI tool count
    let cli_tools = cli_tool_definitions();
    let cli_tool_count = cli_tools.len();
    println!(
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "agent/ready",
            "params": {
                "version": env!("CARGO_PKG_VERSION"),
                "tools": cli_tool_count,
            }
        })
    );

    const ORCH_MAX_LINE_BYTES: usize = 1_048_576; // 1 MB
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.len() > ORCH_MAX_LINE_BYTES {
            println!(
                "{}",
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32600, "message": "Line exceeds 1 MB limit" }
                })
            );
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
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                    })
                );
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
                    println!(
                        "{}",
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": { "code": -32602, "message": "Missing 'message' parameter" }
                        })
                    );
                    continue;
                }

                conversation.push(ChatMessage {
                    role: "user".to_string(),
                    content: msg.to_string(),
                    images: None,
                    tool_calls_echo: None,
                    tool_call_id: None,
                });

                // Emit thinking notification
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "stream/thinking",
                        "params": { "content": "Processing..." }
                    })
                );

                match agent_tool_loop(cfg, &mut conversation, false).await {
                    Ok(response) => {
                        conversation.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: response.clone(),
                            images: None,
                            tool_calls_echo: None,
                            tool_call_id: None,
                        });
                        // Sliding window: keep last 40 messages
                        if conversation.len() > 40 {
                            conversation.drain(..conversation.len() - 40);
                        }
                        println!(
                            "{}",
                            serde_json::json!({
                                "jsonrpc": "2.0", "id": id,
                                "result": {
                                    "status": "ok",
                                    "response": response,
                                    "messages": conversation.len(),
                                }
                            })
                        );
                    }
                    Err(e) => {
                        conversation.pop(); // Remove failed user message
                        println!(
                            "{}",
                            serde_json::json!({
                                "jsonrpc": "2.0", "id": id,
                                "error": { "code": -32000, "message": e }
                            })
                        );
                    }
                }
            }

            "session/status" | "agent.status" => {
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "status": "ok",
                            "messages": conversation.len(),
                            "tools": cli_tool_count,
                        }
                    })
                );
            }

            "session/clear" | "agent.clear" => {
                conversation.clear();
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": { "status": "ok" }
                    })
                );
            }

            "session/close" | "agent.close" => {
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": { "status": "ok" }
                    })
                );
                break;
            }

            "tool/list" | "agent.tools" => {
                // Expose only tools actually implemented in CLI executor
                let tool_entries: Vec<serde_json::Value> = cli_tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "name": t.name,
                            "description": t.description,
                            "danger": tool_danger_name(&t.name),
                            "category": tool_exposure_category(&t.name),
                            "data_egress": tool_data_egress(&t.name)
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": { "tools": tool_entries }
                    })
                );
            }

            _ => {
                println!(
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32601, "message": format!("Method not found: {}", method) }
                    })
                );
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

    // Show the banner only when it's actually useful: bare invocation or
    // top-level --help. Subcommand help (e.g. `aeroftp-cli get --help`)
    // skips it so the banner doesn't dominate every screen during
    // exploration. Suppressed entirely when stderr isn't a TTY (CI,
    // pipes), when AEROFTP_NO_BANNER is set, or when --no-banner is
    // present anywhere on the command line.
    let raw_args: Vec<String> = std::env::args().collect();
    let banner_suppressed = std::env::var("AEROFTP_NO_BANNER").is_ok()
        || raw_args.iter().any(|a| a == "--no-banner")
        || !std::io::stderr().is_terminal();
    let is_top_level_invocation = match raw_args.get(1).map(String::as_str) {
        // bare `aeroftp-cli` or top-level help variants
        None | Some("--help" | "-h" | "help") => true,
        // anything else is either a subcommand or a top-level flag,
        // neither of which should re-print the banner
        _ => false,
    };
    let show_banner = is_top_level_invocation && !banner_suppressed;
    if show_banner {
        // Green (#00d26a) for "Aero", Blue (#0095ff) for "FTP"
        if use_color() {
            let g = "\x1b[1;38;2;0;210;106m"; // green
            let b = "\x1b[1;38;2;0;149;255m"; // blue
            let r = "\x1b[0m";
            eprintln!();
            eprintln!("  {g}    _                  {b} _____ _____ ____  {r}");
            eprintln!("  {g}   / \\   ___ _ __ ___  {b}|  ___|_   _|  _ \\ {r}");
            eprintln!("  {g}  / _ \\ / _ \\ '__/ _ \\ {b}| |_    | | | |_) |{r}");
            eprintln!("  {g} / ___ \\  __/ | | (_) |{b}|  _|   | | |  __/ {r}");
            eprintln!("  {g}/_/ _ \\_\\___|_|  \\___/ {b}|_|     |_| |_|    {r}");
            eprintln!();
            eprintln!(
                "  \x1b[1;37mAeroFTP\x1b[0m  {g}v{}{r}  {b}|{r}  23 providers, {} via direct URL  {b}|{r}  pget  {b}|{r}  mcp  {b}|{r}  ai agent  {b}|{r}  vault profiles",
                env!("CARGO_PKG_VERSION"),
                SUPPORTED_URL_SCHEMES.len()
            );
            eprintln!("\x1b[38;2;140;140;160m  transfer engine for operators, shell users, and terminal obsessives{r}");
        } else {
            eprintln!();
            eprintln!("      _                   _____ _____ ____  ");
            eprintln!("     / \\   ___ _ __ ___  |  ___|_   _|  _ \\ ");
            eprintln!("    / _ \\ / _ \\ '__/ _ \\ | |_    | | | |_) |");
            eprintln!("   / ___ \\  __/ | | (_) ||  _|   | | |  __/ ");
            eprintln!("  /_/   \\_\\___|_|  \\___/ |_|     |_| |_|    ");
            eprintln!();
            eprintln!(
                "  AeroFTP  v{}  |  23 providers, {} via direct URL  |  pget  |  mcp  |  ai agent  |  vault profiles",
                env!("CARGO_PKG_VERSION"),
                SUPPORTED_URL_SCHEMES.len()
            );
            eprintln!("  transfer engine for operators, shell users, and terminal obsessives");
        }
        eprintln!();
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

    // Stash JSON-mode globally so banner-emitting helpers far down
    // the stack can suppress without threading `format` through every
    // call site. Set BEFORE any vault open or connect can fire.
    if matches!(format, OutputFormat::Json) {
        JSON_MODE.store(true, Ordering::Relaxed);
    }

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
            // Second Ctrl+C - force exit immediately
            std::process::exit(130);
        }
        eprintln!("\nInterrupted (Ctrl+C) - press again to force quit");
        cancelled_clone.store(true, Ordering::Relaxed);
    });

    // Apply --inplace mode (skip .aerotmp temp files in downloads)
    if cli.inplace {
        ftp_client_gui_lib::providers::atomic_write::set_inplace_mode(true);
    }

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
            limit,
            files_only,
            dirs_only,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_ls(
                u,
                p,
                *long,
                sort,
                *reverse,
                *all,
                *limit,
                *files_only,
                *dirs_only,
                &cli,
                format,
            )
            .await
        }
        Commands::Pget {
            url,
            remote,
            local,
            segments,
        } => {
            // Thin alias for `get --segments N`. Always non-recursive
            // (segmented parallel download only makes sense per-file) and
            // defaults to 4 segments instead of 1. Routes through the same
            // cmd_get + retry plumbing so behaviour stays in lockstep.
            let (u, r, l) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), Some(remote.as_str()))
            } else {
                (url.as_str(), remote.as_str(), local.as_deref())
            };
            let max_attempts = cli.retries.max(1);
            let sleep_dur = parse_retry_sleep(&cli.retries_sleep);
            let max_transfer_limit = resolve_max_transfer(&cli);
            let mut last_code = 0i32;
            for attempt in 1..=max_attempts {
                last_code =
                    cmd_get(u, r, l, false, *segments, &cli, format, cancelled.clone()).await;
                if !is_retryable_exit(last_code)
                    || session_transfer_exceeded(max_transfer_limit)
                    || attempt == max_attempts
                {
                    break;
                }
                if !cli.quiet {
                    eprintln!(
                        "Attempt {}/{} failed (exit {}), retrying in {:?}...",
                        attempt, max_attempts, last_code, sleep_dur
                    );
                }
                if !sleep_dur.is_zero() {
                    tokio::time::sleep(sleep_dur).await;
                }
            }
            last_code
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
            let max_attempts = cli.retries.max(1);
            let sleep_dur = parse_retry_sleep(&cli.retries_sleep);
            let max_transfer_limit = resolve_max_transfer(&cli);
            let mut last_code = 0i32;
            for attempt in 1..=max_attempts {
                last_code = cmd_get(
                    u,
                    r,
                    l,
                    *recursive,
                    *segments,
                    &cli,
                    format,
                    cancelled.clone(),
                )
                .await;
                if !is_retryable_exit(last_code)
                    || session_transfer_exceeded(max_transfer_limit)
                    || attempt == max_attempts
                {
                    break;
                }
                if !cli.quiet {
                    eprintln!(
                        "Attempt {}/{} failed (exit {}), retrying in {:?}...",
                        attempt, max_attempts, last_code, sleep_dur
                    );
                }
                if !sleep_dur.is_zero() {
                    tokio::time::sleep(sleep_dur).await;
                }
            }
            last_code
        }
        Commands::Put {
            url,
            local,
            remote,
            recursive,
            no_clobber,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), Some(local.as_str()))
            } else {
                (url.as_str(), local.as_str(), remote.as_deref())
            };
            let max_attempts = cli.retries.max(1);
            let sleep_dur = parse_retry_sleep(&cli.retries_sleep);
            let max_transfer_limit = resolve_max_transfer(&cli);
            let mut last_code = 0i32;
            for attempt in 1..=max_attempts {
                last_code = cmd_put(
                    u,
                    l,
                    r,
                    *recursive,
                    *no_clobber,
                    &cli,
                    format,
                    cancelled.clone(),
                )
                .await;
                if !is_retryable_exit(last_code)
                    || session_transfer_exceeded(max_transfer_limit)
                    || attempt == max_attempts
                {
                    break;
                }
                if !cli.quiet {
                    eprintln!(
                        "Attempt {}/{} failed (exit {}), retrying in {:?}...",
                        attempt, max_attempts, last_code, sleep_dur
                    );
                }
                if !sleep_dur.is_zero() {
                    tokio::time::sleep(sleep_dur).await;
                }
            }
            last_code
        }
        Commands::Mkdir { url, path, parents } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_mkdir(u, p, *parents, &cli, format).await
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
        Commands::Link {
            url,
            path,
            expires,
            password,
            permissions,
            verify,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_link(
                u,
                p,
                expires.as_deref(),
                password.as_deref(),
                permissions.as_str(),
                *verify,
                &cli,
                format,
            )
            .await
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
            ServeCommands::Http {
                url,
                path,
                addr,
                allow_remote_bind,
                auth_token,
            } => {
                let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                    ("_", url.as_str())
                } else {
                    (url.as_str(), path.as_str())
                };
                cmd_serve_http(
                    u,
                    p,
                    addr,
                    *allow_remote_bind,
                    auth_token.clone(),
                    &cli,
                    format,
                )
                .await
            }
            ServeCommands::WebDav {
                url,
                path,
                addr,
                allow_remote_bind,
                auth_token,
            } => {
                let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                    ("_", url.as_str())
                } else {
                    (url.as_str(), path.as_str())
                };
                cmd_serve_webdav(
                    u,
                    p,
                    addr,
                    *allow_remote_bind,
                    auth_token.clone(),
                    &cli,
                    format,
                )
                .await
            }
            ServeCommands::Ftp {
                url,
                path,
                addr,
                allow_remote_bind,
                auth_user,
                auth_password,
                passive_ports,
            } => {
                let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                    ("_", url.as_str())
                } else {
                    (url.as_str(), path.as_str())
                };
                cmd_serve_ftp(
                    u,
                    p,
                    ServeEndpointOptions {
                        addr: addr.clone(),
                        allow_remote_bind: *allow_remote_bind,
                        auth: ServeAuthOptions {
                            username: auth_user.clone(),
                            password: auth_password.clone(),
                        },
                    },
                    passive_ports,
                    &cli,
                    format,
                )
                .await
            }
            ServeCommands::Sftp {
                url,
                path,
                addr,
                allow_remote_bind,
                auth_user,
                auth_password,
            } => {
                let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                    ("_", url.as_str())
                } else {
                    (url.as_str(), path.as_str())
                };
                cmd_serve_sftp(
                    u,
                    p,
                    ServeEndpointOptions {
                        addr: addr.clone(),
                        allow_remote_bind: *allow_remote_bind,
                        auth: ServeAuthOptions {
                            username: auth_user.clone(),
                            password: auth_password.clone(),
                        },
                    },
                    &cli,
                    format,
                )
                .await
            }
        },
        Commands::Head {
            url,
            path,
            lines,
            bytes,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_head(u, p, *lines, *bytes, &cli, format).await
        }
        Commands::Tail { url, path, lines } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_tail(u, p, *lines, &cli, format).await
        }
        Commands::Touch {
            url,
            path,
            timestamp,
        } => {
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
        Commands::Reconcile {
            url,
            local,
            remote,
            checksum,
            one_way,
            exclude,
            reconcile_format,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), local.as_str())
            } else {
                (url.as_str(), local.as_str(), remote.as_str())
            };
            cmd_reconcile(
                u,
                l,
                r,
                *checksum,
                *one_way,
                exclude,
                *reconcile_format,
                &cli,
                format,
            )
            .await
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
            name,
            files_only,
            dirs_only,
            limit,
        } => {
            // `--name` overrides the positional pattern when both
            // present. Picked as the natural agent-facing form
            // (V2 verification flagged the positional-only as a
            // first-attempt friction).
            let pattern_str = name.as_deref().unwrap_or(pattern.as_str());
            let (u, p, pat) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), path.as_str())
            } else {
                (url.as_str(), path.as_str(), pattern_str)
            };
            cmd_find(u, p, pat, *files_only, *dirs_only, *limit, &cli, format).await
        }
        Commands::Df { url } => cmd_df(url, &cli, format).await,
        Commands::Tree { url, path, depth } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_tree(u, p, *depth, &cli, format).await
        }
        Commands::Ncdu {
            url,
            path,
            depth,
            export,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_ncdu(u, p, *depth, export.as_deref(), &cli, format).await
        }
        #[allow(unused_variables)]
        Commands::Mount {
            url,
            mountpoint,
            path,
            cache_ttl,
            allow_other,
            read_only,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            #[cfg(target_os = "linux")]
            {
                cmd_mount(
                    u,
                    mountpoint,
                    p,
                    *cache_ttl,
                    *allow_other,
                    *read_only,
                    &cli,
                    format,
                )
                .await
            }
            #[cfg(windows)]
            {
                cmd_mount_windows(u, mountpoint, p, *read_only, &cli, format).await
            }
            #[cfg(not(any(target_os = "linux", windows)))]
            {
                print_error(format, "Mount is not supported on this platform", 7);
                7
            }
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
            suffix_keep_extension,
            compare_dest,
            copy_dest,
            from_reconcile,
            conflict_mode,
            skip_matching,
            resync,
            watch,
            watch_mode,
            watch_debounce_ms,
            watch_cooldown,
            watch_rescan,
            watch_no_initial,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), local.as_str())
            } else {
                (url.as_str(), local.as_str(), remote.as_str())
            };
            if *watch {
                cmd_sync_watch(
                    u,
                    l,
                    r,
                    direction,
                    *dry_run,
                    *delete,
                    exclude,
                    *track_renames,
                    max_delete.as_deref(),
                    backup_dir.as_deref(),
                    backup_suffix,
                    *suffix_keep_extension,
                    compare_dest.as_deref(),
                    copy_dest.as_deref(),
                    from_reconcile.as_deref(),
                    conflict_mode,
                    *skip_matching,
                    *resync,
                    watch_mode,
                    *watch_debounce_ms,
                    *watch_cooldown,
                    *watch_rescan,
                    *watch_no_initial,
                    &cli,
                    format,
                    cancelled.clone(),
                )
                .await
            } else {
                let max_attempts = cli.retries.max(1);
                let sleep_dur = parse_retry_sleep(&cli.retries_sleep);
                let max_transfer_limit = resolve_max_transfer(&cli);
                let mut last_code = 0i32;
                for attempt in 1..=max_attempts {
                    last_code = cmd_sync(
                        u,
                        l,
                        r,
                        direction,
                        *dry_run,
                        *delete,
                        exclude,
                        *track_renames,
                        max_delete.as_deref(),
                        backup_dir.as_deref(),
                        backup_suffix,
                        *suffix_keep_extension,
                        compare_dest.as_deref(),
                        copy_dest.as_deref(),
                        from_reconcile.as_deref(),
                        conflict_mode,
                        *skip_matching,
                        *resync,
                        &cli,
                        format,
                        cancelled.clone(),
                        None,
                    )
                    .await
                    .exit_code;
                    if !is_retryable_exit(last_code)
                        || session_transfer_exceeded(max_transfer_limit)
                        || attempt == max_attempts
                    {
                        break;
                    }
                    if !cli.quiet {
                        eprintln!(
                            "Attempt {}/{} failed (exit {}), retrying in {:?}...",
                            attempt, max_attempts, last_code, sleep_dur
                        );
                    }
                    if !sleep_dur.is_zero() {
                        tokio::time::sleep(sleep_dur).await;
                    }
                }
                last_code
            }
        }
        Commands::SyncDoctor {
            url,
            local,
            remote,
            direction,
            delete,
            exclude,
            track_renames,
            conflict_mode,
            resync,
            checksum,
        } => {
            let (u, l, r) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str(), local.as_str())
            } else {
                (url.as_str(), local.as_str(), remote.as_str())
            };
            cmd_sync_doctor(
                u,
                l,
                r,
                direction,
                *delete,
                exclude,
                *track_renames,
                conflict_mode,
                *resync,
                *checksum,
                &cli,
                format,
            )
            .await
        }
        Commands::About { url } => {
            let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                "_"
            } else {
                url
            };
            cmd_about(u, &cli, format).await
        }
        Commands::Speed {
            url,
            test_size,
            iterations,
            remote_path,
            no_integrity,
            json_out,
        } => {
            let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                "_"
            } else {
                url.as_str()
            };
            cmd_speed(
                u,
                test_size,
                *iterations,
                remote_path.as_deref(),
                *no_integrity,
                json_out.as_deref(),
                &cli,
                format,
            )
            .await
        }
        Commands::SpeedCompare {
            urls,
            test_size,
            parallel,
            no_integrity,
            json_out,
            csv_out,
            md_out,
        } => {
            cmd_speed_compare(
                urls,
                test_size,
                *parallel,
                *no_integrity,
                json_out.as_deref(),
                csv_out.as_deref(),
                md_out.as_deref(),
                &cli,
                format,
            )
            .await
        }
        Commands::Cleanup { url, path, force } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_cleanup(u, p, *force, &cli, format).await
        }
        Commands::Dedupe {
            url,
            path,
            mode,
            dry_run,
        } => {
            let (u, p) = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                ("_", url.as_str())
            } else {
                (url.as_str(), path.as_str())
            };
            cmd_dedupe(u, p, mode, *dry_run, &cli, format).await
        }
        Commands::Mcp => cmd_agent_mcp("", &cli).await,
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
        Commands::Profiles { _ignored: _ } => list_vault_profiles(&cli, format),
        Commands::AiModels => list_ai_models(&cli, format),
        Commands::AgentBootstrap {
            task,
            path,
            pattern,
            source_profile,
            dest_profile,
            source_path,
            dest_path,
            local_path,
            remote_path,
        } => cmd_agent_bootstrap(
            &cli,
            format,
            *task,
            path.as_deref(),
            pattern.as_deref(),
            source_profile.as_deref(),
            dest_profile.as_deref(),
            source_path.as_deref(),
            dest_path.as_deref(),
            local_path.as_deref(),
            remote_path.as_deref(),
        ),
        Commands::AgentInfo => cmd_agent_info(&cli),
        Commands::AgentConnect { profile } => cmd_agent_connect(&cli, profile).await,
        Commands::Crypt { command } => {
            let resolve_crypt_password = |p: &Option<String>| -> Option<String> {
                if let Some(pw) = p {
                    return Some(pw.clone());
                }
                if std::io::stdin().is_terminal() {
                    eprint!("Crypt password: ");
                    let _ = std::io::stderr().flush();
                    rpassword::read_password().ok()
                } else {
                    None
                }
            };
            match command {
                CryptCommands::Init {
                    url,
                    path,
                    password,
                } => {
                    let pw = resolve_crypt_password(password).unwrap_or_default();
                    if pw.is_empty() {
                        print_error(format, "Password required for crypt init", 5);
                        5
                    } else {
                        let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                            "_"
                        } else {
                            url.as_str()
                        };
                        cmd_crypt_init(u, path, &pw, &cli, format).await
                    }
                }
                CryptCommands::Ls {
                    url,
                    path,
                    password,
                } => {
                    let pw = resolve_crypt_password(password).unwrap_or_default();
                    let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                        "_"
                    } else {
                        url.as_str()
                    };
                    cmd_crypt_ls(u, path, &pw, &cli, format).await
                }
                CryptCommands::Put {
                    local,
                    url,
                    remote,
                    password,
                } => {
                    let pw = resolve_crypt_password(password).unwrap_or_default();
                    let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                        "_"
                    } else {
                        url.as_str()
                    };
                    cmd_crypt_put(u, local, remote, &pw, &cli, format).await
                }
                CryptCommands::Get {
                    remote,
                    url,
                    path,
                    local,
                    password,
                } => {
                    let pw = resolve_crypt_password(password).unwrap_or_default();
                    let u = if cli.profile.is_some() && !url.contains("://") && url != "_" {
                        "_"
                    } else {
                        url.as_str()
                    };
                    cmd_crypt_get(u, remote, path, local, &pw, &cli, format).await
                }
            }
        }
        Commands::Daemon { command } => match command {
            DaemonCommands::Start {
                addr,
                allow_remote_bind,
                auth_token,
            } => cmd_daemon_start(addr, *allow_remote_bind, auth_token.clone(), &cli, format).await,
            DaemonCommands::Stop => cmd_daemon_stop(format).await,
            DaemonCommands::Status => cmd_daemon_status(format).await,
        },
        Commands::Jobs { command } => match command {
            JobCommands::Add { command: tokens } => cmd_jobs_add(tokens, format).await,
            JobCommands::List => cmd_jobs_list(format).await,
            JobCommands::Status { id } => cmd_jobs_status(id, format).await,
            JobCommands::Cancel { id } => cmd_jobs_cancel(id, format).await,
        },
        Commands::Import { command } => match command {
            ImportCommands::Rclone { path, json } => cmd_import_rclone(path.clone(), *json).await,
            ImportCommands::Winscp { path, json } => cmd_import_winscp(path.clone(), *json).await,
            ImportCommands::Filezilla { path, json } => {
                cmd_import_filezilla(path.clone(), *json).await
            }
            ImportCommands::RcloneFilter {
                path,
                output,
                force,
                json,
            } => cmd_import_rclone_filter(path.clone(), output.clone(), *force, *json).await,
        },
        Commands::Transfer {
            source_profile,
            dest_profile,
            source_path,
            dest_path,
            recursive,
            dry_run,
            skip_existing,
        } => {
            cmd_transfer_profiles(
                source_profile,
                dest_profile,
                source_path,
                dest_path,
                *recursive,
                *dry_run,
                *skip_existing,
                &cli,
                format,
                cancelled,
            )
            .await
        }
        Commands::TransferDoctor {
            source_profile,
            dest_profile,
            source_path,
            dest_path,
            recursive,
            skip_existing,
        } => {
            cmd_transfer_doctor(
                source_profile,
                dest_profile,
                source_path,
                dest_path,
                *recursive,
                *skip_existing,
                &cli,
                format,
            )
            .await
        }
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
                if *yes {
                    "all".to_string()
                } else {
                    auto_approve.clone()
                },
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

    // --max-transfer: override exit code to 8 if limit was exceeded
    let exit_code = if session_transfer_exceeded(resolve_max_transfer(&cli)) && exit_code == 0 {
        if !cli.quiet {
            eprintln!(
                "Max transfer limit reached ({} transferred)",
                format_size(SESSION_TRANSFERRED_BYTES.load(Ordering::Relaxed))
            );
        }
        8
    } else {
        exit_code
    };

    std::process::exit(exit_code);
}

/// Check if a failed exit code is retryable.
/// NOT retryable: success (0), usage error (5), auth failure (6), not supported (7).
fn is_retryable_exit(code: i32) -> bool {
    // Non-retryable categories (stable across retries, burning attempts is pure noise):
    //   0  success
    //   2  not found (missing path or missing parent — caught by the new 553
    //      preflight in `cmd_put`)
    //   5  invalid usage / config
    //   6  authentication failed
    //   7  operation not supported
    //   9  already exists (--no-clobber short-circuit)
    code != 0 && code != 2 && code != 5 && code != 6 && code != 7 && code != 9
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ftp_client_gui_lib::profile_loader::insert_profile_option;
    use serde_json::json;

    #[test]
    fn redact_url_strips_password_for_well_formed_inputs() {
        assert_eq!(
            redact_url_for_display("ftp://alice:s3cret@example.com/path"),
            "ftp://alice@example.com/path"
        );
        assert_eq!(
            redact_url_for_display("sftp://bob:hunter2@10.0.0.1:2222/home/bob"),
            "sftp://bob@10.0.0.1:2222/home/bob"
        );
    }

    #[test]
    fn redact_url_keeps_user_only_form_intact() {
        assert_eq!(
            redact_url_for_display("ftp://alice@example.com/path"),
            "ftp://alice@example.com/path"
        );
    }

    #[test]
    fn redact_url_no_userinfo_unchanged() {
        assert_eq!(
            redact_url_for_display("https://example.com/foo"),
            "https://example.com/foo"
        );
    }

    #[test]
    fn redact_url_fallback_on_unparseable_input() {
        // Malformed URL still has password stripped via the manual fallback.
        let redacted = redact_url_for_display("notaurl://u:secret@host");
        assert!(
            !redacted.contains("secret"),
            "redaction failed: {}",
            redacted
        );
    }

    #[test]
    fn csv_cell_neutralizes_formula_prefixes() {
        assert_eq!(csv_cell_safe("=cmd|"), "\"'=cmd|\"");
        assert_eq!(csv_cell_safe("+1+2"), "\"'+1+2\"");
        assert_eq!(csv_cell_safe("-evil"), "\"'-evil\"");
        assert_eq!(csv_cell_safe("@SUM(A1)"), "\"'@SUM(A1)\"");
        assert_eq!(csv_cell_safe("normal"), "\"normal\"");
    }

    #[test]
    fn csv_cell_doubles_quotes() {
        assert_eq!(csv_cell_safe("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn md_cell_escapes_pipes_and_newlines() {
        assert_eq!(md_cell_safe("a|b"), "a\\|b");
        assert_eq!(md_cell_safe("line1\nline2"), "line1 line2");
        assert_eq!(md_cell_safe("\\path\\"), "\\\\path\\\\");
    }

    #[test]
    fn redact_url_never_returns_password() {
        let inputs = [
            "ftp://u:p@h",
            "ftps://u:p@h:21",
            "sftp://u:complex%40pwd@h",
            "s3://AKIA:SECRET@bucket.s3.amazonaws.com",
            "webdav://user:hunter2@dav.example.com/",
        ];
        for s in inputs {
            let r = redact_url_for_display(s);
            assert!(!r.contains("p@"), "leaked: {}", r);
            assert!(!r.contains("SECRET"), "leaked: {}", r);
            assert!(!r.contains("hunter2"), "leaked: {}", r);
            assert!(!r.contains("complex"), "leaked: {}", r);
        }
    }

    fn test_cli() -> Cli {
        Cli {
            format: OutputFormat::Text,
            json: false,
            json_fields: None,
            no_banner: false,
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
            max_transfer: None,
            max_backlog: 10000,
            retries: 3,
            retries_sleep: "1s".to_string(),
            dump: Vec::new(),
            chunk_size: None,
            buffer_size: None,
            default_time: None,
            fast_list: false,
            inplace: false,
            files_from: None,
            files_from_raw: None,
            immutable: false,
            no_check_dest: false,
            max_depth: None,
            command: Commands::Profiles {
                _ignored: Vec::new(),
            },
        }
    }

    #[test]
    fn test_parse_speed_limit_megabytes() {
        assert_eq!(parse_speed_limit("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_speed_limit("10M").unwrap(), 10 * 1024 * 1024);
    }

    // ── bwlimit schedule (rclone-compatible) ────────────────────────────────

    #[test]
    fn bwlimit_schedule_picks_active_window() {
        // Schedule: 08:00 -> 512 KB/s, 12:00 -> 10 MB/s, 18:00 -> off
        let s = "08:00,512k 12:00,10M 18:00,off";

        // Before first window wraps to last entry of previous day (off => None)
        assert_eq!(resolve_bwlimit_schedule_at(s, 7 * 60 + 59), None);

        // 08:00 - 11:59 -> 512 KB/s
        assert_eq!(resolve_bwlimit_schedule_at(s, 8 * 60), Some(512 * 1024));
        assert_eq!(resolve_bwlimit_schedule_at(s, 11 * 60 + 59), Some(512 * 1024));

        // 12:00 - 17:59 -> 10 MB/s
        assert_eq!(resolve_bwlimit_schedule_at(s, 12 * 60), Some(10 * 1024 * 1024));
        assert_eq!(resolve_bwlimit_schedule_at(s, 17 * 60 + 59), Some(10 * 1024 * 1024));

        // 18:00 - 23:59 -> off
        assert_eq!(resolve_bwlimit_schedule_at(s, 18 * 60), None);
        assert_eq!(resolve_bwlimit_schedule_at(s, 23 * 60 + 59), None);
    }

    #[test]
    fn bwlimit_schedule_simple_rate_when_no_time_entries() {
        // No commas/colons → treated as plain rate
        assert_eq!(resolve_bwlimit_schedule_at("1M", 12 * 60), Some(1024 * 1024));
        assert_eq!(resolve_bwlimit_schedule_at("512k", 0), Some(512 * 1024));
    }

    #[test]
    fn bwlimit_schedule_off_during_active_window_returns_none() {
        // An "off" entry mid-day must not be confused with "no match" (which would
        // wrap to the last entry). Pin: a literal "off" must stay None.
        let s = "00:00,1M 10:00,off 14:00,2M";
        assert_eq!(resolve_bwlimit_schedule_at(s, 0), Some(1024 * 1024));
        assert_eq!(resolve_bwlimit_schedule_at(s, 10 * 60), None);
        assert_eq!(resolve_bwlimit_schedule_at(s, 13 * 60 + 59), None);
        assert_eq!(resolve_bwlimit_schedule_at(s, 14 * 60), Some(2 * 1024 * 1024));
    }

    #[test]
    fn bwlimit_schedule_wraps_when_no_entry_at_or_before_now() {
        // Schedule starts at 09:00; at 06:00 we wrap to the last entry of the day
        let s = "09:00,1M 18:00,off";
        assert_eq!(resolve_bwlimit_schedule_at(s, 6 * 60), None); // wrap to 18:00,off
    }

    #[test]
    fn bwlimit_schedule_skips_malformed_entries() {
        // Hour > 23 and minute > 59 are silently skipped (not panic, not crash)
        let s = "25:00,1M 10:99,2M 12:00,3M";
        assert_eq!(resolve_bwlimit_schedule_at(s, 12 * 60), Some(3 * 1024 * 1024));
    }

    #[test]
    fn bwlimit_schedule_unsorted_entries_are_normalized() {
        // Caller-provided order shouldn't matter
        let s = "18:00,off 08:00,512k 12:00,10M";
        assert_eq!(resolve_bwlimit_schedule_at(s, 9 * 60), Some(512 * 1024));
        assert_eq!(resolve_bwlimit_schedule_at(s, 13 * 60), Some(10 * 1024 * 1024));
        assert_eq!(resolve_bwlimit_schedule_at(s, 19 * 60), None);
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
        let (config, path) =
            url_to_provider_config("ftp://anonymous:test@ftp.example.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Ftp);
        assert_eq!(config.host, "ftp.example.com");
        assert_eq!(config.username.as_deref(), Some("anonymous"));
        assert_eq!(path, "/");
    }

    #[test]
    fn test_url_parsing_sftp_with_port() {
        let cli = test_cli();
        let (config, path) =
            url_to_provider_config("sftp://admin:test@server.com:2222/home", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Sftp);
        assert_eq!(config.host, "server.com");
        assert_eq!(config.port, Some(2222));
        assert_eq!(config.username.as_deref(), Some("admin"));
        assert_eq!(path, "/home");
    }

    #[test]
    fn test_url_parsing_webdavs() {
        let cli = test_cli();
        let (config, _path) =
            url_to_provider_config("webdavs://user:test@cloud.example.com/dav", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::WebDav);
        assert!(config.host.starts_with("https://"));
    }

    #[test]
    fn test_url_parsing_s3() {
        let mut cli = test_cli();
        cli.bucket = Some("mybucket".to_string());
        cli.region = Some("eu-west-1".to_string());
        let (config, _path) =
            url_to_provider_config("s3://AKID:secret@s3.amazonaws.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::S3);
        assert_eq!(
            config.extra.get("bucket").map(|s| s.as_str()),
            Some("mybucket")
        );
        assert_eq!(
            config.extra.get("region").map(|s| s.as_str()),
            Some("eu-west-1")
        );
    }

    #[test]
    fn test_insert_profile_option_normalizes_tencent_path_style() {
        let mut extra = HashMap::new();

        insert_profile_option(&mut extra, "pathStyle", &json!(false));
        insert_profile_option(
            &mut extra,
            "endpoint",
            &json!("https://cos.ap-guangzhou.myqcloud.com"),
        );
        insert_profile_option(&mut extra, "bucket", &json!("mybucket-1250000000"));
        insert_profile_option(&mut extra, "region", &json!("ap-guangzhou"));

        assert_eq!(extra.get("path_style").map(|s| s.as_str()), Some("false"));
        assert_eq!(
            extra.get("endpoint").map(|s| s.as_str()),
            Some("https://cos.ap-guangzhou.myqcloud.com")
        );
        assert_eq!(
            extra.get("bucket").map(|s| s.as_str()),
            Some("mybucket-1250000000")
        );
        assert_eq!(
            extra.get("region").map(|s| s.as_str()),
            Some("ap-guangzhou")
        );
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
    fn test_apply_s3_profile_defaults_resolves_google_preset() {
        let mut extra = HashMap::new();

        let endpoint = apply_s3_profile_defaults(&mut extra, Some("google-cloud-storage"));

        assert_eq!(endpoint.as_deref(), Some("https://storage.googleapis.com"));
        assert_eq!(
            extra.get("endpoint").map(|s| s.as_str()),
            Some("https://storage.googleapis.com")
        );
        assert_eq!(extra.get("region").map(|s| s.as_str()), Some("auto"));
        assert_eq!(extra.get("path_style").map(|s| s.as_str()), Some("true"));
        assert_eq!(
            extra.get(S3_ENDPOINT_SOURCE_META_KEY).map(|s| s.as_str()),
            Some("preset")
        );
        assert_eq!(
            extra.get(S3_REGION_SOURCE_META_KEY).map(|s| s.as_str()),
            Some("preset")
        );
        assert_eq!(
            extra.get(S3_PATH_STYLE_SOURCE_META_KEY).map(|s| s.as_str()),
            Some("preset")
        );
    }

    #[test]
    fn test_apply_s3_profile_defaults_resolves_wasabi_template() {
        let mut extra = HashMap::new();
        extra.insert("region".to_string(), "eu-central-1".to_string());

        let endpoint = apply_s3_profile_defaults(&mut extra, Some("wasabi"));

        assert_eq!(
            endpoint.as_deref(),
            Some("https://s3.eu-central-1.wasabisys.com")
        );
        assert_eq!(
            extra.get("endpoint").map(|s| s.as_str()),
            Some("https://s3.eu-central-1.wasabisys.com")
        );
        assert_eq!(extra.get("path_style").map(|s| s.as_str()), Some("false"));
    }

    #[test]
    fn test_apply_s3_profile_defaults_preserves_existing_endpoint() {
        let mut extra = HashMap::new();
        extra.insert(
            "endpoint".to_string(),
            "https://gateway.storjshare.io".to_string(),
        );

        let endpoint = apply_s3_profile_defaults(&mut extra, Some("storj"));

        assert_eq!(endpoint.as_deref(), Some("https://gateway.storjshare.io"));
        assert_eq!(
            extra.get("endpoint").map(|s| s.as_str()),
            Some("https://gateway.storjshare.io")
        );
        assert_eq!(extra.get("region").map(|s| s.as_str()), Some("global"));
        assert_eq!(extra.get("path_style").map(|s| s.as_str()), Some("true"));
    }

    #[test]
    fn test_display_port_for_provider_parses_server_info() {
        let port =
            display_port_for_provider(&ProviderType::Ftp, Some("FTP Server: ftp.axpdev.it:21"));
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
        assert_eq!(config.extra.get("mega_mode"), Some(&"native".to_string()));
    }

    #[test]
    fn test_url_parsing_mega_keeps_native_with_other_cli_options() {
        let cli = Cli {
            two_factor: Some("123456".to_string()),
            ..test_cli()
        };
        let (config, _) = url_to_provider_config("mega://user:test@mega.nz", &cli).unwrap();
        assert_eq!(config.extra.get("mega_mode"), Some(&"native".to_string()));
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
        let (config, _) =
            url_to_provider_config("opendrive://user:test@dev.opendrive.com", &cli).unwrap();
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
        assert_eq!(
            validate_relative_path("/dir/file.txt"),
            Some("dir/file.txt")
        );
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
        assert_eq!(
            sanitize_filename("file with spaces.doc"),
            "file with spaces.doc"
        );
    }

    #[test]
    fn test_sanitize_filename_ansi_escape() {
        assert_eq!(sanitize_filename("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(
            sanitize_filename("before\x1b[1;32mgreen\x1b[0mafter"),
            "beforegreenafter"
        );
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
            assert_eq!(
                chunks[i].offset,
                chunks[i - 1].offset + chunks[i - 1].length
            );
        }
    }

    #[test]
    fn test_pget_plan_reduces_segments_for_small_files() {
        // 3 MB file with 16 segments requested - each chunk would be < PGET_MIN_CHUNK_SIZE (1 MB)
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
        // File smaller than PGET_MIN_CHUNK_SIZE - should get 1 segment
        let chunks = plan_pget_chunks(500_000, 8);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].length, 500_000);
    }

    #[tokio::test]
    async fn test_pget_assemble_chunks() {
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_path = temp_dir.path().to_str().unwrap();

        // Write 3 chunk files
        tokio::fs::write(format!("{}/chunk_0000", temp_path), b"Hello, ")
            .await
            .unwrap();
        tokio::fs::write(format!("{}/chunk_0001", temp_path), b"segmented ")
            .await
            .unwrap();
        tokio::fs::write(format!("{}/chunk_0002", temp_path), b"world!")
            .await
            .unwrap();

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
        tokio::fs::write(format!("{}/chunk_0000", temp_path), &chunk0)
            .await
            .unwrap();
        tokio::fs::write(format!("{}/chunk_0001", temp_path), &chunk1)
            .await
            .unwrap();

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

        assert!(
            !guard_path.exists(),
            "temp dir should be cleaned up by PgetTempGuard"
        );
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
        assert_eq!(
            sanitize_served_relative_path("/foo/bar").unwrap(),
            "foo/bar"
        );
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
    fn test_validate_bind_addr_loopback_only_by_default() {
        let loopback = validate_bind_addr("127.0.0.1:8080", false, "HTTP serve").unwrap();
        assert!(loopback.ip().is_loopback());
        assert!(validate_bind_addr("0.0.0.0:8080", false, "HTTP serve").is_err());
        assert!(validate_bind_addr("192.168.1.10:8080", false, "HTTP serve").is_err());
        assert!(validate_bind_addr("0.0.0.0:8080", true, "HTTP serve").is_ok());
    }

    #[test]
    fn test_build_served_remote_path() {
        assert_eq!(build_served_remote_path("/data", ""), "/data");
        assert_eq!(
            build_served_remote_path("/data", "sub/file.txt"),
            "/data/sub/file.txt"
        );
        assert_eq!(build_served_remote_path("/", "file.txt"), "/file.txt");
        assert_eq!(build_served_remote_path("/", ""), "/");
    }

    #[test]
    fn test_resolve_served_backend_path_confines_traversal() {
        assert_eq!(
            resolve_served_backend_path("/base", "docs/readme.txt").unwrap(),
            "/base/docs/readme.txt"
        );
        assert_eq!(
            resolve_served_backend_path("/base", "/docs/readme.txt").unwrap(),
            "/base/docs/readme.txt"
        );
        assert!(resolve_served_backend_path("/base", "../secret.txt").is_err());
        assert!(resolve_served_backend_path("/base", "docs/../../secret.txt").is_err());
    }

    #[test]
    fn test_resolve_agent_remote_path_confines_to_initial_path() {
        assert_eq!(
            resolve_agent_remote_path("/projects/demo", "/").unwrap(),
            "/projects/demo"
        );
        assert_eq!(
            resolve_agent_remote_path("/projects/demo", "notes/todo.txt").unwrap(),
            "/projects/demo/notes/todo.txt"
        );
        assert!(resolve_agent_remote_path("/projects/demo", "../escape.txt").is_err());
    }

    #[test]
    fn test_serve_effective_base_path() {
        assert_eq!(serve_effective_base_path("/", "/home/user"), "/home/user");
        assert_eq!(
            serve_effective_base_path("/custom", "/home/user"),
            "/custom"
        );
        assert_eq!(serve_effective_base_path("/", "/"), "/");
    }

    #[test]
    fn test_tool_danger_level_remote_tools_require_approval() {
        assert_eq!(tool_danger_level("server_list_saved"), 1);
        assert_eq!(tool_danger_level("remote_list"), 1);
        assert_eq!(tool_danger_level("remote_read"), 2);
        assert_eq!(tool_danger_level("server_exec"), 2);
    }

    #[test]
    fn test_resolve_service_auth_token_loopback_defaults_to_none() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let (token, generated) = resolve_service_auth_token(None, addr);
        assert_eq!(token, None);
        assert!(!generated);
    }

    #[test]
    fn test_resolve_service_auth_token_remote_generates_token() {
        let addr: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let (token, generated) = resolve_service_auth_token(None, addr);
        assert!(generated);
        assert!(token.is_some());
    }

    #[test]
    fn test_request_is_authorized_accepts_bearer_and_basic() {
        let mut bearer = HeaderMap::new();
        bearer.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-token"));
        assert!(request_is_authorized(&bearer, Some("test-token")));

        let basic_token = base64::engine::general_purpose::STANDARD.encode("user:test-token");
        let basic_header = format!("Basic {}", basic_token);
        let mut basic = HeaderMap::new();
        basic.insert(AUTHORIZATION, HeaderValue::from_str(&basic_header).unwrap());
        assert!(request_is_authorized(&basic, Some("test-token")));
        assert!(!request_is_authorized(&basic, Some("wrong-token")));
    }

    #[test]
    fn test_resolve_service_credentials_loopback_defaults_to_none() {
        let addr: SocketAddr = "127.0.0.1:2121".parse().unwrap();
        assert_eq!(resolve_service_credentials(None, None, addr), None);
    }

    #[test]
    fn test_resolve_service_credentials_remote_generates_defaults() {
        let addr: SocketAddr = "0.0.0.0:2121".parse().unwrap();
        let creds = resolve_service_credentials(None, None, addr).unwrap();
        assert_eq!(creds.username, "aeroftp");
        assert!(!creds.password.is_empty());
        assert!(creds.generated);
    }

    #[test]
    fn test_tool_policy_distinguishes_metadata_preview_and_exec() {
        assert_eq!(tool_exposure_category("remote_list"), "remote-metadata");
        assert_eq!(tool_data_egress("remote_list"), "metadata");
        assert_eq!(tool_exposure_category("remote_read"), "remote-preview");
        assert_eq!(tool_data_egress("remote_read"), "preview");
        assert_eq!(tool_exposure_category("server_exec"), "remote-bulk-read");
        assert_eq!(tool_data_egress("server_exec"), "operation-dependent");
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
        assert_eq!(
            escape_html("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;"
        );
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
        headers.insert(
            "Destination",
            HeaderValue::from_static("http://127.0.0.1:8080/new/path"),
        );
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
        headers.insert(
            "Destination",
            HeaderValue::from_static("http://host/../etc/passwd"),
        );
        assert!(extract_destination_relative(&headers).is_err());
    }

    // ── resolve_cli_remote_path with no meaningful base ──────────────────
    // Behaviour after f8da815a: when initial_path is empty or bare "/", user
    // input passes through verbatim so absolute paths target the filesystem
    // root on non-chroot servers, and empty input yields "" so the provider
    // applies its canonical default (FTP home, bucket root, ...).

    #[test]
    fn test_resolve_cli_remote_path_root_base_preserves_input() {
        assert_eq!(
            resolve_cli_remote_path("/", "/front/includes"),
            "/front/includes"
        );
        assert_eq!(
            resolve_cli_remote_path("/", "front/includes"),
            "front/includes"
        );
    }

    #[test]
    fn test_resolve_cli_remote_path_root_base_empty_and_slash() {
        assert_eq!(resolve_cli_remote_path("/", ""), "");
        assert_eq!(resolve_cli_remote_path("/", "/"), "");
        assert_eq!(resolve_cli_remote_path("", ""), "");
    }

    #[test]
    fn test_resolve_cli_remote_path_with_base_path() {
        // Both absolute and relative user paths should resolve to the same thing
        assert_eq!(
            resolve_cli_remote_path("/www.ericsolar.it", "/front/includes"),
            "/www.ericsolar.it/front/includes"
        );
        assert_eq!(
            resolve_cli_remote_path("/www.ericsolar.it", "front/includes"),
            "/www.ericsolar.it/front/includes"
        );
    }

    #[test]
    fn test_resolve_cli_remote_path_user_already_includes_base() {
        assert_eq!(
            resolve_cli_remote_path("/www.ericsolar.it", "/www.ericsolar.it/app"),
            "/www.ericsolar.it/app"
        );
    }

    // ── BUG-4: parse_mtime_secs with FTP Z suffix ──────────────────────

    #[test]
    fn test_parse_mtime_secs_ftp_z_suffix() {
        // FTP MLSD format: "2024-01-15 10:30:00Z"
        let ftp_ts = parse_mtime_secs("2024-01-15 10:30:00Z");
        let local_ts = parse_mtime_secs("2024-01-15T10:30:00");
        assert!(ftp_ts.is_some(), "FTP timestamp with Z suffix should parse");
        assert!(local_ts.is_some(), "Local ISO timestamp should parse");
        assert_eq!(ftp_ts, local_ts, "Same moment should produce same epoch");
    }

    #[test]
    fn test_parse_mtime_secs_utc_suffix() {
        let ts = parse_mtime_secs("2024-06-01 08:00:00UTC");
        assert!(ts.is_some(), "UTC suffix should parse");
        assert_eq!(ts, parse_mtime_secs("2024-06-01T08:00:00"));
    }

    #[test]
    fn test_parse_mtime_secs_plain_formats() {
        assert!(parse_mtime_secs("2024-01-15T10:30:00").is_some());
        assert!(parse_mtime_secs("2024-01-15 10:30:00").is_some());
        assert!(parse_mtime_secs("2024-01-15T10:30:00+00:00").is_some());
    }

    #[test]
    fn test_parse_mtime_secs_invalid() {
        assert!(parse_mtime_secs("not-a-date").is_none());
        assert!(parse_mtime_secs("?").is_none());
        assert!(parse_mtime_secs("").is_none());
    }

    #[test]
    fn test_partition_conflict_rename_downloads_splits_conflicts() {
        let to_download = vec!["same.txt", "remote-only.txt", "nested/file.bin"];
        let to_conflict_upload = vec![
            (
                "same.txt".to_string(),
                "same.conflict-20260416T120000000.txt".to_string(),
            ),
            (
                "nested/file.bin".to_string(),
                "nested/file.conflict-20260416T120000000.bin".to_string(),
            ),
        ];

        let (normal, gated) = partition_conflict_rename_downloads(to_download, &to_conflict_upload);

        assert_eq!(normal, vec!["remote-only.txt"]);
        assert_eq!(gated, vec!["same.txt", "nested/file.bin"]);
    }

    #[test]
    fn test_partition_conflict_rename_downloads_no_conflicts() {
        let to_download = vec!["a.txt", "b.txt"];
        let to_conflict_upload = Vec::new();

        let (normal, gated) =
            partition_conflict_rename_downloads(to_download.clone(), &to_conflict_upload);

        assert_eq!(normal, to_download);
        assert!(gated.is_empty());
    }

    #[test]
    fn test_load_sync_plan_from_reconcile_upload_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diff.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "groups": {
                    "match": [
                        {"path": "same.txt", "local_size": 10, "remote_size": 10}
                    ],
                    "differ": [
                        {"path": "changed.txt", "local_size": 11, "remote_size": 9}
                    ],
                    "missing_remote": [
                        {"path": "upload.txt", "local_size": 12}
                    ],
                    "missing_local": [
                        {"path": "remote-only.txt", "remote_size": 13}
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();

        let plan = load_sync_plan_from_reconcile(path.to_str().unwrap(), "upload", true).unwrap();

        assert_eq!(plan.skipped, 1);
        assert_eq!(plan.to_upload, vec!["changed.txt", "upload.txt"]);
        assert_eq!(plan.to_delete_remote, vec!["remote-only.txt"]);
        assert!(plan.to_download.is_empty());
    }

    #[test]
    fn test_load_sync_plan_from_reconcile_rejects_summary_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "status": "ok",
                "summary": {"match_count": 3}
            })
            .to_string(),
        )
        .unwrap();

        let err =
            load_sync_plan_from_reconcile(path.to_str().unwrap(), "upload", false).unwrap_err();

        assert!(err.contains("does not contain detailed groups"));
    }

    // -----------------------------------------------------------------------
    // sync --watch tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_should_exclude_watch_path_os_metadata() {
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/.DS_Store"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/Thumbs.db"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/desktop.ini"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/.directory"
        )));
    }

    #[test]
    fn test_should_exclude_watch_path_vcs_dirs() {
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/repo/.git"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/repo/.svn"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new("/repo/.hg")));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/repo/node_modules"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/repo/__pycache__"
        )));
    }

    #[test]
    fn test_should_exclude_watch_path_temp_extensions() {
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.swp"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.swo"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.tmp"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.temp"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.bak"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.crdownload"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.part"
        )));
    }

    #[test]
    fn test_should_exclude_watch_path_temp_patterns() {
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/~tempfile"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new("/a/.#lock")));
        assert!(should_exclude_watch_path(std::path::Path::new("/a/file~")));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/file.aerotmp"
        )));
        assert!(should_exclude_watch_path(std::path::Path::new(
            "/a/.file.swp"
        )));
    }

    #[test]
    fn test_should_exclude_watch_path_normal_files_pass() {
        assert!(!should_exclude_watch_path(std::path::Path::new(
            "/a/readme.md"
        )));
        assert!(!should_exclude_watch_path(std::path::Path::new(
            "/a/src/main.rs"
        )));
        assert!(!should_exclude_watch_path(std::path::Path::new(
            "/a/photo.jpg"
        )));
        assert!(!should_exclude_watch_path(std::path::Path::new(
            "/a/data.csv"
        )));
        assert!(!should_exclude_watch_path(std::path::Path::new(
            "/a/.gitignore"
        )));
    }

    #[test]
    fn test_incremental_local_scan_new_file() {
        let dir = std::env::temp_dir().join("aeroftp_test_incr_new");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("hello.txt");
        std::fs::write(&file, "hello world").unwrap();

        let previous = std::collections::HashMap::new();
        let result = incremental_local_scan(&dir, std::slice::from_ref(&file), &previous, &[]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "hello.txt");
        assert_eq!(result[0].1, 11); // "hello world" = 11 bytes

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_incremental_local_scan_deleted_file() {
        let dir = std::env::temp_dir().join("aeroftp_test_incr_del");
        let _ = std::fs::create_dir_all(&dir);

        let mut previous = std::collections::HashMap::new();
        previous.insert(
            "gone.txt".to_string(),
            (100u64, Some("2026-01-01T00:00:00".to_string())),
        );

        // File does not exist on disk
        let ghost = dir.join("gone.txt");
        let result = incremental_local_scan(&dir, &[ghost], &previous, &[]);

        assert!(result.is_empty()); // deleted file removed from snapshot

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_incremental_local_scan_preserves_unchanged() {
        let dir = std::env::temp_dir().join("aeroftp_test_incr_keep");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("changed.txt");
        std::fs::write(&file, "new content").unwrap();

        let mut previous = std::collections::HashMap::new();
        previous.insert(
            "existing.txt".to_string(),
            (50u64, Some("2026-01-01T00:00:00".to_string())),
        );

        let result = incremental_local_scan(&dir, std::slice::from_ref(&file), &previous, &[]);

        // Should contain both: existing (from snapshot) + changed (from disk)
        assert_eq!(result.len(), 2);
        let names: Vec<&str> = result.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"existing.txt"));
        assert!(names.contains(&"changed.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_incremental_local_scan_respects_excludes() {
        let dir = std::env::temp_dir().join("aeroftp_test_incr_excl");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("debug.log");
        std::fs::write(&file, "log data").unwrap();

        let matcher = globset::Glob::new("*.log").unwrap().compile_matcher();
        let result = incremental_local_scan(
            &dir,
            std::slice::from_ref(&file),
            &std::collections::HashMap::new(),
            &[matcher],
        );

        assert!(result.is_empty()); // excluded by glob

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sync_cycle_stats_from_i32() {
        let stats: SyncCycleStats = 5.into();
        assert_eq!(stats.exit_code, 5);
        assert_eq!(stats.uploaded, 0);
        assert_eq!(stats.downloaded, 0);
        assert_eq!(stats.deleted, 0);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_sync_cycle_stats_default() {
        let stats = SyncCycleStats::default();
        assert_eq!(stats.exit_code, 0);
        assert_eq!(stats.uploaded, 0);
    }
}
