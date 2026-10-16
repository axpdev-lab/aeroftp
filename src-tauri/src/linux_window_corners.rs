// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Whether the Linux main window draws its own rounded corners.
//!
//! The window is borderless, and the compositor rounds only the windows it
//! decorates, so the page cuts its own corners (`html.rounded-window` in
//! styles.css). That needs a transparent window, and a transparent window is
//! safe only when two things hold:
//!
//! - the screen has a compositor: without one, the transparent pixels show
//!   black instead of the desktop;
//! - WebKit composites on the GPU. With software compositing (see
//!   [`crate::linux_egl`], which turns the GPU path off where EGL cannot feed
//!   it) WebKitGTK paints a transparent window only partly: measured on the
//!   4.2.3 Flatpak, a first-run dialog showed as a lone rectangle, and opening
//!   Settings left the whole window see-through, still after it closed. The
//!   same build with an opaque window painted every frame.
//!
//! `AEROFTP_SQUARE_CORNERS` stays the user's way out for a driver that cannot
//! draw a transparent window even on the GPU path.

/// What the main window gets, and why when it is not rounded.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Corners {
    Rounded,
    Square(&'static str),
}

/// The decision, from the three facts it depends on, so it can be tested
/// without a display.
pub(crate) fn decide(square_requested: bool, composited: bool, gpu_compositing: bool) -> Corners {
    if square_requested {
        Corners::Square("AEROFTP_SQUARE_CORNERS is set")
    } else if !composited {
        Corners::Square("the screen has no compositor, so transparent pixels would show black")
    } else if !gpu_compositing {
        Corners::Square(
            "WebKit composites in software here, which leaves a transparent window blank",
        )
    } else {
        Corners::Rounded
    }
}

/// Whether WebKit keeps its GPU compositor, read the way WebKitGTK reads it:
/// `WEBKIT_DISABLE_COMPOSITING_MODE` set to anything but `0` turns it off.
/// [`crate::linux_egl::configure_webkit_compositing`] sets it before GTK
/// starts, so this is meaningful once that has run.
pub(crate) fn gpu_compositing(disable_compositing_mode: Option<&std::ffi::OsStr>) -> bool {
    disable_compositing_mode.is_none_or(|value| value == "0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn a_window_is_rounded_only_on_a_composited_screen_with_gpu_compositing() {
        assert_eq!(decide(false, true, true), Corners::Rounded);
        for (square, composited, gpu) in [
            (true, true, true),
            (false, false, true),
            (false, true, false),
            (false, false, false),
        ] {
            assert!(
                matches!(decide(square, composited, gpu), Corners::Square(_)),
                "square={square} composited={composited} gpu={gpu}"
            );
        }
    }

    #[test]
    fn software_compositing_is_named_as_the_reason() {
        let Corners::Square(reason) = decide(false, true, false) else {
            panic!("software compositing must keep the window opaque");
        };
        assert!(reason.contains("software"), "{reason}");
    }

    #[test]
    fn the_compositing_variable_is_read_the_way_webkit_reads_it() {
        assert!(gpu_compositing(None));
        assert!(gpu_compositing(Some(OsStr::new("0"))));
        for value in ["1", "true", "", "yes"] {
            assert!(!gpu_compositing(Some(OsStr::new(value))), "{value:?}");
        }
    }

    /// The window builder asks this module, after the EGL decision, before it
    /// requests a transparent window; and nothing else in the app requests one.
    #[test]
    fn the_main_window_is_transparent_only_through_this_decision() {
        let lib = include_str!("lib.rs");
        assert_eq!(
            lib.matches(".transparent(true)").count(),
            1,
            "one transparent window, the main one"
        );
        let builder = lib
            .find(".transparent(true)")
            .expect("the main window builder");
        let gate = lib[..builder]
            .rfind("linux_window_corners::decide(")
            .expect("the transparent window is gated by linux_window_corners::decide");
        assert!(
            lib[gate..builder].contains("linux_window_corners::gpu_compositing("),
            "the gate reads whether WebKit composites on the GPU"
        );
        let egl = lib
            .find("crate::linux_egl::configure_webkit_compositing();")
            .expect("the EGL decision");
        assert!(
            egl < gate,
            "the EGL decision is made before the corners are"
        );
    }
}
