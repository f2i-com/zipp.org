//! Exact span/code-unit predicate fusion and its adjacent-OR collapse.
//!
//! The fast helper is deliberately narrower than JavaScript: only present Int
//! elements in pristine dense Arrays plus a flat String.  Every other shape
//! must deopt before observable work and replay the ordinary leaf call.  These
//! tests pin the hot dense path, UTF-16/WTF-8 code units, fractional indices,
//! holes/prototypes, accessors, proxies, global replacement, callee rebinding,
//! short-circuit side effects, both off-switches, no-JIT parity, and GC stress.

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
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (reference output)");
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

/// The benchmark body and adjacent-OR caller shape, including supplementary
/// and deliberately unpaired surrogate code units.  Every function is called
/// far beyond the Tier-C/leaf thresholds; the census below proves the pair is
/// actually planned rather than this passing on the interpreter.
#[test]
fn span_pair_parity_dense_unicode_wtf8() {
    assert_matches_node(
        r#""use strict";
        var kinds = [4,4,4,4,4,4,4,4];
        var starts = [0,1,2,3,4,5,6,7];
        var ends =   [1,2,3,4,5,6,7,8];
        var src = "*/\uD83D\uDE00\uD800X\uDC00?";
        function tokIs(i, ch) {
          return kinds[i] === 4 && ends[i] - starts[i] === 1 &&
                 src.charCodeAt(starts[i]) === ch;
        }
        function op(i) { return tokIs(i, 42) || tokIs(i, 47); }
        function pairSurrogate(i) { return tokIs(i, 55357) || tokIs(i, 56832); }
        function loneSurrogate(i) { return tokIs(i, 55296) || tokIs(i, 56320); }
        var a = 0, b = 0, c = 0, digest = 0;
        for (var r = 0; r < 90000; r++) {
          var i = r & 7;
          if (op(i)) a++;
          if (pairSurrogate(i)) b++;
          if (loneSurrogate(i)) c++;
          digest = (digest * 33 + src.charCodeAt(i)) | 0;
        }
        console.log(a + ":" + b + ":" + c + ":" + digest);
        console.log(op(0) + ":" + op(1) + ":" + pairSurrogate(2) + ":" +
                    pairSurrogate(3) + ":" + loneSurrogate(4) + ":" +
                    loneSurrogate(6) + ":" + op(7));
        "#,
    );
}

/// A loop inside the caller warms both leaf-call ICs many times before Tier C
/// compiles that caller.  This is the parse-large `pTerm`/`pExpr` topology and
/// guarantees the second side of the OR has a monomorphic witness when the
/// pair plan is built (one call per activation can compile one hit too early).
#[test]
fn span_pair_parity_hot_loop_engagement() {
    assert_matches_node(
        r#""use strict";
        var kinds = [4,4,1,4], starts = [0,1,2,3], ends = [1,2,3,4], src = "*/x+";
        var P_pos = 0;
        function tokIs(i, ch) {
          return kinds[i] === 4 && ends[i] - starts[i] === 1 &&
                 src.charCodeAt(starts[i]) === ch;
        }
        function scan(seed) {
          var hits = 0;
          for (var j = 0; j < 96; j++) {
            P_pos = (j + seed) & 3;
            if (tokIs(P_pos, 42) || tokIs(P_pos, 47)) hits++;
          }
          return hits;
        }
        var total = 0;
        for (var r = 0; r < 3000; r++) total += scan(r);
        console.log(total + ":" + scan(3));
        "#,
    );
}

/// Runtime values outside the helper's closed set must replay the ordinary
/// body.  Fractional span endpoints are especially load-bearing:
/// `1.5 - 0.5 === 1`, then charCodeAt applies ToInteger(0.5) and succeeds.
/// Replacing every live global also pins that the plan stores SLOT numbers,
/// never stale receiver pointers.
#[test]
fn span_pair_parity_fractional_holes_and_live_globals() {
    assert_matches_node(
        r#""use strict";
        var kinds = [4,4], starts = [0,1], ends = [1,2], src = "*/";
        var P_pos = 0;
        function tokIs(i, ch) {
          return kinds[i] === 4 && ends[i] - starts[i] === 1 &&
                 src.charCodeAt(starts[i]) === ch;
        }
        function probe(i, reps) {
          var out = false;
          for (var q = 0; q < reps; q++) {
            P_pos = i;
            out = tokIs(P_pos, 42) || tokIs(P_pos, 47);
          }
          return out;
        }
        var warm = 0;
        for (var r = 0; r < 20; r++) if (probe(r & 1, 128)) warm++;

        // A mapped arguments object is Array-backed internally, but index 0
        // aliases the live formal rather than the backing Vec. The fused helper
        // must decline and let ordinary GetIndex observe the reassignment.
        function mappedKind(kind) {
          kinds = arguments; starts = [0]; ends = [1]; src = "*";
          kind = 4;
          return probe(0, 1);
        }
        var mappedArguments = mappedKind(3);

        starts = [0.5]; ends = [1.5];
        kinds = [4]; src = "*";
        var fractionalSpan = probe(0, 1);

        kinds = []; starts = []; ends = []; src = "/";
        kinds["0.5"] = 4; starts["0.5"] = 0; ends["0.5"] = 1;
        var fractionalIndex = probe(0.5, 1);

        kinds = [4]; starts = [0]; ends = [1]; src = "/";
        delete kinds[0];
        Array.prototype[0] = 4;
        var inheritedHole = probe(0, 1);
        delete Array.prototype[0];

        // A variable concat keeps the rope fallback live in engines that use
        // rope strings; the semantic result is the same in every tier.
        var left = "*"; src = left + "/";
        kinds = [4,4]; starts = [0,1]; ends = [1,2];
        var live = (probe(0, 1) ? 1 : 0) + (probe(1, 1) ? 2 : 0);
        kinds = ["4", 4];
        var nonIntKind = probe(0, 1);
        console.log(warm + ":" + mappedArguments + ":" + fractionalSpan + ":" + fractionalIndex + ":" +
                    inheritedHole + ":" + live + ":" + nonIntKind);
        "#,
    );
}

/// Accessors and proxies are observable.  The helper must decline before the
/// first getter/trap, then the ordinary short-circuit program invokes exactly
/// one predicate for `*`, two for `/`, and one throwing access under `try`.
/// Rebinding the callee after the site is native must likewise take the exact
/// identity-guard fallback and observe both replacement calls.
#[test]
fn span_pair_parity_observable_fallback_and_rebind() {
    assert_matches_node(
        r#""use strict";
        var kinds = [4,4], starts = [0,1], ends = [1,2], src = "*/";
        var P_pos = 0;
        function tokIs(i, ch) {
          return kinds[i] === 4 && ends[i] - starts[i] === 1 &&
                 src.charCodeAt(starts[i]) === ch;
        }
        function probe(i, reps) {
          var out = false;
          for (var q = 0; q < reps; q++) {
            P_pos = i;
            out = tokIs(P_pos, 42) || tokIs(P_pos, 47);
          }
          return out;
        }
        var warm = 0;
        for (var r = 0; r < 20; r++) if (probe(r & 1, 128)) warm++;

        var gets = 0;
        var raw = kinds;
        kinds = new Proxy(raw, {
          get: function (target, key) { gets++; return target[key]; }
        });
        var star = probe(0, 1), slash = probe(1, 1);

        kinds = [4]; starts = [0]; ends = [1]; src = "*";
        var ownGets = 0;
        Object.defineProperty(kinds, "0", {
          configurable: true,
          get: function () { ownGets++; return 4; }
        });
        var own = probe(0, 1);

        var throws = 0, caught = false;
        kinds = new Proxy([4], {
          get: function () { throws++; throw new Error("once"); }
        });
        try { probe(0, 1); } catch (e) { caught = e.message === "once"; }

        var rebound = 0;
        tokIs = function (i, ch) { rebound++; return ch === 47; };
        var reboundResult = probe(0, 1);
        console.log(warm + ":" + star + ":" + slash + ":" + gets + ":" +
                    own + ":" + ownGets + ":" + caught + ":" + throws + ":" +
                    reboundResult + ":" + rebound);
        "#,
    );
}

/// Engagement proof: at least one dense test must plan both the singleton and
/// adjacent-OR mechanisms.  Without this, all parity cases could pass while a
/// recognizer typo silently leaves the benchmark unoptimized.
#[test]
fn span_code_unit_jitlog_engagement() {
    let exe = std::env::current_exe().expect("test exe");
    let out = std::process::Command::new(&exe)
        .arg("span_pair_parity_hot_loop_engagement")
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .output()
        .expect("spawn test child");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stderr}");
    assert!(
        stderr.contains("SPAN-CODEUNIT-PRED"),
        "singleton predicate never planned:\n{stderr}"
    );
    assert!(
        stderr.contains("SPAN-CODEUNIT-PAIR"),
        "adjacent predicate pair never planned:\n{stderr}"
    );
}

/// Both process-latched switches must remove exactly their advertised plan:
/// the pair switch retains singleton fusion, while the parent switch removes
/// both.  Separate children avoid cross-test latch contamination.
#[test]
fn span_code_unit_off_switches() {
    let exe = std::env::current_exe().expect("test exe");
    for (key, want_pred, want_pair) in [
        ("ZIPP_NO_SPAN_CODEUNIT_PAIR", true, false),
        ("ZIPP_NO_SPAN_CODEUNIT_PRED", false, false),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("span_pair_parity_hot_loop_engagement")
            .arg("--exact")
            .arg("--nocapture")
            .env("ZIPP_JITLOG", "1")
            .env(key, "1")
            .output()
            .expect("spawn switch child");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{key} child failed:\n{stderr}");
        assert_eq!(
            stderr.contains("SPAN-CODEUNIT-PRED"),
            want_pred,
            "{key} predicate census mismatch:\n{stderr}"
        );
        assert_eq!(
            stderr.contains("SPAN-CODEUNIT-PAIR"),
            want_pair,
            "{key} pair census mismatch:\n{stderr}"
        );
    }
}

/// Tier parity in fresh processes: generic singleton, generic leaf, pure
/// interpreter, forced-hot JIT, and collect-at-every-safe-point execution all
/// answer identically to Node for every adversarial program above.
#[test]
fn span_code_unit_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe");
    for (key, val) in [
        ("ZIPP_NO_SPAN_CODEUNIT_PAIR", "1"),
        ("ZIPP_NO_SPAN_CODEUNIT_PRED", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("span_pair_parity_")
            .env(key, val)
            .output()
            .expect("spawn parity child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "mode filter matched no parity tests under {key}={val}"
        );
    }
}
