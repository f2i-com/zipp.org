//! A minor collection's root walk skips the queued jobs from before the last
//! collection (gc.rs, `mark_roots`; `Vm::microtasks_old`): every Value they
//! hold was a root of that collection and was promoted, and a queued job
//! never changes. The program queues jobs across several minors that hold
//! the only references to fresh objects, closures, thenables and async
//! activations, mutates an old job's argument to hold a young object, and
//! settles a `Promise.all` over long-queued promises. Node 24.19.0's output,
//! with the skip on, off (ZIPP_NO_OLD_JOBS_SKIP=1), under
//! ZIPP_NURSERY_VERIFY=1 (a full mark beside every minor, which panics on a
//! young object the minor missed) and ZIPP_NO_NURSERY=1. Not under
//! ZIPP_GC_STRESS=1: a collection at every safe point with tens of thousands
//! of jobs queued takes minutes in a release build and hours in a test one;
//! the verifier checks every minor in full instead.

const SRC: &str = include_str!("microtask_old_jobs.js");

const EXPECTED: &str = "all 12497500\ntotal 3604038000";

#[test]
fn old_jobs_child() {
    if std::env::var_os("ZIPP_OLD_JOBS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn old_jobs_skip_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_OLD_JOBS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, var) in [
        ("on", None),
        ("off", Some("ZIPP_NO_OLD_JOBS_SKIP")),
        ("verify", Some("ZIPP_NURSERY_VERIFY")),
        ("no-nursery", Some("ZIPP_NO_NURSERY")),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "old_jobs_child", "--nocapture"])
            .env("ZIPP_OLD_JOBS_CHILD", "1");
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
