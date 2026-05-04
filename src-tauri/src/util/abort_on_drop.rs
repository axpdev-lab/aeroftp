// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! RAII guard that aborts a Tokio task on drop.
//!
//! Raw `tokio::spawn(future)` returns a `JoinHandle<T>`. Dropping the handle
//! does **not** abort the task: it detaches it. Any task that should be
//! bounded by a parent scope (reader loops, progress emitters, OAuth
//! callbacks, plugin hooks) leaks if the handle is dropped.
//!
//! Wrap the handle in `AbortOnDrop<T>` so that when the owning struct or
//! scope drops, the task is cancelled deterministically.
//!
//! ```ignore
//! use crate::util::AbortOnDrop;
//! let task: AbortOnDrop<()> = AbortOnDrop::new(tokio::spawn(async move {
//!     reader_loop().await;
//! }));
//! // when `task` drops, the spawned future is aborted.
//! ```

use std::future::Future;
use tokio::task::{JoinError, JoinHandle};

/// RAII guard that aborts the wrapped Tokio task on drop.
///
/// This is the structural answer to "spawn-without-handle" leaks: if the
/// wrapper outlives nothing, the task cannot leak.
#[must_use = "AbortOnDrop aborts the task as soon as it is dropped; bind it to a name"]
pub struct AbortOnDrop<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    /// Wrap an existing `JoinHandle` so its task is aborted on drop.
    pub fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// Spawn a future and wrap the resulting handle.
    pub fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        Self::new(tokio::spawn(future))
    }

    /// Abort the task eagerly. Subsequent drops are no-ops.
    pub fn abort(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.abort();
        }
    }

    /// Check whether the task has completed.
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true)
    }

    /// Wait for the task to complete. Consumes the guard so drop does not
    /// re-abort. Use this when you need the task's return value.
    pub async fn join(mut self) -> Result<T, JoinError> {
        let handle = self
            .handle
            .take()
            .expect("AbortOnDrop handle already consumed");
        handle.await
    }

    /// Await the task while retaining ownership. Use inside a `tokio::select!`
    /// so that if another branch wins (e.g. a timeout), dropping the guard
    /// still aborts the underlying task.
    ///
    /// `JoinHandle<T>` is `Unpin`, so `&mut JoinHandle` is a valid `Future`.
    pub fn wait(&mut self) -> &mut JoinHandle<T> {
        self.handle
            .as_mut()
            .expect("AbortOnDrop handle already consumed")
    }

    /// Detach the task from this guard, returning the raw handle. Only use
    /// when you are transferring ownership into another guard or truly want
    /// a detached task. Most call sites should prefer `join`.
    pub fn into_handle(mut self) -> JoinHandle<T> {
        self.handle
            .take()
            .expect("AbortOnDrop handle already consumed")
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct MarkOnDrop(Arc<AtomicBool>);
    impl Drop for MarkOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn aborts_on_drop() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mark = MarkOnDrop(cancelled.clone());

        {
            let _guard = AbortOnDrop::spawn(async move {
                let _held = mark;
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
            // Give the task a moment to start.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Allow the abort to propagate and drop to run.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn join_returns_value() {
        let guard = AbortOnDrop::spawn(async { 42u32 });
        assert_eq!(guard.join().await.unwrap(), 42);
    }
}
