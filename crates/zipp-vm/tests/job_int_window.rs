//! A promise reaction's callback runs as a job the host starts, with no
//! frame under it. When the int tier compiled its body, `call_compiled_windowed`
//! now runs it frame-free there too (engine/run.rs): such a body calls no
//! helper but its own self-call, which pushes the frames it needs. These
//! chains drive that path through a hot chain, a bail past the entry (an int
//! overflow to a double, a division), self-recursion (`fib`, and `down` deep
//! enough to leave the native depth), a strict callback's `this`, a rejection
//! handler and a mix of callbacks. Node 24.19.0's output, with the path on,
//! off (ZIPP_NO_JOB_INT_WINDOW=1), under ZIPP_GC_STRESS=1, ZIPP_NOJIT=1 and
//! ZIPP_JIT_THRESHOLD=1.

const SRC: &str = include_str!("job_int_window.js");

const EXPECTED: &str = "-- chain\nchain 5000\n\
-- overflow to double\ndbl 1099511627776\n\
-- division bail\nhalf 0.03125\n\
-- fib as reaction\nfib sum 164175\n\
-- deep recursion\ndown 5000\n\
-- this is undefined\nthis undefined\n\
-- rejection callback\nrejected+1 11\n\
-- mixed callbacks\nmixed 1340";

#[test]
fn job_int_window_child() {
    if std::env::var_os("ZIPP_JOB_INT_WINDOW_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn job_int_window_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_JOB_INT_WINDOW_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (name, var) in [
        ("on", None),
        ("off", Some("ZIPP_NO_JOB_INT_WINDOW")),
        ("gc", Some("ZIPP_GC_STRESS")),
        ("nojit", Some("ZIPP_NOJIT")),
        ("threshold", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "job_int_window_child", "--nocapture"])
            .env("ZIPP_JOB_INT_WINDOW_CHILD", "1");
        if let Some(var) = var {
            cmd.env(var, "1");
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "mode {name} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
