// B233 oracle: JSON.parse must build an object identical to the ordinary
// owned-key build whether or not a cached key plan served it. Every case here
// is one the planned path could plausibly get wrong: repeats that make a plan
// hot, then a deviation of each kind.
var out = [];
function rec(label, v) { out.push(label + ":" + v); }

// A. the plain repeat — what the optimization exists for
var a = JSON.parse('[{"x":1,"y":2},{"x":3,"y":4},{"x":5,"y":6}]');
rec("A", a.map(function (o) { return Object.keys(o).join("") + o.x + o.y; }).join("|"));

// B. a duplicate key must keep the LAST value and appear once
var b = JSON.parse('[{"k":1,"k":2},{"k":1,"k":2}]');
rec("B", b.map(function (o) { return Object.keys(o).join(",") + "=" + o.k; }).join("|"));

// C. a prefix shape after a longer one, and the longer one again
var c = JSON.parse('[{"p":1,"q":2,"r":3},{"p":9},{"p":1,"q":2,"r":3}]');
rec("C", c.map(function (o) { return Object.keys(o).join("") + ":" + JSON.stringify(o); }).join("|"));

// D. canonical array-index keys must enumerate ascending, before string keys
var d = JSON.parse('[{"2":"a","b":1,"1":"c"},{"2":"a","b":1,"1":"c"}]');
rec("D", d.map(function (o) { return Object.keys(o).join(","); }).join("|"));

// E. an escaped name is the SAME property as its plain twin
var e = JSON.parse('[{"a\\u0062c":1},{"abc":2},{"ab\\/c":3},{"ab/c":4}]');
rec("E", e.map(function (o) { return Object.keys(o)[0] + "=" + o[Object.keys(o)[0]]; }).join("|"));

// F. __proto__ from JSON is an OWN data property, never the prototype
var f = JSON.parse('[{"__proto__":{"z":9},"n":1},{"__proto__":{"z":8},"n":2}]');
rec("F", f.map(function (o) {
  return Object.keys(o).join(",") + "/" + (o.z === undefined) + "/" +
    (Object.getPrototypeOf(o) === Object.prototype);
}).join("|"));
rec("F2", f.map(function (o) { return JSON.stringify(Object.getOwnPropertyDescriptor(o, "__proto__")); }).join("|"));

// G. descriptors must be writable/enumerable/configurable on every path
var g = JSON.parse('[{"w":1},{"w":2}]');
rec("G", g.map(function (o) {
  var dsc = Object.getOwnPropertyDescriptor(o, "w");
  return [dsc.writable, dsc.enumerable, dsc.configurable].join("");
}).join("|"));

// H. mutation after a planned build: add, delete, re-add, overwrite
var h = JSON.parse('[{"m":1,"n":2},{"m":3,"n":4}]');
h[1].extra = 7; delete h[1].m; h[1].m = 99; h[0].n = 0;
rec("H", JSON.stringify(h));

// I. more distinct shapes than the cache holds, then a return to the first
var src = [];
for (var s = 0; s < 12; s++) { src.push('{"f' + s + '":' + s + '}'); }
src.push('{"f0":100}');
var i2 = JSON.parse("[" + src.join(",") + "]");
rec("I", i2.map(function (o) { return Object.keys(o)[0] + o[Object.keys(o)[0]]; }).join(","));

// J. non-ASCII and empty names
var j = JSON.parse('[{"\\u00e9":1,"":2},{"\\u00e9":3,"":4}]');
rec("J", j.map(function (o) { return Object.keys(o).length + JSON.stringify(o); }).join("|"));

// K. nested objects of the same shape at different depths
var k = JSON.parse('{"a":{"a":{"a":{"v":1},"v":2},"v":3},"v":4}');
rec("K", JSON.stringify(k) + "/" + k.a.a.a.v + k.a.a.v + k.a.v + k.v);

// L. empty objects interleaved with a repeated shape
var l = JSON.parse('[{},{"t":1},{},{"t":2},{}]');
rec("L", l.map(function (o) { return Object.keys(o).length; }).join("") + JSON.stringify(l));

// M. a reviver still sees every key, planned or not
var m = JSON.parse('[{"r":1,"s":2},{"r":3,"s":4}]', function (key, val) {
  return typeof val === "number" ? val * 10 : val;
});
rec("M", JSON.stringify(m));

// N. shape-identity: two same-shape objects must share an inline-cache shape,
// and reading through a hot polymorphic site must stay correct
var n = JSON.parse('[{"u":1,"v":2},{"u":3,"v":4},{"u":5,"v":6}]');
var acc = 0;
for (var t = 0; t < 5000; t++) { acc = (acc + n[t % 3].u + n[t % 3].v) | 0; }
rec("N", acc);

// O. JSON.stringify round-trips the planned build in key order
var o = JSON.parse('[{"b":1,"a":2},{"b":3,"a":4}]');
rec("O", JSON.stringify(o));

// P. freeze/seal on a planned object
var p = JSON.parse('[{"q":1},{"q":2}]');
Object.freeze(p[0]);
try { p[0].q = 5; } catch (err) { }
rec("P", p[0].q + "/" + Object.isFrozen(p[0]) + "/" + Object.isFrozen(p[1]));

// Q. for-in and spread over planned objects
var q = JSON.parse('[{"c":1,"d":2},{"c":3,"d":4}]');
var seen = [];
for (var key in q[1]) { seen.push(key); }
rec("Q", seen.join("") + JSON.stringify(Object.assign({}, q[0])));

// R. names the plain scan must REFUSE: an embedded NUL (a control character,
// so the scan stops), a name ending in an escaped quote, an escaped backslash,
// and a tab escape. (A LONE SURROGATE also takes the refusal branch, but zipp
// folds it to U+FFFD in a property name with this wave on OR off -- object
// keys are Rust `String`s -- so it is a separate, pre-existing gap and not
// pinned here.)
var r = JSON.parse('[{"\\u0041":1,"a\\"":2,"b\\\\":3,"c\\t":4},{"\\u0041":5,"a\\"":6,"b\\\\":7,"c\\t":8}]');
rec("R", r.map(function (o) {
  return Object.keys(o).map(function (k) {
    return k.length + ":" + k.charCodeAt(k.length - 1);
  }).join(",") + "=" + Object.keys(o).map(function (k) { return o[k]; }).join("");
}).join("|"));

// S. a multi-byte name with an escape in the middle, beside its plain twin
var s2 = JSON.parse('[{"\u00e9\\u00e9\u00e9":1},{"\u00e9\u00e9\u00e9":2}]');
rec("S", s2.map(function (o) {
  return Object.keys(o)[0].length + "/" + o[Object.keys(o)[0]];
}).join("|") + "/" + (Object.keys(s2[0])[0] === Object.keys(s2[1])[0]));

console.log(out.join("\n"));
