//! Transfer engine router.
//!
//! Decides at runtime whether to route a single-file transfer through the
//! DAG-shaped engine or through the legacy provider-direct path. The
//! decision is data-driven from the Phase A benchmark matrix (round 1
//! 2026-05-24 + overnight cross-Nextcloud 2026-05-25).
//!
//! Net finding: DAG wins on average and wins massively on high-latency
//! providers (Koofr WAN upload 1G: +132.9% throughput, download 100M:
//! +161.6%). A suspected WebDAV "vanilla" download regression was
//! revalidated in T-DEBT-RTR-04 and not reproduced as an engine-level
//! difference: forced DAG and forced Legacy had identical network syscall
//! shape. Default routing is therefore DAG everywhere except local-to-local.
//!
//! Design decisions:
//! - Default policy is `Engine::Dag` for every network-backed provider.
//!   `Engine::Legacy` remains available as an explicit override and for
//!   local-to-local transfers.
//! - The router is a pure function: `(provider_hint, operation, size) ->
//!   Engine`. No I/O, no global state, trivial to test.
//! - User overrides (`--transfer-engine dag|legacy`) short-circuit the
//!   lookup before the hint table is consulted.
//! - The hint table is owned by [`hints`] and lives alongside the unit
//!   tests that lock in the Phase A measurements. Updating it requires
//!   pointing at a new benchmark JSON in the doc comment, not changing
//!   call sites.

pub mod hints;

use std::fmt;

/// Which transfer engine to route a single-file operation through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    /// Shaped-file DAG runner (`crate::transfer_dag_single_file`).
    Dag,
    /// Legacy provider-direct path (the pre-DAG behaviour). Falls back to
    /// `provider.download` / `provider.upload` without wrapping the call
    /// in a DAG node graph.
    Legacy,
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Engine::Dag => f.write_str("dag"),
            Engine::Legacy => f.write_str("legacy"),
        }
    }
}

/// Operation kind for routing purposes. The router does not need the
/// full payload, only the I/O direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Upload,
    Download,
}

/// Coarse provider classification for routing. The router does not need
/// to know which specific cloud backend is in use, only the family it
/// belongs to in terms of transfer behaviour. WebDAV is split into
/// `WebDavVanilla` vs `WebDavNextcloud` because that distinction is
/// exactly what the Phase A matrix found to matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderHint {
    /// Apache mod_dav, lighttpd mod_webdav, nginx_dav, and anything else
    /// that responds to WebDAV requests without a known Nextcloud or
    /// gateway signal.
    WebDavVanilla,
    /// Nextcloud / ownCloud detected via URL pattern. Koofr WebDAV
    /// gateway also belongs here in spirit: gateway-style WebDAV with
    /// per-request server overhead, which is exactly the shape that
    /// benefits most from the DAG runner.
    WebDavNextcloud,
    /// Koofr WebDAV gateway (no Nextcloud URL pattern, but per-request
    /// server overhead profile). Kept distinct from `WebDavNextcloud`
    /// because the URL detection cannot conflate them and the user may
    /// want a different override policy.
    WebDavGateway,
    /// Native S3 (AWS / Wasabi / MinIO / DO / etc).
    S3,
    /// Backblaze B2 native.
    B2,
    /// Azure Blob Storage native. Uses Put Block / Put Block List for
    /// multipart uploads and HTTP Range for segmented downloads.
    AzureBlob,
    /// SFTP.
    Sftp,
    /// Plain FTP / FTPS.
    Ftp,
    /// OAuth cloud (Google Drive, Dropbox, Box, OneDrive, pCloud, Zoho,
    /// kDrive, Yandex, Internxt, Filen, MEGA, Jottacloud, 4shared,
    /// FileLu, OpenDrive, ImageKit, Uploadcare, Cloudinary). Treated as
    /// a single class until per-provider measurements differentiate.
    OAuthCloud,
    /// Local-to-local transfer. Never routed to DAG (DAG is a network
    /// engine, local I/O always uses the direct filesystem path).
    Local,
}

/// Caller-provided override (typically `--force-dag` / `--force-legacy`
/// on the CLI, or a session-level toggle in the GUI).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Override {
    /// No override, the router consults the hint table.
    #[default]
    None,
    /// Force `Engine::Dag` regardless of hints.
    ForceDag,
    /// Force `Engine::Legacy` regardless of hints.
    ForceLegacy,
}

impl Override {
    /// Read the `AEROFTP_TRANSFER_ENGINE` environment variable and turn
    /// it into an [`Override`]. Unknown / unset values map to
    /// [`Override::None`] (the data-driven router is consulted). This
    /// is the only override channel for the GUI Tauri commands, which
    /// do not parse CLI flags themselves.
    pub fn from_env() -> Self {
        match std::env::var("AEROFTP_TRANSFER_ENGINE").as_deref() {
            Ok("dag") => Override::ForceDag,
            Ok("legacy") => Override::ForceLegacy,
            _ => Override::None,
        }
    }
}

/// Routing context fed to [`Router::pick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteContext {
    pub provider: ProviderHint,
    pub operation: Operation,
    pub size_bytes: u64,
    pub user_override: Override,
}

impl RouteContext {
    pub fn new(provider: ProviderHint, operation: Operation, size_bytes: u64) -> Self {
        Self {
            provider,
            operation,
            size_bytes,
            user_override: Override::None,
        }
    }

    pub fn with_override(mut self, ovr: Override) -> Self {
        self.user_override = ovr;
        self
    }
}

/// Outcome of a routing decision. The `reason` is a static label
/// suitable for logging or telemetry: it never depends on user data so
/// it is safe to ship to community-benchmark reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub engine: Engine,
    pub reason: &'static str,
}

/// Stateless router. Construct once per process (cheap) and call
/// [`Router::pick`] per transfer.
#[derive(Debug, Default, Clone, Copy)]
pub struct Router;

impl Router {
    pub fn new() -> Self {
        Router
    }

    /// Decide which engine should handle a transfer.
    ///
    /// Order of evaluation:
    /// 1. User override short-circuits everything.
    /// 2. Local transfers never go through DAG.
    /// 3. Hint table lookup ([`hints::recommend`]).
    /// 4. Default to [`Engine::Dag`] (the round 1 + overnight matrix
    ///    showed net-positive averages everywhere DAG was measured).
    pub fn pick(&self, ctx: RouteContext) -> Decision {
        match ctx.user_override {
            Override::ForceDag => {
                return Decision {
                    engine: Engine::Dag,
                    reason: "user override: --force-dag",
                };
            }
            Override::ForceLegacy => {
                return Decision {
                    engine: Engine::Legacy,
                    reason: "user override: --force-legacy",
                };
            }
            Override::None => {}
        }

        if ctx.provider == ProviderHint::Local {
            return Decision {
                engine: Engine::Legacy,
                reason: "local-to-local: DAG is a network engine",
            };
        }

        hints::recommend(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(p: ProviderHint, op: Operation, size: u64) -> RouteContext {
        RouteContext::new(p, op, size)
    }

    #[test]
    fn force_dag_short_circuits_local() {
        // User override beats even the structural local-to-local rule:
        // this lets developers benchmark the DAG against a local mock
        // without removing the safety default.
        let d = Router::new().pick(
            ctx(ProviderHint::Local, Operation::Upload, 100).with_override(Override::ForceDag),
        );
        assert_eq!(d.engine, Engine::Dag);
        assert!(d.reason.contains("override"));
    }

    #[test]
    fn force_legacy_short_circuits_dag_winning_provider() {
        let d = Router::new().pick(
            ctx(
                ProviderHint::WebDavGateway,
                Operation::Download,
                100 * 1024 * 1024,
            )
            .with_override(Override::ForceLegacy),
        );
        assert_eq!(d.engine, Engine::Legacy);
    }

    #[test]
    fn local_to_local_never_goes_to_dag() {
        let d = Router::new().pick(ctx(ProviderHint::Local, Operation::Upload, 1 << 30));
        assert_eq!(d.engine, Engine::Legacy);
        assert!(d.reason.contains("local"));
    }

    #[test]
    fn engine_and_operation_display_are_stable_for_logs() {
        assert_eq!(format!("{}", Engine::Dag), "dag");
        assert_eq!(format!("{}", Engine::Legacy), "legacy");
    }
}
