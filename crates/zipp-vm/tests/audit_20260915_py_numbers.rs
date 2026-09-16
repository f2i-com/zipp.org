//! Regression coverage for the Python runtime's numbers and stdlib track
//! (the CPython-checked cases live in `tests/python_corpus/py_numbers_*.py`).
//! These are the behaviours a differential corpus cannot pin: a hang that
//! must fail inside an instruction budget, wall-clock properties of the
//! `time` module, and the hosted/standalone split. Python programs always
//! run with the VM JIT disabled, so there is a single tier to cover.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_project_hosted;

/// Runs `main.py` on the CLI's stack size under an instruction budget, so a
/// regression into an endless loop fails instead of hanging the suite.
fn run(source: &str, hosted: bool) -> Result<Vec<String>, String> {
    let modules = vec![("main".to_string(), source.to_string())];
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut compiled = compile_python_project_hosted("main", &modules, hosted)?;
            let state = compiled.state_mut();
            state.set_limits(200_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

#[test]
fn re_sub_callback_that_reruns_its_pattern_terminates() {
    // The compiled RegExp is shared per pattern; a callback that used it
    // again reset its lastIndex and the outer scan restarted forever.
    let src = "import re\n\
        env = {'a': '{b}', 'b': 'B', 'c': 'C'}\n\
        def expand(text):\n    return re.sub(r'\\{(\\w+)\\}', lambda m: expand(env[m.group(1)]), text)\n\
        print(expand('x {a} y {c} z'))\n\
        def g(m):\n    re.search(r'\\d', 'zz')\n    return '#'\n\
        print(re.sub(r'\\d', g, 'a1b2c3'))\n\
        p = re.compile(r'\\d')\n\
        print(p.sub(lambda m: str(len(p.findall('123'))), 'a1b2'), [m.group() for m in p.finditer('4x5') if p.search('9')])\n";
    assert_eq!(
        run(src, false).unwrap(),
        vec!["x B y C z", "a#b#c#", "a3b3 ['4', '5']"]
    );
}

#[test]
fn json_dict_keys_are_data_not_markers() {
    // Marker objects for big ints and non-finite floats once let a dict key
    // named `__big`/`__special` inject raw text into the output.
    let src = "import json\n\
        print(json.dumps({'profile': json.loads('{\"__big\": \"1, \\\\\"admin\\\\\": true\"}')}))\n\
        print(json.dumps({'__special': 'alert(1)'}), json.dumps({'__proto__': 1, '10': 2, '2': 3}), json.dumps({1: 'int', '1': 'str'}))\n\
        print(json.dumps({'loss': 1.0, 'big': 2**70, 'neg0': -0.0}), type(json.loads(json.dumps(3.0))).__name__)\n";
    assert_eq!(
        run(src, false).unwrap(),
        vec![
            r#"{"profile": {"__big": "1, \"admin\": true"}}"#,
            r#"{"__special": "alert(1)"} {"__proto__": 1, "10": 2, "2": 3} {"1": "int", "1": "str"}"#,
            r#"{"loss": 1.0, "big": 1180591620717411303424, "neg0": -0.0} float"#,
        ]
    );
}

#[test]
fn interval_clocks_resolve_below_a_millisecond() {
    // perf_counter/monotonic read the engine's fractional-millisecond clock,
    // not Date.now(): consecutive distinct readings are far closer than 1 ms.
    let src = "import time\n\
        def step(clock):\n    a = clock()\n    b = clock()\n    while b == a:\n        b = clock()\n    return b - a\n\
        best = min(step(time.perf_counter) for _ in range(20))\n\
        mono = min(step(time.monotonic) for _ in range(20))\n\
        ns = time.perf_counter_ns()\n\
        print(0 < best < 0.0005, 0 < mono < 0.0005, type(ns).__name__, time.perf_counter_ns() >= ns)\n";
    assert_eq!(run(src, false).unwrap(), vec!["True True int True"]);
}

#[test]
fn sleep_blocks_standalone_and_never_holds_an_embedding_host() {
    let src = "import time\n\
        t = time.perf_counter()\n\
        time.sleep(0.05)\n\
        time.sleep(0)\n\
        elapsed = time.perf_counter() - t\n\
        for bad in (-1, float('nan'), 'x'):\n    try:\n        time.sleep(bad)\n    except (ValueError, TypeError) as e:\n        print(type(e).__name__, e)\n\
        print('slept', elapsed >= 0.045, elapsed < 30)\n";
    let standalone = run(src, false).unwrap();
    assert_eq!(
        standalone,
        vec![
            "ValueError sleep length must be non-negative",
            "ValueError Invalid value NaN (not a number)",
            "TypeError 'str' object cannot be interpreted as an integer",
            "slept True True",
        ]
    );
    // Hosted (the wasm engine): the call validates its argument but returns at once.
    let hosted = "import time\nt = time.perf_counter()\ntime.sleep(2)\nprint(time.perf_counter() - t < 1)\n";
    assert_eq!(run(hosted, true).unwrap(), vec!["True"]);
}

#[test]
fn int_size_is_bounded_by_one_limit_on_every_path() {
    // `**` and `<<` past the engine's BigInt limit fail fast with
    // OverflowError instead of computing (or looping) first.
    let src = "for thunk in (lambda: 1 << (1 << 40), lambda: 2 ** (2 ** 40), lambda: 7 ** (10 ** 30)):\n    try:\n        thunk()\n        print('no error')\n    except OverflowError as e:\n        print('OverflowError')\n\
        print(-1 >> 10**30, 0 << 10**30, 1 ** (10**30), (-1) ** (10**30 + 1))\n";
    assert_eq!(
        run(src, false).unwrap(),
        vec!["OverflowError", "OverflowError", "OverflowError", "-1 0 1 -1"]
    );
}

#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn ints_past_two_to_the_twenty_bits_work_on_the_helper_paths() {
    // The runtime once capped `**`, `<<` and its own arithmetic at 2^20 bits
    // while the emitter's inline `+ - *` did not.
    let src = "x = (1 << 3000000) - 1\n\
        print(x.bit_length(), (x - True).bit_length(), (x << 1).bit_length(), sum([x, x]).bit_length(), (2 ** 1100000).bit_length())\n";
    assert_eq!(
        run(src, false).unwrap(),
        vec!["3000000 3000000 3000001 3000001 1100001"]
    );
}
