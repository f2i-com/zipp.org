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

globalThis.HOIST_CC = { LT: 60, GT: 62, SP: 32 };
function testCharCodeAtAndHoist() {
  function tok(obj, n) {
    let tags = 0;
    let sum = 0;
    for (let i = 0; i < n; i++) {
      const s = obj.buf;
      const c = s.charCodeAt(i);
      if (c === HOIST_CC.LT) tags++;
      else if (c === HOIST_CC.GT) tags += 2;
      else if (c === HOIST_CC.SP) tags += 3;
      if (c === c) sum += c;
    }
    return tags * 100000 + sum;
  }
  const o1 = { buf: "<div class=\"a b\">hello</div>" };
  const rope = "<p>" + "x".repeat(24) + "</p>";
  const o2 = { buf: rope };
  const o3 = { buf: "<\u{1F600}>" };
  let acc = 0;
  for (let i = 0; i < 400; i++) {
    if (i === 300) HOIST_CC.SP = 97;
    if (i === 350) acc += tok(null, 0);
    const o = (i % 7 === 0) ? o2 : (i % 11 === 0) ? o3 : o1;
    const n = (i % 13 === 0) ? o.buf.length + 2 : o.buf.length;
    acc += tok(o, n);
  }
  function negAndOverride(s, k) {
    const a = s.charCodeAt(k);
    return a === a ? a : -1;
  }
  let extra = 0;
  for (let i = 0; i < 400; i++) {
    extra += negAndOverride("abc", i === 320 ? -1 : (i % 4));
  }
  const orig = String.prototype.charCodeAt;
  String.prototype.charCodeAt = function(k) { return 777 + k; };
  extra += negAndOverride("abc", 2);
  String.prototype.charCodeAt = orig;
  delete globalThis.HOIST_CC;
  return `cca_hoist:${acc}:${extra}`;
}

function testVolPreserveColdPaths() {
  let up = 0;
  function crunch(o, arr, s1, s2, m1, m2, k1, k2, k3, k4) {
    let a = k1 + 1, b = k2 + 2, c = k3 + 3, d = k4 + 4;
    let e = k1 * 2, f = k2 * 3;
    let x = m1 + 0.5, y = m2 + 0.25;
    let p = o.x;
    o.y = a;
    let len = arr.length;
    let elem = arr[k1 & 3];
    if (typeof arr !== "string") arr[k1 & 3] = elem;
    let modv = (x * -2) % y;
    let cmp = s1 < s2 ? 1 : 0;
    let cat = "p" + s1 + (k1 & 3);
    up = b;
    return (a + b + c + d + e + f) + (p | 0) + len + (typeof elem === "string" ? elem.charCodeAt(0) : (elem | 0)) + (modv === 0 ? 1 : 2) + cmp + cat.length + up;
  }
  let acc = 0;
  const oMono = { x: 10, y: 0 };
  const oAlt = { z: 1, x: 20, y: 0 };
  const arr = [1, 2, 3, 4];
  const ta = new Int32Array([5, 6, 7, 8]);
  for (let i = 0; i < 400; i++) {
    const o = (i === 320) ? oAlt : oMono;
    const a = (i === 330) ? ta : (i === 340) ? "abcd" : arr;
    const s1 = (i === 350) ? "bbb" : (i & 3);
    const s2 = (i === 350) ? "aaa" : ((i + 1) & 3);
    const m1 = (i === 360) ? -4.5 : (i % 7);
    const m2 = (i === 360) ? 1.5 : 2;
    acc += crunch(o, a, s1, s2, m1, m2, i & 7, (i + 1) & 7, (i + 2) & 7, (i + 3) & 7);
  }
  const S1 = Symbol("s1"), S2 = Symbol("s2"), S3 = Symbol("s3");
  class BaseSym { constructor(a) { this.a = a; } }
  class SubSym extends BaseSym {
    constructor(a, b) {
      super(a);
      this[S1] = b;
      this.meta = { [S1]: a, [S2]: b, [S3]: this };
      this[S2] = a + b;
    }
  }
  let symAcc = 0;
  for (let i = 0; i < 400; i++) {
    const x = new SubSym(i, i + 1);
    symAcc += x[S1] + x[S2] + x.meta[S1] + x.meta[S2] + x.meta[S3].a;
  }
  return `vol_cold:${acc}:${symAcc}`;
}

function testPackedStrAndPureLen() {
  // Exercise 1..=8 byte inline ASCII packing (including byte 7 / bit 63),
  // >8 byte heap ASCII, non-ASCII, and dynamic slices across ===, s[i], charCodeAt.
  const base = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
  let hits = 0, ccSum = 0;
  function checkTag(t, idx) {
    const eq1 = (t === "ABCDEFGH") ? 1 : 0;
    const eq2 = (t === "ABCDEFGI") ? 2 : 0;
    const eq3 = (t === "ABCDEFG") ? 4 : 0;
    const eq4 = (t === "A") ? 8 : 0;
    const eq5 = (t === "0123456789ABCDEF") ? 16 : 0;
    const c = t.charCodeAt(idx);
    const ch = t[idx];
    return (eq1 + eq2 + eq3 + eq4 + eq5) + (c === c ? c : 0) + (ch === "H" ? 100 : 0);
  }
  const s8 = base.slice(10, 18); // "ABCDEFGH" (dynamic 8B ASCII)
  const s8b = base.slice(10, 17) + "I"; // "ABCDEFGI" (8B diff at byte 7!)
  const s7 = base.slice(10, 17); // "ABCDEFG"
  const s1 = base.slice(10, 11); // "A"
  const s16 = base.slice(0, 16); // "0123456789ABCDEF"
  const sUni = "A\u00e9CDEFGH"; // non-ASCII len 8
  for (let i = 0; i < 400; i++) {
    const t = (i % 6 === 0) ? s8 : (i % 6 === 1) ? s8b : (i % 6 === 2) ? s7
            : (i % 6 === 3) ? s1 : (i % 6 === 4) ? s16 : sUni;
    const idx = i % t.length;
    ccSum += checkTag(t, idx);
    if (s8 === "ABCDEFGH") hits++;
    if (s8 === s8b) hits += 1000;
  }
  // Pure Op::Length hoisting + TypedArray + generic getter deopt fallback.
  function sumLen(x, n) {
    let s = 0;
    for (let i = 0; i < n; i++) s += x.length;
    return s;
  }
  const ta = new Float32Array(12);
  let getterHits = 0;
  const weird = { get length() { getterHits++; return 5; } };
  let lenSum = 0;
  for (let i = 0; i < 400; i++) {
    const x = (i === 350) ? weird : (i === 360) ? { length: 9 } : (i & 1) ? ta : s8;
    lenSum += sumLen(x, (i === 350) ? 3 : 4);
  }
  return `packed_len:${hits}:${ccSum}:${lenSum}:${getterHits}`;
}

function testReplanBackedgeAndOps() {
  // 1. Mixed packed-vs-heap 1..=8B ASCII === (one side promoted by charCodeAt).
  const dynHeap = ("xx" + "DIV".toLowerCase()).slice(2);
  let cc = dynHeap.charCodeAt(0);
  let mixHits = 0;
  for (let i = 0; i < 300; i++) {
    if (dynHeap === "div") mixHits++;
    if ("div" === dynHeap) mixHits++;
    if (dynHeap === "diw") mixHits += 1000;
  }
  // 2. Loop-backedge Tier O tier-up on a low-entry-count function.
  function loopFewCalls(n) {
    let s = 0;
    for (let i = 0; i < n; i++) s = (s + i) | 0;
    return s;
  }
  let loopSum = 0;
  for (let k = 0; k < 12; k++) loopSum += loopFewCalls(100);
  // 3. Polymorphic GetProp replanning: 1-shape -> 4-shape GetPropPoly -> 5th shape mega_prop.
  function readTag(o) { return o.tag; }
  const s1 = { tag: 1 };
  const s2 = { a: 0, tag: 2 };
  const s3 = { a: 0, b: 0, tag: 3 };
  const s4 = { tag: 4, c: 0 };
  const s5 = { d: 1, e: 2, f: 3, g: 4, tag: 5 };
  let polySum = 0;
  for (let i = 0; i < 300; i++) polySum += readTag(s1);
  for (let i = 0; i < 400; i++) {
    const o = (i & 3) === 0 ? s1 : (i & 3) === 1 ? s2 : (i & 3) === 2 ? s3 : s4;
    polySum += readTag(o);
  }
  for (let i = 0; i < 100; i++) polySum += readTag(s5);
  // 4. Private #field, IterToArray destructuring, GlobalFn::String(1), StrAppendInPlace.
  class Box {
    #v;
    constructor(x) { this.#v = x; }
    bump(pair) {
      const [a, b] = pair;
      this.#v += a + b;
      let s = "";
      for (let k = 0; k < 3; k++) s += String(a);
      return this.#v + s.length;
    }
  }
  const box = new Box(0);
  let privSum = 0;
  for (let i = 0; i < 300; i++) privSum += box.bump([1, 2]);
  // 5. Direct Tier O StrAppendInPlace (with both string and non-string rhs fallback).
  function joinTicks(n, suffix) {
    let acc = "a";
    for (let k = 0; k < n; k++) acc += (k & 1) ? suffix : k;
    return acc.length;
  }
  let appendSum = 0;
  for (let i = 0; i < 300; i++) appendSum += joinTicks(6, "bc");
  return `replan_ops:${mixHits}:${cc}:${loopSum}:${polySum}:${privSum}:${appendSum}`;
}

function testInlineAddWaySymAddAndDerived() {
  const NEXT = Symbol("next");
  const PREV = Symbol("prev");
  class Base {
    constructor(tag, v) {
      this.tag = tag;
      this.v = v;
      this[NEXT] = null;
      this[PREV] = v + 1;
    }
  }
  class Elem extends Base {
    constructor(tag, v) {
      super(tag, v);
      this.a = v + 2;
      this.b = v + 3;
      this.c = v + 4;
      this.d = v + 5;
    }
  }
  class Div extends Elem {}
  class Span extends Elem {}
  class Img extends Div {}
  let sum = 0;
  for (let i = 0; i < 300; i++) {
    const o = (i % 3 === 0) ? new Div("div", i) : (i % 3 === 1) ? new Span("span", i) : new Img("img", i);
    o[NEXT] = o.a + o.d;
    sum += o.v + o[PREV] + o[NEXT] + o.b + o.c;
  }
  const customProto = { base: 7 };
  let plainSum = 0;
  for (let i = 0; i < 200; i++) {
    const p1 = {};
    p1.x = i; p1.y = i + 1; p1.z = i + 2; p1.w = i + 3; p1.q = i + 4;
    const p2 = Object.create(customProto);
    p2.x = i; p2.y = i + 2;
    const p3 = Object.create(null);
    p3.x = i; p3.y = i + 3;
    plainSum += p1.q + p2.y + p2.base + p3.y;
  }
  let guardSum = 0;
  function setFields(o, k) {
    "use strict";
    let caught = 0;
    try { o.extra = k; } catch (e) { caught += 1; }
    try { o[NEXT] = k; } catch (e) { caught += 10; }
    return caught;
  }
  for (let i = 0; i < 200; i++) {
    const d = new Div("d", i);
    if (i === 150) {
      const f = Object.preventExtensions(Object.create(Div.prototype));
      guardSum += setFields(f, i);
    } else {
      guardSum += setFields(d, i);
    }
  }
  let setHits = 0;
  let symHits = 0;
  function addDyn(o, v) {
    o.dyn = v;
    o[PREV] = v + 1;
  }
  for (let i = 0; i < 250; i++) {
    const o = Object.create(Elem.prototype);
    if (i === 200) {
      Object.defineProperty(Elem.prototype, "dyn", { set(v) { setHits += v; }, configurable: true });
      Object.defineProperty(Elem.prototype, PREV, { set(v) { symHits += v; }, configurable: true });
    }
    addDyn(o, i);
  }
  class AltBase {
    constructor(tag, v) { this.alt = v * 10; }
  }
  class Rewired extends Div {}
  let rewSum = 0;
  for (let i = 0; i < 220; i++) {
    if (i === 180) Object.setPrototypeOf(Rewired, AltBase);
    const r = new Rewired("r", i);
    rewSum += (r.alt !== undefined ? r.alt : r.v);
  }
  class Many extends Array {
    constructor(...xs) { super(...xs); }
  }
  let manySum = 0;
  for (let i = 0; i < 100; i++) {
    const m = new Many(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
    const re = new RegExp("a+", "g");
    manySum += m.length + (re.global ? 1 : 0);
  }
  return `addway_derived:${sum}:${plainSum}:${guardSum}:${setHits}:${symHits}:${rewSum}:${manySum}`;
}

function testPolyClosureAndInlineMega() {
  let want = "div";
  const adapter = {
    getName(el) { return el.localName; },
    getLen(el) { return el.tags.length; },
  };
  class ClassAdapter {
    getName(el) { return el.localName === want ? want : el.localName; }
  }
  const cadapter = new ClassAdapter();

  function makeTagPred(tag) {
    return (el) => el.localName === tag;
  }
  const pTrue = () => true;
  const pDiv = makeTagPred("div");
  const pSpan = (el) => el.localName === want;
  const pLen = (el) => el.tags.length > 1 && el.localName !== "em";
  const pFifth = (el) => (el.extra ? 1 : 0) + el.tags.length;

  function runMatch(a, el, test) {
    const nm = a.getName(el);
    if (!nm) return 0;
    return test(el) ? (nm.length + a.getLen(el)) : 0;
  }
  function runClassMatch(a, el, test) {
    const nm = a.getName(el);
    return test(el) ? nm.length : -1;
  }

  const es5Proto = { localName: "protoTag", tags: "abc" };
  const es5Obj = Object.create(es5Proto);
  es5Obj.x = 1;
  const nullObj = Object.create(null);
  nullObj.localName = "nullTag";
  nullObj.tags = [1, 2];
  class NodeCls {
    constructor(n, t) { this.localName = n; this.tags = t; }
  }
  const nodes = [
    { localName: "div", tags: [1, 2], a: 1 },
    { localName: "span", tags: [1], b: 2 },
    { localName: "p", tags: [1, 2, 3], c: 3 },
    { localName: "a", tags: "xy", d: 4 },
    { localName: "em", tags: "éêë", e: 5 },
    { localName: "div", tags: [1, 2, 3, 4], f: 6, g: 7 },
    es5Obj,
    nullObj,
    new NodeCls("span", [9, 8]),
    { tags: [1], h: 8 },
  ];
  const preds = [pTrue, pDiv, pSpan, pLen];
  let sum = 0;
  for (let i = 0; i < 600; i++) {
    if (i === 300) want = "span";
    const el = nodes[i % nodes.length];
    const p = preds[i & 3];
    sum += runMatch(adapter, el, p);
    sum += runClassMatch(cadapter, el, p);
  }
  let fifthSum = 0;
  for (let i = 0; i < 50; i++) {
    const el = nodes[i % (nodes.length - 1)];
    fifthSum += runMatch(adapter, el, pFifth);
  }
  let getterHits = 0;
  const getterObj = {
    get localName() { getterHits++; return "div"; },
    tags: { get length() { getterHits += 10; return 3; } },
  };
  const getterRes = runMatch(adapter, getterObj, pDiv) + runMatch(adapter, getterObj, pLen);

  let nullErrs = 0;
  try { runMatch(adapter, null, pDiv); } catch (e) { nullErrs++; }

  function callWithThisPoly(fn, obj, x) {
    return fn.call(obj, x);
  }
  function makeArrows(prefix) {
    const o = {
      prefix,
      make1() { return (x) => this.prefix + x; },
      make2() { return (x) => this.prefix * 2 + x; },
    };
    return [o.make1(), o.make2()];
  }
  const [arrA, arrB] = makeArrows(10);
  const [arrC, arrD] = makeArrows(25);
  const arrowList = [arrA, arrB, arrC, arrD];
  let arrowSum = 0;
  function invokeArrow(fn, x) { return fn(x); }
  for (let i = 0; i < 600; i++) {
    arrowSum += invokeArrow(arrowList[i & 3], i & 7);
    arrowSum += callWithThisPoly(arrowList[i & 3], { prefix: 999 }, 1);
  }

  let tdzErrs = 0;
  class Base { constructor(v) { this.v = v; } }
  class Sub extends Base {
    constructor(readBefore) {
      const noThis = (k) => k + 1;
      const withThis = () => this.v;
      let acc = 0;
      for (let i = 0; i < 300; i++) acc += invokeArrow(noThis, 1);
      if (readBefore) {
        try { invokeArrow(withThis, 0); } catch (e) { tdzErrs++; }
      }
      super(acc);
      this.after = invokeArrow(withThis, 0);
    }
  }
  const s1 = new Sub(true);
  const s2 = new Sub(false);

  let upTdzErrs = 0;
  {
    const readLate = () => lateVal;
    function callIt(f) { return f(); }
    for (let i = 0; i < 300; i++) {
      try { callIt(readLate); } catch (e) { upTdzErrs++; }
    }
    let lateVal = 42;
    upTdzErrs += callIt(readLate);
  }

  return `poly_inline:${sum}:${fifthSum}:${getterHits}:${getterRes}:${nullErrs}:${arrowSum}:${tdzErrs}:${s1.after}:${s2.after}:${upTdzErrs}`;
}

function testStrCaseInline() {
  const getName = (el) => {
    const { localName } = el;
    return el.ic ? localName.toLowerCase() : localName.toUpperCase();
  };
  const tagMatcher = (el, want) => getName(el) === want;
  const items = [
    { localName: "", ic: true },
    { localName: "p", ic: true },
    { localName: "div", ic: true },
    { localName: "noscript", ic: true },
    { localName: "blockquote", ic: true },
    { localName: "DIV", ic: true },
    { localName: "AbCdEfGhIj", ic: true },
    { localName: "ÉTAG", ic: true },
    { localName: "SPAN", ic: false },
    { localName: "BLOCKQUOTE", ic: false },
    { localName: "article", ic: false },
    { localName: { toLowerCase() { return "custom"; } }, ic: true },
  ];
  let hits = 0;
  let lenSum = 0;
  for (let i = 0; i < 600; i++) {
    const it = items[i % items.length];
    const r = getName(it);
    lenSum += r.length;
    if (tagMatcher(it, "div") || tagMatcher(it, "blockquote") || tagMatcher(it, "SPAN") || tagMatcher(it, "custom")) {
      hits++;
    }
  }
  const origLower = String.prototype.toLowerCase;
  String.prototype.toLowerCase = function () { return "over:" + this; };
  const overRes = getName({ localName: "div", ic: true });
  String.prototype.toLowerCase = origLower;
  const restRes = getName({ localName: "div", ic: true });
  return `strcase:${hits}:${lenSum}:${overRes}:${restRes}`;
}

function testForOfFinallyAndDeoptFeedback() {
  let closed = 0;
  let finCount = 0;

  function findArr(items, target) {
    for (const x of items) {
      if (x === target) return x * 10;
    }
    return -1;
  }
  function breakNested(grid, target) {
    let seen = 0;
    outer: for (const row of grid) {
      for (const v of row) {
        seen += v;
        if (v === target) break outer;
      }
    }
    return seen;
  }
  function returnThroughUserFinally(items, target) {
    try {
      for (const x of items) {
        if (x === target) return x + 100;
      }
      return 0;
    } finally {
      finCount++;
    }
  }
  function makeCustomIter(arr) {
    let i = 0;
    return {
      [Symbol.iterator]() { return this; },
      next() { return i < arr.length ? { value: arr[i++], done: false } : { value: undefined, done: true }; },
      return() { closed++; return { value: undefined, done: true }; },
    };
  }
  function* gen(arr) {
    try {
      for (const x of arr) yield x;
    } finally {
      closed += 10;
    }
  }
  const ownRetArr = [1, 2, 3, 4];
  ownRetArr.return = function() { closed += 100; return { value: undefined, done: true }; };
  const weirdIterable = {
    [Symbol.iterator]() {
      let i = 0;
      const a = [1, 2, 3];
      a.next = () => i < a.length ? { value: a[i++], done: false } : { value: undefined, done: true };
      a.return = () => { closed += 1000; return { value: undefined, done: true }; };
      return a;
    },
  };

  const arr = [1, 2, 3, 4, 5];
  const grid = [[1, 2], [3, 4], [5, 6]];
  const grid3 = [[1, 2, 3], [4, 5, 6], [7, 8, 9]];
  const st = new Set([1, 2, 3, 4]);
  function continueNested(g, skip) {
    let acc = 0;
    outer: for (const row of g) {
      for (const v of row) {
        if (v === skip) continue outer;
        if ((v & 1) === 0) continue;
        acc += v;
      }
    }
    return acc;
  }
  let sum = 0;
  let contSum = 0;
  for (let i = 0; i < 400; i++) {
    sum += findArr(arr, (i % 5) + 1);
    sum += findArr(st, (i % 4) + 1);
    sum += breakNested(grid, (i % 6) + 1);
    sum += returnThroughUserFinally(arr, (i % 5) + 1);
    contSum += continueNested(grid3, (i % 9) + 1);
  }
  sum += findArr("abcde", "c").length || 0;
  sum += findArr(makeCustomIter([1, 2, 3]), 2);
  sum += findArr(gen([1, 2, 3]), 2);
  sum += findArr(weirdIterable, 2);
  sum += findArr(ownRetArr, 2);

  function readVal(o) { return o.val + 1; }
  function altRead(o) { return o.val + 2; }
  function invokeMono(fn, o) { return fn(o); }
  let callSum = 0;
  const gObj = { get val() { return 5; } };
  for (let i = 0; i < 400; i++) {
    const fn = (i >= 320 && (i & 1)) ? altRead : readVal;
    const o = (i >= 280 && i < 360 && (i % 3 === 0)) ? gObj : { val: i & 7 };
    callSum += invokeMono(fn, o);
  }
  return `forof_fin:${sum}:${contSum}:${closed}:${finCount}:${callSum}`;
}

function testD3DirectRegisterCalls() {
  "use strict";
  function leaf3(a, b, c) {
    return (a | 0) + (b === undefined ? 10 : b) + (c ? 3 : 1);
  }
  function swapArgs(a, b, c) {
    const t = leaf3(b, a, c);
    return t + leaf3(a, undefined, !c);
  }
  function makeAdder(base) {
    let bump = base + 1;
    return function add(x, y) {
      return bump + x + (y || 0);
    };
  }
  const add10 = makeAdder(10);
  const add20 = makeAdder(20);

  function strictThis(x) {
    return (this && typeof this === "object" ? this.v : (this === undefined ? -1 : -2)) + x;
  }
  const sloppyThis = new Function("x", "return (typeof this === 'object' && this !== null && 'v' in this ? this.v : (this === globalThis ? 100 : -9)) + x;");
  function callPlain(fn, x) { return fn(x); }

  function makeArrowHost(base) {
    return {
      base,
      getArrow() { return (x) => this.base + x; }
    };
  }
  const host = makeArrowHost(7);
  const lexArrow = host.getArrow();

  class BaseBox {
    constructor(k, v) { this.k = k; this.v = v; }
    check(el) { return (el instanceof BaseBox) ? (this.v + el.v) : 0; }
  }
  class SubBox extends BaseBox {
    constructor(k, v) { super(k, v); }
    run(el) { return this.check(el); }
  }
  const b1 = new SubBox("a", 3);
  const b2 = new SubBox("b", 5);
  const b3 = new BaseBox("c", 7);
  b1.strictM = strictThis;
  b2.sloppyM = sloppyThis;

  function orNode(a, b) {
    return function testOr(el) {
      return a(el) || b(el);
    };
  }
  function makeTagMatch(tag, next) {
    return function tagMatcher(el) {
      return el.k === tag && next(el);
    };
  }
  function alwaysTrue(el) { return el instanceof BaseBox; }
  const reZ = /^z/;
  function regexLeaf(el) { return reZ.test(el.k); }

  let pureTree = makeTagMatch("t0", alwaysTrue);
  for (let i = 1; i < 6; i++) {
    pureTree = orNode(pureTree, makeTagMatch("t" + i, alwaysTrue));
  }
  let taintedTree = regexLeaf;
  for (let i = 1; i < 6; i++) {
    taintedTree = orNode(taintedTree, makeTagMatch("q" + i, alwaysTrue));
  }
  const elPureHit = new BaseBox("t4", 1);
  const elPureMiss = new BaseBox("miss", 1);
  const elTaintHit = new BaseBox("z9", 1);
  const elTaintMiss = new BaseBox("nope", 1);

  function factSum(n) {
    if (n <= 0) return 0;
    return n + factSum(n - 1);
  }

  let getterHits = 0;
  function sharedM(x) { return this.k + x; }
  function nonD3M(x) { return String(this.k) + x; }
  const methObjA = { k: 1, m: sharedM };
  const methObjB = { k: 2, pad: 0, m: sharedM };
  const methObjC = { k: 3, p1: 0, p2: 0, m: sharedM };
  const methObjD = { k: 4, p1: 0, p2: 0, p3: 0, m: sharedM };
  const methObjE = { k: 5, p1: 0, p2: 0, p3: 0, p4: 0, m: nonD3M };
  const methGetter = {
    k: 6,
    get m() {
      getterHits++;
      return sharedM;
    }
  };
  const methArr = [methObjA, methObjB, methObjC, methObjD, methObjE];
  function callM(o, x) { return o.m(x); }

  globalThis.__d3RouteVal = 11;
  function readGlobalD3(x) { return __d3RouteVal + x; }

  let acc = 0;
  let treeAcc = 0;
  let methAcc = 0;
  let globalAcc = 0;
  for (let i = 0; i < 400; i++) {
    acc += swapArgs(i & 7, (i & 3) + 0.5, (i & 1) === 0);
    acc += (i & 1 ? add10 : add20)(i & 7, i & 3);
    acc += lexArrow(i & 7);
    acc += b1.run(i & 1 ? b2 : b3);
    acc += callPlain(strictThis, 1);
    acc += callPlain(sloppyThis, 1);
    acc += b1.strictM(1);
    acc += b2.sloppyM(1);
    treeAcc += pureTree(i & 1 ? elPureHit : elPureMiss) ? 1 : 0;
    treeAcc += taintedTree(i & 1 ? elTaintHit : elTaintMiss) ? 2 : 0;
    const mr = callM(methArr[i % 5], i & 3);
    methAcc += typeof mr === "string" ? mr.length : mr;
    if (i === 320) {
      let gReads = 0;
      Object.defineProperty(globalThis, "__d3RouteVal", {
        configurable: true,
        get() { gReads++; return 20 + gReads; }
      });
    }
    globalAcc += readGlobalD3(1);
  }
  methAcc += callM(methGetter, 4);
  methAcc += callM(methGetter, 5);
  const recVal = factSum(20);
  delete globalThis.__d3RouteVal;
  return `d3_calls:${Math.round(acc)}:${treeAcc}:${methAcc}:${getterHits}:${globalAcc}:${recVal}`;
}

function testD3BuiltinsGettersAndCall() {
  const SYM = Symbol("v");
  class Node {
    constructor(val, cls, href, parent) {
      this[SYM] = val;
      this.cls = cls;
      this.href = href;
      this._p = parent;
    }
    get val() { return this[SYM]; }
    get parent() { return this._p; }
    get allocList() { return [this[SYM], 1]; }
  }
  const re = /(?:^|\s)ad-banner(?:$|\s)/;
  const reG = /foo/g;
  const tagSet = new Set(["div", "span"]);
  const scoreMap = new Map([["div", 3], ["span", 5]]);
  const wm = new WeakMap();
  const ws = new WeakSet();
  const tagsArr = ["div", "span", "p"];

  function checkClass(n, pfx, sfx) {
    const c = n.cls;
    const h = n.href;
    if (c === "THROW_ME") throw new TypeError("cold-throw");
    return (c != null && re.test(c))
      || (h != null && (h.startsWith(pfx) || h.endsWith(sfx) || h.includes("mid")));
  }
  function checkColl(n, tag) {
    const s = scoreMap.get(tag) || 0;
    const h = tagSet.has(tag) ? 1 : 0;
    const w = wm.get(n) || 0;
    const wh = ws.has(n) ? 1 : 0;
    const a = tagsArr.includes(tag) ? 1 : 0;
    return s + h + w + wh + a;
  }
  function readGetter(n) {
    const p = n.parent;
    return n.val + (p ? p.val : 0);
  }
  function readNonD3Getter(n) {
    return n.allocList.length;
  }
  function addThis(x, y) { return this.k + x + y; }
  function viaCall(fn, ctx, a, b) {
    const r1 = fn.call(ctx, a, b);
    const r2 = fn.call(ctx, a + 1, b);
    return r1 + r2;
  }
  function testGlobalRe(s) { return reG.test(s) ? 1 : 0; }
  function orNode(a, b) { return function(n) { return a(n) || b(n); }; }
  const tree = orNode(
    (n) => n.parent !== null && checkClass(n, "/ad/", ".png"),
    (n) => checkColl(n, n.val) > 5
  );

  const root = new Node("div", "normal", "/home", null);
  const n1 = new Node("span", "box ad-banner", "/notes", root);
  const n2 = new Node("p", null, "/ad/promo", root);
  const n3 = new Node("a", null, "/img/pic.png", root);
  const n4 = new Node("div", null, "/x/mid/y", root);
  wm.set(n1, 7);
  ws.add(n4);
  const nodes = [root, n1, n2, n3, n4];
  const ctx = { k: 10 };

  let acc = 0, gAcc = 0, callAcc = 0, reGAcc = 0, allocAcc = 0;
  for (let i = 0; i < 400; i++) {
    const n = nodes[i % 5];
    if (tree(n)) acc += 1;
    acc += checkColl(n, n.val);
    gAcc += readGetter(n).length;
    allocAcc += readNonD3Getter(n);
    callAcc += viaCall(addThis, ctx, i & 7, 1);
    reGAcc += testGlobalRe("foo bar foo");
  }
  let caught = 0;
  try {
    checkClass(new Node("x", "THROW_ME", "", null), "/a/", ".b");
  } catch (e) {
    if (e instanceof TypeError && e.message === "cold-throw") caught = 1;
  }
  return `d3_ext:${acc}:${gAcc}:${allocAcc}:${callAcc}:${reGAcc}:${caught}`;
}

function testRestApplyOnly() {
  function makeAny(checks) {
    return function any(...parameters) {
      let i = -1;
      while (++i < checks.length) {
        if (checks[i].apply(this, parameters)) return true;
      }
      return false;
    };
  }
  const cTag = (t) => function check(node, idx, parent) {
    return node && node.tag === t && idx >= 0 && parent !== null;
  };
  const anyTag = makeAny([cTag("h1"), cTag("h2"), cTag("div"), cTag("p")]);
  const nodes = [
    { tag: "span" }, { tag: "h2" }, { tag: "a" }, { tag: "p" }, { tag: "div" }
  ];
  const parent = { tag: "root" };
  let hits = 0;
  for (let i = 0; i < 400; i++) {
    if (anyTag(nodes[i % 5], i, parent)) hits += 1;
  }
  function sum5(a, b, c, d, e) {
    return (a | 0) + (b | 0) * 2 + (c | 0) * 3 + (d | 0) * 4 + (e | 0) * 5;
  }
  function fwd0(...rest) { return sum5.apply(this, rest); }
  function fwd1(k, ...rest) { return k * 100 + sum5.apply(this, rest); }
  function recFwd(depth, ...rest) {
    if (depth <= 0) return sum5.apply(this, rest);
    return recFwd.apply(this, [depth - 1].concat(rest));
  }
  function chainA(...p) { return chainB.apply(this, p); }
  function chainB(...p) { return sum5.apply(this, p); }
  let fwdAcc = 0;
  const heapArr = [1, 2, 3, 4, 5];
  for (let i = 0; i < 350; i++) {
    fwdAcc += fwd0() + fwd0(1) + fwd0(1, 2, 3, 4, 5);
    fwdAcc += fwd1(2, 1, 2, 3);
    fwdAcc += fwd0.call(null, 2, 3, 4);
    fwdAcc += fwd0.apply(null, heapArr);
    fwdAcc += fwd1.apply(null, [3, 4, 5]);
    fwdAcc += chainA(1, 2, 3);
  }
  const anyNative = makeAny([Array.isArray, (x) => x === 42]);
  let natHits = 0;
  for (let i = 0; i < 320; i++) {
    if (anyNative(i % 4 === 0 ? [i] : i % 5 === 0 ? 42 : i)) natHits += 1;
  }
  const seen = [];
  const spy1 = function () { return false; };
  const spy2 = function () { return false; };
  spy1.apply = function (t, arr) { seen.push(arr); return false; };
  spy2.apply = function (t, arr) { seen.push(arr); return true; };
  const anySpy = makeAny([spy1, spy2]);
  let spyOk = 0;
  for (let i = 0; i < 320; i++) {
    seen.length = 0;
    const r = anySpy(10, 20, 30);
    if (r && seen.length === 2 && seen[0] === seen[1] && Array.isArray(seen[0])
        && seen[0].length === 3 && seen[0][0] === 10 && seen[0][2] === 30) {
      spyOk += 1;
    }
  }
  function boom(x) { if (x < 0) throw new Error("neg"); return x * 2; }
  function safe(x, y) { return x + y; }
  function tryApply(...params) {
    try {
      return boom.apply(this, params);
    } catch (e) {
      return safe.apply(this, params);
    }
  }
  let tryAcc = 0;
  for (let i = 0; i < 320; i++) {
    tryAcc += tryApply(i % 7 === 0 ? -3 : 4, 10);
  }
  const sloppyTest = new Function("makeAny", "x",
    "var a = arguments;" +
    "var mutateAndCheck = makeAny([" +
    "  function mut() { a[0] = 999; return false; }," +
    "  function chk(v) { return v === 7; }" +
    "]);" +
    "return mutateAndCheck(x);"
  );
  let sloppyOk = 0;
  for (let i = 0; i < 320; i++) {
    if (sloppyTest(makeAny, 7)) sloppyOk += 1;
  }
  return `rest_apply:${hits}:${fwdAcc}:${natHits}:${spyOk}:${tryAcc}:${sloppyOk}:${recFwd(2, 1, 2, 3)}`;
}

function testHasPropStrictEqWeakMap() {
  const shaped = { div: 1, span: 2, p: 3 };
  const nullObj = Object.create(null);
  nullObj.div = 1;
  nullObj.span = 2;
  const dictObj = { div: 1, span: 2, tmp: 3 };
  delete dictObj.tmp;
  const keys = ["div", "span", "p", "missing", "toString", String.fromCharCode(35) + "priv"];
  function checkIn(o, k) { return k in o ? 1 : 0; }
  let inHits = 0;
  for (let i = 0; i < 360; i++) {
    const k = keys[i % 6];
    inHits += checkIn(shaped, k) + checkIn(nullObj, k) + checkIn(dictObj, k);
  }
  const protoKey = "__s11Test";
  checkIn(shaped, protoKey);
  checkIn(nullObj, protoKey);
  Object.prototype[protoKey] = 1;
  const pHit = checkIn(shaped, protoKey);
  const pNull = checkIn(nullObj, protoKey);
  delete Object.prototype[protoKey];
  const pAfter = checkIn(shaped, protoKey);

  const arr = [10, , 30, 40];
  const idxs = [0, 1, 2, 3, 4, -1];
  let arrIn = 0;
  for (let i = 0; i < 360; i++) {
    arrIn += checkIn(arr, idxs[i % 6]);
  }

  function eqCheck(x, y) { return x === y ? 1 : 0; }
  const a = "a";
  const aNul = "a\0";
  const aCopy = "a" + "";
  const d1 = "div";
  const d2 = "di" + "v";
  const long1 = "blockquote";
  const long2 = "block" + "quote";
  const o1 = {};
  const o2 = {};
  let eqHits = 0;
  for (let i = 0; i < 320; i++) {
    eqHits += eqCheck(a, aNul) + eqCheck(a, aCopy) + eqCheck(d1, d2) + eqCheck(d1, "span");
    eqHits += eqCheck(long1, long2) + eqCheck(long1, "figcaption") + eqCheck(o1, o2) + eqCheck(o1, o1);
  }

  class Item {
    constructor(a, b, c) {
      this.a = a;
      this.b = b;
      this.c = c;
    }
  }
  const wm = new WeakMap();
  const keep = [];
  let wmAcc = 0;
  for (let i = 0; i < 320; i++) {
    const it = new Item(i, i + 1, i + 2);
    wm.set(it, (i & 7) + 1);
    wmAcc += wm.get(it);
    if (i % 3 === 0) {
      wm.delete(it);
      if (wm.has(it)) wmAcc += 10000;
    } else if (keep.length < 16) {
      keep.push(it);
    }
  }
  for (const it of keep) wmAcc += wm.get(it);
  return `has_eq_wm:${inHits}:${pHit}:${pNull}:${pAfter}:${arrIn}:${eqHits}:${wmAcc}`;
}

function testSprint12aLoopsKeyedIcWeakMap() {
  "use strict";
  function dotAffine(w, bias, inp, inpOff, out, rows, cols) {
    for (let row = 0, base = 0; row < rows; row++, base += cols) {
      let sum = bias[row];
      for (let col = 0; col < cols; col++) {
        sum += w[base + col] * inp[inpOff + col];
      }
      out[row] = sum;
    }
  }
  let macCheck = 0;
  for (let cols = 0; cols <= 17; cols++) {
    const rows = 4;
    const w = new Float32Array(rows * cols);
    const inp = new Float32Array(cols + 3);
    const bias = [0.25, -0.5, 0.75, 1.0];
    const out = new Float32Array(rows);
    for (let i = 0; i < w.length; i++) w[i] = ((i * 7) % 13 - 6) * 0.125;
    for (let i = 0; i < inp.length; i++) inp[i] = ((i * 5) % 11 - 5) * 0.25;
    for (let r = 0; r < 60; r++) dotAffine(w, bias, inp, 2, out, rows, cols);
    macCheck += Math.round((out[0] + out[1] + out[2] + out[3]) * 1000);
  }
  function readAt(arr, base, n) {
    let s = 0;
    for (let i = 0; i < n; i++) {
      const v = arr[base + i];
      if (v !== undefined) s += v;
    }
    return s;
  }
  const fArr = [10, 20, 30, 40, 50, 60];
  let fracSum = 0;
  for (let i = 0; i < 150; i++) fracSum += readAt(fArr, 1, 4);
  fracSum += readAt(fArr, 0.5, 4);

  const SYM_A = Symbol("a");
  const SYM_B = Symbol("b");
  class NBase { constructor(a, b) { this[SYM_A] = a; this[SYM_B] = b; } }
  class NDiv extends NBase { constructor(a, b) { super(a, b); this.tag = "div"; } }
  class NP extends NBase { constructor(a, b) { super(a, b); this.p = 1; this.tag = "p"; } }
  class NSpan extends NBase { constructor(a, b) { super(a, b); this.s1 = 1; this.s2 = 2; } }
  const doc = { [SYM_A]: { flag: 3 } };
  const readDocFlag = (d) => d[SYM_A].flag;
  const readPolySym = (n, k) => n[k];
  const nodes = [new NBase(1, 10), new NDiv(2, 20), new NP(3, 30), new NSpan(4, 40)];
  function wrapRead(d, n, useAlt) {
    return readDocFlag(d) + readPolySym(n, useAlt ? SYM_B : SYM_A);
  }
  let symAcc = 0;
  for (let i = 0; i < 400; i++) {
    symAcc += wrapRead(doc, nodes[i & 3], i >= 350 && (i & 1) === 1);
  }
  symAcc += readPolySym([100, 200, 300], 2);

  function singleEntryHotLoop(n) {
    let s = 0;
    for (let i = 0; i < n; i++) s += (i & 7);
    return s;
  }
  let loopAcc = singleEntryHotLoop(20000) + singleEntryHotLoop(20000) + singleEntryHotLoop(20000);

  const wmSmall = new WeakMap();
  const wmLarge = new WeakMap();
  const objs = [];
  for (let i = 0; i < 40; i++) objs.push({ id: i });
  let wmSum = 0;
  for (let i = 0; i < 8; i++) {
    wmSmall.set(objs[i], i + 1);
    wmSmall.set(objs[i], (i + 1) * 2);
    wmSum += wmSmall.get(objs[i]);
  }
  for (let i = 0; i < 40; i++) wmLarge.set(objs[i], i + 10);
  for (let i = 0; i < 40; i += 2) wmLarge.delete(objs[i]);
  for (let i = 0; i < 40; i++) {
    if (i % 4 === 0) wmLarge.set(objs[i], i + 100);
    wmSum += wmLarge.get(objs[i]) || 0;
  }
  return `s12a:${macCheck}:${fracSum}:${symAcc}:${loopAcc}:${wmSum}`;
}

function testSprint12b() {
  "use strict";
  // 1. Hoistable CharCodeStr + CharCodeAt (flat, packed, UTF-16, rope, OOB, inlined, patched)
  const peekChar = (s, i) => s.charCodeAt(i);
  function sumCodes(s) {
    let acc = 0;
    for (let i = 0; i <= s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === c) acc += c;
    }
    return acc + (peekChar(s, 0) | 0);
  }
  const longAscii = "abcdefghijklmnopqrstuvwxyz0123456789";
  const packedAscii = "abc12";
  const utf16 = "αβγδ";
  let ccaSum = 0;
  for (let i = 0; i < 350; i++) {
    ccaSum += sumCodes(longAscii);
    ccaSum += sumCodes(packedAscii);
  }
  ccaSum += sumCodes(utf16);
  const rope = ("hello_" + "world_") + ("test_" + String(ccaSum & 7));
  ccaSum += sumCodes(rope);
  const origCca = String.prototype.charCodeAt;
  String.prototype.charCodeAt = function(i) { return i === 0 ? 777 : origCca.call(this, i); };
  let patchedVal = 0;
  try {
    patchedVal = sumCodes("ab");
  } finally {
    String.prototype.charCodeAt = origCca;
  }

  // 2. StrictEqH inline Heap::atoms + branch fusion
  const atomPool = ["div", "span", "section", "article", "header", "footer"];
  function cmpAtoms(a, b) {
    let r = 0;
    if (a === b) r += 1;
    if (a !== b) r += 2;
    const eqv = (a === b);
    const nev = (a !== b);
    return r + (eqv ? 4 : 0) + (nev ? 8 : 0);
  }
  let eqSum = 0;
  for (let i = 0; i < 400; i++) {
    eqSum += cmpAtoms(atomPool[i % 6], atomPool[(i + 1) % 6]);
    eqSum += cmpAtoms(atomPool[i % 6], atomPool[i % 6]);
  }
  const dynDiv = "d" + "i" + "v";
  eqSum += cmpAtoms(dynDiv, "div");
  eqSum += cmpAtoms(dynDiv, "span");
  eqSum += cmpAtoms(+0, -0);
  eqSum += cmpAtoms(NaN, NaN);
  eqSum += cmpAtoms(1, "1");
  eqSum += cmpAtoms(null, undefined);

  // 3. Tail-committed StoreSlot in D3 (`entry | 1`) + nested D3 replay safety
  class MiniTok {
    constructor() { this._state = 1; this._start = 0; this._index = 0; this._count = 0; }
    step(c) {
      if (c === 60) {
        this._state = 2;
        this._start = this._index;
      } else if (c === 62) {
        this._state = 1;
        this._count = this._count + 1;
      }
      return this._state;
    }
  }
  function nestedD3Guard(tok, c, box) {
    const s = tok.step(c);
    return s + box.v;
  }
  function driveTok(tok, s) {
    let acc = 0;
    for (let i = 0; i < s.length; i++) {
      tok._index = i;
      acc += tok.step(s.charCodeAt(i));
    }
    return acc;
  }
  const tok = new MiniTok();
  let tokAcc = 0;
  for (let i = 0; i < 120; i++) tokAcc += driveTok(tok, "<a><b><c>text</c></b></a>");
  const tok2 = new MiniTok();
  const boxA = { v: 10 };
  const boxB = { extra: 1, v: 20 };
  let nestedAcc = 0;
  for (let i = 0; i < 380; i++) {
    nestedAcc += nestedD3Guard(tok2, 62, i >= 350 ? boxB : boxA);
  }

  // 4. Inline constant-key shaped SetIndex IC (`keyed_set`)
  const K_NEXT = Symbol("next");
  const K_ALT = Symbol("alt");
  class S1 { constructor() { this[K_NEXT] = null; this.a = 1; } }
  class S2 { constructor() { this[K_NEXT] = null; this.b = 2; this.c = 3; } }
  class S3 { constructor() { this[K_NEXT] = null; this.d = 4; this.e = 5; this.f = 6; } }
  function setNext(o, v) { o[K_NEXT] = v; }
  function setDyn(o, k, v) { o[k] = v; }
  const sNodes = [new S1(), new S2(), new S3()];
  let setAcc = 0;
  for (let i = 0; i < 400; i++) {
    const cur = sNodes[i % 3];
    const nxt = (i & 1) ? { tag: i } : (i & 15);
    setNext(cur, nxt);
    setDyn(cur, i >= 380 ? K_ALT : K_NEXT, i);
    const v = cur[K_NEXT];
    setAcc += typeof v === "object" && v !== null ? v.tag : v;
  }
  const roObj = {};
  Object.defineProperty(roObj, K_NEXT, { value: 42, writable: false });
  let roCaught = 0;
  try { setNext(roObj, 99); } catch (e) { if (e instanceof TypeError) roCaught = 1; }
  const iArr = [1, 2, 3];
  setDyn(iArr, 1, 55);
  setAcc += roObj[K_NEXT] + iArr[1];

  // 5. Tier O `#private` fields & `IterToArray`
  class PrivBox {
    #count = 0;
    #cache = null;
    bump(x) {
      this.#count = this.#count + 1;
      if ((x & 3) === 0) this.#cache = { v: x };
      return this.#count + (this.#cache ? this.#cache.v : 0);
    }
    read() { return this.#count; }
  }
  const pb = new PrivBox();
  let privAcc = 0;
  for (let i = 0; i < 400; i++) privAcc += pb.bump(i);
  let privCaught = 0;
  try { pb.read.call({}); } catch (e) { if (e instanceof TypeError) privCaught = 1; }

  function unpack(it) {
    const [a, b, ...rest] = it;
    return (a | 0) + (b | 0) + rest.length;
  }
  function* gen3() { yield 7; yield 8; yield 9; yield 10; }
  let iterAcc = 0;
  for (let i = 0; i < 400; i++) {
    iterAcc += unpack([i & 7, (i & 3) + 1, 10, 20]);
  }
  iterAcc += unpack(new Set([3, 4, 5, 6, 7]));
  iterAcc += unpack(gen3());

  return `s12b:${ccaSum}:${patchedVal}:${eqSum}:${tokAcc}:${tok._count}:${nestedAcc}:${tok2._count}:${setAcc}:${roCaught}:${privAcc}:${privCaught}:${iterAcc}`;
}

function testSprint13a() {
  // 1. GEN_WEAK_KEY + lazy WeakMap/WeakSet indexing (fresh keys, duplicate keys across 2 WeakMaps,
  //    get/has/delete/churn compaction, WeakSet add/has/delete)
  const wm1 = new WeakMap();
  const wm2 = new WeakMap();
  const ws1 = new WeakSet();
  const kept = [];
  let wmAcc = 0;
  for (let i = 0; i < 600; i++) {
    const o = { id: i };
    wm1.set(o, i * 3 + 1);
    if ((i & 7) === 0) kept.push(o);
    if ((i & 15) === 0) {
      wm2.set(o, i + 100);
      ws1.add(o);
    }
    if ((i & 31) === 0) {
      // Update existing key in wm1 after GEN_WEAK_KEY is already set
      wm1.set(o, i * 3 + 2);
    }
  }
  const neverWeak = { id: -1 };
  wmAcc += wm1.has(neverWeak) ? 1000 : 0;
  wmAcc += wm1.get(neverWeak) === undefined ? 1 : 0;
  wmAcc += ws1.has(neverWeak) ? 1000 : 0;
  for (let i = 0; i < kept.length; i++) {
    const o = kept[i];
    wmAcc += wm1.get(o) || 0;
    wmAcc += wm2.get(o) || 0;
    wmAcc += ws1.has(o) ? 1 : 0;
    if ((i & 3) === 0) {
      wmAcc += wm1.delete(o) ? 2 : 0;
      wmAcc += wm1.has(o) ? 1000 : 0;
    }
  }
  // 2. Deep derived class chains + 0-arg new Map()/Set()/WeakMap()/WeakSet() + private & field thunk
  const evWm = new WeakMap();
  class EvTarget {
    constructor() { evWm.set(this, new Map()); }
  }
  class BaseNode extends EvTarget {
    constructor(doc, name) { super(); this.doc = doc; this.name = name; }
  }
  class ElemNode extends BaseNode {
    constructor(doc, name) { super(doc, name); this.attrs = new Set(); }
  }
  class HtmlElem extends ElemNode {}
  class DivElem extends HtmlElem {}
  class PrivDerived extends ElemNode {
    #tag = 7;
    extra = 11;
    constructor(doc, name) { super(doc, name); }
    val() { return this.#tag + this.extra; }
  }
  let ctorAcc = 0;
  for (let i = 0; i < 320; i++) {
    const d = new DivElem(i, "div");
    const m = evWm.get(d);
    if ((i & 7) === 0) m.set("k", i);
    ctorAcc += d.doc + d.attrs.size + (m.get("k") || 0);
  }
  const pd = new PrivDerived(5, "p");
  ctorAcc += pd.val() + pd.doc;
  // Subclass of Map/Set still honors newTarget.prototype and iterable arg
  class MyMap extends Map {}
  const mm = new MyMap([["a", 1], ["b", 2]]);
  ctorAcc += (mm instanceof MyMap ? 100 : 0) + mm.size;

  // 3. Symbol-keyed class-instance appends preserving planned layouts across many subclasses
  const S1 = Symbol("s1"), S2 = Symbol("s2"), S3 = Symbol("s3"), S4 = Symbol("s4");
  const subClasses = [];
  for (let c = 0; c < 16; c++) {
    subClasses.push(class extends DivElem {});
  }
  function initSyms(o, v) {
    o[S1] = v;
    o[S2] = v + 1;
    o[S3] = v + 2;
    o[S4] = v + 3;
    o[S2] = v + 10;
    return o[S1] + o[S2] + o[S3] + o[S4];
  }
  let symAcc = 0;
  for (let i = 0; i < 350; i++) {
    const Cls = subClasses[i & 15];
    symAcc += initSyms(new Cls(i, "el"), i & 7);
  }

  // 4. Direct Native & captured FN_CALL (`own.call(obj, key)`), captured Array/String intrinsics
  const own = {}.hasOwnProperty;
  const arrMap = Array.prototype.map;
  const strSlice = String.prototype.slice;
  function checkOwn(obj, k) {
    return own.call(obj, k) ? 1 : 0;
  }
  function fnCallHelper(fn, ctx, a, b) {
    return fn.call(ctx, a, b);
  }
  function addCtx(a, b) { return this.v + a + b; }
  const infoObj = { 1: true, 3: true, a: 1 };
  const ctxObj = { v: 10 };
  let natAcc = 0;
  for (let i = 0; i < 350; i++) {
    natAcc += checkOwn(infoObj, i & 3);
    natAcc += checkOwn(infoObj, "a");
    natAcc += fnCallHelper(addCtx, ctxObj, i & 7, 2);
    const s = strSlice.call("abcdef", 1, 4);
    const m = arrMap.call([1, 2, 3], (x) => x + (i & 1));
    natAcc += s.length + m[2];
  }
  return `s13a:${wmAcc}:${ctorAcc}:${symAcc}:${natAcc}`;
}

function testSprint13b() {
  // 1. RegExp fast slots (all 7 methods + override/restore) & functional replacer fast path
  const reEsc = /[\\`*_{}[\]()#+\-.!]/g;
  const reOne = /([a-z]+)(\d+)/;
  const reNamed = /(?<word>[a-z]+)-(?<num>\d+)/g;
  let reAcc = 0;
  for (let i = 0; i < 320; i++) {
    // 0 matches
    const s0 = "plain text without specials";
    if (s0.replace(reEsc, (m) => "\\" + m) === s0) reAcc += 1;
    // 1 match (global & non-global)
    const s1 = "hello*world".replace(reEsc, (m) => "\\" + m);
    if (s1 === "hello\\*world") reAcc += 1;
    const s1b = "ab12cd34".replace(reOne, (m, a, b, off) => `${b}:${a}@${off}`);
    if (s1b === "12:ab@0cd34" && RegExp.$1 === "ab" && RegExp.$2 === "12") reAcc += 1;
    // Multi-match global + short ASCII & non-ASCII callback outputs
    const s2 = "*a_b*".replace(reEsc, (m) => "\\" + m);
    if (s2 === "\\*a\\_b\\*") reAcc += 1;
    const s3 = "x".replace(/x/g, () => "");
    if (s3 === "") reAcc += 1;
    const s4 = "ab".replace(/a/g, () => "Z");
    if (s4 === "Zb") reAcc += 1;
    const s5 = "a-1 b-2".replace(reNamed, (m, w, n, off, str, g) => `${g.word}=${g.num}`);
    if (s5 === "a=1 b=2") reAcc += 1;
    const s6 = "hello".replace(/l/g, () => "\u00e9");
    if (s6 === "he\u00e9\u00e9o") reAcc += 1;
    // Match & test intrinsic slots
    const mm = "foo123bar".match(reOne);
    if (mm && mm[0] === "foo123" && mm.index === 0 && mm.input === "foo123bar") reAcc += 1;
    if (reOne.test("xyz9")) reAcc += 1;
  }
  // Override RegExp.prototype.test / [Symbol.match] and verify fast slots invalidate
  const origTest = RegExp.prototype.test;
  const origMatch = RegExp.prototype[Symbol.match];
  let ovOk = 0;
  RegExp.prototype.test = function() { return "ov-test"; };
  if (/a/.test("b") === "ov-test") ovOk += 1;
  RegExp.prototype.test = origTest;
  if (/a/.test("a") === true && /a/.test("b") === false) ovOk += 1;
  RegExp.prototype[Symbol.match] = function() { return "ov-match"; };
  if ("abc".match(/a/) === "ov-match") ovOk += 1;
  RegExp.prototype[Symbol.match] = origMatch;
  if ("abc".match(/b/)[0] === "b") ovOk += 1;

  // 2. fn_props fast set path (closures, overwrite, call/apply/bind, own non-writable, proto setter)
  let fnAcc = 0;
  for (let i = 0; i < 320; i++) {
    const k = i & 7;
    const fn = (x) => x + k;
    fn.shouldTestNextSiblings = (i & 1) === 0;
    fn.tag = i;
    fn.tag = i + 1;
    if (fn.shouldTestNextSiblings) fnAcc += 1;
    fnAcc += fn.tag + fn(2);
  }
  const roFn = () => 1;
  Object.defineProperty(roFn, "locked", { value: 42, writable: false, configurable: true });
  let roErr = 0;
  try {
    roFn.locked = 99;
  } catch (e) {
    if (e instanceof TypeError) roErr += 1;
  }
  if (roFn.locked === 42) roErr += 10;
  const frozenFn = Object.freeze(() => 2);
  try {
    frozenFn.extra = 1;
  } catch (e) {
    if (e instanceof TypeError) roErr += 100;
  }
  let protoSetSeen = 0;
  Object.defineProperty(Function.prototype, "__s13bTrap", {
    set(v) { protoSetSeen += v; },
    configurable: true,
  });
  const trapFn = () => 3;
  trapFn.__s13bTrap = 7;
  delete Function.prototype.__s13bTrap;
  trapFn.__s13bTrap = 13;
  if (trapFn.__s13bTrap === 13) protoSetSeen += 100;

  // 3. InitDataProp planned append (spread + static keys + computed + duplicate + getter) & ready_refill
  const base = { p: 1, q: 2 };
  let initAcc = 0;
  for (let i = 0; i < 400; i++) {
    const k = i & 1 ? "dynA" : "dynB";
    const o = { ...base, a: i, b: i + 1, c: i + 2 };
    const o2 = { [k]: i, x: 10, y: 20, x: 30 };
    initAcc += o.p + o.a + o.c + o2.x + o2.y + o2[k];
  }
  const gLit = { get a() { return 5; }, a: 17 };
  initAcc += gLit.a;

  // 4. Cached derived ctor JIT entry + native_cb_entry cache + empty/1-elem array_each
  class A13 { constructor(v) { this.a = v; } }
  class B13 extends A13 { constructor(v) { super(v + 1); this.b = v * 2; } }
  class C13 extends B13 { constructor(v) { super(v + 2); this.c = v * 3; } }
  let cbAcc = 0;
  const emptyArr = [];
  const oneArr = [7];
  const smallArr = [1, 2, 3, 4];
  const addK = (k) => (x) => x + k;
  const tryCb = (x) => { try { return x * 2; } catch (e) { return 0; } };
  for (let i = 0; i < 320; i++) {
    const inst = new C13(i & 15);
    cbAcc += inst.a + inst.b + inst.c;
    cbAcc += emptyArr.map((x) => x + 1).length;
    cbAcc += emptyArr.filter((x) => x > 0).length;
    emptyArr.forEach(() => { cbAcc += 1000; });
    const m1 = oneArr.map(addK(i & 3));
    const m2 = smallArr.map(tryCb);
    const f1 = smallArr.filter((x) => (x & 1) === 1);
    cbAcc += m1[0] + m2[3] + f1.length;
  }
  return `s13b:${reAcc}:${ovOk}:${fnAcc}:${roErr}:${protoSetSeen}:${initAcc}:${cbAcc}`;
}

function testSprint13c() {
  // 1. fn_props_set_fast with Function.prototype[Symbol.hasInstance] intact + strict rejection of Symbol.hasInstance write
  let fnOk = 0;
  for (let i = 0; i < 320; i++) {
    const f = () => i;
    f.shouldTest = (i & 1) === 0;
    f.weight = i * 2 + 1;
    f.weight = i * 3 + 2;
    fnOk += (f.shouldTest ? 1 : 0) + f.weight;
  }
  let symHiReject = 0;
  try {
    const g = () => 1;
    g[Symbol.hasInstance] = () => true;
  } catch (e) {
    if (e instanceof TypeError) symHiReject = 1;
  }
  // 2. 16-slot ta_read_cache across 6 Float32Arrays + Uint32Array (> i32::MAX) + Int32Array + Float64Array + Value::num edge cases
  const f32s = [];
  for (let k = 0; k < 6; k++) {
    const a = new Float32Array(32);
    for (let j = 0; j < 32; j++) a[j] = ((j + 1) * 0.25 + k * 0.125) * (j & 1 ? -1 : 1);
    f32s.push(a);
  }
  const u32 = new Uint32Array([0, 1, 0x7fffffff, 0x80000000, 0x80000005, 0xffffffff]);
  const i32 = new Int32Array([-2147483648, -1, 0, 1, 2147483647]);
  const f64 = new Float64Array([-0.0, NaN, -2147483648.0, -2147483648.5, 2147483648.0, 1.5]);
  function dotAll() {
    let s = 0.0;
    for (let rep = 0; rep < 40; rep++) {
      for (let j = 0; j < 32; j++) {
        s += f32s[0][j] * f32s[1][j] + f32s[2][j] * f32s[3][j] + f32s[4][j] * f32s[5][j];
      }
    }
    return Math.round(s * 100);
  }
  const taDot = dotAll();
  let u32Sum = 0;
  for (let rep = 0; rep < 60; rep++) {
    for (let j = 0; j < u32.length; j++) u32Sum += u32[j];
  }
  let edgeMask = 0;
  if (Object.is(f64[0], -0)) edgeMask |= 1;
  if (Number.isNaN(f64[1])) edgeMask |= 2;
  if (f64[2] === -2147483648 && i32[0] === -2147483648) edgeMask |= 4;
  if (f64[3] === -2147483648.5 && f64[4] === 2147483648) edgeMask |= 8;
  if (Object.is(-0.5 * 0, -0) && Number.isNaN(0 / 0) && Number.isNaN(f64[5] * NaN)) edgeMask |= 16;
  // 3. Symbol class-instance set_index (sym_add_memo in NOJIT/WASM + JIT) + base-ctor super() cache
  const S_NEXT = Symbol("next");
  const S_PREV = Symbol("prev");
  class N13c {
    constructor(v) {
      this.v = v;
      this[S_NEXT] = null;
      this[S_PREV] = null;
    }
  }
  class E13c extends N13c {
    constructor(v, tag) {
      super(v);
      this.tag = tag;
    }
  }
  class D13c extends E13c {
    constructor(v) {
      super(v + 3, "div");
      this[S_NEXT] = v * 2;
      this[S_PREV] = v * 3;
      this[S_NEXT] = v * 4;
    }
  }
  let symAcc = 0;
  for (let i = 0; i < 400; i++) {
    const d = new D13c(i & 31);
    symAcc += d.v + d[S_NEXT] + d[S_PREV] + d.tag.length;
  }
  let symSetterSeen = 0;
  Object.defineProperty(N13c.prototype, S_NEXT, {
    set(x) { symSetterSeen += (x || 0); },
    configurable: true,
  });
  const dAfter = new D13c(5);
  delete N13c.prototype[S_NEXT];
  const dRestored = new D13c(6);
  symAcc += symSetterSeen + dAfter.v + dRestored[S_NEXT];
  return `s13c:${fnOk}:${symHiReject}:${taDot}:${u32Sum}:${edgeMask}:${symAcc}`;
}

function testSprint13d() {
  // 1. Proxy plain_non_object_target_desc: arrays (plain, non-writable length,
  //    non-configurable override, frozen/sealed dense elements), functions
  //    (plain, fn_props non-configurable, frozen), classes (statics + frozen).
  const arr = [10, 20, 30];
  let trapHits = 0;
  const pArr = new Proxy(arr, {
    get(t, k, r) {
      trapHits++;
      return Reflect.get(t, k, r);
    },
  });
  let arrSum = 0;
  for (let i = 0; i < 300; i++) {
    arrSum += pArr[i % 3] + pArr.length + (typeof pArr.map === "function" ? 1 : 0);
  }
  const fzArr = Object.freeze([7, 8, 9]);
  const badFzArr = new Proxy(fzArr, { get() { return 999; } });
  const ncArr = [1, 2, 3];
  Object.defineProperty(ncArr, "1", { value: 2, writable: false, configurable: false });
  const badNcArr = new Proxy(ncArr, { get(t, k) { return k === "1" ? 777 : t[k]; } });
  const nwLenArr = [1, 2];
  Object.defineProperty(nwLenArr, "length", { writable: false });
  const badLenArr = new Proxy(nwLenArr, { get(t, k) { return k === "length" ? 99 : t[k]; } });

  function fTarget(a, b) { return a + b; }
  fTarget.tag = 5;
  Object.defineProperty(fTarget, "fixed", { value: 42, writable: false, configurable: false });
  const pFn = new Proxy(fTarget, { get(t, k, r) { return Reflect.get(t, k, r); } });
  const badFn = new Proxy(fTarget, { get(t, k) { return k === "fixed" ? 0 : t[k]; } });
  const fzFn = Object.freeze(function frozenFn(x) { return x; });
  const badFzFn = new Proxy(fzFn, { get(t, k) { return k === "name" ? "wrong" : t[k]; } });

  class CTarget {
    static get dyn() { return 11; }
  }
  CTarget.count = 3;
  Object.defineProperty(CTarget, "locked", { value: 88, writable: false, configurable: false });
  const pCls = new Proxy(CTarget, { get(t, k, r) { return Reflect.get(t, k, r); } });
  const badCls = new Proxy(CTarget, { get(t, k) { return k === "locked" ? 1 : t[k]; } });
  const badProtoCls = new Proxy(CTarget, { get(t, k) { return k === "prototype" ? {} : t[k]; } });

  let fnClsSum = 0;
  for (let i = 0; i < 200; i++) {
    fnClsSum += pFn.tag + pFn.fixed + pFn.length + pCls.count + pCls.locked + pCls.dyn;
  }
  let invMask = 0;
  try { void badFzArr[0]; } catch (e) { if (e instanceof TypeError) invMask |= 1; }
  try { void badNcArr[1]; } catch (e) { if (e instanceof TypeError) invMask |= 2; }
  try { void badLenArr.length; } catch (e) { if (e instanceof TypeError) invMask |= 4; }
  try { void badFn.fixed; } catch (e) { if (e instanceof TypeError) invMask |= 8; }
  try { void badFzFn.name; } catch (e) { if (e instanceof TypeError) invMask |= 16; }
  try { void badCls.locked; } catch (e) { if (e instanceof TypeError) invMask |= 32; }
  try { void badProtoCls.prototype; } catch (e) { if (e instanceof TypeError) invMask |= 64; }

  // 2. TypedArray subarray views (non-zero byteOffset) + float arith + pool churn
  const raw = new ArrayBuffer(64);
  const fullU32 = new Uint32Array(raw);
  for (let i = 0; i < fullU32.length; i++) fullU32[i] = (i + 1) * 100000000;
  const subU32 = new Uint32Array(raw, 8, 6);
  let subSum = 0;
  for (let rep = 0; rep < 40; rep++) {
    for (let j = 0; j < subU32.length; j++) subSum += subU32[j];
  }
  let churnSum = 0;
  for (let i = 0; i < 4000; i++) {
    const o = { a: i * 1.5, b: i - 0.5 };
    const pair = [o.a / 2.0, o.b % 7.5];
    churnSum += Math.round((pair[0] + pair[1]) * 10);
  }
  return `s13d:${trapHits}:${arrSum}:${fnClsSum}:${invMask}:${subSum}:${churnSum}`;
}

function testSprint14a() {
  // 1. Captured `const` immutability across nested closures (eliding MarkCellConst):
  //    simple const, transitive grandchild, forward-referenced block const,
  //    TDZ ReferenceError-before-TypeError, destructuring with default closure,
  //    for-of const, for-in const, direct eval still throwing TypeError,
  //    and hot mutable `let` UpvalSet while captured consts exist.
  let mask = 0;
  let rhsHits = 0;
  function bump(v) { rhsHits++; return v; }
  {
    const c = 10;
    const w1 = () => { c = bump(20); };
    const w2 = () => () => { c = bump(30); };
    try { w1(); } catch (e) { if (e instanceof TypeError) mask |= 1; }
    try { w2()(); } catch (e) { if (e instanceof TypeError) mask |= 2; }
  }
  // Forward-referenced const in hoisted function: TDZ before init, TypeError after
  {
    function writeBefore() { fc = 99; }
    try { writeBefore(); } catch (e) { if (e instanceof ReferenceError) mask |= 4; }
    const fc = 1;
    try { writeBefore(); } catch (e) { if (e instanceof TypeError) mask |= 8; }
  }
  // Destructuring const with closure inside default initializer
  {
    let cap;
    const { a = (cap = () => { a = 2; }, 5) } = {};
    try { cap(); } catch (e) { if (e instanceof TypeError && a === 5) mask |= 16; }
  }
  // for-of and for-in const captured by closures
  const ofFns = [];
  for (const x of [1, 2, 3]) ofFns.push(() => { x = 9; });
  try { ofFns[1](); } catch (e) { if (e instanceof TypeError) mask |= 32; }
  const inFns = [];
  for (const k in { a: 1, b: 2 }) inFns.push(() => { k = "z"; });
  try { inFns[0](); } catch (e) { if (e instanceof TypeError) mask |= 64; }
  // Direct eval in subtree must still keep MarkCellConst and throw TypeError
  {
    const ec = 7;
    const badEval = () => eval("ec = 8");
    try { badEval(); } catch (e) { if (e instanceof TypeError && ec === 7) mask |= 128; }
  }
  // Hot mutable UpvalSet alongside captured consts (tests inline UpvalSet fast path)
  function makeCounter(step) {
    const k = step * 2;
    let acc = 0;
    return function inc(n) {
      for (let i = 0; i < n; i++) acc += k;
      return acc;
    };
  }
  const inc = makeCounter(3);
  let ctr = 0;
  for (let i = 0; i < 400; i++) ctr = inc(5);
  // 2. RegExp non-global match churn exercising SlotTable push_new/set_pos fast path
  let rxSum = 0;
  const re = /([a-z]+)-(\d+)/;
  for (let i = 0; i < 600; i++) {
    const m = ("tag-" + i).match(re);
    if (m) rxSum += m[1].length + Number(m[2]);
  }
  return `s14a:${mask}:${rhsHits}:${ctr}:${rxSum}`;
}

function testSprint14b() {
  // 1. Closures with 1, 2 (bit-63 packed), 3, 8 (bit-62 inline slot-indexed),
  //    and 10 (Upvals::Heap spilled) upvalues: reads, writes, D3 calls, inlining.
  function make1(a) {
    let n = a;
    return (x) => { n += x; return n; };
  }
  function make2(a, b) {
    let x = a, y = b;
    return (k) => { x += k; y += k * 2; return x + y; };
  }
  function make3(a, b, c) {
    let x = a, y = b, z = c;
    return (k) => { x += k; z += y; return x + y + z; };
  }
  function make8(a, b, c, d, e, f, g, h) {
    let s0 = a, s1 = b, s2 = c, s3 = d, s4 = e, s5 = f, s6 = g, s7 = h;
    return (k) => {
      s0 += k; s7 += k;
      return s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7;
    };
  }
  function make10(a, b, c, d, e, f, g, h, i, j) {
    let v0 = a, v1 = b, v2 = c, v3 = d, v4 = e, v5 = f, v6 = g, v7 = h, v8 = i, v9 = j;
    return (k) => {
      v0 += k; v9 += k;
      return v0 + v1 + v2 + v3 + v4 + v5 + v6 + v7 + v8 + v9;
    };
  }
  // 2. Transitive ParentUpval chains (parent has 2, 3, and 10 upvals -> child via ReadyClosures).
  function chain2(a, b) {
    let x = a, y = b;
    return function mid(m) {
      let z = m;
      return (k) => x + y + z + k;
    };
  }
  function chain3(a, b, c) {
    let x = a, y = b, z = c;
    return function mid(m) {
      let w = m;
      return (k) => x + y + z + w + k;
    };
  }
  function chain10(a, b, c, d, e, f, g, h, i, j) {
    let u0 = a, u1 = b, u2 = c, u3 = d, u4 = e, u5 = f, u6 = g, u7 = h, u8 = i, u9 = j;
    return function mid(m) {
      return (k) => u0 + u4 + u9 + m + k;
    };
  }
  // 3. Arrows with lexical `this` + 2, 3, 8 upvalues.
  const obj = {
    base: 100,
    build2(a, b) { return (k) => this.base + a + b + k; },
    build3(a, b, c) { return (k) => this.base + a + b + c + k; },
    build8(a, b, c, d, e, f, g, h) { return (k) => this.base + a + b + c + d + e + f + g + h + k; },
  };
  const f1 = make1(1);
  const f2 = make2(2, 3);
  const f3 = make3(1, 2, 3);
  const f8 = make8(1, 2, 3, 4, 5, 6, 7, 8);
  const f10 = make10(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
  const c2 = chain2(10, 20);
  const c3 = chain3(10, 20, 30);
  const c10 = chain10(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
  const a2 = obj.build2(1, 2);
  const a3 = obj.build3(1, 2, 3);
  const a8 = obj.build8(1, 2, 3, 4, 5, 6, 7, 8);

  let sumA = 0;
  for (let i = 0; i < 400; i++) {
    sumA += f1(1) + f2(1) + f3(1) + f8(1) + f10(1);
    sumA += c2(i)(1) + c3(i)(1) + c10(i)(1);
    sumA += a2(i) + a3(i) + a8(i);
  }
  // 4. Force Heap::objs growth & GC churn while inline closures remain live.
  const keep = [];
  for (let i = 0; i < 20000; i++) {
    const tmp = make2(i, i + 1);
    if ((i & 63) === 0) keep.push(tmp);
  }
  let sumB = 0;
  for (let i = 0; i < 400; i++) {
    sumB += f1(2) + f2(2) + f3(2) + f8(2) + f10(2);
    sumB += c2(i)(2) + c3(i)(2) + c10(i)(2);
    sumB += a2(i) + a3(i) + a8(i);
  }
  // 5. Native Array HOF callbacks (upvals_mirror path) with 2, 3, 10 upvalues.
  const arr = [1, 2, 3, 4, 5, 6, 7, 8];
  const bias2 = 10, scale2 = 2;
  const hof2 = arr.map((v) => v * scale2 + bias2);
  const hof3 = (() => {
    let x = 1, y = 2, z = 3;
    return arr.map((v) => v + x + y + z);
  })();
  const hof10 = (() => {
    let u0 = 1, u1 = 1, u2 = 1, u3 = 1, u4 = 1, u5 = 1, u6 = 1, u7 = 1, u8 = 1, u9 = 1;
    return arr.map((v) => v + u0 + u1 + u2 + u3 + u4 + u5 + u6 + u7 + u8 + u9);
  })();
  let sumC = 0;
  for (let i = 0; i < arr.length; i++) sumC += hof2[i] + hof3[i] + hof10[i];
  for (let i = 0; i < keep.length; i++) sumC += keep[i](1);
  return `s14b:${sumA}:${sumB}:${sumC}`;
}

function testSprint14c() {
  "use strict";
  // 1. KeyedSetPlan: mono/poly shaped Symbol & string writes, write barriers,
  //    shape transition, non-writable strict TypeError, accessor setter fallback.
  const NEXT = Symbol("next");
  const PREV = Symbol("prev");
  const EXTRA = Symbol("extra");
  class SNodeA { constructor(v) { this[NEXT] = null; this[PREV] = null; this.v = v; } }
  class SNodeB { constructor(v) { this.tag = "b"; this[NEXT] = null; this[PREV] = null; this.v = v; } }
  class SNodeC { constructor(v) { this.x = 1; this.y = 2; this[NEXT] = null; this[PREV] = null; this.v = v; } }
  function linkNext(a, b) { a[NEXT] = b; }
  function linkPrev(a, b) { a[PREV] = b; }
  function setStrKey(o, k, v) { o[k] = v; }
  const pool = [];
  for (let i = 0; i < 600; i++) {
    const n = i % 3 === 0 ? new SNodeA(i) : i % 3 === 1 ? new SNodeB(i) : new SNodeC(i);
    pool.push(n);
  }
  for (let i = 0; i < pool.length - 1; i++) {
    linkNext(pool[i], pool[i + 1]);
    linkPrev(pool[i + 1], pool[i]);
  }
  for (let i = 0; i < 12000; i++) {
    const tmp = { k: i, arr: [i, i + 1] };
    if (i === 11999) linkNext(pool[0], { [NEXT]: null, v: tmp.k + 7 });
  }
  let ksSum = pool[0][NEXT].v + pool[pool.length - 1][PREV].v;
  const trans = new SNodeA(1);
  trans[EXTRA] = 99;
  linkNext(trans, pool[5]);
  ksSum += trans[NEXT].v + trans[EXTRA];
  const ro = {};
  Object.defineProperty(ro, NEXT, { value: 1, writable: false });
  let errMask = 0;
  try { linkNext(ro, 2); } catch (e) { if (e instanceof TypeError) errMask |= 1; }
  let setHits = 0;
  const accObj = {};
  Object.defineProperty(accObj, NEXT, { set(v) { setHits += v; } });
  linkNext(accObj, 42);
  const strObjs = [{ val: 0 }, { a: 1, val: 0 }];
  for (let i = 0; i < 400; i++) setStrKey(strObjs[i & 1], "val", i);
  ksSum += setHits + strObjs[0].val + strObjs[1].val;

  // 2. Empty Map/Set GEN_LEAF + thin alloc + Op::NewEmptyColl + GC survival
  const wm = new WeakMap();
  class DOMEventTarget {
    constructor() {
      wm.set(this, new Map());
    }
  }
  class DNode extends DOMEventTarget {
    constructor(doc, type) {
      super();
      this.doc = doc;
      this.type = type;
      this[NEXT] = null;
    }
  }
  class DParent extends DNode {
    constructor(doc, type) {
      super(doc, type);
    }
  }
  class DElem extends DParent {
    constructor(doc, tag) {
      super(doc, 1);
      this.tag = tag;
    }
  }
  class DHtml extends DElem {
    constructor(doc, tag) {
      super(doc, tag);
    }
  }
  class DDiv extends DHtml {}

  function makeSet() { return new Set(); }
  const doc = { id: 7 };
  const kept = [];
  for (let i = 0; i < 1200; i++) {
    const el = new DDiv(doc, i & 1 ? "div" : "span");
    if ((i & 31) === 0) kept.push(el);
    const s = makeSet();
    if (i === 500) { s.add({ alive: 123 }); kept.push(s); }
  }
  const oldMap = wm.get(kept[0]);
  oldMap.set("click", { fn: 555 });
  const freshMap = new Map();
  freshMap.set({ k: 1 }, { v: 777 });
  for (let i = 0; i < 20000; i++) {
    const g = new DDiv(doc, "p");
    if (i === 19999) g.tag = "end";
  }
  let domSum = 0;
  for (let i = 0; i < kept.length; i++) {
    const item = kept[i];
    if (item instanceof Set) {
      for (const x of item) domSum += x.alive;
    } else {
      domSum += item.doc.id + item.type + (wm.get(item) instanceof Map ? 1 : 0);
    }
  }
  domSum += oldMap.get("click").fn;
  for (const [k, v] of freshMap) domSum += k.k + v.v;

  // 3. SuperCtor edge cases: base/derived return overrides, primitive return TypeError,
  //    missing super() ReferenceError, double super() ReferenceError, setPrototypeOf,
  //    outer this_tdz -> inner derived, throw propagation.
  class RetBase {
    constructor(ov) {
      if (ov) return ov;
      this.ok = 1;
    }
  }
  class RetDer extends RetBase {
    constructor(ov) {
      super(ov);
      this.der = 2;
    }
  }
  const ov = { custom: 10 };
  let edgeSum = 0;
  for (let i = 0; i < 400; i++) {
    const a = new RetDer(null);
    edgeSum += a.ok + a.der;
  }
  const b = new RetDer(ov);
  if (b === ov && b.custom === 10 && b.der === 2) errMask |= 2;

  class BadPrim extends RetBase {
    constructor(bad) {
      super(null);
      if (bad) return 123;
    }
  }
  class BadPrimChild extends BadPrim {
    constructor(bad) { super(bad); }
  }
  for (let i = 0; i < 350; i++) new BadPrimChild(false);
  try { new BadPrimChild(true); } catch (e) { if (e instanceof TypeError) errMask |= 4; }

  class NoSuper extends RetBase {
    constructor(skip) { if (!skip) super(null); }
  }
  class NoSuperChild extends NoSuper {
    constructor(skip) { super(skip); }
  }
  for (let i = 0; i < 350; i++) new NoSuperChild(false);
  try { new NoSuperChild(true); } catch (e) { if (e instanceof ReferenceError) errMask |= 8; }

  class DoubleSuper extends RetBase {
    constructor(dbl) { super(null); if (dbl) super(null); }
  }
  for (let i = 0; i < 350; i++) new DoubleSuper(false);
  try { new DoubleSuper(true); } catch (e) { if (e instanceof ReferenceError) errMask |= 16; }

  class DynP1 { constructor() { this.p = 1; } }
  class DynP2 { constructor() { this.p = 20; } }
  class DynC extends DynP1 { constructor() { super(); } }
  let dynSum = 0;
  for (let i = 0; i < 350; i++) dynSum += new DynC().p;
  Object.setPrototypeOf(DynC, DynP2);
  dynSum += new DynC().p;

  class TdzInner extends RetBase {
    constructor() { super(null); this.inn = 5; }
  }
  class TdzOuter extends TdzInner {
    constructor(bad) {
      if (bad) void this.x;
      super();
      this.out = 6;
    }
  }
  for (let i = 0; i < 350; i++) {
    const t = new TdzOuter(false);
    edgeSum += t.inn + t.out;
  }
  try { new TdzOuter(true); } catch (e) { if (e instanceof ReferenceError) errMask |= 32; }

  class ThrowBase {
    constructor(boom) { if (boom) throw new RangeError("ctor-boom"); this.tb = 3; }
  }
  class ThrowMid extends ThrowBase { constructor(boom) { super(boom); } }
  class ThrowSub extends ThrowMid { constructor(boom) { super(boom); } }
  for (let i = 0; i < 350; i++) edgeSum += new ThrowSub(false).tb;
  try { new ThrowSub(true); } catch (e) { if (e instanceof RangeError && e.message === "ctor-boom") errMask |= 64; }

  return `s14c:${ksSum}:${domSum}:${edgeSum}:${dynSum}:${errMask}`;
}

function testSprint14d() {
  // 1. 1-, 2-, and 3-capture closures over immutable params + top-level consts
  // (css-select `tag` shape: adapter + name + next)
  const makeTag = (next, tagName, prefix) => {
    const upper = prefix + tagName.toUpperCase();
    return function tag(el) {
      return el.t === upper && next(el);
    };
  };
  const basePred = (el) => el.ok;
  const preds = [];
  for (let i = 0; i < 400; i++) {
    preds.push(makeTag(basePred, i % 2 === 0 ? "div" : "span", "H:"));
  }
  let tagHits = 0;
  const elDiv = { t: "H:DIV", ok: true };
  const elSpan = { t: "H:SPAN", ok: true };
  for (let i = 0; i < 400; i++) {
    if (preds[i](elDiv)) tagHits++;
    if (preds[i](elSpan)) tagHits += 2;
  }

  // 2. +0.0 capture (must NOT collide with upvals_mirror == 0 sentinel!)
  function makeZero(z, k) {
    const z2 = z;
    return (x) => x + z2 + k;
  }
  const fz1 = makeZero(0, 0);
  const fz2 = makeZero(0, 5);
  let zeroSum = 0;
  for (let i = 0; i < 400; i++) zeroSum += fz1(i) + fz2(i);

  // 3. Curried expression arrows + grandchild ParentUpvalVal + mixed cell fallback
  const curry3 = (a) => (b) => (c) => a * 100 + b * 10 + c;
  let currySum = 0;
  for (let i = 0; i < 400; i++) currySum += curry3(i % 7)(i % 5)(i % 3);

  function outerValToMixed(a) {
    return function mid() {
      let mut = 1;
      const mixed = (x) => {
        mut += x;
        return a + mut;
      };
      const pure = (x) => a + x;
      return [mixed, pure];
    };
  }
  const [mFn, pFn] = outerValToMixed(10)();
  let mixSum = 0;
  for (let i = 0; i < 400; i++) mixSum += mFn(1) + pFn(i % 4);

  // 4. 4-capture fallback (> INLINE_VAL_UPVALS_CAP = 3)
  function cap4(a, b, c, d) {
    const e = d + 1;
    return (x) => x + a + b + c + e;
  }
  const f4 = cap4(1, 2, 3, 4);
  let c4Sum = 0;
  for (let i = 0; i < 400; i++) c4Sum += f4(i);

  // 5. Immutable late_boxed (loop-mutated var before return, immutable inside closure)
  function lateImm(n) {
    var acc = 1;
    for (var i = 1; i <= n; i++) acc += i;
    var tag = "k" + acc;
    return function (x) {
      return x + acc + tag.length;
    };
  }
  let lateSum = 0;
  for (let i = 0; i < 350; i++) {
    const fn = lateImm((i % 8) + 1);
    lateSum += fn(i % 5);
  }

  // 6. Mixed late_boxed (one mutated inside closure, one immutable)
  function lateMixed(step) {
    var total = 0;
    return function () {
      total += step;
      return total;
    };
  }
  const lm = lateMixed(3);
  let lmSum = 0;
  for (let i = 0; i < 350; i++) lmSum += lm();

  // 7. Class method copy_boxed + Array.map/filter/find kernels over Upvals::Vals
  class Scaler {
    scaleAndFilter(arr, mul, bias) {
      const lim = mul * 3 + bias;
      return arr
        .map((x) => x * mul + bias)
        .filter((x) => x >= lim)
        .reduce((a, b) => a + b, 0);
    }
  }
  const sc = new Scaler();
  const baseArr = [1, 2, 3, 4, 5, 6];
  let arrSum = 0;
  for (let i = 0; i < 350; i++) arrSum += sc.scaleAndFilter(baseArr, (i % 3) + 1, i % 4);

  // 8. Const reassignment TypeError + TDZ ReferenceError preserved
  let errMask = 0;
  function constWrite() {
    const k = 7;
    return () => { k = 8; };
  }
  try { constWrite()(); } catch (e) { if (e instanceof TypeError) errMask |= 1; }
  function tdzBefore() {
    const g = () => v;
    const r = g();
    const v = 9;
    return r;
  }
  try { tdzBefore(); } catch (e) { if (e instanceof ReferenceError) errMask |= 2; }
  function tdzSelf() {
    const v = (() => v)();
    return v;
  }
  try { tdzSelf(); } catch (e) { if (e instanceof ReferenceError) errMask |= 4; }

  // 9. GC survival of young heap objects held inside Upvals::Vals
  const gcFns = [];
  for (let i = 0; i < 600; i++) {
    const obj = { v: i, s: "item_" + i };
    const arr = [i, i + 1];
    gcFns.push(() => obj.v + obj.s.length + arr[1]);
  }
  let gcSum = 0;
  for (let i = 0; i < gcFns.length; i++) gcSum += gcFns[i]();

  return `s14d:${tagHits}:${zeroSum}:${currySum}:${mixSum}:${c4Sum}:${lateSum}:${lmSum}:${arrSum}:${errMask}:${gcSum}`;
}

function testSprint14e() {
  // 1. invoke_cb_windowed route_run_begin/end + LoadGlobal inside callbacks,
  //    AND mid-callback global route mutation invalidation
  globalThis.__s14eG = 10;
  function readG(x) { return x + __s14eG; }
  function runCb(arr) {
    let acc = 0;
    arr.forEach((v) => { acc += readG(v); });
    const mapped = arr.map((v) => readG(v) * 2);
    const filt = mapped.filter((v) => v > __s14eG * 2);
    return acc + filt.length;
  }
  const baseArr = [1, 2, 3, 4];
  let cbSum = runCb(baseArr);
  // Bump global_route_epoch via unrelated accessor on globalThis
  Object.defineProperty(globalThis, "__s14eUnrelated", {
    get() { return 1; },
    configurable: true,
  });
  for (let i = 0; i < 320; i++) cbSum += runCb(baseArr);
  // Mutate __s14eG INSIDE a forEach callback and read immediately in same callback
  let midCbSeen = 0;
  [1, 2, 3].forEach((v) => {
    if (v === 2) {
      Object.defineProperty(globalThis, "__s14eG", {
        get() { return 77; },
        configurable: true,
      });
    }
    midCbSeen += readG(v);
  });
  midCbSeen += readG(0);
  delete globalThis.__s14eUnrelated;
  delete globalThis.__s14eG;

  // 2. Stale ic_class_method_read / ic_class_getter_read after class_proto_epoch bump
  //    + genuine prototype method/getter override invalidation
  class Parser14e {
    constructor(mode, pos, len) {
      this.mode = mode;
      this.pos = pos;
      this.len = len;
    }
    inAttr() { return this.mode === 1; }
    get ready() { return this.pos < this.len; }
    step() {
      let s = 0;
      if (this.inAttr()) s += 3;
      if (this.ready) s += 5;
      return s;
    }
  }
  const p1 = new Parser14e(1, 0, 4);
  const p2 = new Parser14e(2, 5, 4);
  let clsSum = 0;
  for (let i = 0; i < 10; i++) clsSum += p1.step() + p2.step();
  // Unrelated class with computed member bumps class_proto_epoch
  class Unrelated14e {
    [Symbol.iterator]() { return { next() { return { done: true }; } }; }
  }
  if (typeof new Unrelated14e()[Symbol.iterator] === "function") clsSum += 1;
  // Now drive Tier O compilation & execution
  for (let i = 0; i < 320; i++) clsSum += p1.step() + p2.step();
  // Genuine prototype mutation must invalidate immediately
  Parser14e.prototype.inAttr = function() { return this.mode === 2; };
  Object.defineProperty(Parser14e.prototype, "ready", {
    get() { return this.pos === 5; },
    configurable: true,
  });
  for (let i = 0; i < 40; i++) clsSum += p1.step() + p2.step();

  // 3. jit_inline_mega_get_prop on HeapObj::Native (Array.isArray) +
  //    Op::GetPropPoly SHAPE_PAIR_ABSENT_SLOT + Object.prototype mutation + null proto
  function isParent14e(n) { return !!(n && Array.isArray(n.children)); }
  function walk14e(list) {
    let c = 0;
    for (let i = 0; i < list.length; i++) {
      const n = list[i];
      if (isParent14e(n)) c += n.children.length;
      else c += 1;
    }
    return c;
  }
  const elNode = { type: "el", children: [10, 20, 30] };
  const txtNode = { type: "txt", value: "hello" };
  const cmtNode = { type: "cmt", data: "note", extra: 1 };
  const nullNode = Object.create(null);
  nullNode.type = "null";
  const nodes = [elNode, txtNode, cmtNode, nullNode];
  let walkSum = 0;
  for (let i = 0; i < 350; i++) walkSum += walk14e(nodes);
  // Mutate Object.prototype.children -> txtNode & cmtNode see [9, 8], nullNode still absent!
  Object.prototype.children = [9, 8];
  const afterProtoMut = walk14e(nodes);
  delete Object.prototype.children;
  const afterProtoRestore = walk14e(nodes);
  // Mutate Array.isArray (data overwrite + getter defineProperty + restore) inside inlined isParent14e
  const strChildNodes = [elNode, { type: "str", children: "abcde" }];
  const origIsArray = Array.isArray;
  Array.isArray = () => false;
  const afterArrDataOv = walk14e(strChildNodes);
  Object.defineProperty(Array, "isArray", {
    get() { return (x) => x !== undefined; },
    configurable: true,
  });
  const afterArrGetOv = walk14e(strChildNodes);
  Object.defineProperty(Array, "isArray", {
    value: origIsArray,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  const afterArrRestore = walk14e(strChildNodes);

  // 4. jit_str_case_pure_pin inside inlined callee (changed ASCII, 1-char, cached, >32B, non-ASCII)
  function localCase14e(s, up) { return up ? s.toUpperCase() : s.toLowerCase(); }
  function tagScore14e(arr) {
    let s = 0;
    for (let i = 0; i < arr.length; i++) {
      const up = localCase14e(arr[i], true);
      const lo = localCase14e(arr[i], false);
      if (up === "DIV") s += 7;
      if (lo === "span") s += 5;
      s += up.length + lo.charCodeAt(0);
    }
    return s;
  }
  const strInputs = ["div", "DIV", "Span", "a", "Z", "abcdefghijklmnopqrstuvwxyz0123456789", "caf\u00e9"];
  let caseSum = 0;
  for (let i = 0; i < 350; i++) caseSum += tagScore14e(strInputs);
  // Override String.prototype.toUpperCase -> must observe override immediately
  const origUpper = String.prototype.toUpperCase;
  String.prototype.toUpperCase = function() { return "DIV"; };
  const caseOv = tagScore14e(["xyz"]);
  String.prototype.toUpperCase = origUpper;
  const caseRest = tagScore14e(["div"]);

  // 5. Commit-tail StoreSlot D3 (d3-mut): windowed Tier O + cross3_dyn +
  //    pure D3 caller rejecting bit-0 entry (no double mutation on later deopt) +
  //    pre-deopt StoreSlot function (no double mutation on mid-body deopt)
  class Lexer14e {
    constructor() { this.state = 0; this.pos = 0; this.count = 0; }
    stateStep(ch) {
      if (ch === 60) {
        this.state = 2;
        this.pos += 1;
        return 1;
      }
      this.pos += 1;
      return 0;
    }
    preDeoptStore(x, obj) {
      this.count += 1;
      return obj.v + x;
    }
  }
  const lex = new Lexer14e();
  function driveWin(lx, n) {
    let s = 0;
    for (let i = 0; i < n; i++) s += lx.stateStep((i & 3) === 0 ? 60 : 65);
    return s;
  }
  function driveDyn(target, ch) { return target.stateStep(ch); }
  function pureCallingMut(lx, ch, box) {
    const r = lx.stateStep(ch);
    return r + box.x;
  }
  let d3Sum = 0;
  for (let i = 0; i < 60; i++) d3Sum += driveWin(lex, 8);
  const alt = { stateStep(c) { return c === 60 ? 2 : 0; } };
  for (let i = 0; i < 350; i++) {
    d3Sum += driveDyn((i & 7) === 0 ? alt : lex, (i & 1) === 0 ? 60 : 65);
  }
  const okBox = { x: 4 };
  for (let i = 0; i < 350; i++) d3Sum += pureCallingMut(lex, 60, okBox);
  // Trigger deopt in pureCallingMut AFTER lex.stateStep(60) via getter on box.x
  const posBefore = lex.pos;
  const trapBox = { get x() { return 100; } };
  d3Sum += pureCallingMut(lex, 60, trapBox);
  const deltaPure = lex.pos - posBefore; // MUST be 1, never 2!

  // Pre-deopt StoreSlot function: trigger getter on obj.v after this.count += 1
  const vOk = { v: 3 };
  for (let i = 0; i < 350; i++) d3Sum += lex.preDeoptStore(1, vOk);
  const cntBefore = lex.count;
  const vTrap = { get v() { return 50; } };
  d3Sum += lex.preDeoptStore(1, vTrap);
  const deltaPre = lex.count - cntBefore; // MUST be 1, never 2!

  return `s14e:${cbSum}:${midCbSeen}:${clsSum}:${walkSum}:${afterProtoMut}:${afterProtoRestore}:${afterArrDataOv}:${afterArrGetOv}:${afterArrRestore}:${caseSum}:${caseOv}:${caseRest}:${d3Sum}:${lex.pos}:${deltaPure}:${lex.count}:${deltaPre}`;
}

function testSprint14f() {
  // 1. Hoisted Stmt::FnDecl factories (hast-util-is-element / unist-util-is shape)
  //    + nested ParentUpvalVal FnDecl + TDZ const rejection + recursive FnDecl + mutated param
  function tagNameFactory(tagName) {
    return check;
    function check(node) {
      return Boolean(node && node.type === "element" && node.tagName === tagName);
    }
  }
  function castFactory(test) {
    return check;
    function check(node, index, parent) {
      return Boolean(test.call(this, node, index, parent));
    }
  }
  function nestedFactory(a, b) {
    return makeInner;
    function makeInner(c) {
      return leaf;
      function leaf(d) {
        return a + b + c + d;
      }
    }
  }
  function tdzHoist(callBefore) {
    function readC() { return c; }
    let caught = 0;
    if (callBefore) {
      try { readC(); } catch (e) { if (e instanceof ReferenceError) caught = 1; }
    }
    const c = 77;
    return caught * 100 + readC();
  }
  function recHoist(k, n) {
    function fact(m) { return m <= 1 ? k : m * fact(m - 1); }
    return fact(n);
  }
  function mutParamHoist(x) {
    function bump() { x += 1; return x; }
    const a = bump();
    const b = bump();
    return a * 10 + b + x;
  }

  const nodes = [
    { type: "element", tagName: "div" },
    { type: "element", tagName: "span" },
    { type: "text", value: "hi" },
    null,
  ];
  let factHits = 0;
  let nestSum = 0;
  // > 5000 closures so ready_closures drains and refills multiple times under D3!
  for (let i = 0; i < 2600; i++) {
    const f1 = tagNameFactory((i & 1) === 0 ? "div" : "span");
    const f2 = castFactory(f1);
    const nd = nodes[i & 3];
    if (f1(nd)) factHits += 1;
    if (f2(nd, i, null)) factHits += 2;
  }
  for (let i = 0; i < 400; i++) {
    const mid = nestedFactory(i & 7, 1.5);
    const lf = mid((i & 3) + 1);
    nestSum += lf(2);
  }
  const tdzRes = tdzHoist(true) + tdzHoist(false);
  let recSum = 0;
  let mutSum = 0;
  for (let i = 0; i < 350; i++) {
    recSum += recHoist(2, 4);
    mutSum += mutParamHoist(i & 3);
  }

  // 2. GlobalFn::Boolean across all value types + 0-arg + inlined + D3 + monkey-patch guard
  function boolInlined(x) { return Boolean(x); }
  function boolD3(x, empty) { return empty ? Boolean() : Boolean(x); }
  const vals = [
    false, true, 0, -0, 1, -5, 0.0, -0.0, NaN, 1.5,
    null, undefined, "", "a", {}, [], 0n, 1n, Symbol("s"),
  ];
  let boolSum = 0;
  for (let i = 0; i < 400; i++) {
    const v = vals[i % vals.length];
    if (boolInlined(v)) boolSum += 1;
    if (boolD3(v, (i & 15) === 0)) boolSum += 2;
    if (Boolean(v)) boolSum += 4;
    if (Boolean()) boolSum += 8;
  }
  const origBool = globalThis.Boolean;
  globalThis.Boolean = (x) => "p:" + String(x);
  const patchInl = boolInlined(0);
  const patchD3 = boolD3(0, false);
  const patchTop = Boolean(0);
  globalThis.Boolean = origBool;
  const restInl = boolInlined(0);
  const restD3 = boolD3(1, false);

  // 3. D3 inline MakeClosure / MakeArrow (1..4 captures, mixed I32/F64/Bool/Tagged, lexical this)
  function pack4(a, b, c) {
    const flt = c + 0.5;
    const flag = (a & 1) === 0;
    return function(x) {
      return (flag ? a : b) + flt + x;
    };
  }
  const arrowHost = {
    base: 100,
    make(a, b) {
      return (x) => this.base + a + b + x;
    },
  };
  let packSum = 0;
  let arrSum = 0;
  for (let i = 0; i < 400; i++) {
    const f4 = pack4(i & 7, (i & 3) + 10, i & 1);
    packSum += f4(3);
    const af = arrowHost.make(i & 7, 2);
    arrSum += af(5);
  }

  // 4. Unsettled ObjMap recycling (SetProp-only dead ctor objects) + key_pool_used dynamic keys
  function Box14f(a, b) {
    this.a = a;
    this.b = b;
  }
  let boxSum = 0;
  for (let i = 0; i < 2500; i++) {
    new Box14f(i, i + 1); // never read -> hot_mirror.shape == 0 when collected
    const live = new Box14f(i & 7, 1);
    boxSum += live.a + live.b;
  }
  const dyn = {};
  let dynSum = 0;
  for (let i = 0; i < 400; i++) {
    const k = "k_" + (i & 31);
    dyn[k] = i;
    dynSum += dyn[k];
    if ((i & 3) === 0) delete dyn[k];
  }

  // 5. Dead-end throw block inside pure D3 guard-clause function (convertElement shape)
  function guardThrow(x) {
    if (typeof x === "string") return x.length;
    if (typeof x === "number") return x * 2;
    throw new Error("bad:" + String(x));
  }
  let gSum = 0;
  let gCaught = 0;
  for (let i = 0; i < 400; i++) {
    gSum += guardThrow((i & 1) === 0 ? "abcd" : (i & 7));
  }
  try {
    guardThrow(true);
  } catch (e) {
    if (e instanceof Error && e.message === "bad:true") gCaught++;
  }

  return `s14f:${factHits}:${nestSum}:${tdzRes}:${recSum}:${mutSum}:${boolSum}:${patchInl}:${patchD3}:${patchTop}:${restInl}:${restD3}:${packSum}:${arrSum}:${boxSum}:${dynSum}:${gSum}:${gCaught}`;
}

function testSprint14f2() {
  "use strict";
  // 1. Unboxed I32 CharCodeAt: ASCII, packed short, UTF-16/CJK, cons string, OOB NaN, custom receiver
  function scanCodes(s) {
    let sum = 0;
    let cjk = 0;
    let words = 0;
    let inW = false;
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if ((c >= 0x3040 && c <= 0x309f) || (c >= 0x4e00 && c <= 0x9fff)) {
        cjk++;
        inW = false;
      } else if (c <= 32) {
        inW = false;
      } else if (!inW) {
        words++;
        inW = true;
      }
      sum += c;
    }
    return sum + cjk * 1000 + words * 100;
  }
  function oobCheck(s, idx) {
    const c = s.charCodeAt(idx);
    return Number.isNaN(c) ? -1 : c;
  }
  const consStr = ("hello world foo bar ").repeat(3) + "\u4e2d\u6587\u3042";
  const samples = ["a", "abc123", "Hello world test string 12345", "caf\u00e9 \u4e2d\u6587", consStr];
  let ccSum = 0;
  for (let i = 0; i < 350; i++) {
    ccSum += scanCodes(samples[i % samples.length]);
  }
  let oobSum = 0;
  for (let i = 0; i < 400; i++) {
    const s = samples[i % samples.length];
    oobSum += oobCheck(s, i < 300 ? (i % s.length) : s.length + 2);
  }
  const customRecv = { charCodeAt(i) { return 700 + i; } };
  const customRes = oobCheck(customRecv, 5);

  // 2. Inline constant-key shaped SetIndex (Symbol NEXT/PREV + polymorphic shapes + freeze/readonly/setter + GC barrier)
  const NEXT = Symbol("next");
  const PREV = Symbol("prev");
  const OTHER = Symbol("other");
  function linkNodes(prev, curr, next) {
    prev[NEXT] = curr;
    curr[PREV] = prev;
    curr[NEXT] = next;
    next[PREV] = curr;
  }
  class NodeA { constructor(id) { this.id = id; this[NEXT] = null; this[PREV] = null; } }
  class NodeB { constructor(id) { this.id = id; this.tag = "b"; this[NEXT] = null; this[PREV] = null; } }
  const n0 = new NodeA(0);
  const nEnd = new NodeB(999);
  let linkSum = 0;
  for (let i = 0; i < 600; i++) {
    const mid = (i & 1) === 0 ? new NodeA(i) : new NodeB(i);
    linkNodes(n0, mid, nEnd);
    linkSum += n0[NEXT].id + nEnd[PREV].id + mid[PREV].id + mid[NEXT].id;
  }
  let strictErrs = 0;
  const frozenNode = Object.freeze(new NodeA(7));
  try { linkNodes(frozenNode, new NodeA(8), nEnd); } catch (e) { if (e instanceof TypeError) strictErrs += 1; }
  const roNode = new NodeA(9);
  Object.defineProperty(roNode, NEXT, { writable: false });
  try { linkNodes(roNode, new NodeA(10), nEnd); } catch (e) { if (e instanceof TypeError) strictErrs += 10; }
  let setHits = 0;
  const setNode = new NodeA(11);
  Object.defineProperty(setNode, NEXT, { set(v) { setHits += v.id; }, get() { return null; }, configurable: true });
  linkNodes(setNode, new NodeA(12), nEnd);
  function writeKey(o, k, v) { o[k] = v; }
  const wkObj = { [NEXT]: 0, [OTHER]: 0 };
  for (let i = 0; i < 350; i++) writeKey(wkObj, NEXT, i);
  writeKey(wkObj, OTHER, 42);
  const keyMismatchRes = wkObj[NEXT] + wkObj[OTHER];

  // 3. Guarded Partial-D3: Mode B (fast-return void state handler) + Mode C (CommitTailMut with cold branch)
  class MiniTok {
    constructor() {
      this.state = 1;
      this.index = 0;
      this.sectionStart = 0;
      this.emits = 0;
      this.piCount = 0;
    }
    isEnd(c) { return c === 62 || c === 32 || c === 47; }
    stateInTagName(c) {
      if (this.isEnd(c)) {
        this.emits += 1;
        this.sectionStart = -1;
        this.state = 2;
        this.stateAfter(c);
      }
    }
    stateAfter(c) {
      if (c === 62) { this.state = 1; this.sectionStart = this.index + 1; }
    }
    stateBeforeTagName(c) {
      if (c === 33) {
        this.state = 5;
        this.sectionStart = this.index + 1;
      } else if (c === 63) {
        this.piCount += 1;
        this.piExtra = this.index;
        this.state = 6;
      } else if (c === 47) {
        this.state = 3;
      } else {
        this.state = 4;
        this.sectionStart = this.index;
      }
    }
  }
  const tok = new MiniTok();
  const tagChars = [100, 105, 118, 115, 112, 97, 110, 62];
  for (let i = 0; i < 600; i++) {
    tok.index = i;
    tok.stateInTagName(tagChars[i & 7]);
  }
  const preChars = [97, 47, 33, 98];
  for (let i = 0; i < 600; i++) {
    tok.index = i;
    tok.stateBeforeTagName(preChars[i & 3]);
  }
  tok.index = 777;
  tok.stateBeforeTagName(63);
  const tokRes = `${tok.emits}:${tok.state}:${tok.sectionStart}:${tok.piCount}:${tok.piExtra}`;

  return `s14f2:${ccSum}:${oobSum}:${customRes}:${linkSum}:${strictErrs}:${setHits}:${keyMismatchRes}:${tokRes}`;
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
console.log(testCharCodeAtAndHoist());
console.log(testVolPreserveColdPaths());
console.log(testPackedStrAndPureLen());
console.log(testReplanBackedgeAndOps());
console.log(testInlineAddWaySymAddAndDerived());
console.log(testPolyClosureAndInlineMega());
console.log(testStrCaseInline());
console.log(testForOfFinallyAndDeoptFeedback());
console.log(testD3DirectRegisterCalls());
console.log(testD3BuiltinsGettersAndCall());
console.log(testRestApplyOnly());
console.log(testHasPropStrictEqWeakMap());
console.log(testSprint12aLoopsKeyedIcWeakMap());
console.log(testSprint12b());
console.log(testSprint13a());
console.log(testSprint13b());
console.log(testSprint13c());
console.log(testSprint13d());
console.log(testSprint14a());
console.log(testSprint14b());
console.log(testSprint14c());
console.log(testSprint14d());
console.log(testSprint14e());
console.log(testSprint14f());
console.log(testSprint14f2());
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
    "cca_hoist:421722152:29981",
    "vol_cold:27754:480000",
    "packed_len:400:32798:15987:3",
    "replan_ops:600:100:59400:1800:136350:3000",
    "addway_derived:273600:62900:11:11225:27565:95910:1100",
    "poly_inline:2640:297:34:12:1:34200:1:600:600:342",
    "strcase:250:3300:over:div:div",
    "forof_fin:66996:7609:1011:400:1879",
    "d3_calls:69600:600:1461:2:8760:210",
    "d3_ext:2400:2000:800:12000:267:1",
    "rest_apply:240:235550:129:320:2514:320:14",
    "has_eq_wm:540:1:0:0:180:1280:1512",
    "s12a:-31340:21000:3184:210000:1852",
    "s12b:1392355:1652:6050:4800:720:4480:380:76089:1:159400:1:3227",
    "s13a:81819:57405:10126:8219",
    "s13b:3200:4:53280:111:107:260617:22560",
    "s13c:153920:1:9622000:644245094640:31:51038",
    "s13d:900:7200:30200:127:132000000000:60125900",
    "s14a:255:2:12000:181500",
    "s14b:1583800:4066800:6251588",
    "s14c:13547:1798:6100:370:127",
    "s14d:600:161600:128099:89200:84200:7184:184275:14674:7:364690",
    "s14e:17334:247:2961:2100:8:6:2:8:4:280350:130:110:3641:1137:1:351:1",
    "s14f:3900:3800:254:16800:11176:1301:p:0:p:0:p:0:false:true:4600:44200:11242:79800:2400:1",
    "s14f2:8438290:465700:705:958800:11:12:391:75:6:599:1:777",
];

const D3_OVERFLOW_PROGRAM: &str = r#"
"use strict";
function infRec(n) {
  return infRec(n + 1) + 1;
}
function warm(n) {
  if (n <= 0) return 0;
  return 1 + warm(n - 1);
}
class LoopG {
  constructor(stop) { this._stop = stop; }
  get boom() { return this._stop ? 1 : this.boom; }
}
function readBoom(x) { return x.boom; }
let s = 0;
const okG = new LoopG(true);
for (let i = 0; i < 320; i++) s += warm(3) + readBoom(okG);
let caught = 0;
try {
  infRec(0);
} catch (e) {
  if (e instanceof RangeError) caught += 1;
}
try {
  readBoom(new LoopG(false));
} catch (e) {
  if (e instanceof RangeError) caught += 10;
}
console.log(`overflow:${s}:${caught}`);
"#;

#[test]
fn opt_d3_stack_overflow_range_error_single() {
    assert_eq!(run_ok(D3_OVERFLOW_PROGRAM), ["overflow:1280:11"]);
}

#[test]
fn opt_d3_stack_overflow_range_error() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, envs) in [
        ("opt", vec![("ZIPP_OPT", "1")]),
        ("opt_deopt", vec![("ZIPP_OPT", "1"), ("ZIPP_OPT_DEOPT_EVERY", "5")]),
        ("nojit", vec![("ZIPP_NOJIT", "1")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("opt_d3_stack_overflow_range_error_single").arg("--nocapture");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} overflow mode failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn run_ok(source: &str) -> Vec<String> {
    let out = zipp_vm::run(source).expect("program compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}\noutput so far: {:?}",
        out.error,
        out.output
    );
    out.output
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
