// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Rclone crypt compatibility layer.
//!
//! Decrypts files and filenames produced by `rclone crypt` (XSalsa20-Poly1305
//! content encryption, EME/AES-256 filename encryption in `standard` mode).

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit as AesKeyInit};
use aes::Aes256;
use crypto_secretbox::aead::Aead;
use crypto_secretbox::XSalsa20Poly1305;
use rand::RngCore;
use scrypt::{scrypt, Params as ScryptParams};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

// ── Constants ──────────────────────────────────────────────────────────────

/// Magic header for rclone crypt files.
const RCLONE_MAGIC: &[u8; 8] = b"RCLONE\x00\x00";

/// File nonce size (24 bytes for XSalsa20).
const FILE_NONCE_SIZE: usize = 24;

/// Header size = magic (8) + nonce (24) = 32 bytes.
const HEADER_SIZE: usize = 8 + FILE_NONCE_SIZE;

/// Plaintext chunk size: 64 KB.
const CHUNK_DATA_SIZE: usize = 65536;

/// Poly1305 auth tag size.
const CHUNK_TAG_SIZE: usize = 16;

/// Ciphertext chunk size = plaintext + tag.
const CHUNK_CIPHER_SIZE: usize = CHUNK_DATA_SIZE + CHUNK_TAG_SIZE;

/// scrypt parameters matching rclone: N=16384 (2^14), r=8, p=1, output=80.
const SCRYPT_LOG_N: u8 = 14;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const SCRYPT_KEY_LEN: usize = 80;
const SCRYPT_PARAMS_LEN: usize = 64;
type RcloneCryptKeyMaterial = ([u8; 32], [u8; 32], [u8; 16]);
const RCLONE_DEFAULT_SALT: [u8; 16] = [
    0xA8, 0x0D, 0xF4, 0x3A, 0x8F, 0xBD, 0x03, 0x08, 0xA7, 0xCA, 0xB8, 0x3E, 0x58, 0x1F, 0x86, 0xB1,
];

/// AES block size.
const AES_BLOCK: usize = 16;
const MAX_DECRYPT_INPUT_BYTES: usize = 512 * 1024 * 1024;

// ── Types ──────────────────────────────────────────────────────────────────

/// Filename encryption mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilenameEncryption {
    Standard,
    Obfuscate,
    Off,
}

/// Suffix rclone appends to file names when `filename_encryption = off`.
///
/// With name encryption off rclone still tags every encrypted object with a
/// suffix (default `.bin`) so it can tell encrypted files apart; a remote
/// without it triggers rclone's "not an encrypted file - does not match
/// suffix" skip. We append/strip the same suffix for drop-in interop. An
/// empty suffix (or the literal `none`) disables it, matching rclone's
/// `suffix = none`.
pub const DEFAULT_OFF_SUFFIX: &str = ".bin";

/// Resolve a user-supplied suffix string to the effective suffix.
/// `None` -> rclone default `.bin`; `Some("none")` or `Some("")` -> no suffix.
pub fn resolve_off_suffix(arg: Option<&str>) -> String {
    match arg {
        None => DEFAULT_OFF_SUFFIX.to_string(),
        Some(s) if s.eq_ignore_ascii_case("none") || s.is_empty() => String::new(),
        Some(s) => s.to_string(),
    }
}

/// Derived keys for an unlocked rclone crypt remote.
///
/// `Clone` backs the connection-scoped key cache (an instant re-arm after a
/// view-only lock); every clone still zeroizes on drop via the `Drop` impl below.
#[derive(Clone)]
pub struct RcloneCryptKeys {
    pub name_key: [u8; 32],
    pub data_key: [u8; 32],
    pub name_tweak: [u8; 16],
    pub filename_encryption: FilenameEncryption,
    pub off_suffix: String,
    #[allow(dead_code)] // Used in Phase 4 directory traversal
    pub directory_name_encryption: bool,
}

struct OutputPathGuard {
    final_path: PathBuf,
    temp_file: tempfile::NamedTempFile,
}

impl OutputPathGuard {
    fn new(output_path: &str) -> Result<Self, String> {
        crate::filesystem::validate_path(output_path)?;

        let final_path = PathBuf::from(output_path);
        let parent = final_path
            .parent()
            .ok_or_else(|| "Output path must have a parent directory".to_string())?;

        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|e| format!("failed to resolve output parent directory: {}", e))?;
        if canonical_parent
            .symlink_metadata()
            .map_err(|e| format!("failed to inspect output parent directory: {}", e))?
            .file_type()
            .is_symlink()
        {
            return Err("Output parent directory cannot be a symlink".to_string());
        }

        if let Ok(meta) = std::fs::symlink_metadata(&final_path) {
            if meta.file_type().is_symlink() {
                return Err("Output path cannot be a symlink".to_string());
            }
            if meta.is_dir() {
                return Err("Output path cannot be a directory".to_string());
            }
        }

        let temp_file = tempfile::NamedTempFile::new_in(&canonical_parent)
            .map_err(|e| format!("failed to create temporary output file: {}", e))?;

        Ok(Self {
            final_path,
            temp_file,
        })
    }

    fn write_all(mut self, plaintext: &[u8]) -> Result<String, String> {
        use std::io::Write;

        self.temp_file
            .write_all(plaintext)
            .map_err(|e| format!("failed to write temporary output: {}", e))?;
        self.temp_file
            .as_file_mut()
            .sync_all()
            .map_err(|e| format!("failed to flush temporary output: {}", e))?;

        self.temp_file
            .persist(&self.final_path)
            .map_err(|e| format!("failed to persist output file: {}", e.error))?;

        Ok(self.final_path.to_string_lossy().to_string())
    }
}

impl Drop for RcloneCryptKeys {
    fn drop(&mut self) {
        self.name_key.zeroize();
        self.data_key.zeroize();
        self.name_tweak.zeroize();
    }
}

// ── Phase 1: Key derivation ────────────────────────────────────────────────

/// Derive name_key (32 bytes) and data_key (32 bytes) from password and
/// optional salt (password2). Compatible with rclone's scrypt parameters.
pub fn derive_keys(password: &str, salt: &str) -> Result<([u8; 32], [u8; 32]), String> {
    let (name_key, data_key, _) = derive_keys_with_tweak(password, salt)?;
    Ok((name_key, data_key))
}

/// Which secret is being revealed. The two are NOT interchangeable and the
/// difference decides one ambiguous case, so the caller has to say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RcloneSecret {
    /// `password` in rclone terms. An empty reveal here would be rclone's
    /// empty crypt password, which rclone accepts and turns into an all-zero
    /// key (rclone v1.75.1 `backend/crypt/cipher.go`, `Key`). The guess for a
    /// secret whose form is not recorded never reads a password that way: it
    /// keeps the input, a literal password that happened to look like base64.
    /// A password recorded obscured that reveals to nothing does reach that
    /// case, and [`resolve_crypt_password`] refuses it.
    Password,
    /// `password2`, the salt. Here `obscure("")` is documented and means
    /// "no salt", which selects rclone's built-in default salt, so an empty
    /// reveal is a legitimate value and not a failure.
    Salt,
}

/// If `value` is an rclone-obscured secret (`rclone obscure` / `rclone.conf`
/// password / password2), return the revealed plaintext. Otherwise return
/// `value` unchanged. Closed: a string that is not valid rclone-obscure is
/// never modified (#600: Ehud pasted rclone.conf password1/password2).
///
/// # The empty reveal, and why the answer depends on the field
///
/// rclone-obscure is AES-CTR with a 16-byte IV prepended, base64url without
/// padding. A 22-character string decodes to exactly 16 bytes: all IV and no
/// ciphertext, so it reveals as the empty string.
///
/// Measured rather than assumed, because the first statement of this was wider
/// than the truth: it is NOT every 22-character string over `[A-Za-z0-9_-]`.
/// Strict base64 requires the discarded low bits of the final symbol to be
/// zero, so the last character must be one of `A`, `Q`, `g`, `w`. That is 4 of
/// 64, one in sixteen of such passwords, which is rare enough to never appear
/// in testing and common enough to reach a user: password managers generate
/// inside that alphabet.
///
/// Treating that empty result as the secret would derive the key from nothing
/// and encrypt with it, silently, which is the worst available outcome: the
/// data is written, no error is raised, and rclone cannot open it.
///
/// The field decides. For a PASSWORD an empty result would mean rclone's
/// empty password, which rclone does accept (with an all-zero key), but the
/// overlay requires a password while a 22-character password drawn from that
/// alphabet is ordinary, so the input is kept as the literal. For a SALT it is
/// exactly what `password2 = obscure("")` means, which `rclone config create`
/// writes for an empty salt. Same bytes, opposite reading.
fn maybe_rclone_reveal(value: &str, secret: RcloneSecret) -> String {
    if value.is_empty() {
        return String::new();
    }
    match crate::rclone_import::reveal_obscured(value) {
        Ok(plain) if plain.is_empty() => match secret {
            // Never plausible: keep the literal the user typed.
            RcloneSecret::Password => value.to_string(),
            // rclone `password2 = obscure("")`: omitted salt, which must
            // collapse to the default salt, not stay as the blob.
            RcloneSecret::Salt => plain,
        },
        Ok(plain) if plain != value && plain.chars().all(|c| !c.is_control()) => plain,
        _ => value.to_string(),
    }
}

/// How an rclone-crypt password or salt is held: as the user typed it, or
/// rclone-obscured (the form rclone.conf keeps). The shape of a value cannot
/// tell the two apart: a salt rclone generates (22 URL-safe base64 characters)
/// is also what `rclone obscure ""` prints, and a 23 or 24 character
/// alphanumeric password decodes to one or two characters about one time in
/// six. So the binding records it (`passwordForm` / `saltForm`), and a secret
/// whose form is not recorded is only guessed at when the guess is safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptSecretForm {
    Clear,
    Obscured,
    /// Not recorded, on a binding the rclone importer wrote before forms were
    /// recorded: the value is the one the importer revealed, unless an obscured
    /// one was pasted over it later. Never parsed from, nor written to, a
    /// binding ([`crypt_secret_forms`] alone produces it).
    ImportedUnrecorded,
}

impl CryptSecretForm {
    /// `"clear"` / `"obscured"`; anything else records no form.
    pub fn parse(text: Option<&str>) -> Option<Self> {
        match text {
            Some("clear") => Some(Self::Clear),
            Some("obscured") => Some(Self::Obscured),
            _ => None,
        }
    }

    /// The form a binding records under `field`, `None` when it records none.
    pub fn from_binding(binding: &serde_json::Value, field: &str) -> Option<Self> {
        Self::parse(binding.get(field).and_then(|v| v.as_str()))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Obscured => "obscured",
            Self::ImportedUnrecorded => "imported-unrecorded",
        }
    }
}

/// The forms a profile's rclone-crypt password and salt are held in: what the
/// binding records, or, for a binding the rclone importer wrote before forms
/// were recorded (its options keep `rcloneCryptOverlayName` /
/// `rcloneCryptRemote`), [`CryptSecretForm::ImportedUnrecorded`].
pub fn crypt_secret_forms(
    profile: &serde_json::Value,
) -> (Option<CryptSecretForm>, Option<CryptSecretForm>) {
    let binding = profile
        .get("aeroCryptOverlay")
        .unwrap_or(&serde_json::Value::Null);
    let recorded = (
        CryptSecretForm::from_binding(binding, "passwordForm"),
        CryptSecretForm::from_binding(binding, "saltForm"),
    );
    let imported = profile
        .get("options")
        .map(|o| o.get("rcloneCryptOverlayName").is_some() || o.get("rcloneCryptRemote").is_some())
        .unwrap_or(false);
    if imported {
        (
            recorded.0.or(Some(CryptSecretForm::ImportedUnrecorded)),
            recorded.1.or(Some(CryptSecretForm::ImportedUnrecorded)),
        )
    } else {
        recorded
    }
}

/// The form of a secret that came from the vault (`recorded`, the binding's) or,
/// when `from_env`, from an environment variable: then the one `form_var`
/// states (`clear` / `obscured`), or none.
pub fn secret_form_for_source(
    recorded: Option<CryptSecretForm>,
    from_env: bool,
    form_var: &str,
) -> Option<CryptSecretForm> {
    if from_env {
        CryptSecretForm::parse(std::env::var(form_var).ok().as_deref())
    } else {
        recorded
    }
}

/// How to answer a refusal, in every place a form can be recorded. It names no
/// GUI label: this text is English in every language, the labels are not.
const RECORD_THE_FORM: &str = "Say how it was entered, typed as it is or pasted from \
     rclone.conf (a value AeroFTP imported from rclone.conf was stored as typed): in \
     AeroFTP, with the choice under the password and the salt in the profile's crypt \
     settings; with the CLI, run `aeroftp-cli crypt set-form --profile \
     <name> --password-form clear|obscured --salt-form clear|obscured`, pass \
     `--password-form` / `--salt-form` to a command that takes the secret itself, or set \
     AEROFTP_CRYPT_OVERLAY_PASSWORD_FORM / AEROFTP_CRYPT_OVERLAY_SALT_FORM for a secret \
     taken from the environment";

/// 22 characters are the IV of an obscured value alone: from there a password
/// or a salt rclone generated (128 bits, 22 URL-safe base64 characters) can
/// decode to nothing or to a character or two.
const SHORT_READING_MIN_INPUT: usize = 22;
const SHORT_READING_MAX_CHARS: usize = 2;

/// The length of the reading the unrecorded guess ([`maybe_rclone_reveal`])
/// would use, when it is too short to trust: an input of 22 characters or more
/// that the guess turns into 2 characters or fewer. A reading the guess does
/// not use is not counted, because it is the guess's result that is measured:
/// a password that reveals to nothing, or to control characters, was always
/// kept as typed, and a value kept as typed is 22 characters or more. The
/// input is measured in bytes, which for base64 (ASCII, the only input that
/// decodes) is its length in characters; the reading can be any UTF-8, so it
/// is measured in characters.
fn short_guess(value: &str, secret: RcloneSecret) -> Option<usize> {
    if value.len() < SHORT_READING_MIN_INPUT {
        return None;
    }
    let chars = maybe_rclone_reveal(value, secret).chars().count();
    (chars <= SHORT_READING_MAX_CHARS).then_some(chars)
}

/// The error for an unrecorded secret whose guessed reading is too short.
fn ambiguous_secret_error(field: &str, revealed_chars: usize) -> String {
    let other = if revealed_chars == 0 {
        format!("an rclone-obscured empty {field}")
    } else {
        format!("an rclone-obscured value that gives a {revealed_chars}-character {field}")
    };
    format!(
        "the crypt {field} reads two ways: as typed, or as {other}. AeroFTP does not \
         guess. {RECORD_THE_FORM}"
    )
}

/// Reveals `value` as rclone does for a field it knows to be obscured, with
/// the importer's reveal (`rclone_import::reveal_rclone_password`): raw URL-safe
/// base64, trailing bits ignored as rclone ignores them, no plaintext fallback.
fn reveal_known_obscured(value: &str, field: &str) -> Result<String, String> {
    crate::rclone_import::reveal_rclone_password(value).map_err(|why| {
        format!(
            "the crypt {field} is marked as pasted from rclone.conf but does not reveal \
             ({why}). {RECORD_THE_FORM}"
        )
    })
}

/// A secret the rclone importer stored before forms were recorded. It is the
/// revealed value, so clear, unless an obscured value was pasted over it later
/// (#600): a value that also reveals to 3 or more printable characters could
/// be either, and reading it as the other would change the key of an overlay
/// that works, so it is refused rather than guessed. Only a value exactly as
/// rclone writes one counts ([`crate::rclone_import::reveal_canonical_rclone`]):
/// the lenient decoder also reads about one 26-character value in twenty that
/// rclone could never have written, and those were always used as they are.
/// The refusal names both secrets: they were stored by the same import, and a
/// form recorded for one only leaves the other on this reading.
fn resolve_imported_unrecorded(value: &str, field: &str) -> Result<String, String> {
    match crate::rclone_import::reveal_canonical_rclone(value) {
        Ok(revealed)
            if revealed.chars().count() > SHORT_READING_MAX_CHARS
                && !revealed.chars().any(char::is_control) =>
        {
            Err(format!(
                "the crypt {field} was stored by the rclone import, but it also reads as \
                 a value pasted from rclone.conf later. AeroFTP does not guess: record how \
                 both the password and the salt were entered. {RECORD_THE_FORM}"
            ))
        }
        _ => Ok(value.to_string()),
    }
}

/// The crypt password a key is derived from, for `value` held in `form`. An
/// empty value is no password in every form, and [`derive_keys_with_forms`]
/// refuses to derive a key from it.
///
/// - `Clear`: as it is.
/// - `Obscured`: revealed the way rclone reveals it, or an error. A reveal of
///   2 characters or fewer is refused: rclone's generated passwords are 22
///   characters, which is exactly an obscured empty value, so a password typed
///   as it is and marked obscured by mistake lands there, on a key from nothing.
/// - `ImportedUnrecorded`: [`resolve_imported_unrecorded`].
/// - `None`: the reading used before forms were recorded ([`maybe_rclone_reveal`]),
///   refused when the reading it would use is too short ([`short_guess`]).
pub fn resolve_crypt_password(
    value: &str,
    form: Option<CryptSecretForm>,
) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    match form {
        Some(CryptSecretForm::Clear) => Ok(value.to_string()),
        Some(CryptSecretForm::Obscured) => {
            let revealed = reveal_known_obscured(value, "password")?;
            let chars = revealed.chars().count();
            if chars <= SHORT_READING_MAX_CHARS {
                return Err(format!(
                    "the crypt password is marked as pasted from rclone.conf, but it reveals \
                     to {chars} character(s); a password rclone generates, typed as it is, \
                     looks exactly like that. {RECORD_THE_FORM}"
                ));
            }
            Ok(revealed)
        }
        Some(CryptSecretForm::ImportedUnrecorded) => resolve_imported_unrecorded(value, "password"),
        None => match short_guess(value, RcloneSecret::Password) {
            Some(chars) => Err(ambiguous_secret_error("password", chars)),
            None => Ok(maybe_rclone_reveal(value, RcloneSecret::Password)),
        },
    }
}

/// [`resolve_crypt_password`] for the salt (`password2`). Recorded obscured,
/// an empty reveal is rclone's omitted salt (`rclone config create ...
/// password2=` writes `obscure("")`) and selects the default one; unrecorded,
/// the same short readings as the password's are refused, since a salt rclone
/// generated reveals to nothing.
pub fn resolve_crypt_salt(value: &str, form: Option<CryptSecretForm>) -> Result<String, String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    match form {
        Some(CryptSecretForm::Clear) => Ok(value.to_string()),
        Some(CryptSecretForm::Obscured) => reveal_known_obscured(value, "salt"),
        Some(CryptSecretForm::ImportedUnrecorded) => resolve_imported_unrecorded(value, "salt"),
        None => match short_guess(value, RcloneSecret::Salt) {
            Some(chars) => Err(ambiguous_secret_error("salt", chars)),
            None => Ok(maybe_rclone_reveal(value, RcloneSecret::Salt)),
        },
    }
}

/// What the eye button of the profile form shows for a stored rclone-crypt
/// password or salt: the secret itself when the value is recorded as
/// obscured, the stored value as it is otherwise (recorded clear, or not
/// recorded, which the form shows as saved). Async so it stays off the main
/// thread, as every command does (`sync_command_audit`); the work is a reveal
/// in memory and needs no blocking pool.
#[tauri::command]
pub async fn rclone_crypt_secret_for_display(
    value: String,
    form: Option<String>,
    field: String,
) -> Result<String, String> {
    match CryptSecretForm::parse(form.as_deref()) {
        Some(CryptSecretForm::Obscured) if field == "salt" => {
            resolve_crypt_salt(&value, Some(CryptSecretForm::Obscured))
        }
        Some(CryptSecretForm::Obscured) => {
            resolve_crypt_password(&value, Some(CryptSecretForm::Obscured))
        }
        _ => Ok(value),
    }
}

/// Derive data_key (32 bytes), name_key (32 bytes), and name_tweak (16 bytes),
/// reading the password and salt in the forms the caller knows them to be in
/// ([`resolve_crypt_password`], [`resolve_crypt_salt`]).
pub fn derive_keys_with_forms(
    password: &str,
    password_form: Option<CryptSecretForm>,
    salt: &str,
    salt_form: Option<CryptSecretForm>,
) -> Result<RcloneCryptKeyMaterial, String> {
    let password = zeroize::Zeroizing::new(resolve_crypt_password(password, password_form)?);
    // The one place every rclone-crypt reader goes through (overlay, unlock and
    // create-remote commands, compare, CLI, MCP): rclone would take an empty
    // password and derive the all-zero key, which anyone can open.
    if password.is_empty() {
        return Err("an rclone-crypt password is required".to_string());
    }
    let salt = zeroize::Zeroizing::new(resolve_crypt_salt(salt, salt_form)?);
    derive_keys_from_clear(&password, &salt)
}

/// [`derive_keys_with_forms`] for a caller that does not know the forms (a CLI
/// flag, an environment variable, a value typed where no form is asked).
pub fn derive_keys_with_tweak(
    password: &str,
    salt: &str,
) -> Result<RcloneCryptKeyMaterial, String> {
    derive_keys_with_forms(password, None, salt, None)
}

/// Rclone derives 80 bytes in this order: data key, name key, then EME tweak.
/// An empty password gives an all-zero key, as in rclone.
fn derive_keys_from_clear(password: &str, salt: &str) -> Result<RcloneCryptKeyMaterial, String> {
    // scrypt 0.11 limits Params::len to <=64 for password-hash metadata, but
    // the raw scrypt() function accepts rclone's 80-byte output buffer.
    let params = ScryptParams::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_PARAMS_LEN)
        .map_err(|e| format!("invalid scrypt params: {}", e))?;

    let mut key_bytes = [0u8; SCRYPT_KEY_LEN];
    if !password.is_empty() {
        let salt_bytes: &[u8] = if salt.is_empty() {
            &RCLONE_DEFAULT_SALT
        } else {
            salt.as_bytes()
        };
        scrypt(password.as_bytes(), salt_bytes, &params, &mut key_bytes)
            .map_err(|e| format!("scrypt failed: {}", e))?;
    }

    let mut name_key = [0u8; 32];
    let mut data_key = [0u8; 32];
    let mut name_tweak = [0u8; 16];
    data_key.copy_from_slice(&key_bytes[..32]);
    name_key.copy_from_slice(&key_bytes[32..64]);
    name_tweak.copy_from_slice(&key_bytes[64..80]);

    key_bytes.zeroize();
    Ok((name_key, data_key, name_tweak))
}

// ── Phase 1: File content decryption ───────────────────────────────────────

/// Decrypt an rclone-crypt encrypted file.
///
/// `data` must include the full file: magic header + nonce + encrypted chunks.
/// Returns the decrypted plaintext. Empty files (header-only) return empty vec.
pub fn decrypt_file_content(data: &[u8], data_key: &[u8; 32]) -> Result<Vec<u8>, String> {
    if data.len() > MAX_DECRYPT_INPUT_BYTES {
        return Err(format!(
            "encrypted input too large for in-memory decrypt path ({} bytes > {} bytes)",
            data.len(),
            MAX_DECRYPT_INPUT_BYTES
        ));
    }

    // Validate header
    if data.len() < HEADER_SIZE {
        return Err("file too short for rclone crypt header".into());
    }
    if &data[..8] != RCLONE_MAGIC {
        return Err("invalid rclone crypt magic header".into());
    }

    // Read file nonce
    let mut file_nonce = [0u8; FILE_NONCE_SIZE];
    file_nonce.copy_from_slice(&data[8..HEADER_SIZE]);

    // Create cipher
    let cipher = XSalsa20Poly1305::new(data_key.into());

    // Decrypt chunks
    let chunk_data = &data[HEADER_SIZE..];
    if chunk_data.is_empty() {
        return Ok(Vec::new()); // empty file
    }

    let mut plaintext = Vec::new();
    let mut offset = 0;
    let mut chunk_num: u64 = 0;

    while offset < chunk_data.len() {
        let remaining = chunk_data.len() - offset;
        let chunk_size = remaining.min(CHUNK_CIPHER_SIZE);

        // Minimum valid chunk: tag (16) + 1 byte plaintext = 17
        if chunk_size <= CHUNK_TAG_SIZE {
            return Err(format!(
                "chunk {} truncated ({} bytes, need > {})",
                chunk_num, chunk_size, CHUNK_TAG_SIZE
            ));
        }

        let chunk = &chunk_data[offset..offset + chunk_size];

        // Compute per-chunk nonce: file_nonce + chunk_num (LE addition on first 8 bytes)
        let nonce = chunk_nonce(&file_nonce, chunk_num);

        let decrypted = cipher
            .decrypt((&nonce).into(), chunk)
            .map_err(|_| format!("chunk {} decrypt failed (wrong key?)", chunk_num))?;

        plaintext.extend_from_slice(&decrypted);
        offset += chunk_size;
        chunk_num += 1;
    }

    Ok(plaintext)
}

/// Encrypt plaintext into the rclone-crypt file format.
///
/// Output layout: magic header + 24-byte file nonce + XSalsa20-Poly1305 chunks.
/// The per-chunk nonce follows rclone's counter semantics and is shared with
/// the decrypt path through `chunk_nonce`.
pub fn encrypt_file_content(plaintext: &[u8], data_key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let cipher = XSalsa20Poly1305::new(data_key.into());

    let mut file_nonce = [0u8; FILE_NONCE_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut file_nonce);

    let mut output = Vec::with_capacity(
        HEADER_SIZE + plaintext.len() + ((plaintext.len() / CHUNK_DATA_SIZE) + 1) * CHUNK_TAG_SIZE,
    );
    output.extend_from_slice(RCLONE_MAGIC);
    output.extend_from_slice(&file_nonce);

    for (chunk_num, chunk) in plaintext.chunks(CHUNK_DATA_SIZE).enumerate() {
        let nonce = chunk_nonce(&file_nonce, chunk_num as u64);
        let encrypted = cipher
            .encrypt((&nonce).into(), chunk)
            .map_err(|_| format!("chunk {} encrypt failed", chunk_num))?;
        output.extend_from_slice(&encrypted);
    }

    Ok(output)
}

/// Compute the nonce for a specific chunk by adding chunk_num to the file nonce.
/// Matches rclone's nonce.add(): treats first 8 bytes as LE u64 counter.
fn chunk_nonce(file_nonce: &[u8; FILE_NONCE_SIZE], chunk_num: u64) -> [u8; FILE_NONCE_SIZE] {
    let mut nonce = *file_nonce;
    let base = u64::from_le_bytes(nonce[..8].try_into().unwrap());
    let new_val = base.wrapping_add(chunk_num);
    nonce[..8].copy_from_slice(&new_val.to_le_bytes());
    nonce
}

// ── Phase 2: Filename decryption ───────────────────────────────────────────

/// Decrypt a filename encrypted with rclone's `standard` mode.
///
/// Flow: Base32-decode -> EME-decrypt with name_key + dir_iv -> PKCS#7 unpad -> UTF-8.
pub fn decrypt_name(
    name_key: &[u8; 32],
    dir_iv: &[u8; 16],
    encrypted_name: &str,
) -> Result<String, String> {
    // 1. Base32hex decode (rclone uses lowercase base32hex, no padding;
    //    decode is case-insensitive for legacy uppercase vaults)
    let ciphertext = base32hex_decode(encrypted_name)?;

    if ciphertext.is_empty() || ciphertext.len() % AES_BLOCK != 0 {
        return Err(format!(
            "ciphertext length {} not a multiple of {}",
            ciphertext.len(),
            AES_BLOCK
        ));
    }

    // 2. EME decrypt
    let padded = eme_decrypt(name_key, dir_iv, &ciphertext)?;

    // 3. PKCS#7 unpad
    let plain = pkcs7_unpad(&padded)?;

    // 4. UTF-8
    String::from_utf8(plain).map_err(|e| format!("filename not valid UTF-8: {}", e))
}

/// Encrypt a filename with rclone's `standard` mode.
pub fn encrypt_name(
    name_key: &[u8; 32],
    dir_iv: &[u8; 16],
    plain_name: &str,
) -> Result<String, String> {
    // 1. PKCS#7 pad
    let padded = pkcs7_pad(plain_name.as_bytes());

    // 2. EME encrypt
    let ciphertext = eme_encrypt(name_key, dir_iv, &padded)?;

    // 3. Base32hex encode (lowercase, no padding) to match rclone on the wire
    Ok(base32hex_encode(&ciphertext))
}

// ── EME (ECB-Mix-ECB) wide-block cipher ────────────────────────────────────

/// EME-decrypt: decrypts data that is a multiple of 16 bytes using the
/// EME (Halevi-Rogaway) wide-block cipher with AES-256.
fn eme_decrypt(key: &[u8; 32], tweak: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    eme_transform(key, tweak, data, false)
}

/// EME-encrypt: encrypts data that is a multiple of 16 bytes.
fn eme_encrypt(key: &[u8; 32], tweak: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    eme_transform(key, tweak, data, true)
}

/// Core EME transform (encrypt or decrypt).
/// Ported verbatim from rfjakob/eme (Go), Halevi-Rogaway 2003.
fn eme_transform(
    key: &[u8; 32],
    tweak: &[u8; 16],
    data: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let m = data.len() / AES_BLOCK;
    if m == 0 || !data.len().is_multiple_of(AES_BLOCK) {
        return Err("EME: data must be a non-empty multiple of 16 bytes".into());
    }

    let bc = Aes256::new(key.into());

    // L = E_K(0^128), then build L table: L_table[j] = 2^(j+1) * L
    let mut l_init = [0u8; AES_BLOCK];
    bc.encrypt_block((&mut l_init).into());
    let l_table = tabulate_l(&l_init, m);

    // C is our working buffer (same size as input)
    let mut c = vec![0u8; data.len()];

    // Steps 1-2: PPj = Pj XOR L_table[j], then PPPj = AES(K, PPj) or AES_dec
    let mut ppj = [0u8; AES_BLOCK];
    for j in 0..m {
        xor_into(
            &mut ppj,
            &data[j * AES_BLOCK..(j + 1) * AES_BLOCK],
            &l_table[j],
        );
        let mut block = ppj;
        if encrypt {
            bc.encrypt_block((&mut block).into());
        } else {
            bc.decrypt_block((&mut block).into());
        }
        c[j * AES_BLOCK..(j + 1) * AES_BLOCK].copy_from_slice(&block);
    }

    // Step 3: MP = T XOR PPP[0] XOR PPP[1] XOR ... XOR PPP[m-1]
    let mut mp = [0u8; AES_BLOCK];
    xor_into(&mut mp, &c[0..AES_BLOCK], tweak);
    for j in 1..m {
        xor_mut(&mut mp, &c[j * AES_BLOCK..(j + 1) * AES_BLOCK]);
    }

    // Step 4: MC = AES(K, MP): same direction as overall transform
    let mut mc = mp;
    if encrypt {
        bc.encrypt_block((&mut mc).into());
    } else {
        bc.decrypt_block((&mut mc).into());
    }

    // Step 5: M = MP XOR MC
    let mut m_val = [0u8; AES_BLOCK];
    xor_into(&mut m_val, &mp, &mc);

    // Step 6: For j=1..m-1: M = 2*M, CCC[j] = PPP[j] XOR M
    for j in 1..m {
        m_val = gf128_double(&m_val);
        let mut cccj = [0u8; AES_BLOCK];
        xor_into(&mut cccj, &c[j * AES_BLOCK..(j + 1) * AES_BLOCK], &m_val);
        c[j * AES_BLOCK..(j + 1) * AES_BLOCK].copy_from_slice(&cccj);
    }

    // Step 7: CCC[0] = MC XOR T XOR CCC[1] XOR ... XOR CCC[m-1]
    let mut ccc0 = [0u8; AES_BLOCK];
    xor_into(&mut ccc0, &mc, tweak);
    for j in 1..m {
        xor_mut(&mut ccc0, &c[j * AES_BLOCK..(j + 1) * AES_BLOCK]);
    }
    c[0..AES_BLOCK].copy_from_slice(&ccc0);

    // Step 8: For j=0..m-1: CC[j] = AES(K, CCC[j]), C[j] = CC[j] XOR L_table[j]
    for j in 0..m {
        let mut block = [0u8; AES_BLOCK];
        block.copy_from_slice(&c[j * AES_BLOCK..(j + 1) * AES_BLOCK]);
        if encrypt {
            bc.encrypt_block((&mut block).into());
        } else {
            bc.decrypt_block((&mut block).into());
        }
        xor_mut(&mut block, &l_table[j]);
        c[j * AES_BLOCK..(j + 1) * AES_BLOCK].copy_from_slice(&block);
    }

    Ok(c)
}

/// Build a table of L * 2^i for i = 1..n in GF(2^128).
fn tabulate_l(l: &[u8; AES_BLOCK], n: usize) -> Vec<[u8; AES_BLOCK]> {
    let mut table = Vec::with_capacity(n);
    let mut current = *l;
    for _ in 0..n {
        current = gf128_double(&current);
        table.push(current);
    }
    table
}

/// Multiply by 2 in GF(2^128) using the EME/rfjakob convention:
/// byte 0 = least significant, byte 15 = most significant.
/// Reduction polynomial: x^128 + x^7 + x^2 + x + 1 (0x87 into byte 0).
fn gf128_double(val: &[u8; AES_BLOCK]) -> [u8; AES_BLOCK] {
    let mut result = [0u8; AES_BLOCK];
    // Byte 0: shift left, then conditionally XOR reduction if byte 15 MSB was set
    result[0] = val[0] << 1;
    if val[AES_BLOCK - 1] & 0x80 != 0 {
        result[0] ^= 0x87;
    }
    // Bytes 1..15: shift left with carry from previous byte's MSB
    for j in 1..AES_BLOCK {
        result[j] = (val[j] << 1) | (val[j - 1] >> 7);
    }
    result
}

/// XOR: out = a XOR b (slice version, both must be AES_BLOCK length).
fn xor_into(out: &mut [u8; AES_BLOCK], a: &[u8], b: &[u8; AES_BLOCK]) {
    for i in 0..AES_BLOCK {
        out[i] = a[i] ^ b[i];
    }
}

/// XOR: a ^= b (in-place, slice version).
fn xor_mut(a: &mut [u8; AES_BLOCK], b: &[u8]) {
    for i in 0..AES_BLOCK {
        a[i] ^= b[i];
    }
}

// ── PKCS#7 padding ─────────────────────────────────────────────────────────

fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let pad_len = AES_BLOCK - (data.len() % AES_BLOCK);
    let mut padded = Vec::with_capacity(data.len() + pad_len);
    padded.extend_from_slice(data);
    padded.resize(data.len() + pad_len, pad_len as u8);
    padded
}

fn pkcs7_unpad(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Err("pkcs7: empty data".into());
    }
    let pad_byte = *data.last().unwrap();
    if pad_byte == 0 || pad_byte as usize > AES_BLOCK || pad_byte as usize > data.len() {
        return Err(format!("pkcs7: invalid padding byte {}", pad_byte));
    }
    // Verify all padding bytes
    for &b in &data[data.len() - pad_byte as usize..] {
        if b != pad_byte {
            return Err("pkcs7: inconsistent padding".into());
        }
    }
    Ok(data[..data.len() - pad_byte as usize].to_vec())
}

// ── Base32hex encoding (rclone-compatible) ─────────────────────────────────

/// Base32hex encode (lowercase, no padding): matches rclone's filename encoding.
///
/// rclone emits lowercase base32hex names. On case-sensitive backends (S3 and
/// most cloud object stores) any case difference makes rclone's forward-derived
/// lookup name miss the object, breaking `sync`/`ls`/dedupe interop. We emit
/// lowercase to stay byte-identical with rclone.
fn base32hex_encode(data: &[u8]) -> String {
    // data_encoding::BASE32HEX_NOPAD uses the uppercase RFC 4648 base32hex
    // alphabet; lowercase it so the on-wire name matches rclone exactly. The
    // alphabet is ASCII, so to_lowercase() is a 1:1 byte-preserving transform.
    data_encoding::BASE32HEX_NOPAD.encode(data).to_lowercase()
}

/// Base32hex decode (case-insensitive, no padding).
fn base32hex_decode(s: &str) -> Result<Vec<u8>, String> {
    // rclone emits lowercase; older AeroFTP vaults emitted uppercase. Accept
    // both by normalizing to the uppercase alphabet data_encoding expects.
    let upper = s.to_uppercase();
    data_encoding::BASE32HEX_NOPAD
        .decode(upper.as_bytes())
        .map_err(|e| format!("base32hex decode failed: {}", e))
}

// ── Obfuscate filename encryption (rclone-compatible) ───────────────────────
//
// Rclone's `obfuscate` mode uses character-rotation within Unicode "buckets".
// The rotation amount is derived from the directory IV via the first 4 bytes
// (treated as a u32). Each character is rotated within its own bucket (ASCII
// upper, ASCII lower, ASCII digits, plus a small set of Latin-1 / Latin
// extended ranges); characters outside any bucket pass through unchanged.
// The output is prefixed by `<rotation>.` to make decoding deterministic.

#[derive(Clone, Copy)]
struct ObfBucket {
    lower: u32,
    upper: u32,
}

const OBF_BUCKETS: &[ObfBucket] = &[
    // ASCII digits
    ObfBucket {
        lower: 0x30,
        upper: 0x39,
    },
    // ASCII uppercase letters
    ObfBucket {
        lower: 0x41,
        upper: 0x5A,
    },
    // ASCII lowercase letters
    ObfBucket {
        lower: 0x61,
        upper: 0x7A,
    },
    // Latin-1 supplement letters (À..ÿ excluding × ÷)
    ObfBucket {
        lower: 0x00C0,
        upper: 0x00D6,
    },
    ObfBucket {
        lower: 0x00D8,
        upper: 0x00F6,
    },
    ObfBucket {
        lower: 0x00F8,
        upper: 0x00FF,
    },
];

fn obf_rotation_from_dir_iv(dir_iv: &[u8; 16]) -> u32 {
    // First 4 bytes of dirIV as little-endian u32 -> rotation amount.
    u32::from_le_bytes([dir_iv[0], dir_iv[1], dir_iv[2], dir_iv[3]])
}

fn obf_rotate_char(ch: char, rotate_by: u32, encrypt: bool) -> char {
    let cp = ch as u32;
    for b in OBF_BUCKETS {
        if cp >= b.lower && cp <= b.upper {
            let size = b.upper - b.lower + 1;
            let off = cp - b.lower;
            let r = rotate_by.rem_euclid(size);
            let new_off = if encrypt {
                (off + r) % size
            } else {
                (off + size - r) % size
            };
            return char::from_u32(b.lower + new_off).unwrap_or(ch);
        }
    }
    ch
}

/// Obfuscate a filename using rclone's `obfuscate` algorithm.
pub fn obfuscate_name(dir_iv: &[u8; 16], plain_name: &str) -> Result<String, String> {
    if plain_name.is_empty() {
        return Err("obfuscate: empty filename".into());
    }
    let rotate_by = obf_rotation_from_dir_iv(dir_iv);
    let mut out = format!("{}.", rotate_by);
    for ch in plain_name.chars() {
        out.push(obf_rotate_char(ch, rotate_by, true));
    }
    Ok(out)
}

/// Deobfuscate a filename produced by `obfuscate_name`. The dir_iv must match
/// the directory the entry was placed in.
pub fn deobfuscate_name(dir_iv: &[u8; 16], obf_name: &str) -> Result<String, String> {
    let dot = obf_name
        .find('.')
        .ok_or_else(|| "obfuscate: missing rotation prefix".to_string())?;
    let prefix = &obf_name[..dot];
    let rotate_by_recorded: u32 = prefix
        .parse()
        .map_err(|e| format!("obfuscate: invalid rotation prefix {:?}: {}", prefix, e))?;
    let rotate_by_expected = obf_rotation_from_dir_iv(dir_iv);
    if rotate_by_recorded != rotate_by_expected {
        return Err(format!(
            "obfuscate: rotation mismatch (file={}, expected={})",
            rotate_by_recorded, rotate_by_expected
        ));
    }
    let body = &obf_name[dot + 1..];
    let mut out = String::with_capacity(body.len());
    for ch in body.chars() {
        out.push(obf_rotate_char(ch, rotate_by_expected, false));
    }
    Ok(out)
}

/// Decrypt one already-split filename segment from unlocked keys.
pub fn decrypt_one_name(keys: &RcloneCryptKeys, encrypted_name: &str) -> Option<String> {
    match keys.filename_encryption {
        FilenameEncryption::Off => {
            if keys.off_suffix.is_empty() {
                Some(encrypted_name.to_string())
            } else {
                encrypted_name
                    .strip_suffix(keys.off_suffix.as_str())
                    .map(|name| name.to_string())
            }
        }
        FilenameEncryption::Obfuscate => deobfuscate_name(&keys.name_tweak, encrypted_name).ok(),
        FilenameEncryption::Standard => {
            decrypt_name(&keys.name_key, &keys.name_tweak, encrypted_name).ok()
        }
    }
}

// ── Tauri state and commands (Phase 3) ─────────────────────────────────────

use std::collections::HashMap;
use tokio::sync::Mutex;

/// Info returned after unlock.
#[derive(Debug, Clone, Serialize)]
pub struct RcloneCryptVaultInfo {
    pub vault_id: String,
    pub filename_encryption: FilenameEncryption,
    pub off_suffix: String,
    pub directory_name_encryption: bool,
}

/// Managed state holding all unlocked rclone crypt remotes.
pub struct RcloneCryptState {
    pub vaults: Mutex<HashMap<String, RcloneCryptKeys>>,
}

impl RcloneCryptState {
    pub fn new() -> Self {
        Self {
            vaults: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for RcloneCryptState {
    fn default() -> Self {
        Self::new()
    }
}

/// Unlock an rclone crypt remote by deriving keys from password (and optional salt).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn rclone_crypt_unlock(
    state: tauri::State<'_, RcloneCryptState>,
    password: String,
    salt: Option<String>,
    filename_encryption: Option<String>,
    suffix: Option<String>,
    directory_name_encryption: Option<bool>,
    password_form: Option<String>,
    salt_form: Option<String>,
) -> Result<RcloneCryptVaultInfo, String> {
    let secret_pwd = secrecy::SecretString::from(password);
    let salt_str = salt.unwrap_or_default();

    let (name_key, data_key, name_tweak) = derive_keys_with_forms(
        secrecy::ExposeSecret::expose_secret(&secret_pwd),
        CryptSecretForm::parse(password_form.as_deref()),
        &salt_str,
        CryptSecretForm::parse(salt_form.as_deref()),
    )?;

    let fe = match filename_encryption.as_deref() {
        Some("off") => FilenameEncryption::Off,
        Some("obfuscate") => FilenameEncryption::Obfuscate,
        _ => FilenameEncryption::Standard,
    };
    let off_suffix = resolve_off_suffix(suffix.as_deref());
    let dne = directory_name_encryption.unwrap_or(true);

    let vault_id = uuid::Uuid::new_v4().to_string();
    let keys = RcloneCryptKeys {
        name_key,
        data_key,
        name_tweak,
        filename_encryption: fe,
        off_suffix: off_suffix.clone(),
        directory_name_encryption: dne,
    };

    let info = RcloneCryptVaultInfo {
        vault_id: vault_id.clone(),
        filename_encryption: fe,
        off_suffix,
        directory_name_encryption: dne,
    };

    state.vaults.lock().await.insert(vault_id, keys);
    Ok(info)
}

/// Lock (forget) an unlocked rclone crypt remote, zeroizing keys.
#[tauri::command]
pub async fn rclone_crypt_lock(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
) -> Result<(), String> {
    let mut vaults = state.vaults.lock().await;
    if vaults.remove(&vault_id).is_none() {
        return Err("Vault not found or already locked".to_string());
    }
    // Keys are zeroized via Drop impl
    Ok(())
}

/// Decrypt a single filename using the unlocked keys and a directory IV.
#[tauri::command]
pub async fn rclone_crypt_decrypt_name(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    _dir_iv_base64: String,
    encrypted_name: String,
) -> Result<String, String> {
    let vaults = state.vaults.lock().await;
    let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    if keys.filename_encryption == FilenameEncryption::Off {
        let name = if keys.off_suffix.is_empty() {
            encrypted_name
        } else {
            encrypted_name
                .strip_suffix(keys.off_suffix.as_str())
                .unwrap_or(&encrypted_name)
                .to_string()
        };
        return Ok(name);
    }

    if keys.filename_encryption == FilenameEncryption::Obfuscate {
        return deobfuscate_name(&keys.name_tweak, &encrypted_name);
    }
    decrypt_name(&keys.name_key, &keys.name_tweak, &encrypted_name)
}

/// Decrypt file from a local encrypted file path to a local decrypted output path.
#[tauri::command]
pub async fn rclone_crypt_decrypt_file_path(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    encrypted_file_path: String,
    output_path: String,
) -> Result<String, String> {
    crate::filesystem::validate_path(&encrypted_file_path)?;

    let encrypted_meta = std::fs::symlink_metadata(Path::new(&encrypted_file_path))
        .map_err(|e| format!("failed to inspect encrypted input file: {}", e))?;
    if encrypted_meta.file_type().is_symlink() {
        return Err("Encrypted input path cannot be a symlink".to_string());
    }
    if !encrypted_meta.is_file() {
        return Err("Encrypted input path must be a regular file".to_string());
    }
    if encrypted_meta.len() > MAX_DECRYPT_INPUT_BYTES as u64 {
        return Err(format!(
            "encrypted input too large for MVP decrypt path ({} bytes > {} bytes)",
            encrypted_meta.len(),
            MAX_DECRYPT_INPUT_BYTES
        ));
    }

    let vaults = state.vaults.lock().await;
    let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    let encrypted_data = std::fs::read(&encrypted_file_path)
        .map_err(|e| format!("failed to read encrypted file: {}", e))?;

    let plaintext = decrypt_file_content(&encrypted_data, &keys.data_key)?;
    let guard = OutputPathGuard::new(&output_path)?;
    guard.write_all(&plaintext)
}

/// Helper for cryptcheck: stream-decrypts a remote file and computes its hash (e.g. MD5 or SHA-256).
pub fn decrypt_and_hash<H: sha2::digest::Digest>(
    blob: &[u8],
    data_key: &[u8; 32],
) -> Result<(sha2::digest::Output<H>, u64), String> {
    if blob.len() < 32 {
        return Err("Blob too short (missing header/nonce)".to_string());
    }
    if &blob[..8] != b"RCLONE\x00\x00" {
        return Err("Invalid Rclone crypt header".to_string());
    }
    let mut file_nonce = [0u8; 24];
    file_nonce.copy_from_slice(&blob[8..32]);

    let cipher = XSalsa20Poly1305::new(data_key.into());
    let mut hasher = H::new();
    let mut offset = 32;
    let mut chunk_num = 0u64;
    let mut total_len = 0u64;

    while offset < blob.len() {
        let chunk_size = std::cmp::min(blob.len() - offset, CHUNK_CIPHER_SIZE);
        let chunk = &blob[offset..offset + chunk_size];
        let nonce = chunk_nonce(&file_nonce, chunk_num);
        let plain = cipher
            .decrypt((&nonce).into(), chunk)
            .map_err(|_| format!("chunk {} decrypt failed", chunk_num))?;
        hasher.update(&plain);
        total_len += plain.len() as u64;
        offset += chunk_size;
        chunk_num += 1;
    }

    Ok((hasher.finalize(), total_len))
}

/// Helper for cryptcheck: async stream-decrypts a remote reader and computes its hash.
pub async fn decrypt_and_hash_async<R: tokio::io::AsyncRead + Unpin, H: sha2::digest::Digest>(
    mut reader: R,
    data_key: &[u8; 32],
) -> Result<(sha2::digest::Output<H>, u64), String> {
    use tokio::io::AsyncReadExt;
    let mut header = [0u8; 8];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|e| e.to_string())?;
    if &header != b"RCLONE\x00\x00" {
        return Err("Invalid Rclone crypt header".to_string());
    }

    let mut file_nonce = [0u8; 24];
    reader
        .read_exact(&mut file_nonce)
        .await
        .map_err(|e| e.to_string())?;

    let cipher = XSalsa20Poly1305::new(data_key.into());
    let mut hasher = H::new();
    let mut chunk_num = 0u64;
    let mut total_len = 0u64;

    loop {
        let mut chunk_buf = vec![0u8; CHUNK_CIPHER_SIZE];
        let mut chunk_len = 0;
        while chunk_len < CHUNK_CIPHER_SIZE {
            let n = reader
                .read(&mut chunk_buf[chunk_len..])
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            chunk_len += n;
        }
        if chunk_len == 0 {
            break;
        }

        let nonce = chunk_nonce(&file_nonce, chunk_num);
        let plain = cipher
            .decrypt((&nonce).into(), &chunk_buf[..chunk_len])
            .map_err(|_| format!("chunk {} decrypt failed", chunk_num))?;

        hasher.update(&plain);
        total_len += plain.len() as u64;
        chunk_num += 1;

        if chunk_len < CHUNK_CIPHER_SIZE {
            break;
        }
    }

    Ok((hasher.finalize(), total_len))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    // Golden vectors copied from rclone/backend/crypt/cipher_test.go
    // (TestEncryptData + TestStandardEncryptFileNameBase32).
    const RCLONE_GOLDEN_FILE0: &[u8] = &[
        0x52, 0x43, 0x4c, 0x4f, 0x4e, 0x45, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18,
    ];
    const RCLONE_GOLDEN_FILE1: &[u8] = &[
        0x52, 0x43, 0x4c, 0x4f, 0x4e, 0x45, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x09, 0x5b, 0x44, 0x6c, 0xd6, 0x23, 0x7b, 0xbc, 0xb0, 0x8d, 0x09, 0xfb, 0x52,
        0x4c, 0xe5, 0x65, 0xaa,
    ];
    const RCLONE_GOLDEN_FILE16: &[u8] = &[
        0x52, 0x43, 0x4c, 0x4f, 0x4e, 0x45, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0xb9, 0xc4, 0x55, 0x2a, 0x27, 0x10, 0x06, 0x29, 0x18, 0x96, 0x0a, 0x3e, 0x60,
        0x8c, 0x29, 0xb9, 0xaa, 0x8a, 0x5e, 0x1e, 0x16, 0x5b, 0x6d, 0x07, 0x5d, 0xe4, 0xe9, 0xbb,
        0x36, 0x7f, 0xd6, 0xd4,
    ];

    // ── Phase 1 tests ──

    #[test]
    fn derive_keys_produces_64_bytes() {
        let (name_key, data_key) = derive_keys("testpassword", "testsalt").unwrap();
        // Keys should be 32 bytes each and non-zero
        assert_ne!(name_key, [0u8; 32]);
        assert_ne!(data_key, [0u8; 32]);
        // Different inputs should produce different keys
        let (name_key2, data_key2) = derive_keys("other", "salt2").unwrap();
        assert_ne!(name_key, name_key2);
        assert_ne!(data_key, data_key2);
    }

    #[test]
    fn derive_keys_deterministic() {
        let (nk1, dk1) = derive_keys("password", "salt").unwrap();
        let (nk2, dk2) = derive_keys("password", "salt").unwrap();
        assert_eq!(nk1, nk2);
        assert_eq!(dk1, dk2);
    }

    #[test]
    fn derive_keys_empty_salt() {
        // rclone allows empty password2 (salt)
        let (nk, dk) = derive_keys("password", "").unwrap();
        assert_ne!(nk, [0u8; 32]);
        assert_ne!(dk, [0u8; 32]);
    }

    #[test]
    fn chunk_nonce_zero() {
        let file_nonce = [0x42u8; FILE_NONCE_SIZE];
        let nonce = chunk_nonce(&file_nonce, 0);
        assert_eq!(nonce, file_nonce); // no change for chunk 0
    }

    #[test]
    fn chunk_nonce_increment() {
        let mut file_nonce = [0u8; FILE_NONCE_SIZE];
        file_nonce[0] = 0x10;
        file_nonce[8] = 0xFF; // upper bytes stay unchanged

        let nonce = chunk_nonce(&file_nonce, 1);
        assert_eq!(nonce[0], 0x11); // 0x10 + 1
        assert_eq!(nonce[8], 0xFF); // upper half unchanged
    }

    #[test]
    fn chunk_nonce_wrapping() {
        let mut file_nonce = [0u8; FILE_NONCE_SIZE];
        file_nonce[0] = 0xFF;
        file_nonce[1] = 0x00;

        let nonce = chunk_nonce(&file_nonce, 1);
        assert_eq!(nonce[0], 0x00); // 0xFF + 1 = 0x100, wraps
        assert_eq!(nonce[1], 0x01); // carry
    }

    #[test]
    fn reject_short_header() {
        let data = b"RCLONE"; // too short
        let key = [0u8; 32];
        assert!(decrypt_file_content(data, &key).is_err());
    }

    #[test]
    fn reject_bad_magic() {
        let mut data = [0u8; HEADER_SIZE + 17]; // header + min chunk
        data[..6].copy_from_slice(b"BADMAG");
        let key = [0u8; 32];
        assert!(decrypt_file_content(&data, &key).is_err());
    }

    #[test]
    fn empty_file_decrypts_to_empty() {
        // Valid rclone crypt file with zero content (header only)
        let mut data = [0u8; HEADER_SIZE];
        data[..8].copy_from_slice(RCLONE_MAGIC);
        let key = [0u8; 32];
        let result = decrypt_file_content(&data, &key).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn encrypt_empty_file_roundtrip() {
        let (_, data_key) = derive_keys("empty", "").unwrap();
        let encrypted = encrypt_file_content(&[], &data_key).unwrap();
        assert_eq!(&encrypted[..8], RCLONE_MAGIC);
        assert_eq!(encrypted.len(), HEADER_SIZE);

        let decrypted = decrypt_file_content(&encrypted, &data_key).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn golden_rclone_decrypt_file_vectors() {
        let key = [0u8; 32];

        let out0 = decrypt_file_content(RCLONE_GOLDEN_FILE0, &key).unwrap();
        assert!(out0.is_empty());

        let out1 = decrypt_file_content(RCLONE_GOLDEN_FILE1, &key).unwrap();
        assert_eq!(out1, vec![0x01]);

        let out16 = decrypt_file_content(RCLONE_GOLDEN_FILE16, &key).unwrap();
        assert_eq!(out16, (1u8..=16u8).collect::<Vec<u8>>());
    }

    #[test]
    fn end_to_end_single_chunk() {
        // Derive keys, encrypt a small payload manually, then decrypt
        let (_, data_key) = derive_keys("test", "").unwrap();
        let cipher = XSalsa20Poly1305::new((&data_key).into());

        let plaintext = b"Hello, rclone crypt!";
        let file_nonce = [0xABu8; FILE_NONCE_SIZE];
        let nonce0 = chunk_nonce(&file_nonce, 0);
        let encrypted_chunk = cipher
            .encrypt((&nonce0).into(), plaintext.as_ref())
            .unwrap();

        // Build file: magic + nonce + chunk
        let mut file_data = Vec::new();
        file_data.extend_from_slice(RCLONE_MAGIC);
        file_data.extend_from_slice(&file_nonce);
        file_data.extend_from_slice(&encrypted_chunk);

        let decrypted = decrypt_file_content(&file_data, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn end_to_end_multi_chunk() {
        let (_, data_key) = derive_keys("multipass", "salty").unwrap();
        let cipher = XSalsa20Poly1305::new((&data_key).into());
        let file_nonce = [0x01u8; FILE_NONCE_SIZE];

        // Create plaintext larger than one chunk
        let plaintext: Vec<u8> = (0..CHUNK_DATA_SIZE + 100)
            .map(|i| (i % 256) as u8)
            .collect();

        let mut file_data = Vec::new();
        file_data.extend_from_slice(RCLONE_MAGIC);
        file_data.extend_from_slice(&file_nonce);

        // Chunk 0: full 64KB
        let nonce0 = chunk_nonce(&file_nonce, 0);
        let enc0 = cipher
            .encrypt((&nonce0).into(), &plaintext[..CHUNK_DATA_SIZE])
            .unwrap();
        file_data.extend_from_slice(&enc0);

        // Chunk 1: remaining 100 bytes
        let nonce1 = chunk_nonce(&file_nonce, 1);
        let enc1 = cipher
            .encrypt((&nonce1).into(), &plaintext[CHUNK_DATA_SIZE..])
            .unwrap();
        file_data.extend_from_slice(&enc1);

        let decrypted = decrypt_file_content(&file_data, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_file_content_roundtrip_single_chunk() {
        let (_, data_key) = derive_keys("write-single", "salt").unwrap();
        let plaintext = b"rclone crypt write path";

        let encrypted = encrypt_file_content(plaintext, &data_key).unwrap();
        assert_eq!(&encrypted[..8], RCLONE_MAGIC);
        assert!(encrypted.len() > HEADER_SIZE);

        let decrypted = decrypt_file_content(&encrypted, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_file_content_roundtrip_multi_chunk() {
        let (_, data_key) = derive_keys("write-multi", "salt").unwrap();
        let plaintext: Vec<u8> = (0..(CHUNK_DATA_SIZE * 2 + 257))
            .map(|i| (i % 251) as u8)
            .collect();

        let encrypted = encrypt_file_content(&plaintext, &data_key).unwrap();
        assert_eq!(&encrypted[..8], RCLONE_MAGIC);
        assert!(encrypted.len() > plaintext.len());

        let decrypted = decrypt_file_content(&encrypted, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_file_content_uses_fresh_nonce() {
        let (_, data_key) = derive_keys("nonce", "salt").unwrap();
        let plaintext = b"same plaintext";

        let encrypted_a = encrypt_file_content(plaintext, &data_key).unwrap();
        let encrypted_b = encrypt_file_content(plaintext, &data_key).unwrap();

        assert_ne!(&encrypted_a[8..HEADER_SIZE], &encrypted_b[8..HEADER_SIZE]);
        assert_ne!(encrypted_a, encrypted_b);
        assert_eq!(
            decrypt_file_content(&encrypted_a, &data_key).unwrap(),
            plaintext
        );
        assert_eq!(
            decrypt_file_content(&encrypted_b, &data_key).unwrap(),
            plaintext
        );
    }

    // ── Phase 2 tests ──

    #[test]
    fn pkcs7_pad_unpad_roundtrip() {
        let data = b"test";
        let padded = pkcs7_pad(data);
        assert_eq!(padded.len(), 16); // padded to block size
        assert_eq!(padded[4..], [12u8; 12]); // 12 bytes of padding
        let unpadded = pkcs7_unpad(&padded).unwrap();
        assert_eq!(unpadded, data);
    }

    #[test]
    fn pkcs7_pad_exact_block() {
        let data = [0u8; 16]; // exactly one block
        let padded = pkcs7_pad(&data);
        assert_eq!(padded.len(), 32); // adds full block of padding
        let unpadded = pkcs7_unpad(&padded).unwrap();
        assert_eq!(unpadded, data);
    }

    #[test]
    fn pkcs7_unpad_invalid() {
        assert!(pkcs7_unpad(&[]).is_err());
        assert!(pkcs7_unpad(&[0]).is_err()); // pad byte 0 invalid
        assert!(pkcs7_unpad(&[5, 5, 5, 3]).is_err()); // inconsistent
    }

    #[test]
    fn base32hex_roundtrip() {
        let data = b"test filename";
        let encoded = base32hex_encode(data);
        let decoded = base32hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base32hex_case_insensitive() {
        let data = b"hello";
        let encoded = base32hex_encode(data);
        let decoded_lower = base32hex_decode(&encoded.to_lowercase()).unwrap();
        assert_eq!(decoded_lower, data);
    }

    #[test]
    fn gf128_double_zero() {
        let zero = [0u8; AES_BLOCK];
        let doubled = gf128_double(&zero);
        assert_eq!(doubled, zero);
    }

    #[test]
    fn gf128_double_one() {
        let mut one = [0u8; AES_BLOCK];
        one[0] = 0x01;
        let doubled = gf128_double(&one);
        assert_eq!(doubled[0], 0x02);
    }

    #[test]
    fn eme_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let tweak = [0x01u8; 16];
        let plaintext = [0xABu8; 32]; // 2 blocks

        let encrypted = eme_encrypt(&key, &tweak, &plaintext).unwrap();
        assert_ne!(encrypted, plaintext.to_vec());

        let decrypted = eme_decrypt(&key, &tweak, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext.to_vec());
    }

    #[test]
    fn eme_different_tweaks() {
        let key = [0x42u8; 32];
        let tweak1 = [0x01u8; 16];
        let tweak2 = [0x02u8; 16];
        let plaintext = [0xABu8; 16]; // 1 block

        let enc1 = eme_encrypt(&key, &tweak1, &plaintext).unwrap();
        let enc2 = eme_encrypt(&key, &tweak2, &plaintext).unwrap();
        assert_ne!(enc1, enc2); // different tweaks produce different output
    }

    #[test]
    fn name_encrypt_decrypt_roundtrip() {
        let (name_key, _) = derive_keys("nametest", "").unwrap();
        let dir_iv = [0x55u8; 16];
        let name = "my-document.txt";

        let encrypted = encrypt_name(&name_key, &dir_iv, name).unwrap();
        let decrypted = decrypt_name(&name_key, &dir_iv, &encrypted).unwrap();
        assert_eq!(decrypted, name);
    }

    #[test]
    fn name_encrypt_decrypt_unicode() {
        let (name_key, _) = derive_keys("unicode", "salt").unwrap();
        let dir_iv = [0xAAu8; 16];
        let name = "foto_2026_è.txt";

        let encrypted = encrypt_name(&name_key, &dir_iv, name).unwrap();
        let decrypted = decrypt_name(&name_key, &dir_iv, &encrypted).unwrap();
        assert_eq!(decrypted, name);
    }

    #[test]
    fn golden_rclone_filename_standard_vectors() {
        let name_key = [0u8; 32];
        let dir_iv = [0u8; 16];

        let encrypted_1 = encrypt_name(&name_key, &dir_iv, "1").unwrap();
        assert_eq!(encrypted_1.to_lowercase(), "p0e52nreeaj0a5ea7s64m4j72s");
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, "p0e52nreeaj0a5ea7s64m4j72s").unwrap(),
            "1"
        );

        let encrypted_12 = encrypt_name(&name_key, &dir_iv, "12").unwrap();
        assert_eq!(encrypted_12.to_lowercase(), "l42g6771hnv3an9cgc8cr2n1ng");
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, "l42g6771hnv3an9cgc8cr2n1ng").unwrap(),
            "12"
        );

        let encrypted_123 = encrypt_name(&name_key, &dir_iv, "123").unwrap();
        assert_eq!(encrypted_123.to_lowercase(), "qgm4avr35m5loi1th53ato71v0");
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, "qgm4avr35m5loi1th53ato71v0").unwrap(),
            "123"
        );
    }

    #[test]
    fn standard_filename_is_lowercase_no_masking() {
        // F2 regression: rclone emits lowercase base32hex. Assert the raw
        // output is byte-identical to rclone WITHOUT a masking to_lowercase(),
        // so case-sensitive backends (S3) resolve the same key.
        let name_key = [0u8; 32];
        let dir_iv = [0u8; 16];

        let e = encrypt_name(&name_key, &dir_iv, "1").unwrap();
        assert_eq!(e, "p0e52nreeaj0a5ea7s64m4j72s");
        assert!(
            !e.chars().any(|c| c.is_ascii_uppercase()),
            "encrypted name must not contain uppercase: {e}"
        );
        // Legacy uppercase vaults must still decode (case-insensitive).
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, &e.to_uppercase()).unwrap(),
            "1"
        );
    }

    #[test]
    fn off_suffix_resolution() {
        // F3: default suffix is rclone's `.bin`; `none`/empty disables it.
        assert_eq!(resolve_off_suffix(None), ".bin");
        assert_eq!(resolve_off_suffix(Some(".bin")), ".bin");
        assert_eq!(resolve_off_suffix(Some("none")), "");
        assert_eq!(resolve_off_suffix(Some("NONE")), "");
        assert_eq!(resolve_off_suffix(Some("")), "");
        assert_eq!(resolve_off_suffix(Some(".enc")), ".enc");
    }

    #[test]
    fn golden_rclone_174_real_standard_filenames() {
        // Generated with rclone v1.74.0:
        //   password = test-password-179
        //   password2 = test-salt-179
        //   filename_encryption = standard
        //   directory_name_encryption = true
        //   rclone backend encode crypt: docs report.txt docs/report.txt
        let (name_key, _, name_tweak) =
            derive_keys_with_tweak("test-password-179", "test-salt-179").unwrap();

        assert_eq!(
            encrypt_name(&name_key, &name_tweak, "docs")
                .unwrap()
                .to_lowercase(),
            "d5qf5g718ha9us9plevsksujks"
        );
        assert_eq!(
            decrypt_name(&name_key, &name_tweak, "d5qf5g718ha9us9plevsksujks").unwrap(),
            "docs"
        );

        assert_eq!(
            encrypt_name(&name_key, &name_tweak, "report.txt")
                .unwrap()
                .to_lowercase(),
            "flgaonufs8d1utd0ev8qhjikgg"
        );
        assert_eq!(
            decrypt_name(&name_key, &name_tweak, "flgaonufs8d1utd0ev8qhjikgg").unwrap(),
            "report.txt"
        );

        let encoded_path = format!(
            "{}/{}",
            encrypt_name(&name_key, &name_tweak, "docs")
                .unwrap()
                .to_lowercase(),
            encrypt_name(&name_key, &name_tweak, "report.txt")
                .unwrap()
                .to_lowercase()
        );
        assert_eq!(
            encoded_path,
            "d5qf5g718ha9us9plevsksujks/flgaonufs8d1utd0ev8qhjikgg"
        );
    }

    #[test]
    fn golden_rclone_174_standard_filenames_with_omitted_password2() {
        // Generated with rclone v1.74.3 and an omitted/empty password2. This is
        // the exact #600 setup: rclone substitutes its public default salt;
        // AeroFTP must do the same or every name fails with PKCS#7 padding.
        let (name_key, _, name_tweak) = derive_keys_with_tweak("triage-password-600", "").unwrap();
        assert_eq!(
            encrypt_name(&name_key, &name_tweak, "folder").unwrap(),
            "785v69hnpanb9p84bhrlki9lp0"
        );
        assert_eq!(
            encrypt_name(&name_key, &name_tweak, "file.txt").unwrap(),
            "h5p2oibs3erqnaspobsargglqs"
        );
    }

    #[test]
    fn derive_keys_accepts_rclone_obscured_password_and_salt() {
        // rclone.conf stores password / password2 already obscured. Pasting
        // those into AeroFTP used to scrypt the ciphertext (#600).
        let obscured_pw = "LZ9RxVK9L7SryViTF1LcFaIhT4Pe_wQkOD3Gud9FnQ";
        let from_obscured = derive_keys_with_tweak(obscured_pw, "").unwrap();
        let from_plain = derive_keys_with_tweak("testpassword123", "").unwrap();
        assert_eq!(from_obscured.0, from_plain.0);
        assert_eq!(from_obscured.1, from_plain.1);
        assert_eq!(from_obscured.2, from_plain.2);
    }

    /// A 22-character password drawn from `[A-Za-z0-9_-]` decodes to exactly
    /// 16 bytes under base64url, which is the IV and nothing else, so
    /// rclone-reveal returns the EMPTY STRING for it. Before this was decided
    /// per field, such a password was silently replaced by nothing and the
    /// content was encrypted under a key derived from an empty password: no
    /// error, no sign, and unopenable by rclone. Password managers generate
    /// inside that alphabet, so the input is ordinary rather than exotic.
    ///
    /// The assertion is that the 22-character password behaves like a
    /// password and NOT like the empty one, which is stronger than asserting
    /// its literal survival: it would still fail if the value were replaced by
    /// any other constant.
    #[test]
    fn a_22_char_password_is_not_swallowed_by_the_reveal() {
        use CryptSecretForm::Clear;
        let colliding = "Aa0-_Bb1cDd2eEf3gHh4iA";
        assert_eq!(colliding.len(), 22, "the collision needs exactly 22 chars");
        assert_eq!(
            crate::rclone_import::reveal_obscured(colliding).as_deref(),
            Ok(""),
            "precondition: this input really does reveal as empty"
        );

        // As typed: the key of THAT password, neither the empty one nor any
        // other constant.
        let from_literal = derive_keys_with_forms(colliding, Some(Clear), "", None).unwrap();
        let from_empty = derive_keys_from_clear("", "").unwrap();
        assert_ne!(from_literal.0, from_empty.0);
        let control =
            derive_keys_with_forms("Aa0-_Bb1cDd2eEf3gHh4iQ", Some(Clear), "", None).unwrap();
        assert_ne!(from_literal.0, control.0);

        // Unrecorded, the guess always kept this one as typed: still does.
        assert_eq!(
            derive_keys_with_tweak(colliding, "").unwrap().0,
            from_literal.0
        );
        // Marked obscured, it would be a key from nothing: refused.
        let e = resolve_crypt_password(colliding, Some(CryptSecretForm::Obscured)).unwrap_err();
        assert!(e.contains("reveals to 0 character"), "{e}");
    }

    /// The same bytes as a salt: rclone's `obscure("")` (the default salt) or a
    /// salt rclone generated, typed as it is. The recorded form decides; with
    /// none recorded the salt is refused, never read as the default.
    #[test]
    fn a_22_char_salt_is_read_by_its_recorded_form() {
        use CryptSecretForm::{Clear, Obscured};
        let colliding = "Aa0-_Bb1cDd2eEf3gHh4iA";
        let default_salt = derive_keys_with_forms("pw-600", Some(Clear), "", None).unwrap();
        let as_obscured =
            derive_keys_with_forms("pw-600", Some(Clear), colliding, Some(Obscured)).unwrap();
        let as_typed =
            derive_keys_with_forms("pw-600", Some(Clear), colliding, Some(Clear)).unwrap();
        assert_eq!(as_obscured.0, default_salt.0);
        assert_ne!(as_typed.0, default_salt.0);
        let refused = derive_keys_with_tweak("pw-600", colliding)
            .map(|_| ())
            .unwrap_err();
        assert!(refused.contains("salt reads two ways"), "{refused}");
        for way_out in [
            "profile's crypt settings",
            "crypt set-form --profile",
            "--salt-form",
            "AEROFTP_CRYPT_OVERLAY_SALT_FORM",
        ] {
            assert!(
                refused.contains(way_out),
                "the message names {way_out}: {refused}"
            );
        }
    }

    #[test]
    fn obscured_empty_password2_is_the_default_salt() {
        // `rclone config create ... password2=` writes `obscure("")`. Marked
        // as obscured it is the default salt.
        let obscured_empty = crate::rclone_import::obscure_password("").unwrap();
        let with_blob = derive_keys_with_forms(
            "triage-password-600",
            Some(CryptSecretForm::Clear),
            &obscured_empty,
            Some(CryptSecretForm::Obscured),
        )
        .unwrap();
        let omitted = derive_keys_with_tweak("triage-password-600", "").unwrap();
        assert_eq!(with_blob.0, omitted.0);
        assert_eq!(
            encrypt_name(&with_blob.0, &with_blob.2, "folder").unwrap(),
            "785v69hnpanb9p84bhrlki9lp0"
        );
    }

    /// Unrecorded, only a reading the old guess actually used is refused, and
    /// only when it is 2 characters or fewer: a salt that reveals to 0, 1 or 2
    /// characters and a password that reveals to 1 or 2. A password that
    /// reveals to nothing was always kept as typed, and still is. From 3
    /// characters the guess is kept. The 24-character password is a real one
    /// that decodes to 2 characters.
    #[test]
    fn unmarked_secrets_are_refused_only_on_a_short_reading_the_guess_used() {
        let obscure = |s: &str| crate::rclone_import::obscure_password(s).unwrap();
        for plain in ["", "a", "ab", "abc"] {
            let value = obscure(plain);
            assert!(value.len() >= 22, "{value}");
            let password = resolve_crypt_password(&value, None);
            let salt = resolve_crypt_salt(&value, None);
            match plain {
                "" => {
                    assert_eq!(password.as_deref(), Ok(value.as_str()), "kept as typed");
                    assert!(salt.unwrap_err().contains("salt reads two ways"));
                }
                "a" | "ab" => {
                    assert!(password.unwrap_err().contains("password reads two ways"));
                    assert!(salt.unwrap_err().contains("salt reads two ways"));
                }
                _ => {
                    assert_eq!(password.as_deref(), Ok(plain));
                    assert_eq!(salt.as_deref(), Ok(plain));
                }
            }
        }

        let real = "TxJvZRpvYFpHytjG6UP2WcIv";
        let short = crate::rclone_import::reveal_obscured(real).unwrap();
        assert!(short.chars().count() <= 2, "precondition: {short:?}");
        assert!(resolve_crypt_password(real, None).is_err());
        assert_eq!(
            resolve_crypt_password(real, Some(CryptSecretForm::Clear)).as_deref(),
            Ok(real)
        );
        // Below 22 characters nothing changes.
        assert_eq!(
            resolve_crypt_password("short-pass", None).as_deref(),
            Ok("short-pass")
        );
    }

    /// Marked obscured, a password that reveals to 2 characters or fewer is
    /// refused (a key from almost nothing); a salt may reveal to anything,
    /// nothing included (the default salt). An empty value is no secret in
    /// every form, so a command given no salt still runs.
    #[test]
    fn obscured_passwords_that_reveal_short_are_refused_and_empty_values_pass() {
        use CryptSecretForm::{Clear, ImportedUnrecorded, Obscured};
        let obscure = |s: &str| crate::rclone_import::obscure_password(s).unwrap();
        for plain in ["", "a", "ab"] {
            assert!(
                resolve_crypt_password(&obscure(plain), Some(Obscured)).is_err(),
                "{plain:?}"
            );
            assert_eq!(
                resolve_crypt_salt(&obscure(plain), Some(Obscured)).as_deref(),
                Ok(plain)
            );
        }
        assert_eq!(
            resolve_crypt_password(&obscure("abc"), Some(Obscured)).as_deref(),
            Ok("abc")
        );
        for form in [None, Some(Clear), Some(Obscured), Some(ImportedUnrecorded)] {
            assert_eq!(
                resolve_crypt_password("", form).as_deref(),
                Ok(""),
                "{form:?}"
            );
            assert_eq!(resolve_crypt_salt("", form).as_deref(), Ok(""), "{form:?}");
        }
    }

    /// A secret the rclone importer stored before forms were recorded is the
    /// revealed value, so it is used as it is, unless it also reveals to 3 or
    /// more printable characters: then an obscured value may have been pasted
    /// over it, and reading it either way could change the key of a working
    /// overlay, so it is refused.
    #[test]
    fn imported_unrecorded_secrets_are_clear_unless_they_read_two_ways() {
        use CryptSecretForm::ImportedUnrecorded;
        let generated_salt = "hD1lB5uyIChoDFqhaHOsUg";
        assert_eq!(
            resolve_crypt_salt(generated_salt, Some(ImportedUnrecorded)).as_deref(),
            Ok(generated_salt)
        );
        assert_eq!(
            resolve_crypt_password("crypt-pass-954", Some(ImportedUnrecorded)).as_deref(),
            Ok("crypt-pass-954")
        );
        let pasted = crate::rclone_import::obscure_password("crypt-pass-954").unwrap();
        let e = resolve_crypt_password(&pasted, Some(ImportedUnrecorded)).unwrap_err();
        assert!(e.contains("rclone import"), "{e}");
        assert!(e.contains("both the password and the salt"), "{e}");
        assert!(
            e.contains("imported from rclone.conf was stored as typed"),
            "{e}"
        );
    }

    /// Only a value exactly as rclone writes it can be "pasted over" an imported
    /// one: flip a discarded low bit of an obscured value and the lenient
    /// decoder still reads it, but rclone could not have written it, so it
    /// stays the value the import stored.
    #[test]
    fn imported_unrecorded_probe_counts_only_values_rclone_could_write() {
        use CryptSecretForm::ImportedUnrecorded;
        const URL_SAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        // 16-byte IV + 4 bytes = 20 bytes: 27 symbols, the last carrying 2
        // discarded bits.
        let canonical = crate::rclone_import::obscure_password("abcd").unwrap();
        assert_eq!(canonical.len(), 27, "{canonical}");
        let last = URL_SAFE.find(canonical.chars().last().unwrap()).unwrap();
        assert_eq!(last & 0b11, 0, "rclone leaves the discarded bits at zero");
        let lenient_only = format!(
            "{}{}",
            &canonical[..26],
            URL_SAFE.chars().nth(last | 1).unwrap()
        );
        assert_eq!(
            crate::rclone_import::reveal_rclone_password(&lenient_only).as_deref(),
            Ok("abcd"),
            "precondition: the lenient decoder reads it"
        );
        assert_eq!(
            resolve_crypt_password(&lenient_only, Some(ImportedUnrecorded)).as_deref(),
            Ok(lenient_only.as_str())
        );
        assert!(resolve_crypt_password(&canonical, Some(ImportedUnrecorded)).is_err());
    }

    /// Lengths are counted in characters, not bytes: "€" is one character in
    /// three bytes. Obscured, it is a key from almost nothing (refused, recorded
    /// obscured or guessed); imported, a 1-character reading is not a paste.
    #[test]
    fn short_readings_are_counted_in_characters() {
        use CryptSecretForm::{ImportedUnrecorded, Obscured};
        let euro = crate::rclone_import::obscure_password("\u{20ac}").unwrap();
        assert!(euro.len() >= SHORT_READING_MIN_INPUT, "{euro}");
        assert!(resolve_crypt_password(&euro, Some(Obscured)).is_err());
        assert!(resolve_crypt_password(&euro, None).is_err());
        assert!(resolve_crypt_salt(&euro, None).is_err());
        assert_eq!(
            resolve_crypt_password(&euro, Some(ImportedUnrecorded)).as_deref(),
            Ok(euro.as_str())
        );
    }

    /// No reader derives a key from an empty password, whatever the form:
    /// rclone would give the all-zero key.
    #[test]
    fn an_empty_password_derives_no_key() {
        use CryptSecretForm::{Clear, ImportedUnrecorded, Obscured};
        for form in [None, Some(Clear), Some(Obscured), Some(ImportedUnrecorded)] {
            let e = derive_keys_with_forms("", form, "salt", None)
                .map(|_| ())
                .unwrap_err();
            assert!(e.contains("password is required"), "{form:?}: {e}");
        }
        assert!(derive_keys_with_tweak("", "").is_err());
        assert!(derive_keys_with_tweak("pw", "").is_ok());
    }

    /// A secret taken from the environment carries the form its companion
    /// variable states, or none; one from the vault keeps the binding's.
    #[test]
    fn env_secrets_take_the_form_their_variable_states() {
        let var = "AEROFTP_TEST_955_SECRET_FORM";
        std::env::remove_var(var);
        assert_eq!(
            secret_form_for_source(Some(CryptSecretForm::Obscured), true, var),
            None
        );
        std::env::set_var(var, "clear");
        assert_eq!(
            secret_form_for_source(Some(CryptSecretForm::Obscured), true, var),
            Some(CryptSecretForm::Clear)
        );
        assert_eq!(
            secret_form_for_source(Some(CryptSecretForm::Obscured), false, var),
            Some(CryptSecretForm::Obscured)
        );
        std::env::remove_var(var);
    }

    /// A clear secret is used as it is, control characters included; unrecorded,
    /// a value whose reveal has control characters was always kept as typed; an
    /// obscured one that does not reveal is an error, never the literal.
    #[test]
    fn recorded_forms_are_taken_literally() {
        let odd = "pa\tss word ";
        assert_eq!(
            resolve_crypt_password(odd, Some(CryptSecretForm::Clear)).as_deref(),
            Ok(odd)
        );
        assert!(resolve_crypt_password("not-obscured!", Some(CryptSecretForm::Obscured)).is_err());
        assert!(resolve_crypt_salt("abc", Some(CryptSecretForm::Obscured)).is_err());
        let with_control = crate::rclone_import::obscure_password("x\u{1}yz").unwrap();
        assert_eq!(
            resolve_crypt_password(&with_control, None).as_deref(),
            Ok(with_control.as_str())
        );
    }

    #[test]
    fn live_rclone_1743_file_roundtrip_when_env_set() {
        let Ok(workdir) = std::env::var("AEROFTP_600_WORKDIR") else {
            return;
        };
        let rclone_cipher =
            format!("{workdir}/cipher/785v69hnpanb9p84bhrlki9lp0/h5p2oibs3erqnaspobsargglqs");
        let data = std::fs::read(&rclone_cipher)
            .unwrap_or_else(|e| panic!("read rclone ciphertext {rclone_cipher}: {e}"));
        let (name_key, data_key, name_tweak) =
            derive_keys_with_tweak("triage-password-600", "").unwrap();
        let pt = decrypt_file_content(&data, &data_key).expect("AeroFTP decrypt of rclone 1.74.3");
        assert_eq!(pt, b"hello-from-rclone-1743\n");

        let aero_pt = b"hello-from-aeroftp-600\n";
        let aero_ct = encrypt_file_content(aero_pt, &data_key).expect("AeroFTP encrypt");
        let enc_name = encrypt_name(&name_key, &name_tweak, "aero.txt")
            .unwrap()
            .to_lowercase();
        let aero_dir = format!("{workdir}/cipher-aero");
        std::fs::create_dir_all(&aero_dir).unwrap();
        std::fs::write(format!("{aero_dir}/{enc_name}"), &aero_ct).unwrap();
        std::fs::write(format!("{workdir}/aero-enc-name"), enc_name.as_bytes()).unwrap();
    }

    #[test]
    fn name_decrypt_rejects_invalid_base32() {
        let key = [0u8; 32];
        let iv = [0u8; 16];
        assert!(decrypt_name(&key, &iv, "!!!invalid!!!").is_err());
    }

    #[test]
    fn decrypt_and_hash_roundtrip_sha256() {
        let (_, data_key) = derive_keys("hash-test", "salt").unwrap();
        let plaintext = b"streaming hash test";
        let encrypted = encrypt_file_content(plaintext, &data_key).unwrap();

        let (hash, len) = decrypt_and_hash::<sha2::Sha256>(&encrypted, &data_key).unwrap();
        let expected_hash = sha2::Sha256::digest(plaintext);

        assert_eq!(hash, expected_hash);
        assert_eq!(len, plaintext.len() as u64);
    }

    #[test]
    fn decrypt_and_hash_empty_file() {
        let (_, data_key) = derive_keys("empty-hash", "").unwrap();
        let encrypted = encrypt_file_content(&[], &data_key).unwrap();

        let (hash, len) = decrypt_and_hash::<sha2::Sha256>(&encrypted, &data_key).unwrap();
        let expected_hash = sha2::Sha256::digest(b"");

        assert_eq!(hash, expected_hash);
        assert_eq!(len, 0);
    }

    #[tokio::test]
    async fn decrypt_and_hash_async_roundtrip() {
        let (_, data_key) = derive_keys("async-hash-test", "salt").unwrap();
        let plaintext: Vec<u8> = (0..(CHUNK_DATA_SIZE * 2 + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        let encrypted = encrypt_file_content(&plaintext, &data_key).unwrap();

        let cursor = std::io::Cursor::new(encrypted);
        let (hash, len) = decrypt_and_hash_async::<_, sha2::Sha256>(cursor, &data_key)
            .await
            .unwrap();
        let expected_hash = sha2::Sha256::digest(&plaintext);

        assert_eq!(hash, expected_hash);
        assert_eq!(len, plaintext.len() as u64);
    }

    // ── Obfuscate filename encryption tests ─────────────────────────────────

    #[test]
    fn obfuscate_roundtrip_ascii() {
        let dir_iv = [0xA5u8; 16];
        for plain in [
            "report.txt",
            "Photos2026",
            "Mixed_CASE-123.zip",
            "a",
            "ALLCAPS",
            "alllower",
            "0123456789",
        ] {
            let obf = obfuscate_name(&dir_iv, plain).unwrap();
            assert!(
                obf.contains('.'),
                "obfuscated output must carry rotation prefix"
            );
            let back = deobfuscate_name(&dir_iv, &obf).unwrap();
            assert_eq!(back, plain, "roundtrip mismatch on {:?}", plain);
        }
    }

    #[test]
    fn obfuscate_roundtrip_latin1() {
        let dir_iv = [0x11u8; 16];
        let plain = "café_éà_naïve.txt";
        let obf = obfuscate_name(&dir_iv, plain).unwrap();
        let back = deobfuscate_name(&dir_iv, &obf).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn obfuscate_passthrough_outside_buckets() {
        // Spaces, dots, dashes, underscores, slashes-equivalent stay invariant.
        let dir_iv = [0u8; 16];
        let obf = obfuscate_name(&dir_iv, "a b.c-d_e").unwrap();
        // rotation = 0 -> letters unchanged; punctuation always unchanged
        assert!(obf.ends_with("a b.c-d_e"));
        let back = deobfuscate_name(&dir_iv, &obf).unwrap();
        assert_eq!(back, "a b.c-d_e");
    }

    #[test]
    fn obfuscate_different_dir_iv_different_output() {
        let plain = "secret.bin";
        let a = obfuscate_name(&[0x01u8; 16], plain).unwrap();
        let b = obfuscate_name(&[0xFFu8; 16], plain).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn obfuscate_rejects_empty() {
        assert!(obfuscate_name(&[0u8; 16], "").is_err());
    }

    #[test]
    fn deobfuscate_rejects_missing_prefix() {
        assert!(deobfuscate_name(&[0u8; 16], "no-dot-here").is_err());
    }

    #[test]
    fn deobfuscate_rejects_rotation_mismatch() {
        let plain = "doc.txt";
        let dir_iv_a = [0x01u8; 16];
        let dir_iv_b = [0x02u8; 16];
        let obf = obfuscate_name(&dir_iv_a, plain).unwrap();
        // Same encoding payload but a different dirIV must refuse to decode.
        let res = deobfuscate_name(&dir_iv_b, &obf);
        assert!(res.is_err(), "expected mismatch error, got {:?}", res);
    }

    // ── End-to-end smoke roundtrip (no rclone CLI dependency) ────────────────
    //
    // Simulates a full crypt workflow against an in-memory provider mock:
    //   1. derive keys from password + salt
    //   2. for each plaintext file/folder, encrypt name + content
    //   3. write encrypted bytes into a HashMap keyed by encrypted path
    //   4. read everything back, decrypt names + contents, compare to input
    //
    // Covers all three filename-encryption modes (standard, off, obfuscate)
    // plus single-chunk and multi-chunk file sizes.

    fn smoke_roundtrip_inner(mode: FilenameEncryption) {
        let (name_key, data_key) = derive_keys("smoke-pass", "smoke-salt").unwrap();
        let dir_iv: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10,
        ];

        let inputs: Vec<(&str, Vec<u8>)> = vec![
            ("hello.txt", b"hello world".to_vec()),
            ("EMPTY.bin", Vec::new()),
            (
                "big_blob.dat",
                (0..(CHUNK_DATA_SIZE * 2 + 137))
                    .map(|i| (i % 251) as u8)
                    .collect(),
            ),
            ("photos2026.jpg", vec![0xFEu8; 1024]),
        ];

        let mut wire: HashMap<String, Vec<u8>> = HashMap::new();

        let encrypt_one_name = |plain: &str| -> String {
            match mode {
                FilenameEncryption::Off => plain.to_string(),
                FilenameEncryption::Standard => encrypt_name(&name_key, &dir_iv, plain).unwrap(),
                FilenameEncryption::Obfuscate => obfuscate_name(&dir_iv, plain).unwrap(),
            }
        };
        let decrypt_one_name = |encoded: &str| -> String {
            match mode {
                FilenameEncryption::Off => encoded.to_string(),
                FilenameEncryption::Standard => decrypt_name(&name_key, &dir_iv, encoded).unwrap(),
                FilenameEncryption::Obfuscate => deobfuscate_name(&dir_iv, encoded).unwrap(),
            }
        };

        for (plain_name, plaintext) in &inputs {
            let encrypted_name = encrypt_one_name(plain_name);
            let encrypted_blob = encrypt_file_content(plaintext, &data_key).unwrap();
            wire.insert(encrypted_name, encrypted_blob);
        }

        let mut recovered: HashMap<String, Vec<u8>> = HashMap::new();
        for (encrypted_name, encrypted_blob) in &wire {
            let plain_name = decrypt_one_name(encrypted_name);
            let plaintext = decrypt_file_content(encrypted_blob, &data_key).unwrap();
            recovered.insert(plain_name, plaintext);
        }

        assert_eq!(recovered.len(), inputs.len());
        for (plain_name, plaintext) in &inputs {
            let got = recovered
                .get(*plain_name)
                .unwrap_or_else(|| panic!("missing recovered entry for {:?}", plain_name));
            assert_eq!(got, plaintext, "content mismatch for {:?}", plain_name);
        }
    }

    #[test]
    fn smoke_roundtrip_standard() {
        smoke_roundtrip_inner(FilenameEncryption::Standard);
    }

    #[test]
    fn smoke_roundtrip_off() {
        smoke_roundtrip_inner(FilenameEncryption::Off);
    }

    #[test]
    fn smoke_roundtrip_obfuscate() {
        smoke_roundtrip_inner(FilenameEncryption::Obfuscate);
    }
}
