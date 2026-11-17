//! Copy every per-profile secret of a profile onto a new profile id, for a
//! duplicate made in the app or in the CLI.

use std::collections::HashMap;

use zeroize::Zeroizing;

use crate::credential_store::{CredentialError, CredentialStore};

/// Read one secret from the shared vault: `Ok(None)` when the vault has no such
/// key, `Err` when the vault could not be read.
pub fn read_vault_secret(
    store: &CredentialStore,
    key: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    match store.get_secret(key) {
        Ok(value) => Ok(Some(value)),
        Err(CredentialError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// True for a secret no writer gives a partition home: the editor saves the
/// Filen API key through `store_credential` and the OneDrive drive id and type
/// are captured at connect, both into the shared vault, unless an import or a
/// relocation gave the account a row. Connections and exports read them from
/// the account's row first, then the vault (`get_credential`,
/// `resolve_active_credential` in the profile export), so a duplicate reads
/// them the same way: the owner-aware reader would skip them for any account
/// but the legacy owner and the copy would lose them.
pub fn is_shared_vault_key(key: &str) -> bool {
    [
        "filen_api_key_",
        "onedrive_drive_id_",
        "onedrive_drive_type_",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}

/// The partition credential type a copied secret is written with, `None` for a
/// plain server secret. Tokens are typed so that, for a user other than the
/// legacy owner, they get a partition row of their kind instead of landing only
/// in the shared vault. `oauth__client_id` is app configuration, not a token.
pub fn copied_secret_type(base: &str) -> Option<&'static str> {
    if crate::profile_auth::DURABLE_KEY_PREFIXES.contains(&base) {
        Some("oauth")
    } else if base == "proton_native_session" {
        Some("proton_native_session")
    } else if base == "jottacloud_refresh" {
        Some("jottacloud_refresh")
    } else if base.starts_with("oauth_") && !base.starts_with("oauth__") {
        Some("oauth")
    } else {
        None
    }
}

/// Copy the secrets named by `source_keys` (each `<base>_<source_id>`) onto
/// `<base>_<target_id>`, all or nothing.
///
/// `read` returns `Ok(None)` for a secret the source does not have, `write`
/// stores one secret under its target key, `remove` deletes one target key.
/// Every source secret is read before anything is written, so a secret that
/// cannot be read stops the copy with nothing written. When a write fails,
/// every target key already written, and the one that failed, is removed; a
/// removal that fails does not stop the others, and the error then names each
/// secret kind left under the target id, since the copy is not clean. A
/// copy without a secret its source has would connect without it, or fall
/// back to a shared token that may be another account's. Errors name the
/// secret's kind (`base`), never its value.
///
/// Returns `base -> true` for each secret copied and `base -> false` for each
/// one the source does not have.
pub fn copy_profile_secret_keys<Read, Write, Remove>(
    source_id: &str,
    target_id: &str,
    source_keys: impl IntoIterator<Item = String>,
    mut read: Read,
    mut write: Write,
    mut remove: Remove,
) -> Result<HashMap<String, bool>, String>
where
    Read: FnMut(&str) -> Result<Option<Zeroizing<String>>, String>,
    Write: FnMut(&str, &str, &Zeroizing<String>) -> Result<(), String>,
    Remove: FnMut(&str, &str) -> Result<(), String>,
{
    let source_suffix = format!("_{source_id}");
    let mut copied = HashMap::new();
    let mut to_write = Vec::new();
    for source_key in source_keys {
        let Some(base) = source_key.strip_suffix(&source_suffix) else {
            continue;
        };
        match read(&source_key) {
            Ok(Some(value)) => to_write.push((base.to_string(), value)),
            Ok(None) => {
                copied.entry(base.to_string()).or_insert(false);
            }
            Err(error) => {
                return Err(format!(
                    "The {base} secret of the source profile could not be read, so nothing was copied: {error}"
                ))
            }
        }
    }
    let mut written: Vec<(String, String)> = Vec::new();
    for (base, value) in to_write {
        let target_key = format!("{base}_{target_id}");
        if let Err(error) = write(&base, &target_key, &value) {
            let mut left = Vec::new();
            if remove(&base, &target_key).is_err() {
                left.push(base.clone());
            }
            for (written_base, key) in written.iter().rev() {
                if remove(written_base, key).is_err() {
                    left.push(written_base.clone());
                }
            }
            let mut message = format!(
                "The {base} secret could not be written to the copy, so the copy was not created: {error}"
            );
            if !left.is_empty() {
                message.push_str(&format!(
                    "; these secrets could not be removed from the copy's id {target_id} and remain in the keystore: {}",
                    left.join(", ")
                ));
            }
            return Err(message);
        }
        copied.insert(base.clone(), true);
        written.push((base, target_key));
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const SOURCE: &str = "srv_source";
    const TARGET: &str = "srv_copy";

    fn keys(bases: &[&str]) -> Vec<String> {
        bases
            .iter()
            .map(|base| format!("{base}_{SOURCE}"))
            .collect()
    }

    /// A vault holding the source's secrets, where reading `fail_read`,
    /// writing `fail_write` (nothing stored), writing `store_then_fail` (the
    /// value stored, then an error) or removing `fail_remove` fails.
    struct Vault {
        entries: RefCell<BTreeMap<String, String>>,
        fail_read: Option<String>,
        fail_write: Option<String>,
        store_then_fail: Option<String>,
        fail_remove: Option<String>,
    }

    impl Vault {
        fn new(source: &[(&str, &str)]) -> Self {
            Self {
                entries: RefCell::new(
                    source
                        .iter()
                        .map(|(base, value)| (format!("{base}_{SOURCE}"), value.to_string()))
                        .collect(),
                ),
                fail_read: None,
                fail_write: None,
                store_then_fail: None,
                fail_remove: None,
            }
        }

        fn copy(&self, bases: &[&str]) -> Result<HashMap<String, bool>, String> {
            copy_profile_secret_keys(
                SOURCE,
                TARGET,
                keys(bases),
                |key| {
                    if self.fail_read.as_deref() == Some(key) {
                        return Err("keystore locked".to_string());
                    }
                    Ok(self.entries.borrow().get(key).cloned().map(Zeroizing::new))
                },
                |_, key, value| {
                    if self.fail_write.as_deref() == Some(key) {
                        return Err("disk full".to_string());
                    }
                    self.entries
                        .borrow_mut()
                        .insert(key.to_string(), value.to_string());
                    if self.store_then_fail.as_deref() == Some(key) {
                        return Err("partition mirror failed".to_string());
                    }
                    Ok(())
                },
                |_, key| {
                    if self.fail_remove.as_deref() == Some(key) {
                        return Err("keystore locked".to_string());
                    }
                    self.entries.borrow_mut().remove(key);
                    Ok(())
                },
            )
        }

        fn target_keys(&self) -> Vec<String> {
            self.entries
                .borrow()
                .keys()
                .filter(|key| key.ends_with(&format!("_{TARGET}")))
                .cloned()
                .collect()
        }
    }

    #[test]
    fn copies_every_secret_the_source_has_and_reports_the_absent_ones() {
        let vault = Vault::new(&[("server", "pw"), ("oauth_dropbox_token", "tok")]);
        let copied = vault
            .copy(&["server", "oauth_dropbox_token", "filen_api_key"])
            .unwrap();
        assert_eq!(copied.get("server"), Some(&true));
        assert_eq!(copied.get("oauth_dropbox_token"), Some(&true));
        assert_eq!(copied.get("filen_api_key"), Some(&false));
        assert_eq!(
            vault
                .entries
                .borrow()
                .get(&format!("server_{TARGET}"))
                .map(String::as_str),
            Some("pw")
        );
    }

    #[test]
    fn a_secret_that_cannot_be_read_stops_the_copy_before_anything_is_written() {
        let mut vault = Vault::new(&[("server", "pw"), ("oauth_dropbox_token", "tok")]);
        vault.fail_read = Some(format!("oauth_dropbox_token_{SOURCE}"));
        let error = vault
            .copy(&["server", "oauth_dropbox_token"])
            .expect_err("a read error must not give a copy without the token");
        assert!(error.contains("oauth_dropbox_token"), "{error}");
        assert!(error.contains("keystore locked"), "{error}");
        assert!(vault.target_keys().is_empty(), "{:?}", vault.target_keys());
    }

    #[test]
    fn a_secret_that_cannot_be_written_removes_every_secret_already_copied() {
        let mut vault = Vault::new(&[
            ("server", "pw"),
            ("oauth_dropbox_token", "tok"),
            ("filen_api_key", "key"),
        ]);
        vault.fail_write = Some(format!("oauth_dropbox_token_{TARGET}"));
        let error = vault
            .copy(&["server", "oauth_dropbox_token", "filen_api_key"])
            .expect_err("a write error must not give a copy without the token");
        assert!(error.contains("oauth_dropbox_token"), "{error}");
        assert!(error.contains("disk full"), "{error}");
        assert!(vault.target_keys().is_empty(), "{:?}", vault.target_keys());
    }

    #[test]
    fn the_error_never_carries_the_secret_value() {
        let mut vault = Vault::new(&[("server", "hunter2-secret")]);
        vault.fail_write = Some(format!("server_{TARGET}"));
        let error = vault.copy(&["server"]).unwrap_err();
        assert!(!error.contains("hunter2-secret"), "{error}");
    }

    #[test]
    fn a_source_key_of_another_profile_is_skipped() {
        let vault = Vault::new(&[("server", "pw")]);
        let copied = copy_profile_secret_keys(
            SOURCE,
            TARGET,
            vec!["server_other".to_string()],
            |key| Ok(vault.entries.borrow().get(key).cloned().map(Zeroizing::new)),
            |_, _, _| Ok(()),
            |_, _| Ok(()),
        )
        .unwrap();
        assert!(copied.is_empty());
    }

    #[test]
    fn a_removal_that_fails_does_not_stop_the_others_and_is_named_in_the_error() {
        let mut vault = Vault::new(&[
            ("server", "pw"),
            ("filen_api_key", "key"),
            ("oauth_dropbox_token", "tok"),
        ]);
        vault.fail_write = Some(format!("oauth_dropbox_token_{TARGET}"));
        // The rollback visits filen_api_key before server: server must still go.
        vault.fail_remove = Some(format!("filen_api_key_{TARGET}"));
        let error = vault
            .copy(&["server", "filen_api_key", "oauth_dropbox_token"])
            .unwrap_err();
        assert_eq!(vault.target_keys(), vec![format!("filen_api_key_{TARGET}")]);
        assert!(error.contains("disk full"), "{error}");
        assert!(
            error.contains("remain in the keystore: filen_api_key"),
            "the secret left behind must be named: {error}"
        );
        assert!(!error.contains("pw"), "{error}");
    }

    #[test]
    fn tokens_are_copied_with_their_type_and_server_secrets_are_not() {
        assert_eq!(copied_secret_type("oauth_dropbox"), Some("oauth"));
        assert_eq!(copied_secret_type("github_oauth"), Some("oauth"));
        assert_eq!(
            copied_secret_type("jottacloud_refresh"),
            Some("jottacloud_refresh")
        );
        assert_eq!(
            copied_secret_type("proton_native_session"),
            Some("proton_native_session")
        );
        assert_eq!(copied_secret_type("oauth__client_id"), None);
        assert_eq!(copied_secret_type("server"), None);
        assert_eq!(copied_secret_type("filen_api_key"), None);
    }

    #[test]
    fn a_write_that_stored_its_value_before_failing_is_removed_with_the_earlier_ones() {
        let mut vault = Vault::new(&[
            ("server", "pw"),
            ("oauth_dropbox_token", "tok"),
            ("filen_api_key", "key"),
        ]);
        // The vault write lands, then the partition mirror fails.
        vault.store_then_fail = Some(format!("filen_api_key_{TARGET}"));
        let error = vault
            .copy(&["server", "oauth_dropbox_token", "filen_api_key"])
            .unwrap_err();
        assert!(error.contains("filen_api_key"), "{error}");
        assert!(!error.contains("remain in the keystore"), "{error}");
        assert!(vault.target_keys().is_empty(), "{:?}", vault.target_keys());
    }

    #[test]
    fn only_keys_without_a_partition_home_are_read_from_the_shared_vault() {
        for key in [
            "filen_api_key_srv",
            "onedrive_drive_id_srv",
            "onedrive_drive_type_srv",
        ] {
            assert!(is_shared_vault_key(key), "{key}");
        }
        for key in [
            "server_srv",
            "server_modes_srv",
            "oauth_google_srv",
            "jottacloud_refresh_srv",
            "aerocrypt_overlay_pw_srv",
            "proton_native_session_srv",
            "github_oauth_srv",
        ] {
            assert!(!is_shared_vault_key(key), "{key}");
        }
    }
}
