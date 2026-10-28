//! Compiled `new` of store-only constructors through the memos of the last
//! class resolved (`Vm::new_class_memo`) and the last constructor served
//! (`Vm::ctor_store_last`): two classes alternating at one site, a class made
//! afresh each round, a setter added to a prototype mid-run, a plain
//! constructor function and a swapped `prototype`, a subclass, a class with
//! fields, and a function compiled at its first call. Run interpreted, at the
//! default thresholds, with every function compiled at once, under GC stress,
//! with the store path off and with lazy compilation off; the expected lines
//! are Node 24's.

const EXPECTED: &str = r####"mix 600040000
fresh 1584000
setter 24492500 seen 15998000
plain 200050000
swapped 12512500 swapped
sub 50015000
fields 50065000
late 4498500 4498500"####;

#[test]
fn new_store_memo_child() {
    if std::env::var_os("ZIPP_NEW_MEMO_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn compiled_new_matches_node() {
    if std::env::var_os("ZIPP_NEW_MEMO_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("store-off", Some(("ZIPP_NO_CTOR_STORE", "1"))),
        ("eager", Some(("ZIPP_LAZY", "0"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "new_store_memo_child", "--nocapture"])
            .env("ZIPP_NEW_MEMO_CHILD", "1")
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

const PROGRAM: &str = r####"// Compiled `new` of store-only constructors through the memos of the last
// class resolved and the last constructor served: two classes alternating
// at one site, a class made afresh each round (its old slot freed and
// reused), a setter added to a prototype mid-run (the stores must then run
// as the body would), a plain constructor function, a subclass, a class with
// fields, and a function compiled at its first call.
"use strict";
const out = [];
class A { constructor(x, y) { this.x = x; this.y = y; } }
class B { constructor(x, y) { this.y = y; this.x = x; } }
function mix(n) {
  let t = 0;
  for (let i = 0; i < n; i++) {
    const o = (i & 1) ? new A(i, 1) : new B(i, 2);
    t += o.x * 3 + o.y + Object.keys(o).join("").length;
  }
  return t;
}
out.push("mix " + mix(20000));
function fresh(rounds) {
  let t = 0;
  for (let r = 0; r < rounds; r++) {
    const C = class { constructor(v) { this.v = v; this.r = r; } };
    for (let i = 0; i < 200; i++) t += new C(i).v + new C(1).r;
    if (r % 7 === 0) for (let g = 0; g < 2000; g++) t += [g, g].length;
  }
  return t;
}
out.push("fresh " + fresh(60));
class S { constructor(v) { this.v = v; this.w = v * 2; } }
let seen = 0;
function setterRun(n, at) {
  let t = 0;
  for (let i = 0; i < n; i++) {
    if (i === at) Object.defineProperty(S.prototype, "w", { set(x) { seen += x; }, get() { return -1; }, configurable: true });
    const s = new S(i);
    t += s.v + s.w + (Object.hasOwn(s, "w") ? 1000 : 0);
  }
  return t;
}
out.push("setter " + setterRun(5000, 3000) + " seen " + seen);
function P(x) { this.x = x; this.z = null; }
function plain(n) { let t = 0; for (let i = 0; i < n; i++) { const p = new P(i); t += p.x + (p.z === null ? 1 : 0) + (p instanceof P ? 2 : 0); } return t; }
out.push("plain " + plain(20000));
P.prototype = { tag: "swapped" };
out.push("swapped " + plain(5000) + " " + new P(1).tag);
class Base { constructor(a) { this.a = a; } }
class Sub extends Base { constructor(a, b) { super(a); this.b = b; } }
function sub(n) { let t = 0; for (let i = 0; i < n; i++) { const s = new Sub(i, 2); t += s.a + s.b; } return t; }
out.push("sub " + sub(10000));
class F { q = 7; constructor(v) { this.v = v; } }
function fields(n) { let t = 0; for (let i = 0; i < n; i++) t += new F(i).q + new F(i).v; return t; }
out.push("fields " + fields(10000));
function late() {
  class L { constructor(u) { this.u = u; } }
  let t = 0;
  for (let i = 0; i < 3000; i++) t += new L(i).u;
  return t;
}
out.push("late " + late() + " " + late());
console.log(out.join("\n"));
"####;
