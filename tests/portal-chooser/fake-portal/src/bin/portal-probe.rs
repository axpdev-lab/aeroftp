//! A minimal portal *client*, used to test the fake portal the way GTK uses a
//! real one.
//!
//! Watching the bus with `gdbus monitor` was the first attempt and it is the
//! wrong instrument twice over: it needs a `--dest`, and a destination filter on
//! the well-known name does not necessarily see a signal the portal emits under
//! its unique name. Worse, observing a message on the bus would not prove the
//! thing that actually matters, which is that a client which subscribed the way
//! GTK subscribes *receives* it.
//!
//! So this does exactly what a real caller does, in the same order:
//!   1. invent a handle_token,
//!   2. subscribe to Response on the PREDICTED request path BEFORE calling,
//!      because the reply can arrive before the method return,
//!   3. call OpenFile,
//!   4. wait, with a timeout, and report what came back,
//!   5. optionally call Close() on the handle it was given, which is what GTK
//!      does when its dialog goes away (`--close`).
//!
//! Exit codes:
//!   0 - a Response arrived (its code is printed)
//!   4 - the call succeeded but no Response ever arrived (the hang the fake
//!       portal must never cause)
//!   5 - the method call itself failed (what --mode error looks like from here)
//!   7 - Close() on the returned handle was not dispatched (the stand-in
//!       exported the Request somewhere the caller cannot reach)
//!   8 - `--netmon`: the portal owns the name but does not answer
//!       org.freedesktop.portal.NetworkMonitor
//!   9 - `--netmon`: same, for org.freedesktop.portal.ProxyResolver
//!   2 - usage/bus error

use std::collections::HashMap;
use std::time::Duration;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{proxy, Connection};

use aeroftp_fake_portal::{request_path, sanitize_path_element};

/// The interface GIO reaches for when something owns the portal name. Probed
/// here because its absence did not look like its absence: it looked like
/// WebKit failing to render in CI.
#[proxy(
    interface = "org.freedesktop.portal.NetworkMonitor",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait NetworkMonitor {
    fn get_available(&self) -> zbus::Result<bool>;
    fn get_metered(&self) -> zbus::Result<bool>;
    fn get_connectivity(&self) -> zbus::Result<u32>;
    /// What a v3-aware GIO calls instead of the three getters above.
    fn get_status(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

/// The other interface GIO takes over once the portal name is owned. WebKit
/// resolves a proxy before fetching a URL, loopback included.
#[proxy(
    interface = "org.freedesktop.portal.ProxyResolver",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait ProxyResolver {
    fn lookup(&self, uri: &str) -> zbus::Result<Vec<String>>;
}

#[proxy(
    interface = "org.freedesktop.portal.FileChooser",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait FileChooser {
    fn open_file(
        &self,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.portal.Request",
    default_service = "org.freedesktop.portal.Desktop"
)]
trait Request {
    #[zbus(signal)]
    fn response(&self, response: u32, results: HashMap<String, OwnedValue>) -> zbus::Result<()>;

    /// What GTK calls on the handle when its own dialog goes away. It is a
    /// method on the RETURNED path, not on the portal's path, which is the
    /// distinction the stand-in got wrong once.
    fn close(&self) -> zbus::Result<()>;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut directory = false;
    let mut token = "probe0".to_string();
    let mut timeout_secs = 10u64;
    let mut close_handle = false;
    let mut netmon_only = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--directory" => {
                directory = true;
                i += 1;
            }
            "--close" => {
                close_handle = true;
                i += 1;
            }
            "--netmon" => {
                netmon_only = true;
                i += 1;
            }
            "--token" => {
                token = args.get(i + 1).cloned().ok_or("--token needs a value")?;
                i += 2;
            }
            "--timeout" => {
                timeout_secs = args
                    .get(i + 1)
                    .ok_or("--timeout needs a value")?
                    .parse()
                    .map_err(|_| "--timeout wants an integer")?;
                i += 2;
            }
            other => {
                eprintln!("portal-probe: unknown argument '{other}'");
                std::process::exit(2);
            }
        }
    }

    // Same sanitisation as the portal, from the same function: a token is an
    // object path ELEMENT, so anything outside [A-Za-z0-9_] has to be mapped
    // before it is used to predict a path, or the prediction fails before the
    // call is even made. Sharing the code is what keeps the prediction and the
    // export from drifting apart silently.
    let token = sanitize_path_element(&token, "probe");

    let conn = Connection::session().await?;

    // Ask the way GIO asks, and stop. A portal that owns the name but does not
    // answer this leaves the application with a network monitor that cannot say
    // the network is up, and WebKit then refuses to load anything at all --
    // including loopback.
    if netmon_only {
        let nm = NetworkMonitorProxy::new(&conn).await?;
        match (
            nm.get_available().await,
            nm.get_metered().await,
            nm.get_connectivity().await,
        ) {
            (Ok(a), Ok(m), Ok(c)) => {
                println!("probe: NETMON available={a} metered={m} connectivity={c}");
            }
            (a, m, c) => {
                println!("probe: NETMON FAILED available={a:?} metered={m:?} connectivity={c:?}");
                std::process::exit(8);
            }
        }

        // Ask the way a v3-aware GIO asks. The stand-in advertises version 3, so
        // this method has to exist: claiming a version and not honouring it is
        // the same defect as not implementing the interface at all, only it
        // waits for whichever runner upgrades GLib first.
        match nm.get_status().await {
            Ok(status) => {
                let mut keys: Vec<&str> = status.keys().map(|k| k.as_str()).collect();
                keys.sort_unstable();
                println!("probe: NETMON GetStatus keys={keys:?}");
            }
            Err(e) => {
                println!("probe: NETMON GetStatus FAILED {e:?}");
                std::process::exit(8);
            }
        }

        let pr = ProxyResolverProxy::new(&conn).await?;
        match pr.lookup("http://127.0.0.1:14321/index.html").await {
            Ok(proxies) => {
                println!("probe: PROXY lookup={proxies:?}");
                return Ok(());
            }
            Err(e) => {
                println!("probe: PROXY FAILED {e:?}");
                std::process::exit(9);
            }
        }
    }

    let unique = conn
        .unique_name()
        .ok_or("no unique name on the session bus")?
        .to_string();
    let predicted = request_path(&unique, &token);
    println!("probe: predicted request path {predicted}");

    // Subscribe FIRST. This ordering is the whole point: a portal is allowed to
    // answer before its method call returns, and a client that subscribes
    // afterwards can miss the reply and wait forever.
    let request = RequestProxy::builder(&conn)
        .path(predicted.clone())?
        .build()
        .await?;
    let mut responses = request.receive_response().await?;

    let chooser = FileChooserProxy::new(&conn).await?;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    if directory {
        options.insert("directory", Value::from(true));
    }

    let handle = match chooser.open_file("", "probe", options).await {
        Ok(h) => h,
        Err(e) => {
            println!("probe: OpenFile failed: {e}");
            std::process::exit(5);
        }
    };
    println!("probe: OpenFile returned {}", handle.as_str());
    if handle.as_str() != predicted {
        // Not fatal for the protocol, but it means a real client would have
        // subscribed to the wrong path, so say so loudly.
        println!("probe: MISMATCH - returned path differs from the predicted one");
    }

    use futures_util::StreamExt;
    match tokio::time::timeout(Duration::from_secs(timeout_secs), responses.next()).await {
        Ok(Some(signal)) => {
            let args = signal.args()?;
            let keys: Vec<&str> = args.results.keys().map(|k| k.as_str()).collect();
            println!("probe: RESPONSE code={} results={:?}", args.response, keys);

            // Close() has to reach an implementation on the path we were HANDED
            // BACK. A stand-in that exports Request on the portal's own path
            // answers everything else correctly and fails only here, so without
            // this the omission is invisible: the app just logs a bus error
            // that reads like the portal misbehaving.
            if close_handle {
                let on_handle = RequestProxy::builder(&conn)
                    .path(handle.as_str().to_string())?
                    .build()
                    .await?;
                match on_handle.close().await {
                    Ok(()) => println!("probe: CLOSE ok on {}", handle.as_str()),
                    Err(e) => {
                        println!("probe: CLOSE FAILED on {}: {e}", handle.as_str());
                        std::process::exit(7);
                    }
                }
            }
            Ok(())
        }
        Ok(None) => {
            println!("probe: signal stream ended without a Response");
            std::process::exit(4);
        }
        Err(_) => {
            println!("probe: TIMEOUT after {timeout_secs}s with no Response");
            std::process::exit(4);
        }
    }
}
