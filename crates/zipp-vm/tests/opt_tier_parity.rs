//! Regression tests for Commit 4A Tier O parity & core fixes:
//! - `TypeOfIs` on shaped constructors (`Uint8Array`), shaped functions, and static classes
//! - `refresh_cross_route_epoch` & `jit_globals_still_routable` on inlined callees reading globals
//! - `fn.apply` / `fn.call` / `PROP_CLASS_GETTER` / proto method reads
//! - `ReadyCells` / `ReadyClosures` / spill-slot recycling with evicted victims / `Loc::Param`
//! - Cold ops (`ArrayFrom`, spread calls, `DeleteIndex`, `JSON`, `super`, `ToPropKey`, `InitDataPropDyn`, `ArrayRest`)
//! - Inlined `CheckCoercible` / `GetIndex` / `HasProp` / `IsArray` + `arith_fb` + TypedArray reg-homed loops
//!
//! Expected output lines are Node 24's.

const PROGRAM: &str = r#"
"use strict";

function testTypeOfIs() {
  function fnWithProp(x) { return x + 1; }
  fnWithProp.tag = "custom";
  class StaticCls { static id = 7; }
  const items = [Uint8Array, Uint16Array, Array, Object, fnWithProp, StaticCls, { a: 1 }, "hi", 42, null, undefined, true];
  let fns = 0, objs = 0, strs = 0;
  function check(v) {
    if (typeof v === "function") return 1;
    if (typeof v === "object") return 2;
    if (typeof v === "string") return 3;
    return 0;
  }
  for (let i = 0; i < 400; i++) {
    const r = check(items[i % items.length]);
    if (r === 1) fns++;
    else if (r === 2) objs++;
    else if (r === 3) strs++;
  }
  return `typeof:${fns}:${objs}:${strs}`;
}

globalThis.ROUTE_G = 10;
function readRouteGlobal(x) { return x + ROUTE_G; }
function callerRouteGlobal(x) { return readRouteGlobal(x) * 2; }
function testGlobalInlineRoute() {
  let sum = 0;
  for (let i = 0; i < 400; i++) {
    if (i === 320) {
      Object.defineProperty(globalThis, "ROUTE_G", { get() { return 100; }, configurable: true });
    }
    sum += callerRouteGlobal(i & 7);
  }
  delete globalThis.ROUTE_G;
  return `route:${sum}`;
}

function testApplyAndGetters() {
  class Box {
    constructor(v) { this._v = v; }
    get val() { return this._v * 3; }
    add(a, b) { return this._v + a + b; }
  }
  class SubBox extends Box {
    get val() { return this._v * 5; }
  }
  function sum3(a, b, c) { return (this ? this.base : 0) + a + b + c; }
  function runApply(fn, ctx, arr, box) {
    const x = fn.apply(ctx, arr);
    const y = fn.call(ctx, 1, 2, 3);
    const g = box.val;
    const ch = "hello".charCodeAt(1);
    return x + y + g + ch;
  }
  const b1 = new Box(4);
  const b2 = new SubBox(4);
  const ctx = { base: 10 };
  let acc = 0;
  for (let i = 0; i < 400; i++) {
    acc += runApply(sum3, ctx, [1, 2, i & 3], i < 320 ? b1 : b2);
  }
  return `apply:${acc}`;
}

function testSpillsAndClosures() {
  function heavy(p0, p1, p2, p3) {
    const a0 = p0 + 1, a1 = p1 + 2, a2 = p2 + 3, a3 = p3 + 4;
    const a4 = a0 + a1, a5 = a2 + a3, a6 = a0 * 2, a7 = a1 * 3;
    const a8 = a2 * 4, a9 = a3 * 5, a10 = a4 + a5, a11 = a6 + a7;
    const fn = (k) => k + a0 + a11;
    const mid = fn(p0) + Math.max(a8, a9);
    let s = 0;
    for (let i = 0; i < 4; i++) {
      const t = (a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + i) | 0;
      s = (s + t + p1 + p2 + p3) | 0;
    }
    return (s + mid + p0) | 0;
  }
  let total = 0;
  for (let i = 0; i < 400; i++) {
    total = (total + heavy(i & 15, (i + 1) & 15, (i + 2) & 15, (i + 3) & 15)) | 0;
  }
  return `spills:${total}`;
}

function testColdOpsAndSuper() {
  class Base {
    constructor(x) { this.x = x; }
  }
  class Derived extends Base {
    constructor(x, y) {
      super(x + 1);
      this.y = y;
    }
  }
  function pushSpread(arr, extra) {
    arr.push(...extra);
    return arr.length;
  }
  function work(i) {
    const d = new Derived(i, i * 2);
    const a = Array.from([1, 2, i & 7]);
    const [head, ...rest] = a;
    const k = "k" + (i & 3);
    const obj = { [k]: d.x + d.y + head + rest.length };
    delete a[1];
    const len = pushSpread(a, [9, 10]);
    const j = JSON.parse(JSON.stringify(obj));
    return j[k] + len;
  }
  let acc = 0;
  for (let i = 0; i < 400; i++) acc += work(i);
  return `cold:${acc}`;
}

function testInlineAndTypedArray() {
  const MIME = Symbol("mime");
  function ignoreCase({ ownerDocument }) {
    return ownerDocument[MIME].ignoreCase;
  }
  function getTag(el, k) {
    const ic = ignoreCase(el);
    const has = "name" in el;
    const arr = Array.isArray(el.list);
    return (ic && has && !arr ? el.name : "x") + ":" + k;
  }
  const doc = { [MIME]: { ignoreCase: true } };
  const el = { ownerDocument: doc, name: "div", list: null };
  let lenSum = 0;
  for (let i = 0; i < 400; i++) {
    lenSum += getTag(el, i & 7).length;
  }
  const w = new Float32Array(16);
  const x = new Float32Array(16);
  for (let i = 0; i < 16; i++) { w[i] = (i + 1) * 0.25; x[i] = i * 0.5; }
  function affine(a, b) {
    let s = 0;
    for (let i = 0; i < a.length; i++) s += a[i] * b[i];
    return s;
  }
  let af = 0;
  for (let i = 0; i < 400; i++) af += affine(w, x);
  return `inline_ta:${lenSum}:${Math.round(af)}`;
}

function testEnumSwitchAndConstProps() {
  const State = { ZERO: 0, ONE: 1, TWO: 2, THREE: 3, FOUR: 4, FIVE: 5 };
  function step(s) {
    switch (s) {
      case State.ZERO: return 10;
      case State.ONE: return 20;
      case State.TWO: return 30;
      case State.THREE: return 40;
      case State.FOUR: return 50;
      case State.FIVE: return 60;
      default: return -1;
    }
  }
  let acc = 0;
  for (let i = 0; i < 400; i++) {
    if (i === 320) State.TWO = 99;
    const x = i === 300 ? -0 : (i % 6);
    acc += step(x);
  }
  return `enum_sw:${acc}`;
}

globalThis.CP_TBL = { A: 65, Z: 90, a: 97, z: 122 };
function isUpper(cp) { return cp >= CP_TBL.A && cp <= CP_TBL.Z; }
function isLower(cp) { return cp >= CP_TBL.a && cp <= CP_TBL.z; }
function isLetter(cp) { return isUpper(cp) || isLower(cp); }
function testDepth2AndConstPropMutation() {
  let count = 0;
  function scan(cp) { return isLetter(cp) ? 1 : 0; }
  for (let i = 0; i < 400; i++) {
    if (i === 320) CP_TBL.Z = 70;
    count += scan(65 + (i % 64));
  }
  delete globalThis.CP_TBL;
  return `depth2:${count}`;
}

function testSlowGlobalsAndStrLen() {
  Object.defineProperty(globalThis, "SLOW_G_ACC", {
    get() { return 7; },
    configurable: true,
  });
  function probe(s, k) {
    const t = typeof UNDECLARED_SLOW_G_XYZ;
    const g = SLOW_G_ACC;
    return s.length + (t === "undefined" ? g : 0) + (s + k).length;
  }
  const strs = ["", "abc", "hello world!", "\u{1F600}x", "a".repeat(20) + "b".repeat(20), [1, 2, 3, 4]];
  let acc = 0;
  for (let i = 0; i < 400; i++) {
    acc += probe(strs[i % strs.length], i & 3);
  }
  delete globalThis.SLOW_G_ACC;
  return `slow_len:${acc}`;
}

console.log(testTypeOfIs());
console.log(testGlobalInlineRoute());
console.log(testApplyAndGetters());
console.log(testSpillsAndClosures());
console.log(testColdOpsAndSuper());
console.log(testInlineAndTypedArray());
console.log(testEnumSwitchAndConstProps());
console.log(testDepth2AndConstPropMutation());
console.log(testSlowGlobalsAndStrLen());
"#;

const EXPECTED: &[&str] = &[
    "typeof:202:66:33",
    "route:25200",
    "apply:58040",
    "spills:587100",
    "cold:243000",
    "inline_ta:2000:68000",
    "enum_sw:13526",
    "depth2:298",
    "slow_len:11618",
];

fn run_ok(source: &str) -> Vec<String> {
    zipp_vm::run(source).expect("program runs").output
}

#[test]
fn opt_tier_parity_matches_node() {
    assert_eq!(run_ok(PROGRAM), EXPECTED);
}

#[test]
fn opt_tier_parity_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, envs) in [
        ("eager", vec![("ZIPP_JIT_THRESHOLD", "1")]),
        ("stress", vec![("ZIPP_GC_STRESS", "1")]),
        ("nojit", vec![("ZIPP_NOJIT", "1")]),
        ("nursery_verify", vec![("ZIPP_NURSERY_VERIFY", "1")]),
        ("opt", vec![("ZIPP_OPT", "1")]),
        ("opt_deopt", vec![("ZIPP_OPT", "1"), ("ZIPP_OPT_DEOPT_EVERY", "5")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("opt_tier_parity_matches_node").arg("--nocapture");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
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
