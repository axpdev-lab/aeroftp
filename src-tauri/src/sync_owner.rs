//! Backend ownership of a GUI connection and its resumable sync journal.
//! Only a keyed, versioned tag crosses IPC; credentials stay in the provider.
use crate::credential_store::CredentialStore;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GuiOwnerContext {
    user: Option<i64>,
    vault_generation: u64,
    account_snapshot: (Option<i64>, i64, i64, u64),
}

fn changed() -> String {
    crate::providers::ProviderError::TargetChanged.to_string()
}

impl GuiOwnerContext {
    pub(crate) fn capture() -> Result<Self, String> {
        let generation = CredentialStore::cache_generation();
        let account_snapshot = crate::user_partitions::gui_account_snapshot()?;
        let user = account_snapshot.0;
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
        let tag = crate::user_partitions::sync_owner_tag_for_user(&store, user, material)?;
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
}
