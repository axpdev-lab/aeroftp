//! Progress of a streamed upload body, measured on the bytes it hands to the
//! HTTP client.
//!
//! A provider that streams a file as its request body and calls the progress
//! callback only once the response is in shows a bar that jumps from 0 to 100
//! at the end of the transfer: on a 300 MB upload that is minutes of a bar
//! standing still. [`UploadProgress`] wraps the body stream instead. Each chunk
//! is counted when the stream yields it to the HTTP client, after the bandwidth
//! governor has paced it, so the reported figure follows what is being sent,
//! not what has been read ahead from disk.
//!
//! The last byte on the wire is not the upload being accepted: a server can
//! still refuse it, or answer success without storing it. The streamed updates
//! therefore stop short of the total, and `(total, total)` is reported only by
//! [`UploadProgress::complete`], which the provider calls once it has read and
//! checked the acknowledgement. A failed upload never shows a completed one.
//! This is the contract pCloud's single upload established; the helper makes it
//! the shared one.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};

use super::multi_thread::ProgressCallback;
use crate::transfer_dag::governor::TransferDirection;

/// Progress reporting for one streamed upload of `total` bytes.
///
/// The callback is `Send` and not `Sync`, and it is reached from the body
/// stream (on the HTTP client's side) and from the provider (for the final
/// update), so it is shared behind a lock. The two never run at the same time:
/// the final update follows the response, which follows the last chunk.
pub struct UploadProgress {
    callback: Option<Arc<Mutex<ProgressCallback>>>,
    total: u64,
}

impl UploadProgress {
    pub fn new(callback: Option<ProgressCallback>, total: u64) -> Self {
        Self {
            callback: callback.map(|cb| Arc::new(Mutex::new(cb))),
            total,
        }
    }

    /// Wrap a body stream so every chunk it yields reports the running byte
    /// count, while that count is below the total. Errors pass through
    /// uncounted.
    pub fn track<S, E>(&self, stream: S) -> impl Stream<Item = Result<Bytes, E>> + Send + 'static
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: Send + 'static,
    {
        let callback = self.callback.clone();
        let total = self.total;
        let mut sent = 0u64;
        stream.inspect(move |chunk| {
            let Ok(bytes) = chunk else {
                return;
            };
            sent = sent.saturating_add(bytes.len() as u64);
            if sent < total {
                if let Some(callback) = &callback {
                    let callback = callback.lock().unwrap_or_else(|e| e.into_inner());
                    callback(sent, total);
                }
            }
        })
    }

    /// The chunks of `file`: read, paced by the upload governor, then
    /// counted. Nothing is buffered beyond the reader's chunk.
    pub fn file_stream(
        &self,
        file: tokio::fs::File,
    ) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
        let chunks = tokio_util::io::ReaderStream::new(file);
        let paced =
            crate::transfer_dag::throttle::throttle_stream(chunks, TransferDirection::Upload);
        self.track(paced)
    }

    /// [`Self::file_stream`] as a request body.
    pub fn file_body(&self, file: tokio::fs::File) -> reqwest::Body {
        reqwest::Body::wrap_stream(self.file_stream(file))
    }

    /// [`Self::file_body`] over a fresh handle on `path`, for a request built
    /// again on every attempt: a retry sends the whole file again, and its
    /// progress starts over from zero with it. The open is synchronous, as
    /// the request builder is, and a failure to open travels as the body's
    /// error, so that attempt fails instead of going out empty.
    pub fn reopened_file_body(&self, path: &std::path::Path) -> reqwest::Body {
        match std::fs::File::open(path) {
            Ok(file) => self.file_body(tokio::fs::File::from_std(file)),
            Err(error) => reqwest::Body::wrap_stream(futures_util::stream::once(async move {
                Err::<Bytes, std::io::Error>(error)
            })),
        }
    }

    /// Report `(0, total)` before the first byte, for a provider whose bar
    /// has always opened at zero.
    pub fn start(&self) {
        if let Some(callback) = &self.callback {
            let callback = callback.lock().unwrap_or_else(|e| e.into_inner());
            callback(0, self.total);
        }
    }

    /// The server has acknowledged the upload: report `(total, total)`.
    pub fn complete(&self) {
        if let Some(callback) = &self.callback {
            let callback = callback.lock().unwrap_or_else(|e| e.into_inner());
            callback(self.total, self.total);
        }
    }
}

/// Test fixture shared by the providers' upload tests: a local HTTP server
/// whose routes read each request to the end without holding it (a stream
/// upload carries hundreds of MB), a recorder for the progress callback, and
/// the progress contract every streamed upload must keep.
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;

    /// One route: method, path (no query), status and JSON body to answer.
    /// `{base}` in the body becomes the server's own base URL, for an API
    /// that answers with the URL of its next step.
    pub(crate) struct Route {
        pub method: axum::http::Method,
        pub path: &'static str,
        pub status: u16,
        pub body: String,
        /// Answer the first request 503, the later ones as configured.
        pub busy_first: bool,
    }

    /// Every request the server read, in order: route path and body length.
    pub(crate) type Received = Arc<Mutex<Vec<(&'static str, usize)>>>;

    impl Route {
        pub(crate) fn post(path: &'static str, status: u16, body: impl Into<String>) -> Self {
            Self {
                method: axum::http::Method::POST,
                path,
                status,
                body: body.into(),
                busy_first: false,
            }
        }

        pub(crate) fn get(path: &'static str, status: u16, body: impl Into<String>) -> Self {
            Self {
                method: axum::http::Method::GET,
                path,
                status,
                body: body.into(),
                busy_first: false,
            }
        }

        pub(crate) fn busy_first(mut self) -> Self {
            self.busy_first = true;
            self
        }
    }

    /// Serve `routes` on 127.0.0.1; returns `http://127.0.0.1:port` and the
    /// task to abort. Unrouted requests answer 404.
    pub(crate) async fn serve(routes: Vec<Route>) -> (String, tokio::task::JoinHandle<()>) {
        let (base, server, _) = serve_logged(routes).await;
        (base, server)
    }

    /// [`serve`], plus the log of the requests it read.
    pub(crate) async fn serve_logged(
        routes: Vec<Route>,
    ) -> (String, tokio::task::JoinHandle<()>, Received) {
        let received: Received = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut app = axum::Router::new();
        for route in routes {
            let status = axum::http::StatusCode::from_u16(route.status).unwrap();
            let body = route.body.replace("{base}", &base);
            let path = route.path;
            let busy_first = route.busy_first;
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let received = Arc::clone(&received);
            let handler = move |request: axum::body::Body| {
                let body = body.clone();
                let calls = Arc::clone(&calls);
                let received = Arc::clone(&received);
                async move {
                    let mut stream = request.into_data_stream();
                    let mut len = 0;
                    while let Some(Ok(chunk)) = stream.next().await {
                        len += chunk.len();
                    }
                    received.lock().unwrap().push((path, len));
                    let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let status = if busy_first && call == 0 {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        status
                    };
                    (
                        status,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                }
            };
            let filter = axum::routing::MethodFilter::try_from(route.method).unwrap();
            app = app.route(route.path, axum::routing::on(filter, handler));
        }
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, server, received)
    }

    /// The progress updates a [`recorder`] callback received, in order.
    pub(crate) type Updates = Arc<Mutex<Vec<(u64, u64)>>>;

    pub(crate) fn recorder() -> (ProgressCallback, Updates) {
        let updates = Arc::new(Mutex::new(Vec::new()));
        let captured = updates.clone();
        let callback: ProgressCallback = Box::new(move |sent, total| {
            captured.lock().unwrap().push((sent, total));
        });
        (callback, updates)
    }

    /// The contract: at least one step strictly between 0 and the total (no
    /// jump from 0 to 100), steps in order, every total equal to the file,
    /// and `(total, total)` last on success, never on failure.
    pub(crate) fn assert_real_progress(updates: &[(u64, u64)], total: u64, succeeded: bool) {
        assert!(
            updates.iter().any(|&(sent, t)| sent > 0 && sent < t),
            "no intermediate update, the bar would jump from 0 to 100: {:?}",
            &updates[..updates.len().min(5)]
        );
        assert!(
            updates.windows(2).all(|w| w[0].0 <= w[1].0),
            "not monotonic"
        );
        assert!(updates.iter().all(|&(sent, t)| t == total && sent <= t));
        if succeeded {
            assert_eq!(updates.last(), Some(&(total, total)));
        } else {
            assert!(
                updates.iter().all(|&(sent, t)| sent < t),
                "a failed upload reported a completed one"
            );
        }
    }

    /// A file of `size` bytes in a temp dir, and its path as a `&str` owner.
    pub(crate) fn temp_file(size: usize) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![5u8; size]).unwrap();
        file
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::recorder;
    use super::*;

    /// A multi-chunk body reports every chunk as it goes, monotonically, never
    /// the total; the total comes from `complete` alone.
    #[tokio::test]
    async fn a_multi_chunk_body_reports_each_chunk_and_the_total_only_on_complete() {
        let (callback, updates) = recorder();
        let progress = UploadProgress::new(Some(callback), 30);
        let chunks = futures_util::stream::iter(
            [10usize, 10, 10].map(|n| Ok::<_, std::io::Error>(Bytes::from(vec![0u8; n]))),
        );
        let sent: Vec<_> = progress.track(chunks).collect().await;
        assert_eq!(sent.len(), 3);
        assert_eq!(*updates.lock().unwrap(), [(10, 30), (20, 30)]);
        progress.complete();
        assert_eq!(*updates.lock().unwrap(), [(10, 30), (20, 30), (30, 30)]);
    }

    /// An error chunk is passed on and counted as nothing.
    #[tokio::test]
    async fn an_error_chunk_is_not_counted() {
        let (callback, updates) = recorder();
        let progress = UploadProgress::new(Some(callback), 30);
        let chunks = futures_util::stream::iter([
            Ok(Bytes::from(vec![0u8; 10])),
            Err(std::io::Error::other("disk")),
            Ok(Bytes::from(vec![0u8; 10])),
        ]);
        let items: Vec<_> = progress.track(chunks).collect().await;
        assert!(items[1].is_err());
        assert_eq!(*updates.lock().unwrap(), [(10, 30), (20, 30)]);
    }

    /// A file body read from disk reports in steps below the total, in order,
    /// and adds up to the file: the reader's chunks, not one jump at the end.
    #[tokio::test]
    async fn a_file_body_reports_the_bytes_it_reads_in_steps() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![7u8; 100_000]).unwrap();
        let (callback, updates) = recorder();
        let progress = UploadProgress::new(Some(callback), 100_000);
        let stream = progress.file_stream(tokio::fs::File::open(file.path()).await.unwrap());
        futures_util::pin_mut!(stream);
        let mut read = 0usize;
        while let Some(chunk) = stream.next().await {
            read += chunk.unwrap().len();
        }
        assert_eq!(read, 100_000);
        let updates = updates.lock().unwrap();
        assert!(updates.len() > 1, "{updates:?}");
        assert!(updates.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(updates
            .iter()
            .all(|&(sent, total)| total == 100_000 && sent < total));
    }

    /// A file that cannot be opened fails the request through the body's
    /// error: the attempt never reaches the server as an empty upload.
    #[tokio::test]
    async fn a_reopened_body_over_a_missing_file_fails_the_request() {
        use fixture::{serve_logged, Route};
        let (base, server, received) = serve_logged(vec![Route::post("/up", 200, "{}")]).await;
        let dir = tempfile::tempdir().unwrap();
        let progress = UploadProgress::new(None, 10);
        let outcome = reqwest::Client::new()
            .post(format!("{base}/up"))
            .body(progress.reopened_file_body(&dir.path().join("gone")))
            .send()
            .await;
        server.abort();
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(received.lock().unwrap().is_empty());
    }

    /// No callback, no work, no panic.
    #[tokio::test]
    async fn without_a_callback_the_body_is_unchanged() {
        let progress = UploadProgress::new(None, 3);
        let chunks =
            futures_util::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"abc"))]);
        let items: Vec<_> = progress.track(chunks).collect().await;
        assert_eq!(items.len(), 1);
        progress.complete();
    }
}
