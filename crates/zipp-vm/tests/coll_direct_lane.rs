//! Compiled `m.get(k)` / `m.has(k)` / `s.has(v)`, lowered in spec order (the
//! callee captured before the argument), call the lookup directly when the
//! captured callee's bits are the boot intrinsic's (`jit_coll_lookup_direct`).
//! The bits decide which function runs; the receiver must still be of its
//! brand. Everything else — an override installed before the capture, one
//! installed BY the argument (the captured intrinsic still runs), an own
//! shadow, a subclass override, a borrowed intrinsic on a non-Map, a WeakMap,
//! a Set's `has` — gives exactly what Node gives. Every case runs in a loop
//! hot enough to compile. The expected lines are Node 24's.

const SOURCE: &str = r#"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 3000;
function tryIt(f) { try { return f(); } catch (e) { return e.constructor.name; } }

// The plain lanes: string, int, object, NaN and -0 keys, hits and misses.
const m = new Map([["a", 1], [2, "two"], [NaN, "nan"], [0, "zero"]]);
const ko = {}; m.set(ko, "obj");
const s = new Set(["x", 3, ko]);
let acc = "";
for (let i = 0; i < N; i++) {
  const k = [ "a", 2, NaN, -0, ko, "zz" ][i % 6];
  const v = m.get(k);
  if (i < 6) acc += String(v) + "|" + m.has(k) + "|" + s.has(i % 2 ? "x" : 3) + ";";
}
log("plain", acc);

// Replaced before the capture: the replacement runs.
const saved = Map.prototype.get;
Map.prototype.get = function (k) { return "patched:" + k; };
let p = "";
for (let i = 0; i < N; i++) { const r = m.get("a"); if (i === 0) p = r; }
Map.prototype.get = saved;
log("patched", p, m.get("a"));

// Replaced BY the argument after the capture: the captured intrinsic runs.
let flips = 0;
function key() { Map.prototype.get = function () { return "late"; }; flips++; return "a"; }
let q = "";
for (let i = 0; i < N; i++) { const r = m.get(key()); Map.prototype.get = saved; if (i === 0) q = r; }
log("late-override", q, flips);

// An own shadow and a subclass override.
const shadowed = new Map([["a", 1]]);
shadowed.get = function () { return "own"; };
class Sub extends Map { get(k) { return "sub:" + super.get(k); } }
const sub = new Sub([["a", 5]]);
let o1 = "", o2 = "";
for (let i = 0; i < N; i++) { const x = shadowed.get("a"), y = sub.get("a"); if (i === 0) { o1 = x; o2 = y; } }
log("shadow", o1, o2);

// The intrinsic borrowed onto a receiver that is not a Map: TypeError.
const fake = { get: Map.prototype.get, has: Map.prototype.has };
let e1 = "", e2 = "";
for (let i = 0; i < N; i++) { const a = tryIt(() => fake.get("a")), b = tryIt(() => fake.has("a")); if (i === 0) { e1 = a; e2 = b; } }
log("borrowed", e1, e2);

// Map.has on a Set and Set.has on a Map: TypeError; WeakMap.get works.
const mixed = { mh: Map.prototype.has, sh: Set.prototype.has };
let e3 = "", e4 = "";
for (let i = 0; i < N; i++) {
  const a = tryIt(() => mixed.mh.call(s, "x")), b = tryIt(() => mixed.sh.call(m, "a"));
  if (i === 0) { e3 = a; e4 = b; }
}
const wm = new WeakMap([[ko, "weak"]]);
let w = "";
for (let i = 0; i < N; i++) { const r = wm.get(ko); if (i === 0) w = r + "|" + wm.has(ko); }
log("cross", e3, e4, w);

// The mutations: Map.set / Set.add, their return values, a replacement
// installed before the capture, the intrinsics borrowed onto a non-collection.
const m2 = new Map(), s2 = new Set();
let ret = "";
for (let i = 0; i < N; i++) {
  const a = m2.set(i % 7, i), b = s2.add(i % 5);
  if (i === 0) ret = (a === m2) + "|" + (b === s2);
}
log("mutate", ret, m2.size, m2.get(3), s2.size, [...s2].join(","), [...m2.keys()].join(","));
const savedSet = Map.prototype.set, savedAdd = Set.prototype.add;
Map.prototype.set = function (k, v) { return "patched-set"; };
Set.prototype.add = function (v) { return "patched-add"; };
let pm = "";
for (let i = 0; i < N; i++) { const a = m2.set("x", 1), b = s2.add("y"); if (i === 0) pm = a + "|" + b; }
Map.prototype.set = savedSet; Set.prototype.add = savedAdd;
log("mutate-patched", pm, m2.has("x"), s2.has("y"));
const fake2 = { set: Map.prototype.set, add: Set.prototype.add };
let e5 = "", e6 = "";
for (let i = 0; i < N; i++) { const a = tryIt(() => fake2.set(1, 2)), b = tryIt(() => fake2.add(1)); if (i === 0) { e5 = a; e6 = b; } }
log("mutate-borrowed", e5, e6);

// A receiver that changes kind at one call site: Map, then Set, then WeakMap.
const recvs = [m, s, wm];
let cs = "";
for (let i = 0; i < N; i++) { const r = recvs[i % 3]; const h = r.has(i % 3 === 2 ? ko : "x"); if (i < 3) cs += h + ","; }
log("polymorphic", cs);
console.log(out.join("\n"));
"#;

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

#[test]
fn coll_direct_lane_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: with every function compiled at
/// once, with the direct lane off (the proved lane alone), in the
/// interpreter, and under GC stress.
#[test]
fn coll_direct_lane_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("eager", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("no-direct", &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_COLL_DIRECT_LANE", "1")][..]),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["coll_direct_lane_matches_node", "--exact"]);
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
