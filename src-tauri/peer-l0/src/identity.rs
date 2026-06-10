//! Peer identity, sealed capabilities and AeroFTP-ID encoding (T-PEER production track, WI-4a).
//!
//! Storage-agnostic crypto for the user-to-user P2P drive. This module knows nothing about the
//! vault or iroh; it operates on key BYTES, so the per-partition storage facade (WI-4b) and the
//! publish/replicate wiring (WI-4c) build on it without touching the primitives.
//!
//! Key types (see APPENDIX-PEER/L2-DESIGN-peer-identity-keys-sharing-MVP.md):
//! - [`Identity`]: a user's Ed25519 signing key + X25519 sealing key. Private; the root of trust.
//! - [`IdentityPublic`]: the matching public keys = the shareable "AeroFTP ID".
//! - [`Capability`]: a per-drive grant, sealed to a recipient and signed by the issuer, carried as a token.
//!
//! Constructions (all standard, reusing the crate's existing aes-gcm / hkdf / sha2):
//! - sign / verify: Ed25519 (ed25519-dalek).
//! - seal / open: ephemeral X25519 ECDH -> HKDF-SHA256 -> AES-256-GCM (anonymous-sender box).
//! - capability: seal the JSON body to the recipient, then Ed25519-sign the sealed bytes; the opener checks that signature against the EXPECTED issuer.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret as XStaticSecret};

/// HKDF domain separation for the sealed-box key (bump the version if the construction changes).
const SEAL_INFO: &[u8] = b"aeroftp-peer-seal-v1";
/// Human-facing prefix on an AeroFTP ID string.
const AFID_PREFIX: &str = "AFID1";
/// URI scheme carrying a sealed + signed drive capability.
const DRIVE_SCHEME: &str = "aeroftp-drive://";

/// A user's PRIVATE P2P identity: an Ed25519 signing key (authorship / authentication) plus an
/// X25519 secret (receiving sealed capabilities). Never serialize except via [`Identity::to_secret_bytes`].
pub struct Identity {
    ed: SigningKey,
    x: XStaticSecret,
}

/// The public half of an [`Identity`] = the shareable "AeroFTP ID".
#[derive(Clone)]
pub struct IdentityPublic {
    ed: VerifyingKey,
    x: XPublicKey,
}

/// A per-drive read grant. Sealed to a recipient and signed by the issuer, then carried as a token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capability {
    /// The iroh-docs `NamespaceId` (drive address), hex.
    pub namespace_id: String,
    /// The 32-byte symmetric drive content key (what decrypts the manifest + blobs).
    pub content_key: [u8; 32],
    /// Publisher node addresses (ticket nodes) the recipient can dial.
    pub node_addrs: Vec<String>,
    /// Human label for the drive.
    pub drive_name: String,
    /// Drive version at issue time.
    pub version: u64,
    /// The recipient's Ed25519 public key this grant was issued to (binds the grant to a contact).
    pub granted_to_ed: [u8; 32],
    /// Unix seconds at issue time.
    pub issued_at: i64,
}

impl Identity {
    /// Generate a fresh identity from OS randomness.
    pub fn generate() -> Self {
        let mut ed_seed = [0u8; 32];
        OsRng.fill_bytes(&mut ed_seed);
        let mut x_seed = [0u8; 32];
        OsRng.fill_bytes(&mut x_seed);
        Self {
            ed: SigningKey::from_bytes(&ed_seed),
            x: XStaticSecret::from(x_seed),
        }
    }

    /// The public identity (AeroFTP ID material).
    pub fn public(&self) -> IdentityPublic {
        IdentityPublic {
            ed: self.ed.verifying_key(),
            x: XPublicKey::from(&self.x),
        }
    }

    /// Serialize the PRIVATE identity to 64 bytes (`ed32 || x32`) for the vault facade (WI-4b).
    /// Store this ONLY inside the per-user DEK-wrapped vault.
    pub fn to_secret_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.ed.to_bytes());
        out[32..].copy_from_slice(&self.x.to_bytes());
        out
    }

    /// Reconstruct an identity from [`Identity::to_secret_bytes`] output.
    pub fn from_secret_bytes(bytes: &[u8; 64]) -> Self {
        let mut ed = [0u8; 32];
        ed.copy_from_slice(&bytes[..32]);
        let mut x = [0u8; 32];
        x.copy_from_slice(&bytes[32..]);
        Self {
            ed: SigningKey::from_bytes(&ed),
            x: XStaticSecret::from(x),
        }
    }

    /// Sign an arbitrary message with the identity's Ed25519 key.
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.ed.sign(msg).to_bytes()
    }

    /// Open a sealed box addressed to this identity (reverse of [`seal`]). Fails closed on any
    /// tamper / wrong-recipient (AES-GCM tag) or malformed input.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 32 + 12 {
            bail!("sealed box too short");
        }
        let mut eph = [0u8; 32];
        eph.copy_from_slice(&sealed[..32]);
        let eph_pub = XPublicKey::from(eph);
        let nonce = &sealed[32..44];
        let ct = &sealed[44..];
        let shared = self.x.diffie_hellman(&eph_pub);
        let key = seal_key(shared.as_bytes());
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        cipher.decrypt(Nonce::from_slice(nonce), ct).map_err(|e| {
            anyhow::anyhow!("sealed box open failed (wrong recipient or tampered): {e}")
        })
    }
}

impl IdentityPublic {
    /// Encode as the shareable AeroFTP ID string: `AFID1<base64url(ed || x || checksum)>`.
    pub fn to_aeroftp_id(&self) -> String {
        let mut body = Vec::with_capacity(68);
        body.extend_from_slice(self.ed.as_bytes());
        body.extend_from_slice(self.x.as_bytes());
        let cksum = blake3::hash(&body);
        body.extend_from_slice(&cksum.as_bytes()[..4]);
        format!("{AFID_PREFIX}{}", b64(&body))
    }

    /// Parse + validate an AeroFTP ID (checks prefix, length and checksum). Fails closed.
    pub fn from_aeroftp_id(s: &str) -> Result<Self> {
        let rest = s
            .strip_prefix(AFID_PREFIX)
            .context("not an AeroFTP ID (bad prefix)")?;
        let raw = unb64(rest).context("AeroFTP ID is not valid base64url")?;
        if raw.len() != 68 {
            bail!("AeroFTP ID wrong length");
        }
        let (body, cksum) = raw.split_at(64);
        if cksum != &blake3::hash(body).as_bytes()[..4] {
            bail!("AeroFTP ID checksum mismatch");
        }
        let mut ed = [0u8; 32];
        ed.copy_from_slice(&body[..32]);
        let mut x = [0u8; 32];
        x.copy_from_slice(&body[32..64]);
        Ok(Self {
            ed: VerifyingKey::from_bytes(&ed).context("invalid Ed25519 key in AeroFTP ID")?,
            x: XPublicKey::from(x),
        })
    }

    /// Raw 32-byte Ed25519 public key (the issuer/recipient identifier used in capability tokens).
    pub fn ed_bytes(&self) -> [u8; 32] {
        self.ed.to_bytes()
    }

    /// Verify a signature produced by [`Identity::sign`] of the matching identity.
    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        self.ed.verify(msg, &Signature::from_bytes(sig)).is_ok()
    }
}

/// Seal `plaintext` so that ONLY `recipient` can open it (anonymous-sender box): ephemeral X25519 ->
/// ECDH -> HKDF-SHA256 -> AES-256-GCM. Output = `eph_pub(32) || nonce(12) || ciphertext`.
pub fn seal(recipient: &IdentityPublic, plaintext: &[u8]) -> Vec<u8> {
    let mut eph_seed = [0u8; 32];
    OsRng.fill_bytes(&mut eph_seed);
    let esk = XStaticSecret::from(eph_seed);
    let epk = XPublicKey::from(&esk);
    let shared = esk.diffie_hellman(&recipient.x);
    let key = seal_key(shared.as_bytes());
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .expect("aes-256-gcm encryption never fails for valid key/nonce");
    let mut out = Vec::with_capacity(32 + 12 + ct.len());
    out.extend_from_slice(epk.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Seal a capability to `recipient` and sign it with `issuer`; returns an `aeroftp-drive://` token.
/// Token bytes = `issuer_ed(32) || signature(64) || sealed`.
pub fn seal_capability(
    issuer: &Identity,
    recipient: &IdentityPublic,
    cap: &Capability,
) -> Result<String> {
    let body = serde_json::to_vec(cap).context("serialize capability")?;
    let sealed = seal(recipient, &body);
    let sig = issuer.sign(&sealed);
    let mut token = Vec::with_capacity(32 + 64 + sealed.len());
    token.extend_from_slice(&issuer.public().ed_bytes());
    token.extend_from_slice(&sig);
    token.extend_from_slice(&sealed);
    Ok(format!("{DRIVE_SCHEME}{}", b64(&token)))
}

/// Open a capability token. Requires it to come from `expected_issuer` (defends against a forged
/// issuer field), verifies the signature, then unseals to this identity. Fails closed on any step.
pub fn open_capability(
    me: &Identity,
    expected_issuer: &IdentityPublic,
    token: &str,
) -> Result<Capability> {
    let raw = token
        .strip_prefix(DRIVE_SCHEME)
        .context("not a drive token (bad scheme)")?;
    let bytes = unb64(raw).context("drive token is not valid base64url")?;
    if bytes.len() < 32 + 64 {
        bail!("drive token too short");
    }
    if bytes[..32] != expected_issuer.ed_bytes() {
        bail!("drive token issuer does not match the expected contact");
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&bytes[32..96]);
    let sealed = &bytes[96..];
    if !expected_issuer.verify(sealed, &sig) {
        bail!("drive token signature invalid");
    }
    let body = me
        .open(sealed)
        .context("could not unseal drive token (not the named recipient?)")?;
    serde_json::from_slice(&body).context("malformed capability body")
}

// --------------------------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------------------------

/// HKDF-SHA256 the raw X25519 shared secret into a 32-byte AES key, domain-separated by [`SEAL_INFO`].
fn seal_key(shared: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(SEAL_INFO, &mut key)
        .expect("hkdf-sha256 expand of 32 bytes is always valid");
    key
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>> {
    base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s.trim())
        .map_err(|e| anyhow::anyhow!("base64url decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cap(recipient: &IdentityPublic) -> Capability {
        Capability {
            namespace_id: "8ce68153cc3b80d778b594b7e3787e35".to_string(),
            content_key: [7u8; 32],
            node_addrs: vec!["node-a".into(), "node-b".into()],
            drive_name: "photos".into(),
            version: 3,
            granted_to_ed: recipient.ed_bytes(),
            issued_at: 1_749_500_000,
        }
    }

    // ---- positive ----

    #[test]
    fn t1_sign_verify_roundtrip() {
        let id = Identity::generate();
        let pubk = id.public();
        let msg = b"the quick brown fox";
        let sig = id.sign(msg);
        assert!(pubk.verify(msg, &sig), "good signature must verify");
        let mut bad = msg.to_vec();
        bad[0] ^= 0x01;
        assert!(
            !pubk.verify(&bad, &sig),
            "tampered message must fail verify"
        );
    }

    #[test]
    fn t2_aeroftp_id_roundtrip() {
        let id = Identity::generate();
        let pubk = id.public();
        let s = pubk.to_aeroftp_id();
        assert!(s.starts_with("AFID1"));
        let back = IdentityPublic::from_aeroftp_id(&s).expect("decode");
        assert_eq!(back.ed_bytes(), pubk.ed_bytes());
        assert_eq!(back.x.as_bytes(), pubk.x.as_bytes());
    }

    #[test]
    fn t3_seal_open_roundtrip() {
        let bob = Identity::generate();
        let pt = b"a 32-byte content key would go here";
        let sealed = seal(&bob.public(), pt);
        let got = bob.open(&sealed).expect("bob opens his own box");
        assert_eq!(got, pt);
    }

    #[test]
    fn t4_capability_roundtrip() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let cap = sample_cap(&bob.public());
        let token = seal_capability(&alice, &bob.public(), &cap).expect("seal");
        assert!(token.starts_with("aeroftp-drive://"));
        let got = open_capability(&bob, &alice.public(), &token).expect("bob opens, issuer=alice");
        assert_eq!(
            got, cap,
            "every field incl. the 32-byte content_key round-trips"
        );
    }

    // ---- negative (must fail closed) ----

    #[test]
    fn n1_aeroftp_id_flipped_byte_rejected() {
        let id = Identity::generate();
        let mut s = id.public().to_aeroftp_id();
        // flip a byte in the encoded body (after the prefix)
        let mut chars: Vec<char> = s.chars().collect();
        let i = AFID_PREFIX.len() + 3;
        chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
        s = chars.into_iter().collect();
        assert!(
            IdentityPublic::from_aeroftp_id(&s).is_err(),
            "checksum must reject a flipped byte"
        );
    }

    #[test]
    fn n2_wrong_recipient_cannot_open() {
        let bob = Identity::generate();
        let carol = Identity::generate();
        let sealed = seal(&bob.public(), b"secret");
        assert!(
            carol.open(&sealed).is_err(),
            "a non-recipient must not open the box"
        );
    }

    #[test]
    fn n3_tampered_ciphertext_rejected() {
        let bob = Identity::generate();
        let mut sealed = seal(&bob.public(), b"secret payload");
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(
            bob.open(&sealed).is_err(),
            "AES-GCM tag must reject tampered ciphertext"
        );
    }

    #[test]
    fn n4_wrong_expected_issuer_rejected() {
        let alice = Identity::generate();
        let mallory = Identity::generate();
        let bob = Identity::generate();
        let cap = sample_cap(&bob.public());
        let token = seal_capability(&alice, &bob.public(), &cap).expect("seal");
        // token is genuinely from alice; opening while EXPECTING mallory must be rejected.
        assert!(
            open_capability(&bob, &mallory.public(), &token).is_err(),
            "issuer mismatch must be rejected even for an otherwise valid token"
        );
    }

    #[test]
    fn n5_tampered_signature_or_body_rejected() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let cap = sample_cap(&bob.public());
        let token = seal_capability(&alice, &bob.public(), &cap).expect("seal");

        // decode, flip a byte inside the signature, re-encode
        let raw = token.strip_prefix(DRIVE_SCHEME).unwrap();
        let mut bytes = unb64(raw).unwrap();
        bytes[40] ^= 0x01; // within the 32..96 signature region
        let tampered = format!("{DRIVE_SCHEME}{}", b64(&bytes));
        assert!(
            open_capability(&bob, &alice.public(), &tampered).is_err(),
            "tampered signature must be rejected"
        );

        // flip a byte inside the sealed body region
        let mut bytes2 = unb64(raw).unwrap();
        let last = bytes2.len() - 1;
        bytes2[last] ^= 0x01;
        let tampered2 = format!("{DRIVE_SCHEME}{}", b64(&bytes2));
        assert!(
            open_capability(&bob, &alice.public(), &tampered2).is_err(),
            "tampered sealed body must be rejected (signature or GCM)"
        );
    }
}
