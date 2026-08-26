//! W16 BUG: the DOUBLE (regalloc) region emitter destroyed live `Bool` homes,
//! so an ordinary JS boolean local silently read back as the wrong boolean — or
//! as a `Number`.
//!
//! THE DEFECT, and why it is a CLASS. `BOOL_GPRS = [8, 9, 10, 11]`
//! (codegen/plan.rs) is planner-owned for the whole life of a compiled region:
//! it holds `Bool` register homes on every tier, `gpr_const` compare mirrors on
//! the xmm INT tier, and spare numeric i64 homes on the INT-GPR tier. Nothing
//! reloads any of them per iteration, so a body arm that scratches one corrupts
//! a live JS value for the rest of the region, across the backedge. The
//! register contract is stated once, on `BOOL_GPRS` itself.
//!
//! W14 found the first violation (the xmm INT tier's dense-Array tag check) and
//! fixed that one USE. W16 found two more, both on the DOUBLE tier and both
//! landing on the same `BOOL_GPRS[2] = r10`:
//!
//!   * `regalloc.rs`'s `Bitwise` arm materialised the INT64_MIN "integer
//!     indefinite" sentinel in r10 — so `|`, `&`, `^`, `<<`, `>>`, `>>>`,
//!     including the bare `x | 0` that int-flavoured JS writes on every line,
//!     each destroyed the region's third bool;
//!   * `emit_box_to_home` (emit.rs) tag-checked in r10, and the DOUBLE tier's
//!     dense-Array `GetIndex` arm calls it on EVERY element read. This is the
//!     exact twin of the W14 defect, one tier over.
//!
//! Both now scratch rdx, and so does `emit_int_entry_load`, which had the same
//! r10 habit and was kept safe only by every caller loading its bool homes last.
//!
//! WHAT IT LOOKED LIKE. `var b2 = false` read back as `NaN` (`typeof` = number):
//! the flush boxes a bool as `BOOL_TAG | home`, and `BOOL_TAG | i64::MIN` sets
//! the sign bit, which is no longer a NaN-boxed tag at all — the interpreter
//! read the bits back as a raw negative double. The dense-Array face was
//! quieter: the tag check leaves `bits >> 48` in the home, which is odd for an
//! Int element, so the bool read back `true` no matter what it was.
//!
//! THE SHAPE OF THIS FILE. Every case is generated on two axes and asserted
//! against `node -e`, never against `ZIPP_NOJIT=1` (an emitter bug that also
//! existed in the interpreter would pass that):
//!
//!   * ONE to FOUR live bools. This is the axis that matters: the bool bump
//!     allocator hands out `BOOL_GPRS` in order, so `k` live bools occupy
//!     exactly `r8..r(7+k)` and the sweep puts a live value in each of the four
//!     registers in turn. A body arm that scratches any one of them fails here.
//!   * the body op that does the scratching — each `Bitwise` operator, a dense
//!     all-Int Array read, a dense DOUBLE-element Array read, an `Int32Array`
//!     read, a `Float64Array` read, `%`, and a `none` control.
//!
//! run on the DOUBLE tier ([`boolhome_parity_double_matrix`]) and again on the
//! INT tiers ([`boolhome_parity_int_matrix`], the tier W14 fixed — a regression
//! gate for the same class over there).
//!
//! [`boolhome_no_fused_cmpjump_is_a_pure_fallback`] is the fifth soak finding:
//! `ZIPP_NO_FUSED_CMPJUMP=1` answered WRONG on programs the default got right.
//! It was never a second defect. Unfusing a compare turns a `JumpIfNotLt` into
//! a `Lt` + `JumpIfFalse` pair, which needs one more `Bool` — and that extra
//! bool is what pushes a live value into the register the body was scratching.
//! The case is kept because that switch is specified as a PURE FALLBACK and
//! this campaign runs one-binary A/B ablations through it.
//!
//! [`boolhome_all_modes_answer_identically`] re-runs the whole matrix in child
//! processes under each switch, and [`boolhome_mechanism_*`] reads the tier back
//! out of a child's `ZIPP_JITLOG`, so an admission change that quietly drops
//! these kernels to the MEM tier fails the suite instead of making it vacuous.

// The whole suite drives the x86-64 JIT tiers through a spawned CLI; in a
// no-JIT config (safe-sandbox) the kernels cannot reach the emitters under
// test — and the sandbox's own nesting limits reject the generated sources.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

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

/// The same program's output from `node -e`, so expectations are neither
/// hand-computed nor taken from our own interpreter.
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

// ── the matrix ──────────────────────────────────────────────────────────────

/// `(tag, prelude, per-iteration statement)` — the body op under test. Each one
/// is a thing a region BODY does that has historically needed a scratch gpr.
/// `t` is the value it produces; the kernel folds `t` back into the accumulator
/// so the op cannot be dead-code-eliminated.
struct Op {
    tag: &'static str,
    prelude: &'static str,
    stmt: &'static str,
}

/// The DOUBLE-tier ops. The kernel's accumulator is an f64, so the region
/// declines INT (`region_is_int=false`) and lands on `regalloc.rs`.
const DOUBLE_OPS: &[Op] = &[
    Op {
        tag: "none",
        prelude: "",
        stmt: "t = h + 1;",
    },
    // Every Bitwise operator: the arm whose INT64_MIN sentinel lived in r10.
    Op {
        tag: "or",
        prelude: "",
        stmt: "t = (h | 0);",
    },
    Op {
        tag: "and",
        prelude: "",
        stmt: "t = (h | 0) & 63;",
    },
    Op {
        tag: "xor",
        prelude: "",
        stmt: "t = (h | 0) ^ 5;",
    },
    Op {
        tag: "shl",
        prelude: "",
        stmt: "t = (h | 0) << 1;",
    },
    Op {
        tag: "shr",
        prelude: "",
        stmt: "t = (h | 0) >> 1;",
    },
    Op {
        tag: "ushr",
        prelude: "",
        stmt: "t = (h | 0) >>> 1;",
    },
    // Dense ordinary Array: the `GetIndex` arm that calls `emit_box_to_home`.
    Op {
        tag: "denseint",
        prelude: "var KI = [0, 3, 6, 2, 5, 1, 4, 7];",
        stmt: "t = KI[i & 7];",
    },
    Op {
        tag: "densedbl",
        prelude: "var KD = [0.5, 3.25, 6.5, 2.25, 5.5, 1.25, 4.5, 7.75];",
        stmt: "t = KD[i & 7];",
    },
    // The same read with the loop counter as the index — no `& 7`, hence NO
    // `Bitwise` anywhere in the region. This is what isolates the element read's
    // own clobber: the two rows above would fail on the `Bitwise` arm alone.
    Op {
        tag: "denseplain",
        prelude: "var KP = []; for (var q = 0; q < 128; q++) KP.push((q * 3) & 7);",
        stmt: "t = KP[i];",
    },
    Op {
        tag: "densedblplain",
        prelude: "var KQ = []; for (var q = 0; q < 128; q++) KQ.push(q * 0.25);",
        stmt: "t = KQ[i];",
    },
    // The Float64Array arm (raw f64 elements — no tag check), as a control.
    // (An `Int32Array` read has no DOUBLE-tier arm at all: `regalloc.rs`'s
    // `GetIndex` handles dense Arrays and kind-8 Float64Arrays, and anything
    // else declines with "element not a pinned TypedArray". It is covered on
    // the INT side instead.)
    Op {
        tag: "f64arr",
        prelude: "var TF = new Float64Array(8); for (var q = 0; q < 8; q++) TF[q] = q * 0.25;",
        stmt: "t = TF[i & 7];",
    },
    // `%` on the DOUBLE path (B113's arm — it documents rax/rcx/rdx/xmm0 too).
    Op {
        tag: "mod",
        prelude: "",
        stmt: "t = (h * 4) % 7;",
    },
];

/// The INT-tier ops. Everything stays int32 so `region_is_int` holds and the
/// region goes to `region_int.rs` / `region_int_gpr.rs` — the tier W14 fixed.
const INT_OPS: &[Op] = &[
    Op {
        tag: "none",
        prelude: "",
        stmt: "t = (h + 1) | 0;",
    },
    Op {
        tag: "or",
        prelude: "",
        stmt: "t = (h | 0);",
    },
    Op {
        tag: "and",
        prelude: "",
        stmt: "t = h & 63;",
    },
    Op {
        tag: "xor",
        prelude: "",
        stmt: "t = h ^ 5;",
    },
    Op {
        tag: "shl",
        prelude: "",
        stmt: "t = (h << 1) | 0;",
    },
    Op {
        tag: "shr",
        prelude: "",
        stmt: "t = h >> 1;",
    },
    Op {
        tag: "ushr",
        prelude: "",
        stmt: "t = (h >>> 1) | 0;",
    },
    Op {
        tag: "denseint",
        prelude: "var KI = [0, 3, 6, 2, 5, 1, 4, 7];",
        stmt: "t = KI[i & 7] | 0;",
    },
    Op {
        tag: "i32arr",
        prelude: "var TI = new Int32Array(8); for (var q = 0; q < 8; q++) TI[q] = (q * 3) & 7;",
        stmt: "t = TI[i & 7] | 0;",
    },
];

/// The `k` bool definitions, in allocation order — so bool `j` lands in
/// `BOOL_GPRS[j]`. Polarity alternates and two of the four are data-dependent,
/// so a stray bit pattern in a home cannot pass by accidentally agreeing with
/// the right answer (the dense-Array clobber, for one, always read back `true`).
///
/// Two spellings because the compare constant decides the tier: `t > 2.5` makes
/// `region_is_int` false, which would quietly move the INT matrix onto DOUBLE.
const BOOL_DEFS: [&str; 4] = [
    "b0 = i >= 4;",
    "b1 = i < 4;",
    "b2 = t > 2.5;",
    "b3 = t < 2.5;",
];
const BOOL_DEFS_INT: [&str; 4] = ["b0 = i >= 4;", "b1 = i < 4;", "b2 = t > 2;", "b3 = t < 2;"];

/// A kernel with `k` live-out bools around one body op.
///
/// The bools are DEFINED before the op and READ only after the loop, which is
/// what makes a clobber observable: the home has to survive the rest of the
/// iteration and the backedge. `typeof` is printed beside each one because the
/// two faces of this defect differ — a corrupted home reads back as the wrong
/// boolean in one and as a `Number` (NaN) in the other.
fn kernel(op: &Op, k: usize, dbl: bool, tag: &str) -> String {
    let init = (0..k)
        .map(|j| format!("var b{j} = false;\n  "))
        .collect::<String>();
    let table = if dbl { &BOOL_DEFS } else { &BOOL_DEFS_INT };
    let defs = table[..k]
        .iter()
        .map(|d| format!("    {d}\n"))
        .collect::<String>();
    let acc = if dbl {
        "h = h * 0.5 + t;"
    } else {
        "h = (h + t) | 0;"
    };
    let h0 = if dbl { "1.5" } else { "1" };
    let out = (0..k)
        .map(|j| format!(r#" + " " + (typeof b{j}) + ":" + b{j}"#))
        .collect::<String>();
    format!(
        r#"{prelude}
function kernel(n) {{
  var h = {h0}, i = 0, t = 0;
  {init}for (i = 0; i < n; i++) {{
{defs}    {stmt}
    {acc}
  }}
  return "" + h{out};
}}
var s = "";
for (var r = 0; r < 3; r++) s += "|" + kernel(120);
console.log("{tag} " + s);
"#,
        prelude = op.prelude,
        stmt = op.stmt,
    )
}

fn double_case(op: &Op, k: usize) -> String {
    kernel(op, k, true, &format!("dbl-{}-{k}", op.tag))
}

fn int_case(op: &Op, k: usize) -> String {
    kernel(op, k, false, &format!("int-{}-{k}", op.tag))
}

// ── the parity cases ────────────────────────────────────────────────────────

/// The DOUBLE tier, one to four live bools, every body op. This is the matrix
/// the two W16 defects live in.
///
/// Measured on the paired pre-fix binary (the two hunks reverted, everything
/// else identical), across this matrix and the INT one below in four modes —
/// 320 (case, mode) pairs — SEVENTY answered differently from node, and none
/// does now:
///   * all six `Bitwise` rows, at `k >= 3` in `base`/`nogpr`/`thr1` and at
///     `k = 2` under `nofused` (unfusing the loop guard adds one bool, which
///     shifts which value lands in `BOOL_GPRS[2]` — the fifth soak finding, in
///     one line);
///   * `denseplain` at `k >= 3`, the element read's own clobber with no
///     `Bitwise` anywhere in the region;
///   * `denseint`/`densedbl`/`f64arr`, whose `& 7` index makes them fail on
///     whichever arm gets there first.
/// The INT rows were all correct pre-fix, as expected: that tier's twin of this
/// clobber is what W14 fixed, and these rows are its regression gate.
#[test]
fn boolhome_parity_double_matrix() {
    for op in DOUBLE_OPS {
        for k in 1..=BOOL_DEFS.len() {
            assert_matches_node(&double_case(op, k));
        }
    }
}

/// The same sweep on the INT tiers — the regression gate for W14's fix, run
/// over all four `BOOL_GPRS` rather than the one shape that first exposed it.
#[test]
fn boolhome_parity_int_matrix() {
    for op in INT_OPS {
        for k in 1..=BOOL_DEFS.len() {
            assert_matches_node(&int_case(op, k));
        }
    }
}

/// The soak's most common signature, and the one with a consequence beyond
/// correctness: 12 of its 28 divergences were programs where the DEFAULT is
/// right and `ZIPP_NO_FUSED_CMPJUMP=1` ALONE is wrong.
///
/// It is the same defect from the other side. With the compare fused, this
/// kernel has two bools (r8, r9) and nothing lives in r10; unfusing the loop
/// guard adds a third, which lands in r10 — the register `(t0 | 0)` was
/// destroying. Pre-fix this printed `201,329,201`: right, wrong, right, the
/// middle call being the one the DOUBLE region served.
///
/// The point of pinning it is that the switch is documented as a PURE FALLBACK
/// and this campaign has run one-binary A/B ablations through it.
#[test]
fn boolhome_no_fused_cmpjump_is_a_pure_fallback() {
    const SRC: &str = r#"
var arr = [0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2];
function kernel(n) {
  var h = 1, i = 0, t0 = 1;
  var b0 = false, b1 = false;
  for (i = 0; i < n; i++) {
    b0 = (t0 | 0) !== 7;
    b1 = (t0 | 0) === 8;
    if (b0) h = (h + 1) | 0;
    if (b1) h = (h + 4) | 0;
    t0 = arr[i];
    h = (h + (t0 | 0)) | 0;
  }
  return (h ^ ((t0 | 0) * 3) ^ (b0 ? 17 : 0) ^ (b1 ? 18 : 0)) | 0;
}
var o = [];
for (var r = 0; r < 3; r++) o.push(kernel(120));
console.log(o.join(","));
"#;
    assert_matches_node(SRC);
}

/// The NaN face, kept verbatim from the fuzzer's minimized case: a `Bool` local
/// that observably holds a `Number` after its region compiles.
#[test]
fn boolhome_parity_live_out_bool_is_not_a_number() {
    const SRC: &str = r#"
var arr = [0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2, 5, 1, 4, 0, 3, 6, 2];
function kernel(n) {
  var h = 1, i = 0, t0 = 1;
  var b0 = false, b1 = false, b2 = false;
  for (i = 0; i < n; i++) {
    b0 = (t0 | 0) < 2;
    b1 = (t0 | 0) < 3;
    b2 = i >= 4;
    t0 = arr[((h * 3) & 63)];
    h = (h + (t0 | 0)) | 0;
    if (b0) h = (h + 2) | 0;
  }
  return typeof b2 + ":" + b2;
}
console.log(kernel(120));
"#;
    assert_matches_node(SRC);
}

// ── mode and mechanism pins ─────────────────────────────────────────────────

/// Every case above must answer identically in every mode.
///
/// `ZIPP_NO_FUSED_CMPJUMP=1` is the load-bearing one — it is the mode that owns
/// the fifth finding, and it also RE-PLANS every case in the matrix (one more
/// bool per unfused guard shifts which `BOOL_GPRS` register each value gets, so
/// it re-runs the whole sweep against a different assignment). `ZIPP_NO_GPR_HOMES=1`
/// pins the INT cases onto the xmm emitter that carried W14's defect, and
/// `ZIPP_JIT_THRESHOLD=1` compiles before the interpreter has warmed anything.
#[test]
fn boolhome_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_NO_FUSED_CMPJUMP", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = Command::new(&exe);
        cmd.arg("boolhome_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the boolhome_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

/// Run `src` in a child under `ZIPP_JITLOG=1` and hand back its stderr. The
/// child is this same test binary running [`boolhome_jitlog_child`], with the
/// program passed through the environment.
fn jitlog_of(src: &str) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let out = Command::new(&exe)
        .arg("boolhome_jitlog_child")
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_BOOLHOME_SRC", src)
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "jitlog child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// The worker for [`jitlog_of`]. A no-op unless `ZIPP_BOOLHOME_SRC` is set,
/// because the JIT switches are memoized latches: a mode IS a process.
#[test]
#[ignore = "worker: spawned by jitlog_of with ZIPP_BOOLHOME_SRC set"]
fn boolhome_jitlog_child() {
    let Some(src) = std::env::var_os("ZIPP_BOOLHOME_SRC") else {
        return;
    };
    let _ = run_ok(&src.to_string_lossy());
}

/// The DOUBLE matrix really does compile its kernel on the DOUBLE tier.
///
/// Without this the parity assertions could go green on a build where every one
/// of these loops quietly fell back to MEM — the vacuous state a future
/// admission change would otherwise reach silently.
#[test]
fn boolhome_mechanism_double_matrix_reaches_the_double_tier() {
    for op in DOUBLE_OPS {
        for k in 1..=BOOL_DEFS.len() {
            let src = double_case(op, k);
            let log = jitlog_of(&src);
            assert!(
                log.contains("DOUBLE region fn1 ["),
                "dbl-{}-{k}: the kernel's region is not DOUBLE — the case no \
                 longer reaches the emitter it is testing:\n{log}",
                op.tag
            );
            assert!(
                !log.contains("MEM region fn1 ["),
                "dbl-{}-{k}: the kernel dropped to the MEM tier:\n{log}",
                op.tag
            );
        }
    }
}

/// The INT matrix really does compile its kernel on an INT tier (either
/// emitter — `ZIPP_NO_GPR_HOMES=1` in the mode sweep is what pins the xmm one).
#[test]
fn boolhome_mechanism_int_matrix_reaches_the_int_tier() {
    for op in INT_OPS {
        for k in 1..=BOOL_DEFS.len() {
            let src = int_case(op, k);
            let log = jitlog_of(&src);
            assert!(
                log.contains("INT region fn1 ["),
                "int-{}-{k}: the kernel's region is not INT:\n{log}",
                op.tag
            );
            assert!(
                !log.contains("MEM region fn1 ["),
                "int-{}-{k}: the kernel dropped to the MEM tier:\n{log}",
                op.tag
            );
        }
    }
}
