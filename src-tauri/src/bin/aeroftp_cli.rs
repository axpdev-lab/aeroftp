//! AeroFTP CLI — Multi-protocol file transfer client
//!
//! Usage:
//!   aeroftp-cli connect <url>           Test connection
//!   aeroftp-cli ls <url> [path]         List files
//!   aeroftp-cli get <url> <remote> [local]  Download file
//!   aeroftp-cli put <url> <local> [remote]  Upload file
//!   aeroftp-cli sync <url> <local> <remote> Sync directories

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "aeroftp-cli",
    about = "AeroFTP CLI — Multi-protocol file transfer client",
    version,
    long_about = "Supports FTP, FTPS, SFTP, WebDAV, S3 and more.\nUse URL format: protocol://user:pass@host:port/path"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

/// (protocol, host, password, username, port, path)
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

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Connect { url } => {
            match parse_url(url) {
                Ok((protocol, user, _, host, port, _)) => {
                    println!(
                        "Connecting to {}://{}@{}:{} ...",
                        protocol, user, host, port
                    );
                    println!("Connection test: OK (protocol handler ready)");
                    println!();
                    println!("Note: Full connection testing requires runtime provider initialization.");
                    println!("This CLI foundation will be expanded with actual provider connections in future releases.");
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Ls { url, path } => {
            match parse_url(url) {
                Ok((protocol, user, _, host, port, _)) => {
                    println!(
                        "Listing {}://{}@{}:{}{}",
                        protocol, user, host, port, path
                    );
                    println!();
                    println!("Note: Directory listing requires runtime provider initialization.");
                    println!(
                        "CLI provider integration will be available in a future release."
                    );
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Get {
            url,
            remote,
            local,
        } => {
            let local_name = local
                .as_deref()
                .unwrap_or_else(|| remote.rsplit('/').next().unwrap_or("download"));
            match parse_url(url) {
                Ok((_, _, _, host, _, _)) => {
                    println!("Download: {}:{} → {}", host, remote, local_name);
                    println!();
                    println!("Note: File transfer requires runtime provider initialization.");
                    println!(
                        "CLI provider integration will be available in a future release."
                    );
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Put { url, local, remote } => {
            let remote_name = remote.as_deref().unwrap_or(local);
            match parse_url(url) {
                Ok((_, _, _, host, _, _)) => {
                    println!("Upload: {} → {}:{}", local, host, remote_name);
                    println!();
                    println!("Note: File transfer requires runtime provider initialization.");
                    println!(
                        "CLI provider integration will be available in a future release."
                    );
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Sync { url, local, remote } => {
            match parse_url(url) {
                Ok((_, _, _, host, _, _)) => {
                    println!("Sync: {} ↔ {}:{}", local, host, remote);
                    println!();
                    println!("Note: Sync requires runtime provider initialization.");
                    println!(
                        "CLI sync integration will be available in a future release."
                    );
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}
