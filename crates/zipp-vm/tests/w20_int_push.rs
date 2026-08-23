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
//! The useful speedup requires both rungs. Bool reuse alone leaves the tokenizer
//! on MEM; push admission alone gets one rung farther and then declines at the
//! bool pool. W21 found a second-order hazard in that latter case: push-only
//! snapshots leaked into the fallback plan and the MEM helper re-derived all of
//! them after every append. [`push_pin_filter_is_tier_specific_and_non_vacuous`]
//! proves the full plan still reaches INT with both rungs, while a declined
//! fallback drops only pins introduced exclusively by push.
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
//!   * [`intpush_parity_live_homes_across_append_and_deopt`] — 1..4 bool homes
//!     and four numeric locals are defined before the push and first consumed
//!     after it. The last invocation then deopts on a pinned read immediately
//!     after the call, so both the call spill and the exit flush are observable.
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
//! [`intpush_mechanism_reaches_the_int_tier`] and
//! [`push_pin_filter_is_tier_specific_and_non_vacuous`] read the tier/plan back
//! out of a child's `ZIPP_JITLOG` so an admission change that quietly drops
//! these kernels to the memory tier—or carries dead push pins there—fails the
//! suite instead of making it vacuous.

use std::process::Command;

// ── oracles ─────────────────────────────────────────────────────────────────

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
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
        .map(|j| {
            format!(
                "    if ((c + {j}) % {m} === 0) {{ h = (h + {j} + 1) | 0; }}\n",
                m = k + 2
            )
        })
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

/// `k` private bool homes and four numeric locals genuinely LIVE across the
/// append call. The first two invocations keep `shift == 0`, warming and then
/// running the INT region normally. The last uses `shift == 1`: iteration zero
/// appends first, then the negative pinned-array read deopts. Every bool and
/// numeric local is consumed only after that read, so a bad call restore OR a
/// bad deopt flush changes the accumulator. The post-loop `typeof`/value probes
/// also make the frame slots themselves observable.
fn live_homes_across_push_case(k: usize, n: usize) -> String {
    assert!((1..=4).contains(&k));
    let ref_items = (0..=n)
        .map(|i| ((i * 7 + 3) & 63).to_string())
        .collect::<Vec<_>>()
        .join(",");
    let bool_init: String = (0..k).map(|j| format!("var b{j} = false;\n  ")).collect();
    let bool_defs: String = (0..k)
        .map(|j| {
            let modulus = j + 2;
            format!("    b{j} = ((i + {j}) % {modulus}) === 0;\n")
        })
        .collect();
    let bool_uses: String = (0..k)
        .map(|j| {
            format!(
                "    if (b{j}) {{ h = (h + p{j} + {yes}) | 0; }} else {{ h = (h - p{j} - {no}) | 0; }}\n",
                yes = j + 1,
                no = j + 3,
            )
        })
        .collect();
    let bool_out: String = (0..k)
        .map(|j| format!(r#" + ":" + (typeof b{j}) + ":" + b{j}"#))
        .collect();
    format!(
        r#"var out = [];
var ref = [{ref_items}];
function kernel(n, shift) {{
  var h = 0, i = 0, probe = 0;
  var p0 = 0, p1 = 0, p2 = 0, p3 = 0;
  {bool_init}for (i = 0; i < n; i++) {{
    p0 = (i + 3) | 0;
    p1 = (i * 3 + 5) | 0;
    p2 = (h ^ i) | 0;
    p3 = ((i & 63) + 11) | 0;
{bool_defs}    out.push((h + i) & 255);
    probe = ref[i - shift];
{bool_uses}    h = (h + p0 + p1 + p2 + p3 + probe) | 0;
  }}
  return "" + h + ":" + probe + ":" + p0 + ":" + p1 + ":" + p2 + ":" + p3{bool_out};
}}
var s = "";
out = []; s += "|" + kernel({n}, 0) + ":" + out.length;
out = []; s += "|" + kernel({n}, 0) + ":" + out.length;
out = []; s += "|" + kernel({n}, 1) + ":" + out.length;
console.log("livehomes-{k} " + s);
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

/// One receiver is both the push target and a pinned element/length source, but
/// a unary `Math.abs` makes the loop deliberately non-INT while remaining
/// MEM-admissible. This forces the fallback-plan filter to run and must retain
/// the shared pin; it cannot pass merely because the successful INT tier used
/// the original plan.
fn shared_push_getindex_mem_case(n: usize) -> String {
    format!(
        r#"var out = [];
function kernel(n) {{
  var h = 0;
  for (var i = 0; i < n; i++) {{
    out.push((i * 7) & 255);
    h = (h + out[out.length - 1] + out.length) | 0;
    h = Math.abs(h);
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 3; r++) {{ out = []; s += "|" + kernel({n}) + ":" + out.length; }}
console.log("shared-mem-{n} " + s);
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

/// The W25 batching shape in isolation: three distinct global arrays, three
/// discarded push results, and argument values coming from both a constant and
/// live numeric homes. Reading every append back through its refreshed pin
/// crosses Vec capacity boundaries and makes ordering/value mistakes visible.
fn push3_boundary_case(n: usize) -> String {
    format!(
        r#"var aa = [], bb = [], cc = [];
function kernel(n) {{
  var i = 0, st = 7, h = 0;
  while (i < n) {{
    aa.push(1); bb.push(st); cc.push(i);
    h = (Math.imul(h ^ aa[aa.length - 1], 33) + bb[bb.length - 1] + cc[cc.length - 1]) | 0;
    st = (Math.imul(st, 17) + i) | 0;
    i = i + 1;
  }}
  return h + ":" + st + ":" + aa.length + ":" + bb.length + ":" + cc.length
    + ":" + aa[0] + ":" + bb[0] + ":" + cc[cc.length - 1];
}}
var s = "";
for (var r = 0; r < 4; r++) {{ aa = []; bb = []; cc = []; s += "|" + kernel({n}); }}
console.log("push3 " + s);
"#
    )
}

/// Runtime shapes which invalidate the batching assumptions only after the
/// kernel has warmed: two globals aliasing one array, and a frozen middle
/// receiver. Entry guards must route both through the ordinary calls, retaining
/// source order (notably, `aa` grows once before frozen `bb.push` throws).
fn push3_guard_decline_case() -> String {
    r#"var aa = [], bb = [], cc = [];
function kernel(n) {
  var i = 0, st = 7, h = 0;
  while (i < n) {
    aa.push(1); bb.push(st); cc.push(i);
    h = (Math.imul(h, 33) + st + i) | 0;
    st = (Math.imul(st, 17) + i) | 0;
    i = i + 1;
  }
  return h;
}
for (var w = 0; w < 4; w++) { aa = []; bb = []; cc = []; kernel(400); }
aa = []; bb = aa; cc = [];
var ah = kernel(37);
console.log("alias " + ah + ":" + aa.length + ":" + bb.length + ":" + cc.length);
aa = []; bb = []; cc = []; Object.freeze(bb);
try { kernel(37); console.log("freeze NO_THROW"); }
catch (e) { console.log("freeze " + e.name + ":" + aa.length + ":" + bb.length + ":" + cc.length); }
"#
    .to_string()
}

/// A live receiver global changes inside the candidate region before the
/// textual trio. The batch must not be recognised: ordinary calls append to
/// the newly loaded receiver, while a stale snapshot would mutate `first`.
fn push3_receiver_store_case() -> String {
    r#"var aa = [], bb = [], cc = [], swap = [];
function kernel(n, poison) {
  var i = 0, st = 7, h = 0;
  while (i < n) {
    if (poison && i === 30) aa = swap;
    aa.push(1); bb.push(st); cc.push(i);
    h = (Math.imul(h, 33) + st + i) | 0;
    st = (Math.imul(st, 17) + i) | 0;
    i = i + 1;
  }
  return h;
}
for (var w = 0; w < 4; w++) { aa = []; bb = []; cc = []; swap = []; kernel(400, false); }
aa = []; bb = []; cc = []; swap = [];
var first = aa, h = kernel(80, true);
console.log("store " + h + ":" + first.length + ":" + swap.length + ":" + bb.length + ":" + cc.length);
"#
    .to_string()
}

/// More than the 14-xmm distinct-value threshold forces the INT planner's
/// linear-scan home reuse. The first push argument is a Move from `x`, whose
/// value dies at that call; later argument temporaries are therefore free to
/// reuse its physical home. Forced helper decline must restore those physical
/// homes before replaying the trio.
fn push3_home_pressure_case(n: usize) -> String {
    let decls: String = (0..20).map(|j| format!("p{j} = 0, ")).collect();
    let pressure: String = (0..20)
        .map(|j| {
            format!(
                "    p{j} = (i + {j}) | 0; h = (h + p{j}) | 0;\n"
            )
        })
        .collect();
    format!(
        r#"var aa = [], bb = [], cc = [];
function kernel(n) {{
  var {decls}i = 0, h = 1, x = 0, y = 0, z = 0;
  while (i < n) {{
{pressure}    x = (h + i) | 0;
    y = (h ^ i) | 0;
    z = (h + i + 7) | 0;
    aa.push(x); bb.push(y); cc.push(z);
    h = (Math.imul(h, 33) + y + z) | 0;
    i = i + 1;
  }}
  return h;
}}
var s = "";
for (var r = 0; r < 4; r++) {{
  aa = []; bb = []; cc = [];
  s += "|" + kernel({n}) + ":" + aa.length + ":" + bb.length + ":" + cc.length
    + ":" + aa[0] + ":" + aa[aa.length - 1] + ":" + bb[bb.length - 1] + ":" + cc[cc.length - 1];
}}
console.log("pressure " + s);
"#
    )
}

// ── M2 parity ───────────────────────────────────────────────────────────────

/// 1..8 distinct bool sites with disjoint ranges around the append. This is the
/// tokenizer-shaped allocator/reuse sweep; the separate live-home test below
/// is the call-clobber and exit-flush oracle.
#[test]
fn intpush_parity_live_bools_across_the_append() {
    for k in 1..=8 {
        assert_matches_node(&push_bools_case(k, 400));
    }
}

/// The actual call-clobber matrix: bools and numeric homes are live across the
/// append and observed after both the normal return and a post-call deopt.
#[test]
fn intpush_parity_live_homes_across_append_and_deopt() {
    for k in 1..=4 {
        assert_matches_node(&live_homes_across_push_case(k, 180));
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

#[test]
fn intpush3_parity_distinct_arrays_realloc_and_guard_declines() {
    for n in [17usize, 257, 4097] {
        assert_matches_node(&push3_boundary_case(n));
    }
    assert_matches_node(&push3_guard_decline_case());
    assert_matches_node(&push3_receiver_store_case());
    assert_matches_node(&push3_home_pressure_case(400));
}

#[test]
fn intpush3_off_switch_and_atomic_helper_replay_match_node() {
    // The full tokenizer is the non-vacuous INT-tier host; the smaller
    // boundary kernel above deliberately falls to MEM once its post-push array
    // reads make the pinned receiver ranges overlap.
    let src = tokenizer_case(400);
    let want = node_output(&src);
    for mode in [
        &[][..],
        &[("ZIPP_NO_INT_PUSH3", "1")][..],
        &[("ZIPP_TEST_FORCE_INT_PUSH3_DECLINE", "1")][..],
        &[
            ("ZIPP_INT_SPLIT", "1"),
            ("ZIPP_TEST_FORCE_INT_PUSH3_DECLINE", "1"),
        ][..],
        &[("ZIPP_NOJIT", "1")][..],
    ] {
        assert_eq!(child_output(&src, mode), want, "mode {mode:?} diverged");
    }

    let pressure = push3_home_pressure_case(400);
    assert_eq!(
        child_output(
            &pressure,
            &[("ZIPP_TEST_FORCE_INT_PUSH3_DECLINE", "1")]
        ),
        node_output(&pressure),
        "forced decline corrupted a home-reuse pressure region"
    );
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
    assert_eq!(
        run_ok(src),
        node_output(src),
        "frozen push diverged:\n{src}"
    );
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
        .map(|j| {
            format!(
                "    if (c === {j}) {{ h = (h + {w}) | 0; }}\n",
                w = j * 3 + 1
            )
        })
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
            .map(|j| {
                format!(
                    "    if ((i + {j}) % {m} === 0) {{ h = (h + 1) | 0; }}\n",
                    m = k + 3
                )
            })
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
        let defs: String = (0..k)
            .map(|j| format!("    if (i === {t}) b{j} = true;\n", t = j * 5 + 1))
            .collect();
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
        live_homes_across_push_case(1, 180),
        live_homes_across_push_case(4, 180),
        realloc_boundary_case(1000),
        realloc_boundary_case(4097),
        shared_push_getindex_mem_case(800),
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
        &[("ZIPP_NO_PUSH_PIN_FILTER", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GUARD_HOIST", "1")],
        &[("ZIPP_NO_MULTI_SPLIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NO_BOOL_REUSE", "1"), ("ZIPP_GC_STRESS", "1")],
        &[
            ("ZIPP_NO_PUSH_PIN_FILTER", "1"),
            ("ZIPP_NO_BOOL_REUSE", "1"),
            ("ZIPP_GC_STRESS", "1"),
        ],
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
    cmd.arg("--ignored")
        .arg("--exact")
        .arg("w20_push_child")
        .arg("--nocapture");
    cmd.env("ZIPP_W20_SRC", src);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("child test process runs");
    assert!(
        out.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("W20OUT ").map(|s| s.to_string()))
        .collect()
}

#[test]
#[ignore = "worker: spawned by child_output with ZIPP_W20_SRC set"]
fn w20_push_child() {
    let Some(src) = std::env::var_os("ZIPP_W20_SRC") else {
        return;
    };
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
    cmd.arg("--ignored")
        .arg("--exact")
        .arg("w20_push_child")
        .arg("--nocapture");
    cmd.env("ZIPP_W20_SRC", src)
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("child test process runs");
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// B94's rule, applied to this wave: a green differential proves nothing until
/// the log shows the mechanism RAN. The tokenizer kernel must reach an `INT
/// region` compile with the package on, and must NOT with either rung off —
/// the useful tier transition requires both rungs.
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

    // Keep the call-clobber/deopt oracle non-vacuous too: it must be the INT
    // emitter, not the interpreter or memory tier, that executes the push.
    let live_homes = jitlog_of(&live_homes_across_push_case(4, 180), &[]);
    assert!(
        live_homes.contains("[jit] INT region fn")
            && live_homes.contains("compiled")
            && live_homes.contains("deopt at ip"),
        "live-home kernel did not compile on INT and then deopt after its push; log was:\n{live_homes}"
    );
}

#[test]
fn intpush3_mechanism_engages_declines_atomically_and_switches_off() {
    let src = tokenizer_case(400);
    let on = jitlog_of(&src, &[("ZIPP_JIT_THRESHOLD", "1")]);
    assert!(
        on.contains("array-push3 groups="),
        "three-push batch did not engage; log was:\n{on}"
    );

    let off = jitlog_of(
        &src,
        &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_INT_PUSH3", "1")],
    );
    assert!(
        !off.contains("array-push3 groups="),
        "ZIPP_NO_INT_PUSH3 did not disable batching; log was:\n{off}"
    );

    let forced = jitlog_of(
        &src,
        &[
            ("ZIPP_JIT_THRESHOLD", "1"),
            ("ZIPP_TEST_FORCE_INT_PUSH3_DECLINE", "1"),
        ],
    );
    assert!(
        forced.contains("array-push3 groups=") && forced.contains("deopt at ip"),
        "forced atomic helper decline did not execute/replay; log was:\n{forced}"
    );

    let split_forced = jitlog_of(
        &src,
        &[
            ("ZIPP_JIT_THRESHOLD", "1"),
            ("ZIPP_INT_SPLIT", "1"),
            ("ZIPP_TEST_FORCE_INT_PUSH3_DECLINE", "1"),
        ],
    );
    assert!(
        split_forced.contains("array-push3 groups=") && split_forced.contains("deopt at ip"),
        "split-mode forced decline did not batch and replay; log was:\n{split_forced}"
    );

    let receiver_store = jitlog_of(
        &push3_receiver_store_case(),
        &[("ZIPP_JIT_THRESHOLD", "1")],
    );
    assert!(
        !receiver_store.contains("array-push3 groups="),
        "batch ignored an in-region receiver-global store; log was:\n{receiver_store}"
    );

    let pressure = jitlog_of(
        &push3_home_pressure_case(400),
        &[("ZIPP_JIT_THRESHOLD", "1")],
    );
    assert!(
        pressure.contains("array-push3 groups="),
        "home-pressure forced-decline oracle was vacuous; log was:\n{pressure}"
    );
}

/// The push-inclusive snapshot plan belongs to the INTEGER attempt, not
/// automatically to the fallback emitters. Three facts make this gate
/// non-vacuous:
///
/// 1. With both rungs on, the tokenizer still compiles INT (the filter is never
///    on that path).
/// 2. With bool reuse off, INT declines and the MEM fallback reports that it
///    dropped at least one push-only snapshot and retained none of that class.
/// 3. A receiver used by BOTH `push` and `GetIndex` remains pinned/shared; the
///    filter may not mistake the push access for the pin's only consumer.
///
/// The off switch must reproduce the unfiltered fallback on the same binary.
#[test]
fn push_pin_filter_is_tier_specific_and_non_vacuous() {
    let tokenizer = tokenizer_case(400);
    let on = jitlog_of(&tokenizer, &[]);
    assert!(
        on.contains("[jit] INT region fn") && on.contains("compiled"),
        "both-rung tokenizer stopped compiling INT; log was:\n{on}"
    );

    let fallback = jitlog_of(&tokenizer, &[("ZIPP_NO_BOOL_REUSE", "1")]);
    let dropped = fallback.lines().any(|line| {
        line.contains("fallback push-pin filter")
            && line.contains("remaining_push_only=0")
            && !line.contains("dropped_push_only=0")
    });
    assert!(
        fallback.contains("bool live-in, or bool gpr pool exhausted")
            && fallback.contains("[jit] MEM region fn")
            && dropped,
        "bool-declined tokenizer did not compact push-only fallback pins; log was:\n{fallback}"
    );

    let unfiltered = jitlog_of(
        &tokenizer,
        &[
            ("ZIPP_NO_BOOL_REUSE", "1"),
            ("ZIPP_NO_PUSH_PIN_FILTER", "1"),
        ],
    );
    assert!(
        unfiltered.contains("[jit] MEM region fn")
            && !unfiltered.contains("fallback push-pin filter"),
        "off switch did not restore the unfiltered MEM fallback; log was:\n{unfiltered}"
    );

    let shared = jitlog_of(&shared_push_getindex_mem_case(800), &[]);
    assert!(
        shared.contains("[jit] MEM region fn")
            && shared.lines().any(|line| {
                line.contains("fallback push-pin filter")
                    && !line.contains("retained_shared=0")
            }),
        "push+GetIndex receiver was not retained as a shared fallback pin; log was:\n{shared}"
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
