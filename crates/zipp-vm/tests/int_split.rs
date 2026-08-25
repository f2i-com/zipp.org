//! B94 receiver splitting on the INT tier: the typedarray-math i32 xorshift
//! fill (`iv[i] = st` where the bytecode compiler recycles `st`'s register as
//! both the pinned Int32Array receiver and the running xorshift value)
//! declined the whole region with "pinned receiver reg not cleanly
//! excludable". The split gives the register an i64 home while its memory
//! slot stays the receiver's, with every numeric def written through BOXED
//! and the flush skipping it.
//!
//! The XMM-emitter mechanism is OPT-IN (`ZIPP_INT_SPLIT=1`; default off — see
//! `int_split_enabled` in plan_region.rs for the measured refutation). Since
//! W8, DEFAULT settings route a split plan into the GPR-home emitter instead
//! (see `int_gpr_split.rs`), so the default runs below exercise that path.
//! Every `isplit_parity_*` case asserts byte-identical output against
//! `node -e` at DEFAULT settings;
//! the final test re-runs the set in four more modes in child processes:
//! `ZIPP_INT_SPLIT=1` (the mechanism ON, default thresholds),
//! `ZIPP_INT_SPLIT=1` + `ZIPP_JIT_THRESHOLD=1` (ON, compile everything
//! immediately), `ZIPP_INT_SPLIT=1` + `ZIPP_GC_STRESS=1` (ON under GC stress —
//! the receiver slot must stay a traceable Value at every safe point; a raw
//! i64 flushed over it would be a forged root), and `ZIPP_NOJIT=1` (pure
//! interpreter).

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

/// The bench's exact fill shape (bench/real/typedarray-math.js:24-29), scaled
/// down: top-level vars (globals), xorshift32 into `st`, `iv[i] = st | 0` —
/// the recycled-receiver region that now compiles on the INT tier.
#[test]
fn isplit_parity_xorshift_fill_shape() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 200000;
        var iv = new Int32Array(NI);
        var st = 0x9E3779B9 | 0;
        for (var i = 0; i < NI; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i] = st | 0;
        }
        var s = 0;
        for (var i = 0; i < NI; i += 1000) s = (s + iv[i]) | 0;
        console.log(iv[0] + " " + iv[1] + " " + iv[NI - 1] + " " + st + " " + s);
        "#,
    );
}

/// The RECEIVER global reassigned INSIDE the loop (two Int32Arrays): the
/// identity guard misses on the swapped iterations, the region deopts and
/// re-enters, and every store must land in the CURRENT `iv` — while `st`
/// keeps threading through the recycled register.
#[test]
fn isplit_parity_receiver_reassigned_mid_loop() {
    assert_matches_node(
        r#"
        "use strict";
        var a1 = new Int32Array(4096);
        var a2 = new Int32Array(4096);
        var iv = a1;
        var st = 0x12345678 | 0;
        for (var i = 0; i < 60000; i++) {
          if ((i & 8191) === 8191) iv = (iv === a1) ? a2 : a1;
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i & 4095] = (st + i) | 0;
        }
        var s1 = 0, s2 = 0;
        for (var i = 0; i < 4096; i += 97) { s1 = (s1 + a1[i]) | 0; s2 = (s2 + a2[i]) | 0; }
        console.log(st + " " + s1 + " " + s2);
        "#,
    );
}

/// Mixed int/float writes in one loop: the Int32Array store keeps the split
/// INT shape while a Float64Array write and fractional arithmetic force the
/// sibling region off the int tier — values crossing between them must stay
/// node-identical (the split register's slot is read by the interpreter
/// whenever the region exits).
#[test]
fn isplit_parity_mixed_int_float_writes() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 50000;
        var iv = new Int32Array(NI);
        var fv = new Float64Array(NI);
        var st = 0x9E3779B9 | 0;
        for (var i = 0; i < NI; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i] = st | 0;
        }
        var fs = 0;
        for (var i = 0; i < NI; i++) {
          fv[i] = (iv[i] >>> 8) / 256 + 0.5;
          fs += fv[i];
        }
        var s = 0;
        for (var i = 0; i < NI; i += 500) s = (s + iv[i]) | 0;
        console.log(st + " " + s + " " + fs.toFixed(6) + " " + fv[7].toFixed(6));
        "#,
    );
}

/// The split value read AFTER the loop plus an early exit taken mid-iteration
/// (a `break` on a value condition): every exit path must leave `st`'s frame
/// slot holding the boxed CURRENT value (write-through), never the receiver
/// object or a stale number.
#[test]
fn isplit_parity_early_exit_reads_split_value() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(8192);
        var st = 0x0BADF00D | 0;
        var stop = -1;
        for (var i = 0; i < 2000000; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i & 8191] = st | 0;
          if ((st & 1023) === 7) { stop = i; break; }
        }
        console.log(st + " " + stop + " " + iv[stop >= 0 ? (stop & 8191) : 0] + " " + (typeof st));
        "#,
    );
}

/// Re-run every `isplit_parity_` case in four more modes, each in its own
/// child process (the env latches are read once per process). The same
/// node-derived assertions passing in all modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_INT_SPLIT", "1")],
        &[("ZIPP_INT_SPLIT", "1"), ("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_INT_SPLIT", "1"), ("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("isplit_parity_");
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
            "the isplit_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
