//! A loose for-of loop's step result object is rewritten in place by the
//! next step (`bytecode::step_call_sites`, `Vm::bound_step_call`), against
//! Node.
//!
//! The probe drives the Babel helper's `it.next.bind(it)` steps over
//! arrays, strings, Sets, Map keys, typed arrays, holes, an array that grows
//! while it is walked, generators, recursion through the same call site,
//! nested loops, break, continue and a throw; then the cases recycling must
//! leave alone: a step variable stored into an array (each step must stay a
//! distinct object), and a user iterator returning its own cached result
//! object, including from the same call site in the same frame just before
//! an array loop. A hot loop at the end runs the JIT tiers too. The expected
//! lines are Node's (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));

function helper(r) {
  var t2 = "undefined" != typeof Symbol && r[Symbol.iterator] || r["@@iterator"];
  if (t2) return (t2 = t2.call(r)).next.bind(t2);
  throw new TypeError("not iterable");
}
function sum(xs) {
  var s = 0;
  for (var it = helper(xs), st; !(st = it()).done;) s += st.value;
  return s;
}
function joined(xs) {
  var parts = [];
  for (var it = helper(xs), st; !(st = it()).done;) parts.push(String(st.value));
  return parts.join(",");
}
// The step variable escapes: it must not be recycled.
function keepSteps(xs) {
  var kept = [];
  for (var it = helper(xs), st; !(st = it()).done;) kept.push(st);
  return kept.map((s) => s.value + ":" + s.done).join(",") + " distinct=" + (kept.length < 2 || kept[0] !== kept[1]);
}
// A user iterator that hands back one cached result object.
function cachedIterable(n) {
  const res = { value: 0, done: false };
  return {
    res,
    [Symbol.iterator]() {
      let i = 0;
      return { next() { if (i < n) { res.value = i * 10; res.done = false; i++; } else { res.done = true; res.value = undefined; } return res; } };
    },
  };
}
// One frame, one call site: a user iterable first, then an array.
function mixed(first, second) {
  var log2 = [];
  for (var k = 0; k < 2; k++) {
    for (var it = helper(k === 0 ? first : second), st; !(st = it()).done;) log2.push(st.value);
  }
  return log2.join(",");
}
function recur(depth) {
  var s = 0;
  for (var it = helper([1, 2, 3]), st; !(st = it()).done;) {
    s += st.value;
    if (depth > 0) s += recur(depth - 1);
  }
  return s;
}
function breaks(xs) {
  var seen = [];
  for (var it = helper(xs), st; !(st = it()).done;) {
    if (st.value === 2) continue;
    if (st.value === 4) break;
    seen.push(st.value);
  }
  return seen.join(",");
}
function nested(a, b) {
  var pairs = [];
  for (var i1 = helper(a), s1; !(s1 = i1()).done;) {
    for (var i2 = helper(b), s2; !(s2 = i2()).done;) pairs.push(s1.value + s2.value);
  }
  return pairs.join(",");
}
function throwing(xs) {
  var got = [];
  try {
    for (var it = helper(xs), st; !(st = it()).done;) {
      if (st.value === 3) throw new Error("stop at " + st.value);
      got.push(st.value);
    }
  } catch (e) {
    got.push(e.message);
  }
  return got.join(",");
}
function* gen() { yield 1; yield 2; }
function mutating() {
  var xs = [1, 2], seen = [];
  for (var it = helper(xs), st; !(st = it()).done;) {
    seen.push(st.value);
    if (xs.length < 5) xs.push(st.value * 10);
  }
  return seen.join(",");
}
function doneValue(xs) {
  var last;
  for (var it = helper(xs), st; !(st = it()).done;) last = st.value;
  return last + " " + st.done + " " + st.value;
}

log("sum", sum([1, 2, 3, 4]), sum([]), sum([5]));
log("joined", joined(["a", "b"]), joined(new Set([1, 2])), joined(new Map([[1, 2]]).keys()), joined("xy"));
log("keep", keepSteps([7, 8, 9]));
const ci = cachedIterable(3);
log("cached", joined(ci), ci.res.value, ci.res.done);
const ci2 = cachedIterable(2);
log("mixed", mixed(ci2, [100, 200, 300]), ci2.res.value, ci2.res.done);
log("recur", recur(2));
log("breaks", breaks([1, 2, 3, 4, 5]));
log("nested", nested([1, 2], [10, 20]));
log("throwing", throwing([1, 2, 3, 4]), throwing([9]));
log("generator", joined(gen()));
log("holes", joined([1, , 3]), joined(new Int8Array([4, 5])));
log("mutating", mutating());
log("doneValue", doneValue([1, 2]));
let acc = 0;
for (let i = 0; i < 100000; i++) acc += sum([i & 7, 1, 2]) + recur(0);
log("hot", acc);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"sum 10 0 5
joined a,b 1,2 1 x,y
keep 7:false,8:false,9:false distinct=true
cached 0,10,20 undefined true
mixed 0,10,100,200,300 undefined true
recur 78
breaks 1,3
nested 11,21,12,22
throwing 1,2,stop at 3 9
generator 1,2
holes 1,undefined,3 4,5
mutating 1,2,10,20,100
doneValue 2 true undefined
hot 1250000"###;

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
fn recycled_step_results_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
