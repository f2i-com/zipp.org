//! Object literals of installed code: an imported module's, and the code
//! `eval` and `new Function` compile from inside one.
//!
//! The one-step literal (`FinalizeObject`: every value staged, then the
//! object made complete) was the root program's only. A program a VM
//! installs beside it can have its literal plans taken back when it does not
//! fit the VM's plan ceilings, and a one-step site cannot be rewritten in
//! place, so the compiler gave installed code the older form: an allocation
//! and then one instruction (compiled: one runtime helper call) a property,
//! which also left the new object's first read to a miss. An application is
//! almost all imported modules, so that was nearly every literal it makes: a
//! four-key literal cost 118 ns in a module and 14 ns in the root script.
//!
//! Installed code is now compiled within what its VM has left under those
//! ceilings (`PlanBudget`) and charged for it at once, so it always fits and
//! takes the one-step form.
//!
//! The fixture's `lib.mjs` makes every form of literal from hot code: static,
//! shorthand, numeric and quoted keys; nested ones; sixteen keys (the widest
//! one-step literal) and eighteen; a repeated key; `__proto__`; computed keys;
//! spreads before and after static keys; accessors; methods that use `super`,
//! generator and async methods; a value that throws part way; a literal whose
//! values build the same literal; `this` and arrows; default parameters; a
//! generator's and an async function's; `eval`, `new Function` and indirect
//! `eval`; literals changed, frozen and described afterwards; a class's field
//! and static initialisers; and literals kept across collections.
//!
//! `expected.txt` is Node 24's output for `entry.mjs`.

#![cfg(not(feature = "wasm-no-fs-loader"))]

const EXPECTED: &str = include_str!("fixtures/module_literals/expected.txt");

fn fixture_entry() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("module_literals")
        .join("entry.mjs")
}

#[test]
fn module_literals_answer_as_node() {
    let outcome = zipp_vm::run_module_file(&fixture_entry(), None).expect("module compiles");
    assert!(outcome.error.is_none(), "unexpected module error: {:?}", outcome.error);
    let got: Vec<String> = outcome.output.iter().flat_map(|l| l.lines().map(str::to_string)).collect();
    let want: Vec<&str> = EXPECTED.lines().collect();
    assert_eq!(got, want);
}

/// The same answers with the JIT compiling at once, with a collection at
/// every safe point, and with no native code.
#[test]
fn module_literals_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("module_literals_answer")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
