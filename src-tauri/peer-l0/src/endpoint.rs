//! Minimal PeerEndpoint + types for the isolated L0 spike.
//! (This will be merged into the main tree later.)

use anyhow::{Context, Result};
use iroh::{endpoint::Connection, Endpoint, NodeId};
use iroh_blobs::Hash;
use std::net::SocketAddr;
use std::time::Instant;
use tracing::{debug, info};

#[derive(Debug, Clone, Default)]
pub struct PeerEndpointConfig {
    pub bind_addr: Option<SocketAddr>,
    pub secret_key_path: Option<std::path::PathBuf>,
}

pub struct PeerEndpoint {
    endpoint: Endpoint,
    node_id: NodeId,
}

impl PeerEndpoint {
    pub async fn new(cfg: PeerEndpointConfig) -> Result<Self> {
        let builder = Endpoint::builder();

        // Older iroh 0.9x API on our rust-version uses bind_addr_v4 / bind_addr_v6
        // or simply lets the builder pick. For the spike we keep it simple.
        if let Some(addr) = cfg.bind_addr {
            // Best effort; many 0.9x builds accept SocketAddr via other means.
            // If this still fails at runtime we fall back to default bind.
            let _ = addr; // ignored for maximum compatibility in L0
        }

        let endpoint = builder.bind().await.context("failed to bind iroh endpoint")?;
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConnectivitySample {
    pub success: bool,
    pub used_relay: bool,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub network_note: Option<String>,
}

impl ConnectivitySample {
    pub fn success(duration_ms: u64, used_relay: bool, note: Option<String>) -> Self {
        Self {
            success: true,
            used_relay,
            duration_ms,
            error: None,
            network_note: note,
        }
    }

    pub fn failure(err: impl ToString, note: Option<String>) -> Self {
        Self {
            success: false,
            used_relay: false,
            duration_ms: 0,
            error: Some(err.to_string()),
            network_note: note,
        }
    }
}
