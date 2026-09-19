// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Allocator tuning for the part-sized buffers the transfer engine owns.

/// Size from which an allocation should come from `mmap` rather than a heap
/// arena. The buffers this is for are the multipart part bodies (16 MiB by
/// default, 5 MiB at the S3 minimum), so the threshold sits below the smallest
/// of them and above everything else the process allocates in bulk.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const PART_BUFFER_MMAP_THRESHOLD_BYTES: libc::c_int = 4 * 1024 * 1024;

/// Ask glibc to serve part-sized allocations with `mmap`, so freeing one
/// returns it to the kernel instead of leaving it in a per-thread arena.
///
/// Why: a multipart upload allocates and frees one full part per part, on
/// whichever worker thread runs it. glibc's dynamic `mmap` threshold adapts
/// upward past 16 MiB, after which those blocks come from arenas and are
/// retained after `free`, once per arena. Peak RSS then grows with the file
/// while the bytes actually live stay inside the transfer engine's buffer
/// budget: measured on a 1 GiB S3 upload (16 MiB parts, loopback MinIO,
/// release build, three repetitions), 277 to 359 MB of RSS against the 139 MB
/// the same upload uses with the threshold pinned, which is the engine's own
/// figure (process baseline plus four parts in flight). Time was unchanged,
/// 1.51 to 2.70 s against 1.55 to 1.67 s.
///
/// `MALLOC_ARENA_MAX=1` reaches the same RSS and is NOT what this does: one
/// arena serialises allocation across the worker threads, and the same upload
/// took 3.67 to 10.00 s.
///
/// Setting the threshold explicitly also turns off glibc's dynamic adjustment,
/// which is the point: the adaptation is what pushes part buffers back into
/// the arenas. Returns whether the tuning was applied, which is false on every
/// platform without glibc's `mallopt` (musl, macOS, Windows), where the
/// allocator does not have this behaviour to correct.
pub fn tune_for_transfer_buffers() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `mallopt` is a glibc tunable setter with no memory operand.
        let applied =
            unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, PART_BUFFER_MMAP_THRESHOLD_BYTES) } == 1;
        if !applied {
            log::debug!("mallopt(M_MMAP_THRESHOLD) refused; leaving glibc's dynamic threshold");
        }
        applied
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn the_threshold_is_accepted_on_glibc() {
        // The RSS effect is a measurement, not a unit test (the tunable is
        // process-global and one-way, so a test cannot observe both states).
        // What a test can pin is that the call is accepted where we claim it
        // applies, so a libc change that renames or rejects the option is not
        // silent.
        assert!(tune_for_transfer_buffers());
    }

    #[test]
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    fn it_is_a_no_op_without_glibc() {
        assert!(!tune_for_transfer_buffers());
    }
}
