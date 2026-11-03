//! `instanceof` against plain functions that carry own properties (a side
//! table): the fast path reads `@@hasInstance` and `prototype` from that table
//! in one pass. Every way either key can be present or change is met at one
//! site: statics only, an own `Symbol.hasInstance` (data and getter), an own
//! `prototype` reassigned to an object and to a primitive, defined as an
//! accessor, a table big enough to be indexed, a class extending such a
//! function, and arrows and bound functions on the right. The expected lines
//! are Node 24's.

const SOURCE: &str = r#"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const test = (v, C) => { try { return String(v instanceof C); } catch (e) { return e.constructor.name; } };

function Node() {}
Node.BASE = null; Node.PROPS = ["a"]; Node.TYPE = "Node"; Node.CTOR = Node;
function Leaf() {}
Leaf.prototype = Object.create(Node.prototype);
Leaf.BASE = Node; Leaf.TYPE = "Leaf"; Leaf.PROPS = []; Leaf.SUBCLASSES = [];
const n = new Node(), l = new Leaf();
for (let r = 0; r < 3; r++) log("statics", test(n, Node), test(l, Node), test(n, Leaf), test(l, Leaf), test({}, Node));

function Odd() {}
Odd.x = 1;
Object.defineProperty(Odd, Symbol.hasInstance, { value: () => true });
try { Node[Symbol.hasInstance] = () => true; } catch (e) { log("assign hasInstance", e.constructor.name); }
log("own hasInstance", test(1, Odd), test({}, Odd));
function Getter() {}
Getter.y = 2;
Object.defineProperty(Getter, Symbol.hasInstance, { get() { return (v) => v === 42; } });
log("hasInstance getter", test(42, Getter), test(41, Getter));

function Reproto() {}
Reproto.z = 3;
const before = new Reproto();
Reproto.prototype = { tag: 1 };
const after = new Reproto();
log("reassigned", test(before, Reproto), test(after, Reproto));
function Prim() {}
Prim.w = 4;
Prim.prototype = 7;
log("primitive prototype", test({}, Prim));
function Acc() {}
Acc.v = 5;
log("plain first", test(new Acc(), Acc));

const big = function Big() {};
for (let i = 0; i < 80; i++) big["k" + i] = i;
log("indexed", test(new big(), big), test({}, big));
Object.defineProperty(big, Symbol.hasInstance, { value: () => "yes" });
log("indexed hasInstance", test({}, big));

class Sub extends Node {}
log("class over function", test(new Sub(), Sub), test(new Sub(), Node), test(n, Sub));
const arrow = () => {};
arrow.q = 1;
log("arrow", test({}, arrow));
const bound = Node.bind(null);
bound.r = 1;
log("bound", test(n, bound), test({}, bound));
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
fn instanceof_fn_statics_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone, with the JIT
/// compiling eagerly, and under GC stress.
#[test]
fn instanceof_fn_statics_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["instanceof_fn_statics_match_node", "--exact"]);
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
