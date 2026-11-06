//! Compiled `in` on a plain object answers a flat string or an index key
//! from the key as it lies (`has_property_jit`: the interpreter's
//! `plain_string_key_has` / `plain_index_key_has` arms;
//! `ZIPP_NO_JIT_IN_PLAIN` asks `has_property`, which spelled the key
//! afresh and hashed it at every level): own, inherited and missing keys,
//! index keys and canonical numeric strings (`-0`, `1.5`,
//! 4294967295), a getter, `Object.prototype` and array-prototype members,
//! `Object.create(null)`, a class instance, a Symbol, a lone surrogate, a
//! dictionary with deletes, and an index added to `Object.prototype` and
//! removed again. The expected lines are Node's (v24), with the lane,
//! without it and in the interpreter.

const SOURCE: &str = r##"
"use strict";
const out = [];
const base = { inherited: 1, 7: "seven" };
const o = Object.create(base);
o.own = 1; o[3] = "three"; o["42"] = "x";
Object.defineProperty(o, "g", { get() { return 1; }, enumerable: false });
const bare = Object.create(null); bare.k = 1; bare[0] = 0;
class C { constructor() { this.f = 1; } m() {} }
const ci = new C();
const arrBase = Object.create([1, 2, 3]);
const sym = Symbol("s"); o[sym] = 1;
const keys = ["own", "inherited", "missing", "g", "3", "7", "42", "-0", "1.5", "toString", "hasOwnProperty",
  "constructor", "length", "\uD800", "k", "0", "f", "m"];
const nums = [3, 7, 42, 0, 1, -1, 1.5, 4294967295];
function probe(target) {
  let s = "";
  for (let r = 0; r < 300; r++) {
    s = "";
    for (const k of keys) s += (k in target) ? "1" : "0";
    for (const n of nums) s += (n in target) ? "1" : "0";
    s += (sym in target) ? "1" : "0";
  }
  return s;
}
out.push("o " + probe(o));
out.push("base " + probe(base));
out.push("bare " + probe(bare));
out.push("ci " + probe(ci));
out.push("arrBase " + probe(arrBase));
// A dictionary with deletes and re-adds, and an index added to Object.prototype later.
const d = {};
for (let i = 0; i < 300; i++) d["k" + i] = i;
for (let i = 0; i < 300; i += 3) delete d["k" + i];
let hits = 0;
for (let r = 0; r < 50; r++) for (let i = 0; i < 300; i++) if (("k" + i) in d) hits++;
out.push("dict " + hits);
Object.prototype[99] = 1;
let late = 0;
for (let r = 0; r < 500; r++) { if (99 in d) late++; if ("99" in o) late++; }
delete Object.prototype[99];
for (let r = 0; r < 500; r++) { if (99 in d) late += 1000; }
out.push("late " + late);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "o 110111100111000000111000001\nbase 010001000111000000010000000\nbare 000000000000001100000100000\nci 000000000111000011000000000\narrBase 000000000111100100000110000\ndict 10000\nlate 1000";

#[test]
fn jit_in_plain_worker() {
    if std::env::var_os("ZIPP_JIT_IN_PLAIN_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn jit_in_plain_matches_node() {
    if std::env::var_os("ZIPP_JIT_IN_PLAIN_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("lane-off", Some("ZIPP_NO_JIT_IN_PLAIN")),
        ("interpreter", Some("ZIPP_NOJIT")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["jit_in_plain_worker", "--exact", "--nocapture"])
            .env("ZIPP_JIT_IN_PLAIN_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_JIT_IN_PLAIN");
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