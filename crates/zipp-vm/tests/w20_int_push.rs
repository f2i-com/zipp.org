//! W20 — the two rungs that let a tokenizer-shaped scan loop reach the INTEGER
//! register tier, and the differential gates for both.
//!
//! THE LADDER. `parse-large-js`'s `tokenize` is 100% integer-and-pinned-access
//! work that the register tier refused over two opcode classes, in this order:
//!
//!   * RUNG 1 — `arr.push(int)`. A `CallMethod` whose receiver is not a pinned
//!     string/DataView declined the whole region, so the 22M-`charCodeAt` scan
//!     ran on the boxed memory tier because it also emitted 10.8M appends.
//!     M2 admits the append on a dense all-Int Array pin (`ARR_INT_PIN_KIND`).
//!   * RUNG 2 — the BOOL gpr pool. `BOOL_GPRS` is four registers handed out
//!     first-fit with no allocator, and the region declines at the fifth
//!     DISTINCT bool temp even though at most two are ever live at once. M1
//!     linear-scan-reuses them over non-overlapping live ranges.
//!
//! Neither rung is worth anything alone — rung 1 declines first everywhere, so
//! rung 2's decline string appears in no bench row today — so both are gated
//! here together.
//!
//! WHAT THESE TESTS ARE DEFENDING, and why the list is what it is.
//!
//! M2 puts a CALL inside a region whose whole register discipline was built on
//! there not being one. `BOOL_GPRS` (r8..r11) and any numeric home in
//! xmm2..xmm5 are volatile under win64, and the pin snapshots hold raw pointers
//! into a `Vec` the append can REALLOCATE. Three prior silent wrong answers in
//! this campaign came from exactly this family (W14's dense-element tag check,
//! W16's `Bitwise` sentinel and `emit_box_to_home`), so every one of those
//! surfaces gets a case:
//!
//!   * [`intpush_parity_live_bools_across_the_append`] — 1..8 live bools around
//!     the push, so every `BOOL_GPRS` register is occupied in turn while the
//!     call runs. This is `bool_home_clobber.rs`'s sweep pointed at the one arm
//!     that calls out.
//!   * [`intpush_parity_realloc_boundary`] — the append is read back through
//!     the SAME pin (`out[out.length - 1]`) on the same iteration, across every
//!     `Vec` capacity doubling from 0 to 4096. A stale `base` or `len` in the
//!     snapshot is a wrong answer here, not a crash.
//!   * [`intpush_parity_sibling_pin_is_not_stale`] — a second pinned array read
//!     in the same loop, to prove the append repairs its own pin and leaves the
//!     others alone.
//!   * [`intpush_parity_two_pins_one_array`] — the aliasing case the prologue's
//!     pairwise `obj_bits` check exists for: two globals naming ONE array, one
//!     of them pushed. The answer must still be node's.
//!   * [`intpush_parity_deopt_shapes`] — frozen, sealed, non-writable `length`,
//!     a prototype index, a sparse virtual length, and a receiver swapped
//!     mid-loop. Each must take the helper's deopt and produce node's answer
//!     (including node's TypeError).
//!   * [`intpush_parity_value_boxing`] — pushed values that leave i32, so the
//!     helper's Int-if-it-fits-else-double boxing is compared against node's
//!     own numbers rather than against our interpreter.
//!
//! M1 changes which register a bool lives in, and drops the entry load for a
//! bool that shares one. Its cases are:
//!
//!   * [`boolreuse_parity_disjoint_bools`] — 1..24 distinct bool temps with
//!     non-overlapping ranges (5, 8 and 21 are the interesting counts: today's
//!     pool is 4 and the real tokenizer has 21).
//!   * [`boolreuse_parity_conditional_def_bool`] — the W18 defect shape with a
//!     BOOL: a bool whose only def is behind a branch, read on the path that
//!     skips it. It must keep a private home and its entry load.
//!   * [`boolreuse_parity_read_after_the_loop`] — a bool read AFTER the region
//!     may never share (its slot is observable), so this is the case that fails
//!     if `bool_shareable` is ever relaxed to region-local liveness.
//!
//! Every expectation comes from `node -e`, never from `ZIPP_NOJIT=1`: a planner
//! bug that also existed in the interpreter would pass that oracle.
//! [`intpush_all_modes_answer_identically`] re-runs the whole set in child
//! processes under each switch (including both off-switches and GC stress), and
//! [`intpush_mechanism_reaches_the_int_tier`] reads the tier back out of a
//! child's `ZIPP_JITLOG` so an admission change that quietly drops these
//! kernels to the memory tier fails the suite instead of making it vacuous.

use std::process::Command;

// ── oracles ─────────────────────────────────────────────────────────────────

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Zipp's output including a thrown error, rendered the way the cases that
/// EXPECT a throw compare against node (which prints the message to stderr and
/// exits non-zero).
fn run_any(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    let mut v = out.output;
    if let Some(e) = out.error {
        v.push(format!("THREW {e}"));
    }
    v
}

fn node_output(src: &str) -> Vec<String> {
    let out = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from `node -e`)");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

// ── kernels ─────────────────────────────────────────────────────────────────

/// `k` bool temps with DISJOINT live ranges around an `arr.push(int)`.
///
/// Each bool is defined, consumed by its own branch, and never mentioned again,
/// which is precisely the tokenizer's shape (one bool per condition SITE, at
/// most two live at once). The accumulator folds each branch's effect back in,
/// so a bool that reads back wrong changes the printed answer.
fn push_bools_case(k: usize, n: usize) -> String {
    let body: String = (0..k)
        .map(|j| format!("    if ((c + {j}) % {m} === 0) {{ h = (h + {j} + 1) | 0; }}\n", m = k + 2))
        .collect();
    format!(
        r#"var out = [];
function kernel(n) {{
  var h = 0, i = 0, c = 0;
  for (i = 0; i < n; i++) {{
    c = i & 63;
{body}    out.push(h & 255);
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 3; r++) {{ out = []; s += "|" + kernel({n}) + ":" + out.length + ":" + out[0] + ":" + out[out.length - 1]; }}
console.log("pushbools-{k} " + s);
"#
    )
}

/// The append read straight back through its OWN pin on the same iteration.
/// `out.length` and `out[...]` both come from the 32-byte snapshot slot that
/// `jit_array_push_pinned` rewrites, so this is the case a stale `base`/`len`
/// fails.
fn realloc_boundary_case(n: usize) -> String {
    format!(
        r#"var out = [];
function kernel(n) {{
  var h = 0;
  for (var i = 0; i < n; i++) {{
    var a = i > 2, b = i < 1000000, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0;
    if (b) h = (h + 2) | 0;
    if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0;
    if (e) h = (h + 5) | 0;
    out.push(h & 1023);
    h = (h + out[out.length - 1] + out.length) | 0;
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 3; r++) {{ out = []; s += "|" + kernel({n}) + ":" + out.length + ":" + out[0] + ":" + out[out.length - 1]; }}
console.log("realloc-{n} " + s);
"#
    )
}

/// A second pinned dense array READ in the same loop as the append. Its base
/// must survive the other array's realloc — the append repairs its own pin and
/// touches nothing else.
fn sibling_pin_case(n: usize) -> String {
    format!(
        r#"var out = [];
var ref = [];
for (var q = 0; q < 64; q++) ref.push((q * 7) & 31);
function kernel(n) {{
  var h = 0;
  for (var i = 0; i < n; i++) {{
    var a = i > 2, b = i < 1000000, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1, f = i > 9;
    if (a) h = (h + 1) | 0;
    if (b) h = (h + 2) | 0;
    if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0;
    if (e) h = (h + 5) | 0;
    if (f) h = (h + 6) | 0;
    h = (h + ref[i & 63]) | 0;
    out.push(h & 1023);
    h = (h + ref[(i + 7) & 63] + ref.length) | 0;
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 3; r++) {{ out = []; s += "|" + kernel({n}) + ":" + out.length + ":" + out[out.length - 1]; }}
console.log("sibling-{n} " + s);
"#
    )
}

/// TWO globals naming ONE array, one of them pushed and the other read. The
/// prologue's pairwise `obj_bits` check is what stops the read's pin from going
/// stale behind the append; the answer must be node's either way.
fn aliased_pin_case(n: usize) -> String {
    format!(
        r#"var out = [];
var alias = out;
function kernel(n) {{
  var h = 0;
  for (var i = 0; i < n; i++) {{
    var a = i > 2, b = i < 1000000, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0;
    if (b) h = (h + 2) | 0;
    if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0;
    if (e) h = (h + 5) | 0;
    out.push(h & 1023);
    h = (h + alias[alias.length - 1] + alias.length) | 0;
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 3; r++) {{ out = []; alias = out; s += "|" + kernel({n}) + ":" + out.length; }}
console.log("aliased-{n} " + s);
"#
    )
}

/// The tokenizer shape itself, shrunk: a `charCodeAt` scan with many bool
/// condition sites that emits three parallel token arrays. This is the kernel
/// [`intpush_mechanism_reaches_the_int_tier`] asserts the tier on.
fn tokenizer_case(n: usize) -> String {
    format!(
        r#"var src = "";
for (var q = 0; q < {n}; q++) src += "var ab_" + (q % 97) + " = " + (q % 1000) + "; // x\n";
var kinds = [], starts = [], ends = [];
function tokenize() {{
  kinds = []; starts = []; ends = [];
  var i = 0, n = src.length, d = 0;
  while (i < n) {{
    var c = src.charCodeAt(i);
    if (c === 32 || c === 10 || c === 9 || c === 13) {{ i = i + 1; continue; }}
    var st = i;
    if (c === 47) {{
      i = i + 2;
      while (i < n && src.charCodeAt(i) !== 10) i = i + 1;
      kinds.push(5); starts.push(st); ends.push(i); continue;
    }}
    if ((c >= 97 && c <= 122) || (c >= 65 && c <= 90) || c === 95 || c === 36) {{
      i = i + 1;
      while (i < n) {{
        d = src.charCodeAt(i);
        if ((d >= 97 && d <= 122) || (d >= 48 && d <= 57) || d === 95) i = i + 1; else break;
      }}
      kinds.push(1); starts.push(st); ends.push(i); continue;
    }}
    if (c >= 48 && c <= 57) {{
      i = i + 1;
      while (i < n && (d = src.charCodeAt(i)) >= 48 && d <= 57) i = i + 1;
      kinds.push(2); starts.push(st); ends.push(i); continue;
    }}
    i = i + 1;
    kinds.push(4); starts.push(st); ends.push(i);
  }}
  var h = 0;
  for (var k = 0; k < kinds.length; k++) h = (h * 31 + kinds[k] + starts[k] + ends[k]) | 0;
  return kinds.length + ":" + h;
}}
var s = "";
for (var r = 0; r < 4; r++) s += "|" + tokenize();
console.log("tok " + s);
"#
    )
}

// ── M2 parity ───────────────────────────────────────────────────────────────

/// 1..8 live bool temps around the append, so every `BOOL_GPRS` register (and,
/// past four, every SHARED one) is occupied while the win64 call runs. A save
/// or restore this arm forgets shows up here as a wrong boolean.
#[test]
fn intpush_parity_live_bools_across_the_append() {
    for k in 1..=8 {
        assert_matches_node(&push_bools_case(k, 400));
    }
}

/// Every `Vec` capacity doubling from empty to 4096, with the appended element
/// read back through the same pin on the same iteration.
#[test]
fn intpush_parity_realloc_boundary() {
    for n in [1usize, 2, 3, 5, 8, 9, 16, 17, 33, 65, 129, 257, 1000, 4097] {
        assert_matches_node(&realloc_boundary_case(n));
    }
}

#[test]
fn intpush_parity_sibling_pin_is_not_stale() {
    for n in [7usize, 64, 300, 2000] {
        assert_matches_node(&sibling_pin_case(n));
    }
}

#[test]
fn intpush_parity_two_pins_one_array() {
    for n in [7usize, 300, 2000] {
        assert_matches_node(&aliased_pin_case(n));
    }
}

#[test]
fn intpush_parity_tokenizer_shape() {
    for n in [40usize, 400, 2000] {
        assert_matches_node(&tokenizer_case(n));
    }
}

/// The shapes `jit_array_push_pinned` must DEOPT on rather than handle. Each
/// one is legal JS with an answer node will state; the arm is only allowed to
/// be faster, never different.
#[test]
fn intpush_parity_deopt_shapes() {
    // A prototype index property: a new index resolves through OrdinarySet, so
    // the prototype's setter/value participates.
    assert_matches_node(
        r#"var out = [];
function kernel(n) { var h = 0;
  for (var i = 0; i < n; i++) {
    var a = i > 2, b = i < 9e5, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0; if (b) h = (h + 2) | 0; if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0; if (e) h = (h + 5) | 0;
    if (i === 600) Object.defineProperty(Array.prototype, "3", { value: 7, writable: true, configurable: true });
    out.push(h & 255);
  }
  return h; }
var s = "";
for (var r = 0; r < 2; r++) { out = []; s += "|" + kernel(1200) + ":" + out.length + ":" + out[3]; }
console.log("protoidx " + s);
"#,
    );
    // A SPARSE array: the virtual-length side table governs where push lands.
    assert_matches_node(
        r#"var out = [];
function kernel(n) { var h = 0;
  for (var i = 0; i < n; i++) {
    var a = i > 2, b = i < 9e5, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0; if (b) h = (h + 2) | 0; if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0; if (e) h = (h + 5) | 0;
    if (i === 600) out.length = 100000;
    out.push(h & 255);
  }
  return h; }
var s = "";
for (var r = 0; r < 2; r++) { out = []; s += "|" + kernel(1200) + ":" + out.length; }
console.log("sparse " + s);
"#,
    );
    // The receiver REPLACED mid-loop: the pin's identity guard misses and the
    // append must land in the new array.
    assert_matches_node(
        r#"var out = [];
function kernel(n) { var h = 0;
  for (var i = 0; i < n; i++) {
    var a = i > 2, b = i < 9e5, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0; if (b) h = (h + 2) | 0; if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0; if (e) h = (h + 5) | 0;
    if (i === 600) out = [];
    out.push(h & 255);
  }
  return h; }
var s = "";
for (var r = 0; r < 2; r++) { out = []; s += "|" + kernel(1200) + ":" + out.length; }
console.log("swap " + s);
"#,
    );
}

/// A FROZEN array's `push` throws TypeError in strict mode. Compared against
/// node's own message so the arm cannot quietly succeed where the spec throws.
#[test]
fn intpush_frozen_array_still_throws() {
    let src = r#""use strict";
var out = [];
function kernel(n) { var h = 0;
  for (var i = 0; i < n; i++) {
    var a = i > 2, b = i < 9e5, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0; if (b) h = (h + 2) | 0; if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0; if (e) h = (h + 5) | 0;
    if (i === 600) Object.freeze(out);
    out.push(h & 255);
  }
  return h; }
try { kernel(1200); } catch (err) { console.log("threw " + (err instanceof TypeError)); }
console.log("len " + out.length);
"#;
    assert_eq!(run_ok(src), node_output(src), "frozen push diverged:\n{src}");
}

/// Pushed values that leave the i32 range, where the helper's boxing rule
/// (Int if it fits, else the exact double) has to agree with node's numbers.
#[test]
fn intpush_parity_value_boxing() {
    assert_matches_node(
        r#"var out = [];
function kernel(n) { var h = 0;
  for (var i = 0; i < n; i++) {
    var a = i > 2, b = i < 9e5, c = (i & 1) === 0, d = i !== 5, e = (i & 3) === 1;
    if (a) h = (h + 1) | 0; if (b) h = (h + 2) | 0; if (c) h = (h + 3) | 0;
    if (d) h = (h + 4) | 0; if (e) h = (h + 5) | 0;
    out.push(i * 1000003 + 2147483000);
  }
  return h; }
var s = "";
for (var r = 0; r < 3; r++) { out = []; s += "|" + kernel(3000) + ":" + out[0] + ":" + out[2999] + ":" + (typeof out[2999]); }
console.log("boxing " + s);
"#,
    );
}

// ── M1 parity ───────────────────────────────────────────────────────────────

/// `k` DISJOINT bool temps in one INT region, k = 1..24. Past four this is the
/// linear scan; 21 is the real `tokenize` count.
fn disjoint_bools_case(k: usize, n: usize) -> String {
    let body: String = (0..k)
        .map(|j| format!("    if (c === {j}) {{ h = (h + {w}) | 0; }}\n", w = j * 3 + 1))
        .collect();
    format!(
        r#"function kernel(n) {{
  var h = 0, c = 0;
  for (var i = 0; i < n; i++) {{
    c = i % {m};
{body}    h = (h + 1) | 0;
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 3; r++) s += "|" + kernel({n});
console.log("bools-{k} " + s);
"#,
        m = k + 3
    )
}

#[test]
fn boolreuse_parity_disjoint_bools() {
    for k in 1..=24 {
        assert_matches_node(&disjoint_bools_case(k, 600));
    }
}

/// The W18 conditional-def defect shape, with a BOOL. `t`'s only def is behind
/// a branch, so its textual first occurrence is a def while a path from region
/// entry reads it undefined — it must keep a PRIVATE home and its entry load,
/// which is what `bool_shareable`'s `live_in` half buys.
#[test]
fn boolreuse_parity_conditional_def_bool() {
    for k in [0usize, 4, 8, 16] {
        let extra: String = (0..k)
            .map(|j| format!("    if ((i + {j}) % {m} === 0) {{ h = (h + 1) | 0; }}\n", m = k + 3))
            .collect();
        let src = format!(
            r#"function kernel(n) {{
  var h = 0, t = false, u = false;
  for (var i = 0; i < n; i++) {{
    if (i === 3) {{ t = true; }}
    if (i === 7) {{ u = true; }}
    if (t) h = (h + 5) | 0;
    if (u) h = (h + 9) | 0;
{extra}  }}
  return h + ":" + t + ":" + u;
}}
var s = "";
for (var r = 0; r < 3; r++) s += "|" + kernel(600);
console.log("conddef-{k} " + s);
"#
        );
        assert_matches_node(&src);
    }
}

/// Bools READ AFTER the region. Their frame slots are observable, so they may
/// never share a home — this is the case that fails if `bool_shareable` is ever
/// relaxed to region-local liveness.
#[test]
fn boolreuse_parity_read_after_the_loop() {
    for k in [2usize, 5, 9, 20] {
        let decls: String = (0..k).map(|j| format!("var b{j} = false; ")).collect();
        let defs: String =
            (0..k).map(|j| format!("    if (i === {t}) b{j} = true;\n", t = j * 5 + 1)).collect();
        let outs: String = (0..k).map(|j| format!(r#" + ":" + b{j}"#)).collect();
        let src = format!(
            r#"function kernel(n) {{
  var h = 0; {decls}
  for (var i = 0; i < n; i++) {{
{defs}    h = (h + (i & 7)) | 0;
  }}
  return h{outs};
}}
var s = "";
for (var r = 0; r < 3; r++) s += "|" + kernel(600);
console.log("readafter-{k} " + s);
"#
        );
        assert_matches_node(&src);
    }
}

// ── mode sweep ──────────────────────────────────────────────────────────────

/// The whole set again in child processes, once per switch mode. The two
/// off-switches are the load-bearing ones: `ZIPP_NO_INT_PUSH=1` and
/// `ZIPP_NO_BOOL_REUSE=1` must reproduce the pre-wave planner, and every mode
/// must answer identically. `ZIPP_GC_STRESS=1` is here because an append that
/// ran while homes were live is the sharpest failure this wave could have had.
#[test]
fn intpush_all_modes_answer_identically() {
    let cases: Vec<String> = vec![
        push_bools_case(1, 400),
        push_bools_case(5, 400),
        push_bools_case(8, 400),
        realloc_boundary_case(1000),
        realloc_boundary_case(4097),
        sibling_pin_case(2000),
        aliased_pin_case(2000),
        tokenizer_case(400),
        disjoint_bools_case(5, 600),
        disjoint_bools_case(21, 600),
    ];
    let modes: &[&[(&str, &str)]] = &[
        &[],
        &[("ZIPP_NO_INT_PUSH", "1")],
        &[("ZIPP_NO_BOOL_REUSE", "1")],
        &[("ZIPP_NO_INT_PUSH", "1"), ("ZIPP_NO_BOOL_REUSE", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GUARD_HOIST", "1")],
        &[("ZIPP_NO_MULTI_SPLIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for src in &cases {
        let want = node_output(src);
        for mode in modes {
            let got = child_output(src, mode);
            assert_eq!(got, want, "mode {mode:?} diverged for:\n{src}");
        }
    }
}

/// Run `src` in a child process under `env`, returning its stdout lines. Uses
/// the test binary itself as the worker (the pattern `bool_home_clobber.rs`
/// established) so no extra build artefact is needed.
fn child_output(src: &str, env: &[(&str, &str)]) -> Vec<String> {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = Command::new(exe);
    cmd.arg("--ignored").arg("--exact").arg("w20_push_child").arg("--nocapture");
    cmd.env("ZIPP_W20_SRC", src);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("child test process runs");
    assert!(out.status.success(), "child failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("W20OUT ").map(|s| s.to_string()))
        .collect()
}

#[test]
#[ignore = "worker: spawned by child_output with ZIPP_W20_SRC set"]
fn w20_push_child() {
    let Some(src) = std::env::var_os("ZIPP_W20_SRC") else { return };
    for line in run_any(&src.to_string_lossy()) {
        println!("W20OUT {line}");
    }
}

// ── mechanism gates ─────────────────────────────────────────────────────────

/// Run `src` in a child under `ZIPP_JITLOG=1`/`ZIPP_JITDECLINE=1` and hand back
/// its stderr.
fn jitlog_of(src: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = Command::new(exe);
    cmd.arg("--ignored").arg("--exact").arg("w20_push_child").arg("--nocapture");
    cmd.env("ZIPP_W20_SRC", src).env("ZIPP_JITLOG", "1").env("ZIPP_JITDECLINE", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("child test process runs");
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// B94's rule, applied to this wave: a green differential proves nothing until
/// the log shows the mechanism RAN. The tokenizer kernel must reach an `INT
/// region` compile with the package on, and must NOT with either rung off —
/// which is also the evidence that neither rung pays alone.
#[test]
fn intpush_mechanism_reaches_the_int_tier() {
    let src = tokenizer_case(400);
    let on = jitlog_of(&src, &[]);
    assert!(
        on.contains("[jit] INT region fn") && on.contains("compiled"),
        "package ON did not reach the INT tier; log was:\n{on}"
    );
    // Rung 1 off: the `CallMethod` decline is back and nothing else changes.
    let no_push = jitlog_of(&src, &[("ZIPP_NO_INT_PUSH", "1")]);
    assert!(
        no_push.contains("CallMethod (receiver not a pinned string/DataView)"),
        "ZIPP_NO_INT_PUSH=1 did not restore the CallMethod decline; log was:\n{no_push}"
    );
    // Rung 2 off: the region gets past `push` and declines one rung later, at
    // the bool pool — the decline string that appears on NO bench row today.
    let no_bools = jitlog_of(&src, &[("ZIPP_NO_BOOL_REUSE", "1")]);
    assert!(
        no_bools.contains("bool live-in, or bool gpr pool exhausted"),
        "ZIPP_NO_BOOL_REUSE=1 did not expose the bool-pool rung; log was:\n{no_bools}"
    );
}

/// The bool reuse actually reuses: 21 disjoint bools must plan onto fewer than
/// four registers, and the planner says so.
#[test]
fn boolreuse_mechanism_shares_registers() {
    let log = jitlog_of(&disjoint_bools_case(21, 600), &[]);
    assert!(
        log.contains("bool reuse: 21 bools ->") || log.contains("bools ->"),
        "no bool-reuse line in the log:\n{log}"
    );
}
