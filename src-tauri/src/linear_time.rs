//! The wall-clock form of "linear", for tests of code that must not go
//! quadratic on a hostile input.
//!
//! An absolute bound ("a 1 MB run ends in 2 s") measures the runner as much
//! as the code: under the parallel suite, or on a CI job that stops the whole
//! test process for over a second, linear code goes over it. A ratio of one
//! big run over one small run is no better: the small run fits between two
//! preemptions and the big one does not. What a loaded runner cannot skew is
//! equal work on both sides: `input(n)` once against `input(n / PARTS)`
//! PARTS times, best of five with the two sides interleaved. Linear work
//! takes about as long either way; a step quadratic in the input takes PARTS
//! times longer whole.

use std::time::{Duration, Instant};

/// The input is cut in this many parts.
const PARTS: usize = 10;
/// Whole over parts at or past this is not linear.
const BOUND: u32 = 3;

/// The best time of `run` on `input(n)` whole and on `input(n / PARTS)`
/// PARTS times, each result kept until the clock is read.
fn whole_and_parts<R>(
    n: usize,
    input: impl Fn(usize) -> String,
    run: impl Fn(&str) -> R,
) -> (Duration, Duration) {
    let whole = input(n);
    let parts: Vec<String> = (0..PARTS).map(|_| input(n / PARTS)).collect();
    let mut best_whole = Duration::MAX;
    let mut best_parts = Duration::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        let kept = run(&whole);
        best_whole = best_whole.min(started.elapsed());
        drop(kept);
        let started = Instant::now();
        let kept: Vec<R> = parts.iter().map(|part| run(part)).collect();
        best_parts = best_parts.min(started.elapsed());
        drop(kept);
    }
    (best_whole, best_parts)
}

/// `run` takes about as long on `input(n)` as on PARTS inputs of `n / PARTS`.
pub(crate) fn assert_linear<R>(
    what: &str,
    n: usize,
    input: impl Fn(usize) -> String,
    run: impl Fn(&str) -> R,
) {
    let (whole, parts) = whole_and_parts(n, input, run);
    assert!(
        whole < parts * BOUND,
        "{what} is not linear: {whole:?} whole, {parts:?} in {PARTS} parts"
    );
}
