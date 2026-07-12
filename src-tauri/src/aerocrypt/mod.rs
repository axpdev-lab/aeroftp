//! Shared AeroCrypt codec primitives.
//!
//! The audited crypto core extracted (P0, pure refactor) from `aerovault_v3.rs`
//! so the AeroVault container and the native AeroCrypt overlay can share one
//! implementation instead of maintaining two parallel cipher stacks
//! (#276 "one codec, one audit pass"). This module owns only the
//! format-agnostic primitives; the per-format key schedule (HKDF labels, AAD
//! domains, header layout) stays with each consumer.
//!
//! - Content AEAD: AES-256-GCM-SIV (RFC 8452, nonce-misuse resistant).
//! - Key wrap: AES-256-KW (RFC 3394).
//! - KDF: Argon2id (RFC 9106) at the audited AeroVault profile.
//! - Subkey derivation: HKDF-SHA256.
//! - CSPRNG helper over `OsRng`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm_siv::aead::{Aead, Payload};
use aes_gcm_siv::{Aes256GcmSiv, KeyInit, Nonce};
use aes_kw::Kek;
#[cfg(not(feature = "test-vectors"))]
use rand::RngCore;
use zeroize::Zeroizing;

pub mod emergency_kit;
pub mod names;
pub mod overlay;

/// AES-256 key length, in bytes.
pub const KEY_SIZE: usize = 32;
/// Argon2id salt length, in bytes.
pub const SALT_SIZE: usize = 32;
/// AES-GCM-SIV nonce length, in bytes.
pub const NONCE_SIZE: usize = 12;
/// Length of an AES-KW-wrapped 256-bit key (key + 8-byte integrity check).
pub const WRAPPED_KEY_SIZE: usize = 40;

/// The audited AeroVault Argon2id profile: 128 MiB / t=4 / p=4
/// (RFC 9106, exceeds OWASP 2024). Tuned once, shared by every consumer.
const ARGON2_MEM_KIB: u32 = 128 * 1024;
const ARGON2_TIME: u32 = 4;
const ARGON2_LANES: u32 = 4;

/// Argon2id memory cost (KiB) of the shared profile. Exposed so consumers can
/// bind the KDF parameters into authenticated metadata (e.g. the overlay config
/// MAC) without re-hardcoding them.
pub fn argon2_mem_kib() -> u32 {
    ARGON2_MEM_KIB
}
/// Argon2id time cost (passes) of the shared profile.
pub fn argon2_time() -> u32 {
    ARGON2_TIME
}
/// Argon2id parallelism (lanes) of the shared profile.
pub fn argon2_lanes() -> u32 {
    ARGON2_LANES
}

/// Fill an `N`-byte array from the OS CSPRNG.
#[cfg(not(feature = "test-vectors"))]
pub fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut out);
    out
}

// --- Deterministic test-vector mode (feature `test-vectors`) -----------------
//
// Mirror of the `aerovault` crate's deterministic generator. The seed string
// and per-call fill rule MUST stay byte-identical to the crate's copy
// (aerovault/src/aerocrypt.rs) or the AEROVAULT3 cross-impl golden diverges.
// Never enabled in a production build.
#[cfg(feature = "test-vectors")]
const TEST_VECTOR_SEED: &[u8] = b"AEROVAULT3 test-vectors v1";

#[cfg(feature = "test-vectors")]
thread_local! {
    static TEST_VECTOR_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Rewind the deterministic test-vector stream to its start. Only present under
/// the `test-vectors` feature.
#[cfg(feature = "test-vectors")]
pub fn reset_test_vectors() {
    TEST_VECTOR_COUNTER.with(|c| c.set(0));
}

#[cfg(feature = "test-vectors")]
pub fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut off = 0usize;
    while off < N {
        let ctr = TEST_VECTOR_COUNTER.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        let mut input = TEST_VECTOR_SEED.to_vec();
        input.extend_from_slice(&ctr.to_le_bytes());
        let block = blake3::hash(&input);
        let take = core::cmp::min(32, N - off);
        out[off..off + take].copy_from_slice(&block.as_bytes()[..take]);
        off += take;
    }
    out
}

/// Derive the base key-encryption key from a password + salt via Argon2id.
pub fn derive_base_kek(password: &str, salt: &[u8; SALT_SIZE]) -> Result<[u8; KEY_SIZE], String> {
    let params = argon2::Params::new(ARGON2_MEM_KIB, ARGON2_TIME, ARGON2_LANES, Some(KEY_SIZE))
        .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_SIZE];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {e}"))?;
    Ok(key)
}

// --- Optional keyfile second factor (AeroCrypt Tier 1, scheme `aecr-t1-combined-v1`) ---
//
// A keyfile is an OPTIONAL "something you have" factor mixed into the same
// Argon2id call as the password. FROZEN decisions (a keyfile vault's key
// material depends on all of them; changing any one breaks every keyfile vault):
//   F1: the keyfile digest uses BLAKE3 in KDF (`derive_key`) mode with a fixed
//       context, NEVER a bare `blake3::hash`, so it is domain-separated from
//       every checksum use of BLAKE3 anywhere in the app.
//   F2: keyfile bytes are canonicalized. A structured `AEROFTP-KEYFILE-V1` file
//       is decoded (immune to CRLF/BOM/trailing-newline mangling by FTP ASCII
//       mode, editors, or email); any other file is hashed verbatim.
//   F4: the KDF secret is `keyfile_digest || password_bytes` (digest first,
//       fixed-width, so the concatenation parses unambiguously).

/// Domain-separation context for keyfile digests. FROZEN.
const KEYFILE_DERIVE_CONTEXT: &str = "aeroftp.app/aerocrypt keyfile v1";
/// Header line of the structured, transfer-safe keyfile format.
pub const KEYFILE_HEADER: &str = "AEROFTP-KEYFILE-V1";

/// Digest an already-canonicalized keyfile payload via BLAKE3's KDF mode (F1).
/// Never a bare hash: this value is key material and must be disjoint from any
/// checksum use of BLAKE3.
pub fn keyfile_digest(payload: &[u8]) -> [u8; KEY_SIZE] {
    blake3::derive_key(KEYFILE_DERIVE_CONTEXT, payload)
}

/// Canonicalize raw keyfile bytes (F2). A well-formed `AEROFTP-KEYFILE-V1` file
/// yields its decoded 32-byte payload (transfer-safe); anything else yields the
/// raw bytes verbatim. A file that LOOKS structured but is malformed is a hard
/// error (never a silent raw fallback that would brick the vault).
fn canonicalize_keyfile(raw: &[u8]) -> Result<Vec<u8>, String> {
    // Strip a leading UTF-8 BOM if present.
    let body = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
    if let Ok(text) = std::str::from_utf8(body) {
        let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();
        let mut non_empty = lines.iter().filter(|l| !l.is_empty());
        if let Some(first) = non_empty.next() {
            if *first == KEYFILE_HEADER {
                let hex_str: String = non_empty
                    .flat_map(|l| l.chars())
                    .filter(|c| !c.is_whitespace())
                    .collect();
                let decoded = hex::decode(&hex_str).map_err(|_| {
                    "malformed AEROFTP-KEYFILE-V1 keyfile (invalid hex)".to_string()
                })?;
                if decoded.len() != KEY_SIZE {
                    return Err(format!(
                        "malformed AEROFTP-KEYFILE-V1 keyfile (expected {KEY_SIZE} bytes, got {})",
                        decoded.len()
                    ));
                }
                return Ok(decoded);
            }
        }
    }
    Ok(body.to_vec())
}

/// Canonicalize then digest raw keyfile file bytes (F1 + F2). Use this at the
/// I/O boundary where a keyfile has just been read from disk.
pub fn keyfile_digest_from_file(raw: &[u8]) -> Result<[u8; KEY_SIZE], String> {
    let payload = Zeroizing::new(canonicalize_keyfile(raw)?);
    Ok(keyfile_digest(&payload))
}

/// Generate the content of a fresh structured keyfile (`AEROFTP-KEYFILE-V1`):
/// a header line plus the hex of 32 random bytes. Printable and transfer-safe.
pub fn generate_keyfile_v1() -> String {
    let raw = random_array::<KEY_SIZE>();
    format!("{}\n{}\n", KEYFILE_HEADER, hex::encode(raw))
}

/// Create a brand-new keyfile at `path`, private from the first byte and never
/// clobbering anything already there.
///
/// A keyfile is a key factor whose 32 bytes are secret: overwriting one makes
/// its vaults unopenable, and even a brief world-readable window leaks the key.
/// We open with `create_new(true)` so creation is exclusive: it fails if the
/// path exists and, crucially, never follows a symlink pre-planted at the target
/// to clobber a victim file. That closes the TOCTOU gap an `exists()` pre-check
/// leaves open. On unix `.mode(0o600)` sets owner-only permissions at open time,
/// so the file is never born world-readable under the process umask (0644 under
/// umask 022) and no follow-up chmod (which could also fail silently) is needed.
/// Every byte is written and `sync_all()`d before we report success.
pub fn write_keyfile_new(path: &std::path::Path, content: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "keyfile '{}' already exists; refusing to overwrite",
                path.display()
            )
        } else {
            format!("cannot write keyfile '{}': {e}", path.display())
        }
    })?;
    f.write_all(content)
        .map_err(|e| format!("cannot write keyfile '{}': {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("cannot flush keyfile '{}': {e}", path.display()))?;
    Ok(())
}

/// Derive the base KEK from a password and an OPTIONAL keyfile digest via a
/// single Argon2id (F4: `keyfile_digest || password_bytes`). With `None` this is
/// byte-identical to [`derive_base_kek`], so existing password-only vaults are
/// unaffected. The combined single-Argon2id form (not password-then-HKDF) forces
/// a full memory-hard pass per keyfile candidate, so an attacker who knows the
/// password still cannot enumerate low-entropy keyfiles cheaply.
pub fn derive_base_kek_with_keyfile(
    password: &str,
    keyfile_digest: Option<&[u8; KEY_SIZE]>,
    salt: &[u8; SALT_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let Some(kf) = keyfile_digest else {
        return derive_base_kek(password, salt);
    };
    let mut secret = Zeroizing::new(Vec::with_capacity(KEY_SIZE + password.len()));
    secret.extend_from_slice(kf);
    secret.extend_from_slice(password.as_bytes());
    let params = argon2::Params::new(ARGON2_MEM_KIB, ARGON2_TIME, ARGON2_LANES, Some(KEY_SIZE))
        .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_SIZE];
    argon2
        .hash_password_into(&secret, salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {e}"))?;
    Ok(key)
}

/// HKDF-SHA256 expand of `ikm` under a domain-separating `label`.
pub fn hkdf_expand<const N: usize>(ikm: &[u8], label: &[u8]) -> Result<[u8; N], String> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, ikm);
    let mut out = [0u8; N];
    hk.expand(label, &mut out)
        .map_err(|_| "HKDF expand failed".to_string())?;
    Ok(out)
}

/// Wrap a 256-bit key under `kek` (AES-256-KW).
pub fn wrap_key(
    kek: &[u8; KEY_SIZE],
    key: &[u8; KEY_SIZE],
) -> Result<[u8; WRAPPED_KEY_SIZE], String> {
    let kek = Kek::from(*kek);
    let mut out = [0u8; WRAPPED_KEY_SIZE];
    kek.wrap(key, &mut out)
        .map_err(|_| "AES-KW wrap failed".to_string())?;
    Ok(out)
}

/// Unwrap a 256-bit key wrapped under `kek` (AES-256-KW).
pub fn unwrap_key(
    kek: &[u8; KEY_SIZE],
    wrapped: &[u8; WRAPPED_KEY_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let kek = Kek::from(*kek);
    let mut out = [0u8; KEY_SIZE];
    kek.unwrap(wrapped, &mut out)
        .map_err(|_| "AES-KW unwrap failed".to_string())?;
    Ok(out)
}

/// Encrypt `plaintext` with AES-256-GCM-SIV under `key`, binding `aad`.
/// The fresh random nonce is prefixed to the returned ciphertext.
pub fn encrypt_with_aad(
    key: &[u8; KEY_SIZE],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|e| format!("AES-GCM-SIV init: {e}"))?;
    let nonce_bytes = random_array::<NONCE_SIZE>();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| "AES-GCM-SIV encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a nonce-prefixed AES-256-GCM-SIV payload under `key`, binding `aad`.
pub fn decrypt_with_aad(
    key: &[u8; KEY_SIZE],
    encrypted: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    if encrypted.len() < NONCE_SIZE + 16 {
        return Err("AES-GCM-SIV payload is too short".to_string());
    }
    let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|e| format!("AES-GCM-SIV init: {e}"))?;
    let nonce = Nonce::from_slice(&encrypted[..NONCE_SIZE]);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &encrypted[NONCE_SIZE..],
                aad,
            },
        )
        .map_err(|_| "AES-GCM-SIV decrypt failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcm_siv_round_trip_binds_aad() {
        let key = [7u8; KEY_SIZE];
        let msg = b"AeroCrypt shared codec round trip";
        let ct = encrypt_with_aad(&key, msg, b"domain-a").unwrap();
        assert_eq!(decrypt_with_aad(&key, &ct, b"domain-a").unwrap(), msg);
        // Wrong AAD must fail closed.
        assert!(decrypt_with_aad(&key, &ct, b"domain-b").is_err());
        // Wrong key must fail closed.
        assert!(decrypt_with_aad(&[8u8; KEY_SIZE], &ct, b"domain-a").is_err());
    }

    #[test]
    fn gcm_siv_rejects_short_payload() {
        assert!(decrypt_with_aad(&[0u8; KEY_SIZE], &[0u8; 4], b"").is_err());
    }

    #[test]
    fn aes_kw_wrap_unwrap_round_trip() {
        let kek = [3u8; KEY_SIZE];
        let key = [9u8; KEY_SIZE];
        let wrapped = wrap_key(&kek, &key).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_KEY_SIZE);
        assert_eq!(unwrap_key(&kek, &wrapped).unwrap(), key);
        // A different KEK must not unwrap.
        assert!(unwrap_key(&[4u8; KEY_SIZE], &wrapped).is_err());
    }

    #[test]
    fn argon2_and_hkdf_are_deterministic() {
        let salt = [1u8; SALT_SIZE];
        let a = derive_base_kek("correct horse battery staple", &salt).unwrap();
        let b = derive_base_kek("correct horse battery staple", &salt).unwrap();
        assert_eq!(a, b);
        let c = derive_base_kek("different password", &salt).unwrap();
        assert_ne!(a, c);

        let k1 = hkdf_expand::<KEY_SIZE>(&a, b"label-1").unwrap();
        let k2 = hkdf_expand::<KEY_SIZE>(&a, b"label-1").unwrap();
        let k3 = hkdf_expand::<KEY_SIZE>(&a, b"label-2").unwrap();
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn keyfile_digest_is_domain_separated_from_bare_hash() {
        // F1: the keyfile digest must NOT equal a bare BLAKE3 checksum of the
        // same bytes, or any feature that logs/stores a file's BLAKE3 leaks key
        // material.
        let bytes = b"a file the user happened to pick as a keyfile";
        let digest = keyfile_digest(bytes);
        let bare = *blake3::hash(bytes).as_bytes();
        assert_ne!(digest, bare);
        // Deterministic.
        assert_eq!(keyfile_digest(bytes), digest);
    }

    #[test]
    fn keyfile_changes_the_derived_key() {
        let salt = [2u8; SALT_SIZE];
        let pw_only = derive_base_kek_with_keyfile("hunter2", None, &salt).unwrap();
        // None path is byte-identical to the password-only KDF (back-compat).
        assert_eq!(pw_only, derive_base_kek("hunter2", &salt).unwrap());

        let kf = keyfile_digest(b"secret-keyfile-payload");
        let with_kf = derive_base_kek_with_keyfile("hunter2", Some(&kf), &salt).unwrap();
        assert_ne!(with_kf, pw_only);

        // A different keyfile yields a different key even with the same password.
        let kf2 = keyfile_digest(b"other-keyfile-payload");
        let with_kf2 = derive_base_kek_with_keyfile("hunter2", Some(&kf2), &salt).unwrap();
        assert_ne!(with_kf, with_kf2);
    }

    #[test]
    fn digest_first_order_is_unambiguous() {
        // F4: digest-first means (password P, keyfile K) cannot collide with a
        // password-only vault whose password is P prefixed by the digest, since
        // the digest occupies the fixed-width leading field. Concretely, swapping
        // which factor leads must change the key.
        let salt = [5u8; SALT_SIZE];
        let kf = keyfile_digest(b"K");
        let normal = derive_base_kek_with_keyfile("P", Some(&kf), &salt).unwrap();
        // Password-only with the SAME concatenated bytes but password-first order
        // would differ; emulate a hypothetical password-first mix and confirm it
        // is not what we produce.
        let mut password_first = Zeroizing::new(Vec::new());
        password_first.extend_from_slice(b"P");
        password_first.extend_from_slice(&kf);
        let params =
            argon2::Params::new(ARGON2_MEM_KIB, ARGON2_TIME, ARGON2_LANES, Some(KEY_SIZE)).unwrap();
        let argon2 =
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let mut alt = [0u8; KEY_SIZE];
        argon2
            .hash_password_into(&password_first, &salt, &mut alt)
            .unwrap();
        assert_ne!(normal, alt);
    }

    #[test]
    fn structured_keyfile_survives_line_ending_mangling() {
        // F2: a generated keyfile must digest identically after CRLF, BOM, and
        // trailing-newline mangling (FTP ASCII mode / editors / email).
        let clean = generate_keyfile_v1();
        let d_clean = keyfile_digest_from_file(clean.as_bytes()).unwrap();

        // CRLF + extra trailing newline.
        let crlf = clean.replace('\n', "\r\n") + "\r\n";
        assert_eq!(keyfile_digest_from_file(crlf.as_bytes()).unwrap(), d_clean);

        // Leading UTF-8 BOM + leading/trailing whitespace on lines.
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(clean.replace('\n', "  \n\t").as_bytes());
        assert_eq!(keyfile_digest_from_file(&bom).unwrap(), d_clean);
    }

    #[test]
    fn raw_keyfile_is_hashed_verbatim() {
        // A non-structured file is hashed as-is; changing one byte changes the
        // digest (so binary keyfiles work, with the documented fragility caveat).
        let raw = [0x42u8; 64];
        let d = keyfile_digest_from_file(&raw).unwrap();
        assert_eq!(d, keyfile_digest(&raw));
        let mut raw2 = raw;
        raw2[0] = 0x43;
        assert_ne!(keyfile_digest_from_file(&raw2).unwrap(), d);
    }

    #[test]
    fn malformed_structured_keyfile_is_rejected() {
        // Looks structured (header present) but the payload is not valid: hard
        // error, never a silent raw fallback that would produce a wrong key.
        let bad_hex = format!("{KEYFILE_HEADER}\nnot-hex-at-all\n");
        assert!(keyfile_digest_from_file(bad_hex.as_bytes()).is_err());
        let wrong_len = format!("{KEYFILE_HEADER}\n{}\n", "ab".repeat(8)); // 8 bytes, not 32
        assert!(keyfile_digest_from_file(wrong_len.as_bytes()).is_err());
    }

    /// Build a unique path under the OS temp dir (no external uuid dependency):
    /// pid + a nanosecond stamp + a process-local counter keep parallel tests
    /// from colliding.
    fn unique_temp_path(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "aerocrypt-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ))
    }

    #[test]
    fn write_keyfile_new_creates_private_file() {
        let path = unique_temp_path("kf-create");
        let content = generate_keyfile_v1();
        write_keyfile_new(&path, content.as_bytes()).unwrap();
        // Content round-trips verbatim.
        assert_eq!(std::fs::read(&path).unwrap(), content.as_bytes());
        // Born owner-only (0600), never world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_keyfile_new_refuses_to_overwrite() {
        let path = unique_temp_path("kf-nooverwrite");
        let original = b"AEROFTP-KEYFILE-V1 original-material\n";
        write_keyfile_new(&path, original).unwrap();
        // A second call must error and leave the original intact.
        let err = write_keyfile_new(&path, b"attacker replacement").unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn write_keyfile_new_does_not_follow_symlink() {
        // A symlink pre-planted at the target must not be followed: exclusive
        // create fails (EEXIST) and the victim it points at is untouched, so the
        // TOCTOU symlink-swap that an exists()+write pattern is vulnerable to
        // cannot clobber another file.
        let victim = unique_temp_path("kf-victim");
        let victim_content = b"precious victim bytes";
        std::fs::write(&victim, victim_content).unwrap();
        let link = unique_temp_path("kf-symlink");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let err = write_keyfile_new(&link, b"malicious keyfile").unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read(&victim).unwrap(), victim_content);
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&victim);
    }
}
