//! The AeroAgent approval window.
//!
//! When an AeroAgent tool needs the user's approval outside the chat panel, the
//! request is shown in a dedicated window (`ai-approval.html`) instead of the
//! chat webview. Only that window can answer: `ai_approval_decide` accepts a
//! decision solely from a window whose label this module created and still has
//! pending. Labels are chosen by the backend when the window is built, the
//! webviews have no permission to create windows, and a label cannot be forged
//! from JavaScript, so the chat webview (the page that renders what the agent
//! reads) can ask for an approval but cannot grant one.
//!
//! A window closed without a decision counts as a refusal.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use tokio::sync::oneshot;

/// Prefix of every approval window label.
pub(crate) const LABEL_PREFIX: &str = "ai-approval-";

/// What the approval window shows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPrompt {
    /// Human label of the action, e.g. "Write Local File".
    pub action: String,
    /// The details lines (paths, command, credentials notice).
    pub message: String,
    /// True when approving remembers the tool for the chat session.
    pub remember_for_session: bool,
}

struct Pending {
    prompt: ApprovalPrompt,
    responder: oneshot::Sender<bool>,
}

static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending() -> std::sync::MutexGuard<'static, HashMap<String, Pending>> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Registers a prompt under a fresh label and returns the label with the
/// receiver that resolves to the user's decision.
fn register(prompt: ApprovalPrompt) -> (String, oneshot::Receiver<bool>) {
    let label = format!("{LABEL_PREFIX}{}", uuid::Uuid::new_v4().simple());
    let (responder, receiver) = oneshot::channel();
    pending().insert(label.clone(), Pending { prompt, responder });
    (label, receiver)
}

/// The prompt for the window with this label, refused for any other window.
fn prompt_for(label: &str) -> Result<ApprovalPrompt, String> {
    if !label.starts_with(LABEL_PREFIX) {
        return Err("Only the approval window can read an approval request.".to_string());
    }
    pending()
        .get(label)
        .map(|p| p.prompt.clone())
        .ok_or_else(|| "This approval request is no longer pending.".to_string())
}

/// Delivers a decision from the window with this label, refused for any other
/// window. Consumes the request, so a decision is taken at most once.
fn decide(label: &str, approved: bool) -> Result<(), String> {
    if !label.starts_with(LABEL_PREFIX) {
        return Err("Only the approval window can answer an approval request.".to_string());
    }
    let entry = pending()
        .remove(label)
        .ok_or_else(|| "This approval request is no longer pending.".to_string())?;
    // The waiting side may have given up already; nothing else to do then.
    let _ = entry.responder.send(approved);
    Ok(())
}

/// Drops the request of a window that went away: its receiver resolves as a
/// refusal.
fn forget(label: &str) {
    pending().remove(label);
}

/// Shows `prompt` in a new approval window and waits for the user's answer.
/// `Ok(false)` when the user refuses or closes the window.
pub(crate) async fn ask(app: &tauri::AppHandle, prompt: ApprovalPrompt) -> Result<bool, String> {
    let (label, receiver) = register(prompt);

    // Building a window touches GTK on Linux: marshal it onto the main thread
    // (LT1, the same discipline as the extract window).
    let (built_tx, built_rx) = oneshot::channel();
    let app_main = app.clone();
    let label_main = label.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        let _ = built_tx.send(build_window(&app_main, &label_main));
    }) {
        forget(&label);
        return Err(format!("Failed to open the approval window: {e}"));
    }
    match built_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            forget(&label);
            return Err(format!("Failed to open the approval window: {e}"));
        }
        Err(_) => {
            forget(&label);
            return Err("Failed to open the approval window.".to_string());
        }
    }

    // A dropped sender means the window closed without answering.
    Ok(receiver.await.unwrap_or(false))
}

fn build_window(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    use tauri::Manager;

    let mut builder =
        tauri::WebviewWindowBuilder::new(app, label, crate::app_page_url("ai-approval.html"))
            .title("AeroFTP")
            .inner_size(520.0, 380.0)
            .min_inner_size(420.0, 300.0)
            .resizable(true)
            .center()
            .always_on_top(true)
            .focused(true);
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.decorations(false);
    }
    if let Some(main) = app.get_webview_window("main") {
        builder = builder
            .parent(&main)
            .map_err(|e| format!("cannot attach to the main window: {e}"))?;
    }
    if let Some(dir) = crate::portable::webview_data_dir() {
        builder = builder.data_directory(dir);
    }
    let window = builder.build().map_err(|e| e.to_string())?;
    // GTK hands every window the global app menu: not this one.
    let _ = window.remove_menu();

    let label_owned = label.to_string();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            forget(&label_owned);
        }
    });
    Ok(())
}

/// Read by the approval window to show its request. Async so it does not run
/// on the main thread (see `sync_command_audit`); it only reads a map.
#[tauri::command]
pub async fn ai_approval_prompt(window: tauri::WebviewWindow) -> Result<ApprovalPrompt, String> {
    prompt_for(window.label())
}

/// The approval window's answer. Any other window is refused.
#[tauri::command]
pub async fn ai_approval_decide(
    window: tauri::WebviewWindow,
    approved: bool,
) -> Result<(), String> {
    decide(window.label(), approved)?;
    let _ = window.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> ApprovalPrompt {
        ApprovalPrompt {
            action: "Write Local File".to_string(),
            message: "path: /tmp/x".to_string(),
            remember_for_session: false,
        }
    }

    #[test]
    fn approval_windows_count_as_secondary_and_lose_the_app_menu() {
        assert!(crate::is_secondary_window_label(&format!(
            "{LABEL_PREFIX}abc"
        )));
        assert!(crate::is_secondary_window_label("extract-2"));
        assert!(crate::is_secondary_window_label("splashscreen"));
        assert!(!crate::is_secondary_window_label("main"));
    }

    #[test]
    fn the_main_window_cannot_answer_a_pending_request() {
        let (label, mut receiver) = register(prompt());
        assert!(decide("main", true).is_err());
        assert!(prompt_for("main").is_err());
        // Still pending, still unanswered.
        assert!(receiver.try_recv().is_err());
        assert!(prompt_for(&label).is_ok());
        forget(&label);
    }

    #[test]
    fn an_unknown_approval_label_cannot_answer_either() {
        let (label, mut receiver) = register(prompt());
        assert!(decide(&format!("{LABEL_PREFIX}forged"), true).is_err());
        assert!(receiver.try_recv().is_err());
        forget(&label);
    }

    #[test]
    fn the_approval_window_answers_once() {
        let (label, mut receiver) = register(prompt());
        assert_eq!(prompt_for(&label).unwrap(), prompt());
        decide(&label, true).unwrap();
        assert_eq!(receiver.try_recv(), Ok(true));
        assert!(
            decide(&label, true).is_err(),
            "a second decision must be refused"
        );
    }

    #[test]
    fn closing_the_window_without_answering_is_a_refusal() {
        let (label, receiver) = register(prompt());
        forget(&label);
        let answer = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async { receiver.await.unwrap_or(false) });
        assert!(!answer);
    }
}
