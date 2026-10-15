//! The run-time global pool holds the bindings created after compile time:
//! globals an indirect `eval` or `new Function` introduces, the `var` and
//! function declarations of every script an embedder runs through
//! `eval_in_context`, and module bindings. The hardened (safe-sandbox) and
//! wasm profiles reserved 1,024 slots and stopped there, so the reproduction
//! of f2i-com/zipp.org#50 (5,000 script globals) failed in exactly the
//! profile that report's embedding runs, with "too many global bindings".
//! Every profile now has the native pool (16,384 slots); the profiles
//! without a JIT grow their vectors into it rather than reserving it.
//! The first two tests expect Node 24's output (`node -e`, a script); the
//! third pins this engine's own limit, which Node does not have.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// #50's program without the timing: 5,000 script globals and 5,000 script
/// functions, each read and called back by name after the heap has been
/// churned, and a function compiled before them still reading a global
/// that lives on the global object.
#[test]
fn five_thousand_script_globals() {
    let out = run_ok(
        r#"
        globalThis.d = { x: 1 };
        const read = (0, eval)("(function () { let s = 0; for (let i = 0; i < 1000; i++) s += d.x; return s; })");
        const before = read();
        for (let i = 0; i < 5000; i++) {
          (0, eval)("var g" + i + " = " + i + "; function f" + i + "() { return g" + i + " * 2; }");
        }
        let junk = null;
        for (let k = 0; k < 200000; k++) junk = { k, next: junk && k % 64 ? junk : null };
        let sum = 0;
        for (let i = 0; i < 5000; i++) sum += (0, eval)("f" + i + "()");
        const keys = Object.keys(globalThis).filter((k) => /^g\d+$/.test(k)).length;
        console.log(before, read(), sum, g4999, typeof f0, f4999(), keys);
        "#,
    );
    assert_eq!(out, ["1000 1000 24995000 4999 function 9998 5000"]);
}

/// The embedding the report describes: every script runs through
/// `eval_in_context`, with limits set where the build meters, and declares
/// its own globals.
#[test]
fn three_thousand_scripts_through_eval_in_context() {
    let mut state = zipp_vm::embed::compile_script("var total = 0;").expect("compiles");
    #[cfg(feature = "instrument")]
    {
        state.set_limits(u64::MAX, None);
        state.set_heap_limit(256 << 20);
    }
    state.run_init().expect("init runs");
    for i in 0..3000 {
        let src = format!("var s{i} = {i}; function h{i}() {{ return s{i} + 1; }} total += h{i}();");
        state.eval_in_context(&src).expect("script runs");
    }
    let total = state
        .eval_in_context("total + ',' + s2999 + ',' + h0()")
        .expect("reads back");
    assert_eq!(total.as_str(), Some("4501500,2999,1"));
}

/// Past the pool the error is still the clean RangeError, and nothing
/// declared before it is lost.
#[test]
fn exhausting_the_pool_throws_and_keeps_what_it_made() {
    let out = run_ok(
        r#"
        let made = 0, err = "none";
        try {
          for (let i = 0; i < 20000; i++) { (0, eval)("var p" + i + " = " + i); made++; }
        } catch (e) {
          err = e.name + ": " + (e.message.indexOf("too many global bindings") >= 0);
        }
        console.log(made >= 16000, made < 20000, err, p0 + p15999);
        "#,
    );
    assert_eq!(out, ["true true RangeError: true 15999"]);
}
