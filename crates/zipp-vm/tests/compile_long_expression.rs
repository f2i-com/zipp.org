//! A chain of n `+` compiles in time linear in n.
//!
//! The concat-chain fusion test walked the whole `+` spine below every `+`
//! node to look for a string producer. A numeric chain has none, so each node
//! was then compiled pairwise, and its left operand walked its own spine
//! again: `return 0 + 1 + ... + 6399` took 65 ms and 25,600 terms 1.2 s. The
//! test compares two lengths four times apart (linear is about 4x, quadratic
//! 16x) and checks the values, numeric and string.
//!
//! The hardened profile refuses a chain past 128 links at parse time, so the
//! test is the ordinary profile's.

#![cfg(not(feature = "safe-sandbox"))]

use std::time::{Duration, Instant};
use zipp_vm::embed::{compile_script, JsValue};

fn sum_source(n: usize) -> String {
    let terms: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    format!(
        "function sum() {{ return {}; }}\nfunction text() {{ return ('' + {}).length; }}\n",
        terms.join(" + "),
        terms.join(" + ")
    )
}

/// Compile, run and call both functions; the quickest of three.
fn time_and_check(n: usize) -> Duration {
    let src = sum_source(n);
    let sum = (n * (n - 1) / 2) as f64;
    // The string chain concatenates the decimal digits of 0..n.
    let digits: usize = (0..n).map(|i| i.to_string().len()).sum();
    (0..3)
        .map(|_| {
            let start = Instant::now();
            let mut state = compile_script(&src).expect("source compiles");
            state.run_init().expect("script runs");
            let total = state.call_global("sum", &[]);
            let length = state.call_global("text", &[]);
            let elapsed = start.elapsed();
            assert_eq!(total, Ok(JsValue::Number(sum)));
            assert_eq!(length, Ok(JsValue::Number(digits as f64)));
            elapsed
        })
        .min()
        .expect("three runs")
}

#[test]
fn a_long_plus_chain_compiles_in_linear_time() {
    // The compiler recurses once per `+`, so run where the CLI does: on a
    // 256 MiB stack, not the test harness's default thread.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let (small, large) = (3_000, 12_000);
            let t_small = time_and_check(small);
            let t_large = time_and_check(large);
            let ratio = t_large.as_secs_f64() / t_small.as_secs_f64().max(1e-6);
            assert!(
                ratio < 9.0,
                "{small} terms took {t_small:?}, {large} took {t_large:?}: {ratio:.1}x for 4x the terms"
            );
        })
        .expect("spawn the compile thread")
        .join()
        .expect("the compile thread finishes");
}
