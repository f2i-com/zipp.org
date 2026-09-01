//! The fused pristine matchAll STEP (B118) must be INVISIBLE.
//!
//! When the iterator was created through the pristine dispatch over a global
//! regex on a flat-ASCII subject, each `next()` runs the builtin exec with
//! flag bits cached on the iterator record instead of re-proving the whole
//! exec protocol — guarded per step only by the version-pinned slot memo
//! (`matchall_fast_from_slots`), whose `exec` pin re-reads the slot's value
//! identity, catching the no-version-bump in-place
//! `RegExp.prototype.exec = f` write (B67). Everything that can pollute the
//! protocol MID-ITERATION, after a hot warm-up, is exercised here.
//!
//! Every `mastep_parity_*` expectation below was executed in node v24.12.0
//! (each block in its OWN process — several deliberately leave the realm
//! polluted) and diffs byte-identical. The whole set re-runs in child
//! processes with `ZIPP_NO_MATCHALL_STEP=1`, `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1`.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Hot enough to cross the JIT thresholds and warm the slot memo before any
/// mutation lands.
const HOT: usize = 2000;

#[test]
fn mastep_parity_hot_loop_with_captures() {
    let out = run_ok(&format!(
        r#"
        var re = /([a-z]+)-(\d+)/g;
        var s = "aa-1 bbb-22 c-333 dd-4";
        var total = 0, caps = "";
        for (var i = 0; i < {HOT}; i++) {{
          var n = 0;
          for (var m of s.matchAll(re)) {{
            n++;
            if (i === 0) caps += m[1] + ":" + m[2] + "@" + m.index + " ";
          }}
          total += n;
        }}
        console.log("t1=" + (total / {HOT}) + " " + caps.trim() + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "t1=4 aa:1@0 bbb:22@5 c:333@12 dd:4@18 li=0");
}

#[test]
fn mastep_parity_exec_replaced_between_steps_of_an_inflight_iterator() {
    // The in-place data write bumps no version; the slot memo's value-identity
    // pin must catch it on the NEXT step: 2 remaining matches + the final
    // null = 3 calls through the replacement.
    let out = run_ok(&format!(
        r#"
        var re = /a/g;
        var s = "aaa";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        var first = it.next().value[0];
        var calls = 0;
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function (str) {{ calls++; return orig.call(this, str); }};
        var rest = 0;
        for (var m of it) rest++;
        RegExp.prototype.exec = orig;
        console.log("t2=" + first + "/" + rest + "/" + calls);
        "#
    ));
    assert_eq!(out[0], "t2=a/2/3");
}

#[test]
fn mastep_parity_a_lying_null_exec_ends_the_iteration() {
    let out = run_ok(&format!(
        r#"
        var re = /b/g;
        var s = "bbbb";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        it.next();
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function () {{ return null; }};
        var r = it.next();
        RegExp.prototype.exec = orig;
        console.log("t3=" + r.done + "/" + it.next().done);
        "#
    ));
    assert_eq!(out[0], "t3=true/true");
}

#[test]
fn mastep_parity_lastindex_poked_on_the_source_regex_has_no_effect() {
    // The matcher is a creation-time clone: mutating the SOURCE regex between
    // steps is spec-invisible to the iteration.
    let out = run_ok(&format!(
        r#"
        var re = /c/g;
        var s = "ccc";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        it.next();
        re.lastIndex = 999;
        var n = 0;
        for (var m of it) n++;
        console.log("t4=" + n + "/" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "t4=2/999");
}

#[test]
fn mastep_parity_flags_accessor_patched_mid_iteration_is_never_read_per_step() {
    // Per spec `flags` is consumed at CREATION (CreateRegExpStringIterator
    // captures global/fullUnicode); a step never reads it. The proto version
    // bump also invalidates the slot memo — the fallback path must agree.
    let out = run_ok(&format!(
        r#"
        var re = /d/g;
        var s = "dd";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        it.next();
        var reads = 0;
        Object.defineProperty(RegExp.prototype, "flags", {{
          get: function () {{ reads++; return "g"; }},
          configurable: true
        }});
        var n = 0;
        for (var m of it) n++;
        console.log("t5=" + n + "/" + reads);
        "#
    ));
    assert_eq!(out[0], "t5=1/0");
}

#[test]
fn mastep_parity_empty_match_advance_ascii() {
    let out = run_ok(&format!(
        r#"
        var re = /e*/g;
        var s = "xeexe";
        var out = [];
        for (var i = 0; i < {HOT}; i++) {{
          out = [];
          for (var m of s.matchAll(re)) out.push(m.index + ":" + m[0]);
        }}
        console.log("t6=" + out.join(",") + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "t6=0:,1:ee,3:,4:e,5: li=0");
}

#[test]
fn mastep_parity_sticky_global_stops_at_the_first_gap() {
    let out = run_ok(&format!(
        r#"
        var re = /f/gy;
        var s = "ffx ff";
        var out = [];
        for (var i = 0; i < {HOT}; i++) {{
          out = [];
          for (var m of s.matchAll(re)) out.push(m.index);
        }}
        console.log("t7=" + out.join(",") + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "t7=0,1 li=0");
}

#[test]
fn mastep_parity_unicode_empty_matches_skip_the_low_surrogate() {
    // Non-ASCII subject: never fused-eligible; AdvanceStringIndex must still
    // step by 2 over the astral pair in `u` mode.
    let out = run_ok(
        r#"
        var re = /(?:)/gu;
        var s = "a\u{1F600}b";
        var out = [];
        for (var m of s.matchAll(re)) out.push(m.index);
        console.log("t8=" + out.join(","));
        "#,
    );
    assert_eq!(out[0], "t8=0,1,3,4");
}

#[test]
fn mastep_parity_result_objects_escape_and_stay_readable() {
    // The per-step result arrays are stored, re-read after the loop, spread,
    // and JSON-stringified — everything the compact record must survive.
    let out = run_ok(&format!(
        r##"
        var re = /([g-i])(\d)/g;
        var s = "g1h2i3";
        var kept = [];
        for (var i = 0; i < {HOT}; i++) {{
          kept = [];
          for (var m of s.matchAll(re)) kept.push(m);
        }}
        var spread = [...s.matchAll(re)];
        console.log(
          "t9=" + kept.map(function (m) {{ return m[1] + m[2] + "@" + m.index + "#" + m.input; }}).join(",") +
          "|" + spread.length + "|" + JSON.stringify(kept[1])
        );
        "##
    ));
    assert_eq!(
        out[0],
        r#"t9=g1@0#g1h2i3,h2@2#g1h2i3,i3@4#g1h2i3|3|["h2","h","2"]"#
    );
}

#[test]
fn mastep_parity_freezing_the_prototype_mid_iteration() {
    // Object.freeze(RegExp.prototype) runs DefinePropertyOrThrow over every
    // key (bumping the proto version — memo invalid, fallback path). The
    // matcher's own lastIndex is unaffected: iteration completes.
    let out = run_ok(&format!(
        r#"
        var re = /j/g;
        var s = "jj";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        it.next();
        Object.freeze(RegExp.prototype);
        var n = 0;
        for (var m of it) n++;
        console.log("t10=" + n);
        "#
    ));
    assert_eq!(out[0], "t10=1");
}

#[test]
fn mastep_parity_legacy_statics_refresh_per_step() {
    // Annex B statics are refreshed by every successful builtin exec — the
    // fused step's deferred-range record must serve them per step.
    let out = run_ok(&format!(
        r#"
        var re = /(k)(\d)/g;
        var s = "k1 k2";
        var seen = [];
        for (var i = 0; i < {HOT}; i++) {{
          seen = [];
          for (var m of s.matchAll(re)) seen.push(RegExp.$2 + RegExp.lastMatch);
        }}
        console.log("t11=" + seen.join(","));
        "#
    ));
    assert_eq!(out[0], "t11=1k1,2k2");
}

/// RXSTATS evidence: a hot matchAll loop's steps are served by the fused
/// path. Runs in a child process because the `ZIPP_RXSTATS` latch is read
/// once per process and the counters are process-global.
#[test]
fn the_fused_path_serves_the_hot_steps() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .args(["mastep_child_counts", "--exact", "--nocapture"])
        .env("ZIPP_RXSTATS", "1")
        .env("ZIPP_MASTEP_CHILD", "1")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "child counts run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Child body for `the_fused_path_serves_the_hot_steps` — a no-op unless
/// spawned with the env markers set.
#[test]
fn mastep_child_counts() {
    if std::env::var_os("ZIPP_MASTEP_CHILD").is_none() {
        return;
    }
    let out = run_ok(
        r#"
        var re = /([a-z]+)-(\d+)/g;
        var s = "aa-1 bbb-22 c-333 dd-4";
        var total = 0;
        for (var i = 0; i < 500; i++) {
          for (var m of s.matchAll(re)) total += m[2].length;
        }
        console.log(total);
        "#,
    );
    assert_eq!(out[0], "3500");
    let (_, _, fused, fallback) = zipp_vm::regexp_result_stats();
    // 500 iterations x 5 steps (4 matches + the terminating null). The first
    // few can fall back while the slot memo warms; the overwhelming majority
    // must be fused.
    assert!(
        fused >= 2000,
        "expected the hot steps on the fused path: fused={fused} fallback={fallback}"
    );
}

/// The env latches are read once per process, so each mode needs its own
/// child: re-run every `mastep_parity_` expectation with the switch set. The
/// same node-derived assertions passing in every mode IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_MATCHALL_STEP", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("mastep_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the mastep_parity_ filter matched nothing:\n{stdout}"
        );
    }
}
