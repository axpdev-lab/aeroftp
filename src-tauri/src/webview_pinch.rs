// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Touchpad pinch on Linux.
//!
//! WebKitGTK answers a touchpad pinch by magnifying the whole page, and the
//! page never sees the gesture: in the image preview a pinch zoomed the
//! window instead of the picture (#1075). A desktop app's interface is not
//! meant to be pinch-zoomed, so the gesture is taken before WebKit sees it
//! and handed to the frontend as the `touchpad-pinch` event, where the image
//! viewer zooms the picture under the fingers. Elsewhere the event is unused
//! and a pinch does nothing, as in other desktop apps.
//!
//! On Windows (WebView2) and macOS a pinch already reaches the page as a
//! Ctrl+wheel or a gesture event, so nothing is needed there.

use gtk::gdk;
use gtk::prelude::*;
use log::warn;
use serde::Serialize;
use tauri::{Emitter, Runtime, WebviewWindow};

pub const EVENT: &str = "touchpad-pinch";

/// One step of a pinch: `scale` is relative to the start of the gesture,
/// `x` and `y` are where the fingers are, in the page's CSS pixels.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pinch {
    pub phase: &'static str,
    pub scale: f64,
    pub x: f64,
    pub y: f64,
}

/// The name of a GDK touchpad gesture phase, as the frontend reads it.
fn phase_name(raw: i32) -> Option<&'static str> {
    match raw {
        gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_BEGIN => Some("begin"),
        gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_UPDATE => Some("update"),
        gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_END => Some("end"),
        gdk::ffi::GDK_TOUCHPAD_GESTURE_PHASE_CANCEL => Some("cancel"),
        _ => None,
    }
}

/// The pinch carried by a GDK touchpad pinch event.
fn pinch_of(event: &gdk::EventTouchpadPinch) -> Option<Pinch> {
    let raw: &gdk::ffi::GdkEventTouchpadPinch = event.as_ref();
    let phase = phase_name(raw.phase as i32)?;
    let (x, y) = event.position();
    let scale = event.scale();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    Some(Pinch { phase, scale, x, y })
}

/// Take touchpad pinches from `window`'s WebKitGTK view and emit them to its
/// frontend instead.
pub fn install<R: Runtime>(window: &WebviewWindow<R>) {
    let target = window.clone();
    let result = window.with_webview(move |platform| {
        // `event` runs before WebKit's own handler for the same signal, and
        // stopping it here keeps WebKit from magnifying the page.
        platform.inner().connect_event(move |_, event| {
            if event.event_type() != gdk::EventType::TouchpadPinch {
                return gtk::glib::Propagation::Proceed;
            }
            if let Some(pinch) = event
                .downcast_ref::<gdk::EventTouchpadPinch>()
                .and_then(pinch_of)
            {
                if let Err(e) = target.emit(EVENT, pinch) {
                    warn!("[webview] cannot forward a touchpad pinch: {e}");
                }
            }
            gtk::glib::Propagation::Stop
        });
    });
    if let Err(e) = result {
        warn!(
            "[webview] cannot take touchpad pinches of '{}': {e}",
            window.label()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_gesture_phase() {
        assert_eq!(phase_name(0), Some("begin"));
        assert_eq!(phase_name(1), Some("update"));
        assert_eq!(phase_name(2), Some("end"));
        assert_eq!(phase_name(3), Some("cancel"));
        assert_eq!(phase_name(4), None);
    }
}
