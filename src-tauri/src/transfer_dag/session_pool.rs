// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::ftp_session_pool::{FtpSessionLease, FtpSessionPool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLeaseKind {
    LegacySingle,
    Ftp,
    HttpClone,
    Sftp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionLeaseId(u64);

impl SessionLeaseId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPoolCapacity {
    pub kind: SessionLeaseKind,
    pub label: String,
    pub max_leases: usize,
}

impl SessionPoolCapacity {
    fn new(kind: SessionLeaseKind, label: &str, max_leases: usize) -> Self {
        Self {
            kind,
            label: label.to_string(),
            max_leases: max_leases.max(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPoolError {
    Closed {
        kind: SessionLeaseKind,
        label: String,
    },
    Timeout {
        kind: SessionLeaseKind,
        label: String,
    },
    Adapter {
        kind: SessionLeaseKind,
        label: String,
        message: String,
    },
}

impl SessionPoolError {
    fn closed(kind: SessionLeaseKind, label: &str) -> Self {
        Self::Closed {
            kind,
            label: label.to_string(),
        }
    }

    fn adapter(kind: SessionLeaseKind, label: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        let lowered = message.to_lowercase();
        if lowered.contains("timed out") || lowered.contains("timeout") {
            return Self::Timeout {
                kind,
                label: label.to_string(),
            };
        }
        if lowered.contains("closed") {
            return Self::Closed {
                kind,
                label: label.to_string(),
            };
        }
        Self::Adapter {
            kind,
            label: label.to_string(),
            message,
        }
    }
}

impl fmt::Display for SessionPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed { kind, label } => {
                write!(f, "{kind:?} session pool '{label}' is closed")
            }
            Self::Timeout { kind, label } => {
                write!(f, "{kind:?} session pool '{label}' timed out")
            }
            Self::Adapter {
                kind,
                label,
                message,
            } => write!(f, "{kind:?} session pool '{label}' failed: {message}"),
        }
    }
}

impl std::error::Error for SessionPoolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLeaseInfo {
    pub id: SessionLeaseId,
    pub kind: SessionLeaseKind,
    pub label: String,
}

impl SessionLeaseInfo {
    fn new(id: SessionLeaseId, kind: SessionLeaseKind, label: &str) -> Self {
        Self {
            id,
            kind,
            label: label.to_string(),
        }
    }
}

#[async_trait]
pub trait TransferSessionPool: Send + Sync {
    async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError>;
    fn label(&self) -> &str;
    fn kind(&self) -> SessionLeaseKind;
    fn capacity(&self) -> SessionPoolCapacity;
}

#[derive(Clone)]
pub struct TransferSessionPoolHandle {
    inner: Arc<dyn TransferSessionPool>,
}

impl TransferSessionPoolHandle {
    pub fn legacy_single(label: impl Into<String>) -> Self {
        Self::new(SingleSessionPool::new(label))
    }

    pub fn http_clone(label: impl Into<String>, capacity: usize) -> Self {
        Self::new(SemaphoreSessionPool::with_capacity(
            SessionLeaseKind::HttpClone,
            label,
            capacity,
        ))
    }

    pub fn ftp(label: impl Into<String>, pool: Arc<FtpSessionPool>) -> Self {
        Self::new(FtpSessionPoolAdapter::new(label, pool))
    }

    pub fn executor_managed_ftp(label: impl Into<String>, capacity: usize) -> Self {
        Self::new(ExecutorManagedSessionPool::new(
            SessionLeaseKind::Ftp,
            label,
            capacity,
        ))
    }

    pub fn new(pool: impl TransferSessionPool + 'static) -> Self {
        Self {
            inner: Arc::new(pool),
        }
    }

    pub async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError> {
        self.inner.acquire().await
    }

    pub fn capacity(&self) -> SessionPoolCapacity {
        self.inner.capacity()
    }

    pub fn kind(&self) -> SessionLeaseKind {
        self.inner.kind()
    }

    pub fn label(&self) -> &str {
        self.inner.label()
    }
}

impl fmt::Debug for TransferSessionPoolHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransferSessionPoolHandle")
            .field("capacity", &self.capacity())
            .finish()
    }
}

pub struct TransferSessionLease {
    inner: TransferSessionLeaseInner,
}

enum TransferSessionLeaseInner {
    Semaphore(SemaphoreSessionLease),
    Ftp(FtpPoolSessionLease),
    ExecutorManaged(SessionLeaseInfo),
}

impl TransferSessionLease {
    pub fn info(&self) -> &SessionLeaseInfo {
        match &self.inner {
            TransferSessionLeaseInner::Semaphore(lease) => &lease.info,
            TransferSessionLeaseInner::Ftp(lease) => &lease.info,
            TransferSessionLeaseInner::ExecutorManaged(info) => info,
        }
    }

    pub fn kind(&self) -> SessionLeaseKind {
        self.info().kind
    }

    pub fn into_ftp(self) -> Result<FtpPoolSessionLease, Self> {
        match self.inner {
            TransferSessionLeaseInner::Ftp(lease) => Ok(lease),
            inner => Err(Self { inner }),
        }
    }
}

struct LeaseIdSource {
    next: AtomicU64,
}

impl LeaseIdSource {
    fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    fn next(&self) -> SessionLeaseId {
        SessionLeaseId(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

pub type SingleSessionPool = SemaphoreSessionPool;

pub struct SemaphoreSessionPool {
    kind: SessionLeaseKind,
    label: String,
    semaphore: Arc<Semaphore>,
    capacity: usize,
    ids: LeaseIdSource,
}

pub struct SemaphoreSessionLease {
    info: SessionLeaseInfo,
    _permit: Option<OwnedSemaphorePermit>,
}

impl SemaphoreSessionPool {
    pub fn new(label: impl Into<String>) -> Self {
        Self::with_capacity(SessionLeaseKind::LegacySingle, label, 1)
    }

    fn with_capacity(kind: SessionLeaseKind, label: impl Into<String>, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            kind,
            label: label.into(),
            semaphore: Arc::new(Semaphore::new(capacity)),
            capacity,
            ids: LeaseIdSource::new(),
        }
    }
}

#[async_trait]
impl TransferSessionPool for SemaphoreSessionPool {
    async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SessionPoolError::closed(self.kind, &self.label))?;
        Ok(TransferSessionLease {
            inner: TransferSessionLeaseInner::Semaphore(SemaphoreSessionLease {
                info: SessionLeaseInfo::new(self.ids.next(), self.kind, &self.label),
                _permit: Some(permit),
            }),
        })
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> SessionLeaseKind {
        self.kind
    }

    fn capacity(&self) -> SessionPoolCapacity {
        SessionPoolCapacity::new(self.kind, &self.label, self.capacity)
    }
}

pub struct FtpSessionPoolAdapter {
    label: String,
    pool: Arc<FtpSessionPool>,
    ids: LeaseIdSource,
}

pub struct FtpPoolSessionLease {
    info: SessionLeaseInfo,
    lease: Option<FtpSessionLease>,
}

impl FtpSessionPoolAdapter {
    pub fn new(label: impl Into<String>, pool: Arc<FtpSessionPool>) -> Self {
        Self {
            label: label.into(),
            pool,
            ids: LeaseIdSource::new(),
        }
    }
}

#[async_trait]
impl TransferSessionPool for FtpSessionPoolAdapter {
    async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError> {
        let lease = self.pool.acquire().await.map_err(|error| {
            SessionPoolError::adapter(SessionLeaseKind::Ftp, &self.label, error)
        })?;
        Ok(TransferSessionLease {
            inner: TransferSessionLeaseInner::Ftp(FtpPoolSessionLease {
                info: SessionLeaseInfo::new(self.ids.next(), SessionLeaseKind::Ftp, &self.label),
                lease: Some(lease),
            }),
        })
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> SessionLeaseKind {
        SessionLeaseKind::Ftp
    }

    fn capacity(&self) -> SessionPoolCapacity {
        SessionPoolCapacity::new(
            SessionLeaseKind::Ftp,
            &self.label,
            self.pool.total_sessions(),
        )
    }
}

impl FtpPoolSessionLease {
    pub fn info(&self) -> &SessionLeaseInfo {
        &self.info
    }

    pub fn manager(&self) -> Option<Arc<tokio::sync::Mutex<crate::ftp::FtpManager>>> {
        self.lease.as_ref().and_then(FtpSessionLease::manager)
    }

    pub async fn ensure_healthy(&self) -> Result<(), SessionPoolError> {
        self.lease
            .as_ref()
            .ok_or_else(|| SessionPoolError::closed(SessionLeaseKind::Ftp, &self.info.label))?
            .ensure_healthy()
            .await
            .map_err(|error| {
                SessionPoolError::adapter(SessionLeaseKind::Ftp, &self.info.label, error)
            })
    }

    pub async fn reset_state(&self) -> Result<(), SessionPoolError> {
        self.lease
            .as_ref()
            .ok_or_else(|| SessionPoolError::closed(SessionLeaseKind::Ftp, &self.info.label))?
            .reset_state()
            .await
            .map_err(|error| {
                SessionPoolError::adapter(SessionLeaseKind::Ftp, &self.info.label, error)
            })
    }

    pub async fn release(mut self) -> Result<(), SessionPoolError> {
        match self.lease.take() {
            Some(lease) => lease.release().await.map_err(|error| {
                SessionPoolError::adapter(SessionLeaseKind::Ftp, &self.info.label, error)
            }),
            None => Ok(()),
        }
    }
}

pub struct ExecutorManagedSessionPool {
    kind: SessionLeaseKind,
    label: String,
    capacity: usize,
    ids: LeaseIdSource,
}

impl ExecutorManagedSessionPool {
    fn new(kind: SessionLeaseKind, label: impl Into<String>, capacity: usize) -> Self {
        Self {
            kind,
            label: label.into(),
            capacity: capacity.max(1),
            ids: LeaseIdSource::new(),
        }
    }
}

#[async_trait]
impl TransferSessionPool for ExecutorManagedSessionPool {
    async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError> {
        Ok(TransferSessionLease {
            inner: TransferSessionLeaseInner::ExecutorManaged(SessionLeaseInfo::new(
                self.ids.next(),
                self.kind,
                &self.label,
            )),
        })
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> SessionLeaseKind {
        self.kind
    }

    fn capacity(&self) -> SessionPoolCapacity {
        SessionPoolCapacity::new(self.kind, &self.label, self.capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn single_session_pool_is_a_real_single_lease_gate() {
        let pool = SingleSessionPool::new("legacy");
        let lease = pool.acquire().await.unwrap();
        assert_eq!(pool.semaphore.available_permits(), 0);
        drop(lease);
        assert_eq!(pool.semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn http_clone_pool_reports_capacity_and_uses_lightweight_leases() {
        let pool = TransferSessionPoolHandle::http_clone("webdav", 4);
        let lease = pool.acquire().await.unwrap();

        assert_eq!(pool.capacity().max_leases, 4);
        assert_eq!(lease.kind(), SessionLeaseKind::HttpClone);
        assert_eq!(lease.info().label, "webdav");
    }

    #[tokio::test]
    async fn executor_managed_ftp_lease_preserves_capacity_without_double_acquire() {
        let pool = TransferSessionPoolHandle::executor_managed_ftp("ftp-batch", 3);
        let lease = pool.acquire().await.unwrap();

        assert_eq!(pool.capacity().kind, SessionLeaseKind::Ftp);
        assert_eq!(pool.capacity().max_leases, 3);
        assert_eq!(lease.kind(), SessionLeaseKind::Ftp);
    }
}
