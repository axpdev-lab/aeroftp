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
//!   4. wait, with a timeout, and report what came back.
//!
//! Exit codes:
//!   0 - a Response arrived (its code is printed)
//!   4 - the call succeeded but no Response ever arrived (the hang the fake
//!       portal must never cause)
//!   5 - the method call itself failed (what --mode error looks like from here)
//!   2 - usage/bus error

use std::collections::HashMap;
use std::time::Duration;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{proxy, Connection};

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
}

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
        "probe".to_string()
    } else {
        mapped
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut directory = false;
    let mut token = "probe0".to_string();
    let mut timeout_secs = 10u64;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--directory" => {
                directory = true;
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

    // Same sanitisation as the portal: a token is an object path ELEMENT, so
    // anything outside [A-Za-z0-9_] has to be mapped before it is used to
    // predict a path, or the prediction fails before the call is even made.
    let token = sanitize_path_element(&token);

    let conn = Connection::session().await?;
    let unique = conn
        .unique_name()
        .ok_or("no unique name on the session bus")?
        .to_string();
    let escaped = unique.trim_start_matches(':').replace('.', "_");
    let predicted = format!("/org/freedesktop/portal/desktop/request/{escaped}/{token}");
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
