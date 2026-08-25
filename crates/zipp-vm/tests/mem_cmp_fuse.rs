//! B118 memory-path fused compare→branch (`emit_fused_cmp_branch_head`): when
//! `Lt/Le/Gt/Ge/Eq/Ne {dst}` is immediately followed by `JumpIfTrue/False{cond:
//! dst}` in a MEM region, two Int-tagged operands compare and branch on flags,
//! skipping the boxed-bool store→load round trip. The bool is still stored to
//! `dst` (chained `||`/`&&` arms jump straight to the JumpIf ip, and deopt
//! wants the register file exact), and every non-Int operand pair falls
//! through to the unchanged generic sequence.
//!
//! Every `cmpfuse_parity_` case asserts byte-identical output against
//! `node -e` (node v24 on PATH, the `dv_double_tier.rs` precondition), at
//! default thresholds — hot enough for the loops to OSR-compile as MEM regions
//! (each loop carries an array push or string op so it cannot land on the INT
//! tier, where this head is never emitted). The final test re-runs the set in
//! three more modes, each in its own child process (the env latch is read once
//! per process): `ZIPP_NO_MEM_CMPJUMP=1` (the off-switch — unfused pair),
//! `ZIPP_NOJIT=1` (pure interpreter), and `ZIPP_JIT_THRESHOLD=1`.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The same program's output from `node -e`, so expectations aren't
/// hand-computed.
fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// The `parse-large-js` tokenize shape, scaled down: a charCodeAt scanner whose
/// dispatch is chained `c === k` ORs (each arm's JumpIf is entered BOTH by
/// fall-through from its own Eq and by direct jumps from earlier arms), with
/// `push` keeping the loop on the memory path.
#[test]
fn cmpfuse_parity_tokenizer_shape() {
    assert_matches_node(
        r#"
        "use strict";
        var src = "";
        for (var r = 0; r < 400; r++) src += "var ab = cd + 12;\n  // note\n x.y = \"s\";\t";
        var kinds = [], starts = [];
        var i = 0, n = src.length;
        while (i < n) {
          var c = src.charCodeAt(i);
          if (c === 32 || c === 10 || c === 9 || c === 13) { i++; continue; }
          var st = i;
          if ((c >= 97 && c <= 122) || (c >= 65 && c <= 90) || c === 95 || c === 36) {
            i++;
            while (i < n) {
              var d = src.charCodeAt(i);
              if ((d >= 97 && d <= 122) || (d >= 48 && d <= 57)) i++; else break;
            }
            kinds.push(1); starts.push(st); continue;
          }
          if (c >= 48 && c <= 57) {
            i++;
            while (i < n && src.charCodeAt(i) >= 48 && src.charCodeAt(i) <= 57) i++;
            kinds.push(2); starts.push(st); continue;
          }
          i++;
          kinds.push(4); starts.push(st);
        }
        var h = 5381;
        for (var k = 0; k < kinds.length; k++) h = (h * 33 + kinds[k] * 7 + starts[k]) | 0;
        console.log(kinds.length + " " + h);
        "#,
    );
}

/// The same fused site fed MIXED operand types over iterations: Ints, doubles
/// (fractional and integral-valued), NaN, undefined, single-char strings, and
/// a longer string — the Int fast head must miss to the generic sequence and
/// answer identically for every pair, including `NaN === NaN` (false) and
/// `1.0 === 1` (true).
#[test]
fn cmpfuse_parity_mixed_types_through_one_site() {
    assert_matches_node(
        r#"
        "use strict";
        var vals = [1, 2, 1.5, 2.0, NaN, undefined, "a", "b", "ab", -1, 0, 2147483647, -2147483648];
        var log = [];
        var eqc = 0, nec = 0, ltc = 0, gec = 0;
        for (var r = 0; r < 4000; r++) {
          var a = vals[r % vals.length];
          var b = vals[(r * 7 + 3) % vals.length];
          if (a === b) { eqc++; } else { nec++; }
          if (a < b) { ltc++; }
          if (a >= b) { gec++; }
          if (r < 26) log.push((a === b) + "," + (a < b) + "," + (a >= b));
        }
        console.log(eqc + " " + nec + " " + ltc + " " + gec);
        console.log(log.join(";"));
        "#,
    );
}

/// Int boundary values through fused Lt/Le/Gt/Ge: i32 extremes must compare
/// SIGNED (the head compares the 32-bit payloads), and overflow into doubles
/// must fall off the Int lane and still agree with node.
#[test]
fn cmpfuse_parity_signed_boundaries() {
    assert_matches_node(
        r#"
        "use strict";
        var xs = [-2147483648, -1, 0, 1, 2147483647, 2147483648, -2147483649];
        var out = [];
        var acc = [];
        for (var r = 0; r < 3000; r++) {
          var a = xs[r % xs.length];
          var b = xs[(r + 3) % xs.length];
          var s = 0;
          if (a < b) s += 1;
          if (a <= b) s += 2;
          if (a > b) s += 4;
          if (a >= b) s += 8;
          if (a === b) s += 16;
          if (a !== b) s += 32;
          acc.push(s);
          if (r < xs.length * xs.length && r < 49) out.push(a + "?" + b + "=" + s);
        }
        var h = 0;
        for (var i = 0; i < acc.length; i++) h = (h * 31 + acc[i]) | 0;
        console.log(h);
        console.log(out.join(" "));
        "#,
    );
}

/// The compare RESULT read again after the branch (`var t = (a === b); if (t)
/// ... ; uses += t ? 1 : 0`): the fused head must still store the Bool to its
/// register, not just branch on flags.
#[test]
fn cmpfuse_parity_bool_result_reused_after_branch() {
    assert_matches_node(
        r#"
        "use strict";
        var sink = [];
        var taken = 0, reread = 0;
        for (var r = 0; r < 5000; r++) {
          var t = (r % 3) === 1;
          if (t) { taken++; }
          reread += t ? 2 : 1;
          if ((r & 1023) === 0) sink.push(String(t));
        }
        console.log(taken + " " + reread + " " + sink.join(","));
        "#,
    );
}

/// A fused compare whose branch EXITS the loop (`break` — the JumpIf target is
/// outside the region, resolved via an exit stub) and one that spans nested
/// loops, plus `continue` arms. Exercises `region_target`'s out-of-region path
/// from the fused jcc.
#[test]
fn cmpfuse_parity_branch_exits_region() {
    assert_matches_node(
        r#"
        "use strict";
        var total = 0, brk = 0;
        for (var r = 0; r < 300; r++) {
          var seen = [];
          for (var i = 0; i < 500; i++) {
            var v = (i * 37 + r) & 255;
            if (v === 251) { brk++; break; }
            if (v === 13) { continue; }
            if (v <= 7) { seen.push(v); }
            total = (total + v) | 0;
          }
          total = (total + seen.length) | 0;
        }
        console.log(total + " " + brk);
        "#,
    );
}

/// W6 Tier-C shape: a hot LOOP-FREE function whose whole body is compare
/// chains (the recursive-descent parser's `tokIs`/`pFactor` shape — B118's
/// recorded follow-up). The function is fn-compiled on the whole-function mem
/// path (no loop, so OSR never sees it), where the same fused head now runs;
/// chained `===`/`||` arms enter each JumpIf ip from earlier arms, so the bool
/// store must survive fusion here exactly as in a region.
#[test]
fn cmpfuse_parity_tierc_classifier_function() {
    assert_matches_node(TIERC_CLASSIFIER_SRC);
}

/// The Tier-C classifier program, shared with the JITLOG child probe below.
/// The `charCodeAt` keeps the function OFF the INT tier (`fn_int::can_compile`
/// rejects CallMethod) so it lands on the whole-function MEM path.
const TIERC_CLASSIFIER_SRC: &str = r#"
    "use strict";
    function classify(s, i) {
      var c = s.charCodeAt(i);
      if (c === 32 || c === 10 || c === 9 || c === 13) return 0;
      if (c >= 48 && c <= 57) return 1;
      if (c >= 97 && c <= 122) return 2;
      if (c >= 65 && c <= 90) return 3;
      if (c === 95 || c === 36) return 4;
      return 5;
    }
    var src = "";
    for (var r = 0; r < 120; r++) src += "var ab = cd + 12;\n  // note\n x.y = 's';\t";
    var counts = [0, 0, 0, 0, 0, 0];
    for (var p = 0; p < 8; p++) {
      for (var i = 0; i < src.length; i++) counts[classify(src, i)]++;
    }
    console.log(counts.join(","));
"#;

/// Tier-C shape with the compare RESULT reused after its branch inside the
/// function body (`var t = a === b; if (t) ...; sum += t ? 2 : 1`), plus
/// relational chains over mixed Int/double/string arguments — the fused head's
/// non-Int miss lane and the mandatory bool store, in a fn-compiled body.
#[test]
fn cmpfuse_parity_tierc_bool_reuse_mixed_args() {
    assert_matches_node(
        r#"
        "use strict";
        function score(a, b, tag) {
          // The charCodeAt keeps score() off the INT tier (CallMethod is
          // rejected there) so it fn-compiles on the MEM path (Tier C).
          var t = a === b;
          if (tag.charCodeAt(0) === 122) return -1;
          var s = 0;
          if (t) s += 16;
          s += t ? 2 : 1;
          if (a < b) s += 4;
          if (a >= b) s += 8;
          if (a !== b) s += 32;
          return s;
        }
        var vals = [1, 2, 1.5, 2.0, NaN, "a", "b", -2147483648, 2147483647, 0];
        var h = 0, log = [];
        for (var r = 0; r < 25000; r++) {
          var s = score(vals[r % vals.length], vals[(r * 7 + 3) % vals.length], "q");
          h = (h * 31 + s) | 0;
          if (r < 20) log.push(s);
        }
        console.log(h + " " + log.join(","));
        "#,
    );
}

/// CHILD half of the Tier-C verification: runs the classifier program in-process
/// so the parent (`zz_tierc_classifier_compiles_tier_c`) can read its JITLOG.
/// Runs as an ordinary parity test in a normal invocation.
#[test]
fn tierc_jitlog_child_probe() {
    assert_matches_node(TIERC_CLASSIFIER_SRC);
}

/// PARENT half: spawn the probe with `ZIPP_JITLOG=1` (latches process-wide) and
/// prove the classifier really lands on TIER C — a `[jit] Tier C fn… compiled
/// (whole-function mem path…)` line — not merely an OSR region of the driver
/// loop. This is what makes the two `cmpfuse_parity_tierc_` cases evidence
/// about `compile_proto_mem`'s fused head rather than the region's.
#[test]
fn zz_tierc_classifier_compiles_tier_c() {
    if std::env::var_os("ZIPP_TIERC_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .args(["tierc_jitlog_child_probe", "--exact", "--nocapture"])
        .env("ZIPP_TIERC_CHILD", "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_NO_FNJIT_MEM")
        .env_remove("ZIPP_NO_MEM_CMPJUMP")
        .output()
        .expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "Tier-C probe child failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("Tier C fn") && stderr.contains("whole-function mem path"),
        "classifier did not compile on Tier C (whole-function mem path):\n{stderr}"
    );
}

/// Re-run every `cmpfuse_parity_` case in three more modes, each in its own
/// child process (the env latch is read once per process): the off-switch, the
/// pure interpreter, and threshold-1. The same node-derived assertions passing
/// in all modes IS the four-mode parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_MEM_CMPJUMP", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("cmpfuse_parity_")
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
            "the cmpfuse_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
