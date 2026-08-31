//! B94 split receiver × B97 write-through: the emitted store at the receiver's
//! own `LoadGlobal` ip.
//!
//! A register the bytecode compiler recycled is a pinned-access RECEIVER over
//! one range and a NUMBER over a disjoint one (B94 `split_recvs`). Its frame
//! slot is kept authoritative: the receiver `LoadGlobal` stores the object
//! there, every numeric def writes its home through, and `flush_exit` skips it.
//! B97 gives the same treatment to a register whose SHARED home is read after
//! the region (`write_through`), and the two sets used to overlap — every
//! top-level loop temp that is textually read outside the region qualifies for
//! B97, and the DOUBLE tier's planner call can never reach the `outside_dead`
//! escape. At a register in BOTH sets the DOUBLE emitter took the B97 disjunct
//! at the receiver's `LoadGlobal` ip and stored the register's NUMERIC home
//! over the receiver object it had written two instructions earlier — so the
//! slot never held the receiver at any observable moment, and every native exit
//! resuming at an ip that reads it as a receiver re-executed on a number.
//!
//! The three cases below are that shape with three different exits, and all
//! three failed at DEFAULT SETTINGS on some tier before the fix:
//!
//!   * `splitwt_parity_array_elem_deopt` — a `GetIndex` type deopt. `NaN`
//!     instead of the sum: a SILENT WRONG NUMBER, no switch required.
//!   * `splitwt_parity_ta_oob_store_strict` — a strict-mode out-of-bounds
//!     TypedArray store, a spec no-op. Threw `TypeError: Cannot assign to read
//!     only property` (the message for assigning to a NUMBER's index) and
//!     aborted the loop four iterations early. No switch required.
//!   * `splitwt_parity_dv_oob` — the reported shape: an out-of-bounds DataView
//!     `get*`. `TypeError: undefined is not a function` instead of node's
//!     `RangeError`, because the re-executed method call found a number. The
//!     current capture-first bytecode hosts this case on INT-GPR at default
//!     settings; the switch matrix still pins its fallback semantics.
//!
//! `pre`, the constant-folding nest in the first two, and the first case's
//! explicit `+ 0.0` are LOAD-BEARING: they make the bytecode compiler recycle
//! the receiver's register number earlier in the proto, which is what puts it
//! in `read_outside` and therefore in `write_through`. Drop them and the same
//! loop answers correctly — so the
//! `splitwt_mechanism_*` pins read the plan back out of a child's ZIPP_JITLOG
//! and fail if the shape stops engaging the DOUBLE tier, stops carrying a split
//! receiver, stops being a B97 candidate, or stops taking a native exit inside
//! the receiver window.

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
        .expect("node v24 on PATH (expected values come from `node -e`)");
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

/// A dense ordinary Array (a B95 pin) with ONE non-double element, read by a
/// loop whose temp shares a register number with the array's receiver. The
/// string at `a[4000]` makes the boxed-element guard deopt mid-loop, and the
/// interpreter resumes at the `GetIndex` — which must find the ARRAY in the
/// receiver slot, not the `1.5` the numeric home was holding.
const ARRAY_ELEM_DEOPT: &str = r#"
"use strict";
var N = 4096;
var a = [];
for (var i = 0; i < N; i++) a[i] = (i % 97) + 0.5;
a[4000] = "7";
var s = 0.0;
var pre = (((1+2)*(3+4))+((5+6)*(7+8)))+((((9+10)*(11+12))+((13+14)*(15+16)))*(((17+18)*(19+20))+((21+22)*(23+24))));
for (var j = 0; j < N; j++) {
  var t = ((j * 0.25) + 1.5) + 0.0;
  s = s + a[j] * t;
}
console.log(s.toFixed(4), t.toFixed(4), j, pre);
"#;

/// The same recycling, exited by a STORE past the end of a pinned
/// Float64Array. An out-of-bounds TypedArray store is a spec no-op even in
/// strict mode, so node runs the loop to `j === 4100` and reports `none`; a
/// receiver slot holding a double instead makes it a strict-mode assignment to
/// a number's index, which throws.
const TA_OOB_STORE: &str = r#"
"use strict";
var N = 4096;
var a = new Float64Array(N);
var s = 0.0;
var pre = (((1+2)*(3+4))+((5+6)*(7+8)))+((((9+10)*(11+12))+((13+14)*(15+16)))*(((17+18)*(19+20))+((21+22)*(23+24))));
var msg = "none";
try {
  for (var j = 0; j < N + 4; j++) {
    var t = (j * 0.25) + 1.5;
    a[j] = t;
    s = s + t;
  }
} catch (e) { msg = e.constructor.name + ": " + e.message; }
console.log(s.toFixed(4), j, pre, msg);
"#;

/// The reported shape: the last `getUint32` reads past the view, so the
/// RangeError is raised from a re-executed `CallMethod` after a native exit —
/// the one place the receiver slot's contents are observable as an error CLASS.
/// The `le` flag is the second recycled register here; the endian-flag `Eq` is
/// fused into the access, so the exit resumes at the elided `Eq`, one ip before
/// the call.
const DV_OOB: &str = r#"
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
"#;

#[test]
fn splitwt_parity_array_elem_deopt() {
    assert_matches_node(ARRAY_ELEM_DEOPT);
}

#[test]
fn splitwt_parity_ta_oob_store_strict() {
    assert_matches_node(TA_OOB_STORE);
}

#[test]
fn splitwt_parity_dv_oob() {
    assert_matches_node(DV_OOB);
}

/// One `[jit] DOUBLE region [S,E] B94 split receiver rN lg=[..]` line, parsed.
struct SplitRecv {
    span: String,
    reg: u16,
    lg: Vec<usize>,
}

fn split_recvs(log: &str) -> Vec<SplitRecv> {
    log.lines()
        .filter_map(|l| {
            let rest = l.split("DOUBLE region [").nth(1)?;
            let span = rest.split(']').next()?.to_string();
            let reg = rest.split("B94 split receiver r").nth(1)?;
            let lg = reg
                .split("lg=[")
                .nth(1)?
                .split(']')
                .next()?
                .split(',')
                .filter_map(|t| t.trim().parse::<usize>().ok())
                .collect();
            let reg = reg.split_whitespace().next()?.parse::<u16>().ok()?;
            Some(SplitRecv { span, reg, lg })
        })
        .collect()
}

/// One `[jit] INT-GPR region [S,E] B94 split receiver rN` line, parsed.
struct IntGprSplitRecv {
    span: String,
    reg: u16,
}

fn int_gpr_split_recvs(log: &str) -> Vec<IntGprSplitRecv> {
    log.lines()
        .filter_map(|l| {
            let rest = l.split("INT-GPR region [").nth(1)?;
            let span = rest.split(']').next()?.to_string();
            let reg = rest
                .split("B94 split receiver r")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse::<u16>()
                .ok()?;
            Some(IntGprSplitRecv { span, reg })
        })
        .collect()
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
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_GLOB_RANGE")
        .env_remove("ZIPP_NO_GPR_HOMES")
        .env_remove("ZIPP_NO_DV_GPR")
        .env_remove("ZIPP_NO_DV_DOUBLE")
        .env_remove("ZIPP_NO_WT_SHARE")
        .env_remove("ZIPP_NO_GUARD_HOIST")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_GC_STRESS")
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

/// The non-vacuity pin, and the only reason the parity cases above test
/// anything: the shape must still (a) host on the DOUBLE tier, (b) carry a B94
/// split receiver whose receiver `LoadGlobal` ip the log names, (c) be a B97
/// write-through CANDIDATE — the overlap that used to produce the clobbering
/// store, now excluded at plan level and announced by the planner instead —
/// and (d) take a native exit STRICTLY AFTER that `LoadGlobal`, i.e. inside the
/// window where the slot must hold the receiver. Without (d) the case exits
/// nowhere near the receiver and cannot observe the defect at all; without (c)
/// a change to `read_outside`/`shareable` would leave a green test that no
/// longer covers the intersection.
fn assert_split_recv_mechanism(test: &str, extra: &[(&str, &str)]) {
    let log = logged_child(test, extra);
    let srs = split_recvs(&log);
    assert!(
        !srs.is_empty(),
        "{test} no longer plans a DOUBLE-tier B94 split receiver — the case has \
         stopped exercising the write-through intersection:\n{log}"
    );
    let mut covered = None;
    for sr in &srs {
        let Some(&first_lg) = sr.lg.first() else {
            continue;
        };
        let ips = deopt_ips(&log, &sr.span);
        if ips.iter().any(|&ip| ip > first_lg) {
            covered = Some((sr, first_lg, ips));
            break;
        }
    }
    let (sr, first_lg, ips) = covered.unwrap_or_else(|| {
        panic!(
            "no native exit landed after a split receiver's LoadGlobal — the \
             receiver-clobber hazard is unpinned. splits={:?} \n{log}",
            srs.iter()
                .map(|s| (&s.span, s.reg, &s.lg))
                .collect::<Vec<_>>()
        )
    });
    assert!(
        log.contains(&format!(
            "region [{}] B97 write-through excludes B94 split receiver r{}",
            sr.span, sr.reg
        )),
        "r{} of region [{}] is no longer a B97 write-through candidate, so this \
         case no longer covers the set intersection the bug lived in \
         (exit ips {:?} after lg {}):\n{log}",
        sr.reg,
        sr.span,
        ips,
        first_lg
    );
}

#[test]
fn splitwt_mechanism_array_elem_deopt() {
    assert_split_recv_mechanism("splitwt_parity_array_elem_deopt", &[]);
}

#[test]
fn splitwt_mechanism_ta_oob_store_strict() {
    assert_split_recv_mechanism("splitwt_parity_ta_oob_store_strict", &[]);
}

/// Capture-first member calls add a guarded `GetProp` before each DataView
/// `CallWithThis`. That makes the old `ZIPP_NO_GLOB_RANGE=1` forced-DOUBLE
/// fixture safely decline to MEM: its recycled endian Bool is live across the
/// next potentially-deopting lookup, while type-split homes are GPR-only. Pin
/// the production route instead: default settings must compile INT-GPR with a
/// B94 receiver that is also a B97 candidate, and must take a native exit from
/// that region. The two tests above retain the original DOUBLE-tier coverage.
#[test]
fn splitwt_mechanism_dv_oob_int_gpr() {
    let log = logged_child("splitwt_parity_dv_oob", &[]);
    let srs = int_gpr_split_recvs(&log);
    let covered = srs.iter().find(|sr| {
        log.contains(&format!(
            "region [{}] B97 write-through excludes B94 split receiver r{}",
            sr.span, sr.reg
        ))
    });
    let sr = covered.unwrap_or_else(|| {
        panic!(
            "DataView default route no longer compiles an INT-GPR B94 receiver that is also a B97 candidate: {:?}\n{log}",
            srs.iter()
                .map(|sr| (&sr.span, sr.reg))
                .collect::<Vec<_>>()
        )
    });
    assert!(
        log.contains(&format!("INT region fn0 [{}] compiled", sr.span)),
        "the intersecting INT-GPR plan was not installed:\n{log}"
    );
    let start = sr
        .span
        .split(',')
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .expect("logged region start is numeric");
    let ips = deopt_ips(&log, &sr.span);
    assert!(
        ips.iter().any(|&ip| ip > start),
        "the intersecting INT-GPR region took no in-body native exit: {ips:?}\n{log}"
    );
}

/// Every case must answer identically in every mode — including alternate
/// planner routes and `ZIPP_NO_WT_SHARE=1`, which empties `write_through` and so
/// isolates the vector: before the fix it was the only mode in which these
/// shapes answered correctly.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 9] = [
        &[("ZIPP_NO_GLOB_RANGE", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_DV_GPR", "1")],
        &[("ZIPP_NO_DV_DOUBLE", "1")],
        &[("ZIPP_NO_WT_SHARE", "1")],
        &[("ZIPP_NO_GUARD_HOIST", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("splitwt_parity_");
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
            "the splitwt_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
