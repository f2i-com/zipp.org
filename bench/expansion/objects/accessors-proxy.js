// accessors-proxy: accessor properties, Proxy traps, Reflect and frozen objects.
//
// What it measures: the property paths that do not end at a plain data slot.
//   1. object-literal getters and setters (get n() / set n(v)), monomorphic;
//   2. Object.defineProperty accessors, both on a constructor prototype and on
//      each instance (closure-backed, redefined every round), plus
//      Object.defineProperties and Object.getOwnPropertyDescriptor;
//   3. Proxy objects with get/set/has traps (an observable store, a
//      default-value dictionary, a negative-index array view, and an
//      empty-handler forwarding proxy) used in hot loops;
//   4. Reflect.get/set/has/ownKeys/deleteProperty/defineProperty, including
//      Reflect.get with a receiver through an inherited getter;
//   5. Object.freeze'd configuration objects and arrays read in a loop, with
//      sloppy-mode writes to frozen, sealed and non-extensible objects that
//      must be silently ignored.
// Why it is a likely weak spot: inline caches are usually built for data
// properties; accessor calls, trap dispatch (handler lookup, argument
// marshalling, invariant checks), Reflect's generic paths and the frozen /
// non-writable checks on the store path all fall off the fast path, and an
// interpreter without a cheap native-to-JS call sequence pays per access.
// Dominant operations: getter/setter invocation, Proxy [[Get]]/[[Set]]/[[Has]]
// trap calls, Reflect.* calls, frozen-object element and property loads.
//
// SIZE is the number of rounds; every round does the same fixed amount of work.

const SIZE = 200;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }

// ---- 1. object-literal accessors ------------------------------------------
function makeCounter(start) {
  return {
    _n: start,
    _hits: 0,
    get n() { this._hits++; return this._n; },
    set n(v) { this._n = v & 0xffff; },
    get twice() { return this._n * 2; },
  };
}

function phaseLiteral(rounds) {
  const objs = [];
  for (let i = 0; i < 8; i++) objs.push(makeCounter(i * 7));
  let h = 0;
  for (let r = 0; r < rounds; r++) {
    for (let i = 0; i < 3000; i++) {
      const o = objs[i & 7];
      o.n = o.n + i + r;
      h = (h + o.twice) | 0;
    }
  }
  let hits = 0;
  for (let i = 0; i < objs.length; i++) { hits += objs[i]._hits; h = mix(h, objs[i]._n); }
  return [h, hits];
}

// ---- 2. Object.defineProperty accessors ----------------------------------
function Vec(x, y) { this._x = x; this._y = y; }
Object.defineProperty(Vec.prototype, "x", {
  get() { return this._x; }, set(v) { this._x = v | 0; }, configurable: true,
});
Object.defineProperty(Vec.prototype, "y", {
  get() { return this._y; }, set(v) { this._y = v | 0; }, configurable: true,
});
Object.defineProperties(Vec.prototype, {
  manhattan: { get() { return Math.abs(this._x) + Math.abs(this._y); } },
  kind: { value: 3, writable: false, enumerable: false },
});

function makeTemp(c) {
  const t = { id: c };
  let celsius = c;
  Object.defineProperty(t, "c", {
    get() { return celsius; }, set(v) { celsius = v; }, enumerable: true, configurable: true,
  });
  Object.defineProperty(t, "f", {
    get() { return celsius * 9 / 5 + 32; },
    set(v) { celsius = (v - 32) * 5 / 9; },
    enumerable: true, configurable: true,
  });
  return t;
}

function phaseDefine(rounds) {
  let h = 0, descs = 0;
  const vecs = [];
  for (let i = 0; i < 64; i++) vecs.push(new Vec(i - 32, 17 - i));
  for (let r = 0; r < rounds; r++) {
    for (let i = 0; i < 1500; i++) {
      const v = vecs[i & 63];
      v.x = v.y + (i & 15) - 7;
      v.y = (v.x * 3 - v.y) % 1000;
      h = (h + v.manhattan + v.kind) | 0;
    }
    const temps = [];
    for (let i = 0; i < 24; i++) temps.push(makeTemp(i + r % 13));
    for (let i = 0; i < 600; i++) {
      const t = temps[i % 24];
      t.f = t.f + 1.8;
      h = (h + ((t.c * 16) | 0)) | 0;
    }
    const d = Object.getOwnPropertyDescriptor(temps[r % 24], "f");
    if (typeof d.get === "function" && d.enumerable && !("value" in d)) descs++;
    h = mix(h, Object.keys(temps[5]).length);
  }
  for (let i = 0; i < vecs.length; i++) h = mix(h, vecs[i]._x ^ (vecs[i]._y << 8));
  return [h, descs];
}

// ---- 3. Proxy traps ------------------------------------------------------
function makeObservable(target, log) {
  return new Proxy(target, {
    get(t, k, recv) { log[0]++; return Reflect.get(t, k, recv); },
    set(t, k, v, recv) { log[1]++; return Reflect.set(t, k, v, recv); },
    has(t, k) { log[2]++; return k in t; },
  });
}

function makeDefaultDict() {
  return new Proxy({}, {
    get(t, k) { return k in t ? t[k] : 0; },
    set(t, k, v) { t[k] = v; return true; },
  });
}

function makeNegIndex(arr) {
  return new Proxy(arr, {
    get(t, k, recv) {
      if (typeof k === "string") {
        const i = +k;
        if (i < 0) return t[t.length + i];
      }
      return Reflect.get(t, k, recv);
    },
  });
}

function phaseProxy(rounds, words) {
  const log = [0, 0, 0];
  const store = makeObservable({ a: 1, b: 2, c: 3, total: 0 }, log);
  const dict = makeDefaultDict();
  const base = [];
  for (let i = 0; i < 64; i++) base.push((i * 37) & 255);
  const neg = makeNegIndex(base);
  const fwd = new Proxy({ p: 5, q: 6 }, {});
  let h = 0, present = 0;
  for (let r = 0; r < rounds; r++) {
    for (let i = 0; i < 400; i++) {
      store.total = (store.total + store.a * store.b + (i & 7)) & 0xfffff;
      if ("c" in store) store.c = store.c + 1;
      if ("missing" in store) present++;
    }
    for (let i = 0; i < 300; i++) {
      const w = words[(i * 7 + r) & 255];
      dict[w] = dict[w] + 1;
    }
    for (let i = 1; i <= 300; i++) {
      h = (h + neg[-(i & 63) - 1] + neg[i & 63] + neg.length) | 0;
    }
    for (let i = 0; i < 300; i++) {
      fwd.p = (fwd.p + fwd.q) & 1023;
      if ("q" in fwd) h = (h + fwd.p) | 0;
    }
  }
  const keys = Object.keys(dict);
  for (let i = 0; i < keys.length; i++) h = mix(h, dict[keys[i]] + keys[i].length);
  h = mix(h, store.total);
  h = mix(h, store.c);
  return [h, log[0], log[1], log[2], present, keys.length];
}

// ---- 4. Reflect ----------------------------------------------------------
function phaseReflect(rounds, words) {
  const tag = Symbol("tag");
  const proto = { get scaled() { return this.raw * 3; } };
  let h = 0, keyTotal = 0, deleted = 0;
  for (let r = 0; r < rounds; r++) {
    const o = Object.create(proto);
    o.raw = r & 31;
    for (let i = 0; i < 12; i++) Reflect.set(o, words[(r + i * 5) & 255], i);
    o[tag] = r;
    o[r & 7] = r;
    for (let i = 0; i < 600; i++) {
      const k = words[(i * 3 + r) & 255];
      if (Reflect.has(o, k)) h = (h + Reflect.get(o, k)) | 0;
      else Reflect.set(o, "raw", (Reflect.get(o, "raw") + 1) & 255);
      h = (h + Reflect.get(proto, "scaled", o)) | 0;
    }
    const ks = Reflect.ownKeys(o);
    keyTotal += ks.length;
    for (let i = 0; i < ks.length; i++) if (typeof ks[i] === "string") h = mix(h, ks[i].length);
    if (Reflect.deleteProperty(o, words[r & 255])) deleted++;
    Reflect.defineProperty(o, "fixed", { value: r, writable: false });
    if (!Reflect.set(o, "fixed", 1)) deleted++;
    h = mix(h, Reflect.getPrototypeOf(o) === proto ? Reflect.get(o, "fixed") : -1);
  }
  return [h, keyTotal, deleted];
}

// ---- 5. frozen objects ---------------------------------------------------
function phaseFrozen(rounds) {
  const rates = [];
  for (let i = 0; i < 16; i++) rates.push(i * 3 + 1);
  const config = Object.freeze({
    rates: Object.freeze(rates),
    limits: Object.freeze({ lo: 3, hi: 90 }),
    name: "cfg",
    depth: 4,
  });
  const records = [];
  for (let i = 0; i < 32; i++) records.push(Object.freeze({ id: i, w: (i * 13) & 31, next: (i + 5) & 31 }));
  const table = Object.freeze(records);
  const sealed = Object.seal({ a: 1, b: 2 });
  const closed = Object.preventExtensions({ v: 0 });
  let h = 0, ignored = 0;
  for (let r = 0; r < rounds; r++) {
    let at = r & 31;
    for (let i = 0; i < 2500; i++) {
      const rec = table[at];
      const v = rec.w * config.rates[i & 15];
      h = (h + (v < config.limits.lo ? config.limits.lo : v > config.limits.hi ? config.limits.hi : v) + config.depth) | 0;
      at = rec.next;
    }
    for (let i = 0; i < 100; i++) {
      config.depth = i;             // ignored: frozen
      table[i & 31] = null;         // ignored: frozen array
      config.rates[i & 15] = -1;    // ignored: frozen nested array
      sealed.a = sealed.a + 1;      // allowed: sealed but writable
      sealed.extra = i;             // ignored: sealed
      closed.v = closed.v + 1;      // allowed
      closed.w = i;                 // ignored: not extensible
    }
    if (config.depth === 4 && table[0] !== null && config.rates[3] === 10 &&
        !("extra" in sealed) && !("w" in closed)) ignored++;
    if (Object.isFrozen(config) && Object.isFrozen(table) && Object.isSealed(sealed) && !Object.isExtensible(closed)) ignored++;
  }
  h = mix(h, sealed.a);
  h = mix(h, closed.v);
  return [h, ignored];
}

function main() {
  const rounds = SIZE;
  const words = [];
  for (let i = 0; i < 256; i++) {
    let s = "";
    const len = 3 + ((rnd() * 6) | 0);
    for (let j = 0; j < len; j++) s += String.fromCharCode(97 + ((rnd() * 26) | 0));
    words.push(s + (i & 15));
  }
  const a = phaseLiteral(rounds);
  console.log("literal", a[0] >>> 0, a[1]);
  const b = phaseDefine(rounds);
  console.log("define", b[0] >>> 0, b[1]);
  const c = phaseProxy(rounds, words);
  console.log("proxy", c[0] >>> 0, c[1], c[2], c[3], c[4], c[5]);
  const d = phaseReflect(rounds, words);
  console.log("reflect", d[0] >>> 0, d[1], d[2]);
  const e = phaseFrozen(rounds);
  console.log("frozen", e[0] >>> 0, e[1]);
}

main();
