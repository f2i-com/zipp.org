//! The pinned-access receiver's frame slot, and the exits that read it.
//!
//! A receiver register in `RegionPlan::ta_recv_regs` — one defined by a single
//! `LoadGlobal` and used only as the `obj` of a pinned element / `charCodeAt` /
//! DataView access — gets NO numeric home: all three register emitters read the
//! live receiver through the pin's own source, so the register itself is never
//! needed in the body. Its `LoadGlobal` therefore used to emit NOTHING.
//!
//! That was a silent wrong answer (W16 defect 3). Every pinned access carries
//! guards that DEOPT **at their own ip** — an out-of-range or negative index, a
//! non-Int element tag, a hole, an identity miss — and the interpreter then
//! re-executes that access, reading the receiver out of `regs[obj]`. `flush_exit`
//! cannot repair the slot: it writes NUMERIC homes back, and this register has
//! none. So the slot held whatever the interpreter had last left there, which for
//! an access the interpreter had never reached — a cold `if` body first entered
//! under compiled code — is the frame's initial `undefined`:
//!
//! ```text
//! var a = [1, 2, 3];
//! function kernel(n) { var t = 4;
//!   for (var i = 0; i < n; i++) { if (i === 17) { t = a[9999]; } }
//!   return t; }
//! typeof kernel(20)   // node: "undefined".  zipp: threw TypeError.
//! ```
//!
//! Reading past the end of an array is ordinary JS and `typeof x === "undefined"`
//! is the ordinary way to test the result, so this reached any program doing
//! bounds-checked-by-value element access. The receiver's `LoadGlobal` now stores
//! the object into the register's frame slot on every tier, which is the
//! invariant the B94 split receiver already documents and the sibling suite
//! `split_recv_writethrough.rs` already pins: **the memory slot is authoritative,
//! so every exit is correct without knowing which path reached it.**
//!
//! The eight parity cases below all THREW or answered wrong before the fix, and
//! between them cover every emitter that has the elided-`LoadGlobal` arm (INT
//! xmm, INT-GPR, DOUBLE) and four different guards that reach the deopt (unsigned
//! bounds, negative index, an out-of-bounds TypedArray STORE — a spec no-op — and
//! a `charCodeAt` past the end of a pinned flat-ASCII string). `coldrecv_mechanism_*`
//! reads the plan back out of a child's `ZIPP_JITLOG` and fails if a shape stops
//! carrying a pinned receiver, or stops taking a native exit after the receiver's
//! `LoadGlobal` — i.e. if it stops covering the hazard at all.

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
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

// ───────────────────────────── the shapes ─────────────────────────────
//
// Every one is the same skeleton: a counted loop whose body the interpreter runs
// to the OSR threshold, plus ONE conditional read that the interpreter never
// reaches before the region compiles and that misses its guard when the compiled
// body finally takes it.

/// The reported case. A dense all-Int Array pins as `ARR_INT_PIN_KIND` and hosts
/// on the INT (xmm-home) tier; `a[9999]` misses the unsigned bounds guard.
const DENSE_ARRAY_OOB: &str = r#"
var a = [1, 2, 3];
function kernel(n) {
  var t = 4;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = a[9999]; }
  }
  return t;
}
console.log(typeof kernel(20));
"#;

/// Same shape over an `Int32Array` — the original pinned-TypedArray element
/// path, whose read is a raw `movsxd` rather than a boxed `Value` load.
const INT32ARRAY_OOB: &str = r#"
var a = new Int32Array(3);
a[0] = 1; a[1] = 2; a[2] = 3;
function kernel(n) {
  var t = 4;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = a[9999]; }
  }
  return t;
}
console.log(typeof kernel(20));
"#;

/// A `Float64Array` receiver: f64 elements route the region to the DOUBLE tier
/// (`regalloc.rs`), which has its own copy of the elided-`LoadGlobal` arm.
const FLOAT64ARRAY_OOB: &str = r#"
var a = new Float64Array(3);
a[0] = 1.5; a[1] = 2.5; a[2] = 3.5;
function kernel(n) {
  var t = 4.5;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = a[9999]; }
  }
  return t;
}
console.log(typeof kernel(20));
"#;

/// A dense Array of DOUBLES (`ARR_NUM_PIN_KIND`) — the other way onto the DOUBLE
/// tier, and a boxed element load rather than a raw one.
const DOUBLE_ARRAY_OOB: &str = r#"
var a = [1.5, 2.5, 3.5];
function kernel(n) {
  var t = 4.5;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = a[9999]; }
  }
  return t;
}
console.log(typeof kernel(20));
"#;

/// The INT-GPR emitter (`region_int_gpr.rs`), reached by making the loop a
/// bitwise chain — the B118 GPR-home sub-mode. The xorshift steps are all
/// `| 0`-closed so no i53 guard fires: without that the region exits mid-iteration
/// every time and the interpreter, not the compiled body, runs the cold read.
/// The result is observed through `===` rather than `typeof` on purpose (see the
/// note in the wave report about `instr_uses` not modelling `TypeOf`).
const INT_GPR_OOB: &str = r#"
var a = new Int32Array(8);
for (var k = 0; k < 8; k++) a[k] = k * 7;
function kernel(n) {
  var h = 777 | 0;
  var t = 4;
  for (var i = 0; i < n; i++) {
    h = (h ^ (h << 13)) | 0;
    h = (h ^ (h >>> 17)) | 0;
    h = (h ^ (h << 5)) | 0;
    h = (h + i) | 0;
    if (i === 17) { t = a[9999]; }
  }
  return (h ^ (t === undefined ? 1 : 2)) | 0;
}
console.log(kernel(20));
"#;

/// A NEGATIVE index. Same guard (the bounds compare is unsigned, so it catches
/// `< 0` too), but a different JS-level answer: `a[-1]` is a plain missing
/// property, not an out-of-range one.
const NEGATIVE_INDEX: &str = r#"
var a = [1, 2, 3];
function kernel(n) {
  var t = 4;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = a[-1]; }
  }
  return t;
}
console.log(typeof kernel(20));
"#;

/// An out-of-bounds TypedArray STORE, which the spec makes a silent no-op: the
/// interpreter must re-execute `a[9999] = 7` on the ARRAY and do nothing. With a
/// receiver slot holding `undefined` it threw instead, so the loop never
/// finished — the error class is the observable.
const TA_OOB_STORE: &str = r#"
var a = new Int32Array(3);
var msg = "none";
function kernel(n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    if (i === 17) { a[9999] = 7; t = 1; }
  }
  return t;
}
try { console.log(kernel(20) + " " + a.length + " " + a[0]); }
catch (e) { console.log("THREW " + e.constructor.name); }
"#;

/// A pinned flat-ASCII STRING receiver (`STR_PIN_KIND`) reached through
/// `charCodeAt` — a `CallMethod`, not an index op, and a different emitter arm
/// with the same receiver contract. Past the end it yields `NaN`; on the stale
/// slot the re-executed `CallMethod` found `undefined` and threw.
const STR_CHARCODEAT_OOB: &str = r#"
var s = "abc";
function kernel(n) {
  var t = 4;
  for (var i = 0; i < n; i++) {
    if (i === 17) { t = s.charCodeAt(9999); }
  }
  return t;
}
console.log(kernel(20));
"#;

#[test]
fn coldrecv_parity_dense_array_oob() {
    assert_matches_node(DENSE_ARRAY_OOB);
}

#[test]
fn coldrecv_parity_int32array_oob() {
    assert_matches_node(INT32ARRAY_OOB);
}

#[test]
fn coldrecv_parity_float64array_oob() {
    assert_matches_node(FLOAT64ARRAY_OOB);
}

#[test]
fn coldrecv_parity_double_array_oob() {
    assert_matches_node(DOUBLE_ARRAY_OOB);
}

#[test]
fn coldrecv_parity_int_gpr_oob() {
    assert_matches_node(INT_GPR_OOB);
}

#[test]
fn coldrecv_parity_negative_index() {
    assert_matches_node(NEGATIVE_INDEX);
}

#[test]
fn coldrecv_parity_ta_oob_store() {
    assert_matches_node(TA_OOB_STORE);
}

#[test]
fn coldrecv_parity_str_charcodeat_oob() {
    assert_matches_node(STR_CHARCODEAT_OOB);
}

// ─────────────────────────── the mechanism pins ───────────────────────────

/// One `[jit] {TIER} region [S,E] pinned receiver rN lg=[..]` line, parsed.
struct PinnedRecv {
    tier: String,
    span: String,
    reg: u16,
    lg: Vec<usize>,
}

fn pinned_recvs(log: &str) -> Vec<PinnedRecv> {
    log.lines()
        .filter_map(|l| {
            let head = l.strip_prefix("[jit] ")?;
            let tier = head.split(" region [").next()?.to_string();
            let rest = head.split(" region [").nth(1)?;
            let span = rest.split(']').next()?.to_string();
            let tail = rest.split("] pinned receiver r").nth(1)?;
            let reg = tail.split_whitespace().next()?.parse::<u16>().ok()?;
            let lg = tail
                .split("lg=[")
                .nth(1)?
                .split(']')
                .next()?
                .split(',')
                .filter_map(|t| t.trim().parse::<usize>().ok())
                .collect();
            Some(PinnedRecv {
                tier,
                span,
                reg,
                lg,
            })
        })
        .collect()
}

/// The ips at which a region (named by its START, as the deopt log does) took a
/// NATIVE exit.
fn deopt_ips(log: &str, span: &str) -> Vec<usize> {
    let head = format!("[{}] deopt at ip ", span.split(',').next().unwrap_or(span));
    log.lines()
        .filter_map(|l| l.split(&head).nth(1))
        .filter_map(|t| t.split_whitespace().next()?.parse::<usize>().ok())
        .collect()
}

fn logged_child(test: &str) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg(test)
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_NO_GUARD_HOIST")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        .env_remove("ZIPP_GC_STRESS")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "logged re-run of {test} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The non-vacuity pin, and the only reason the parity cases above test
/// anything: the shape must still (a) plan a pinned receiver on a REGISTER tier,
/// whose `LoadGlobal` ip the log names, and (b) take a native exit STRICTLY AFTER
/// that `LoadGlobal` — the window in which the receiver's frame slot is what the
/// interpreter reads. Without (b) the case exits nowhere near the receiver and
/// cannot observe the defect at all. `want_tier` additionally pins WHICH emitter
/// is covered, so the three copies of the arm stay covered as a set.
fn assert_pinned_recv_mechanism(test: &str, want_tier: &str) {
    let log = logged_child(test);
    let recvs = pinned_recvs(&log);
    assert!(
        !recvs.is_empty(),
        "{test} no longer plans a pinned receiver — the case has stopped \
         exercising the receiver-slot hazard:\n{log}"
    );
    let mut covered = None;
    for pr in &recvs {
        let Some(&first_lg) = pr.lg.first() else {
            continue;
        };
        let ips = deopt_ips(&log, &pr.span);
        if ips.iter().any(|&ip| ip > first_lg) {
            covered = Some((pr, first_lg, ips));
            break;
        }
    }
    let (pr, first_lg, ips) = covered.unwrap_or_else(|| {
        panic!(
            "no native exit landed after a pinned receiver's LoadGlobal — the \
             stale-receiver-slot hazard is unpinned. receivers={:?}\n{log}",
            recvs
                .iter()
                .map(|p| (&p.tier, &p.span, p.reg, &p.lg))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        pr.tier, want_tier,
        "{test} now hosts its pinned receiver on the {} tier, not {want_tier} — \
         the emitter this case was chosen to cover is no longer covered \
         (r{} of region [{}], lg {first_lg}, exits {ips:?}):\n{log}",
        pr.tier, pr.reg, pr.span
    );
}

#[test]
fn coldrecv_mechanism_dense_array_oob() {
    assert_pinned_recv_mechanism("coldrecv_parity_dense_array_oob", "INT");
}

#[test]
fn coldrecv_mechanism_float64array_oob() {
    assert_pinned_recv_mechanism("coldrecv_parity_float64array_oob", "DOUBLE");
}

#[test]
fn coldrecv_mechanism_int_gpr_oob() {
    assert_pinned_recv_mechanism("coldrecv_parity_int_gpr_oob", "INT-GPR");
}

#[test]
fn coldrecv_mechanism_str_charcodeat_oob() {
    assert_pinned_recv_mechanism("coldrecv_parity_str_charcodeat_oob", "INT");
}

/// Every case must answer identically in every mode. `ZIPP_NOJIT=1` is the
/// reference; `ZIPP_JIT_THRESHOLD=1` moves the OSR point so the compiled body
/// owns even more of the loop; the `ZIPP_NO_*` rows are the engine's own
/// fallbacks, none of which avoided the defect (which is what made it critical —
/// there was no configuration that answered correctly with the JIT on).
#[test]
fn coldrecv_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 8] = [
        &[("ZIPP_NOJIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NO_GUARD_HOIST", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GLOB_RANGE", "1")],
        &[("ZIPP_NO_TYPED_SPLICE", "1")],
        &[("ZIPP_NO_FUSED_CMPJUMP", "1")],
        &[("ZIPP_GC_STRESS", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("coldrecv_parity_");
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
            "the coldrecv_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
