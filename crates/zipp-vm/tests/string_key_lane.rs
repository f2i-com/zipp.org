//! Computed access with a string key, in loops hot enough to be compiled.
//!
//! `counts[w] = (counts[w] || 0) + 1` misses for every word it has not seen
//! and then appends it. A compiled region left for the interpreter on each
//! miss and each append; 32 of those evicted it and the function ran
//! interpreted from then on, so a word count took 4.4 times Node's time.
//! Compiled code now answers an own miss from the prototype chain and appends
//! a new key itself (`jit_get_index`, `jit_set_index`), where the chain is
//! plain objects and data properties; a key that is still a rope is flattened
//! first.
//!
//! What must not change is anything a program can see: a getter or setter on
//! the chain still runs, a read-only inherited property still blocks the
//! append, frozen, sealed and non-extensible objects still refuse it, class
//! instances, proxies, arrays, functions, the global object and the built-in
//! prototypes keep their own rules, `__proto__` and index-like and escaped
//! keys keep theirs, and a property that arrives on the chain part way
//! through a loop is seen at once. The expected lines are Node 24's output
//! for the same program; they are the same with the compiler off.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(2_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Computed access with string keys in loops hot enough to be compiled: every
// line must print what Node prints. N is past the tier-up count.
var N = 6000;
var out = [];
function show(name, v) { out.push(name + ": " + v); }

// 1. own misses on a plain object, and on its default chain. Every answer of
// every iteration is folded into the sum, so one wrong answer anywhere shows.
(function () {
  var o = { a: 1, b: 2 };
  function code(v) { return v === undefined ? 1 : typeof v === "function" ? 2 : typeof v === "number" ? 3 + v : v === Object.prototype ? 11 : 13; }
  var keys = ["a", "zz", "b", "toString", "constructor", "hasOwnProperty", "valueOf", "isPrototypeOf",
    "propertyIsEnumerable", "toLocaleString", "__defineGetter__", "__lookupSetter__", "nope", "also-not-there"];
  var h = 0;
  for (var i = 0; i < N; i++) h = (h * 31 + code(o[keys[i % keys.length]])) | 0;
  // The same with the keys the lane declines among them.
  var odd = ["a", "__proto__", "zz", "", "0", "-1", "#x", "@@iterator", "toString"];
  var h2 = 0;
  for (var i = 0; i < N; i++) h2 = (h2 * 31 + code(o[odd[i % odd.length]])) | 0;
  // The functions found are the intrinsic ones.
  var same = 0;
  var named = [o.toString, o.constructor, o.hasOwnProperty, o.valueOf];
  var names = ["toString", "constructor", "hasOwnProperty", "valueOf"];
  for (var i = 0; i < N; i++) if (o[names[i & 3]] === named[i & 3]) same++;
  show("miss/default-chain", "h=" + h + " h2=" + h2 + " same=" + same);
})();

// 2. a null prototype, a data prototype, a getter on the prototype (it must run every time)
(function () {
  var bare = Object.create(null); bare.x = 5;
  var calls = 0;
  var proto = { inherited: 7, get got() { calls++; return 9; } };
  var child = Object.create(proto); child.own = 1;
  var grand = Object.create(child);
  var keys = ["x", "toString", "inherited", "got", "own", "missing"];
  var s = 0, und = 0;
  for (var i = 0; i < N; i++) {
    var k = keys[i % keys.length];
    var a = bare[k], b = child[k], c = grand[k];
    if (a === undefined) und++; else s += a;
    if (b === undefined) und++; else s += typeof b === "number" ? b : 100;
    if (c === undefined) und++; else s += typeof c === "number" ? c : 100;
  }
  show("miss/chains", "sum=" + s + " undefined=" + und + " getterCalls=" + calls);
})();

// 3. a property that appears on the chain part way through
(function () {
  var proto = {};
  var o = Object.create(proto);
  var before = 0, after = 0;
  for (var i = 0; i < N; i++) {
    if (i === 3000) proto["late"] = 4;
    if (i === 4500) Object.prototype["veryLate"] = 6;
    var k = i % 2 ? "late" : "veryLate";
    var v = o[k];
    if (v === undefined) before++; else after += v;
  }
  delete Object.prototype.veryLate;
  show("miss/late-chain", "before=" + before + " after=" + after);
})();

// 4. appends: a dictionary built by computed stores, with a named read beside them
(function () {
  var words = [];
  for (var i = 0; i < 500; i++) words.push("w" + (i * 7919 % 211));
  var total = 0, keys = 0;
  for (var r = 0; r < 40; r++) {
    var counts = { fixed: r };
    for (var i = 0; i < words.length; i++) {
      var w = words[i];
      counts[w] = (counts[w] || 0) + 1;
      total += counts.fixed;
    }
    keys += Object.keys(counts).length;
    total += counts.w7 + counts["w210"];
  }
  show("add/wordcount", "total=" + total + " keys=" + keys);
})();

// 5. appends the chain has a say in: a setter, a read-only property, an ordinary one
(function () {
  var set = 0;
  var proto = { plain: 1 };
  Object.defineProperty(proto, "viaSetter", { set: function (v) { set += v; }, get: function () { return -1; }, configurable: true });
  Object.defineProperty(proto, "readOnly", { value: 42, writable: false });
  var ownSetter = 0, ownReadOnly = 0, ownPlain = 0, threw = 0;
  function strictStore(o, k, v) { "use strict"; o[k] = v; }
  for (var i = 0; i < N; i++) {
    var o = Object.create(proto);
    o["viaSetter"] = 1;
    o["readOnly"] = 2;
    o["plain"] = 3;
    if (Object.prototype.hasOwnProperty.call(o, "viaSetter")) ownSetter++;
    if (Object.prototype.hasOwnProperty.call(o, "readOnly")) ownReadOnly++;
    if (Object.prototype.hasOwnProperty.call(o, "plain")) ownPlain++;
    try { strictStore(o, i % 2 ? "readOnly" : "plain2", 1); } catch (e) { if (e instanceof TypeError) threw++; }
  }
  show("add/chain", "setterSum=" + set + " ownSetter=" + ownSetter + " ownReadOnly=" + ownReadOnly + " ownPlain=" + ownPlain + " strictThrew=" + threw);
})();

// 6. receivers that must refuse or route the append
(function () {
  var frozen = Object.freeze({ a: 1 });
  var sealed = Object.seal({ a: 1 });
  var closed = Object.preventExtensions({ a: 1 });
  function C() { this.a = 1; }
  class K { constructor() { this.a = 1; } set viaClass(v) { this.seen = (this.seen || 0) + v; } }
  var inst = new C(), kinst = new K();
  var arr = [1, 2, 3], fn = function () {}, map = new Map(), date = new Date(0);
  var threw = 0;
  function strictStore(o, k, v) { "use strict"; o[k] = v; }
  for (var i = 0; i < N; i++) {
    var k = "k" + (i % 5);
    frozen[k] = i; sealed[k] = i; closed[k] = i;
    inst[k] = i; kinst[k] = i; kinst["viaClass"] = 1;
    arr[k] = i; fn[k] = i; map[k] = i; date[k] = i;
    try { strictStore(i % 3 === 0 ? frozen : i % 3 === 1 ? sealed : closed, k, i); } catch (e) { if (e instanceof TypeError) threw++; }
  }
  show("add/receivers", [Object.keys(frozen), Object.keys(sealed), Object.keys(closed)].join("|") + " inst=" + Object.keys(inst) + " kinst=" + Object.keys(kinst) + " seen=" + kinst.seen +
    " arr=" + Object.keys(arr) + " len=" + arr.length + " fn=" + Object.keys(fn) + " map=" + Object.keys(map) + " size=" + map.size + " date=" + Object.keys(date) + " strictThrew=" + threw);
})();

// 7. keys only a computed access can spell, and their order when enumerated
(function () {
  var keys = ["", "b", "10", "2", "-1", "1.5", "01", "@@iterator", "#priv", "__proto__", "length", "constructor",
    "café", "日本", "a b", "prototype", "NaN", "Infinity", "-0", "0"];
  var o = {};
  var hits = 0;
  for (var i = 0; i < N; i++) {
    var k = keys[i % keys.length];
    if (o[k] !== undefined && k !== "constructor" && k !== "__proto__") hits++;
    o[k] = i;
  }
  show("keys/spellings", JSON.stringify(Object.keys(o)) + " hits=" + hits + " proto=" + (Object.getPrototypeOf(o) === Object.prototype) + " ctor=" + typeof o.constructor);
  var p = {};
  for (var i = 0; i < N; i++) p["__proto__"] = i % 2 ? null : { marker: 1 };
  show("keys/__proto__", Object.keys(p).length + " " + (Object.getPrototypeOf(p) === null ? "null" : Object.getPrototypeOf(p).marker));
})();

// 8. keys that are ropes, numbers, symbols and objects
(function () {
  var o = {};
  var sym = Symbol("s"), calls = 0;
  var objKey = { toString: function () { calls++; return "fromObject"; } };
  var s = 0;
  for (var i = 0; i < N; i++) {
    var rope = "left" + (i % 37) + "right" + (i % 3);
    o[rope] = (o[rope] || 0) + 1;
    o[i % 7] = i;
    o[sym] = i;
    o[objKey] = i;
    s += o["left" + (i % 37) + "right" + (i % 3)];
  }
  show("keys/kinds", "count=" + Object.keys(o).length + " s=" + s + " sym=" + o[sym] + " obj=" + o.fromObject + " toStringCalls=" + calls + " first=" + Object.keys(o).slice(0, 9));
})();

// 9. the object changes under the loop: an accessor arrives, a key goes, the object freezes
(function () {
  var o = { v: 0 };
  var gets = 0, sets = 0, s = 0;
  for (var i = 0; i < N; i++) {
    if (i === 2000) Object.defineProperty(o, "acc", { get: function () { gets++; return 5; }, set: function (x) { sets++; }, configurable: true, enumerable: true });
    if (i === 3000) delete o["k1"];
    if (i === 5000) Object.freeze(o);
    var k = i % 4 === 0 ? "acc" : "k" + (i % 3);
    o[k] = i;
    var r = o[k];
    s += r === undefined ? 1 : r;
  }
  show("mutate/under-loop", "s=" + s + " gets=" + gets + " sets=" + sets + " keys=" + Object.keys(o) + " k0=" + o.k0 + " k1=" + o.k1);
})();

// 10. a prototype that gains keys while a child's named reads are cached
(function () {
  var proto = { base: 1 };
  var child = Object.create(proto);
  var s = 0;
  for (var i = 0; i < N; i++) {
    proto["p" + (i % 50)] = i;
    s += child.base + (child.p7 || 0) + (child["p" + (i % 50)] || 0);
    if (i === 4000) proto["base"] = 2;
  }
  show("proto/grows", "s=" + s + " keys=" + Object.keys(proto).length);
})();

// 11. the global object and the built-in prototypes as receivers
(function () {
  var g = globalThis, s = 0;
  for (var i = 0; i < N; i++) {
    var k = "strkeyGlobal" + (i % 4);
    g[k] = (g[k] || 0) + 1;
    Object.prototype["strkeyProto" + (i % 2)] = i;
    Array.prototype["strkeyArr"] = i;
    s += g[k] + ({})["strkeyProto0"] + [].strkeyArr + (g["strkeyMissing"] === undefined ? 1 : 0);
  }
  var a = typeof strkeyGlobal3 + ":" + strkeyGlobal0;
  delete Object.prototype.strkeyProto0; delete Object.prototype.strkeyProto1; delete Array.prototype.strkeyArr;
  for (var j = 0; j < 4; j++) delete g["strkeyGlobal" + j];
  show("receivers/global-and-anchors", "s=" + s + " " + a);
})();

// 12. a big dictionary: appended, read, overwritten, deleted and appended again
(function () {
  var o = {}, s = 0;
  for (var i = 0; i < 20000; i++) {
    var k = "key" + (i % 3000);
    if (i % 7 === 3) delete o[k];
    else o[k] = (o[k] === undefined ? 0 : o[k]) + i;
    var probe = o["key" + ((i * 31) % 3500)];
    s = (s + (probe === undefined ? 1 : probe)) | 0;
  }
  var n = 0, last = "";
  for (var k in o) { n++; last = k; }
  show("dict/churn", "s=" + s + " n=" + n + " last=" + last);
})();

// 13. a proxy and a class instance in the chain
(function () {
  var traps = 0;
  var target = { t: 1 };
  var proxy = new Proxy(target, { get: function (t, k, r) { traps++; return k === "viaProxy" ? 8 : Reflect.get(t, k, r); }, set: function (t, k, v, r) { traps++; return Reflect.set(t, k, v, r); } });
  var o = Object.create(proxy);
  class Base { get fromClass() { return 3; } }
  var viaClass = Object.create(new Base());
  var s = 0;
  for (var i = 0; i < N; i++) {
    var k = i % 3 === 0 ? "viaProxy" : i % 3 === 1 ? "t" : "absent";
    var v = o[k];
    s += v === undefined ? 1 : v;
    o["store" + (i % 2)] = i;
    s += viaClass[i % 2 ? "fromClass" : "absent"] === undefined ? 1 : 3;
  }
  show("chain/proxy-class", "s=" + s + " traps=" + traps + " own=" + Object.keys(o) + " target=" + Object.keys(target));
})();

console.log(out.join("\n"));
"####;

#[test]
fn string_keyed_access_in_hot_loops_matches_node() {
    let expected: &[&str] = &[
        r####"miss/default-chain: h=-67526062 h2=1184525351 same=6000"####,
        r####"miss/chains: sum=239000 undefined=9000 getterCalls=2000"####,
        r####"miss/late-chain: before=3750 after=10500"####,
        r####"add/wordcount: total=390200 keys=8480"####,
        r####"add/chain: setterSum=6000 ownSetter=0 ownReadOnly=0 ownPlain=6000 strictThrew=3000"####,
        r####"add/receivers: a|a|a inst=a,k0,k1,k2,k3,k4 kinst=a,k0,seen,k1,k2,k3,k4 seen=6000 arr=0,1,2,k0,k1,k2,k3,k4 len=3 fn=k0,k1,k2,k3,k4 map=k0,k1,k2,k3,k4 size=0 date=k0,k1,k2,k3,k4 strictThrew=6000"####,
        r####"keys/spellings: ["0","2","10","","b","-1","1.5","01","@@iterator","#priv","length","constructor","café","日本","a b","prototype","NaN","Infinity","-0"] hits=5382 proto=true ctor=number"####,
        r####"keys/__proto__: 1 null"####,
        r####"keys/kinds: count=119 s=165165 sym=5999 obj=5999 toStringCalls=6000 first=0,1,2,3,4,5,6,left0right0,fromObject"####,
        r####"mutate/under-loop: s=13627500 gets=1000 sets=1000 keys=v,acc,k2,k0,k1 k0=4998 k1=4999"####,
        r####"proto/grows: s=35855300 keys=51"####,
        r####"receivers/global-and-anchors: s=40500000 number:1500"####,
        r####"dict/churn: s=260890555 n=2572 last=key1999"####,
        r####"chain/proxy-class: s=32000 traps=6002 own=store0,store1 target=t"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
