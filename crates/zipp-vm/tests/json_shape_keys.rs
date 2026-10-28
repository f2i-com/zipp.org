//! JSON.stringify over objects that share shapes, which the plain walk
//! decides once per shape (`mathjson::JsonShapes`): keys needing escapes, a
//! non-enumerable or accessor property, integer-like keys, `toJSON` own,
//! inherited and on Object.prototype, class instances, a changed prototype, a
//! deleted key, symbols, and numbers of every kind (also through String). Run
//! interpreted, at the default thresholds, with every function compiled at
//! once and under GC stress; the expected lines are Node 24's.

const EXPECTED: &str = r####"32027 [{"id":0,"name":"n0","price":0,"tags":[0,"t\"0"],"nested":{"a":0,"b":null,"c":true}},{"id":1,"name":"n1","price":0.37,"tags":[1,"t\"1"],"nested":{"a":0.14285714285714285,"b":null,"c":false}},{"id":2,"name":"n2","price":0.74,"tags":[2,"t\"2"],"nested":{"a":0.2857142857142857,"b":null,"c":true}}]
[{"id":0,"name":"n0","price":0,"tags":[0,"t\"0"],"nested":{"a":0,"b":null,"c":true}},{"id":1,"name":"n1","price":0.37,"tags":[1,"t\"1"],"nested":{"a":0.14285714285714285,"b":null,"c":false}},{"id":2,"name":"n2","price":0.74,"tags":[2,"t\"2"],"nested":{"a":0.2857142857142857,"b":null,"c":true},"got":42},{"2":"two","10":"ten","id":3,"name":"n3","price":1.11,"tags":[0,"t\"3"],"nested":{"a":0.42857142857142855,"b":null,"c":false}}]
[{"quo\"te":1,"back\\slash":2,"unié":3,"ctl\u0001":4,"line\nbreak":5},{"quo\"te":6,"back\\slash":7,"unié":8,"ctl\u0001":9,"line\nbreak":10}]
["custom",{"x":2},{"x":3}]
["fromProto",{"y":1}]
"everywhere"
[{"z":1},{"z":2}]
[{"a":1,"b":[1]},{"a":2,"b":[2]},{"a":3,"b":[3]}]
[{"k1":1,"k2":2,"k3":3},{"k1":4,"k3":6}]
[{"v":2},{"v":4}]
[{"n":0.1},{"n":-0.5},{"n":1e+21},{"n":1e-7},{"n":123.456},{"n":0},{"n":null},{"n":null},{"n":9007199254740994},{"n":0.30000000000000004},{"n":0.3333333333333333},{"n":5e-324},{"n":1.7976931348623157e+308},{"n":0.000123},{"n":99999999999999.98}]
0.1 42.17 12.5 -3.75 0.00030000000000000003
[{"p":1,"q":{"p":1,"q":null}},{"p":1,"q":{"p":1,"q":null}},{"p":2,"q":{"p":1,"q":{"p":1,"q":null}}}]
[{"v":1},{"u":1,"f":2,"v":3}]"####;

#[test]
fn json_shape_keys_child() {
    if std::env::var_os("ZIPP_JSON_SHAPE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn stringify_matches_node_across_shapes() {
    if std::env::var_os("ZIPP_JSON_SHAPE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "json_shape_keys_child", "--nocapture"])
            .env("ZIPP_JSON_SHAPE_CHILD", "1")
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

const PROGRAM: &str = r####"// JSON.stringify over objects that share shapes: the plain walk decides a
// settled shape's keys once per call. Keys needing escapes, a non-enumerable
// or accessor property, integer-like keys (reordered), toJSON (own,
// inherited, non-enumerable), class instances, a changed prototype, a
// deleted key (dictionary mode), symbols, and numbers of every kind.
"use strict";
const out = [];
const rows = [];
for (let i = 0; i < 300; i++) rows.push({ id: i, name: "n" + i, price: (i * 37 % 1000) / 100, tags: [i % 3, "t\"" + i], nested: { a: i / 7, b: null, c: i % 2 === 0 } });
out.push(JSON.stringify(rows).length + " " + JSON.stringify(rows.slice(0, 3)));
const odd = rows.slice(0, 4).map((r) => ({ ...r }));
Object.defineProperty(odd[1], "hidden", { value: 1, enumerable: false });
Object.defineProperty(odd[2], "got", { get() { return 42; }, enumerable: true });
odd[3]["2"] = "two";
odd[3]["10"] = "ten";
out.push(JSON.stringify(odd));
const esc = [{ "quo\"te": 1, "back\\slash": 2, "unié": 3, "ctl\u0001": 4, "line\nbreak": 5 }, { "quo\"te": 6, "back\\slash": 7, "unié": 8, "ctl\u0001": 9, "line\nbreak": 10 }];
out.push(JSON.stringify(esc));
const withTo = [{ x: 1, toJSON() { return "custom"; } }, { x: 2 }, { x: 3, toJSON: undefined }];
out.push(JSON.stringify(withTo));
const proto = { toJSON() { return "fromProto"; } };
const kids = [Object.create(proto), { y: 1 }];
kids[0].y = 2;
out.push(JSON.stringify(kids));
Object.prototype.toJSON = function () { return "everywhere"; };
out.push(JSON.stringify([{ z: 1 }, { z: 2 }]));
delete Object.prototype.toJSON;
out.push(JSON.stringify([{ z: 1 }, { z: 2 }]));
class P { constructor(a) { this.a = a; this.b = [a]; } }
out.push(JSON.stringify([new P(1), new P(2), { a: 3, b: [3] }]));
const dict = [{ k1: 1, k2: 2, k3: 3 }, { k1: 4, k2: 5, k3: 6 }];
delete dict[1].k2;
out.push(JSON.stringify(dict));
const sym = Symbol("s");
out.push(JSON.stringify([{ [sym]: 1, v: 2 }, { [sym]: 3, v: 4 }]));
const nums = [{ n: 0.1 }, { n: -0.5 }, { n: 1e21 }, { n: 1e-7 }, { n: 123.456 }, { n: -0 }, { n: NaN }, { n: Infinity }, { n: 2 ** 53 + 2 }, { n: 0.1 + 0.2 }, { n: 1 / 3 }, { n: 5e-324 }, { n: 1.7976931348623157e308 }, { n: 0.000123 }, { n: 99999999999999.98 }];
out.push(JSON.stringify(nums));
out.push(String(0.1) + " " + String(42.17) + " " + (12.5).toString() + " " + `${-3.75}` + " " + (0.0001 + 0.0002));
const same = { p: 1, q: { p: 1, q: null } };
out.push(JSON.stringify([same, same, { p: 2, q: same }]));
const undef = [{ u: undefined, f() {}, v: 1 }, { u: 1, f: 2, v: 3 }];
out.push(JSON.stringify(undef));
console.log(out.join("\n"));
"####;
