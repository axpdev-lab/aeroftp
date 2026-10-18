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
    HttpList,
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

    pub fn http_list_clone(label: impl Into<String>, capacity: usize) -> Self {
        Self::new(SemaphoreSessionPool::with_capacity(
            SessionLeaseKind::HttpList,
            label,
            capacity,
        ))
    }

    pub fn ftp(label: impl Into<String>, pool: Arc<FtpSessionPool>) -> Self {
        Self::new(FtpSessionPoolAdapter::new(label, pool))
    }

    /// Concurrency gate for SFTP file-level parallelism (PD-SFTP-1).
    ///
    /// The real resource is N **independent** SSH connections re-dialled by
    /// `clone_for_transfer()` workers from a retained secure connection
    /// spec (same model the FTP pool uses). This handle only bounds how
    /// many run at once. Labelled `Sftp`, never `HttpClone`, so capacity
    /// and metrics tell the truth about the transport.
    pub fn sftp_connection(label: impl Into<String>, capacity: usize) -> Self {
        Self::new(SemaphoreSessionPool::with_capacity(
            SessionLeaseKind::Sftp,
            label,
            capacity,
        ))
    }

    /// Concurrency gate for FTP file-level / intra-file parallelism
    /// (PD-FTP-1, the FTP mirror of [`Self::sftp_connection`]).
    ///
    /// The real resource is N **independent** FTP connections re-dialled by
    /// `clone_for_transfer()` workers from a retained connection spec. This
    /// handle only bounds how many run at once. Labelled `Ftp` (reusing the
    /// PD-Core-2 lease kind), never `HttpClone`, so capacity and metrics
    /// tell the truth about the transport. Distinct from [`Self::ftp`],
    /// which adapts the GUI's stateful `FtpSessionPool` itself (no duplicate
    /// pool on either side: a `SemaphoreSessionPool` is just the concurrency
    /// gate, not a second stateful pool).
    pub fn ftp_connection(label: impl Into<String>, capacity: usize) -> Self {
        Self::new(SemaphoreSessionPool::with_capacity(
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
}

impl TransferSessionLease {
    pub fn info(&self) -> &SessionLeaseInfo {
        match &self.inner {
            TransferSessionLeaseInner::Semaphore(lease) => &lease.info,
            TransferSessionLeaseInner::Ftp(lease) => &lease.info,
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

/// One DAG checkout is the only wait a whole-file node gets. A timeout is
/// tried this many times, the pre-#1221 default retry budget, so a busy pool
/// does not fail the file after a single wait.
const FTP_CHECKOUT_ATTEMPTS: u32 = 3;

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
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.pool.acquire().await {
                Ok(lease) => {
                    return Ok(TransferSessionLease {
                        inner: TransferSessionLeaseInner::Ftp(FtpPoolSessionLease {
                            info: SessionLeaseInfo::new(
                                self.ids.next(),
                                SessionLeaseKind::Ftp,
                                &self.label,
                            ),
                            lease: Some(lease),
                        }),
                    });
                }
                Err(error) => {
                    let mapped =
                        SessionPoolError::adapter(SessionLeaseKind::Ftp, &self.label, error);
                    let retry = matches!(mapped, SessionPoolError::Timeout { .. })
                        && attempt < FTP_CHECKOUT_ATTEMPTS;
                    if !retry {
                        return Err(mapped);
                    }
                }
            }
        }
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

    /// The pool lease for the executor. `None` if it was already released.
    pub fn into_inner(mut self) -> Option<FtpSessionLease> {
        self.lease.take()
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

/// One session slot. Nobody is admitted until two callers are inside
/// [`TransferSessionPool::acquire`]. The closure runs then, before either
/// permit is granted, so a test can see that the other gate is still free.
///
/// After that, the semaphore is an ordinary capacity-1 session pool.
#[cfg(test)]
pub struct TwoPartySessionPool {
    label: String,
    arrivals: std::sync::atomic::AtomicUsize,
    released: std::sync::atomic::AtomicBool,
    checked: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
    semaphore: Arc<Semaphore>,
    ids: LeaseIdSource,
    on_both_waiting: Box<dyn Fn() + Send + Sync>,
}

#[cfg(test)]
impl TwoPartySessionPool {
    pub fn new(
        label: impl Into<String>,
        on_both_waiting: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            arrivals: std::sync::atomic::AtomicUsize::new(0),
            released: std::sync::atomic::AtomicBool::new(false),
            checked: std::sync::atomic::AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            semaphore: Arc::new(Semaphore::new(1)),
            ids: LeaseIdSource::new(),
            on_both_waiting: Box::new(on_both_waiting),
        }
    }

    async fn wait_until(&self, ready: impl Fn(&Self) -> bool) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if ready(self) {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
#[async_trait]
impl TransferSessionPool for TwoPartySessionPool {
    async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError> {
        self.arrivals
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
        self.wait_until(|pool| pool.arrivals.load(std::sync::atomic::Ordering::SeqCst) >= 2)
            .await;
        if self
            .checked
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            (self.on_both_waiting)();
            self.released
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.notify.notify_waiters();
        }
        self.wait_until(|pool| pool.released.load(std::sync::atomic::Ordering::SeqCst))
            .await;
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SessionPoolError::closed(SessionLeaseKind::HttpClone, &self.label))?;
        Ok(TransferSessionLease {
            inner: TransferSessionLeaseInner::Semaphore(SemaphoreSessionLease {
                info: SessionLeaseInfo::new(
                    self.ids.next(),
                    SessionLeaseKind::HttpClone,
                    &self.label,
                ),
                _permit: Some(permit),
            }),
        })
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> SessionLeaseKind {
        SessionLeaseKind::HttpClone
    }

    fn capacity(&self) -> SessionPoolCapacity {
        SessionPoolCapacity::new(SessionLeaseKind::HttpClone, &self.label, 1)
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

        assert_eq!(pool.capacity().kind, SessionLeaseKind::HttpClone);
        assert_eq!(pool.capacity().max_leases, 4);
        assert_eq!(lease.kind(), SessionLeaseKind::HttpClone);
        assert_eq!(lease.info().label, "webdav");
    }

    #[tokio::test]
    async fn http_list_pool_reports_checker_capacity() {
        let pool = TransferSessionPoolHandle::http_list_clone("s3-list", 3);
        let lease = pool.acquire().await.unwrap();

        assert_eq!(pool.capacity().kind, SessionLeaseKind::HttpList);
        assert_eq!(pool.capacity().max_leases, 3);
        assert_eq!(lease.kind(), SessionLeaseKind::HttpList);
        assert_eq!(lease.info().label, "s3-list");
    }

    #[tokio::test]
    async fn sftp_connection_pool_reports_sftp_kind_not_http() {
        let pool = TransferSessionPoolHandle::sftp_connection("axpbuntu-remote", 4);
        let lease = pool.acquire().await.unwrap();

        assert_eq!(pool.capacity().kind, SessionLeaseKind::Sftp);
        assert_eq!(pool.capacity().max_leases, 4);
        assert_eq!(lease.kind(), SessionLeaseKind::Sftp);
        assert_eq!(lease.info().label, "axpbuntu-remote");
    }

    /// The FTP adapter reports the pool's real capacity and `acquire` really
    /// gates on it: a pool that can never hand out a session fails with its
    /// acquire timeout instead of returning instantly.
    #[tokio::test]
    async fn ftp_adapter_reports_pool_capacity_and_gates_acquire() {
        let pool = Arc::new(FtpSessionPool::empty_for_test(50));
        let handle = TransferSessionPoolHandle::ftp("ftp-batch", Arc::clone(&pool));

        assert_eq!(handle.capacity().kind, SessionLeaseKind::Ftp);
        assert_eq!(
            handle.capacity().max_leases,
            pool.total_sessions().max(1),
            "capacity mirrors the pool's session count"
        );

        let started = std::time::Instant::now();
        let error = handle
            .acquire()
            .await
            .err()
            .expect("an empty pool cannot hand out a lease");
        assert!(
            matches!(error, SessionPoolError::Timeout { .. }),
            "an exhausted pool fails with its acquire timeout: {error}"
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(25),
            "acquire must wait for a real session, not return a descriptive token"
        );
    }

    /// A dead session is discarded inside the deadline, then a timeout is
    /// retried. The file must not fail on the first health error, and the
    /// broken session must not come back onto the queue.
    #[tokio::test]
    async fn ftp_adapter_retries_an_unhealthy_checkout_until_the_budget() {
        let pool = Arc::new(FtpSessionPool::unhealthy_for_test(40));
        let handle = TransferSessionPoolHandle::ftp("ftp-batch", Arc::clone(&pool));
        let started = std::time::Instant::now();
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), handle.acquire())
            .await
            .expect("checkout must finish inside the retry budget");
        let error = match acquired {
            Err(error) => error,
            Ok(_) => panic!("an unhealthy session is not a lease"),
        };
        assert!(
            matches!(error, SessionPoolError::Timeout { .. }),
            "a dead session must surface as the checkout timeout: {error}"
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(90),
            "one health failure must not end the checkout: {:?}",
            started.elapsed()
        );
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert_eq!(
            pool.available_count().expect("pool mutex"),
            0,
            "a failed checkout must not requeue the session"
        );
    }

    /// Runs on tokio's paused clock, which the pool's deadline reads: any wait
    /// for a session would move it, and a loaded runner cannot.
    #[tokio::test(start_paused = true)]
    async fn ftp_adapter_does_not_retry_a_closed_pool() {
        let pool = Arc::new(FtpSessionPool::empty_for_test(500));
        pool.close().await.expect("close");
        let handle = TransferSessionPoolHandle::ftp("ftp-batch", pool);
        let started = tokio::time::Instant::now();
        let error = match handle.acquire().await {
            Err(error) => error,
            Ok(_) => panic!("a closed pool cannot hand out a lease"),
        };
        assert!(
            matches!(error, SessionPoolError::Closed { .. }),
            "closed is not a timeout: {error}"
        );
        assert_eq!(
            tokio::time::Instant::now(),
            started,
            "a closed pool must fail at once, not wait for a session"
        );
    }
}
