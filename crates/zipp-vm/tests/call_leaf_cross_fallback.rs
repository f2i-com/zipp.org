//! Plain calls of small closures inlined as leaves, where each round makes a new
//! closure of the same code or calls another function, so the inlined arm
//! misses and the fallback (`jit_call_ic_cross`) runs: closures mutating a
//! captured counter, a sloppy and a strict function, a throwing callee, a
//! native and a non-callable. Run interpreted, at the default thresholds, with
//! every function compiled at once, under GC stress and with the cross call
//! off; the expected line is Node 24's.

const EXPECTED: &str = "108829460,108829460,108829460,108829460,108829460,108829460,106956845,108266443,RangeError,30000,54545000,TypeError";

#[test]
fn call_leaf_cross_fallback_child() {
    if std::env::var_os("ZIPP_CALL_LEAF_CROSS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn leaf_fallback_calls_match_node() {
    if std::env::var_os("ZIPP_CALL_LEAF_CROSS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("cross-off", Some(("ZIPP_NO_CALL_CROSS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "call_leaf_cross_fallback_child", "--nocapture"])
            .env("ZIPP_CALL_LEAF_CROSS_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Plain calls of small closures inlined as leaves, where each round makes a
// new closure (a fresh identity of the same code) or a different function:
// the inlined arm misses and the call takes the general route. Closures
// mutating a captured counter, a sloppy and a strict function, a throwing
// callee, a native, and a non-callable.
const out = [];
function makeOrders(n, mk) {
  let s = 7;
  const rnd = mk ? mk() : (m) => ((s = (s * 16807) % 2147483647) % m);
  let t = 0;
  for (let id = 0; id < n; id++) {
    t = (t + rnd(5) + rnd(300) * 3 + rnd(4) + rnd(10000)) % 1000000007;
  }
  return t;
}
for (let r = 0; r < 6; r++) out.push(makeOrders(20000));
let state = 3;
const sloppy = function (m) { state = (state * 31 + (this === undefined ? 1 : 2)) % 9973; return state % m; };
const strict = function (m) { "use strict"; state = (state * 17 + (this === undefined ? 1 : 2)) % 9973; return state % m; };
out.push(makeOrders(20000, () => sloppy), makeOrders(20000, () => strict));
let calls = 0;
const thrower = (m) => { if (++calls === 30000) throw new RangeError("r" + m); return m >> 1; };
try { out.push(makeOrders(20000, () => thrower)); } catch (e) { out.push(e.constructor.name, calls); }
out.push(makeOrders(5000, () => Math.abs));
try { makeOrders(10, () => 5); out.push("no throw"); } catch (e) { out.push(e.constructor.name); }
console.log(out.join(","));
"####;
