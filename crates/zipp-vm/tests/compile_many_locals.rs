//! Compiling a function with many locals takes time linear in their number.
//!
//! Each declaration and each name resolution scanned its scope's bindings,
//! in the parser (the redeclaration checks) and in the compiler (`resolve`,
//! the declaration paths, the `with` checks), so a body of n declarations
//! compiled in O(n^2): 1,600, 3,200 and 6,400 `var`s took 19, 74 and 352 ms
//! (f2i-com/zipp.org#72). The test compares two sizes four times apart, so it
//! needs no absolute budget: linear is about 4x, quadratic 16x.

use std::time::{Duration, Instant};
use zipp_vm::embed::{compile_script, JsValue};

/// A function with `n` vars, `n` block-scoped lets and a reference to each.
fn source(n: usize) -> String {
    let mut s = String::from("function big() {\n");
    for i in 0..n {
        s.push_str(&format!("var v{i} = {i};\n"));
    }
    s.push_str("var sum = 0;\n{\n");
    for i in 0..n {
        s.push_str(&format!("let l{i} = v{i};\n"));
    }
    for i in 0..n {
        s.push_str(&format!("sum += l{i};\n"));
    }
    s.push_str("}\nreturn sum;\n}\n");
    s
}

/// Compile `src`, run it and call `big` once (a body may be compiled when it
/// is first called), checking the sum of `n` locals; the quickest of three.
fn compile_and_call_time(src: &str, n: usize) -> Duration {
    let expected = (n * (n - 1) / 2) as f64;
    (0..3)
        .map(|_| {
            let start = Instant::now();
            let mut state = compile_script(src).expect("source compiles");
            state.run_init().expect("script runs");
            let sum = state.call_global("big", &[]);
            let elapsed = start.elapsed();
            assert_eq!(sum, Ok(JsValue::Number(expected)));
            elapsed
        })
        .min()
        .expect("three runs")
}

#[test]
fn compile_time_is_linear_in_the_number_of_locals() {
    let (small, large) = (1_500, 6_000);
    let small_src = source(small);
    let large_src = source(large);
    let t_small = compile_and_call_time(&small_src, small);
    let t_large = compile_and_call_time(&large_src, large);
    let ratio = t_large.as_secs_f64() / t_small.as_secs_f64().max(1e-6);
    assert!(
        ratio < 9.0,
        "{small} locals took {t_small:?}, {large} took {t_large:?}: {ratio:.1}x for 4x the locals"
    );
}
