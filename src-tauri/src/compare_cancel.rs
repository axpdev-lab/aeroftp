// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! One cancel flag per AeroSync compare scan.
//!
//! The compare commands used to take the transfer cancel flag of their state:
//! they reset it to false when they started and read it while scanning. So
//! nothing in AeroSync could stop a scan without also stopping the transfers,
//! closing the dialog left the scan running, and starting a compare could
//! clear the Stop a user had just pressed on a transfer. Each scan now has a
//! flag of its own, found by the `progress_id` the frontend already passes,
//! and `cancel_compare` raises it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

static FLAGS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The cancel flag of one running compare. Registered under its progress id
/// for as long as this value lives; a compare without an id still gets a
/// flag, which only it can see.
pub(crate) struct CompareCancel {
    id: Option<String>,
    flag: Arc<AtomicBool>,
}

impl CompareCancel {
    pub(crate) fn register(progress_id: Option<&str>) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        if let Some(id) = progress_id {
            FLAGS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), Arc::clone(&flag));
        }
        Self {
            id: progress_id.map(str::to_string),
            flag,
        }
    }

    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    pub(crate) fn shared(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

impl Drop for CompareCancel {
    fn drop(&mut self) {
        let Some(id) = &self.id else { return };
        let mut flags = FLAGS.lock().unwrap_or_else(|e| e.into_inner());
        // Only our own entry: a later compare may have registered the same id.
        if flags.get(id).is_some_and(|f| Arc::ptr_eq(f, &self.flag)) {
            flags.remove(id);
        }
    }
}

/// Raise the cancel flag of the compare running under `progress_id`.
/// `false` when no compare runs under that id (already finished, or never
/// started), which is not an error: the scan the user wanted stopped is over.
pub(crate) fn cancel(progress_id: &str) -> bool {
    let flags = FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    match flags.get(progress_id) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_reaches_only_the_compare_under_that_id() {
        let first = CompareCancel::register(Some("compare-a"));
        let second = CompareCancel::register(Some("compare-b"));
        assert!(cancel("compare-a"));
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
    }

    #[test]
    fn a_finished_compare_can_no_longer_be_cancelled() {
        let compare = CompareCancel::register(Some("compare-c"));
        drop(compare);
        assert!(!cancel("compare-c"));
    }

    #[test]
    fn an_earlier_compare_ending_leaves_a_later_one_with_the_same_id_registered() {
        let earlier = CompareCancel::register(Some("compare-d"));
        let later = CompareCancel::register(Some("compare-d"));
        drop(earlier);
        assert!(cancel("compare-d"));
        assert!(later.is_cancelled());
    }

    #[test]
    fn a_compare_without_an_id_has_a_flag_nobody_else_can_raise() {
        let compare = CompareCancel::register(None);
        assert!(!compare.is_cancelled());
        assert!(!cancel(""));
    }
}
