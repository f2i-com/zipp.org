//! Objects with many properties against Node: a constructor writing
//! seventeen fields (Babel's NodePath), constructors of 12 to 70 fields
//! across the shape plan's 32-key cap and the 64-key point where a map whose
//! keys are a shared plan builds its index, literals of 14 keys, and every
//! read path over them (named, computed, `in`, own-property checks,
//! enumeration order, JSON, Object.assign, for-in) after deletes, re-adds,
//! defineProperty and appends past the plan. The expected lines are Node's
//! (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
function NodePath(hub, parent) {
  this.contexts = []; this.state = null; this.opts = null; this._traverseFlags = 0;
  this.skipKeys = null; this.parentPath = null; this.container = null; this.listKey = null;
  this.key = null; this.node = null; this.type = null; this._store = null;
  this.parent = parent; this.hub = hub; this.data = null; this.context = null; this.scope = null;
}
const paths = [];
for (let i = 0; i < 300; i++) paths.push(new NodePath("h" + i, { i }));
const p = paths[123];
log("keys", Object.keys(p).length, Object.keys(p).join());
log("reads", p.hub, p.parent.i, p.scope, p.contexts.length, p._traverseFlags);
let computed = 0;
for (const k of Object.keys(p)) if (p[k] !== undefined) computed++;
log("computed", computed, "scope" in p, "missing" in p, p.hasOwnProperty("listKey"), Object.prototype.hasOwnProperty.call(p, "nope"));
p.extra = 1;
p.key = "k";
delete p.state;
log("after edits", Object.keys(p).length, p.state, p.key, p.extra, Object.keys(p).slice(-3).join());
log("other untouched", Object.keys(paths[7]).length, paths[7].state, paths[7].extra);
function Wide(n) { for (let i = 0; i < n; i++) this["f" + i] = i * 2; }
for (const n of [12, 13, 16, 17, 31, 32, 33, 40, 63, 64, 65, 70]) {
  const w = new Wide(n);
  let sum = 0;
  for (let i = 0; i < n; i++) sum += w["f" + i];
  const keys = Object.keys(w);
  log("wide", n, keys.length, sum, keys[0], keys[n - 1], w["f" + (n - 1)], "f" + n in w, JSON.stringify(w).length);
}
const w40 = new Wide(40);
delete w40.f5;
w40.f5 = "back";
Object.defineProperty(w40, "f10", { value: "fixed", writable: false });
try { w40.f10 = "nope"; } catch (e) { log("readonly", e.constructor.name); }
log("w40", Object.keys(w40).length, w40.f5, w40.f10, Object.keys(w40).indexOf("f5"), w40.f39);
const lits = [];
for (let i = 0; i < 50; i++) {
  lits.push({ a: i, b: 1, c: 2, d: 3, e: 4, f: 5, g: 6, h: 7, i2: 8, j: 9, k: 10, l: 11, m: 12, n: 13 });
}
let ls = 0;
for (const o of lits) for (const k in o) ls += o[k];
lits[3].z = 99;
delete lits[4].c;
log("literals", ls, Object.keys(lits[3]).length, lits[3].z, Object.keys(lits[4]).join(), lits[5].n);
const forIn = [];
for (const k in new Wide(14)) forIn.push(k);
log("for-in", forIn.join());
const copy = Object.assign({}, new Wide(20));
log("assign", Object.keys(copy).length, copy.f19);
class Big { constructor() { for (let i = 0; i < 20; i++) this["c" + i] = i; this.done = true; } }
const bigs = Array.from({ length: 100 }, () => new Big());
log("class", Object.keys(bigs[50]).length, bigs[99].c19, bigs[0].done, JSON.stringify(bigs[1]).slice(0, 30));
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"keys 17 contexts,state,opts,_traverseFlags,skipKeys,parentPath,container,listKey,key,node,type,_store,parent,hub,data,context,scope
reads h123 123 null 0 0
computed 17 true false true false
after edits 17 undefined k 1 context,scope,extra
other untouched 17 null undefined
wide 12 12 132 f0 f11 22 false 94
wide 13 13 156 f0 f12 24 false 103
wide 16 16 240 f0 f15 30 false 130
wide 17 17 272 f0 f16 32 false 139
wide 31 31 930 f0 f30 60 false 265
wide 32 32 992 f0 f31 62 false 274
wide 33 33 1056 f0 f32 64 false 283
wide 40 40 1560 f0 f39 78 false 346
wide 63 63 3906 f0 f62 124 false 566
wide 64 64 4032 f0 f63 126 false 576
wide 65 65 4160 f0 f64 128 false 586
wide 70 70 4830 f0 f69 138 false 636
readonly TypeError
w40 40 back fixed 39 78
literals 5775 15 99 a,b,d,e,f,g,h,i2,j,k,l,m,n 13
for-in f0,f1,f2,f3,f4,f5,f6,f7,f8,f9,f10,f11,f12,f13
assign 20 38
class 21 19 true {"c0":0,"c1":1,"c2":2,"c3":3,""###;
/// Run src on a 256 MiB stack, as the CLI does, and return its output.
fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn wide_objects_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
