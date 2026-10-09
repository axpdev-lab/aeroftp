//! `SITE` commands through an open FTP session, pinned on the wire.
//!
//! The scripted server records every line it receives on every connection,
//! so each test asserts what crossed the wire, not only what came back: a
//! SITE command must reach the server once and only once, whatever happens
//! to the reply, and a refused line must not reach it at all.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftp_client_gui_lib::providers::ftp_site::{
    run_site_command, NotSentReason, ReplyEncoding, SiteInputError, SiteOptions, SiteOutcome,
    UnknownCause,
};
use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode, ProviderError, WebDavConfig};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider, WebDavProvider};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// What the server does when it reads a `SITE` line.
#[derive(Clone)]
enum OnSite {
    /// Write these bytes as the reply.
    Reply(Vec<u8>),
    /// Never answer.
    Silent,
    /// Close the connection without answering.
    Close,
}

/// How the scripted server answers a `SITE` line on the connection with the
/// given index (0 = first). Later connections answer `200 ok` unless the
/// script says otherwise, so a redial can be told from a resend.
type Script = Arc<dyn Fn(usize, &str) -> OnSite + Send + Sync>;

#[derive(Default)]
struct Wire {
    /// `(connection index, line)` for every line received.
    lines: Vec<(usize, String)>,
    connections: usize,
}

impl Wire {
    fn site_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter(|(_, line)| line.to_ascii_uppercase().starts_with("SITE"))
            .map(|(_, line)| line.clone())
            .collect()
    }

    fn lines_on(&self, connection: usize) -> Vec<String> {
        self.lines
            .iter()
            .filter(|(index, _)| *index == connection)
            .map(|(_, line)| line.clone())
            .collect()
    }
}

async fn serve(listener: TcpListener, wire: Arc<Mutex<Wire>>, script: Script) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let index = {
            let mut wire = wire.lock().unwrap();
            wire.connections += 1;
            wire.connections - 1
        };
        let wire = Arc::clone(&wire);
        let script = Arc::clone(&script);
        tokio::spawn(async move { session(stream, index, wire, script).await });
    }
}

async fn session(
    stream: tokio::net::TcpStream,
    index: usize,
    wire: Arc<Mutex<Wire>>,
    script: Script,
) {
    let (read, mut write) = stream.into_split();
    if write
        .write_all(b"220 Scripted FTP ready\r\n")
        .await
        .is_err()
    {
        return;
    }
    let mut cwd = "/".to_string();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        wire.lock().unwrap().lines.push((index, line.clone()));
        let verb = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let reply: Vec<u8> = match verb.as_str() {
            "SITE" => match script(index, &line) {
                OnSite::Reply(bytes) => bytes,
                OnSite::Silent => continue,
                OnSite::Close => return,
            },
            "USER" => b"331 Password required\r\n".to_vec(),
            "PASS" => b"230 Logged in\r\n".to_vec(),
            "FEAT" => b"211-Features:\r\n UTF8\r\n211 End\r\n".to_vec(),
            "PWD" => format!("257 \"{cwd}\" is the current directory\r\n").into_bytes(),
            "CWD" => {
                let target = line[3..].trim();
                cwd = if target.starts_with('/') {
                    target.to_string()
                } else {
                    format!("{}/{}", cwd.trim_end_matches('/'), target)
                };
                b"250 Directory changed\r\n".to_vec()
            }
            "QUIT" => {
                let _ = write.write_all(b"221 Goodbye\r\n").await;
                return;
            }
            _ => b"200 Ok\r\n".to_vec(),
        };
        if write.write_all(&reply).await.is_err() {
            return;
        }
    }
}

async fn provider_with(script: Script) -> (FtpProvider, Arc<Mutex<Wire>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let wire = Arc::new(Mutex::new(Wire::default()));
    tokio::spawn(serve(listener, Arc::clone(&wire), script));
    let config = FtpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "siteop".to_string(),
        password: secrecy::SecretString::from("login-password".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    };
    let mut provider = FtpProvider::new(config);
    provider
        .connect()
        .await
        .expect("connect to the scripted server");
    (provider, wire)
}

/// A server whose first connection answers `SITE` with `on_first`, and every
/// later one with `200 ok`.
fn first_connection(on_first: OnSite) -> Script {
    Arc::new(move |index, _| {
        if index == 0 {
            on_first.clone()
        } else {
            OnSite::Reply(b"200 ok\r\n".to_vec())
        }
    })
}

fn reply(bytes: &[u8]) -> Script {
    first_connection(OnSite::Reply(bytes.to_vec()))
}

fn quick() -> SiteOptions {
    SiteOptions::with_reply_timeout_secs(1)
}

fn replied(outcome: SiteOutcome) -> (u16, Vec<String>, ReplyEncoding, bool) {
    match outcome {
        SiteOutcome::Replied {
            reply,
            session_reset,
            ..
        } => (reply.code, reply.lines, reply.encoding, session_reset),
        other => panic!("expected a complete reply, got {other:?}"),
    }
}

const GLFTPD_USER_BOX: &[u8] = b"200- User Comment: Added by siteop\r\n\
200- +=======================================================================+\r\n\
200- | Username: example                   Created: 10-07-26                  |\r\n\
200- | Ratio: 1:3                         Credits:      10.0 GB              |\r\n\
200-\r\n\
200- +=======================================================================+\r\n\
200 Command Successful.\r\n";

// T1
#[tokio::test]
async fn a_glftpd_box_reply_arrives_whole_and_in_order() {
    let (mut provider, wire) = provider_with(reply(GLFTPD_USER_BOX)).await;
    let run = run_site_command(&mut provider, "SITE USER example", &quick()).await;
    assert_eq!(run.label, "SITE USER");
    let (code, lines, encoding, session_reset) = replied(run.outcome);
    assert_eq!(code, 200);
    assert_eq!(encoding, ReplyEncoding::Utf8);
    assert!(!session_reset);
    assert_eq!(lines.len(), 7, "{lines:#?}");
    assert_eq!(lines[0], "200- User Comment: Added by siteop");
    assert_eq!(lines[4], "200-", "an empty continuation line is a line too");
    assert_eq!(lines[6], "200 Command Successful.");
    assert_eq!(wire.lock().unwrap().site_lines(), vec!["SITE USER example"]);
}

// T2
#[tokio::test]
async fn a_refusal_glftpd_reports_with_200_is_reported_as_received() {
    let (mut provider, _) = provider_with(reply(
        b"200 Invalid Access. Cannot change flags for other SITEOPS.\r\n",
    ))
    .await;
    let run = run_site_command(&mut provider, "CHANGE other flags +1", &quick()).await;
    let (code, lines, _, _) = replied(run.outcome);
    assert_eq!(code, 200);
    assert_eq!(
        lines,
        vec!["200 Invalid Access. Cannot change flags for other SITEOPS."]
    );
}

// T3
#[tokio::test]
async fn a_refused_command_is_reported_and_never_sent_again() {
    let (mut provider, wire) =
        provider_with(reply(b"500 'SITE FOO': command not understood.\r\n")).await;
    let run = run_site_command(&mut provider, "FOO bar", &quick()).await;
    let (code, _, _, session_reset) = replied(run.outcome);
    assert_eq!(code, 500);
    assert!(!session_reset);
    let wire = wire.lock().unwrap();
    assert_eq!(
        wire.site_lines(),
        vec!["SITE FOO bar"],
        "a refused SITE command must cross the wire exactly once"
    );
    assert_eq!(wire.connections, 1, "a refusal is not a reason to redial");
}

// T4
#[tokio::test]
async fn an_unanswered_command_is_unknown_redialed_and_not_resent() {
    let (mut provider, wire) = provider_with(first_connection(OnSite::Silent)).await;
    provider.cd("/sub").await.unwrap();
    let run = run_site_command(&mut provider, "DELUSER example", &quick()).await;
    match run.outcome {
        SiteOutcome::Unknown {
            cause,
            session_reconnected,
            ..
        } => {
            assert_eq!(cause, UnknownCause::Timeout);
            assert!(session_reconnected);
        }
        other => panic!("expected an unknown outcome, got {other:?}"),
    }
    provider
        .keep_alive()
        .await
        .expect("the redialed session works");
    assert_eq!(provider.pwd().await.unwrap(), "/sub");
    let wire = wire.lock().unwrap();
    assert_eq!(wire.connections, 2);
    assert_eq!(
        wire.site_lines(),
        vec!["SITE DELUSER example"],
        "after an unknown outcome the command must not be sent again"
    );
    assert!(
        wire.lines_on(1).iter().any(|line| line == "CWD /sub"),
        "the redialed session goes back to its directory: {:?}",
        wire.lines_on(1)
    );
}

// T5
#[tokio::test]
async fn a_connection_lost_mid_command_is_unknown_and_not_resent() {
    let (mut provider, wire) = provider_with(first_connection(OnSite::Close)).await;
    let run = run_site_command(
        &mut provider,
        "ADDUSER example s3cret *@192.0.2.*",
        &quick(),
    )
    .await;
    match run.outcome {
        SiteOutcome::Unknown {
            cause,
            session_reconnected,
            ..
        } => {
            assert_eq!(cause, UnknownCause::ConnectionLost);
            assert!(session_reconnected);
        }
        other => panic!("expected an unknown outcome, got {other:?}"),
    }
    let wire = wire.lock().unwrap();
    assert_eq!(wire.site_lines().len(), 1, "{:?}", wire.site_lines());
    assert_eq!(wire.connections, 2);
}

// T6
#[tokio::test]
async fn site_help_with_214_is_a_reply_not_an_error() {
    let (mut provider, _) = provider_with(reply(
        b"214-The following SITE commands are recognized:\r\n CHMOD CHGRP HELP\r\n214 Direct comments to root\r\n",
    ))
    .await;
    let run = run_site_command(&mut provider, "HELP", &quick()).await;
    let (code, lines, _, _) = replied(run.outcome);
    assert_eq!(code, 214);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1], " CHMOD CHGRP HELP");
}

// T7
#[tokio::test]
async fn a_code_suppaftp_does_not_list_keeps_its_number() {
    let (mut provider, _) = provider_with(reply(b"299 Odd but final\r\n")).await;
    let (code, _, _, _) = replied(
        run_site_command(&mut provider, "VERS", &quick())
            .await
            .outcome,
    );
    assert_eq!(code, 299, "not 0, which is what Status::Unknown carries");
}

// T9
#[tokio::test]
async fn a_refused_line_never_reaches_the_wire() {
    let (mut provider, wire) = provider_with(reply(b"200 ok\r\n")).await;
    for (raw, expected) in [
        ("WHO\r\nDELE ledger", SiteInputError::ControlCharacter),
        ("CHPASS a b\0c", SiteInputError::ControlCharacter),
        ("SITE", SiteInputError::Empty),
        ("   ", SiteInputError::Empty),
    ] {
        let run = run_site_command(&mut provider, raw, &quick()).await;
        assert_eq!(
            run.outcome,
            SiteOutcome::NotSent(NotSentReason::Input(expected)),
            "{raw:?}"
        );
    }
    let too_long = format!("MSG {}", "x".repeat(2000));
    let run = run_site_command(&mut provider, &too_long, &quick()).await;
    assert_eq!(
        run.outcome,
        SiteOutcome::NotSent(NotSentReason::Input(SiteInputError::TooLong))
    );
    let wire = wire.lock().unwrap();
    assert!(wire.site_lines().is_empty(), "{:?}", wire.site_lines());
    assert!(
        !wire.lines.iter().any(|(_, line)| line.contains("DELE")),
        "no fragment of a refused line may cross the wire: {:?}",
        wire.lines
    );
}

// T10
#[tokio::test]
async fn the_typed_prefix_is_not_doubled() {
    let (mut provider, wire) = provider_with(reply(b"200 ok\r\n")).await;
    run_site_command(&mut provider, "  site who  ", &quick()).await;
    assert_eq!(wire.lock().unwrap().site_lines(), vec!["SITE who"]);
}

// T11
#[tokio::test]
async fn a_latin1_reply_is_decoded_byte_for_byte() {
    let (mut provider, _) = provider_with(reply(b"200 Caf\xe9 ok\r\n")).await;
    let (_, lines, encoding, _) = replied(
        run_site_command(&mut provider, "VERS", &quick())
            .await
            .outcome,
    );
    assert_eq!(encoding, ReplyEncoding::Latin1);
    assert_eq!(lines, vec!["200 Caf\u{e9} ok"]);
}

// T12
#[tokio::test]
async fn a_surplus_reply_resets_the_session_so_the_next_command_reads_its_own() {
    let (mut provider, wire) = provider_with(reply(b"200 First\r\n226 Surplus\r\n")).await;
    let (code, lines, _, session_reset) = replied(
        run_site_command(&mut provider, "WHO", &quick())
            .await
            .outcome,
    );
    assert_eq!(code, 200);
    assert_eq!(lines, vec!["200 First"]);
    assert!(session_reset);
    let next = run_site_command(&mut provider, "WHO", &quick()).await;
    let (_, lines, _, _) = replied(next.outcome);
    assert_eq!(
        lines,
        vec!["200 ok"],
        "the surplus must not be read as this reply"
    );
    assert_eq!(wire.lock().unwrap().connections, 2);
}

// T13
#[tokio::test]
async fn a_preliminary_reply_is_followed_to_the_final_one() {
    let (mut provider, _) = provider_with(reply(b"150 Working\r\n200 Done\r\n")).await;
    let (code, lines, _, session_reset) = replied(
        run_site_command(&mut provider, "RESCAN", &quick())
            .await
            .outcome,
    );
    assert_eq!(code, 200);
    assert_eq!(lines, vec!["150 Working", "200 Done"]);
    assert!(!session_reset);
}

// T14
#[tokio::test]
async fn a_session_that_is_not_ftp_is_refused_before_anything_is_sent() {
    let mut webdav = WebDavProvider::new(WebDavConfig {
        url: "https://dav.invalid/".to_string(),
        username: "u".to_string(),
        password: secrecy::SecretString::from("p".to_string()),
        initial_path: None,
        provider_id: None,
        verify_cert: true,
        anonymous: false,
    })
    .unwrap();
    let run = run_site_command(&mut webdav, "WHO", &quick()).await;
    assert_eq!(run.outcome, SiteOutcome::NotSent(NotSentReason::NotFtp));
}

#[tokio::test]
async fn a_provider_that_never_connected_has_nothing_to_send_on() {
    let mut provider = FtpProvider::new(FtpConfig {
        host: "127.0.0.1".to_string(),
        port: 9,
        username: "u".to_string(),
        password: secrecy::SecretString::from("p".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: None,
    });
    let run = run_site_command(&mut provider, "WHO", &quick()).await;
    assert_eq!(
        run.outcome,
        SiteOutcome::NotSent(NotSentReason::NotConnected)
    );
}

// T15
#[tokio::test]
async fn a_reply_over_the_cap_is_abandoned_and_the_session_rebuilt() {
    let mut big = Vec::new();
    while big.len() <= 300 * 1024 {
        big.extend_from_slice(
            b"200- a line of a very long user list, padded to make it longer\r\n",
        );
    }
    big.extend_from_slice(b"200 end\r\n");
    let (mut provider, wire) = provider_with(reply(&big)).await;
    let run = run_site_command(&mut provider, "USERS", &quick()).await;
    match run.outcome {
        SiteOutcome::Unknown {
            cause,
            session_reconnected,
            ..
        } => {
            assert_eq!(cause, UnknownCause::ReplyTooLarge);
            assert!(session_reconnected);
        }
        other => panic!("expected an abandoned reply, got {other:?}"),
    }
    assert_eq!(wire.lock().unwrap().site_lines().len(), 1);
}

// T18
/// Many preliminary replies in one burst: the follow-up loop decodes only the
/// reply it just read, so the cost is linear. Decoding the whole accumulated
/// body on every turn made 128 KiB of `150` lines take about 16 s.
#[tokio::test]
async fn a_burst_of_preliminary_replies_is_followed_in_linear_time() {
    let mut burst = Vec::new();
    while burst.len() <= 128 * 1024 {
        burst.extend_from_slice(b"150 x\r\n");
    }
    burst.extend_from_slice(b"200 Done\r\n");
    let (mut provider, wire) = provider_with(reply(&burst)).await;
    let started = std::time::Instant::now();
    let run = run_site_command(
        &mut provider,
        "RESCAN",
        &SiteOptions::with_reply_timeout_secs(60),
    )
    .await;
    let elapsed = started.elapsed();
    let (code, lines, _, _) = replied(run.outcome);
    assert_eq!(code, 200);
    assert_eq!(lines.last().map(String::as_str), Some("200 Done"));
    assert!(
        elapsed < Duration::from_secs(8),
        "following the 1xx replies took {elapsed:?}"
    );
    assert_eq!(wire.lock().unwrap().site_lines().len(), 1);
}

// T19
/// suppaftp caps each reply at 256 KiB; the exchange as a whole has the same
/// cap, so endless `1xx` lines cannot grow it without limit.
#[tokio::test]
async fn preliminary_replies_past_the_exchange_cap_are_abandoned() {
    let mut burst = Vec::new();
    while burst.len() <= 300 * 1024 {
        burst.extend_from_slice(b"150 x\r\n");
    }
    burst.extend_from_slice(b"200 Done\r\n");
    let (mut provider, wire) = provider_with(reply(&burst)).await;
    let started = std::time::Instant::now();
    let run = run_site_command(
        &mut provider,
        "RESCAN",
        &SiteOptions::with_reply_timeout_secs(60),
    )
    .await;
    match run.outcome {
        SiteOutcome::Unknown { cause, .. } => assert_eq!(cause, UnknownCause::ReplyTooLarge),
        other => panic!("expected the exchange to be abandoned at the cap, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(wire.lock().unwrap().site_lines().len(), 1);
}

// T16
#[tokio::test]
async fn a_final_line_without_text_cannot_hang_the_session() {
    let (mut provider, _) = provider_with(reply(b"200\r\n")).await;
    let started = std::time::Instant::now();
    let run = run_site_command(&mut provider, "WHO", &quick()).await;
    assert!(
        matches!(
            run.outcome,
            SiteOutcome::Unknown {
                cause: UnknownCause::Timeout,
                ..
            }
        ),
        "{:?}",
        run.outcome
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

// T17
#[tokio::test]
async fn chmod_takes_any_2xx_and_reports_a_refusal() {
    let (mut provider, wire) = provider_with(reply(b"250 Mode changed\r\n")).await;
    provider
        .chmod("/pub/a file ", 0o644)
        .await
        .expect("250 is done");
    assert_eq!(
        wire.lock().unwrap().site_lines(),
        vec!["SITE CHMOD 644 /pub/a file "],
        "the path is sent as given, trailing space included"
    );

    // A name with a TAB reaches the server, as it did before chmod moved onto
    // the SITE core: only CR, LF and NUL are refused on this path.
    let (mut provider, wire) = provider_with(reply(b"200 ok\r\n")).await;
    provider
        .chmod("/pub/a\tb", 0o600)
        .await
        .expect("a TAB in a name is sent");
    assert_eq!(
        wire.lock().unwrap().site_lines(),
        vec!["SITE CHMOD 600 /pub/a\tb"]
    );

    let (mut provider, _) = provider_with(reply(b"550 Permission denied\r\n")).await;
    let err = provider.chmod("/pub/x", 0o600).await.unwrap_err();
    assert!(
        matches!(&err, ProviderError::ServerError(message) if message.contains("550 Permission denied")),
        "{err:?}"
    );
}
