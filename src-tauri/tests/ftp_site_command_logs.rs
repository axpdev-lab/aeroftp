//! No log line of a `SITE` exchange carries its arguments or the reply text.
//!
//! `SITE ADDUSER` and `SITE CHPASS` carry passwords, and glFTPd replies name
//! users and IP masks. This guard lives in its own test binary on purpose: it
//! captures with a thread-scoped subscriber, and `tracing` caches whether a
//! call site is enabled across threads, so a test running in parallel in the
//! same binary can make the capture miss the very lines it must inspect.

use std::sync::{Arc, Mutex};

use ftp_client_gui_lib::providers::ftp_site::{run_site_command, SiteOptions, SiteOutcome};
use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const SECRET: &str = "t8-sentinel-password";
const REPLY_SECRET: &str = "t8-reply-ident";

/// A server that logs in anyone and answers `SITE` with `site_reply`, or
/// never answers it when `site_reply` is `None`.
async fn provider_answering(site_reply: Option<String>) -> FtpProvider {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let site_reply = site_reply.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let _ = write.write_all(b"220 ready\r\n").await;
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let verb = line
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    let reply = match verb.as_str() {
                        "SITE" => match &site_reply {
                            Some(reply) => reply.clone(),
                            None => continue,
                        },
                        "USER" => "331 Password required\r\n".to_string(),
                        "PASS" => "230 Logged in\r\n".to_string(),
                        "FEAT" => "211-Features:\r\n UTF8\r\n211 End\r\n".to_string(),
                        "PWD" => "257 \"/\"\r\n".to_string(),
                        _ => "200 Ok\r\n".to_string(),
                    };
                    if write.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let mut provider = FtpProvider::new(FtpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "siteop".to_string(),
        password: secrecy::SecretString::from("login-password".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    });
    provider
        .connect()
        .await
        .expect("connect to the scripted server");
    provider
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for Capture {
    type Writer = Capture;
    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn no_log_line_carries_an_argument_or_the_reply_text() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let quick = SiteOptions::with_reply_timeout_secs(1);

    let mut answering = provider_answering(Some(format!(
        "200- IP '{REPLY_SECRET}@192.0.2.*' added\r\n200 ok\r\n"
    )))
    .await;
    let replied =
        run_site_command(&mut answering, &format!("CHPASS example {SECRET}"), &quick).await;
    assert!(
        matches!(replied.outcome, SiteOutcome::Replied { .. }),
        "{:?}",
        replied.outcome
    );

    // A server that never answers, so the unknown-outcome path logs too.
    let mut silent = provider_answering(None).await;
    let unknown = run_site_command(&mut silent, &format!("ADDUSER example {SECRET}"), &quick).await;
    assert!(
        matches!(unknown.outcome, SiteOutcome::Unknown { .. }),
        "{:?}",
        unknown.outcome
    );

    let out = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(
        out.contains("SITE command replied") && out.contains("no complete reply"),
        "the capture did not record the SITE path's own lines, so the absence below would prove nothing:\n{out}"
    );
    assert!(
        !out.contains(SECRET),
        "a SITE argument reached the log:\n{out}"
    );
    assert!(
        !out.contains(REPLY_SECRET),
        "SITE reply text reached the log:\n{out}"
    );
}
