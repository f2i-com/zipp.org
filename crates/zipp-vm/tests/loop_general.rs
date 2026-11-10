//! The loop runner's handlers that finish in the engine's general operations
//! (`vm/loop_tier.rs`) against Node: a builtin's method, an element or
//! property read or store past the fast cases, `+` on what is not two
//! numbers, an iterator's step. Such an operation may run program code under
//! the loop (a callback, a getter, a conversion), which may enter the runner
//! again, move the register file, raise, or change what the loop's other
//! instructions were told at its entry. String methods fused and in spec
//! order, one with a callback that loops; array methods with callbacks and a
//! comparator; Map, Set and WeakMap methods; number methods and natives;
//! element reads of a string, past an end, of a hole over a prototype's
//! element, by a string key, through a getter; element stores that append,
//! leave holes, hit a frozen array and a prototype's setter; `+` on strings,
//! on an object whose conversion runs a loop, on BigInts; getters and setters
//! as frames; `for…of` over a Map, a Set, a string, a typed array, a
//! generator, a user iterator and `entries()`; errors the operations raise,
//! caught in the loop, around it and in a caller; a prototype method
//! replaced and put back by a callback under the loop; recursion in a
//! callback deep
//! enough to grow the register file; and all of it again from frames the
//! runner pushed. In the interpreter with the runner entering as it does by
//! itself, from every loop's first back-edge (`ZIPP_LOOP_RUNNER_EAGER`) with
//! and without the lite loop and without frames of its own
//! (`ZIPP_NO_LOOP_FRAMES`), and switched off; by default; and with every
//! function compiled at once.

const SOURCE: &str = r##"// A hot loop's instructions that finish in the engine's general operations: builtin methods, element and property
// reads past the fast cases, `+` on what is not two numbers, iterators. Each may run program code under the loop
// (callbacks, getters, conversions), raise, or change what the loop's other instructions assumed.
var out = [];

// 1. String methods, fused and in spec order (an argument that is a global splits the call).
var gOff = 1;
function strings(n) {
  var s = "the quick brown fox jumps over the lazy dog", t = 0, acc = "";
  for (var i = 0; i < n; i++) {
    t += s.charCodeAt(i % s.length) + s.charCodeAt((i + gOff) % s.length);
    t += s.indexOf("o", i % 20) + s.slice(i % 7, 12).length + s.substring(gOff, 3 + (i & 3)).length;
    if ((i & 255) === 0) acc += s[i % s.length] + s.charAt(gOff) + s.toUpperCase().slice(0, 2);
    if (s.startsWith("the") && s.includes("fox", gOff)) t++;
  }
  return t + ":" + acc.length + ":" + acc.slice(0, 12);
}
out.push(strings(4000));

// 2. A string method with a callback: program code under the loop, with a loop of its own.
function replacer(n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    var r = "a1b22c333".replace(/\d+/g, function (m) { var k = 0; for (var j = 0; j < m.length + 40; j++) k += j; return String(k + i % 3); });
    t += r.length;
  }
  return t;
}
out.push(replacer(300));

// 3. Array methods with callbacks inside a hot loop; a comparator; nested loops in the callbacks.
function callbacks(n) {
  var a = [5, 3, 8, 1, 9, 2], t = 0;
  for (var i = 0; i < n; i++) {
    t += a.map(function (x) { var s = 0; for (var j = 0; j < 40; j++) s += x; return s; }).reduce(function (p, c) { return p + c; }, 0);
    if ((i & 63) === 0) { var b = a.slice().sort(function (x, y) { return x - y; }); t += b[0] * 100 + b[5]; }
    t += a.indexOf(i % 10) + (a.includes(9) ? 1 : 0) + a.join("").length + a.slice(1, 3).length + a.concat([i]).length;
  }
  return t;
}
out.push(callbacks(500));

// 4. Map and Set methods, fused and split; a growing Map; a WeakMap.
var gKey = 3;
function collections(n) {
  var m = new Map(), s = new Set(), w = new WeakMap(), keys = [{}, {}, {}], t = 0;
  for (var i = 0; i < n; i++) {
    m.set(i & 63, i); s.add(i % 17); w.set(keys[i % 3], i);
    t += (m.get(i & 31) | 0) + (m.has(gKey) ? 1 : 0) + (s.has(i % 19) ? 2 : 0) + m.size + s.size + (w.get(keys[(i + 1) % 3]) | 0) % 5;
    if ((i & 127) === 0) { m.delete(i & 63); s.delete(i % 17); }
    t += m.get(gKey) === undefined ? 0 : 1;
  }
  return t;
}
out.push(collections(3000));

// 5. Number methods; a number's toString in a loop; parseInt and String as natives.
function numbers(n) {
  var t = 0;
  for (var i = 0; i < n; i++) { t += (i * 1.5).toFixed(2).length + i.toString(16).length + (i % 7).toString().length + parseInt("12" + (i % 10), 10) + String(i).length; }
  return t;
}
out.push(numbers(2000));

// 6. Element reads past the fast cases: a string's characters, past the end, a hole over a prototype's element,
// an object by a string key, a typed array past its end, a negative index, a getter on an index.
function elements(n) {
  var s = "hello", a = [1, , 3], o = { a: 1, b: 2, c: 3 }, names = ["a", "b", "c", "zz"], f = new Float64Array(4), t = "";
  Object.defineProperty(a, 5, { get: function () { return 55; } });
  var u = 0;
  for (var i = 0; i < n; i++) {
    t = s[i % 7] === undefined ? t : (i & 511) === 0 ? t + s[i % 7] : t;
    u += (a[i % 4] === undefined ? 1 : a[i % 4]) + (o[names[i & 3]] || 7) + (f[i % 6] === undefined ? 2 : f[i % 6]) + (a[-1] === undefined ? 1 : 0) + a[5];
  }
  return t + ":" + u;
}
Array.prototype[1] = 20;
out.push(elements(3000));
delete Array.prototype[1];

// 7. Element stores past the fast cases: appends, holes, a string key, a frozen array, a setter on a prototype.
function stores(n) {
  var a = [], o = {}, fr = Object.freeze([1, 2]), hits = 0, t = 0;
  var p = Object.create({ set k(v) { hits += v; } });
  for (var i = 0; i < n; i++) {
    a[i % 50] = i; a[60 + (i % 3) * 2] = i; o["k" + (i % 5)] = i; fr[0] = i; p.k = 1; p["k"] = 2;
    t += a.length + fr[0];
  }
  return t + ":" + hits + ":" + Object.keys(o).length + ":" + a.length;
}
out.push(stores(2000));

// 8. `+` on strings, on a string and a number, on objects with conversions that run code, on BigInts.
function adds(n) {
  var s = "", t = 0, calls = 0;
  var conv = { valueOf: function () { calls++; var k = 0; for (var j = 0; j < 30; j++) k += j; return k; } };
  var big = 1n;
  for (var i = 0; i < n; i++) {
    if ((i & 127) === 0) { s = s + i + "," + (i + conv); big = big + 2n; }
    t += conv + 1 + ("" + i).length;
  }
  return s.length + ":" + t + ":" + calls + ":" + big;
}
out.push(adds(2000));

// 9. Getters and setters the property cache names, as frames; a getter that is a leaf; an accessor on a class.
class Temp {
  constructor(c) { this._c = c; this.reads = 0; }
  get f() { this.reads++; var x = this._c; for (var j = 0; j < 3; j++) x += 0; return x * 9 / 5 + 32; }
  get c() { return this._c; }
  set c(v) { this._c = v; }
}
function accessors(n) { var t = new Temp(10), s = 0; for (var i = 0; i < n; i++) { t.c = i % 40; s += t.f + t.c; } return s + ":" + t.reads; }
out.push(accessors(3000));

// 10. Iterators of every kind in `for…of`: a Map, a Set, a string, a typed array, a generator, a user iterator,
// `entries()`, with calls in the bodies.
function* countTo(n) { for (var i = 0; i < n; i++) yield i; }
function userIter(n) { var i = 0; return { [Symbol.iterator]: function () { return this; }, next: function () { return i < n ? { value: i++, done: false } : { value: undefined, done: true }; } }; }
function iterators(n) {
  var m = new Map([[1, 2], [3, 4], [5, 6]]), s = new Set([7, 8, 9]), ta = new Int16Array([1, 2, 3]), t = 0;
  for (var r = 0; r < n; r++) {
    for (var [k, v] of m) t += k * v;
    for (var x of s) t += x;
    for (var ch of "abc") t += ch.charCodeAt(0);
    for (var y of ta) t += y;
    for (var g of countTo(4)) t += g;
    for (var z of userIter(3)) t += z;
    for (var [ix, e] of [10, 20].entries()) t += ix + e;
  }
  return t;
}
out.push(iterators(400));

// 11. Errors the general operations raise, caught inside the loop, around it, and in a caller.
function raises(n) {
  var t = 0, caught = 0;
  for (var i = 0; i < n; i++) {
    try { if (i % 100 === 99) null.x; t += "abc".repeat(i % 3).length; if (i % 250 === 249) "x".repeat(-1); } catch (e) { caught += e instanceof TypeError ? 1 : e instanceof RangeError ? 10 : 100; }
  }
  try { for (var j = 0; j < n; j++) { t += [1, 2, 3].map(function (x) { if (j === 700) throw new Error("cb " + j); return x; }).length; } } catch (e) { return t + ":" + caught + ":" + e.message; }
  return t + ":" + caught;
}
out.push(raises(1000));
function unguarded(n) { var t = 0; for (var i = 0; i < n; i++) { t += new Array(i < 900 ? 2 : -1).length; } return t; }
try { unguarded(1000); out.push("no throw"); } catch (e) { out.push((e instanceof RangeError) + ""); }

// 12. Program code under the loop that changes what the loop's other instructions rely on: a prototype method
// replaced and put back (a global whose route changes under a loop has a test of its own, global_route_live).
function protoUnder(n) {
  var t = 0, saved = String.prototype.charCodeAt;
  for (var i = 0; i < n; i++) {
    t += "xyz".charCodeAt(i % 3) + "xyz".charCodeAt(gOff);
    if (i === 1000) [0].forEach(function () { String.prototype.charCodeAt = function () { return 1; }; });
    if (i === 2000) [0].forEach(function () { String.prototype.charCodeAt = saved; });
  }
  return t;
}
out.push(protoUnder(3000));

// 13. Deep recursion inside a callback under the loop (the register file grows under the runner), and a loop
// that goes on after it. At most 3,700 deep: the sandbox profile stops a recursion at 4,096 frames.
function depth(k) { return k === 0 ? 0 : 1 + depth(k - 1); }
function growsUnder(n) {
  var t = 0, a = [1, 2, 3, 4];
  for (var i = 0; i < n; i++) {
    var d = a.map(function (x) { return x + (i % 200 === 0 ? depth(3000 + (i >> 1)) : 0); });
    t = (t + d[0] + d[3] + a[i & 3]) | 0;
  }
  return t;
}
out.push(growsUnder(1500));

// 14. The same operations from frames the runner pushed: a callee that calls builtins, reads elements and adds
// strings, called in a hot loop and recursively.
function worker(s, i) { var t = s.indexOf("b") + s[i % s.length].charCodeAt(0); var m = new Map(); m.set(i, s + i); return t + m.get(i).length; }
function nested(k, i) { if (k === 0) return worker("abcabc", i); return nested(k - 1, i) + [k].map(function (x) { return x * 2; })[0]; }
function fromFrames(n) { var t = 0; for (var i = 0; i < n; i++) { t += worker("xbz", i) + nested(i & 3, i); } return t; }
out.push(fromFrames(2000));

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "873380:64:thTHohTH hTH\n3600\n569422\n4671577\n276877\nhello:198500\n131994:6000:5:65\n129:878890:2016:33\n259800:3000\n163200\n5070:30:cb 700\ntrue\n485999\n64850\n468437";

/// Every switch a mode below may set, cleared before each run.
const SWITCHES: [&str; 6] = [
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_LOOP_RUNNER_EAGER",
    "ZIPP_NO_LOOP_RUNNER",
    "ZIPP_NO_LOOP_FRAMES",
    "ZIPP_NO_LITE",
];

#[test]
fn loop_general_worker() {
    if std::env::var_os("ZIPP_LOOP_GENERAL_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn loop_general_match_node() {
    if std::env::var_os("ZIPP_LOOP_GENERAL_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let modes: [(&str, &[&str]); 7] = [
        ("default", &[]),
        ("interpreter", &["ZIPP_NOJIT"]),
        ("interpreter, runner from the first back-edge", &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER"]),
        (
            "interpreter, runner from the first back-edge, no lite loop",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LITE"],
        ),
        (
            "interpreter, runner from the first back-edge, no frame of its own",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LOOP_FRAMES"],
        ),
        ("interpreter, no runner", &["ZIPP_NOJIT", "ZIPP_NO_LOOP_RUNNER"]),
        ("compile-at-once", &["ZIPP_JIT_THRESHOLD"]),
    ];
    for (mode, vars) in modes {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["loop_general_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_GENERAL_WORKER", "1");
        for v in SWITCHES {
            command.env_remove(v);
        }
        for v in vars {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
