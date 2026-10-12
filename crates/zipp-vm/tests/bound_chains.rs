//! Calling and constructing chains of bound functions.
//!
//! A bind-of-bind chain is unwrapped in one loop (`Vm::unwrap_bound_call`,
//! and the bound arm of [[Construct]]) instead of one native recursion a
//! level, which ran a 300-deep chain past the checked profile's native
//! recursion cap of 32 (a RangeError where Node calls it). The lines pin the
//! `this` and argument order through a chain, 3,000- and 2,000-deep call and
//! construct chains, `new.target` and prototypes through bound constructors,
//! `Reflect.construct` with a bound target and a bound newTarget, bound arrow
//! functions, and a throw from the end of a chain.
//!
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Differential probe: calling and constructing chains of bound functions.
var out = [];
function log(label, v) { out.push(label + ": " + v); }

function show() { return [this && this.tag, Array.prototype.join.call(arguments, ",")].join("|"); }
var f1 = show.bind({ tag: "inner" }, 1, 2);
var f2 = f1.bind({ tag: "outer" }, 3);
var f3 = f2.bind(null, 4, 5);
log("call chain", f3(6) + " " + f2() + " " + f1.call({ tag: "x" }, 9) + " " + f3.apply({ tag: "y" }, [7, 8]));

var deep = function base() { return this === undefined ? "u" : typeof this; };
for (var i = 0; i < 3000; i++) deep = deep.bind(i === 0 ? "s" : null, i);
log("deep call", deep() + " " + deep.length + " " + deep.name.length);
var adder = function () { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s; };
for (var i = 0; i < 400; i++) adder = adder.bind(null, i);
log("deep args", adder(1000) + " " + adder.length);

function Point(x, y) { this.x = x; this.y = y; this.nt = new.target === Point ? "Point" : String(new.target && new.target.name); }
var P1 = Point.bind({ ignored: true }, 1);
var P2 = P1.bind(null);
var p = new P2(2);
log("construct chain", JSON.stringify(p) + " " + (p instanceof Point) + " " + (p instanceof P2) + " " + (Object.getPrototypeOf(p) === Point.prototype));
var deepCtor = Point;
for (var i = 0; i < 2000; i++) deepCtor = deepCtor.bind(null);
var q = new deepCtor(3, 4);
log("deep construct", JSON.stringify(q));
class Base { constructor(a, b) { this.a = a; this.b = b; this.nt = new.target.name; } }
var B2 = Base.bind(null, "x").bind(null, "y");
var ext;
try { (class Sub extends B2 {}); ext = "no throw"; } catch (e) { ext = e.name; }
var s = new B2();
log("class chain", JSON.stringify(s) + " " + (s instanceof Base) + " " + ext);
function Other() {}
Other.prototype.mark = "other";
var r = Reflect.construct(P2, [5], Other);
log("reflect newTarget", JSON.stringify(r) + " " + r.mark + " " + (Object.getPrototypeOf(r) === Other.prototype));
var r2 = Reflect.construct(Point, [6, 7], P2);
log("bound as newTarget", JSON.stringify(r2) + " " + (Object.getPrototypeOf(r2) === Point.prototype));
try { new (function () {}.bind(null)).bind(null)(); log("arrow ok", "constructed"); } catch (e) { log("arrow ok", e.name); }
try { new ((() => 1).bind(null).bind(null))(); log("arrow ctor", "no throw"); } catch (e) { log("arrow ctor", e.name); }
var thrower = function () { throw new RangeError("t" + arguments.length); }.bind(null, 1).bind(null, 2);
try { thrower(3); } catch (e) { log("throw", e.message); }
console.log(out.join("\n"));
"####;

#[test]
fn bound_chains_match_node() {
    let expected: &[&str] = &[
        r####"call chain: inner|1,2,3,4,5,6 inner|1,2,3 inner|1,2,9 inner|1,2,3,4,5,7,8"####,
        r####"deep call: object 0 18004"####,
        r####"deep args: 80800 0"####,
        r####"construct chain: {"x":1,"y":2,"nt":"Point"} true true true"####,
        r####"deep construct: {"x":3,"y":4,"nt":"Point"}"####,
        r####"class chain: {"a":"x","b":"y","nt":"Base"} true TypeError"####,
        r####"reflect newTarget: {"x":1,"y":5,"nt":"Other"} other true"####,
        r####"bound as newTarget: {"x":6,"y":7,"nt":"bound bound Point"} false"####,
        r####"arrow ok: TypeError"####,
        r####"arrow ctor: TypeError"####,
        r####"throw: t3"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
