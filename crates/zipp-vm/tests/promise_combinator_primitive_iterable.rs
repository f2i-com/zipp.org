//! Promise combinators over a primitive iterable (`Promise.all(0)` with an
//! `@@iterator` getter on `Number.prototype`): the getter and the method see
//! a number `this` in strict code, and the result vectors are sized from the
//! inline array lane only (a debug build asserted reading a primitive as a
//! heap index). The expected lines are Node's.

const SOURCE: &str = r##"
"use strict";
var out = [];
var empty = { next() { return { done: true }; } };
var two = function () { var i = 0; return { next() { return i < 2 ? { value: i++, done: false } : { done: true }; } }; };
Object.defineProperty(Number.prototype, Symbol.iterator, {
  configurable: true,
  get() {
    out.push("get " + typeof this + " " + this);
    var self = this;
    return function () { out.push("call " + typeof this + " " + (this === self)); return this === 7 ? two() : empty; };
  }
});
Promise.all(0).then(function (v) { out.push("all " + JSON.stringify(v)); });
Promise.allSettled(0).then(function (v) { out.push("allSettled " + JSON.stringify(v)); });
Promise.race(0);
Promise.any(7).then(function (v) { out.push("any " + v); });
Promise.all(7).then(function (v) { out.push("all7 " + JSON.stringify(v)); });
Promise.all([1, 2]).then(function (v) { out.push("array " + JSON.stringify(v)); });
Promise.all("ab").then(function (v) { out.push("string " + JSON.stringify(v)); });
setTimeout(function () { console.log(out.join("\n")); }, 0);
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
fn promise_combinator_primitive_iterable_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
