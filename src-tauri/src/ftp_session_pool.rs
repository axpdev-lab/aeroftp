// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

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

    pub async fn acquire(&self) -> Result<FtpSessionLease, String> {
        loop {
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
                let lease = FtpSessionLease {
                    inner: self.inner.clone(),
                    session: Some(session),
                };
                lease.ensure_healthy().await?;
                lease.reset_state().await?;
                return Ok(lease);
            }

            let timeout = Duration::from_millis(self.inner.config.acquire_timeout_ms);
            tokio::time::timeout(timeout, notified)
                .await
                .map_err(|_| "Timed out waiting for an FTP session lease".to_string())?;
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
                        format!("Failed to disconnect FTP pooled session {}: {}", session.id, e)
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
        self.return_to_pool();
        reset_result
    }

    fn return_to_pool(&mut self) {
        if let Some(session) = self.session.take() {
            if self.inner.closed.load(Ordering::Relaxed) {
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
                return;
            }

            if let Ok(mut queue) = self.inner.available.lock() {
                queue.push_back(session);
                self.inner.notify.notify_one();
            }
        }
    }
}

impl Drop for FtpSessionLease {
    fn drop(&mut self) {
        self.return_to_pool();
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
}
