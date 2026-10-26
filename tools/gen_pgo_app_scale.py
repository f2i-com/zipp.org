"""gen_pgo_app_scale.py OUT.js : write bench/pgo-training/app-scale-mix.js, the
app-scale PGO training input (deterministic: rerunning reproduces the file).

A bundle-shaped program: a few dozen module closures, each exporting a small
"class" (constructor + prototype methods) and a handful of helpers, wired
through a registry, run over generated records of several layouts. Enough
function literals (well past 256) that the engine runs it the way it runs a
large application: interpreter only, bodies compiled at first call. ASCII,
LF, no template literals, no regular expressions, no division.
"""
import random
import sys

rng = random.Random(20261026)
out = []
w = out.append

N_MODULES = 44
WORDS = ["amber", "basil", "cedar", "dune", "ember", "fjord", "grove", "heron", "iris", "juniper",
         "kelp", "lumen", "moss", "nectar", "onyx", "pollen", "quartz", "reed", "sorrel", "thyme",
         "umber", "vetch", "willow", "yarrow", "zinnia"]
KINDS = ["tile", "rack", "lane", "bolt", "seam", "knot", "flag", "pane"]
FIELDS = ["w", "h", "depth", "rank", "tier", "span", "gap", "pad", "lot", "mark", "grade", "phase"]

w('"use strict";')
w("")
w("// PGO-only workload: an application-scale program in the shape of a")
w("// bundled library. Module closures export small constructor classes and")
w("// helpers through a registry; records of several layouts flow through")
w("// them. Its function count puts it in the engine's large-program mode")
w("// (interpreter only, bodies compiled at first call), which the other")
w("// training inputs never reach. Generated; independent of every scored")
w("// benchmark.")
w("")
w("var registry = {};")
w("var order = [];")
w("function define(name, factory) {")
w("  var mod = factory(registry);")
w("  registry[name] = mod;")
w("  order.push(name);")
w("  return mod;")
w("}")
w("")
w("var state = 7;")
w("function roll(n) {")
w("  state = (Math.imul(state, 1103) + 977) & 0x7fffffff;")
w("  return (state >>> 7) % n;")
w("}")
w("")
w("function mix(acc, v) {")
w("  return (Math.imul(acc ^ (v | 0), 0x2f1d) + 0x3b) | 0;")
w("}")
w("")
w("function strHash(s) {")
w("  var h = 0x55;")
w("  for (var i = 0; i < s.length; i++) h = (Math.imul(h, 131) + s.charCodeAt(i)) | 0;")
w("  return h;")
w("}")
w("")
w("function looseIter(arr) {")
w("  var it = typeof Symbol !== \"object\" && arr[Symbol.iterator];")
w("  if (it) return (it = it.call(arr)).next.bind(it);")
w("  var i = 0;")
w("  return function () {")
w("    return i < arr.length ? { done: false, value: arr[i++] } : { done: true };")
w("  };")
w("}")
w("")
w("function variadic() {")
w("  var total = 0;")
w("  for (var i = 0; i < arguments.length; i++) total = mix(total, arguments[i]);")
w("  return total;")
w("}")
w("")
w("function forward(fn, self) {")
w("  return function () {")
w("    return fn.apply(self, arguments);")
w("  };")
w("}")
w("")

# Record layouts: each a key list; records are built as literals of these.
layouts = []
for k in range(10):
    n = 3 + rng.randrange(8)
    keys = ["kind"] + rng.sample(FIELDS, n)
    layouts.append(keys)

w("var LAYOUTS = [")
for keys in layouts:
    w("  [" + ", ".join('"%s"' % k for k in keys) + "],")
w("];")
w("")
w("function makeRecord(i) {")
w("  var layout = LAYOUTS[i % LAYOUTS.length];")
w("  switch (i % LAYOUTS.length) {")
for li, keys in enumerate(layouts):
    parts = []
    for k in keys:
        if k == "kind":
            parts.append('kind: "%s"' % KINDS[li % len(KINDS)])
        else:
            parts.append("%s: roll(%d)" % (k, 50 + rng.randrange(400)))
    w("    case %d:" % li)
    w("      return { " + ", ".join(parts) + " };")
w("  }")
w("  return { kind: layout[0] };")
w("}")
w("")

OPS = [
    "a + b", "a - b", "(a ^ b) & 0xffff", "Math.imul(a, 3) + b", "(a << 2) - b", "a > b ? a - b : b - a",
    "(a | b) & 0x3ff", "Math.max(a, b) - Math.min(a, b)", "a * 2 + (b & 7)", "(a + b) >>> 1",
]

for m in range(N_MODULES):
    name = WORDS[m % len(WORDS)] + str(m)
    cls = name[0].upper() + name[1:]
    fa, fb, fc = rng.sample(FIELDS, 3)
    op1, op2, op3 = rng.sample(OPS, 3)
    dep = WORDS[(m - 1) % len(WORDS)] + str(m - 1) if m > 0 else None
    w('define("%s", function (reg) {' % name)
    if dep:
        w('  var dep = reg["%s"];' % dep)
    else:
        w("  var dep = null;")
    w("  var seen = new Map();")
    w("  var tags = new Set();")
    w("  function %s(label, size) {" % cls)
    w("    this.label = label;")
    w("    this.size = size;")
    w("    this.items = [];")
    w("    this.score = 0;")
    w("  }")
    w("  %s.prototype.add = function (rec) {" % cls)
    w("    this.items.push(rec);")
    w("    var a = rec.%s | 0;" % fa)
    w("    var b = rec.%s | 0;" % fb)
    w("    this.score = mix(this.score, %s);" % op1)
    w("    return this;")
    w("  };")
    w("  %s.prototype.measure = function () {" % cls)
    w("    var total = 0;")
    w("    for (var step, next = looseIter(this.items); !(step = next()).done; ) {")
    w("      var rec = step.value;")
    w("      var a = rec.%s | 0;" % fc)
    w("      var b = rec.%s | 0;" % fa)
    w("      total = mix(total, %s);" % op2)
    w("    }")
    w("    return total;")
    w("  };")
    w("  %s.prototype.describe = function () {" % cls)
    w("    var parts = [this.label, \":\", String(this.items.length)];")
    w("    if (this.score & 1) parts.push(\"+odd\");")
    w("    return parts.join(\"\");")
    w("  };")
    w("  function classify(rec) {")
    w("    switch (rec.kind) {")
    kinds = rng.sample(KINDS, 4)
    for ki, kd in enumerate(kinds):
        w('      case "%s":' % kd)
        w("        return %d;" % (ki + 1))
    w("      default:")
    w("        return 0;")
    w("    }")
    w("  }")
    w("  function score(rec) {")
    w("    var a = rec.%s | 0;" % fb)
    w("    var b = rec.%s | 0;" % fc)
    w("    var k = classify(rec);")
    w("    if (rec.%s === undefined) b = k;" % fa)
    w("    return %s + k;" % op3)
    w("  }")
    w("  function bucket(recs) {")
    w("    var out = new %s(\"%s\", recs.length);" % (cls, name))
    w("    for (var i = 0; i < recs.length; i++) {")
    w("      var r = recs[i];")
    w("      if (score(r) & %d) out.add(r);" % (1 << rng.randrange(3)))
    w("      var key = r.kind + \"-\" + (i & 7);")
    w("      seen.set(key, (seen.get(key) || 0) + 1);")
    w("      if (!tags.has(r.kind)) tags.add(r.kind);")
    w("    }")
    w("    return out;")
    w("  }")
    w("  function render(group) {")
    w("    var text = group.describe();")
    w("    var sum = strHash(text);")
    w("    seen.forEach(function (count, key) {")
    w("      sum = mix(sum, count + key.length);")
    w("    });")
    w("    return sum;")
    w("  }")
    w("  function pipeline(recs) {")
    w("    var picked = recs.filter(function (r) { return classify(r) !== %d; });" % rng.randrange(5))
    w("    var mapped = picked.map(function (r) { return score(r); });")
    w("    var acc = mapped.reduce(function (s, v) { return mix(s, v); }, %d);" % rng.randrange(100))
    w("    var group = bucket(picked);")
    w("    acc = mix(acc, group.measure());")
    w("    acc = mix(acc, render(group));")
    w("    if (dep) acc = mix(acc, dep.lite(picked.slice(0, 8)));")
    w("    return acc;")
    w("  }")
    w("  function lite(recs) {")
    w("    var t = variadic(recs.length, tags.size, seen.size);")
    w("    for (var i = 0; i < recs.length; i++) t = mix(t, score(recs[i]));")
    w("    return t;")
    w("  }")
    w("  function safe(recs) {")
    w("    try {")
    w("      if (recs.length === 0) throw new Error(\"empty-%s\");" % name)
    w("      return pipeline(recs);")
    w("    } catch (e) {")
    w("      return strHash(e.message);")
    w("    }")
    w("  }")
    w("  return { pipeline: safe, lite: lite, run: forward(safe, null), classify: classify };")
    w("});")
    w("")

w("function main() {")
w("  var records = [];")
w("  for (var i = 0; i < 2400; i++) records.push(makeRecord(i));")
w("  var total = 0;")
w("  var calls = 0;")
w("  for (var round = 0; round < 64; round++) {")
w("    for (var m = 0; m < order.length; m++) {")
w("      var mod = registry[order[m]];")
w("      var start = (round * 37 + m * 11) % records.length;")
w("      var slice = records.slice(start, start + 120 + (m & 15) * 6);")
w("      total = mix(total, mod.run(slice));")
w("      calls++;")
w("    }")
w("    records.sort(function (x, y) { return (x.kind < y.kind ? -1 : x.kind > y.kind ? 1 : 0) || ((x.w | 0) - (y.w | 0)); });")
w("    for (var j = 0; j < 40; j++) records[roll(records.length)] = makeRecord(roll(4000));")
w("  }")
w("  console.log(\"app-scale=\" + order.length + \":\" + calls + \":\" + (total >>> 0));")
w("}")
w("")
w("main();")

src = "\n".join(out) + "\n"
assert all(ord(c) < 128 for c in src)
assert "`" not in src and "/" not in src.replace("//", "")
open(sys.argv[1], "w", encoding="ascii", newline="\n").write(src)
print("lines", len(out), "bytes", len(src))
