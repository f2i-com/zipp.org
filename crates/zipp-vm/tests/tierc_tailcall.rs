//! W12: Tier-C `TailCall` admission — the compiler emits `TailCall` as a
//! frame-reuse PREFIX immediately followed by an ordinary `Call`+`Return` of
//! the same site, so Tier C admits the op and emits NOTHING for it. What a
//! compiled body forgoes is only the constant-stack reuse: a tail chain
//! cross-calls to JIT_SELF_RECURSE_MAX, deopts to the interpreter, and frame
//! reuse resumes there (streak-bounded at MAX_TAIL_REUSE_STREAK with the same
//! catchable RangeError as before). These tests pin both directions: a
//! 500k-deep strict tail loop COMPLETES with the function compiled, and a
//! runaway (>1M streak) still throws catchable. `ZIPP_NO_TIERC_TAILCALL=1`
//! restores the blacklist (re-run in a child, the latch is per-process).

use std::sync::mpsc;
use std::time::Duration;

/// Run `src` in-process with a timeout guard (a tail-reuse regression HANGS
/// rather than fails) on the big stack the CLI gives the VM thread.
fn run_guarded(src: &'static str) -> Vec<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(zipp_vm::run(src));
        })
        .expect("spawn guarded VM thread");
    let out = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("engine hung: tail chain neither completed nor threw")
        .expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const DEEP_TAIL_SRC: &str = r#"
    "use strict";
    function f(n) { if (n <= 0) return 0; return f(n - 1); }
    for (var i = 0; i < 4000; i++) f(8);
    console.log("deep:" + f(500000));
"#;

/// Strict self tail recursion 500,000 deep — past MAX_FRAMES (100k), far under
/// the 1M reuse-streak budget — must COMPLETE after the warm-up loop has made
/// the function Tier-C hot (node v24 overflows at ~10k on this shape; the
/// constant-stack completion is zipp's intentional PTC divergence).
#[test]
fn ttc_parity_deep_tail_500k() {
    assert_eq!(run_guarded(DEEP_TAIL_SRC), ["deep:0"]);
}

/// The mutual pair a() <-> b(): both warm, both tail-call the other, 500k deep
/// from each entry (even step count, so each chain ends in its own starter).
#[test]
fn ttc_parity_mutual_tail_pair() {
    let out = run_guarded(
        r#"
        "use strict";
        function a(n) { if (n <= 0) return "a"; return b(n - 1); }
        function b(n) { if (n <= 0) return "b"; return a(n - 1); }
        for (var i = 0; i < 4000; i++) a(8);
        console.log("mutual:" + a(500000) + b(500000));
        "#,
    );
    assert_eq!(out, ["mutual:ab"]);
}

/// A runaway chain (>1M reuse streak) in a WARMED (Tier-C-compilable) function
/// must throw the same catchable RangeError as today, and execution continues
/// after the catch. The warm calls take the early-return branch, so only the
/// final call runs away.
#[test]
fn ttc_parity_runaway_still_catchable() {
    let out = run_guarded(
        r#"
        "use strict";
        function inf(n) { if (n === -1) return 0; return inf(n + 1); }
        for (var i = 0; i < 4000; i++) inf(-1);
        try { inf(0); console.log("none"); } catch (e) {
          console.log("caught:" + (e instanceof RangeError));
        }
        console.log("after:" + inf(-1));
        "#,
    );
    assert_eq!(out, ["caught:true", "after:0"]); // node v24 (kind matches; depth differs)
}

/// Engagement: under ZIPP_JITLOG + threshold-1 the tail-recursive shape must
/// COMPILE Tier C (no `[tierC-reject] op TailCall` blacklist line). Runs the
/// deep-tail case in a child so the log lines land on a capturable stderr.
#[test]
fn tierc_jitlog_shows_tailcall_admitted() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("ttc_parity_deep_tail_500k")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stderr}");
    assert!(
        !stderr.contains("[tierC-reject] op TailCall"),
        "TailCall still blacklists Tier C:\n{stderr}"
    );
    assert!(
        stderr.contains("Tier C fn") && stderr.contains("compiled"),
        "no Tier C compile logged for the tail-recursive shape:\n{stderr}"
    );
}

/// Same child, switch OFF: the blacklist line must come back and the answers
/// must not change (the off path is today's behavior).
#[test]
fn tierc_jitlog_off_switch_restores_reject() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("ttc_parity_deep_tail_500k")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_NO_TIERC_TAILCALL", "1")
        .output()
        .expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stderr}");
    assert!(
        stderr.contains("[tierC-reject] op TailCall (disabled)"),
        "off-switch did not restore the blacklist:\n{stderr}"
    );
}

/// Re-run every `ttc_parity_` case with the admission off, the JIT off, and
/// the JIT forced hot, each in its own child process (env latches are read
/// once per process). Identical answers in all modes is the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_TIERC_TAILCALL", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
    ] {
        let mut child = std::process::Command::new(&exe)
            .arg("ttc_parity_")
            .env(key, val)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the test binary");
        // Timeout guard: a tail-reuse regression HANGS — never wedge CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        let status = loop {
            match child.try_wait().expect("wait on the test binary") {
                Some(s) => break s,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    panic!("{key}={val} mode HUNG");
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
            "the ttc_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
