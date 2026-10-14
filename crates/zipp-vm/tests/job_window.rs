//! A run of promise reaction jobs that call one compiled int-tier callback
//! runs over one register window the drain keeps open across the run
//! (async_runtime.rs, `run_job_windowed`), as `forEach` runs its callback.
//! Each case changes what the window must survive: a 20,000-job run,
//! alternating callbacks (the window reopened per job), two chains whose
//! jobs interleave, a bail past the entry inside the window (an int overflow
//! to a double), a RangeError from deep self-recursion, a self-recursive
//! callback whose global binding is replaced between two runs (the call
//! must see the new binding), and a run of rejection handlers. Node
//! 24.19.0's output, with the window on, off (ZIPP_NO_JOB_WINDOW=1), under
//! ZIPP_GC_STRESS=1 (without the deep recursion), ZIPP_NOJIT=1 and
//! ZIPP_JIT_THRESHOLD=1, and with ZIPP_ASYNCSTATS=1's count of windowed jobs
//! showing the window engaged.

const SRC: &str = include_str!("job_window.js");

const EXPECTED: &str = "-- long run\nlong 20000\n\
-- alternating\nalternating -2999\n\
-- two chains interleave\ninterleave 2000,-5900\n\
-- bail inside the window\nbail 3458764513820541000\n\
-- deep recursion throws\ndeep RangeError\n\
-- rebound self-call\nrf before 55\nrf after 2017\n\
-- same callback, rejection handler\nreject run 105";

/// The deep recursion's step, which ZIPP_GC_STRESS=1 skips: a collection
/// at every safe point of a 100,000-frame recursion takes minutes in a
/// debug build.
const DEEP: &str = "-- deep recursion throws\ndeep RangeError\n";

#[test]
fn job_window_child() {
    let Some(mode) = std::env::var_os("ZIPP_JOB_WINDOW_CHILD") else {
        return;
    };
    let (src, expected) = if mode == "gc" {
        let src = SRC.replacen("\"use strict\";", "\"use strict\";\nvar SKIP_DEEP = 1;", 1);
        let expected = EXPECTED.replacen(DEEP, "", 1);
        assert!(src != SRC && expected != EXPECTED, "the gc mode must skip the deep step");
        (src, expected)
    } else {
        (SRC.to_string(), EXPECTED.to_string())
    };
    let out = zipp_vm::run(&src).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output.join("\n"), expected);
    let hits = zipp_vm::async_job_window_stats();
    let compiled = cfg!(all(feature = "jit", target_arch = "x86_64"));
    match mode.to_str() {
        Some("off") | Some("nojit") => assert_eq!(hits, 0, "no job may take the window"),
        // The long run alone is 20,000 jobs of one compiled callback.
        _ if compiled => assert!(hits > 15_000, "the job window did not engage: {hits}"),
        _ => assert_eq!(hits, 0, "a build without the JIT has no window"),
    }
}

#[test]
fn job_window_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_JOB_WINDOW_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, var) in [
        ("on", None),
        ("off", Some("ZIPP_NO_JOB_WINDOW")),
        ("gc", Some("ZIPP_GC_STRESS")),
        ("nojit", Some("ZIPP_NOJIT")),
        ("threshold", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "job_window_child", "--nocapture"])
            .env("ZIPP_JOB_WINDOW_CHILD", mode)
            .env("ZIPP_ASYNCSTATS", "1");
        if let Some(var) = var {
            cmd.env(var, "1");
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "mode {mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
