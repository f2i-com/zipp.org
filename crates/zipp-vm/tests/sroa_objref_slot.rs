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
//! The fix does not touch the liveness. The object-ref loads are LEFT ALONE and
//! recognised by `plan_region` as what they are — a `LoadGlobal` whose dst the
//! region never reads, i.e. a PINNED RECEIVER (`ta_recv_regs`): no numeric home
//! for `r`, no global home for `o`, and the load lowers to
//! `emit_recv_slot_store`, two `mov`s that keep `r`'s frame slot exactly what the
//! interpreted `LoadGlobal` would have left. That needs no claim about liveness
//! at all, and it also retires the older hazard the neutralisation carried: a
//! flush of the fake home would have written `0` over the object in a slot the
//! interpreter reads back at the very heap op a deopt resumes on.
//!
//! `sroa_mechanism_*` reads a child's `ZIPP_JITLOG` back and fails if an SROA
//! region stops being installed or starts deopting — the regression itself,
//! observed rather than timed. Three of the four failed at f0f3fd9 with 64
//! deopts and a MEM recompile; `sroa_mechanism_in_function` is the control, a
//! callee frame whose register high-water leaves the object-ref temps below the
//! tail's argument window, so it never entered `read_outside` and never
//! regressed. `COLD_BRANCH` has no mechanism pin on purpose — field promotion
//! declines that shape on both sides, so it is a parity case only.

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
        .expect("node on PATH (expected values come from `node -e`)");
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

/// The SROA region must be installed AND must never bail out of it. An entry
/// guard rejecting the object-ref register's frame slot is the regression, and
/// it shows up as `deopt at ip {start}` followed — 64 of them later — by the
/// same span recompiling on the MEM tier.
fn assert_sroa_region_survives(test: &str) {
    let log = logged_child(test);
    let regions = sroa_regions(&log);
    assert!(
        !regions.is_empty(),
        "{test} no longer installs an SROA region — the case has stopped \
         exercising field promotion at all:\n{log}"
    );
    for (f, s, e) in &regions {
        let deopt = format!("[jit] region {f} [{s}] deopt at ip ");
        let n = log.lines().filter(|l| l.starts_with(&deopt)).count();
        assert_eq!(
            n, 0,
            "{test}: the SROA region {f} [{s},{e}] deopted {n} times. An \
             object-ref `LoadGlobal` whose dst the region never reads must be \
             planned as a pinned receiver (no numeric home, frame slot written \
             by `emit_recv_slot_store`); a numeric home for it is entry-loaded \
             from a slot holding the OBJECT and bails on every entry:\n{log}"
        );
        let mem = format!("[jit] MEM region {f} [{s},{e}] compiled");
        assert!(
            !log.lines().any(|l| l.starts_with(&mem)),
            "{test}: the SROA region {f} [{s},{e}] was evicted and recompiled on \
             the boxed MEM tier — the 4.33x `bench/object.js` regression:\n{log}"
        );
    }
}

#[test]
fn sroa_mechanism_object_bench() {
    assert_sroa_region_survives("sroa_parity_object_bench");
}

#[test]
fn sroa_mechanism_many_tail_args() {
    assert_sroa_region_survives("sroa_parity_many_tail_args");
}

#[test]
fn sroa_mechanism_read_object_after() {
    assert_sroa_region_survives("sroa_parity_read_object_after");
}

#[test]
fn sroa_mechanism_in_function() {
    assert_sroa_region_survives("sroa_parity_in_function");
}
