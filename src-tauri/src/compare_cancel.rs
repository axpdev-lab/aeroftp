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
//!
//! A cancel can also arrive before its compare registers: the compare command
//! is an async task, and a Stop pressed right after Start can reach the
//! backend first. Such a cancel is remembered and applied when the compare
//! registers. Progress ids are never reused (`aerosync-<ms>-<seq>`), so a
//! remembered cancel can only ever reach the compare it was meant for.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Default)]
struct Registry {
    running: HashMap<String, Arc<AtomicBool>>,
    /// Cancels for ids not registered (yet). Most are for compares that
    /// already finished, sent by the frontend as it stops waiting; the list
    /// is bounded, so those cost nothing.
    early: VecDeque<String>,
}

/// More than enough: a remembered cancel is needed only for the moment
/// between a compare being invoked and registering.
const EARLY_CANCELS_KEPT: usize = 32;

impl Registry {
    fn cancel(&mut self, progress_id: &str) -> bool {
        if let Some(flag) = self.running.get(progress_id) {
            flag.store(true, Ordering::Relaxed);
            return true;
        }
        if !self.early.iter().any(|early| early == progress_id) {
            if self.early.len() == EARLY_CANCELS_KEPT {
                self.early.pop_front();
            }
            self.early.push_back(progress_id.to_string());
        }
        false
    }

    /// Whether a cancel for `progress_id` arrived before it registered; the
    /// remembered cancel is used up.
    fn take_early(&mut self, progress_id: &str) -> bool {
        match self.early.iter().position(|early| early == progress_id) {
            Some(at) => {
                self.early.remove(at);
                true
            }
            None => false,
        }
    }
}

static FLAGS: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);

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
            let mut registry = FLAGS.lock().unwrap_or_else(|e| e.into_inner());
            if registry.take_early(id) {
                flag.store(true, Ordering::Relaxed);
            }
            registry.running.insert(id.to_string(), Arc::clone(&flag));
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
        let mut registry = FLAGS.lock().unwrap_or_else(|e| e.into_inner());
        // Only our own entry: a later compare may have registered the same id.
        if registry
            .running
            .get(id)
            .is_some_and(|f| Arc::ptr_eq(f, &self.flag))
        {
            registry.running.remove(id);
        }
    }
}

/// Raise the cancel flag of the compare running under `progress_id`.
/// `false` when no compare runs under that id, which is not an error: either
/// it already finished, or it has not registered yet, and then the cancel is
/// remembered and stops it as it starts.
pub(crate) fn cancel(progress_id: &str) -> bool {
    FLAGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cancel(progress_id)
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
    fn a_cancel_that_arrives_before_its_compare_registers_still_stops_it() {
        assert!(!cancel("compare-e"));
        let compare = CompareCancel::register(Some("compare-e"));
        assert!(compare.is_cancelled());
        // Used once: it does not stop a later compare under the same id.
        drop(compare);
        assert!(!CompareCancel::register(Some("compare-e")).is_cancelled());
    }

    /// On a registry of its own: the global one is shared by the tests that
    /// run in parallel, and this one would push their cancels out.
    #[test]
    fn remembered_cancels_are_bounded_and_kept_once() {
        let mut registry = Registry::default();
        assert!(!registry.cancel("compare-f"));
        assert!(!registry.cancel("compare-f"));
        assert_eq!(registry.early.len(), 1);
        for n in 0..EARLY_CANCELS_KEPT {
            registry.cancel(&format!("finished-{n}"));
        }
        assert_eq!(registry.early.len(), EARLY_CANCELS_KEPT);
        assert!(!registry.take_early("compare-f"));
        assert!(registry.take_early("finished-31"));
        assert!(!registry.take_early("finished-31"));
    }

    #[test]
    fn a_compare_without_an_id_has_a_flag_nobody_else_can_raise() {
        let compare = CompareCancel::register(None);
        assert!(!compare.is_cancelled());
        assert!(!cancel(""));
    }
}
