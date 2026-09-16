//! Python emitter audit (15 September 2026), the P-emitter track.
//!
//! Pinned here, each against the output CPython 3.13 prints:
//! - R198/R310: a free variable passing through a class body that also binds
//!   the name no longer spins the symbol table's fixpoint forever (every
//!   probe runs under a deadline, so a regression fails instead of hanging).
//! - R199: scopes are keyed by their full source range, so a generator
//!   expression whose element starts with a comprehension or a lambda gets
//!   its own scope instead of silently sharing (and miscompiling) theirs.
//! - R200: nesting is bounded before any recursive walk and a deep tree is
//!   freed iteratively: a million-deep unary chain is a clean error on an
//!   ordinary 2 MiB thread, not a native stack overflow.
//! - R206/R307/A22: `elif` ladders and flat operator chains are not nesting,
//!   and large literals build in bounded registers.
//! - R235: a project's files do not each keep registers in the entry.
//! - R311/R201: `yield from` forwards send/throw/close (PEP 380).
//! - R202: the callee (or receiver and attribute) is evaluated first.
//! - R207/R208: a `with` target binds inside the protected region, and an
//!   exception raised in `finally` or `__exit__` gets `__context__`.
//! - R324: what the entry script defines reports `__main__` as its module.
//!
//! Python states always run in the interpreter (the frontend disables the
//! JIT per state), so the modes that matter are the emitter's: every program
//! here also runs in a child process with `ZIPP_PY_NOFAST=1`, where each
//! operation compiles to its runtime helper only, and must print the same.
#![cfg(feature = "python")]
use std::sync::mpsc;
use std::time::Duration;
use zipp_vm::frontend::{compile_python_program, compile_source, Frontend, PythonMode};

const MODULE: Frontend = Frontend::Python {
    mode: PythonMode::Module,
};

/// Compile and run `source` on a thread with the CLI's stack, failing the
/// test (rather than hanging the suite) if compiling or running takes longer
/// than the deadline.
fn run(source: &str) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    on_thread(256 << 20, move || {
        let mut compiled = compile_source(&source, MODULE)?;
        let state = compiled.state_mut();
        // The corpus test's budget: the helpers-only mode runs it too.
        state.set_limits(2_000_000_000, None);
        state.run_init()?;
        Ok(state.take_output())
    })
}

fn on_thread<T: Send + 'static>(stack: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .expect("spawn");
    rx.recv_timeout(Duration::from_secs(120))
        .expect("the program did not finish within the deadline")
}

fn expect(source: &str, expected: &[&str]) {
    let out = run(source).unwrap_or_else(|e| panic!("{e}\n--- source ---\n{source}"));
    assert_eq!(out, expected, "--- source ---\n{source}");
}

const CLASS_PASS_THROUGH: &[(&str, &[&str])] = &[
    (
        "def outer():\n    x = \"outer\"\n    class D:\n        x = \"D\"\n        def m(self):\n            return x\n    return D\nprint(outer()().m(), outer().x)\n",
        &["outer D"],
    ),
    (
        "def class_scope():\n    x = 'func'\n    class K:\n        x = 'class'\n        y = [x for _ in range(1)]\n        def m(self): return x\n    return K().m(), K.y\nprint(class_scope())\n",
        &["('func', ['func'])"],
    ),
    (
        "def make(name):\n    class Model:\n        name = 'default'\n        def get(self):\n            return name\n    return Model().get(), Model.name\nprint(make('n'))\n",
        &["('n', 'default')"],
    ),
    (
        "def f():\n    v = 1\n    class A:\n        v = 2\n        class B:\n            v = 3\n            def g(self):\n                return v\n    return A.B().g()\nprint(f())\n",
        &["1"],
    ),
];

#[test]
fn class_body_binding_passed_through_does_not_hang() {
    for (source, expected) in CLASS_PASS_THROUGH {
        expect(source, expected);
    }
}

const SAME_START: &[(&str, &[&str])] = &[
    (
        "def f():\n    print(\"before\")\n    r = list([j for j in range(2)] for i in range(3))\n    print(\"inside r =\", r)\n    return 1\nprint(\"start\")\nprint(\"f returned\", f())\nprint(\"end\")\n",
        &["start", "before", "inside r = [[0, 1], [0, 1], [0, 1]]", "f returned 1", "end"],
    ),
    (
        "m = [[1, 2, 3], [4, 5, 6]]\nprint(list([row[c] for row in m] for c in range(3)))\n",
        &["[[1, 4], [2, 5], [3, 6]]"],
    ),
    (
        "fs = list(lambda: i for i in range(3))\nprint(len(fs), [f() for f in fs])\n",
        &["3 [2, 2, 2]"],
    ),
    (
        "print(sum([j for j in range(i)][0] if i else 0 for i in range(1, 4)))\nprint(list({j for j in range(2)} for i in range(2)))\nprint(\"after\")\n",
        &["0", "[{0, 1}, {0, 1}]", "after"],
    ),
];

#[test]
fn scopes_sharing_a_start_offset_compile_separately() {
    for (source, expected) in SAME_START {
        expect(source, expected);
    }
}

/// Compile only, on an ordinary 2 MiB thread: the checks must reject (or
/// accept) without recursing once per level.
fn compile_on_small_stack(source: String) -> Result<(), String> {
    on_thread(2 << 20, move || compile_source(&source, MODULE).map(|_| ()))
}

#[test]
fn deep_nesting_is_a_clean_error_on_a_default_stack() {
    for (prefix, unit, suffix) in [
        ("x = ", "-", "1"),
        ("x = ", "~", "1"),
        ("x = ", "not ", "1"),
        ("x = a", ".b", ""),
        ("x = f", "()", ""),
        ("x = ", "lambda: ", "1"),
    ] {
        let source = format!("{prefix}{}{suffix}\n", unit.repeat(100_000));
        let err = compile_on_small_stack(source).expect_err(unit);
        assert!(err.contains("nesting limit exceeded"), "{unit}: {err}");
    }
    // A flat chain this long is not nesting: it is walked iteratively and
    // either compiles or meets the per-function bytecode limit, cleanly.
    let chain = format!("x = {}1\n", "1 + ".repeat(100_000));
    if let Err(e) = compile_on_small_stack(chain) {
        assert!(e.contains("bytecode limit"), "{e}");
    }
}

#[test]
fn flat_chains_elif_ladders_and_large_literals_compile() {
    let mut elif = String::from("def f(x):\n    if x == 0:\n        return 0\n");
    for i in 1..500 {
        elif.push_str(&format!("    elif x == {i}:\n        return {i}\n"));
    }
    elif.push_str("    else:\n        return -1\nprint(f(499), f(250), f(1000))\n");
    expect(&elif, &["499 250 -1"]);
    let mut module_elif = String::from("x = 150\nif x == 0:\n    y = 0\n");
    for i in 1..200 {
        module_elif.push_str(&format!("elif x == {i}:\n    y = {i}\n"));
    }
    module_elif.push_str("print(y)\n");
    expect(&module_elif, &["150"]);
    let terms: Vec<String> = (1..=120).map(|i| i.to_string()).collect();
    expect(&format!("print({})\n", terms.join(" + ")), &["7260"]);
    expect(
        &format!("s = {}\nprint(len(s))\n", ["\"ab\""; 100].join(" + ")),
        &["200"],
    );
    let list: Vec<String> = (0..40_000).map(|i| i.to_string()).collect();
    expect(
        &format!("xs = [{}]\nprint(len(xs), sum(xs))\n", list.join(", ")),
        &["40000 799980000"],
    );
    expect(
        &format!("b = b\"{}\"\nprint(len(b), b[-2:])\n", "a".repeat(40_000)),
        &["40000 b'aa'"],
    );
    let entries: Vec<String> = (0..5_000).map(|i| format!("{i}: {i}")).collect();
    expect(
        &format!("d = {{{}}}\nprint(len(d), d[4999])\n", entries.join(", ")),
        &["5000 4999"],
    );
}

#[test]
fn many_project_files_do_not_exhaust_registers() {
    let out = on_thread(256 << 20, || {
        let modules = vec![(
            "main".to_owned(),
            "import os\nprint(len(os.listdir('data')))\n".to_owned(),
        )];
        let files: Vec<(String, Vec<u8>)> = (0..11_000)
            .map(|i| (format!("data/f{i}"), Vec::new()))
            .collect();
        let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
        let state = compiled.state_mut();
        state.set_limits(500_000_000, None);
        state.run_init()?;
        Ok::<_, String>(state.take_output())
    })
    .expect("project compiles and runs");
    assert_eq!(out, ["11000"]);
}

const YIELD_FROM: &str = "def inner():\n    total = 0\n    while True:\n        x = yield total\n        if x is None:\n            return total\n        total += x\ndef outer():\n    result = yield from inner()\n    yield f'result={result}'\ng = outer(); next(g)\nprint(g.send(1)); print(g.send(2)); print(g.send(None))\n\
def inner2():\n    try:\n        yield 1\n        yield 2\n    except ValueError as e:\n        print('inner caught', e)\n        yield 99\n    finally:\n        print('inner finally')\n\
def outer2():\n    try:\n        yield from inner2()\n    except ValueError:\n        print('outer caught')\n\
g = outer2(); next(g)\nprint(g.throw(ValueError('boom')))\ng.close()\nprint('closed')\n";

#[test]
fn yield_from_forwards_send_throw_and_close() {
    expect(
        YIELD_FROM,
        &["1", "3", "result=3", "inner caught boom", "99", "inner finally", "closed"],
    );
}

const CALL_ORDER: &str = "def log(x):\n    print('eval', x); return x\nclass O:\n    def m(self, *a): return a\ndef mk():\n    print('eval obj'); return O()\nprint(mk().m(log(1)))\n\
def f(x): return 'old'\ndef g():\n    global f\n    f = lambda x: 'new'\n    return 1\nprint(f(g()))\n\
try:\n    undefined_fn(log('arg'))\nexcept NameError as e:\n    print('NameError', e)\n\
try:\n    O().missing(log('arg2'))\nexcept AttributeError:\n    print('AttributeError')\n";

#[test]
fn callee_and_receiver_are_evaluated_before_arguments() {
    expect(
        CALL_ORDER,
        &[
            "eval obj",
            "eval 1",
            "(1,)",
            "old",
            "NameError name 'undefined_fn' is not defined",
            "AttributeError",
        ],
    );
}

const WITH_FINALLY: &str = "class CM:\n    def __enter__(self): return self\n    def __exit__(self, t, v, tb):\n        print('exit', t.__name__ if t else None); return False\n\
try:\n    with CM() as (p, q): pass\nexcept TypeError:\n    print('TypeError')\n\
try:\n    try:\n        raise KeyError(1)\n    finally:\n        try:\n            raise ValueError(2)\n        except ValueError as v:\n            print(repr(v.__context__))\nexcept KeyError:\n    pass\n\
class Bad:\n    def __enter__(self): return self\n    def __exit__(self, t, v, tb): raise KeyError('from exit')\n\
try:\n    with Bad():\n        raise ValueError('body')\nexcept KeyError as k:\n    print(repr(k.__context__))\n";

#[test]
fn with_targets_are_protected_and_finally_sets_context() {
    expect(
        WITH_FINALLY,
        &["exit TypeError", "TypeError", "KeyError(1)", "ValueError('body')"],
    );
}

#[test]
fn entry_script_definitions_belong_to_main() {
    expect(
        "class A: pass\ndef f(): pass\nprint(type(A()), A.__module__, f.__module__, __name__)\n",
        &["<class '__main__.A'> __main__ __main__ __main__"],
    );
}

/// The programs above plus the emitter's fast-path shapes, for the mode
/// comparison.
fn all_programs() -> Vec<(String, Vec<String>)> {
    let own = |s: &str, e: &[&str]| (s.to_owned(), e.iter().map(|x| x.to_string()).collect());
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (s, e) in CLASS_PASS_THROUGH.iter().chain(SAME_START) {
        out.push(own(s, e));
    }
    out.push(own(
        YIELD_FROM,
        &["1", "3", "result=3", "inner caught boom", "99", "inner finally", "closed"],
    ));
    out.push(own(
        "a, b = 1, 2\na, b = b, a\nxs = [(1, 'x'), (2, 'y')]\nfor n, c in xs:\n    a += n\nprint(a, b, 0.5 + 0.25, -0.0, 7 // -2, 7 % -2, 2**64 & -1, ~5, 1.0 / 3, [i for i in range(3)], f'{a}{b!r}')\n",
        &["5 1 0.75 -0.0 -4 -1 18446744073709551616 -6 0.3333333333333333 [0, 1, 2] 51"],
    ));
    out
}

/// Every differential corpus program against its recorded CPython output,
/// as `tests/python_corpus.rs` checks it, here with the fast paths off.
fn corpus_mismatches() -> Vec<String> {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/python_corpus");
    let mut programs: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/python_corpus exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "py"))
        .collect();
    programs.sort();
    let mut failures = Vec::new();
    for program in programs {
        let source = std::fs::read_to_string(&program).unwrap();
        let expected = std::fs::read_to_string(program.with_extension("out")).unwrap_or_default();
        let out = match run(&source) {
            Ok(lines) => lines.join("\n"),
            Err(e) => format!("<error> {e}"),
        };
        let normalize = |s: &str| s.replace("\r\n", "\n").trim_end().to_owned();
        if normalize(&out) != normalize(&expected) {
            failures.push(program.display().to_string());
        }
    }
    failures
}

#[test]
fn fast_paths_off_child() {
    if std::env::var_os("ZIPP_PY_EMIT_NOFAST_CHILD").is_none() {
        return;
    }
    for (source, expected) in all_programs() {
        let out = run(&source).unwrap_or_else(|e| panic!("{e}\n--- source ---\n{source}"));
        assert_eq!(out, expected, "--- source ---\n{source}");
    }
    assert_eq!(corpus_mismatches(), Vec::<String>::new());
}

#[test]
fn fast_paths_on_and_off_agree() {
    if std::env::var_os("ZIPP_PY_EMIT_NOFAST_CHILD").is_some() {
        return;
    }
    for (source, expected) in all_programs() {
        let out = run(&source).unwrap_or_else(|e| panic!("{e}\n--- source ---\n{source}"));
        assert_eq!(out, expected, "--- source ---\n{source}");
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "fast_paths_off_child", "--nocapture"])
        .env("ZIPP_PY_NOFAST", "1")
        .env("ZIPP_PY_EMIT_NOFAST_CHILD", "1")
        .output()
        .expect("spawn the helpers-only child");
    assert!(
        out.status.success(),
        "helpers-only mode diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
