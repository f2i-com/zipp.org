//! B117: strict-mode runaway recursion must throw a catchable RangeError, not
//! hang. Strict `return f()` is a proper tail call here (frame reuse, O(1)
//! stack), so it never tripped MAX_FRAMES — the interpreter looped forever
//! where node (no PTC) overflows at ~10k depth. The fix bounds the streak of
//! pop-free tail reuses (MAX_TAIL_REUSE_STREAK, ~100x node's tolerance) so the
//! runaway shape throws the same catchable RangeError as sloppy mode, while a
//! legitimate deep-but-terminating tail loop keeps its constant-stack win.
//!
//! Every `rec_parity_*` case runs in-process at default settings under a
//! timeout guard (a regression HANGS, it doesn't fail). The final test re-runs
//! the set with `ZIPP_NOJIT=1` (pure interpreter) and `ZIPP_JIT_THRESHOLD=1`
//! (compile everything immediately), each in its own child process (the env
//! latches are read once per process).

use std::sync::mpsc;
use std::time::Duration;

/// Run `src` in-process with a timeout guard: a hang (the B117 symptom) fails
/// the test instead of wedging the suite.
fn run_guarded(src: &'static str) -> Vec<String> {
    let (tx, rx) = mpsc::channel();
    // Same big stack the CLI gives the VM thread (zipp-cli/src/main.rs): the
    // MAX_RUN_LOOP_DEPTH native-re-entry budget assumes far more than a test
    // thread's 2 MiB default, especially for debug-sized frames.
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(zipp_vm::run(src));
        })
        .expect("spawn guarded VM thread");
    let out = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("engine hung (B117 regression): runaway recursion did not throw")
        .expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Runaway MUTUAL recursion, strict: the original B117 hang. Tail reuse keeps
/// the frame count flat, so only the reuse-streak budget can stop it.
#[test]
fn rec_parity_strict_mutual_runaway() {
    let out = run_guarded(
        r#"
        "use strict";
        function inf() { return inf2(); }
        function inf2() { return inf(); }
        try { inf(); console.log("none"); } catch (e) {
          console.log("caught:" + (e instanceof RangeError));
        }
        console.log("after");
        "#,
    );
    assert_eq!(out, ["caught:true", "after"]); // node v24 (message text differs; kind matches)
}

/// Runaway SELF recursion, strict — the single-function shape of the same bug.
#[test]
fn rec_parity_strict_self_runaway() {
    let out = run_guarded(
        r#"
        "use strict";
        function inf() { return inf(); }
        try { inf(); console.log("none"); } catch (e) {
          console.log("caught:" + (e instanceof RangeError));
        }
        console.log("after");
        "#,
    );
    assert_eq!(out, ["caught:true", "after"]); // node v24
}

/// Sloppy mutual + self runaway: the already-working MAX_FRAMES path — must
/// stay a catchable RangeError.
#[test]
fn rec_parity_sloppy_runaway() {
    let out = run_guarded(
        r#"
        function inf(n) { return inf2(n + 1); }
        function inf2(n) { return inf(n + 1); }
        try { inf(0); console.log("none"); } catch (e) {
          console.log("m:" + (e instanceof RangeError));
        }
        function selfy(n) { return selfy(n + 1); }
        try { selfy(0); console.log("none"); } catch (e) {
          console.log("s:" + (e instanceof RangeError));
        }
        console.log("after");
        "#,
    );
    assert_eq!(out, ["m:true", "s:true", "after"]); // node v24
}

/// Deep-but-TERMINATING recursion at node's typical overflow depth (~10.4k
/// measured for this shape on node v24) must still succeed in both modes —
/// non-tail (frame-growing) and strict tail-shaped alike.
#[test]
fn rec_parity_deep_terminating_recursion_succeeds() {
    let out = run_guarded(
        r#"
        function down(n) { if (n <= 0) return 0; return down(n - 1) + 1; }
        console.log("sloppy:" + down(10000));
        "#,
    );
    assert_eq!(out, ["sloppy:10000"]); // node v24
    let out = run_guarded(
        r#"
        "use strict";
        function down(n) { if (n <= 0) return 0; return down(n - 1) + 1; }
        console.log("strict:" + down(10000));
        "#,
    );
    assert_eq!(out, ["strict:10000"]); // node v24
}

/// A strict tail-shaped loop DEEPER than node tolerates (100k iterations —
/// node overflows at ~10k on this shape) must keep the PTC constant-stack win
/// and terminate normally: the streak budget (1M) sits far above it.
#[test]
fn rec_parity_long_tail_loop_still_constant_stack() {
    let out = run_guarded(
        r#"
        "use strict";
        function loop(n, acc) { if (n === 0) return acc; return loop(n - 1, acc + 1); }
        console.log("tail:" + loop(100000, 0));
        "#,
    );
    assert_eq!(out, ["tail:100000"]); // node v24 overflows here; PTC keeps it — intentional divergence
}

/// The overflow is CATCHABLE and execution continues: catch it twice in a row
/// (proving the streak counter resets) and keep computing after.
#[test]
fn rec_parity_catch_and_continue() {
    let out = run_guarded(
        r#"
        "use strict";
        function inf() { return inf2(); }
        function inf2() { return inf(); }
        var hits = 0;
        try { inf(); } catch (e) { if (e instanceof RangeError) hits++; }
        try { inf(); } catch (e) { if (e instanceof RangeError) hits++; }
        function down(n) { if (n <= 0) return 0; return down(n - 1) + 1; }
        console.log("hits:" + hits + " then:" + down(5000));
        "#,
    );
    assert_eq!(out, ["hits:2 then:5000"]); // node v24
}

/// Re-run every `rec_parity_` case with the pure interpreter and with
/// threshold-1 JIT, each in its own child process. The same assertions passing
/// in all three modes is the parity check; a per-child timeout guards against
/// the hang coming back in a mode the in-process runs don't cover.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [("ZIPP_NOJIT", "1"), ("ZIPP_JIT_THRESHOLD", "1")] {
        let mut child = std::process::Command::new(&exe)
            .arg("rec_parity_")
            .env(key, val)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the test binary");
        // Timeout guard: the bug is a HANG — never let a regression wedge CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        let status = loop {
            match child.try_wait().expect("wait on the test binary") {
                Some(s) => break s,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    panic!("{key}={val} mode HUNG (B117 regression)");
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        let out = child.wait_with_output().expect("collect child output");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the rec_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
