//! `#x in obj` brand checks remembered per site: fields, methods, accessors
//! and static privates, on instances, foreign objects and look-alikes, from
//! one site shared by every evaluation of a class expression (each with its
//! own brand), from arrows and inner functions, against primitives and
//! proxies. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
class A {
  #f = 1;
  #m() { return 1; }
  get #g() { return 2; }
  static #s = 3;
  static #sm() {}
  static hasF(o) { return #f in o; }
  static hasM(o) { return #m in o; }
  static hasG(o) { return #g in o; }
  static hasS(o) { return #s in o; }
  static hasSm(o) { return #sm in o; }
  viaArrow(o) { return [1, 2].map(() => #f in o).join(); }
  viaInner(o) { function inner(x) { return #m in x; } return inner(o); }
}
class B extends A { #f = "b's own"; static hasBF(o) { return #f in o; } }
var a = new A(), b = new B(), plain = { f: 1, "#f": "public spelling" };
var rows = [];
for (var r = 0; r < 30; r++) {
  var subjects = [a, b, plain, A, B, Object.create(a), [], function () {}];
  for (var j = 0; j < subjects.length; j++) {
    var o = subjects[j];
    rows.push([A.hasF(o), A.hasM(o), A.hasG(o), A.hasS(o), A.hasSm(o), B.hasBF(o)].map(Number).join(""));
  }
}
log("matrix", rows.slice(0, 8).join(" "), rows.slice(-8).join(" "));
log("arrow and inner", a.viaArrow(a), a.viaArrow(plain), a.viaInner(b), a.viaInner({}));
function makeCell(v) {
  return class { #v = v; static has(o) { return #v in o; } static make() { return new this(); } };
}
var cells = [];
for (var i = 0; i < 6; i++) cells.push(makeCell(i));
var cross = [];
for (var r = 0; r < 20; r++) {
  for (var i = 0; i < cells.length; i++) {
    var mine = cells[i].make(), theirs = cells[(i + 1) % cells.length].make();
    cross.push(Number(cells[i].has(mine)) + "" + Number(cells[i].has(theirs)));
  }
}
log("per-evaluation brands", cross.slice(0, 6).join(), cross.slice(-6).join());
var prims = [1, "s", true, null, undefined, Symbol("x"), 10n];
var errs = [];
for (var r = 0; r < 3; r++) for (var i = 0; i < prims.length; i++) {
  try { A.hasF(prims[i]); errs.push("ok"); } catch (e) { errs.push(e.constructor.name); }
}
log("primitives", errs.slice(0, 7).join());
var px = new Proxy(a, {});
log("proxy", A.hasF(px), A.hasM(px), A.hasF(a));
var frozenLater = new A();
Object.freeze(frozenLater);
log("frozen instance", A.hasF(frozenLater), A.hasM(frozenLater));
var sym = Symbol("k");
function makeComputed() {
  return class {
    #f = "field";
    get #g() { return "getter"; }
    #m() { return "method"; }
    [sym](o) { return [o.#f, o.#g, o.#m()].join("/"); }
    ["str" + "Key"](o) { return o.#g; }
    *[Symbol.iterator]() { yield this.#f; yield this.#g; }
    get [sym.description + "Acc"]() { return this.#m(); }
    static [sym](o) { return #f in o; }
  };
}
var C1 = makeComputed(), C2 = makeComputed();
var c1 = new C1(), c2 = new C2();
var computedRows = [];
for (var r = 0; r < 10; r++) {
  computedRows.push(c1[sym](c1), c1.strKey(c1), [...c1].join("+"), c1.kAcc, C1[sym](c1), C1[sym](c2), C2[sym](c1));
  try { computedRows.push(c1[sym](c2)); } catch (e) { computedRows.push(e.constructor.name); }
  try { computedRows.push(C1.prototype[Symbol.iterator].call(c2).next().value); } catch (e) { computedRows.push(e.constructor.name); }
}
log("computed keys", computedRows.slice(0, 9).join(), computedRows.slice(-9).join());
var key = { toString: function () { log("toString ran"); return "f"; } };
log("ordinary in still coerces", key in plain, "#f" in plain, "#f" in a);
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

#[test]
fn private_brand_checks_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
