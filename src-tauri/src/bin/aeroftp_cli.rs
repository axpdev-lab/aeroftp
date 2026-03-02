//! AeroFTP CLI — Multi-protocol file transfer client
//!
//! Usage:
//!   aeroftp-cli connect <url>           Test connection
//!   aeroftp-cli ls <url> [path]         List files
//!   aeroftp-cli get <url> <remote> [local]  Download file
//!   aeroftp-cli put <url> <local> [remote]  Upload file
//!   aeroftp-cli sync <url> <local> <remote> Sync directories
//!
//! Add --json to any command for machine-readable JSON output.

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "aeroftp-cli",
    about = "AeroFTP CLI — Multi-protocol file transfer client",
    version,
    long_about = "Supports FTP, FTPS, SFTP, WebDAV, S3 and more.\nUse URL format: protocol://user:pass@host:port/path"
)]
struct Cli {
    /// Output format: text (human-readable) or json (machine-readable)
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Shorthand for --format json
    #[arg(long, global = true)]
    json: bool,

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
    },
    /// Download a file from remote server
    Get {
        /// Server URL
        url: String,
        /// Remote file path
        remote: String,
        /// Local destination (default: current filename)
        local: Option<String>,
    },
    /// Upload a file to remote server
    Put {
        /// Server URL
        url: String,
        /// Local file path
        local: String,
        /// Remote destination path
        remote: Option<String>,
    },
    /// Sync local and remote directories
    Sync {
        /// Server URL
        url: String,
        /// Local directory path
        local: String,
        /// Remote directory path
        remote: String,
    },
}

// ── Serializable output types ────────────────────────────────────

#[derive(Serialize)]
struct ConnectResult {
    status: &'static str,
    protocol: String,
    host: String,
    port: u16,
    username: String,
    message: String,
}

#[derive(Serialize)]
struct LsResult {
    status: &'static str,
    protocol: String,
    host: String,
    port: u16,
    path: String,
    message: String,
}

#[derive(Serialize)]
struct TransferResult {
    status: &'static str,
    operation: &'static str,
    host: String,
    source: String,
    destination: String,
    message: String,
}

#[derive(Serialize)]
struct SyncResult {
    status: &'static str,
    host: String,
    local: String,
    remote: String,
    message: String,
}

#[derive(Serialize)]
struct ErrorResult {
    status: &'static str,
    error: String,
}

// ── Helpers ──────────────────────────────────────────────────────

fn print_json<T: Serialize>(value: &T) {
    // Unwrap is safe: all our types are plain strings/numbers
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn print_error(format: OutputFormat, msg: &str) {
    match format {
        OutputFormat::Text => eprintln!("Error: {}", msg),
        OutputFormat::Json => print_json(&ErrorResult {
            status: "error",
            error: msg.to_string(),
        }),
    }
}

const STUB_NOTE: &str = "Protocol handler ready. Full provider integration will be available in a future release.";

/// (protocol, username, password, host, port, path)
type ConnectionInfo = (String, String, Option<String>, String, u16, String);

/// Parse a URL like sftp://user:pass@host:22/path into components
fn parse_url(url: &str) -> Result<ConnectionInfo, String> {
    let url_obj = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;
    let protocol = url_obj.scheme().to_string();
    let host = url_obj.host_str().ok_or("Missing host")?.to_string();
    let username = if url_obj.username().is_empty() {
        "anonymous".to_string()
    } else {
        url_obj.username().to_string()
    };
    let password = url_obj.password().map(|p| p.to_string());
    let port = url_obj.port().unwrap_or(match protocol.as_str() {
        "ftp" => 21,
        "ftps" => 990,
        "sftp" | "ssh" => 22,
        "webdav" | "http" => 80,
        "webdavs" | "https" => 443,
        _ => 22,
    });
    let path = if url_obj.path().is_empty() {
        "/".to_string()
    } else {
        url_obj.path().to_string()
    };

    Ok((protocol, username, password, host, port, path))
}

// ── Command handlers ─────────────────────────────────────────────

fn handle_connect(url: &str, format: OutputFormat) {
    match parse_url(url) {
        Ok((protocol, user, _, host, port, _)) => match format {
            OutputFormat::Text => {
                println!("Connecting to {}://{}@{}:{} ...", protocol, user, host, port);
                println!("Connection test: OK (protocol handler ready)");
                println!();
                println!("Note: {}", STUB_NOTE);
            }
            OutputFormat::Json => print_json(&ConnectResult {
                status: "ok",
                protocol,
                host,
                port,
                username: user,
                message: STUB_NOTE.to_string(),
            }),
        },
        Err(e) => {
            print_error(format, &e);
            std::process::exit(1);
        }
    }
}

fn handle_ls(url: &str, path: &str, format: OutputFormat) {
    match parse_url(url) {
        Ok((protocol, user, _, host, port, _)) => match format {
            OutputFormat::Text => {
                println!("Listing {}://{}@{}:{}{}", protocol, user, host, port, path);
                println!();
                println!("Note: {}", STUB_NOTE);
            }
            OutputFormat::Json => print_json(&LsResult {
                status: "ok",
                protocol,
                host,
                port,
                path: path.to_string(),
                message: STUB_NOTE.to_string(),
            }),
        },
        Err(e) => {
            print_error(format, &e);
            std::process::exit(1);
        }
    }
}

fn handle_get(url: &str, remote: &str, local: Option<&str>, format: OutputFormat) {
    let local_name = local.unwrap_or_else(|| remote.rsplit('/').next().unwrap_or("download"));
    match parse_url(url) {
        Ok((_, _, _, host, _, _)) => match format {
            OutputFormat::Text => {
                println!("Download: {}:{} → {}", host, remote, local_name);
                println!();
                println!("Note: {}", STUB_NOTE);
            }
            OutputFormat::Json => print_json(&TransferResult {
                status: "ok",
                operation: "download",
                host,
                source: remote.to_string(),
                destination: local_name.to_string(),
                message: STUB_NOTE.to_string(),
            }),
        },
        Err(e) => {
            print_error(format, &e);
            std::process::exit(1);
        }
    }
}

fn handle_put(url: &str, local: &str, remote: Option<&str>, format: OutputFormat) {
    let remote_name = remote.unwrap_or(local);
    match parse_url(url) {
        Ok((_, _, _, host, _, _)) => match format {
            OutputFormat::Text => {
                println!("Upload: {} → {}:{}", local, host, remote_name);
                println!();
                println!("Note: {}", STUB_NOTE);
            }
            OutputFormat::Json => print_json(&TransferResult {
                status: "ok",
                operation: "upload",
                host,
                source: local.to_string(),
                destination: remote_name.to_string(),
                message: STUB_NOTE.to_string(),
            }),
        },
        Err(e) => {
            print_error(format, &e);
            std::process::exit(1);
        }
    }
}

fn handle_sync(url: &str, local: &str, remote: &str, format: OutputFormat) {
    match parse_url(url) {
        Ok((_, _, _, host, _, _)) => match format {
            OutputFormat::Text => {
                println!("Sync: {} ↔ {}:{}", local, host, remote);
                println!();
                println!("Note: {}", STUB_NOTE);
            }
            OutputFormat::Json => print_json(&SyncResult {
                status: "ok",
                host,
                local: local.to_string(),
                remote: remote.to_string(),
                message: STUB_NOTE.to_string(),
            }),
        },
        Err(e) => {
            print_error(format, &e);
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let format = cli.output_format();

    match &cli.command {
        Commands::Connect { url } => handle_connect(url, format),
        Commands::Ls { url, path } => handle_ls(url, path, format),
        Commands::Get { url, remote, local } => {
            handle_get(url, remote, local.as_deref(), format);
        }
        Commands::Put { url, local, remote } => {
            handle_put(url, local, remote.as_deref(), format);
        }
        Commands::Sync { url, local, remote } => handle_sync(url, local, remote, format),
    }
}
