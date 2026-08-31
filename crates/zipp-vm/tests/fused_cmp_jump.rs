//! A bare `<` / `<=` branch test compiles to the fused `JumpIfNotLt` /
//! `JumpIfNotLe` instead of the `Lt`/`Le` + `JumpIfFalse` pair.
//!
//! The fused ops existed (interpreter arm and every JIT tier) but were never
//! emitted. `emit_test_jump` now fuses at the AST level — `if`/`while`/`for`
//! tests, conditional-expression tests and the for-in index guard — ONLY when
//! the whole test is the bare binary, so the boolean is a freshly allocated
//! temp nothing else can observe. The fused interpreter arm runs the same
//! `cmp_lt`/`cmp_le` as the unfused pair, so operand evaluation, ToPrimitive
//! order and NaN behaviour are identical by construction; these tests pin
//! that down.
//!
//! Every expectation below was executed in node (v24) as a script and diffs
//! byte-identical. The whole file must also pass with `ZIPP_NO_FUSED_CMPJUMP=1`
//! (the unfused pair): the runtime cases assert identical output either way,
//! and the bytecode-shape assertions read the switch themselves.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The compiled bytecode's canonical text (`compile_to_text`), for asserting
/// which comparison shape was emitted without running anything.
fn text_of(src: &str) -> String {
    zipp_vm::compile_to_text(src, false).expect("source compiles")
}

/// Mirrors the compiler's switch: fusion is ON unless the env var is set.
fn fused_mode() -> bool {
    std::env::var_os("ZIPP_NO_FUSED_CMPJUMP").is_none()
}

#[test]
fn int_compares_drive_branches() {
    let out = run_ok(
        r#"
        if (1 < 2) console.log("lt-taken"); else console.log("x");
        if (2 < 1) console.log("x"); else console.log("lt-false");
        if (2 <= 2) console.log("le-taken"); else console.log("x");
        if (3 <= 2) console.log("x"); else console.log("le-false");
        if (-1 < 0) console.log("neg"); else console.log("x");
        "#,
    );
    assert_eq!(out, ["lt-taken", "lt-false", "le-taken", "le-false", "neg"]);
}

#[test]
fn double_and_nan_compares_drive_branches() {
    // NaN makes every ordered comparison FALSE — the not-taken branch runs.
    let out = run_ok(
        r#"
        if (0.1 < 0.2) console.log("d1"); else console.log("x");
        if (-0 < 0) console.log("x"); else console.log("d2");
        if (0 <= -0) console.log("d3"); else console.log("x");
        if (1.5 <= 1.5) console.log("d4"); else console.log("x");
        var n = NaN;
        if (n < 1) console.log("x"); else console.log("nan1");
        if (1 < n) console.log("x"); else console.log("nan2");
        if (n <= n) console.log("x"); else console.log("nan3");
        while (n < 5) { console.log("never"); break; }
        console.log("done");
        "#,
    );
    assert_eq!(
        out,
        ["d1", "d2", "d3", "d4", "nan1", "nan2", "nan3", "done"]
    );
}

#[test]
fn string_compares_drive_branches() {
    // Both sides strings → code-unit comparison, no numeric coercion
    // ("10" < "9" is TRUE lexicographically).
    let out = run_ok(
        r#"
        if ("a" < "b") console.log("s1"); else console.log("x");
        if ("b" <= "a") console.log("x"); else console.log("s2");
        if ("apple" < "apples") console.log("s3"); else console.log("x");
        if ("" <= "") console.log("s4"); else console.log("x");
        if ("10" < "9") console.log("s5"); else console.log("x");
        "#,
    );
    assert_eq!(out, ["s1", "s2", "s3", "s4", "s5"]);
}

#[test]
fn mixed_string_number_coercion() {
    // One non-string side → ToNumber on both ("10" < 9 is 10 < 9, FALSE;
    // "abc" coerces to NaN → FALSE; null → 0; undefined → NaN; true → 1).
    let out = run_ok(
        r#"
        if ("10" < 9) console.log("x"); else console.log("m1");
        if (9 < "10") console.log("m2"); else console.log("x");
        if ("abc" < 5) console.log("x"); else console.log("m3");
        if (5 <= "5") console.log("m4"); else console.log("x");
        if (null < 1) console.log("m5"); else console.log("x");
        if (undefined < 1) console.log("x"); else console.log("m6");
        if (true < 2) console.log("m7"); else console.log("x");
        "#,
    );
    assert_eq!(out, ["m1", "m2", "m3", "m4", "m5", "m6", "m7"]);
}

#[test]
fn valueof_side_effects_run_exactly_once_left_first() {
    // ToPrimitive order: `<` and `<=` both coerce the SOURCE-LEFT operand
    // first, exactly once per evaluation of the comparison.
    let out = run_ok(
        r#"
        var log = [];
        var a = { valueOf: function () { log.push("A"); return 1; } };
        var b = { valueOf: function () { log.push("B"); return 2; } };
        if (a < b) log.push("taken"); else log.push("not");
        console.log(log.join(","));
        log = [];
        if (b <= a) log.push("taken"); else log.push("not");
        console.log(log.join(","));
        "#,
    );
    assert_eq!(out, ["A,B,taken", "B,A,not"]);
}

#[test]
fn valueof_runs_once_per_loop_iteration() {
    // A while guard re-coerces its operands each test: 4 tests for 3
    // iterations, and the for-loop interleaving is V,i0,V,i1,V.
    let out = run_ok(
        r#"
        var calls = 0;
        var i = 0;
        var lim = { valueOf: function () { calls++; return 3; } };
        while (i < lim) { i++; }
        console.log(i, calls);
        var log2 = [];
        var m = { valueOf: function () { log2.push("V"); return 2; } };
        for (var q = 0; q < m; q++) log2.push("i" + q);
        console.log(log2.join(","));
        "#,
    );
    assert_eq!(out, ["3 4", "V,i0,V,i1,V"]);
}

#[test]
fn for_loops_iterate_the_same() {
    let out = run_ok(
        r#"
        for (let i = 0; i < 3; i++) console.log("iter", i);
        console.log("end");
        var t = [];
        for (var j = 0; j <= 3; j++) t.push(j);
        console.log(t.join(","));
        "#,
    );
    assert_eq!(out, ["iter 0", "iter 1", "iter 2", "end", "0,1,2,3"]);
}

#[test]
fn for_in_still_visits_every_key() {
    // The for-in index guard over the key snapshot is a fused site too.
    let out = run_ok(
        r#"
        var o = { a: 1, b: 2, c: 3 };
        var ks = [];
        for (var k in o) ks.push(k);
        console.log(ks.join(","));
        "#,
    );
    assert_eq!(out, ["a,b,c"]);
}

// ── MEM-tier fused guards ── each loop body carries a `%` (Mod is a
// regalloc-emit-unhandled op, so the OSR region falls to the MEM tier — the
// B113 typedarray-math shape) and runs 3000 iterations, far past the OSR
// threshold. The guard is the fused `JumpIfNotLt`, exercised over doubles
// (native in the MEM arm), strings, and mixed number/string (both bail to the
// interpreter's full coercion — from the OLD pair, the fused form, and the
// pair-shape MEM lowering alike). Expectations executed in node v24; the file
// also passes with `ZIPP_MEM_FUSED_DJUMP=1`, `ZIPP_NO_FUSED_CMPJUMP=1` and
// `ZIPP_NOJIT=1`, byte-identical.

#[test]
fn mem_tier_fused_guard_over_doubles() {
    let out = run_ok(
        r#"
        var s = 0;
        for (var i = 0.5; i < 3000; i++) { s = s + (i % 7); }
        console.log(s, i);
        "#,
    );
    assert_eq!(out, ["10494 3000.5"]);
}

#[test]
fn mem_tier_fused_guard_over_strings() {
    // Both guard operands are strings → code-unit comparison ("0999" < "3000"
    // lexicographically tracks the numeric order here because the keys are
    // zero-padded to equal length).
    let out = run_ok(
        r#"
        var i = 0;
        var s = 0;
        var key = "0000";
        while (key < "3000") {
            s = s + (i % 7);
            i = i + 1;
            key = ("0000" + i).slice(-4);
        }
        console.log(i, s, key);
        "#,
    );
    assert_eq!(out, ["3000 8994 3000"]);
}

#[test]
fn mem_tier_fused_guard_mixed_number_string() {
    // One non-string side → ToNumber on both: `i < "3000"` is `i < 3000`.
    let out = run_ok(
        r#"
        var i = 0;
        var s = "";
        while (i < "3000") { s = s + (i % 7); i = i + 1; }
        console.log(i, s.length, s.slice(0, 10), s.slice(-10));
        "#,
    );
    assert_eq!(out, ["3000 3000 0123456012 1234560123"]);
}

#[test]
fn comparison_used_as_a_value_stays_correct() {
    // The result register is OBSERVED (stored, pushed, compared), so these
    // must not fuse — and must stay correct either way.
    let out = run_ok(
        r#"
        var x = 1, y = 2;
        var r = x < y;
        console.log(r, typeof r);
        var s = (y <= x);
        console.log(s);
        var arr = [];
        for (var i = 0; i < 5; i++) arr.push(i < 2);
        console.log(arr.join(","));
        console.log((x < y) === true);
        if (x < y && r) console.log("and-taken"); else console.log("x");
        "#,
    );
    assert_eq!(
        out,
        [
            "true boolean",
            "false",
            "true,true,false,false,false",
            "true",
            "and-taken"
        ]
    );
}

#[test]
fn bytecode_shape_branch_tests_fuse_value_uses_do_not() {
    // A bare `<` branch test: fused op iff the switch is on; the unfused pair
    // exactly when it is off.
    let t = text_of("function f(a, b) { if (a < b) return a; return b; }");
    if fused_mode() {
        assert!(t.contains("JumpIfNotLt"), "branch `<` should fuse:\n{t}");
        assert!(
            !t.contains("JumpIfFalse"),
            "fused test leaves no JumpIfFalse:\n{t}"
        );
    } else {
        assert!(!t.contains("JumpIfNotLt"), "switch off, no fused op:\n{t}");
        assert!(t.contains("JumpIfFalse"), "unfused pair expected:\n{t}");
    }

    // `<=` in a for-loop head.
    let t = text_of("function g(n) { var s = 0; for (var i = 0; i <= n; i++) s += i; return s; }");
    assert_eq!(
        t.contains("JumpIfNotLe"),
        fused_mode(),
        "for `<=` guard:\n{t}"
    );

    // A conditional expression also consumes the test only as control flow.
    // This is the recursive-fibonacci shape and must share the same fusion.
    let t = text_of("function c(n) { return n < 2 ? n : c(n - 1) + c(n - 2); }");
    assert_eq!(
        t.contains("JumpIfNotLt"),
        fused_mode(),
        "conditional-expression `<` guard:\n{t}"
    );
    if fused_mode() {
        assert!(
            !t.contains("JumpIfFalse"),
            "fused conditional test leaves no JumpIfFalse:\n{t}"
        );
    }

    // The for-in index guard.
    let t = text_of("function h(o) { var n = 0; for (var k in o) n++; return n; }");
    assert_eq!(
        t.contains("JumpIfNotLt"),
        fused_mode(),
        "for-in guard:\n{t}"
    );

    // A comparison whose result is ALSO a value never fuses, in either mode.
    let t = text_of("function v(a, b) { var r = a < b; return r; }");
    assert!(!t.contains("JumpIfNotLt"), "value use must not fuse:\n{t}");

    // `>` / `>=` never fuse (no operand-swap flag on the fused ops).
    let t = text_of("function w(a, b) { if (a > b) return a; if (a >= b) return b; return 0; }");
    assert!(!t.contains("JumpIfNot"), "`>`/`>=` must not fuse:\n{t}");
}
