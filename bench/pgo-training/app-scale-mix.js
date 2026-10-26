"use strict";

// PGO-only workload: an application-scale program in the shape of a
// bundled library. Module closures export small constructor classes and
// helpers through a registry; records of several layouts flow through
// them. Its function count puts it in the engine's large-program mode
// (interpreter only, bodies compiled at first call), which the other
// training inputs never reach. Generated; independent of every scored
// benchmark.

var registry = {};
var order = [];
function define(name, factory) {
  var mod = factory(registry);
  registry[name] = mod;
  order.push(name);
  return mod;
}

var state = 7;
function roll(n) {
  state = (Math.imul(state, 1103) + 977) & 0x7fffffff;
  return (state >>> 7) % n;
}

function mix(acc, v) {
  return (Math.imul(acc ^ (v | 0), 0x2f1d) + 0x3b) | 0;
}

function strHash(s) {
  var h = 0x55;
  for (var i = 0; i < s.length; i++) h = (Math.imul(h, 131) + s.charCodeAt(i)) | 0;
  return h;
}

function looseIter(arr) {
  var it = typeof Symbol !== "object" && arr[Symbol.iterator];
  if (it) return (it = it.call(arr)).next.bind(it);
  var i = 0;
  return function () {
    return i < arr.length ? { done: false, value: arr[i++] } : { done: true };
  };
}

function variadic() {
  var total = 0;
  for (var i = 0; i < arguments.length; i++) total = mix(total, arguments[i]);
  return total;
}

function forward(fn, self) {
  return function () {
    return fn.apply(self, arguments);
  };
}

var LAYOUTS = [
  ["kind", "rank", "grade", "depth", "pad"],
  ["kind", "lot", "h", "w", "gap", "span", "depth", "phase", "grade", "pad"],
  ["kind", "span", "grade", "phase", "w", "rank", "depth", "pad", "tier", "h", "mark"],
  ["kind", "gap", "phase", "tier", "rank", "grade"],
  ["kind", "span", "tier", "h", "phase", "lot", "mark", "rank", "depth", "grade"],
  ["kind", "span", "pad", "phase", "rank", "w", "h", "tier", "gap", "mark", "grade"],
  ["kind", "phase", "rank", "w", "gap", "lot", "mark", "grade", "depth", "pad", "tier"],
  ["kind", "h", "pad", "depth", "gap", "mark", "w"],
  ["kind", "phase", "tier", "mark", "h", "span", "pad", "w"],
  ["kind", "tier", "pad", "h", "gap", "mark", "depth", "w"],
];

function makeRecord(i) {
  var layout = LAYOUTS[i % LAYOUTS.length];
  switch (i % LAYOUTS.length) {
    case 0:
      return { kind: "tile", rank: roll(204), grade: roll(319), depth: roll(210), pad: roll(91) };
    case 1:
      return { kind: "rack", lot: roll(341), h: roll(186), w: roll(344), gap: roll(341), span: roll(74), depth: roll(56), phase: roll(307), grade: roll(79), pad: roll(106) };
    case 2:
      return { kind: "lane", span: roll(230), grade: roll(442), phase: roll(382), w: roll(144), rank: roll(279), depth: roll(230), pad: roll(250), tier: roll(302), h: roll(68), mark: roll(160) };
    case 3:
      return { kind: "bolt", gap: roll(92), phase: roll(277), tier: roll(196), rank: roll(332), grade: roll(211) };
    case 4:
      return { kind: "seam", span: roll(377), tier: roll(301), h: roll(273), phase: roll(138), lot: roll(235), mark: roll(403), rank: roll(204), depth: roll(245), grade: roll(280) };
    case 5:
      return { kind: "knot", span: roll(269), pad: roll(267), phase: roll(314), rank: roll(281), w: roll(437), h: roll(90), tier: roll(118), gap: roll(212), mark: roll(220), grade: roll(89) };
    case 6:
      return { kind: "flag", phase: roll(398), rank: roll(68), w: roll(294), gap: roll(442), lot: roll(239), mark: roll(414), grade: roll(233), depth: roll(148), pad: roll(206), tier: roll(204) };
    case 7:
      return { kind: "pane", h: roll(431), pad: roll(335), depth: roll(184), gap: roll(314), mark: roll(438), w: roll(202) };
    case 8:
      return { kind: "tile", phase: roll(131), tier: roll(231), mark: roll(411), h: roll(225), span: roll(410), pad: roll(405), w: roll(250) };
    case 9:
      return { kind: "rack", tier: roll(319), pad: roll(260), h: roll(285), gap: roll(149), mark: roll(314), depth: roll(237), w: roll(266) };
  }
  return { kind: layout[0] };
}

define("amber0", function (reg) {
  var dep = null;
  var seen = new Map();
  var tags = new Set();
  function Amber0(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Amber0.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.grade | 0;
    var b = rec.phase | 0;
    this.score = mix(this.score, (a ^ b) & 0xffff);
    return this;
  };
  Amber0.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.span | 0;
      var b = rec.grade | 0;
      total = mix(total, (a << 2) - b);
    }
    return total;
  };
  Amber0.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "pane":
        return 2;
      case "bolt":
        return 3;
      case "seam":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.phase | 0;
    var b = rec.span | 0;
    var k = classify(rec);
    if (rec.grade === undefined) b = k;
    return Math.max(a, b) - Math.min(a, b) + k;
  }
  function bucket(recs) {
    var out = new Amber0("amber0", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 53);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-amber0");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("basil1", function (reg) {
  var dep = reg["amber0"];
  var seen = new Map();
  var tags = new Set();
  function Basil1(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Basil1.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.depth | 0;
    var b = rec.pad | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Basil1.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.tier | 0;
      var b = rec.depth | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Basil1.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "bolt":
        return 1;
      case "knot":
        return 2;
      case "flag":
        return 3;
      case "seam":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.pad | 0;
    var b = rec.tier | 0;
    var k = classify(rec);
    if (rec.depth === undefined) b = k;
    return a > b ? a - b : b - a + k;
  }
  function bucket(recs) {
    var out = new Basil1("basil1", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 28);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-basil1");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("cedar2", function (reg) {
  var dep = reg["basil1"];
  var seen = new Map();
  var tags = new Set();
  function Cedar2(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Cedar2.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.depth | 0;
    this.score = mix(this.score, a > b ? a - b : b - a);
    return this;
  };
  Cedar2.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.tier | 0;
      var b = rec.rank | 0;
      total = mix(total, (a ^ b) & 0xffff);
    }
    return total;
  };
  Cedar2.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "knot":
        return 1;
      case "bolt":
        return 2;
      case "seam":
        return 3;
      case "flag":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.depth | 0;
    var b = rec.tier | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return a * 2 + (b & 7) + k;
  }
  function bucket(recs) {
    var out = new Cedar2("cedar2", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 72);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-cedar2");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("dune3", function (reg) {
  var dep = reg["cedar2"];
  var seen = new Map();
  var tags = new Set();
  function Dune3(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Dune3.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.tier | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Dune3.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.h | 0;
      var b = rec.tier | 0;
      total = mix(total, a * 2 + (b & 7));
    }
    return total;
  };
  Dune3.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "tile":
        return 2;
      case "bolt":
        return 3;
      case "knot":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.h | 0;
    var k = classify(rec);
    if (rec.tier === undefined) b = k;
    return a - b + k;
  }
  function bucket(recs) {
    var out = new Dune3("dune3", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 22);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-dune3");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("ember4", function (reg) {
  var dep = reg["dune3"];
  var seen = new Map();
  var tags = new Set();
  function Ember4(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Ember4.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.gap | 0;
    var b = rec.mark | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Ember4.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.w | 0;
      var b = rec.gap | 0;
      total = mix(total, a + b);
    }
    return total;
  };
  Ember4.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "pane":
        return 1;
      case "flag":
        return 2;
      case "rack":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.mark | 0;
    var b = rec.w | 0;
    var k = classify(rec);
    if (rec.gap === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Ember4("ember4", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 99);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-ember4");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("fjord5", function (reg) {
  var dep = reg["ember4"];
  var seen = new Map();
  var tags = new Set();
  function Fjord5(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Fjord5.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.span | 0;
    this.score = mix(this.score, (a << 2) - b);
    return this;
  };
  Fjord5.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.mark | 0;
      var b = rec.rank | 0;
      total = mix(total, (a ^ b) & 0xffff);
    }
    return total;
  };
  Fjord5.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "tile":
        return 1;
      case "flag":
        return 2;
      case "seam":
        return 3;
      case "knot":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.span | 0;
    var b = rec.mark | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return a + b + k;
  }
  function bucket(recs) {
    var out = new Fjord5("fjord5", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 73);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-fjord5");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("grove6", function (reg) {
  var dep = reg["fjord5"];
  var seen = new Map();
  var tags = new Set();
  function Grove6(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Grove6.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.w | 0;
    this.score = mix(this.score, a > b ? a - b : b - a);
    return this;
  };
  Grove6.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.h | 0;
      var b = rec.rank | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Grove6.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "flag":
        return 2;
      case "tile":
        return 3;
      case "bolt":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.w | 0;
    var b = rec.h | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Grove6("grove6", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 61);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-grove6");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("heron7", function (reg) {
  var dep = reg["grove6"];
  var seen = new Map();
  var tags = new Set();
  function Heron7(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Heron7.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.phase | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, a > b ? a - b : b - a);
    return this;
  };
  Heron7.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.h | 0;
      var b = rec.phase | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Heron7.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "pane":
        return 1;
      case "knot":
        return 2;
      case "seam":
        return 3;
      case "bolt":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.h | 0;
    var k = classify(rec);
    if (rec.phase === undefined) b = k;
    return Math.imul(a, 3) + b + k;
  }
  function bucket(recs) {
    var out = new Heron7("heron7", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 10);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-heron7");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("iris8", function (reg) {
  var dep = reg["heron7"];
  var seen = new Map();
  var tags = new Set();
  function Iris8(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Iris8.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.mark | 0;
    var b = rec.depth | 0;
    this.score = mix(this.score, Math.imul(a, 3) + b);
    return this;
  };
  Iris8.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.w | 0;
      var b = rec.mark | 0;
      total = mix(total, a - b);
    }
    return total;
  };
  Iris8.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "lane":
        return 2;
      case "tile":
        return 3;
      case "seam":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.depth | 0;
    var b = rec.w | 0;
    var k = classify(rec);
    if (rec.mark === undefined) b = k;
    return a + b + k;
  }
  function bucket(recs) {
    var out = new Iris8("iris8", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 74);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-iris8");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("juniper9", function (reg) {
  var dep = reg["iris8"];
  var seen = new Map();
  var tags = new Set();
  function Juniper9(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Juniper9.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.gap | 0;
    var b = rec.rank | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Juniper9.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.phase | 0;
      var b = rec.gap | 0;
      total = mix(total, a + b);
    }
    return total;
  };
  Juniper9.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "knot":
        return 1;
      case "seam":
        return 2;
      case "lane":
        return 3;
      case "flag":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.rank | 0;
    var b = rec.phase | 0;
    var k = classify(rec);
    if (rec.gap === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Juniper9("juniper9", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 57);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-juniper9");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("kelp10", function (reg) {
  var dep = reg["juniper9"];
  var seen = new Map();
  var tags = new Set();
  function Kelp10(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Kelp10.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.phase | 0;
    var b = rec.mark | 0;
    this.score = mix(this.score, Math.imul(a, 3) + b);
    return this;
  };
  Kelp10.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.span | 0;
      var b = rec.phase | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Kelp10.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "pane":
        return 2;
      case "knot":
        return 3;
      case "bolt":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.mark | 0;
    var b = rec.span | 0;
    var k = classify(rec);
    if (rec.phase === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Kelp10("kelp10", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 82);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-kelp10");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("lumen11", function (reg) {
  var dep = reg["kelp10"];
  var seen = new Map();
  var tags = new Set();
  function Lumen11(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Lumen11.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.pad | 0;
    var b = rec.depth | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Lumen11.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.gap | 0;
      var b = rec.pad | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Lumen11.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "lane":
        return 1;
      case "seam":
        return 2;
      case "tile":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.depth | 0;
    var b = rec.gap | 0;
    var k = classify(rec);
    if (rec.pad === undefined) b = k;
    return Math.imul(a, 3) + b + k;
  }
  function bucket(recs) {
    var out = new Lumen11("lumen11", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 86);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-lumen11");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("moss12", function (reg) {
  var dep = reg["lumen11"];
  var seen = new Map();
  var tags = new Set();
  function Moss12(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Moss12.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.mark | 0;
    this.score = mix(this.score, (a << 2) - b);
    return this;
  };
  Moss12.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.phase | 0;
      var b = rec.rank | 0;
      total = mix(total, a - b);
    }
    return total;
  };
  Moss12.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "flag":
        return 1;
      case "pane":
        return 2;
      case "seam":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.mark | 0;
    var b = rec.phase | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return a > b ? a - b : b - a + k;
  }
  function bucket(recs) {
    var out = new Moss12("moss12", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 18);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-moss12");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("nectar13", function (reg) {
  var dep = reg["moss12"];
  var seen = new Map();
  var tags = new Set();
  function Nectar13(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Nectar13.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.lot | 0;
    var b = rec.tier | 0;
    this.score = mix(this.score, (a ^ b) & 0xffff);
    return this;
  };
  Nectar13.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.phase | 0;
      var b = rec.lot | 0;
      total = mix(total, a + b);
    }
    return total;
  };
  Nectar13.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "knot":
        return 1;
      case "bolt":
        return 2;
      case "tile":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.tier | 0;
    var b = rec.phase | 0;
    var k = classify(rec);
    if (rec.lot === undefined) b = k;
    return a * 2 + (b & 7) + k;
  }
  function bucket(recs) {
    var out = new Nectar13("nectar13", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 52);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-nectar13");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("onyx14", function (reg) {
  var dep = reg["nectar13"];
  var seen = new Map();
  var tags = new Set();
  function Onyx14(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Onyx14.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.depth | 0;
    var b = rec.mark | 0;
    this.score = mix(this.score, (a ^ b) & 0xffff);
    return this;
  };
  Onyx14.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.pad | 0;
      var b = rec.depth | 0;
      total = mix(total, Math.imul(a, 3) + b);
    }
    return total;
  };
  Onyx14.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "flag":
        return 1;
      case "bolt":
        return 2;
      case "seam":
        return 3;
      case "tile":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.mark | 0;
    var b = rec.pad | 0;
    var k = classify(rec);
    if (rec.depth === undefined) b = k;
    return a * 2 + (b & 7) + k;
  }
  function bucket(recs) {
    var out = new Onyx14("onyx14", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 44);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-onyx14");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("pollen15", function (reg) {
  var dep = reg["onyx14"];
  var seen = new Map();
  var tags = new Set();
  function Pollen15(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Pollen15.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.pad | 0;
    var b = rec.phase | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Pollen15.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.h | 0;
      var b = rec.pad | 0;
      total = mix(total, a > b ? a - b : b - a);
    }
    return total;
  };
  Pollen15.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "lane":
        return 1;
      case "bolt":
        return 2;
      case "pane":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.phase | 0;
    var b = rec.h | 0;
    var k = classify(rec);
    if (rec.pad === undefined) b = k;
    return Math.max(a, b) - Math.min(a, b) + k;
  }
  function bucket(recs) {
    var out = new Pollen15("pollen15", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 96);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-pollen15");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("quartz16", function (reg) {
  var dep = reg["pollen15"];
  var seen = new Map();
  var tags = new Set();
  function Quartz16(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Quartz16.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.h | 0;
    var b = rec.w | 0;
    this.score = mix(this.score, a > b ? a - b : b - a);
    return this;
  };
  Quartz16.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.gap | 0;
      var b = rec.h | 0;
      total = mix(total, (a ^ b) & 0xffff);
    }
    return total;
  };
  Quartz16.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "bolt":
        return 1;
      case "tile":
        return 2;
      case "pane":
        return 3;
      case "knot":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.w | 0;
    var b = rec.gap | 0;
    var k = classify(rec);
    if (rec.h === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Quartz16("quartz16", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 25);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-quartz16");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("reed17", function (reg) {
  var dep = reg["quartz16"];
  var seen = new Map();
  var tags = new Set();
  function Reed17(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Reed17.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.mark | 0;
    var b = rec.span | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Reed17.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.lot | 0;
      var b = rec.mark | 0;
      total = mix(total, a * 2 + (b & 7));
    }
    return total;
  };
  Reed17.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "knot":
        return 1;
      case "seam":
        return 2;
      case "lane":
        return 3;
      case "flag":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.span | 0;
    var b = rec.lot | 0;
    var k = classify(rec);
    if (rec.mark === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Reed17("reed17", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 63);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-reed17");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("sorrel18", function (reg) {
  var dep = reg["reed17"];
  var seen = new Map();
  var tags = new Set();
  function Sorrel18(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Sorrel18.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.lot | 0;
    var b = rec.depth | 0;
    this.score = mix(this.score, a * 2 + (b & 7));
    return this;
  };
  Sorrel18.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.mark | 0;
      var b = rec.lot | 0;
      total = mix(total, (a ^ b) & 0xffff);
    }
    return total;
  };
  Sorrel18.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "flag":
        return 1;
      case "rack":
        return 2;
      case "lane":
        return 3;
      case "tile":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.depth | 0;
    var b = rec.mark | 0;
    var k = classify(rec);
    if (rec.lot === undefined) b = k;
    return (a + b) >>> 1 + k;
  }
  function bucket(recs) {
    var out = new Sorrel18("sorrel18", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 78);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-sorrel18");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("thyme19", function (reg) {
  var dep = reg["sorrel18"];
  var seen = new Map();
  var tags = new Set();
  function Thyme19(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Thyme19.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.h | 0;
    var b = rec.w | 0;
    this.score = mix(this.score, (a << 2) - b);
    return this;
  };
  Thyme19.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.rank | 0;
      var b = rec.h | 0;
      total = mix(total, a - b);
    }
    return total;
  };
  Thyme19.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "pane":
        return 1;
      case "rack":
        return 2;
      case "lane":
        return 3;
      case "tile":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.w | 0;
    var b = rec.rank | 0;
    var k = classify(rec);
    if (rec.h === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Thyme19("thyme19", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 43);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-thyme19");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("umber20", function (reg) {
  var dep = reg["thyme19"];
  var seen = new Map();
  var tags = new Set();
  function Umber20(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Umber20.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.h | 0;
    var b = rec.span | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Umber20.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.rank | 0;
      var b = rec.h | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Umber20.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "flag":
        return 2;
      case "pane":
        return 3;
      case "tile":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.span | 0;
    var b = rec.rank | 0;
    var k = classify(rec);
    if (rec.h === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Umber20("umber20", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 37);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-umber20");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("vetch21", function (reg) {
  var dep = reg["umber20"];
  var seen = new Map();
  var tags = new Set();
  function Vetch21(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Vetch21.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.pad | 0;
    var b = rec.span | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Vetch21.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.phase | 0;
      var b = rec.pad | 0;
      total = mix(total, (a << 2) - b);
    }
    return total;
  };
  Vetch21.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "pane":
        return 2;
      case "lane":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.span | 0;
    var b = rec.phase | 0;
    var k = classify(rec);
    if (rec.pad === undefined) b = k;
    return Math.imul(a, 3) + b + k;
  }
  function bucket(recs) {
    var out = new Vetch21("vetch21", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 96);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-vetch21");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("willow22", function (reg) {
  var dep = reg["vetch21"];
  var seen = new Map();
  var tags = new Set();
  function Willow22(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Willow22.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.gap | 0;
    var b = rec.phase | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Willow22.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.grade | 0;
      var b = rec.gap | 0;
      total = mix(total, a + b);
    }
    return total;
  };
  Willow22.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "pane":
        return 2;
      case "rack":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.phase | 0;
    var b = rec.grade | 0;
    var k = classify(rec);
    if (rec.gap === undefined) b = k;
    return a * 2 + (b & 7) + k;
  }
  function bucket(recs) {
    var out = new Willow22("willow22", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 25);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-willow22");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("yarrow23", function (reg) {
  var dep = reg["willow22"];
  var seen = new Map();
  var tags = new Set();
  function Yarrow23(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Yarrow23.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.tier | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, (a | b) & 0x3ff);
    return this;
  };
  Yarrow23.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.w | 0;
      var b = rec.tier | 0;
      total = mix(total, (a ^ b) & 0xffff);
    }
    return total;
  };
  Yarrow23.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "knot":
        return 1;
      case "flag":
        return 2;
      case "pane":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.w | 0;
    var k = classify(rec);
    if (rec.tier === undefined) b = k;
    return (a + b) >>> 1 + k;
  }
  function bucket(recs) {
    var out = new Yarrow23("yarrow23", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 51);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-yarrow23");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("zinnia24", function (reg) {
  var dep = reg["yarrow23"];
  var seen = new Map();
  var tags = new Set();
  function Zinnia24(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Zinnia24.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.pad | 0;
    this.score = mix(this.score, a + b);
    return this;
  };
  Zinnia24.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.tier | 0;
      var b = rec.rank | 0;
      total = mix(total, Math.imul(a, 3) + b);
    }
    return total;
  };
  Zinnia24.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "pane":
        return 1;
      case "seam":
        return 2;
      case "rack":
        return 3;
      case "bolt":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.pad | 0;
    var b = rec.tier | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return (a + b) >>> 1 + k;
  }
  function bucket(recs) {
    var out = new Zinnia24("zinnia24", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 41);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-zinnia24");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("amber25", function (reg) {
  var dep = reg["zinnia24"];
  var seen = new Map();
  var tags = new Set();
  function Amber25(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Amber25.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.span | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, Math.imul(a, 3) + b);
    return this;
  };
  Amber25.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.grade | 0;
      var b = rec.span | 0;
      total = mix(total, a * 2 + (b & 7));
    }
    return total;
  };
  Amber25.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "tile":
        return 2;
      case "seam":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.grade | 0;
    var k = classify(rec);
    if (rec.span === undefined) b = k;
    return a + b + k;
  }
  function bucket(recs) {
    var out = new Amber25("amber25", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 78);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-amber25");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("basil26", function (reg) {
  var dep = reg["amber25"];
  var seen = new Map();
  var tags = new Set();
  function Basil26(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Basil26.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.gap | 0;
    var b = rec.phase | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Basil26.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.w | 0;
      var b = rec.gap | 0;
      total = mix(total, a > b ? a - b : b - a);
    }
    return total;
  };
  Basil26.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "flag":
        return 2;
      case "tile":
        return 3;
      case "pane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.phase | 0;
    var b = rec.w | 0;
    var k = classify(rec);
    if (rec.gap === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Basil26("basil26", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 29);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-basil26");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("cedar27", function (reg) {
  var dep = reg["basil26"];
  var seen = new Map();
  var tags = new Set();
  function Cedar27(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Cedar27.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.span | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Cedar27.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.tier | 0;
      var b = rec.rank | 0;
      total = mix(total, a > b ? a - b : b - a);
    }
    return total;
  };
  Cedar27.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "bolt":
        return 2;
      case "pane":
        return 3;
      case "tile":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.span | 0;
    var b = rec.tier | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return Math.max(a, b) - Math.min(a, b) + k;
  }
  function bucket(recs) {
    var out = new Cedar27("cedar27", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 41);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-cedar27");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("dune28", function (reg) {
  var dep = reg["cedar27"];
  var seen = new Map();
  var tags = new Set();
  function Dune28(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Dune28.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.span | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Dune28.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.depth | 0;
      var b = rec.span | 0;
      total = mix(total, (a | b) & 0x3ff);
    }
    return total;
  };
  Dune28.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "flag":
        return 1;
      case "tile":
        return 2;
      case "seam":
        return 3;
      case "pane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.depth | 0;
    var k = classify(rec);
    if (rec.span === undefined) b = k;
    return a > b ? a - b : b - a + k;
  }
  function bucket(recs) {
    var out = new Dune28("dune28", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 35);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-dune28");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("ember29", function (reg) {
  var dep = reg["dune28"];
  var seen = new Map();
  var tags = new Set();
  function Ember29(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Ember29.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.depth | 0;
    var b = rec.pad | 0;
    this.score = mix(this.score, Math.max(a, b) - Math.min(a, b));
    return this;
  };
  Ember29.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.tier | 0;
      var b = rec.depth | 0;
      total = mix(total, (a + b) >>> 1);
    }
    return total;
  };
  Ember29.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "flag":
        return 2;
      case "knot":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.pad | 0;
    var b = rec.tier | 0;
    var k = classify(rec);
    if (rec.depth === undefined) b = k;
    return a - b + k;
  }
  function bucket(recs) {
    var out = new Ember29("ember29", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 12);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-ember29");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("fjord30", function (reg) {
  var dep = reg["ember29"];
  var seen = new Map();
  var tags = new Set();
  function Fjord30(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Fjord30.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.span | 0;
    var b = rec.w | 0;
    this.score = mix(this.score, Math.imul(a, 3) + b);
    return this;
  };
  Fjord30.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.phase | 0;
      var b = rec.span | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Fjord30.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "bolt":
        return 1;
      case "knot":
        return 2;
      case "flag":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.w | 0;
    var b = rec.phase | 0;
    var k = classify(rec);
    if (rec.span === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Fjord30("fjord30", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 83);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-fjord30");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("grove31", function (reg) {
  var dep = reg["fjord30"];
  var seen = new Map();
  var tags = new Set();
  function Grove31(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Grove31.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.depth | 0;
    var b = rec.w | 0;
    this.score = mix(this.score, Math.max(a, b) - Math.min(a, b));
    return this;
  };
  Grove31.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.span | 0;
      var b = rec.depth | 0;
      total = mix(total, a - b);
    }
    return total;
  };
  Grove31.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "flag":
        return 2;
      case "lane":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.w | 0;
    var b = rec.span | 0;
    var k = classify(rec);
    if (rec.depth === undefined) b = k;
    return a * 2 + (b & 7) + k;
  }
  function bucket(recs) {
    var out = new Grove31("grove31", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 10);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-grove31");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("heron32", function (reg) {
  var dep = reg["grove31"];
  var seen = new Map();
  var tags = new Set();
  function Heron32(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Heron32.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.span | 0;
    var b = rec.phase | 0;
    this.score = mix(this.score, (a | b) & 0x3ff);
    return this;
  };
  Heron32.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.mark | 0;
      var b = rec.span | 0;
      total = mix(total, a * 2 + (b & 7));
    }
    return total;
  };
  Heron32.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "pane":
        return 1;
      case "lane":
        return 2;
      case "flag":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.phase | 0;
    var b = rec.mark | 0;
    var k = classify(rec);
    if (rec.span === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Heron32("heron32", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 58);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-heron32");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("iris33", function (reg) {
  var dep = reg["heron32"];
  var seen = new Map();
  var tags = new Set();
  function Iris33(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Iris33.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.gap | 0;
    var b = rec.h | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Iris33.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.tier | 0;
      var b = rec.gap | 0;
      total = mix(total, a > b ? a - b : b - a);
    }
    return total;
  };
  Iris33.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "tile":
        return 2;
      case "seam":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.h | 0;
    var b = rec.tier | 0;
    var k = classify(rec);
    if (rec.gap === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Iris33("iris33", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 93);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-iris33");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("juniper34", function (reg) {
  var dep = reg["iris33"];
  var seen = new Map();
  var tags = new Set();
  function Juniper34(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Juniper34.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.span | 0;
    var b = rec.lot | 0;
    this.score = mix(this.score, a + b);
    return this;
  };
  Juniper34.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.rank | 0;
      var b = rec.span | 0;
      total = mix(total, (a | b) & 0x3ff);
    }
    return total;
  };
  Juniper34.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "bolt":
        return 1;
      case "seam":
        return 2;
      case "lane":
        return 3;
      case "flag":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.lot | 0;
    var b = rec.rank | 0;
    var k = classify(rec);
    if (rec.span === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Juniper34("juniper34", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 22);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-juniper34");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("kelp35", function (reg) {
  var dep = reg["juniper34"];
  var seen = new Map();
  var tags = new Set();
  function Kelp35(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Kelp35.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.w | 0;
    var b = rec.rank | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Kelp35.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.depth | 0;
      var b = rec.w | 0;
      total = mix(total, (a + b) >>> 1);
    }
    return total;
  };
  Kelp35.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "flag":
        return 1;
      case "tile":
        return 2;
      case "knot":
        return 3;
      case "seam":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.rank | 0;
    var b = rec.depth | 0;
    var k = classify(rec);
    if (rec.w === undefined) b = k;
    return Math.max(a, b) - Math.min(a, b) + k;
  }
  function bucket(recs) {
    var out = new Kelp35("kelp35", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 52);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-kelp35");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("lumen36", function (reg) {
  var dep = reg["kelp35"];
  var seen = new Map();
  var tags = new Set();
  function Lumen36(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Lumen36.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.grade | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, a + b);
    return this;
  };
  Lumen36.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.rank | 0;
      var b = rec.grade | 0;
      total = mix(total, Math.imul(a, 3) + b);
    }
    return total;
  };
  Lumen36.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "rack":
        return 1;
      case "bolt":
        return 2;
      case "seam":
        return 3;
      case "flag":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.rank | 0;
    var k = classify(rec);
    if (rec.grade === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Lumen36("lumen36", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 66);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-lumen36");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("moss37", function (reg) {
  var dep = reg["lumen36"];
  var seen = new Map();
  var tags = new Set();
  function Moss37(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Moss37.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.h | 0;
    var b = rec.grade | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Moss37.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.phase | 0;
      var b = rec.h | 0;
      total = mix(total, (a << 2) - b);
    }
    return total;
  };
  Moss37.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "tile":
        return 1;
      case "flag":
        return 2;
      case "rack":
        return 3;
      case "lane":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.grade | 0;
    var b = rec.phase | 0;
    var k = classify(rec);
    if (rec.h === undefined) b = k;
    return (a ^ b) & 0xffff + k;
  }
  function bucket(recs) {
    var out = new Moss37("moss37", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 4; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 51);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-moss37");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("nectar38", function (reg) {
  var dep = reg["moss37"];
  var seen = new Map();
  var tags = new Set();
  function Nectar38(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Nectar38.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.rank | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, Math.imul(a, 3) + b);
    return this;
  };
  Nectar38.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.pad | 0;
      var b = rec.rank | 0;
      total = mix(total, a > b ? a - b : b - a);
    }
    return total;
  };
  Nectar38.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "lane":
        return 1;
      case "bolt":
        return 2;
      case "flag":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.pad | 0;
    var k = classify(rec);
    if (rec.rank === undefined) b = k;
    return a + b + k;
  }
  function bucket(recs) {
    var out = new Nectar38("nectar38", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 3; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 79);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-nectar38");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("onyx39", function (reg) {
  var dep = reg["nectar38"];
  var seen = new Map();
  var tags = new Set();
  function Onyx39(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Onyx39.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.depth | 0;
    var b = rec.tier | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Onyx39.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.gap | 0;
      var b = rec.depth | 0;
      total = mix(total, a > b ? a - b : b - a);
    }
    return total;
  };
  Onyx39.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "knot":
        return 2;
      case "tile":
        return 3;
      case "bolt":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.tier | 0;
    var b = rec.gap | 0;
    var k = classify(rec);
    if (rec.depth === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Onyx39("onyx39", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 73);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-onyx39");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("pollen40", function (reg) {
  var dep = reg["onyx39"];
  var seen = new Map();
  var tags = new Set();
  function Pollen40(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Pollen40.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.pad | 0;
    var b = rec.tier | 0;
    this.score = mix(this.score, a - b);
    return this;
  };
  Pollen40.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.mark | 0;
      var b = rec.pad | 0;
      total = mix(total, a * 2 + (b & 7));
    }
    return total;
  };
  Pollen40.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "flag":
        return 1;
      case "lane":
        return 2;
      case "knot":
        return 3;
      case "rack":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.tier | 0;
    var b = rec.mark | 0;
    var k = classify(rec);
    if (rec.pad === undefined) b = k;
    return a > b ? a - b : b - a + k;
  }
  function bucket(recs) {
    var out = new Pollen40("pollen40", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 4) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 52);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-pollen40");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("quartz41", function (reg) {
  var dep = reg["pollen40"];
  var seen = new Map();
  var tags = new Set();
  function Quartz41(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Quartz41.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.gap | 0;
    var b = rec.depth | 0;
    this.score = mix(this.score, (a | b) & 0x3ff);
    return this;
  };
  Quartz41.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.lot | 0;
      var b = rec.gap | 0;
      total = mix(total, a + b);
    }
    return total;
  };
  Quartz41.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "pane":
        return 1;
      case "rack":
        return 2;
      case "lane":
        return 3;
      case "knot":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.depth | 0;
    var b = rec.lot | 0;
    var k = classify(rec);
    if (rec.gap === undefined) b = k;
    return a > b ? a - b : b - a + k;
  }
  function bucket(recs) {
    var out = new Quartz41("quartz41", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 2; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 34);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-quartz41");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("reed42", function (reg) {
  var dep = reg["quartz41"];
  var seen = new Map();
  var tags = new Set();
  function Reed42(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Reed42.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.tier | 0;
    var b = rec.h | 0;
    this.score = mix(this.score, (a + b) >>> 1);
    return this;
  };
  Reed42.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.span | 0;
      var b = rec.tier | 0;
      total = mix(total, a + b);
    }
    return total;
  };
  Reed42.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "seam":
        return 1;
      case "tile":
        return 2;
      case "flag":
        return 3;
      case "bolt":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.h | 0;
    var b = rec.span | 0;
    var k = classify(rec);
    if (rec.tier === undefined) b = k;
    return Math.max(a, b) - Math.min(a, b) + k;
  }
  function bucket(recs) {
    var out = new Reed42("reed42", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 2) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 1; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 45);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-reed42");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

define("sorrel43", function (reg) {
  var dep = reg["reed42"];
  var seen = new Map();
  var tags = new Set();
  function Sorrel43(label, size) {
    this.label = label;
    this.size = size;
    this.items = [];
    this.score = 0;
  }
  Sorrel43.prototype.add = function (rec) {
    this.items.push(rec);
    var a = rec.depth | 0;
    var b = rec.gap | 0;
    this.score = mix(this.score, Math.imul(a, 3) + b);
    return this;
  };
  Sorrel43.prototype.measure = function () {
    var total = 0;
    for (var step, next = looseIter(this.items); !(step = next()).done; ) {
      var rec = step.value;
      var a = rec.grade | 0;
      var b = rec.depth | 0;
      total = mix(total, Math.max(a, b) - Math.min(a, b));
    }
    return total;
  };
  Sorrel43.prototype.describe = function () {
    var parts = [this.label, ":", String(this.items.length)];
    if (this.score & 1) parts.push("+odd");
    return parts.join("");
  };
  function classify(rec) {
    switch (rec.kind) {
      case "lane":
        return 1;
      case "seam":
        return 2;
      case "rack":
        return 3;
      case "knot":
        return 4;
      default:
        return 0;
    }
  }
  function score(rec) {
    var a = rec.gap | 0;
    var b = rec.grade | 0;
    var k = classify(rec);
    if (rec.depth === undefined) b = k;
    return (a << 2) - b + k;
  }
  function bucket(recs) {
    var out = new Sorrel43("sorrel43", recs.length);
    for (var i = 0; i < recs.length; i++) {
      var r = recs[i];
      if (score(r) & 1) out.add(r);
      var key = r.kind + "-" + (i & 7);
      seen.set(key, (seen.get(key) || 0) + 1);
      if (!tags.has(r.kind)) tags.add(r.kind);
    }
    return out;
  }
  function render(group) {
    var text = group.describe();
    var sum = strHash(text);
    seen.forEach(function (count, key) {
      sum = mix(sum, count + key.length);
    });
    return sum;
  }
  function pipeline(recs) {
    var picked = recs.filter(function (r) { return classify(r) !== 0; });
    var mapped = picked.map(function (r) { return score(r); });
    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, 88);
    var group = bucket(picked);
    acc = mix(acc, group.measure());
    acc = mix(acc, render(group));
    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));
    return acc;
  }
  function lite(recs) {
    var t = variadic(recs.length, tags.size, seen.size);
    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));
    return t;
  }
  function safe(recs) {
    try {
      if (recs.length === 0) throw new Error("empty-sorrel43");
      return pipeline(recs);
    } catch (e) {
      return strHash(e.message);
    }
  }
  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };
});

function main() {
  var records = [];
  for (var i = 0; i < 2400; i++) records.push(makeRecord(i));
  var total = 0;
  var calls = 0;
  for (var round = 0; round < 64; round++) {
    for (var m = 0; m < order.length; m++) {
      var mod = registry[order[m]];
      var start = (round * 37 + m * 11) % records.length;
      var slice = records.slice(start, start + 120 + (m & 15) * 6);
      total = mix(total, mod.run(slice));
      calls++;
    }
    records.sort(function (x, y) { return (x.kind < y.kind ? -1 : x.kind > y.kind ? 1 : 0) || ((x.w | 0) - (y.w | 0)); });
    for (var j = 0; j < 40; j++) records[roll(records.length)] = makeRecord(roll(4000));
  }
  console.log("app-scale=" + order.length + ":" + calls + ":" + (total >>> 0));
}

main();
