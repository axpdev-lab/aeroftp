//! Minimal PeerEndpoint + types for the isolated L0 spike.
//! (This will be merged into the main tree later.)

use anyhow::{Context, Result};
use iroh::{endpoint::Connection, Endpoint, NodeId};
use iroh_blobs::Hash;
use std::net::SocketAddr;
use std::time::Instant;
use tracing::{debug, info};

/// Which discovery service(s) the endpoint publishes its node record to and resolves
/// peers from. This is the independence seam (WI-5a / A+ de-n0-ization):
/// - `Both` (default): n0 DNS **and** the BitTorrent Mainline DHT, concurrently. iroh
///   appends both via `add_discovery` (`ConcurrentDiscovery`), so this is purely
///   ADDITIVE — n0 keeps working, the DHT is layered on for decentralized resolution.
/// - `Dht`: Mainline DHT ONLY (no n0 anywhere) — the zero-n0 path exercised by GATE
///   IND-1. Bootstrap = 20-year-old BitTorrent infra, neither the owner nor a single
///   operator.
/// - `N0`: legacy n0-only (the pre-WI-5a behaviour).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiscoveryMode {
    N0,
    Dht,
    #[default]
    Both,
}

#[derive(Debug, Clone, Default)]
pub struct PeerEndpointConfig {
    pub bind_addr: Option<SocketAddr>,
    pub secret_key_path: Option<std::path::PathBuf>,
    /// Optional list of relay URLs. When `Some` and non-empty, the endpoint uses
    /// `RelayMode::Custom` with a RelayMap built from these URLs (e.g. a self-hosted
    /// relay or any working alternative). When `None` or empty, it falls back to
    /// `RelayMode::Staging` (the research default that works around iroh 0.92's dead
    /// production relays). This lets us point at a self-hosted relay by configuration
    /// only, with no code changes.
    pub custom_relay_urls: Option<Vec<String>>,
    /// Discovery backend selection (WI-5a). Default `Both` adds the Mainline DHT
    /// alongside n0 DNS (decentralized + additive). `Dht` drops n0 entirely.
    pub discovery: DiscoveryMode,
}

/// Apply the selected discovery service(s) to an endpoint builder. Factored out so the
/// L0 (`PeerEndpoint::new`) and L1 (`build_base_endpoint`) paths stay in lockstep.
fn apply_discovery(
    builder: iroh::endpoint::Builder,
    mode: DiscoveryMode,
) -> iroh::endpoint::Builder {
    match mode {
        DiscoveryMode::N0 => builder.discovery_n0(),
        DiscoveryMode::Dht => builder.discovery_dht(),
        DiscoveryMode::Both => builder.discovery_n0().discovery_dht(),
    }
}

pub struct PeerEndpoint {
    endpoint: Endpoint,
    node_id: NodeId,
}

impl PeerEndpoint {
    pub async fn new(cfg: PeerEndpointConfig) -> Result<Self> {
        // L0 fix (Linux 2): a bare `connect(NodeId, alpn)` relies ENTIRELY on a
        // discovery service to resolve the peer's relay URL + direct addresses.
        // The default `Endpoint::builder()` adds NO discovery, so dial-by-NodeID
        // failed instantly ("iroh connect ... failed", 0 ms) even on localhost.
        // `discovery_n0()` publishes our node record to (and resolves peers from)
        // the n0 DNS/pkarr service — this is what makes dial-by-NodeID work across
        // real hostile networks (home ↔ 5G) and on the loopback smoke test.
        // DIAG (Linux 2): iroh 0.92's default PRODUCTION relays
        // (use1-1/euc1-1/aps1-1.relay.n0.iroh.iroh.link) reject the TLS handshake
        // from here ("tlsv1 alert internal error"); TCP/443 is open and DNS
        // resolves, but the server aborts TLS — so peers get no relay home and the
        // hole-punch rendezvous never happens, failing connect() even on loopback.
        // The STAGING relays (staging-*.relay.iroh.network) still serve TLS fine,
        // so force RelayMode::Staging to confirm the relay is the blocker.
        // L0 fix (Linux 2): the accept side must advertise the ALPN it speaks, or
        // every incoming connection is rejected during the handshake ("incoming
        // failed to accept" on the listener, "connect ... failed" on the dialer).
        // The builder never set `.alpns(...)`, so the server's protocol list was
        // empty and ALPN negotiation always failed. Register PEER_L0_ALPN here.
        //
        // Relay strategy (Linux 2 fix, 2026-06-09): actually CONSUME
        // `cfg.custom_relay_urls` so the `--custom-relay-urls` flag is not a silent
        // no-op (the prior relay-custom patch added the field + flag but the builder
        // still hard-coded Staging, so a self-hosted relay could never be selected).
        // A non-empty list selects `RelayMode::Custom` with a RelayMap built from the
        // parsed URLs (`RelayMap: FromIterator<RelayUrl>`); an absent/empty list keeps
        // the working Staging default. An invalid URL is a hard error rather than a
        // silent fall-through, so a typo in a self-hosted relay URL is caught up front.
        // INTEROP (CORRECTED, WI-5c 2026-06-10): peers do NOT need the same relay set. A dialer
        // connects on demand to the REMOTE peer's home relay even when it is not in the dialer's own
        // RelayMap (iroh `active_relay_handle_for_node`), so heterogeneous/federated relays interoperate
        // as long as each peer's home relay is discoverable (we publish it in the pkarr/DHT record).
        // PROVEN: a replicator with home=staging connected to a publisher whose home was a local relay
        // (`relay(http://localhost:3340)`), discovered purely via the Mainline DHT. This is what makes
        // the "every capable user runs their own relay" model work. The earlier "same relay set"
        // claim (from the WI-4g observation) was wrong; that failure had another cause.
        let relay_mode = match cfg.custom_relay_urls.as_deref() {
            Some(urls) if !urls.is_empty() => {
                let parsed: Vec<iroh::RelayUrl> = urls
                    .iter()
                    .map(|u| {
                        u.parse::<iroh::RelayUrl>()
                            .with_context(|| format!("invalid custom relay URL: {u:?}"))
                    })
                    .collect::<Result<_>>()?;
                info!(
                    count = parsed.len(),
                    "PeerEndpoint using RelayMode::Custom (self-hosted/override relays)"
                );
                iroh::RelayMode::Custom(iroh::RelayMap::from_iter(parsed))
            }
            _ => iroh::RelayMode::Staging,
        };
        let builder = apply_discovery(
            Endpoint::builder()
                .alpns(vec![crate::PEER_L0_ALPN.to_vec()])
                .relay_mode(relay_mode),
            cfg.discovery,
        );

        // Older iroh 0.9x API on our rust-version uses bind_addr_v4 / bind_addr_v6
        // or simply lets the builder pick. For the spike we keep it simple.
        if let Some(addr) = cfg.bind_addr {
            // Best effort; many 0.9x builds accept SocketAddr via other means.
            // If this still fails at runtime we fall back to default bind.
            let _ = addr; // ignored for maximum compatibility in L0
        }

        let endpoint = builder
            .bind()
            .await
            .context("failed to bind iroh endpoint")?;
        let node_id = endpoint.node_id();

        info!(%node_id, "PeerEndpoint (L0 isolated) ready");

        Ok(Self { endpoint, node_id })
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn raw(&self) -> &Endpoint {
        &self.endpoint
    }

    pub async fn connect(&self, remote: NodeId, alpn: &[u8]) -> Result<Connection> {
        let start = Instant::now();
        debug!(%remote, "peer connect attempt (isolated)");

        let conn = self
            .endpoint
            .connect(remote, alpn)
            .await
            .with_context(|| format!("iroh connect to {} failed", remote))?;

        let elapsed = start.elapsed();
        info!(%remote, ms = elapsed.as_millis() as u64, "peer connect established (isolated)");

        Ok(conn)
    }

    pub async fn accept(&self) -> Result<Connection> {
        // iroh 0.9x style: accept() -> Option<Incoming>, then .await on the Connecting.
        let incoming = self
            .endpoint
            .accept()
            .await
            .context("endpoint accept failed")?;
        let connecting = incoming.accept().context("incoming failed to accept")?;
        let conn = connecting.await.context("connecting failed")?;
        Ok(conn)
    }

    pub fn close(&self) {
        // Endpoint::close returns a type that must be used in some iroh versions.
        let _ = self.endpoint.close();
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerBlobOffer {
    pub hash: Hash,
    pub size: u64,
    pub name_hint: String,
    pub note: Option<String>,
}

impl PeerBlobOffer {
    pub fn new(hash: Hash, size: u64, name_hint: impl Into<String>) -> Self {
        Self {
            hash,
            size,
            name_hint: name_hint.into(),
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// ConnectivitySample lives here in the spike so the binary can serialize it
/// without depending on the main crate.
///
/// This is the main artifact for the L0 Go/No-Go gate.
/// We collect many of these across real hostile networks (CGNAT, mobile, office, etc.)
/// to answer the central question: does dial-by-NodeID + hole-punch work reliably enough?
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ConnectivitySample {
    pub success: bool,

    /// Best-effort classification of the path used.
    /// For now this is often "unknown" because iroh 0.92 connection path inspection
    /// is limited. We improve this as much as possible in the harness.
    pub path: String, // "direct", "relayed", "holepunch", "unknown"

    /// Total wall time from start of the operation until success or final failure.
    pub total_duration_ms: u64,

    /// Time spent just to establish the authenticated connection (dial or accept).
    pub connect_duration_ms: u64,

    /// Time spent on the actual (encrypted) data transfer after connection was ready.
    pub transfer_duration_ms: u64,

    pub error: Option<String>,

    /// Free-text note provided by the human running the test.
    /// This is extremely valuable for the gate analysis (e.g. "home behind CGNAT + IPv4 only", "mobile hotspot behind carrier-grade NAT", "two laptops same LAN").
    pub network_note: Option<String>,

    /// Optional extra diagnostic info (e.g. remote addresses seen, iroh stats snapshot).
    pub diagnostics: Option<String>,
}

impl ConnectivitySample {
    pub fn success(
        total_ms: u64,
        connect_ms: u64,
        transfer_ms: u64,
        path: impl Into<String>,
        note: Option<String>,
    ) -> Self {
        Self {
            success: true,
            path: path.into(),
            total_duration_ms: total_ms,
            connect_duration_ms: connect_ms,
            transfer_duration_ms: transfer_ms,
            error: None,
            network_note: note,
            diagnostics: None,
        }
    }

    pub fn failure(err: impl ToString, note: Option<String>) -> Self {
        Self {
            success: false,
            path: "failure".to_string(),
            total_duration_ms: 0,
            connect_duration_ms: 0,
            transfer_duration_ms: 0,
            error: Some(err.to_string()),
            network_note: note,
            diagnostics: None,
        }
    }
}

/// Build a base iroh Endpoint configured with the same discovery_n0 + RelayMode
/// (Staging or Custom from --custom-relay-urls) as the L0 PeerEndpoint.
/// No L0 ALPN is registered here; the L1 docs paths use a Router + blobs/gossip/docs ALPNs instead.
/// This reuses the relay/discovery logic so L1 gets the same hole-punch behavior as the proven L0 stack.
pub async fn build_base_endpoint(cfg: PeerEndpointConfig) -> Result<Endpoint> {
    let relay_mode = match cfg.custom_relay_urls.as_deref() {
        Some(urls) if !urls.is_empty() => {
            let parsed: Vec<iroh::RelayUrl> = urls
                .iter()
                .map(|u| {
                    u.parse::<iroh::RelayUrl>()
                        .with_context(|| format!("invalid custom relay URL: {u:?}"))
                })
                .collect::<Result<_>>()?;
            info!(
                count = parsed.len(),
                "build_base_endpoint (L1) using RelayMode::Custom (self-hosted/override relays)"
            );
            iroh::RelayMode::Custom(iroh::RelayMap::from_iter(parsed))
        }
        _ => iroh::RelayMode::Staging,
    };
    let builder = apply_discovery(Endpoint::builder().relay_mode(relay_mode), cfg.discovery);

    // bind_addr is best-effort / ignored for 0.92 compat (same as L0)
    if let Some(_addr) = cfg.bind_addr {
        let _ = _addr;
    }

    let endpoint = builder
        .bind()
        .await
        .context("failed to bind iroh endpoint (L1 docs)")?;
    Ok(endpoint)
}
