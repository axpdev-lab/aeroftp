// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Decide whether WebKitGTK gets an accelerated compositor on this display.
//!
//! WebKitGTK's accelerated compositor needs a usable EGL display, and when it
//! does not get one it does not degrade: the window stays alive, reports itself
//! visible, answers commands, and paints nothing. That is the blank window of
//! issue #462, and it is indistinguishable from a working app until somebody
//! looks at the screen.
//!
//! Forcing `WEBKIT_DISABLE_COMPOSITING_MODE=1` for everyone removes the failure
//! and the GPU with it — canvas/WebGL in AeroPlayer's visualizers and every CSS
//! animation move onto the CPU, on machines whose driver was fine. So ask EGL
//! first and fall back only when the answer is no.
//!
//! The question has to be asked before any GTK/WebKit initialization: by the
//! time WebKit is up it has already read the variable.

use std::ffi::{c_void, CString};
use std::path::PathBuf;
use std::sync::OnceLock;

use log::{info, warn};

/// The startup decision, held until a logger exists to receive it.
static DECISION: OnceLock<(bool, String)> = OnceLock::new();

/// The variable WebKitGTK reads to turn its accelerated compositor off.
const DISABLE_COMPOSITING: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";

/// Written immediately before the probe and removed immediately after, so a
/// driver that takes the process down inside `eglInitialize` leaves evidence
/// that outlives the crash.
const SENTINEL_LEAF: &str = "gpu-probe-incomplete";

const SENTINEL_NOTE: &str = "\
AeroFTP writes this file while it asks EGL whether the GPU can be used, and
removes it as soon as EGL answers. Finding it at startup therefore means the
previous answer never came: the graphics driver took the process down. AeroFTP
then keeps WebKit on software rendering, which always paints.

Delete this file to make AeroFTP try the GPU again.
";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// The environment already carries an explicit choice; leave it alone.
    RespectOverride,
    /// EGL answered: WebKit may use the GPU.
    Accelerated,
    /// No usable EGL, or an answer that never came.
    Software(&'static str),
}

/// The decision itself, with the probe injected so it can be tested on a
/// machine that has no GPU, no display, and no EGL.
pub(crate) fn decide(
    override_present: bool,
    previous_probe_crashed: bool,
    probe: impl FnOnce() -> Result<(), &'static str>,
) -> Decision {
    if override_present {
        return Decision::RespectOverride;
    }
    if previous_probe_crashed {
        return Decision::Software("a previous GPU probe never came back");
    }
    match probe() {
        Ok(()) => Decision::Accelerated,
        Err(reason) => Decision::Software(reason),
    }
}

/// Resolve the WebKit compositing mode for this process. Call once, before GTK
/// or WebKit touch anything.
pub fn configure_webkit_compositing() {
    let sentinel = sentinel_path();
    let previous_probe_crashed = sentinel.as_ref().is_some_and(|p| p.exists());

    let decision = decide(
        std::env::var_os(DISABLE_COMPOSITING).is_some(),
        previous_probe_crashed,
        || {
            // Arm before, disarm after: the gap between the two is exactly the
            // window in which a driver crash is attributable to us.
            if let Some(path) = sentinel.as_ref() {
                let _ = std::fs::write(path, SENTINEL_NOTE);
            }
            let outcome = probe_egl();
            // Removed on success and on a graceful failure alike. Only a crash
            // can leave it behind, which is the whole point of it.
            if let Some(path) = sentinel.as_ref() {
                let _ = std::fs::remove_file(path);
            }
            outcome
        },
    );

    let record = match decision {
        Decision::RespectOverride => (
            false,
            format!(
                "WebKit compositing left to the environment: {DISABLE_COMPOSITING} is already set"
            ),
        ),
        Decision::Accelerated => (
            false,
            "WebKit accelerated compositing enabled: EGL is usable on this display".to_string(),
        ),
        Decision::Software(reason) => {
            // Same startup contract as the sibling WebKit variables set around
            // the call site: single-threaded, before anything reads them.
            std::env::set_var(DISABLE_COMPOSITING, "1");
            if previous_probe_crashed {
                (
                    true,
                    format!(
                        "WebKit software compositing: {reason}. Delete {} to try the GPU again.",
                        sentinel
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| SENTINEL_LEAF.to_string())
                    ),
                )
            } else {
                (false, format!("WebKit software compositing: {reason}"))
            }
        }
    };
    let _ = DECISION.set(record);
}

/// Emit the startup decision once a logger exists.
///
/// `configure_webkit_compositing` has to run before `tauri_plugin_log` is
/// installed, because WebKit reads the variable on its way up. Logging from
/// there therefore writes into a facade with no logger behind it, and the one
/// line that says which path WebKit took would be missing from precisely the
/// logs a blank-window report arrives with. So the decision is stored and
/// replayed from `setup`, where the logger is live.
pub fn log_decision() {
    if let Some((warning, message)) = DECISION.get() {
        if *warning {
            warn!("{message}");
        } else {
            info!("{message}");
        }
    }
}

fn sentinel_path() -> Option<PathBuf> {
    crate::portable::aeroftp_data_root().map(|root| root.join(SENTINEL_LEAF))
}

// --- EGL, the little of it that answers the question -----------------------

type EglBoolean = std::os::raw::c_uint;
type EglEnum = std::os::raw::c_uint;
type EglInt = std::os::raw::c_int;
type EglDisplay = *mut c_void;
type EglConfig = *mut c_void;
type EglContext = *mut c_void;

const EGL_TRUE: EglBoolean = 1;
const EGL_NONE: EglInt = 0x3038;
const EGL_SURFACE_TYPE: EglInt = 0x3033;
const EGL_PBUFFER_BIT: EglInt = 0x0001;
const EGL_RENDERABLE_TYPE: EglInt = 0x3040;
const EGL_OPENGL_ES2_BIT: EglInt = 0x0004;
const EGL_CONTEXT_CLIENT_VERSION: EglInt = 0x3098;
const EGL_OPENGL_ES_API: EglEnum = 0x30A0;

type FnGetDisplay = unsafe extern "C" fn(*mut c_void) -> EglDisplay;
type FnInitialize = unsafe extern "C" fn(EglDisplay, *mut EglInt, *mut EglInt) -> EglBoolean;
type FnBindApi = unsafe extern "C" fn(EglEnum) -> EglBoolean;
type FnChooseConfig = unsafe extern "C" fn(
    EglDisplay,
    *const EglInt,
    *mut EglConfig,
    EglInt,
    *mut EglInt,
) -> EglBoolean;
type FnCreateContext =
    unsafe extern "C" fn(EglDisplay, EglConfig, EglContext, *const EglInt) -> EglContext;
type FnDestroyContext = unsafe extern "C" fn(EglDisplay, EglContext) -> EglBoolean;
type FnTerminate = unsafe extern "C" fn(EglDisplay) -> EglBoolean;

/// Ask EGL for what WebKit's compositor will ask it for: a display it can
/// initialize, a renderable config, and a GL ES 2 context. Anything short of
/// all three is a blank window waiting to happen.
fn probe_egl() -> Result<(), &'static str> {
    // SAFETY: every pointer is checked before use, every symbol is resolved by
    // name from a handle we opened, and the display is terminated on all paths.
    unsafe {
        let handle = dlopen_first(&["libEGL.so.1", "libEGL.so"])
            .ok_or("libEGL is not available on this system")?;
        // Deliberately not dlclose()d. Unloading a GL stack after its driver has
        // initialized is a known way to crash at exit, and WebKit is about to
        // map the same library anyway, so nothing is saved by closing it.

        let get_display: FnGetDisplay = std::mem::transmute::<*mut c_void, FnGetDisplay>(
            dlsym(handle, "eglGetDisplay").ok_or("libEGL is missing eglGetDisplay")?,
        );
        let initialize: FnInitialize = std::mem::transmute::<*mut c_void, FnInitialize>(
            dlsym(handle, "eglInitialize").ok_or("libEGL is missing eglInitialize")?,
        );
        let bind_api: FnBindApi = std::mem::transmute::<*mut c_void, FnBindApi>(
            dlsym(handle, "eglBindAPI").ok_or("libEGL is missing eglBindAPI")?,
        );
        let choose_config: FnChooseConfig = std::mem::transmute::<*mut c_void, FnChooseConfig>(
            dlsym(handle, "eglChooseConfig").ok_or("libEGL is missing eglChooseConfig")?,
        );
        let create_context: FnCreateContext = std::mem::transmute::<*mut c_void, FnCreateContext>(
            dlsym(handle, "eglCreateContext").ok_or("libEGL is missing eglCreateContext")?,
        );
        let destroy_context: FnDestroyContext = std::mem::transmute::<*mut c_void, FnDestroyContext>(
            dlsym(handle, "eglDestroyContext").ok_or("libEGL is missing eglDestroyContext")?,
        );
        let terminate: FnTerminate = std::mem::transmute::<*mut c_void, FnTerminate>(
            dlsym(handle, "eglTerminate").ok_or("libEGL is missing eglTerminate")?,
        );

        // NULL is EGL_DEFAULT_DISPLAY: let GLVND pick the platform the session
        // is actually running, exactly as WebKit does.
        let display = get_display(std::ptr::null_mut());
        if display.is_null() {
            return Err("EGL offers no display for this session");
        }

        let (mut major, mut minor) = (0, 0);
        if initialize(display, &mut major, &mut minor) != EGL_TRUE {
            return Err("EGL could not initialize a display (no GPU userspace?)");
        }

        let result = (|| {
            if bind_api(EGL_OPENGL_ES_API) != EGL_TRUE {
                return Err("EGL has no OpenGL ES support here");
            }
            let attributes = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES2_BIT,
                EGL_NONE,
            ];
            let mut config: EglConfig = std::ptr::null_mut();
            let mut found: EglInt = 0;
            if choose_config(display, attributes.as_ptr(), &mut config, 1, &mut found) != EGL_TRUE
                || found == 0
                || config.is_null()
            {
                return Err("EGL has no renderable configuration on this display");
            }

            let context_attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            let context = create_context(
                display,
                config,
                std::ptr::null_mut(),
                context_attributes.as_ptr(),
            );
            if context.is_null() {
                return Err("EGL could not create a GL context on this display");
            }
            destroy_context(display, context);
            Ok(())
        })();

        terminate(display);
        result
    }
}

/// dlopen the first name that resolves, so a distro that ships only the
/// unversioned symlink is not treated as having no EGL at all.
unsafe fn dlopen_first(names: &[&str]) -> Option<*mut c_void> {
    for name in names {
        let Ok(c_name) = CString::new(*name) else {
            continue;
        };
        let handle = libc::dlopen(c_name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if !handle.is_null() {
            return Some(handle);
        }
    }
    None
}

unsafe fn dlsym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
    let c_name = CString::new(name).ok()?;
    let symbol = libc::dlsym(handle, c_name.as_ptr());
    if symbol.is_null() {
        None
    } else {
        Some(symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_setting_is_never_second_guessed() {
        let decision = decide(true, false, || panic!("the probe must not run"));
        assert_eq!(decision, Decision::RespectOverride);
    }

    #[test]
    fn a_usable_egl_keeps_the_gpu_path() {
        assert_eq!(decide(false, false, || Ok(())), Decision::Accelerated);
    }

    #[test]
    fn an_unusable_egl_falls_back_and_says_why() {
        assert_eq!(
            decide(false, false, || Err(
                "EGL offers no display for this session"
            )),
            Decision::Software("EGL offers no display for this session")
        );
    }

    #[test]
    fn a_probe_that_crashed_the_app_is_not_run_again() {
        let decision = decide(false, true, || panic!("the probe must not run again"));
        assert_eq!(
            decision,
            Decision::Software("a previous GPU probe never came back")
        );
    }

    /// The decision tests above inject the probe, so nothing there would notice
    /// a wrong FFI signature — and a wrong one segfaults rather than fails. This
    /// runs the real thing against the real libEGL. Ignored by default: CI
    /// runners have no GPU userspace, and the answer is legitimately different
    /// on every machine. Run it on a desktop with:
    ///   cargo test --lib -- --ignored --nocapture the_real_probe
    #[test]
    #[ignore = "needs a real libEGL and display; verdict is machine-specific"]
    fn the_real_probe_answers_instead_of_crashing() {
        println!("EGL probe verdict: {:?}", probe_egl());
    }

    #[test]
    fn an_explicit_setting_wins_over_a_crashed_probe() {
        let decision = decide(true, true, || panic!("the probe must not run"));
        assert_eq!(decision, Decision::RespectOverride);
    }
}
