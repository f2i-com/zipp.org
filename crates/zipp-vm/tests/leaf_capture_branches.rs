//! Frameless leaf callbacks (`leaf_scan`) that write a capture behind a
//! forward branch, against Node: a conditional counter, a write skipped on
//! some paths and read back, a TDZ cell (ReferenceError, nothing written), a
//! const cell (TypeError), writes on two branches and a conditional after a
//! write (both not leaves), non-number operands declining before the write,
//! and object conditions. In the interpreter, by default and with every
//! function compiled at once.

const SOURCE: &str = r##""use strict";
const out = [];
const arr = []; for (let i = 0; i < 200; i++) arr.push(i % 17);
// 1. a conditional captured write
let n = 0; const k = 3;
for (let r = 0; r < 300; r++) arr.forEach((v) => { if (v === k) n++; });
out.push("A " + n);
// 2. a write skipped on some paths, then a read of the cell
let last = -1;
const got = arr.map((v) => { if (v > 10) last = v; return last; });
out.push("B " + got.slice(0, 20).join(",") + " " + last);
// 3. a TDZ cell: the write must throw ReferenceError, not run
function tdz() {
  const cb = (v) => { if (v >= 0) t = v; return 1; };
  let e = "none";
  try { [1, 2].forEach(cb); } catch (err) { e = err.constructor.name; }
  let t = 0;
  [5].forEach(cb);
  return e + ":" + t;
}
out.push("C " + tdz());
// 4. a const cell: the write throws TypeError
function cst() {
  const c = 1; let e = "none";
  try { [1].forEach((v) => { if (v) c = 2; }); } catch (err) { e = err.constructor.name; }
  return e + ":" + c;
}
out.push("D " + cst());
// 5. writes on two branches, and a conditional after a write
let p = 0, q = 0;
arr.forEach((v) => { if (v & 1) { p += v; } else { q += 1; } });
let w = 0; const ws = arr.map((v) => { w += v; if (v > 8) return 1; return 0; });
out.push("E " + p + " " + q + " " + w + " " + ws.reduce((a, b) => a + b, 0));
// 6. a non-number on the arithmetic path declines before the write
let s = 0; const mixed = [1, "2", 3, null, 4.5, undefined, {}];
mixed.forEach((v) => { if (v !== null) s = s + v; });
out.push("F " + s);
// 7. the same leaf, hot, with conditions that are objects
let hits = 0; const objs = [{}, null, [], 0, "x", ""];
for (let r = 0; r < 500; r++) objs.forEach((o) => { if (o) hits = hits + 1; });
out.push("G " + hits);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "A 3600\nB -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,11,12,13,14,15,16,16,16,16 12\nC ReferenceError:5\nD TypeError:1\nE 740 106 1574 92\nF 1234.5undefined[object Object]\nG 1500";

#[test]
fn leaf_capture_branches_worker() {
    if std::env::var_os("ZIPP_LEAF_CAPTURE_BRANCHES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn leaf_capture_branches_match_node() {
    if std::env::var_os("ZIPP_LEAF_CAPTURE_BRANCHES_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["leaf_capture_branches_worker", "--exact", "--nocapture"])
            .env("ZIPP_LEAF_CAPTURE_BRANCHES_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
