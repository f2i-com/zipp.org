//! Inline prototype method reads and direct captured calls for `Map`, `Set`,
//! `WeakMap`, `WeakSet`, and `RegExp` receivers.
//!
//! Pristine instances carry kind tags below `FID_MIRROR_STR` in `HotMirror::fid`
//! (cleared by `Heap::bump_version` on any `arr_props` own-property addition),
//! letting compiled code load methods directly from live prototype slots under a
//! version guard, and compare captured callee bits directly for Map/Set/WeakMap/
//! WeakSet lookups and mutations.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 1500;
const pick = (x) => x;

function mapOps(m, ks, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const k = ks[i & 3];
    m.set(pick(k), i);
    if (m.has(pick(k))) s += m.get(pick(k));
    if ((i & 7) === 7) m.delete(pick(k));
  }
  return s + ":" + m.size;
}
function setOps(st, ks, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const k = ks[i & 3];
    st.add(pick(k));
    if (st.has(pick(k))) s++;
    if ((i & 7) === 7) st.delete(pick(k));
  }
  return s + ":" + st.size;
}
function wmOps(wm, objs, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const o = objs[i & 3];
    wm.set(pick(o), i);
    if (wm.has(pick(o))) s += wm.get(pick(o));
    if ((i & 7) === 7) wm.delete(pick(o));
  }
  return s;
}
function wsOps(ws, objs, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const o = objs[i & 3];
    ws.add(pick(o));
    if (ws.has(pick(o))) s++;
    if ((i & 7) === 7) ws.delete(pick(o));
  }
  return s;
}
function reOps(re, xs, n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const str = xs[i & 3];
    if (re.test(pick(str))) s++;
    const e = re.exec(pick(str));
    if (e) s += e[0].length;
  }
  return s;
}
function typeofAndEq(vals, n) {
  let s = "";
  for (let i = 0; i < n; i++) {
    const v = vals[i % vals.length];
    const t = typeof v;
    const isObj = typeof v === "object";
    const isFn = typeof v === "function";
    const isStr = typeof v === "string";
    const tr = Boolean(v);
    const eqSelf = v === v;
    const eqNext = v === vals[(i + 1) % vals.length];
    if (i < vals.length) s += t + "/" + isObj + "/" + isFn + "/" + isStr + "/" + tr + "/" + eqSelf + "/" + eqNext + ";";
  }
  return s;
}
function mixedGet(recvs, k, n) {
  let out = "";
  for (let i = 0; i < n; i++) {
    const r = recvs[i % recvs.length];
    let v;
    try { v = r.get(pick(k)); } catch (e) { v = e.name; }
    if (i < recvs.length) out += String(v) + ",";
  }
  return out;
}

const ks = ["a", "b", "c", "d"];
const objs = ks.map((k) => ({ k }));
const m = new Map();
const st = new Set();
const wm = new WeakMap();
const ws = new WeakSet();
const re = /^(?:a|b|c)$/;

log("ops", mapOps(m, ks, N), setOps(st, ks, N), wmOps(wm, objs, N), wsOps(ws, objs, N), reOps(re, ks, N));
log("typeof", typeofAndEq([m, st, wm, ws, re, () => 1, "hi", "", {}, []], 200));

let errWm = "", errWs = "";
for (let i = 0; i < 40; i++) {
  try { wm.set(pick(i === 39 ? 123 : objs[0]), i); } catch (e) { errWm = e.name; }
  try { ws.add(pick(i === 39 ? "bad" : objs[0])); } catch (e) { errWs = e.name; }
}
log("weak_err", errWm, errWs);

class Box { get(k) { return "box:" + k; } }
class SubMap extends Map { get(k) { return "sub:" + super.get(k); } }
const sm = new SubMap([["a", 42]]);
const m2 = new Map([["a", 10]]);
const wm2 = new WeakMap();
const keyObj = { x: 1 };
wm2.set(keyObj, 99);
const plain = { get(k) { return "plain:" + k; } };
const prox = new Proxy(m2, {});
log("mixed", mixedGet([m2, sm, new Box(), plain, prox], "a", 200), mixedGet([wm2, plain], keyObj, 200));

function readMapGet(x, k, n) { let r; for (let i = 0; i < n; i++) r = x.get(pick(k)); return r; }
function readSetHas(x, k, n) { let r; for (let i = 0; i < n; i++) r = x.has(pick(k)); return r; }
function readReTest(x, s, n) { let r; for (let i = 0; i < n; i++) r = x.test(pick(s)); return r; }
const lm = new Map([["a", 1]]);
const ls = new Set(["a"]);
const lr = /a/;
log("before_own", readMapGet(lm, "a", 300), readSetHas(ls, "a", 300), readReTest(lr, "a", 300));
lm.get = () => "own-map";
Object.defineProperty(ls, "has", { value: () => "own-set", configurable: true });
lr.test = () => "own-re";
log("after_own", readMapGet(lm, "a", 300), readSetHas(ls, "a", 300), readReTest(lr, "a", 300));
delete lm.get;
delete ls.has;
delete lr.test;
log("after_del", readMapGet(lm, "a", 300), readSetHas(ls, "a", 300), readReTest(lr, "a", 300));

const pm = new Map([["a", 5]]);
log("proto_before", readMapGet(pm, "a", 300));
Object.setPrototypeOf(pm, { get() { return "reproto"; }, __proto__: Map.prototype });
log("proto_after", readMapGet(pm, "a", 300));

const savedGet = Map.prototype.get;
Map.prototype.get = function () { return "proto-replaced"; };
log("proto_replace", readMapGet(m2, "a", 300));
Map.prototype.get = savedGet;
log("proto_restore", readMapGet(m2, "a", 300));

const savedHas = Set.prototype.has;
Object.defineProperty(Set.prototype, "has", { get() { return () => "acc-has"; }, configurable: true });
log("proto_acc", readSetHas(ls, "a", 300));
Object.defineProperty(Set.prototype, "has", { value: savedHas, writable: true, configurable: true, enumerable: false });
log("proto_acc_restore", readSetHas(ls, "a", 300));

const savedTest = RegExp.prototype.test;
RegExp.prototype.test = function () { return "re-patched"; };
log("re_patch", readReTest(lr, "a", 300));
RegExp.prototype.test = savedTest;
log("re_restore", readReTest(lr, "a", 300));

console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
        .iter()
        .flat_map(|l| l.lines().map(str::to_string))
        .collect()
}

#[test]
fn kind_proto_method_read_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "ops 1124250:4 1500:4 1124250 1500 2250",
            "typeof object/true/false/false/true/true/false;object/true/false/false/true/true/false;object/true/false/false/true/true/false;object/true/false/false/true/true/false;object/true/false/false/true/true/false;function/false/true/false/true/true/false;string/false/false/true/true/true/false;string/false/false/true/false/true/false;object/true/false/false/true/true/false;object/true/false/false/true/true/false;",
            "weak_err TypeError TypeError",
            "mixed 10,sub:42,box:a,plain:a,TypeError, 99,plain:[object Object],",
            "before_own 1 true true",
            "after_own own-map own-set own-re",
            "after_del 1 true true",
            "proto_before 5",
            "proto_after reproto",
            "proto_replace proto-replaced",
            "proto_restore 10",
            "proto_acc acc-has",
            "proto_acc_restore true",
            "re_patch re-patched",
            "re_restore true",
        ]
    );
}

#[test]
fn kind_proto_method_read_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("kind_proto_method_read_answers_as_node")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
