//! Integration tests for the Python frontend: single-file programs through
//! `compile_source`, checked against outputs CPython produces.
#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

/// Runs on the CLI's stack size: dunder dispatch re-enters the interpreter
/// natively, and debug frames are large.
fn execute(source: &str) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut result = compile_source(
                &source,
                Frontend::Python {
                    mode: PythonMode::Module,
                },
            )?;
            let state = result.state_mut();
            state.set_limits(200_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

#[test]
fn fibonacci_direct_bytecode() {
    let output = execute("def fib(n):\n    a = 0\n    b = 1\n    for i in range(n):\n        a, b = b, a + b\n    return a\nprint(fib(30))\n").unwrap();
    assert_eq!(output, vec!["832040"]);
}

#[test]
fn integer_and_truth_semantics() {
    assert_eq!(
        execute("print(-5 // 2, -5 % 2, 5 // -2, 5 % -2)\nprint(9007199254740993 + 10)\nprint(bool([]), bool(()), bool(range(0)), bool([0]))\n").unwrap(),
        vec!["-3 1 -3 -1", "9007199254741003", "False False False True"]
    );
}

#[test]
fn floats_and_true_division() {
    assert_eq!(
        execute("print(7 / 2, 2 ** 10, 2 ** -1, 1e16, 0.1 + 0.2, round(2.5), round(3.5), 10 ** 20 / 3, int(3.9), float('1e3'))\n").unwrap(),
        vec!["3.5 1024 0.5 1e+16 0.30000000000000004 2 4 3.333333333333333e+19 3 1000.0"]
    );
}

#[test]
fn unicode_and_sequences() {
    assert_eq!(
        execute("a = [1, 2]\nb = a\na += [3]\na[-1] = 4\nprint(a, b, len('A😀B'), 'A😀B'[-2])\nprint([1] == (1,), (1,))\n").unwrap(),
        vec!["[1, 2, 4] [1, 2, 4] 3 😀", "False (1,)"]
    );
}

#[test]
fn call_and_comparison_evaluation_order() {
    let s = "def mark(n):\n    print(n)\n    return n\nprint(mark(1) < mark(2) < mark(3))\nprint(mark(3) < mark(2) < mark(1))\nprint(0 and mark(9), 7 or mark(8))\n";
    assert_eq!(
        execute(s).unwrap(),
        vec!["1", "2", "3", "True", "3", "2", "False", "0 7"]
    );
}

#[test]
fn loop_else_and_continue() {
    let s = "for i in range(3):\n    if i == 1:\n        continue\n    print(i)\nelse:\n    print('complete')\nfor i in range(3):\n    break\nelse:\n    print('wrong')\n";
    assert_eq!(execute(s).unwrap(), vec!["0", "2", "complete"]);
}

#[test]
fn recursive_functions() {
    assert_eq!(
        execute("def fact(n):\n    if n < 2:\n        return 1\n    return n * fact(n - 1)\nprint(fact(10))\n").unwrap(),
        vec!["3628800"]
    );
    // Deep recursion lives on the VM's explicit frame stack and is bounded
    // by a catchable RecursionError rather than the native stack.
    let out = execute("def depth(n):\n    return 0 if n == 0 else 1 + depth(n - 1)\nprint(depth(2000))\ntry:\n    depth(10 ** 7)\nexcept RecursionError as e:\n    print('RecursionError')\n").unwrap();
    assert_eq!(out, vec!["2000", "RecursionError"]);
}

#[test]
fn function_local_analysis() {
    let err = execute("x = 4\ndef f():\n    print(x)\n    x = 5\nf()\n").unwrap_err();
    assert!(err.contains("UnboundLocalError"), "{err}");
}

#[test]
fn classes_exceptions_and_generators() {
    let s = "class A:\n    def __init__(self, v):\n        self.v = v\n    def __repr__(self):\n        return f'A({self.v})'\n    def __add__(self, o):\n        return A(self.v + o.v)\nclass B(A):\n    def double(self):\n        return A(self.v * 2)\nprint(A(1) + B(2), B(3).double(), isinstance(B(1), A))\ndef gen():\n    yield 1\n    yield 2\nprint(list(gen()), [x * x for x in gen()], {x: x for x in gen()})\ntry:\n    raise ValueError('bad')\nexcept ValueError as e:\n    print('caught', e)\nfinally:\n    print('finally')\n";
    assert_eq!(
        execute(s).unwrap(),
        vec![
            "A(3) A(6) True",
            "[1, 2] [1, 4] {1: 1, 2: 2}",
            "caught bad",
            "finally"
        ]
    );
}

#[test]
fn unsupported_syntax_is_a_compile_error() {
    for source in [
        "async def f():\n    pass\n",
        "type Point = tuple[int, int]\n",
        "x = 1j\n",
        "def f(:\n",
        "x = = 1\n",
    ] {
        assert!(
            compile_source(
                source,
                Frontend::Python {
                    mode: PythonMode::Module
                }
            )
            .is_err(),
            "{source}"
        );
    }
}

#[test]
fn python_names_do_not_access_host_globals() {
    for source in [
        "print(console)",
        "print(__zipp_py)",
        "print(eval)",
        "print(JSON)",
    ] {
        assert!(execute(source).is_err(), "{source}");
    }
}

#[test]
fn per_engine_global_isolation() {
    assert!(execute("private_name = 4").is_ok());
    assert!(execute("print(private_name)").is_err());
}

#[test]
fn source_complexity_is_bounded() {
    assert!(execute(&format!("x = {}", "(".repeat(300) + "1" + &")".repeat(300))).is_err());
    assert!(execute(&format!("x = {}", "-".repeat(300) + "1")).is_err());
    // A flat operator chain is not nesting.
    assert_eq!(
        execute(&format!("print({})", "1 + ".repeat(300) + "1")).unwrap(),
        ["301"]
    );
    assert!(execute(&"x = 1\n".repeat(20_000)).is_ok());
}

#[test]
fn uncaught_exceptions_report_type_message_and_location() {
    let err = execute("def f():\n    raise KeyError('k')\n\nf()\n").unwrap_err();
    assert!(err.starts_with("KeyError: 'k'"), "{err}");
    assert!(err.contains("main.py:2"), "{err}");
    let err = execute("import sys\nsys.exit(3)\n").unwrap_err();
    assert!(err.contains("SystemExit"), "{err}");
    assert!(execute("import sys\nsys.exit(0)\n").is_ok());
}

#[test]
fn maintained_unicode_tables_preserve_python_identifier_rules() {
    // Greek, CJK, supplementary-plane and combining continuation characters.
    assert_eq!(
        execute("π = 2\n变量 = 3\n𐐀 = 4\ná = 5\nprint(π + 变量 + 𐐀 + á)\n").unwrap(),
        ["14"]
    );
    // Todhri was assigned after the old UNIC tables; the fork uses Unicode 17.
    assert_eq!(execute("𐗀 = 9\nprint(𐗀)\n").unwrap(), ["9"]);
    // Preserve the upstream single-emoji extension, not the broader Emoji set.
    assert_eq!(execute("🙂 = 7\nprint(🙂)\n").unwrap(), ["7"]);
    for source in ["© = 1\n", "́a = 1\n", "1name = 1\n"] {
        assert!(
            execute(source).is_err(),
            "invalid identifier accepted: {source:?}"
        );
    }
}
