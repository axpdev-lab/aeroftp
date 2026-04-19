// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! RAII guard that disconnects a `StorageProvider` on drop.
//!
//! `Drop` cannot be async, so we run the disconnect on the current Tokio
//! runtime handle using `spawn`. This fire-and-forget is *deliberate* and
//! strictly different from a raw spawn leak:
//!
//! 1. The spawned task owns the provider — nothing else can use it after
//!    drop, so there is no external dependency on completion.
//! 2. The task is bounded (one `disconnect().await`, not a loop).
//! 3. Its lifetime is independent of the caller, so it cannot hold a guard
//!    or a `&mut` that would otherwise leak.
//!
//! For call sites that *need* to await the disconnect (e.g. reconnecting on
//! the same socket immediately), use `ProviderGuard::disconnect` which
//! consumes the guard and returns the `Future`.

use crate::providers::StorageProvider;

type BoxedProvider = Box<dyn StorageProvider>;

/// RAII guard around a boxed `StorageProvider`. Disconnects on drop via the
/// current Tokio runtime handle.
#[must_use = "ProviderGuard disconnects on drop; bind it to a name"]
pub struct ProviderGuard {
    provider: Option<BoxedProvider>,
    label: &'static str,
}

impl ProviderGuard {
    /// Wrap a connected provider. `label` is used in warning logs if the
    /// disconnect fails.
    pub fn new(provider: BoxedProvider, label: &'static str) -> Self {
        Self {
            provider: Some(provider),
            label,
        }
    }

    /// Borrow the provider mutably for I/O.
    ///
    /// Named `provider_mut` (not `as_mut`) so call sites stay unambiguous with
    /// the `AsMut<T>` trait method.
    pub fn provider_mut(&mut self) -> &mut dyn StorageProvider {
        self.provider
            .as_mut()
            .expect("ProviderGuard provider already taken")
            .as_mut()
    }

    /// Borrow the provider immutably.
    pub fn provider_ref(&self) -> &dyn StorageProvider {
        self.provider
            .as_ref()
            .expect("ProviderGuard provider already taken")
            .as_ref()
    }

    /// Disconnect awaited-style. Prefer this when the caller explicitly
    /// wants to observe a disconnect failure or needs the socket released
    /// before the next operation.
    pub async fn disconnect(mut self) -> Result<(), crate::providers::ProviderError> {
        if let Some(mut provider) = self.provider.take() {
            provider.disconnect().await?;
        }
        Ok(())
    }
}

impl Drop for ProviderGuard {
    fn drop(&mut self) {
        if let Some(mut provider) = self.provider.take() {
            let label = self.label;
            // `Drop` is sync; we schedule the disconnect on the current
            // runtime. If no runtime handle exists (e.g. drop during shutdown
            // after the runtime has stopped) we warn and leak the provider
            // rather than panicking — better than taking down the process.
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        if let Err(err) = provider.disconnect().await {
                            tracing::warn!("provider-guard ({}) disconnect failed: {}", label, err);
                        }
                    });
                }
                Err(_) => {
                    tracing::warn!(
                        "provider-guard ({}) dropped without an active Tokio runtime; disconnect skipped",
                        label
                    );
                }
            }
        }
    }
}
