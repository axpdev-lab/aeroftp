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

use std::hint::black_box;
use std::time::{Duration, Instant};

/// The input is cut in this many parts.
const PARTS: usize = 10;
/// Whole over parts at or past this is not linear.
pub(crate) const BOUND: u32 = 3;

/// The ten parts hold this share of the whole's bytes, at most, either way.
/// A generator with a fixed overhead (a wrapper, a prefix) gives the parts
/// more bytes in all than the whole: at 10 % over, a step quadratic in the
/// bytes still reads about 8 times slower whole; at 2.3 times over (twelve
/// levels of JSON around a 10k payload) it read 1.9 and passed.
const EQUAL_WORK: f64 = 0.10;

/// The best time of `run` on `input(n)` whole and on `input(n / PARTS)`
/// PARTS times, each result kept until the clock is read. Inputs and results
/// go through `black_box`, so an optimized build cannot drop or move the work.
/// The two sides alternate which one runs first, so neither always gets the
/// warmer or the colder start.
pub(crate) fn whole_and_parts<R>(
    n: usize,
    input: impl Fn(usize) -> String,
    run: impl Fn(&str) -> R,
) -> (Duration, Duration) {
    let whole = input(n);
    let parts: Vec<String> = (0..PARTS).map(|_| input(n / PARTS)).collect();
    let parts_len: usize = parts.iter().map(String::len).sum();
    let share = parts_len as f64 / whole.len() as f64;
    assert!(
        (share - 1.0).abs() <= EQUAL_WORK,
        "not equal work: {} bytes whole, {parts_len} in {PARTS} parts",
        whole.len()
    );
    let time_whole = || {
        let started = Instant::now();
        let kept = black_box(run(black_box(&whole)));
        let elapsed = started.elapsed();
        drop(kept);
        elapsed
    };
    let time_parts = || {
        let started = Instant::now();
        let kept: Vec<R> = parts
            .iter()
            .map(|part| black_box(run(black_box(part))))
            .collect();
        let elapsed = started.elapsed();
        drop(kept);
        elapsed
    };
    let mut best_whole = Duration::MAX;
    let mut best_parts = Duration::MAX;
    for sample in 0..5 {
        if sample % 2 == 0 {
            best_whole = best_whole.min(time_whole());
            best_parts = best_parts.min(time_parts());
        } else {
            best_parts = best_parts.min(time_parts());
            best_whole = best_whole.min(time_whole());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(text: &str) -> u64 {
        text.bytes().map(u64::from).sum()
    }

    /// The helper itself: linear work passes, and work quadratic in the
    /// input is past the bound.
    #[test]
    fn it_tells_linear_work_from_quadratic_work() {
        assert_linear("a sum", 2_000_000, |n| "x".repeat(n), sum);
        let prefix_sums = |text: &str| (0..text.len()).map(|i| sum(&text[..i])).sum::<u64>();
        let (whole, parts) = whole_and_parts(10_000, |n| "x".repeat(n), prefix_sums);
        assert!(
            whole >= parts * BOUND,
            "quadratic work read as linear: {whole:?} whole, {parts:?} in {PARTS} parts"
        );
    }

    /// A fixed overhead per input gives the parts more work than the whole,
    /// which hides a quadratic: the helper refuses such an input.
    #[test]
    #[should_panic(expected = "not equal work")]
    fn it_refuses_an_input_whose_parts_hold_more_bytes() {
        let wrapped = |n: usize| format!("{}{}", "w".repeat(16_000), "x".repeat(n));
        whole_and_parts(100_000, wrapped, sum);
    }
}
