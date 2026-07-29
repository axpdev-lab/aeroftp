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
//!             present but refusing looks like. What the test pins here is that
//!             the call is still recorded and the app survives it. Whether GTK
//!             then falls back to an in-process chooser is NOT asserted: the
//!             source comment in lib.rs claims it does, and the no-portal case
//!             measured that it does not, so the claim is not repeated as fact
//!             anywhere it has not been measured.
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

use aeroftp_fake_portal::{request_path, sanitize_path_element};

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

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
        //
        // Say so loudly when that write fails. A broken --log path otherwise
        // leaves an empty evidence file, and an empty evidence file is read by
        // the harness as "the portal was never asked" -- the exact false
        // accusation this whole crate exists to prevent.
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
        {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{line}").and_then(|()| f.flush()) {
                    eprintln!(
                        "fake-portal: FAILED to write the call record to {}: {e}\n\
                         The evidence file is incomplete; do not read it as 'never asked'.",
                        self.log.display()
                    );
                }
            }
            Err(e) => eprintln!(
                "fake-portal: FAILED to open the log {} for append: {e}\n\
                 The evidence file is incomplete; do not read it as 'never asked'.",
                self.log.display()
            ),
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
}

impl FileChooser {
    fn token_of(options: &HashMap<String, OwnedValue>) -> Option<String> {
        options
            .get("handle_token")
            .and_then(|v| String::try_from(v.clone()).ok())
            .map(|t| sanitize_path_element(&t, "aeroftp"))
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
        sender: String,
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
            // A portal that is present but refuses. The negative test asserts
            // that the call was still recorded and that the app survives it,
            // not what GTK does next -- that has not been measured.
            return Err(fdo::Error::Failed(
                "fake portal refusing on purpose (--mode error)".into(),
            ));
        }

        let serial = self.serial.fetch_add(1, Ordering::SeqCst);
        let token = token.unwrap_or_else(|| format!("aeroftp{serial}"));
        let path_str = request_path(&sender, &token);
        let path = ObjectPath::try_from(path_str.clone())
            .map_err(|e| fdo::Error::Failed(format!("bad request path {path_str}: {e}")))?;

        // Export the Request on the handle we are about to hand back, not on the
        // portal's own path. GTK calls Close() on the returned handle when its
        // dialog goes away, and zbus dispatches on an exact path match: a
        // Request registered at /org/freedesktop/portal/desktop is never reached
        // from /org/freedesktop/portal/desktop/request/<sender>/<token>, so the
        // app would log a bus error that reads like a portal failure.
        //
        // It is deliberately NOT unexported after the Response. The real portal
        // does drop it, but keeping it costs one object per call in a process
        // that lives for one test, and it means a Close() racing the Response
        // still finds an implementation instead of producing that same
        // misleading error.
        conn.object_server().at(&path, Request).await.map_err(|e| {
            fdo::Error::Failed(format!("cannot export the request {path_str}: {e}"))
        })?;

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
    /// The caller's unique bus name, which decides the Request path.
    ///
    /// It is read from the header and passed down the call it belongs to. It
    /// used to be stashed in a shared field first and read back a few lines
    /// later, which is a race: zbus runs a task per incoming method call, so a
    /// second chooser call arriving in between made the first one predict its
    /// Request path from the WRONG sender -- and emit its Response where its own
    /// caller was not listening. That is the one failure this stand-in must
    /// never produce, because it looks exactly like the app hanging.
    fn sender_of(hdr: &Header<'_>) -> String {
        hdr.sender().map(|s| s.to_string()).unwrap_or_default()
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
        let sender = Self::sender_of(&hdr);
        self.answer(conn, "OpenFile", parent_window, title, options, sender)
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
        let sender = Self::sender_of(&hdr);
        self.answer(conn, "SaveFile", parent_window, title, options, sender)
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
///
/// It is exported per call, on the handle returned to that caller. See the
/// registration in `answer()` for why the portal's own path is the wrong place
/// for it.
struct Request;

#[interface(name = "org.freedesktop.portal.Request")]
impl Request {
    fn close(&self) {}
}

/// `org.freedesktop.portal.NetworkMonitor`, and it is NOT optional surface.
///
/// Measured on a CI runner, three cases out of three: with the stand-in owning
/// the portal name the app never reached `app_ready` and the splash hit its
/// safety timeout, while the no-portal case in the SAME job started normally
/// and rendered its frontend. The only portal-related line in the difference was
///
///   GLib-GIO-WARNING: GDBus.Error:org.freedesktop.DBus.Error.UnknownInterface:
///   Unknown interface 'org.freedesktop.portal.NetworkMonitor'
///
/// Under `GTK_USE_PORTAL=1` GIO routes `g_network_monitor_get_default()` through
/// the portal instead of netlink. A portal that owns the name and does not
/// answer leaves GIO with a network monitor that cannot say the network is up,
/// and WebKitGTK consults exactly that before loading a URL -- so it declined to
/// load even `http://127.0.0.1:14321`, which the harness proved was serving a
/// 200 with the real index.html at that moment.
///
/// So an incomplete stand-in does not degrade into "no portal". It becomes a
/// portal that breaks the application, and the damage lands nowhere near the
/// file chooser: it looked like WebKit failing to render in CI, and cost a round
/// of environment flags aimed at the wrong thing.
///
/// The answers are the boring healthy ones. Nothing here is asserted by the
/// chooser test; it exists so the app under test sees a portal that behaves.
struct NetworkMonitor;

#[interface(name = "org.freedesktop.portal.NetworkMonitor")]
impl NetworkMonitor {
    fn get_available(&self) -> bool {
        true
    }

    fn get_metered(&self) -> bool {
        false
    }

    /// 4 = `G_NETWORK_CONNECTIVITY_FULL`.
    fn get_connectivity(&self) -> u32 {
        4
    }

    fn can_reach(&self, _hostname: String, _port: u32) -> bool {
        true
    }

    /// GIO reads this to decide which calls it may make. The properties below
    /// cover the older path that reads them directly instead.
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property, name = "available")]
    fn available(&self) -> bool {
        true
    }

    #[zbus(property, name = "metered")]
    fn metered(&self) -> bool {
        false
    }

    #[zbus(property, name = "connectivity")]
    fn connectivity(&self) -> u32 {
        4
    }
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
    };

    // FileChooser and NetworkMonitor live at the portal's own path. Each Request
    // is exported later, on the per-call handle the caller is given.
    //
    // NetworkMonitor is here because owning the portal name is a promise: GIO
    // stops using netlink the moment somebody answers to it. See the type for
    // what an unanswered promise did to the app under test.
    let conn = connection::Builder::session()?
        .name(PORTAL_NAME)?
        .serve_at(PORTAL_PATH, chooser)?
        .serve_at(PORTAL_PATH, NetworkMonitor)?
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
