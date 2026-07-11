//! FTP Storage Provider
//!
//! Implementation of the StorageProvider trait for FTP and FTPS protocols.
//! Uses the suppaftp crate for FTP operations.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use globset::GlobBuilder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use suppaftp::types::FileType;
use suppaftp::{FtpError, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::multi_thread::{
    aerotmp_path_for, run_concurrent_range_download, ConcurrentRangeConfig, ConcurrentRangeOutcome,
};
use super::{
    FtpConfig, FtpTlsMode, ProviderError, ProviderTransferExecutorKind, ProviderType, RemoteEntry,
    StorageProvider,
};

/// Hard cap on intra-file FTP range streams (PD-FTP-1), mirroring the SFTP
/// `MULTI_THREAD_MAX_STREAMS`. Each stream is a full independent FTP
/// control+data connection re-dialled from the retained connection spec, so
/// the cap stays conservative; the live benchmark says where it pays.
const FTP_MULTI_THREAD_MAX_STREAMS: usize = 16;

/// Default intra-file cutoff: below this a single FTP stream beats paying N
/// control-connection handshakes. Matches the SFTP/S3 default (250 MiB) so
/// `--multi-thread-cutoff` behaves identically across backends.
const FTP_MULTI_THREAD_CUTOFF_DEFAULT: u64 = 250 * 1024 * 1024;

/// Per-range streaming read buffer. The generic `read_range` allocates the
/// whole window; the intra-file path streams in fixed chunks so a multi-MiB
/// window never buffers itself in RAM per worker.
const FTP_RANGE_READ_CHUNK: usize = 256 * 1024;

/// FTP/FTPS Storage Provider
pub struct FtpProvider {
    config: FtpConfig,
    stream: Option<AsyncRustlsFtpStream>,
    current_path: String,
    /// Whether server supports MLSD/MLST (RFC 3659)
    mlsd_supported: bool,
    // Whether the server specifically advertises the control-only MLST verb.
    // Some compatible servers advertise MLSD alone; probing those with MLST
    // must not turn a working listing into a hard failure.
    mlst_supported: bool,
    /// Once MLSD proves unreliable, keep using LIST for the lifetime of this provider.
    mlsd_broken: bool,
    /// Whether server supports MFMT (RFC 3659) for setting remote file mtime
    mfmt_supported: bool,
    /// Whether server supports HASH, XMD5, XCRC, or XSHA1 for remote checksums
    hash_supported: Option<String>,
    /// Set to true if ExplicitIfAvailable mode fell back to plaintext
    pub tls_downgraded: bool,
    /// Buffer size for download/upload (default: 8 KB)
    buffer_size: usize,
    /// Connection spec retained after `connect()` so the shared transfer
    /// engine can re-dial N independent FTP connections for intra-file
    /// parallelism (PD-FTP-1, the FTP mirror of PD-SFTP-1/2). `FtpConfig`
    /// already holds the password as `SecretString` for the provider's
    /// lifetime: this is the security posture the FTP session pool already
    /// ships. `Some` once captured at a successful `connect()`, or carried
    /// by a `clone_for_transfer()` worker that has not dialled yet.
    connection_spec: Option<FtpConfig>,
    /// Intra-file parallel streams (PD-FTP-1). `1` (default) keeps the
    /// single-stream path the only behaviour; `>= 2` splits files at/above
    /// `multi_thread_cutoff` into N concurrent REST+RETR ranges over N
    /// independent connections. Set via `set_multi_thread_download`
    /// (CLI `--multi-thread-streams`).
    multi_thread_streams: usize,
    /// File size at/above which intra-file parallelism engages.
    multi_thread_cutoff: u64,
}

impl FtpProvider {
    /// Create a new FTP provider with the given configuration
    pub fn new(config: FtpConfig) -> Self {
        Self {
            config,
            stream: None,
            current_path: "/".to_string(),
            mlsd_supported: false,
            mlst_supported: false,
            mlsd_broken: false,
            mfmt_supported: false,
            hash_supported: None,
            tls_downgraded: false,
            buffer_size: 8192,
            connection_spec: None,
            multi_thread_streams: 1,
            multi_thread_cutoff: FTP_MULTI_THREAD_CUTOFF_DEFAULT,
        }
    }

    /// Get mutable reference to the FTP stream, returning error if not connected
    fn stream_mut(&mut self) -> Result<&mut AsyncRustlsFtpStream, ProviderError> {
        self.stream.as_mut().ok_or(ProviderError::NotConnected)
    }

    /// Connection spec captured at `connect()` (`None` until connected, or
    /// until set by `clone_for_transfer()` on a pool worker). Mirrors
    /// `SftpProvider::connection_spec()`.
    pub fn connection_spec(&self) -> Option<FtpConfig> {
        self.connection_spec.clone()
    }

    /// Ensure this provider has its own independent FTP connection
    /// (PD-FTP-1). A `clone_for_transfer()` worker starts unconnected and
    /// carries only the spec; the first transfer dials a **separate**
    /// control+data connection so N transfers run truly in parallel,
    /// exactly like the SFTP pool. A no-op when already connected, so the
    /// CLI single-stream path (provider already connected) is unchanged.
    async fn ensure_connected(&mut self) -> Result<(), ProviderError> {
        if self.stream.is_some() {
            return Ok(());
        }
        let spec = self
            .connection_spec
            .clone()
            .ok_or(ProviderError::NotConnected)?;
        self.config = spec;
        self.connect().await
    }

    /// PD-FTP-1: split one large file into N gap-free windows, each
    /// downloaded over its **own** independent FTP connection (REST+RETR,
    /// the exact connection model of the FTP session pool and PD-SFTP-2),
    /// assembled into a pre-allocated `.aerotmp` and atomically renamed.
    /// Reuses the shared [`run_concurrent_range_download`] orchestrator so
    /// HTTP, SFTP and FTP share one engine, not a fourth implementation.
    ///
    /// Strict gate (the FTP equivalent of HTTP `206` + `Content-Range`):
    /// every window must yield exactly `end - start + 1` bytes; a premature
    /// EOF is a hard error, never a silent short read. FTP REST+RETR has no
    /// `ServerIgnoredRange` analogue (it cannot answer `200 OK` ignoring the
    /// offset), so that orchestrator arm is unreachable here and fails loud
    /// if hit, never a silent re-download that would double the bytes.
    async fn download_intra_file_pooled(
        &self,
        remote_path: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let spec = self
            .connection_spec
            .clone()
            .ok_or(ProviderError::NotConnected)?;
        let streams = self
            .multi_thread_streams
            .clamp(2, FTP_MULTI_THREAD_MAX_STREAMS);
        let remote_path_owned = remote_path.to_string();

        let cfg = ConcurrentRangeConfig {
            final_path: PathBuf::from(local_path),
            provider_type: ProviderType::Ftp,
            total_size,
            streams,
            max_streams: FTP_MULTI_THREAD_MAX_STREAMS,
            max_parallel: streams,
        };

        tracing::info!(
            "FTP: intra-file download {} ({} bytes) over {} independent connections",
            remote_path_owned,
            total_size,
            streams
        );

        let write_one_range = move |start: u64,
                                    end: u64,
                                    temp_path: PathBuf,
                                    aggregate: Arc<AtomicU64>,
                                    cancel: CancellationToken| {
            let spec = spec.clone();
            let remote_path = remote_path_owned.clone();
            async move {
                ftp_download_one_range(
                    spec,
                    remote_path,
                    FTP_RANGE_READ_CHUNK,
                    start,
                    end,
                    temp_path,
                    aggregate,
                    cancel,
                )
                .await
            }
        };

        match run_concurrent_range_download(
            cfg,
            write_one_range,
            CancellationToken::new(),
            on_progress,
        )
        .await?
        {
            ConcurrentRangeOutcome::Completed => {
                let temp = aerotmp_path_for(Path::new(local_path));
                tokio::fs::rename(&temp, local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                tracing::info!("FTP: intra-file download complete: {}", remote_path);
                Ok(())
            }
            ConcurrentRangeOutcome::ServerIgnoredRange => {
                // Unreachable for FTP: REST+RETR cannot "ignore" a range.
                // Never silently re-download (it would double the bytes).
                let _ = tokio::fs::remove_file(aerotmp_path_for(Path::new(local_path))).await;
                Err(ProviderError::TransferFailed(
                    "FTP intra-file: unexpected ServerIgnoredRange (REST/RETR has no HTTP-200 analogue)"
                        .to_string(),
                ))
            }
        }
    }

    /// Create a TLS connector with rustls for TLS session reuse support (RFC 4217 §10.2).
    ///
    /// Capped to TLS 1.2: TLS 1.3 tickets are single-use (`take_tls13_ticket`
    /// consumes them), so the second data connection would resume a *different*
    /// session than the control channel.  TLS 1.2 session-ID resumption is
    /// non-destructive and satisfies the RFC 4217 requirement that every data
    /// connection resumes the *same* session as the control connection.
    /// This matches the behaviour of FileZilla, WinSCP, and CyberDuck.
    fn make_tls_connector(&self) -> Result<AsyncRustlsConnector, ProviderError> {
        // Name the crypto backend explicitly rather than relying on rustls'
        // process-level default. Both `aws-lc-rs` and `ring` are in the
        // dependency tree, so the implicit lookup cannot disambiguate and
        // panics unless some earlier code installed a default. A panic here
        // leaves the `provider_connect` IPC call unanswered (endless spinner,
        // dead Cancel), so this must not depend on process-wide state.
        let versions = || {
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS12])
            .map_err(|e| ProviderError::ConnectionFailed(format!("TLS setup failed: {e}")))
        };

        let config = if !self.config.verify_cert {
            // M6: Log a warning when TLS certificate verification is disabled.
            tracing::warn!(
                "[FTP] TLS certificate verification DISABLED for {}:{}: connection is vulnerable to MITM attacks",
                self.config.host, self.config.port
            );
            versions()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(danger::NoVerifier))
                .with_no_client_auth()
        } else {
            let mut root_store =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            // Load native OS certificate store (Windows/macOS/Linux) so that
            // system-trusted CAs (e.g. Let's Encrypt via Windows cert store,
            // enterprise CAs, custom roots) are accepted alongside the
            // bundled Mozilla roots.  Errors are non-fatal: if the native
            // store can't be read we still have webpki-roots as fallback.
            let native = rustls_native_certs::load_native_certs();
            if !native.errors.is_empty() {
                tracing::warn!(
                    "[FTP] Errors loading native certificates: {:?}",
                    native.errors
                );
            }
            let count = native.certs.len();
            let mut added = 0u32;
            for cert in native.certs {
                if root_store.add(cert).is_ok() {
                    added += 1;
                }
            }
            if count > 0 {
                tracing::debug!("[FTP] Loaded {added}/{count} native root certificates");
            }
            versions()?
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };

        Ok(AsyncRustlsConnector::from(
            tokio_rustls::TlsConnector::from(Arc::new(config)),
        ))
    }

    /// Parse FTP listing into RemoteEntry
    fn parse_listing(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        // Try Unix format first, then DOS format
        self.parse_unix_listing(line, base_path)
            .or_else(|| self.parse_dos_listing(line, base_path))
    }

    fn join_remote_path(base_path: &str, name: &str) -> String {
        if name.starts_with('/') {
            return name.to_string();
        }

        let trimmed_base = base_path.trim_end_matches('/');
        if trimmed_base.is_empty() {
            format!("/{}", name.trim_start_matches('/'))
        } else {
            format!("{}/{}", trimmed_base, name.trim_start_matches('/'))
        }
    }

    fn normalize_mlsd_name(name: &str) -> String {
        let trimmed = name.trim_end_matches('/');
        std::path::Path::new(trimmed)
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| name.to_string())
    }

    /// Parse Unix-style listing (ls -l format)
    fn parse_unix_listing(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            return None;
        }

        let permissions = parts[0];
        let is_dir = permissions.starts_with('d');
        let is_symlink = permissions.starts_with('l');

        // Get size (might be in different position depending on format)
        let size: u64 = parts[4].parse().unwrap_or(0);

        // Name is everything after the 8th part (to handle spaces in names)
        let name = parts[8..].join(" ");

        // Handle symlinks (name -> target)
        let (actual_name, link_target) = if is_symlink && name.contains(" -> ") {
            let parts: Vec<&str> = name.splitn(2, " -> ").collect();
            (
                parts[0].to_string(),
                Some(parts.get(1).unwrap_or(&"").to_string()),
            )
        } else {
            (name, None)
        };

        // Skip . and .. entries
        if actual_name == "." || actual_name == ".." {
            return None;
        }

        let path = Self::join_remote_path(base_path, &actual_name);

        // Parse date (parts[5..8] typically contain month day time/year)
        let modified = if parts.len() >= 8 {
            Some(format!("{} {} {}", parts[5], parts[6], parts[7]))
        } else {
            None
        };

        Some(RemoteEntry {
            name: actual_name,
            path,
            is_dir,
            size,
            modified,
            permissions: Some(permissions.to_string()),
            owner: Some(parts[2].to_string()),
            group: Some(parts[3].to_string()),
            is_symlink,
            link_target,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    /// Parse DOS-style listing (Windows FTP servers)
    fn parse_dos_listing(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        // DOS format: 01-23-24  10:30AM       <DIR>          folder_name
        // Or:         01-23-24  10:30AM           12345      file.txt

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            return None;
        }

        // In the DOS format parts[2] is always either "<DIR>" or the numeric
        // size. Requiring that keeps this parser from resurrecting a Unix line
        // that parse_unix_listing already rejected (e.g. the "." / ".." rows that
        // `LIST -a` adds, whose parts[2] is the group name): without this guard
        // those become bogus entries and recursive delete would issue `DELE .`.
        let is_dir = parts[2] == "<DIR>";
        let size: u64 = if is_dir {
            0
        } else {
            match parts[2].parse() {
                Ok(value) => value,
                Err(_) => return None,
            }
        };
        let name = parts[3..].join(" ");

        // Skip . and .. entries
        if name == "." || name == ".." {
            return None;
        }

        let path = Self::join_remote_path(base_path, &name);

        let modified = Some(format!("{} {}", parts[0], parts[1]));

        Some(RemoteEntry {
            name,
            path,
            is_dir,
            size,
            modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    /// Parse MLSD/MLST line (RFC 3659 machine-readable format)
    /// Format: "fact1=val1;fact2=val2; filename"
    fn parse_mlsd_entry(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        // Split on first space after semicolons to get facts and filename
        let (facts_str, name) = line.split_once(' ')?;
        let raw_name = name.trim();
        let name = Self::normalize_mlsd_name(raw_name);

        if name == "." || name == ".." {
            return None;
        }

        let mut is_dir = false;
        let mut is_symlink = false;
        let mut size: u64 = 0;
        let mut modified: Option<String> = None;
        let mut permissions: Option<String> = None;
        let mut owner: Option<String> = None;
        let mut group: Option<String> = None;

        for fact in facts_str.split(';') {
            let fact = fact.trim();
            if fact.is_empty() {
                continue;
            }
            let (key, value) = match fact.split_once('=') {
                Some((k, v)) => (k.to_lowercase(), v),
                None => continue,
            };

            match key.as_str() {
                "type" => {
                    let v_lower = value.to_lowercase();
                    is_dir = v_lower == "dir" || v_lower == "cdir" || v_lower == "pdir";
                    is_symlink = v_lower == "os.unix=symlink" || v_lower == "os.unix=slink";
                }
                "size" | "sizd" => {
                    size = value.parse().unwrap_or(0);
                }
                "modify" => {
                    // YYYYMMDDHHMMSS[.sss] → format nicely
                    modified = Some(Self::format_mlsd_time(value));
                }
                "unix.mode" => {
                    permissions = Some(value.to_string());
                }
                "unix.owner" | "unix.uid" => {
                    owner = Some(value.to_string());
                }
                "unix.group" | "unix.gid" => {
                    group = Some(value.to_string());
                }
                "perm"
                    // MLSD perm facts (e.g. "rwcedf") - store as metadata
                    if permissions.is_none() => {
                        permissions = Some(value.to_string());
                    }
                _ => {}
            }
        }

        // Skip cdir/pdir (current/parent directory entries)
        if facts_str.to_lowercase().contains("type=cdir")
            || facts_str.to_lowercase().contains("type=pdir")
        {
            return None;
        }

        let path = Self::join_remote_path(base_path, raw_name);

        Some(RemoteEntry {
            name,
            path,
            is_dir,
            size,
            modified,
            permissions,
            owner,
            group,
            is_symlink,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    /// Format MLSD timestamp (YYYYMMDDHHMMSS) to readable form.
    /// Appends 'Z' suffix because MLSD timestamps are always UTC per RFC 3659.
    fn format_mlsd_time(ts: &str) -> String {
        if ts.len() >= 14 {
            format!(
                "{}-{}-{} {}:{}:{}Z",
                &ts[0..4],
                &ts[4..6],
                &ts[6..8],
                &ts[8..10],
                &ts[10..12],
                &ts[12..14]
            )
        } else if ts.len() >= 8 {
            format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8])
        } else {
            ts.to_string()
        }
    }

    fn is_stale_data_connection_error(err: &ProviderError) -> bool {
        let message = match err {
            ProviderError::ServerError(msg)
            | ProviderError::TransferFailed(msg)
            | ProviderError::ConnectionFailed(msg)
            | ProviderError::Other(msg) => msg,
            _ => return false,
        };
        let lower = message.to_lowercase();
        lower.contains("data connection is already open")
            || (lower.contains("425") && lower.contains("data connection"))
            // Control-connection desync on a reused session: a prior operation
            // left the control stream misaligned, so suppaftp fails to parse the
            // next reply ("Response contains an invalid syntax"). This is the
            // failure the interactive TUI hits on the first transfer after
            // navigating; reconnecting a clean control session recovers it. A
            // server's own "501 Syntax error" is "syntax error", not "invalid
            // syntax", so this stays narrowly the parser-desync signature.
            || lower.contains("invalid syntax")
            || lower.contains("invalid response")
            // A dropped/idle control connection: reconnect and retry.
            || lower.contains("connection reset")
            || lower.contains("broken pipe")
    }

    async fn reconnect_after_data_error(
        &mut self,
        operation: &str,
        path: &str,
        err: &ProviderError,
    ) -> Result<(), ProviderError> {
        tracing::warn!(
            "[FTP] {} hit stale data connection on {}: {}. Reconnecting control session.",
            operation,
            path,
            err
        );
        let _ = self.disconnect().await;
        self.connect().await
    }

    async fn list_inner(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.list_inner_opts(path, false).await
    }

    /// List a directory, optionally including hidden (dotfile) entries.
    ///
    /// `include_hidden` is opt-in and currently used only by recursive delete.
    /// Some servers (notably vsftpd, which does not advertise MLSD) hide
    /// dotfiles on a bare `LIST`, so `rmdir_recursive` would leave an invisible
    /// `.aeroftp-crypt.json` / `.env` behind and the final `RMD` would then fail
    /// with `550` on a "non-empty" directory. When the server speaks MLSD the
    /// normal path already enumerates dotfiles (it filters only `.`/`..`), so it
    /// serves both cases; for the `LIST` fallback we `CWD` into the target and
    /// issue a bare `LIST -a` (no path argument, the portable form across Unix
    /// FTP servers), then restore the working directory. Public `list()`
    /// semantics are unchanged (callers pass `include_hidden = false`).
    async fn list_inner_opts(
        &mut self,
        path: &str,
        include_hidden: bool,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let list_path = if path.is_empty() || path == "." {
            None
        } else {
            Some(path.to_string())
        };

        let base_path = list_path
            .as_deref()
            .unwrap_or(&self.current_path)
            .to_string();

        if self.mlsd_supported {
            // Anti-hang probe: an MLSD issued against a directory that does not
            // exist makes some servers (Plesk / ProFTPD on shared hosting) open a
            // PASV data connection they then never service, wedging the listing
            // forever behind an unresolved "Listing ..." spinner. A control-only
            // MLST answers 550 for a missing path instantly (no data channel), so
            // we fail fast with a clear NotFound instead of hanging. Only probe an
            // explicit target; a current-directory listing is known to exist.
            if self.mlst_supported {
                if let Some(ref target) = list_path {
                    let probe = {
                        let stream = self.stream_mut()?;
                        stream.mlst(Some(target.as_str())).await
                    };
                    match probe {
                        Ok(_) => {}
                        Err(FtpError::UnexpectedResponse(response))
                            if response.status == Status::FileUnavailable =>
                        {
                            return Err(ProviderError::InvalidPath(response.to_string()));
                        }
                        // MLST is only an anti-hang preflight. A transient failure
                        // (or a server that falsely advertises it) must not replace
                        // MLSD's existing error/fallback/reconnect behaviour.
                        Err(error) => tracing::debug!(
                            "[FTP] MLST preflight failed for {}: {}; trying MLSD",
                            target,
                            error
                        ),
                    }
                }
            }
            let mlsd_result = {
                let stream = self.stream_mut()?;
                stream.mlsd(list_path.as_deref()).await
            };

            match mlsd_result {
                Ok(lines) => {
                    let entries: Vec<RemoteEntry> = lines
                        .iter()
                        .filter_map(|line| self.parse_mlsd_entry(line, &base_path))
                        .collect();
                    return Ok(entries);
                }
                Err(err) => {
                    let provider_err = ProviderError::ServerError(err.to_string());
                    tracing::debug!(
                        "[FTP] MLSD failed for {}: {}. Disabling MLSD fallback for this session.",
                        base_path,
                        provider_err
                    );

                    self.mlsd_broken = true;
                    self.mlsd_supported = false;

                    if Self::is_stale_data_connection_error(&provider_err) {
                        self.reconnect_after_data_error("MLSD", &base_path, &provider_err)
                            .await?;
                    }
                }
            }
        }

        let lines = if include_hidden {
            // vsftpd & friends hide dotfiles on a bare `LIST`. CWD into the
            // directory and issue a bare `LIST -a`; the combined `LIST -a <path>`
            // form is server-specific and unreliable, so we avoid it. Restore the
            // previous working directory best-effort afterwards. The `.`/`..`
            // entries that `-a` adds are dropped by the listing parsers.
            let saved_cwd = self.current_path.clone();
            let stream = self.stream_mut()?;
            stream
                .cwd(&base_path)
                .await
                .map_err(|e| ProviderError::ServerError(e.to_string()))?;
            let listed = stream.list(Some("-a")).await;
            let _ = stream.cwd(&saved_cwd).await;
            listed.map_err(|e| ProviderError::ServerError(e.to_string()))?
        } else {
            let stream = self.stream_mut()?;
            stream
                .list(list_path.as_deref())
                .await
                .map_err(|e| ProviderError::ServerError(e.to_string()))?
        };

        Ok(lines
            .iter()
            .filter_map(|line| self.parse_listing(line, &base_path))
            .collect())
    }

    /// Enumerate a directory for recursive deletion, including hidden dotfiles.
    /// Mirrors the stale-connection retry of the public `list()`.
    async fn list_for_delete(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        match self.list_inner_opts(path, true).await {
            Ok(entries) => Ok(entries),
            Err(err) if Self::is_stale_data_connection_error(&err) => {
                self.reconnect_after_data_error("LIST", path, &err).await?;
                self.list_inner_opts(path, true).await
            }
            Err(err) => Err(err),
        }
    }

    async fn stat_inner(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if self.mlsd_supported {
            let stream = self.stream_mut()?;
            if let Ok(mlst_line) = stream.mlst(Some(path)).await {
                let parent = std::path::Path::new(path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "/".to_string());
                if let Some(entry) = self.parse_mlsd_entry(mlst_line.trim(), &parent) {
                    return Ok(entry);
                }
            }
        }

        let parent = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());

        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| ProviderError::InvalidPath(path.to_string()))?;

        let entries = self.list_inner(&parent).await?;

        entries
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))
    }

    async fn size_inner(&mut self, path: &str) -> Result<u64, ProviderError> {
        let stream = self.stream_mut()?;
        let size = stream
            .size(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(size as u64)
    }
}

#[async_trait]
impl StorageProvider for FtpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        if self.config.tls_mode != FtpTlsMode::None {
            ProviderType::Ftps
        } else {
            ProviderType::Ftp
        }
    }

    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let domain = self.config.host.clone();

        // Connect and optionally upgrade to TLS based on tls_mode
        let mut stream = match self.config.tls_mode {
            FtpTlsMode::None => {
                // Plain FTP - no TLS
                AsyncRustlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            }
            FtpTlsMode::Explicit => {
                // Explicit TLS (AUTH TLS) - connect plain, then upgrade
                let stream = AsyncRustlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
                let connector = self.make_tls_connector()?;
                stream.into_secure(connector, &domain).await.map_err(|e| {
                    ProviderError::ConnectionFailed(format!("TLS upgrade failed: {}", e))
                })?
            }
            FtpTlsMode::Implicit => {
                // Implicit TLS - TLS from the start, no AUTH TLS (port 990)
                let connector = self.make_tls_connector()?;
                #[allow(deprecated)]
                AsyncRustlsFtpStream::connect_secure_implicit(&addr, connector, &domain)
                    .await
                    .map_err(|e| {
                        ProviderError::ConnectionFailed(format!("Implicit TLS failed: {}", e))
                    })?
            }
            FtpTlsMode::ExplicitIfAvailable => {
                // A3-02: Try explicit TLS, but NEVER fall back to plaintext silently.
                // Sending credentials over an unencrypted connection without user consent
                // is a security risk. If TLS fails, return an error instead.
                let stream = AsyncRustlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
                let connector = self.make_tls_connector()?;
                match stream.into_secure(connector, &domain).await {
                    Ok(secure) => {
                        self.tls_downgraded = false;
                        secure
                    }
                    Err(e) => {
                        tracing::warn!(
                            "SECURITY: TLS upgrade failed for {}:{} ({}). \
                             Refusing to send credentials over plaintext.",
                            self.config.host,
                            self.config.port,
                            e
                        );
                        return Err(ProviderError::ConnectionFailed(format!(
                            "TLS upgrade failed: {}. Connection would be unencrypted. \
                             Use 'None' encryption mode explicitly to connect without TLS.",
                            e
                        )));
                    }
                }
            }
        };

        // Login
        use secrecy::ExposeSecret;
        let pwd = self.config.password.expose_secret();
        stream
            .login(self.config.username.as_str(), pwd)
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(e.to_string()))?;

        // Set binary transfer mode
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // Navigate to initial path if specified.
        //
        // Skip CWD on bare "/" so we don't override the server-provided
        // post-login working directory. For non-chroot FTP servers
        // (vsftpd, ProFTPD with default config) PWD post-login returns
        // the user's home (e.g. /home/user), which is where rclone, lftp,
        // FileZilla, ftp(1) and curl all default to. Issuing CWD / would
        // jump to the filesystem root, which is typically not writable
        // by the authenticated user and breaks `put`/`mkdir` of relative
        // paths. Profiles that genuinely need a non-home base set a
        // non-"/" `initial_path` explicitly.
        if let Some(ref initial_path) = self.config.initial_path {
            if !initial_path.is_empty() && initial_path != "/" {
                stream
                    .cwd(initial_path)
                    .await
                    .map_err(|e| ProviderError::InvalidPath(e.to_string()))?;
            }
        }

        // Check FEAT for MLSD and MFMT support (RFC 3659)
        match stream.feat().await {
            Ok(features) => {
                let server_supports_mlsd =
                    features.contains_key("MLST") || features.contains_key("MLSD");
                self.mlsd_supported = server_supports_mlsd && !self.mlsd_broken;
                self.mlst_supported = features.contains_key("MLST");
                self.mfmt_supported = features.contains_key("MFMT");
                // B3: Detect hash/checksum commands (prefer HASH > XMD5 > XCRC > XSHA1)
                self.hash_supported = if features.contains_key("HASH") {
                    Some("HASH".to_string())
                } else if features.contains_key("XMD5") {
                    Some("XMD5".to_string())
                } else if features.contains_key("XCRC") {
                    Some("XCRC".to_string())
                } else if features.contains_key("XSHA1") {
                    Some("XSHA1".to_string())
                } else {
                    None
                };
                tracing::debug!(
                    "FTP FEAT: MLSD={}, MFMT={}, HASH={:?}",
                    self.mlsd_supported,
                    self.mfmt_supported,
                    self.hash_supported
                );
            }
            Err(_) => {
                self.mlsd_supported = false;
                self.mlst_supported = false;
                self.mfmt_supported = false;
                self.hash_supported = None;
            }
        };

        // Get current directory (normalize Windows backslashes from FTP servers)
        self.current_path = stream
            .pwd()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?
            .replace('\\', "/");

        self.stream = Some(stream);

        // PD-FTP-1: capture the connection spec now so the shared transfer
        // engine can re-dial N independent FTP connections for intra-file
        // parallelism. `FtpProvider` owns `self.config` (password as
        // `SecretString`) for its whole life: the same posture as the FTP
        // session pool already in production.
        self.connection_spec = Some(self.config.clone());
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.quit().await;
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        match self.list_inner(path).await {
            Ok(entries) => Ok(entries),
            Err(err) if Self::is_stale_data_connection_error(&err) => {
                self.reconnect_after_data_error("LIST", path, &err).await?;
                self.list_inner(path).await
            }
            Err(err) => Err(err),
        }
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        let stream = self.stream_mut()?;
        let path = stream
            .pwd()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?
            .replace('\\', "/");
        self.current_path = path.clone();
        Ok(path)
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .cwd(path)
            .await
            .map_err(|e| ProviderError::InvalidPath(e.to_string()))?;

        self.current_path = stream
            .pwd()
            .await
            .unwrap_or_else(|_| path.to_string())
            .replace('\\', "/");

        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .cdup()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        self.current_path = stream
            .pwd()
            .await
            .unwrap_or_else(|_| "/".to_string())
            .replace('\\', "/");

        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // No-op when the provider is already connected (CLI path); connects
        // a `clone_for_transfer()` worker on its first transfer (generic
        // clone-pool path), mirroring SFTP.
        self.ensure_connected().await?;

        // Get file size for progress + intra-file gating.
        let total_size = {
            let stream = self.stream_mut()?;
            stream.size(remote_path).await.unwrap_or(0) as u64
        };

        // PD-FTP-1: intra-file parallelism. Engaged only when the user
        // opted in (`set_multi_thread_download(streams >= 2, ...)`), the
        // file is at/above the cutoff, and a real connection spec exists so
        // we can re-dial N independent FTP connections. Without all three
        // this is a no-op and the single-stream path below is unchanged:
        // honest non-regression, no protocol overclaim.
        if self.multi_thread_streams >= 2
            && total_size >= self.multi_thread_cutoff
            && self.connection_spec.is_some()
        {
            return self
                .download_intra_file_pooled(remote_path, local_path, total_size, on_progress)
                .await;
        }

        // Single-stream download with one reconnect-and-retry on a desynced
        // control connection. A reused FTP session (the interactive TUI after
        // navigation) can leave the control stream misaligned so the TYPE/RETR
        // reply fails to parse; reconnecting a clean session recovers it. A
        // fresh single-shot CLI connection never hits this, so the retry is a
        // no-op there.
        match self
            .download_single(remote_path, local_path, total_size, on_progress)
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if Self::is_stale_data_connection_error(&err) => {
                self.reconnect_after_data_error("RETR", remote_path, &err)
                    .await?;
                let total_size = {
                    let stream = self.stream_mut()?;
                    stream
                        .size(remote_path)
                        .await
                        .unwrap_or(total_size as usize) as u64
                };
                // Progress is dropped on the first attempt; the rare retry runs
                // without it (the transfer still completes).
                self.download_single(remote_path, local_path, total_size, None)
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::AsyncReadExt;
        let limit = super::MAX_DOWNLOAD_TO_BYTES;

        // PD-FTP-1: dial the connection so an in-memory read works on a
        // `clone_for_transfer()` pool worker too, symmetric with the
        // streaming `download()`. A no-op when already connected.
        self.ensure_connected().await?;

        let stream = self.stream_mut()?;

        // Check file size first if server supports SIZE command
        if let Ok(size) = stream.size(remote_path).await {
            if size as u64 > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                    size as f64 / 1_048_576.0,
                    limit as f64 / 1_048_576.0,
                )));
            }
        }

        // Set binary mode
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // Download using retr_as_stream
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // H2: Read with size cap to prevent OOM
        let mut data = Vec::new();
        let limit_usize = (limit + 1) as usize;
        loop {
            let mut buf = [0u8; 8192];
            let n = data_stream
                .read(&mut buf)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if data.len() > limit_usize {
                break;
            }
        }
        let bytes_read = data.len();

        // Finalize the stream
        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if bytes_read as u64 > limit {
            return Err(ProviderError::TransferFailed(format!(
                "Download exceeded {:.0} MB size limit. Use streaming download for large files.",
                limit as f64 / 1_048_576.0,
            )));
        }

        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // PD-FTP-1: a `clone_for_transfer()` pool worker starts unconnected
        // and carries only the spec; it must dial its own control+data
        // connection on the first transfer. `download()` and `read_range()`
        // already do this; `upload()` omitted it, so every clone-pool upload
        // (folder upload, multi-file batch via the executor) failed with
        // `NotConnected`. A no-op when the provider is already connected.
        self.ensure_connected().await?;

        // One reconnect-and-retry on a desynced control connection (same
        // reused-session recovery as download); a fresh CLI connection never
        // hits it, so this is a no-op for the single-shot path. Progress is
        // dropped on the rare retry (the transfer still completes).
        match self
            .upload_single(local_path, remote_path, on_progress)
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if Self::is_stale_data_connection_error(&err) => {
                self.reconnect_after_data_error("STOR", remote_path, &err)
                    .await?;
                self.upload_single(local_path, remote_path, None).await
            }
            Err(err) => Err(err),
        }
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .mkdir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .rm(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .rmdir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Include hidden dotfiles so the final RMD does not fail with 550 on a
        // directory that still holds an invisible `.aeroftp-crypt.json` / `.env`
        // etc. (see list_inner_opts). SFTP/WebDAV already return dotfiles; FTP is
        // the outlier that relies on the server to hide them on a bare LIST.
        let entries = self.list_for_delete(path).await?;

        // Delete contents first
        for entry in entries {
            if entry.is_dir {
                // Use Box::pin for recursive async call
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }

        // Now delete the empty directory
        self.rmdir(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .rename(from, to)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        match self.stat_inner(path).await {
            Ok(entry) => Ok(entry),
            Err(err) if Self::is_stale_data_connection_error(&err) => {
                self.reconnect_after_data_error("STAT", path, &err).await?;
                self.stat_inner(path).await
            }
            Err(err) => Err(err),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        match self.size_inner(path).await {
            Ok(size) => Ok(size),
            Err(err) if Self::is_stale_data_connection_error(&err) => {
                self.reconnect_after_data_error("SIZE", path, &err).await?;
                self.size_inner(path).await
            }
            Err(err) => Err(err),
        }
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .noop()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        // FTP doesn't have a standard server info command
        // Return basic connection info
        Ok(format!(
            "FTP Server: {}:{}",
            self.config.host, self.config.port
        ))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let matcher = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
            .map_err(|e| {
                ProviderError::InvalidConfig(format!("Invalid find pattern '{}': {}", pattern, e))
            })?
            .compile_matcher();
        let mut results = Vec::new();
        let search_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else {
            path.to_string()
        };
        let mut dirs_to_scan = vec![search_path];

        while let Some(dir) = dirs_to_scan.pop() {
            // Save current_path, list, restore
            let saved = self.current_path.clone();
            self.current_path = dir.clone();
            let entries = match self.list(&dir).await {
                Ok(e) => e,
                Err(_) => {
                    self.current_path = saved;
                    continue;
                }
            };
            self.current_path = saved;

            for entry in entries {
                if entry.is_dir {
                    dirs_to_scan.push(entry.path.clone());
                }

                if matcher.is_match(&entry.name) {
                    results.push(entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt as _};

        // PD-FTP-1: the transfer executor calls `resume_download()` instead of
        // `download()` whenever a retry carries a partial offset, so a resumed
        // download on a `clone_for_transfer()` pool worker hit `NotConnected`.
        // Dial the connection first, symmetric with `download()`/`upload()`/
        // `read_range()`. A no-op when already connected.
        self.ensure_connected().await?;

        let stream = self.stream_mut()?;

        // Get total file size
        let total_size = stream.size(remote_path).await.unwrap_or(0) as u64;

        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // Send REST command to set offset
        stream
            .resume_transfer(offset as usize)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("REST failed: {}", e)))?;

        // Retrieve from offset
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // H3: Stream directly to file instead of buffering entire file in memory
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        // Seek to the resume offset (no set_len: preserve existing bytes before offset)
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(ProviderError::IoError)?;

        // Stream chunks from FTP data stream directly to disk
        let mut transferred = offset;
        let mut buf = vec![0u8; 64 * 1024]; // 64 KB chunks
        loop {
            let n = data_stream
                .read(&mut buf)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])
                .await
                .map_err(ProviderError::IoError)?;
            transferred += n as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
        }

        file.flush().await.map_err(ProviderError::IoError)?;

        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncSeekExt;

        let total_size = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?
            .len();

        if offset >= total_size {
            return Ok(()); // Nothing to upload
        }

        // Open file and seek to offset for streaming append
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(ProviderError::IoError)?;

        let stream = self.stream_mut()?;
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        stream
            .append_file(remote_path, &mut file)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if let Some(progress) = on_progress {
            progress(total_size, total_size);
        }

        Ok(())
    }

    fn supports_chmod(&self) -> bool {
        true
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;

        // SITE CHMOD command
        let chmod_cmd = format!("CHMOD {:o} {}", mode, path);
        stream
            .site(&chmod_cmd)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        Ok(())
    }

    fn supports_checksum(&self) -> bool {
        self.hash_supported.is_some()
    }

    async fn checksum(
        &mut self,
        path: &str,
    ) -> Result<std::collections::HashMap<String, String>, ProviderError> {
        self.remote_checksum(path).await
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T09): intentionally NotSupported
        // by design on FTP and FTPS.
        //
        // The FTP protocol (RFC 959 + RFC 3659 + RFC 4217) has no native
        // notion of independent "parts" that can be uploaded out of order
        // and stitched together at commit time: STOR is monolithic, REST
        // only rewinds the byte offset on a single transfer stream, and
        // APPE concatenates without ordering guarantees across multiple
        // sessions. Real file-level parallelism on FTP comes from
        // re-dialling N independent control connections via the dedicated
        // `FtpSessionPool`/`FtpDownloadExecutor` (see
        // `transfer_executor_kind` below: `FtpConnectionPool` once a
        // connection spec exists). Wiring a fake-multipart shim that
        // sliced one upload into REST/STOR pairs across one connection
        // would not improve resumability and would defeat the executor
        // pool, so we leave `supports_multipart=false` and let the runner
        // pick the legacy single-stream path with REST-based resume.
        super::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            supports_range_download: true,
            ..Default::default()
        }
    }

    /// PD-FTP-1: advertise real file-level parallelism only once a
    /// connection spec exists to re-dial independent FTP connections from.
    /// Without it (never connected) FTP stays a single locked lease:
    /// honest non-regression, no overclaim. The legacy GUI FTP transfer
    /// path uses its own dedicated `FtpSessionPool`/`FtpDownloadExecutor`
    /// and never consults this, so it is unaffected (no double pool).
    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        if self.connection_spec.is_some() {
            ProviderTransferExecutorKind::FtpConnectionPool
        } else {
            ProviderTransferExecutorKind::LockedSingle
        }
    }

    /// Conservative initial cap, mirroring the SFTP pool / FTP pool clamp.
    /// Each lease is a full independent FTP connection.
    fn transfer_executor_max_sessions(&self) -> u16 {
        4
    }

    /// Produce an independent transfer worker. It is **not connected**: it
    /// carries only the connection spec and dials its own separate FTP
    /// connection lazily on the first transfer (`ensure_connected`). No
    /// control/data connection is shared, so N workers are N independent
    /// connections, exactly like the SFTP pool.
    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        let spec = self.connection_spec.clone().ok_or_else(|| {
            ProviderError::NotSupported(
                "FTP clone_for_transfer requires a captured connection spec".to_string(),
            )
        })?;
        let mut worker = FtpProvider::new(spec.clone());
        worker.buffer_size = self.buffer_size;
        worker.connection_spec = Some(spec);
        worker.multi_thread_streams = self.multi_thread_streams;
        worker.multi_thread_cutoff = self.multi_thread_cutoff;
        Ok(Box::new(worker))
    }

    fn set_chunk_sizes(&mut self, upload: Option<u64>, download: Option<u64>) {
        // Cap at 16 MB. BUFFER-01: FTP uses a single transfer buffer for both
        // directions; apply the larger of --chunk-size/--buffer-size deterministically
        // (the previous `.or()` silently dropped --buffer-size whenever --chunk-size
        // was also set).
        let cap = 16 * 1024 * 1024;
        if let Some(size) = upload.into_iter().chain(download).max() {
            self.buffer_size = (size as usize).clamp(4096, cap);
        }
    }

    /// PD-FTP-1: opt into intra-file parallelism. `streams <= 1` keeps the
    /// single-stream path (honest default). `cutoff` floors at 1 MiB so a
    /// degenerate value can never split a tiny file into N handshakes. The
    /// intra-file path additionally requires a real connection spec
    /// (`FtpConnectionPool`) at `download()` time, so a not-connected
    /// provider never overclaims.
    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        self.multi_thread_streams = streams.clamp(1, FTP_MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes.max(1024 * 1024);
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::AsyncReadExt;

        const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
        if len > MAX_READ_RANGE {
            return Err(ProviderError::Other(format!(
                "Read range size {} exceeds maximum {} bytes",
                len, MAX_READ_RANGE
            )));
        }

        // PD-FTP-1 mirror of the SFTP path: clone-for-transfer workers
        // start unconnected and self-dial on first transfer. The GUI
        // segmented engine calls `read_range` directly on the pool
        // worker, so without this self-dial every segmented download
        // against a clone pool fails with `Not connected`.
        self.ensure_connected().await?;
        let stream = self.stream_mut()?;

        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // REST sets the byte offset for the next RETR
        stream
            .resume_transfer(offset as usize)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("REST failed: {}", e)))?;

        let mut data_stream = stream
            .retr_as_stream(path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Read exactly `len` bytes (or until EOF if file is shorter)
        let mut buf = vec![0u8; len as usize];
        let mut total_read = 0usize;
        while total_read < len as usize {
            let n = data_stream
                .read(&mut buf[total_read..])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Range read failed: {}", e)))?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        buf.truncate(total_read);

        // Bounded FTP reads intentionally stop before EOF. Some servers will report an
        // error while finalizing that partial RETR; when that happens we proactively
        // disconnect so the disposable chunk connection cannot be reused in a bad state.
        let finalize_result = {
            let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
            stream.finalize_retr_stream(data_stream).await
        };
        if finalize_result.is_err() {
            let _ = self.disconnect().await;
        }

        Ok(buf)
    }
}

// =============================================================================
// FTP Hash/Checksum Commands (B3)
// =============================================================================

/// Map an FTP server's hash-algorithm label (the leading token of a
/// RFC-draft `HASH` reply, or the implied algo of `XMD5`/`XCRC`/`XSHA1`)
/// to the canonical lowercase key shared by every
/// `StorageProvider::checksum()` impl and the `hashsum` / `lsjson --hash`
/// consumers. Separators and case vary across servers (`SHA-256`,
/// `sha256`, `SHA256`), so the label is normalised before matching; an
/// unrecognised label degrades to its lowercased, separator-stripped form
/// rather than being dropped (still server-side, just an exotic algo).
fn canonical_hash_key(server_algo: &str) -> String {
    let norm: String = server_algo
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    match norm.as_str() {
        "SHA256" => "sha256",
        "SHA512" => "sha512",
        "SHA384" => "sha384",
        "SHA1" => "sha1",
        "MD5" => "md5",
        "CRC32" => "crc32",
        "ADLER32" => "adler32",
        _ => return norm.to_ascii_lowercase(),
    }
    .to_string()
}

impl FtpProvider {
    /// One single-stream RETR attempt. Factored out of the trait `download` so
    /// it can be retried once after reconnecting a clean control session
    /// (reused-session recovery). `on_progress` is owned; the retry passes
    /// `None`.
    async fn download_single(
        &mut self,
        remote_path: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;

        // Set binary mode
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // Download using retr_as_stream: stream directly to disk (no full-file RAM buffer)
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut chunk = vec![0u8; self.buffer_size];
        let mut transferred: u64 = 0;

        loop {
            let n = data_stream
                .read(&mut chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            atomic
                .write_all(&chunk[..n])
                .await
                .map_err(ProviderError::IoError)?;
            transferred += n as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
        }

        atomic.commit().await.map_err(ProviderError::IoError)?;

        // Finalize the stream - need to get stream again after the borrow
        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    /// One STOR attempt. Factored out of the trait `upload` so it can be retried
    /// once after reconnecting a clean control session. `on_progress` is owned;
    /// the retry passes `None`.
    async fn upload_single(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use suppaftp::types::FileType;
        use tokio::io::AsyncReadExt;

        // Capture before the &mut self borrow below; needed later to decide
        // whether to insert the TLS-drain sleep.
        let tls_active = !matches!(self.config.tls_mode, FtpTlsMode::None);

        let stream = self.stream_mut()?;

        // Set binary transfer mode explicitly
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total_size = file.metadata().await.map_err(ProviderError::IoError)?.len();

        // Open streaming upload channel (PASV + STOR)
        let mut data_stream = stream
            .put_with_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Write in 64KB chunks for optimal throughput
        let mut chunk = [0u8; 65536];
        let mut total_written: u64 = 0;

        loop {
            let n = file
                .read(&mut chunk)
                .await
                .map_err(ProviderError::IoError)?;
            if n == 0 {
                break;
            }
            data_stream
                .write_all(&chunk[..n])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Data write error: {}", e)))?;
            total_written += n as u64;
            if let Some(ref progress) = on_progress {
                progress(total_written, total_size);
            }
        }

        // Flush all (TLS) buffers to the wire
        data_stream
            .flush()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Flush error: {}", e)))?;

        // TLS shutdown races with TCP send buffer when a close_notify is
        // sent before the kernel has drained the last TLS records. On
        // **TLS-protected** FTP connections we therefore wait for the
        // socket to drain in proportion to the upload size before letting
        // suppaftp issue close_notify and read the 226 reply. Plain FTP
        // has no close_notify and the underlying TCP FIN ordering is fine
        //: the sleep there is pure dead time and was the dominant cost
        // on small/medium uploads (50-100ms per file × 500 files = 25-50s
        // wasted on the bulk-of-small-files benchmark).
        if tls_active {
            let drain_ms = (total_written / 4096).clamp(100, 2000);
            tokio::time::sleep(std::time::Duration::from_millis(drain_ms)).await;
        }

        // Finalize: sends TLS close_notify (when TLS), reads 226 from control channel
        stream
            .finalize_put_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Preserve local file's mtime on the remote file via MFMT (draft-somers-ftp-mfxx).
        // MFMT is a standalone FTP command, NOT a SITE sub-command.
        // Best practice: FileZilla, WinSCP, lftp all do this after upload.
        if self.mfmt_supported {
            if let Ok(local_meta) = std::fs::metadata(local_path) {
                if let Ok(mtime) = local_meta.modified() {
                    if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        let dt = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0);
                        if let Some(dt) = dt {
                            let mfmt_time = dt.format("%Y%m%d%H%M%S").to_string();
                            if let Some(stream) = self.stream.as_mut() {
                                // MFMT <time-val> <pathname>: expects 213 response
                                let cmd = format!("MFMT {} {}", mfmt_time, remote_path);
                                if let Err(e) =
                                    stream.custom_command(&cmd, &[suppaftp::Status::File]).await
                                {
                                    tracing::debug!("FTP MFMT failed (non-fatal): {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Compute a remote file checksum using the best available command.
    /// Returns a map like {"MD5": "abc123..."} or {"CRC32": "..."} etc.
    pub async fn remote_checksum(
        &mut self,
        path: &str,
    ) -> Result<std::collections::HashMap<String, String>, ProviderError> {
        let hash_cmd = self.hash_supported.clone().ok_or_else(|| {
            ProviderError::Other("Server does not support hash commands".to_string())
        })?;

        let stream = self.stream_mut()?;

        let (cmd_str, default_algo) = match hash_cmd.as_str() {
            "HASH" => (format!("HASH {}", path), "SHA-256"),
            "XMD5" => (format!("XMD5 {}", path), "MD5"),
            "XCRC" => (format!("XCRC {}", path), "CRC32"),
            "XSHA1" => (format!("XSHA1 {}", path), "SHA-1"),
            _ => {
                return Err(ProviderError::Other(format!(
                    "Unknown hash command: {}",
                    hash_cmd
                )))
            }
        };

        let response = stream
            .custom_command(
                &cmd_str,
                &[suppaftp::Status::File, suppaftp::Status::CommandOk],
            )
            .await
            .map_err(|e| ProviderError::ServerError(format!("Hash command failed: {}", e)))?;

        let body = String::from_utf8_lossy(&response.body).into_owned();
        let mut result = std::collections::HashMap::new();

        if hash_cmd == "HASH" {
            // RFC draft HASH response: "<algo> <range> <hash> <path>"
            // e.g. "SHA-256 0-EOF abc123def456 /path/to/file.txt"
            let parts: Vec<&str> = body.splitn(4, ' ').collect();
            if parts.len() >= 3 {
                result.insert(
                    canonical_hash_key(parts[0]),
                    parts[2].trim().to_ascii_lowercase(),
                );
            } else {
                result.insert(
                    canonical_hash_key(default_algo),
                    body.trim().to_ascii_lowercase(),
                );
            }
        } else {
            // XMD5/XCRC/XSHA1: response is just the hex hash
            result.insert(
                canonical_hash_key(default_algo),
                body.trim().to_ascii_lowercase(),
            );
        }

        Ok(result)
    }
}

/// PD-FTP-1 per-range writer. Dials a fresh independent FTP connection from
/// `spec`, REST+RETR from `start`, and streams **exactly** `end - start + 1`
/// bytes into `temp_path` at absolute offset `start`. One call == one fresh
/// control+data connection, so N ranges of one file = N independent
/// connections, the same model as the FTP session pool and PD-SFTP-2.
///
/// Strict gate: a `read() == 0` before `expected` bytes is a hard
/// [`ProviderError`], never a silent short read, never `ServerIgnoredRange`
/// (FTP REST+RETR has no HTTP-200 analogue). Writes are clamped to the
/// window so a remote file that grew mid-transfer cannot corrupt the
/// neighbouring range.
#[allow(clippy::too_many_arguments)]
async fn ftp_download_one_range(
    spec: FtpConfig,
    remote_path: String,
    chunk_size: usize,
    start: u64,
    end: u64,
    temp_path: PathBuf,
    aggregate: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Result<ConcurrentRangeOutcome, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt as _};

    let expected = end - start + 1;

    // Independent worker: its own control + data connection.
    let mut worker = FtpProvider::new(spec);
    worker.connect().await?;

    {
        let stream = worker.stream_mut()?;
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        // REST sets the byte offset for the next RETR.
        stream
            .resume_transfer(start as usize)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("REST failed: {}", e)))?;
    }

    let mut data_stream = {
        let stream = worker.stream_mut()?;
        stream
            .retr_as_stream(&remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?
    };

    let mut out = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&temp_path)
        .await
        .map_err(ProviderError::IoError)?;
    out.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(ProviderError::IoError)?;

    let mut buf = vec![0u8; chunk_size];
    let mut written: u64 = 0;

    while written < expected {
        tokio::select! {
            _ = cancel.cancelled() => {
                return Err(ProviderError::TransferFailed(
                    "Transfer cancelled by user".to_string(),
                ));
            }
            read = data_stream.read(&mut buf) => {
                let n = read.map_err(|e| {
                    ProviderError::TransferFailed(format!("Range read error: {}", e))
                })?;
                if n == 0 {
                    // Strict gate: premature EOF, no silent short read.
                    return Err(ProviderError::TransferFailed(format!(
                        "FTP range short read: expected {} bytes at offset {}, got {}",
                        expected, start, written
                    )));
                }
                let take = std::cmp::min(n as u64, expected - written) as usize;
                out.write_all(&buf[..take])
                    .await
                    .map_err(ProviderError::IoError)?;
                aggregate.fetch_add(take as u64, Ordering::Relaxed);
                written += take as u64;
            }
        }
    }

    out.flush().await.map_err(ProviderError::IoError)?;
    out.sync_all().await.map_err(ProviderError::IoError)?;

    // The bounded RETR intentionally stopped before EOF; finalizing that
    // partial RETR may error. The connection is disposable (one per range),
    // so disconnect regardless: the same posture as `read_range`.
    let finalize_result = {
        let stream = worker.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream.finalize_retr_stream(data_stream).await
    };
    let _ = finalize_result;
    let _ = worker.disconnect().await;

    Ok(ConcurrentRangeOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TLS connector must build without a process-level rustls
    /// `CryptoProvider` installed. `aws-lc-rs` and `ring` are both in the
    /// dependency tree, so the implicit `builder_with_protocol_versions()`
    /// panics here, which strands every FTPS connect: the `provider_connect`
    /// IPC call never answers, the spinner never stops, and Cancel is dead
    /// because the panic already de-registered the connect token.
    #[test]
    fn tls_connector_builds_without_a_process_default_crypto_provider() {
        for verify_cert in [true, false] {
            let provider = FtpProvider::new(FtpConfig {
                host: "example.invalid".to_string(),
                port: 21,
                username: "user".to_string(),
                password: "pass".to_string().into(),
                tls_mode: FtpTlsMode::Explicit,
                verify_cert,
                initial_path: None,
            });
            assert!(
                provider.make_tls_connector().is_ok(),
                "make_tls_connector must not panic or fail (verify_cert={verify_cert})"
            );
        }
    }

    #[test]
    fn stale_control_connection_error_is_detected_for_reconnect() {
        // The reused-session desync (suppaftp parse failure) and dropped
        // control connections must trigger the reconnect-and-retry path.
        let stale = [
            "Response contains an invalid syntax",
            "Invalid response: bad",
            "an invalid response",
            "connection reset by peer",
            "broken pipe",
            "425 Can't open data connection",
            "data connection is already open",
        ];
        for msg in stale {
            assert!(
                FtpProvider::is_stale_data_connection_error(&ProviderError::ServerError(
                    msg.to_string()
                )),
                "expected '{}' to be recoverable",
                msg
            );
        }
        // A server's own 5xx (not a parser desync) must NOT trigger a reconnect.
        for msg in [
            "550 No such file or directory",
            "501 Syntax error in parameters",
        ] {
            assert!(
                !FtpProvider::is_stale_data_connection_error(&ProviderError::ServerError(
                    msg.to_string()
                )),
                "expected '{}' to be non-recoverable",
                msg
            );
        }
    }

    #[test]
    fn test_parse_unix_listing() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let line = "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 projects";
        let entry = provider.parse_unix_listing(line, "/").unwrap();

        assert_eq!(entry.name, "projects");
        assert!(entry.is_dir);
        assert_eq!(entry.size, 4096);
    }

    #[test]
    fn list_a_output_keeps_dotfiles_and_drops_dot_dirs() {
        // `LIST -a` (the include_hidden path used by recursive delete) returns
        // the `.` and `..` directory entries alongside dotfiles. The parser must
        // KEEP the dotfile (so rmdir_recursive removes it and the final RMD
        // succeeds) and DROP `.`/`..` (so it never issues `DELE .`, the regression
        // that 550'd ordinary populated directories in the earlier attempt).
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let listing = [
            "drwx------    2 ftp      ftp          4096 Jun 13 15:17 .",
            "drwxr-xr-x    3 ftp      ftp          4096 Jun 13 15:17 ..",
            "-rw-------    1 ftp      ftp             6 Jun 13 15:17 .aeroftp-crypt.json",
            "-rw-------    1 ftp      ftp             2 Jun 13 15:17 visible.txt",
        ];
        let names: Vec<String> = listing
            .iter()
            .filter_map(|line| provider.parse_listing(line, "/scope"))
            .map(|entry| entry.name)
            .collect();

        assert_eq!(names, vec![".aeroftp-crypt.json", "visible.txt"]);
    }

    #[test]
    fn test_parse_mlsd_entry() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let line = "type=file;size=12345;modify=20260131120000;unix.mode=0644; readme.txt";
        let entry = provider.parse_mlsd_entry(line, "/home").unwrap();

        assert_eq!(entry.name, "readme.txt");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 12345);
        assert_eq!(entry.modified.as_deref(), Some("2026-01-31 12:00:00Z"));
        assert_eq!(entry.permissions.as_deref(), Some("0644"));
        assert_eq!(entry.path, "/home/readme.txt");
    }

    #[test]
    fn test_parse_mlsd_directory() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let line = "type=dir;modify=20260115080000; projects";
        let entry = provider.parse_mlsd_entry(line, "/").unwrap();

        assert_eq!(entry.name, "projects");
        assert!(entry.is_dir);
        assert_eq!(entry.path, "/projects");
    }

    #[test]
    fn test_detects_stale_data_connection_error() {
        let err = ProviderError::ServerError("425 Data connection is already open".to_string());
        assert!(FtpProvider::is_stale_data_connection_error(&err));
    }

    #[test]
    fn test_ignores_non_stale_ftp_errors() {
        let err = ProviderError::ServerError("550 Permission denied".to_string());
        assert!(!FtpProvider::is_stale_data_connection_error(&err));
    }

    #[test]
    fn test_parse_mlsd_skips_cdir_pdir() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        assert!(provider
            .parse_mlsd_entry("type=cdir;modify=20260101000000; .", "/")
            .is_none());
        assert!(provider
            .parse_mlsd_entry("type=pdir;modify=20260101000000; ..", "/")
            .is_none());
    }

    #[test]
    fn test_parse_dos_listing() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let line = "01-20-26  10:00AM       <DIR>          Projects";
        let entry = provider.parse_dos_listing(line, "/").unwrap();

        assert_eq!(entry.name, "Projects");
        assert!(entry.is_dir);
    }

    #[test]
    fn ftp_hash_keys_canonicalised() {
        // RFC-draft HASH labels and the X* family map to the same
        // lowercase keys every checksum() consumer expects.
        assert_eq!(canonical_hash_key("SHA-256"), "sha256");
        assert_eq!(canonical_hash_key("sha256"), "sha256");
        assert_eq!(canonical_hash_key("SHA-512"), "sha512");
        assert_eq!(canonical_hash_key("SHA-1"), "sha1");
        assert_eq!(canonical_hash_key("MD5"), "md5");
        assert_eq!(canonical_hash_key("CRC32"), "crc32");
        // Unknown algo degrades, never dropped.
        assert_eq!(canonical_hash_key("Whirlpool"), "whirlpool");
    }
}

/// Dangerous TLS certificate verifier that accepts all certificates.
/// Used only when the user explicitly enables "Accept invalid or self-signed certificates".
mod danger {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    pub struct NoVerifier;

    impl ServerCertVerifier for NoVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PKCS1_SHA256,
                SignatureScheme::RSA_PKCS1_SHA384,
                SignatureScheme::RSA_PKCS1_SHA512,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ECDSA_NISTP384_SHA384,
                SignatureScheme::ECDSA_NISTP521_SHA512,
                SignatureScheme::RSA_PSS_SHA256,
                SignatureScheme::RSA_PSS_SHA384,
                SignatureScheme::RSA_PSS_SHA512,
                SignatureScheme::ED25519,
                SignatureScheme::ED448,
            ]
        }
    }
}
