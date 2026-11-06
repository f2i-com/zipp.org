//! A for-of step over a user iterator object: compiled code calls its
//! cached `next` native to native (`jit_iter_next`, the caller's window
//! size in `idx_reg`'s high half), and both lanes read a `{value, done}`
//! result where it lies (`iter_step_result`). Results with getters (each
//! read observed), reordered or extra keys, a missing or non-boolean
//! `done`, a non-object result, a throwing `next`, `break` closing the
//! iterator, `next` replaced mid-loop (the cached one keeps running),
//! arrow, bound and `arguments`-using `next`, deep recursion inside it,
//! private-field iterators, and the spread, destructuring and generator
//! paths beside them. The expected lines are Node's (v24), with the lane,
//! without it, in the interpreter and with every function compiled at once.

const SOURCE: &str = r##"
"use strict";
// for-of over user iterators from hot compiled loops: every result and `next` shape.
const out = [];
class RangeIter {
  #cur; #end;
  constructor(a, b) { this.#cur = a; this.#end = b; }
  next() { if (this.#cur >= this.#end) return { value: undefined, done: true }; return { value: this.#cur++, done: false }; }
  [Symbol.iterator]() { return this; }
}
function sumOf(it) { let s = 0; for (const v of it) s += v; return s; }
let a = 0;
for (let r = 0; r < 400; r++) a += sumOf(new RangeIter(0, 50));
out.push("range " + a);
// results with getters (observable reads), done read once per step, value only when not done
let gets = { v: 0, d: 0 };
const getterIt = { i: 0, next() { const i = this.i++; return { get value() { gets.v++; return i; }, get done() { gets.d++; return i >= 20; } }; }, [Symbol.iterator]() { return this; } };
let g = 0;
for (let r = 0; r < 200; r++) { getterIt.i = 0; for (const v of getterIt) g += v; }
out.push("getters " + g + " " + gets.v + " " + gets.d);
// reordered result, extra keys, missing done, truthy non-boolean done
function mk(kind) {
  let i = 0;
  return { next() { i++; if (kind === 0) return i > 10 ? { done: true, value: -1 } : { done: false, value: i };
    if (kind === 1) return i > 10 ? { value: 0, done: 1, x: 2 } : { value: i, done: 0, x: 3 };
    return i > 10 ? { value: i, done: "yes" } : { value: i }; }, [Symbol.iterator]() { return this; } };
}
let k = 0;
for (let r = 0; r < 300; r++) for (const v of mk(r % 3)) k += v;
out.push("shapes " + k);
// non-object result: TypeError; a throwing next; break calls return()
let errs = [0, 0, 0];
for (let r = 0; r < 300; r++) {
  try { for (const v of { n: 0, next() { return this.n++ < 5 ? { value: 1, done: false } : 42; }, [Symbol.iterator]() { return this; } }) k += v; } catch (e) { errs[0] += e instanceof TypeError ? 1 : 100; }
  try { for (const v of { n: 0, next() { if (this.n++ === 3) throw new RangeError("x"); return { value: 2, done: false }; }, [Symbol.iterator]() { return this; } }) k += v; } catch (e) { errs[1] += e instanceof RangeError ? 1 : 100; }
  const closer = { n: 0, closed: 0, next() { return { value: this.n++, done: false }; }, return() { this.closed++; return {}; }, [Symbol.iterator]() { return this; } };
  for (const v of closer) { if (v === 4) break; }
  errs[2] += closer.closed;
}
out.push("errors " + errs.join(",") + " " + k);
// next replaced mid-loop: the loop keeps the next it started with
let rep = 0;
for (let r = 0; r < 200; r++) {
  const it = { n: 0, next() { if (this.n === 2) this.next = () => ({ value: 1000, done: true }); return { value: this.n++, done: this.n > 6 }; }, [Symbol.iterator]() { return this; } };
  for (const v of it) rep += v;
}
out.push("replaced " + rep);
// arrow, bound and class-method next; a next using `arguments`; deep recursion in next
const arrowIt = { n: 0 }; arrowIt.next = () => ({ value: arrowIt.n++, done: arrowIt.n > 5 }); arrowIt[Symbol.iterator] = () => arrowIt;
const boundIt = { n: 0, [Symbol.iterator]() { return this; } }; boundIt.next = function () { return { value: this.n++, done: this.n > 5 }; }.bind(boundIt);
const argsIt = { n: 0, next() { return { value: arguments.length + this.n++, done: this.n > 5 }; }, [Symbol.iterator]() { return this; } };
function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1); }
const deepIt = { n: 0, next() { return { value: deep(200), done: this.n++ > 3 }; }, [Symbol.iterator]() { return this; } };
let m = 0;
for (let r = 0; r < 300; r++) {
  arrowIt.n = 0; boundIt.n = 0; argsIt.n = 0; deepIt.n = 0;
  for (const v of arrowIt) m += v;
  for (const v of boundIt) m += v;
  for (const v of argsIt) m += v;
  for (const v of deepIt) m += v;
}
out.push("kinds " + m);
// a spread and a destructuring of a user iterable (other paths), and a generator
let sp = 0;
for (let r = 0; r < 300; r++) { const xs = [...new RangeIter(0, 7)]; const [p, q, , s = -1] = new RangeIter(r, r + 2); sp += xs.length + p + q + s; }
function* gen(n) { for (let i = 0; i < n; i++) yield i; }
for (let r = 0; r < 300; r++) for (const v of gen(5)) sp += v;
out.push("other " + sp);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "range 490000\ngetters 38000 4000 4200\nshapes 16500\nerrors 300,300,300 19800\nreplaced 3000\nkinds 249000\nother 94800";

#[test]
fn user_iterator_step_worker() {
    if std::env::var_os("ZIPP_USER_ITERATOR_STEP_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn user_iterator_step_matches_node() {
    if std::env::var_os("ZIPP_USER_ITERATOR_STEP_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("lane-off", Some("ZIPP_NO_ITER_CROSS")),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["user_iterator_step_worker", "--exact", "--nocapture"])
            .env("ZIPP_USER_ITERATOR_STEP_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_ITER_CROSS")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}