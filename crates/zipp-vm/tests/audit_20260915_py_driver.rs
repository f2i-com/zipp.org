//! The Python compile driver, startup and console (15 September 2026 review,
//! track P-driver).
//!
//! - The runtime seed (~420 KB of runtime JavaScript) is memoized per
//!   process and cloned per program: later compiles do no JavaScript parsing
//!   or compiling at all, and programs built from the one seed share nothing
//!   at run time.
//! - Each module is lexed once: the compiler limits are counted on the
//!   parser's own token stream, with the same messages as the old separate
//!   pass.
//! - `\N{...}` escapes resolve from a bundled table of common names (the
//!   832 KB unicode_names2 tables are gone), case-insensitively as in CPython,
//!   and an unknown name is a SyntaxError that says why.
//! - Virtual-filesystem files reach the program through a native base64
//!   decode instead of an interpreted per-byte loop, and written files reach
//!   the host through a native encode, byte-exact for every value and
//!   padding length.
//! - A console sink receives lines as they are produced, in production order
//!   across both streams, in every execution tier, and output accounting is
//!   unchanged.

use std::cell::RefCell;
use std::rc::Rc;
use zipp_vm::embed::{compile_script, ConsoleStream};

/// Runs on the CLI's stack size: debug frames are large and the Python
/// runtime re-enters the interpreter natively.
#[cfg(feature = "python")]
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("test thread")
}

#[cfg(feature = "python")]
fn run_python_program(
    entry: &str,
    modules: &[(&str, &str)],
    files: &[(&str, &[u8])],
) -> Result<Vec<String>, String> {
    let entry = entry.to_owned();
    let modules: Vec<(String, String)> = modules
        .iter()
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    let files: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_vec()))
        .collect();
    big_stack(move || {
        let mut compiled =
            zipp_vm::frontend::compile_python_program(&entry, &modules, &files, &[], false)?;
        let state = compiled.state_mut();
        state.set_limits(50_000_000, None);
        state.run_init()?;
        Ok(state.take_output())
    })
}

#[cfg(feature = "python")]
fn compile_error(source: &str) -> String {
    let source = source.to_owned();
    big_stack(move || {
        match zipp_vm::frontend::compile_python_program(
            "main",
            &[("main".to_owned(), source)],
            &[],
            &[],
            false,
        ) {
            Ok(_) => panic!("the program compiled"),
            Err(error) => error,
        }
    })
}

/// `(parse + compile)` samples the profiler has attributed so far.
#[cfg(feature = "python")]
fn js_front_samples() -> u64 {
    zipp_vm::prof_stats()
        .0
        .iter()
        .filter(|(name, _, _)| *name == "parse" || *name == "compile")
        .map(|(_, samples, _)| *samples)
        .sum()
}

/// Child half of `runtime_seed_is_memoized_per_process`: under `ZIPP_PROF`,
/// the first Python compile parses and compiles the runtime JavaScript
/// (samples tagged `parse`/`compile`) and memoizes it, and later compiles
/// enter neither phase: they clone the seed and compile only their own
/// modules.
#[cfg(feature = "python")]
#[test]
fn runtime_seed_memo_child() {
    if std::env::var_os("ZIPP_PY_DRIVER_SEED_CHILD").is_none() {
        return;
    }
    let compile = |source: &str| {
        let started = std::time::Instant::now();
        let modules = [("main".to_owned(), source.to_owned())];
        let compiled =
            zipp_vm::frontend::compile_python_program("main", &modules, &[], &[], false)
                .expect("compiles");
        drop(compiled);
        started.elapsed()
    };
    let first = compile("print('first')\n");
    let cold = js_front_samples();
    let later: Vec<_> = [
        "print('second')\n",
        "import json\nprint(json.dumps([3]))\n",
        "print('fourth')\n",
        "print('fifth')\n",
    ]
    .into_iter()
    .map(compile)
    .collect();
    let warm = js_front_samples() - cold;
    let ms = |d: &std::time::Duration| format!("{:.2}", d.as_secs_f64() * 1e3);
    eprintln!(
        "python compile: first {} ms, later {} ms",
        ms(&first),
        later.iter().map(ms).collect::<Vec<_>>().join(" / ")
    );
    assert!(cold > 0, "the first compile should parse the runtime seed");
    // Zero in principle; the sampler thread can land a sample it read during
    // the first compile after `cold` was taken.
    assert!(
        warm * 20 < cold,
        "later compiles must reuse the memoized runtime seed: {warm} samples after it vs {cold} building it"
    );
}

#[cfg(feature = "python")]
#[test]
fn runtime_seed_is_memoized_per_process() {
    if std::env::var_os("ZIPP_PY_DRIVER_SEED_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "runtime_seed_memo_child", "--nocapture"])
        .env("ZIPP_PY_DRIVER_SEED_CHILD", "1")
        .env("ZIPP_PROF", "1")
        .output()
        .expect("spawn child");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "child failed:\n--- stdout ---\n{}\n--- stderr ---\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    // Surface the timings in `--nocapture` runs.
    for line in stderr.lines().filter(|l| l.starts_with("python compile:")) {
        eprintln!("{line}");
    }
}

/// Programs built from the one memoized seed are independent: each runs its
/// own entry, sees only its own modules and files, and a runtime-level
/// mutation in one is invisible to the next.
#[cfg(feature = "python")]
#[test]
fn programs_from_the_memoized_seed_share_nothing() {
    let first = run_python_program(
        "main",
        &[
            ("main", "import os, sys, helper\nsys.path.append('first')\nprint(helper.WHO, os.path.exists('data.txt'), open('data.txt').read())\n"),
            ("helper", "WHO = 'helper'\n"),
        ],
        &[("data.txt", b"one")],
    )
    .expect("first program");
    assert_eq!(first, ["helper True one"]);
    let second = run_python_program(
        "app",
        &[(
            "app",
            "import importlib, os, sys\nprint('first' in sys.path, os.path.exists('data.txt'))\ntry:\n    importlib.import_module('helper')\nexcept ImportError as e:\n    print(type(e).__name__)\n",
        )],
        &[],
    )
    .expect("second program");
    assert_eq!(second, ["False False", "ModuleNotFoundError"]);
}

/// Every byte value, and every base64 padding length, arrives intact.
#[cfg(feature = "python")]
#[test]
fn vfs_files_round_trip_every_byte_through_the_native_decode() {
    let all: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
    let files: Vec<(String, Vec<u8>)> = std::iter::once(("all.bin".to_owned(), all.clone()))
        .chain((0..5).map(|n| (format!("len{n}.bin"), all[..n].to_vec())))
        .collect();
    let file_refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let out = run_python_program(
        "main",
        &[(
            "main",
            "data = open('all.bin', 'rb').read()\n\
             print(len(data), data == bytes(range(256)) + bytes(range(255, -1, -1)))\n\
             for n in range(5):\n    print(n, list(open(f'len{n}.bin', 'rb').read()))\n\
             print(open('all.bin', 'rb').read()[250:262].hex())\n",
        )],
        &file_refs,
    )
    .expect("runs");
    assert_eq!(
        out,
        [
            "512 True",
            "0 []",
            "1 [0]",
            "2 [0, 1]",
            "3 [0, 1, 2]",
            "4 [0, 1, 2, 3]",
            "fafbfcfdfefffffefdfcfbfa",
        ]
    );
}

/// Standard padded base64, to check the host's view of written files.
#[cfg(feature = "python")]
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                TABLE[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Files the program writes reach the host byte-exact through the native
/// encode, for every byte value and padding length.
#[cfg(feature = "python")]
#[test]
fn vfs_changes_report_written_bytes_through_the_native_encode() {
    let listing = big_stack(|| {
        let modules = [(
            "main".to_owned(),
            "data = bytes(range(256)) + bytes(range(255, -1, -1))\n\
             for n in range(5):\n    with open(f'out{n}.bin', 'wb') as f:\n        f.write(data[:n])\n\
             with open('all.bin', 'wb') as f:\n    f.write(data)\n"
                .to_owned(),
        )];
        let mut compiled =
            zipp_vm::frontend::compile_python_program("main", &modules, &[], &[], false)
                .expect("compiles");
        let state = compiled.state_mut();
        state.run_init().expect("runs");
        match state.call_global("__zipp_py_vfs_changed", &[]) {
            Ok(zipp_vm::embed::JsValue::String(listing)) => listing,
            other => panic!("{other:?}"),
        }
    });
    let all: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
    let written = (0..5)
        .map(|n| (format!("out{n}.bin"), all[..n].to_vec()))
        .chain([("all.bin".to_owned(), all.clone())]);
    for (path, bytes) in written {
        let expected = format!("{{\"path\":\"{path}\",\"base64\":\"{}\"}}", base64(&bytes));
        assert!(listing.contains(&expected), "{path}: {listing}");
    }
}

#[cfg(feature = "python")]
#[test]
fn named_unicode_escapes_resolve_from_the_bundled_table() {
    let out = run_python_program(
        "main",
        &[(
            "main",
            "print('\\N{LATIN SMALL LETTER A}\\N{latin small letter b}', '\\N{GREEK SMALL LETTER ALPHA}')\n\
             print('\\N{EM DASH}' == '\\u2014', '\\N{SNOWMAN}' == '\\u2603', '\\N{NULL}' == '\\x00', '\\N{NBSP}' == '\\xa0')\n\
             print(hex(ord('\\N{CJK UNIFIED IDEOGRAPH-4E00}')), hex(ord('\\N{cjk unified ideograph-20000}')))\n\
             print(f'{1}\\N{DEGREE SIGN}', rb'\\N{X}', len('\\N{PARTY POPPER}'))\n",
        )],
        &[],
    )
    .expect("runs");
    assert_eq!(
        out,
        [
            "ab α",
            "True True True True",
            "0x4e00 0x20000",
            "1° b'\\\\N{X}' 1"
        ]
    );
    for unknown in [
        "x = '\\N{NOT A REAL CHARACTER NAME}'\n",
        "x = '\\N{CJK UNIFIED IDEOGRAPH-FFFF}'\n",
        "x = '\\N{CJK UNIFIED IDEOGRAPH-04E00}'\n",
    ] {
        let error = compile_error(unknown);
        assert!(
            error.starts_with("SyntaxError:")
                && error.contains("unknown Unicode character name")
                && error.contains("main.py:1:"),
            "{unknown:?}: {error}"
        );
    }
}

/// The compiler limits, counted on the parser's token stream, still stop a
/// program with the same message, and ordinary lexical and syntax errors
/// keep their `file:line:col` positions.
#[cfg(feature = "python")]
#[test]
fn single_lex_pass_keeps_the_limits_and_error_positions() {
    let limit = "Python: compiler complexity limit exceeded in main.py";
    let brackets = format!("x = {}1{}\n", "(".repeat(201), ")".repeat(201));
    assert_eq!(compile_error(&brackets), limit);
    let mut nested = String::new();
    for depth in 0..101 {
        nested.push_str(&" ".repeat(depth));
        nested.push_str("if True:\n");
    }
    nested.push_str(&" ".repeat(101));
    nested.push_str("pass\n");
    assert_eq!(compile_error(&nested), limit);
    // Just under the bracket and indent ceilings still compiles.
    let ok = format!("x = {}1{}\nprint(x)\n", "(".repeat(200), ")".repeat(200));
    assert_eq!(
        run_python_program("main", &[("main", ok.as_str())], &[]).expect("runs"),
        ["1"]
    );
    assert_eq!(
        compile_error("x = 1\ny = = 2\n"),
        "SyntaxError: invalid syntax. Got unexpected token '=' (main.py:2:5)"
    );
    assert_eq!(
        compile_error("x = 1\ny = 'open\n"),
        "SyntaxError: EOL while scanning string literal (main.py:3:1)"
    );
    assert_eq!(
        compile_error("if True:\n  x = 1\n y = 2\n"),
        "SyntaxError: unindent does not match any outer indentation level (main.py:3:2)"
    );
}

/// Lines reach the sink in the order the program produced them, each tagged
/// with its stream, and nothing is left in the buffers.
fn sink_lines(source: &str) -> (Vec<(ConsoleStream, String)>, usize) {
    let lines: Rc<RefCell<Vec<(ConsoleStream, String)>>> = Rc::default();
    let seen = Rc::clone(&lines);
    let mut state = compile_script(source).expect("compiles");
    state.set_console_sink(Box::new(move |stream, line| {
        seen.borrow_mut().push((stream, line.to_owned()))
    }));
    state.run_init().expect("runs");
    let left = state.take_console().len();
    let lines = lines.borrow().clone();
    (lines, left)
}

const SINK_PROBE: &str = r#"
    function emit(i) {
        if (i % 3 === 0) console.error("e" + i);
        else if (i % 3 === 1) console.log("o" + i);
        else print("p" + i);
    }
    for (var i = 0; i < 300; i++) emit(i);
    console.warn("done");
"#;

fn expected_sink() -> Vec<(ConsoleStream, String)> {
    (0..300)
        .map(|i| match i % 3 {
            0 => (ConsoleStream::Stderr, format!("e{i}")),
            1 => (ConsoleStream::Stdout, format!("o{i}")),
            _ => (ConsoleStream::Stdout, format!("p{i}")),
        })
        .chain([(ConsoleStream::Stderr, "done".to_owned())])
        .collect()
}

#[test]
fn console_sink_child() {
    if std::env::var_os("ZIPP_PY_DRIVER_SINK_CHILD").is_none() {
        return;
    }
    let (lines, left) = sink_lines(SINK_PROBE);
    assert_eq!(lines, expected_sink());
    assert_eq!(left, 0, "a sink leaves nothing buffered");
}

/// The sink sees production order in the interpreter, under a forced JIT and
/// under GC stress.
#[test]
fn console_sink_keeps_production_order_in_every_tier() {
    if std::env::var_os("ZIPP_PY_DRIVER_SINK_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "console_sink_child", "--nocapture"])
            .env("ZIPP_PY_DRIVER_SINK_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Without a sink the buffers behave as before.
#[test]
fn console_without_a_sink_still_buffers_in_order() {
    let mut state = compile_script(SINK_PROBE).expect("compiles");
    state.run_init().expect("runs");
    assert_eq!(state.take_console(), expected_sink());
}

/// A sink does not bypass the output ceiling: the line that crosses it is
/// refused before it reaches the sink.
#[cfg(feature = "instrument")]
#[test]
fn console_sink_is_charged_against_the_output_limit() {
    let lines: Rc<RefCell<Vec<String>>> = Rc::default();
    let seen = Rc::clone(&lines);
    let mut state = compile_script("for (var i = 0; i < 100; i++) console.log('line ' + i);")
        .expect("compiles");
    state.set_limits(10_000_000, None);
    state.set_output_limit(200);
    state.set_console_sink(Box::new(move |_, line| seen.borrow_mut().push(line.to_owned())));
    assert!(state.run_init().is_err(), "the ceiling must stop the program");
    let lines = lines.borrow();
    assert!(!lines.is_empty() && lines.len() < 100, "{} lines", lines.len());
    assert!(lines.iter().enumerate().all(|(i, l)| *l == format!("line {i}")));
}

/// Python's `print` and `sys.stderr` writes arrive through the sink in order.
#[cfg(feature = "python")]
#[test]
fn python_output_streams_through_the_sink_in_order() {
    let lines = big_stack(|| {
        let modules = [(
            "main".to_owned(),
            "import sys\nprint('one')\nsys.stderr.write('two\\n')\nprint('three', end='')\nprint(' four')\nprint('five', file=sys.stderr)\nraise SystemExit(0)\n".to_owned(),
        )];
        let mut compiled =
            zipp_vm::frontend::compile_python_program("main", &modules, &[], &[], false)
                .expect("compiles");
        let state = compiled.state_mut();
        let lines: Rc<RefCell<Vec<(ConsoleStream, String)>>> = Rc::default();
        let seen = Rc::clone(&lines);
        state.set_console_sink(Box::new(move |stream, line| {
            seen.borrow_mut().push((stream, line.to_owned()))
        }));
        let _ = state.run_init();
        let lines = lines.borrow().clone();
        lines
            .into_iter()
            .map(|(stream, line)| (stream == ConsoleStream::Stderr, line))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        lines,
        [
            (false, "one".to_owned()),
            (true, "two".to_owned()),
            (false, "three four".to_owned()),
            (true, "five".to_owned()),
        ]
    );
}
