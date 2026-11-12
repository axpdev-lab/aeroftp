//! Backend ownership of a GUI connection and its resumable sync journal.
//! Only a keyed, versioned tag crosses IPC; credentials stay in the provider.
use crate::credential_store::CredentialStore;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GuiOwnerContext {
    user: Option<i64>,
    vault_generation: u64,
    account_snapshot: crate::user_partitions::GuiAccountSnapshot,
}

fn changed() -> String {
    crate::providers::ProviderError::TargetChanged.to_string()
}

impl GuiOwnerContext {
    pub(crate) fn capture() -> Result<Self, String> {
        let generation = CredentialStore::cache_generation();
        let account_snapshot = crate::user_partitions::gui_account_snapshot()?;
        let user = account_snapshot.user;
        if let Some(user) = user {
            crate::user_partitions::ensure_credential_user_unlocked(user)?;
        }
        if generation != CredentialStore::cache_generation() {
            return Err(changed());
        }
        Ok(Self {
            user,
            vault_generation: generation,
            account_snapshot,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.vault_generation != CredentialStore::cache_generation()
            || self.account_snapshot != crate::user_partitions::gui_account_snapshot()?
        {
            return Err(changed());
        }
        Ok(())
    }

    pub(crate) fn with_current<T>(
        &self,
        publish: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        crate::user_partitions::with_gui_account_snapshot(&self.account_snapshot, || {
            if self.vault_generation != CredentialStore::cache_generation() {
                return Err(changed());
            }
            publish()
        })
    }

    pub(crate) fn user(&self) -> Option<i64> {
        self.user
    }

    pub(crate) fn persistent_tag(&self, material: &str) -> Result<Option<String>, String> {
        self.validate()?;
        let Some(user) = self.user else {
            return Ok(None);
        };
        let Some(store) = CredentialStore::from_cache() else {
            return Ok(None);
        };
        let material = zeroize::Zeroizing::new(
            serde_json::to_string(&(user, material)).map_err(|_| changed())?,
        );
        let tag = crate::user_partitions::sync_owner_tag_for_user(&store, user, &material)?;
        self.validate()?;
        Ok(Some(format!("owner-v1:{tag}")))
    }
}

pub(crate) fn epoch_binding(nonce: &str, epoch: u64) -> String {
    format!("epoch-v1:{nonce}:{epoch}")
}

pub(crate) fn bindings_match(current: &str, journal: &str) -> bool {
    use subtle::ConstantTimeEq;
    (journal.starts_with("owner-v1:hmac-sha256:") || journal.starts_with("epoch-v1:"))
        && bool::from(current.as_bytes().ct_eq(journal.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn epoch_owner_never_survives_another_connection_or_process() {
        let a = epoch_binding("process-A", 4);
        assert!(bindings_match(&a, &a));
        assert!(!bindings_match(&epoch_binding("process-A", 5), &a));
        assert!(!bindings_match(&epoch_binding("process-B", 4), &a));
        assert!(!bindings_match(
            "owner-v2:unsupported",
            "owner-v2:unsupported"
        ));
        assert!(!bindings_match(&a, ""));
    }

    /// `with_current` compares the vault generation before `publish` runs, so
    /// a `clear_cache` during `publish` still publishes. What it publishes is
    /// an owner of the retired generation, and every reader of a published
    /// owner validates it first: the validation, the persistent tag, a second
    /// publication and the pin of the state that holds it all refuse.
    #[test]
    #[ignore = "process-global vault/session/env; run alone with --ignored --test-threads=1"]
    fn an_owner_published_across_a_vault_generation_change_fails_closed() {
        use crate::provider_commands::ProviderState;
        use crate::user_partitions as partitions;

        let _env = crate::test_env::lock();
        let _partition = partitions::test_lock();
        let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let previous_passphrase = std::env::var_os("AEROFTP_USER_PASSPHRASE");
        let temporary = tempfile::TempDir::new().expect("temporary config");
        std::env::set_var("XDG_CONFIG_HOME", temporary.path());
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        let open_vault = || {
            CredentialStore::unlock_with_master("sync-owner-test-pass", None)
                .expect("unlock vault");
        };
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password("sync-owner-test-pass")
            .expect("bootstrap vault");
        open_vault();
        let store = CredentialStore::from_cache().expect("cached vault");
        partitions::init_or_migrate_cli(&store).expect("init partitions");

        // The generation moves while the publication closure runs.
        let owner = GuiOwnerContext::capture().expect("capture owner");
        assert!(owner.validate().is_ok());
        assert!(owner.persistent_tag("material").unwrap().is_some());
        let slot = std::sync::Mutex::new(None);
        owner
            .with_current(|| {
                CredentialStore::clear_cache();
                *slot.lock().unwrap() = Some(owner.clone());
                Ok(())
            })
            .expect("the comparison ran before the generation moved");
        let published = slot.lock().unwrap().clone().expect("published owner");
        assert_eq!(published.validate(), Err(changed()));
        assert_eq!(published.persistent_tag("material"), Err(changed()));
        assert_eq!(published.with_current(|| Ok(())), Err(changed()));
        // Reopening the vault does not bring the retired generation back.
        open_vault();
        assert_eq!(published.validate(), Err(changed()));
        assert_eq!(published.persistent_tag("material"), Err(changed()));

        // The same through the state that holds a published owner.
        let owner = GuiOwnerContext::capture().expect("capture owner again");
        let state = ProviderState::new();
        state
            .set_legacy_gui_owner(owner, None)
            .expect("publish the owner");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert!(runtime.block_on(state.pin_gui_target(Some(0))).is_ok());
        CredentialStore::clear_cache();
        open_vault();
        let refused = runtime
            .block_on(state.pin_gui_target(Some(0)))
            .map(|_| ())
            .expect_err("a stale owner must not pin a target");
        assert_eq!(refused, changed());

        CredentialStore::clear_cache();
        match previous_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match previous_passphrase {
            Some(value) => std::env::set_var("AEROFTP_USER_PASSPHRASE", value),
            None => std::env::remove_var("AEROFTP_USER_PASSPHRASE"),
        }
    }
}
