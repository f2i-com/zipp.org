//! Wide numeric leaf inlining remains a pure guarded prefix: module globals
//! are read live, and a later non-Int representation falls back to the real
//! call instead of exposing stale/baked values. Fresh children exercise both
//! independently cached ablation switches.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_WIDE_LEAF_TEST_CHILD";
const ROUTE_CHILD_ENV: &str = "ZIPP_WIDE_ROUTE_TEST_CHILD";
const OUTPUT_MARKER: &str = "[wide-leaf-test] ";
const ROUTE_OUTPUT_MARKER: &str = "[wide-route-test] ";
const EXPECTED: &str = "-377466008|13";

fn fixture_entry() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("wide_leaf_module")
        .join("entry.mjs")
}

fn child_run() {
    let outcome = zipp_vm::run_module_file(&fixture_entry(), None).expect("module compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected module error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, [EXPECTED]);
    eprintln!("{OUTPUT_MARKER}{}", outcome.output[0]);
}

fn run_fresh(ablate: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("--exact")
        .arg("wide_leaf_reads_live_module_globals_and_falls_back")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_MODULE_JIT")
        .env_remove("ZIPP_NO_WIDE_LEAF")
        .env_remove("ZIPP_NO_TYPED_GLOBAL_LOAD");
    if let Some(flag) = ablate {
        command.env(flag, "1");
    }
    command.output().expect("spawn fresh test process")
}

fn assert_child_ok(output: &std::process::Output, mode: &str) -> String {
    assert!(
        output.status.success(),
        "{mode} child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains(OUTPUT_MARKER),
        "{mode} child omitted output marker:\n{stderr}"
    );
    stderr
}

fn has_guarded_typed_lane(log: &str) -> bool {
    log.lines().any(|line| {
        let Some(rest) = line
            .strip_prefix("[leaf] ")
            .and_then(|line| line.split_once("TYPED-LANE (ops=").map(|(_, rest)| rest))
        else {
            return false;
        };
        rest.split_once(" guards=")
            .and_then(|(_, guards)| guards.strip_suffix(')'))
            .and_then(|guards| guards.parse::<u16>().ok())
            .is_some_and(|guards| guards > 0)
    })
}

#[test]
fn wide_leaf_reads_live_module_globals_and_falls_back() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_run();
        return;
    }

    let enabled = assert_child_ok(&run_fresh(None), "enabled");
    assert!(
        enabled.contains("INLINE-ELIGIBLE")
            && enabled.contains("NESTED-INLINE")
            && has_guarded_typed_lane(&enabled),
        "wide global-reading lane did not engage:\n{enabled}"
    );

    let no_globals = assert_child_ok(
        &run_fresh(Some("ZIPP_NO_TYPED_GLOBAL_LOAD")),
        "typed-global ablation",
    );
    assert!(
        no_globals.contains("typed-lane=DECLINED(callee-value-escapes)"),
        "typed-global ablation still scheduled the lane:\n{no_globals}"
    );
}

fn run_route_fresh(ablate: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("--exact")
        .arg("wide_leaf_route_change_falls_back_before_raw_global_read")
        .arg("--nocapture")
        .env(ROUTE_CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_WIDE_LEAF")
        .env_remove("ZIPP_NO_TYPED_GLOBAL_LOAD");
    if let Some(flag) = ablate {
        command.env(flag, "1");
    }
    command.output().expect("spawn route-change child")
}

fn assert_route_child_ok(output: &std::process::Output, mode: &str) -> String {
    assert!(
        output.status.success(),
        "{mode} child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains(ROUTE_OUTPUT_MARKER),
        "{mode} child omitted route output marker:\n{stderr}"
    );
    stderr
}

/// A route change is stronger than a live slot write: after an accessor appears
/// on the global object, every read must run its getter. The compiled lane's
/// epoch guard therefore has to miss and replay the real call for the entire
/// second half of this loop. `routeReads` makes even one stale raw-slot read
/// observable in the output.
#[test]
fn wide_leaf_route_change_falls_back_before_raw_global_read() {
    if std::env::var_os(ROUTE_CHILD_ENV).is_some() {
        let source = include_str!("fixtures/wide_leaf_module/route_change.js");
        let outcome = zipp_vm::run(source).expect("route-change fixture compiles");
        assert!(
            outcome.error.is_none(),
            "unexpected route-change error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.output, ["752628315|90000"]);
        eprintln!("{ROUTE_OUTPUT_MARKER}{}", outcome.output[0]);
        return;
    }

    let stderr = assert_route_child_ok(&run_route_fresh(None), "enabled route-change");
    assert!(
        stderr.contains("INLINE-ELIGIBLE")
            && stderr.contains("slot_guard=g")
            && stderr.contains("INT splice")
            && has_guarded_typed_lane(&stderr),
        "guarded wide lane did not engage:\n{stderr}"
    );

    let no_typed_globals = assert_route_child_ok(
        &run_route_fresh(Some("ZIPP_NO_TYPED_GLOBAL_LOAD")),
        "typed-global ablation",
    );
    assert!(
        no_typed_globals.contains("typed-lane=DECLINED(callee-value-escapes)")
            && !has_guarded_typed_lane(&no_typed_globals),
        "typed-global ablation still scheduled the guarded lane:\n{no_typed_globals}"
    );
}
