//! Object literals whose run of static keys ends before their last member, made
//! in one `FinalizeObject` step with the rest added to that object: the order
//! of evaluation (side effects in values, spread getters, a throwing spread), a
//! spread or later key overriding an earlier one in place, a computed key, an
//! accessor, a duplicate static key, `__proto__:`, a method using `super`,
//! spreads of null, strings and arrays, and key order. Run interpreted, at the
//! default thresholds, with every function compiled at once, under GC stress and
//! with the lowering off; the expected lines are Node 24's.

const EXPECTED: &str = r####"keys 10000 {"id":3,"name":"n3","note":"x","tail":6} tail,id,name,tail,id,name,note,tail
{"a":1,"name":"over","g":7,"z":26} a,name,getter,z
{"a":1,"b":2,"dyn1":3,"c":4}
{"a":1,"b":2,"c":3,"d":4} get c
{"a":3,"b":2,"c":4} ab
{"a":1,"b":2,"c":3} true abc
sub+base a,b,extra,hello
{"a":1,"b":2} {"0":"h","1":"i","a":1,"c":3} {"0":7,"1":8,"a":1}
1,2,b,a,c
{"a":1,"q":2} false
[{"a":1,"b":1,"c":1},"boom2"] a,b,a,b
1000 {"x":5,"y":10,"odd":true,"z":-5} {"x":6,"y":12,"even":true,"z":-6}"####;

#[test]
fn object_literal_prefix_child() {
    if std::env::var_os("ZIPP_OBJ_PREFIX_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn literal_prefixes_match_node() {
    if std::env::var_os("ZIPP_OBJ_PREFIX_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("prefix-off", Some(("ZIPP_NO_PREFIX_FINALIZE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "object_literal_prefix_child", "--nocapture"])
            .env("ZIPP_OBJ_PREFIX_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Object literals whose run of static keys ends before their last member:
// the run is made in one step and the rest added to it. What must hold: the
// order of evaluation (side effects in values, keys, spread getters), a
// spread or later key overriding an earlier one in place, a computed key, an
// accessor, a duplicate static key, `__proto__:`, a method using `super`,
// spreads of null, strings and arrays, and key order (integer keys first).
"use strict";
const out = [];
const log = [];
const v = (tag, x) => (log.push(tag), x);
function make(i) {
  return { id: v("id", i), name: v("name", "n" + i), ...(i % 3 === 0 ? { note: v("note", "x") } : {}), tail: v("tail", i * 2) };
}
let s = 0;
for (let i = 0; i < 3000; i++) s += Object.keys(make(i)).length;
out.push("keys " + s + " " + JSON.stringify(make(3)) + " " + log.slice(-8).join(","));
log.length = 0;
const src = { get g() { log.push("getter"); return 7; }, name: "over" };
const o1 = { a: v("a", 1), name: v("name", "first"), ...src, z: v("z", 26) };
out.push(JSON.stringify(o1) + " " + log.join(","));
const k = "dyn";
out.push(JSON.stringify({ a: 1, b: 2, [k + "1"]: 3, c: 4 }));
const acc = { a: 1, b: 2, get c() { return this.a + this.b; }, d: 4 };
out.push(JSON.stringify(acc) + " " + Object.getOwnPropertyDescriptor(acc, "c").get.name);
out.push(JSON.stringify({ a: 1, b: 2, a: 3, c: 4 }) + " " + Object.keys({ a: 1, b: 2, a: 3 }).join(""));
const pro = { a: 1, b: 2, __proto__: { inherited: true }, c: 3 };
out.push(JSON.stringify(pro) + " " + pro.inherited + " " + Object.keys(pro).join(""));
const base = { hello() { return "base"; } };
const sup = { a: 1, b: 2, ...{ extra: 1 }, hello() { return "sub+" + super.hello(); } };
Object.setPrototypeOf(sup, base);
out.push(sup.hello() + " " + Object.keys(sup).join(","));
out.push(JSON.stringify({ a: 1, ...null, b: 2, ...undefined }) + " " + JSON.stringify({ a: 1, ..."hi", c: 3 }) + " " + JSON.stringify({ a: 1, ...[7, 8] }));
out.push(Object.keys({ b: 1, a: 2, ...{ 2: "x", 1: "y" }, c: 3 }).join(","));
const frozen = Object.freeze({ q: 1 });
const copy = { a: 1, ...frozen };
copy.q = 2;
out.push(JSON.stringify(copy) + " " + Object.isFrozen(copy));
function thrower(i) {
  try {
    return { a: v("a", i), b: v("b", i), ...(i > 1 ? (() => { throw new Error("boom" + i); })() : {}), c: 1 };
  } catch (e) {
    return e.message;
  }
}
log.length = 0;
out.push(JSON.stringify([thrower(1), thrower(2)]) + " " + log.join(","));
const many = [];
for (let i = 0; i < 2000; i++) many.push({ x: i, y: i * 2, ...(i & 1 ? { odd: true } : { even: true }), z: -i });
out.push(many.filter((o) => o.odd).length + " " + JSON.stringify(many[5]) + " " + JSON.stringify(many[6]));
console.log(out.join("\n"));
"####;
