//! A Python exception leaving the program prints what CPython 3.13 prints on
//! stderr (`Traceback (most recent call last):`, a `File` line and the source
//! line for each frame, the chained exceptions, then `Type: message`) and
//! exits with status 1. Two differences are by design: a file is named by
//! its path inside the project (CPython prints the absolute path), and
//! frames carry no column positions, so no caret line is drawn under the
//! failing expression. Each expected text below is CPython's for the same
//! program with those two differences applied.
#![cfg(feature = "python")]
use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-uncaught-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, files: &[(&str, &str)], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
        for (name, text) in files {
            fs::write(self.0.join(name), text).unwrap();
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zipp"));
        cmd.current_dir(&self.0).args(["py", files[0].0]);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn check(files: &[(&str, &str)], stdout: &str, stderr: &str) {
    for env in [&[][..], &[("ZIPP_PY_JIT", "0")][..]] {
        let f = Fixture::new();
        let (code, out, err) = f.run(files, env);
        assert_eq!(code, Some(1), "{env:?}: {err}");
        assert_eq!(out, stdout, "{env:?}");
        assert_eq!(err, stderr, "{env:?}");
    }
}

#[test]
fn an_uncaught_exception_prints_the_traceback_then_the_exception() {
    check(
        &[("main.py", "def f():\n    raise ValueError(\"msg\")\n\nprint(\"start\")\nf()\n")],
        "start\n",
        "Traceback (most recent call last):\n  File \"main.py\", line 5, in <module>\n    f()\n  File \"main.py\", line 2, in f\n    raise ValueError(\"msg\")\nValueError: msg\n",
    );
    // No message; a class of the main module; a runtime error in a method.
    check(
        &[("main.py", "class Oops(Exception):\n    pass\n\nraise Oops\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 4, in <module>\n    raise Oops\nOops\n",
    );
    check(
        &[("main.py", "class A:\n    def get(self, d):\n        return d[\"k\"]\n\nA().get({})\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 5, in <module>\n    A().get({})\n  File \"main.py\", line 3, in get\n    return d[\"k\"]\nKeyError: 'k'\n",
    );
}

#[test]
fn chained_exceptions_print_each_traceback_in_order() {
    check(
        &[(
            "main.py",
            "def load():\n    try:\n        {}[\"x\"]\n    except KeyError as e:\n        raise RuntimeError(\"load failed\") from e\n\nload()\n",
        )],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 3, in load\n    {}[\"x\"]\nKeyError: 'x'\n\nThe above exception was the direct cause of the following exception:\n\nTraceback (most recent call last):\n  File \"main.py\", line 7, in <module>\n    load()\n  File \"main.py\", line 5, in load\n    raise RuntimeError(\"load failed\") from e\nRuntimeError: load failed\n",
    );
    check(
        &[("main.py", "try:\n    1 / 0\nexcept ZeroDivisionError:\n    [][0]\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 2, in <module>\n    1 / 0\nZeroDivisionError: division by zero\n\nDuring handling of the above exception, another exception occurred:\n\nTraceback (most recent call last):\n  File \"main.py\", line 4, in <module>\n    [][0]\nIndexError: list index out of range\n",
    );
}

#[test]
fn a_reraised_exception_keeps_the_line_it_was_caught_at() {
    check(
        &[(
            "main.py",
            "def f():\n    try:\n        int(\"x\")\n    except ValueError:\n        n = 1\n        raise\n\ntry:\n    f()\nfinally:\n    done = True\n",
        )],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 9, in <module>\n    f()\n  File \"main.py\", line 3, in f\n    int(\"x\")\nValueError: invalid literal for int() with base 10: 'x'\n",
    );
}

#[test]
fn frames_in_other_modules_name_their_file() {
    check(
        &[
            ("main.py", "import helper\n\nhelper.run(3)\n"),
            ("helper.py", "def run(n):\n    return check(n)\n\n\ndef check(n):\n    if n > 2:\n        raise helper_error(n)\n\n\ndef helper_error(n):\n    return OverflowError(\"n = %d\" % n)\n"),
        ],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 3, in <module>\n    helper.run(3)\n  File \"helper.py\", line 2, in run\n    return check(n)\n  File \"helper.py\", line 7, in check\n    raise helper_error(n)\nOverflowError: n = 3\n",
    );
}

#[test]
fn a_long_recursion_prints_repeated_frames_once_counted() {
    check(
        &[("main.py", "def down(n):\n    if n == 0:\n        raise ValueError(\"bottom\")\n    return down(n - 1)\n\ndown(20)\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 6, in <module>\n    down(20)\n  File \"main.py\", line 4, in down\n    return down(n - 1)\n  File \"main.py\", line 4, in down\n    return down(n - 1)\n  File \"main.py\", line 4, in down\n    return down(n - 1)\n  [Previous line repeated 17 more times]\n  File \"main.py\", line 3, in down\n    raise ValueError(\"bottom\")\nValueError: bottom\n",
    );
}

#[test]
fn names_that_are_nearly_right_get_a_suggestion() {
    check(
        &[("main.py", "count = 1\nprint(cuont)\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 2, in <module>\n    print(cuont)\nNameError: name 'cuont' is not defined. Did you mean: 'count'?\n",
    );
    check(
        &[("main.py", "x = math.pi\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 1, in <module>\n    x = math.pi\nNameError: name 'math' is not defined. Did you forget to import 'math'?\n",
    );
    check(
        &[("main.py", "class P:\n    def __init__(self):\n        self.width = 2\n\nprint(P().widht)\n")],
        "",
        "Traceback (most recent call last):\n  File \"main.py\", line 5, in <module>\n    print(P().widht)\nAttributeError: 'P' object has no attribute 'widht'. Did you mean: 'width'?\n",
    );
}
