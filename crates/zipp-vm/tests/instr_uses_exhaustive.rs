//! W17 BUG CLASS: `instr_uses` — the JIT planner's operand table — ended in a
//! catch-all `_ => vec![]`, so every opcode nobody had remembered to list was
//! silently reported as READING NOTHING. Ten-plus JS shapes answered wrong.
//!
//! THE MECHANISM. `read_outside` (codegen/plan_region.rs) runs `instr_uses` over
//! the WHOLE enclosing function to decide which registers' frame slots are still
//! observed after the region. A register that nothing outside appears to read is
//! `shareable`: it needs no entry load (an early exit flushing a stale value into
//! its slot is "unobservable"), so it is dropped from `live_in_regs` — whose own
//! doc states the invariant "every flushed home is entry-loaded" — while staying
//! in `num_regs`. `flush_exit` then writes a home NOTHING EVER FILLED into the
//! frame slot. With the catch-all, "nothing outside reads it" was a false fact
//! for every opcode the table had not been told about: it named 36 of the 221
//! `Instr` variants, and the other 185 fell through.
//!
//! THE ORIGINAL REPRO — nine lines, no arrays, no deopt:
//!
//!     function kernel(n) {
//!       var x;                                  // stays undefined
//!       var s = 0;
//!       for (var i = 0; i < n; i++) { s += i; if (i === 999999) { x = 1; } }
//!       return typeof x;                        // "number"  (node: "undefined")
//!     }
//!
//! `TypeOf` was absent from the table, so `x`'s only post-region use was
//! invisible and its unfilled home was flushed over an `undefined` local.
//!
//! THE FIX is not the `TypeOf` arm. `instr_uses` is now EXHAUSTIVE — no `_` arm,
//! one explicit arm per `Instr` variant — so an opcode added without declaring
//! its operands is a BUILD ERROR instead of a silent wrong answer.
//!
//! THE SHAPE OF THIS FILE. One case per opcode class the audit newly declares
//! operands for. Every case is the same kernel: a local whose ONLY in-region def
//! sits on a branch that never runs (so nothing fills its home), read after the
//! loop by exactly the op under test. The initial value is chosen so the correct
//! answer differs from what a flushed empty home produces. Expectations come
//! from `node -e`, never from `ZIPP_NOJIT=1` — a planner bug that also existed in
//! the interpreter would pass that. 23 of the 37 cases below answered WRONG at
//! HEAD (two of them by THROWING a TypeError node does not throw — `"a" in x`
//! and `Math.max(...x)` on a register the flush had turned into a number); the
//! other 14 pin a declaration whose reachability depends on which register the
//! allocator picked, and would otherwise rot.
//!
//! [`iuses_mechanism_every_case_compiles_a_region`] reads the tier back out of a
//! child's `ZIPP_JITLOG`, so an admission change that quietly stops compiling
//! these loops fails the suite instead of making it vacuous, and
//! [`iuses_the_operand_table_has_no_catch_all_arm`] fails if the `_` arm is ever
//! restored — the exhaustiveness is the fix, the `TypeOf` arm is one line of it.

use std::process::Command;

// ── oracles ─────────────────────────────────────────────────────────────────

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}\nfor:\n{src}",
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

// ── the matrix ──────────────────────────────────────────────────────────────

/// One post-region observation of a register the region homed.
///
/// `init` is the value the local carries INTO the loop — the value the frame
/// slot must still hold on the way out. It is picked per case so that the right
/// answer and the answer an empty (all-zero) home produces differ.
/// `epilogue` holds any helper the observation calls; it goes AFTER `kernel`.
struct Case {
    /// The `Instr` variant(s) this case pins an arm for.
    tag: &'static str,
    init: &'static str,
    /// The observation, as one expression the kernel returns.
    obs: &'static str,
    /// …or, when the observation needs several statements, the whole tail of
    /// the kernel (already indented, ending in a `return`). Overrides `obs`.
    tail: &'static str,
    /// Any helper the observation calls. Emitted AFTER `kernel` (declarations
    /// hoist) so the kernel's proto index does not move.
    epilogue: &'static str,
}

const fn c(tag: &'static str, init: &'static str, obs: &'static str) -> Case {
    Case {
        tag,
        init,
        obs,
        tail: "",
        epilogue: "",
    }
}

const CASES: &[Case] = &[
    // ── unary value ops ──
    c("TypeOf", "undefined", "typeof x"),
    c("TypeOfIs", "undefined", "(typeof x === \"undefined\")"),
    c("TypeOfSame", "undefined", "(typeof x === typeof i)"),
    c("ToNum", "undefined", "(+x)"),
    c("BitNot", "5", "(~x)"),
    c("Not", "5", "(!x)"),
    c("ToStr", "\"keep\"", "`${x}`"),
    c("IsArray", "[1,2]", "Array.isArray(x)"),
    c("JsonParse", "\"[7]\"", "JSON.parse(x)[0]"),
    c("JsonStringify", "{a:1}", "JSON.stringify(x)"),
    // ── binary ops that accept any type ──
    c("LooseEq", "undefined", "(x == null)"),
    c("LooseNe", "undefined", "(x != null)"),
    c("Pow", "undefined", "(x ** 2)"),
    // ── call-family argument WINDOWS ──
    Case {
        tag: "Call",
        init: "\"keep\"",
        obs: "id(x)",
        tail: "",
        epilogue: "function id(v) { return v; }",
    },
    // A transparent argument list fuses a member call to `CallMethod`; the
    // trailing member-read argument keeps the next two on the captured
    // `GetProp` + `CallWithThis` / `RegExpMethod` lowering instead.
    c("CallMethod", "\"keep\"", "x.concat(x)"),
    c(
        "CallWithThis",
        "\"keep\"",
        "String.prototype.concat.call(x, x.length)",
    ),
    c("RegExpMethod", "\"keep\"", "/keep/.test(x, x.length)"),
    c("Print", "\"keep\"", "(console.log(x), 0)"),
    c("NewArray", "\"keep\"", "JSON.stringify([x])"),
    Case {
        tag: "New",
        init: "\"keep\"",
        obs: "(new Box(x)).v",
        tail: "",
        epilogue: "function Box(v) { this.v = v; }",
    },
    c("StaticFn", "5", "Number.isInteger(x)"),
    c("GlobalFn", "\"12\"", "parseInt(x, 10)"),
    c("MathSpread", "[1,9,3]", "Math.max(...x)"),
    c("DateNew", "5", "(new Date(2020, x)).getMonth()"),
    // ── object / array construction ──
    c("ArrayAppend", "\"keep\"", "JSON.stringify([].concat([x]))"),
    c("InitDataProp", "\"keep\"", "JSON.stringify({p: x})"),
    c("ObjectSpread", "\"keep\"", "JSON.stringify({...{a: x}})"),
    c("NewRegExp", "\"ab\"", "(new RegExp(x)).source"),
    // ── property probes and deletes ──
    c(
        "DeleteProp",
        "{a:1}",
        "(delete x.a) + \":\" + JSON.stringify(x)",
    ),
    c(
        "DeleteIndex",
        "{a:1}",
        "(delete x[\"a\"]) + \":\" + JSON.stringify(x)",
    ),
    c("HasProp", "{a:1}", "(\"a\" in x)"),
    c("InstanceOf", "[1]", "(x instanceof Array)"),
    c("LenOf", "\"abcd\"", "x.length"),
    c("ObjectKeys", "{a:1,b:2}", "Object.keys(x).join(\",\")"),
    c("ToObject", "\"ab\"", "Object(x).length"),
    c(
        "CheckCoercible",
        "{m: function () { return \"ok\"; }}",
        "x.m()",
    ),
    // ── control flow and iteration ──
    c("IterPrime", "[1,2,3]", "Array.from(x).length"),
    // The three below are spelled as multi-statement tails rather than one
    // expression, and WITHOUT a helper IIFE on purpose: a nested closure inside
    // `kernel` drops its loop to the MEM tier, which never consults the operand
    // table at all — the case would still pass and prove nothing.
    Case {
        tag: "Throw",
        init: "\"keep\"",
        obs: "",
        tail: "  try { throw x; } catch (e) { return e; }\n",
        epilogue: "",
    },
    Case {
        tag: "IterNext",
        init: "[3,4]",
        obs: "",
        tail: "  var t = 0;\n  for (const v of x) t += v;\n  return t;\n",
        epilogue: "",
    },
    Case {
        tag: "ArrayRest",
        init: "[3,4,5]",
        obs: "",
        tail: "  const [, ...r] = x;\n  return r.join(\"|\");\n",
        epilogue: "",
    },
];

/// The kernel. `x`'s ONLY in-region def is on a branch that never runs, so
/// nothing fills its home; the loop body is a plain integer accumulator so the
/// region lands on the INT tier. `n` is small on purpose — the OSR threshold is
/// well under 200, and every case pays a `node` spawn.
fn source(case: &Case) -> String {
    let tail = if case.tail.is_empty() {
        format!("  return {};\n", case.obs)
    } else {
        case.tail.to_string()
    };
    format!(
        "function kernel(n) {{\n  \
         var x = {init};\n  \
         var s = 0;\n  \
         for (var i = 0; i < n; i++) {{\n    \
         s += i;\n    \
         if (i === 999999) {{ x = 1; }}\n  \
         }}\n\
         {tail}\
         }}\n\
         console.log(kernel(200));\n\
         {epilogue}\n",
        init = case.init,
        epilogue = case.epilogue,
    )
}

/// Every observation must read back the value the interpreter left in the slot.
#[test]
fn iuses_parity_post_region_observations() {
    for case in CASES {
        let src = source(case);
        assert_eq!(
            run_ok(&src),
            node_output(&src),
            "{}: zipp != node — the region flushed a home nothing filled over `x`\n{src}",
            case.tag
        );
    }
}

/// The nine-line repro from the wave report, kept verbatim as its own case.
#[test]
fn iuses_parity_typeof_after_a_hot_loop() {
    let src = "function kernel(n) {\n  \
               var x;\n  \
               var s = 0;\n  \
               for (var i = 0; i < n; i++) {\n    \
               s += i;\n    \
               if (i === 999999) { x = 1; }\n  \
               }\n  \
               return typeof x;\n\
               }\n\
               console.log(kernel(200));\n";
    assert_eq!(run_ok(src), node_output(src), "the W17 repro:\n{src}");
    assert_eq!(run_ok(src), vec!["undefined".to_string()]);
}

/// A register read after the region ONLY through a closure capture. The capture
/// sources live in the CALLEE proto's upvalue list, which `instr_uses` cannot
/// see from an `&Instr`; the `MakeCell` that boxes the local is what declares
/// them instead.
///
/// FORWARD-DEFENSIVE, and deliberately so: it passed before the fix too, and it
/// cannot fail today. A captured local IS a cell, so the loop reads and writes
/// it with `CellGet`/`CellSet`, which the numeric planner declines — this kernel
/// runs on the MEM tier, which never consults the operand table. The case is
/// here so the reasoning above has an executable statement attached to it if a
/// future admission change ever puts a cell-carrying loop on a numeric tier.
#[test]
fn iuses_parity_capture_only_read_after_the_region() {
    let src = "function kernel(n) {\n  \
               var cap = \"keep\";\n  \
               var s = 0;\n  \
               for (var i = 0; i < n; i++) {\n    \
               s += i;\n    \
               if (i === 999999) { cap = 1; }\n  \
               }\n  \
               return function () { return cap; };\n\
               }\n\
               console.log(kernel(200)());\n";
    assert_eq!(run_ok(src), node_output(src), "capture-only read:\n{src}");
}

// ── mode and mechanism pins ─────────────────────────────────────────────────

/// Every case must answer identically in every mode. `ZIPP_NOJIT=1` is the
/// interpreter reference; the tier-forcing switches re-plan each kernel onto a
/// different allocation, and `ZIPP_JIT_THRESHOLD=1` compiles before the
/// interpreter has warmed anything (so the region's very first execution is the
/// native one).
#[test]
fn iuses_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 5] = [
        &[("ZIPP_NOJIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_WT_SHARE", "1")],
        &[("ZIPP_NO_GLOB_RANGE", "1")],
    ];
    for mode in modes {
        let mut cmd = Command::new(&exe);
        cmd.arg("iuses_parity_");
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
            "the iuses_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

/// Run `src` in a child under `ZIPP_JITLOG=1` and hand back its stderr.
fn jitlog_of(src: &str) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let out = Command::new(&exe)
        .arg("iuses_jitlog_child")
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_IUSES_SRC", src)
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

/// The worker for [`jitlog_of`]. A no-op unless `ZIPP_IUSES_SRC` is set,
/// because the JIT switches are memoized latches: a mode IS a process.
#[test]
#[ignore = "worker: spawned by jitlog_of with ZIPP_IUSES_SRC set"]
fn iuses_jitlog_child() {
    let Some(src) = std::env::var_os("ZIPP_IUSES_SRC") else {
        return;
    };
    let _ = run_ok(&src.to_string_lossy());
}

/// Every case's loop really does compile a region on a NUMERIC tier — INT or
/// DOUBLE, the two `plan_region` serves and the ones the defect lived on. The
/// kernel's `for` is the only loop hot enough to OSR in these programs, so any
/// numeric-tier region in the log is that loop's. Without this the parity
/// assertions could go green on a build where none of these loops was ever
/// handed to the planner at all — a MEM region never consults the operand table.
#[test]
fn iuses_mechanism_every_case_compiles_a_region() {
    for case in CASES {
        let log = jitlog_of(&source(case));
        assert!(
            log.contains("INT region fn") || log.contains("DOUBLE region fn"),
            "{}: the kernel's loop no longer compiles a numeric-tier region — \
             this case no longer reaches the planner it is testing:\n{log}",
            case.tag
        );
    }
}

/// The exhaustiveness IS the fix. `instr_uses` must have no catch-all arm: with
/// one, a new `Instr` variant compiles clean and is silently reported as reading
/// nothing, which is the defect this suite exists for. The compiler enforces
/// this on every build; this test is what keeps someone from taking the
/// enforcement back out with a one-line `_ => vec![]`.
#[test]
fn iuses_the_operand_table_has_no_catch_all_arm() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/codegen/plan_region.rs");
    let src = std::fs::read_to_string(path)
        .expect("plan_region.rs is readable")
        .replace("\r\n", "\n");
    let start = src
        .find("pub(crate) fn instr_uses(i: &Instr) -> Vec<u16> {")
        .expect("instr_uses is still declared in plan_region.rs");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("instr_uses has a closing brace");
    let body = &body[..end];
    assert!(
        !body.contains("_ =>"),
        "instr_uses has grown a catch-all arm again. Every `Instr` variant needs \
         an explicit arm: the catch-all reports an unlisted opcode as reading \
         NOTHING, which is what made `typeof x` answer \"number\" after a hot loop."
    );
}
