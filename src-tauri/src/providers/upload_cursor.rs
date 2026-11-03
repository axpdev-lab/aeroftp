//! One open file for every retry of a streamed upload.
//!
//! `try_clone` shares the kernel offset, so a retry that seeks while the
//! refused body is still reading can send a short or mixed body. Each attempt
//! takes the next epoch and waits until that read has released the cursor,
//! then seeks to the start. The abandoned reader returns
//! [`std::io::ErrorKind::Interrupted`] instead of another byte. The path stays
//! the one opened for this upload.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::io::{Read, Seek, SeekFrom};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Shared read cursor for one upload's open file.
#[derive(Clone)]
pub(crate) struct SharedUploadCursor {
    file: Arc<Mutex<std::fs::File>>,
    epoch: Arc<AtomicU64>,
    /// Test-only pause, in milliseconds, taken while the cursor lock is held,
    /// so a retry can be shown to wait out an in-flight read.
    #[cfg(test)]
    read_hold_ms: Arc<AtomicU64>,
    #[cfg(test)]
    pub(crate) hold_entered: Arc<AtomicBool>,
    /// Set while the test hold still owns the cursor lock, before it lets go.
    #[cfg(test)]
    pub(crate) hold_released: Arc<AtomicBool>,
    /// Set when a later attempt acquires the cursor after that release.
    #[cfg(test)]
    pub(crate) acquired_after_hold: Arc<AtomicBool>,
}

fn cursor_taken() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "upload retry took the file cursor",
    )
}

impl SharedUploadCursor {
    pub(crate) async fn from_tokio(file: tokio::fs::File) -> Self {
        Self {
            file: Arc::new(Mutex::new(file.into_std().await)),
            epoch: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            read_hold_ms: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            hold_entered: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            hold_released: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            acquired_after_hold: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_read_hold_ms(&self, ms: u64) {
        self.read_hold_ms.store(ms, Ordering::Relaxed);
    }

    /// Claim the cursor and rewind it. A newer attempt that already claimed
    /// it does not get seeked back by this one.
    async fn claim(&self) -> std::io::Result<u64> {
        let file = Arc::clone(&self.file);
        let epoch = Arc::clone(&self.epoch);
        #[cfg(test)]
        let hold_released = Arc::clone(&self.hold_released);
        #[cfg(test)]
        let acquired_after_hold = Arc::clone(&self.acquired_after_hold);
        match tokio::task::spawn_blocking(move || {
            let mine = epoch.fetch_add(1, Ordering::AcqRel) + 1;
            let mut guard = file.lock().unwrap_or_else(|poison| poison.into_inner());
            #[cfg(test)]
            if hold_released.load(Ordering::SeqCst) {
                acquired_after_hold.store(true, Ordering::SeqCst);
            }
            if epoch.load(Ordering::Acquire) != mine {
                return Err(cursor_taken());
            }
            guard.seek(SeekFrom::Start(0))?;
            Ok(mine)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("upload rewind task stopped")),
        }
    }

    pub(crate) async fn attempt_stream(
        &self,
        file_size: u64,
    ) -> std::io::Result<
        std::pin::Pin<
            Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>,
        >,
    > {
        let mine = self.claim().await?;
        let file = Arc::clone(&self.file);
        let epoch = Arc::clone(&self.epoch);
        #[cfg(test)]
        let read_hold_ms = Arc::clone(&self.read_hold_ms);
        #[cfg(test)]
        let hold_entered = Arc::clone(&self.hold_entered);
        #[cfg(test)]
        let hold_released = Arc::clone(&self.hold_released);
        let stream = futures_util::stream::unfold(Some(0u64), move |state| {
            let file = Arc::clone(&file);
            let epoch = Arc::clone(&epoch);
            #[cfg(test)]
            let read_hold_ms = Arc::clone(&read_hold_ms);
            #[cfg(test)]
            let hold_entered = Arc::clone(&hold_entered);
            #[cfg(test)]
            let hold_released = Arc::clone(&hold_released);
            async move {
                let offset = state?;
                if file_size == 0 || offset >= file_size {
                    return None;
                }
                let want = (64 * 1024).min((file_size - offset) as usize);
                #[cfg(test)]
                let hold_ms = read_hold_ms.load(Ordering::Relaxed);
                let read = tokio::task::spawn_blocking(move || {
                    let mut guard = file.lock().unwrap_or_else(|poison| poison.into_inner());
                    if epoch.load(Ordering::Acquire) != mine {
                        return Err(cursor_taken());
                    }
                    #[cfg(test)]
                    if hold_ms > 0 {
                        hold_entered.store(true, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(hold_ms));
                        // Still holding the cursor. A retry blocked on this
                        // lock observes the release only after this store.
                        hold_released.store(true, Ordering::SeqCst);
                        if epoch.load(Ordering::Acquire) != mine {
                            return Err(cursor_taken());
                        }
                    }
                    let mut buf = vec![0u8; want];
                    let n = guard.read(&mut buf)?;
                    if n == 0 {
                        return Ok(None);
                    }
                    buf.truncate(n);
                    Ok(Some(bytes::Bytes::from(buf)))
                })
                .await;
                match read {
                    Ok(Ok(Some(chunk))) => {
                        let next = offset + chunk.len() as u64;
                        Some((Ok(chunk), Some(next)))
                    }
                    Ok(Ok(None)) => None,
                    Ok(Err(error)) => Some((Err(error), None)),
                    Err(_) => Some((Err(std::io::Error::other("upload read task stopped")), None)),
                }
            }
        });
        Ok(Box::pin(stream))
    }

    pub(crate) async fn attempt_body(
        &self,
        progress: &super::upload_progress::UploadProgress,
        file_size: u64,
    ) -> std::io::Result<reqwest::Body> {
        let stream = self.attempt_stream(file_size).await?;
        let paced = crate::transfer_dag::throttle::throttle_stream(
            stream,
            crate::transfer_dag::governor::TransferDirection::Upload,
        );
        Ok(reqwest::Body::wrap_stream(progress.track(paced)))
    }
}

#[cfg(test)]
pub(crate) mod replacement_fixture {
    use std::sync::{Arc, Mutex};
    pub(crate) type Received = Arc<Mutex<Vec<Vec<u8>>>>;

    pub(crate) async fn serve(
        source: &std::path::Path,
        success_body: String,
    ) -> (String, Received, tokio::task::JoinHandle<()>) {
        let source = source.to_path_buf();
        let replacement = vec![b'X'; std::fs::read(&source).unwrap().len()];
        let received: Received = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&received);
        let app = axum::Router::new().fallback(axum::routing::any(move |body: bytes::Bytes| {
            let source = source.clone();
            let replacement = replacement.clone();
            let success_body = success_body.clone();
            let seen = Arc::clone(&seen);
            async move {
                let first = {
                    let mut bodies = seen.lock().unwrap();
                    bodies.push(body.to_vec());
                    bodies.len() == 1
                };
                if first {
                    let aside = source.with_extension("replacement");
                    std::fs::write(&aside, replacement).unwrap();
                    std::fs::rename(aside, source).unwrap();
                }
                (
                    if first {
                        axum::http::StatusCode::TOO_MANY_REQUESTS
                    } else {
                        axum::http::StatusCode::OK
                    },
                    [("retry-after", "0")],
                    success_body,
                )
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, received, server)
    }
}
