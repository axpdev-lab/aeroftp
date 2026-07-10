//! IPC panic safety net.
//!
//! A Tauri async command that dies by panic never sends a response, so the
//! frontend `invoke` promise stays pending *forever*: an endless spinner and a
//! Cancel button that does nothing. `tauri`'s `respond_async` spawns the command
//! future with no `catch_unwind` and the resolver has no `Drop` that answers the
//! IPC call, so nothing rescues the caller.
//!
//! [`catch`] wraps a command's future so a panic is caught during unwind and
//! turned into a normal `Err(String)` that the UI can render, instead of a hang.
//! Applied to the high-risk connect family (`provider_connect`, `connect_ftp`,
//! ...); the frontend `cancellableConnect` hard timeout is the complementary
//! defence in depth for any command not yet wrapped.
//!
//! Requires the default `unwind` panic strategy (any profile setting
//! `panic = "abort"` turns this into a no-op); `src-tauri/Cargo.toml` sets no
//! `panic =` key, so both dev and release unwind.

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
use tracing::error;

/// Extract a human-readable message from a panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Run a command future so a panic becomes an `Err` instead of stranding the
/// frontend `invoke` promise. `label` names the command in the log line and the
/// returned error. `AssertUnwindSafe` is sound here: the future owns its state
/// and a caught panic ends the command, so no half-mutated state is observed
/// across the boundary.
pub async fn catch<T, F>(label: &str, fut: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match AssertUnwindSafe(fut).catch_unwind().await {
        Ok(res) => res,
        Err(payload) => {
            let msg = panic_message(payload.as_ref());
            error!("command `{label}` panicked, returning Err instead of hanging: {msg}");
            Err(format!("Internal error: `{label}` panicked: {msg}"))
        }
    }
}

/// Deliberately panicking command used to prove the safety net end to end: with
/// [`catch`] in place the frontend `invoke('debug_panic_command')` must *reject*,
/// not hang. Debug builds only.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn debug_panic_command() -> Result<(), String> {
    catch("debug_panic_command", async {
        panic!("deliberate panic to exercise the IPC panic safety net");
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ok_passes_through() {
        let out: Result<i32, String> = catch("ok", async { Ok(7) }).await;
        assert_eq!(out, Ok(7));
    }

    #[tokio::test]
    async fn err_passes_through() {
        let out: Result<i32, String> = catch("err", async { Err("boom".to_string()) }).await;
        assert_eq!(out, Err("boom".to_string()));
    }

    #[tokio::test]
    async fn str_panic_becomes_err() {
        let out: Result<i32, String> = catch("panicky", async {
            panic!("kaboom");
        })
        .await;
        let msg = out.unwrap_err();
        assert!(msg.contains("panicky"), "message was: {msg}");
        assert!(msg.contains("kaboom"), "message was: {msg}");
    }

    #[tokio::test]
    async fn string_panic_becomes_err() {
        let out: Result<i32, String> = catch("panicky", async {
            panic!("{}", String::from("dynamic"));
        })
        .await;
        assert!(out.unwrap_err().contains("dynamic"));
    }
}
