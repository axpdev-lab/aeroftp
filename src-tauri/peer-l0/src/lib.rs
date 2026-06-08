//! Library surface for the isolated L0 spike crate.
//! The real integration (once deps are resolved in the main tree) will
//! live under src-tauri/src/peer/.

pub mod endpoint;
pub mod protocol;

pub use endpoint::{PeerBlobOffer, PeerEndpoint, PeerEndpointConfig};
pub use protocol::{recv_blob, recv_offer, send_blob, send_offer};

pub use crate::endpoint::ConnectivitySample; // re-export the one from endpoint for simplicity in spike

// Re-export the ALPN constant so the binary and future tests can share it.
pub const PEER_L0_ALPN: &[u8] = b"/aeroftp/peer/l0";
