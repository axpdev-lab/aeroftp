// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroFTP Security Toolkit: Hash Forge, CryptoLab, Password Forge
// Production-grade cryptographic utilities for file integrity, encryption, and credential generation
//
// All crypto operations use audited crates (RustCrypto ecosystem).
// Sensitive data is zeroized on drop via secrecy crate.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::{seq::SliceRandom, Rng};
use sha2::Digest;
// A2-02: Zeroize import removed: derive_key now returns Zeroizing<> wrapper

use crate::filesystem::validate_path;

// ─── Hash Forge ─────────────────────────────────────────────────────────────

/// BLAKE3 XOF output length bounds (bytes). Default classic digest is 32.
const BLAKE3_OUTPUT_LEN_MIN: usize = 1;
const BLAKE3_OUTPUT_LEN_MAX: usize = 1024;
const BLAKE3_DEFAULT_LEN: usize = 32;

/// Hash arbitrary text with the specified algorithm.
/// Returns lowercase hex-encoded hash.
///
/// - `encoding`: optional input encoding — `utf-8` (default), `base64`, `hex`,
///   or `binary` (plaintext 0/1 bits, whitespace tolerated). Decoded on the
///   Rust side so the UI stays thin and decode errors are testable.
/// - `output_len`: optional BLAKE3 XOF length in bytes (1..=1024). Ignored for
///   non-BLAKE3 algorithms. `None` keeps the classic 32-byte BLAKE3 digest.
/// - Empty input is valid (empty-string / empty-decoded bytes are hashed).
#[tauri::command]
pub fn hash_text(
    text: String,
    algorithm: String,
    output_len: Option<usize>,
    encoding: Option<String>,
) -> Result<String, String> {
    let enc = encoding.as_deref().unwrap_or("utf-8");
    let bytes = decode_text_input(&text, enc)?;
    hash_bytes(&bytes, &algorithm, output_len)
}

/// Hash a local file with the specified algorithm (64KB streaming buffer).
///
/// `output_len` is the optional BLAKE3 XOF length (same rules as `hash_text`).
#[tauri::command]
pub async fn hash_file(
    path: String,
    algorithm: String,
    output_len: Option<usize>,
) -> Result<String, String> {
    validate_path(&path)?;
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("Failed to open file: {}", e))?;

    let mut buffer = vec![0u8; 64 * 1024];

    match algorithm.to_lowercase().as_str() {
        "md5" => {
            let mut hasher = md5::Md5::new();
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Read error: {}", e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha1" => {
            let mut hasher = sha1::Sha1::new();
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Read error: {}", e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha256" => {
            let mut hasher = sha2::Sha256::new();
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Read error: {}", e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha512" => {
            let mut hasher = sha2::Sha512::new();
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Read error: {}", e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "blake3" => {
            let mut hasher = blake3::Hasher::new();
            loop {
                let n = file
                    .read(&mut buffer)
                    .await
                    .map_err(|e| format!("Read error: {}", e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            blake3_finalize_hex(&hasher, output_len)
        }
        _ => Err(format!("Unsupported algorithm: {}", algorithm)),
    }
}

/// Stage a browser-dropped file (HTML5 `File` blob) to a temp path so
/// `hash_file` can stream it. Used when the webview has
/// `disable_drag_drop_handler` (Windows HTML5 requirement) and the
/// DataTransfer does not expose an absolute OS path — common on WebKitGTK,
/// where `File.path` is absent and `text/uri-list` is often empty even
/// though `files[0]` is present with the real contents.
///
/// `data_base64` is the raw file bytes as standard base64 (no data-URL
/// prefix). Cap is 256 MiB to keep IPC + temp write bounded.
#[tauri::command]
pub async fn stage_hash_drop(name: String, data_base64: String) -> Result<String, String> {
    const MAX_STAGE_BYTES: usize = 256 * 1024 * 1024;

    let data = B64
        .decode(data_base64.as_bytes())
        .map_err(|e| format!("Invalid base64 drop payload: {e}"))?;
    if data.len() > MAX_STAGE_BYTES {
        return Err(format!(
            "Dropped file is too large to stage ({} bytes, max {})",
            data.len(),
            MAX_STAGE_BYTES
        ));
    }

    let dir = std::env::temp_dir().join("aeroftp-hash-drops");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create drop stage dir: {e}"))?;
    // The staged copy is the file's plaintext: keep the directory owner-only.
    // create_dir_all leaves 0777&!umask (0755 on a default umask), which would
    // let every local account read a dropped key or vault file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await;
    }
    // Sweep stale drops from earlier sessions: a staged file is consumed by the
    // very next hash_file call, so anything older than an hour is a leftover.
    sweep_stale_hash_drops(&dir).await;

    let safe: String = std::path::Path::new(&name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("drop.bin")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "drop.bin".to_string()
    } else {
        safe
    };

    let path = dir.join(format!("{}_{}", uuid::Uuid::new_v4(), safe));
    tokio::fs::write(&path, &data)
        .await
        .map_err(|e| format!("Failed to stage dropped file: {e}"))?;
    // Same reason as the directory: 0666&!umask (0644) would publish the
    // plaintext of whatever the user dropped to every local account.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await
        {
            // Cannot restrict it: do not leave a world-readable copy behind.
            let _ = tokio::fs::remove_file(&path).await;
            return Err(format!("Failed to secure staged file: {e}"));
        }
    }

    log::info!(
        "[HASHDROP] staged {} bytes as {}",
        data.len(),
        path.display()
    );
    Ok(path.to_string_lossy().into_owned())
}

/// Remove staged drops left by earlier runs.
///
/// A staged file exists only to be handed straight back to `hash_file`, so it
/// has no reason to outlive the operation. Nothing used to delete them and they
/// accumulated in the shared temp directory as plaintext copies of whatever the
/// user had hashed.
async fn sweep_stale_hash_drops(dir: &std::path::Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    let now = std::time::SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let stale = match entry.metadata().await {
            Ok(m) if m.is_file() => m
                .modified()
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|age| age >= STALE_AFTER)
                .unwrap_or(false),
            _ => false,
        };
        if stale {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// Delete a file staged by `stage_hash_drop`.
///
/// The frontend calls this once the hash has been computed (or when the drop is
/// abandoned) so the plaintext copy does not linger in the temp directory.
/// Restricted to our own staging directory so it can never be turned into an
/// arbitrary-delete primitive.
#[tauri::command]
pub async fn discard_hash_drop(path: String) -> Result<(), String> {
    let dir = std::env::temp_dir().join("aeroftp-hash-drops");
    let target = std::path::PathBuf::from(&path);
    if target.parent() != Some(dir.as_path()) {
        return Err("Not a staged drop path".to_string());
    }
    match tokio::fs::remove_file(&target).await {
        Ok(()) => Ok(()),
        // Already gone (swept, or never created): nothing to do.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to discard staged file: {e}")),
    }
}

/// Constant-time hash comparison to prevent timing attacks.
#[tauri::command]
pub fn compare_hashes(hash_a: String, hash_b: String) -> bool {
    let a = hash_a.trim().to_lowercase();
    let b = hash_b.trim().to_lowercase();
    if a.len() != b.len() {
        return false;
    }
    // XOR accumulator for constant-time comparison
    let mut diff: u8 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Decode Hash Forge text input according to `encoding`.
///
/// - `utf-8` / `utf8` (default): raw UTF-8 bytes of the string
/// - `base64`: standard Base64
/// - `hex`: hex digits, whitespace stripped
/// - `binary`: plaintext `0`/`1` bits (whitespace tolerated); length must be a
///   multiple of 8 (empty → empty bytes)
fn decode_text_input(text: &str, encoding: &str) -> Result<Vec<u8>, String> {
    match encoding.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => Ok(text.as_bytes().to_vec()),
        "base64" => B64
            .decode(text.trim().as_bytes())
            .map_err(|e| format!("Invalid Base64 input: {e}")),
        "hex" => {
            let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if cleaned.is_empty() {
                return Ok(Vec::new());
            }
            hex::decode(&cleaned).map_err(|e| format!("Invalid Hex input: {e}"))
        }
        "binary" => {
            let bits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if bits.is_empty() {
                return Ok(Vec::new());
            }
            if !bits.chars().all(|c| c == '0' || c == '1') {
                return Err("Invalid Binary input: only 0 and 1 (and whitespace) allowed".into());
            }
            if bits.len() % 8 != 0 {
                return Err("Invalid Binary input: bit length must be a multiple of 8".into());
            }
            let mut bytes = Vec::with_capacity(bits.len() / 8);
            for chunk in bits.as_bytes().chunks(8) {
                let mut b = 0u8;
                for &bit in chunk {
                    b = (b << 1) | u8::from(bit == b'1');
                }
                bytes.push(b);
            }
            Ok(bytes)
        }
        other => Err(format!("Unsupported encoding: {other}")),
    }
}

fn validate_blake3_output_len(output_len: usize) -> Result<(), String> {
    if !(BLAKE3_OUTPUT_LEN_MIN..=BLAKE3_OUTPUT_LEN_MAX).contains(&output_len) {
        return Err(format!(
            "BLAKE3 output length must be between {BLAKE3_OUTPUT_LEN_MIN} and {BLAKE3_OUTPUT_LEN_MAX} bytes"
        ));
    }
    Ok(())
}

/// Finalize a BLAKE3 hasher to lowercase hex. `None` (or classic 32 via
/// `finalize`) keeps legacy behavior; other lengths use XOF.
fn blake3_finalize_hex(
    hasher: &blake3::Hasher,
    output_len: Option<usize>,
) -> Result<String, String> {
    match output_len {
        None => Ok(hasher.finalize().to_hex().to_string()),
        Some(n) => {
            validate_blake3_output_len(n)?;
            if n == BLAKE3_DEFAULT_LEN {
                // Classic 32-byte digest is byte-identical to XOF[:32], but
                // keep the historic finalize path for None-equivalent callers.
                return Ok(hasher.finalize().to_hex().to_string());
            }
            let mut out = vec![0u8; n];
            hasher.finalize_xof().fill(&mut out);
            Ok(hex::encode(out))
        }
    }
}

/// Hash in-memory bytes (shared helper).
fn hash_bytes(data: &[u8], algorithm: &str, output_len: Option<usize>) -> Result<String, String> {
    match algorithm.to_lowercase().as_str() {
        "md5" => {
            let mut h = md5::Md5::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "sha1" => {
            let mut h = sha1::Sha1::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "sha256" => {
            let mut h = sha2::Sha256::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "sha512" => {
            let mut h = sha2::Sha512::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "blake3" => {
            let mut hasher = blake3::Hasher::new();
            hasher.update(data);
            blake3_finalize_hex(&hasher, output_len)
        }
        _ => Err(format!("Unsupported algorithm: {}", algorithm)),
    }
}

// ─── CryptoLab ──────────────────────────────────────────────────────────────

/// Encrypt text with password. Returns portable format: $ALGO$salt$nonce$ciphertext (all base64).
/// Key derivation: Argon2id (64 MB / t=3 / p=4) via crypto::derive_key().
#[tauri::command]
pub async fn crypto_encrypt_text(
    plaintext: String,
    password: String,
    algorithm: String,
) -> Result<String, String> {
    if plaintext.is_empty() {
        return Err("Input text is empty".into());
    }
    if password.is_empty() {
        return Err("Password is required".into());
    }

    let algo = algorithm.to_lowercase();
    if algo != "aes-256-gcm" && algo != "chacha20-poly1305" {
        return Err(format!(
            "Unsupported algorithm: {}. Use aes-256-gcm or chacha20-poly1305",
            algorithm
        ));
    }

    // Generate random salt (32 bytes) and nonce (12 bytes)
    let salt = crate::crypto::random_bytes(32);
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill(&mut nonce_bytes);

    // Derive 256-bit key from password via Argon2id (A2-02: auto-zeroized via Zeroizing wrapper)
    let key = crate::crypto::derive_key(&password, &salt)?;

    // Encrypt
    let ciphertext = match algo.as_str() {
        "aes-256-gcm" => crate::crypto::encrypt_aes_gcm(&key, &nonce_bytes, plaintext.as_bytes())?,
        "chacha20-poly1305" => encrypt_chacha20(*key, &nonce_bytes, plaintext.as_bytes())?,
        _ => unreachable!(),
    };
    // key auto-zeroized on drop via Zeroizing wrapper

    // Format: $algo$base64(salt)$base64(nonce)$base64(ciphertext)
    Ok(format!(
        "${}${}${}${}",
        algo,
        B64.encode(&salt),
        B64.encode(nonce_bytes),
        B64.encode(&ciphertext)
    ))
}

/// Decrypt text from portable format. Parses algorithm from the encoded string.
#[tauri::command]
pub async fn crypto_decrypt_text(encoded: String, password: String) -> Result<String, String> {
    if password.is_empty() {
        return Err("Password is required".into());
    }

    // Parse format: $algo$salt$nonce$ciphertext
    let parts: Vec<&str> = encoded.split('$').collect();
    if parts.len() != 5 || !parts[0].is_empty() {
        return Err("Invalid format. Expected: $algo$salt$nonce$ciphertext".into());
    }

    let algo = parts[1];
    let salt = B64.decode(parts[2]).map_err(|_| "Invalid base64 salt")?;
    let nonce_bytes = B64.decode(parts[3]).map_err(|_| "Invalid base64 nonce")?;
    let ciphertext = B64
        .decode(parts[4])
        .map_err(|_| "Invalid base64 ciphertext")?;

    if salt.len() != 32 {
        return Err("Invalid salt length".into());
    }
    if nonce_bytes.len() != 12 {
        return Err("Invalid nonce length".into());
    }

    // Derive key (A2-02: auto-zeroized via Zeroizing wrapper)
    let key = crate::crypto::derive_key(&password, &salt)?;

    // Decrypt
    let plaintext = match algo {
        "aes-256-gcm" => crate::crypto::decrypt_aes_gcm(&key, &nonce_bytes, &ciphertext)?,
        "chacha20-poly1305" => decrypt_chacha20(*key, &nonce_bytes, &ciphertext)?,
        _ => {
            return Err(format!("Unknown algorithm: {}", algo));
        }
    };
    // key auto-zeroized on drop via Zeroizing wrapper

    String::from_utf8(plaintext).map_err(|_| "Decrypted data is not valid UTF-8".into())
}

/// ChaCha20-Poly1305 encryption helper
fn encrypt_chacha20(key: [u8; 32], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::generic_array::GenericArray;
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&key));
    let nonce = GenericArray::from_slice(nonce);
    cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("ChaCha20-Poly1305 encrypt failed: {}", e))
}

/// ChaCha20-Poly1305 decryption helper
fn decrypt_chacha20(key: [u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::generic_array::GenericArray;
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&key));
    let nonce = GenericArray::from_slice(nonce);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| "Decryption failed: wrong password or corrupted data".into())
}

// ─── Password Forge ─────────────────────────────────────────────────────────

const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const UPPER_FULL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghjkmnpqrstuvwxyz";
const LOWER_FULL: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"23456789";
const DIGITS_FULL: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"!@#$%^&*()-_=+[]{}|;:,.<>?/~";
const SYMBOL_PUNCTUATION: &[u8] = b"!@#$%^&*";
const SYMBOL_BRACKETS: &[u8] = b"()[]{}<>";
const SYMBOL_SEPARATORS: &[u8] = b"-_=+";
const SYMBOL_SPECIAL: &[u8] = b"|;:,.?/~";

fn symbol_group(name: &str) -> Option<&'static [u8]> {
    match name {
        "punctuation" => Some(SYMBOL_PUNCTUATION),
        "brackets" => Some(SYMBOL_BRACKETS),
        "separators" => Some(SYMBOL_SEPARATORS),
        "special" => Some(SYMBOL_SPECIAL),
        _ => None,
    }
}

struct PasswordGroupOptions<'a> {
    uppercase: bool,
    lowercase: bool,
    digits: bool,
    symbols: bool,
    exclude_ambiguous: bool,
    symbol_groups: Option<&'a [String]>,
    custom_characters: Option<&'a str>,
    excluded_characters: Option<&'a str>,
}

fn build_password_groups(options: PasswordGroupOptions<'_>) -> Result<Vec<Vec<u8>>, String> {
    let excluded = options.excluded_characters.unwrap_or_default().as_bytes();
    let filter_group = |source: &[u8]| {
        let mut group = Vec::new();
        for byte in source {
            if !excluded.contains(byte) && !group.contains(byte) {
                group.push(*byte);
            }
        }
        group
    };

    let mut groups = Vec::new();
    let mut push_selected = |selected: bool, source: &[u8], label: &str| -> Result<(), String> {
        if selected {
            let group = filter_group(source);
            if group.is_empty() {
                return Err(format!(
                    "The {label} character group is empty after exclusions"
                ));
            }
            groups.push(group);
        }
        Ok(())
    };

    push_selected(
        options.uppercase,
        if options.exclude_ambiguous {
            UPPER
        } else {
            UPPER_FULL
        },
        "uppercase",
    )?;
    push_selected(
        options.lowercase,
        if options.exclude_ambiguous {
            LOWER
        } else {
            LOWER_FULL
        },
        "lowercase",
    )?;
    push_selected(
        options.digits,
        if options.exclude_ambiguous {
            DIGITS
        } else {
            DIGITS_FULL
        },
        "digits",
    )?;

    if options.symbols {
        match options.symbol_groups {
            Some(names) => {
                if names.is_empty() {
                    return Err("Select at least one symbol group".into());
                }
                for name in names {
                    let source = symbol_group(name)
                        .ok_or_else(|| format!("Unknown symbol group: {name}"))?;
                    let group = filter_group(source);
                    if group.is_empty() {
                        return Err(format!("The {name} symbol group is empty after exclusions"));
                    }
                    groups.push(group);
                }
            }
            None => push_selected(true, SYMBOLS, "symbols")?,
        }
    }

    if let Some(custom) = options.custom_characters.filter(|value| !value.is_empty()) {
        if !custom.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("Custom characters must be printable ASCII without spaces".into());
        }
        let group = filter_group(custom.as_bytes());
        if group.is_empty() {
            return Err("The custom character group is empty after exclusions".into());
        }
        groups.push(group);
    }

    if groups.is_empty() {
        return Err("Select at least one character set".into());
    }
    Ok(groups)
}

/// Generate cryptographically secure random passwords.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn generate_password(
    length: usize,
    uppercase: bool,
    lowercase: bool,
    digits: bool,
    symbols: bool,
    exclude_ambiguous: bool,
    count: usize,
    symbol_groups: Option<Vec<String>>,
    custom_characters: Option<String>,
    excluded_characters: Option<String>,
    require_each_group: Option<bool>,
) -> Result<Vec<String>, String> {
    if !(8..=128).contains(&length) {
        return Err("Length must be between 8 and 128".into());
    }
    let count = count.clamp(1, 10);

    let groups = build_password_groups(PasswordGroupOptions {
        uppercase,
        lowercase,
        digits,
        symbols,
        exclude_ambiguous,
        symbol_groups: symbol_groups.as_deref(),
        custom_characters: custom_characters.as_deref(),
        excluded_characters: excluded_characters.as_deref(),
    })?;
    if require_each_group.unwrap_or(false) && length < groups.len() {
        return Err("Password length is smaller than the number of required groups".into());
    }
    let mut pool = Vec::new();
    for group in &groups {
        for byte in group {
            if !pool.contains(byte) {
                pool.push(*byte);
            }
        }
    }

    let mut rng = rand::thread_rng();
    let mut results = Vec::with_capacity(count);

    for _ in 0..count {
        let mut password = Vec::with_capacity(length);
        if require_each_group.unwrap_or(false) {
            for group in &groups {
                password.push(group[rng.gen_range(0..group.len())]);
            }
        }
        while password.len() < length {
            password.push(pool[rng.gen_range(0..pool.len())]);
        }
        password.shuffle(&mut rng);
        results.push(String::from_utf8(password).expect("password groups are ASCII"));
    }

    Ok(results)
}

/// Generate random passphrases from AeroFTP's 1133-word EFF-derived short list.
#[tauri::command]
pub fn generate_passphrase(
    word_count: usize,
    separator: String,
    capitalize: bool,
    count: usize,
) -> Result<Vec<String>, String> {
    if !(3..=24).contains(&word_count) {
        return Err("Word count must be between 3 and 24".into());
    }
    let count = count.clamp(1, 10);
    let sep = if separator.is_empty() {
        "-"
    } else {
        &separator
    };

    let mut rng = rand::thread_rng();
    let mut results = Vec::with_capacity(count);

    for _ in 0..count {
        let words: Vec<String> = (0..word_count)
            .map(|_| {
                let word = WORDLIST[rng.gen_range(0..WORDLIST.len())];
                if capitalize {
                    let mut c = word.chars();
                    match c.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().to_string() + c.as_str(),
                    }
                } else {
                    word.to_string()
                }
            })
            .collect();
        results.push(words.join(sep));
    }

    Ok(results)
}

/// Calculate password entropy in bits.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn calculate_entropy(
    length: usize,
    uppercase: bool,
    lowercase: bool,
    digits: bool,
    symbols: bool,
    exclude_ambiguous: bool,
    symbol_groups: Option<Vec<String>>,
    custom_characters: Option<String>,
    excluded_characters: Option<String>,
) -> f64 {
    let Ok(groups) = build_password_groups(PasswordGroupOptions {
        uppercase,
        lowercase,
        digits,
        symbols,
        exclude_ambiguous,
        symbol_groups: symbol_groups.as_deref(),
        custom_characters: custom_characters.as_deref(),
        excluded_characters: excluded_characters.as_deref(),
    }) else {
        return 0.0;
    };
    let mut pool = Vec::new();
    for group in groups {
        for byte in group {
            if !pool.contains(&byte) {
                pool.push(byte);
            }
        }
    }
    let pool_size = pool.len();
    if pool_size == 0 {
        return 0.0;
    }
    length as f64 * (pool_size as f64).log2()
}

// ─── EFF-Derived Short Wordlist (1133 words) ───────────────────────────────
// Compact subset derived from the Electronic Frontier Foundation lists.

const WORDLIST: &[&str] = &[
    "acid", "acme", "acre", "acts", "aged", "agent", "agile", "aging", "agony", "agree", "ahead",
    "aide", "aim", "ajar", "alarm", "album", "alert", "alike", "alive", "alley", "allot", "allow",
    "alone", "alpha", "amaze", "ample", "amuse", "angel", "anger", "angle", "ankle", "annex",
    "anvil", "apart", "apex", "apple", "apply", "arena", "argon", "arise", "armor", "army",
    "aroma", "array", "arrow", "arson", "ash", "asset", "atlas", "atom", "attic", "audio", "audit",
    "avert", "avid", "avoid", "awake", "award", "azure", "bacon", "badge", "badly", "bagel",
    "baker", "balmy", "banjo", "barge", "baron", "basic", "basin", "batch", "bath", "beach",
    "beard", "beast", "begin", "being", "belly", "below", "bench", "berry", "bible", "bike",
    "bind", "birch", "birth", "blade", "blame", "blank", "blast", "blaze", "bleak", "blend",
    "bless", "blimp", "blind", "bliss", "block", "bloom", "blown", "bluff", "blunt", "blurt",
    "blush", "board", "boat", "bogus", "bolt", "bonus", "boost", "booth", "born", "bound", "bowed",
    "boxer", "brain", "brand", "brave", "bread", "break", "breed", "brick", "bride", "brief",
    "bring", "brink", "brisk", "broad", "broil", "brook", "broth", "brown", "brush", "buddy",
    "budge", "build", "bulge", "bunch", "bunny", "burst", "buyer", "cabin", "cache", "camel",
    "candy", "cargo", "carry", "carve", "catch", "cause", "cedar", "chain", "chair", "chalk",
    "champ", "chaos", "charm", "chart", "chase", "cheap", "check", "cheek", "cheer", "chess",
    "chest", "chief", "child", "chill", "china", "chip", "choir", "chord", "chunk", "cinch",
    "claim", "clamp", "clash", "clasp", "class", "clay", "clean", "clear", "clerk", "click",
    "cliff", "climb", "cling", "clip", "cloak", "clock", "clone", "close", "cloth", "cloud",
    "clown", "club", "clue", "coach", "coast", "cobra", "cocoa", "coil", "color", "comet", "comic",
    "comma", "cone", "coral", "core", "couch", "count", "court", "cover", "crack", "craft",
    "crane", "crash", "crate", "crawl", "crazy", "cream", "creek", "creep", "crest", "crisp",
    "cross", "crowd", "crown", "crush", "crust", "cubic", "curve", "cycle", "daily", "dance",
    "darts", "datum", "dawn", "dealt", "debug", "decay", "decor", "decoy", "delay", "delta",
    "delve", "demon", "dense", "depot", "depth", "derby", "desk", "detox", "deter", "digit",
    "dimly", "diner", "dirty", "disco", "dish", "ditch", "diver", "dizzy", "dodge", "doing",
    "donor", "donut", "doubt", "dough", "draft", "drain", "drape", "drawn", "dream", "dress",
    "drift", "drill", "drink", "drive", "droit", "drone", "drool", "dusty", "dwarf", "dwell",
    "dying", "eager", "eagle", "early", "earth", "easel", "eaten", "eater", "ebony", "eclat",
    "edges", "edict", "eight", "elbow", "elder", "elect", "elfin", "elite", "ember", "empty",
    "enact", "enemy", "enjoy", "enter", "entry", "envoy", "epoch", "equal", "equip", "erode",
    "error", "essay", "ethic", "evade", "event", "every", "evict", "exact", "exalt", "exam",
    "exile", "exist", "extra", "fable", "facet", "faint", "fairy", "faith", "fancy", "fatal",
    "favor", "feast", "fence", "ferry", "fetch", "fever", "fewer", "fiber", "field", "fifth",
    "fifty", "fight", "finch", "first", "flame", "flank", "flare", "flash", "flask", "fleet",
    "flesh", "flick", "fling", "flint", "float", "flock", "flood", "floor", "flora", "flour",
    "flown", "fluid", "flush", "flute", "focal", "focus", "foggy", "folly", "force", "forge",
    "form", "forth", "forum", "fossil", "found", "fox", "foyer", "frail", "frame", "frank",
    "fraud", "fresh", "friar", "front", "frost", "froze", "fruit", "fuel", "fully", "fungi",
    "fury", "fused", "fuzzy", "gamer", "gamma", "gauge", "gavel", "gaze", "gecko", "ghost",
    "giant", "given", "gizmo", "glad", "gland", "glare", "glass", "gleam", "glide", "glint",
    "globe", "gloom", "glory", "gloss", "glove", "glyph", "going", "golden", "golfer", "goose",
    "gorge", "grace", "grade", "grain", "grand", "grant", "grape", "graph", "grasp", "grass",
    "grave", "graze", "great", "greed", "green", "greet", "grief", "grill", "grind", "gripe",
    "groin", "groom", "gross", "group", "grove", "growl", "grown", "gruel", "grump", "guard",
    "guess", "guide", "guild", "guilt", "guise", "gulch", "gummy", "gusto", "gusty", "gutter",
    "haven", "havoc", "hazel", "heart", "heath", "heavy", "hedge", "heist", "hello", "hence",
    "herb", "heron", "hired", "hitch", "hobby", "homer", "honey", "honor", "horse", "hotel",
    "house", "hover", "human", "humid", "humor", "husky", "hyena", "icing", "ideal", "igloo",
    "image", "imply", "inbox", "index", "indie", "inert", "infer", "ingot", "inner", "input",
    "intro", "ionic", "irate", "ivory", "jaunt", "jazzy", "jelly", "jewel", "jiffy", "joint",
    "joker", "jolly", "joust", "judge", "juice", "jumbo", "jumpy", "juror", "karma", "kayak",
    "kebab", "khaki", "kinky", "kiosk", "knife", "knack", "kneel", "knelt", "knobs", "knock",
    "knoll", "known", "koala", "label", "lance", "lapse", "large", "laser", "latch", "later",
    "lathe", "layer", "leach", "leafy", "lean", "learn", "lease", "ledge", "legal", "lemon",
    "level", "lever", "light", "lilac", "limbo", "linen", "liner", "lingo", "llama", "lobby",
    "local", "lodge", "lofty", "logic", "login", "lotus", "lousy", "lover", "loyal", "lucid",
    "lucky", "lunar", "lunch", "lunge", "lusty", "lying", "lyric", "macro", "mafia", "magic",
    "major", "maker", "mango", "manor", "maple", "march", "marsh", "mason", "match", "matte",
    "maxim", "mayor", "mealy", "media", "melon", "mercy", "merit", "mesa", "metal", "meter",
    "might", "mince", "minor", "minus", "mirth", "miser", "misty", "mixer", "mocha", "model",
    "mogul", "moist", "money", "month", "moose", "moral", "morph", "motel", "motor", "mound",
    "mount", "mourn", "mouse", "moved", "movie", "mower", "mucus", "muddy", "mulch", "mural",
    "music", "musty", "naive", "nanny", "nasal", "natal", "naval", "nerve", "never", "newly",
    "nexus", "night", "ninja", "noble", "noise", "north", "notch", "noted", "novel", "nudge",
    "nurse", "nylon", "oasis", "ocean", "offer", "olive", "omega", "onset", "opera", "optic",
    "orbit", "order", "organ", "other", "otter", "ought", "outer", "overt", "oxide", "ozone",
    "paddy", "paint", "panda", "panel", "panic", "paper", "party", "paste", "patch", "patio",
    "pause", "peach", "pearl", "pedal", "penny", "perch", "peril", "phase", "photo", "piano",
    "piece", "pilot", "pinch", "pixel", "pizza", "place", "plaid", "plain", "plane", "plank",
    "plant", "plate", "plaza", "plead", "pleat", "plier", "pluck", "plumb", "plume", "plump",
    "plunge", "plush", "poach", "point", "polar", "polka", "poser", "posit", "pouch", "pound",
    "power", "prank", "prawn", "press", "price", "pride", "prime", "print", "prior", "prism",
    "prize", "probe", "prone", "proof", "prose", "proud", "prune", "psych", "pulse", "pupil",
    "puppy", "purge", "pushy", "quack", "qualm", "quart", "queen", "query", "quest", "queue",
    "quick", "quiet", "quill", "quirk", "quota", "quote", "radar", "radio", "rally", "ranch",
    "range", "rapid", "raven", "rayon", "razor", "reach", "react", "realm", "rebel", "rebus",
    "recap", "regal", "rehab", "reign", "relax", "relay", "relic", "remit", "renew", "repay",
    "repel", "reply", "retry", "rhino", "ridge", "rifle", "rigid", "rinse", "ripen", "risen",
    "risky", "rival", "river", "roast", "robin", "robot", "rocky", "rogue", "roman", "roomy",
    "roster", "rover", "royal", "rugby", "ruler", "rumor", "rural", "rusty", "sadly", "saint",
    "salad", "salon", "salsa", "satin", "sauna", "savor", "scale", "scare", "scene", "scent",
    "scone", "scope", "score", "scout", "scrap", "sedan", "seize", "sense", "serum", "serve",
    "seven", "shade", "shaft", "shake", "shall", "shame", "shape", "share", "shark", "sharp",
    "shawl", "sheep", "sheer", "sheet", "shelf", "shell", "shift", "shirt", "shock", "shore",
    "short", "shout", "shown", "shrub", "sight", "sigma", "since", "sixth", "sixty", "sized",
    "skill", "skimp", "skull", "skunk", "slate", "sleep", "sleet", "slept", "slice", "slide",
    "slope", "sloth", "slump", "smack", "small", "smart", "smear", "smell", "smile", "smirk",
    "smith", "smoke", "snack", "snake", "snare", "sneak", "snore", "snout", "solar", "solid",
    "solve", "sonic", "south", "space", "spare", "spark", "spawn", "speak", "spear", "speed",
    "spend", "spent", "spice", "spicy", "spine", "spite", "split", "spoke", "spoon", "sport",
    "spray", "spree", "squad", "stack", "staff", "stage", "stain", "stair", "stake", "stale",
    "stall", "stamp", "stand", "stank", "stark", "start", "state", "stave", "stays", "steam",
    "steel", "steep", "steer", "stems", "stern", "stick", "stiff", "still", "stock", "stoic",
    "stone", "stood", "stool", "store", "storm", "story", "stout", "stove", "strap", "straw",
    "stray", "strip", "stuck", "study", "stuff", "stump", "style", "sugar", "suite", "sunny",
    "super", "surge", "swamp", "swarm", "swear", "sweat", "sweep", "sweet", "swept", "swift",
    "swing", "swirl", "swore", "sworn", "syrup", "tabby", "table", "tally", "talon", "tangy",
    "tango", "tapir", "taste", "taunt", "tempo", "tense", "tepid", "theta", "thick", "thing",
    "think", "thorn", "those", "three", "threw", "throw", "thump", "tiger", "tight", "timer",
    "timid", "tipsy", "titan", "title", "toast", "token", "tonic", "topaz", "torch", "total",
    "touch", "tough", "towel", "tower", "toxic", "trace", "track", "trade", "trail", "train",
    "trait", "trash", "trawl", "trend", "trial", "tribe", "trick", "troop", "trout", "truck",
    "truly", "trump", "trunk", "trust", "truth", "tulip", "tumor", "tuner", "turbo", "tutor",
    "twang", "tweed", "twice", "twist", "ultra", "uncut", "under", "undue", "unfit", "union",
    "unite", "unity", "until", "upper", "upset", "urban", "usage", "usher", "using", "usual",
    "utter", "valet", "valid", "valor", "valve", "vault", "venom", "venue", "verse", "vigor",
    "vinyl", "viola", "viper", "viral", "visit", "visor", "vista", "vital", "vivid", "vocal",
    "vodka", "voice", "voter", "vouch", "vowel", "wages", "wagon", "waist", "waste", "watch",
    "water", "waved", "waxed", "weary", "weave", "wedge", "weird", "wheat", "wheel", "where",
    "which", "while", "white", "whole", "widen", "width", "wield", "windy", "witch", "woken",
    "woman", "world", "worry", "worst", "worth", "wound", "wrath", "wrist", "wrote", "yacht",
    "yearn", "yeast", "yield", "young", "youth", "zebra", "zilch", "zones",
];

#[cfg(test)]
mod hash_forge_tests {
    use super::*;

    /// BLAKE3 of empty input (32-byte classic digest) — known test vector.
    const BLAKE3_EMPTY: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn blake3_empty_input_matches_known_vector() {
        let hex = hash_text("".into(), "blake3".into(), None, None).expect("empty blake3");
        assert_eq!(hex, BLAKE3_EMPTY);
    }

    #[test]
    fn blake3_xof_32_equals_classic_digest() {
        let classic = hash_text("hello".into(), "blake3".into(), None, None).unwrap();
        let xof32 = hash_text("hello".into(), "blake3".into(), Some(32), None).unwrap();
        assert_eq!(classic, xof32);
        assert_eq!(classic.len(), 64); // 32 bytes → 64 hex chars
    }

    #[test]
    fn blake3_xof_64_is_128_hex_chars_and_extends_classic() {
        let classic = hash_text("hello".into(), "blake3".into(), None, None).unwrap();
        let xof64 = hash_text("hello".into(), "blake3".into(), Some(64), None).unwrap();
        assert_eq!(xof64.len(), 128);
        assert!(xof64.starts_with(&classic));
    }

    #[test]
    fn blake3_output_len_bounds() {
        assert!(hash_text("x".into(), "blake3".into(), Some(0), None).is_err());
        assert!(hash_text("x".into(), "blake3".into(), Some(1025), None).is_err());
        assert!(hash_text("x".into(), "blake3".into(), Some(1), None).is_ok());
        assert!(hash_text("x".into(), "blake3".into(), Some(1024), None).is_ok());
    }

    #[test]
    fn encoding_utf8_default_hashes_raw_bytes() {
        let a = hash_text("hi".into(), "sha256".into(), None, None).unwrap();
        let b = hash_text("hi".into(), "sha256".into(), None, Some("utf-8".into())).unwrap();
        assert_eq!(a, b);
        // SHA-256("hi") known value
        assert_eq!(
            a,
            "8f434346648f6b96df89dda901c5176b10a6d83961dd3c1ac88b59b2dc327aa4"
        );
    }

    #[test]
    fn encoding_base64_happy_path() {
        // base64("hi") = "aGk="
        let from_b64 =
            hash_text("aGk=".into(), "sha256".into(), None, Some("base64".into())).unwrap();
        let from_utf8 =
            hash_text("hi".into(), "sha256".into(), None, Some("utf-8".into())).unwrap();
        assert_eq!(from_b64, from_utf8);
    }

    #[test]
    fn encoding_hex_happy_path() {
        // hex("hi") = "6869"
        let from_hex =
            hash_text("68 69".into(), "sha256".into(), None, Some("hex".into())).unwrap();
        let from_utf8 =
            hash_text("hi".into(), "sha256".into(), None, Some("utf-8".into())).unwrap();
        assert_eq!(from_hex, from_utf8);
    }

    #[test]
    fn encoding_binary_happy_path() {
        // "hi" = 0x68 0x69 → binary
        let bits = "01101000 01101001";
        let from_bin =
            hash_text(bits.into(), "sha256".into(), None, Some("binary".into())).unwrap();
        let from_utf8 =
            hash_text("hi".into(), "sha256".into(), None, Some("utf-8".into())).unwrap();
        assert_eq!(from_bin, from_utf8);
    }

    #[test]
    fn encoding_invalid_inputs_error() {
        assert!(hash_text("!!!".into(), "sha256".into(), None, Some("base64".into())).is_err());
        assert!(hash_text("zz".into(), "sha256".into(), None, Some("hex".into())).is_err());
        assert!(hash_text("012".into(), "sha256".into(), None, Some("binary".into())).is_err()); // not multiple of 8
        assert!(hash_text("0123".into(), "sha256".into(), None, Some("binary".into())).is_err()); // not only 0/1
        assert!(hash_text("x".into(), "sha256".into(), None, Some("rot13".into())).is_err());
    }

    #[test]
    fn non_blake3_ignores_output_len() {
        let a = hash_text("hi".into(), "md5".into(), None, None).unwrap();
        let b = hash_text("hi".into(), "md5".into(), Some(64), None).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 32); // md5 is always 16 bytes
    }
}

#[cfg(test)]
mod password_forge_tests {
    use super::*;

    fn generate_with(
        length: usize,
        symbols: bool,
        symbol_groups: Option<Vec<String>>,
        custom: Option<&str>,
        excluded: Option<&str>,
        require_each: bool,
    ) -> Vec<String> {
        generate_password(
            length,
            true,
            true,
            true,
            symbols,
            false,
            10,
            symbol_groups,
            custom.map(str::to_owned),
            excluded.map(str::to_owned),
            Some(require_each),
        )
        .unwrap()
    }

    #[test]
    fn custom_and_excluded_characters_are_enforced() {
        let values = generate_with(32, false, None, Some("!_"), Some("_O0l1"), true);
        for value in values {
            assert_eq!(value.len(), 32);
            assert!(value.contains('!'));
            assert!(!value.chars().any(|c| "_O0l1".contains(c)));
        }
    }

    #[test]
    fn every_selected_symbol_group_is_represented() {
        let values = generate_with(
            20,
            true,
            Some(vec!["punctuation".into(), "brackets".into()]),
            None,
            None,
            true,
        );
        for value in values {
            assert!(value.bytes().any(|c| SYMBOL_PUNCTUATION.contains(&c)));
            assert!(value.bytes().any(|c| SYMBOL_BRACKETS.contains(&c)));
            assert!(value.bytes().any(|c| UPPER_FULL.contains(&c)));
            assert!(value.bytes().any(|c| LOWER_FULL.contains(&c)));
            assert!(value.bytes().any(|c| DIGITS_FULL.contains(&c)));
        }
    }

    #[test]
    fn rejects_unknown_or_empty_required_groups() {
        assert!(generate_password(
            16,
            false,
            false,
            false,
            true,
            false,
            1,
            Some(vec!["unknown".into()]),
            None,
            None,
            Some(true),
        )
        .is_err());
        assert!(generate_password(
            16,
            true,
            true,
            true,
            true,
            false,
            1,
            Some(vec![]),
            None,
            None,
            Some(true),
        )
        .is_err());
        assert!(generate_password(
            8,
            true,
            true,
            true,
            true,
            false,
            1,
            Some(vec!["punctuation".into(), "brackets".into()]),
            Some("xyz".into()),
            None,
            Some(true),
        )
        .is_ok());
    }

    #[test]
    fn passphrase_wordlist_is_unique() {
        let unique: std::collections::HashSet<_> = WORDLIST.iter().collect();
        assert_eq!(WORDLIST.len(), 1133);
        assert_eq!(unique.len(), WORDLIST.len());
    }
}
