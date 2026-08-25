//! The fused matchAll DRAIN batch (W12) must be INVISIBLE.
//!
//! When a fused (`ITFB_FUSED`) iterator's step is batch-eligible, the FIRST
//! step drains up to 16 matches in one host-side scan and later steps publish
//! from the stored triples — but everything observable (`lastIndex` writes,
//! the Annex-B statics, the result arrays, a swapped `RegExp.prototype.exec`
//! honoured per step) must stay exactly per-step. Every way the memoized scan
//! could desynchronize from the live protocol — a mid-iteration exec swap
//! that moves the matcher's lastIndex, one that does NOT move it, empty-match
//! advances, cap-boundary re-drains, break-early leftovers, manual `next()`
//! interleaving — is exercised here.
//!
//! Every `mabatch_parity_` expectation below was executed in node v24 (each
//! block in its OWN process — several deliberately leave the realm polluted)
//! and diffs byte-identical. The whole set re-runs in child processes with
//! `ZIPP_NO_MATCHALL_BATCH=1` (the off-switch must reproduce the same bytes),
//! `ZIPP_NO_SLIM_EXEC=1`, `ZIPP_NO_MATCHALL_STEP=1`, `ZIPP_NO_RX_SCANSESSION=1`,
//! `ZIPP_NOJIT=1` and `ZIPP_GC_STRESS=1`.

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
fn mabatch_parity_forty_matches_cross_the_cap_boundary() {
    // 40 matches per subject = 16 + 16 + 8: two cap-boundary re-drains per
    // iteration plus the exhausted-tail done step.
    let out = run_ok(&format!(
        r#"
        var re = /([a-z]+)-(\d+)/g;
        var parts = [];
        for (var i = 0; i < 40; i++) parts.push(String.fromCharCode(97 + (i % 26)) + "q-" + i);
        var s = parts.join(" ");
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
    assert_eq!(
        out[0],
        "t1=40 aq:0@0 bq:1@5 cq:2@10 dq:3@15 eq:4@20 fq:5@25 gq:6@30 hq:7@35 iq:8@40 jq:9@45 \
         kq:10@50 lq:11@56 mq:12@62 nq:13@68 oq:14@74 pq:15@80 qq:16@86 rq:17@92 sq:18@98 \
         tq:19@104 uq:20@110 vq:21@116 wq:22@122 xq:23@128 yq:24@134 zq:25@140 aq:26@146 \
         bq:27@152 cq:28@158 dq:29@164 eq:30@170 fq:31@176 gq:32@182 hq:33@188 iq:34@194 \
         jq:35@200 kq:36@206 lq:37@212 mq:38@218 nq:39@224 li=0"
    );
}

#[test]
fn mabatch_parity_exec_swapped_to_a_logger_and_restored_mid_iteration() {
    // Two batched steps, then a logging exec (which calls the intrinsic, so
    // the matcher's lastIndex MOVES under the batch), then a restore: the
    // resumed fused step must detect the moved position and re-drain — the
    // logger's observed lastIndex values prove the batch published its
    // per-step writes before the swap.
    let out = run_ok(&format!(
        r#"
        var re = /([a-h])(\d)/g;
        var s = "a1 b2 c3 d4 e5 f6 g7 h8";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        var log = [];
        log.push(it.next().value[0]);
        log.push(it.next().value[0]);
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function (str) {{
          log.push("li" + this.lastIndex);
          var r = orig.call(this, str);
          log.push("r" + (r && r[0]));
          return r;
        }};
        log.push(it.next().value[0]);
        log.push(it.next().value[0]);
        RegExp.prototype.exec = orig;
        for (var m of it) log.push(m[0] + "/" + RegExp.$1 + RegExp.$2);
        console.log("t2=" + log.join(","));
        "#
    ));
    assert_eq!(
        out[0],
        "t2=a1,b2,li5,rc3,c3,li8,rd4,d4,e5/e5,f6/f6,g7/g7,h8/h8"
    );
}

#[test]
fn mabatch_parity_always_empty_matches_advance_by_one() {
    let out = run_ok(&format!(
        r#"
        var re = /(?:)/g;
        var s = "abc";
        var out = [];
        for (var i = 0; i < {HOT}; i++) {{
          out = [];
          for (var m of s.matchAll(re)) out.push(m.index + ":" + m[0].length);
        }}
        console.log("t3=" + out.join(",") + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "t3=0:0,1:0,2:0,3:0 li=0");
}

#[test]
fn mabatch_parity_mixed_empty_and_nonempty_matches() {
    let out = run_ok(&format!(
        r#"
        var re = /a*/g;
        var s = "xaaxa";
        var out = [];
        for (var i = 0; i < {HOT}; i++) {{
          out = [];
          for (var m of s.matchAll(re)) out.push(m.index + ":" + m[0]);
        }}
        console.log("t4=" + out.join(",") + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(out[0], "t4=0:,1:aa,3:,4:a,5: li=0");
}

#[test]
fn mabatch_parity_break_early_then_a_second_matchall() {
    // Break at 2 of 20 leaves a live batch behind on every hot iteration;
    // each new matchAll is its own iterator (own batch), and the abandoned
    // entries must neither leak state into it nor disturb GC.
    let out = run_ok(&format!(
        r#"
        var re = /p(\d\d?)/g;
        var parts = [];
        for (var i = 0; i < 20; i++) parts.push("p" + i);
        var s = parts.join("");
        for (var i = 0; i < {HOT}; i++) {{
          var k = 0;
          for (var m of s.matchAll(re)) {{ k++; if (k === 2) break; }}
        }}
        var out = [];
        for (var m of s.matchAll(re)) out.push(m[1]);
        console.log("t5=" + out.join(",") + " li=" + re.lastIndex);
        "#
    ));
    assert_eq!(
        out[0],
        "t5=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19 li=0"
    );
}

#[test]
fn mabatch_parity_manual_next_interleaved_with_for_of() {
    // The for-of drives the same iterator the manual next() calls advance:
    // both must consume from one shared position stream.
    let out = run_ok(&format!(
        r#"
        var re = /([a-f])(\d)/g;
        var s = "a1 b2 c3 d4 e5 f6";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        var out = [];
        out.push(it.next().value[0]);
        for (var m of it) {{
          out.push("f" + m[0]);
          var extra = it.next();
          out.push(extra.done ? "D" : "n" + extra.value[0]);
        }}
        console.log("t6=" + out.join(","));
        "#
    ));
    assert_eq!(out[0], "t6=a1,fb2,nc3,fd4,ne5,ff6,D");
}

#[test]
fn mabatch_parity_legacy_statics_read_every_iteration() {
    // Annex B statics are refreshed by every successful builtin exec — the
    // batch publishes them per STEP from the stored triple, never at drain
    // time, so left/right context change per iteration.
    let out = run_ok(&format!(
        r#"
        var re = /([st])(\d+)/g;
        var s = "s1 t22 s333";
        var seen = [];
        for (var i = 0; i < {HOT}; i++) {{
          seen = [];
          for (var m of s.matchAll(re)) {{
            seen.push(RegExp.$1 + RegExp.$2 + "|" + RegExp.lastMatch + "|" + RegExp.leftContext + "/" + RegExp.rightContext);
          }}
        }}
        console.log("t7=" + seen.join(" "));
        "#
    ));
    assert_eq!(
        out[0],
        "t7=s1|s1|/ t22 s333 t22|t22|s1 / s333 s333|s333|s1 t22 /"
    );
}

#[test]
fn mabatch_parity_a_lying_null_exec_ends_the_iteration_mid_batch() {
    let out = run_ok(&format!(
        r#"
        var re = /b(\d)/g;
        var s = "b1b2b3b4b5";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        it.next();
        it.next();
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function () {{ return null; }};
        var r = it.next();
        RegExp.prototype.exec = orig;
        console.log("t8=" + r.done + "/" + it.next().done);
        "#
    ));
    assert_eq!(out[0], "t8=true/true");
}

#[test]
fn mabatch_parity_a_fake_exec_that_never_moves_lastindex() {
    // The fake rounds yield fake objects WITHOUT touching the matcher's
    // lastIndex, so after the restore the live position still equals the
    // batch's expected position — the batch must resume mid-stream, not
    // re-yield or skip a match.
    let out = run_ok(&format!(
        r#"
        var re = /c(\d)/g;
        var s = "c1c2c3c4c5";
        for (var i = 0; i < {HOT}; i++) Array.from(s.matchAll(re));
        var it = s.matchAll(re);
        var out = [];
        out.push(it.next().value[0]);
        out.push(it.next().value[0]);
        var orig = RegExp.prototype.exec;
        RegExp.prototype.exec = function () {{ return ["zz", "z"]; }};
        out.push(it.next().value[0]);
        out.push(it.next().value[0]);
        RegExp.prototype.exec = orig;
        for (var m of it) out.push(m[0]);
        console.log("t9=" + out.join(","));
        "#
    ));
    assert_eq!(out[0], "t9=c1,c2,zz,zz,c3,c4,c5");
}

/// RXSTATS evidence: a hot batch-eligible matchAll loop's steps are all
/// served by the fused path (the batch counts its published and done steps
/// exactly as the one-shot counted its execs). Runs in a child process
/// because the `ZIPP_RXSTATS` latch is read once per process and the
/// counters are process-global.
#[test]
fn the_fused_path_serves_the_batched_steps() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .args(["mabatch_child_counts", "--exact", "--nocapture"])
        .env("ZIPP_RXSTATS", "1")
        .env("ZIPP_MABATCH_CHILD", "1")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "child counts run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Child body for `the_fused_path_serves_the_batched_steps` — a no-op unless
/// spawned with the env markers set.
#[test]
fn mabatch_child_counts() {
    if std::env::var_os("ZIPP_MABATCH_CHILD").is_none() {
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
    // 500 iterations x 5 steps (4 matches + the terminating done step). The
    // first few can fall back while the slot memo warms; the overwhelming
    // majority must be fused.
    assert!(
        fused >= 2000,
        "expected the hot steps on the fused path: fused={fused} fallback={fallback}"
    );
}

/// The env latches are read once per process, so each mode needs its own
/// child: re-run every `mabatch_parity_` expectation with the switch set. The
/// same node-derived assertions passing in every mode IS the parity check —
/// `ZIPP_NO_MATCHALL_BATCH=1` in particular proves the off-switch reproduces
/// today's bytes.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_MATCHALL_BATCH", "1"),
        ("ZIPP_NO_SLIM_EXEC", "1"),
        ("ZIPP_NO_MATCHALL_STEP", "1"),
        ("ZIPP_NO_RX_SCANSESSION", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("mabatch_parity_")
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
            "the mabatch_parity_ filter matched nothing:\n{stdout}"
        );
    }
}
