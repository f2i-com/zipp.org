//! A global that is an own PROPERTY of the global object — `this.x = 0`,
//! `globalThis.x = 0`, an eval-created var — must read correctly from
//! JIT-compiled code.
//!
//! Those bindings do not live in the `globals` slot array; the interpreter's
//! `LoadGlobal` finds the slot uninitialized and falls back to the global
//! object's own property. Compiled code cannot: Tier C emits
//! `mov rax, [r12 + idx*8]`, which reads the uninitialized sentinel, so `x++`
//! silently evaluated to `NaN` after the eighth call.
//!
//! The guard already existed in two of the three places that need it — the
//! region (loop) compiler's `region_globals_ok` and the leaf-inline planner —
//! and was missing from the whole-function Tier C path, which is why loops were
//! correct and plain hot functions were not. Same shape as B59's missing
//! `SuperBase` whitelist arm.
//!
//! Found by running test262 under `ZIPP_JIT_THRESHOLD=1`
//! (`language/types/object/S8.6.2_A5_T1.js` and `_T2`), which is the whole
//! reason that switch exists: the region JIT only compiles hot LOOPS, and
//! test262 asserts once, straight-line, so 95,936 executions never reach Tier C.
//!
//! See PERF_ROADMAP B65.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Well past `JIT_THRESHOLD` (8), so the function is compiled and the later
/// calls run native.
const HOT: usize = 50;

#[test]
fn this_dot_global_reads_correctly_from_compiled_code() {
    let out = run_ok(&format!(
        r#"
        this.count = 0;
        var o = {{ touch: function () {{ count++; }} }};
        for (var i = 0; i < {HOT}; i++) o.touch();
        console.log("count=" + count);
        "#
    ));
    assert_eq!(out[0], format!("count={HOT}"));
}

#[test]
fn global_this_assignment_reads_correctly_from_compiled_code() {
    let out = run_ok(&format!(
        r#"
        globalThis.g = 0;
        function bump() {{ g++; }}
        for (var i = 0; i < {HOT}; i++) bump();
        console.log("g=" + g);
        "#
    ));
    assert_eq!(out[0], format!("g={HOT}"));
}

/// A `var`-declared global DOES live in a slot and must keep working — this is
/// the case the fix must not have made slower or wrong.
#[test]
fn var_declared_global_is_unaffected() {
    let out = run_ok(&format!(
        r#"
        var v = 0;
        function bump() {{ v++; }}
        for (var i = 0; i < {HOT}; i++) bump();
        console.log("v=" + v);
        "#
    ));
    assert_eq!(out[0], format!("v={HOT}"));
}

/// Reads AND writes, mixed slot-backed and own-prop-backed in one function.
#[test]
fn mixed_slot_and_own_prop_globals_agree() {
    let out = run_ok(&format!(
        r#"
        var slotG = 0;
        this.propG = 0;
        function both() {{ slotG += 2; propG += 3; return slotG + propG; }}
        var last = 0;
        for (var i = 0; i < {HOT}; i++) last = both();
        console.log(slotG + "," + propG + "," + last);
        "#
    ));
    assert_eq!(out[0], format!("{},{},{}", HOT * 2, HOT * 3, HOT * 5));
}

/// The binding is created on the global object only AFTER the function is
/// already hot. Deferral must re-arm rather than blacklist, and the reads must
/// be right on both sides of the transition.
#[test]
fn a_global_that_appears_late_still_reads_correctly() {
    let out = run_ok(&format!(
        r#"
        function read() {{ return typeof late === "undefined" ? -1 : late; }}
        var before = 0;
        for (var i = 0; i < {HOT}; i++) before = read();
        this.late = 7;
        var after = 0;
        for (var i = 0; i < {HOT}; i++) after = read();
        console.log(before + "," + after);
        "#
    ));
    assert_eq!(out[0], "-1,7");
}

/// The exact test262 shape that found it: a plain call then a computed call.
#[test]
fn test262_s8_6_2_a5_shape() {
    let out = run_ok(&format!(
        r#"
        this.position = 0;
        var seat = {{}};
        seat['move'] = function () {{ position++; }};
        for (var i = 0; i < {HOT}; i++) seat.move();
        var mid = position;
        for (var i = 0; i < {HOT}; i++) seat['move']();
        console.log(mid + "," + position);
        "#
    ));
    assert_eq!(out[0], format!("{},{}", HOT, HOT * 2));
}
