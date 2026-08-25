//! The transactional own-method/global Tier-C lane accepts loader-recorded ES
//! module functions through the shared immutable-code boundary, while ordinary
//! eval and `Function` constructor bodies remain excluded.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_METHOD_GLOBAL_MODULE_CHILD";

fn fixture_entry() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tierc_method_global_module")
        .join("entry.mjs")
}

#[test]
fn loader_module_is_admitted_but_eval_functions_decline() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let outcome = zipp_vm::run_module_file(&fixture_entry(), None).expect("module compiles");
        assert!(
            outcome.error.is_none(),
            "unexpected module error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.output, ["256|256|23|29"]);
        return;
    }

    let out = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args([
            "loader_module_is_admitted_but_eval_functions_decline",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("ZIPP_JIT_THRESHOLD", "2")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_MODULE_JIT")
        .env_remove("ZIPP_NO_TIERC_METHOD_GLOBAL_INLINE")
        .output()
        .expect("spawn module child");
    assert!(
        out.status.success(),
        "module child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("method-global=1")
            && stderr.contains("method LANE")
            && stderr.contains("INLINE method arms=1"),
        "loader-installed module method was not admitted:\n{stderr}"
    );
    let declines = stderr.matches("DECLINE method key=random").count();
    assert!(
        declines >= 2,
        "ordinary eval/new Function methods did not both decline (count={declines}):\n{stderr}"
    );
}
