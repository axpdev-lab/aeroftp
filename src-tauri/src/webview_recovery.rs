// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Reload a window whose WebKitGTK web process died.
//!
//! WebKitGTK renders each page in a separate `WebKitWebProcess`. When that
//! process crashes (a JavaScriptCore JIT segfault was seen on 2.52.6) or is
//! killed for exceeding its memory limit, the AeroFTP process keeps running,
//! the tray keeps answering, and the window turns grey and stays grey: nothing
//! in the app ever asked WebKit for a new web process. The only way out was to
//! quit from the tray and start again.
//!
//! WebKit emits `web-process-terminated` for exactly this case, and reloading
//! the view spawns a new web process. The reload is rate-limited so a page
//! that crashes the web process on load cannot turn into a reload loop: after
//! [`MAX_RELOADS`] reloads inside [`RELOAD_WINDOW`] the window is left grey
//! and the failure is logged.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use log::{error, info, warn};
use tauri::{Runtime, WebviewWindow};

/// Reloads allowed inside [`RELOAD_WINDOW`] before giving up.
const MAX_RELOADS: usize = 2;

/// The span over which reloads are counted.
const RELOAD_WINDOW: Duration = Duration::from_secs(60);

/// Record a reload at `now` if fewer than [`MAX_RELOADS`] happened in the
/// last [`RELOAD_WINDOW`]. Returns whether the reload is allowed.
fn admit_reload(history: &mut VecDeque<Instant>, now: Instant) -> bool {
    while history
        .front()
        .is_some_and(|t| now.duration_since(*t) >= RELOAD_WINDOW)
    {
        history.pop_front();
    }
    if history.len() >= MAX_RELOADS {
        return false;
    }
    history.push_back(now);
    true
}

/// Watch `window` for a dead web process and reload it when one dies.
///
/// A termination requested through the API is deliberate and left alone.
pub fn install<R: Runtime>(window: &WebviewWindow<R>) {
    let label = window.label().to_string();
    let result = window.with_webview(move |platform| {
        use webkit2gtk::{WebProcessTerminationReason, WebViewExt};

        // The signal is delivered on the GTK main thread, which owns the
        // closure, so a RefCell is enough.
        let history = RefCell::new(VecDeque::with_capacity(MAX_RELOADS));
        platform
            .inner()
            .connect_web_process_terminated(move |view, reason| {
                if reason == WebProcessTerminationReason::TerminatedByApi {
                    info!("[webview] web process of '{label}' terminated by the API, not reloading");
                    return;
                }
                if admit_reload(&mut history.borrow_mut(), Instant::now()) {
                    warn!("[webview] web process of '{label}' terminated ({reason:?}), reloading");
                    view.reload();
                } else {
                    error!(
                        "[webview] web process of '{label}' terminated ({reason:?}) after {MAX_RELOADS} reloads within {}s, leaving the window as is",
                        RELOAD_WINDOW.as_secs()
                    );
                }
            });
    });
    if let Err(e) = result {
        warn!(
            "[webview] cannot watch the web process of '{}': {e}",
            window.label()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_the_limit_then_refuses() {
        let mut history = VecDeque::new();
        let t0 = Instant::now();
        for i in 0..MAX_RELOADS {
            assert!(admit_reload(
                &mut history,
                t0 + Duration::from_secs(i as u64)
            ));
        }
        assert!(!admit_reload(&mut history, t0 + Duration::from_secs(10)));
    }

    #[test]
    fn a_refused_reload_is_not_recorded() {
        let mut history = VecDeque::new();
        let t0 = Instant::now();
        for _ in 0..MAX_RELOADS {
            assert!(admit_reload(&mut history, t0));
        }
        assert!(!admit_reload(&mut history, t0 + Duration::from_secs(1)));
        assert_eq!(history.len(), MAX_RELOADS);
    }

    #[test]
    fn admits_again_once_the_oldest_reload_leaves_the_window() {
        let mut history = VecDeque::new();
        let t0 = Instant::now();
        for _ in 0..MAX_RELOADS {
            assert!(admit_reload(&mut history, t0));
        }
        assert!(!admit_reload(
            &mut history,
            t0 + RELOAD_WINDOW - Duration::from_millis(1)
        ));
        assert!(admit_reload(&mut history, t0 + RELOAD_WINDOW));
    }
}
