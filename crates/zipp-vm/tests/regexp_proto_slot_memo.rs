//! `re.test(s)` and `re.exec(s)` in compiled code while %RegExp.prototype%
//! changes under them, after `Vm::regexp_proto_pos` has remembered where
//! `exec` and `test` live: other properties deleted and re-added, `exec`
//! replaced by a user function that `test` must call, `test` deleted (the
//! call throws) and restored, `test` replaced, and `test` shadowed on one
//! instance. Run interpreted, at the default thresholds, with every function
//! compiled at once and under GC stress; the expected lines are Node 24's.

const EXPECTED: &str = "2000/4000
2000/4000
2000/4000
10000
2000/4000
TypeError
2000/4000
1000/4000
5000/2000
2000/4000
3000";

#[test]
fn regexp_proto_slot_memo_child() {
    if std::env::var_os("ZIPP_REGEXP_SLOT_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn compiled_regexp_calls_follow_the_prototype() {
    if std::env::var_os("ZIPP_REGEXP_SLOT_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "regexp_proto_slot_memo_child", "--nocapture"])
            .env("ZIPP_REGEXP_SLOT_CHILD", "1")
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

const PROGRAM: &str = r####"// `re.test(s)` / `re.exec(s)` in compiled code while %RegExp.prototype%
// changes under them: other properties deleted and re-added (slots move),
// `exec` replaced by a user function (which `test` must call), `test`
// deleted and restored, `test` shadowed on one instance.
"use strict";
const out = [];
const RE = /[a-z]+/;
const subjects = ["abc", "123", "x9", "", "Q"];
function run(re, n) {
  let t = 0, e = 0;
  for (let k = 0; k < n; k++) {
    const s = subjects[k % subjects.length];
    if (re.test(s)) t++;
    const m = re.exec(s);
    if (m !== null) e += m[0].length;
  }
  return t + "/" + e;
}
const proto = RegExp.prototype;
const origExec = proto.exec, origTest = proto.test;
out.push(run(RE, 5000));
// Slots move: delete and re-add properties that come before exec/test.
for (const k of ["compile", "toString"]) {
  const d = Object.getOwnPropertyDescriptor(proto, k);
  delete proto[k];
  Object.defineProperty(proto, k, d);
}
out.push(run(RE, 5000));
// A user exec: test must call it.
let calls = 0;
proto.exec = function (s) { calls++; return origExec.call(this, s); };
out.push(run(RE, 5000), calls);
proto.exec = origExec;
out.push(run(RE, 5000));
// test deleted: the call throws; restored: works again.
delete proto.test;
try { run(RE, 10); out.push("no throw"); } catch (e) { out.push(e.constructor.name); }
proto.test = origTest;
out.push(run(RE, 5000));
// test redefined as an accessor-free data property holding another function.
proto.test = function (s) { return s.length === 1; };
out.push(run(RE, 5000));
proto.test = origTest;
// Shadow test on one instance only.
const other = /\d/;
other.test = () => true;
out.push(run(other, 5000), run(RE, 5000));
// A fresh literal per call.
let lit = 0;
for (let k = 0; k < 5000; k++) if (/[a-z_0-9]/.test(subjects[k % 5])) lit++;
out.push(lit);
console.log(out.join("\n"));
"####;
