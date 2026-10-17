//! Frozen and sealed objects keep a shape (one with their new descriptor
//! bits), so the property caches serve them. Everything those bits and the
//! missing extensibility forbid must still be refused at cache-hot sites:
//! writes to non-writable properties, adds to non-extensible objects, writes
//! through a frozen prototype, in sloppy and strict code. The expected lines
//! are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function make(i) { return { id: i, w: i * 3, name: "n" + i }; }
var frozen = [], sealed = [], closed = [], open = [];
for (var i = 0; i < 8; i++) {
  frozen.push(Object.freeze(make(i)));
  sealed.push(Object.seal(make(i)));
  closed.push(Object.preventExtensions(make(i)));
  open.push(make(i));
}
// Same keys and bits as a frozen object, but still extensible.
var lookalike = {};
Object.defineProperty(lookalike, "id", { value: 99, enumerable: true });
Object.defineProperty(lookalike, "w", { value: 7, enumerable: true });
Object.defineProperty(lookalike, "name", { value: "look", enumerable: true });
function readAll(list) { var s = 0; for (var r = 0; r < 50; r++) for (var j = 0; j < list.length; j++) s += list[j].id + list[j].w + list[j].name.length; return s; }
log("reads", readAll(frozen), readAll(sealed), readAll(closed), readAll(open), readAll([lookalike, frozen[0]]));
function writeW(o, v) { o.w = v; return o.w; }
function addExtra(o, v) { o.extra = v; return o.extra; }
var row = [];
for (var r = 0; r < 40; r++) {
  var all = [open[r & 7], frozen[r & 7], sealed[r & 7], closed[r & 7], lookalike];
  for (var j = 0; j < all.length; j++) row.push(writeW(all[j], r) + "/" + addExtra(all[j], r));
}
log("sloppy writes", row.slice(0, 10).join(), row.slice(-10).join());
log("flags", Object.isFrozen(frozen[0]), Object.isSealed(sealed[0]), Object.isExtensible(closed[0]), Object.isFrozen(lookalike), Object.isExtensible(lookalike));
function strictWrite(o, v) { "use strict"; o.w = v; return o.w; }
function strictAdd(o, v) { "use strict"; o.extra2 = v; return o.extra2; }
var errs = [];
for (var r = 0; r < 20; r++) {
  var subjects = [open[1], frozen[1], sealed[1], closed[1], lookalike];
  for (var j = 0; j < subjects.length; j++) {
    try { strictWrite(subjects[j], r); errs.push("w ok"); } catch (e) { errs.push("w " + e.constructor.name); }
    try { strictAdd(subjects[j], r); errs.push("a ok"); } catch (e) { errs.push("a " + e.constructor.name); }
  }
}
log("strict", errs.slice(0, 10).join(), errs.slice(-10).join());
var proto = Object.freeze({ inherited: 1, other: 2 });
function child() { return Object.create(proto); }
function setInherited(o, v) { o.inherited = v; return o.inherited + ":" + Object.keys(o).length; }
function setInheritedStrict(o, v) { "use strict"; try { o.inherited = v; return "set " + o.inherited; } catch (e) { return e.constructor.name; } }
var inh = [];
for (var r = 0; r < 20; r++) inh.push(setInherited(child(), r), setInheritedStrict(child(), r));
log("through a frozen prototype", inh.slice(0, 4).join(), inh.slice(-4).join());
var acc = Object.freeze({ _v: 3, get v() { return this._v * 2; }, set v(x) { log("setter ran", x); } });
var accRow = [];
for (var r = 0; r < 20; r++) { acc.v = r; accRow.push(acc.v); }
log("frozen accessors", accRow.join());
class Point { constructor(x, y) { this.x = x; this.y = y; Object.freeze(this); } sum() { return this.x + this.y; } }
var pts = [];
for (var r = 0; r < 30; r++) pts.push(new Point(r, r * 2));
var ps = 0;
for (var r = 0; r < 30; r++) { var p = pts[r]; p.x = 1000; ps += p.sum() + p.x; }
log("frozen instances", ps, Object.isFrozen(pts[0]));
var wide = {}, wider = {};
for (var k = 0; k < 64; k++) wide["k" + k] = k;
for (var k = 0; k < 65; k++) wider["k" + k] = k;
Object.freeze(wide); Object.freeze(wider);
var ws = 0;
for (var r = 0; r < 30; r++) { ws += wide.k63 + wider.k64; wide.k1 = -1; wider.k1 = -1; }
log("wide", ws, wide.k1, wider.k1, Object.isFrozen(wide), Object.isFrozen(wider));
var later = make(5);
var site = [];
for (var r = 0; r < 10; r++) { if (r === 5) Object.freeze(later); site.push(writeW(later, r)); }
log("frozen mid-loop", site.join());
console.log(out.join("\n"));
"##;

/// The same refusals at sites hot enough to compile, after 20,000 calls with
/// open objects and with a frozen object's extensible look-alike.
const HOT: &str = r##"
"use strict";
function mk(i) { return { id: i, w: i }; }
function add(o, v) { o.extra = v; return o.extra; }
function write(o, v) { o.w = v; return o.w; }
function readSum(o) { return o.id + o.w; }
let s = 0;
for (let i = 0; i < 20000; i++) { s = (s + add(mk(i), i) + write(mk(i), i + 1) + readSum(mk(i))) | 0; }
const look = {};
Object.defineProperty(look, "id", { value: 1, enumerable: true });
Object.defineProperty(look, "w", { value: 2, enumerable: true });
for (let i = 0; i < 20000; i++) { s = (s + readSum(look) + readSum(Object.freeze(mk(i & 1023)))) | 0; }
const res = [];
for (const o of [Object.freeze(mk(1)), Object.seal(mk(2)), Object.preventExtensions(mk(3)), look]) {
  try { res.push("add " + add(o, 5)); } catch (e) { res.push("add " + e.constructor.name); }
  try { res.push("write " + write(o, 6)); } catch (e) { res.push("write " + e.constructor.name); }
  res.push("read " + readSum(o) + " keys " + Object.keys(o).join("/"));
}
console.log(s, res.join(" | "));"##;

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
fn frozen_and_sealed_objects_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn frozen_objects_at_hot_sites_match_node() {
    let out = zipp_vm::run(HOT).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(HOT));
}
