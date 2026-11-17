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

/// Copy the secrets named by `source_keys` (each `<base>_<source_id>`) onto
/// `<base>_<target_id>`, all or nothing.
///
/// `read` returns `Ok(None)` for a secret the source does not have, `write`
/// stores one secret under its target key, `remove` deletes one target key.
/// Every source secret is read before anything is written, so a secret that
/// cannot be read stops the copy with nothing written. When a write fails,
/// every target key already written, and the one that failed, is removed. A
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
    Remove: FnMut(&str, &str),
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
            remove(&base, &target_key);
            for (base, key) in written.iter().rev() {
                remove(base, key);
            }
            return Err(format!(
                "The {base} secret could not be written to the copy, so the copy was not created: {error}"
            ));
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

    /// A vault holding the source's secrets, where reading `fail_read` or
    /// writing `fail_write` fails.
    struct Vault {
        entries: RefCell<BTreeMap<String, String>>,
        fail_read: Option<String>,
        fail_write: Option<String>,
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
                    Ok(())
                },
                |_, key| {
                    self.entries.borrow_mut().remove(key);
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
            |_, _| {},
        )
        .unwrap();
        assert!(copied.is_empty());
    }
}
