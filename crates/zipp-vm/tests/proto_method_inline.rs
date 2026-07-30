//! The method inliner admits PROTOTYPE-CHAIN receivers (B78).
//!
//! `build_method_shape` used to resolve exactly two receiver shapes — an ES
//! class instance, and a plain object holding the function in an OWN slot — and
//! declined everything else, so `Object.create(proto)` and the classic
//! `Ctor.prototype.m = function …` both fell to the per-call helper on every
//! iteration. Measured on a 4M-call loop: **29.5ns/call at ONE receiver**
//! against 5.5ns for the identical method on a class, and 1.0ns in node. With
//! the arm: 5.25ns, i.e. −82%, and flat to the 8-arm cap exactly like the class
//! path.
//!
//! The arm's guards are the receiver's identity+version (which already covers an
//! own-property SHADOW and a re-pointed first proto link — `ordinary_set_
//! prototype_of` bumps the receiver's version), plus a version guard per chain
//! hop and a `holder_vals_ptr[slot] == fn_bits` check. Each test below perturbs
//! exactly one of those facts mid-loop.
//!
//! Every expectation was executed in node (as a SCRIPT, via
//! `vm.runInThisContext`) and diffs byte-identical, and each was additionally
//! confirmed identical under `ZIPP_NOJIT=1` and `ZIPP_JIT_THRESHOLD=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Hot enough to compile an OSR region and bake the arm.
const HOT: usize = 4000;

#[test]
fn an_inherited_method_returns_its_value_and_binds_this() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return this.x + 1; }} }};
        var o = Object.create(P); o.x = 41;
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = o.m();
        console.log(s);
        "#
    ));
    assert_eq!(out[0], "42");
}

#[test]
fn reassigning_the_method_on_the_prototype_is_observed() {
    // The holder-slot value guard: `P.m = other` overwrites an existing slot in
    // place, which deliberately does NOT bump P's version, so the hop guards
    // alone would keep running the OLD body.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return 1; }} }};
        var o = Object.create(P);
        var first = 0, last = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) P.m = function () {{ return 100; }};
          var r = o.m();
          if (i === 0) first = r;
          last = r;
        }}
        console.log(first + "," + last);
        "#
    ));
    assert_eq!(out[0], "1,100");
}

#[test]
fn an_own_shadow_added_mid_loop_wins() {
    // Covered by the RECEIVER's version guard: a key add bumps it.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return 1; }} }};
        var o = Object.create(P);
        var first = 0, last = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) o.m = function () {{ return 7; }};
          var r = o.m();
          if (i === 0) first = r;
          last = r;
        }}
        console.log(first + "," + last);
        "#
    ));
    assert_eq!(out[0], "1,7");
}

#[test]
fn repointing_the_receivers_prototype_is_observed() {
    // Also the receiver's version guard: `ordinary_set_prototype_of` bumps it,
    // which is the single reason this arm can guard the first chain link
    // WITHOUT re-reading `proto_of` (the interpreter's `ic_chain_ok` does).
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return 1; }} }};
        var Q = {{ m: function () {{ return 2; }} }};
        var o = Object.create(P);
        var first = 0, last = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) Object.setPrototypeOf(o, Q);
          var r = o.m();
          if (i === 0) first = r;
          last = r;
        }}
        console.log(first + "," + last);
        "#
    ));
    assert_eq!(out[0], "1,2");
}

#[test]
fn a_nearer_holder_gaining_the_name_mid_loop_wins() {
    // Two-hop chain A <- B <- o. `B.m = …` is a key ADD on a guarded hop, so
    // the hop version bumps and the arm misses.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var A = {{ m: function () {{ return 1; }} }};
        var B = Object.create(A);
        var o = Object.create(B);
        var first = 0, last = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) B.m = function () {{ return 5; }};
          var r = o.m();
          if (i === 0) first = r;
          last = r;
        }}
        console.log(first + "," + last);
        "#
    ));
    assert_eq!(out[0], "1,5");
}

#[test]
fn an_inherited_arrow_keeps_its_lexical_this() {
    // The arm must DECLINE an arrow. Inlining drops `HeapObj::Closure`'s
    // captured `this_val` and binds reg 0 to the receiver instead, so this
    // would print 999 (or 3) rather than 111 — the same silent wrong answer
    // the own-slot arm's `lexical_this` check exists to prevent.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var holder = {{ f: 111 }};
        var P = {{ f: 3, m: (function () {{ return () => this.f; }}).call(holder) }};
        var o = Object.create(P); o.f = 999;
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = o.m();
        console.log(s);
        "#
    ));
    assert_eq!(out[0], "111");
}

#[test]
fn an_inherited_getter_still_runs_on_every_call() {
    // `ic_walk` reports `ChainAcc` for an accessor, which the arm declines —
    // an inlined body would never run the getter at all.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var calls = 0;
        var P = {{}};
        Object.defineProperty(P, "m", {{
          get: function () {{ calls++; return function () {{ return 9; }}; }}
        }});
        var o = Object.create(P);
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = o.m();
        console.log(s + "," + calls);
        "#
    ));
    assert_eq!(out[0], format!("9,{HOT}"));
}

#[test]
fn arguments_flow_into_an_inherited_method() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function (a, b) {{ return (a * 10 + b + this.z) | 0; }} }};
        var o = Object.create(P); o.z = 3;
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = o.m(4, 5);
        console.log(s + "," + o.m(1));
        "#
    ));
    // `o.m(1)` leaves `b` undefined: (10 + NaN + 3) | 0 === 0.
    assert_eq!(out[0], "48,0");
}

#[test]
fn an_unrelated_key_added_to_the_prototype_does_not_change_the_answer() {
    // Bumps the holder's hop version → the arm misses and the helper
    // re-resolves. A miss must be slower, never wrong.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return 1; }} }};
        var o = Object.create(P);
        var last = 0;
        for (var i = 0; i < {HOT}; i++) {{
          if (i === {HOT} / 2) P["k" + i] = 1;
          last = o.m();
        }}
        console.log(last);
        "#
    ));
    assert_eq!(out[0], "1");
}

#[test]
fn deleting_the_method_from_the_prototype_throws() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return 1; }} }};
        var o = Object.create(P);
        var last = 0, err = "none";
        try {{
          for (var i = 0; i < {HOT}; i++) {{
            if (i === {HOT} / 2) delete P.m;
            last = o.m();
          }}
        }} catch (e) {{ err = e.constructor.name; }}
        console.log(last + "," + err);
        "#
    ));
    assert_eq!(out[0], "1,TypeError");
}

#[test]
fn twenty_receivers_over_one_prototype_stay_correct_past_the_arm_cap() {
    // Only 8 arms exist; receivers 9..20 fall to the helper every iteration.
    let out = run_ok(&format!(
        r#"
        "use strict";
        var P = {{ m: function () {{ return this.v | 0; }} }};
        var a = [];
        for (var i = 0; i < 20; i++) {{ var o = Object.create(P); o.v = i; a.push(o); }}
        var s = 0, k = 0;
        for (var i = 0; i < {HOT}; i++) {{ s = (s + a[k].m()) | 0; k++; if (k === 20) k = 0; }}
        console.log(s);
        "#
    ));
    // 4000 / 20 = 200 full cycles of sum(0..19) = 190.
    assert_eq!(out[0], "38000");
}

#[test]
fn a_constructor_prototype_method_is_inlined_the_same_way() {
    // The pre-ES6 shape, and what most transpiler output still looks like.
    let out = run_ok(&format!(
        r#"
        "use strict";
        function K(i) {{ this.v = i; }}
        K.prototype.m = function () {{ return this.v * 2; }};
        var k = new K(21);
        var s = 0;
        for (var i = 0; i < {HOT}; i++) s = k.m();
        console.log(s);
        "#
    ));
    assert_eq!(out[0], "42");
}

#[test]
fn a_sloppy_inherited_method_receives_the_receiver_as_this() {
    let out = run_ok(&format!(
        r#"
        var P = {{ m: function () {{ return typeof this; }} }};
        var o = Object.create(P);
        var s = "";
        for (var i = 0; i < {HOT}; i++) s = o.m();
        console.log(s);
        "#
    ));
    assert_eq!(out[0], "object");
}
