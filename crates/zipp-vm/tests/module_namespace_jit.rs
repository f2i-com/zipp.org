//! The x86-64 JIT may read a module namespace's exact global slot directly,
//! but it must re-read that slot on every access so exported `let` bindings
//! stay live. The fixture reads through a barrel namespace whose re-export map
//! points at another module's slots. Run both sides of the process-cached
//! ablation switch in fresh children and require byte-identical output.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_MODULE_NAMESPACE_JIT_TEST_CHILD";
const OUTPUT_MARKER: &str = "[module-namespace-jit-test] ";
const EXPECTED: &str = "1620000|8|undefined";

fn fixture_entry() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("module_namespace_jit")
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
        .arg("module_namespace_jit_preserves_live_bindings")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_MODULE_JIT")
        .env_remove("ZIPP_NO_JIT_MODULE_NS_GET");
    if ablate {
        command.env("ZIPP_NO_JIT_MODULE_NS_GET", "1");
    }
    command.output().expect("spawn fresh test process")
}

#[test]
fn module_namespace_jit_preserves_live_bindings() {
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
        "module entry never reached a native tier:\n{enabled_err}"
    );
    assert!(
        !enabled_err.contains("EVICTED"),
        "live namespace reads still evicted the hot region:\n{enabled_err}"
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
        disabled_err.contains("EVICTED"),
        "ablation did not restore interpreter replay at the namespace get:\n{disabled_err}"
    );
}
