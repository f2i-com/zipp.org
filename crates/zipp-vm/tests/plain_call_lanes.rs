//! Plain calls on the emitted cross-call lane that a script's functions
//! need: a sloppy callee, whose `this` the lane binds as the global object,
//! beside a strict one; and a Tier C body's self call through its own
//! global name (the self arm), recursive object builders included, with the
//! global rebound mid-run so the old body's self call must reach the new
//! function. Run interpreted, at the default thresholds (which must plan
//! the self arm, per `ZIPP_JITLOG`), with every function compiled at once,
//! and with both lanes switched off; the expected line is Node 24's.

const EXPECTED: &str = "3000/3000/3000|3000|232000|122734200|-5|-1|720";

#[test]
fn plain_call_lanes_child() {
    if std::env::var_os("ZIPP_PLAIN_CALL_LANES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn plain_call_lanes_run_as_node_runs_them() {
    if std::env::var_os("ZIPP_PLAIN_CALL_LANES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let off: &[(&str, &str)] = &[("ZIPP_NO_CROSS3_SLOPPY", "1"), ("ZIPP_NO_CROSS3_SELF_GLOBAL", "1")];
    for (mode, env) in [
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("default-thresholds", &[][..]),
        ("compile-at-once", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("lanes-off", off),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "plain_call_lanes_child", "--nocapture"])
            .env("ZIPP_PLAIN_CALL_LANES_CHILD", "1")
            .env("ZIPP_JITLOG", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr (tail) ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            stderr.lines().rev().take(40).collect::<Vec<_>>().join("\n")
        );
        if cfg!(all(feature = "jit", target_arch = "x86_64")) && mode == "default-thresholds" {
            assert!(stderr.contains("CROSS3 self arm"), "the default tiers never planned a self arm");
        }
    }
}

const PROGRAM: &str = r####"// Plain calls of sloppy functions and self calls through a global name.
const out = [];
function isGlobal() { return this === globalThis; }
function bump(k) { this.gcount = (this.gcount | 0) + k; return this.gcount; }
function strictThis() { "use strict"; return this === undefined; }
function hot(n) { let g = 0, s = 0, u = 0; for (let i = 0; i < n; i++) { if (isGlobal()) g++; s = bump(1); if (strictThis()) u++; } return g + "/" + s + "/" + u; }
out.push(hot(3000), globalThis.gcount);
// Recursion through the global name, an object-building one included.
function depth(n) { return n <= 0 ? 0 : 1 + depth(n - 1); }
function build(n) { return n <= 0 ? { v: 0 } : { v: n, next: build(n - 1) }; }
function sumList(l) { return l === undefined ? 0 : l.v + sumList(l.next); }
function rec(n) { let s = 0; for (let i = 0; i < n; i++) s += depth(i & 31) + sumList(build(i & 15)); return s; }
out.push(rec(4000));
// The global rebound mid-run: the old body's self call reaches the new one.
function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }
let t = 0;
for (let i = 0; i < 3000; i++) t += fact(i % 10);
const orig = fact;
fact = function (n) { return -1; };
out.push(t, orig(5), fact(5));
fact = orig;
out.push(fact(6));
console.log(out.join("|"));
"####;
