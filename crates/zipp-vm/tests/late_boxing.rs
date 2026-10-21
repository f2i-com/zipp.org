//! Bindings only directly returned closures capture are boxed at that
//! `return`, not at entry (`capture::late_box_names`), against Node.
//!
//! The probe covers early returns that allocate no cell, mutation through
//! the returned closure, returns inside loops and for-of, two return sites in
//! one function (the second used to reuse the first's cell snapshot), state
//! kept across calls, recursion, and Babel's loose for-of helper on arrays,
//! array-likes and Sets; then the cases that must stay boxed at entry: a
//! block-scoped shadow, a closure that is not returned, `arguments`
//! aliasing, a `finally` that writes after the return, and a parameter
//! default. A hot loop at the end lets a small program's JIT compile the
//! functions too. The expected lines are Node's (v24).

const PROBE: &str = r###""use strict0";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));

function earlyOrClosure(a, useClosure) {
  var b = a * 2;
  if (!useClosure) return b;
  return function () { return a + b; };
}
function mutating(a) {
  if (a < 0) return null;
  return () => ++a;
}
function inLoop(n) {
  var i = 0;
  while (true) {
    i++;
    if (i === n) return () => i * 10;
  }
}
function twoSites(x, y, pick) {
  var z = x + y;
  if (pick) return function () { return x + z; };
  return () => y * z;
}
function nested(a) {
  var v = a + 1;
  return function () {
    var inner = () => v * 2;
    v++;
    return inner() + a;
  };
}
function shadowed(x) {
  { let x = 5; return () => x; }
}
function earlyCapture() {
  var v = 1;
  var g = () => v;
  v = 2;
  return () => v + g();
}
function withArguments(a) {
  arguments[0] = 9;
  return () => a;
}
function withFinally() {
  var v = 1;
  try { return () => v; } finally { v = 2; }
}
function forOf(arr) {
  var v = 0;
  for (const x of arr) {
    v += x;
    if (v > 3) return () => v;
  }
  return null;
}
function rec(n) {
  if (n === 0) return () => n;
  var g = rec(n - 1);
  return () => n + g();
}
function withDefault(a, b = () => a) {
  return () => a + b();
}
function counter() {
  var c = 0;
  return () => ++c;
}
function reassigned() {
  var v = 1;
  v = 2;
  return () => v;
}
function closureArgs(p, q) {
  if (typeof p !== "number") return "not a number";
  return (r) => p * q + r;
}
function _unsupportedIterableToArray(r) {
  if (r && typeof r.length === "number") return Array.prototype.slice.call(r);
  return null;
}
function _createForOfIteratorHelperLoose(r, e) {
  var t2 = "undefined" != typeof Symbol && r[Symbol.iterator] || r["@@iterator"];
  if (t2) return (t2 = t2.call(r)).next.bind(t2);
  if (Array.isArray(r) || (t2 = _unsupportedIterableToArray(r)) || e) {
    t2 && (r = t2);
    var o = 0;
    return function () {
      return o >= r.length ? { done: true } : { done: false, value: r[o++] };
    };
  }
  throw new TypeError("Invalid attempt to iterate non-iterable instance.");
}
function collect(it) {
  const xs = [];
  for (var st; !(st = it()).done;) xs.push(st.value);
  return xs.join(",");
}

log("early", earlyOrClosure(3, false), earlyOrClosure(3, true)());
const m = mutating(5);
log("mutating", m(), m(), mutating(-1));
log("inLoop", inLoop(4)());
log("twoSites", twoSites(1, 2, true)(), twoSites(1, 2, false)());
const nf = nested(1);
log("nested", nf(), nf());
log("shadowed", shadowed(1)());
log("earlyCapture", earlyCapture()());
log("arguments", withArguments(1)());
log("finally", withFinally()());
log("forOf", forOf([1, 2, 3])(), forOf([1]));
log("rec", rec(3)());
log("default", withDefault(4)());
const c1 = counter(), c2 = counter();
log("counter", c1(), c1(), c2(), c1());
log("reassigned", reassigned()());
log("closureArgs", closureArgs("x"), closureArgs(2, 3)(4));
log("helper", collect(_createForOfIteratorHelperLoose([1, 2, 3])), collect(_createForOfIteratorHelperLoose({ length: 2, 0: "a", 1: "b" }, true)), collect(_createForOfIteratorHelperLoose(new Set([7, 8]))));
try { _createForOfIteratorHelperLoose(5); } catch (e) { log("helper-throw", e.constructor.name); }
// Hot: let a small program's JIT compile these.
let acc = 0;
for (let i = 0; i < 200000; i++) {
  acc += earlyOrClosure(i, (i & 7) === 0) instanceof Function ? 1 : 0;
  const f = mutating(i & 15);
  acc += f();
  acc += inLoop(3)();
  acc += collect(_createForOfIteratorHelperLoose([i & 3, 1])).length;
}
log("hot", acc);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"early 6 9
mutating 6 7 null
inLoop 40
twoSites 4 6
nested 7 9
shadowed 5
earlyCapture 4
arguments 9
finally 2
forOf 6 null
rec 6
default 8
counter 1 2 1 3
reassigned 2
closureArgs not a number 10
helper 1,2,3 a,b 7,8
helper-throw TypeError
hot 8325000"###;

fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn late_boxed_bindings_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
