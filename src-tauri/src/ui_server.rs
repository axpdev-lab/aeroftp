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
//! What it accepts is deliberately narrow: GET and HEAD, a `Host` naming this
//! origin (so a DNS-rebound page cannot read it), and a path that stays inside
//! the asset root after decoding.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{
    HeaderName, HeaderValue, ALLOW, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::sync::Semaphore;

/// The header `localhost_security::wait_for_owned_server` looks for (it
/// compares names case-insensitively, as HTTP does).
const NONCE_HEADER: HeaderName = HeaderName::from_static("x-aeroftp-ui-nonce");

/// One embedded asset, as the resolver hands it over.
pub(crate) struct ServedAsset {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub csp: Option<String>,
}

/// Where the bytes come from: Tauri's embedded assets in the app, a map in tests.
pub(crate) trait AssetSource: Send + Sync + 'static {
    fn asset(&self, path: &str) -> Option<ServedAsset>;
}

impl<R: tauri::Runtime> AssetSource for tauri::AssetResolver<R> {
    fn asset(&self, path: &str) -> Option<ServedAsset> {
        self.get(path.to_string()).map(|asset| ServedAsset {
            bytes: asset.bytes,
            mime_type: asset.mime_type,
            csp: asset.csp_header,
        })
    }
}

/// Bounds that keep one client from holding the server.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    /// A request head must arrive within this, and an idle keep-alive connection
    /// is closed after it: hyper runs the same timer while it waits for the next
    /// head. WebKit simply reconnects.
    pub header_read_timeout: Duration,
    /// Connections served at once. Beyond it a new connection is closed at
    /// accept, which confines a local connection flood to this server instead of
    /// letting it exhaust the process's file descriptors.
    pub max_connections: usize,
}

impl Limits {
    pub(crate) const APP: Limits = Limits {
        header_read_timeout: Duration::from_secs(10),
        max_connections: 128,
    };
}

struct Site {
    source: Box<dyn AssetSource>,
    nonce: String,
    allowed_hosts: [String; 2],
}

/// Bind `addr` and serve `source` from it until the process exits. The bind is
/// synchronous, so a port that is already taken is reported to the caller here,
/// before any webview could load an origin that belongs to someone else.
pub(crate) fn start(
    source: impl AssetSource,
    addr: SocketAddr,
    nonce: String,
    limits: Limits,
) -> std::io::Result<SocketAddr> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let local = listener.local_addr()?;
    let port = local.port();
    let site = Arc::new(Site {
        source: Box::new(source),
        nonce,
        allowed_hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
    });
    tauri::async_runtime::spawn(accept_loop(listener, site, limits));
    Ok(local)
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
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                // EMFILE and friends: back off instead of spinning on accept.
                log::warn!("UI server accept failed: {error}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            log::warn!(
                "UI server at {} connections, closing a new one",
                limits.max_connections
            );
            continue;
        };
        let site = site.clone();
        tauri::async_runtime::spawn(async move {
            let service = service_fn(move |request| {
                let site = site.clone();
                async move { Ok::<_, Infallible>(site.respond(request).await) }
            });
            let served = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_read_timeout)
                .serve_connection(TokioIo::new(stream), service)
                .await;
            // A client that went away, or one that was too slow: its own
            // connection ends, nothing else does.
            if let Err(error) = served {
                log::debug!("UI server connection ended: {error}");
            }
            drop(slot);
        });
    }
}

impl Site {
    async fn respond(self: Arc<Self>, request: Request<Incoming>) -> Response<Full<Bytes>> {
        if !self.host_allowed(&request) {
            return plain(StatusCode::MISDIRECTED_REQUEST);
        }
        let method = request.method();
        if method != Method::GET && method != Method::HEAD {
            let mut response = plain(StatusCode::METHOD_NOT_ALLOWED);
            response
                .headers_mut()
                .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
            return response;
        }
        let Some(path) = asset_path(request.uri().path()) else {
            return plain(StatusCode::NOT_FOUND);
        };
        // Resolving decompresses the embedded asset: keep it off the reactor.
        let site = self.clone();
        let asset = tokio::task::spawn_blocking(move || site.source.asset(&path))
            .await
            .ok()
            .flatten();
        match asset {
            Some(asset) => self.asset_response(asset),
            None => plain(StatusCode::NOT_FOUND),
        }
    }

    /// DNS rebinding makes a hostile page same-origin with whatever name it
    /// resolves to 127.0.0.1, and the `Host` it sends is still its own name.
    fn host_allowed(&self, request: &Request<Incoming>) -> bool {
        let Some(host) = request.headers().get(HOST).and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let host = host.trim().to_ascii_lowercase();
        self.allowed_hosts.contains(&host)
    }

    fn asset_response(&self, asset: ServedAsset) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(Bytes::from(asset.bytes)));
        let headers = response.headers_mut();
        if let Ok(value) = HeaderValue::from_str(&asset.mime_type) {
            headers.insert(CONTENT_TYPE, value);
        }
        if let Some(Ok(value)) = asset.csp.as_deref().map(HeaderValue::from_str) {
            headers.insert(CONTENT_SECURITY_POLICY, value);
        }
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        if let Ok(value) = HeaderValue::from_str(&self.nonce) {
            headers.insert(NONCE_HEADER, value);
        }
        response
    }
}

fn plain(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
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
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex;
    use std::time::Instant;

    const NONCE: &str = "test-nonce";

    #[derive(Default)]
    struct MapSource {
        assets: HashMap<String, (Vec<u8>, &'static str, Option<&'static str>)>,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl AssetSource for MapSource {
        fn asset(&self, path: &str) -> Option<ServedAsset> {
            self.asked.lock().unwrap().push(path.to_string());
            self.assets.get(path).map(|(bytes, mime, csp)| ServedAsset {
                bytes: bytes.clone(),
                mime_type: mime.to_string(),
                csp: csp.map(str::to_string),
            })
        }
    }

    fn serve(limits: Limits) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let mut source = MapSource::default();
        source.assets.insert(
            "/index.html".into(),
            (
                b"<html></html>".to_vec(),
                "text/html",
                Some("default-src 'self'"),
            ),
        );
        source.assets.insert(
            "/assets/app.css".into(),
            (b"body{}".to_vec(), "text/css", None),
        );
        source.assets.insert(
            "/big.bin".into(),
            (vec![7u8; 8 << 20], "application/octet-stream", None),
        );
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
        let text = String::from_utf8(raw[..end].to_vec()).unwrap();
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
        let mut body = raw[end + 4..].to_vec();
        let length: usize = headers
            .get("content-length")
            .map_or(0, |v| v.parse().unwrap());
        if !head {
            while body.len() < length {
                let n = stream.read(&mut buf).expect("body cut short");
                assert!(n > 0, "connection closed mid-body");
                body.extend_from_slice(&buf[..n]);
            }
        }
        (status, headers, body)
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
        for (value, expected) in [
            (Some(format!("127.0.0.1:{port}")), 200),
            (Some(format!("LOCALHOST:{port}")), 200),
            (Some(format!("attacker.example:{port}")), 421),
            (Some("127.0.0.1".to_string()), 421),
            (Some(format!("127.0.0.1:{}", port.wrapping_add(1))), 421),
            (None, 421),
        ] {
            let mut stream = connect(addr);
            send(&mut stream, "GET", "/index.html", value.clone());
            assert_eq!(
                read_response(&mut stream, false).0,
                expected,
                "Host {value:?}"
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
        for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
            let mut stream = connect(addr);
            write!(
                stream,
                "{method} /index.html HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 0\r\n\r\n",
                addr.port()
            )
            .unwrap();
            let (status, headers, _) = read_response(&mut stream, false);
            assert_eq!(status, 405, "{method}");
            assert_eq!(headers["allow"], "GET, HEAD");
        }
    }

    #[test]
    fn paths_cannot_leave_the_asset_root() {
        let (addr, asked) = serve(Limits::APP);
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
        let (addr, _) = serve(Limits::APP);
        for _ in 0..8 {
            let mut stream = connect(addr);
            send(&mut stream, "GET", "/big.bin", host(addr));
            let mut first = [0u8; 1024];
            let _ = stream.read(&mut first).unwrap();
            // Dropped with most of the 8 MiB body unread: the server's write fails.
        }
        let mut stream = connect(addr);
        send(&mut stream, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut stream, false).0, 200);
    }

    #[test]
    fn connections_beyond_the_cap_are_closed_at_accept() {
        let limits = Limits {
            max_connections: 3,
            ..Limits::APP
        };
        let (addr, _) = serve(limits);
        let mut held: Vec<TcpStream> = (0..3).map(|_| connect(addr)).collect();
        for stream in &mut held {
            send(stream, "GET", "/index.html", host(addr));
            assert_eq!(read_response(stream, false).0, 200);
        }
        // Closed by the server (EOF or reset), not merely left waiting: a read
        // timeout here would mean the connection was accepted and kept.
        let mut over = connect(addr);
        let mut rest = Vec::new();
        let closed = match over.read_to_end(&mut rest) {
            Ok(_) => rest.is_empty(),
            Err(error) => error.kind() == std::io::ErrorKind::ConnectionReset,
        };
        assert!(closed, "a connection over the cap was kept open");

        // A slot frees up as soon as a held connection closes.
        drop(held.pop());
        std::thread::sleep(Duration::from_millis(200));
        let mut again = connect(addr);
        send(&mut again, "GET", "/index.html", host(addr));
        assert_eq!(read_response(&mut again, false).0, 200);
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
