//! The slim per-call exec on the fused matchAll step (B124) must be
//! INVISIBLE.
//!
//! When `ITFB_FUSED` holds, each step runs `regexp_exec_fused_slim`: the
//! builtin exec minus the steps the creation proof already paid for (the
//! duplicate lastIndex read + ToInteger, the per-step flatten/is_ascii/
//! str_units heap.gets, the per-step twin probe, the result-array
//! empty-match probe) with lastIndex written straight to the heap slot.
//! The pristine (non-fused) exec's flag decode also becomes a single pass.
//! Everything those elisions could conceivably change — mid-iteration
//! mutation, the write-through the fallback path resumes from, statics,
//! the compact result record, zero-length advances, non-ASCII subjects
//! that must NOT take the slim path — is exercised here.
//!
//! Every `slimexec_parity_*` expectation below was executed in node
//! v24.12.0 (each block in its OWN process — several deliberately leave
//! the realm polluted) and diffs byte-identical. The whole set re-runs in
//! child processes with `ZIPP_NO_SLIM_EXEC=1`, `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Hot enough to cross the JIT thresholds and warm the slot memo before any
/// mutation lands.
const HOT: usize = 2000;

#[test]
fn slimexec_parity_rope_subject_flattens_at_creation() {
    // The subject is a CONS built hot immediately before matchAll — the
    // fused creation arm flattens it and proves ASCII; every step then
    // trusts flat-ness without a per-step flatten call.
    let out = run_ok(&format!(
        r#"
        var head = "k1=", tail = "9 k2=88 k3=777";
        var out = [];
        for (var j = 0; j < {HOT}; j++) {{
          var s = "";
          for (var i = 0; i < 3; i++) s = s + head;
          s = s + tail;
          var re = /(k\d)=(\d+)/g;
          out = [];
          for (var m of s.matchAll(re)) out.push(m[1] + ":" + m[2] + "@" + m.index);
          out.push("len=" + s.length);
          out.push("li=" + re.lastIndex);
        }}
        console.log("a=" + out.join(","));
        "#
    ));
    assert_eq!(out[0], "a=k1:9@6,k2:88@11,k3:777@17,len=23,li=0");
}

#[test]
fn slimexec_parity_astral_subject_stays_on_the_inner_path() {
    // Non-ASCII: never fused-eligible. Empty matches under `u` must advance
    // +2 over the astral pair; `.` consumes the whole pair.
    let out = run_ok(&format!(
        r#"
        var s = "a\u{{1F600}}bé";
        var out = [], out2 = [];
        for (var i = 0; i < {HOT}; i++) {{
          out = []; out2 = [];
          for (var m of s.matchAll(/(?:)/gu)) out.push(m.index);
          for (var m of s.matchAll(/./gu)) out2.push(m.index + ":" + m[0].length);
        }}
        console.log("b1=" + out.join(",") + "|" + out2.join(","));
        "#
    ));
    assert_eq!(out[0], "b1=0,1,3,4,5|0:1,1:2,3:1,4:1");
}

#[test]
fn slimexec_parity_lone_surrogate_subject() {
    let out = run_ok(&format!(
        r#"
        var s = "x" + String.fromCharCode(0xD800) + "y";
        var out = [], hex = [];
        for (var i = 0; i < {HOT}; i++) {{
          out = []; hex = [];
          for (var m of s.matchAll(/[xy]/g)) out.push(m.index);
          for (var m of s.matchAll(/./gu)) hex.push(m[0].charCodeAt(0).toString(16));
        }}
        console.log("b2=" + out.join(",") + "|" + hex.join(",") + "|" + s.length);
        "#
    ));
    assert_eq!(out[0], "b2=0,2|78,d800,79|3");
}

#[test]
fn slimexec_parity_source_lastindex_poked_mid_iteration_is_ignored() {
    // The matcher is a creation-time clone: writing the SOURCE regex's
    // lastIndex between next() calls is spec-invisible to the iteration.
    let out = run_ok(&format!(
        r#"
        var re = /n/g;
        var s = "nnn";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        var first = it.next().value.index;
        re.lastIndex = 99;
        var rest = [];
        for (var m of it) rest.push(m.index);
        console.log("c1=" + first + "|" + rest.join(",") + "|" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "c1=0|1,2|99");
}

#[test]
fn slimexec_parity_exec_swap_resumes_from_the_written_heap_lastindex() {
    // THE write-through proof: the slim path writes lastIndex straight to
    // the matcher's heap slot; a mid-iteration in-place
    // `RegExp.prototype.exec` swap (no version bump — B67) fails the memo
    // and the fallback must resume from exactly that slot. The replacement
    // logs `this.lastIndex` at every call to pin the continuation points.
    let out = run_ok(&format!(
        r#"
        var re = /p/g;
        var s = "ppqpp";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        it.next();
        var calls = [];
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function (str) {{ calls.push(this.lastIndex); return orig.call(this, str); }};
        var rest = [];
        for (var m of it) rest.push(m.index);
        RegExp.prototype.exec = orig;
        console.log("c2=" + rest.join(",") + "|" + calls.join(","));
        "#
    ));
    assert_eq!(out[0], "c2=1,3,4|1,2,4,5");
}

#[test]
fn slimexec_parity_result_mutation_then_reread() {
    // Element write, `index` write, delete, keys — the compact record must
    // materialise into the exact ordinary properties.
    let out = run_ok(&format!(
        r##"
        var re = /(q)(\d)/g;
        var s = "q1q2";
        var kept = [];
        for (var i = 0; i < {HOT}; i++) {{ kept = []; for (var m of s.matchAll(re)) kept.push(m); }}
        var m = kept[0];
        m[1] = "x";
        m.index = 9;
        var before = m[1] + "@" + m.index + "#" + m[2];
        delete m[1];
        var keys = Object.keys(m).join("+");
        var has = m.hasOwnProperty(1) + "/" + m.hasOwnProperty("index");
        console.log("d=" + before + "|" + keys + "|" + has + "|" + (1 in m) + "|" + m.length);
        "##
    ));
    assert_eq!(out[0], "d=x@9#1|0+2+index+input+groups|false/true|false|3");
}

#[test]
fn slimexec_parity_named_groups() {
    let out = run_ok(&format!(
        r#"
        var re = /(?<k>\w+)=(?<v>[-\d]+)/g;
        var s = "aa=-1 b=22";
        var out = [];
        for (var i = 0; i < {HOT}; i++) {{ out = []; for (var m of s.matchAll(re)) out.push(m.groups.k + ":" + m.groups.v); }}
        var last;
        for (var m of s.matchAll(re)) last = m;
        console.log("e=" + out.join(",") + "|" + (Object.getPrototypeOf(last.groups) === null) + "|" + Object.keys(last.groups).join("+") + "|" + JSON.stringify(last.groups));
        "#
    ));
    assert_eq!(out[0], r#"e=aa:-1,b:22|true|k+v|{"k":"b","v":"22"}"#);
}

#[test]
fn slimexec_parity_sticky_and_global_combos() {
    // `gy` stops at the first gap; a `y`-only (non-global) iterator via the
    // direct @@matchAll call yields one match then latches done; a flagless
    // regex the same. Source-regex lastIndex stays untouched in all three.
    let out = run_ok(&format!(
        r#"
        var out1 = [];
        var re1 = /f/gy;
        for (var i = 0; i < {HOT}; i++) {{ out1 = []; for (var m of "ffx ff".matchAll(re1)) out1.push(m.index); }}
        var out2 = [];
        var re2 = /a/y;
        for (var m of RegExp.prototype[Symbol.matchAll].call(re2, "aaa")) out2.push(m.index + ":" + m[0]);
        var out3 = [];
        for (var m of RegExp.prototype[Symbol.matchAll].call(/b/, "abcb")) out3.push(m.index);
        console.log("f=" + out1.join(",") + "|" + out2.join(",") + "|" + out3.join(",") + "|" + re1.lastIndex + "/" + re2.lastIndex);
        "#
    ));
    assert_eq!(out[0], "f=0,1|0:a|1|0/0");
}

#[test]
fn slimexec_parity_zero_length_matches_and_indices() {
    // /a*/g over "aaa": the empty match at 3 must terminate (the returned
    // empty-end drives the +1 advance the old path probed the array for).
    // `/d` exercises the indices build on the slim path.
    let out = run_ok(&format!(
        r#"
        var out = [];
        var re = /a*/g;
        for (var i = 0; i < {HOT}; i++) {{ out = []; for (var m of "aaa".matchAll(re)) out.push(m.index + ":" + m[0]); }}
        var di = [];
        for (var m of "x a bb".matchAll(/(\w+)/dg)) di.push(JSON.stringify(m.indices));
        console.log("g=" + out.join(",") + "|" + di.join(";"));
        "#
    ));
    assert_eq!(out[0], "g=0:aaa,3:|[[0,1],[0,1]];[[2,3],[2,3]];[[4,6],[4,6]]");
}

#[test]
fn slimexec_parity_results_escape_json_and_spread() {
    let out = run_ok(&format!(
        r##"
        var re = /([r-t])(\d)/g;
        var s = "r1s2t3";
        var kept = [];
        for (var i = 0; i < {HOT}; i++) {{ kept = []; for (var m of s.matchAll(re)) kept.push(m); }}
        var spread = [...kept[2]];
        console.log("h=" + kept.map(function (m) {{ return m[1] + m[2] + "@" + m.index + "#" + m.input; }}).join(",") + "|" + JSON.stringify(kept[1]) + "|" + spread.join("") + "|" + kept[0].length);
        "##
    ));
    assert_eq!(out[0], r#"h=r1@0#r1s2t3,s2@2#r1s2t3,t3@4#r1s2t3|["s2","s","2"]|t3t3|3"#);
}

#[test]
fn slimexec_parity_legacy_statics_after_a_completed_iteration() {
    // Annex-B statics refresh on EVERY successful builtin exec, slim steps
    // included — the deferred-range record must serve all of them.
    let out = run_ok(&format!(
        r#"
        var re = /(u)(\d)/g;
        var s = "xx u1 u2 yy";
        for (var i = 0; i < {HOT}; i++) for (var m of s.matchAll(re)) {{}}
        console.log("s=" + RegExp.$1 + RegExp.$2 + "|" + RegExp.lastMatch + "|" + RegExp.leftContext + "|" + RegExp.rightContext + "|" + RegExp["$+"]);
        "#
    ));
    assert_eq!(out[0], "s=u2|u2|xx u1 | yy|2");
}

#[test]
fn slimexec_parity_compile_inside_lastindex_valueof() {
    // The pristine exec's one-pass flag decode must sit AFTER ToLength: a
    // `lastIndex.valueOf` that calls `re.compile()` replaces the flags, and
    // the run must use what it left behind (here: `g` dropped).
    let out = run_ok(
        r#"
        var re = /a/g;
        re.exec("aa");
        var li = { valueOf: function () { re.compile("a", ""); return 0; } };
        re.lastIndex = li;
        var r = re.exec("bba");
        console.log("i=" + (r === null ? "null" : r.index + ":" + r[0]) + "|" + re.flags + "|" + re.lastIndex);
        "#,
    );
    assert_eq!(out[0], "i=2:a||0");
}

/// RXSTATS structural evidence: the hot steps are still served (and counted)
/// by the fused path with the slim entry in place. Runs in a child process
/// because the `ZIPP_RXSTATS` latch is read once per process and the
/// counters are process-global.
#[test]
fn the_slim_path_serves_and_counts_the_hot_steps() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .args(["slimexec_child_counts", "--exact", "--nocapture"])
        .env("ZIPP_RXSTATS", "1")
        .env("ZIPP_SLIMEXEC_CHILD", "1")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "child counts run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Child body for `the_slim_path_serves_and_counts_the_hot_steps` — a no-op
/// unless spawned with the env markers set.
#[test]
fn slimexec_child_counts() {
    if std::env::var_os("ZIPP_SLIMEXEC_CHILD").is_none() {
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
    let (compact, materialized, fused, fallback) = zipp_vm::regexp_result_stats();
    // 500 iterations x 5 steps (4 matches + the terminating null). The first
    // few can fall back while the slot memo warms; the overwhelming majority
    // must be fused — and the slim entry must keep feeding the SAME counters
    // (compact constructions, zero materialisations on a read-only workload).
    assert!(
        fused >= 2000,
        "expected the hot steps on the fused path: fused={fused} fallback={fallback}"
    );
    assert!(
        compact >= 2000 && materialized == 0,
        "expected compact-only results: compact={compact} materialized={materialized}"
    );
}

/// The env latches are read once per process, so each mode needs its own
/// child: re-run every `slimexec_parity_` expectation with the switch set.
/// The same node-derived assertions passing in every mode IS the parity
/// check (default vs `ZIPP_NO_SLIM_EXEC=1` in particular).
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_SLIM_EXEC", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("slimexec_parity_")
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
            "the slimexec_parity_ filter matched nothing:\n{stdout}"
        );
    }
}
