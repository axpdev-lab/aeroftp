//! A process-wide environment variable changed for the length of a scope.
//!
//! The integration tests that redirect one (`HOME`, so an accepted host key
//! cannot reach the real `known_hosts`; the SFTP read pipeline flag) include
//! this file. A variable set and never put back stays set for every test
//! after it in the same binary, `HOME` pointing at a directory that test has
//! deleted; a restore written at the end of a test is skipped by an assertion
//! that fails before it.

use std::ffi::{OsStr, OsString};

/// `name` changed until the guard drops, then put back as it was (or removed
/// if it was not set), unwinding included. The environment is process-wide:
/// the tests that change the same variable hold one lock while a guard lives.
pub struct ScopedEnv {
    name: &'static str,
    previous: Option<OsString>,
}

impl ScopedEnv {
    /// Set `name` to `value`.
    pub fn set(name: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(name);
        // Safety: test process only, under the caller's lock for `name`.
        unsafe { std::env::set_var(name, value) };
        Self { name, previous }
    }

    /// Remove `name`.
    #[allow(dead_code)] // Not every test that includes this file removes one.
    pub fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        // Safety: as in `set`.
        unsafe { std::env::remove_var(name) };
        Self { name, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        // Safety: as in `set`.
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var(self.name, value) },
            None => unsafe { std::env::remove_var(self.name) },
        }
    }
}
