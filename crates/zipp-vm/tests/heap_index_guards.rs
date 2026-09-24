//! Values that are not heap references must never be read as heap indices
//! (debug builds assert it in `Value::heap_index`). Each case here once did,
//! silently in a release build: a primitive `this` for an iterator's `next`,
//! a direct-eval closure capturing a loop's internal registers as if they
//! were cells, and an eval-defined class's computed-key placeholder rebased
//! as if it were a function id.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

#[test]
fn iterator_next_on_a_primitive_this_throws() {
    let out = run_ok(
        r#"
const nexts = [
  new Map([[1, 2]]).keys().next,
  new Set([1]).values().next,
  [1][Symbol.iterator]().next,
  "ab".matchAll(/a/g).next,
];
const names = [];
for (const next of nexts) {
  for (const v of [true, 1, null, undefined, "s", Symbol(), 1n]) {
    try { next.call(v); names.push("accepted"); } catch (e) { names.push(e.constructor.name); }
  }
}
console.log(new Set(names).size === 1 ? names[0] : names.join());
"#,
    );
    assert_eq!(out, vec!["TypeError"]);
}

#[test]
fn eval_closures_in_loops_see_their_bindings() {
    // A function holding a direct eval boxes its locals; the loops' own
    // internal registers (iterator, index, keys) must stay plain.
    let out = run_ok(
        r#"
function forOf() {
  const got = [];
  for (var b of ["x", "y"]) { got.push((() => eval("b"))()); }
  return got.join("");
}
function forIn() {
  const got = [];
  for (var k in { p: 1, q: 2 }) { got.push((() => eval("k"))()); }
  return got.join("");
}
function strictForOf() {
  "use strict";
  const got = [];
  for (const v of [1, 2, 3]) { got.push((() => eval("v * 2"))()); }
  return got.join(",");
}
function nested() {
  let total = 0;
  for (const a of [1, 2]) for (const c in { u: 0, w: 0 }) total += (() => eval("a"))() + c.length;
  return total;
}
console.log(forOf(), forIn(), strictForOf(), nested());
"#,
    );
    assert_eq!(out, vec!["xy pq 2,4,6 10"]);
}

#[test]
fn eval_defined_classes_with_computed_members() {
    let out = run_ok(
        r#"
const k = "dyn";
const C = eval(`(class extends Array {
  [k]() { return "m"; }
  get [k + "G"]() { return "g"; }
  set [k + "S"](v) { this.s = v; }
  static get [k + "SG"]() { return "sg"; }
  static set [k + "SS"](v) { this.ss = v; }
  plain() { return "p"; }
})`);
const c = new C();
c.dynS = 5;
C.dynSS = 6;
console.log(c.dyn(), c.dynG, c.s, C.dynSG, C.ss, c.plain(), c instanceof Array);
"#,
    );
    assert_eq!(out, vec!["m g 5 sg 6 p true"]);
}
