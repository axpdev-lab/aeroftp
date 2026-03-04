//! AeroFTP CLI — Production multi-protocol file transfer client
//!
//! Usage:
//!   aeroftp connect <url>                     Test connection
//!   aeroftp ls <url> [path] [-l]              List files
//!   aeroftp get <url> <remote> [local] [-r]   Download file(s)
//!   aeroftp put <url> <local> [remote] [-r]   Upload file(s)
//!   aeroftp mkdir <url> <path>                Create directory
//!   aeroftp rm <url> <path> [-rf]             Delete file/directory
//!   aeroftp mv <url> <from> <to>              Rename/move
//!   aeroftp cat <url> <path>                  Print to stdout
//!   aeroftp stat <url> <path>                 File metadata
//!   aeroftp find <url> <path> <pattern>       Search files
//!   aeroftp df <url>                          Storage quota
//!   aeroftp sync <url> <local> <remote>       Sync directories
//!   aeroftp batch <file>                      Execute .aeroftp script
//!
//! URL format: protocol://user:pass@host:port/path
//! Add --json for machine-readable output.

use clap::{Parser, Subcommand, ValueEnum};
use ftp_client_gui_lib::providers::{
    ProviderConfig, ProviderError, ProviderFactory, ProviderType, RemoteEntry, StorageProvider,
};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write as IoWrite};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

// ── CLI Argument Parsing ───────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "aeroftp",
    about = "AeroFTP CLI — Multi-protocol file transfer client",
    version,
    long_about = "Supports FTP, FTPS, SFTP, WebDAV, S3, MEGA, Azure, Filen, Internxt, Jottacloud, FileLu, Koofr.\n\nURL format: protocol://user@host:port/path",
    after_help = "EXAMPLES:\n  aeroftp connect sftp://user@myserver.com\n  aeroftp ls sftp://user@myserver.com /var/www/ -l\n  aeroftp get sftp://user@myserver.com /backup.tar.gz\n  aeroftp put sftp://user@myserver.com ./deploy.zip /var/www/\n  aeroftp cat sftp://user@myserver.com /etc/config.ini | grep DB_HOST\n  aeroftp sync sftp://user@myserver.com ./local/ /remote/ --dry-run"
)]
struct Cli {
    /// Output format
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Shorthand for --format json
    #[arg(long, global = true)]
    json: bool,

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
    #[arg(long, global = true)]
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
    #[arg(long, global = true)]
    two_factor: Option<String>,

    /// Verbose output (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Quiet mode (errors only)
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Maximum retries on failure (default: 0)
    #[arg(long, global = true, default_value = "0")]
    retries: u32,

    /// Speed limit (e.g., "1M", "500K")
    #[arg(long, global = true)]
    limit_rate: Option<String>,

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

#[derive(Subcommand)]
enum Commands {
    /// Test connection to a remote server
    Connect {
        /// Server URL (e.g., sftp://user@host:22)
        url: String,
    },
    /// List files on a remote server
    Ls {
        /// Server URL
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
        /// Server URL
        url: String,
        /// Remote file path (supports glob patterns like "*.csv")
        remote: String,
        /// Local destination (default: current filename)
        local: Option<String>,
        /// Recursive download (directories)
        #[arg(short, long)]
        recursive: bool,
    },
    /// Upload file(s) to remote server
    Put {
        /// Server URL
        url: String,
        /// Local file path
        local: String,
        /// Remote destination path
        remote: Option<String>,
        /// Recursive upload (directories)
        #[arg(short, long)]
        recursive: bool,
    },
    /// Create a remote directory
    Mkdir {
        /// Server URL
        url: String,
        /// Remote directory path
        path: String,
    },
    /// Delete a remote file or directory
    Rm {
        /// Server URL
        url: String,
        /// Remote path to delete
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
        /// Server URL
        url: String,
        /// Source path
        from: String,
        /// Destination path
        to: String,
    },
    /// Print remote file to stdout (for piping)
    Cat {
        /// Server URL
        url: String,
        /// Remote file path
        path: String,
    },
    /// Show file/directory metadata
    Stat {
        /// Server URL
        url: String,
        /// Remote path
        path: String,
    },
    /// Search for files by pattern
    Find {
        /// Server URL
        url: String,
        /// Base path to search from
        path: String,
        /// Search pattern (glob-style)
        pattern: String,
    },
    /// Show storage quota/usage
    Df {
        /// Server URL
        url: String,
    },
    /// Synchronize local and remote directories
    Sync {
        /// Server URL
        url: String,
        /// Local directory path
        local: String,
        /// Remote directory path
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
    },
    /// Execute commands from a batch script (.aeroftp file)
    Batch {
        /// Path to .aeroftp script file
        file: String,
    },
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
struct CliLsResult {
    status: &'static str,
    path: String,
    entries: Vec<CliFileEntry>,
    summary: LsSummary,
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
struct LsSummary {
    total: usize,
    files: usize,
    dirs: usize,
    total_bytes: u64,
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
struct CliStatResult {
    status: &'static str,
    entry: CliFileEntry,
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

// ── Helpers ────────────────────────────────────────────────────────

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn print_error(format: OutputFormat, msg: &str, code: i32) {
    match format {
        OutputFormat::Text => eprintln!("Error: {}", msg),
        OutputFormat::Json => print_json(&CliError {
            status: "error",
            error: msg.to_string(),
            code,
        }),
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

fn create_progress_bar(filename: &str, total: u64) -> ProgressBar {
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

    // 2. --password-stdin
    if cli.password_stdin {
        let mut password = String::new();
        io::stdin()
            .read_line(&mut password)
            .map_err(|e| format!("Failed to read password from stdin: {}", e))?;
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
        if cli.verbose > 0 {
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
        _ => return Err(format!("Unsupported protocol: {}. Supported: ftp, ftps, sftp, webdav, webdavs, s3, mega, azure, filen, internxt, jottacloud, filelu, koofr", scheme)),
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

    // For WebDAV, the URL path is already part of the host — initial_path is always /
    let url_path = match provider_type {
        ProviderType::WebDav => "/".to_string(),
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

    // FTP TLS
    if let Some(ref tls) = cli.tls {
        extra.insert("tls_mode".to_string(), tls.clone());
    } else if provider_type == ProviderType::Ftps {
        extra.insert("tls_mode".to_string(), "implicit".to_string());
    }

    // TLS cert verification
    if cli.insecure {
        extra.insert("verify_cert".to_string(), "false".to_string());
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

async fn create_and_connect(
    url: &str,
    cli: &Cli,
    format: OutputFormat,
) -> Result<(Box<dyn StorageProvider>, String), i32> {
    let (config, path) = match url_to_provider_config(url, cli) {
        Ok(v) => v,
        Err(e) => {
            print_error(format, &e, 5);
            return Err(5);
        }
    };

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

    Ok((provider, path))
}

// ── Command Handlers ───────────────────────────────────────────────

async fn cmd_connect(url: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let start = Instant::now();
    let spinner = if matches!(format, OutputFormat::Text) && !cli.quiet {
        Some(create_spinner("Connecting..."))
    } else {
        None
    };

    let (config, _path) = match url_to_provider_config(url, cli) {
        Ok(v) => v,
        Err(e) => {
            if let Some(sp) = spinner { sp.finish_and_clear(); }
            print_error(format, &e, 5);
            return 5;
        }
    };

    let pt = config.provider_type;
    let host = config.host.clone();
    let port = config.effective_port();
    let user = config.username.clone().unwrap_or_default();

    let mut provider = match ProviderFactory::create(&config) {
        Ok(p) => p,
        Err(e) => {
            if let Some(sp) = spinner { sp.finish_and_clear(); }
            print_error(format, &format!("Failed to create provider: {}", e), provider_error_to_exit_code(&e));
            return provider_error_to_exit_code(&e);
        }
    };

    if let Err(e) = provider.connect().await {
        if let Some(sp) = spinner { sp.finish_and_clear(); }
        print_error(format, &format!("Connection failed: {}", e), provider_error_to_exit_code(&e));
        return provider_error_to_exit_code(&e);
    }

    let elapsed = start.elapsed();
    let server_info = provider.server_info().await.ok();

    if let Some(sp) = spinner { sp.finish_and_clear(); }

    match format {
        OutputFormat::Text => {
            println!("Connected to {} ({})", host, pt);
            println!("  User:     {}", user);
            println!("  Port:     {}", port);
            println!("  Protocol: {}", pt);
            if let Some(ref info) = server_info {
                if !info.is_empty() {
                    println!("  Server:   {}", info);
                }
            }
            println!("  Time:     {:.0}ms", elapsed.as_millis());

            // Try to show storage info
            if let Ok(storage) = provider.storage_info().await {
                if storage.total > 0 {
                    let pct = if storage.total > 0 {
                        (storage.used as f64 / storage.total as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!(
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
                    // Truncate date to first 16 chars (YYYY-MM-DD HH:MM)
                    let date_short = if date.len() > 16 { &date[..16] } else { date };
                    let name = if e.is_dir {
                        format!("{}/", e.name)
                    } else {
                        e.name.clone()
                    };
                    println!("{}  {}  {}  {}", perms, size_str, date_short, name);
                }
            } else {
                // Short format: just names
                for e in &entries {
                    if e.is_dir {
                        println!("{}/", e.name);
                    } else {
                        println!("{}", e.name);
                    }
                }
            }

            if !cli.quiet {
                println!(
                    "\n{} items ({} directories, {} files) — {} total",
                    entries.len(),
                    dir_count,
                    file_count,
                    format_size(total_bytes)
                );
            }
        }
        OutputFormat::Json => {
            print_json(&CliLsResult {
                status: "ok",
                path: effective_path.to_string(),
                entries: entries.iter().map(remote_entry_to_cli).collect(),
                summary: LsSummary {
                    total: entries.len(),
                    files: file_count,
                    dirs: dir_count,
                    total_bytes,
                },
            });
        }
    }

    let _ = provider.disconnect().await;
    0
}

async fn cmd_get(
    url: &str,
    remote: &str,
    local: Option<&str>,
    recursive: bool,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let (mut provider, _url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    if recursive {
        return cmd_get_recursive(&mut *provider, remote, local, cli, format, cancelled).await;
    }

    // Check for glob patterns
    if remote.contains('*') || remote.contains('?') {
        return cmd_get_glob(&mut *provider, remote, local, cli, format, cancelled).await;
    }

    let filename = remote.rsplit('/').next().unwrap_or("download");
    let local_path = local.unwrap_or(filename);
    let start = Instant::now();

    // Get file size for progress bar
    let total_size = provider.size(remote).await.unwrap_or(0);

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

    match provider.download(remote, local_path, progress_cb).await {
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
            print_error(format, &format!("Download failed: {}", e), provider_error_to_exit_code(&e));
            let _ = provider.disconnect().await;
            provider_error_to_exit_code(&e)
        }
    }
}

async fn cmd_get_recursive(
    provider: &mut dyn StorageProvider,
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
        let sp = mp.add(create_spinner("Scanning remote directory..."));
        Some(sp)
    } else {
        None
    };

    // BFS to collect all files
    let mut queue = vec![remote_dir.to_string()];
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();

    while let Some(dir) = queue.pop() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        match provider.list(&dir).await {
            Ok(entries) => {
                for e in entries {
                    if e.is_dir {
                        queue.push(e.path.clone());
                        dirs.push(e.path);
                    } else {
                        files.push((e.path, e.size));
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

    let total_bytes: u64 = files.iter().map(|(_, s)| *s).sum();
    let total_files = files.len();

    if !quiet {
        eprintln!(
            "Found {} files ({}) in {} directories",
            total_files,
            format_size(total_bytes),
            dirs.len() + 1
        );
    }

    // Create local directories
    for dir in &dirs {
        let relative = dir.strip_prefix(remote_dir).unwrap_or(dir);
        let relative = relative.trim_start_matches('/');
        let local_dir = format!("{}/{}", local_base, relative);
        let _ = std::fs::create_dir_all(&local_dir);
    }

    // Download files
    let start = Instant::now();
    let mut downloaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();

    let overall_pb = if !quiet {
        let pb = mp.add(ProgressBar::new(total_files as u64));
        pb.set_style(
            ProgressStyle::default_bar()
                .template("Overall [{bar:30.green/dim}] {pos}/{len} files")
                .unwrap()
                .progress_chars("━╸─"),
        );
        Some(pb)
    } else {
        None
    };

    for (remote_path, _size) in &files {
        if cancelled.load(Ordering::Relaxed) {
            errors.push("Cancelled by user".to_string());
            break;
        }

        let relative = remote_path
            .strip_prefix(remote_dir)
            .unwrap_or(remote_path);
        let relative = relative.trim_start_matches('/');
        let local_path = format!("{}/{}", local_base, relative);

        // Ensure parent exists
        if let Some(parent) = Path::new(&local_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match provider.download(remote_path, &local_path, None).await {
            Ok(()) => {
                downloaded += 1;
            }
            Err(e) => {
                errors.push(format!("{}: {}", remote_path, e));
            }
        }

        if let Some(ref pb) = overall_pb {
            pb.inc(1);
        }
    }

    if let Some(pb) = overall_pb {
        pb.finish_and_clear();
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
                skipped: (total_files as u32) - downloaded - (errors.len() as u32),
                errors,
                elapsed_secs: elapsed.as_secs_f64(),
            });
        }
    }

    let _ = provider.disconnect().await;
    if downloaded == total_files as u32 {
        0
    } else {
        4
    }
}

async fn cmd_get_glob(
    provider: &mut dyn StorageProvider,
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

    let entries = match provider.list(dir).await {
        Ok(e) => e,
        Err(e) => {
            print_error(format, &format!("ls failed: {}", e), provider_error_to_exit_code(&e));
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

    let start = Instant::now();
    let total = matched.len();
    let mut downloaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();

    let _ = std::fs::create_dir_all(local_base);

    for entry in &matched {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let local_path = format!("{}/{}", local_base, entry.name);
        match provider.download(&entry.path, &local_path, None).await {
            Ok(()) => {
                downloaded += 1;
                if !cli.quiet && matches!(format, OutputFormat::Text) {
                    println!("  {} → {}", entry.name, local_path);
                }
            }
            Err(e) => {
                errors.push(format!("{}: {}", entry.name, e));
            }
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

    let _ = provider.disconnect().await;
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
    let (mut provider, _url_path) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    if recursive {
        return cmd_put_recursive(&mut *provider, local, remote, cli, format, cancelled).await;
    }

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

    match provider.upload(local, remote_path, progress_cb).await {
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
    provider: &mut dyn StorageProvider,
    local_dir: &str,
    remote_base: Option<&str>,
    cli: &Cli,
    format: OutputFormat,
    cancelled: Arc<AtomicBool>,
) -> i32 {
    let remote_base = remote_base.unwrap_or("/");
    let quiet = cli.quiet || matches!(format, OutputFormat::Json);

    // Walk local directory
    let walker = walkdir::WalkDir::new(local_dir).follow_links(false);
    let mut files: Vec<(String, String, u64)> = Vec::new(); // (local, remote, size)
    let mut dirs: Vec<String> = Vec::new();

    for entry in walker {
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

        let remote_path = format!(
            "{}/{}",
            remote_base.trim_end_matches('/'),
            relative_str
        );

        if entry.file_type().is_dir() {
            dirs.push(remote_path);
        } else if entry.file_type().is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push((entry.path().to_string_lossy().to_string(), remote_path, size));
        }
    }

    let total_bytes: u64 = files.iter().map(|(_, _, s)| *s).sum();
    let total_files = files.len();

    if !quiet {
        eprintln!(
            "Found {} files ({}) in {} directories",
            total_files,
            format_size(total_bytes),
            dirs.len()
        );
    }

    // Create remote directories
    for dir in &dirs {
        let _ = provider.mkdir(dir).await;
    }

    // Upload files
    let start = Instant::now();
    let mut uploaded: u32 = 0;
    let mut errors: Vec<String> = Vec::new();

    for (local_path, remote_path, _size) in &files {
        if cancelled.load(Ordering::Relaxed) {
            errors.push("Cancelled by user".to_string());
            break;
        }

        match provider.upload(local_path, remote_path, None).await {
            Ok(()) => {
                uploaded += 1;
                if !quiet && matches!(format, OutputFormat::Text) {
                    println!("  {} → {}", local_path, remote_path);
                }
            }
            Err(e) => {
                errors.push(format!("{}: {}", local_path, e));
            }
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

    let _ = provider.disconnect().await;
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
                        println!("Created directory: {}", path);
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
        let _ = io::stdout().flush();
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
                        println!("Deleted: {}", path);
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
                        println!("{} → {}", from, to);
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

async fn cmd_cat(url: &str, path: &str, cli: &Cli, format: OutputFormat) -> i32 {
    let (mut provider, _) = match create_and_connect(url, cli, format).await {
        Ok(v) => v,
        Err(code) => return code,
    };

    match provider.download_to_bytes(path).await {
        Ok(data) => {
            match format {
                OutputFormat::Text => {
                    let stdout = io::stdout();
                    let mut handle = stdout.lock();
                    let _ = handle.write_all(&data);
                    let _ = handle.flush();
                }
                OutputFormat::Json => {
                    // For JSON, encode as UTF-8 string (lossy) or base64 for binary
                    let text = String::from_utf8_lossy(&data);
                    print_json(&CliOk {
                        status: "ok",
                        message: text.to_string(),
                    });
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
                    print_json(&CliStatResult {
                        status: "ok",
                        entry: remote_entry_to_cli(&entry),
                    });
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

            let mut queue = vec![path.to_string()];
            let mut found = Vec::new();

            while let Some(dir) = queue.pop() {
                if let Ok(entries) = provider.list(&dir).await {
                    for e in entries {
                        if e.is_dir {
                            queue.push(e.path.clone());
                        }
                        if matcher.is_match(&e.name) {
                            found.push(e);
                        }
                    }
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
                println!("{}", e.path);
            }
            if !cli.quiet {
                println!("\n{} matches", results.len());
            }
        }
        OutputFormat::Json => {
            let file_count = results.iter().filter(|e| !e.is_dir).count();
            let dir_count = results.iter().filter(|e| e.is_dir).count();
            let total_bytes: u64 = results.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            print_json(&CliLsResult {
                status: "ok",
                path: path.to_string(),
                entries: results.iter().map(remote_entry_to_cli).collect(),
                summary: LsSummary {
                    total: results.len(),
                    files: file_count,
                    dirs: dir_count,
                    total_bytes,
                },
            });
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
                    let bar_width = 40;
                    let filled = ((pct / 100.0) * bar_width as f64) as usize;
                    let empty = bar_width - filled;
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

#[allow(clippy::too_many_arguments)]
async fn cmd_sync(
    url: &str,
    local: &str,
    remote: &str,
    direction: &str,
    dry_run: bool,
    delete: bool,
    exclude: &[String],
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

    // Scan local files
    let local_entries: Vec<(String, u64, Option<String>)> = {
        let walker = walkdir::WalkDir::new(local).follow_links(false);
        let mut entries = Vec::new();
        for entry in walker {
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

            // Check excludes
            if exclude.iter().any(|pat| {
                globset::Glob::new(pat)
                    .ok()
                    .map(|g| g.compile_matcher().is_match(&relative) || g.compile_matcher().is_match(entry.file_name().to_string_lossy().to_string()))
                    .unwrap_or(false)
            }) {
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

    // Scan remote files (recursive)
    let mut remote_entries: Vec<(String, u64, Option<String>)> = Vec::new();
    {
        let mut queue = vec![remote.to_string()];
        while let Some(dir) = queue.pop() {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            match provider.list(&dir).await {
                Ok(entries) => {
                    for e in entries {
                        if e.is_dir {
                            queue.push(e.path.clone());
                        } else {
                            let relative = e
                                .path
                                .strip_prefix(remote)
                                .unwrap_or(&e.path)
                                .trim_start_matches('/')
                                .to_string();
                            if !relative.is_empty() {
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

    if !quiet {
        eprintln!(
            "\nSync plan: {} upload, {} download, {} delete, {} skipped (identical)",
            to_upload.len(),
            to_download.len(),
            to_delete_remote.len() + to_delete_local.len(),
            skipped
        );
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

    // Execute sync
    let mut uploaded: u32 = 0;
    let mut downloaded: u32 = 0;
    let mut deleted: u32 = 0;
    let mut errors: Vec<String> = Vec::new();

    for path in &to_upload {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let local_path = format!("{}/{}", local, path);
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
        // Ensure remote parent dir
        if let Some(parent) = Path::new(&remote_path).parent() {
            let _ = provider.mkdir(&parent.to_string_lossy()).await;
        }
        match provider.upload(&local_path, &remote_path, None).await {
            Ok(()) => uploaded += 1,
            Err(e) => errors.push(format!("upload {}: {}", path, e)),
        }
    }

    for path in &to_download {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let local_path = format!("{}/{}", local, path);
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
        if let Some(parent) = Path::new(&local_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match provider.download(&remote_path, &local_path, None).await {
            Ok(()) => downloaded += 1,
            Err(e) => errors.push(format!("download {}: {}", path, e)),
        }
    }

    for path in &to_delete_remote {
        let remote_path = format!("{}/{}", remote.trim_end_matches('/'), path);
        match provider.delete(&remote_path).await {
            Ok(()) => deleted += 1,
            Err(e) => errors.push(format!("delete remote {}: {}", path, e)),
        }
    }

    for path in &to_delete_local {
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
                    "\nSync complete: {} uploaded, {} downloaded, {} deleted in {:.1}s",
                    uploaded,
                    downloaded,
                    deleted,
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

async fn cmd_batch(file: &str, cli: &Cli, format: OutputFormat, cancelled: Arc<AtomicBool>) -> i32 {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => {
            print_error(format, &format!("Cannot read batch file '{}': {}", file, e), 2);
            return 2;
        }
    };

    let mut variables: HashMap<String, String> = HashMap::new();
    let mut current_url: Option<String> = None;
    let mut exit_code = 0;

    for (line_num, raw_line) in content.lines().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            eprintln!("Batch interrupted at line {}", line_num + 1);
            return 4;
        }

        let line = raw_line.trim();
        // Skip comments and blank lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Variable substitution
        let mut expanded = line.to_string();
        for (key, value) in &variables {
            expanded = expanded.replace(&format!("${}", key), value);
            expanded = expanded.replace(&format!("${{{}}}", key), value);
        }

        let parts: Vec<&str> = expanded.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let cmd = parts[0].to_uppercase();

        match cmd.as_str() {
            "SET" => {
                // SET KEY=VALUE
                let rest = expanded[3..].trim();
                if let Some(eq_pos) = rest.find('=') {
                    let key = rest[..eq_pos].trim().to_string();
                    let value = rest[eq_pos + 1..].trim().to_string();
                    variables.insert(key, value);
                }
            }
            "CONNECT" => {
                if parts.len() < 2 {
                    eprintln!("Line {}: CONNECT requires a URL", line_num + 1);
                    return 5;
                }
                current_url = Some(parts[1].to_string());
                exit_code = cmd_connect(parts[1], cli, format).await;
                if exit_code != 0 {
                    eprintln!("Batch failed at line {} (CONNECT): exit code {}", line_num + 1, exit_code);
                    return exit_code;
                }
            }
            "DISCONNECT" => {
                current_url = None;
            }
            "GET" => {
                let url = match &current_url {
                    Some(u) => u.clone(),
                    None => {
                        eprintln!("Line {}: No active connection. Use CONNECT first.", line_num + 1);
                        return 5;
                    }
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: GET requires a remote path", line_num + 1);
                    return 5;
                }
                let local = if parts.len() > 2 { Some(parts[2]) } else { None };
                exit_code = cmd_get(&url, parts[1], local, false, cli, format, cancelled.clone()).await;
                if exit_code != 0 {
                    eprintln!("Batch failed at line {} (GET): exit code {}", line_num + 1, exit_code);
                    return exit_code;
                }
            }
            "PUT" => {
                let url = match &current_url {
                    Some(u) => u.clone(),
                    None => {
                        eprintln!("Line {}: No active connection. Use CONNECT first.", line_num + 1);
                        return 5;
                    }
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: PUT requires a local path", line_num + 1);
                    return 5;
                }
                let remote = if parts.len() > 2 { Some(parts[2]) } else { None };
                exit_code = cmd_put(&url, parts[1], remote, false, cli, format, cancelled.clone()).await;
                if exit_code != 0 {
                    eprintln!("Batch failed at line {} (PUT): exit code {}", line_num + 1, exit_code);
                    return exit_code;
                }
            }
            "MKDIR" => {
                let url = match &current_url {
                    Some(u) => u.clone(),
                    None => {
                        eprintln!("Line {}: No active connection. Use CONNECT first.", line_num + 1);
                        return 5;
                    }
                };
                if parts.len() < 2 {
                    eprintln!("Line {}: MKDIR requires a path", line_num + 1);
                    return 5;
                }
                exit_code = cmd_mkdir(&url, parts[1], cli, format).await;
                if exit_code != 0 {
                    eprintln!("Batch failed at line {} (MKDIR): exit code {}", line_num + 1, exit_code);
                    return exit_code;
                }
            }
            "SYNC" => {
                let url = match &current_url {
                    Some(u) => u.clone(),
                    None => {
                        eprintln!("Line {}: No active connection. Use CONNECT first.", line_num + 1);
                        return 5;
                    }
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
                    cli,
                    format,
                    cancelled.clone(),
                )
                .await;
                if exit_code != 0 {
                    eprintln!("Batch failed at line {} (SYNC): exit code {}", line_num + 1, exit_code);
                    return exit_code;
                }
            }
            _ => {
                eprintln!(
                    "Line {}: Unknown command '{}'. Supported: SET, CONNECT, DISCONNECT, GET, PUT, MKDIR, SYNC",
                    line_num + 1,
                    cmd
                );
                return 5;
            }
        }
    }

    exit_code
}

// ── Main ───────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
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

    // Setup Ctrl+C handler
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let _ = ctrlc::set_handler(move || {
        eprintln!("\nInterrupted (Ctrl+C)");
        cancelled_clone.store(true, Ordering::Relaxed);
    });

    let exit_code = match &cli.command {
        Commands::Connect { url } => cmd_connect(url, &cli, format).await,
        Commands::Ls {
            url,
            path,
            long,
            sort,
            reverse,
            all,
        } => cmd_ls(url, path, *long, sort, *reverse, *all, &cli, format).await,
        Commands::Get {
            url,
            remote,
            local,
            recursive,
        } => cmd_get(url, remote, local.as_deref(), *recursive, &cli, format, cancelled).await,
        Commands::Put {
            url,
            local,
            remote,
            recursive,
        } => cmd_put(url, local, remote.as_deref(), *recursive, &cli, format, cancelled).await,
        Commands::Mkdir { url, path } => cmd_mkdir(url, path, &cli, format).await,
        Commands::Rm {
            url,
            path,
            recursive,
            force,
        } => cmd_rm(url, path, *recursive, *force, &cli, format).await,
        Commands::Mv { url, from, to } => cmd_mv(url, from, to, &cli, format).await,
        Commands::Cat { url, path } => cmd_cat(url, path, &cli, format).await,
        Commands::Stat { url, path } => cmd_stat(url, path, &cli, format).await,
        Commands::Find {
            url,
            path,
            pattern,
        } => cmd_find(url, path, pattern, &cli, format).await,
        Commands::Df { url } => cmd_df(url, &cli, format).await,
        Commands::Sync {
            url,
            local,
            remote,
            direction,
            dry_run,
            delete,
            exclude,
        } => {
            cmd_sync(
                url, local, remote, direction, *dry_run, *delete, exclude, &cli, format, cancelled,
            )
            .await
        }
        Commands::Batch { file } => cmd_batch(file, &cli, format, cancelled).await,
    };

    std::process::exit(exit_code);
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        let cli = Cli::parse_from(["aeroftp", "connect", "ftp://anonymous@ftp.example.com"]);
        let (config, path) = url_to_provider_config("ftp://anonymous@ftp.example.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Ftp);
        assert_eq!(config.host, "ftp.example.com");
        assert_eq!(config.username.as_deref(), Some("anonymous"));
        assert_eq!(path, "/");
    }

    #[test]
    fn test_url_parsing_sftp_with_port() {
        let cli = Cli::parse_from(["aeroftp", "connect", "sftp://admin@server.com:2222/home"]);
        let (config, path) = url_to_provider_config("sftp://admin@server.com:2222/home", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Sftp);
        assert_eq!(config.host, "server.com");
        assert_eq!(config.port, Some(2222));
        assert_eq!(config.username.as_deref(), Some("admin"));
        assert_eq!(path, "/home");
    }

    #[test]
    fn test_url_parsing_webdavs() {
        let cli = Cli::parse_from(["aeroftp", "connect", "webdavs://user@cloud.example.com/dav"]);
        let (config, _path) = url_to_provider_config("webdavs://user@cloud.example.com/dav", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::WebDav);
        assert!(config.host.starts_with("https://"));
    }

    #[test]
    fn test_url_parsing_s3() {
        let cli = Cli::parse_from([
            "aeroftp", "--bucket", "mybucket", "--region", "eu-west-1",
            "connect", "s3://AKID:secret@s3.amazonaws.com",
        ]);
        let (config, _path) = url_to_provider_config("s3://AKID:secret@s3.amazonaws.com", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::S3);
        assert_eq!(config.extra.get("bucket").map(|s| s.as_str()), Some("mybucket"));
        assert_eq!(config.extra.get("region").map(|s| s.as_str()), Some("eu-west-1"));
    }

    #[test]
    fn test_url_parsing_unsupported() {
        let cli = Cli::parse_from(["aeroftp", "connect", "gopher://host"]);
        assert!(url_to_provider_config("gopher://host", &cli).is_err());
    }

    #[test]
    fn test_url_parsing_mega() {
        let cli = Cli::parse_from(["aeroftp", "connect", "mega://user@mega.nz"]);
        let (config, _) = url_to_provider_config("mega://user@mega.nz", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Mega);
    }

    #[test]
    fn test_url_parsing_koofr() {
        let cli = Cli::parse_from(["aeroftp", "connect", "koofr://user@koofr.net"]);
        let (config, _) = url_to_provider_config("koofr://user@koofr.net", &cli).unwrap();
        assert_eq!(config.provider_type, ProviderType::Koofr);
        assert_eq!(config.host, "app.koofr.net");
    }
}
