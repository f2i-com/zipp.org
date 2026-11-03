//! `Vm::tail_call_lean`: a strict `return f(x)` of a plain function takes over
//! the frame from the function's `frame_descs` entry, or runs a leaf with no
//! frame at all. Every kind of callee goes through the one site here: fewer and
//! more arguments than parameters, arguments that overlap the parameters they
//! become, a larger and a smaller window, sloppy, strict and arrow `this`,
//! `arguments` and rest callees (full path), generators, async functions,
//! natives, bound functions and classes (not plain), a constructing frame, a
//! tail position inside `try` and `finally` (no tail call), a callee that
//! changes at the site, and a deferred body replacing its stub. The expected
//! lines are Node 24's.

const SOURCE: &str = include_str!("tail_call_lean.js");

/// [`SOURCE`] behind enough functions that a deferring build (at 256) defers
/// every body here.
fn deferred_source() -> String {
    let (directive, body) = SOURCE.split_once('\n').expect("the source starts with its directive");
    assert!(directive.starts_with("\"use strict\";"));
    let mut src = String::from("\"use strict\";\n");
    for i in 0..300 {
        src.push_str(&format!("function pad{i}(a) {{ return a + {i}; }}\n"));
    }
    src.push_str(body);
    src
}

fn node_output(src: &str) -> Vec<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("node")
        .args(["--input-type=commonjs", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node on PATH");
    child.stdin.take().expect("stdin").write_all(src.as_bytes()).expect("write source");
    let out = child.wait_with_output().expect("node output");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn check(src: &str) {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(src));
}

#[test]
fn tail_calls_match_node() {
    check(SOURCE);
}

#[test]
fn deferred_tail_calls_match_node() {
    check(&deferred_source());
}

/// Past `MAX_FRAMES`, so only frame reuse finishes it; a runaway chain still
/// ends in the catchable RangeError of the streak's limit, and the streak
/// starts again after it.
#[test]
fn tail_calls_reuse_the_frame() {
    let src = r#"
        "use strict";
        function plain(n, s) { if (n === 0) return s; return plain(n - 1, s + 1); }
        console.log(plain(500000, 0));
        function forever(n) { return forever(n + 1); }
        try { forever(0); console.log("no limit"); } catch (e) { console.log(e.constructor.name); }
        console.log(plain(1000, 0));
    "#;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(zipp_vm::run(src));
        })
        .expect("spawn VM thread");
    let out = rx
        .recv_timeout(std::time::Duration::from_secs(120))
        .expect("engine hung")
        .expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output, ["500000", "RangeError", "1000"]);
}

/// The same programs in fresh children: the interpreter alone (where leaves
/// run), with each lean path off, with every body deferred, with the JIT
/// compiling eagerly, and under GC stress.
#[test]
fn tail_calls_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("no-lite", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LITE", "1")][..]),
        ("no-leaf", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LITE_LEAF", "1")][..]),
        ("no-lean", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LEAN_CALL", "1")][..]),
        ("run-all", &[("ZIPP_NOJIT", "1"), ("ZIPP_LAZY", "run-all")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        for test in ["tail_calls_match_node", "deferred_tail_calls_match_node", "tail_calls_reuse_the_frame"] {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args([test, "--exact"]);
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("spawn mode child");
            assert!(
                out.status.success(),
                "{name} {test} failed:\n{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
