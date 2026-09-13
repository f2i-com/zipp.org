//! The differential corpus: every `tests/python_corpus/*.py` must produce
//! exactly the stdout CPython produced for it (recorded next to it as
//! `.out` by `tools/python_corpus.py --write-expected`), with the same
//! success/failure status.
#![cfg(feature = "python")]
use std::fs;
use std::path::PathBuf;
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("python_corpus")
}

fn run(source: &str) -> (Vec<String>, bool) {
    let mut compiled = match compile_source(
        source,
        Frontend::Python {
            mode: PythonMode::Module,
        },
    ) {
        Ok(c) => c,
        Err(e) => return (vec![format!("<compile error> {e}")], false),
    };
    let state = compiled.state_mut();
    state.set_limits(2_000_000_000, None);
    let ok = state.run_init().is_ok();
    // A single print of multi-line text is one console record; compare by
    // physical lines, as the CLI prints them.
    let lines = state
        .take_output()
        .join("\n")
        .split('\n')
        .map(String::from)
        .collect();
    (lines, ok)
}

/// Dunder dispatch and JS-to-Python calls re-enter the interpreter on the
/// native stack; debug frames are large, so run on the CLI's stack size.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

#[test]
fn corpus_matches_cpython() {
    big_stack(corpus_body);
}

fn corpus_body() {
    let dir = corpus_dir();
    let mut programs: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("tests/python_corpus exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "py"))
        .collect();
    programs.sort();
    assert!(
        !programs.is_empty(),
        "no corpus programs in {}",
        dir.display()
    );
    let mut failures = Vec::new();
    for program in &programs {
        let source = fs::read_to_string(program).unwrap();
        let expected_path = program.with_extension("out");
        let Ok(expected) = fs::read_to_string(&expected_path) else {
            failures.push(format!(
                "{}: no recorded output; run tools/python_corpus.py --write-expected",
                program.display()
            ));
            continue;
        };
        let expected_ok = fs::read_to_string(program.with_extension("status"))
            .map(|s| s.trim() == "0")
            .unwrap_or(true);
        let (got, ok) = run(&source);
        let expected_lines: Vec<&str> = expected.lines().collect();
        let mut mismatch = None;
        for i in 0..expected_lines.len().max(got.len()) {
            let e = expected_lines.get(i).copied().unwrap_or("<missing>");
            let g = got.get(i).map(String::as_str).unwrap_or("<missing>");
            if e != g {
                mismatch = Some(format!(
                    "line {}:\n    cpython: {e}\n    zipp:    {g}",
                    i + 1
                ));
                break;
            }
        }
        if let Some(m) = mismatch {
            failures.push(format!(
                "{}: {m}",
                program.file_name().unwrap().to_string_lossy()
            ));
        } else if ok != expected_ok {
            failures.push(format!(
                "{}: exit status differs (cpython ok={expected_ok}, zipp ok={ok})",
                program.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n"));
}
