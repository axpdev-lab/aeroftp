//! A stand-in for `xdg-desktop-portal`, for one purpose: to find out whether
//! AeroFTP's file chooser really goes through the portal, and to answer it
//! deterministically so a test never waits on a human.
//!
//! Why a fake instead of the real portal: the real one opens a chooser window
//! that only a person can dismiss, picks a backend that depends on the host
//! desktop, and gives no way to assert *that it was asked*. This one owns
//! `org.freedesktop.portal.Desktop` on an isolated session bus, records every
//! `FileChooser` call to a JSON file, and answers with a scripted outcome.
//!
//! It records rather than infers. The interesting assertion in #464 is not that
//! a dialog appeared, it is that the request left our process at all: under
//! `GTK_USE_PORTAL=1` the chooser is supposed to run out-of-process precisely so
//! it cannot corrupt the GLib heap, and nothing in the app can prove that from
//! the inside.
//!
//! Modes (`--mode`):
//!   cancel  - answer as if the user dismissed the dialog (Response code 1).
//!             The app must treat this as "no selection", not as an error.
//!   error   - fail the D-Bus call itself, which is what a portal that is
//!             present but refusing looks like. GTK is then expected to fall
//!             back to the in-process chooser, which is the documented
//!             behaviour the negative test pins.
//!   accept  - answer with a real path (Response code 0), so the success path
//!             is covered too and "cancel" cannot pass by doing nothing.
//!
//! The exit code carries the verdict so a shell harness needs no parsing:
//!   0 - at least one FileChooser call was recorded
//!   3 - the portal was never asked (this is the failure #464 exists to catch)
//!   2 - usage or bus error

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use zbus::message::Header;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{connection, fdo, interface, Connection};

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

/// A D-Bus object path element accepts only `[A-Za-z0-9_]`. The portal spec says
/// `handle_token` must already be a valid element, and GTK obeys that, but a
/// token with a hyphen in it makes the portal build a path the bus rejects and
/// the caller then sees `InvalidObjectPath` from somewhere deep in zvariant,
/// with nothing pointing at the token. Found exactly that way while testing this
/// harness. Both sides sanitize identically so a sloppy token degrades into a
/// working request instead of an unreadable error.
fn sanitize_path_element(token: &str) -> String {
    let mapped: String = token
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() {
        "aeroftp".to_string()
    } else {
        mapped
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Cancel,
    Error,
    Accept,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "cancel" => Some(Mode::Cancel),
            "error" => Some(Mode::Error),
            "accept" => Some(Mode::Accept),
            _ => None,
        }
    }
}

/// One recorded portal call. Written as JSON so the shell harness can assert on
/// it without re-implementing D-Bus.
#[derive(Debug)]
struct Call {
    method: String,
    parent_window: String,
    title: String,
    /// `true` when the caller asked for a folder rather than a file. GTK sets
    /// this for `open({directory:true})`, which is the AeroFTP flow that
    /// historically tripped the heap corruption.
    directory: bool,
    handle_token: Option<String>,
    answered: &'static str,
}

impl Call {
    fn to_json(&self) -> String {
        let esc = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
        format!(
            "{{\"method\":{},\"parent_window\":{},\"title\":{},\"directory\":{},\"handle_token\":{},\"answered\":{}}}",
            esc(&self.method),
            esc(&self.parent_window),
            esc(&self.title),
            self.directory,
            match &self.handle_token {
                Some(t) => esc(t),
                None => "null".to_string(),
            },
            esc(self.answered),
        )
    }
}

struct Recorder {
    calls: Mutex<Vec<Call>>,
    log: PathBuf,
}

impl Recorder {
    fn record(&self, call: Call) {
        let line = call.to_json();
        self.calls.lock().expect("recorder poisoned").push(call);
        // Append immediately: if the app crashes or the harness times out, the
        // evidence that the portal WAS reached must survive. Buffering it until
        // shutdown would lose exactly the runs worth diagnosing.
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
        {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
        eprintln!("fake-portal: recorded {line}");
    }

    fn count(&self) -> usize {
        self.calls.lock().expect("recorder poisoned").len()
    }
}

struct FileChooser {
    mode: Mode,
    recorder: Arc<Recorder>,
    serial: AtomicU32,
    /// Path handed back on `accept`. A directory that exists, so the app's own
    /// validation of the returned path cannot reject it for being fictional.
    accept_path: String,
    /// The caller's unique bus name, taken from the message header on every
    /// call, because the Request path is caller-specific.
    last_sender: Mutex<String>,
}

impl FileChooser {
    /// Build the Request object path the way xdg-desktop-portal does, because
    /// the caller predicts it and subscribes BEFORE the method returns.
    ///
    /// Getting this wrong is the classic way a fake portal appears to work and
    /// silently never delivers: the reply goes to a path nobody is listening
    /// on, the caller waits forever, and the failure looks like a hang in the
    /// application rather than a bug in the stub. The rule is
    /// `/org/freedesktop/portal/desktop/request/<SENDER>/<TOKEN>` where SENDER
    /// is the caller's unique name with the leading ':' dropped and every '.'
    /// replaced by '_'.
    fn request_path(sender: &str, token: &str) -> String {
        let escaped = sender.trim_start_matches(':').replace('.', "_");
        format!("/org/freedesktop/portal/desktop/request/{escaped}/{token}")
    }

    fn token_of(options: &HashMap<String, OwnedValue>) -> Option<String> {
        options
            .get("handle_token")
            .and_then(|v| String::try_from(v.clone()).ok())
            .map(|t| sanitize_path_element(&t))
    }

    fn is_directory(options: &HashMap<String, OwnedValue>) -> bool {
        options
            .get("directory")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false)
    }

    async fn answer(
        &self,
        conn: &Connection,
        method: &str,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<OwnedObjectPath> {
        let directory = Self::is_directory(&options);
        let token = Self::token_of(&options);

        if self.mode == Mode::Error {
            self.recorder.record(Call {
                method: method.to_string(),
                parent_window,
                title,
                directory,
                handle_token: token,
                answered: "dbus-error",
            });
            // A portal that is present but refuses. GTK falls back to the
            // in-process chooser from here, which is what the negative test
            // asserts.
            return Err(fdo::Error::Failed(
                "fake portal refusing on purpose (--mode error)".into(),
            ));
        }

        let serial = self.serial.fetch_add(1, Ordering::SeqCst);
        let token = token.unwrap_or_else(|| format!("aeroftp{serial}"));
        let sender = self.last_sender.lock().expect("sender poisoned").clone();
        let path_str = Self::request_path(&sender, &token);
        let path = ObjectPath::try_from(path_str.clone())
            .map_err(|e| fdo::Error::Failed(format!("bad request path {path_str}: {e}")))?;

        let (response, answered): (u32, &'static str) = match self.mode {
            Mode::Cancel => (1, "cancelled"),
            Mode::Accept => (0, "accepted"),
            Mode::Error => unreachable!("handled above"),
        };

        self.recorder.record(Call {
            method: method.to_string(),
            parent_window,
            title,
            directory,
            handle_token: Some(token),
            answered,
        });

        let mut results: HashMap<String, OwnedValue> = HashMap::new();
        if response == 0 {
            let uri = format!("file://{}", self.accept_path);
            let uris = vec![uri];
            let v = Value::from(uris)
                .try_to_owned()
                .map_err(|e| fdo::Error::Failed(format!("uris: {e}")))?;
            results.insert("uris".to_string(), v);
        }

        // Emit AFTER returning the handle would be a race; emitting before the
        // method reply is what the real portal does and is safe because the
        // caller subscribed to the predicted path first.
        let conn = conn.clone();
        let emit_path = path.to_owned();
        tokio::spawn(async move {
            // A beat, so the caller's method reply lands first even on a bus
            // with no ordering guarantees between a reply and a signal.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = conn
                .emit_signal(
                    None::<&str>,
                    &emit_path,
                    REQUEST_IFACE,
                    "Response",
                    &(response, results),
                )
                .await;
        });

        Ok(OwnedObjectPath::from(path))
    }
}

impl FileChooser {
    fn set_sender(&self, hdr: &Header<'_>) {
        if let Some(s) = hdr.sender() {
            *self.last_sender.lock().expect("sender poisoned") = s.to_string();
        }
    }
}

#[interface(name = "org.freedesktop.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        self.set_sender(&hdr);
        self.answer(conn, "OpenFile", parent_window, title, options)
            .await
    }

    async fn save_file(
        &self,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<OwnedObjectPath> {
        self.set_sender(&hdr);
        self.answer(conn, "SaveFile", parent_window, title, options)
            .await
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        // GTK refuses the portal path outright if this is missing or too low,
        // which would make the whole test pass vacuously by falling back.
        3
    }
}

/// The Request object the caller may call `Close()` on. Exporting it matters:
/// GTK closes the request when its own dialog goes away, and a missing
/// interface turns that into a bus error in the app's log that reads like a
/// portal failure.
struct Request;

#[interface(name = "org.freedesktop.portal.Request")]
impl Request {
    fn close(&self) {}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mode = Mode::Cancel;
    let mut log = PathBuf::from("portal-calls.jsonl");
    let mut accept_path = "/tmp".to_string();
    let mut ready_file: Option<PathBuf> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let need = |i: usize| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--mode" => {
                let v = need(i)?;
                mode = Mode::parse(&v).ok_or_else(|| format!("unknown mode '{v}'"))?;
                i += 2;
            }
            "--log" => {
                log = PathBuf::from(need(i)?);
                i += 2;
            }
            "--accept-path" => {
                accept_path = need(i)?;
                i += 2;
            }
            "--ready-file" => {
                ready_file = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            other => {
                eprintln!("fake-portal: unknown argument '{other}'");
                std::process::exit(2);
            }
        }
    }

    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!(
            "fake-portal: refusing to run without DBUS_SESSION_BUS_ADDRESS.\n\
             This must own {PORTAL_NAME} on an ISOLATED bus; on a developer's real\n\
             session it would fight the desktop's own portal and could take over the\n\
             file chooser for every running application."
        );
        std::process::exit(2);
    }

    let recorder = Arc::new(Recorder {
        calls: Mutex::new(Vec::new()),
        log: log.clone(),
    });

    let chooser = FileChooser {
        mode,
        recorder: Arc::clone(&recorder),
        serial: AtomicU32::new(0),
        accept_path,
        last_sender: Mutex::new(String::new()),
    };

    let conn = connection::Builder::session()?
        .name(PORTAL_NAME)?
        .serve_at(PORTAL_PATH, chooser)?
        .serve_at(PORTAL_PATH, Request)?
        .build()
        .await?;

    eprintln!(
        "fake-portal: owning {PORTAL_NAME} in --mode {mode:?}, log {}",
        log.display()
    );

    // The harness waits on this file rather than on a sleep: a fixed delay is
    // how a slow runner turns "the portal was not up yet" into "the app does
    // not use the portal", which is the exact false negative this test exists
    // to avoid.
    if let Some(rf) = &ready_file {
        std::fs::write(rf, "ready\n")?;
    }

    tokio::signal::ctrl_c().await?;

    let n = recorder.count();
    eprintln!("fake-portal: {n} FileChooser call(s) recorded");
    let _ = conn;
    if n == 0 {
        eprintln!("fake-portal: the portal was NEVER asked - the chooser did not go through it");
        std::process::exit(3);
    }
    Ok(())
}
