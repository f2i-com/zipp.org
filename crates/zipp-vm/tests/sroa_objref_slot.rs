//! SROA field promotion and the object-ref register's frame slot.
//!
//! `rewrite_for_field_promotion` clones a region and turns every
//! `GetProp`/`SetProp` on one non-escaping global object into a scratch-global
//! read/write, so the loop compiles on a purely numeric tier. That leaves the
//! `LoadGlobal o -> r` that fed each heap op with no consumer inside the region.
//!
//! It used to NEUTRALISE those loads to `LoadInt 0` and rely on the region
//! planner's dead-code pass to delete `r`. That pass's licence is
//! `!read_outside(r)` — and it was only ever granted because `instr_uses` was
//! blind to 185 of 221 opcodes. W17 made the table exhaustive, so a register the
//! enclosing function reuses anywhere is now (correctly) NOT dead, and the fake
//! `LoadInt` pinned an xmm home that gets entry-loaded out of a frame slot
//! holding the OBJECT. The numeric entry guard rejects it, so the region
//! entry-bailed on every OSR entry, took 64 deopts, evicted, and recompiled on
//! the boxed MEM tier: `bench/object.js` 0.89ms -> 3.84ms, output still correct.
//!
//! `bench/object.js` is the exact shape, and the `console.log(s)` after the loop
//! is what triggers it: the argument register of the `Print` at ip 37 is the same
//! register the SROA region loads the object into at ip 29.
//!
//! The fix does not weaken liveness. The object-ref loads are LEFT ALONE. When a
//! receiver register is otherwise dead, `plan_region` treats it as a pinned
//! receiver (`ta_recv_regs`). Capture-first lowering may instead recycle that
//! register for a later numeric range, so the planner proves a B94 split: the
//! object load writes the authoritative boxed frame slot, and the later numeric
//! definitions use their normal homes. In both cases there is no numeric entry
//! load of the object. This also retires the older neutralisation hazard: a
//! flush of the fake home could have written `0` over the object in a slot the
//! interpreter reads back at the very heap op where a deopt resumes.
//!
//! `sroa_mechanism_*` reads a child's `ZIPP_JITLOG` back and fails if an SROA
//! region stops being installed, promotes fewer fields, loses its
//! object-ref plan, or starts deopting — the regression itself, observed rather
//! than timed. Three of the four originally failed at f0f3fd9 with 64 deopts
//! and a MEM recompile. `COLD_BRANCH` has no mechanism pin on purpose — field
//! promotion declines that shape on both sides, so it is a parity case only.
//!
//! ── 07b400dc (2026-09-02), "Give booleans and global receivers their own
//! register classes" ── WHICH OF THE TWO SAFE OUTCOMES THESE FIXTURES REACH.
//! The paragraph above says the fix admits both: a receiver register that is
//! otherwise dead becomes a pinned receiver, and one the compiler recycles for
//! a later numeric range gets a proven B94 split — "in both cases there is no
//! numeric entry load of the object", which is the whole invariant.
//!
//! 07b400dc removed the recycling. `Scopes::recv_expr` now routes a receiver
//! that is a plain global identifier into a RECV-class register that is never
//! reclaimed, so the tail's `console.log` argument windows can no longer land
//! on the object-ref register: it has one definition, it is not in
//! `read_outside`, and it needs no split. All four fixtures therefore take the
//! FIRST outcome now, and the mechanism pins were asserting the second. What
//! still matters is pinned exactly and unchanged — the SROA region is installed
//! with its full field count, the region takes ZERO deopts (an entry guard
//! rejecting the object's frame slot logs `deopt at ip {start}`), and the span
//! never recompiles on the boxed MEM tier, which is the 4.33x regression.
//! `sroa_mechanism_split_receiver_under_legacy_alloc` keeps the recycled-
//! receiver outcome pinned under `ZIPP_NO_REG_CLASSES=1`, the latch 07b400dc
//! shipped, with the exact split count of each fixture.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

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

/// `bench/object.js`, shortened. Three promoted fields, and a `console.log`
/// whose argument register the loop body also uses for the object ref.
const OBJECT_BENCH: &str = r#"
let o={a:0,b:0,c:0};
let s=0;
for(let i=0;i<60000;i++){ o.a=i; o.b=o.a+1; o.c=o.b*2; s+=o.c; }
console.log(s);
"#;

/// The object itself is read AFTER the loop, through the same fields the region
/// promoted — so the post-run sync and the frame slot both have to be right.
const READ_OBJECT_AFTER: &str = r#"
let o={a:0,b:0};
let s=0;
for(let i=0;i<60000;i++){ o.a=i; o.b=o.a+3; s+=o.b; }
console.log(s, o.a, o.b);
"#;

/// The tail of the program reuses the loop's register window for several
/// `console.log` argument lists, so several object-ref registers — not just one
/// — land in `read_outside`.
const MANY_TAIL_ARGS: &str = r#"
let o={a:1,b:2,c:3,d:4};
let s=0;
for(let i=0;i<60000;i++){ o.a=i; o.b=o.a+1; o.c=o.b+1; o.d=o.c+1; s+=o.d; }
console.log(s);
console.log(o.a, o.b);
console.log(o.c, o.d);
"#;

/// The COLD shape: the heap ops sit behind a branch the interpreter never takes
/// before the region compiles, so the object-ref registers' frame slots hold
/// whatever the frame happened to hold when compiled code first runs the branch.
/// If the region wrote a number over them, the deopt that re-executes the access
/// reads a non-object.
const COLD_BRANCH: &str = r#"
let o={a:0,b:0};
let t=0;
let s=0;
for(let i=0;i<60000;i++){ if(i===50000){ o.a=i; o.b=o.a+1; t=o.b; } s+=i; }
console.log(s, t, o.a, o.b);
"#;

/// The promoted loop runs INSIDE a function, so the object-ref registers share
/// the callee frame's window with the tail's argument list rather than a
/// top-level proto's recycled temps.
const IN_FUNCTION: &str = r#"
let o={x:0,y:0};
function k(n){ let s=0; for(let i=0;i<n;i++){ o.x=i; o.y=o.x*2; s+=o.y; } console.log(o.x, o.y); return s; }
console.log(k(60000));
"#;

#[test]
fn sroa_parity_object_bench() {
    assert_matches_node(OBJECT_BENCH);
}

#[test]
fn sroa_parity_read_object_after() {
    assert_matches_node(READ_OBJECT_AFTER);
}

#[test]
fn sroa_parity_many_tail_args() {
    assert_matches_node(MANY_TAIL_ARGS);
}

#[test]
fn sroa_parity_cold_branch() {
    assert_matches_node(COLD_BRANCH);
}

#[test]
fn sroa_parity_in_function() {
    assert_matches_node(IN_FUNCTION);
}

// ─────────────────────────── the mechanism pins ───────────────────────────

fn logged_child(test: &str, extra: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test)
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_NO_GUARD_HOIST")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        // The register-allocation regime decides which of the two safe
        // object-ref outcomes a fixture reaches (07b400dc, 2026-09-02):
        // cleared here, re-set by `extra`.
        .env_remove("ZIPP_NO_REG_CLASSES")
        .env_remove("ZIPP_GC_STRESS");
    for (key, val) in extra {
        cmd.env(key, val);
    }
    let out = cmd.output().expect("spawn the test binary");
    assert!(
        out.status.success(),
        "logged re-run of {test} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `[jit] SROA region fn{F} [{S},{E}] fields=N -> compiled`, as `(fn, s, e)`.
fn sroa_regions(log: &str) -> Vec<(String, String, String)> {
    log.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("[jit] SROA region ")?;
            if !rest.trim_end().ends_with("-> compiled") {
                return None;
            }
            let mut it = rest.split_whitespace();
            let f = it.next()?.to_string();
            let span = it.next()?.trim_start_matches('[').trim_end_matches(']');
            let (s, e) = span.split_once(',')?;
            Some((f, s.to_string(), e.to_string()))
        })
        .collect()
}

/// One SROA region, promoting exactly `fields` fields, planning exactly
/// `splits` recycled-receiver B94 splits for its object refs, and never bailing
/// out of it. An entry guard rejecting the object-ref register's frame slot is
/// the regression, and it shows up as `deopt at ip {start}` followed — 64 of
/// them later — by the same span recompiling on the MEM tier.
///
/// `splits` is exact in both directions, which is what keeps this pin honest
/// across 07b400dc (see the module header): `0` asserts the default lowering
/// leaves the object ref out of the numeric plan entirely, and a positive count
/// asserts the legacy allocation still proves the split for every recycled
/// object-ref register it creates. Neither value can drift into the other.
fn assert_sroa_region_survives(test: &str, fields: usize, splits: usize, extra: &[(&str, &str)]) {
    let log = logged_child(test, extra);
    let regions = sroa_regions(&log);
    assert_eq!(
        regions.len(),
        1,
        "{test} should install exactly one SROA region under {extra:?} — the \
         case has stopped exercising field promotion as written:\n{log}"
    );
    let (f, s, e) = &regions[0];
    assert!(
        log.contains(&format!(
            "[jit] SROA region {f} [{s},{e}] fields={fields} -> compiled"
        )),
        "{test}: field promotion no longer promotes {fields} fields — a lower \
         count means part of the object stayed on the heap path:\n{log}"
    );
    let split = format!("[jit] DOUBLE region [{s},{e}] B94 split receiver ");
    assert_eq!(
        log.lines().filter(|l| l.starts_with(&split)).count(),
        splits,
        "{test}: expected exactly {splits} B94 split receivers in the SROA \
         region {f} [{s},{e}] under {extra:?}. A count above {splits} means an \
         object-ref register is being recycled where it should not be; below \
         it, that the split stopped being proven for one that is:\n{log}"
    );
    let deopt = format!("[jit] region {f} [{s}] deopt at ip ");
    let n = log.lines().filter(|l| l.starts_with(&deopt)).count();
    assert_eq!(
        n, 0,
        "{test}: the SROA region {f} [{s},{e}] deopted {n} times. An \
         object-ref `LoadGlobal` must remain memory-authoritative while any \
         recycled numeric range uses a split home; otherwise a numeric home \
         is entry-loaded from a slot holding the OBJECT and bails on every \
         entry:\n{log}"
    );
    let mem = format!("[jit] MEM region {f} [{s},{e}] compiled");
    assert!(
        !log.lines().any(|l| l.starts_with(&mem)),
        "{test}: the SROA region {f} [{s},{e}] was evicted and recompiled on \
         the boxed MEM tier — the 4.33x `bench/object.js` regression:\n{log}"
    );
}

/// `(parity test, promoted fields, B94 splits under the legacy allocation)`.
const SHAPES: [(&str, usize, usize); 4] = [
    ("sroa_parity_object_bench", 3, 2),
    ("sroa_parity_many_tail_args", 4, 3),
    ("sroa_parity_read_object_after", 2, 1),
    ("sroa_parity_in_function", 2, 1),
];

#[test]
fn sroa_mechanism_object_bench() {
    assert_sroa_region_survives(SHAPES[0].0, SHAPES[0].1, 0, &[]);
}

#[test]
fn sroa_mechanism_many_tail_args() {
    assert_sroa_region_survives(SHAPES[1].0, SHAPES[1].1, 0, &[]);
}

#[test]
fn sroa_mechanism_read_object_after() {
    assert_sroa_region_survives(SHAPES[2].0, SHAPES[2].1, 0, &[]);
}

#[test]
fn sroa_mechanism_in_function() {
    assert_sroa_region_survives(SHAPES[3].0, SHAPES[3].1, 0, &[]);
}

/// The RECYCLED object-ref outcome, which only the pre-07b400dc allocation can
/// produce (see the module header).
///
/// `ZIPP_NO_REG_CLASSES=1` is the latch that commit shipped; under it the
/// tail's argument windows land back on the object-ref registers, so each
/// fixture's object refs are recycled and every one of them must have a proven
/// B94 split — the exact counts below — while still promoting the same fields,
/// still taking zero deopts and still keeping off the MEM tier. This is the
/// arm that covers the f0f3fd9 regression in the allocation it was found in;
/// the four default-lowering pins above cover today's.
#[test]
fn sroa_mechanism_split_receiver_under_legacy_alloc() {
    for (test, fields, splits) in SHAPES {
        assert!(splits > 0, "{test}: a legacy-alloc row must prove a split");
        assert_sroa_region_survives(test, fields, splits, &[("ZIPP_NO_REG_CLASSES", "1")]);
    }
}

/// Every parity case must still answer identically under the legacy
/// allocation, so the split route above is checked against node rather than
/// only asserted about.
#[test]
fn sroa_legacy_alloc_answers_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("sroa_parity_")
        .env("ZIPP_NO_REG_CLASSES", "1")
        .output()
        .expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "ZIPP_NO_REG_CLASSES=1 failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.contains("running 0 tests"),
        "the sroa_parity_ filter matched nothing:\n{stdout}"
    );
}
