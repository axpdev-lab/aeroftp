//! Library surface for the isolated L0 spike crate.
//! The real integration (once deps are resolved in the main tree) will
//! live under src-tauri/src/peer/.

pub mod crypto;
pub mod drive;
pub mod endpoint;
pub mod identity;
pub mod protocol;

pub use crypto::{
    decode_secret, decrypt_blob, derive_drive_key, derive_session_key, encode_secret, encrypt_blob,
    generate_pairing_secret, PAIRING_SECRET_LEN,
};
pub use endpoint::{PeerBlobOffer, PeerEndpoint, PeerEndpointConfig};
pub use identity::{open_capability, seal, seal_capability, Capability, Identity, IdentityPublic};
pub use protocol::{
    recv_blob, recv_encrypted_blob, recv_offer, send_blob, send_encrypted_blob, send_offer,
};

pub use crate::endpoint::ConnectivitySample;

// Re-export the ALPN constant so the binary and future tests can share it.
pub const PEER_L0_ALPN: &[u8] = b"/aeroftp/peer/l0";
