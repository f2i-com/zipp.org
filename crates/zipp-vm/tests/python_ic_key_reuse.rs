//! A Python attribute site's cache entry must not outlive the dict key it
//! matched: `tests/python_corpus/py_native_ic_key_reuse.py`, whose run-time
//! built keys die and have their heap slots reused by other names' keys at
//! the same dict position. An entry keyed on the key's bits alone then read
//! (or overwrote) the other attribute; with every safepoint collecting
//! (`ZIPP_GC_STRESS`) that reuse happens on nearly every round.
#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

const SOURCE: &str = include_str!("../../../tests/python_corpus/py_native_ic_key_reuse.py");
const WANT: &str = include_str!("../../../tests/python_corpus/py_native_ic_key_reuse.out");

fn run() -> Vec<String> {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            let mut state = compile_source(SOURCE, Frontend::Python { mode: PythonMode::Module }).expect("compiles").into_state();
            state.set_limits(2_000_000_000, None);
            state.run_init().expect("runs");
            state.take_output()
        })
        .expect("spawn")
        .join()
        .expect("program thread")
}

#[test]
fn attribute_cache_entries_do_not_match_a_reused_key_slot() {
    let want: Vec<String> = WANT.replace("\r\n", "\n").lines().map(str::to_owned).collect();
    assert_eq!(run(), want);
}

#[test]
fn attribute_cache_entries_do_not_match_a_reused_key_slot_under_gc_stress() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["attribute_cache_entries_do_not_match_a_reused_key_slot", "--exact", "--nocapture"])
        .env("ZIPP_GC_STRESS", "1")
        .env_remove("ZIPP_NO_NURSERY")
        .output()
        .expect("spawn the stress child");
    assert!(
        out.status.success(),
        "under ZIPP_GC_STRESS:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
