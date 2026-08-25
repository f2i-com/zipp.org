//! W8: B94 split receivers on GPR homes — the follow-up `int_split_enabled`'s
//! refutation named ("the real blocker is gpr (not xmm) homes for
//! bitwise-chain regions"). With the split OFF by default, the typedarray-math
//! i32 xorshift fill (`iv[i] = st` with `st`'s register recycled as both the
//! pinned Int32Array receiver and the running xorshift value) failed
//! `plan_region` outright and ran on the MEM tier. `compile_region_int` now
//! retries the split plan ONLY into the GPR emitter (the xmm emitter still
//! never sees a split plan unless `ZIPP_INT_SPLIT=1`): each numeric def of the
//! split register is written through BOXED to its reg-file slot before any i53
//! guard, the receiver's LoadGlobal stores the object to the same slot, and
//! flush_exit skips the register — memory is what the interpreter reads on any
//! exit.
//!
//! Every `gsplit_parity_*` case asserts byte-identical output against
//! `node -e` at DEFAULT settings (the mechanism ON — the fill shape hosts on
//! GPR homes). The final test re-runs the set in five more modes in child
//! processes: `ZIPP_NO_GPR_SPLIT=1` (the W8 retry alone off — the region falls
//! to the MEM tier exactly as before), `ZIPP_NO_GPR_HOMES=1` (the whole GPR
//! sub-mode off — the retry must decline cleanly), `ZIPP_JIT_THRESHOLD=1`
//! (compile everything immediately), `ZIPP_GC_STRESS=1` (the split register's
//! slot must hold a traceable Value at every safe point — a raw i64 or a
//! skipped write-through would be a forged or stale root), and `ZIPP_NOJIT=1`
//! (pure interpreter).

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
/// the recycled-receiver region that now hosts on GPR homes by default.
#[test]
fn gsplit_parity_xorshift_fill_shape() {
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

/// Out-of-bounds and negative store indices taken MID-LOOP: the per-access
/// unsigned bounds check must deopt, the interpreter performs the spec
/// coerce-then-silent-noop, and the loop re-enters — with the split register's
/// slot reading back the CURRENT xorshift value each time, not the receiver.
#[test]
fn gsplit_parity_oob_and_negative_store() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(1024);
        var st = 0x1234ABCD | 0;
        for (var i = 0; i < 30000; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          var k = i & 2047;              // 1024..2047 are OOB half the time
          if ((i & 63) === 63) k = -1;   // negative index: silent noop too
          iv[k] = st | 0;
        }
        var s = 0;
        for (var i = 0; i < 1024; i += 31) s = (s + iv[i]) | 0;
        console.log(st + " " + s + " " + iv[1023] + " " + (typeof st));
        "#,
    );
}

/// The RECEIVER global reassigned INSIDE the loop (two Int32Arrays): the
/// identity guard misses on the swapped iterations, the region deopts and
/// re-enters, and every store must land in the CURRENT `iv` — while `st`
/// keeps threading through the recycled register.
#[test]
fn gsplit_parity_receiver_reassigned_mid_loop() {
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

/// The buffer DETACHED (`transfer()`) between two fill loops over the same
/// global: the second region entry snapshots a dead view ({0,0,0}), the entry
/// guard bails, and the interpreter runs the spec semantics (stores to a
/// detached view are silent noops, reads are undefined, length is 0).
#[test]
fn gsplit_parity_detach_between_loops() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(2048);
        var st = 0x0BADF00D | 0;
        for (var i = 0; i < 40000; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i & 2047] = st | 0;
        }
        var kept = iv[100];
        iv.buffer.transfer();
        for (var i = 0; i < 40000; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i & 2047] = st | 0;
        }
        console.log(kept + " " + st + " " + iv[100] + " " + iv.length);
        "#,
    );
}

/// An i53 range guard FIRING on a def inside the split region: `acc` doubles
/// every iteration until it leaves ±2^53, the guard exits at ip+1 expecting
/// the result flushed, and from then on the loop runs interpreted with `acc`
/// a double — every value (and `st`'s slot) must stay node-identical across
/// the transition.
#[test]
fn gsplit_parity_i53_guard_exit() {
    assert_matches_node(
        r#"
        "use strict";
        var iv = new Int32Array(256);
        var st = 0x5EED | 0;
        var acc = 3;
        for (var i = 0; i < 20000; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i & 255] = st | 0;
          if (i < 60) acc = acc + acc;
        }
        var s = 0;
        for (var i = 0; i < 256; i += 7) s = (s + iv[i]) | 0;
        console.log(st + " " + s + " " + acc);
        "#,
    );
}

/// An early `break` on a value condition: every exit path must leave `st`'s
/// frame slot holding the boxed CURRENT value (write-through), never the
/// receiver object or a stale number.
#[test]
fn gsplit_parity_early_exit_reads_split_value() {
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

/// Re-run every `gsplit_parity_` case in five more modes, each in its own
/// child process (the env latches are read once per process). The same
/// node-derived assertions passing in all modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 5] = [
        &[("ZIPP_NO_GPR_SPLIT", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("gsplit_parity_");
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
            "the gsplit_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
