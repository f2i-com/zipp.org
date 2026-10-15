//! `arr.sort((x, y) => x - y)` over an array of Numbers computes each
//! comparison in place instead of calling the comparator (the specialization
//! the WebAssembly artifact profile had alone; now every profile takes it
//! while no step recorder is installed). The comparator's value is exactly
//! the subtraction it would return, NaN, -0 and the infinities included, so
//! the merge sees the same comparisons and the order is the same. Comparators
//! of any other shape, and arrays holding anything but Numbers, keep the real
//! call. The expected lines are Node 24's.

const SOURCE: &str = r#"
"use strict";
const out = [];
const show = (a) => a.map((v) => (Object.is(v, -0) ? "-0" : String(v))).join(",");
// Integers, doubles, signed zeros in both orders, infinities.
out.push(show([5, 3, 9, 1, 3, 7, 2].sort((x, y) => x - y)));
out.push(show([2.5, -1.25, 0.5, 1e300, -1e-300, 3].sort((x, y) => x - y)));
out.push(show([0, -0, 1, -0, 0, -1].sort((x, y) => x - y)));
out.push(show([-0, 0, -0].sort((x, y) => x - y)));
out.push(show([Infinity, -Infinity, 0, 5, -Infinity].sort((x, y) => x - y)));
// undefined sorts last and is never passed to the comparator.
out.push(show([3, undefined, 1, undefined, 2].sort((x, y) => x - y)));
// A large array, in place and through toSorted.
const big = [];
for (let i = 0; i < 5000; i++) big.push(((i * 7919) % 5003) - 2500 + (i % 3) * 0.5);
const sorted = big.toSorted((x, y) => x - y);
let ok = true;
for (let i = 1; i < sorted.length; i++) if (sorted[i - 1] > sorted[i]) ok = false;
big.sort((x, y) => x - y);
out.push(ok, sorted.length, sorted[0], sorted[2500], sorted[4999], big.join(",") === sorted.join(","));
// Other shapes keep the real call: a function expression, a closure, a
// reversed subtraction, and a mixed array whose coercion is observable.
out.push(show([3, 1, 2].sort(function (x, y) { return x - y; })));
let k = 0;
out.push(show([3, 1, 2].sort((x, y) => x - y + k * 0)));
out.push(show([3, 1, 2].sort((x, y) => y - x)));
let coerced = 0;
const obj = { valueOf() { coerced++; return 2; } };
const mixed = [3, obj, 1].sort((x, y) => x - y);
out.push(mixed[0], mixed[2], coerced > 0);
console.log(out.join(" | "));
"#;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn sort_numeric_sub_matches_node() {
    assert_eq!(run_ok(SOURCE), node_output(SOURCE));
}

/// NaN makes the comparator inconsistent, so the order is the sort
/// algorithm's own (V8's differs). The in-place subtraction must reproduce
/// exactly what the real call gives: compare it with a function expression
/// of the same body, which always calls.
#[test]
fn sort_numeric_sub_nan_matches_the_real_call() {
    let out = run_ok(
        r#"
        const cases = [[3, NaN, 1, NaN, 2, 0], [NaN, -0, 0, NaN, -1], [1, 2, NaN]];
        const fast = cases.map((c) => c.slice().sort((x, y) => x - y).join(","));
        const real = cases.map((c) => c.slice().sort(function (x, y) { return x - y; }).join(","));
        console.log(fast.join(" | "));
        console.log(real.join(" | "));
        "#,
    );
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0], out[1]);
}

/// The same program in fresh children: the interpreter alone, the JIT
/// compiling everything at once, and under GC stress.
#[test]
fn sort_numeric_sub_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("nojit", "ZIPP_NOJIT", "1"),
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("gc", "ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["sort_numeric_sub_", "--skip", "sort_numeric_sub_all_modes"])
            .env(key, value)
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// With a step budget installed the comparator is really called: every
/// comparison charges its `Sub` and `Return` (outside the WebAssembly
/// artifact profile, whose hook the fast path replays tick for tick).
#[cfg(feature = "instrument")]
#[test]
fn metered_sort_still_charges_every_comparison() {
    use zipp_vm::embed;
    let mut st = embed::compile_script("var calls = 0;").expect("compiles");
    st.set_limits(1_000_000_000, None);
    st.run_init().expect("init runs");
    // Count the comparisons the merge makes over this data.
    let calls = st
        .eval_in_context(
            "var a = []; for (var i = 0; i < 2000; i++) a.push((i * 7919) % 2003); \
             a.slice().sort(function (x, y) { calls++; return x - y; }); calls",
        )
        .expect("runs");
    let zipp_vm::embed::JsValue::Number(calls) = calls else {
        panic!("calls is a number: {calls:?}");
    };
    assert!(calls > 10_000.0, "comparisons: {calls}");
    // The arrow sort alone makes the same comparisons, each at least its
    // `Sub` and `Return`: an elided call would charge almost nothing.
    let before = st.steps_remaining();
    st.eval_in_context("a.sort((x, y) => x - y); 0").expect("runs");
    let used = before - st.steps_remaining();
    assert!(used as f64 >= 2.0 * calls, "steps {used} for {calls} comparisons");
}
