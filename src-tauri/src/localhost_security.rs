// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A successful TCP connect proves only that someone owns the port.
/// `ui_server` marks every asset it serves with a fresh secret that a prior
/// listener cannot know. Once verified, its listener keeps the fixed origin
/// reserved.
pub(crate) fn wait_for_owned_server(port: u16, nonce: &str) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(150)) {
            Ok(mut stream) => return verify_owned_response(&mut stream, port, nonce),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(format!(
                    "AeroFTP's UI server did not bind 127.0.0.1:{port} within 5 seconds: {error}"
                ));
            }
        }
    }
}

/// What the user reads when another AeroFTP holds the UI port.
fn another_aeroftp_message(port: u16) -> String {
    format!(
        "Another AeroFTP is already running from a different installation (for example a .deb or \
         AppImage next to the snap), and both serve their window from 127.0.0.1:{port}. Close the \
         other AeroFTP, then start this one again."
    )
}

fn verify_owned_response(stream: &mut TcpStream, port: u16, nonce: &str) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_millis(500)))
        .map_err(|error| error.to_string())?;
    let request =
        format!("GET /index.html HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("Cannot verify the UI server: {error}"))?;

    let mut headers = Vec::new();
    let mut buf = [0u8; 1024];
    while !headers.windows(4).any(|part| part == b"\r\n\r\n") && headers.len() < 16_384 {
        let size = stream
            .read(&mut buf)
            .map_err(|error| format!("Port {port} is occupied by an unverified server: {error}"))?;
        if size == 0 {
            break;
        }
        headers.extend_from_slice(&buf[..size]);
    }
    let header_end = headers
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .ok_or_else(|| format!("Port {port} is occupied by an unverified server"))?;
    let header_text = std::str::from_utf8(&headers[..header_end])
        .map_err(|_| format!("Port {port} is occupied by an unverified server"))?;
    let mut lines = header_text.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let mut has_nonce = false;
    let mut other_nonce = false;
    let mut content_length = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("X-AeroFTP-UI-Nonce") {
                if value.trim() == nonce {
                    has_nonce = true;
                } else {
                    other_nonce = true;
                }
            }
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
    if !(status.starts_with("HTTP/1.1 200 ") || status.starts_with("HTTP/1.0 200 ")) || !has_nonce {
        // A server that answers with this header and another nonce is, most
        // likely, AeroFTP from another installation (the .deb or AppImage next
        // to the snap, Requested 137): each serves its window from this port.
        // It is named as such so the user knows what to close; the server is
        // still not trusted.
        if other_nonce && !has_nonce {
            return Err(another_aeroftp_message(port));
        }
        return Err(format!(
            "Port 127.0.0.1:{port} is occupied by another process, so AeroFTP cannot safely load its UI"
        ));
    }
    // Read the whole asset before closing. `ui_server` survives an early close
    // (tiny_http, under the plugin it replaced, did not), but a body that stops
    // short of its Content-Length is not a response from it.
    let mut remaining = content_length
        .ok_or_else(|| format!("Port {port} returned an incomplete UI response"))?
        .saturating_sub(headers.len() - header_end - 4);
    while remaining > 0 {
        let limit = remaining.min(buf.len());
        let size = stream
            .read(&mut buf[..limit])
            .map_err(|error| format!("Cannot finish reading the UI response: {error}"))?;
        if size == 0 {
            return Err(format!("Port {port} returned an incomplete UI response"));
        }
        remaining -= size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wait_for_owned_server;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn occupied_loopback_port_is_not_our_ui_server() {
        let squatter = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = squatter.local_addr().unwrap().port();
        assert!(wait_for_owned_server(port, "private-startup-nonce").is_err());
    }

    #[test]
    fn matching_server_nonce_allows_the_fixed_origin() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let read = stream.read(&mut request).unwrap();
            assert!(read > 0, "the probe sent no request");
            stream
                .write_all(b"HTTP/1.0 200 OK\r\nX-AeroFTP-UI-Nonce: private-startup-nonce\r\nContent-Length: 4\r\n\r\nbody")
                .unwrap();
        });
        assert!(wait_for_owned_server(port, "private-startup-nonce").is_ok());
        server.join().unwrap();
    }

    // Requested 137: the .deb's server answers the snap's probe with its own
    // nonce. The refusal names the other AeroFTP instead of "another process".
    #[test]
    fn another_aeroftps_server_is_named_as_such_and_still_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let read = stream.read(&mut request).unwrap();
            assert!(read > 0, "the probe sent no request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nX-AeroFTP-UI-Nonce: the-other-instance\r\nContent-Length: 4\r\n\r\nbody")
                .unwrap();
        });
        let refusal = wait_for_owned_server(port, "private-startup-nonce").unwrap_err();
        assert!(
            refusal.starts_with("Another AeroFTP is already running"),
            "{refusal}"
        );
        assert!(refusal.contains(&format!("127.0.0.1:{port}")), "{refusal}");
        server.join().unwrap();
    }

    #[test]
    fn a_server_without_our_header_is_another_process() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let refusal = wait_for_owned_server(port, "private-startup-nonce").unwrap_err();
        assert!(refusal.contains("occupied by another process"), "{refusal}");
        server.join().unwrap();
    }

    #[test]
    fn another_servers_nonce_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 512];
            let read = stream.read(&mut request).unwrap();
            assert!(read > 0, "the probe sent no request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nX-AeroFTP-UI-Nonce: attacker-value\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        assert!(wait_for_owned_server(port, "private-startup-nonce").is_err());
        server.join().unwrap();
    }
}
