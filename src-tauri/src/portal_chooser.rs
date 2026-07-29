//! Whether a file chooser can be presented at all, and why not when it cannot.
//!
//! This exists because the failure is otherwise **invisible to everyone above
//! it**, which the #464 harness measured rather than guessed:
//!
//! - `lib.rs` forces `GTK_USE_PORTAL=1`, because the in-process
//!   `GtkFileChooser` corrupts the GLib heap under WebKitGTK. That makes the
//!   portal the only safe chooser, not merely the preferred one.
//! - On a host without `xdg-desktop-portal`, GTK logs `Can't open portal file
//!   chooser: ...Error.ServiceUnknown` and presents nothing. It does **not**
//!   fall back, whatever the old comment in `lib.rs` claimed.
//! - `rfd`, which `tauri-plugin-dialog` uses, exposes
//!   `pick_file() -> Option<PathBuf>`: **no error channel at all**. So the
//!   plugin answers with a successful `null`, and the frontend cannot tell
//!   "the chooser could not open" from "the user pressed Cancel".
//!
//! The information dies inside `rfd`, so it has to be recovered before the
//! dialog is ever invoked. That is all this module does: it answers whether the
//! portal is reachable, so the UI can say why the picker did nothing instead of
//! leaving the user with a button that silently does nothing.
//!
//! It deliberately does not try to find another chooser. On such a host there
//! isn't one: the in-process fallback is the heap corruption this whole
//! arrangement exists to avoid.

/// The well-known name a desktop portal owns.
#[cfg(target_os = "linux")]
const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";

/// How long to wait for the bus before giving up, in milliseconds. Short on
/// purpose: this runs in front of a user gesture, and a wedged session bus must
/// not turn a click into a freeze. Timing out is reported as "reachable", since
/// refusing to open a chooser on a slow bus would be worse than the bug.
#[cfg(target_os = "linux")]
const BUS_TIMEOUT_MS: i32 = 1_000;

/// Why the file chooser cannot be presented, or `None` when it can.
///
/// The string is a stable machine-readable reason, not user-facing copy: the
/// frontend maps it to a translated message.
#[cfg(not(target_os = "linux"))]
pub fn chooser_unavailable_reason() -> Option<String> {
    // Windows and macOS use their own native pickers, which are always present.
    None
}

#[cfg(target_os = "linux")]
pub fn chooser_unavailable_reason() -> Option<String> {
    // A user who set GTK_USE_PORTAL=0 has deliberately asked for the in-process
    // chooser. It is their call and it does work, so nothing here applies. Only
    // an explicit "0" counts: `lib.rs` sets "1" when the variable is absent.
    if std::env::var("GTK_USE_PORTAL").as_deref() == Ok("0") {
        return None;
    }

    match portal_is_reachable() {
        Some(true) => None,
        Some(false) => Some("portal-missing".to_string()),
        // The bus could not be questioned. Say nothing rather than accuse the
        // host: a false "your chooser is broken" on a healthy machine is worse
        // than the silence this module exists to fix.
        None => None,
    }
}

/// `true` when a portal would answer, `false` when nothing could, `None` when
/// the session bus itself could not be questioned.
///
/// Two conditions, and the second is the one that is easy to forget: a portal
/// is **D-Bus activatable**, so on a perfectly healthy machine that has not
/// opened a chooser yet, nobody owns the name and it starts on first use.
/// Checking only `NameHasOwner` would therefore report "no portal" to every
/// user until their first file dialog, which is precisely the kind of false
/// alarm that gets a warning ignored.
#[cfg(target_os = "linux")]
fn portal_is_reachable() -> Option<bool> {
    use gtk::gio;
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    reachable_on(&conn)
}

/// The decision itself, on a connection handed in.
///
/// Split from `portal_is_reachable` so it can be exercised against a bus built
/// for the purpose. `gio::bus_get_sync` caches one connection per bus type for
/// the life of the process, so a test that swapped `DBUS_SESSION_BUS_ADDRESS`
/// would silently keep talking to the first bus it ever touched, and would pass
/// while proving nothing.
#[cfg(target_os = "linux")]
fn reachable_on(conn: &gtk::gio::DBusConnection) -> Option<bool> {
    use gtk::gio;
    use gtk::glib;
    use gtk::prelude::*;

    let call = |method: &str, args: Option<&glib::Variant>| -> Option<glib::Variant> {
        conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            args,
            None,
            gio::DBusCallFlags::NONE,
            BUS_TIMEOUT_MS,
            gio::Cancellable::NONE,
        )
        .ok()
    };

    let owned = call("NameHasOwner", Some(&(PORTAL_NAME,).to_variant()))
        .and_then(|r| r.child_value(0).get::<bool>())?;
    if owned {
        return Some(true);
    }

    // Not running. Installed and merely waiting to be activated?
    let activatable =
        call("ListActivatableNames", None).and_then(|r| r.child_value(0).get::<Vec<String>>())?;

    Some(activatable.iter().any(|n| n == PORTAL_NAME))
}

/// Exposed to the frontend so a picker call site can explain itself instead of
/// resolving to a `null` that means two different things.
///
/// The log line is not decoration: it is the only way an outside observer can
/// tell that the frontend really consulted this before opening a picker. The
/// #464 gate greps for it, because the remedy is a message rendered inside the
/// WebView and no window count or D-Bus trace can see that from outside.
///
/// `async`, and blocking on the blocking pool, for one reason each. Tauri runs a
/// **synchronous** command on the main thread, which on Linux is the GTK thread,
/// so a sync version of this would freeze the whole window for as long as the bus
/// takes to answer: `BUS_TIMEOUT_MS` twice, plus whatever `bus_get_sync` spends,
/// in front of the very click this module exists to make honest. That is the
/// promise `BUS_TIMEOUT_MS` documents above and a sync command cannot keep, so
/// declaring it `async` takes it off the main thread and `spawn_blocking` keeps
/// the blocking round trip off the async workers as well.
#[tauri::command]
pub async fn chooser_unavailable() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let reason = chooser_unavailable_reason();
        match &reason {
            Some(r) => eprintln!("{CHOOSER_UNAVAILABLE_LOG} {r}"),
            None => eprintln!("{CHOOSER_CHECKED_LOG}"),
        }
        reason
    })
    .await
    .unwrap_or_else(|err| {
        // The check could not even be run. Same rule as a bus that cannot be
        // questioned: say nothing rather than accuse a healthy host, because a
        // false "your chooser is broken" is worse than the silence being fixed.
        eprintln!("[picker] the chooser check could not run: {err}");
        None
    })
}

/// Printed when a picker cannot be presented. Matched literally by
/// `tests/portal-chooser/portal-chooser-test.sh`; changing it breaks that gate.
pub const CHOOSER_UNAVAILABLE_LOG: &str = "chooser unavailable, telling the user:";

/// Printed when the check ran and found nothing wrong. Its presence is what
/// proves the frontend asked at all, which is what separates "the picker is fine"
/// from "nobody ever checked".
pub const CHOOSER_CHECKED_LOG: &str = "chooser checked: presentable";

#[cfg(test)]
mod tests {
    use super::*;

    /// Held by every test that writes or depends on `GTK_USE_PORTAL`.
    ///
    /// The test harness runs tests on several threads, so "restored immediately
    /// after" is not on its own enough: a concurrent test reading the variable can
    /// land inside the window where it is set. `portal_chooser.rs` is the only
    /// place in the crate that reads it during a test run (`lib.rs` writes it once
    /// at startup, which no test reaches), so serialising the tests in this module
    /// is what makes the claim true rather than merely likely.
    #[cfg(target_os = "linux")]
    static PORTAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the guard, surviving a poisoned lock: a panic in one test must fail
    /// that test, not turn every later one into a different failure.
    #[cfg(target_os = "linux")]
    fn lock_portal_env() -> std::sync::MutexGuard<'static, ()> {
        PORTAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The user override wins on every platform, and short-circuits before any
    /// bus traffic. Pinned because getting this wrong would nag exactly the
    /// users who chose the other chooser on purpose.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_explicit_opt_out_reports_no_problem() {
        let _guard = lock_portal_env();
        // SAFETY: the guard above keeps every other reader of this variable in the
        // crate out for the duration, and the value is restored before it drops.
        let previous = std::env::var_os("GTK_USE_PORTAL");
        unsafe { std::env::set_var("GTK_USE_PORTAL", "0") };
        let reason = chooser_unavailable_reason();
        match previous {
            Some(v) => unsafe { std::env::set_var("GTK_USE_PORTAL", v) },
            None => unsafe { std::env::remove_var("GTK_USE_PORTAL") },
        }
        assert_eq!(reason, None, "GTK_USE_PORTAL=0 is a deliberate choice");
    }

    /// On the platforms with a guaranteed native picker there is nothing to
    /// report, and no bus to ask.
    #[test]
    #[cfg(not(target_os = "linux"))]
    fn other_platforms_always_have_a_chooser() {
        assert_eq!(chooser_unavailable_reason(), None);
    }

    /// The command must never run on the main thread: Tauri puts a *synchronous*
    /// command there, and on Linux that is the GTK thread, so a sync version of a
    /// check that waits on the session bus freezes the window in front of a click.
    ///
    /// The pin is the `.await`, and it acts at **compile time**: make
    /// `chooser_unavailable` synchronous again and this test stops building, which
    /// no one can quietly delete the way an assertion can be deleted. The verdict
    /// itself belongs to the four tests around this one, so what is asserted here
    /// is the other half: the wrapper only logs, it does not change the answer.
    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn the_command_stays_off_the_main_thread() {
        // Both calls must see the same `GTK_USE_PORTAL`, or the opt-out test can
        // flip it between them and the two verdicts would disagree for a reason
        // that has nothing to do with what is being pinned here.
        let _guard = lock_portal_env();
        assert_eq!(
            chooser_unavailable().await,
            chooser_unavailable_reason(),
            "the command must report the verdict it was given, not invent one"
        );
    }

    /// The same pin on the platforms where there is no environment to serialise.
    #[tokio::test]
    #[cfg(not(target_os = "linux"))]
    async fn the_command_stays_off_the_main_thread() {
        assert_eq!(chooser_unavailable().await, chooser_unavailable_reason());
    }

    /// A throwaway session bus, built to order, so the three cases below are
    /// measured rather than reasoned about. Returns the connection and the
    /// daemon, which the caller must kill.
    #[cfg(target_os = "linux")]
    fn private_bus(
        servicedir: Option<&std::path::Path>,
    ) -> Option<(gtk::gio::DBusConnection, std::process::Child)> {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("aeroftp-portal-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let conf = dir.join(format!(
            "bus-{}.conf",
            servicedir.map(|_| "svc").unwrap_or("bare")
        ));
        let services = match servicedir {
            Some(p) => format!("<servicedir>{}</servicedir>", p.display()),
            None => String::new(),
        };
        let mut f = std::fs::File::create(&conf).ok()?;
        write!(
            f,
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>{services}
<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>"#
        )
        .ok()?;

        let out = std::process::Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .arg("--print-address=1")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        // `--print-address=1` writes to fd 1 and the daemon keeps running.
        let mut child = out;
        let stdout = child.stdout.take()?;
        let mut reader = std::io::BufReader::new(stdout);
        let mut addr = String::new();
        std::io::BufRead::read_line(&mut reader, &mut addr).ok()?;
        let addr = addr.trim().to_string();
        if addr.is_empty() {
            let _ = child.kill();
            return None;
        }

        let conn = gtk::gio::DBusConnection::for_address_sync(
            &addr,
            gtk::gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gtk::gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None::<&gtk::gio::DBusAuthObserver>,
            gtk::gio::Cancellable::NONE,
        );
        match conn {
            Ok(c) => Some((c, child)),
            Err(_) => {
                let _ = child.kill();
                None
            }
        }
    }

    /// THE pin. A bus where nothing owns the portal name and nothing can be
    /// activated to provide it is exactly the host the #464 harness measured:
    /// GTK presents no chooser and `rfd` reports the same `null` as a cancel.
    /// If this ever returns "reachable", the silence comes back.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_bus_with_no_portal_is_reported_missing() {
        let Some((conn, mut daemon)) = private_bus(None) else {
            eprintln!("skipped: dbus-daemon unavailable");
            return;
        };
        let verdict = reachable_on(&conn);
        let _ = daemon.kill();
        assert_eq!(
            verdict,
            Some(false),
            "a bus with no portal and no way to activate one must read as missing"
        );
    }

    /// The other half, so the check cannot pass by always saying "missing":
    /// when something does own the name, the chooser is available.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_bus_where_the_name_is_owned_is_reported_available() {
        use gtk::prelude::*;
        let Some((conn, mut daemon)) = private_bus(None) else {
            eprintln!("skipped: dbus-daemon unavailable");
            return;
        };
        // Own the portal name on this throwaway bus, the way a portal would.
        let reply = conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(PORTAL_NAME, 0u32).to_variant()),
            None,
            gtk::gio::DBusCallFlags::NONE,
            BUS_TIMEOUT_MS,
            gtk::gio::Cancellable::NONE,
        );
        assert!(reply.is_ok(), "could not claim the name on the test bus");

        let verdict = reachable_on(&conn);
        let _ = daemon.kill();
        assert_eq!(
            verdict,
            Some(true),
            "an owned portal name means a chooser exists"
        );
    }

    /// The subtle one, and the reason `ListActivatableNames` is consulted at
    /// all. A portal is D-Bus activatable: on a healthy machine that has not
    /// opened a chooser yet, nobody owns the name and it starts on first use.
    /// Checking only `NameHasOwner` would warn every user until their first
    /// file dialog, and a warning that cries wolf is a warning nobody reads.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_activatable_portal_is_not_reported_missing() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("aeroftp-portal-svc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svc = dir.join(format!("{PORTAL_NAME}.service"));
        let mut f = std::fs::File::create(&svc).unwrap();
        write!(f, "[D-BUS Service]\nName={PORTAL_NAME}\nExec=/bin/true\n").unwrap();
        drop(f);

        let Some((conn, mut daemon)) = private_bus(Some(&dir)) else {
            eprintln!("skipped: dbus-daemon unavailable");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let verdict = reachable_on(&conn);
        let _ = daemon.kill();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            verdict,
            Some(true),
            "an installed-but-not-yet-running portal must NOT read as missing"
        );
    }
}
