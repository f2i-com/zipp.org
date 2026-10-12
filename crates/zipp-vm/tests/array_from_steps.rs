//! `Array.from` over a plain array, and the short-circuiting searches.
//!
//! `Array.from(array[, mapfn])` drove the array iterator protocol from the
//! outside: a `next` call, a result object and two property reads per
//! element, eight times what Node pays. For a real Array whose `@@iterator`
//! and iterator-prototype `next` are the intrinsics it now takes the
//! iterator's steps itself, on the live array (`Vm::array_from_dense`), and
//! calls a compiled mapping function over one reused window; the array-like
//! path (`Array.from({ length: n }, f)`) and `some`, `every`, `find` and
//! `findIndex` use the same window.
//!
//! What the lines pin: holes read through the prototype, the index and
//! `this` arguments, a mapping function that grows, shrinks, rewrites or
//! redefines the source while it is being read, a throw that must close the
//! iterator (a `return` put on the iterator prototypes is still looked up
//! and called), patched `next` and own `@@iterator` (which take the driven
//! protocol), subclasses, array-likes, `arguments`, strings, Set, Map, typed
//! arrays and generators, and searches whose predicate throws or changes the
//! array.
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

const PROGRAM: &str = r####"// Differential probe: Array.from over arrays, array-likes and iterables, and the short-circuiting searches.
var out = [];
function log(label, v) { out.push(label + ": " + v); }
function show(a) { return a.map(function (x) { return x === undefined ? "u" : String(x); }).join(","); }
function heat(f) { for (var i = 0; i < 500; i++) f(); }

// 1. Plain arrays, with and without a mapping function; holes; index and this arguments.
var base = []; for (var i = 0; i < 40; i++) base.push((i * 7) % 11);
function copy() { return Array.from(base); }
function mapped(k) { return Array.from(base, function (x, i) { var t; if (x > 5) t = x * k + i; return t === undefined ? -1 : t; }); }
heat(copy); heat(function () { mapped(2); });
log("copy", show(copy().slice(0, 10)) + " " + (copy() !== base) + " " + Array.isArray(copy()));
log("mapped", show(mapped(2).slice(0, 10)) + " | " + show(mapped(10).slice(0, 6)));
log("holes", show(Array.from([1, , 3, , ])) + " " + show(Array.from([1, , 3], function (x, i) { return x === undefined ? "h" + i : x; })) + " " + (1 in Array.from([1, , 3])));
Array.prototype[1] = "proto";
log("hole reads prototype", show(Array.from([1, , 3])) + " " + show(Array.from([1, , 3], function (x) { return x + "!"; })));
delete Array.prototype[1];
var ctx = { k: 5 };
log("this and arity", show(Array.from([1, 2, 3], function (x, i) { return this.k + x + i + arguments.length; }, ctx)) + " " + show(Array.from([1, 2], function () { "use strict"; return this === undefined ? "u" : "o"; })));

// 2. The mapping function changes the source: the iterator reads the live array.
function grow() { var a = [1, 2, 3]; return Array.from(a, function (x, i) { if (i < 3) a.push(x * 10); return x; }); }
function shrink() { var a = [1, 2, 3, 4, 5, 6]; return Array.from(a, function (x, i) { if (i === 1) a.length = 3; return x; }); }
function swap() { var a = [1, 2, 3, 4]; return Array.from(a, function (x, i) { if (i === 0) { a[2] = "w"; delete a[3]; } return x; }); }
function accessorMid() { var a = [1, 2, 3, 4]; return Array.from(a, function (x, i) { if (i === 0) Object.defineProperty(a, 2, { get: function () { return "g"; }, configurable: true }); return x; }); }
function sparseMid() { var a = [1, 2, 3]; return Array.from(a, function (x, i) { if (i === 0) a[9] = 9; return x; }); }
heat(grow); heat(shrink); heat(swap);
log("live source", show(grow()) + " | " + show(shrink()) + " | " + show(swap()) + " | " + show(accessorMid()) + " | " + show(sparseMid()));

// 3. A throw from the mapping function closes the iterator: `return` is looked up and called.
var closes = [];
var ArrayIteratorPrototype = Object.getPrototypeOf([][Symbol.iterator]());
function thrower(limit) { try { return Array.from([1, 2, 3, 4], function (x, i) { if (i === limit) throw new RangeError("at " + i); return x; }).length; } catch (e) { return e.name + ":" + e.message; } }
heat(function () { thrower(-1); });
log("throw", thrower(2) + " " + thrower(-1));
ArrayIteratorPrototype.return = function () { closes.push("return called on " + Object.prototype.toString.call(this)); return {}; };
log("throw closes", thrower(1) + " " + closes.join(";") + " " + thrower(-1) + " " + closes.length);
delete ArrayIteratorPrototype.return;
Object.defineProperty(Object.prototype, "return", { get: function () { closes.push("getter"); return undefined; }, configurable: true });
log("throw reads return", thrower(0) + " " + closes.slice(1).join(";"));
delete Object.prototype.return;

// 4. Patched protocols take the full path and are honoured.
var origNext = ArrayIteratorPrototype.next, nexts = 0;
ArrayIteratorPrototype.next = function () { nexts++; return origNext.call(this); };
log("patched next", show(Array.from([1, 2, 3], function (x) { return x * 2; })) + " " + nexts);
ArrayIteratorPrototype.next = origNext;
function patchDuring() { var n = 0; var r = Array.from([1, 2, 3], function (x, i) { if (i === 0) ArrayIteratorPrototype.next = function () { n++; return { done: true }; }; return x; }); ArrayIteratorPrototype.next = origNext; return show(r) + " " + n; }
log("next patched during", patchDuring());
var own = [1, 2, 3]; own[Symbol.iterator] = function* () { yield "a"; yield "b"; };
log("own iterator", show(Array.from(own)) + " " + show(Array.from(own, function (x) { return x + x; })));
class Sub extends Array {}
var sub = Sub.from([1, 2, 3], function (x) { return x + 1; });
log("subclass", show(Array.from(sub)) + " " + (sub instanceof Sub) + " " + (Array.from(new Sub(2)) instanceof Sub) + " " + Sub.from([4, 5]).constructor.name);

// 5. Other sources: array-likes (with a compiled mapper), arguments, strings, Set, Map, typed arrays, generators.
function seq(n, k) { return Array.from({ length: n }, function (_, i) { var t; if (i & 1) t = i * k; return t === undefined ? 0 : t; }); }
heat(function () { seq(8, 2); });
log("array-like", show(seq(8, 2)) + " | " + show(seq(5, 10)) + " | " + show(Array.from({ length: 3, 0: "a", 2: "c" })) + " | " + Array.from({ length: "2", 0: 1, 1: 2 }, function (x) { return x * 3; }).join("+"));
function args() { return Array.from(arguments, function (x) { return x + 1; }); }
function argsMapped(a, b) { arguments[0] = 9; return Array.from(arguments); }
heat(function () { args(1, 2, 3); });
log("arguments", show(args(1, 2, 3)) + " " + show(argsMapped(1, 2)));
log("string set map", show(Array.from("a😀b")) + " " + show(Array.from(new Set([3, 1, 3]), function (x) { return x * 2; })) + " " + Array.from(new Map([[1, 2]]))[0].join("=") + " " + show(Array.from(new Float32Array([1.5, 2]), function (x) { return x * 2; })));
function* gen() { yield 1; yield 2; }
log("generator", show(Array.from(gen(), function (x, i) { return x * 10 + i; })));
try { Array.from([1], 5); log("bad mapfn", "no throw"); } catch (e) { log("bad mapfn", e.name); }
try { Array.from(null); log("null source", "no throw"); } catch (e) { log("null source", e.name); }

// 6. some / every / find / findIndex with compiled predicates, capturing and not.
function searches(arr, k) {
  var calls = 0;
  return [arr.some(function (x) { calls++; return x > k; }), arr.every(function (x) { calls++; return x >= 0; }), arr.find(function (x) { calls++; var t; if (x === k) t = true; return t; }), arr.findIndex(function (x) { calls++; return x === k + 1; }), calls].join(",");
}
heat(function () { searches(base, 3); });
log("searches", searches(base, 3) + " | " + searches(base, 100) + " | " + searches([], 1) + " | " + searches([1, , 3], 2));
function searchThrow(arr, at) { var seen = 0; try { return arr.every(function (x, i) { seen++; if (i === at) throw new TypeError("p" + i); return true; }); } catch (e) { return e.message + "@" + seen; } }
heat(function () { searchThrow(base, -1); });
log("search throws", searchThrow(base, 3) + " " + searchThrow(base, -1));
function searchMutate() { var a = [1, 2, 3, 4, 5]; var seen = []; a.some(function (x, i) { seen.push(x); if (i === 1) a.length = 3; return false; }); a.every(function (x, i) { if (i === 0) a.push(99); seen.push(x); return true; }); return seen.join(","); }
heat(searchMutate);
log("search mutates", searchMutate());

console.log(out.join("\n"));
"####;

#[test]
fn array_from_and_searches_match_node() {
    let expected: &[&str] = &[
        r####"copy: 0,7,3,10,6,2,9,5,1,8 true true"####,
        r####"mapped: -1,15,-1,23,16,-1,24,-1,-1,25 | -1,71,-1,103,64,-1"####,
        r####"holes: 1,u,3,u 1,h1,3 true"####,
        r####"hole reads prototype: 1,proto,3 1!,proto!,3!"####,
        r####"this and arity: 8,10,12 u,u"####,
        r####"live source: 1,2,3,10,20,30 | 1,2,3 | 1,2,w,u | 1,2,g,4 | 1,2,3,u,u,u,u,u,u,9"####,
        r####"throw: RangeError:at 2 4"####,
        r####"throw closes: RangeError:at 1 return called on [object Array Iterator] 4 1"####,
        r####"throw reads return: RangeError:at 0 getter"####,
        r####"patched next: 2,4,6 4"####,
        r####"next patched during: 1,2,3 0"####,
        r####"own iterator: a,b aa,bb"####,
        r####"subclass: 2,3,4 true false Sub"####,
        r####"array-like: 0,2,0,6,0,10,0,14 | 0,10,0,30,0 | a,u,c | 3+6"####,
        r####"arguments: 2,3,4 9,2"####,
        r####"string set map: a,😀,b 6,2 1=2 3,4"####,
        r####"generator: 10,21"####,
        r####"bad mapfn: TypeError"####,
        r####"null source: TypeError"####,
        r####"searches: true,true,3,10,56 | false,true,,-1,160 | false,true,,-1,0 | true,true,,2,10"####,
        r####"search throws: p3@4 true"####,
        r####"search mutates: 1,2,3,1,2,3"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
