//! The private-name memo is keyed by the running callee: its heap index
//! and slot version. Classes evaluated in a loop and dropped hand their
//! methods' slots to the next evaluations' methods. Every private access
//! must still resolve against the running method's own class, throwing on
//! another evaluation's instance, and one class's sites shared by several
//! methods must stay distinct. Expected output is node's.

#[test]
fn private_access_resolves_against_the_running_method() {
    let src = r###""use strict";
// Classes evaluated in a loop and dropped, so their methods' heap slots are
// reused by the next evaluations' methods: every private access must resolve
// against the running method's own class, never a remembered predecessor's.
function make(tag) {
  return class {
    #v = tag;
    #w = tag * 2;
    static get(o) { return o.#v; }
    static both(o) { return o.#v + o.#w; }
    bump() { this.#w += 1; return this.#w; }
    static has(o) { return #v in o; }
  };
}
const out = [];
let ok = 0, threw = 0, foreign = 0;
let prev = null;
for (let i = 0; i < 3000; i++) {
  const C = make(i);
  const o = new C();
  if (C.get(o) === i && C.both(o) === i * 3 && o.bump() === i * 2 + 1) ok++;
  if (prev) {
    try { C.get(prev.o); } catch (e) { if (e instanceof TypeError) threw++; }
    if (!C.has(prev.o) && prev.C.has(prev.o)) foreign++;
  }
  prev = (i % 7 === 0) ? { C, o } : prev;
  for (let k = 0; k < 40; k++) ({ k, s: "g" + k });
}
out.push(ok + " " + threw + " " + foreign);
// one class, many instances, the same sites from two methods
class P { #a = 1; #b = 2; sum() { return this.#a + this.#b; } swap() { const t = this.#a; this.#a = this.#b; this.#b = t; return this.#a; } }
let s = 0;
for (let i = 0; i < 5000; i++) { const p = new P(); s += p.sum() + p.swap(); }
out.push(String(s));
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"3000 2999 2999
25000"###);
}
