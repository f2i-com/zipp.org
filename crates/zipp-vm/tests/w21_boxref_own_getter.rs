//! W21: an exact own getter (`get v() { return this.field; }`) may run as a
//! guarded prefix inside a DOUBLE/BOXREF region instead of forcing the whole
//! loop back to the memory tier.
//!
//! This is deliberately narrower than the ordinary accessor inliner. The
//! register tier may not run user code, and its r8..r11 / xmm2.. homes are live
//! across the GetProp. The bridge therefore accepts only the planner-validated
//! two-op body, re-emits its receiver/version/function guards with rax/rcx/rdx,
//! loads the baked DATA field, and dynamically numeric-guards the result. Every
//! miss falls through to the unchanged IC probe and accessor site gate.

use std::process::Command;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from node -e)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

/// Eight receivers (the IC-way limit), with the accessor in the last slot.
/// Four Bool homes and four numeric accumulators stay live across both the
/// seven ordinary probe paths and the matching own-getter prefix. Reusing r10
/// as guard scratch, or borrowing a typed-lane xmm without saving it, changes
/// the final line.
#[test]
fn boxref_own_getter_preserves_register_homes() {
    assert_matches_node(
        r#""use strict";
        var A = [];
        for (var q = 0; q < 7; q++) A.push({ pad: q, v: q * 3 + 1 });
        var acc = { hidden: 22, pad: 0 };
        Object.defineProperty(acc, "v", {
            get: function () { return this.hidden; }, configurable: true
        });
        A.push(acc);
        function kernel(n) {
            var h0 = 1.5, h1 = 2.5, h2 = 3.5, h3 = 4.5, t = 0;
            var b0 = false, b1 = false, b2 = false, b3 = false;
            for (var i = 0; i < n; i++) {
                b0 = i >= 4;
                b1 = i < 4;
                b2 = t > 2.5;
                b3 = t < 2.5;
                t = A[i & 7].v;
                h0 = h0 * 0.5 + t;
                h1 = h1 * 0.25 + t;
                h2 = h2 * 0.125 + t;
                h3 = h3 * 0.0625 + t;
            }
            return h0 + ":" + h1 + ":" + h2 + ":" + h3 + " " +
                (typeof b0) + ":" + b0 + " " + (typeof b1) + ":" + b1 + " " +
                (typeof b2) + ":" + b2 + " " + (typeof b3) + ":" + b3;
        }
        console.log(kernel(50000));
        "#,
    );
}

/// Each mutation happens between invocations of the hot inner loop, so the
/// mutation op itself cannot make the BOXREF region vacuous. Together these
/// cases pin every baked-address prerequisite plus the dynamic result guard.
#[test]
fn boxref_own_getter_guards_mutation_and_result_type() {
    let cases = [
        // __defineGetter__ merges into the existing descriptor WITHOUT a
        // version bump: only the live accessor-function re-read catches it.
        r#""use strict";
        var o = { hidden: 1 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o], s = 0;
        for (var outer = 0; outer < 120; outer++) {
            for (var i = 0; i < 300; i++) s = (s + A[0].v) | 0;
            if (outer === 59) o.__defineGetter__("v", function () { return 100; });
        }
        console.log(s);"#,
        // Data/accessor flips bump the receiver version. The replacement is
        // numeric so dropping that guard produces a stable wrong sum rather
        // than merely reaching the result-type bail.
        r#""use strict";
        var o = { hidden: 3 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o], s = 0;
        for (var outer = 0; outer < 120; outer++) {
            for (var i = 0; i < 300; i++) s = (s + A[0].v) | 0;
            if (outer === 59) Object.defineProperty(o, "v", { value: 77, writable: true, configurable: true });
        }
        console.log(s);"#,
        // Deleting an earlier key moves the baked field slot; `tail` is numeric
        // so a stale slot read also looks superficially type-correct.
        r#""use strict";
        var o = { pad: 1, hidden: 12, tail: 99 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o], s = 0;
        for (var outer = 0; outer < 120; outer++) {
            for (var i = 0; i < 300; i++) s = (s + A[0].v) | 0;
            if (outer === 59) { delete o.pad; o.hidden = 30; }
        }
        console.log(s);"#,
        // Force the receiver's vals Vec to grow after native code baked both
        // the accessor address and the field base.
        r#""use strict";
        var o = { hidden: 5 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o], s = 0;
        for (var outer = 0; outer < 120; outer++) {
            for (var i = 0; i < 300; i++) s = (s + A[0].v) | 0;
            if (outer === 59) { for (var k = 0; k < 80; k++) o["grow_" + k] = k; o.hidden = 20; }
        }
        console.log(s);"#,
        // A DATA-field value change does not bump the receiver version. Int and
        // double must both pass the dynamic guard; a heap string must fall back
        // before any bits are installed in the xmm home.
        r#""use strict";
        var o = { hidden: 2 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o], s = 0;
        for (var outer = 0; outer < 120; outer++) {
            for (var i = 0; i < 300; i++) s = (s + A[0].v) | 0;
            if (outer === 39) o.hidden = 2.5;
            if (outer === 79) o.hidden = "7";
        }
        console.log(s + ":" + typeof o.v + ":" + o.v);"#,
        // The BOXREF element changes identity while the accessor arm remains in
        // the code. Its receiver guard must miss and let the ordinary data way
        // answer for the replacement.
        r#""use strict";
        var o = { hidden: 4 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o], s = 0;
        for (var outer = 0; outer < 120; outer++) {
            for (var i = 0; i < 300; i++) s = (s + A[0].v) | 0;
            if (outer === 59) A[0] = { v: 55 };
        }
        console.log(s);"#,
        // Raw doubles with unusual bit patterns must take the numeric path,
        // not be confused with a NaN-box tag. The same compiled getter sees
        // -0, canonical NaN, then -0 again without a receiver-version bump.
        r#""use strict";
        var o = { hidden: -0 };
        Object.defineProperty(o, "v", { get: function () { return this.hidden; }, configurable: true });
        var A = [o];
        function hot(n) { var x = 1; for (var i = 0; i < n; i++) x = A[0].v; return 1 / x; }
        console.log(hot(30000));
        o.hidden = 0 / 0;
        console.log(hot(30000));
        o.hidden = -0;
        console.log(hot(30000));"#,
    ];
    for src in cases {
        assert_matches_node(src);
    }
}

/// An accessor with an observable body must never satisfy the exact-body gate.
/// This also keeps the pre-existing BOXREF rule honest: unsupported accessors
/// still reach the interpreter/memory tier and execute once per read.
#[test]
fn boxref_effectful_getter_stays_on_the_fallback() {
    assert_matches_node(
        r#""use strict";
        var hits = 0, A = [];
        for (var q = 0; q < 7; q++) A.push({ v: q + 1 });
        var o = { hidden: 9 };
        Object.defineProperty(o, "v", {
            get: function () { hits++; return this.hidden; }, configurable: true
        });
        A.push(o);
        var s = 0;
        for (var i = 0; i < 80000; i++) s = (s + A[i & 7].v) | 0;
        console.log(s + ":" + hits);
        "#,
    );
}

const MECHANISM_SRC: &str = r#""use strict";
    var A = [];
    for (var q = 0; q < 7; q++) A.push({ pad: q, v: q + 1 });
    var o = { hidden: 17, pad: 0 };
    Object.defineProperty(o, "v", {
        get: function () { return this.hidden; }, configurable: true
    });
    A.push(o);
    var s = 0;
    for (var i = 0; i < 100000; i++) s = (s + A[i & 7].v) | 0;
    console.log(s);
"#;

#[test]
fn boxref_own_getter_mechanism_child() {
    if std::env::var_os("ZIPP_BOXREF_GETTER_CHILD").is_none() {
        return;
    }
    assert_matches_node(MECHANISM_SRC);
}

/// Prove this is not a semantic-only test: default code must keep a BOXREF
/// region with one direct getter arm and avoid the accessor site-gate eviction.
/// The dedicated off switch must reproduce the old BOXREF -> gate -> MEM path.
#[test]
fn zz_boxref_own_getter_engages_and_switch_disengages() {
    if std::env::var_os("ZIPP_BOXREF_GETTER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let run = |off: bool| {
        let mut cmd = Command::new(&exe);
        cmd.args([
            "boxref_own_getter_mechanism_child",
            "--exact",
            "--nocapture",
        ])
        .env("ZIPP_BOXREF_GETTER_CHILD", "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_BOX_HOME")
        .env_remove("ZIPP_NO_METHOD_INLINE")
        .env_remove("ZIPP_NO_OWN_ACCESSOR_INLINE")
        .env_remove("ZIPP_ACC_ALWAYS_EMIT")
        .env_remove("ZIPP_NO_BOXREF_OWN_GETTER");
        if off {
            cmd.env("ZIPP_NO_BOXREF_OWN_GETTER", "1");
        }
        cmd.output().expect("spawn mechanism child")
    };

    let on = run(false);
    let on_stdout = String::from_utf8_lossy(&on.stdout);
    let on_log = String::from_utf8_lossy(&on.stderr);
    assert!(
        on.status.success() && !on_stdout.contains("running 0 tests"),
        "mechanism child failed:\n--- stdout ---\n{on_stdout}\n--- stderr ---\n{on_log}"
    );
    assert!(
        on_log.contains("BOXREF") && on_log.contains("own_getters=1"),
        "the direct own-getter BOXREF arm did not engage:\n{on_log}"
    );
    assert!(
        !on_log.contains("accessor site gate"),
        "the direct arm still reached the accessor site gate:\n{on_log}"
    );

    let off = run(true);
    let off_stdout = String::from_utf8_lossy(&off.stdout);
    let off_log = String::from_utf8_lossy(&off.stderr);
    assert!(
        off.status.success() && !off_stdout.contains("running 0 tests"),
        "off-switch child failed:\n--- stdout ---\n{off_stdout}\n--- stderr ---\n{off_log}"
    );
    assert!(
        off_log.contains("own_getters=0") && off_log.contains("accessor site gate"),
        "off switch did not reproduce the old gate path:\n{off_log}"
    );
}

/// All fallback configurations must remain byte-for-byte observable equivalents.
/// Each latch is process-wide, so every mode runs in its own child test process.
#[test]
fn zz_boxref_own_getter_modes_agree() {
    if std::env::var_os("ZIPP_BOXREF_GETTER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_BOXREF_OWN_GETTER", "1"),
        ("ZIPP_NO_BOX_HOME", "1"),
        ("ZIPP_NO_METHOD_INLINE", "1"),
        ("ZIPP_NO_OWN_ACCESSOR_INLINE", "1"),
        ("ZIPP_NO_ACCESSOR_WAY", "1"),
        ("ZIPP_ACC_ALWAYS_EMIT", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = Command::new(&exe)
            .args(["--skip", "zz_"])
            .env(key, val)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{key}={val} mode failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
