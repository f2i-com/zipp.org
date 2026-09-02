//! B263: register classes the INT region tier can plan.
//!
//! Two allocation rules keep a tokenizer-shaped loop on the INT tier while
//! the ordinary scratch stack keeps its v0.0.5 reclaim. A receiver read from a
//! global (`src.charCodeAt(i)`) is placed in a receiver class register that no
//! reclaim ever hands out again, so it has exactly one definition
//! (`plan_region`'s "cleanly excludable" pinned-receiver rule). A
//! boolean-valued expression is placed in a boolean class register, so no
//! register is defined as both a number and a boolean ("type conflict on a
//! reused register" declined whole loops to the boxed MEM tier: parse-large-js
//! 251 -> 396 ms). Class registers are renumbered to the top of the frame at
//! finalisation.
//!
//! `ZIPP_NO_REG_CLASSES=1` restores the shared scratch stack; the children
//! below run the file under that latch and under every execution mode. The
//! expected output is node-oracled (v24.12.0).

const SRC: &str = include_str!("reg_classes.js");
const EXPECTED: [&str; 2] = ["160000:934504848", "dv:-1726487896"];

#[test]
fn reg_classes_output_matches_node() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, EXPECTED.iter().map(|s| s.to_string()).collect::<Vec<_>>());
}

/// The tokenize loop plans on the INT tier: no `[decline-reason] fn=tokenize`
/// line under `ZIPP_JITDECLINE=1`. With the shared scratch stack restored the
/// same loop declines (a recycled receiver or a reused-register type
/// conflict, whichever the allocator happens to produce first).
// Only the x86-64 JIT has an INT tier to keep the loop on; the interpreter
// builds check the same file's output in the two tests around this one.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn reg_classes_keeps_the_tokenizer_on_the_int_tier() {
    if std::env::var_os("ZIPP_REG_CLASSES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (latched, expect_decline) in [(false, false), (true, true)] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["reg_classes_output_matches_node", "--nocapture"])
            .env("ZIPP_REG_CLASSES_CHILD", "1")
            .env("ZIPP_JITDECLINE", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_NURSERY")
            .env_remove("ZIPP_NO_REG_CLASSES");
        if latched {
            cmd.env("ZIPP_NO_REG_CLASSES", "1");
        }
        let out = cmd.output().expect("spawn child");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "child failed (latched={latched}):\n{stderr}");
        let tokenize_declines: Vec<&str> = stderr
            .lines()
            .filter(|l| l.starts_with("[decline-reason] fn=tokenize"))
            .collect();
        if expect_decline {
            assert!(
                !tokenize_declines.is_empty(),
                "latch off: expected tokenize to decline the INT tier, got:\n{stderr}"
            );
        } else {
            assert!(
                tokenize_declines.is_empty(),
                "tokenize declined the INT tier:\n{}",
                tokenize_declines.join("\n")
            );
        }
    }
}

/// Exact output in every execution mode and with the latch off. Each child is
/// a fresh process (the latch is read once per process).
#[test]
fn reg_classes_matches_in_every_mode_and_with_the_latch_off() {
    if std::env::var_os("ZIPP_REG_CLASSES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    const LATCHES: [&str; 4] = [
        "ZIPP_NO_REG_CLASSES",
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
    ];
    for (mode, env) in [
        ("default", None),
        ("no-reg-classes", Some(("ZIPP_NO_REG_CLASSES", "1"))),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("no-nursery", Some(("ZIPP_NO_NURSERY", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["reg_classes_output_matches_node", "--nocapture"])
            .env("ZIPP_REG_CLASSES_CHILD", "1");
        for l in LATCHES {
            cmd.env_remove(l);
        }
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
