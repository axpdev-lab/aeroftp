// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Session-scoped probe cache for [`Capability::SupportedAfterProbe`].
//!
//! Some capabilities cannot be answered from static provider metadata: WebDAV
//! and Koofr advertise range download only `SupportedAfterProbe`, because the
//! honest answer needs a one-byte `Range` request against the live server.
//!
//! Re-probing once per file in a 100-file transfer would add 100 wasted round
//! trips. [`SessionProbeCache`] memoises each probe by key for the lifetime of
//! one transfer session: the probe closure runs at most once per key even when
//! many file graphs resolve the same capability concurrently, because every
//! key is backed by a [`tokio::sync::OnceCell`].
//!
//! The cache is deliberately provider-free: it stores only `bool` results
//! against opaque string keys, so the builder and the runner can share it
//! without the graph engine depending on any provider type.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::OnceCell;

use super::capabilities::Capability;

/// Memoises capability probe results for one transfer session.
///
/// Construct one [`SessionProbeCache`] per session, share it as `Arc`, and
/// resolve every `SupportedAfterProbe` capability through it. Distinct keys
/// probe independently; the same key probes exactly once.
#[derive(Default)]
pub struct SessionProbeCache {
    cells: Mutex<HashMap<String, Arc<OnceCell<bool>>>>,
}

impl SessionProbeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `OnceCell` for `key`, created on first use.
    fn cell(&self, key: &str) -> Arc<OnceCell<bool>> {
        let mut cells = self.cells.lock().expect("probe cache mutex poisoned");
        Arc::clone(
            cells
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new())),
        )
    }

    /// Resolve `key`, running `probe` at most once for the session.
    ///
    /// Concurrent callers for the same key all observe the single result: the
    /// first to arrive runs `probe`, the rest await its completion. A later
    /// call returns the cached value without touching `probe` at all.
    pub async fn resolve<F, Fut>(&self, key: &str, probe: F) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        let cell = self.cell(key);
        *cell.get_or_init(probe).await
    }

    /// Resolve a [`Capability`] to a concrete `bool`.
    ///
    /// `Supported` and `Experimental` are available without a probe;
    /// `Unsupported` is not; only `SupportedAfterProbe` runs (and caches) the
    /// `probe` closure. `probe` is never even constructed by the caller's hot
    /// path when the capability is statically known — pass it as a closure.
    pub async fn resolve_capability<F, Fut>(
        &self,
        key: &str,
        capability: Capability,
        probe: F,
    ) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        match capability {
            Capability::Supported | Capability::Experimental => true,
            Capability::Unsupported => false,
            Capability::SupportedAfterProbe => self.resolve(key, probe).await,
        }
    }

    /// Number of distinct keys probed so far. Diagnostic only.
    pub fn probed_keys(&self) -> usize {
        self.cells
            .lock()
            .expect("probe cache mutex poisoned")
            .values()
            .filter(|cell| cell.initialized())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn resolve_runs_the_probe_exactly_once_under_concurrency() {
        let cache = Arc::new(SessionProbeCache::new());
        let runs = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let cache = Arc::clone(&cache);
            let runs = Arc::clone(&runs);
            tasks.push(tokio::spawn(async move {
                cache
                    .resolve("webdav:range", || async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        true
                    })
                    .await
            }));
        }

        for task in tasks {
            assert!(task.await.unwrap(), "every caller sees the probe result");
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "16 concurrent callers must trigger exactly one probe"
        );
        assert_eq!(cache.probed_keys(), 1);
    }

    #[tokio::test]
    async fn a_later_call_returns_the_cached_value_without_probing() {
        let cache = SessionProbeCache::new();
        let runs = AtomicUsize::new(0);

        let first = cache
            .resolve("k", || async {
                runs.fetch_add(1, Ordering::SeqCst);
                false
            })
            .await;
        let second = cache
            .resolve("k", || async {
                runs.fetch_add(1, Ordering::SeqCst);
                true // a different result the cache must never reach
            })
            .await;

        assert!(!first);
        assert!(!second, "the cached false wins over the second closure");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn distinct_keys_probe_independently() {
        let cache = SessionProbeCache::new();
        let runs = AtomicUsize::new(0);

        for key in ["a", "b", "c"] {
            cache
                .resolve(key, || async {
                    runs.fetch_add(1, Ordering::SeqCst);
                    true
                })
                .await;
        }

        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert_eq!(cache.probed_keys(), 3);
    }

    #[tokio::test]
    async fn resolve_capability_short_circuits_static_capabilities() {
        let cache = SessionProbeCache::new();
        let probe_ran = AtomicUsize::new(0);
        let probe = || async {
            probe_ran.fetch_add(1, Ordering::SeqCst);
            true
        };

        assert!(
            cache
                .resolve_capability("k1", Capability::Supported, probe)
                .await
        );
        assert!(
            cache
                .resolve_capability("k2", Capability::Experimental, || async { false })
                .await
        );
        assert!(
            !cache
                .resolve_capability("k3", Capability::Unsupported, || async { true })
                .await
        );
        assert_eq!(
            probe_ran.load(Ordering::SeqCst),
            0,
            "static capabilities never construct or run the probe"
        );
        assert_eq!(cache.probed_keys(), 0);
    }

    #[tokio::test]
    async fn resolve_capability_probes_only_for_supported_after_probe() {
        let cache = SessionProbeCache::new();
        let probe_ran = AtomicUsize::new(0);

        let resolved = cache
            .resolve_capability("webdav:range", Capability::SupportedAfterProbe, || async {
                probe_ran.fetch_add(1, Ordering::SeqCst);
                true
            })
            .await;

        assert!(resolved);
        assert_eq!(probe_ran.load(Ordering::SeqCst), 1);
    }
}
