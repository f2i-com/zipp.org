//! A wide self-call must not enter Tier A: its cold helper ABI carries argc in
//! eight bits. The first call initializes the recursive register high-water;
//! the second changes the last formal so stale slots cannot mask truncation.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_TIER_A_ARG_WIDTH_CHILD";
const MARKER: &str = "[tier-a-arg-width] ";
const EXPECTED: &str = "777|888";

fn source() -> String {
    let params = (0..=256)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let forwarded = (1..=256)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let middle = vec!["7"; 255].join(",");
    format!(
        "function f({params}) {{ if (p0 === 0) return p256; return f(p0 - 1,{forwarded}); }}\n\
         console.log(f(257,{middle},777));\n\
         console.log(f(257,{middle},888));\n"
    )
}

fn child_run() {
    let outcome = zipp_vm::run(&source()).expect("wide self-call source compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, ["777", "888"]);
    eprintln!("{MARKER}{}", outcome.output.join("|"));
}

fn run_fresh(nojit: bool) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("wide_self_call_declines_tier_a")
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT");
    if nojit {
        command.env("ZIPP_NOJIT", "1");
    }
    command.output().expect("spawn wide self-call child")
}

#[test]
fn wide_self_call_declines_tier_a() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_run();
        return;
    }

    for nojit in [false, true] {
        let output = run_fresh(nojit);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "child failed (nojit={nojit}):\nstdout:\n{}\nstderr:\n{stderr}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(stderr.contains(&format!("{MARKER}{EXPECTED}")), "{stderr}");
        if !nojit {
            assert!(
                !stderr.contains("[jit] Tier A fn1 compiled"),
                "wide call entered Tier A:\n{stderr}"
            );
        }
    }
}
