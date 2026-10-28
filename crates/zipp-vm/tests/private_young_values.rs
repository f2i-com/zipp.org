//! Young values stored into private fields of old instances, by
//! interpreted and compiled code alike, survive the minor collections that
//! run while the fields are their only references. A minor does not walk
//! every instance's private fields: each private write of a young value
//! records it (`Heap::record_young_value`), and the minor marks the record.
//! Compiled code's way-0 private write once stored without it, and the
//! field was left naming a swept slot. The expected line is Node's.

const SOURCE: &str = r##"
"use strict";
class Box {
  #v = null;
  #w;
  static #shared = null;
  constructor(i) { this.#w = { i }; }
  put(v) { this.#v = v; }
  get() { return this.#v; }
  w() { return this.#w; }
  swapW(o) { const t = this.#w; this.#w = o.#w; o.#w = t; }
  static share(v) { Box.#shared = v; }
  static shared() { return Box.#shared; }
}
class Sub extends Box {
  #extra;
  constructor(i) { super(i); this.#extra = [i]; }
  extra() { return this.#extra; }
  setExtra(v) { this.#extra = v; }
}
const boxes = [];
for (let i = 0; i < 200; i++) boxes.push(i & 1 ? new Sub(i) : new Box(i));
// Age the boxes: plenty of garbage so several collections run.
let junk = 0;
for (let i = 0; i < 200000; i++) junk += [i, { i }].length;
let sum = 0;
for (let round = 0; round < 60; round++) {
  for (let i = 0; i < boxes.length; i++) {
    const b = boxes[i];
    b.put({ round, i, s: "r" + round + ":" + i });
    if (b instanceof Sub && (i + round) % 3 === 0) b.setExtra([round, i, { deep: { round } }]);
    if (i % 17 === 0) b.swapW(boxes[(i + 1) % boxes.length]);
  }
  Box.share({ round, list: [round, round + 1] });
  // Churn so a minor (or several) runs while the only references are private.
  for (let k = 0; k < 20000; k++) junk += ({ k }).k & 1;
  for (let i = 0; i < boxes.length; i++) {
    const b = boxes[i];
    const v = b.get();
    sum = (sum + v.round + v.i + v.s.length + b.w().i) | 0;
    if (b instanceof Sub) {
      const e = b.extra();
      sum = (sum + e.length + (e[2] ? e[2].deep.round : e[0])) | 0;
    }
  }
  const sh = Box.shared();
  sum = (sum + sh.round + sh.list[1]) | 0;
}
console.log("privyoung", sum, junk);
"##;

#[test]
fn young_private_values_survive_minors() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n").trim(), "privyoung 3020065 1000000");
}
