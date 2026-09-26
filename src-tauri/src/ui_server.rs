// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The loopback HTTP server the Linux webviews load the frontend from.
//!
//! Why an HTTP origin at all is explained where the server is started in
//! `lib.rs` (WebKitGTK workers, canvas and iframe CSS need it). This module is
//! about HOW it is served, because the previous server could leave a request
//! unread for as long as other connections stayed open.
//!
//! That server was `tauri-plugin-localhost`, built on tiny_http 0.12.0. tiny_http
//! gives every connection a pooled thread for the connection's whole life, and
//! decides between "queue it for an idle thread" and "start a new thread" from a
//! counter that a woken thread decrements only after it has retaken the pool
//! lock. On a cold start WebKit opens several connections at once, the counter
//! is read stale, and a connection is queued behind threads that are about to
//! be pinned by keep-alive connections. Its request then sits unread until one
//! of those connections closes. Measured on the portal-chooser gate runner: 5
//! cold starts out of 24, each with 444-457 bytes unread in the receive queue of
//! a server socket, the page stuck in `readyState: loading` on the render
//! blocking stylesheet that request asked for, so the module script never ran
//! and the main window stayed blank.
//!
//! Here every connection is its own task, so no connection waits for another
//! one to close. A client that disconnects mid-response ends its own connection
//! only; the plugin treated a failed write as fatal and took the server down for
//! the rest of the session.
//!
//! What it accepts is deliberately narrow: GET and HEAD without content, one
//! `Host` naming this origin (so a DNS-rebound page cannot read it), and a path
//! that stays inside the asset root after decoding. Anything else is refused
//! and its connection closed.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use http_body_util::Full;
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{
    HeaderName, HeaderValue, ALLOW, CACHE_CONTROL, CONNECTION, CONTENT_SECURITY_POLICY,
    CONTENT_TYPE, HOST, X_CONTENT_TYPE_OPTIONS,
};
use hyper::http::uri::Scheme;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::{MissedTickBehavior, Sleep};

/// The header `localhost_security::wait_for_owned_server` looks for (it
/// compares names case-insensitively, as HTTP does).
const NONCE_HEADER: HeaderName = HeaderName::from_static("x-aeroftp-ui-nonce");

const CROSS_ORIGIN_RESOURCE_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-resource-policy");

/// hyper's request limits, pinned rather than inherited: the largest request
/// head, and the most header lines in it. Both sit far above what a webview
/// sends. hyper's defaults (about 400 KB, 100 lines) would let each connection
/// hold 400 KB of a head that never ends. Past either, hyper answers 431 and
/// closes the connection.
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_HEADERS: usize = 32;

/// How often, at most, the connections closed at the cap and the failed
/// accepts are logged. Every log line is also an event sent to the webview, so
/// a line per connection would let any local client flood the log and push
/// the useful lines out of it; they are counted and summed up instead.
const SUMMARY_PERIOD: Duration = Duration::from_secs(10);

/// One embedded asset, as the resolver hands it over. Cloning shares the bytes.
#[derive(Clone)]
pub(crate) struct ServedAsset {
    pub bytes: Bytes,
    pub mime_type: String,
    pub csp: Option<String>,
}

/// Where the bytes come from: Tauri's embedded assets in the app, a map in tests.
pub(crate) trait AssetSource: Send + Sync + 'static {
    /// Every path the source holds, keyed as Tauri keys them (`/index.html`).
    /// Empty for a source that reads a directory on disk, as a dev build does.
    fn paths(&self) -> Vec<String>;
    fn asset(&self, path: &str) -> Option<ServedAsset>;
}

impl<R: tauri::Runtime> AssetSource for tauri::AssetResolver<R> {
    fn paths(&self) -> Vec<String> {
        // Empty in a dev build with a `devUrl`: tauri-codegen embeds nothing
        // there and `get` reads `frontendDist` from disk instead.
        self.iter().map(|(path, _)| path.into_owned()).collect()
    }

    fn asset(&self, path: &str) -> Option<ServedAsset> {
        self.get(path.to_string()).map(|asset| ServedAsset {
            bytes: asset.bytes.into(),
            mime_type: asset.mime_type,
            csp: asset.csp_header,
        })
    }
}

/// The frontend as this server hands it out.
///
/// Tauri brotli-decompresses an embedded asset on every `get`, and the largest
/// one is about 15 MB, so resolving per request let any local client turn a
/// short request into a decompression and a private copy of the asset per
/// connection. Here an asset is resolved once, on its first request, and every
/// response shares that copy. The table is keyed on the embedded set, so a
/// request can fill a slot but never add one: a path outside the set is
/// answered with `index.html`, as Tauri answers it.
///
/// HTML pages are resolved per response instead: Tauri stamps a fresh CSP
/// nonce into each one whenever asset CSP modification is enabled (it is off
/// in tauri.conf.json today, a setting that can change), and the pages are
/// about 1 KB each.
///
/// A dev build embeds nothing and its resolver reads the frontend directory on
/// each request, so nothing is cached there and a rebuilt file is served as is.
struct Frontend {
    source: Box<dyn AssetSource>,
    cache: HashMap<String, OnceLock<Result<Prepared, StatusCode>>>,
}

/// An asset ready to send: its bytes, shared, and the header values that
/// describe it, encoded once.
#[derive(Clone)]
struct Prepared {
    body: Bytes,
    content_type: HeaderValue,
    csp: Option<HeaderValue>,
}

impl Prepared {
    /// No asset is a 404. A type or policy that is not a legal header value
    /// is a 500: sending the asset without its Content-Security-Policy would
    /// fail open.
    fn new(asset: Option<ServedAsset>, key: &str) -> Result<Self, StatusCode> {
        let asset = asset.ok_or(StatusCode::NOT_FOUND)?;
        let header = |value: &str| {
            HeaderValue::from_str(value).map_err(|_| {
                log::error!("UI asset {key} has a header that cannot be sent: {value:?}");
                StatusCode::INTERNAL_SERVER_ERROR
            })
        };
        Ok(Self {
            content_type: header(&asset.mime_type)?,
            csp: asset.csp.as_deref().map(header).transpose()?,
            body: asset.bytes,
        })
    }
}

impl Frontend {
    fn new(source: impl AssetSource) -> Self {
        let cache = source
            .paths()
            .into_iter()
            // Tauri's keys carry the leading `/` (`AssetKey`); the lookups
            // below rely on it, so do not trust that to stay true.
            .map(|path| {
                if path.starts_with('/') {
                    path
                } else {
                    format!("/{path}")
                }
            })
            .map(|path| (path, OnceLock::new()))
            .collect();
        Self {
            source: Box::new(source),
            cache,
        }
    }

    /// The embedded asset a request path names, by Tauri's own lookup order:
    /// the path, `<path>.html`, `<path>/index.html`, then `/index.html`.
    fn key(&self, path: &str) -> Option<&str> {
        let path = path.trim_end_matches('/');
        [
            path.to_string(),
            format!("{path}.html"),
            format!("{path}/index.html"),
            "/index.html".to_string(),
        ]
        .iter()
        .find_map(|candidate| self.cache.get_key_value(candidate.as_str()))
        .map(|(key, _)| key.as_str())
    }

    /// An asset already resolved, without leaving the reactor.
    fn cached(&self, key: &str) -> Option<Result<Prepared, StatusCode>> {
        self.cache.get(key)?.get().cloned()
    }

    /// Resolves `key` (an embedded key, or the request path in a dev build).
    /// Blocks: it decompresses, or waits for the request that does.
    fn fetch(&self, key: &str) -> Result<Prepared, StatusCode> {
        let resolve = || Prepared::new(self.source.asset(key), key);
        match self.cache.get(key) {
            Some(slot) if !key.ends_with(".html") => slot.get_or_init(resolve).clone(),
            _ => resolve(),
        }
    }
}

/// Bounds that keep one client from holding the server.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    /// A request head must arrive within this, and an idle keep-alive connection
    /// is closed after it: hyper runs the same timer while it waits for the next
    /// head. WebKit simply reconnects.
    pub header_read_timeout: Duration,
    /// Connections served at once. At the cap the oldest idle connection is
    /// closed to make room, and a new connection is closed at accept only when
    /// every slot is answering a request. That confines a local connection
    /// flood to this server instead of letting it exhaust the process's file
    /// descriptors. Memory is not what it bounds: responses share the cached
    /// asset and are written without being copied, so a connection costs
    /// hyper's buffers whatever it asks for.
    pub max_connections: usize,
    /// A response write that makes no progress for this long ends the
    /// connection. Without it a client that asks for a large asset and never
    /// reads keeps its connection slot forever, and enough of them hold every
    /// slot the webview needs.
    pub write_stall_timeout: Duration,
    /// Longest a connection is served before it is asked to close. A client
    /// that keeps reading slowly makes progress and never trips the stall
    /// deadline, so without this it could hold a slot indefinitely. The
    /// response in flight is allowed to finish (`shutdown_grace`), then the
    /// connection is dropped; WebKit reconnects for its next request.
    pub max_connection_age: Duration,
    /// How long a connection past its age may take to finish the response it
    /// is sending before it is dropped.
    pub shutdown_grace: Duration,
}

impl Limits {
    pub(crate) const APP: Limits = Limits {
        header_read_timeout: Duration::from_secs(10),
        max_connections: 128,
        write_stall_timeout: Duration::from_secs(10),
        max_connection_age: Duration::from_secs(60),
        shutdown_grace: Duration::from_secs(10),
    };
}

struct Site {
    frontend: Frontend,
    nonce: HeaderValue,
    /// `127.0.0.1:<port>`, the one `Host` this origin is reached by: the
    /// webviews load `http://127.0.0.1:<port>`, and nothing in the app loads a
    /// `localhost` URL from this server.
    origin_host: String,
    stats: Stats,
}

/// Running totals behind the summary line.
#[derive(Default)]
struct Stats {
    /// Closed at accept: at the cap, with no connection idle.
    refused: AtomicU64,
    /// Idle connections closed to make room for a newcomer.
    evicted: AtomicU64,
    /// Accepts that failed, usually for want of file descriptors.
    failed_accepts: AtomicU64,
    /// Connections served to their end.
    ended: AtomicU64,
}

impl Stats {
    fn totals(&self) -> [u64; 4] {
        [
            &self.refused,
            &self.evicted,
            &self.failed_accepts,
            &self.ended,
        ]
        .map(|total| total.load(Ordering::Relaxed))
    }
}

/// What the accept loop has already reported.
struct Summary {
    reported: [u64; 4],
    accept_error: Option<io::Error>,
}

impl Summary {
    fn log(&mut self, stats: &Stats, cap: usize) {
        let totals = stats.totals();
        let [refused, evicted, failed, ended] =
            std::array::from_fn(|i| totals[i] - self.reported[i]);
        self.reported = totals;
        if refused + evicted + failed == 0 {
            return;
        }
        let accept_error = self
            .accept_error
            .take()
            .map(|error| format!(" (last: {error})"))
            .unwrap_or_default();
        log::warn!(
            "UI server, last {}s: {refused} connections closed at the cap of {cap} with none idle, \
             {evicted} idle ones closed to make room, {failed} failed accepts{accept_error}, \
             {ended} connections ended",
            SUMMARY_PERIOD.as_secs()
        );
    }
}

/// Bind `addr` and serve `source` from it until the process exits. The bind is
/// synchronous, so a port that is already taken is reported to the caller here,
/// before any webview could load an origin that belongs to someone else.
///
/// `addr` must be a loopback address: the frontend and its nonce are for this
/// machine's webviews, never for the network.
pub(crate) fn start(
    source: impl AssetSource,
    addr: SocketAddr,
    nonce: String,
    limits: Limits,
) -> std::io::Result<SocketAddr> {
    launch(source, addr, nonce, limits).map(|(local, _)| local)
}

/// `start`, handing back the running site as well (tests read its totals).
fn launch(
    source: impl AssetSource,
    addr: SocketAddr,
    nonce: String,
    limits: Limits,
) -> io::Result<(SocketAddr, Arc<Site>)> {
    if !addr.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the UI server binds a loopback address only, not {addr}"),
        ));
    }
    let nonce = HeaderValue::from_str(&nonce).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the UI nonce is not a header value",
        )
    })?;
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    let frontend = Frontend::new(source);
    match frontend.cache.len() {
        0 => log::info!("UI server on {local} reads the frontend directory per request"),
        assets => log::info!("UI server on {local} serves {assets} embedded assets"),
    }
    let site = Arc::new(Site {
        frontend,
        nonce,
        origin_host: local.to_string(),
        stats: Stats::default(),
    });
    tauri::async_runtime::spawn(accept_loop(listener, site.clone(), limits));
    Ok((local, site))
}

async fn accept_loop(listener: std::net::TcpListener, site: Arc<Site>, limits: Limits) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            log::error!("UI server could not start listening: {error}");
            return;
        }
    };
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let occupants = Arc::new(Occupants::default());
    let mut accepted: u64 = 0;
    let mut summary = Summary {
        reported: site.stats.totals(),
        accept_error: None,
    };
    let mut report = tokio::time::interval(SUMMARY_PERIOD);
    report.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let incoming = tokio::select! {
            incoming = listener.accept() => incoming,
            _ = report.tick() => {
                summary.log(&site.stats, limits.max_connections);
                continue;
            }
        };
        let stream = match incoming {
            Ok((stream, _)) => stream,
            Err(error) => {
                // EMFILE and friends: back off instead of spinning on accept.
                site.stats.failed_accepts.fetch_add(1, Ordering::Relaxed);
                summary.accept_error = Some(error);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        accepted += 1;
        let admission = match slots.clone().try_acquire_owned() {
            Ok(slot) => Admission::Now(slot),
            // An idle connection gives its slot up rather than the newcomer
            // being refused, so holding every slot takes that many requests in
            // flight, not that many open sockets.
            Err(_) => match occupants.evict_oldest_idle() {
                Some(handover) => {
                    site.stats.evicted.fetch_add(1, Ordering::Relaxed);
                    Admission::After(handover)
                }
                None => {
                    site.stats.refused.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            },
        };
        tauri::async_runtime::spawn(serve(
            stream,
            site.clone(),
            limits,
            occupants.clone(),
            admission,
            accepted,
        ));
    }
}

/// How a new connection gets its slot.
enum Admission {
    Now(OwnedSemaphorePermit),
    /// From an idle connection asked to close for it, once that one has.
    After(oneshot::Receiver<OwnedSemaphorePermit>),
}

async fn serve(
    stream: TcpStream,
    site: Arc<Site>,
    limits: Limits,
    occupants: Arc<Occupants>,
    admission: Admission,
    accepted: u64,
) {
    let slot = match admission {
        Admission::Now(slot) => slot,
        Admission::After(handover) => match handover.await {
            Ok(slot) => slot,
            Err(_) => return,
        },
    };
    let tenancy = Tenancy::admit(occupants, slot, accepted);
    let occupant = tenancy.occupant.clone();
    let service = {
        let (site, occupant) = (site.clone(), occupant.clone());
        service_fn(move |request| {
            occupant.answering.store(true, Ordering::Relaxed);
            let answered = Answered(occupant.clone());
            let site = site.clone();
            async move {
                let response = site.respond(request).await;
                Ok::<_, Infallible>(response.map(|body| Reply {
                    body,
                    _answered: answered,
                }))
            }
        })
    };
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout)
        .max_buf_size(MAX_HEAD_BYTES)
        .max_headers(MAX_HEADERS)
        .serve_connection(
            TokioIo::new(StallGuard::new(
                stream,
                limits.write_stall_timeout,
                occupant.clone(),
            )),
            service,
        );
    tokio::pin!(connection);
    // Past its age, or asked to make room: the response in flight may finish
    // within the grace (hyper closes an idle connection at once), then the
    // connection is dropped.
    let finished = tokio::select! {
        served = connection.as_mut() => Some(served),
        () = tokio::time::sleep(limits.max_connection_age) => None,
        () = occupant.evict.notified() => None,
    };
    let served = match finished {
        Some(served) => served,
        None => {
            connection.as_mut().graceful_shutdown();
            tokio::time::timeout(limits.shutdown_grace, connection.as_mut())
                .await
                .unwrap_or(Ok(()))
        }
    };
    // A client that went away, was too slow, or stayed too long: its own
    // connection ends, nothing else does.
    if let Err(error) = served {
        if !ordinary_end(&error) {
            log::trace!("UI server connection ended: {error}");
        }
    }
    site.stats.ended.fetch_add(1, Ordering::Relaxed);
}

/// How a client ends a connection on its own: it went quiet, left, reset,
/// stopped reading, or sent something that is not a request. hyper answered
/// what it could; a log line for each would let any local client write the log.
fn ordinary_end(error: &hyper::Error) -> bool {
    if error.is_timeout() || error.is_incomplete_message() || error.is_parse() {
        return true;
    }
    let mut cause = std::error::Error::source(error);
    while let Some(inner) = cause {
        if let Some(io_error) = inner.downcast_ref::<io::Error>() {
            return matches!(
                io_error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::UnexpectedEof
            );
        }
        cause = inner.source();
    }
    false
}

/// A connection holding a slot, as the accept loop sees it.
#[derive(Default)]
struct Occupant {
    /// Its place in the accept order. The list below is in the order the
    /// connection tasks first ran, which the runtime does not keep: it runs
    /// the task spawned last first when it can.
    accepted: u64,
    /// A request arrived and hyper has not yet taken all of its response.
    answering: AtomicBool,
    /// hyper wrote to the socket and has not since found its buffer drained.
    unflushed: AtomicBool,
    /// Wakes the connection to close for a newcomer.
    evict: Notify,
    /// The newcomer waiting for this connection's slot.
    successor: Mutex<Option<oneshot::Sender<OwnedSemaphorePermit>>>,
}

impl Occupant {
    /// Nothing in flight, so closing it loses nothing. It is a hint read
    /// across threads: hyper's graceful shutdown closes an idle connection at
    /// once and lets a request it is serving finish first, so a wrong guess
    /// costs the newcomer time, never a response.
    fn idle(&self) -> bool {
        !self.answering.load(Ordering::Relaxed) && !self.unflushed.load(Ordering::Relaxed)
    }
}

/// The connections holding slots.
#[derive(Default)]
struct Occupants(Mutex<Vec<Arc<Occupant>>>);

impl Occupants {
    /// Asks the oldest idle connection to close, and returns where its slot
    /// will arrive once it has.
    fn evict_oldest_idle(&self) -> Option<oneshot::Receiver<OwnedSemaphorePermit>> {
        let occupants = lock(&self.0);
        let oldest = occupants
            .iter()
            .filter(|occupant| occupant.idle() && lock(&occupant.successor).is_none())
            .min_by_key(|occupant| occupant.accepted)?;
        let (handover, arrival) = oneshot::channel();
        *lock(&oldest.successor) = Some(handover);
        oldest.evict.notify_one();
        Some(arrival)
    }
}

/// A slot held by one connection. Dropping it, when the connection ends or
/// its task unwinds, takes the connection off the list and passes the slot to
/// the newcomer waiting for it, or back to the pool.
struct Tenancy {
    occupants: Arc<Occupants>,
    occupant: Arc<Occupant>,
    slot: Option<OwnedSemaphorePermit>,
}

impl Tenancy {
    fn admit(occupants: Arc<Occupants>, slot: OwnedSemaphorePermit, accepted: u64) -> Self {
        let occupant = Arc::new(Occupant {
            accepted,
            ..Occupant::default()
        });
        lock(&occupants.0).push(occupant.clone());
        Self {
            occupants,
            occupant,
            slot: Some(slot),
        }
    }
}

impl Drop for Tenancy {
    fn drop(&mut self) {
        lock(&self.occupants.0).retain(|occupant| !Arc::ptr_eq(occupant, &self.occupant));
        let successor = lock(&self.occupant.successor).take();
        if let (Some(handover), Some(slot)) = (successor, self.slot.take()) {
            // A newcomer that is gone refuses it, and the permit returns to
            // the pool as the failed send drops it.
            let _ = handover.send(slot);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Clears `answering` once hyper has taken the whole response body, or the
/// request was abandoned: whichever drops it.
struct Answered(Arc<Occupant>);

impl Drop for Answered {
    fn drop(&mut self) {
        self.0.answering.store(false, Ordering::Relaxed);
    }
}

/// A response body that carries its connection's `Answered`.
struct Reply {
    body: Full<Bytes>,
    _answered: Answered,
}

impl Body for Reply {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Pin::new(&mut self.get_mut().body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

/// A connection stream whose writes fail once they stop making progress. Reads
/// are left to hyper's header timer.
struct StallGuard<S> {
    inner: S,
    stall: Duration,
    stalled_since: Option<Pin<Box<Sleep>>>,
    /// Told when hyper writes and when its buffer has drained: half of what
    /// makes the connection idle.
    occupant: Arc<Occupant>,
}

impl<S> StallGuard<S> {
    fn new(inner: S, stall: Duration, occupant: Arc<Occupant>) -> Self {
        Self {
            inner,
            stall,
            stalled_since: None,
            occupant,
        }
    }

    fn wrote(&self, polled: &Poll<io::Result<usize>>) {
        if matches!(polled, Poll::Ready(Ok(written)) if *written > 0) {
            self.occupant.unflushed.store(true, Ordering::Relaxed);
        }
    }

    /// A write that went through restarts the stall clock. A pending one
    /// starts it, or fails once it has run out.
    fn progress<T>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        if polled.is_ready() {
            self.stalled_since = None;
            return polled;
        }
        let stall = self.stall;
        let timer = self
            .stalled_since
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(stall)));
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the client stopped reading the response",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for StallGuard<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for StallGuard<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.wrote(&polled);
        this.progress(cx, polled)
    }

    /// Forwarded along with `is_write_vectored`: a stream that does not report
    /// vectored writes makes hyper copy each response body into its own buffer
    /// before writing it, one private copy of the asset per connection.
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.wrote(&polled);
        this.progress(cx, polled)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_flush(cx);
        // hyper flushes the stream only once its own buffer is empty.
        if let Poll::Ready(Ok(())) = polled {
            this.occupant.unflushed.store(false, Ordering::Relaxed);
        }
        this.progress(cx, polled)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl Site {
    async fn respond(self: Arc<Self>, request: Request<Incoming>) -> Response<Full<Bytes>> {
        if let Some(refused) = self.misdirected(&request) {
            return refusal(refused);
        }
        let method = request.method();
        if method != Method::GET && method != Method::HEAD {
            let mut response = refusal(StatusCode::METHOD_NOT_ALLOWED);
            response
                .headers_mut()
                .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
            return response;
        }
        // Neither carries content, and a webview sends none. Refusing a
        // request that does, and closing, leaves no body for hyper to drain
        // or skip, so no byte of one is ever parsed as the next request.
        if !request.body().is_end_stream() {
            return refusal(StatusCode::BAD_REQUEST);
        }
        let Some(path) = asset_path(request.uri().path()) else {
            return refusal(StatusCode::NOT_FOUND);
        };
        match self.resolve(path).await {
            Ok(asset) => self.asset_response(asset),
            Err(status) => refusal(status),
        }
    }

    async fn resolve(self: &Arc<Self>, path: String) -> Result<Prepared, StatusCode> {
        let key = if self.frontend.cache.is_empty() {
            path
        } else {
            let key = self.frontend.key(&path).ok_or(StatusCode::NOT_FOUND)?;
            if let Some(cached) = self.frontend.cached(key) {
                return cached;
            }
            key.to_string()
        };
        // Resolving decompresses the embedded asset: keep it off the reactor.
        let site = self.clone();
        tokio::task::spawn_blocking(move || site.frontend.fetch(&key))
            .await
            // The resolver panicked.
            .unwrap_or(Err(StatusCode::INTERNAL_SERVER_ERROR))
    }

    /// The refusal a request earns by not naming this origin, if any.
    ///
    /// DNS rebinding makes a hostile page same-origin with whatever name it
    /// resolves to 127.0.0.1, and the `Host` it sends is still its own name.
    fn misdirected(&self, request: &Request<Incoming>) -> Option<StatusCode> {
        let mut hosts = request.headers().get_all(HOST).iter();
        // RFC 9112 3.2: a request without exactly one Host is malformed.
        let (Some(host), None) = (hosts.next(), hosts.next()) else {
            return Some(StatusCode::BAD_REQUEST);
        };
        // An absolute-form target names its own authority, which takes the
        // place of Host (RFC 9112 3.2.2), so it must name this origin too.
        let target = request.uri();
        let target_is_ours = target.authority().is_none_or(|authority| {
            authority.as_str() == self.origin_host
                && target.scheme().is_none_or(|scheme| *scheme == Scheme::HTTP)
        });
        (host.as_bytes() != self.origin_host.as_bytes() || !target_is_ours)
            .then_some(StatusCode::MISDIRECTED_REQUEST)
    }

    fn asset_response(&self, asset: Prepared) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(asset.body));
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, asset.content_type);
        if let Some(csp) = asset.csp {
            headers.insert(CONTENT_SECURITY_POLICY, csp);
        }
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        // Every asset in `dist` gets a type Tauri derives (scripts
        // `text/javascript`, styles `text/css`), so the webview has nothing
        // to guess, and no page of another origin may load one.
        headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        headers.insert(
            CROSS_ORIGIN_RESOURCE_POLICY,
            HeaderValue::from_static("same-origin"),
        );
        headers.insert(NONCE_HEADER, self.nonce.clone());
        response
    }
}

/// An empty answer that closes the connection: a refused client keeps no
/// slot on the strength of answers it was refused.
fn refusal(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

/// The request path, decoded once, if it stays inside the asset root.
///
/// Tauri's resolver percent-decodes what it is given, and in a dev build it
/// joins the result onto the frontend directory on disk without dropping `..`.
/// So the path is decoded here, refused if any segment climbs or if a `%`
/// survives (a second decoding would happen downstream), and handed over
/// already decoded, which makes the resolver's own decoding a no-op.
fn asset_path(raw: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    if !decoded.starts_with('/')
        || decoded.contains(['\\', '\0', '%'])
        || decoded
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
    {
        return None;
    }
    Some(decoded.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Instant;

    const NONCE: &str = "test-nonce";

    type Asked = Arc<Mutex<Vec<String>>>;

    #[derive(Default)]
    struct MapSource {
        assets: HashMap<String, (Bytes, &'static str, Option<&'static str>)>,
        /// Behaves like a dev build: no embedded set, every request reaches it.
        disk: bool,
        /// Answers every path, as Tauri's resolver does with its fallbacks: a
        /// request that reached it unchecked would show as a 200.
        catch_all: bool,
        asked: Asked,
    }

    impl AssetSource for MapSource {
        fn paths(&self) -> Vec<String> {
            if self.disk {
                Vec::new()
            } else {
                self.assets.keys().cloned().collect()
            }
        }

        fn asset(&self, path: &str) -> Option<ServedAsset> {
            self.asked.lock().unwrap().push(path.to_string());
            let fallback = self.catch_all.then_some("/index.html");
            let found = self.assets.get(path).or_else(|| self.assets.get(fallback?));
            found.map(|(bytes, mime, csp)| ServedAsset {
                bytes: bytes.clone(),
                mime_type: mime.to_string(),
                csp: csp.map(str::to_string),
            })
        }
    }

    /// Larger than all the kernel can buffer between a server socket and a
    /// client that does not read (both loopback buffers at their autotuning
    /// ceiling), so a response of this size really waits on the client. A
    /// fixed 16 MiB did not on a host whose ceilings add up to more.
    fn big() -> Bytes {
        static BIG: OnceLock<Bytes> = OnceLock::new();
        BIG.get_or_init(|| {
            let ceiling = |file: &str| {
                std::fs::read_to_string(file)
                    .ok()
                    .and_then(|v| v.split_whitespace().nth(2)?.parse::<usize>().ok())
                    .unwrap_or(32 << 20)
            };
            let buffered =
                ceiling("/proc/sys/net/ipv4/tcp_wmem") + ceiling("/proc/sys/net/ipv4/tcp_rmem");
            Bytes::from(vec![7u8; (2 * buffered).max(16 << 20)])
        })
        .clone()
    }

    fn site() -> MapSource {
        let mut source = MapSource::default();
        for (path, bytes, mime, csp) in [
            (
                "/index.html",
                Bytes::from_static(b"<html></html>"),
                "text/html",
                Some("default-src 'self'"),
            ),
            (
                "/splash.html",
                Bytes::from_static(b"<p>splash</p>"),
                "text/html",
                None,
            ),
            (
                "/assets/app.css",
                Bytes::from_static(b"body{}"),
                "text/css",
                None,
            ),
            (
                "/assets/vendor.js",
                Bytes::from(vec![b'/'; 1 << 20]),
                "text/javascript",
                None,
            ),
            ("/big.bin", big(), "application/octet-stream", None),
        ] {
            source.assets.insert(path.into(), (bytes, mime, csp));
        }
        source
    }

    fn serve(limits: Limits) -> (SocketAddr, Asked) {
        serve_source(site(), limits)
    }

    fn serve_source(source: MapSource, limits: Limits) -> (SocketAddr, Asked) {
        let asked = source.asked.clone();
        let addr = start(source, "127.0.0.1:0".parse().unwrap(), NONCE.into(), limits).unwrap();
        (addr, asked)
    }

    fn connect(addr: SocketAddr) -> TcpStream {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }

    fn send(stream: &mut TcpStream, method: &str, path: &str, host: Option<String>) {
        let host = host.map(|h| format!("Host: {h}\r\n")).unwrap_or_default();
        write!(stream, "{method} {path} HTTP/1.1\r\n{host}\r\n").unwrap();
    }

    fn host(addr: SocketAddr) -> Option<String> {
        Some(format!("127.0.0.1:{}", addr.port()))
    }

    /// Reads one response. `head` means no body follows whatever Content-Length says.
    fn read_response(
        stream: &mut TcpStream,
        head: bool,
    ) -> (u16, HashMap<String, String>, Vec<u8>) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 8192];
        let end = loop {
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break end;
            }
            let n = stream.read(&mut buf).expect("no response head");
            assert!(n > 0, "connection closed before a response head");
            raw.extend_from_slice(&buf[..n]);
        };
        let (status, headers, length) = parse_head(&raw[..end]);
        let mut body = raw[end + 4..].to_vec();
        if !head {
            while body.len() < length {
                let n = stream.read(&mut buf).expect("body cut short");
                assert!(n > 0, "connection closed mid-body");
                body.extend_from_slice(&buf[..n]);
            }
        }
        (status, headers, body)
    }

    /// Status, headers (names lowercased) and Content-Length of a response head.
    fn parse_head(head: &[u8]) -> (u16, HashMap<String, String>, usize) {
        let text = std::str::from_utf8(head).unwrap();
        let mut lines = text.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: HashMap<String, String> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let length = headers
            .get("content-length")
            .map_or(0, |v| v.parse().unwrap());
        (status, headers, length)
    }

    /// Every response a connection gets until the server closes it (or the
    /// read times out, when it does not).
    fn responses_until_close(stream: &mut TcpStream) -> Vec<(u16, HashMap<String, String>)> {
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        let mut rest = raw.as_slice();
        let mut responses = Vec::new();
        while let Some(end) = rest.windows(4).position(|w| w == b"\r\n\r\n") {
            let (status, headers, length) = parse_head(&rest[..end]);
            responses.push((status, headers));
            rest = &rest[(end + 4 + length).min(rest.len())..];
        }
        responses
    }

    /// The defect this module exists for. Open a burst of keep-alive
    /// connections at once, make every one of them ask for something and keep
    /// them all open, then ask on a fresh connection. Every request must be
    /// answered. The same client against the loop tauri-plugin-localhost ran on
    /// tiny_http 0.12.0 left requests unanswered in 4 rounds out of 10 (up to 35
    /// of the 48), with the fresh connection still answered: the gate runner's
    /// signature. Ten rounds make a regression all but certain to show.
    #[test]
    fn a_burst_of_open_keep_alive_connections_starves_nobody() {
        let (addr, _) = serve(Limits::APP);
        for round in 0..10 {
            let mut open: Vec<TcpStream> = (0..48).map(|_| connect(addr)).collect();
            for stream in &mut open {
                send(stream, "GET", "/assets/app.css", host(addr));
            }
            let started = Instant::now();
            for stream in &mut open {
                let (status, _, body) = read_response(stream, false);
                assert_eq!(
                    (status, body.as_slice()),
                    (200, &b"body{}"[..]),
                    "round {round}"
                );
            }
            let mut fresh = connect(addr);
            send(&mut fresh, "GET", "/index.html", host(addr));
            assert_eq!(read_response(&mut fresh, false).0, 200, "round {round}");
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "round {round}: the burst was served late"
            );
        }
    }

    #[test]
    fn asset_responses_carry_the_headers_the_app_relies_on() {
        let (addr, _) = serve(Limits::APP);
        let mut stream = connect(addr);
        send(&mut stream, "GET", "/index.html", host(addr));
        let (status, headers, body) = read_response(&mut stream, false);
        assert_eq!(status, 200);
        assert_eq!(body, b"<html></html>");
        assert_eq!(headers["content-type"], "text/html");
        assert_eq!(headers["content-security-policy"], "default-src 'self'");
        assert_eq!(headers["cache-control"], "no-cache");
        assert_eq!(headers[NONCE_HEADER.as_str()], NONCE);
        assert_eq!(headers["content-length"], "13");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
        assert!(
            !headers.contains_key("access-control-allow-origin"),
            "another origin must not be let to read the assets"
        );

        // No CSP on an asset that has none, and keep-alive still works.
        send(&mut stream, "GET", "/assets/app.css", host(addr));
        let (_, headers, _) = read_response(&mut stream, false);
        assert_eq!(headers["content-type"], "text/css");
        assert!(!headers.contains_key("content-security-policy"));
    }

    #[test]
    fn the_host_must_name_this_origin() {
        let (addr, asked) = serve(Limits::APP);
        let port = addr.port();
        let ours = format!("Host: 127.0.0.1:{port}\r\n");
        let with = |host: &str| format!("Host: {host}\r\n");
        let get = |target: &str, hosts: &str| format!("GET {target} HTTP/1.1\r\n{hosts}\r\n");
        for (request, expected) in [
            (get("/index.html", &ours), 200),
            (
                get(&format!("http://127.0.0.1:{port}/index.html"), &ours),
                200,
            ),
            // Near misses that a `contains`, `starts_with` or `ends_with`
            // check, or a case-folded `localhost`, would let through.
            (get("/index.html", &with(&format!("localhost:{port}"))), 421),
            (
                get("/index.html", &with(&format!("localhost.evil.com:{port}"))),
                421,
            ),
            (
                get("/index.html", &with(&format!("127.0.0.1.nip.io:{port}"))),
                421,
            ),
            (
                get("/index.html", &with(&format!("evil.localhost:{port}"))),
                421,
            ),
            (get("/index.html", &with(&format!("[::1]:{port}"))), 421),
            (
                get("/index.html", &with(&format!("attacker.example:{port}"))),
                421,
            ),
            (get("/index.html", &with("127.0.0.1")), 421),
            (
                get(
                    "/index.html",
                    &with(&format!("127.0.0.1:{}", port.wrapping_add(1))),
                ),
                421,
            ),
            // An absolute-form target's authority takes the place of Host.
            (get("http://evil.example/index.html", &ours), 421),
            (
                get(&format!("http://evil.example:{port}/index.html"), &ours),
                421,
            ),
            (
                get(&format!("https://127.0.0.1:{port}/index.html"), &ours),
                421,
            ),
            // Exactly one Host (RFC 9112 3.2).
            (get("/index.html", ""), 400),
            (
                get("/index.html", &format!("{ours}{}", with("evil.example"))),
                400,
            ),
            (get("/index.html", &format!("{ours}{ours}")), 400),
        ] {
            let mut stream = connect(addr);
            stream.write_all(request.as_bytes()).unwrap();
            let (status, headers, _) = read_response(&mut stream, false);
            assert_eq!(status, expected, "{request:?}");
            assert_eq!(
                headers.contains_key(NONCE_HEADER.as_str()),
                expected == 200,
                "{request:?}: the nonce goes with an asset and nothing else"
            );
        }
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "a refused host must not reach the assets"
        );
    }

    #[test]
    fn only_get_and_head_are_served() {
        let (addr, _) = serve(Limits::APP);
        let mut stream = connect(addr);
        send(&mut stream, "HEAD", "/index.html", host(addr));
        let (status, headers, body) = read_response(&mut stream, true);
        assert_eq!((status, headers["content-length"].as_str()), (200, "13"));
        assert!(body.is_empty(), "HEAD sent a body");
        // An allowlist: every other method is refused, including the ones a
        // denylist of the usual writes would miss.
        let origin = format!("127.0.0.1:{}", addr.port());
        for (method, target) in [
            ("POST", "/index.html"),
            ("PUT", "/index.html"),
            ("DELETE", "/index.html"),
            ("OPTIONS", "/index.html"),
            ("PATCH", "/index.html"),
            ("TRACE", "/index.html"),
            ("PROPFIND", "/index.html"),
            ("CONNECT", origin.as_str()),
        ] {
            let mut stream = connect(addr);
            write!(
                stream,
                "{method} {target} HTTP/1.1\r\nHost: {origin}\r\nContent-Length: 0\r\n\r\n"
            )
            .unwrap();
            let (status, headers, _) = read_response(&mut stream, false);
            assert_eq!(status, 405, "{method}");
            assert_eq!(headers["allow"], "GET, HEAD");
        }
    }

    #[test]
    fn paths_cannot_leave_the_asset_root() {
        // Both resolvers answer anything that reaches them: the embedded one
        // with `index.html`, the dev build's from disk (modelled by a
        // catch-all). So a climbing path that got through would be a 200.
        let mut disk = site();
        disk.disk = true;
        disk.catch_all = true;
        for (addr, asked) in [serve(Limits::APP), serve_source(disk, Limits::APP)] {
            refuses_climbing_paths(addr, &asked);
        }
    }

    fn refuses_climbing_paths(addr: SocketAddr, asked: &Asked) {
        for path in [
            "/../etc/passwd",
            "/assets/../../etc/passwd",
            "/%2e%2e/etc/passwd",
            "/assets/%2E%2E/%2e%2e/etc/passwd",
            "/%252e%252e/etc/passwd",
            "/assets/..%2fetc",
            "/assets/%5c..%5cetc",
            "/./index.html",
            "/index.html%00",
            "/%ff",
        ] {
            let mut stream = connect(addr);
            send(&mut stream, "GET", path, host(addr));
            assert_eq!(read_response(&mut stream, false).0, 404, "{path}");
        }
        assert!(
            asked.lock().unwrap().is_empty(),
            "a refused path reached the assets: {:?}",
            asked.lock().unwrap()
        );

        // A legitimately encoded name is decoded once and served.
        let mut stream = connect(addr);
        send(&mut stream, "GET", "/assets/app%2Ecss", host(addr));
        assert_eq!(read_response(&mut stream, false).0, 200);
        assert_eq!(asked.lock().unwrap().as_slice(), ["/assets/app.css"]);
    }

    #[test]
    fn an_idle_or_silent_connection_is_closed() {
        let limits = Limits {
            header_read_timeout: Duration::from_millis(300),
            ..Limits::APP
        };
        let (addr, _) = serve(limits);

        // Idle keep-alive after one answered request.
        let mut idle = connect(addr);
        send(&mut idle, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut idle, false).0, 200);
        // Slowloris: a head that never finishes.
        let mut slow = connect(addr);
        slow.write_all(b"GET /index.html HTTP/1.1\r\nHost: ")
            .unwrap();

        std::thread::sleep(Duration::from_millis(900));
        for (name, stream) in [("idle", &mut idle), ("slow", &mut slow)] {
            let mut rest = Vec::new();
            // Either the server closed it (EOF) or answered 408 and closed.
            let closed = stream.read_to_end(&mut rest).is_ok();
            assert!(closed, "the {name} connection was not closed");
        }
    }

    #[test]
    fn a_client_that_leaves_mid_response_takes_down_nothing() {
        let (addr, site) = launch(
            site(),
            "127.0.0.1:0".parse().unwrap(),
            NONCE.into(),
            Limits::APP,
        )
        .unwrap();
        for _ in 0..8 {
            let mut stream = connect(addr);
            send(&mut stream, "GET", "/big.bin", host(addr));
            let mut first = [0u8; 1024];
            let _ = stream.read(&mut first).unwrap();
            // Dropped with most of the body unread: the server's write fails.
        }
        // Each of those connection tasks ran to its end. One that panicked on
        // the failed write (the old plugin's `expect`) would stop the count
        // short while every later connection is still served.
        let ended = || site.stats.ended.load(Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(5);
        while ended() < 8 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(ended(), 8, "a connection task did not reach its end");
        let mut stream = connect(addr);
        send(&mut stream, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut stream, false).0, 200);
    }

    /// With every slot answering a request, a newcomer is closed at accept:
    /// nothing in flight is cut short for it, and it is not left waiting.
    #[test]
    fn at_the_cap_with_no_idle_connection_a_newcomer_is_closed() {
        let limits = Limits {
            max_connections: 1,
            ..Limits::APP
        };
        let (addr, _) = serve(limits);
        let mut busy = connect(addr);
        send(&mut busy, "GET", "/big.bin", host(addr));
        // Time for the server to start the response and fill the buffers.
        std::thread::sleep(Duration::from_millis(300));
        // Closed by the server (EOF or reset), not merely left waiting: a read
        // timeout here would mean the connection was accepted and kept.
        let mut over = connect(addr);
        let mut rest = Vec::new();
        let closed = match over.read_to_end(&mut rest) {
            Ok(_) => rest.is_empty(),
            Err(error) => error.kind() == std::io::ErrorKind::ConnectionReset,
        };
        assert!(closed, "a connection over the cap was kept open");
        let (status, _, body) = read_response(&mut busy, false);
        assert_eq!((status, body.len()), (200, big().len()));
    }

    /// At the cap the oldest idle connection closes to make room, so a slot
    /// is not held by a socket that merely stays open.
    #[test]
    fn at_the_cap_the_oldest_idle_connection_makes_room() {
        let limits = Limits {
            max_connections: 2,
            ..Limits::APP
        };
        let (addr, _) = serve(limits);
        let mut oldest = connect(addr);
        let mut newer = connect(addr);
        for stream in [&mut oldest, &mut newer] {
            send(stream, "GET", "/index.html", host(addr));
            assert_eq!(read_response(stream, false).0, 200);
        }
        let mut third = connect(addr);
        send(&mut third, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut third, false).0, 200);
        let mut rest = Vec::new();
        let closed = oldest.read_to_end(&mut rest).is_ok() && rest.is_empty();
        assert!(closed, "the oldest idle connection was kept");
        send(&mut newer, "GET", "/assets/app.css", host(addr));
        assert_eq!(read_response(&mut newer, false).0, 200);
    }

    /// A socket that never sends a request (a preconnect, or a flood) counts
    /// as idle and gives its slot up at once.
    #[test]
    fn at_the_cap_a_silent_connection_makes_room() {
        let limits = Limits {
            max_connections: 1,
            ..Limits::APP
        };
        let (addr, _) = serve(limits);
        let mut silent = connect(addr);
        std::thread::sleep(Duration::from_millis(100));
        let mut next = connect(addr);
        send(&mut next, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut next, false).0, 200);
        let mut rest = Vec::new();
        assert!(silent.read_to_end(&mut rest).is_ok() && rest.is_empty());
    }

    /// A refused request closes its connection, so a client (a DNS-rebound
    /// page among them) keeps no slot open by asking for what it is refused.
    #[test]
    fn a_refusal_closes_its_connection() {
        let (addr, _) = serve(Limits::APP);
        let port = addr.port();
        for (request, expected) in [
            (
                format!("GET /index.html HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n"),
                421,
            ),
            (
                format!("DELETE /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
                405,
            ),
            (
                format!("GET /../etc/passwd HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
                404,
            ),
        ] {
            let mut stream = connect(addr);
            stream.write_all(request.as_bytes()).unwrap();
            let (status, headers, _) = read_response(&mut stream, false);
            assert_eq!(status, expected, "{request}");
            assert_eq!(headers.get("connection").map(String::as_str), Some("close"));
            let mut rest = Vec::new();
            let closed = stream.read_to_end(&mut rest).is_ok() && rest.is_empty();
            assert!(closed, "{request}: the connection was kept open");
        }
    }

    /// The write side of a slowloris: ask for an asset larger than the socket
    /// buffers can hold (`big`) and never read it. With one slot, the next connection is
    /// served only if the stalled one gives its slot back.
    #[test]
    fn a_client_that_stops_reading_gives_its_slot_back() {
        let limits = Limits {
            max_connections: 1,
            write_stall_timeout: Duration::from_millis(300),
            ..Limits::APP
        };
        let (addr, _) = serve(limits);
        let mut stalled = connect(addr);
        send(&mut stalled, "GET", "/big.bin", host(addr));
        std::thread::sleep(Duration::from_millis(1500));
        let mut next = connect(addr);
        send(&mut next, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut next, false).0, 200);
        drop(stalled);
    }

    /// A client that keeps reading, slowly, makes progress and never trips the
    /// stall deadline. With one slot, the next connection is served only if the
    /// slow one is retired when it reaches its age.
    #[test]
    fn a_slow_but_steady_reader_is_retired_at_its_age() {
        let limits = Limits {
            max_connections: 1,
            max_connection_age: Duration::from_millis(400),
            shutdown_grace: Duration::from_millis(200),
            ..Limits::APP
        };
        let (addr, _) = serve(limits);
        let mut slow = connect(addr);
        send(&mut slow, "GET", "/big.bin", host(addr));
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            let started = Instant::now();
            // Keep draining a little at a time for longer than age + grace.
            while started.elapsed() < Duration::from_millis(2500) {
                match slow.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        std::thread::sleep(Duration::from_millis(1200));
        let mut next = connect(addr);
        send(&mut next, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut next, false).0, 200);
        reader.join().unwrap();
    }

    /// Every response for an asset shares one resolution of it, including
    /// requests that race for it before it is cached.
    #[test]
    fn an_asset_is_resolved_once_however_often_it_is_asked_for() {
        let (addr, asked) = serve(Limits::APP);
        let racers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    let mut stream = connect(addr);
                    send(&mut stream, "GET", "/assets/vendor.js", host(addr));
                    let (status, _, body) = read_response(&mut stream, false);
                    (status, body.len())
                })
            })
            .collect();
        for racer in racers {
            assert_eq!(racer.join().unwrap(), (200, 1 << 20));
        }
        let mut stream = connect(addr);
        for method in ["GET", "HEAD", "GET", "HEAD"] {
            send(&mut stream, method, "/assets/app.css", host(addr));
            let (status, headers, body) = read_response(&mut stream, method == "HEAD");
            assert_eq!((status, headers["content-length"].as_str()), (200, "6"));
            let expected: &[u8] = if method == "HEAD" { b"" } else { b"body{}" };
            assert_eq!(body, expected);
        }
        let asked = asked.lock().unwrap();
        let count = |path: &str| asked.iter().filter(|asked| *asked == path).count();
        assert_eq!(
            (count("/assets/vendor.js"), count("/assets/app.css")),
            (1, 1),
            "{asked:?}"
        );
    }

    /// Tauri answers a path it does not hold with `index.html` (the app routes
    /// on the client). So does this server, without handing the unknown path
    /// to the resolver or keeping anything for it.
    #[test]
    fn a_path_outside_the_embedded_set_is_answered_with_index_html() {
        let (addr, asked) = serve(Limits::APP);
        let mut stream = connect(addr);
        for path in [
            "/",
            "/settings",
            "/no/such/route",
            "/assets/",
            "/assets/missing.js",
        ] {
            send(&mut stream, "GET", path, host(addr));
            let (status, headers, body) = read_response(&mut stream, false);
            assert_eq!(
                (status, body.as_slice()),
                (200, &b"<html></html>"[..]),
                "{path}"
            );
            assert_eq!(headers["content-type"], "text/html", "{path}");
        }
        // Tauri's `<path>.html` rule.
        send(&mut stream, "GET", "/splash", host(addr));
        assert_eq!(read_response(&mut stream, false).2, b"<p>splash</p>");
        let asked = asked.lock().unwrap();
        assert!(
            asked
                .iter()
                .all(|path| path == "/index.html" || path == "/splash.html"),
            "an unknown path reached the resolver: {asked:?}"
        );
    }

    /// A dev build has no embedded set: its resolver reads `dist` on disk, so
    /// every request goes through to it and nothing is kept.
    #[test]
    fn a_dev_build_reads_every_request_through() {
        let mut source = site();
        source.disk = true;
        let (addr, asked) = serve_source(source, Limits::APP);
        let mut stream = connect(addr);
        for _ in 0..2 {
            send(&mut stream, "GET", "/assets/app.css", host(addr));
            assert_eq!(read_response(&mut stream, false).2, b"body{}");
        }
        send(&mut stream, "GET", "/settings", host(addr));
        assert_eq!(read_response(&mut stream, false).0, 404);
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            ["/assets/app.css", "/assets/app.css", "/settings"]
        );
    }

    /// hyper queues a response body and writes it with `writev` only when the
    /// stream says it writes vectored; otherwise it first copies the whole
    /// body into its own buffer, a private copy of the asset per connection.
    #[test]
    fn hyper_sees_a_stream_that_writes_vectored() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (server, _) = listener.accept().await.unwrap();
            let mut stream = TokioIo::new(StallGuard::new(
                server,
                Duration::from_secs(1),
                Arc::default(),
            ));
            assert!(hyper::rt::Write::is_write_vectored(&stream));
            let parts = [io::IoSlice::new(b"head"), io::IoSlice::new(b"body")];
            let written = std::future::poll_fn(|cx| {
                hyper::rt::Write::poll_write_vectored(Pin::new(&mut stream), cx, &parts)
            })
            .await
            .unwrap();
            assert_eq!(written, 8, "only the first buffer went out");
            drop(client);
        });
    }

    #[test]
    fn the_server_binds_loopback_only() {
        for addr in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0"] {
            let refused = start(site(), addr.parse().unwrap(), NONCE.into(), Limits::APP);
            let error = refused.expect_err(addr);
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{addr}");
        }
    }

    /// A type or policy that cannot be put in a header is a server error,
    /// never a response that silently goes without it.
    #[test]
    fn an_unsendable_header_is_a_server_error() {
        let mut source = site();
        source.assets.insert(
            "/policy.html".into(),
            (
                Bytes::from_static(b"<p></p>"),
                "text/html",
                Some("default-src 'self'\r\nX-Injected: 1"),
            ),
        );
        source.assets.insert(
            "/typed.js".into(),
            (Bytes::from_static(b"1"), "text/javascript\n", None),
        );
        let (addr, _) = serve_source(source, Limits::APP);
        for path in ["/policy.html", "/typed.js"] {
            let mut stream = connect(addr);
            send(&mut stream, "GET", path, host(addr));
            let (status, headers, body) = read_response(&mut stream, false);
            assert_eq!((status, body.as_slice()), (500, &b""[..]), "{path}");
            assert!(!headers.contains_key(NONCE_HEADER.as_str()), "{path}");
        }
    }

    /// hyper's limits are pinned: a head past either is refused with 431
    /// instead of sitting in a buffer, and an ordinary one still passes.
    #[test]
    fn an_oversized_request_head_is_refused() {
        let (addr, _) = serve(Limits::APP);
        let start = format!(
            "GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n",
            addr.port()
        );
        // Exactly the limit and still unfinished: hyper has read every byte
        // when it refuses, so the connection closes cleanly, not with a reset.
        let filler = MAX_HEAD_BYTES - start.len() - "X-Fill: \r\n".len();
        let long = format!("{start}X-Fill: {}\r\n", "a".repeat(filler));
        let many: String = (0..MAX_HEADERS)
            .map(|i| format!("X-Line-{i}: x\r\n"))
            .collect();
        let usual: String = (0..MAX_HEADERS / 2)
            .map(|i| format!("X-Line-{i}: {}\r\n", "a".repeat(100)))
            .collect();
        for (request, expected) in [
            (long, 431),
            (format!("{start}{many}\r\n"), 431),
            (format!("{start}{usual}\r\n"), 200),
        ] {
            let mut stream = connect(addr);
            stream.write_all(request.as_bytes()).unwrap();
            assert_eq!(
                read_response(&mut stream, false).0,
                expected,
                "{} bytes",
                request.len()
            );
        }
    }

    /// GET and HEAD carry no content. One that does is refused and its
    /// connection closed, so no byte of that body is ever parsed as a request:
    /// not after a Content-Length, and not through a Transfer-Encoding that
    /// contradicts one.
    #[test]
    fn a_request_with_content_is_refused_and_nothing_after_it_is_read() {
        let (addr, asked) = serve(Limits::APP);
        let port = addr.port();
        let smuggled = format!("GET /assets/app.css HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
        for (framing, body) in [
            (
                format!("Content-Length: {}\r\n", smuggled.len()),
                smuggled.clone(),
            ),
            (
                format!(
                    "Transfer-Encoding: chunked\r\nContent-Length: {}\r\n",
                    smuggled.len() + 5
                ),
                format!("0\r\n\r\n{smuggled}"),
            ),
        ] {
            let mut stream = connect(addr);
            write!(
                stream,
                "GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{framing}\r\n{body}"
            )
            .unwrap();
            let (status, headers, _) = read_response(&mut stream, false);
            assert_eq!(status, 400, "{framing}");
            assert_eq!(headers.get("connection").map(String::as_str), Some("close"));
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest);
            assert!(
                rest.is_empty(),
                "{framing}: more was answered: {}",
                String::from_utf8_lossy(&rest)
            );
        }
        assert!(
            asked.lock().unwrap().is_empty(),
            "a request with content was served"
        );
    }

    /// The startup probe and this server agree: the probe's request
    /// (HTTP/1.0, `Host: 127.0.0.1:<port>`) gets the nonce back, and another
    /// nonce is still told apart.
    #[test]
    fn the_startup_probe_recognises_this_server() {
        let (addr, _) = serve(Limits::APP);
        let probe = crate::localhost_security::wait_for_owned_server;
        assert_eq!(probe(addr.port(), NONCE), Ok(()));
        assert!(probe(addr.port(), "another-nonce").is_err());
    }

    /// A port that is taken is the caller's error to report: never a quiet
    /// bind somewhere else, which would leave the origin to its squatter.
    #[test]
    fn a_taken_port_is_reported_to_the_caller() {
        let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = squatter.local_addr().unwrap();
        let error = start(site(), taken, NONCE.into(), Limits::APP).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    }

    /// Every request on a connection is checked, not only the first: a
    /// pipelined request naming another host is refused, and nothing queued
    /// behind it is answered.
    #[test]
    fn a_pipelined_request_for_another_host_is_refused() {
        let (addr, asked) = serve(Limits::APP);
        let port = addr.port();
        let mut stream = connect(addr);
        write!(
            stream,
            "GET /index.html HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n\
             GET /assets/app.css HTTP/1.1\r\nHost: evil.example:{port}\r\n\r\n\
             GET /assets/app.css HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        let responses = responses_until_close(&mut stream);
        let statuses: Vec<u16> = responses.iter().map(|(status, _)| *status).collect();
        assert_eq!(statuses, [200, 421]);
        assert!(!responses[1].1.contains_key(NONCE_HEADER.as_str()));
        assert_eq!(asked.lock().unwrap().as_slice(), ["/index.html"]);
    }

    #[test]
    fn asset_path_decodes_once_and_refuses_climbing() {
        assert_eq!(asset_path("/index.html").as_deref(), Some("/index.html"));
        assert_eq!(asset_path("/vs/a%20b.js").as_deref(), Some("/vs/a b.js"));
        assert_eq!(asset_path("/"), Some("/".to_string()));
        for bad in [
            "index.html",
            "/a/../b",
            "/%2e%2e",
            "/a%2f..%2fb",
            "/%25",
            "/a\\b",
        ] {
            assert_eq!(asset_path(bad), None, "{bad}");
        }
    }
}
