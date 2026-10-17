//! Direct evals repeated at one site: a declaration-free program compiled
//! once re-runs over each activation's own bindings, and everything that
//! must not be shared (closures, `this`, `new.target`, `arguments`, template
//! objects, declarations, errors) behaves as a fresh eval. The expected lines
//! are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}

// Each activation's own locals, read and written.
function scoped(a, b) {
  let c = a ^ b, s = 0;
  for (let i = 0; i < 4; i++) s = (s + eval("a + b * " + (i & 1) + " - c")) | 0;
  eval("c = c + 1");
  return [s, c, eval("i0 = typeof i")].join("/");
}
var row = [];
for (var i = 0; i < 6; i++) row.push(scoped(i * 3, 7 - i));
log("scoped", row.join(" "), typeof i0);

// Closures made by a repeated eval are distinct and capture their activation.
function maker(v) { return eval("(function () { return v * 2; })"); }
var f1 = maker(1), f2 = maker(5), f3 = maker(1);
log("closures", f1(), f2(), f3(), f1 === f3, f1 === maker(1));

// this, new.target and arguments are the caller's, call by call.
function who() { return eval("this && this.name"); }
log("this", who.call({ name: "x" }), who.call({ name: "y" }), who.call(null) === undefined || who.call(null) === "");
function Ctor() { this.t = eval("new.target === Ctor"); }
function plain() { return eval("new.target === undefined"); }
log("new.target", new Ctor().t, plain(), new Ctor().t);
function args() { return eval("arguments.length + ':' + arguments[0]"); }
log("arguments", args(1, 2), args("a"), args());

// A binding still in its temporal dead zone, then initialised.
function tdz() {
  var r = [];
  for (var k = 0; k < 2; k++) {
    try { r.push(eval("late")); } catch (e) { r.push(e.constructor.name); }
  }
  let late = 3;
  r.push(eval("late"));
  return r.join(",");
}
log("tdz", tdz(), tdz());

// with-scoped lookups follow the current object.
var objs = [{ p: 1 }, { p: 2 }, { q: 3 }];
row = [];
for (var j = 0; j < objs.length; j++) {
  var p = "outer";
  with (objs[j]) { row.push(eval("p")); }
}
log("with", row.join(","));

// Declarations still declare on every run.
function declares(n) {
  var r = [];
  for (var k = 0; k < n; k++) { eval("var dv = " + 5 + "; function df() { return dv + " + k + "; }"); r.push(dv + "/" + df()); }
  return r.join(",");
}
log("declares", declares(3), typeof dv);
function strictDecl() { "use strict"; eval("var sv = 1"); return typeof sv; }
log("strict", strictDecl(), strictDecl());
var gcount = 0;
for (var k = 0; k < 3; k++) eval("var gdecl = " + k + "; gcount++");
log("global decl", gdecl, gcount);

// Errors are thrown every time; a global defined later is seen.
function bad() { return eval("1 +"); }
attempt("syntax 1", bad);
attempt("syntax 2", bad);
function readLate() { return eval("lateGlobal"); }
attempt("late 1", readLate);
globalThis.lateGlobal = 7;
attempt("late 2", readLate);
delete globalThis.lateGlobal;
attempt("late 3", readLate);
function thrower(x) { return eval("x.missing.deeper"); }
attempt("runtime 1", function () { return thrower({}); });
attempt("runtime 2", function () { return thrower({ missing: { deeper: 9 } }); });

// Each eval's tagged template is its own object.
function tag(s) { return s; }
function tmpl() { return eval("tag`x${1}y`"); }
var t1 = tmpl(), t2 = tmpl();
log("templates", t1 === t2, t1[0], t2.raw[1]);

// The same source at two sites, and at one site from two functions' caller scopes.
function siteA(z) { return eval("z + 1"); }
function siteB(z) { var w = 100; return eval("z + 1") + eval("w"); }
log("sites", siteA(1), siteB(1), siteA(10), siteB(10));
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
fn repeated_direct_evals_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
