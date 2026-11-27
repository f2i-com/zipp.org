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
