//! Computed stores into arrays: the present elements of an ordinary array
//! take a direct store, and everything that must still reach the general
//! path does (holes, stores past the end, frozen, sealed and non-extensible
//! arrays, defineProperty'd and accessor indices, mapped and strict
//! arguments objects, custom prototypes and index setters on
//! Array.prototype, sparse arrays, a non-writable length), in sloppy and
//! strict code. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function fill(a, k, v) { a[k] = v; return a; }
function fillStrict(a, k, v) { "use strict"; a[k] = v; return a; }
function attempt(label, f) { try { log(label, "ok", JSON.stringify(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
var a = [1, 2, 3, 4];
for (var r = 0; r < 20; r++) { fill(a, r & 3, r); a[(r + 1) & 3]++; }
log("plain", JSON.stringify(a), a.length);
var tmp = new Array(16), src = [];
for (var i = 0; i < 16; i++) src.push((i * 7) % 16);
for (var r = 0; r < 4; r++) { for (var k = 0, j = 15; k < 16; k++, j--) tmp[k] = src[j]; for (var k = 0; k < 16; k++) src[k] = tmp[k]; }
log("copy loop", src.join(","));
var holes = [1, , 3];
fill(holes, 1, "filled"); fill(holes, 5, "far");
log("holes", JSON.stringify(holes), holes.length, 4 in holes);
var frozen = Object.freeze([1, 2, 3]);
fill(frozen, 0, 9);
log("frozen sloppy", JSON.stringify(frozen));
attempt("frozen strict", function () { return fillStrict(frozen, 1, 9); });
var sealed = Object.seal([1, 2, 3]);
fill(sealed, 0, 9); fill(sealed, 3, 9);
log("sealed", JSON.stringify(sealed));
attempt("sealed strict add", function () { return fillStrict(sealed, 3, 9); });
var closed = Object.preventExtensions([1, 2, 3]);
fill(closed, 2, 7); fill(closed, 3, 8);
log("non-extensible", JSON.stringify(closed));
var ro = [1, 2, 3];
Object.defineProperty(ro, 1, { value: "fixed", writable: false });
fill(ro, 1, "changed"); fill(ro, 0, "zero");
log("read-only index", JSON.stringify(ro));
attempt("read-only index strict", function () { return fillStrict(ro, 1, "x"); });
var seen = [];
var acc = [1, 2, 3];
Object.defineProperty(acc, 2, { get: function () { return "got"; }, set: function (v) { seen.push(v); }, configurable: true });
for (var r = 0; r < 3; r++) fill(acc, 2, r);
log("accessor index", acc[2], seen.join());
function mapped(x, y) { arguments[0] = "via arguments"; arguments[1] = "second"; return x + "|" + y + "|" + arguments.length; }
function strictArgs(x) { "use strict"; arguments[0] = "changed"; return x + "|" + arguments[0]; }
log("arguments", mapped(1, 2), strictArgs(1));
var protoSeen = [];
var custom = [1, , 3];
Object.setPrototypeOf(custom, Object.create(Array.prototype, { 1: { set: function (v) { protoSeen.push(v); }, configurable: true } }));
fill(custom, 1, "hole write"); fill(custom, 0, "own write");
log("custom prototype", JSON.stringify(custom), protoSeen.join());
Object.defineProperty(Array.prototype, 7, { set: function (v) { protoSeen.push("proto7 " + v); }, configurable: true });
var withSeven = [0, 1, 2, 3, 4, 5, 6, 7, 8];
fill(withSeven, 7, "own seven");
var shortOne = [0];
fill(shortOne, 7, "absent seven");
delete Array.prototype[7];
log("Array.prototype setter", withSeven[7], shortOne.length, protoSeen.join());
var sparse = [];
sparse[100000] = 1;
fill(sparse, 100000, 2); fill(sparse, 5, 3);
log("sparse", sparse[100000], sparse[5], sparse.length);
var fixedLen = [1, 2, 3];
Object.defineProperty(fixedLen, "length", { writable: false });
fill(fixedLen, 1, "in range"); fill(fixedLen, 3, "past");
log("non-writable length", JSON.stringify(fixedLen), fixedLen.length);
var named = [1, 2];
named.foo = "bar";
fill(named, 0, "after named"); fill(named, "1", "string key"); fill(named, 1.0, "double key");
log("named and keys", JSON.stringify(named), named.foo);
var ta = new Uint8Array(4);
fill(ta, 1, 300); fill(ta, 9, 1);
log("typed", Array.from(ta).join(","));
var obj = { 0: "a", length: 1 };
fill(obj, 0, "b"); fill(obj, 1, "c");
log("object", JSON.stringify(obj));
var nested = [[0, 0], [0, 0]];
for (var r = 0; r < 10; r++) nested[r & 1][(r >> 1) & 1] += r;
log("nested", JSON.stringify(nested));
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
fn array_index_stores_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
