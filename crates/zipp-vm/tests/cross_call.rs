//! Tier C CROSS-CALL fast path (B83): a compiled body's `Call` site whose live
//! callee is itself a Tier-C-compiled plain function is dispatched
//! native→native through `jit_cross_call`, skipping the per-call `ic_call`
//! probe + `setup_call` frame push + nested `run_loop`. Mutual recursion
//! (`pExpr → pTerm → pFactor → pExpr`, the whole `parse-large-js` parse phase)
//! is the target shape.
//!
//! Every expectation below was executed in node (v24) as a script and diffs
//! byte-identical. The whole file must also pass with `ZIPP_NO_CROSSCALL=1`
//! (empty cross plan ⇒ the unchanged `call_ic` helper at every site) and with
//! `ZIPP_NOJIT=1` — the CI-side way to prove the three routes agree.
//!
//! Iteration counts are chosen to cross the fn-JIT threshold with margin, so
//! the callers and callees are Tier-C compiled while the loop is still
//! running (the same warm-up the benches rely on).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn mutual_recursion_hot() {
    // ev/od call each other; both Tier-C compile and cross-call natively.
    let out = run_ok(
        r#"
        function ev(n) { if (n === 0) return true; return od(n - 1); }
        function od(n) { if (n === 0) return false; return ev(n - 1); }
        var s = 0;
        for (var i = 0; i < 60000; i++) s += ev(i % 7) ? 1 : 0;
        console.log("evenodd:" + s);
        "#,
    );
    assert_eq!(out, ["evenodd:34286"]); // node v24
}

#[test]
fn deep_recursion_past_native_depth_caps() {
    // Depth 5000 blows through JIT_REGION_CALL_MAX (64): the cross helper
    // deopts, the site falls to call_ic, that deopts too, and the recursion
    // continues on the interpreter's flat frames — correct value, no crash.
    let out = run_ok(
        r#"
        function dA(n) { if (n <= 0) return 0; return dB(n - 1) + 1; }
        function dB(n) { if (n <= 0) return 0; return dA(n - 1) + 1; }
        for (var w = 0; w < 30000; w++) dA(4); // warm both into Tier C
        console.log("deep:" + dA(5000));
        "#,
    );
    assert_eq!(out, ["deep:5000"]); // node v24
}

#[test]
fn runaway_recursion_throws_catchable_rangeerror() {
    // Sloppy on purpose: strict-mode runaway recursion hangs the BASE
    // interpreter too (pre-existing, unrelated to the cross-call path).
    let out = run_ok(
        r#"
        function inf(n) { return inf2(n + 1); }
        function inf2(n) { return inf(n + 1); }
        try { inf(0); console.log("none"); } catch (e) {
          console.log("caught:" + (e instanceof RangeError));
        }
        "#,
    );
    assert_eq!(out, ["caught:true"]); // node v24 (message text differs; kind matches)
}

#[test]
fn midrun_redefinition_of_one_global_is_observed() {
    // The cross helper resolves the LIVE callee Value each call (read from the
    // caller's register, fed by LoadGlobal), so rebinding gB mid-run must be
    // observed by already-compiled native callers on the next call.
    let out = run_ok(
        r#"
        function gA(n) { if (n <= 0) return 0; return gB(n - 1) + 1; }
        function gB(n) { if (n <= 0) return 0; return gA(n - 1) + 1; }
        var acc = 0;
        for (var i = 0; i < 60000; i++) acc += gA(6);
        gB = function (n) { return 50; };
        for (var i = 0; i < 60000; i++) acc += gA(6);
        console.log("gredef:" + acc);
        "#,
    );
    assert_eq!(out, ["gredef:3420000"]); // node v24
}

#[test]
fn throw_and_finally_across_fast_frames() {
    let out = run_ok(
        r#"
        function tA(n) { if (n === 0) throw new Error("boom"); return tB(n - 1) + 1; }
        function tB(n) { if (n === 0) throw new Error("bang"); return tA(n - 1) + 1; }
        var caught = 0, msg = "";
        for (var i = 0; i < 60000; i++) {
          try { tA(3 + (i % 3)); } catch (e) { caught++; msg = e.message; }
        }
        console.log("throw:" + caught + ":" + msg);
        var fin = 0, fcaught = 0;
        function fA(n) { try { return fB(n); } finally { fin++; } }
        function fB(n) { if (n <= 0) throw new Error("f"); return fA(n - 1) + 1; }
        for (var i = 0; i < 40000; i++) { try { fA(3); } catch (e) { fcaught++; } }
        console.log("finally:" + fin + ":" + fcaught);
        "#,
    );
    assert_eq!(out, ["throw:60000:bang", "finally:160000:40000"]); // node v24
}

#[test]
fn this_binding_strict_and_sloppy() {
    // Strict callee: this === undefined. Sloppy callee: this === globalThis
    // (OrdinaryCallBindThis) — the cross helper bakes both, so parity here is
    // load-bearing.
    let sloppy = run_ok(
        r#"
        function sA(n) { if (n <= 0) return (this === globalThis) ? 1 : 0; return sB(n - 1); }
        function sB(n) { return sA(n - 1); }
        var acc = 0;
        for (var i = 0; i < 60000; i++) acc += sA(4);
        console.log("sloppythis:" + acc);
        "#,
    );
    assert_eq!(sloppy, ["sloppythis:60000"]); // node v24
    let strict = run_ok(
        r#"
        "use strict";
        function sA(n) { if (n <= 0) return (this === undefined) ? 1 : 0; return sB(n - 1); }
        function sB(n) { return sA(n - 1); }
        var acc = 0;
        for (var i = 0; i < 60000; i++) acc += sA(4);
        console.log("strictthis:" + acc);
        "#,
    );
    assert_eq!(strict, ["strictthis:60000"]); // node v24
}

#[test]
fn arity_mismatch_and_hoisted_undefined_locals() {
    // Missing args and never-written locals must read `undefined` in the
    // callee — the cross helper zero-fills the window like `setup_call`.
    let out = run_ok(
        r#"
        "use strict";
        function ar2(a, b) { return "" + a + ":" + b; }
        function callAr(n) { return ar2(n) + "|" + ar2(n, n + 1, n + 2); }
        var last = "";
        for (var i = 0; i < 60000; i++) last = callAr(i % 5);
        console.log("arity:" + last);
        function hoisty(n) { var x; if (n > 100000) x = 1; return "" + x + hoisty2(n); }
        function hoisty2(n) { var y; if (n > 100000) y = 2; return "" + y; }
        var hlast = "";
        for (var i = 0; i < 60000; i++) hlast = hoisty(i % 3);
        console.log("hoist:" + hlast);
        "#,
    );
    assert_eq!(out, ["arity:4:undefined|4:5", "hoist:undefinedundefined"]); // node v24
}

#[test]
fn heap_results_survive_gc_through_fast_frames() {
    // Results are freshly allocated objects; the callee windows are exposed
    // via regs.len(), so a GC at any interior safe point must keep them live.
    let out = run_ok(
        r#"
        "use strict";
        function mkObj(n) { if (n <= 0) return { v: 0 }; var o = mkObj2(n - 1); return { v: o.v + 1 }; }
        function mkObj2(n) { if (n <= 0) return { v: 0 }; var o = mkObj(n - 1); return { v: o.v + 1 }; }
        var gsum = 0;
        for (var i = 0; i < 30000; i++) gsum += mkObj(5).v;
        console.log("gcobjs:" + gsum);
        "#,
    );
    assert_eq!(out, ["gcobjs:150000"]); // node v24
}
