//! `Set.prototype.add`, `Set.prototype.delete` and `Map.prototype.delete` from
//! compiled code (Tier C's collection arm), with the method replaced on the
//! prototype, an own shadow, a subclass, another prototype, and a `delete` site
//! that sees both a Map and a Set. Run interpreted, at the default thresholds,
//! with every function compiled at once, under GC stress and with the loop-head
//! entry off; the expected lines are Node 24's.

const EXPECTED: &str = "churn 80:80
both 50
churn 80:80
both 50
churn 79:80
both 50
replaced-add 81:81 calls=3000
replaced-delete 81:89
own-shadow 0:81
subclass 81:81 n=3000
set-proto -1:81
restored 81:81";

#[test]
fn collection_method_lanes_child() {
    if std::env::var_os("ZIPP_COLL_LANES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn collection_calls_match_the_interpreter() {
    if std::env::var_os("ZIPP_COLL_LANES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("osr-off", Some(("ZIPP_NO_TIERC_OSR", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "collection_method_lanes_child", "--nocapture"])
            .env("ZIPP_COLL_LANES_CHILD", "1")
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

const PROGRAM: &str = r####"// Set.prototype.add, Set.prototype.delete and Map.prototype.delete from
// compiled code (Tier C's collection lanes), with what must send a call
// elsewhere: the method replaced on the prototype, an own property shadowing
// it, a subclass overriding it, a collection given another prototype, and a
// `delete` site that sees both a Map and a Set.
"use strict";
const out = [];
function churn(s, m, n) {
  for (let i = 0; i < n; i++) {
    s.add(i % 97);
    m.set(i % 89, i);
    if (i % 3 === 0) s.delete((i * 7) % 97);
    if (i % 5 === 0) m.delete((i * 11) % 89);
  }
  return s.size + ":" + m.size;
}
function both(cs, n) {
  let t = 0;
  for (let i = 0; i < n; i++) {
    const c = cs[i & 1];
    if (c.delete(i % 50)) t++;
  }
  return t;
}
function attempt(name, f) {
  try {
    out.push(name + " " + f());
  } catch (e) {
    out.push(name + " " + e.constructor.name);
  }
}
for (let round = 0; round < 3; round++) {
  out.push("churn " + churn(new Set(), new Map(), 5000 + round));
  const s = new Set(), m = new Map();
  for (let i = 0; i < 50; i++) { s.add(i); m.set(i, i); }
  out.push("both " + both([s, m], 4000));
}
const addOrig = Set.prototype.add;
let calls = 0;
Set.prototype.add = function (v) { calls++; return addOrig.call(this, v); };
attempt("replaced-add", () => churn(new Set(), new Map(), 3000) + " calls=" + calls);
Set.prototype.add = addOrig;
const delOrig = Map.prototype.delete;
Map.prototype.delete = function (k) { return false; };
attempt("replaced-delete", () => churn(new Set(), new Map(), 3000));
Map.prototype.delete = delOrig;
const own = new Set();
own.add = function (v) { return this; };
attempt("own-shadow", () => churn(own, new Map(), 3000));
class CountSet extends Set {
  constructor() { super(); this.n = 0; }
  add(v) { this.n++; return super.add(v); }
}
const cs = new CountSet();
attempt("subclass", () => churn(cs, new Map(), 3000) + " n=" + cs.n);
const sp = new Set();
Object.setPrototypeOf(sp, { add() { return this; }, delete() { return true; }, get size() { return -1; } });
attempt("set-proto", () => churn(sp, new Map(), 3000));
attempt("restored", () => churn(new Set(), new Map(), 3000));
console.log(out.join("\n"));
"####;
