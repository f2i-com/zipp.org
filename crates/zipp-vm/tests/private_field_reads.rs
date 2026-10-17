//! Private FIELD reads answer from the field table alone: present on the
//! instance (or class value) for the declaring brand, or the TypeError.
//! Every receiver the brand check used to turn away must still be: foreign
//! objects, proxies, primitives, a subclass's class value for a static
//! field, an instance mid-construction before the field exists; and every
//! one it let through must still read: subclass instances, return-override
//! objects, frozen instances, static fields on the declaring class. The
//! expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
class A {
  #x = "a-x";
  static #s = "static-s";
  #m() { return "method"; }
  static readX(o) { return o.#x; }
  static readS(o) { return o.#s; }
  static callM(o) { return o.#m(); }
}
class B extends A { #y = "b-y"; static readY(o) { return o.#y; } }
var a = new A(), b = new B();
var rows = [];
for (var r = 0; r < 20; r++) rows.push(A.readX(r & 1 ? a : b));
log("hot reads", rows.slice(0, 4).join());
attempt("own", function () { return A.readX(a); });
attempt("subclass instance", function () { return A.readX(b) + "/" + B.readY(b); });
attempt("subclass field on base", function () { return B.readY(a); });
attempt("plain object", function () { return A.readX({}); });
attempt("lookalike", function () { return A.readX({ "#x": "public" }); });
attempt("proxy", function () { return A.readX(new Proxy(a, {})); });
attempt("primitive", function () { return A.readX(5); });
attempt("null", function () { return A.readX(null); });
attempt("static on class", function () { return A.readS(A); });
attempt("static on subclass", function () { return A.readS(B); });
attempt("static on instance", function () { return A.readS(a); });
attempt("method on foreign", function () { return A.callM({}); });
attempt("method own", function () { return A.callM(b); });
class Base { constructor(o) { return o; } }
class Stamp extends Base { #tag = "stamped"; static read(o) { return o.#tag; } }
var target = { plain: true };
new Stamp(target);
attempt("return override", function () { return Stamp.read(target); });
attempt("return override other", function () { return Stamp.read({ plain: true }); });
class Early { #a = this.peek(); #b = "b"; peek() { try { return this.#b; } catch (e) { return "early " + e.constructor.name; } } static read(o) { return o.#a + "|" + o.#b; } }
attempt("before initialization", function () { return Early.read(new Early()); });
var frozen = Object.freeze(new A());
attempt("frozen instance", function () { return A.readX(frozen); });
function make() { return class { #v = Math.random() > 2 ? 0 : "fresh"; static read(o) { return o.#v; } }; }
var K1 = make(), K2 = make();
attempt("other evaluation", function () { return K1.read(new K2()); });
attempt("same evaluation", function () { return K1.read(new K1()); });
var mixed = [];
for (var r = 0; r < 12; r++) { try { mixed.push(A.readX([a, b, {}, new Proxy(b, {})][r & 3])); } catch (e) { mixed.push(e.constructor.name); } }
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
fn private_field_reads_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
