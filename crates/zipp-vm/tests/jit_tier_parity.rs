//! Tier parity: compiled code must answer exactly what the interpreter answers.
//!
//! Every case here was found by the audit B66 describes — a systematic sweep for
//! guards applied in SOME of the JIT's tiers and not others, prompted by three
//! bugs of that exact shape (B59, B63, B65). All of them are silent WRONG
//! ANSWERS at DEFAULT thresholds, not crashes, and all of them were invisible to
//! 95,936 test262 executions.
//!
//! B66 left three of the nine as `#[ignore]`d failing specs. All three are now
//! fixed and unignored, so `cargo test -- --ignored` reports nothing here. Two of
//! them turned out to be a tier divergence sitting on top of an INTERPRETER
//! conformance bug, and needed both halves:
//!
//! * `delete globalThis.implicitG` never cleared the global slot at all, so the
//!   interpreter answered `5` too. Only the unqualified `delete implicitG` cleared
//!   it, which is the pure tier case.
//! * a real own descriptor on the global object governed STORES but not LOADS, in
//!   both tiers — so a getter installed over a live binding never ran.
//!
//! The lesson B66 drew still holds: every one of these was a fact maintained by
//! hand in several tiers. Where a fix had a choice, it added ONE predicate the
//! gates share (`global_slot_directly_routable`,
//! `invalidate_indexed_proto_protector`) rather than a fourth copy.

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

// ────────────── was open in B66, closed in B67 ──────────────
// These three were `#[ignore]`d failing specs. See PERF_ROADMAP B67: two of them
// had an INTERPRETER conformance bug underneath the tier bug, so both halves had
// to be fixed — teaching the JIT to agree with the interpreter would have turned
// them green while leaving every answer wrong.

/// `delete` of an implicit global returns its slot to the uninitialized
/// sentinel. Already-compiled code kept reading the slot and seeing `undefined`
/// where the interpreter throws ReferenceError — the compile-time check was never
/// re-validated at entry.
///
/// Two bugs in one, which is why this spec used to fail for a reason B66 did not
/// predict: `delete globalThis.implicitG` never cleared the slot AT ALL, so the
/// INTERPRETER answered `5` too. The slot-clearing half is the conformance fix; the
/// global-route epoch checked at native entry is the tier fix. `delete implicitG`
/// (the unqualified spelling, which did clear the slot) is the pure tier case.
#[test]
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

/// The UNQUALIFIED spelling of the same deletion — the pure tier divergence, with no
/// interpreter bug in front of it. `delete implicitG` has always returned the slot to
/// the sentinel, so before the global-route epoch the interpreter threw and the
/// compiled `read` handed back the sentinel as `undefined`.
#[test]
fn reading_an_unqualified_deleted_global_still_throws() {
    let out = run_ok(&format!(
        r#"
        implicitU = 5;
        function read() {{ return implicitU; }}
        for (var i = 0; i < {HOT}; i++) read();
        delete implicitU;
        var got;
        try {{ got = "value:" + read(); }} catch (e) {{ got = "throw:" + e.constructor.name; }}
        console.log(got + " typeof=" + typeof implicitU);
        "#
    ));
    assert_eq!(out[0], "throw:ReferenceError typeof=undefined");
}

/// Re-creating the deleted binding must bring the compiled body back: the entry check
/// declines while the slot is dead and admits it again once it is live, rather than
/// blacklisting the function for the life of the process.
#[test]
fn a_recreated_global_is_readable_from_compiled_code_again() {
    let out = run_ok(&format!(
        r#"
        implicitR = 1;
        function read() {{ return implicitR; }}
        var a = 0; for (var i = 0; i < {HOT}; i++) a = read();
        delete implicitR;
        var mid; try {{ mid = "v" + read(); }} catch (e) {{ mid = "throw"; }}
        implicitR = 7;
        var b = 0; for (var j = 0; j < {HOT}; j++) b = read();
        console.log(a + "," + mid + "," + b);
        "#
    ));
    assert_eq!(out[0], "1,throw,7");
}

/// A REAL own property on the global object shadowing a live slot IS the binding: a
/// load runs its getter and a store its setter. `StoreGlobal` routed this; `LoadGlobal`
/// did not, in either tier — so the setter ran on every write while the read returned
/// the stale slot. Both halves must agree, and both tiers must agree with the
/// interpreter, INCLUDING when the descriptor appears after the loop is already
/// compiled.
#[test]
fn a_real_own_global_descriptor_governs_reads_and_writes() {
    let out = run_ok(&format!(
        r#"
        imp = 1;
        var seen = 0;
        function w(n) {{ var last; for (var i = 0; i < n; i++) {{ imp = i; last = imp; }} return last; }}
        w({HOT});
        Object.defineProperty(globalThis, "imp", {{
          set: function (v) {{ seen++; }}, get: function () {{ return "G"; }}, configurable: true
        }});
        var after = w({HOT});
        console.log("after=" + after + " seen=" + seen);
        "#
    ));
    // One setter call per write, and only the SECOND loop runs with the descriptor
    // installed — the first ran before `defineProperty` and wrote the raw slot.
    assert_eq!(out[0], format!("after=G seen={HOT}"));
}

/// Tier A's self-recursive call emits a direct `call` to its own entry with no
/// callee-identity guard, so it kept calling itself after its global name had
/// been rebound to something else. Fixed by recording the compile-time
/// self-binding on the `JitFn` and re-checking it at OUTER entry — declining
/// there makes the whole activation interpret, and the interpreter re-resolves
/// every inner call, so `fib`'s hot path pays nothing per recursion.
#[test]
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

/// `setPrototypeOf(Array.prototype, x)` was invisible to the
/// `array_proto_has_index` protector, so the JIT invented absence for an
/// out-of-range index that the new prototype supplies. Fixed by routing BOTH
/// invalidating mutations through `invalidate_indexed_proto_protector`.
#[test]
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
