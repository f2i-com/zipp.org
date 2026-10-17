//! Private FIELD writes store into a present entry with no other check, and
//! every write that must still fail does: to foreign objects, proxies,
//! primitives, a subclass's class value for a static field, and an instance
//! before the field is initialized; while frozen and sealed instances,
//! subclass instances and return-override objects still take plain,
//! compound and update writes. The expected lines are Node's (which adds
//! private fields to non-extensible objects, so those cases are not here).

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
class A {
  #x = 0;
  static #s = 0;
  #m() { return "method"; }
  get #acc() { return "got"; }
  static writeX(o, v) { o.#x = v; return o.#x; }
  static bumpX(o) { o.#x += 2; o.#x++; ++o.#x; o.#x **= 2; return o.#x; }
  static logicX(o) { o.#x ||= 7; o.#x &&= o.#x + 1; o.#x ??= 99; return o.#x; }
  static writeS(o, v) { o.#s = v; return o.#s; }
  static writeM(o) { o.#m = 1; }
  static writeAcc(o) { o.#acc = 1; }
}
class B extends A { #y = 1; static writeY(o, v) { o.#y = v; return o.#y; } }
var a = new A(), b = new B();
for (var r = 0; r < 20; r++) A.writeX(r & 1 ? a : b, r);
log("hot writes", A.writeX(a, "a"), A.writeX(b, "b"));
attempt("compound", function () { return A.bumpX(new A()); });
attempt("logical", function () { return A.logicX(new A()); });
attempt("subclass instance", function () { return A.bumpX(b) + "/" + B.writeY(b, "y"); });
attempt("subclass field on base", function () { return B.writeY(a, 1); });
attempt("plain object", function () { return A.writeX({}, 1); });
attempt("lookalike", function () { return A.writeX({ "#x": 0 }, 1); });
attempt("proxy", function () { return A.writeX(new Proxy(a, {}), 1); });
attempt("compound proxy", function () { return A.bumpX(new Proxy(a, {})); });
attempt("primitive", function () { return A.writeX(5, 1); });
attempt("undefined", function () { return A.writeX(undefined, 1); });
attempt("static on class", function () { return A.writeS(A, "s"); });
attempt("static on subclass", function () { return A.writeS(B, "s"); });
attempt("static on instance", function () { return A.writeS(a, "s"); });
attempt("method write", function () { return A.writeM(a); });
attempt("getter-only write", function () { return A.writeAcc(a); });
attempt("method write foreign", function () { return A.writeM({}); });
var frozen = Object.freeze(new A());
attempt("frozen instance", function () { return A.writeX(frozen, "cold") + "/" + A.bumpX(Object.freeze(new A())); });
var sealed = Object.seal(new B());
attempt("sealed subclass", function () { return B.writeY(sealed, "z") + "/" + A.writeX(sealed, "w"); });
class Base { constructor(o) { return o; } }
class Stamp extends Base { #tag = 1; static write(o, v) { o.#tag = v; o.#tag += "!"; return o.#tag; } }
var target = { plain: true };
new Stamp(target);
attempt("return override", function () { return Stamp.write(target, "stamped"); });
attempt("return override other", function () { return Stamp.write({ plain: true }, 1); });
class Early { #a = this.poke(); #b = "b"; poke() { try { this.#b = "early"; return "wrote"; } catch (e) { return "early " + e.constructor.name; } } static read(o) { return o.#a + "|" + o.#b; } }
attempt("before initialization", function () { return Early.read(new Early()); });
function make() { return class { #v = 0; static write(o, v) { o.#v = v; return o.#v; } }; }
var K1 = make(), K2 = make();
attempt("other evaluation", function () { return K1.write(new K2(), 1); });
attempt("same evaluation", function () { return K1.write(new K1(), "fresh"); });
var mixed = [];
for (var r = 0; r < 12; r++) { try { mixed.push(A.writeX([a, b, {}, new Proxy(b, {})][r & 3], r)); } catch (e) { mixed.push(e.constructor.name); } }
log("one site, many receivers", mixed.join());
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
fn private_field_writes_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
