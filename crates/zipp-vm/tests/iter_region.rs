//! Region-compiled `IterNext` / `PushFinally` / `PopFinally` (the for-of loop
//! body finally joining the MEM tier) plus the `ToNum` string arm: a for-of
//! over `matchAll`, a plain dense Array, an %ArrayIterator%, or a Map/Set no
//! longer blacklists its whole loop region — `jit_iter_next` steps the
//! intrinsic iterator kinds natively and deopts everything else BEFORE any
//! state moves, and `jit_to_num` serves `+someString` (the `+km[2]`
//! capture-sum idiom) via the pure StringToNumber grammar.
//!
//! Every `iter_parity_` case asserts byte-identical output against `node -e`
//! (node v24 on PATH, the same precondition as `dv_double_tier.rs`), at
//! DEFAULT thresholds — hot enough for the loop regions to OSR-compile. The
//! final test re-runs the whole set in three more modes, each in its own
//! child process: `ZIPP_NO_ITER_REGION=1` + `ZIPP_NO_TONUM_STR=1` (both
//! off-switches — the pre-change declines), `ZIPP_NOJIT=1` (pure
//! interpreter), and `ZIPP_JIT_THRESHOLD=1` (compile everything immediately).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
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
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
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

/// The bench's exact shape (bench/real/regex-log-scan.js section 4): for-of
/// over `matchAll` with a `+km[2]` capture sum — IterNext + the finally
/// bracket + the ToNum string arm all in one hot region.
#[test]
fn iter_parity_matchall_capture_sum() {
    assert_matches_node(
        r#"
        "use strict";
        var lines = [];
        for (var i = 0; i < 3000; i++) {
          lines.push("status=" + (200 + (i % 400)) + " bytes=" + (i * 37 % 100000) + " ms=" + (i % 2000));
        }
        var reKv = /([a-z]+)=(\d+)/g;
        var kvCount = 0, kvSum = 0;
        for (var i = 0; i < lines.length; i++) {
          for (var km of lines[i].matchAll(reKv)) {
            kvCount++;
            kvSum = (kvSum + (+km[2])) | 0;
          }
        }
        console.log("kv=" + kvCount + " sum=" + kvSum);
        "#,
    );
}

/// A zero-length global match must advance lastIndex per AdvanceStringIndex —
/// the step's empty-match protocol, now reached from native code.
#[test]
fn iter_parity_matchall_empty_matches() {
    assert_matches_node(
        r#"
        "use strict";
        var n = 0, idxSum = 0;
        for (var r = 0; r < 2000; r++) {
          for (var m of "abc".matchAll(/x?/g)) { n++; idxSum += m.index; }
        }
        console.log(n + " " + idxSum);
        "#,
    );
}

/// Astral subjects under /u: the +2 advance over a surrogate pair.
#[test]
fn iter_parity_matchall_unicode_advance() {
    assert_matches_node(
        r#"
        "use strict";
        var s = "a\u{1F600}b\u{1F601}c";
        var parts = [];
        for (var r = 0; r < 2000; r++) {
          parts.length = 0;
          for (var m of s.matchAll(/\p{L}?/gu)) parts.push(m.index + ":" + m[0].length);
        }
        console.log(parts.join(","));
        "#,
    );
}

/// `break` out of the for-of: the close handler (the finally bracket the
/// region now pushes/pops natively) must run exactly once and the loop state
/// must be consistent afterwards.
#[test]
fn iter_parity_matchall_break_mid_loop() {
    assert_matches_node(
        r#"
        "use strict";
        var total = 0;
        for (var r = 0; r < 3000; r++) {
          var k = 0;
          for (var m of "a=1 b=2 c=3 d=4".matchAll(/([a-z])=(\d)/g)) {
            k++;
            if (k === 2) break;
          }
          total += k;
        }
        console.log("total=" + total);
        "#,
    );
}

/// A throw from the loop BODY unwinds through the natively-pushed finally
/// handler — the handler-stack sync is what `jit_push_finally`/`jit_pop_finally`
/// exist for.
#[test]
fn iter_parity_throw_from_body_unwinds() {
    assert_matches_node(
        r#"
        "use strict";
        var caught = 0, steps = 0;
        for (var r = 0; r < 3000; r++) {
          try {
            for (var m of "x=1 y=2 z=3".matchAll(/(\w)=(\d)/g)) {
              steps++;
              if (m[1] === "z") throw new Error("stop " + r);
            }
          } catch (e) {
            caught++;
          }
        }
        console.log(steps + " " + caught);
        "#,
    );
}

/// Plain dense Array for-of — the positional walk, hot enough to compile.
#[test]
fn iter_parity_dense_array_walk() {
    assert_matches_node(
        r#"
        "use strict";
        var a = new Array(5000);
        for (var i = 0; i < 5000; i++) a[i] = (i * 7) & 255;
        var s = 0;
        for (var r = 0; r < 50; r++) {
          for (var v of a) s = (s + v) | 0;
        }
        console.log("s=" + s);
        "#,
    );
}

/// The array GROWS while being iterated: per spec the walk sees appended
/// elements (the length is re-read per step).
#[test]
fn iter_parity_array_grow_during_iteration() {
    assert_matches_node(
        r#"
        "use strict";
        var out = 0;
        for (var r = 0; r < 3000; r++) {
          var a = [1, 2, 3];
          var n = 0;
          for (var v of a) {
            n++;
            if (v === 2 && a.length < 5) a.push(9);
          }
          out += n;
        }
        console.log("out=" + out);
        "#,
    );
}

/// A HOLE mid-array reads through the prototype chain — the native walk must
/// hand exactly that step back to the interpreter.
#[test]
fn iter_parity_holey_array_prototype_read() {
    assert_matches_node(
        r#"
        "use strict";
        Array.prototype[2] = 77;
        var a = [10, 11, , 13];
        var s = 0;
        for (var r = 0; r < 3000; r++) {
          for (var v of a) s = (s + v) | 0;
        }
        console.log("s=" + s);
        "#,
    );
}

/// %ArrayIterator% shapes: values(), keys(), entries() — the intrinsic-next
/// step paths.
#[test]
fn iter_parity_array_iterator_kinds() {
    assert_matches_node(
        r#"
        "use strict";
        var a = [3, 1, 4, 1, 5];
        var s = 0, k = 0, e = 0;
        for (var r = 0; r < 2000; r++) {
          for (var v of a.values()) s = (s + v) | 0;
          for (var i of a.keys()) k = (k + i) | 0;
          for (var p of a.entries()) e = (e + p[0] * p[1]) | 0;
        }
        console.log(s + " " + k + " " + e);
        "#,
    );
}

/// Map and Set iterators through the collection step path.
#[test]
fn iter_parity_map_set_iterators() {
    assert_matches_node(
        r#"
        "use strict";
        var m = new Map([["a", 1], ["b", 2], ["c", 3]]);
        var st = new Set([10, 20, 30]);
        var s = 0;
        var names = "";
        for (var r = 0; r < 2000; r++) {
          for (var kv of m) s = (s + kv[1]) | 0;
          for (var v of st) s = (s + v) | 0;
        }
        for (var k of m.keys()) names += k;
        console.log(s + " " + names);
        "#,
    );
}

/// A USER iterator (plain object with a JS `next`) must keep full observable
/// semantics — the helper deopts it, the interpreter drives it.
#[test]
fn iter_parity_user_iterator_deopts() {
    assert_matches_node(
        r#"
        "use strict";
        function mkIter(n) {
          var i = 0;
          return { [Symbol.iterator]: function () { return this; },
                   next: function () { return { value: i, done: i++ >= n }; } };
        }
        var s = 0;
        for (var r = 0; r < 2000; r++) {
          for (var v of mkIter(5)) s = (s + v) | 0;
        }
        console.log("s=" + s);
        "#,
    );
}

/// A generator in the hot loop — driven by frame calls, interpreter only.
#[test]
fn iter_parity_generator_deopts() {
    assert_matches_node(
        r#"
        "use strict";
        function* g() { yield 1; yield 2; yield 3; }
        var s = 0;
        for (var r = 0; r < 2000; r++) {
          for (var v of g()) s = (s + v) | 0;
        }
        console.log("s=" + s);
        "#,
    );
}

/// A user `exec` INSTALLED MID-RUN on the matchAll regex: the step re-checks
/// pristine-ness every iteration, whichever engine steps it.
#[test]
fn iter_parity_matchall_user_exec_mid_run() {
    assert_matches_node(
        r#"
        "use strict";
        var re = /(\d)/g;
        var out = [];
        var polluted = false;
        for (var r = 0; r < 2500; r++) {
          var n = 0;
          for (var m of "123".matchAll(re)) n += +m[0];
          out.push(n);
          if (r === 2400 && !polluted) {
            polluted = true;
            re.exec = function (s) { return null; };
          }
        }
        console.log(out[0] + " " + out[2399] + " " + out[2401] + " " + out.length);
        "#,
    );
}

/// The ToNum string arm's coercion grammar: whitespace, empty, hex, binary,
/// exponent, Infinity, junk → NaN, and a leading `+`/`-`. All pure — served
/// natively; an object with valueOf still deopts (and still works).
#[test]
fn iter_parity_tonum_string_grammar() {
    assert_matches_node(
        r#"
        "use strict";
        var cases = ["42", " 42 ", "", "  ", "0x10", "0b101", "0o17", "1e3",
                     "-7.5", "+8", "Infinity", "-Infinity", "12px", ".5", "5.",
                     "1_000", "\t\n 9 \r"];
        var acc = [];
        for (var r = 0; r < 3000; r++) {
          if (r === 0) {
            for (var i = 0; i < cases.length; i++) acc.push(+cases[i]);
          } else {
            var s = 0;
            for (var i = 0; i < cases.length; i++) s += +cases[i] || 0;
          }
        }
        var obj = { valueOf: function () { return 41; } };
        var t = 0;
        for (var r = 0; r < 3000; r++) t = (t + (+obj)) | 0;
        console.log(acc.join(",") + " t=" + t);
        "#,
    );
}

/// Re-run every `iter_parity_` case in three more modes, each in its own
/// child process (the env latches are read once per process): both
/// off-switches together (the pre-change declines), the pure interpreter, and
/// threshold-1. The same node-derived assertions passing in all modes IS the
/// four-mode parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for envs in [
        &[("ZIPP_NO_ITER_REGION", "1"), ("ZIPP_NO_TONUM_STR", "1")][..],
        &[("ZIPP_NOJIT", "1")][..],
        &[("ZIPP_JIT_THRESHOLD", "1")][..],
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("iter_parity_");
        for (key, val) in envs {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{envs:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the iter_parity_ filter matched nothing under {envs:?}:\n{stdout}"
        );
    }
}
