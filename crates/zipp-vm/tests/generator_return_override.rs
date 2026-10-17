//! IteratorClose of a generator calls the `return` a Get finds: an own
//! method, a replaced `%GeneratorPrototype%.return`, or none when it was set
//! to undefined, and a non-callable `return` or a primitive result is a
//! TypeError. The engine used to resume every generator with its intrinsic
//! `return`, so an override was never called and the body's `finally` ran.
//! Covers for-of `break`, destructuring, `Array.from`'s close on a throwing
//! mapper, an iterator helper's `return()` and `yield*` delegation, in
//! Node's order.

const SOURCE: &str = r##"
var log = [];
function* g() { try { yield 1; yield 2; yield 3; } finally { log.push("finally"); } }
function own() { var it = g(); it.return = function (v) { log.push("own"); return {}; }; return it; }
for (var x of own()) { break; }
var [a] = own();
try { Array.from(own(), function () { throw 0; }); } catch (e) {}
var h = own().map(function (v) { return v; }); h.next(); h.return();
for (var y of g()) { break; }
var proto = Object.getPrototypeOf(g.prototype);
var saved = proto.return;
proto.return = function () { log.push("proto"); return {}; };
for (var z of g()) { break; }
proto.return = saved;
var it2 = own(); it2.return = undefined; for (var w of it2) { break; }
var it3 = own(); it3.return = 5; try { for (var q of it3) { break; } } catch (e) { log.push(e.constructor.name); }
var it4 = own(); it4.return = function () { return 1; }; try { for (var q of it4) { break; } } catch (e) { log.push(e.constructor.name); }
function* outer() { yield* own(); }
var o = outer(); o.next(); o.return();
console.log(log.join(","));
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
fn generator_return_override_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}