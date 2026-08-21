//! Stored-global live-range narrowing + mixed-role temp splitting on the
//! INT-GPR region tier (`ZIPP_NO_GLOB_RANGE`).
//!
//! B96 permanence pinned every top-level global touched by a region onto a
//! whole-region home, and the bytecode compiler's register recycling welded a
//! temp's disjoint def-ranges into one wide interval — together they held the
//! DV swizzle nest at 13-14 homes against the 7-9 GPR pool while the same loop
//! with function locals planned 8. The mechanism narrows a stored global to
//! its real touch window when every in-region load is provably dominated by
//! an in-region store (each such store then WRITES THROUGH to the global
//! slot, the exit flush skips it, and there is no entry load), splits a
//! shareable temp into per-def-range intervals bound to one home, and
//! rematerializes single-def in-loop constants as immediates.
//!
//! The correctness surface is mid-iteration exits: a region that bails AFTER
//! the narrowed store must leave the interpreter reading exactly the stored
//! value from the slot (the B9-class hazard), and a bail BEFORE it must leave
//! the previous iteration's value. Every `globrange_parity_*` case asserts
//! byte-identical output against `node -e` at DEFAULT settings; the final
//! tests re-run the set with `ZIPP_NO_GLOB_RANGE=1`, `ZIPP_JIT_THRESHOLD=1`,
//! `ZIPP_GC_STRESS=1` and `ZIPP_NOJIT=1`, and pin the mechanism itself from a
//! child process's ZIPP_JITLOG.
//!
//! A parity case that stops ENGAGING the mechanism keeps passing while
//! testing nothing, so every case that claims to exercise a hazard is paired
//! with a `globrange_mechanism_*` pin that reads the plan back out of the
//! log: narrowing planned, GPR tier engaged, and — for the B9 pin — a native
//! exit ip that lands strictly inside a narrowed global's touch window.

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

/// The row's shape: a top-level DV swizzle nest whose `le`/`v` are stored
/// before every load in the same iteration (the narrowable pair), while `o`
/// and `bsum` are loop-carried (they must keep permanent homes). Sized well
/// past the OSR threshold so both regions compile and the nest hosts on
/// whichever tier the OUTER lands.
#[test]
fn globrange_parity_swizzle_nest() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 4096;
        var iv = new Int32Array(NI);
        var st = 0x9E3779B9 | 0;
        for (var i = 0; i < NI; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i] = st | 0;
        }
        var dv = new DataView(iv.buffer, 0, 4096 * 4);
        var bsum = 0;
        for (var r = 0; r < 40; r++) {
          for (var o = 0; o < 4096 * 4; o += 4) {
            var le = (o >> 2) & 1;
            var v = dv.getUint32(o, le === 1);
            bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
          }
        }
        console.log("bsum=" + bsum + " le=" + le + " v=" + v + " o=" + o + " r=" + r);
        "#,
    );
}

/// THE B9-CLASS PIN — a CHRONIC mid-iteration exit INSIDE a narrowed window.
/// `-le` takes the negative-zero bail every time `le === 0`, i.e. on every
/// other iteration, at an ip that lies strictly AFTER `le`'s write-through
/// store and strictly BEFORE the later loads of `le` and `v` — precisely the
/// state where a narrowed home may already have been lent to another value
/// and the frame slot is the only correct copy. The interpreter resumes at
/// the `Neg` and must read the freshly stored `le`/`v` from their slots, and
/// the loop keeps running natively afterwards, so slot handoff is exercised
/// both ways thousands of times.
///
/// `globrange_mechanism_midexit_inside_window` re-runs this shape under
/// ZIPP_JITLOG and proves those three facts from the log (narrowing engaged,
/// GPR tier engaged, exit ip strictly inside a narrowed window) — without
/// that pin this test would silently stop exercising the mechanism, which is
/// exactly what happened to its predecessor: a `p = p * 3` overflow bail made
/// `p` a double, the region failed INT admission on a type conflict, and the
/// whole case ran on the MEM tier with no glob-range plan at all. That shape
/// is kept below as `globrange_parity_double_carried_mem_tier`.
#[test]
fn globrange_parity_midexit_after_store() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 2048;
        var iv = new Int32Array(NI);
        var st = 0x9E3779B9 | 0;
        for (var i = 0; i < NI; i++) { st ^= st << 13; st ^= st >>> 17; st ^= st << 5; iv[i] = st | 0; }
        var dv = new DataView(iv.buffer, 0, NI * 4);
        var bsum = 0;
        for (var r = 0; r < 30; r++) {
          for (var o = 0; o < NI * 4 - 32; o += 4) {
            var le = (o >> 2) & 1;
            var v = dv.getUint32(o, le === 1);
            bsum = (bsum + (v >>> 24) + (-le) + (v & 255) + dv.getUint16(o, le === 0)) | 0;
          }
        }
        console.log("bsum=" + bsum + " le=" + le + " v=" + v + " o=" + o + " r=" + r);
        "#,
    );
}

/// The NEGATIVE control that used to stand in for the pin above: a
/// loop-carried `p = p * 3` overflows i53 and makes `p` a double, so the
/// region is declined off the INT tier (`region_is_int=false`, a type
/// conflict on a reused register) and the whole loop hosts on MEM with no
/// glob-range plan. Kept because the answers must still be node-identical
/// through 60k mid-iteration deopts on the memory tier — but it proves
/// nothing about narrowing, and the mechanism pins must not be pointed at it.
#[test]
fn globrange_parity_double_carried_mem_tier() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(64);
        for (var i = 0; i < 64; i++) iv[i] = (i * 2654435761) | 0;
        var dv = new DataView(iv.buffer);
        var bsum = 0;
        var p = 3;
        var le = 0;
        var v = 0;
        for (var o = 0; o < 60000; o++) {
          var k = (o & 15) * 4;
          le = (k >> 2) & 1;
          v = dv.getUint32(k, le === 1);
          if (o > 45000) { p = p * 3; }
          bsum = (bsum + (v >>> 24) + le + (p % 97)) | 0;
        }
        console.log(bsum + "," + le + "," + v + "," + p);
        "#,
    );
}

/// The SLOT-MATERIALIZED CONST path (`RegionPlan::slot_consts`): a 3-level
/// nest leaves the MIDDLE region non-confined, so `outside_dead` is empty and
/// the inner loop's bound constants — read outside the region, and not
/// provably run on every pass — cannot be hoisted; they take the slot form
/// instead, where every in-region read is the immediate and the def stores
/// the boxed constant straight to the frame slot with nothing flushed for it
/// at exit. The `-le` negative-zero bail makes that middle region exit
/// NATIVELY on every other iteration, so the "nothing is flushed for it" half
/// of the contract is exercised too. `globrange_mechanism_slot_const_engages`
/// pins `slotc >= 1` and the native exits from the log.
#[test]
fn globrange_parity_slot_const_nest() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 1024;
        var iv = new Int32Array(NI);
        var st = 0x9E3779B9 | 0;
        for (var i = 0; i < NI; i++) { st ^= st << 13; st ^= st >>> 17; st ^= st << 5; iv[i] = st | 0; }
        var dv = new DataView(iv.buffer, 0, NI * 4);
        var bsum = 0;
        for (var q = 0; q < 3; q++) {
          for (var r = 0; r < 30000; r++) {
            for (var o = 0; o < 24; o += 4) {
              var le = (o >> 2) & 1;
              var v = dv.getUint32(o, le === 1);
              bsum = (bsum + (v & 255) + (-le)) | 0;
            }
          }
        }
        console.log("bsum=" + bsum + " le=" + le + " v=" + v + " o=" + o + " r=" + r + " q=" + q);
        "#,
    );
}

/// A mid-iteration exit BEFORE the store: the guard that fails sits between
/// the loop header and `le`'s store, so the slot must still hold the
/// PREVIOUS iteration's value when the interpreter resumes.
#[test]
fn globrange_parity_midexit_before_store() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(64);
        for (var i = 0; i < 64; i++) iv[i] = (i * 40503) | 0;
        var dv = new DataView(iv.buffer);
        var bsum = 0;
        var q = 5;
        var le = 0;
        var v = 0;
        for (var o = 0; o < 60000; o++) {
          if (o > 45000) { q = q * 5; }
          le = (o >> 1) & 1;
          v = dv.getUint16((o & 31) * 4, le === 0);
          bsum = (bsum + v + le + (q % 89)) | 0;
        }
        console.log(bsum + "," + le + "," + v + "," + q);
        "#,
    );
}

/// The loop exits normally and the program READS the narrowed globals
/// afterwards — the write-through slot must hold the final iteration's
/// values (a stale flush would surface here as a wrong printed value).
#[test]
fn globrange_parity_read_after_loop() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(1024);
        for (var i = 0; i < 1024; i++) iv[i] = (i ^ (i << 9)) | 0;
        var dv = new DataView(iv.buffer);
        var bsum = 0;
        for (var o = 0; o < 4096; o += 4) {
          var le = (o >> 2) & 1;
          var v = dv.getUint32(o, le === 1);
          bsum = (bsum + (v & 65535)) | 0;
        }
        console.log(le, v, bsum);
        for (var t = 0; t < 3; t++) { le = le + v; }
        console.log(le);
        "#,
    );
}

/// Loop-carried globals must NOT narrow: `acc` is loaded at the top of the
/// iteration and stored at the bottom (its load is dominated only across the
/// back edge — the fail-closed case), and the split temps' def-ranges are
/// interleaved with a branch inside the body (the jump-target veto).
#[test]
fn globrange_parity_loop_carried_and_branchy() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(256);
        for (var i = 0; i < 256; i++) iv[i] = (i * 69069 + 1) | 0;
        var dv = new DataView(iv.buffer);
        var acc = 1;
        var hits = 0;
        for (var o = 0; o < 40000; o++) {
          var k = (o & 63) * 4;
          var le = k & 1;
          var w = dv.getInt8(k + (acc & 3));
          if ((w & 7) === 3) { hits = (hits + 1) | 0; }
          acc = (acc + w + le) | 0;
        }
        console.log(acc + "," + hits);
        "#,
    );
}

/// GC stress interplays with write-through: the narrowed slots receive boxed
/// Int values mid-iteration, and a stressed collector walks globals at every
/// opportunity — any raw/garbage write would crash or corrupt. (The final
/// mode re-run covers this too; this standalone case keeps a small shape.)
#[test]
fn globrange_parity_small_shapes() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(32);
        for (var i = 0; i < 32; i++) iv[i] = (i * 2246822519) | 0;
        var dv = new DataView(iv.buffer);
        var s1 = 0;
        for (var o = 0; o < 30000; o++) {
          var le = o & 1;
          var v = dv.getUint16((o & 15) * 2, le === 1);
          s1 = (s1 + v + le) | 0;
        }
        console.log(s1 + "," + le + "," + v);
        var s2 = 0;
        for (var q = 0; q < 20000; q++) {
          var m = (q >> 3) & 1;
          s2 = (s2 + ((q * 31) >>> (m + 2)) + (s2 & 255)) | 0;
        }
        console.log(s2 + "," + m);
        "#,
    );
}

/// Mid-run interference through `globalThis`: the loop's own body redefines
/// `v` via a `globalThis` property write on late iterations (which routes
/// through the global-object machinery, not a plain StoreGlobal), and after
/// the loop the narrowed slots are read back through `globalThis` too — both
/// must see exactly the write-through values.
#[test]
fn globrange_parity_globalthis_interference() {
    assert_matches_node(
        r#"
        var iv = new Int32Array(64);
        for (var i = 0; i < 64; i++) iv[i] = (i * 1103515245 + 12345) | 0;
        var dv = new DataView(iv.buffer);
        var bsum = 0;
        // Clean loop first: le/v narrow and the region engages; the readback
        // BELOW goes through the globalThis property route and must see the
        // write-through values.
        for (var o = 0; o < 50000; o++) {
          var le = o & 1;
          var v = dv.getUint32((o & 15) * 4, le === 1);
          bsum = (bsum + (v >>> 24) + le) | 0;
        }
        console.log(bsum + "," + globalThis.le + "," + globalThis.v);
        // Second loop WITH in-body globalThis interference (this one plans to
        // the memory tiers — the write is a property op — and must still
        // answer identically).
        var b2 = 0;
        for (var q = 0; q < 50000; q++) {
          var le2 = q & 1;
          var v2 = dv.getUint32((q & 15) * 4, le2 === 1);
          if (q === 48000) { globalThis.v2 = 7; }
          b2 = (b2 + (v2 >>> 24) + le2) | 0;
        }
        console.log(b2 + "," + globalThis.le2 + "," + globalThis.v2);
        "#,
    );
}

/// The mechanism itself, pinned from a child's ZIPP_JITLOG: on the nest shape
/// the swizzle regions must plan with NARROWED stored globals (exactly two —
/// `le` and `v`; the loop-carried `o`/`bsum` staying permanent is what keeps
/// the narrowed list at two), and the INNER swizzle region must ENGAGE the
/// GPR tier — the pre-wave "N homes > M gprs" decline for that span must be
/// gone.
#[test]
fn globrange_mechanism_narrows_and_engages() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("globrange_parity_swizzle_nest")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NOJIT")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "logged re-run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stderr);
    // A glob-range plan with exactly TWO narrowed slots (le, v) must appear…
    let planned: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("glob-range plan") && l.contains("narrowed=["))
        .collect();
    let narrowed_two = planned
        .iter()
        .find(|l| {
            let list = l.split("narrowed=[").nth(1).and_then(|t| t.split(']').next());
            list.is_some_and(|t| t.split(',').count() == 2 && !t.is_empty())
        })
        .unwrap_or_else(|| panic!("no glob-range plan narrowed exactly {{le, v}}:\n{log}"));
    // …for a region span that then ENGAGES GPR homes with no pool decline.
    let span = narrowed_two
        .split(['[', ']'])
        .nth(3)
        .expect("plan line carries the region span");
    assert!(
        log.contains(&format!("INT region [{span}] GPR homes engaged")),
        "glob-range plan for [{span}] did not engage GPR homes:\n{log}"
    );
    assert!(
        !log.contains(&format!("INT-GPR decline [{span}]")),
        "the pool decline for [{span}] should be gone:\n{log}"
    );
}

/// A THROWING row for the byte-identity matrix, which otherwise only
/// exercises loops that run to completion. The loop's last access is one
/// element past the view, so a RangeError is raised from NATIVE code
/// mid-iteration, AFTER `le`/`v` were written through — and the `catch` then
/// reads the narrowed slots plus the error's own class and message.
///
/// It used to run at DEFAULT SETTINGS ONLY, because the same shape on the
/// DOUBLE/regalloc tier was a live wrong answer: a recycled B94 split
/// receiver's numeric home was stored over the slot that still had to hold the
/// receiver object, so the re-executed `CallMethod` found a NUMBER and reported
/// `TypeError: undefined is not a function (property "getUint32")` instead of
/// node's RangeError. Glob-range hosting it on INT-GPR — an emitter that always
/// excluded the receiver `LoadGlobal` ip — is what hid it, so this case was
/// held out of the mode sweep while `ZIPP_NO_GLOB_RANGE=1`,
/// `ZIPP_NO_GPR_HOMES=1` and `ZIPP_NO_DV_GPR=1` still reproduced it.
///
/// The DOUBLE emitter now takes its write-through def from `emit::wt_def_at`
/// like the other two tiers, so the shape is correct on every tier and the case
/// belongs in the `globrange_parity_` sweep. `split_recv_writethrough.rs` owns
/// the defect itself, including this shape run straight at the DOUBLE tier.
#[test]
fn globrange_parity_dv_oob() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 8192;
        var iv = new Int32Array(NI);
        var st = 777 | 0;
        for (var i = 0; i < NI; i++) { st ^= st << 13; st ^= st >>> 17; st ^= st << 5; iv[i] = st | 0; }
        var dv = new DataView(iv.buffer, 0, NI * 4);
        var bsum = 0;
        var msg = "none";
        try {
          for (var o = 0; o < NI * 4 + 8; o += 4) {
            var le = (o >> 2) & 1;
            var v = dv.getUint32(o, le === 1);
            bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
          }
        } catch (e) { msg = e.constructor.name + ": " + e.message; }
        console.log("bsum=" + bsum + " le=" + le + " v=" + v + " o=" + o + " msg=" + msg);
        "#,
    );
}

/// One `[jit] region [S,E] glob-range plan: narrowed=[..] ... slotc=N ...`
/// line, parsed.
struct GrPlan {
    span: String,
    narrowed: Vec<u32>,
    slotc: usize,
}

fn glob_range_plans(log: &str) -> Vec<GrPlan> {
    log.lines()
        .filter(|l| l.contains("glob-range plan"))
        .filter_map(|l| {
            let span = l.split("region [").nth(1)?.split(']').next()?.to_string();
            let narrowed = l
                .split("narrowed=[")
                .nth(1)?
                .split(']')
                .next()?
                .split(',')
                .filter_map(|t| t.trim().parse::<u32>().ok())
                .collect();
            let slotc =
                l.split("slotc=").nth(1)?.split_whitespace().next()?.parse::<usize>().ok()?;
            Some(GrPlan { span, narrowed, slotc })
        })
        .collect()
}

/// The first plan for a region that ALSO engaged the GPR tier — the only tier
/// that consumes a glob-range plan, so a plan without this line proves nothing
/// about the emitted code.
fn engaged_plan(log: &str, pick: impl Fn(&GrPlan) -> bool) -> GrPlan {
    glob_range_plans(log)
        .into_iter()
        .find(|p| {
            pick(p) && log.contains(&format!("INT region [{}] GPR homes engaged", p.span))
        })
        .unwrap_or_else(|| panic!("no ENGAGED glob-range plan matched in:\n{log}"))
}

/// `[globrange] [S,E] gN: [(a, b)]` — a narrowed global's real touch window
/// (`ZIPP_GLOBRANGE_DEBUG=1`). `a` is its in-region store, `b` its last load.
fn narrowed_window(log: &str, span: &str, slot: u32) -> Option<(usize, usize)> {
    let head = format!("[globrange] [{span}] g{slot}: [(");
    let body = log
        .lines()
        .find(|l| l.trim_start().starts_with(&head))?
        .split("[(")
        .nth(1)?
        .split(')')
        .next()?
        .to_string();
    let mut it = body.split(',');
    let a = it.next()?.trim().parse().ok()?;
    let b = it.next()?.trim().parse().ok()?;
    Some((a, b))
}

/// The ips at which a region (named by its START, as the deopt log does) took
/// a NATIVE exit.
fn deopt_ips(log: &str, span: &str) -> Vec<usize> {
    let head = format!("[{}] deopt at ip ", span.split(',').next().unwrap_or(span));
    log.lines()
        .filter_map(|l| l.split(&head).nth(1))
        .filter_map(|t| t.split_whitespace().next()?.parse::<usize>().ok())
        .collect()
}

fn logged_child(test: &str, extra: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test)
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NOJIT");
    for (k, v) in extra {
        cmd.env(k, v);
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

/// THE B9-CLASS PIN's mechanism half. `globrange_parity_midexit_after_store`
/// must (a) plan narrowing for a region that ENGAGES the GPR tier, (b) take
/// NATIVE exits from that region, and (c) take at least one of them at an ip
/// strictly INSIDE a narrowed global's window — after its write-through store
/// and before a later load of the same global. That is the state the whole
/// mechanism's soundness argument is about: the narrowed home may already
/// have been lent to another value, so the interpreter must read the slot.
/// Without (c) the case degenerates into an ordinary parity test — which is
/// what its predecessor silently was.
#[test]
fn globrange_mechanism_midexit_inside_window() {
    let log = logged_child(
        "globrange_parity_midexit_after_store",
        &[("ZIPP_GLOBRANGE_DEBUG", "1")],
    );
    let plan = engaged_plan(&log, |p| p.narrowed.len() >= 2);
    let ips = deopt_ips(&log, &plan.span);
    assert!(
        !ips.is_empty(),
        "region [{}] narrowed {:?} but never took a native exit:\n{log}",
        plan.span,
        plan.narrowed
    );
    let mut inside: Vec<(u32, usize, (usize, usize))> = Vec::new();
    for &g in &plan.narrowed {
        let Some((a, b)) = narrowed_window(&log, &plan.span, g) else { continue };
        for &ip in &ips {
            if a < ip && ip < b {
                inside.push((g, ip, (a, b)));
            }
        }
    }
    assert!(
        !inside.is_empty(),
        "no native exit landed inside a narrowed window — the B9-class hazard \
         is unpinned. region [{}] narrowed={:?} windows={:?} exit ips={:?}\n{log}",
        plan.span,
        plan.narrowed,
        plan.narrowed
            .iter()
            .map(|&g| (g, narrowed_window(&log, &plan.span, g)))
            .collect::<Vec<_>>(),
        ips
    );
}

/// The slot-materialized-const path's pin: `globrange_parity_slot_const_nest`
/// must plan `slotc >= 1` for a region that ENGAGES the GPR tier (so the two
/// emitter arms that store the boxed constant straight to the frame slot are
/// actually emitted), and that same region must take NATIVE exits — the
/// half of the contract that says nothing is flushed for a slot const.
#[test]
fn globrange_mechanism_slot_const_engages() {
    let log = logged_child("globrange_parity_slot_const_nest", &[]);
    let plan = engaged_plan(&log, |p| p.slotc >= 1);
    assert!(plan.slotc >= 1);
    let ips = deopt_ips(&log, &plan.span);
    assert!(
        !ips.is_empty(),
        "the slot-const region [{}] never took a native exit, so the \
         nothing-is-flushed contract is untested:\n{log}",
        plan.span
    );
}

/// The off-switch restores the pre-wave planner: under ZIPP_NO_GLOB_RANGE=1
/// no glob-range plan line may appear, and the answers stay node-identical
/// (asserted by the parity re-run below; this pins the log surface).
#[test]
fn globrange_off_switch_restores_prewave_plans() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("globrange_parity_swizzle_nest")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_NO_GLOB_RANGE", "1")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "off-switch re-run failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(
        !log.contains("glob-range plan"),
        "ZIPP_NO_GLOB_RANGE=1 must suppress every glob-range plan:\n{log}"
    );
}

/// Everything above must answer identically in every mode. The parity tests
/// re-run in child processes; their node-derived assertions passing in all
/// modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_NO_GLOB_RANGE", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("globrange_parity_");
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
            "the globrange_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
