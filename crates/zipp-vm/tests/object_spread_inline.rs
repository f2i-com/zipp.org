//! An object-literal spread whose source is itself a literal that nothing can
//! observe (`{ ...base, ...(gift ? { note } : {}) }`, `...(c && { a })`)
//! defines the source's properties in place, with no source object
//! (`Compiler::object_spread_inline`). Values, their evaluation order,
//! duplicate and integer keys, function names, attributes, falsy and truthy
//! `&&` operands, nesting, `__proto__` and throws are Node 24's, in every
//! execution mode and with the inlining off (`ZIPP_NO_SPREAD_INLINE=1`).

use std::process::Command;

const CHILD_ENV: &str = "ZIPP_OBJECT_SPREAD_INLINE_CHILD";

#[test]
fn object_spread_inline_child() {
    let out = zipp_vm::run(PROGRAM).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}; output: {:?}", out.error, out.output);
    let lines: Vec<String> = out.output.iter().flat_map(|l| l.lines()).map(str::to_string).collect();
    assert_eq!(
        lines,
        [
            r#"gifts 30 [["id",1],["note","g"]]|id,note [["id",2]]|id"#,
            r#"order xaby [["x",1],["a",2],["b",3],["y",4]]|x,a,b,y"#,
            r#"dup [["a",2],["b",3]]|a,b [["b",3],["a",2]]|b,a [["a",3],["b",2]]|a,b"#,
            r#"ints [["1","y"],["2","x"],["z",1],["q",0]]|1,2,z,q [["3",3],["9",2],["10",1]]|3,9,10"#,
            r#"names f,g,h"#,
            r#"and-falsy [["k",1]]|k [["k",1]]|k [["k",1]]|k [["k",1]]|k [["k",1]]|k [["k",1]]|k [["k",1]]|k [["k",1]]|k"#,
            r#"and-truthy [["a",1]]|a [["b",2]]|b [["c",3]]|c"#,
            r#"and-hot 150"#,
            r#"nest []| [["b",2]]|b []| [["a",1]]|a"#,
            r#"setter 0 true"#,
            r#"proto []| [["__proto__",5]]|__proto__"#,
            r#"attrs truetruetrue [["a",1],["b",2]]|a,b"#,
            r#"throw x"#,
        ]
    );
    // A falsy OBJECT (`IsHTMLDDA`) as the `&&` operand is spread itself:
    // its own properties are copied (the spec; Node has no such object).
    let dda = zipp_vm::run(DDA).expect("source compiles");
    assert!(dda.error.is_none(), "{:?}", dda.error);
    assert_eq!(dda.output.concat().trim(), r#"{"k":1,"own":1} own"#);
}

const DDA: &str = r#"
const dda = $262.IsHTMLDDA;
dda.own = 1;
console.log(JSON.stringify({ k: 1, ...(dda && { a: 1 }) }), Object.keys(dda).join(","));
"#;

#[test]
fn object_spread_inline_in_all_modes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", &[("ZIPP_NOJIT", "1")][..]),
        ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc-stress", &[("ZIPP_GC_STRESS", "1")][..]),
        ("interpreter-gc-stress", &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..]),
        ("no-inline", &[("ZIPP_NO_SPREAD_INLINE", "1")][..]),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["--exact", "object_spread_inline_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("ZIPP_CACHE_DIR", "")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_SPREAD_INLINE");
        cmd.envs(env.iter().copied());
        let out = cmd.output().expect("spawn execution-mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"
// Object-literal spreads of literals: what inlining them must preserve.
const out = [];
const show = (o) => JSON.stringify(Object.entries(o)) + "|" + Object.keys(o).join(",");
const N = 300;
function hot(f) { let r; for (let i = 0; i < N; i++) r = f(i); return r; }

// 1. The optional-field idiom, both branches, hot.
let gifts = 0;
hot((i) => { const o = { id: i, ...(i % 10 === 0 ? { note: "gift" } : {}) }; if (o.note) gifts++; return o; });
out.push("gifts " + gifts + " " + show({ id: 1, ...(true ? { note: "g" } : {}) }) + " " + show({ id: 2, ...(false ? { note: "g" } : {}) }));

// 2. Evaluation order around the spread.
const order = [];
const L = (k, v) => (order.push(k), v);
const o2 = { x: L("x", 1), ...{ a: L("a", 2), b: L("b", 3) }, y: L("y", 4) };
out.push("order " + order.join("") + " " + show(o2));

// 3. Duplicate keys: within the source, and with the target.
out.push("dup " + show({ a: 1, ...{ a: 2, b: 3 } }) + " " + show({ ...{ b: 1 }, a: 2, ...{ b: 3 } }) + " " + show({ ...{ a: 1, b: 2, a: 3 } }));

// 4. Integer-like keys copy first; the target lists them first anyway.
out.push("ints " + show({ z: 1, ...{ 2: "x", 1: "y", q: 0 } }) + " " + show({ ...{ 10: 1, 9: 2 }, 3: 3 }));

// 5. Function names from the source literal's keys.
const o5 = { ...{ f: () => 1, g: function () {}, h: class {} } };
out.push("names " + o5.f.name + "," + o5.g.name + "," + o5.h.name);

// 6. && with every falsy primitive, and truthy values.
const falsy = [false, 0, -0, NaN, "", null, undefined, 0n];
out.push("and-falsy " + falsy.map((v) => show({ k: 1, ...(v && { a: 1 }) })).join(" "));
out.push("and-truthy " + show({ ...(1 && { a: 1 }) }) + " " + show({ ...("s" && { b: 2 }) }) + " " + show({ ...({} && { c: 3 }) }));
let andHits = 0;
hot((i) => { const o = { ...(i & 1 && { odd: true }), i }; if (o.odd) andHits++; });
out.push("and-hot " + andHits);

// 7. Nesting and null.
out.push("nest " + [0, 1, 2, 3].map((i) => show({ ...(i & 1 ? (i & 2 ? { a: 1 } : { b: 2 }) : null) })).join(" "));

// 8. A setter on Object.prototype runs for neither the source nor the copy.
let setterRuns = 0;
Object.defineProperty(Object.prototype, "trap", { set(v) { setterRuns++; }, configurable: true });
const o8 = { ...{ trap: 1 } };
delete Object.prototype.trap;
out.push("setter " + setterRuns + " " + Object.getOwnPropertyDescriptor(o8, "trap").writable);

// 9. __proto__ in the source sets the source's prototype: nothing to copy.
out.push("proto " + show({ ...{ __proto__: { inherited: 1 } } }) + " " + show({ ...{ ["__proto__"]: 5 } }));

// 10. Attributes of a copied property, and a spread at the start.
const o10 = { ...(gifts > 0 ? { a: 1 } : {}), b: 2 };
const d = Object.getOwnPropertyDescriptor(o10, "a");
out.push("attrs " + d.writable + d.enumerable + d.configurable + " " + show(o10));

// 11. Throws inside a source value: the object is never seen.
let thrown = "none";
try { ({ a: 1, ...{ b: (() => { throw new Error("x"); })() } }); } catch (e) { thrown = e.message; }
out.push("throw " + thrown);

console.log(out.join("\n"));
"####;