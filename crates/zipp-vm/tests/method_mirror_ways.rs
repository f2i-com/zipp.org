//! `Vm::ic_call_method_mirror`: a method call's cached ways checked against
//! the receiver's hot mirror alone (its shape, and its fid word for a class
//! instance's class or a plain object). One site meets a class instance and a
//! plain object of the same layout, inherited and own methods, a method
//! replaced on a prototype and on a class, a prototype swapped under an
//! instance, an own method shadowing an inherited one, a non-function where a
//! method was, getters, and more layouts than the site keeps ways. The
//! expected lines are Node 24's.

const SOURCE: &str = r#"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));

class A { constructor() { this.x = 1; } m() { return "A.m" + this.x; } }
class B { constructor() { this.x = 2; } m() { return "B.m" + this.x; } }
function P() { this.x = 3; }
P.prototype.m = function () { return "P.m" + this.x; };
const plain = { x: 4, m() { return "own.m" + this.x; } };
const sameLayout = Object.create({ m() { return "proto.m" + this.x; } });
sameLayout.x = 5;
function call(o) { return o.m(); }
const recvs = [new A(), new B(), new P(), plain, sameLayout, new A(), new P()];
for (let r = 0; r < 4; r++) log("poly", recvs.map(call).join(","));

// Replace methods under the site.
A.prototype.m = function () { return "A2" + this.x; };
P.prototype.m = function () { return "P2" + this.x; };
log("replaced", recvs.map(call).join(","));
// An own method shadows the inherited one; a prototype swapped under an instance.
const a = new A();
for (let r = 0; r < 3; r++) call(a);
a.m = () => "own-arrow";
const p = new P();
for (let r = 0; r < 3; r++) call(p);
Object.setPrototypeOf(p, B.prototype);
log("shadow", call(a), call(p), call(new A()));
// A non-function, then a getter, where the method was.
const q = new P();
for (let r = 0; r < 3; r++) call(q);
P.prototype.m = 42;
try { call(q); } catch (e) { log("notfn", e.constructor.name); }
Object.defineProperty(P.prototype, "m", { get() { return () => "got"; }, configurable: true });
log("getter", call(q), call(new P()));
delete P.prototype.m;
try { call(q); } catch (e) { log("gone", e.constructor.name); }

// More layouts than ways at one site.
const many = [];
for (let i = 0; i < 20; i++) {
  const o = { m() { return i; } };
  for (let k = 0; k < i % 7; k++) o["f" + k] = k;
  many.push(o);
}
let s = 0;
for (let r = 0; r < 30; r++) for (const o of many) s += call(o);
log("many", s);

// Class instances: subclass, method added to the class later, static `this`.
class C extends A { m() { return "C" + super.m(); } }
const cs = [new C(), new A(), new C()];
for (let r = 0; r < 3; r++) log("sub", cs.map(call).join(","));
C.prototype.extra = function () { return "extra" + this.x; };
function callExtra(o) { return o.extra(); }
log("added", callExtra(new C()), callExtra(cs[0]));
console.log(out.join("\n"));
"#;

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

#[test]
fn method_calls_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone, with the lite
/// loop off, with the JIT compiling eagerly, and under GC stress.
#[test]
fn method_calls_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("no-lite", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LITE", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["method_calls_match_node", "--exact"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
