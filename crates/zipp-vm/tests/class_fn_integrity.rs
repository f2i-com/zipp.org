//! Integrity levels and own accessors on class and function values: a
//! frozen or sealed class's statics take the frozen or sealed descriptors
//! (and refuse writes, adds and deletes accordingly), a non-extensible
//! class takes no new static, and an own accessor on a function governs
//! assignment (its setter runs; without one the write is rejected) instead
//! of being overwritten. The expected lines are Node's, in strict and
//! sloppy code.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }
var d = function (o, k) { return JSON.stringify(Object.getOwnPropertyDescriptor(o, k)); };
function strict(f) { "use strict"; return catchName(f); }

class F { static x = 1; static m() { return 1; } }
Object.freeze(F);
log("frozen", strict(function () { "use strict"; F.x = 2; }), catchName(function () { F.x = 3; }), F.x,
  strict(function () { "use strict"; F.y = 1; }), F.y, d(F, "x"), d(F, "m"), Object.isFrozen(F),
  strict(function () { "use strict"; return delete F.x; }), delete F.m, typeof F.m);
class S { static x = 1; }
Object.seal(S);
S.x = 2;
log("sealed", S.x, strict(function () { "use strict"; S.y = 1; }), S.y, d(S, "x"), Object.isSealed(S), Object.isFrozen(S),
  strict(function () { "use strict"; return delete S.x; }), S.x);
class NE { static x = 1; }
Object.preventExtensions(NE);
NE.x = 2;
NE.y = 3;
log("no-extend", NE.x, NE.y, strict(function () { "use strict"; NE.z = 1; }), Object.isExtensible(NE), delete NE.x, NE.x);
class D {}
Object.defineProperty(D, "pinned", { value: 1, writable: true, configurable: false });
log("non-configurable", delete D.pinned, D.pinned, strict(function () { "use strict"; return delete D.pinned; }));

function fn() {}
var setLog = [];
Object.defineProperty(fn, "getOnly", { get: function () { return "g"; }, configurable: true });
Object.defineProperty(fn, "both", { get: function () { return "b"; }, set: function (v) { setLog.push(v + ":" + (this === fn)); }, configurable: true });
fn.getOnly = 1;
fn.both = 2;
log("fn-accessors", fn.getOnly, strict(function () { "use strict"; fn.getOnly = 1; }), fn.getOnly, fn.both,
  strict(function () { "use strict"; fn.both = 3; }), setLog.join());
var child = Object.create(fn);
child.both = 4;
log("inherited-setter", setLog.join(), Object.hasOwn(child, "both"));
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
fn class_fn_integrity_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
