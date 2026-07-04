//! WI-4b: per-user-partition P2P secret store (app-side storage facade).
//!
//! This is the SHIPPING-code storage layer for the user-to-user P2P identity
//! and drive keys (owner decision D1: custody is per user-partition, DEK-wrapped
//! in `user_partitions.db`). It is deliberately OPAQUE: `peer-l0` is a
//! [dev-dependency] of the app (WI-3c), so shipping code must not name
//! `aeroftp_peer_l0` types. Callers pass and receive raw bytes; the crypto
//! identity type lives in the peer-l0 crate and is only round-tripped through
//! this API by the tests.
//!
//! Custody model:
//! - private material (the identity secret, per-drive key blobs) is sealed with
//!   the active user DEK via [`user_crypto::encrypt_blob`] and stored as
//!   `(encrypted_blob, nonce)` — at rest it is indistinguishable from random;
//! - public material (the shareable AeroFTP-ID, contact ids/aliases, drive
//!   namespace + role) is stored in plaintext so it can be listed without
//!   unlocking the partition.
//!
//! Every operation is scoped to a `user_id`; the three tables carry an
//! `ON DELETE CASCADE` FK to `users(id)`, so deleting a partition wipes its P2P
//! secrets with it. Engine wiring and CLI verbs are deliberately out of scope
//! here (WI-4c/WI-4d); this is the storage layer only.

use crate::user_crypto::{self, SecretKey};
use rusqlite::{params, Connection, OptionalExtension};
use zeroize::Zeroizing;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ----------------------------------------------------------------------------
// Identity (exactly one per user_id)
// ----------------------------------------------------------------------------

/// Store (or replace) the user's P2P identity. `secret_bytes` is the opaque
/// private identity key material (e.g. a 64-byte Ed25519 + X25519 identity),
/// sealed with the active user `dek`. `public_id` is the shareable AeroFTP-ID,
/// stored in plaintext so it can be listed without unlocking. Idempotent UPSERT
/// keyed by `user_id`.
pub fn store_identity(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    secret_bytes: &[u8],
    public_id: &str,
) -> Result<(), String> {
    let (encrypted_blob, nonce) = user_crypto::encrypt_blob(dek, secret_bytes)?;
    let now = now_ms();
    conn.execute(
        "INSERT INTO peer_identity(
             user_id, encrypted_blob, nonce, aead_alg, public_id, created_at, updated_at
         )
         VALUES (?1, ?2, ?3, 'aes-256-gcm', ?4, ?5, ?5)
         ON CONFLICT(user_id) DO UPDATE SET
             encrypted_blob = excluded.encrypted_blob,
             nonce          = excluded.nonce,
             aead_alg       = excluded.aead_alg,
             public_id      = excluded.public_id,
             updated_at     = excluded.updated_at",
        params![user_id, encrypted_blob, nonce.to_vec(), public_id, now],
    )
    .map_err(|e| format!("Store peer identity: {e}"))?;
    Ok(())
}

/// Load and decrypt the user's P2P identity secret, or `None` if the user has
/// no stored identity. The plaintext is returned in a [`Zeroizing`] buffer so it
/// is wiped on drop. A `dek` that does not match the one used to seal the row
/// fails closed with the AES-GCM tag error (see the DEK-isolation test).
pub fn load_identity(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM peer_identity WHERE user_id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("Read peer identity: {e}"))?;
    match row {
        Some((encrypted_blob, nonce)) => Ok(Some(user_crypto::decrypt_blob(
            dek,
            &nonce,
            &encrypted_blob,
        )?)),
        None => Ok(None),
    }
}

/// Return the user's public AeroFTP-ID, or `None` if unset. Public material:
/// requires no DEK and never unlocks the partition.
pub fn identity_public_id(conn: &Connection, user_id: i64) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT public_id FROM peer_identity WHERE user_id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| format!("Read peer identity public id: {e}"))
}

// ----------------------------------------------------------------------------
// Contacts (public, partitioned)
// ----------------------------------------------------------------------------

/// Add (or rename) a contact in the user's partition. Idempotent UPSERT keyed by
/// `(user_id, contact_id)`: re-adding an existing contact updates its alias.
pub fn add_contact(
    conn: &Connection,
    user_id: i64,
    contact_id: &str,
    alias: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO peer_contacts(user_id, contact_id, alias, added_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(user_id, contact_id) DO UPDATE SET
             alias = excluded.alias",
        params![user_id, contact_id, alias, now_ms()],
    )
    .map_err(|e| format!("Add peer contact: {e}"))?;
    Ok(())
}

/// List the user's contacts as `(contact_id, alias)`, ordered by alias.
pub fn list_contacts(conn: &Connection, user_id: i64) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT contact_id, alias FROM peer_contacts
             WHERE user_id = ?1 ORDER BY alias COLLATE NOCASE, contact_id",
        )
        .map_err(|e| format!("Prepare list peer contacts: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("Query peer contacts: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Read peer contact row: {e}"))?);
    }
    Ok(out)
}

/// Remove a contact from the user's partition. A no-op if it does not exist.
pub fn remove_contact(conn: &Connection, user_id: i64, contact_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM peer_contacts WHERE user_id = ?1 AND contact_id = ?2",
        params![user_id, contact_id],
    )
    .map_err(|e| format!("Remove peer contact: {e}"))?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Mutes (public, partitioned) - per-sender suppression of inbound signals
// ----------------------------------------------------------------------------

/// Mute a sender AFID in the user's partition: every inbound knock/action/offer
/// it sends is dropped before it ever reaches the UI. Idempotent UPSERT keyed by
/// `(user_id, contact_id)`. Public material: no DEK required.
pub fn add_mute(conn: &Connection, user_id: i64, contact_id: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO peer_mutes(user_id, contact_id, muted_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id, contact_id) DO UPDATE SET
             muted_at = excluded.muted_at",
        params![user_id, contact_id, now_ms()],
    )
    .map_err(|e| format!("Add peer mute: {e}"))?;
    Ok(())
}

/// List the user's muted AFIDs, ordered by AFID. Public material: no DEK.
pub fn list_mutes(conn: &Connection, user_id: i64) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT contact_id FROM peer_mutes
             WHERE user_id = ?1 ORDER BY contact_id",
        )
        .map_err(|e| format!("Prepare list peer mutes: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |r| r.get::<_, String>(0))
        .map_err(|e| format!("Query peer mutes: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Read peer mute row: {e}"))?);
    }
    Ok(out)
}

/// Unmute a sender AFID. A no-op if it was not muted.
pub fn remove_mute(conn: &Connection, user_id: i64, contact_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM peer_mutes WHERE user_id = ?1 AND contact_id = ?2",
        params![user_id, contact_id],
    )
    .map_err(|e| format!("Remove peer mute: {e}"))?;
    Ok(())
}

/// `true` if the sender AFID is muted in the user's partition.
pub fn is_muted(conn: &Connection, user_id: i64, contact_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(1) FROM peer_mutes WHERE user_id = ?1 AND contact_id = ?2",
            params![user_id, contact_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("Check peer mute: {e}"))?;
    Ok(n > 0)
}

// ----------------------------------------------------------------------------
// Settings (public, partitioned) - inbound policy + discovery mode
// ----------------------------------------------------------------------------

/// The per-partition AeroShare preferences. `friends_only` gates inbound
/// signals to saved contacts; `discovery_mode` is one of `both|dht|n0|lan|none`;
/// `rate_limit_per_min` caps inbound signals per sender per minute (`0` = off).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerSettings {
    pub friends_only: bool,
    pub discovery_mode: String,
    pub rate_limit_per_min: u32,
}

impl Default for PeerSettings {
    fn default() -> Self {
        PeerSettings {
            friends_only: false,
            discovery_mode: "both".to_string(),
            rate_limit_per_min: 20,
        }
    }
}

/// Load the user's AeroShare settings, falling back to defaults when no row
/// exists yet. Public material: no DEK required.
pub fn get_settings(conn: &Connection, user_id: i64) -> Result<PeerSettings, String> {
    let row: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT friends_only, discovery_mode, rate_limit_per_min
             FROM peer_settings WHERE user_id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("Read peer settings: {e}"))?;
    Ok(match row {
        Some((friends_only, discovery_mode, rate_limit_per_min)) => PeerSettings {
            friends_only: friends_only != 0,
            discovery_mode,
            rate_limit_per_min: rate_limit_per_min.max(0) as u32,
        },
        None => PeerSettings::default(),
    })
}

/// Store (or replace) the user's AeroShare settings. Idempotent UPSERT keyed by
/// `user_id`. `discovery_mode` is validated by the caller (command layer).
pub fn set_settings(
    conn: &Connection,
    user_id: i64,
    settings: &PeerSettings,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO peer_settings(user_id, friends_only, discovery_mode, rate_limit_per_min, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(user_id) DO UPDATE SET
             friends_only       = excluded.friends_only,
             discovery_mode     = excluded.discovery_mode,
             rate_limit_per_min = excluded.rate_limit_per_min,
             updated_at         = excluded.updated_at",
        params![
            user_id,
            settings.friends_only as i64,
            settings.discovery_mode,
            settings.rate_limit_per_min as i64,
            now_ms()
        ],
    )
    .map_err(|e| format!("Store peer settings: {e}"))?;
    Ok(())
}

/// `true` if the sender AFID is a saved contact in the user's partition (used by
/// the friends-only inbound gate).
pub fn is_contact(conn: &Connection, user_id: i64, contact_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(1) FROM peer_contacts WHERE user_id = ?1 AND contact_id = ?2",
            params![user_id, contact_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("Check peer contact: {e}"))?;
    Ok(n > 0)
}

// ----------------------------------------------------------------------------
// Drives (private blob DEK-wrapped; namespace + role plaintext for listing)
// ----------------------------------------------------------------------------

/// Store (or replace) the per-drive key material for `namespace_id`. `blob` is
/// the opaque private drive key material, sealed with the active user `dek`;
/// `role` (plaintext) records the user's relationship to the drive (e.g.
/// `publisher`/`replicator`). Idempotent UPSERT keyed by `(user_id, namespace_id)`.
pub fn store_drive(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    namespace_id: &str,
    role: &str,
    blob: &[u8],
) -> Result<(), String> {
    let (encrypted_blob, nonce) = user_crypto::encrypt_blob(dek, blob)?;
    let now = now_ms();
    conn.execute(
        "INSERT INTO peer_drives(
             user_id, namespace_id, role, encrypted_blob, nonce, aead_alg, created_at, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, 'aes-256-gcm', ?6, ?6)
         ON CONFLICT(user_id, namespace_id) DO UPDATE SET
             role           = excluded.role,
             encrypted_blob = excluded.encrypted_blob,
             nonce          = excluded.nonce,
             aead_alg       = excluded.aead_alg,
             updated_at     = excluded.updated_at",
        params![
            user_id,
            namespace_id,
            role,
            encrypted_blob,
            nonce.to_vec(),
            now
        ],
    )
    .map_err(|e| format!("Store peer drive: {e}"))?;
    Ok(())
}

/// Load and decrypt the per-drive key material for `namespace_id`, or `None` if
/// absent. Returned in a [`Zeroizing`] buffer; a wrong `dek` fails closed.
pub fn load_drive(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    namespace_id: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM peer_drives
             WHERE user_id = ?1 AND namespace_id = ?2",
            params![user_id, namespace_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("Read peer drive: {e}"))?;
    match row {
        Some((encrypted_blob, nonce)) => Ok(Some(user_crypto::decrypt_blob(
            dek,
            &nonce,
            &encrypted_blob,
        )?)),
        None => Ok(None),
    }
}

/// List the user's drives as `(namespace_id, role)`, ordered by namespace.
/// Public material only: no DEK required.
pub fn list_drives(conn: &Connection, user_id: i64) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT namespace_id, role FROM peer_drives
             WHERE user_id = ?1 ORDER BY namespace_id",
        )
        .map_err(|e| format!("Prepare list peer drives: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("Query peer drives: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Read peer drive row: {e}"))?);
    }
    Ok(out)
}

/// Remove a drive from the user's partition. A no-op if it does not exist.
pub fn delete_drive(conn: &Connection, user_id: i64, namespace_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM peer_drives WHERE user_id = ?1 AND namespace_id = ?2",
        params![user_id, namespace_id],
    )
    .map_err(|e| format!("Delete peer drive: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_partitions;

    // Root wrapping key for the in-memory test partition DB (matches the
    // user_partitions test harness convention).
    fn test_root() -> SecretKey {
        user_crypto::secret_key_from_bytes(&[7u8; 32])
    }

    /// Build a fresh in-memory partition DB with two passphrase-less users (A, B)
    /// and return their ids. Mirrors the user_partitions test setup.
    fn two_user_db() -> (Connection, i64, i64) {
        let mut conn = Connection::open_in_memory().expect("memory db");
        user_partitions::init_db_schema(&conn).expect("schema");
        let root = [7u8; 32];
        let a = user_partitions::create_user(&mut conn, &root, "Alice", None, None, None)
            .expect("create A");
        let b = user_partitions::create_user(&mut conn, &root, "Bob", None, None, None)
            .expect("create B");
        (conn, a.id, b.id)
    }

    #[test]
    fn p1_identity_round_trip_through_real_peer_l0_identity() {
        let (conn, user_a, _user_b) = two_user_db();
        let id = aeroftp_peer_l0::Identity::generate();
        let public_id = id.public().to_aeroftp_id();

        user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            store_identity(&conn, user_a, dek, &id.to_secret_bytes(), &public_id)
        })
        .expect("store identity");

        let back = user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            load_identity(&conn, user_a, dek)
        })
        .expect("load identity")
        .expect("identity present");

        let restored = aeroftp_peer_l0::Identity::from_secret_bytes(
            back.as_slice().try_into().expect("64-byte secret"),
        );
        assert_eq!(
            restored.public().to_aeroftp_id(),
            public_id,
            "round-tripped identity must yield the same AeroFTP-ID"
        );
        assert_eq!(
            identity_public_id(&conn, user_a).expect("public id"),
            Some(public_id),
            "public id is readable without the DEK"
        );
    }

    #[test]
    fn p2_drive_blob_round_trip_list_and_delete() {
        let (conn, user_a, _user_b) = two_user_db();
        let ns = "8ce68153cc3b80d778b594b7e3787e3511745ca28b384ebdb4fab5ec41be0832";
        let blob = b"per-drive opaque key material";

        user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            store_drive(&conn, user_a, dek, ns, "publisher", blob)
        })
        .expect("store drive");

        let got = user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            load_drive(&conn, user_a, dek, ns)
        })
        .expect("load drive")
        .expect("drive present");
        assert_eq!(got.as_slice(), &blob[..], "drive blob round-trips");

        let drives = list_drives(&conn, user_a).expect("list");
        assert_eq!(drives, vec![(ns.to_string(), "publisher".to_string())]);

        delete_drive(&conn, user_a, ns).expect("delete");
        assert!(list_drives(&conn, user_a)
            .expect("list after delete")
            .is_empty());
    }

    #[test]
    fn p4_rekey_replaces_the_drive_content_key() {
        // WI-4e: re-keying a drive UPSERTs the per-drive content key under the same namespace, so the
        // vault custodies only the NEW key (old capability holders can no longer derive the drive key).
        let (conn, user_a, _user_b) = two_user_db();
        let ns = "8ce68153cc3b80d778b594b7e3787e3511745ca28b384ebdb4fab5ec41be0832";
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];

        user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            store_drive(&conn, user_a, dek, ns, "publisher", &k1)
        })
        .expect("store k1");
        user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            store_drive(&conn, user_a, dek, ns, "publisher", &k2)
        })
        .expect("re-key to k2");

        let got = user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            load_drive(&conn, user_a, dek, ns)
        })
        .expect("load")
        .expect("present");
        assert_eq!(
            got.as_slice(),
            &k2[..],
            "re-key replaces the stored content key"
        );
        assert_ne!(got.as_slice(), &k1[..], "the old content key is gone");
        // Still exactly one row for the namespace (UPSERT, not insert).
        assert_eq!(
            list_drives(&conn, user_a).expect("list"),
            vec![(ns.to_string(), "publisher".to_string())]
        );
    }

    #[test]
    fn p3_contacts_add_list_remove() {
        let (conn, user_a, _user_b) = two_user_db();
        add_contact(&conn, user_a, "AERO-AAAA", "Carol").expect("add");
        add_contact(&conn, user_a, "AERO-BBBB", "Dave").expect("add");
        // re-add updates alias (UPSERT)
        add_contact(&conn, user_a, "AERO-AAAA", "Carol Renamed").expect("re-add");

        let contacts = list_contacts(&conn, user_a).expect("list");
        assert_eq!(
            contacts,
            vec![
                ("AERO-AAAA".to_string(), "Carol Renamed".to_string()),
                ("AERO-BBBB".to_string(), "Dave".to_string()),
            ]
        );

        remove_contact(&conn, user_a, "AERO-AAAA").expect("remove");
        let contacts = list_contacts(&conn, user_a).expect("list");
        assert_eq!(
            contacts,
            vec![("AERO-BBBB".to_string(), "Dave".to_string())]
        );
    }

    #[test]
    fn p5_mutes_add_list_remove_and_is_muted() {
        let (conn, user_a, user_b) = two_user_db();
        assert!(!is_muted(&conn, user_a, "AERO-X").expect("check"));
        add_mute(&conn, user_a, "AERO-X").expect("mute");
        add_mute(&conn, user_a, "AERO-Y").expect("mute");
        // re-mute is idempotent (UPSERT, still one row)
        add_mute(&conn, user_a, "AERO-X").expect("re-mute");
        assert!(is_muted(&conn, user_a, "AERO-X").expect("check"));
        assert_eq!(
            list_mutes(&conn, user_a).expect("list"),
            vec!["AERO-X".to_string(), "AERO-Y".to_string()]
        );
        // mutes are partitioned: user B sees none of A's
        assert!(!is_muted(&conn, user_b, "AERO-X").expect("check B"));
        assert!(list_mutes(&conn, user_b).expect("list B").is_empty());

        remove_mute(&conn, user_a, "AERO-X").expect("unmute");
        assert!(!is_muted(&conn, user_a, "AERO-X").expect("check"));
        assert_eq!(
            list_mutes(&conn, user_a).expect("list"),
            vec!["AERO-Y".to_string()]
        );
    }

    #[test]
    fn p6_settings_default_get_set_round_trip() {
        let (conn, user_a, _user_b) = two_user_db();
        // Defaults when no row exists yet.
        assert_eq!(
            get_settings(&conn, user_a).expect("default"),
            PeerSettings::default()
        );
        let want = PeerSettings {
            friends_only: true,
            discovery_mode: "none".to_string(),
            rate_limit_per_min: 5,
        };
        set_settings(&conn, user_a, &want).expect("set");
        assert_eq!(get_settings(&conn, user_a).expect("get"), want);
        // UPSERT updates in place.
        let want2 = PeerSettings {
            friends_only: false,
            discovery_mode: "dht".to_string(),
            rate_limit_per_min: 0,
        };
        set_settings(&conn, user_a, &want2).expect("set2");
        assert_eq!(get_settings(&conn, user_a).expect("get2"), want2);
    }

    #[test]
    fn p7_is_contact_reflects_the_allowlist() {
        let (conn, user_a, _user_b) = two_user_db();
        assert!(!is_contact(&conn, user_a, "AERO-Z").expect("check"));
        add_contact(&conn, user_a, "AERO-Z", "Zoe").expect("add");
        assert!(is_contact(&conn, user_a, "AERO-Z").expect("check"));
        remove_contact(&conn, user_a, "AERO-Z").expect("remove");
        assert!(!is_contact(&conn, user_a, "AERO-Z").expect("check"));
    }

    #[test]
    fn n1_dek_isolation_other_partition_cannot_decrypt() {
        let (conn, user_a, user_b) = two_user_db();
        let id = aeroftp_peer_l0::Identity::generate();
        let public_id = id.public().to_aeroftp_id();

        // Store under user A's DEK.
        user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            store_identity(&conn, user_a, dek, &id.to_secret_bytes(), &public_id)
        })
        .expect("store under A");

        // Load row A's blob but decrypt with user B's DEK -> must fail closed
        // (AES-GCM tag), NOT return A's secret. We read A's row but pass B's DEK.
        let result = user_partitions::with_partition_dek(&conn, &test_root(), user_b, |dek_b| {
            load_identity(&conn, user_a, dek_b)
        });
        assert!(
            result.is_err(),
            "decrypting user A's identity with user B's DEK must fail, got {result:?}"
        );
    }

    #[test]
    fn n2_at_rest_blob_does_not_contain_plaintext_secret() {
        let (conn, user_a, _user_b) = two_user_db();
        let id = aeroftp_peer_l0::Identity::generate();
        let secret = id.to_secret_bytes();

        user_partitions::with_partition_dek(&conn, &test_root(), user_a, |dek| {
            store_identity(&conn, user_a, dek, &secret, &id.public().to_aeroftp_id())
        })
        .expect("store");

        let stored: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_blob FROM peer_identity WHERE user_id = ?1",
                params![user_a],
                |r| r.get(0),
            )
            .expect("read blob");
        // The plaintext 64-byte secret must not appear as a contiguous window
        // anywhere in the ciphertext at rest.
        let leaks = stored.windows(secret.len()).any(|w| w == &secret[..]);
        assert!(
            !leaks,
            "encrypted_blob must not contain the plaintext secret"
        );
    }
}
