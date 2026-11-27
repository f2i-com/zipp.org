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
