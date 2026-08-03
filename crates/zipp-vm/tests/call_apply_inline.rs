//! B82: inlining the TARGET of `f.call(…)` / `f.apply(…)` at a region
//! `CallMethod` site (`try_fn_call_apply_inline`), plus the pristine gate the
//! interpreter's name-dispatched `call`/`apply` arm now shares
//! (`fn_call_apply_pristine`) — an own `f.call` shadow or a monkey-patched
//! `Function.prototype.call`/`.apply` must resolve generically.
//!
//! Every expectation below was executed in node (v24) and diffs byte-identical.
//! The whole file must also pass with `ZIPP_NO_CALL_INLINE=1`, `ZIPP_NOJIT=1`
//! and `ZIPP_JIT_THRESHOLD=1`: the splice's guards are re-checked per call and
//! every decline falls to the unchanged generic path, so the output is
//! mode-independent by construction.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Well past JIT_THRESHOLD/OSR_THRESHOLD (both 8).
const HOT: usize = 3000;

#[test]
fn has_own_call_over_a_sparse_array_matches_node() {
    // The sparse-array bench phase's exact shape: a hoisted
    // `Object.prototype.hasOwnProperty` probed via `.call` in a hot loop.
    let out = run_ok(
        r#"
        "use strict";
        var hasOwn = Object.prototype.hasOwnProperty;
        var sp = [];
        sp.length = 5000000;
        for (var i = 0; i < 5000000; i += 1250) sp[i] = (i % 1000) + 1;
        var ownHits = 0, inHits = 0;
        for (var i = 0; i < 30000; i += 14) {
          if (i in sp) inHits++;
          if (hasOwn.call(sp, i + 1)) ownHits++;
        }
        console.log(inHits + "," + ownHits);
        "#,
    );
    assert_eq!(out, ["4,0"]);
}

#[test]
fn call_and_apply_arity_matrix() {
    // 0-3 forwarded args for `.call`; `.apply` with no argArray, null,
    // undefined, and 1-2 element array literals.
    let out = run_ok(&format!(
        r#"
        "use strict";
        function f0() {{ return 7; }}
        function f1(a) {{ return a + 1; }}
        function f2(a, b) {{ return a * 10 + b; }}
        function f3(a, b, c) {{ return a + b + c; }}
        var s = 0;
        for (var i = 0; i < {HOT}; i++) {{
          s += f0.call();
          s += f0.call(null);
          s += f1.call(null, i & 3);
          s += f2.call(null, i & 1, 2);
          s += f3.call(null, 1, 2, i & 7);
          s += f0.apply();
          s += f0.apply(null);
          s += f0.apply(null, null);
          s += f0.apply(null, undefined);
          s += f1.apply(null, [i & 3]);
          s += f2.apply(null, [i & 1, 2]);
        }}
        console.log(s);
        "#
    ));
    assert_eq!(out, ["202500"]);
}

#[test]
fn this_binding_sloppy_vs_strict_and_primitive_boxing() {
    // A SLOPPY target boxes a primitive `this` and substitutes the global for
    // a nullish one (the splice declines those to the frame call); a STRICT
    // target receives `this` exactly as passed.
    let out = run_ok(&format!(
        r#"
        function sloppyThis() {{ return typeof this; }}
        function sloppyIsGlobal() {{ return this === globalThis; }}
        var strictThis = (function () {{ "use strict"; return function () {{ return typeof this; }}; }})();
        var strictRaw = (function () {{ "use strict"; return function () {{ return this; }}; }})();
        var out = [];
        for (var i = 0; i < {HOT}; i++) {{
          out[0] = sloppyThis.call(5);
          out[1] = sloppyThis.call(null);
          out[2] = sloppyIsGlobal.call(undefined);
          out[3] = strictThis.call(5);
          out[4] = strictRaw.call(null);
          out[5] = strictThis.apply("x");
          out[6] = sloppyThis.apply("x");
        }}
        console.log(out.join("|"));
        "#
    ));
    // strictRaw.call(null) returns null -> "" in the join.
    assert_eq!(out, ["object|object|true|number||string|object"]);
}

#[test]
fn monkey_patched_prototype_call_falls_back_mid_loop() {
    let out = run_ok(
        r#"
        "use strict";
        function tgt(a) { return this.v + a; }
        var o = { v: 100 };
        var s = 0;
        for (var i = 0; i < 6000; i++) {
          if (i === 4000) {
            Function.prototype.call = function (t, x) { return -1; };
          }
          s += tgt.call(o, i & 3);
        }
        console.log(s);
        "#,
    );
    // 4000 real calls (this.v + i&3) then 2000 patched calls of -1 each.
    assert_eq!(out, ["404000"]);
}

#[test]
fn monkey_patched_prototype_apply_falls_back_mid_loop() {
    let out = run_ok(
        r#"
        "use strict";
        function f1(a) { return a + 1; }
        var s = 0;
        for (var i = 0; i < 6000; i++) {
          if (i === 4000) Function.prototype.apply = function () { return 500; };
          s += f1.apply(null, [i & 3]);
        }
        console.log(s);
        "#,
    );
    assert_eq!(out, ["1010000"]);
}

#[test]
fn own_call_shadow_on_the_function_wins() {
    let out = run_ok(
        r#"
        "use strict";
        function tgt() { return 1; }
        tgt.call = function () { return 42; };
        var s = 0;
        for (var i = 0; i < 6000; i++) s += tgt.call(null);
        console.log(s);
        "#,
    );
    assert_eq!(out, ["252000"]);
}

#[test]
fn target_rebound_mid_loop_switches_bodies() {
    let out = run_ok(
        r#"
        "use strict";
        function a(x) { return x + 1; }
        function b(x) { return x * 2; }
        var f = a, s = 0;
        for (var i = 0; i < 6000; i++) {
          if (i === 3000) f = b;
          s += f.call(null, i & 3);
        }
        console.log(s);
        "#,
    );
    assert_eq!(out, ["16500"]);
}

#[test]
fn apply_observes_array_mutations_between_calls() {
    let out = run_ok(
        r#"
        "use strict";
        function f2(a, b) { return a * 10 + b; }
        var args = [0, 0];
        var seenNaN = 0;
        var s = 0;
        for (var i = 0; i < 6000; i++) {
          if (i <= 3000) { args[0] = i & 3; args[1] = i & 1; }
          var r = f2.apply(null, args);
          if (r !== r) { seenNaN++; r = 0; }
          s += r;
          if (i === 3000) args.length = 1;
          if (i === 3001) args = [4, 5];
        }
        console.log(s + "|" + seenNaN);
        "#,
    );
    assert_eq!(out, ["181410|1"]);
}

#[test]
fn bound_function_as_target_keeps_its_bound_this() {
    // A Bound target declines the splice (`ic_plain_fn` refuses it); the
    // generic path must still ignore the `.call` thisArg — node-identical.
    let out = run_ok(
        r#"
        "use strict";
        function tgt(a) { return this.v + a; }
        var o1 = { v: 1 }, o2 = { v: 1000 };
        var bfn = tgt.bind(o1);
        var s = 0;
        for (var i = 0; i < 6000; i++) s += bfn.call(o2, i & 1);
        console.log(s);
        "#,
    );
    assert_eq!(out, ["9000"]);
}

#[test]
fn arrow_target_ignores_the_supplied_this() {
    let out = run_ok(
        r#"
        var mk = { v: 9, get: function () { var a = (x) => this.v + x; return a; } };
        var arrow = mk.get();
        var other = { v: 55 };
        var s = 0;
        for (var i = 0; i < 6000; i++) s += arrow.call(other, 1);
        console.log(s);
        "#,
    );
    assert_eq!(out, ["60000"]);
}

#[test]
fn throw_from_the_target_propagates_each_iteration() {
    let out = run_ok(
        r#"
        "use strict";
        function boom(a) { if (a === 3) throw new Error("kaboom " + a); return a; }
        var s = 0, caught = 0;
        for (var i = 0; i < 6000; i++) {
          try { s += boom.call(null, i & 3); }
          catch (e) { caught++; s += e.message === "kaboom 3" ? 100 : -1; }
        }
        console.log(s + "|" + caught);
        "#,
    );
    assert_eq!(out, ["154500|1500"]);
}

#[test]
fn hoisted_apply_array_reused_across_iterations() {
    // The polyfill shape the splice serves: one array object, mutated in
    // place, applied every iteration.
    let out = run_ok(&format!(
        r#"
        "use strict";
        function f2(a, b) {{ return a * 10 + b; }}
        var args = [0, 5];
        var s = 0;
        for (var i = 0; i < {HOT}; i++) {{
          args[0] = i & 1;
          s += f2.apply(null, args);
        }}
        console.log(s);
        "#
    ));
    assert_eq!(out, ["30000"]);
}
