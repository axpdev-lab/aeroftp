//! Pure part planner for the S3 delta upload.
//!
//! Tier 1 of `APPENDIX-S3-DELTA-UPLOAD`: a new version of a large object is
//! assembled as a multipart upload whose unchanged parts are copied
//! server-side out of the object already in the bucket (`UploadPartCopy` with
//! `x-amz-copy-source-range`), so only the changed bytes travel on the wire.
//! The destination stays a plain object, byte-identical to the local file.
//!
//! This module is the planning arithmetic and nothing else: no I/O, no HTTP,
//! no knowledge of the provider. It lives on its own because it is the piece
//! where a mistake is most expensive. S3 refuses any part below 5 MiB except
//! the last one, and it refuses it at `CompleteMultipartUpload`, which is
//! *after* every byte has already been transferred: the user pays for the
//! whole upload and still gets `EntityTooSmall`. So the repair rule that
//! turns a raw match list into a legal plan is written and pinned before any
//! code that spends bandwidth exists.

/// Smallest part S3 accepts, except for the last one.
pub const S3_PART_MIN: u64 = 5 * 1024 * 1024;

/// Largest part S3 accepts.
pub const S3_PART_MAX: u64 = 5 * 1024 * 1024 * 1024;

/// Most parts S3 accepts in one multipart upload.
pub const S3_MAX_PARTS: u32 = 10_000;

/// Default delta grid: the cell the matcher compares and the part the planner
/// emits when nothing forces it wider. A small multiple of the floor, so a cell
/// that matches is likely to become a whole part rather than be absorbed into
/// an upload run by the repair.
pub const DELTA_PART_SIZE: u64 = 8 * 1024 * 1024;

/// Below this a delta is not attempted at all.
///
/// Mirrors the provider's `MULTIPART_THRESHOLD`: under it `upload` sends a
/// single PUT, and a multipart made of one part saves nothing. The value is
/// repeated here rather than imported, so this module stays free of the
/// provider, and `s3.rs` carries a test that fails if the two ever drift.
pub const DELTA_MIN_FILE_SIZE: u64 = 200 * 1024 * 1024;

/// Largest grid this module will ever use or accept, which is NOT the largest
/// part S3 allows.
///
/// The grid is handed to [`plan_delta_parts`] as its `part_min`, and the
/// planner refuses a floor above half its ceiling, because above that an
/// over-long run cannot be split into equal pieces that all still clear the
/// floor. So the usable grid stops at half the part maximum, and every place
/// that bounds a grid reads it from here rather than restating the arithmetic:
/// the first version of this module got the bound right in the function that
/// chooses a grid and left the protocol value in the function that accepts one,
/// which is the same defect twice with one of the two occurrences fixed.
/// `the_planner_accepts_exactly_this_grid_as_a_floor` pins the derivation.
pub const DELTA_GRID_MAX: u64 = S3_PART_MAX / 2;

/// Largest file an S3 delta can cover at all.
///
/// The protocol arithmetic alone would say `S3_MAX_PARTS * S3_PART_MAX`, 48.8
/// TiB, and that number is wrong for this planner: past `S3_MAX_PARTS *
/// DELTA_GRID_MAX`, 24.4 TiB, the grid the rule would choose is one the planner
/// refuses on every plan. The two halves have to be sized against each other
/// rather than each against the protocol.
pub const DELTA_MAX_FILE_SIZE: u64 = S3_MAX_PARTS as u64 * DELTA_GRID_MAX;

/// Choose the grid a file of `file_len` bytes is compared and cut on, or refuse
/// the file.
///
/// The default grid is [`DELTA_PART_SIZE`], and it does not survive every size:
/// a file large enough that `file_len / DELTA_PART_SIZE` exceeds the 10 000
/// part cap cannot be planned on it, because the worst case for the planner is
/// one part per cell (a file whose cells alternate between changed and
/// unchanged produces no coalescing at all). So the grid scales up to at least
/// `file_len / S3_MAX_PARTS`, which is what keeps that worst case legal rather
/// than only the average case.
///
/// The scaled grid is rounded up to a whole number of default cells, and that
/// rounding is the point rather than tidiness. The grid is stored next to the
/// baseline digests and a later delta has to reuse the same one, so a grid that
/// moved with every byte of length would throw the cache away on every upload.
/// Rounded, it only moves when `file_len` crosses a multiple of
/// `S3_MAX_PARTS * DELTA_PART_SIZE`, which is 78.125 GiB, so an append or an
/// edit keeps the grid it had.
///
/// Returns `None` for a file too small for a delta to be worth anything and for
/// a file past [`DELTA_MAX_FILE_SIZE`], where no legal grid exists.
pub fn delta_grid_size(file_len: u64) -> Option<u64> {
    if !(DELTA_MIN_FILE_SIZE..=DELTA_MAX_FILE_SIZE).contains(&file_len) {
        return None;
    }
    let needed = file_len.div_ceil(u64::from(S3_MAX_PARTS));
    let grid = DELTA_PART_SIZE
        .max(needed)
        .div_ceil(DELTA_PART_SIZE)
        .saturating_mul(DELTA_PART_SIZE);
    // `needed` is at most DELTA_GRID_MAX at the maximum file size, and 2.5 GiB
    // is a whole number of 8 MiB cells, so the rounding never lifts the grid
    // past what the planner accepts as a floor. Asserted by
    // `the_grid_never_exceeds_what_the_planner_accepts` rather than left as a
    // claim in a comment.
    Some(grid)
}

/// Whether a grid recorded with an earlier upload can still be used for a file
/// of `file_len` bytes.
///
/// A delta must compare the new file against the baseline on the grid the
/// baseline's digests were computed with, not on the grid the new length would
/// choose, or the cells do not line up and nothing matches. The stored grid can
/// still have gone out of range in the meantime, most obviously when the file
/// has grown far enough that it no longer fits under the part cap on it, and
/// then the honest answer is a full upload and a fresh set of digests.
pub fn delta_grid_fits(grid: u64, file_len: u64) -> bool {
    (S3_PART_MIN..=DELTA_GRID_MAX).contains(&grid)
        && (DELTA_MIN_FILE_SIZE..=DELTA_MAX_FILE_SIZE).contains(&file_len)
        && file_len.div_ceil(grid) <= u64::from(S3_MAX_PARTS)
}

/// One part of a planned delta upload.
///
/// Parts are emitted in ascending `part_number` and concatenated by S3 in
/// that order, so a part carries no destination offset: its position in the
/// new object is the sum of the lengths of the parts before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaPart {
    /// Assembled by the server from the baseline object, with
    /// `UploadPartCopy` and `x-amz-copy-source-range: bytes=<start>-<end>`.
    /// Costs a request and no wire bytes.
    Copy {
        part_number: u32,
        src_start: u64,
        src_end_inclusive: u64,
    },
    /// Uploaded from the local file. These are the bytes that travel.
    Put {
        part_number: u32,
        local_start: u64,
        len: u64,
    },
}

impl DeltaPart {
    /// Number of bytes this part contributes to the new object.
    pub fn byte_len(&self) -> u64 {
        match *self {
            DeltaPart::Copy {
                src_start,
                src_end_inclusive,
                ..
            } => src_end_inclusive - src_start + 1,
            DeltaPart::Put { len, .. } => len,
        }
    }

    pub fn part_number(&self) -> u32 {
        match *self {
            DeltaPart::Copy { part_number, .. } | DeltaPart::Put { part_number, .. } => part_number,
        }
    }

    pub fn is_copy(&self) -> bool {
        matches!(*self, DeltaPart::Copy { .. })
    }
}

/// A stretch of the new object, before it is cut into parts.
///
/// `src_start` is `Some` when the stretch can be served by a ranged copy out
/// of the baseline, `None` when its bytes have to be uploaded. Runs are kept
/// sorted, contiguous, non-empty, and covering `[0, file_len)` exactly, at
/// every step of the repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    local_start: u64,
    len: u64,
    src_start: Option<u64>,
}

impl Run {
    fn is_copy(&self) -> bool {
        self.src_start.is_some()
    }
}

/// Turn a raw match list into a legal S3 multipart plan, or refuse.
///
/// Each entry of `matches` is `(local_off, src_off, len)` and asserts that
/// `local[local_off .. local_off + len]` is byte-identical to
/// `baseline[src_off .. src_off + len]`. The caller certifies that; this
/// function never reads either file and never re-checks a match. The Tier 1
/// matcher compares aligned grid cells, so it always produces
/// `local_off == src_off`; nothing here requires that, so the shifted Tier 2
/// matcher can reuse the planner unchanged.
///
/// Returns `None` when there is no legal plan worth running, which the caller
/// must read as "upload the file the ordinary way". That covers a file below
/// the part floor, a match list that survives the repair with no copy left, a
/// plan that would need more than `max_parts` parts, and a malformed match
/// list (overlapping matches, or a match running past the end of the file):
/// a wrong match list is a matcher bug, and the safe answer to it is the full
/// upload, not a repaired plan built on it.
///
/// The caller is responsible for the checks this function cannot make: that
/// the baseline is the object it thinks it is (pinned with
/// `x-amz-copy-source-if-match` or a `versionId`), and that every `src_off`
/// falls inside it.
///
/// `part_min` and `max_parts` are parameters because the caller varies them
/// upward: the delta grid can be coarser than the floor, and the scale-up rule
/// that keeps a large file under the part cap is the caller's decision. They
/// cannot be varied downward, and this entry point speaks S3, so a `part_min`
/// under the protocol floor or a `max_parts` over the protocol cap is refused
/// here rather than turned into a plan the server would reject at
/// `CompleteMultipartUpload`. All three protocol limits are enforced at this
/// door or none of them would be, which is the asymmetry this guard removes.
pub fn plan_delta_parts(
    file_len: u64,
    matches: &[(u64, u64, u64)],
    part_min: u64,
    max_parts: u32,
) -> Option<Vec<DeltaPart>> {
    if part_min < S3_PART_MIN || max_parts > S3_MAX_PARTS {
        return None;
    }
    plan_delta_parts_bounded(file_len, matches, part_min, S3_PART_MAX, max_parts)
}

/// [`plan_delta_parts`] with the part ceiling as a parameter and without the
/// S3 protocol limits, so the tests can exercise the splitting rule on numbers
/// a reader can check by hand. It is also the shape a backend with a different
/// geometry would reuse: Azure's `Put Block From URL` has no 5 MiB floor, which
/// is Tier 3 of the appendix and the reason the arithmetic is kept free of the
/// S3 numbers.
fn plan_delta_parts_bounded(
    file_len: u64,
    matches: &[(u64, u64, u64)],
    part_min: u64,
    part_max: u64,
    max_parts: u32,
) -> Option<Vec<DeltaPart>> {
    if file_len == 0 || part_min == 0 || max_parts == 0 {
        return None;
    }
    // A run longer than the ceiling is cut into equal pieces, and every piece
    // has to clear the floor. Below a ceiling of two floors that is not
    // always possible, so refuse rather than emit a plan the server rejects.
    // The real S3 numbers are 5 MiB and 5 GiB, a factor of 1024.
    if part_max < part_min.saturating_mul(2) {
        return None;
    }
    // Under one full part there is nothing a multipart can win: a single PUT
    // is both legal and cheaper.
    if file_len < part_min {
        return None;
    }

    let mut runs = build_runs(file_len, matches)?;
    repair_runs(&mut runs, part_min);
    if !runs.iter().any(Run::is_copy) {
        return None;
    }
    emit_parts(&runs, part_max, max_parts)
}

/// Lay the matches out as an alternating sequence of copy and upload runs
/// covering `[0, file_len)`, or refuse a match list that cannot be trusted.
fn build_runs(file_len: u64, matches: &[(u64, u64, u64)]) -> Option<Vec<Run>> {
    let mut sorted: Vec<(u64, u64, u64)> = matches
        .iter()
        .copied()
        .filter(|(_, _, len)| *len > 0)
        .collect();
    sorted.sort_unstable_by_key(|(local_off, _, _)| *local_off);

    let mut runs: Vec<Run> = Vec::new();
    let mut cursor = 0u64;
    for (local_off, src_off, len) in sorted {
        if local_off < cursor {
            return None; // overlapping matches
        }
        let local_end = local_off.checked_add(len)?;
        if local_end > file_len {
            return None; // match runs past the end of the local file
        }
        src_off.checked_add(len)?; // source range must not wrap

        if local_off > cursor {
            runs.push(Run {
                local_start: cursor,
                len: local_off - cursor,
                src_start: None,
            });
        }
        push_copy(&mut runs, local_off, src_off, len);
        cursor = local_end;
    }
    if cursor < file_len {
        runs.push(Run {
            local_start: cursor,
            len: file_len - cursor,
            src_start: None,
        });
    }
    Some(runs)
}

/// Append a copy run, folding it into the previous one when the two are
/// contiguous in *both* coordinates. One `UploadPartCopy` carries one
/// contiguous source range, so two matches that touch locally but come from
/// different places in the baseline stay two runs.
fn push_copy(runs: &mut Vec<Run>, local_start: u64, src_start: u64, len: u64) {
    if let Some(prev) = runs.last_mut() {
        if prev.src_start == Some(src_start.wrapping_sub(prev.len))
            && prev.local_start + prev.len == local_start
            && src_start >= prev.len
        {
            prev.len += len;
            return;
        }
    }
    runs.push(Run {
        local_start,
        len,
        src_start: Some(src_start),
    });
}

/// Repair the run list until every run except the last clears `part_min`.
///
/// Two moves, applied left to right until neither applies:
///
/// 1. A copy run under the floor is demoted to an upload run. It cannot grow:
///    its bytes are the only ones that match, so the alternative to demoting
///    it does not exist. A trailing short run is left alone, since S3 lets the
///    final part be short.
/// 2. An upload run under the floor absorbs bytes from a neighbouring copy
///    run. The following one is preferred: taking bytes off its front leaves
///    its source range contiguous, and keeps the walk moving forward. When the
///    follower cannot spare them without dropping under the floor itself, the
///    preceding run is tried instead, and only if neither can spare them
///    cleanly does the follower give them up anyway and get repaired on the
///    next pass.
///
/// Both moves turn copy bytes into upload bytes, at least one byte each time,
/// so the total number of copied bytes strictly decreases: the loop
/// terminates. It is also why the moves are worth ordering by preference,
/// since every one of them costs wire bytes.
fn repair_runs(runs: &mut Vec<Run>, part_min: u64) {
    loop {
        let last = runs.len().saturating_sub(1);

        if let Some(idx) = runs
            .iter()
            .position(|r| r.is_copy() && r.len < part_min)
            .filter(|idx| *idx != last)
        {
            runs[idx].src_start = None;
            coalesce(runs);
            continue;
        }

        let Some(idx) = runs
            .iter()
            .position(|r| !r.is_copy() && r.len < part_min)
            .filter(|idx| *idx != last)
        else {
            return;
        };

        // A short upload run that is not the last one always has a copy run
        // after it: runs alternate once coalesced.
        let need = part_min - runs[idx].len;
        let follower_spares = can_spare(&runs[idx + 1], need, idx + 1 == last, part_min);
        let predecessor_spares = idx > 0 && can_spare(&runs[idx - 1], need, false, part_min);

        if follower_spares || !predecessor_spares {
            let take = need.min(runs[idx + 1].len);
            trim_front(&mut runs[idx + 1], take);
            runs[idx].len += take;
            if runs[idx + 1].len == 0 {
                runs.remove(idx + 1);
            }
        } else {
            let take = need.min(runs[idx - 1].len);
            runs[idx - 1].len -= take;
            runs[idx].local_start -= take;
            runs[idx].len += take;
            if runs[idx - 1].len == 0 {
                runs.remove(idx - 1);
            }
        }
        coalesce(runs);
    }
}

/// True when `run` can give up `need` bytes and what is left is still a legal
/// run: gone entirely, or still above the floor, or last and therefore exempt.
fn can_spare(run: &Run, need: u64, is_last: bool, part_min: u64) -> bool {
    if run.len < need {
        return false;
    }
    let left = run.len - need;
    left == 0 || left >= part_min || is_last
}

/// Drop `take` bytes off the front of a run, moving both of its offsets.
fn trim_front(run: &mut Run, take: u64) {
    run.local_start += take;
    if let Some(src) = run.src_start.as_mut() {
        *src += take;
    }
    run.len -= take;
}

/// Fold neighbours that can be served by one part: two upload runs always,
/// two copy runs only when their source ranges are contiguous too.
fn coalesce(runs: &mut Vec<Run>) {
    let mut i = 1;
    while i < runs.len() {
        let prev = runs[i - 1];
        let cur = runs[i];
        let mergeable = match (prev.src_start, cur.src_start) {
            (None, None) => true,
            (Some(prev_src), Some(cur_src)) => prev_src + prev.len == cur_src,
            _ => false,
        };
        if mergeable {
            runs[i - 1].len += cur.len;
            runs.remove(i);
        } else {
            i += 1;
        }
    }
}

/// Cut the repaired runs into numbered parts, or refuse a plan that would
/// exceed the part cap.
fn emit_parts(runs: &[Run], part_max: u64, max_parts: u32) -> Option<Vec<DeltaPart>> {
    // Count before allocating: a coarse ceiling on a huge file would otherwise
    // ask for a vector nobody wants to build just to throw it away.
    let total: u64 = runs.iter().map(|r| r.len.div_ceil(part_max)).sum();
    if total == 0 || total > u64::from(max_parts) {
        return None;
    }

    let mut parts = Vec::with_capacity(total as usize);
    let mut part_number = 1u32;
    for run in runs {
        for (offset, len) in split_even(run.len, part_max) {
            parts.push(match run.src_start {
                Some(src) => DeltaPart::Copy {
                    part_number,
                    src_start: src + offset,
                    src_end_inclusive: src + offset + len - 1,
                },
                None => DeltaPart::Put {
                    part_number,
                    local_start: run.local_start + offset,
                    len,
                },
            });
            part_number += 1;
        }
    }
    Some(parts)
}

/// Cut `len` into the fewest pieces that all fit under `part_max`, as evenly
/// as they divide. Even beats greedy here: greedy leaves a remainder that can
/// land under the floor, and a part under the floor in the middle of a plan is
/// exactly the `EntityTooSmall` this module exists to prevent.
fn split_even(len: u64, part_max: u64) -> Vec<(u64, u64)> {
    let pieces = len.div_ceil(part_max).max(1);
    let base = len / pieces;
    let remainder = len % pieces;
    let mut out = Vec::with_capacity(pieces as usize);
    let mut offset = 0u64;
    for i in 0..pieces {
        let piece = base + u64::from(i < remainder);
        out.push((offset, piece));
        offset += piece;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    /// Assert every invariant the S3 multipart API imposes on a plan, plus
    /// the one only the caller can check: a copy part may only cover bytes
    /// the matcher certified as identical, at the offset it certified them.
    ///
    /// `local` walks the destination: parts are concatenated in part-number
    /// order, so a part's position in the new object is the sum of the
    /// lengths before it and nothing in `DeltaPart` needs to carry it.
    fn assert_plan_is_legal(
        file_len: u64,
        matches: &[(u64, u64, u64)],
        part_min: u64,
        part_max: u64,
        max_parts: u32,
        parts: &[DeltaPart],
    ) {
        assert!(!parts.is_empty(), "a plan with no part is not a plan");
        assert!(
            parts.len() <= max_parts as usize,
            "plan has {} parts, over the cap of {max_parts}",
            parts.len()
        );
        assert!(
            parts.iter().any(DeltaPart::is_copy),
            "a plan with no copy part saves nothing and must be None instead"
        );

        let mut local = 0u64;
        for (index, part) in parts.iter().enumerate() {
            let part_len = part.byte_len();
            assert_eq!(
                part.part_number() as usize,
                index + 1,
                "part numbers must be 1..=N with no gap"
            );
            assert!(part_len > 0, "part {} is empty", index + 1);
            assert!(
                part_len <= part_max,
                "part {} is {part_len} bytes, over the ceiling of {part_max}",
                index + 1
            );
            if index + 1 < parts.len() {
                assert!(
                    part_len >= part_min,
                    "part {} is {part_len} bytes, under the floor of {part_min}, and it is not the last part",
                    index + 1
                );
            }

            match *part {
                DeltaPart::Put { local_start, .. } => {
                    assert_eq!(
                        local_start,
                        local,
                        "put part {} starts at {local_start}, expected {local}",
                        index + 1
                    );
                    assert!(
                        local_start + part_len <= file_len,
                        "put part {} reads past the end of the local file",
                        index + 1
                    );
                }
                DeltaPart::Copy {
                    src_start,
                    src_end_inclusive,
                    ..
                } => {
                    assert!(
                        src_end_inclusive >= src_start,
                        "copy part {} has an inverted range",
                        index + 1
                    );
                    // Walk the part against the match list rather than
                    // looking for one match that contains it: the planner is
                    // allowed to fold contiguous matches into one ranged
                    // copy, so a legal part can span several of them. Every
                    // byte still has to be certified, at the right source
                    // offset.
                    let part_end = local + part_len;
                    let mut cursor = local;
                    while cursor < part_end {
                        let covering = matches
                            .iter()
                            .find(|&&(local_off, _, len)| {
                                cursor >= local_off && cursor < local_off + len
                            })
                            .copied();
                        let Some((local_off, src_off, len)) = covering else {
                            panic!(
                                "copy part {} takes baseline bytes for destination offset \
                                 {cursor}, which no match certifies",
                                index + 1
                            );
                        };
                        assert_eq!(
                            src_off + (cursor - local_off),
                            src_start + (cursor - local),
                            "copy part {} maps destination offset {cursor} to the wrong baseline \
                             offset",
                            index + 1
                        );
                        cursor = (local_off + len).min(part_end);
                    }
                }
            }
            local += part_len;
        }
        assert_eq!(
            local, file_len,
            "the plan must cover the file exactly, with no gap and no overlap"
        );
    }

    fn uploaded_bytes(parts: &[DeltaPart]) -> u64 {
        parts
            .iter()
            .filter(|p| !p.is_copy())
            .map(DeltaPart::byte_len)
            .sum()
    }

    // ---- named edge cases -------------------------------------------------

    #[test]
    fn zero_length_file_has_no_plan() {
        assert_eq!(plan_delta_parts_bounded(0, &[], 10, 100, 10), None);
        assert_eq!(plan_delta_parts_bounded(0, &[(0, 0, 0)], 10, 100, 10), None);
    }

    #[test]
    fn file_under_the_part_floor_has_no_plan() {
        // Fully matched, but a single PUT is both legal and cheaper than a
        // one-part multipart, and below the floor nothing else is possible.
        assert_eq!(plan_delta_parts_bounded(9, &[(0, 0, 9)], 10, 100, 10), None);
    }

    #[test]
    fn no_match_has_no_plan() {
        assert_eq!(plan_delta_parts_bounded(100, &[], 10, 100, 10), None);
    }

    #[test]
    fn one_match_over_the_whole_file_is_a_single_copy_part() {
        let matches = [(0, 0, 100)];
        let parts = plan_delta_parts_bounded(100, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![DeltaPart::Copy {
                part_number: 1,
                src_start: 0,
                src_end_inclusive: 99,
            }]
        );
        assert_plan_is_legal(100, &matches, 10, 1000, 10, &parts);
        assert_eq!(uploaded_bytes(&parts), 0);
    }

    #[test]
    fn touching_matches_from_a_contiguous_source_become_one_copy_part() {
        // Two matches that touch locally AND in the baseline are one ranged
        // copy, not two: fewer parts, same bytes.
        let matches = [(0, 0, 40), (40, 40, 60)];
        let parts = plan_delta_parts_bounded(100, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![DeltaPart::Copy {
                part_number: 1,
                src_start: 0,
                src_end_inclusive: 99,
            }]
        );
    }

    #[test]
    fn touching_matches_from_different_source_offsets_stay_two_parts() {
        // One UploadPartCopy carries one contiguous source range, so these
        // cannot be folded even though they touch in the new object.
        let matches = [(0, 500, 30), (30, 0, 30)];
        let parts = plan_delta_parts_bounded(60, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Copy {
                    part_number: 1,
                    src_start: 500,
                    src_end_inclusive: 529,
                },
                DeltaPart::Copy {
                    part_number: 2,
                    src_start: 0,
                    src_end_inclusive: 29,
                },
            ]
        );
        assert_plan_is_legal(60, &matches, 10, 1000, 10, &parts);
    }

    #[test]
    fn match_ending_exactly_at_the_end_of_the_file() {
        let matches = [(10, 10, 40)];
        let parts = plan_delta_parts_bounded(50, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Put {
                    part_number: 1,
                    local_start: 0,
                    len: 10,
                },
                DeltaPart::Copy {
                    part_number: 2,
                    src_start: 10,
                    src_end_inclusive: 49,
                },
            ]
        );
        assert_plan_is_legal(50, &matches, 10, 1000, 10, &parts);
    }

    #[test]
    fn tail_under_the_floor_is_legal_because_it_is_the_last_part() {
        // The append case in miniature: everything matches except the tail,
        // and the tail is shorter than the floor. S3 exempts the last part.
        let matches = [(0, 0, 40)];
        let parts = plan_delta_parts_bounded(45, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Copy {
                    part_number: 1,
                    src_start: 0,
                    src_end_inclusive: 39,
                },
                DeltaPart::Put {
                    part_number: 2,
                    local_start: 40,
                    len: 5,
                },
            ]
        );
        assert_plan_is_legal(45, &matches, 10, 1000, 10, &parts);
    }

    #[test]
    fn over_the_part_cap_is_refused_instead_of_planned() {
        // Refusing here costs nothing. Discovering it at
        // CompleteMultipartUpload costs the whole transfer.
        let matches = [(0, 0, 60)];
        assert!(plan_delta_parts_bounded(60, &matches, 10, 25, 3).is_some());
        assert_eq!(plan_delta_parts_bounded(60, &matches, 10, 25, 2), None);
    }

    #[test]
    fn a_run_over_the_ceiling_is_split_evenly_and_every_piece_clears_the_floor() {
        let matches = [(0, 0, 60)];
        let parts = plan_delta_parts_bounded(60, &matches, 10, 25, 10).expect("plan");
        // 60 over a ceiling of 25 needs 3 pieces. Even split gives 20/20/20;
        // a greedy split would give 25/25/10, which is still legal here but
        // becomes illegal as soon as the remainder falls under the floor.
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert_eq!(part.byte_len(), 20);
        }
        assert_plan_is_legal(60, &matches, 10, 25, 10, &parts);
    }

    #[test]
    fn greedy_split_would_leave_an_illegal_remainder() {
        // 52 bytes, ceiling 25, floor 10: greedy gives 25/25/2 and the 2-byte
        // part is not last in the plan, so it would be rejected. Even split
        // gives 18/17/17.
        let matches = [(0, 0, 52)];
        let parts = plan_delta_parts_bounded(60, &matches, 10, 25, 10).expect("plan");
        assert_plan_is_legal(60, &matches, 10, 25, 10, &parts);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].byte_len(), 18);
    }

    // ---- the repair rule --------------------------------------------------

    #[test]
    fn a_match_under_the_floor_never_becomes_a_copy_part_of_its_own() {
        // The 5-byte match at 60 cannot be a part: it is under the floor and
        // it cannot grow, because its neighbours are bytes that do not match.
        // It is demoted and merged into the surrounding upload.
        let matches = [(0, 0, 40), (60, 60, 5)];
        let parts = plan_delta_parts_bounded(100, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Copy {
                    part_number: 1,
                    src_start: 0,
                    src_end_inclusive: 39,
                },
                DeltaPart::Put {
                    part_number: 2,
                    local_start: 40,
                    len: 60,
                },
            ]
        );
        assert_plan_is_legal(100, &matches, 10, 1000, 10, &parts);
    }

    #[test]
    fn a_short_match_list_that_survives_nothing_has_no_plan() {
        // Every match is under the floor, so every one of them is demoted and
        // no copy part is left: the full upload wins.
        let matches = [(10, 10, 5), (30, 30, 4)];
        assert_eq!(plan_delta_parts_bounded(100, &matches, 10, 1000, 10), None);
    }

    #[test]
    fn a_short_upload_run_absorbs_from_the_following_copy_run() {
        // Gap of 4 between two long matches. The gap has to reach the floor,
        // and it takes the 6 bytes it needs off the FRONT of the copy that
        // follows, which leaves that copy's source range contiguous.
        let matches = [(0, 0, 30), (34, 34, 30)];
        let parts = plan_delta_parts_bounded(64, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Copy {
                    part_number: 1,
                    src_start: 0,
                    src_end_inclusive: 29,
                },
                DeltaPart::Put {
                    part_number: 2,
                    local_start: 30,
                    len: 10,
                },
                DeltaPart::Copy {
                    part_number: 3,
                    src_start: 40,
                    src_end_inclusive: 63,
                },
            ]
        );
        assert_plan_is_legal(64, &matches, 10, 1000, 10, &parts);
        assert_eq!(uploaded_bytes(&parts), 10);
    }

    #[test]
    fn an_upload_run_just_under_the_floor_is_still_grown() {
        // A gap of 7 against a floor of 10. Every upload run below the floor
        // has to be grown, not only the obviously tiny ones.
        let matches = [(0, 0, 30), (37, 37, 30)];
        let parts = plan_delta_parts_bounded(67, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Copy {
                    part_number: 1,
                    src_start: 0,
                    src_end_inclusive: 29,
                },
                DeltaPart::Put {
                    part_number: 2,
                    local_start: 30,
                    len: 10,
                },
                DeltaPart::Copy {
                    part_number: 3,
                    src_start: 40,
                    src_end_inclusive: 66,
                },
            ]
        );
        assert_plan_is_legal(67, &matches, 10, 1000, 10, &parts);
    }

    #[test]
    fn a_short_upload_run_falls_back_to_the_preceding_copy_run() {
        // Same shape, but the following copy is only 12 long: giving up 6
        // would leave 6, under the floor, and it is not the last run. So the
        // bytes come off the BACK of the preceding copy instead, which is 30
        // long and can spare them.
        let matches = [(0, 0, 30), (34, 34, 12)];
        let parts = plan_delta_parts_bounded(66, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(
            parts,
            vec![
                DeltaPart::Copy {
                    part_number: 1,
                    src_start: 0,
                    src_end_inclusive: 23,
                },
                DeltaPart::Put {
                    part_number: 2,
                    local_start: 24,
                    len: 10,
                },
                DeltaPart::Copy {
                    part_number: 3,
                    src_start: 34,
                    src_end_inclusive: 45,
                },
                DeltaPart::Put {
                    part_number: 4,
                    local_start: 46,
                    len: 20,
                },
            ]
        );
        assert_plan_is_legal(66, &matches, 10, 1000, 10, &parts);
    }

    #[test]
    fn a_short_upload_run_at_the_end_is_left_alone() {
        let matches = [(0, 0, 40)];
        let parts = plan_delta_parts_bounded(42, &matches, 10, 1000, 10).expect("plan");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].byte_len(), 2);
        assert_plan_is_legal(42, &matches, 10, 1000, 10, &parts);
    }

    // ---- malformed input --------------------------------------------------

    #[test]
    fn overlapping_matches_are_refused() {
        // A matcher that emits overlapping matches is broken. Repairing its
        // output would hide the bug behind an object that is subtly wrong;
        // the safe answer is the ordinary upload.
        let matches = [(0, 0, 50), (40, 40, 50)];
        assert_eq!(plan_delta_parts_bounded(100, &matches, 10, 1000, 10), None);
    }

    #[test]
    fn a_match_past_the_end_of_the_file_is_refused() {
        let matches = [(80, 80, 40)];
        assert_eq!(plan_delta_parts_bounded(100, &matches, 10, 1000, 10), None);
        let overflowing = [(0, u64::MAX - 4, 100)];
        assert_eq!(
            plan_delta_parts_bounded(100, &overflowing, 10, 1000, 10),
            None
        );
    }

    #[test]
    fn zero_length_matches_are_ignored_not_planned() {
        let matches = [(10, 10, 0), (20, 20, 40)];
        let parts = plan_delta_parts_bounded(100, &matches, 10, 1000, 10).expect("plan");
        assert_plan_is_legal(100, &matches, 10, 1000, 10, &parts);
        assert!(parts.iter().all(|p| p.byte_len() > 0));
    }

    #[test]
    fn a_ceiling_under_two_floors_is_refused() {
        // Not an S3 shape (the real numbers are 5 MiB and 5 GiB), but the
        // even split cannot guarantee the floor under it, so it is refused
        // rather than silently planned wrong.
        assert_eq!(
            plan_delta_parts_bounded(100, &[(0, 0, 100)], 10, 19, 10),
            None
        );
        assert!(plan_delta_parts_bounded(100, &[(0, 0, 100)], 10, 20, 10).is_some());
    }

    // ---- the real S3 numbers ---------------------------------------------

    #[test]
    fn a_floor_under_the_s3_minimum_is_refused_at_the_public_entry_point() {
        // A caller may only coarsen the grid, never take it below the
        // protocol floor. With a floor of 1 these one-byte matches become
        // one-byte copy parts that are not last, which S3 rejects at
        // CompleteMultipartUpload, after the transfer.
        let matches = [(10, 10, 1), (50, 50, 1)];
        assert_eq!(plan_delta_parts(100, &matches, 1, 10_000), None);
        // The guard belongs to the S3 door, not to the arithmetic: the
        // bounded planner still plans it, which is what a backend without the
        // 5 MiB floor would reuse.
        let parts = plan_delta_parts_bounded(100, &matches, 1, S3_PART_MAX, 10_000).expect("plan");
        assert_plan_is_legal(100, &matches, 1, S3_PART_MAX, 10_000, &parts);
        assert!(parts.iter().any(|p| p.is_copy() && p.byte_len() == 1));
    }

    #[test]
    fn a_part_cap_over_the_s3_maximum_is_refused_at_the_public_entry_point() {
        let file_len = 2 * GIB + 50 * MIB;
        let matches = [(0, 0, 2 * GIB)];
        assert_eq!(
            plan_delta_parts(file_len, &matches, 5 * MIB, S3_MAX_PARTS + 1),
            None
        );
        assert!(plan_delta_parts(file_len, &matches, 5 * MIB, S3_MAX_PARTS).is_some());
    }

    #[test]
    fn append_to_a_two_gib_object_uploads_only_the_tail() {
        // The public entry point, with the real 5 MiB floor, the real 5 GiB
        // ceiling and the real 10 000 part cap.
        let file_len = 2 * GIB + 50 * MIB;
        let matches = [(0, 0, 2 * GIB)];
        let parts = plan_delta_parts(file_len, &matches, 5 * MIB, 10_000).expect("plan");
        assert_plan_is_legal(file_len, &matches, 5 * MIB, S3_PART_MAX, 10_000, &parts);
        assert_eq!(parts.len(), 2);
        assert_eq!(uploaded_bytes(&parts), 50 * MIB);
    }

    #[test]
    fn an_edit_in_the_middle_of_a_gib_uploads_only_the_edited_grid_cells() {
        let file_len = GIB;
        let matches = [(0, 0, 512 * MIB), (528 * MIB, 528 * MIB, GIB - 528 * MIB)];
        let parts = plan_delta_parts(file_len, &matches, 5 * MIB, 10_000).expect("plan");
        assert_plan_is_legal(file_len, &matches, 5 * MIB, S3_PART_MAX, 10_000, &parts);
        assert_eq!(parts.len(), 3);
        assert_eq!(uploaded_bytes(&parts), 16 * MIB);
    }

    #[test]
    fn a_copy_run_over_five_gib_is_split_and_the_object_stays_covered() {
        // The appendix's constraint sheet calls the 5 GiB part maximum "not
        // binding here". It binds as soon as adjacent matching grid cells are
        // folded into one part, which is what makes a 12 GiB unchanged
        // stretch a single run.
        let file_len = 12 * GIB;
        let matches = [(0, 0, 11 * GIB)];
        let parts = plan_delta_parts(file_len, &matches, 5 * MIB, 10_000).expect("plan");
        assert_plan_is_legal(file_len, &matches, 5 * MIB, S3_PART_MAX, 10_000, &parts);
        assert!(
            parts.iter().all(|p| p.byte_len() <= S3_PART_MAX),
            "no part may exceed the 5 GiB maximum"
        );
        assert_eq!(uploaded_bytes(&parts), GIB);
    }

    #[test]
    fn a_diffusely_edited_file_is_refused_instead_of_planned_over_the_cap() {
        // Under the real 5 GiB ceiling the part count is driven by the number
        // of alternating runs, not by the grid: a file edited in 6000 places
        // needs 12 000 parts, over the 10 000 cap. Refuse, and let the caller
        // fall back to the ordinary upload.
        //
        // The first draft of this test asserted the cap on a coarse ceiling
        // instead and was wrong: 80 GiB at a 10 MiB ceiling is 8192 parts,
        // comfortably legal. The code said so and the test was corrected.
        let cell = 8 * MIB;
        let matches: Vec<(u64, u64, u64)> = (0..6000u64)
            .map(|i| (i * 2 * cell, i * 2 * cell, cell))
            .collect();
        let file_len = 6000 * 2 * cell;
        assert_eq!(plan_delta_parts(file_len, &matches, 5 * MIB, 10_000), None);
        // The same shape with room under the cap does plan.
        let matches: Vec<(u64, u64, u64)> = (0..2000u64)
            .map(|i| (i * 2 * cell, i * 2 * cell, cell))
            .collect();
        let file_len = 2000 * 2 * cell;
        let parts = plan_delta_parts(file_len, &matches, 5 * MIB, 10_000).expect("plan");
        assert_eq!(parts.len(), 4000);
        assert_plan_is_legal(file_len, &matches, 5 * MIB, S3_PART_MAX, 10_000, &parts);
    }

    // ---- the grid and its scale-up rule -----------------------------------

    #[test]
    fn a_file_under_the_delta_floor_gets_no_grid() {
        assert_eq!(delta_grid_size(DELTA_MIN_FILE_SIZE - 1), None);
        assert_eq!(delta_grid_size(0), None);
        assert_eq!(delta_grid_size(DELTA_MIN_FILE_SIZE), Some(DELTA_PART_SIZE));
    }

    #[test]
    fn an_ordinary_large_file_uses_the_default_grid() {
        // 2 GiB on an 8 MiB grid is 256 cells, nowhere near the cap.
        assert_eq!(delta_grid_size(2 * GIB), Some(DELTA_PART_SIZE));
    }

    #[test]
    fn the_default_grid_survives_up_to_the_part_cap_and_not_past_it() {
        // The default grid covers exactly S3_MAX_PARTS cells, 78.125 GiB.
        let last = u64::from(S3_MAX_PARTS) * DELTA_PART_SIZE;
        assert_eq!(delta_grid_size(last), Some(DELTA_PART_SIZE));
        assert_eq!(delta_grid_size(last + 1), Some(2 * DELTA_PART_SIZE));
    }

    #[test]
    fn the_grid_scales_up_when_the_default_would_blow_the_part_cap() {
        // 200 GiB on the default grid is 25 600 cells. The worst case for the
        // planner is one part per cell, so the grid has to widen until that
        // worst case is legal, not until the average case is.
        let grid = delta_grid_size(200 * GIB).expect("grid");
        assert_eq!(grid, 24 * MIB);
        assert!((200 * GIB).div_ceil(grid) <= u64::from(S3_MAX_PARTS));
        assert_eq!(grid % DELTA_PART_SIZE, 0);
    }

    #[test]
    fn the_grid_never_exceeds_what_the_planner_accepts() {
        // At the largest coverable file the grid lands exactly on half the part
        // ceiling, which is the largest floor `plan_delta_parts` accepts. Half
        // of 5 GiB is a whole number of 8 MiB cells, so the rounding does not
        // push it over; if either constant changes so that it is not, this
        // fails here rather than by refusing every plan at runtime.
        assert_eq!(delta_grid_size(DELTA_MAX_FILE_SIZE), Some(DELTA_GRID_MAX));
        assert_eq!(DELTA_GRID_MAX % DELTA_PART_SIZE, 0);
    }

    #[test]
    fn a_file_past_the_largest_coverable_size_gets_no_grid() {
        assert_eq!(delta_grid_size(DELTA_MAX_FILE_SIZE + 1), None);
        assert_eq!(delta_grid_size(u64::MAX), None);
    }

    #[test]
    fn an_append_does_not_move_the_grid() {
        // The grid is stored with the baseline digests and a later delta has to
        // reuse it. A grid that moved with every byte would throw the cache
        // away exactly in the case the feature exists for.
        let before = delta_grid_size(2 * GIB).expect("grid");
        let after = delta_grid_size(2 * GIB + 50 * MIB).expect("grid");
        assert_eq!(before, after);
        // Past the point where the default no longer covers the file, the raw
        // ratio moves with every byte and only the rounding holds the grid
        // still. 100 GiB and the same file with 50 MiB appended must land on
        // the same grid, or the appended upload would have to rehash the whole
        // baseline it was about to reuse.
        let before = delta_grid_size(100 * GIB).expect("grid");
        let after = delta_grid_size(100 * GIB + 50 * MIB).expect("grid");
        assert_eq!(before, after);
        assert_eq!(before, 2 * DELTA_PART_SIZE);
    }

    #[test]
    fn a_stored_grid_that_no_longer_fits_is_rejected() {
        // The file grew from 2 GiB to 100 GiB: 12 800 cells on the stored 8 MiB
        // grid, over the cap. The answer is a full upload and fresh digests,
        // not a plan the server would refuse.
        assert!(delta_grid_fits(DELTA_PART_SIZE, 2 * GIB));
        assert!(!delta_grid_fits(DELTA_PART_SIZE, 100 * GIB));
        // A grid outside what the planner takes is never usable, whatever
        // produced it. The upper bound here is DELTA_GRID_MAX and not the
        // protocol's 5 GiB: a grid between the two is legal for S3 and refused
        // by the planner on every plan, and this function reads a grid from
        // storage, so a value written by an older rule can arrive here.
        assert!(!delta_grid_fits(0, 2 * GIB));
        assert!(!delta_grid_fits(S3_PART_MIN - 1, 2 * GIB));
        assert!(delta_grid_fits(DELTA_GRID_MAX, DELTA_MAX_FILE_SIZE));
        assert!(!delta_grid_fits(DELTA_GRID_MAX + 1, DELTA_MAX_FILE_SIZE));
        assert!(!delta_grid_fits(S3_PART_MAX, 2 * GIB));
        assert!(!delta_grid_fits(DELTA_PART_SIZE, DELTA_MIN_FILE_SIZE - 1));
    }

    #[test]
    fn every_grid_the_rule_chooses_is_one_the_rule_accepts() {
        // The two halves have to agree: a grid chosen for a length must still
        // be judged usable for that same length, or an upload would store a
        // grid its own delta then refuses.
        let mut rng = Lcg(0xf00d_4242);
        let mut chosen = 0usize;
        let rounds = 3000;
        for _ in 0..rounds {
            let file_len = rng.below(DELTA_MAX_FILE_SIZE + DELTA_MAX_FILE_SIZE / 8);
            match delta_grid_size(file_len) {
                Some(grid) => {
                    assert_eq!(
                        grid % DELTA_PART_SIZE,
                        0,
                        "grid is not a whole number of default cells at {file_len}"
                    );
                    assert!(
                        (S3_PART_MIN..=DELTA_GRID_MAX).contains(&grid),
                        "grid {grid} outside the protocol at {file_len}"
                    );
                    assert!(
                        file_len.div_ceil(grid) <= u64::from(S3_MAX_PARTS),
                        "grid {grid} leaves {} cells at {file_len}",
                        file_len.div_ceil(grid)
                    );
                    assert!(
                        delta_grid_fits(grid, file_len),
                        "chosen grid {grid} rejected for {file_len}"
                    );
                    chosen += 1;
                }
                None => assert!(
                    !(DELTA_MIN_FILE_SIZE..=DELTA_MAX_FILE_SIZE).contains(&file_len),
                    "refused a file of {file_len} bytes that is inside the coverable range"
                ),
            }
        }
        assert!(
            chosen > rounds / 4,
            "the generator produced only {chosen} grids out of {rounds}: the assertions above were barely exercised"
        );
    }

    #[test]
    fn the_planner_accepts_exactly_this_grid_as_a_floor() {
        // DELTA_GRID_MAX is derived from a rule that lives in another function,
        // so it is pinned against that function rather than restated. One byte
        // more and the planner refuses every plan, which is what makes a grid
        // above it useless rather than merely large.
        let file_len = 2 * DELTA_GRID_MAX;
        let matches = [(0, 0, DELTA_GRID_MAX)];
        assert!(
            plan_delta_parts(file_len, &matches, DELTA_GRID_MAX, S3_MAX_PARTS).is_some(),
            "the planner must accept DELTA_GRID_MAX as a floor"
        );
        assert_eq!(
            plan_delta_parts(file_len, &matches, DELTA_GRID_MAX + 1, S3_MAX_PARTS),
            None,
            "one byte over DELTA_GRID_MAX the planner refuses, which is where the constant comes from"
        );
    }

    #[test]
    fn the_chosen_grid_survives_the_planner_s_worst_case() {
        // The rule sizes the grid for the WORST case rather than the average
        // one: a file whose cells alternate between changed and unchanged
        // coalesces nothing and produces one part per cell. This is the seam
        // between the two halves, and it is where the first version was wrong:
        // the ceiling on the file size was computed from the protocol alone,
        // which handed the planner a floor above half its own ceiling and got
        // every plan refused. Found here, before review.
        for file_len in [
            DELTA_MIN_FILE_SIZE,
            2 * GIB,
            79 * GIB,
            200 * GIB,
            DELTA_MAX_FILE_SIZE,
        ] {
            let grid = delta_grid_size(file_len).expect("grid");
            let cells = file_len.div_ceil(grid);
            let matches: Vec<(u64, u64, u64)> = (0..cells)
                .step_by(2)
                .map(|cell| {
                    let start = cell * grid;
                    (start, start, grid.min(file_len - start))
                })
                .collect();
            let parts = plan_delta_parts(file_len, &matches, grid, S3_MAX_PARTS)
                .unwrap_or_else(|| panic!("no plan for {file_len} bytes on a {grid} byte grid"));
            assert!(
                parts.len() <= S3_MAX_PARTS as usize,
                "{file_len} bytes on a {grid} byte grid needs {} parts",
                parts.len()
            );
            assert_eq!(
                parts.iter().map(DeltaPart::byte_len).sum::<u64>(),
                file_len,
                "the plan must still cover the file exactly"
            );
        }
    }

    // ---- generated inputs -------------------------------------------------

    /// Deterministic generator. No new dependency, and a failure is
    /// reproducible from the seed printed in the panic.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 11
        }

        fn below(&mut self, bound: u64) -> u64 {
            if bound == 0 {
                0
            } else {
                self.next_u64() % bound
            }
        }
    }

    #[test]
    fn generated_match_lists_always_produce_a_legal_plan_or_none() {
        let mut rng = Lcg(0x5eed_1234);
        let mut planned = 0usize;
        let mut refused = 0usize;
        let rounds = 4000;

        for _ in 0..rounds {
            let part_min = 4 + rng.below(8);
            let part_max = part_min * (2 + rng.below(3));
            let max_parts = 1 + rng.below(40) as u32;
            let file_len = rng.below(400);

            let mut matches: Vec<(u64, u64, u64)> = Vec::new();
            let mut cursor = 0u64;
            while cursor < file_len {
                cursor += rng.below(30);
                if cursor >= file_len {
                    break;
                }
                let len = rng.below(60).min(file_len - cursor);
                if len == 0 {
                    cursor += 1;
                    continue;
                }
                // Half aligned (what the Tier 1 matcher emits), half shifted
                // (what a Tier 2 matcher would emit), so the planner is
                // exercised on both.
                let src = if rng.below(2) == 0 {
                    cursor
                } else {
                    rng.below(1_000_000)
                };
                matches.push((cursor, src, len));
                cursor += len;
            }

            match plan_delta_parts_bounded(file_len, &matches, part_min, part_max, max_parts) {
                Some(parts) => {
                    assert_plan_is_legal(file_len, &matches, part_min, part_max, max_parts, &parts);
                    planned += 1;
                }
                None => refused += 1,
            }
        }

        // A generator that never plans anything would make every assertion
        // above vacuous and the test would still be green.
        assert_eq!(planned + refused, rounds);
        assert!(
            planned > rounds / 10,
            "the generator produced only {planned} plans out of {rounds}: the invariants above \
             were barely exercised"
        );
    }
}
