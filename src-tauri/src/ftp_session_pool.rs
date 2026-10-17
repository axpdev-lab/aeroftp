// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! FTP session pool for future GUI transfer executors.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use secrecy::ExposeSecret;
use tokio::sync::{Mutex, Notify};

use crate::ftp::{FtpConnectionSpec, FtpManager};

#[derive(Debug, Clone)]
pub struct FtpPoolConfig {
    pub connection: FtpConnectionSpec,
    pub pool_size: usize,
    pub min_ready_sessions: usize,
    pub acquire_timeout_ms: u64,
}

impl FtpPoolConfig {
    pub fn from_connection(
        connection: FtpConnectionSpec,
        pool_size: usize,
        min_ready_sessions: usize,
        acquire_timeout_ms: u64,
    ) -> Self {
        Self {
            connection,
            pool_size,
            min_ready_sessions,
            acquire_timeout_ms,
        }
    }

    pub fn from_manager(
        manager: &FtpManager,
        pool_size: usize,
        min_ready_sessions: usize,
        acquire_timeout_ms: u64,
    ) -> Result<Self, String> {
        let connection = manager
            .connection_spec()
            .map_err(|e| format!("Cannot derive FTP pool config from manager: {}", e))?;

        Ok(Self::from_connection(
            connection,
            pool_size,
            min_ready_sessions,
            acquire_timeout_ms,
        ))
    }

    pub fn validated(mut self) -> Self {
        self.pool_size = self.pool_size.clamp(1, 8);
        self.min_ready_sessions = self.min_ready_sessions.clamp(1, self.pool_size);
        if self.acquire_timeout_ms == 0 {
            self.acquire_timeout_ms = 30_000;
        }
        self
    }
}

#[derive(Clone)]
struct PooledSession {
    id: usize,
    manager: Arc<Mutex<FtpManager>>,
}

struct FtpSessionPoolInner {
    config: FtpPoolConfig,
    available: StdMutex<VecDeque<PooledSession>>,
    all_sessions: Vec<PooledSession>,
    closed: AtomicBool,
    notify: Notify,
}

#[derive(Clone)]
pub struct FtpSessionPool {
    inner: Arc<FtpSessionPoolInner>,
}

pub struct FtpSessionLease {
    inner: Arc<FtpSessionPoolInner>,
    session: Option<PooledSession>,
    /// `true` once `release()` has completed: signals that the session was
    /// returned through the async reset path. If `Drop` runs with this still
    /// `false`, the pool must not trust the session state and must run an
    /// async reset (or disconnect) before anyone else can acquire it.
    released: bool,
}

impl FtpSessionPool {
    pub async fn create(config: FtpPoolConfig) -> Result<Self, String> {
        let config = config.validated();
        let mut available = VecDeque::with_capacity(config.pool_size);
        let mut all_sessions = Vec::with_capacity(config.pool_size);

        for index in 0..config.pool_size {
            match create_connected_session(&config).await {
                Ok(manager) => {
                    let pooled = PooledSession {
                        id: index,
                        manager: Arc::new(Mutex::new(manager)),
                    };
                    available.push_back(pooled.clone());
                    all_sessions.push(pooled);
                }
                Err(error) => {
                    if all_sessions.len() < config.min_ready_sessions {
                        return Err(format!(
                            "FTP pool creation failed before minimum ready sessions were available (session {}): {}",
                            index + 1,
                            error
                        ));
                    }
                    tracing::warn!("FTP pool session {} unavailable: {}", index + 1, error);
                    continue;
                }
            }
        }

        if all_sessions.len() < config.min_ready_sessions {
            return Err(format!(
                "FTP pool created only {} sessions, below minimum {}",
                all_sessions.len(),
                config.min_ready_sessions
            ));
        }

        Ok(Self {
            inner: Arc::new(FtpSessionPoolInner {
                config,
                available: StdMutex::new(available),
                all_sessions,
                closed: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        })
    }

    /// Check out one session that passes health and cwd reset before the deadline.
    /// Checkout I/O shares that deadline. A failed session stays off the queue
    /// until a background repair reconnects it.
    pub async fn acquire(&self) -> Result<FtpSessionLease, String> {
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(self.inner.config.acquire_timeout_ms);
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err("Timed out waiting for an FTP session lease".to_string());
            }

            let notified = self.inner.notify.notified();
            tokio::pin!(notified);

            let session = {
                let mut queue = self
                    .inner
                    .available
                    .lock()
                    .map_err(|_| "FTP pool mutex poisoned".to_string())?;
                if self.inner.closed.load(Ordering::Relaxed) {
                    return Err("FTP pool is closed".to_string());
                }
                notified.as_mut().enable();
                queue.pop_front()
            };

            if let Some(session) = session {
                let mut lease = FtpSessionLease {
                    inner: self.inner.clone(),
                    session: Some(session),
                    released: false,
                };
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    lease.discard_broken();
                    return Err("Timed out waiting for an FTP session lease".to_string());
                }
                match tokio::time::timeout(remaining, lease.checkout()).await {
                    Ok(Ok(())) => return Ok(lease),
                    Ok(Err(error)) => {
                        tracing::warn!(
                            "FTP pooled session {:?} failed checkout ({error}); repairing it",
                            lease.session_id()
                        );
                        lease.discard_broken();
                        continue;
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            "FTP pooled session {:?} checkout exceeded the acquire deadline",
                            lease.session_id()
                        );
                        lease.discard_broken();
                        return Err("Timed out waiting for an FTP session lease".to_string());
                    }
                }
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err("Timed out waiting for an FTP session lease".to_string());
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return Err("Timed out waiting for an FTP session lease".to_string());
            }
        }
    }

    pub fn available_count(&self) -> Result<usize, String> {
        self.inner
            .available
            .lock()
            .map(|queue| queue.len())
            .map_err(|_| "FTP pool mutex poisoned".to_string())
    }

    pub fn total_sessions(&self) -> usize {
        self.inner.all_sessions.len()
    }

    pub fn config(&self) -> FtpPoolConfig {
        self.inner.config.clone()
    }

    /// Test-only pool with zero sessions: `acquire` can never succeed and
    /// times out after `acquire_timeout_ms`, giving executor tests a
    /// deterministic acquire-failure fixture without a live FTP server.
    #[cfg(test)]
    pub(crate) fn empty_for_test(acquire_timeout_ms: u64) -> Self {
        let config = FtpPoolConfig {
            connection: FtpConnectionSpec {
                server: "127.0.0.1:1".to_string(),
                username: "user".to_string(),
                password: secrecy::SecretString::from("unused".to_string()),
                initial_path: "/".to_string(),
                timeouts: crate::ftp::FtpTimeoutConfig::default(),
            },
            pool_size: 1,
            min_ready_sessions: 0,
            acquire_timeout_ms,
        }
        .validated();
        Self {
            inner: Arc::new(FtpSessionPoolInner {
                config,
                available: StdMutex::new(VecDeque::new()),
                all_sessions: Vec::new(),
                closed: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    /// One disconnected session plus a short acquire deadline. Checkout must
    /// discard it and then time out, not return the health error at once.
    #[cfg(test)]
    pub(crate) fn unhealthy_for_test(acquire_timeout_ms: u64) -> Self {
        let timeouts = crate::ftp::FtpTimeoutConfig {
            connect_timeout: Duration::from_millis(20),
            disconnect_timeout: Duration::from_millis(20),
            command_timeout: Duration::from_millis(20),
            ..crate::ftp::FtpTimeoutConfig::default()
        };
        let config = FtpPoolConfig {
            connection: FtpConnectionSpec {
                server: "127.0.0.1:1".to_string(),
                username: "user".to_string(),
                password: secrecy::SecretString::from("unused".to_string()),
                initial_path: "/".to_string(),
                timeouts,
            },
            pool_size: 1,
            min_ready_sessions: 0,
            acquire_timeout_ms,
        }
        .validated();
        let session = PooledSession {
            id: 0,
            manager: Arc::new(Mutex::new(FtpManager::new())),
        };
        let mut available = VecDeque::new();
        available.push_back(session.clone());
        Self {
            inner: Arc::new(FtpSessionPoolInner {
                config,
                available: StdMutex::new(available),
                all_sessions: vec![session],
                closed: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    /// One session on `server`, which accepts the connection and never sends
    /// the greeting, with a short acquire deadline. The connect hangs until
    /// its 5 s timeout, so checkout must stop at the deadline instead.
    #[cfg(test)]
    pub(crate) fn silent_server_for_test(server: &str, acquire_timeout_ms: u64) -> Self {
        let timeouts = crate::ftp::FtpTimeoutConfig {
            connect_timeout: Duration::from_secs(5),
            ..crate::ftp::FtpTimeoutConfig::default()
        };
        let config = FtpPoolConfig {
            connection: FtpConnectionSpec {
                server: server.to_string(),
                username: "user".to_string(),
                password: secrecy::SecretString::from("unused".to_string()),
                initial_path: "/".to_string(),
                timeouts,
            },
            pool_size: 1,
            min_ready_sessions: 0,
            acquire_timeout_ms,
        }
        .validated();
        let session = PooledSession {
            id: 0,
            manager: Arc::new(Mutex::new(FtpManager::new())),
        };
        let mut available = VecDeque::new();
        available.push_back(session.clone());
        Self {
            inner: Arc::new(FtpSessionPoolInner {
                config,
                available: StdMutex::new(available),
                all_sessions: vec![session],
                closed: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    pub async fn close(&self) -> Result<(), String> {
        self.inner.closed.store(true, Ordering::Relaxed);
        self.inner.notify.notify_waiters();

        let available_sessions = {
            let mut queue = self
                .inner
                .available
                .lock()
                .map_err(|_| "FTP pool mutex poisoned".to_string())?;
            queue.drain(..).collect::<Vec<_>>()
        };

        drop(available_sessions);

        for session in &self.inner.all_sessions {
            match session.manager.try_lock() {
                Ok(mut manager) => {
                    manager.disconnect().await.map_err(|e| {
                        format!(
                            "Failed to disconnect FTP pooled session {}: {}",
                            session.id, e
                        )
                    })?;
                }
                Err(_) => {
                    tracing::warn!(
                        "FTP pooled session {} still busy during shutdown; it will disconnect on release",
                        session.id
                    );
                }
            }
        }

        let queue = self
            .inner
            .available
            .lock()
            .map_err(|_| "FTP pool mutex poisoned".to_string())?;
        if !queue.is_empty() {
            tracing::warn!(
                "FTP pool close completed with {} session(s) returned after shutdown started",
                queue.len()
            );
        }
        Ok(())
    }
}

impl FtpSessionLease {
    pub fn session_id(&self) -> Option<usize> {
        self.session.as_ref().map(|session| session.id)
    }

    pub fn manager(&self) -> Option<Arc<Mutex<FtpManager>>> {
        self.session.as_ref().map(|session| session.manager.clone())
    }

    async fn checkout(&self) -> Result<(), String> {
        self.ensure_healthy().await?;
        self.reset_state().await
    }

    pub async fn ensure_healthy(&self) -> Result<(), String> {
        let manager = self
            .manager()
            .ok_or("FTP session lease is no longer valid".to_string())?;
        let mut manager = manager.lock().await;

        if manager.is_connected() && manager.noop().await.is_ok() {
            return Ok(());
        }

        let _ = manager.disconnect().await;
        manager.set_timeout_config(self.inner.config.connection.timeouts);
        manager
            .connect(&self.inner.config.connection.server)
            .await
            .map_err(|e| format!("FTP pool reconnect failed: {}", e))?;
        manager
            .login(
                &self.inner.config.connection.username,
                self.inner.config.connection.password.expose_secret(),
            )
            .await
            .map_err(|e| format!("FTP pool re-login failed: {}", e))?;

        if !self.inner.config.connection.initial_path.is_empty() {
            manager
                .change_dir(&self.inner.config.connection.initial_path)
                .await
                .map_err(|e| format!("FTP pool reset path failed: {}", e))?;
        }

        Ok(())
    }

    pub async fn reset_state(&self) -> Result<(), String> {
        let manager = self
            .manager()
            .ok_or("FTP session lease is no longer valid".to_string())?;
        let mut manager = manager.lock().await;
        let target = if self.inner.config.connection.initial_path.is_empty() {
            "/"
        } else {
            &self.inner.config.connection.initial_path
        };
        manager
            .change_dir(target)
            .await
            .map_err(|e| format!("FTP pooled session reset failed: {}", e))
    }

    pub async fn release(mut self) -> Result<(), String> {
        let reset_result = self.reset_state().await;
        if reset_result.is_ok() {
            self.released = true;
            self.return_to_pool_clean();
        } else {
            self.released = true;
            if let Some(session) = self.session.take() {
                schedule_repair(Arc::clone(&self.inner), session);
            }
        }
        reset_result
    }

    /// Push the session back into the available queue without resetting state.
    /// Only safe to call after `reset_state()` succeeded.
    fn return_to_pool_clean(&mut self) {
        if let Some(session) = self.session.take() {
            if self.inner.closed.load(Ordering::Relaxed) {
                self.spawn_disconnect(session);
                return;
            }
            if let Ok(mut queue) = self.inner.available.lock() {
                queue.push_back(session);
                self.inner.notify.notify_one();
            }
        }
    }

    async fn disconnect_session(&mut self) {
        if let Some(session) = self.session.take() {
            let manager = session.manager.clone();
            let mut manager = manager.lock().await;
            if let Err(error) = manager.disconnect().await {
                tracing::warn!(
                    "FTP pooled session {} disconnect failed: {}",
                    session.id,
                    error
                );
            }
        }
    }

    /// Keep the broken session off the queue. `Drop` would requeue it and the
    /// next loop would pop it again. A background repair puts it back once it
    /// reconnects.
    fn discard_broken(&mut self) {
        self.released = true;
        if let Some(session) = self.session.take() {
            schedule_repair(Arc::clone(&self.inner), session);
        }
    }

    fn spawn_disconnect(&self, session: PooledSession) {
        let manager = session.manager.clone();
        let session_id = session.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut manager = manager.lock().await;
                if let Err(error) = manager.disconnect().await {
                    tracing::warn!(
                        "Failed to disconnect FTP pooled session {} during shutdown: {}",
                        session_id,
                        error
                    );
                }
            });
        } else {
            tracing::warn!(
                "Dropping FTP pooled session {} after shutdown without async runtime",
                session_id
            );
        }
    }
}

fn schedule_repair(inner: Arc<FtpSessionPoolInner>, session: PooledSession) {
    let Some(handle) = tokio::runtime::Handle::try_current().ok() else {
        tracing::warn!(
            "FTP pooled session {} dropped without a runtime to repair it",
            session.id
        );
        return;
    };
    handle.spawn(async move {
        let mut delay = Duration::from_millis(200);
        loop {
            if inner.closed.load(Ordering::Relaxed) {
                let mut manager = session.manager.lock().await;
                let _ = manager.disconnect().await;
                return;
            }
            match reconnect_pooled(&inner.config, &session).await {
                Ok(()) => {
                    if inner.closed.load(Ordering::Relaxed) {
                        let mut manager = session.manager.lock().await;
                        let _ = manager.disconnect().await;
                        return;
                    }
                    if let Ok(mut queue) = inner.available.lock() {
                        queue.push_back(session);
                        inner.notify.notify_one();
                    }
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        "FTP pooled session {} repair failed ({error}); retrying",
                        session.id
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
    });
}

async fn reconnect_pooled(config: &FtpPoolConfig, session: &PooledSession) -> Result<(), String> {
    let mut manager = session.manager.lock().await;
    let _ = manager.disconnect().await;
    manager.set_timeout_config(config.connection.timeouts);
    manager
        .connect(&config.connection.server)
        .await
        .map_err(|error| format!("FTP pool reconnect failed: {error}"))?;
    manager
        .login(
            &config.connection.username,
            config.connection.password.expose_secret(),
        )
        .await
        .map_err(|error| format!("FTP pool re-login failed: {error}"))?;
    if !config.connection.initial_path.is_empty() {
        manager
            .change_dir(&config.connection.initial_path)
            .await
            .map_err(|error| format!("FTP pool reset path failed: {error}"))?;
    }
    Ok(())
}

impl Drop for FtpSessionLease {
    /// If the lease was dropped without an explicit `release()`: panic,
    /// `?` propagation, early return: run the async reset on a detached
    /// task. If reset succeeds, return the session; otherwise disconnect.
    /// This is the only way to avoid handing a dirty session to the next
    /// caller without requiring every call site to be panic-safe.
    fn drop(&mut self) {
        if self.released {
            return;
        }

        let Some(session) = self.session.take() else {
            return;
        };

        // Move ownership into an async cleanup task.
        let inner = self.inner.clone();
        let closed = inner.closed.load(Ordering::Relaxed);
        let config = inner.config.clone();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if closed {
                    let mut manager = session.manager.lock().await;
                    let _ = manager.disconnect().await;
                    return;
                }

                // Try reset first; if it works, return the session to the pool.
                let reset_ok = {
                    let mut manager = session.manager.lock().await;
                    let target = if config.connection.initial_path.is_empty() {
                        "/"
                    } else {
                        &config.connection.initial_path
                    };
                    manager.change_dir(target).await.is_ok()
                };

                if reset_ok {
                    if let Ok(mut queue) = inner.available.lock() {
                        queue.push_back(session);
                        inner.notify.notify_one();
                    }
                } else {
                    let mut manager = session.manager.lock().await;
                    let _ = manager.disconnect().await;
                }
            });
        } else {
            // No runtime available: the process is tearing down. The
            // PooledSession drops naturally; the FTP connection will be
            // reset by the server on socket close.
            tracing::warn!(
                "FTP pool lease dropped without async runtime; session {} closed abruptly",
                session.id,
            );
        }
    }
}

async fn create_connected_session(config: &FtpPoolConfig) -> Result<FtpManager, String> {
    let mut manager = FtpManager::new();
    manager.set_timeout_config(config.connection.timeouts);
    manager
        .connect(&config.connection.server)
        .await
        .map_err(|e| format!("FTP pool connect failed: {}", e))?;
    manager
        .login(
            &config.connection.username,
            config.connection.password.expose_secret(),
        )
        .await
        .map_err(|e| format!("FTP pool login failed: {}", e))?;

    if !config.connection.initial_path.is_empty() && config.connection.initial_path != "/" {
        manager
            .change_dir(&config.connection.initial_path)
            .await
            .map_err(|e| format!("FTP pool initial path failed: {}", e))?;
    }

    Ok(manager)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn test_pool_config_validation() {
        let config = FtpPoolConfig {
            connection: FtpConnectionSpec {
                server: "example.com:21".to_string(),
                username: "user".to_string(),
                password: SecretString::from("secret".to_string()),
                initial_path: "/".to_string(),
                timeouts: crate::ftp::FtpTimeoutConfig::default(),
            },
            pool_size: 10,
            min_ready_sessions: 0,
            acquire_timeout_ms: 0,
        }
        .validated();

        assert_eq!(config.pool_size, 8);
        assert_eq!(config.min_ready_sessions, 1);
        assert_eq!(config.acquire_timeout_ms, 30_000);
    }

    #[tokio::test]
    async fn acquire_discards_unhealthy_session_and_waits_for_the_deadline() {
        let pool = FtpSessionPool::unhealthy_for_test(80);
        let started = std::time::Instant::now();
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(1), pool.acquire())
            .await
            .expect("a broken session must not be retried without a deadline");
        let error = match acquired {
            Err(error) => error,
            Ok(_) => panic!("checkout of a dead session is not a lease"),
        };
        assert!(
            error.contains("Timed out"),
            "health failure must stay inside the deadline, not fail the checkout at once: {error}"
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(40),
            "acquire returned before the deadline: {:?}",
            started.elapsed()
        );
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert_eq!(
            pool.available_count().expect("pool mutex"),
            0,
            "a failed checkout must not requeue the session before it reconnects"
        );
        let _ = pool.close().await;
    }

    /// The listener completes the handshake from its backlog and never greets,
    /// so the checkout hangs on every host, whatever its routing. Paused time
    /// moves only to the next timer: a stalled runner cannot stretch the
    /// measurement, and only the deadline (50 ms), the cap (2 s) and the
    /// connect timeout (5 s) can end the wait.
    #[tokio::test(start_paused = true)]
    async fn acquire_bounds_checkout_io_by_the_deadline() {
        const DEADLINE: std::time::Duration = std::time::Duration::from_millis(50);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the silent server");
        let server = listener.local_addr().expect("silent server address");
        let pool = FtpSessionPool::silent_server_for_test(
            &server.to_string(),
            DEADLINE.as_millis() as u64,
        );
        let started = tokio::time::Instant::now();
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), pool.acquire())
            .await
            .expect("checkout I/O must not run past the test cap");
        let error = match acquired {
            Err(error) => error,
            Ok(_) => panic!("a server that never greets is not a lease"),
        };
        assert!(
            error.contains("Timed out"),
            "checkout that overruns the deadline must time out: {error}"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= DEADLINE && elapsed < DEADLINE * 2,
            "checkout must stop at the acquire deadline ({DEADLINE:?}), it stopped after {elapsed:?}"
        );
        let _ = pool.close().await;
        drop(listener);
    }
}
