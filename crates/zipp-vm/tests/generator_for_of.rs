//! Generators stepped by `for (v of g)`.
//!
//! The loop takes each step without the `{value, done}` result object
//! (`Vm::generator_next_step`), which it used to build, read back and drop.
//! A `yield*` step still hands over the inner iterator's own result object.
//! This program heats each shape first and then checks: plain and empty
//! generators, `yield*` over a generator, an array, a string and a custom
//! iterator whose results carry extra properties, `break` closing the
//! generator through `finally`, a throw from the body, a `finally` that
//! yields, a generator re-entered from its own body, one resumed by `for-of`
//! after manual steps, a completed one, sent values, `this` and `arguments`,
//! destructuring and spread, and the shape of an explicit `next()` result.
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

const PROGRAM: &str = r####"// Differential probe: generators stepped by for-of, hot, with every completion kind.
var out = [];
function log(label, v) { out.push(label + ": " + v); }
function heat(f) { for (var i = 0; i < 300; i++) f(i); }

function* range(n) { for (var i = 0; i < n; i++) yield i; return "end"; }
function sumOf(it) { var s = 0; for (var v of it) s += v; return s; }
heat(function () { sumOf(range(20)); });
log("plain", sumOf(range(10)) + " " + sumOf(range(0)));

// yield* hands over the inner iterator's own results.
function* inner() { yield 1; yield 2; return "inner-done"; }
function* outer() { var r = yield* inner(); yield r; yield* [3, 4]; yield* "ab"; }
heat(function () { [...outer()]; });
var seen = []; for (var v of outer()) seen.push(v);
log("yield*", seen.join(","));
var custom = { [Symbol.iterator]() { var n = 0; return { next() { n++; return n < 3 ? { value: n * 10, done: false, extra: 1 } : { done: true, value: "ignored" }; } }; } };
function* viaCustom() { var r = yield* custom; yield "r=" + r; }
heat(function () { [...viaCustom()]; });
seen = []; for (var v of viaCustom()) seen.push(v);
log("yield* custom", seen.join(","));

// break and return close the generator; finally blocks run; a throw propagates.
var log2 = [];
function* guarded() { try { yield 1; yield 2; yield 3; } finally { log2.push("finally"); } }
heat(function () { for (var v of guarded()) { if (v === 2) break; } });
log2.length = 0;
for (var v of guarded()) { if (v === 2) break; }
var g = guarded(); for (var v of g) { if (v === 1) break; }
log("break closes", log2.join(",") + " " + JSON.stringify(g.next()));
function* thrower() { yield 1; throw new RangeError("boom"); }
var caught;
try { for (var v of thrower()) {} } catch (e) { caught = e.name + ":" + e.message; }
log("throw", caught);
function* finallyYields() { try { yield 1; } finally { yield "from finally"; } }
seen = []; for (var v of finallyYields()) { seen.push(v); }
log("finally yields", seen.join(","));

// A generator already running, re-entered from its own body; a generator stepped manually between.
var self;
function* reentrant() { try { for (var v of self) {} } catch (e) { yield e.name; } }
self = reentrant();
log("reentrant", JSON.stringify(self.next()));
var m = range(5); m.next(); m.next();
log("resumed by for-of", sumOf(m));
var done = range(2); for (var v of done) {}
log("completed", sumOf(done) + " " + JSON.stringify(done.next()));

// Sent values are undefined in for-of; `this` and arguments in the generator.
function* echo() { var got = []; for (var i = 0; i < 3; i++) got.push(yield i); return got; }
var e = echo(); for (var v of e) {}
log("sent", JSON.stringify(e.next()));
var obj = { k: 7, *gen(a, b) { yield this.k; yield arguments.length; yield a + b; } };
seen = []; for (var v of obj.gen(1, 2)) seen.push(v);
log("this args", seen.join(","));

// Destructuring and spread from generators.
var [a, , b, ...rest] = range(6);
log("destructure", a + " " + b + " " + rest.join(","));
log("spread", Math.max(...range(7)) + " " + [...range(3), ...range(2)].join(","));

// Patched prototype `next` is observed by an explicit call, and the result object shape.
var GenProto = Object.getPrototypeOf(range(0));
var r1 = range(2).next();
log("result object", JSON.stringify(r1) + " " + Object.keys(r1).join(",") + " " + (Object.getPrototypeOf(r1) === Object.prototype));

console.log(out.join("\n"));
"####;

#[test]
fn generators_in_for_of_match_node() {
    let expected: &[&str] = &[
        r####"plain: 45 0"####,
        r####"yield*: 1,2,inner-done,3,4,a,b"####,
        r####"yield* custom: 10,20,r=ignored"####,
        r####"break closes: finally,finally {"done":true}"####,
        r####"throw: RangeError:boom"####,
        r####"finally yields: 1,from finally"####,
        r####"reentrant: {"value":"TypeError","done":false}"####,
        r####"resumed by for-of: 9"####,
        r####"completed: 0 {"done":true}"####,
        r####"sent: {"done":true}"####,
        r####"this args: 7,2,3"####,
        r####"destructure: 0 2 3,4,5"####,
        r####"spread: 6 0,1,2,0,1"####,
        r####"result object: {"value":0,"done":false} value,done true"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
