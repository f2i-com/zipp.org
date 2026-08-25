//! Loader-installed ES-module functions may use the native tiers while ordinary
//! `eval`/`new Function` functions remain excluded. The test re-executes itself
//! in fresh processes because the default-on ablation flag is deliberately
//! process-cached: one child proves native compilation happened, and another
//! proves `ZIPP_NO_MODULE_JIT=1` preserves byte-identical semantics with no
//! module JIT log lines.

#![cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]

const CHILD_ENV: &str = "ZIPP_MODULE_JIT_TEST_CHILD";
const OUTPUT_MARKER: &str = "[module-jit-test] ";
const EXPECTED: &str = "5873856|401|module-boom|18003|18047|18047";

fn fixture_entry() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("module_jit")
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

fn run_fresh(ablate: bool) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("--exact")
        .arg("imported_module_jit_preserves_runtime_semantics")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_MODULE_JIT");
    if ablate {
        command.env("ZIPP_NO_MODULE_JIT", "1");
    }
    command.output().expect("spawn fresh test process")
}

#[test]
fn imported_module_jit_preserves_runtime_semantics() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_run();
        return;
    }

    let enabled = run_fresh(false);
    assert!(
        enabled.status.success(),
        "JIT-enabled child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&enabled.stdout),
        String::from_utf8_lossy(&enabled.stderr)
    );
    let enabled_err = String::from_utf8_lossy(&enabled.stderr);
    assert!(
        enabled_err.contains(OUTPUT_MARKER),
        "missing child output marker"
    );
    assert!(
        enabled_err.contains("[jit]"),
        "module functions never reached a native tier:\n{enabled_err}"
    );

    let disabled = run_fresh(true);
    assert!(
        disabled.status.success(),
        "JIT-ablated child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&disabled.stdout),
        String::from_utf8_lossy(&disabled.stderr)
    );
    let disabled_err = String::from_utf8_lossy(&disabled.stderr);
    assert!(
        disabled_err.contains(OUTPUT_MARKER),
        "missing ablated output marker"
    );
    assert!(
        !disabled_err.contains("[jit]"),
        "ZIPP_NO_MODULE_JIT still compiled module code:\n{disabled_err}"
    );
}
