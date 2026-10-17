//! Private member access remembered per access site and class chain: the
//! same site reached from different class evaluations (fresh brands each
//! time), a name an inner class shadows, private methods, accessors and
//! statics, foreign receivers that must still throw, and classes made and
//! dropped by the thousand so the collector frees them between accesses.
//! The expected lines are Node's.

const SOURCE: &str = r#"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}

class Base {
  #id; #tag = 3;
  static #count = 0;
  constructor(id) { this.#id = id; Base.#count++; }
  get id() { return this.#id; }
  bump(n) { this.#tag = (this.#tag + n) & 1023; return this.#tag; }
  static count() { return Base.#count; }
  static has(o) { return #tag in o; }
  peek(o) { return o.#tag; }
}
class Mid extends Base {
  #x = 1; #v;
  constructor(id, v) { super(id); this.#v = v; }
  #step() { this.#x += this.#v; return this.#x; }
  get #twice() { return this.#x * 2; }
  set #twice(v) { this.#x = v >> 1; }
  tick() { this.#step(); this.#twice = this.#twice + 2; return this.bump(this.#x) + this.#x; }
}
class Leaf extends Mid {
  #x = 100;
  tick() { this.#x++; return super.tick() + this.#x; }
  own() { return this.#x; }
}
var objs = [];
for (var i = 0; i < 40; i++) objs.push(i % 3 ? new Leaf(i, i % 5) : new Mid(i, 2));
var h = 0;
for (var r = 0; r < 30; r++) for (var i = 0; i < objs.length; i++) h = (h * 31 + objs[i].tick()) | 0;
log("hierarchy", h, Base.count(), objs[4].own(), objs[3].id, Base.has(objs[5]), Base.has({}));
attempt("peek foreign", function () { return objs[0].peek({}); });
attempt("peek own", function () { return objs[0].peek(objs[1]); });
attempt("leaf own on mid", function () { return Leaf.prototype.own.call(objs[0]); });

// A class expression per call: the same sites, a new brand every time.
function makeCell(initial) {
  return class {
    #v = initial;
    static #made = 0;
    constructor() { this.constructor.bump(); }
    static bump() { return ++this.#made; }
    static made() { return this.#made; }
    get v() { return this.#v; }
    swap(o) { var t = this.#v; this.#v = o.#v; o.#v = t; }
    static has(o) { return #v in o; }
  };
}
var total = 0, crossed = 0, keep = [];
for (var round = 0; round < 3000; round++) {
  var A = makeCell(round), B = makeCell(-round);
  var a1 = new A(), a2 = new A(), b = new B();
  a1.swap(a2);
  total = (total + a1.v + a2.v + A.made() + (A.has(b) ? 1000 : 0) + (B.has(b) ? 1 : 0)) | 0;
  try { a1.swap(b); } catch (e) { crossed++; }
  var junk = [];
  for (var j = 0; j < 40; j++) junk.push({ j: j, s: "x" + j });
  if (round % 500 === 0) keep.push(a1);
}
log("cells", total, crossed, keep.length, keep[3].v);

// An inner class shadowing an outer private name, and reaching the outer one.
class Outer {
  #x = "outer";
  probe() {
    var self = this;
    class Inner {
      #x = "inner";
      read(o) { return o.#x; }
      readOuter() { return self.#y(); }
    }
    var inner = new Inner();
    var r = [inner.read(inner), inner.readOuter()];
    try { inner.read(self); r.push("no"); } catch (e) { r.push(e.constructor.name); }
    return r.join(",");
  }
  #y() { return this.#x; }
}
var o = new Outer();
log("shadow", o.probe(), o.probe());
attempt("private method assign", function () { class K { #m() {} f() { this.#m = 1; } } new K().f(); });
attempt("getter only", function () { class K { get #g() { return 1; } f() { this.#g = 2; } } new K().f(); });
attempt("setter only", function () { class K { set #s(v) {} f() { return this.#s; } } return new K().f(); });
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
fn private_access_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
