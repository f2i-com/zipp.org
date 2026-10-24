//! The interpreter's inline inherited-read hit answers a guarded chain miss
//! (`node.body` on a node without one) and a receiver of another layout
//! than the one its first way was filled against (`ic_proto_hit_other_shape`),
//! `m.size` is read as the member walk's collection arm reads it, `==` of two
//! small integers is decided inline, a constructor's own data property
//! (`Array.isArray`) is read before the property cache, and a constructor's
//! adds rebind the shape's key list in place. Every one must give way when
//! the chain gains the key (as data or a getter), a receiver shadows it or
//! is re-prototyped, `size` is overridden, redefined or patched, a static is
//! replaced or made an accessor, and an instance's keys are deleted and
//! re-added. The expected lines are Node's; the source runs in this process
//! and again in an interpreter-only child, where the inline hits engage.

const SOURCE: &str = r##"
"use strict";
// Behaviours the inline chain-miss and other-shape inherited hits, the
// direct Map/Set size, integer ==, the early constructor read and the
// in-place planned key rebind must keep. Each function runs hot first.
const out = [];
const log = (...a) => out.push(a.map((x) => typeof x === "string" ? x : String(x)).join(" "));
function heat(f) { let r; for (let i = 0; i < 400; i++) r = f(i); return r; }

// Many layouts reading one absent field, then the chain gains it.
const shapes = [];
for (let i = 0; i < 24; i++) {
  const o = { type: "T" + (i % 7) };
  if (i % 2) o.left = i;
  if (i % 3) o.right = i;
  if (i % 5) o.id = "x" + i;
  shapes.push(o);
}
function readBody(o) { return o.body; }
log("absent", heat((i) => String(readBody(shapes[i % 24]))));
Object.prototype.body = "from Object.prototype";
log("chain gained", heat((i) => readBody(shapes[i % 24])));
delete Object.prototype.body;
log("chain lost", heat((i) => String(readBody(shapes[i % 24]))));
Object.defineProperty(Object.prototype, "body", { get() { return "getter " + (this.type || "?"); }, configurable: true });
log("chain getter", heat((i) => readBody(shapes[i % 24])));
delete Object.prototype.body;
shapes[5].body = "own";
log("own shadow", heat((i) => String(readBody(shapes[i % 24]))), readBody(shapes[5]));
delete shapes[5].body;
log("own gone", heat(() => String(readBody(shapes[5]))));

// One inherited method through many receiver layouts.
const proto = { kind() { return "proto"; }, label: "L" };
const recv = [];
for (let i = 0; i < 20; i++) {
  const o = Object.create(proto);
  o["f" + (i % 9)] = i;
  if (i % 4 == 0) o.extra = 1;
  recv.push(o);
}
function callKind(o) { return o.kind() + o.label; }
log("inherited", heat((i) => callKind(recv[i % 20])));
recv[3].kind = function () { return "own"; };
log("shadowed", heat((i) => callKind(recv[i % 20])), callKind(recv[3]));
proto.label = "M";
log("proto value changed", heat((i) => callKind(recv[i % 20])));
Object.defineProperty(proto, "label", { get() { return "G"; }, configurable: true });
log("proto accessor", heat((i) => callKind(recv[i % 20])));
Object.setPrototypeOf(recv[7], { kind() { return "other"; }, label: "O" });
log("reprototyped", heat((i) => callKind(recv[i % 20])), callKind(recv[7]));
const nullProto = Object.create(null);
nullProto.a = 1;
log("null proto absent", heat(() => String(readBody(nullProto))));

// Map / Set size.
const m = new Map(); const s = new Set();
log("size", heat((i) => { m.set(i % 13, i); s.add(i % 11); return m.size + s.size; }));
m.delete(3); s.delete(4);
log("size after delete", heat(() => m.size + "/" + s.size));
m.clear();
log("size after clear", heat(() => m.size));
class BigMap extends Map { get size() { return 1000 + super.size; } }
const bm = new BigMap([[1, 1]]);
log("subclass size", heat(() => bm.size));
const mm = new Map([[1, 2]]);
Object.defineProperty(mm, "size", { value: "own size" });
log("own size", heat(() => mm.size), new Map().size);
const desc = Object.getOwnPropertyDescriptor(Map.prototype, "size");
Object.defineProperty(Map.prototype, "size", { get() { return "patched"; }, configurable: true });
log("patched size", heat(() => new Map().size), heat(() => new Set([1]).size));
Object.defineProperty(Map.prototype, "size", desc);
log("restored size", heat(() => new Map([[1, 1], [2, 2]]).size));
const pm = new Map([[1, 1]]);
Object.setPrototypeOf(pm, { get size() { return "custom proto"; } });
log("setPrototypeOf size", heat(() => pm.size));
log("size on non-map", heat(() => { try { return Object.create(Map.prototype).size; } catch (e) { return e.constructor.name; } }));

// Integer loose equality.
log("int ==", heat((i) => [(i & 3) == 1, (i & 3) != 1, 5 == 5, 5 != 6].join()));
log("mixed ==", [1 == 1.0, 0 == -0, "5" == 5, 5 == "5", null == 0, undefined == 0, true == 1, 2 == true, NaN == NaN, 1 != "1"].join());
log("big ints", heat(() => [2147483647 == 2147483647, -2147483648 == -2147483648, 2147483647 + 1 == 2147483648].join()));

// Constructor statics read early.
log("ctor reads", heat(() => [Array.isArray([]), Array.isArray({}), typeof Object.keys, Number.MAX_SAFE_INTEGER, Math.PI > 3].join()));
const origIsArray = Array.isArray;
Array.isArray = function () { return "replaced"; };
log("replaced static", heat(() => Array.isArray([])));
Array.isArray = origIsArray;
log("restored static", heat(() => Array.isArray([])));
log("ctor intrinsics", heat(() => [typeof Array.prototype, Array.name, Array.length, Object.prototype === Object.getPrototypeOf({})].join()));
Object.defineProperty(Array, "isArray", { get() { return () => "getter static"; }, configurable: true });
log("accessor static", heat(() => Array.isArray([])));
Object.defineProperty(Array, "isArray", { value: origIsArray, writable: true, configurable: true });
log("back static", heat(() => Array.isArray([1])));

// Constructor chains through planned key lists.
function Wide(a) {
  this.k0 = a; this.k1 = a + 1; this.k2 = a + 2; this.k3 = a + 3; this.k4 = a + 4; this.k5 = a + 5;
  this.k6 = a + 6; this.k7 = a + 7; this.k8 = a + 8; this.k9 = a + 9; this.k10 = a + 10; this.k11 = a + 11;
  this.k12 = a + 12; this.k13 = a + 13; this.k14 = a + 14; this.k15 = a + 15; this.k16 = a + 16; this.k17 = a + 17;
}
log("wide", heat((i) => { const w = new Wide(i); return w.k0 + w.k9 + w.k17; }));
const w = new Wide(1);
log("wide keys", Object.keys(w).join(), JSON.stringify(w));
delete w.k3; w.k3 = "re"; w.z = 0;
log("wide edited", Object.keys(w).join(), JSON.stringify(w));
const w2 = new Wide(2); w2.extra = 1; const w3 = new Wide(3);
log("wide siblings", Object.keys(w2).length, Object.keys(w3).length, w3.k17, "extra" in w3);
function Branchy(f) { this.a = 1; if (f) { this.b = 2; this.c = 3; } else { this.c = 3; this.b = 2; } this.d = 4; }
log("branchy", heat((i) => JSON.stringify(new Branchy(i & 1))), JSON.stringify(new Branchy(0)));
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn zipp_lines() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n").lines().map(str::to_owned).collect()
}

#[test]
fn ic_shape_paths_worker() {
    if std::env::var_os("ZIPP_IC_SHAPE_PATHS_WORKER").is_none() {
        return;
    }
    // The harness has printed `test ic_shape_paths_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn ic_shape_paths_match_node() {
    if std::env::var_os("ZIPP_IC_SHAPE_PATHS_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["ic_shape_paths_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_IC_SHAPE_PATHS_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn the worker");
    assert!(out.status.success(), "worker failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_owned))
        .collect();
    assert_eq!(got, want, "interpreter-only run");
}
