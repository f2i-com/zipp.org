//! Property reads and writes compiled with Tier C's known-shape lane (the
//! shape the interpreter last saw at each site, `emit_prop_shape_get` and
//! `emit_prop_shape_set`), then given receivers the lane must not serve:
//! another shape, an object whose shape changed (a key added, deleted, frozen,
//! redefined as a getter), a key it lacks with a setter on its prototype, and
//! young objects stored into long-lived ones through it (the barrier), with
//! collections between rounds. Run interpreted, at the default thresholds,
//! with every function compiled at once, under GC stress and with the lane
//! off; the expected lines are Node 24's.

const EXPECTED: &str = "4684800
2007008
1999
3
10
12
11
9
TypeError
4
107
100
12
1
-1
1
y
1920";

#[test]
fn prop_shape_lane_child() {
    if std::env::var_os("ZIPP_PROP_SHAPE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn compiled_property_lanes_follow_the_receiver() {
    if std::env::var_os("ZIPP_PROP_SHAPE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("lane-off", Some(("ZIPP_NO_PROP_SHAPE_LANE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "prop_shape_lane_child", "--nocapture"])
            .env("ZIPP_PROP_SHAPE_CHILD", "1")
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

const PROGRAM: &str = r####"// Property reads and writes compiled with the known-shape lane (the shape the
// interpreter last saw at each site), then given receivers it must not serve:
// another shape, an object whose shape changed (a key added, deleted,
// frozen, redefined as a getter), a key it lacks with a setter on its
// prototype, and young objects stored into long-lived ones (the barrier),
// with collections between rounds.
"use strict";
const out = [];
class P { constructor(x, y) { this.x = x; this.y = y; this.tag = null; } }
function readXY(o) { return o.x + o.y; }
function writeX(o, v) { o.x = v; return o.x; }
function setTag(o, t) { o.tag = t; }
function churn(n) { let k = 0; for (let i = 0; i < n; i++) { const t = { i, a: [i] }; if (t.a[0] === -1) k++; } return k; }

const pts = [];
for (let i = 0; i < 64; i++) pts.push(new P(i, i * 2));
let s = 0;
for (let r = 0; r < 300; r++) for (const p of pts) { s += readXY(p); writeX(p, p.x + 1); }
out.push(s);
// Another shape at the same sites.
const other = { y: 5, x: 7, tag: null };
let t = 0;
for (let r = 0; r < 2000; r++) { t += readXY(other); writeX(other, r); }
out.push(t, other.x);
// The shape changes under compiled code.
const q = new P(1, 2);
for (let r = 0; r < 2000; r++) readXY(q);
q.z = 3;                       // added key: new shape
out.push(readXY(q), writeX(q, 10));
delete q.z;                    // dictionary mode
out.push(readXY(q), writeX(q, 11));
const f = Object.freeze(new P(4, 5));
out.push(readXY(f));
try { writeX(f, 9); out.push("no throw"); } catch (e) { out.push(e.constructor.name, f.x); }
const g = new P(6, 7);
let gets = 0;
Object.defineProperty(g, "x", { get() { gets++; return 100; }, set(v) { gets += 10; }, configurable: true });
out.push(readXY(g), writeX(g, 1), gets);
// A key the receiver lacks, with a setter on its prototype.
let hits = 0;
const proto = { set x(v) { hits++; }, get x() { return -1; } };
const h = Object.create(proto);
h.y = 2;
out.push(readXY(h), writeX(h, 5), hits, Object.keys(h).join(","));
// Young objects stored into long-lived objects' fields through the lane.
churn(200000);
let ok = 0;
for (let round = 0; round < 30; round++) {
  for (let i = 0; i < pts.length; i++) setTag(pts[i], { round, i, s: "r" + round + "i" + i });
  churn(30000);
  for (let i = 0; i < pts.length; i++) {
    const tg = pts[i].tag;
    if (tg.round === round && tg.i === i && tg.s === "r" + round + "i" + i) ok++;
  }
}
out.push(ok);
console.log(out.join("\n"));
"####;
