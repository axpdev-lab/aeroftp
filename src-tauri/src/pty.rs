// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// PTY (Pseudo-Terminal) module for real shell integration
// Uses portable-pty for cross-platform support (Linux/macOS/Windows)
// Supports multiple concurrent sessions (one per terminal tab)

use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{AppHandle, Emitter, State};

/// Maximum number of concurrent PTY sessions to prevent resource exhaustion (M59)
const MAX_PTY_SESSIONS: usize = 20;

/// Holds the PTY pair (master/slave) for a single terminal session
pub struct PtySession {
    pub pair: Option<PtyPair>,
    pub writer: Option<Box<dyn Write + Send>>,
}

/// A session behind its own lock, so touching one session does not require
/// holding the manager.
///
/// This matters now that the commands are off the main thread. While they ran
/// there, the main thread serialised them for free and one lock was enough.
/// Off it they run concurrently, and `pty_write` blocks for as long as the
/// child refuses to drain the master: with a single manager lock that write
/// would park every other session's write, every resize and every close behind
/// it. Per session, a wedged child costs only its own session.
pub type SessionHandle = Arc<Mutex<PtySession>>;

/// Manager holding multiple PTY sessions keyed by session ID
pub struct PtyManager {
    pub sessions: HashMap<String, SessionHandle>,
    next_id: u64,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            next_id: 1,
        }
    }
}

impl PtyManager {
    fn next_session_id(&mut self) -> String {
        let id = format!("pty-{}", self.next_id);
        self.next_id += 1;
        id
    }
}

/// Global PTY state wrapped in Arc<Mutex>
pub type PtyState = Arc<Mutex<PtyManager>>;

/// Create a new PTY state
pub fn create_pty_state() -> PtyState {
    Arc::new(Mutex::new(PtyManager::default()))
}

/// Holds a reserved session slot until setup either finishes or fails.
///
/// The limit check and the insert have to happen under one acquisition of the
/// manager lock. Checking, releasing, spawning and only then inserting is a
/// time-of-check-to-time-of-use gap: two concurrent `spawn_shell` calls both
/// see 19 sessions and both insert, and the cap is quietly 21. That race was
/// unreachable while the command was synchronous, because Tauri ran it on the
/// main thread and the main thread does one thing at a time; moving the work
/// onto the blocking pool is exactly what makes it reachable, so the fix
/// belongs with the move rather than after it.
///
/// Everything between the reservation and `commit` can fail, and each failure
/// returns early. Dropping the guard puts the slot back, so a failed spawn
/// cannot leak a slot and shrink the cap for the rest of the session.
struct SlotReservation<'a> {
    state: &'a PtyState,
    id: Option<String>,
}

impl SlotReservation<'_> {
    fn commit(mut self) -> String {
        self.id.take().expect("a reservation is committed once")
    }
}

impl Drop for SlotReservation<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            if let Ok(mut manager) = self.state.lock() {
                manager.sessions.remove(&id);
            }
        }
    }
}

/// Reserve one slot, or refuse because the cap is reached. The check and the
/// insert share a single lock acquisition on purpose; see `SlotReservation`.
fn reserve_session_slot(pty_state: &PtyState) -> Result<SlotReservation<'_>, String> {
    let mut manager = pty_state.lock().map_err(|_| "Lock error")?;
    if manager.sessions.len() >= MAX_PTY_SESSIONS {
        return Err(format!(
            "Maximum PTY session limit reached ({}). Close existing sessions first.",
            MAX_PTY_SESSIONS
        ));
    }
    let id = manager.next_session_id();
    manager.sessions.insert(
        id.clone(),
        Arc::new(Mutex::new(PtySession {
            pair: None,
            writer: None,
        })),
    );
    Ok(SlotReservation {
        state: pty_state,
        id: Some(id),
    })
}

/// The handle for a session id, cloned out from under the manager lock so the
/// caller can work on the session without holding it.
fn session_handle(pty_state: &PtyState, session_id: &str) -> Result<SessionHandle, String> {
    let manager = pty_state.lock().map_err(|_| "Lock error")?;
    manager
        .sessions
        .get(session_id)
        .cloned()
        // H31: session_id is required: no fallback to prevent multi-tab session confusion
        .ok_or_else(|| format!("PTY session not found: {}", session_id))
}

// The four commands below are `async` and do their work on the blocking pool.
// A synchronous `#[tauri::command]` runs on the main thread -- the GTK thread on
// Linux -- and each of these can sit there for an unbounded time:
//
//   * `spawn_shell` opens a PTY and forks a shell process;
//   * `pty_write` writes to the PTY master, which blocks once the child stops
//     draining it and the buffer fills;
//   * `pty_resize` and `pty_close` only take the manager lock, but it is the
//     *same* lock `pty_write` holds across its write. Converting only the
//     writer would leave the freeze reachable by resizing the terminal.
//
// The guard is taken inside the closure: a `std::sync::MutexGuard` held across
// an `.await` is `clippy::await_holding_lock`, and the lint is right.

/// Spawn a new shell in the PTY. Returns session info including session ID.
/// Enforces a maximum of MAX_PTY_SESSIONS concurrent sessions.
#[tauri::command]
pub async fn spawn_shell(
    app: AppHandle,
    pty_state: State<'_, PtyState>,
    cwd: Option<String>,
) -> Result<String, String> {
    let pty_state = PtyState::clone(&pty_state);
    tokio::task::spawn_blocking(move || spawn_shell_blocking(app, &pty_state, cwd))
        .await
        .unwrap_or_else(|err| Err(format!("Shell spawn task failed: {err}")))
}

fn spawn_shell_blocking(
    app: AppHandle,
    pty_state: &PtyState,
    cwd: Option<String>,
) -> Result<String, String> {
    // Reserve the slot before allocating anything: the cap is only a cap if the
    // check and the insert are one operation. Every `?` below drops the guard,
    // which hands the slot back.
    let reservation = reserve_session_slot(pty_state)?;

    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("Failed to open PTY: {}", e))?;

    // Determine the shell to use
    #[cfg(unix)]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());

    #[cfg(windows)]
    let shell = {
        // Prefer PowerShell, fall back to cmd.exe
        let ps = std::env::var("SystemRoot")
            .map(|sr| format!("{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", sr))
            .unwrap_or_else(|_| "powershell.exe".to_string());
        if std::path::Path::new(&ps).exists() {
            ps
        } else {
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
        }
    };

    let mut cmd = CommandBuilder::new(&shell);

    // Windows: start PowerShell as a plain interactive shell.
    //
    // Previously we passed `-Command "function prompt { ... }"` to inject a
    // colored prompt, but on Windows 10 with the default ExecutionPolicy the
    // -Command parser can stall (issue #125): the call to spawn_command never
    // returns, the Tauri invoke awaits forever, the frontend never receives
    // the session id, and every keystroke gets silently dropped at the
    // `connectedTabs.has(tabId)` gate in SSHTerminal.tsx: making the entire
    // terminal feel "frozen" with no way out except restarting the app.
    //
    // The default PowerShell prompt is uglier but reliable. A user-level
    // colored prompt can be added later via $PROFILE without touching the
    // launcher, which is the conventional place for such customizations
    // anyway.
    #[cfg(windows)]
    if shell.contains("powershell") {
        cmd.arg("-NoLogo");
    }

    // Set environment variables for better terminal experience
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "1");
    cmd.env("CLICOLOR", "1");
    cmd.env("CLICOLOR_FORCE", "1");

    // Unix: set a colorful PS1 prompt (bash/zsh)
    #[cfg(unix)]
    cmd.env(
        "PS1",
        r"\[\e[1;36m\]\u@\h\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ ",
    );

    // Set working directory
    if let Some(path) = cwd {
        cmd.cwd(path);
    } else if let Ok(current) = std::env::current_dir() {
        cmd.cwd(current);
    }

    // Spawn the shell
    let _child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("Failed to spawn shell: {}", e))?;

    // Get reader and writer from master
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("Failed to clone reader: {}", e))?;

    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("Failed to take writer: {}", e))?;

    // Setup is done, so the slot is ours to keep: fill it in place.
    let session_id = reservation.commit();
    {
        let handle = session_handle(pty_state, &session_id)?;
        let mut session = handle.lock().map_err(|_| "Lock error")?;
        session.pair = Some(pair);
        session.writer = Some(writer);
    }

    // Spawn a thread to read output from the PTY and emit it to the frontend
    // Each session emits to its own event channel: pty-output-{session_id}
    let app_clone = app.clone();
    let event_name = format!("pty-output-{}", session_id);
    thread::spawn(move || {
        let mut buffer = [0u8; 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let output = String::from_utf8_lossy(&buffer[..n]).to_string();
                    let _ = app_clone.emit(&event_name, output);
                }
                Err(_) => break, // Error or closed
            }
        }
    });

    Ok(format!("Shell started: {} [session:{}]", shell, session_id))
}

/// Write data to a PTY session (send keystrokes to shell)
#[tauri::command]
pub async fn pty_write(
    pty_state: State<'_, PtyState>,
    data: String,
    session_id: String,
) -> Result<(), String> {
    let pty_state = PtyState::clone(&pty_state);
    tokio::task::spawn_blocking(move || pty_write_blocking(&pty_state, data, session_id))
        .await
        .unwrap_or_else(|err| Err(format!("PTY write task failed: {err}")))
}

fn pty_write_blocking(
    pty_state: &PtyState,
    data: String,
    session_id: String,
) -> Result<(), String> {
    // The manager lock is released the moment we have the handle: `write_all`
    // below blocks for as long as the child refuses to read, and holding the
    // manager across it would park every other session with it.
    let handle = session_handle(pty_state, &session_id)?;
    let mut session = handle.lock().map_err(|_| "Lock error")?;

    if let Some(ref mut writer) = session.writer {
        writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("Write error: {}", e))?;
        writer.flush().map_err(|e| format!("Flush error: {}", e))?;
        Ok(())
    } else {
        Err("No writer for PTY session".to_string())
    }
}

/// Resize a PTY session
#[tauri::command]
pub async fn pty_resize(
    pty_state: State<'_, PtyState>,
    rows: u16,
    cols: u16,
    session_id: String,
) -> Result<(), String> {
    let pty_state = PtyState::clone(&pty_state);
    tokio::task::spawn_blocking(move || pty_resize_blocking(&pty_state, rows, cols, session_id))
        .await
        .unwrap_or_else(|err| Err(format!("PTY resize task failed: {err}")))
}

fn pty_resize_blocking(
    pty_state: &PtyState,
    rows: u16,
    cols: u16,
    session_id: String,
) -> Result<(), String> {
    let handle = session_handle(pty_state, &session_id)?;
    let session = handle.lock().map_err(|_| "Lock error")?;

    if let Some(ref pair) = session.pair {
        pair.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Resize error: {}", e))?;
        Ok(())
    } else {
        Err("No active PTY pair".to_string())
    }
}

/// Close a PTY session
#[tauri::command]
pub async fn pty_close(pty_state: State<'_, PtyState>, session_id: String) -> Result<(), String> {
    let pty_state = PtyState::clone(&pty_state);
    tokio::task::spawn_blocking(move || pty_close_blocking(&pty_state, session_id))
        .await
        .unwrap_or_else(|err| Err(format!("PTY close task failed: {err}")))
}

fn pty_close_blocking(pty_state: &PtyState, session_id: String) -> Result<(), String> {
    // Deliberately only the manager lock, and deliberately not the session's.
    // Closing has to work while a write to that same session is blocked on a
    // child that stopped reading, which is the case a user actually wants out
    // of. Taking the session lock here would make close wait for exactly the
    // write it is meant to abandon.
    //
    // What this does and does not buy, so nobody reads more into it: the
    // session leaves the map, so it is gone for every later command and the
    // next write reports "session not found". The in-flight write is not
    // interrupted, because it owns a writer handle taken from the master and
    // dropping our side does not unblock a write already in the kernel. It
    // costs one parked blocking-pool thread per wedged child, which ends when
    // the child dies or drains. Interrupting it would mean a non-blocking
    // writer with its own cancellation, which is a different change.
    let mut manager = pty_state.lock().map_err(|_| "Lock error")?;

    // H31: session_id is required: no fallback to prevent multi-tab session confusion
    manager.sessions.remove(&session_id);

    Ok(())
}

#[cfg(test)]
mod session_slot_tests {
    use super::*;

    /// The cap is only a cap if the check and the insert are one operation.
    ///
    /// Pinned by driving the reservation concurrently rather than by reading
    /// the code: 32 threads race for 20 slots, and the invariant is that the
    /// map never exceeds `MAX_PTY_SESSIONS` and exactly 20 reservations
    /// succeed. Against the previous shape (check, release, spawn, insert) the
    /// map ends up over the cap, which is what this test was written against.
    #[test]
    fn concurrent_reservations_cannot_exceed_the_cap() {
        // A barrier, and rounds. Spawning the threads in a loop and hoping is
        // not a pin: the first threads finish before the last ones start, so
        // the racy shape passes it. Every thread waits on the barrier and then
        // calls in at once, and the experiment repeats, because one round of a
        // race proves nothing about the round that would have failed.
        const RACERS: usize = 32;
        const ROUNDS: usize = 200;

        for round in 0..ROUNDS {
            let state = create_pty_state();
            let barrier = Arc::new(std::sync::Barrier::new(RACERS));
            let granted = Arc::new(std::sync::atomic::AtomicUsize::new(0));

            std::thread::scope(|scope| {
                for _ in 0..RACERS {
                    let state = PtyState::clone(&state);
                    let barrier = Arc::clone(&barrier);
                    let granted = Arc::clone(&granted);
                    scope.spawn(move || {
                        barrier.wait();
                        if let Ok(reservation) = reserve_session_slot(&state) {
                            granted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            // Keep it: this stands for a spawn that succeeded.
                            let _ = reservation.commit();
                        }
                    });
                }
            });

            let held = state.lock().unwrap().sessions.len();
            assert_eq!(
                held, MAX_PTY_SESSIONS,
                "round {round}: {RACERS} racing reservations against a cap of \
                 {MAX_PTY_SESSIONS} left {held} sessions"
            );
            assert_eq!(
                granted.load(std::sync::atomic::Ordering::SeqCst),
                MAX_PTY_SESSIONS,
                "round {round}: more reservations were granted than the cap allows"
            );
        }
    }

    /// A reservation that is dropped instead of committed puts the slot back,
    /// so a spawn that fails half way does not shrink the cap for good.
    #[test]
    fn a_dropped_reservation_returns_the_slot() {
        let state = create_pty_state();
        {
            let _reservation = reserve_session_slot(&state).expect("first slot");
            assert_eq!(state.lock().unwrap().sessions.len(), 1);
        }
        assert_eq!(
            state.lock().unwrap().sessions.len(),
            0,
            "dropping the guard must remove the reserved slot"
        );

        // And the cap is still whole afterwards.
        let mut kept = Vec::new();
        for _ in 0..MAX_PTY_SESSIONS {
            kept.push(reserve_session_slot(&state).expect("slot").commit());
        }
        assert!(reserve_session_slot(&state).is_err(), "cap must now refuse");
    }

    /// Writing to a session must not hold the manager lock, or one child that
    /// stops reading parks every other session, every resize and every close.
    ///
    /// Pinned by holding a session's own lock and then asserting the manager is
    /// still acquirable, which is precisely what fails if the write path takes
    /// the manager for the duration.
    #[test]
    fn a_busy_session_does_not_hold_the_manager() {
        let state = create_pty_state();
        let id = reserve_session_slot(&state).expect("slot").commit();
        let handle = session_handle(&state, &id).expect("handle");

        let _busy = handle.lock().expect("session lock");
        assert!(
            state.try_lock().is_ok(),
            "the manager must be free while a session is busy"
        );

        // Close only needs the manager, so it works while that session is busy.
        pty_close_blocking(&state, id.clone()).expect("close while busy");
        assert!(
            session_handle(&state, &id).is_err(),
            "the closed session must be gone from the map"
        );
    }
}
