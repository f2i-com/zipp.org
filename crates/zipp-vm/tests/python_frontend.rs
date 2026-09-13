//! Integration tests to run in an actual ZIPP checkout. NOT run in this kit's
//! delivery environment; Node helper tests do not substitute for these tests.
#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

fn execute(source: &str) -> Result<Vec<String>, String> {
    let mut result = compile_source(
        source,
        Frontend::Python {
            mode: PythonMode::Module,
        },
    )?;
    let state = result.state_mut();
    state.set_limits(20_000_000, None);
    state.run_init()?;
    Ok(state.take_output())
}
#[test]
fn fibonacci_direct_bytecode() {
    let output = execute("def fib(n):\n    a = 0\n    b = 1\n    for i in range(n):\n        a, b = b, a + b\n    return a\nprint(fib(30))\n").unwrap();
    assert_eq!(output, vec!["832040"]);
}
#[test]
fn integer_and_truth_semantics() {
    assert_eq!(execute("print(-5 // 2, -5 % 2, 5 // -2, 5 % -2)\nprint(9007199254740993 + 10)\nprint(bool([]), bool(()), bool(range(0)), bool([0]))\n").unwrap(),
        vec!["-3 1 -3 -1", "9007199254741003", "False False False True"]);
}
#[test]
fn unicode_and_sequences() {
    assert_eq!(execute("a = [1, 2]\nb = a\na += [3]\na[-1] = 4\nprint(a, b, len('A😀B'), 'A😀B'[-2])\nprint([1] == (1,), (1,))\n").unwrap(),
        vec!["[1, 2, 4] [1, 2, 4] 3 😀", "False (1,)"]);
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
    assert_eq!(execute("def fact(n):\n    if n < 2:\n        return 1\n    return n * fact(n - 1)\nprint(fact(10))\n").unwrap(), vec!["3628800"]);
}
#[test]
fn function_local_analysis() {
    let err = execute("x = 4\ndef f():\n    print(x)\n    x = 5\nf()\n").unwrap_err();
    assert!(
        err.contains("UnboundLocalError") || err.contains("unbound"),
        "{err}"
    );
}
#[test]
fn unsupported_is_never_transpiled_or_ignored() {
    for source in [
        "import os",
        "x = 1.5",
        "x = 1 / 2",
        "x = 2 ** 3",
        "x = {'a': 1}",
        "def f(x=1):\n    pass\n",
        "class C:\n    pass\n",
        "x = [n for n in range(3)]",
        "def f():\n    def g():\n        pass\n",
        "print(end='')",
        "a = [1]\na *= 2",
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
        "print(open)",
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
    assert!(execute(&"#".repeat(65537)).is_err());
    assert!(execute(&format!("x = {}", "1 + ".repeat(300) + "1")).is_err());
}
