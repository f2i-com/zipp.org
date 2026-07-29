//! Tier parity: compiled code must answer exactly what the interpreter answers.
//!
//! Every case here was found by the audit B66 describes — a systematic sweep for
//! guards applied in SOME of the JIT's tiers and not others, prompted by three
//! bugs of that exact shape (B59, B63, B65). All of them are silent WRONG
//! ANSWERS at DEFAULT thresholds, not crashes, and all of them were invisible to
//! 95,936 test262 executions.
//!
//! The `#[ignore]`d block at the bottom is the part NOT yet fixed. Each is a
//! runnable specification of the correct behaviour, so `cargo test -- --ignored`
//! reports exactly what is still open. Do not delete one without fixing it.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Enough iterations to cross `JIT_THRESHOLD`/`OSR_THRESHOLD` (both 8) and run
/// the compiled body many times.
const HOT: usize = 3000;

// ───────────────────────────── fixed ─────────────────────────────

/// `push` is Set(O, len, v, true) then Set(O, "length", …, true) and both can
/// fail. `jit_array_push` was an unconditional `Vec::push`.
#[test]
fn array_push_respects_integrity() {
    let out = run_ok(&format!(
        r#"
        function pushLoop(a, n) {{ var r = 0; for (var i = 0; i < n; i++) r = a.push(i); return r; }}
        var warm = []; for (var k = 0; k < 60; k++) pushLoop(warm, 40);
        function t(name, a) {{
          try {{ return name + ":ok" + pushLoop(a, 3); }} catch (e) {{ return name + ":" + e.constructor.name; }}
        }}
        var d = [1]; Object.defineProperty(d, "length", {{ writable: false }});
        console.log([t("freeze", Object.freeze([1])), t("seal", Object.seal([1])),
                     t("prevExt", Object.preventExtensions([1])), t("lenRO", d)].join(" "));
        "#
    ));
    assert_eq!(out[0], "freeze:TypeError seal:TypeError prevExt:TypeError lenRO:TypeError");
}

/// A non-writable `length` lives in `array_length_nonwritable`, a side table
/// separate from `arr_props` — so the existing `arr_props` guard never saw it,
/// and a hot `a[3] = i` grew the array.
#[test]
fn set_index_respects_non_writable_length() {
    let out = run_ok(&format!(
        r#"
        function wloop(a, n) {{ for (var i = 0; i < n; i++) {{ a[3] = i; }} }}
        var a = [1, 2, 3];
        Object.defineProperty(a, "length", {{ writable: false }});
        wloop(a, {HOT});
        console.log("len=" + a.length + " a3=" + a[3] + " own=" + a.hasOwnProperty(3));
        "#
    ));
    assert_eq!(out[0], "len=3 a3=undefined own=false");
}

/// Creating a NEW own index — an append, or filling a HOLE — is OrdinarySet, so
/// an inherited setter at that index must run. An in-range write over a present
/// element is not, and stays on the fast path.
#[test]
fn creating_an_index_consults_the_prototype_setter() {
    let out = run_ok(&format!(
        r#"
        var hits = 0, last = -1;
        Object.defineProperty(Array.prototype, "4", {{
          set: function (v) {{ hits++; last = v; }}, get: function () {{ return "G"; }}, configurable: true
        }});
        var b = [0, 1, 2, 3];
        function wloop(a, n) {{ for (var i = 0; i < n; i++) {{ a[4] = i; }} }}
        wloop(b, {HOT});
        console.log("len=" + b.length + " b4=" + b[4] + " own=" + b.hasOwnProperty(4) +
                    " hits=" + hits + " last=" + last);
        delete Array.prototype[4];
        "#
    ));
    assert_eq!(out[0], format!("len=4 b4=G own=false hits={HOT} last={}", HOT - 1));
}

/// An arrow's reg 0 is its CAPTURED `this`; the receiver is ignored. Three JIT
/// paths handed it the receiver instead: the method-inline plan, the off-frame
/// method evaluator reached through `ic_call_method`, and the accessor arms.
#[test]
fn an_arrow_method_keeps_its_lexical_this() {
    let out = run_ok(&format!(
        r#"
        function Maker() {{
          this.f = 111;
          this.o = {{ f: 3, m: () => this.f }};
          var p = {{ f: 3 }};
          Object.defineProperty(p, "v", {{ get: () => this.f, configurable: true }});
          this.p = p;
        }}
        var mk = new Maker();
        function mloop(o, n) {{ var s = 0; for (var i = 0; i < n; i++) s = o.m(); return s; }}
        function gloop(o, n) {{ var s = 0; for (var i = 0; i < n; i++) s = o.v; return s; }}
        console.log(mloop(mk.o, 1) + "," + mloop(mk.o, {HOT}) + "," +
                    gloop(mk.p, 1) + "," + gloop(mk.p, {HOT}));
        "#
    ));
    assert_eq!(out[0], "111,111,111,111");
}

/// The receiver's own `this` happening to equal the arrow's captured `this` must
/// not be the only reason it works, and a plain (non-arrow) method must keep
/// binding the receiver.
#[test]
fn non_arrow_methods_still_bind_the_receiver() {
    let out = run_ok(&format!(
        r#"
        var o = {{ f: 7, m: function () {{ return this.f; }} }};
        function mloop(o, n) {{ var s = 0; for (var i = 0; i < n; i++) s = o.m(); return s; }}
        var p = {{ f: 9 }}; p.m = o.m;
        console.log(mloop(o, {HOT}) + "," + mloop(p, {HOT}));
        "#
    ));
    assert_eq!(out[0], "7,9");
}

// ─────────────────────── open: still diverging ───────────────────────
// Each of these FAILS today. They are written as the correct behaviour so that
// `cargo test -- --ignored` is an accurate list of what is still broken.
// See PERF_ROADMAP B66 for the analysis of each.

/// `delete` of an implicit global returns its slot to the uninitialized
/// sentinel. Already-compiled code keeps reading the slot and sees `undefined`
/// where the interpreter throws ReferenceError — the compile-time check is never
/// re-validated at entry.
#[test]
#[ignore = "open tier divergence — see PERF_ROADMAP B66"]
fn reading_a_deleted_implicit_global_still_throws() {
    let out = run_ok(&format!(
        r#"
        implicitG = 5;
        function read() {{ return implicitG; }}
        for (var i = 0; i < {HOT}; i++) read();
        delete globalThis.implicitG;
        var got;
        try {{ got = "value:" + read(); }} catch (e) {{ got = "throw:" + e.constructor.name; }}
        console.log(got);
        "#
    ));
    assert_eq!(out[0], "throw:ReferenceError");
}

/// Tier A's self-recursive call emits a direct `call` to its own entry with no
/// callee-identity guard, so it keeps calling itself after its global name has
/// been rebound to something else.
#[test]
#[ignore = "open tier divergence — see PERF_ROADMAP B66"]
fn self_recursion_rechecks_callee_identity() {
    let out = run_ok(&format!(
        r#"
        function fib(n) {{ if (n < 2) return n; return fib(n - 1) + fib(n - 2); }}
        var orig = fib;
        var w = 0;
        for (var i = 0; i < 60; i++) w = orig(18);
        fib = function (n) {{ return 0; }};
        console.log(w > 0 ? "after:" + orig(18) : "warmup-failed");
        "#
    ));
    // Every inner `fib(n-1)` re-resolves the (now rebound) global, so the whole
    // recursion collapses to the new function on the first hop.
    assert_eq!(out[0], "after:0");
}

/// `setPrototypeOf(Array.prototype, x)` is invisible to the
/// `array_proto_has_index` protector, so the JIT still invents absence for an
/// out-of-range index that the new prototype supplies.
#[test]
#[ignore = "open tier divergence — see PERF_ROADMAP B66"]
fn reprototyping_array_prototype_invalidates_the_index_protector() {
    let out = run_ok(&format!(
        r#"
        var a = [1, 2];
        function read(o, n) {{ var s; for (var i = 0; i < n; i++) s = o[5]; return s; }}
        read(a, {HOT});
        Object.setPrototypeOf(Array.prototype, {{ 5: "M5" }});
        console.log(read(a, {HOT}) + "," + (5 in a));
        "#
    ));
    assert_eq!(out[0], "M5,true");
}
