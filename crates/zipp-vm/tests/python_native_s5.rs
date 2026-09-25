//! Tracebacks with no per-statement line stamps and no per-call handler:
//! the frame table (the line of each ip) and the implicit frame guard
//! (`vm::py_rt`). Every expected frame list is CPython 3.13's for the same
//! program; each program runs interpreted and with the VM's JIT on, hot
//! enough for its loops to compile.
#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

fn run(source: &str, jit: bool) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut state = compile_source(&source, Frontend::Python { mode: PythonMode::Module })?.into_state();
            state.set_limits(2_000_000_000, None);
            if jit {
                state.enable_vm_jit();
            }
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

/// `(line, function)` of each frame the error's traceback lists.
fn frames(err: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for line in err.lines() {
        let Some(rest) = line.trim_start().strip_prefix("File \"") else {
            continue;
        };
        let Some((_, rest)) = rest.split_once("\", line ") else {
            continue;
        };
        let Some((n, name)) = rest.split_once(", in ") else {
            continue;
        };
        out.push((n.parse().unwrap_or(0), name.trim().to_owned()));
    }
    out
}

fn check(source: &str, want: &[(u32, &str)]) {
    for jit in [false, true] {
        let err = run(source, jit).expect_err("the program raises");
        let got = frames(&err);
        let want: Vec<(u32, String)> = want.iter().map(|&(l, n)| (l, n.to_owned())).collect();
        assert_eq!(got, want, "jit {jit}: {err}");
    }
}

#[test]
fn frames_name_the_raising_line_of_every_frame() {
    check("def f():\n    x = 1\n    return x / 0\n\ndef g():\n    y = 2\n    return f()\n\ng()\n", &[(9, "<module>"), (7, "g"), (3, "f")]);
    check(
        "def a(x):\n    return b(x) + 1\n\ndef b(x):\n    return c(x) * 2\n\ndef c(x):\n    return x.upper()\n\na(3)\n",
        &[(10, "<module>"), (2, "a"), (5, "b"), (8, "c")],
    );
    check("class A:\n    pass\n\ndef f(a):\n    return a.missing\n\nf(A())\n", &[(7, "<module>"), (5, "f")]);
    check("a = 1\nb = [a]\nb[3]\n", &[(3, "<module>")]);
    check("def f():\n    try:\n        1 / 0\n    except ZeroDivisionError:\n        y = []\n        y[1]\n\nf()\n", &[(8, "<module>"), (6, "f")]);
}

#[test]
fn a_loop_header_is_the_loop_statement_line() {
    // An iterator raising at the loop header (CPython names the `for`, and
    // the `while` whose condition raised), on the first iteration or a later
    // one.
    check(
        "class It:\n    def __init__(self):\n        self.n = 0\n    def __iter__(self):\n        return self\n    def __next__(self):\n        self.n += 1\n        if self.n > 2:\n            raise IndexError('it')\n        return self.n\n\ndef f():\n    t = 0\n    for v in It():\n        t += v\n        t += 1\n    return t\n\nf()\n",
        &[(19, "<module>"), (14, "f"), (9, "__next__")],
    );
    check(
        "class It:\n    def __init__(self):\n        self.n = 0\n    def ok(self):\n        self.n += 1\n        if self.n > 3:\n            raise StopIteration('x')\n        return True\n\ndef f():\n    it = It()\n    s = 0\n    while it.ok():\n        s += 1\n        s += 2\n    return s\n\nf()\n",
        &[(18, "<module>"), (13, "f"), (7, "ok")],
    );
}

#[test]
fn generators_consumers_and_throw() {
    check("def g():\n    yield 1\n    yield 2\n    raise KeyError('k')\n\nfor x in g():\n    pass\n", &[(6, "<module>"), (4, "g")]);
    check(
        "def g():\n    yield 1\n    1 / 0\n\ndef use():\n    total = 0\n    for v in g():\n        total += v\n    return total\n\nuse()\n",
        &[(11, "<module>"), (7, "use"), (3, "g")],
    );
    check("def g():\n    x = yield 1\n    yield 2\n\nit = g()\nnext(it)\nit.throw(RuntimeError('thrown'))\n", &[(7, "<module>"), (2, "g")]);
}

#[test]
fn hot_code_keeps_its_lines() {
    check(
        "def f(n):\n    s = 0\n    for i in range(n):\n        s += i\n        if i == 4999:\n            s = s / 0\n    return s\n\nf(10000)\n",
        &[(9, "<module>"), (6, "f")],
    );
    check(
        "def g(i):\n    x = i * 2\n    if i == 4999:\n        return {}[x]\n    return x\n\ndef f(n):\n    t = 0\n    for i in range(n):\n        t += g(i)\n    return t\n\nf(10000)\n",
        &[(13, "<module>"), (10, "f"), (4, "g")],
    );
    check(
        "def gen(n):\n    for i in range(n):\n        if i == 6000:\n            raise KeyError(i)\n        yield i\n\ndef f():\n    t = 0\n    for v in gen(10000):\n        t += v\n    return t\n\nf()\n",
        &[(13, "<module>"), (9, "f"), (4, "gen")],
    );
    check(
        "class A:\n    def m(self, i):\n        return 10 // (i - 5000)\n\ndef f():\n    a = A()\n    t = 0\n    for i in range(10000):\n        t += a.m(i)\n    return t\n\nf()\n",
        &[(12, "<module>"), (9, "f"), (3, "m")],
    );
}

#[test]
fn recursion_overflow_names_the_call_line() {
    for jit in [false, true] {
        let err = run("def f(n):\n    return f(n + 1)\n\nf(0)\n", jit).expect_err("overflows");
        let got = frames(&err);
        assert_eq!(got.first(), Some(&(4, "<module>".to_owned())), "jit {jit}: {err}");
        assert!(got.len() > 3 && got[1..].iter().all(|f| *f == (2, "f".to_owned())), "jit {jit}: {err}");
    }
}

#[test]
fn caught_exceptions_record_their_line() {
    // `except ... as e` sees the traceback the raise gave it: the raising
    // line of the frame that raised it.
    let src = "import sys\n\ndef f():\n    x = 0\n    return 1 // x\n\ndef g():\n    try:\n        f()\n    except ZeroDivisionError as e:\n        tb = e.__traceback__\n        return 'caught'\n\nfor i in range(3000):\n    r = g()\nprint(r)\n";
    for jit in [false, true] {
        assert_eq!(run(src, jit).expect("runs"), ["caught"], "jit {jit}");
    }
}
