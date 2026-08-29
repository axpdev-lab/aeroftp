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

use super::checksum_matrix;
use super::multi_thread::{
    aerotmp_path_for, run_concurrent_range_download, ConcurrentRangeConfig, ConcurrentRangeOutcome,
};
use super::{
    ChecksumCapability, FtpConfig, FtpTlsMode, ProviderError, ProviderTransferExecutorKind,
    ProviderType, RemoteEntry, StorageProvider,
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

/// Default single-stream transfer buffer. The previous 8 KiB default made
/// TLS record churn dominate the FTPS 1-channel path; the 2026-07-23 buffer
/// A/B (docs/dev/benchmarks/2026-07-23_ftp-presets-matrix, 3 interleaved
/// reps per size on the lab FTPS profile) measured 64 KiB at a 89.7 s median
/// vs 130.8 s for 8 KiB and 112.4 s for 256 KiB on the same 1 GiB object,
/// with the tightest spread. `--buffer-size` still overrides this.
const FTP_DOWNLOAD_BUFFER_DEFAULT: usize = 64 * 1024;

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
            buffer_size: FTP_DOWNLOAD_BUFFER_DEFAULT,
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
            endpoint_identity: self.endpoint_identity(),
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

    // The parsers themselves live in `super::ftp_listing`, shared with the
    // legacy `crate::ftp::FtpManager`. These stay as thin methods so the call
    // sites below read unchanged.
    fn parse_listing(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        super::ftp_listing::parse_listing(line, base_path)
    }

    fn parse_mlsd_entry(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        super::ftp_listing::parse_mlsd_entry(line, base_path)
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
            // form is server-specific and unreliable, so we avoid it. The `.`/`..`
            // entries that `-a` adds are dropped by the listing parsers.
            let saved_cwd = self.cwd_into(&base_path).await?;
            let stream = self.stream_mut()?;
            let listed = stream.list(Some("-a")).await;
            let restored = self.restore_cwd(&saved_cwd).await;
            // Both are reported when both fail. Propagating the listing error
            // first would swallow the restore failure exactly when it matters
            // most: the two fail together whenever the data connection drops,
            // and of the pair it is the unrestored directory that outlives the
            // call and misdirects everything after it.
            match (listed, restored) {
                (Ok(lines), Ok(())) => lines,
                (Err(list_err), Ok(())) => {
                    return Err(ProviderError::ServerError(list_err.to_string()))
                }
                (Ok(_), Err(restore_err)) => return Err(restore_err),
                (Err(list_err), Err(restore_err)) => {
                    return Err(ProviderError::ServerError(format!(
                        "{list_err}; and {restore_err}"
                    )))
                }
            }
        } else {
            let stream = self.stream_mut()?;
            stream
                .list(list_path.as_deref())
                .await
                .map_err(|e| ProviderError::ServerError(e.to_string()))?
        };

        let entries: Vec<RemoteEntry> = lines
            .iter()
            .filter_map(|line| self.parse_listing(line, &base_path))
            .collect();

        // FTP answers a LIST against a directory that does not exist with a
        // successful, empty listing, so "missing" and "empty" arrive identical.
        // WebDAV says "not found" and S3 is entitled to answer empty (a prefix
        // with no keys IS an empty prefix), but an FTP directory either exists
        // or does not, and the server knows which: the information is on the
        // wire and we were discarding it.
        //
        // It is not a listing quirk. A sync with delete reads the empty listing
        // as "the source is gone", and the guard that would refuse to mirror
        // that into a delete is not called on every path, so the planner can
        // delete a local tree against a directory that was merely misspelled.
        //
        // The confirmation costs nothing on the normal path, because it runs
        // only when the listing came back empty, which is the sole ambiguous
        // case. CWD is used rather than a stat because it needs no FEAT support
        // and so behaves the same on servers that advertise MLST, those that
        // advertise MLSD alone, and those that advertise neither.
        if entries.is_empty() && list_path.is_some() && base_path != "/" {
            self.confirm_directory_exists(&base_path).await?;
        }

        Ok(entries)
    }

    /// CWD into `target`, returning where the SERVER says we were.
    ///
    /// The saved directory comes from PWD, not from `self.current_path`. That
    /// field is our mirror of the server's state, and restoring to what we
    /// believe rather than to where we actually were is how a listing leaves
    /// the connection pointing somewhere else.
    async fn cwd_into(&mut self, target: &str) -> Result<String, ProviderError> {
        let stream = self.stream_mut()?;
        let saved = stream
            .pwd()
            .await
            .map_err(|e| ProviderError::ServerError(format!("pwd: {e}")))?;
        if let Err(e) = stream.cwd(target).await {
            return Err(Self::classify_cwd_failure(target, &e));
        }
        // The mirror follows the server immediately, so it is never a lie even
        // if the restore below fails.
        self.current_path = target.to_string();
        Ok(saved)
    }

    /// Decide what a refused CWD means, without claiming more than the reply says.
    ///
    /// The reply carries two independent facts and they must not be collapsed:
    /// the status code says whether the refusal is permanent, and the text says
    /// whether it is about a path that is not there. Reading only one of them
    /// is how this function was wrong twice in the same place, in opposite
    /// directions, and neither error was caught by the person writing it.
    ///
    /// The first version answered `ServerError` for every refusal. That maps to
    /// exit 10, which `is_retryable_exit` treats as retryable, so a directory
    /// that will never be enterable was attempted three times: a permanent
    /// failure dressed as a temporary one.
    ///
    /// The correction overshot. It answered `InvalidPath`, exit 5 and permanent
    /// on both retry paths, for every status rather than for 550, so a `421
    /// Service not available, closing control connection` or a `450 Requested
    /// file action not taken` became permanent. Those are the replies a server
    /// sends when it is overloaded or closing an idle session, which is exactly
    /// the moment a client should retry, and RFC 959 section 4.2 says so in the
    /// code itself: 4yz is "Transient Negative Completion", the action "may be
    /// requested again", while 5yz is permanent and the user "is discouraged
    /// from repeating the exact request". The class digit is the server's own
    /// statement about retrying, so overruling it is not a judgement call.
    ///
    /// Only 550 is read as a statement about the path, and the split inside it
    /// is by text because FTP spends 550 on both "it is not there" and "you may
    /// not enter". Every other status keeps `ServerError` and the server's own
    /// words. That is deliberately unambitious: a 530 is permanent too, but it
    /// is about the session and not the path, and answering `InvalidPath` there
    /// would trade a wrong retry for a false statement, which is the worse of
    /// the two. Turning the 4yz/5yz contract into retry behaviour belongs where
    /// retrying is decided, once, for every FTP call site, not here for one of
    /// them; doing it here would fix this caller and leave the rest, which is
    /// the "patch the case, not the predicate" mistake this branch removed
    /// elsewhere. It is on the register.
    fn classify_cwd_failure(target: &str, err: &FtpError) -> ProviderError {
        let FtpError::UnexpectedResponse(ref response) = err else {
            return ProviderError::ServerError(format!("{target}: {err}"));
        };
        let text = response.to_string();
        if response.status != Status::FileUnavailable {
            return ProviderError::ServerError(format!("{target}: {text}"));
        }
        // A 550 on CWD is not only "it is not there": it is also what a server
        // sends for a directory you may not enter. Reading every one as
        // NotFound turns an inaccessible directory into a nonexistent one,
        // which is the mistake this change already made once on mkdir and had
        // corrected by a live server.
        if super::types::message_names_a_missing_path(&text) {
            ProviderError::NotFound(format!("{target}: {text}"))
        } else {
            ProviderError::InvalidPath(format!("{target}: {text}"))
        }
    }

    /// Put the working directory back, and report it if that fails.
    ///
    /// The failure used to be discarded with `let _`. A discarded restore is
    /// the worst shape this can take: the listing succeeds, the connection is
    /// left in another directory, and the damage appears later in an unrelated
    /// operation with nothing to connect it back to here.
    async fn restore_cwd(&mut self, saved: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        match stream.cwd(saved).await {
            Ok(()) => {
                self.current_path = saved.to_string();
                Ok(())
            }
            Err(e) => Err(ProviderError::ServerError(format!(
                "could not restore the working directory to {saved}: {e}"
            ))),
        }
    }

    /// Recognise a reply that says the path is not there.
    ///
    /// FTP spends 550 on both "you may not" and "it is not there", and spells
    /// the difference only in the text, so the text is what has to be read.
    /// Everything that is not clearly about a missing path is left alone: a
    /// permission error must not be reported as a missing file, and a command
    /// the server simply refuses must not become one either.
    fn classify_missing_path(err: &FtpError) -> Option<ProviderError> {
        let FtpError::UnexpectedResponse(ref response) = err else {
            return None;
        };
        if !matches!(
            response.status,
            Status::FileUnavailable | Status::BadFilename
        ) {
            return None;
        }
        let text = response.to_string();
        super::types::message_names_a_missing_path(&text).then_some(ProviderError::NotFound(text))
    }

    /// Classify a STOR failure instead of calling everything a transfer error.
    ///
    /// A missing parent directory comes back as 550 or as "553 Can't open that
    /// file: No such file or directory", which said nothing about which segment
    /// was missing and, worse, was retried: the retry budget is spent on exit
    /// codes, and a generic transfer failure is a retryable one. So a permanent
    /// 5xx about a path that does not exist was tried three times before
    /// failing, every time.
    ///
    /// The CLI compensated with a preflight `stat` of the parent on FTP only,
    /// which made the message good and the retries stop on that surface alone.
    /// Classifying it here does both for every caller, and the preflight goes
    /// away in the same change.
    fn map_store_error(err: FtpError) -> ProviderError {
        if let Some(missing) = Self::classify_missing_path(&err) {
            return missing;
        }
        ProviderError::TransferFailed(err.to_string())
    }

    /// Tell a directory that is empty from one that is not there.
    ///
    /// Answers `Ok` when it exists and `NotFound` when it does not, and leaves
    /// the working directory where it found it.
    async fn confirm_directory_exists(&mut self, target: &str) -> Result<(), ProviderError> {
        let saved = self.cwd_into(target).await?;
        self.restore_cwd(&saved).await
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

        let mut entry = entries
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;

        // A LIST row can report 0 for a file that is not empty: the column may
        // be missing, unparseable, or simply absent in the server's dialect.
        // SIZE asks the server directly, so ask it rather than pass on a zero
        // that cannot be told from an empty file.
        //
        // The CLI has been doing exactly this since the defect was first hit,
        // in a helper it applies after its own stat calls. That made `stat`
        // correct on one surface out of three: the same call through the GUI or
        // through MCP returned the unhydrated zero. Moving it here is not a new
        // fix, it is the proven one put where all three surfaces reach it, and
        // the CLI helper goes away in the same change.
        if !entry.is_dir && entry.size == 0 {
            if let Ok(size) = self.size_inner(path).await {
                entry.size = size;
            }
        }
        Ok(entry)
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

    fn endpoint_identity(&self) -> crate::transfer_dag::EndpointIdentity {
        crate::transfer_dag::EndpointIdentity::new(
            self.provider_type().to_string(),
            &self.config.host,
            &self.config.username,
        )
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
        //
        // A failure here used to become 0 and the download carried on. That
        // discarded the one thing the server had just told us, and it did it
        // three ways at once: a progress bar with a fabricated total, the
        // intra-file parallelism silently disabled because 0 is below every
        // cutoff, and, when the file simply was not there, a RETR issued
        // anyway for a path the server had already refused.
        //
        // Only a reply that names a missing path is turned into an answer.
        // Servers legitimately refuse SIZE for other reasons (notably in ASCII
        // mode), and those keep the previous behaviour exactly: fall back to 0
        // and let the transfer decide. Refusing a download because SIZE was
        // unavailable would break working setups, which is the trade this
        // whole change exists to avoid.
        let total_size = {
            let stream = self.stream_mut()?;
            match stream.size(remote_path).await {
                Ok(size) => size as u64,
                Err(err) => match Self::classify_missing_path(&err) {
                    Some(missing) => return Err(missing),
                    None => 0,
                },
            }
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
        let attempt = {
            let stream = self.stream_mut()?;
            stream.mkdir(path).await
        };
        match attempt {
            Ok(()) => Ok(()),
            // Every MKD failure used to become one generic ServerError, so
            // "it is already there" and "you may not" arrived indistinguishable
            // and callers that wanted to be idempotent had to guess.
            //
            // Unlike the other defects in this change, the answer is NOT always
            // on the wire. Measured against the repo's vsftpd fixture, a
            // duplicate mkdir replies "550 Create directory operation failed."
            // with no mention of existence at all: the same text it uses for a
            // refusal. Some servers do say "File exists"; vsftpd does not.
            //
            // So the directory is asked about rather than guessed at, always.
            // An earlier version took a shortcut when the reply did say
            // something like "File exists", to spare those servers a round
            // trip. That shortcut skipped the check below, and a server saying
            // "550 File exists" because a FILE holds the name would have been
            // answered "the directory is already there": an idempotent caller
            // carries on and the first upload into it fails somewhere else.
            // The saving was one round trip on a failure; the cost was that the
            // guarantee held on one branch and not the other, and depended on
            // how a server chooses to word a sentence. This whole change exists
            // because a fix that rested on the wording of a reply turned out to
            // rest on nothing.
            //
            // The question is asked with CWD rather than with a stat, and that
            // choice carries the whole cost of this branch. A stat here would
            // be free only on servers that advertise MLST; on the rest it falls
            // back to listing the PARENT directory and searching it, so every
            // duplicate mkdir would cost a listing proportional to the parent's
            // size, and a recursive upload walks a mkdir ladder over every
            // ancestor, paying that once per level of an existing tree. CWD is
            // two commands regardless of the server and regardless of how large
            // the directory is.
            //
            // It also answers a better question. A stat says "something is
            // here" and the directory-ness has to be read off it; CWD cannot
            // succeed on a file at all, so "it is there AND it is a directory"
            // is the only thing a success can mean. The guarantee stops being
            // derived and becomes intrinsic.
            //
            // A 550 that is neither stays the generic error it always was:
            // claiming "permission denied" for a reply that does not say so
            // would trade a vague answer for a wrong one.
            Err(FtpError::UnexpectedResponse(response))
                if response.status == Status::FileUnavailable =>
            {
                match self.confirm_directory_exists(path).await {
                    Ok(()) => Err(ProviderError::AlreadyExists(path.to_string())),
                    // The directory is not there, so the mkdir failed for its
                    // own reason and that is what the caller gets.
                    Err(ProviderError::NotFound(_)) => {
                        Err(ProviderError::ServerError(response.to_string()))
                    }
                    // Anything else is about the probe, not about the mkdir,
                    // and one of those is the probe having entered the
                    // directory and failed to come back out. Collapsing that
                    // into the generic mkdir error would be the discarded
                    // restore this change exists to remove, returning in the
                    // one place a restore can fail during a probe.
                    Err(probe_failure) => Err(probe_failure),
                }
            }
            Err(e) => Err(ProviderError::ServerError(e.to_string())),
        }
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

        // Get total file size. Same reading as `download`, and the case is more
        // likely here rather than less: between the interrupted attempt and the
        // resume there is time for the remote file to go away, and a resume of
        // something that is no longer there is exactly the condition this is
        // for. Any other SIZE failure keeps the previous fallback.
        let total_size = match stream.size(remote_path).await {
            Ok(size) => size as u64,
            Err(err) => match Self::classify_missing_path(&err) {
                Some(missing) => return Err(missing),
                None => 0,
            },
        };

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

    /// Narrowed to what THIS server advertised in FEAT, which is the whole
    /// point on FTP: the matrix lists the four algorithms the protocol can
    /// carry, but a server offering only `XCRC` can produce CRC32 and nothing
    /// else, and the user should read that instead of clicking to find out.
    /// `HASH` alone is negotiated per request (the server picks the
    /// algorithm), so it keeps the flag and the full list.
    fn checksum_capability(&self, _path: &str) -> ChecksumCapability {
        let Some(cmd) = self.hash_supported.as_deref() else {
            return ChecksumCapability::default();
        };
        let base = checksum_matrix::capability(self.provider_type());
        match cmd {
            "HASH" => base,
            "XMD5" => ChecksumCapability {
                algorithms: vec!["md5".to_string()],
                negotiated: false,
                ..base
            },
            "XSHA1" => ChecksumCapability {
                algorithms: vec!["sha1".to_string()],
                negotiated: false,
                ..base
            },
            "XCRC" => ChecksumCapability {
                algorithms: vec!["crc32".to_string()],
                negotiated: false,
                ..base
            },
            _ => ChecksumCapability::default(),
        }
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

    /// File-level clone-pool cap for the shared provider executor
    /// (CLI multi-file batch, PD-FTP-1). Aligned with the FTP/FTPS speed
    /// button's Maximum (5x) tier so a Max-preset multi-file run actually
    /// dials up to 5 independent FTP connections instead of silently
    /// clamping to 4. Each lease is a full independent FTP connection; the
    /// legacy GUI FTP transfer path uses its own dedicated `FtpSessionPool`
    /// and never consults this, so it is unaffected. Raise further only
    /// after a live benchmark on the target server says it pays.
    fn transfer_executor_max_sessions(&self) -> u16 {
        5
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

    /// PD-FTP-2: opt into warm-connection reuse. An FTP worker keeps its
    /// control connection open across files (`download`/`upload` short-circuit
    /// `ensure_connected` when already connected and transfer by absolute path),
    /// so the shared executor can recycle it instead of re-dialling per file,
    /// which is exactly the per-file handshake cost that left multi-file FTP
    /// behind rclone. Only warm workers from SUCCESSFUL transfers are recycled.
    fn supports_transfer_worker_reuse(&self) -> bool {
        true
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
            .map_err(Self::map_store_error)?;

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

    fn cwd_reply(code: u32, body: &str) -> FtpError {
        FtpError::UnexpectedResponse(suppaftp::types::Response::new(
            suppaftp::Status::from(code),
            body.as_bytes().to_vec(),
        ))
    }

    /// The retry contract of a refused CWD, one row per status.
    ///
    /// This table exists because the same line was wrong twice in opposite
    /// directions and nothing failed either time: the classification lived
    /// inside an `async fn` that needs a live socket, so no unit test could
    /// reach it and the live tests only ever exercised 550. A rule no test can
    /// see is a rule that gets rewritten by whoever touches it next.
    ///
    /// The two 4yz rows are the ones that were being answered `InvalidPath`,
    /// exit 5, permanent: a server closing an idle control connection (421) or
    /// briefly refusing an action (450) would have been reported as a path that
    /// does not work, and never retried.
    #[test]
    fn a_refused_cwd_is_only_permanent_when_the_status_says_so() {
        let cases: &[(u32, &str, &str)] = &[
            // 550, and the text names a missing path: the one case that is
            // genuinely about a path that is not there.
            (550, "550 /nope: No such file or directory", "NotFound"),
            // 550 without that vocabulary: permanent, but we do not get to say
            // it is missing. A directory you may not enter reaches here.
            (550, "550 Permission denied", "InvalidPath"),
            (550, "550 Failed to change directory.", "InvalidPath"),
            // Transient. The server is telling us to come back, and a
            // permanent answer here silences the retry that would succeed.
            (
                421,
                "421 Service not available, closing control connection",
                "ServerError",
            ),
            (450, "450 Requested file action not taken", "ServerError"),
            // Permanent, but about the session and not the path. Kept as
            // ServerError on purpose: a wrong retry costs two round trips, a
            // false statement about the path costs the reader's trust.
            (530, "530 Not logged in", "ServerError"),
            (500, "500 Unknown command", "ServerError"),
        ];
        for (code, body, expected) in cases {
            let got = FtpProvider::classify_cwd_failure("/target", &cwd_reply(*code, body));
            let actual = match got {
                ProviderError::NotFound(_) => "NotFound",
                ProviderError::InvalidPath(_) => "InvalidPath",
                ProviderError::ServerError(_) => "ServerError",
                ref other => panic!("{code}: unexpected variant {other:?}"),
            };
            assert_eq!(
                actual, *expected,
                "CWD {code} ({body:?}) classified as {actual}, expected {expected}"
            );
            // Whatever the verdict, the server's own words survive it. A
            // classification that replaces the reply with a sentence of ours
            // leaves the user with our guess and no way back to the fact.
            let rendered = got.to_string();
            assert!(
                rendered.contains(body) && rendered.contains("/target"),
                "CWD {code}: the reply or the target was dropped from {rendered:?}"
            );
        }
    }

    /// A failure that is not a reply at all keeps the transport error.
    #[test]
    fn a_cwd_that_never_got_a_reply_is_not_a_path_verdict() {
        let err = FtpError::ConnectionError(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ));
        let got = FtpProvider::classify_cwd_failure("/target", &err);
        assert!(
            matches!(got, ProviderError::ServerError(_)),
            "a transport failure became {got:?}, which claims something about the path"
        );
    }

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

    /// A STOR into a directory that is not there is permanent, and used to be
    /// reported as a generic transfer failure: a retryable one. It was tried
    /// three times, every time, and the message said nothing about which
    /// segment was missing.
    #[test]
    fn a_store_into_a_missing_directory_is_not_found_not_a_transfer_failure() {
        use suppaftp::types::Response;
        let missing = FtpProvider::map_store_error(FtpError::UnexpectedResponse(Response::new(
            Status::BadFilename,
            b"Can't open that file: No such file or directory".to_vec(),
        )));
        assert!(
            matches!(missing, ProviderError::NotFound(_)),
            "553 with 'no such file' is a missing path: {missing:?}"
        );

        let denied = FtpProvider::map_store_error(FtpError::UnexpectedResponse(Response::new(
            Status::FileUnavailable,
            b"Permission denied".to_vec(),
        )));
        assert!(
            matches!(denied, ProviderError::TransferFailed(_)),
            "a refusal that is not about a missing path must not become NotFound: {denied:?}"
        );

        // A transport failure is not a classification question at all.
        let broken = FtpProvider::map_store_error(FtpError::BadResponse);
        assert!(matches!(broken, ProviderError::TransferFailed(_)));
    }

    // ── Characterisation battery for the listing parser ────────────────────
    //
    // These exist to prove that moving the parser into a shared module changes
    // NOTHING. They pin today's behaviour, defects included: where the parser
    // is wrong the baseline records the wrong answer, marked DEFECT. Changing
    // an expectation here to make it look right would destroy the only
    // evidence that the move preserved behaviour. Fixes belong to a later
    // change, which will edit this baseline deliberately and say why.

    fn charac_provider() -> FtpProvider {
        FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        })
    }

    const CHARAC_LIST_ROWS: &[(&str, &str)] = &[
        (
            "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 projects",
            "/",
        ),
        (
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 notes.txt",
            "/",
        ),
        (
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 my report.txt",
            "/",
        ),
        // DEFECT: runs of spaces inside a name are collapsed to one.
        (
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 a  b.txt",
            "/",
        ),
        (
            "lrwxrwxrwx    1 user     group           7 Jan 20 10:00 link -> target",
            "/",
        ),
        (
            "lrwxrwxrwx    1 user     group           7 Jan 20 10:00 dangling",
            "/",
        ),
        // DEFECT: an unparseable size becomes 0, indistinguishable from empty.
        (
            "-rw-r--r--    1 user     group        ???? Jan 20 10:00 odd.txt",
            "/",
        ),
        // DEFECT: fewer than nine tokens is dropped in silence.
        ("-rw-r--r-- 1 user group 123 Jan 20 10:00", "/"),
        (
            "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 .",
            "/",
        ),
        (
            "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 ..",
            "/",
        ),
        (
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 f.txt",
            "/scope",
        ),
        (
            "-rw-r--r--    1 user     group         123 Jan 20 10:00 f.txt",
            "/scope/",
        ),
        ("", "/"),
        ("total 12", "/"),
        ("01-23-24  10:30AM       <DIR>          folder", "/"),
        ("01-23-24  10:30AM           12345      file.txt", "/"),
        ("01-23-2024  10:30AM         12345      file.txt", "/"),
        ("01-23-24  10:30AM           12345      my file.txt", "/"),
        // DEFECT: a DOS row with a long enough name reaches nine tokens and is
        // taken by the Unix parser first, because the dispatch tries Unix and
        // only falls back when it fails.
        ("01-23-24 10:30AM 12345 a b c d e f", "/"),
        ("not-a-date 10:30AM <DIR> folder", "/"),
        // The DOS parser's guard: a Unix row with numeric owner/group must not
        // be resurrected as a DOS file.
        ("drwxr-xr-x 2 1001 1001 4096 Jul 21 09:41 .", "/"),
    ];

    const CHARAC_MLSD_ROWS: &[(&str, &str)] = &[
        ("type=dir;modify=20260120100000; projects", "/home"),
        (
            "type=file;size=123;modify=20260120100000; notes.txt",
            "/home",
        ),
        (
            "type=file;size=123;modify=20260120100000; my report.txt",
            "/home",
        ),
        (
            "type=file;size=????;modify=20260120100000; odd.txt",
            "/home",
        ),
        ("type=cdir;modify=20260101000000; .", "/"),
        ("type=pdir;modify=20260101000000; ..", "/"),
        ("type=file;size=1; /absolute/path/name.txt", "/"),
        ("no-facts-here", "/"),
    ];

    fn charac_table() -> String {
        let p = charac_provider();
        let mut out = String::new();
        for (line, base) in CHARAC_LIST_ROWS {
            let rendered = match p.parse_listing(line, base) {
                None => "<none>".to_string(),
                Some(e) => format!(
                    "name={:?} path={:?} dir={} size={} sym={} link={:?} perms={:?} owner={:?} group={:?} mod={:?}",
                    e.name, e.path, e.is_dir, e.size, e.is_symlink, e.link_target,
                    e.permissions, e.owner, e.group, e.modified
                ),
            };
            out.push_str(&format!("LIST {line:?} @ {base:?}\n  {rendered}\n"));
        }
        for (line, base) in CHARAC_MLSD_ROWS {
            let rendered = match p.parse_mlsd_entry(line, base) {
                None => "<none>".to_string(),
                Some(e) => format!(
                    "name={:?} path={:?} dir={} size={} mod={:?}",
                    e.name, e.path, e.is_dir, e.size, e.modified
                ),
            };
            out.push_str(&format!("MLSD {line:?} @ {base:?}\n  {rendered}\n"));
        }
        out
    }

    #[test]
    fn charac_listing_parser_behaviour_is_unchanged() {
        let actual = charac_table();
        assert_eq!(
            actual.trim(),
            CHARAC_BASELINE.trim(),
            "\nthe listing parser changed behaviour\n--- ACTUAL ---\n{actual}\n--- EXPECTED ---\n{}\n",
            CHARAC_BASELINE
        );
    }

    const CHARAC_BASELINE: &str = r#"LIST "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 projects" @ "/"
  name="projects" path="/projects" dir=true size=4096 sym=false link=None perms=Some("drwxr-xr-x") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "-rw-r--r--    1 user     group         123 Jan 20 10:00 notes.txt" @ "/"
  name="notes.txt" path="/notes.txt" dir=false size=123 sym=false link=None perms=Some("-rw-r--r--") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "-rw-r--r--    1 user     group         123 Jan 20 10:00 my report.txt" @ "/"
  name="my report.txt" path="/my report.txt" dir=false size=123 sym=false link=None perms=Some("-rw-r--r--") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "-rw-r--r--    1 user     group         123 Jan 20 10:00 a  b.txt" @ "/"
  name="a b.txt" path="/a b.txt" dir=false size=123 sym=false link=None perms=Some("-rw-r--r--") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "lrwxrwxrwx    1 user     group           7 Jan 20 10:00 link -> target" @ "/"
  name="link" path="/link" dir=false size=7 sym=true link=Some("target") perms=Some("lrwxrwxrwx") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "lrwxrwxrwx    1 user     group           7 Jan 20 10:00 dangling" @ "/"
  name="dangling" path="/dangling" dir=false size=7 sym=true link=None perms=Some("lrwxrwxrwx") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "-rw-r--r--    1 user     group        ???? Jan 20 10:00 odd.txt" @ "/"
  name="odd.txt" path="/odd.txt" dir=false size=0 sym=false link=None perms=Some("-rw-r--r--") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "-rw-r--r-- 1 user group 123 Jan 20 10:00" @ "/"
  <none>
LIST "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 ." @ "/"
  <none>
LIST "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 .." @ "/"
  <none>
LIST "-rw-r--r--    1 user     group         123 Jan 20 10:00 f.txt" @ "/scope"
  name="f.txt" path="/scope/f.txt" dir=false size=123 sym=false link=None perms=Some("-rw-r--r--") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "-rw-r--r--    1 user     group         123 Jan 20 10:00 f.txt" @ "/scope/"
  name="f.txt" path="/scope/f.txt" dir=false size=123 sym=false link=None perms=Some("-rw-r--r--") owner=Some("user") group=Some("group") mod=Some("Jan 20 10:00")
LIST "" @ "/"
  <none>
LIST "total 12" @ "/"
  <none>
LIST "01-23-24  10:30AM       <DIR>          folder" @ "/"
  name="folder" path="/folder" dir=true size=0 sym=false link=None perms=None owner=None group=None mod=Some("01-23-24 10:30AM")
LIST "01-23-24  10:30AM           12345      file.txt" @ "/"
  name="file.txt" path="/file.txt" dir=false size=12345 sym=false link=None perms=None owner=None group=None mod=Some("01-23-24 10:30AM")
LIST "01-23-2024  10:30AM         12345      file.txt" @ "/"
  name="file.txt" path="/file.txt" dir=false size=12345 sym=false link=None perms=None owner=None group=None mod=Some("01-23-2024 10:30AM")
LIST "01-23-24  10:30AM           12345      my file.txt" @ "/"
  name="my file.txt" path="/my file.txt" dir=false size=12345 sym=false link=None perms=None owner=None group=None mod=Some("01-23-24 10:30AM")
LIST "01-23-24 10:30AM 12345 a b c d e f" @ "/"
  name="f" path="/f" dir=false size=0 sym=false link=None perms=Some("01-23-24") owner=Some("12345") group=Some("a") mod=Some("c d e")
LIST "not-a-date 10:30AM <DIR> folder" @ "/"
  <none>
LIST "drwxr-xr-x 2 1001 1001 4096 Jul 21 09:41 ." @ "/"
  <none>
MLSD "type=dir;modify=20260120100000; projects" @ "/home"
  name="projects" path="/home/projects" dir=true size=0 mod=Some("2026-01-20 10:00:00Z")
MLSD "type=file;size=123;modify=20260120100000; notes.txt" @ "/home"
  name="notes.txt" path="/home/notes.txt" dir=false size=123 mod=Some("2026-01-20 10:00:00Z")
MLSD "type=file;size=123;modify=20260120100000; my report.txt" @ "/home"
  name="my report.txt" path="/home/my report.txt" dir=false size=123 mod=Some("2026-01-20 10:00:00Z")
MLSD "type=file;size=????;modify=20260120100000; odd.txt" @ "/home"
  name="odd.txt" path="/home/odd.txt" dir=false size=0 mod=Some("2026-01-20 10:00:00Z")
MLSD "type=cdir;modify=20260101000000; ." @ "/"
  <none>
MLSD "type=pdir;modify=20260101000000; .." @ "/"
  <none>
MLSD "type=file;size=1; /absolute/path/name.txt" @ "/"
  name="name.txt" path="/absolute/path/name.txt" dir=false size=1 mod=None
MLSD "no-facts-here" @ "/"
  <none>"#;

    #[test]
    fn test_parse_unix_listing() {
        let line = "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 projects";
        let entry = super::super::ftp_listing::parse_unix_listing(line, "/").unwrap();

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
    fn list_a_output_with_numeric_ids_still_drops_dot_dirs() {
        // vsftpd's default `text_userdb_names=NO` renders owner/group as
        // numeric ids. The `.`/`..` rows then carry a numeric third token,
        // which passed the old "parts[2] is numeric" DOS guard and was
        // resurrected as a bogus file ("1001 4096 Jul 21 09:41 .") that
        // recursive delete tried to DELE: the server answered
        // `550 Delete operation failed` and the whole delete aborted.
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
            "drwxr-xr-x    2 1001     1001         4096 Jul 21 09:41 .",
            "drwxr-xr-x    3 1001     1001         4096 Jul 21 09:41 ..",
            "-rw-r--r--    1 1001     1001            6 Jul 21 09:41 a.txt",
        ];
        let names: Vec<String> = listing
            .iter()
            .filter_map(|line| provider.parse_listing(line, "/scope"))
            .map(|entry| entry.name)
            .collect();

        assert_eq!(names, vec!["a.txt"]);
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
        let line = "01-20-26  10:00AM       <DIR>          Projects";
        let entry = super::super::ftp_listing::parse_dos_listing(line, "/").unwrap();

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

    #[test]
    fn captured_connection_spec_unlocks_pool_kind_and_strict_range_capability() {
        // Speed-button audit contract (PD-FTP-1): before connect() the
        // provider must NOT overclaim intra-file parallelism (LockedSingle,
        // capability derived off the pool branch); once the connection spec
        // is captured it advertises FtpConnectionPool plus strict concurrent
        // range download, which is what lets the GUI single-file segmented
        // path dial N independent FTP connections for one large file.
        let config = FtpConfig {
            host: "example.invalid".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::Explicit,
            verify_cert: true,
            initial_path: None,
        };
        let mut provider = FtpProvider::new(config.clone());
        assert!(matches!(
            provider.transfer_executor_kind(),
            ProviderTransferExecutorKind::LockedSingle
        ));
        provider.connection_spec = Some(config);
        assert!(matches!(
            provider.transfer_executor_kind(),
            ProviderTransferExecutorKind::FtpConnectionPool
        ));
        assert!(matches!(
            provider
                .transfer_capabilities()
                .strict_concurrent_range_download,
            crate::transfer_dag::Capability::Supported
        ));
    }

    #[test]
    fn transfer_executor_max_sessions_matches_maximum_5x_speed_tier() {
        // PD-FTP-1 (Task 2): the file-level clone-pool cap must equal the
        // FTP/FTPS speed button's Maximum (5x) tier, so a Max-preset
        // multi-file batch dials up to 5 independent FTP connections and is
        // not silently clamped to 4 (which would lose one channel versus a
        // fair `rclone --transfers 5`).
        let provider = FtpProvider::new(FtpConfig {
            host: "example.invalid".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });
        assert_eq!(provider.transfer_executor_max_sessions(), 5);
    }

    #[test]
    fn set_multi_thread_download_clamps_streams_and_floors_cutoff() {
        // The CLI `--multi-thread-streams` path and any future GUI wiring
        // share these bounds: streams clamp to [1, FTP_MULTI_THREAD_MAX_STREAMS]
        // and the cutoff never drops below 1 MiB.
        let mut provider = FtpProvider::new(FtpConfig {
            host: "example.invalid".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });
        assert_eq!(provider.multi_thread_streams, 1);
        provider.set_multi_thread_download(999, 0);
        assert_eq!(provider.multi_thread_streams, FTP_MULTI_THREAD_MAX_STREAMS);
        assert_eq!(provider.multi_thread_cutoff, 1024 * 1024);
        provider.set_multi_thread_download(0, 50 * 1024 * 1024);
        assert_eq!(provider.multi_thread_streams, 1);
        assert_eq!(provider.multi_thread_cutoff, 50 * 1024 * 1024);
        provider.set_multi_thread_download(4, 250 * 1024 * 1024);
        assert_eq!(provider.multi_thread_streams, 4);
    }

    #[test]
    fn download_buffer_default_is_64k_and_cli_override_still_wins() {
        // Buffer A/B contract (2026-07-23, lab FTPS single-stream): the
        // provider default is the measured 64 KiB winner, and an explicit
        // `--buffer-size` (set_chunk_sizes) keeps overriding it within the
        // documented [4 KiB, 16 MiB] clamp.
        let mut provider = FtpProvider::new(FtpConfig {
            host: "example.invalid".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });
        assert_eq!(provider.buffer_size, FTP_DOWNLOAD_BUFFER_DEFAULT);
        provider.set_chunk_sizes(None, Some(256 * 1024));
        assert_eq!(provider.buffer_size, 256 * 1024);
        provider.set_chunk_sizes(None, Some(1024));
        assert_eq!(provider.buffer_size, 4096);
        provider.set_chunk_sizes(None, Some(64 * 1024 * 1024));
        assert_eq!(provider.buffer_size, 16 * 1024 * 1024);
        // A no-op call (no overrides) must leave the tuned default untouched.
        let mut fresh = FtpProvider::new(FtpConfig {
            host: "example.invalid".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });
        fresh.set_chunk_sizes(None, None);
        assert_eq!(fresh.buffer_size, FTP_DOWNLOAD_BUFFER_DEFAULT);
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
