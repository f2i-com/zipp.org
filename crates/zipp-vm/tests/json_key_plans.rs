//! `JSON.parse` objects share a key plan per member-name sequence
//! (`mathjson::JsonKeyPlans`), and every object a plan cannot hold takes the
//! old path: repeated names, element names, `__proto__` (an own data
//! property), `@@` names, escaped and non-ASCII names, more than sixteen
//! members, the empty object. Records round-tripped, then mutated, frozen,
//! spread, enumerated and stringified; reordered sequences, nesting and churn.
//! Run interpreted, at the default thresholds, with every function compiled at
//! once, under GC stress, with the plans off and with lazy compilation off; the
//! expected lines are Node 24's.

const EXPECTED: &str = r####"sum 4009000 keys id,name,tags,pos pos x,y
mutated [{"id":5,"name":"n5","tags":["b"],"pos":{"x":0,"y":-5},"extra":1},{"id":6,"tags":["a","b"],"pos":{"x":1,"y":-6}},{"name":"n0","tags":["b"],"pos":{"x":2,"y":-7}},{"id":8,"name":"n1","tags":["a","b"],"pos":{"x":3,"y":-8}},true,{"id":9,"name":"n2","tags":["b"],"pos":{"x":4,"y":-9}}]
spread {"id":-1,"name":"n3","tags":["a","b"],"pos":{"x":0,"y":-10}}
{"a":1,"b":2,"a":3} => keys ["a","b"] own__proto__ false protoIsObject true evil undefined str {"a":3,"b":2}
  for-in a,b
{"b":1,"0":2,"a":3} => keys ["0","b","a"] own__proto__ false protoIsObject true evil undefined str {"0":2,"b":1,"a":3}
  for-in 0,b,a
{"length":5,"x":1} => keys ["length","x"] own__proto__ false protoIsObject true evil undefined str {"length":5,"x":1}
  for-in length,x
{"__proto__":{"evil":1}, => keys ["__proto__","y"] own__proto__ true protoIsObject true evil undefined str {"__proto__":{"evil":1},"y":2}
  for-in __proto__,y
{"@@iterator":1,"z":2} => keys ["@@iterator","z"] own__proto__ false protoIsObject true evil undefined str {"@@iterator":1,"z":2}
  for-in @@iterator,z
{"@@@q":1} => keys ["@@@q"] own__proto__ false protoIsObject true evil undefined str {"@@@q":1}
  for-in @@@q
{"\u0061b":1,"c\n":2} => keys ["ab","c\n"] own__proto__ false protoIsObject true evil undefined str {"ab":1,"c\n":2}
  for-in ab,c

{"é":1,"€":2,"日本":3} => keys ["é","€","日本"] own__proto__ false protoIsObject true evil undefined str {"é":1,"€":2,"日本":3}
  for-in é,€,日本
{} => keys [] own__proto__ false protoIsObject true evil undefined str {}
  for-in 
{"k1":1,"k2":2,"k3":3,"k => keys ["k1","k2","k3","k4","k5","k6","k7","k8","k9","k10","k11","k12","k13","k14","k15","k16"] own__proto__ false protoIsObject true evil undefined str {"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":7,"k8":8,"k9":9,"k10":10,"k11":11,"k12":12,"k13":13,"k14":14,"k15":15,"k16":16}
  for-in k1,k2,k3,k4,k5,k6,k7,k8,k9,k10,k11,k12,k13,k14,k15,k16
{"k1":1,"k2":2,"k3":3,"k => keys ["k1","k2","k3","k4","k5","k6","k7","k8","k9","k10","k11","k12","k13","k14","k15","k16","k17"] own__proto__ false protoIsObject true evil undefined str {"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":7,"k8":8,"k9":9,"k10":10,"k11":11,"k12":12,"k13":13,"k14":14,"k15":15,"k16":16,"k17":17}
  for-in k1,k2,k3,k4,k5,k6,k7,k8,k9,k10,k11,k12,k13,k14,k15,k16,k17
[{"p":1,"q":2},{"q":2,"p => keys ["p","q"] own__proto__ false protoIsObject true evil undefined str {"p":1,"q":2}
  for-in p,q
[{"p":1,"q":2},{"q":2,"p => keys ["q","p"] own__proto__ false protoIsObject true evil undefined str {"q":2,"p":1}
  for-in q,p
[{"p":1,"q":2},{"q":2,"p => keys ["p","q"] own__proto__ false protoIsObject true evil undefined str {"p":3,"q":4}
  for-in p,q
[{"p":1,"q":2},{"q":2,"p => keys ["p"] own__proto__ false protoIsObject true evil undefined str {"p":5}
  for-in p
deep {"a":{"a":{"a":{"a":{"b":1}}}},"b":[{"a":1},{"a":2,"b":3}]}
deep2 {"a":{"a":{"a":{"a":{"b":1}},"c":1}},"b":[{"a":1},{"a":2,"b":3}]}
churn 4506000"####;

#[test]
fn json_key_plans_child() {
    if std::env::var_os("ZIPP_JSONPLANS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn parsed_objects_match_node() {
    if std::env::var_os("ZIPP_JSONPLANS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("plans-off", Some(("ZIPP_NO_JSON_KEY_PLANS", "1"))),
        ("eager", Some(("ZIPP_LAZY", "0"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "json_key_plans_child", "--nocapture"])
            .env("ZIPP_JSONPLANS_CHILD", "1")
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

const PROGRAM: &str = r####"// JSON.parse objects that share a key plan per member-name sequence, and
// every object a plan cannot hold: repeated names (the last value wins, at
// the first one's position), element names ("0", "length"), "__proto__"
// (an own data property, not the prototype), "@@" names, escaped and
// non-ASCII names, more than sixteen members, the empty object. Then what a
// program does with them: add, delete, redefine, freeze, enumerate, spread,
// stringify, compare shapes across sequences.
const out = [];
const show = (label, v) => out.push(label + " " + JSON.stringify(v));
const recs = [];
for (let i = 0; i < 2000; i++) recs.push({ id: i, name: "n" + (i % 7), tags: ["a", "b"].slice(i % 2), pos: { x: i % 5, y: -i } });
const back = JSON.parse(JSON.stringify(recs));
let sum = 0;
for (const r of back) sum += r.id + r.pos.x - r.pos.y + r.name.length + r.tags.length;
out.push("sum " + sum + " keys " + Object.keys(back[3]).join(",") + " pos " + Object.keys(back[3].pos).join(","));
back[5].extra = 1;
delete back[6].name;
Object.defineProperty(back[7], "id", { enumerable: false });
Object.freeze(back[8]);
back[8].id = 99;
show("mutated", [back[5], back[6], back[7], back[8], Object.isFrozen(back[8]), back[9]]);
show("spread", { ...back[10], id: -1 });
const odd = [
  '{"a":1,"b":2,"a":3}',
  '{"b":1,"0":2,"a":3}',
  '{"length":5,"x":1}',
  '{"__proto__":{"evil":1},"y":2}',
  '{"@@iterator":1,"z":2}',
  '{"@@@q":1}',
  '{"\\u0061b":1,"c\\n":2}',
  '{"é":1,"€":2,"日本":3}',
  "{}",
  '{"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":7,"k8":8,"k9":9,"k10":10,"k11":11,"k12":12,"k13":13,"k14":14,"k15":15,"k16":16}',
  '{"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":7,"k8":8,"k9":9,"k10":10,"k11":11,"k12":12,"k13":13,"k14":14,"k15":15,"k16":16,"k17":17}',
  '[{"p":1,"q":2},{"q":2,"p":1},{"p":3,"q":4},{"p":5}]',
];
for (const s of odd) {
  const o = JSON.parse(s);
  const items = Array.isArray(o) ? o : [o];
  for (const it of items) {
    out.push(s.slice(0, 24) + " => keys " + JSON.stringify(Object.keys(it)) + " own__proto__ " + Object.prototype.hasOwnProperty.call(it, "__proto__") + " protoIsObject " + (Object.getPrototypeOf(it) === Object.prototype) + " evil " + it.evil + " str " + JSON.stringify(it));
    const names = [];
    for (const k in it) names.push(k);
    out.push("  for-in " + names.join(","));
  }
}
const deep = JSON.parse('{"a":{"a":{"a":{"a":{"b":1}}}},"b":[{"a":1},{"a":2,"b":3}]}');
show("deep", deep);
deep.a.a.c = 1;
show("deep2", deep);
let churn = 0;
for (let i = 0; i < 3000; i++) {
  const o = JSON.parse('{"t":' + i + ',"u":"' + (i % 3) + '","v":[' + (i % 4) + "]}");
  churn += o.t + o.u.length + o.v[0];
  if (i % 1000 === 0) o.w = 1;
}
out.push("churn " + churn);
console.log(out.join("\n"));
"####;
