//! A property read, a write and a for-of step that no cache answers finish in the`n//! helper: primitives, exotic receivers, proxies, frozen slots, every iterator,
//! and each one's throw. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"function,function,function,function,function,function,function,function,function,function,function,function,function
TypeError Cannot read properties of null (reading 
TypeError Cannot read properties of undefined (rea
20893 19100
3998000
number
undefined
526320
45780
6000
3600
3000
7500,,,1,
1500
3002 2
undefined undefined undefined
TypeError
TypeError
TypeError
TypeError
TypeError
3000 1
2001000 6000 sb,ga,gb
2000
2000
4501500
9000000
4504500
2000
6:😀 3:3 2:3,4 3:3 4:6 3:2 2:3 3:3 3:3 3:2,3 2:b
6000 12000 8000 16000 12000 6000 12000
33000
2000 2000
2000 2000
2000
8000
408000
3001
TypeError4
14670"####;

const PROGRAM: &str = r####"// What compiled code finishes itself: reads, writes and for-of steps no cache answers.
const HOT = 3000;
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
// Reads off every kind of primitive, in a hot loop, and what each throws.
function rd(o) { return o.toString; }
hot(() => rd({}));
var kinds = [1, 1.5, true, "s", 10n, Symbol.iterator, () => 1, [], new Map(), new Date(0), /x/, new Uint8Array(2), new DataView(new ArrayBuffer(2))];
console.log(kinds.map((k) => typeof rd(k)).join());
try { rd(null); } catch (e) { console.log(e.name, e.message.slice(0, 40)); }
try { rd(undefined); } catch (e) { console.log(e.name, e.message.slice(0, 40)); }
function fx(n) { var s = 0; for (var i = 0; i < 2000; i++) { s += (n + i).toFixed(2).length + n.toPrecision(3).length; } return s; }
console.log(fx(1.5), fx(100));
Number.prototype.twice = function () { return this * 2; };
function tw(n) { var r = 0; for (var i = 0; i < 2000; i++) r += (i).twice(); return r; }
console.log(tw(1));
Object.defineProperty(Number.prototype, "self", { get() { "use strict"; return typeof this; }, configurable: true });
function gs(n) { var r; for (var i = 0; i < 2000; i++) r = n.self; return r; }
console.log(gs(1));
delete Number.prototype.self;
console.log(gs(1));
// Methods read off typed arrays, data views, regexps, a match array.
function dvw() { var b = new ArrayBuffer(64), d = new DataView(b), u = new Uint8Array(b), s = 0; for (var i = 0; i < 4000; i++) { var f = d.setUint32; f.call(d, i % 60, i, true); s += u.subarray(0, 4).length + u.slice(0, 2).length + d.getUint8(i % 60); } return s; }
console.log(dvw());
function mg() { var re = /(?<y>\d+)-(?<m>\d+)/, s = 0; for (var i = 0; i < 3000; i++) { var m = re.exec("a" + i + "-" + (i % 7)); s += m.index + m.groups.y.length + m.groups.m.length + m.input.length + m.length; } return s; }
console.log(mg());
function li() { var re = /a/g, s = 0; for (var i = 0; i < 3000; i++) { re.lastIndex = i % 3; re.test("aaaa"); s += re.lastIndex; } return s; }
console.log(li());
function liv() { var re = /a/y, s = 0; for (var i = 0; i < 3000; i++) { re.lastIndex = { valueOf() { return i % 5; } }; try { re.test("aaa"); } catch (e) {} s += re.lastIndex; } return s; }
console.log(liv());
var fz = Object.freeze(/a/g);
function fzs() { "use strict"; var n = 0; for (var i = 0; i < 3000; i++) { try { fz.lastIndex = 1; } catch (e) { n++; } } return n; }
console.log(fzs());
// `.length =` on arrays, in a loop, with holes, and on the array's own write-protect.
function al() { var a = [1, 2, 3, 4, 5], s = 0; for (var i = 0; i < 3000; i++) { a.length = i % 6; s += a.length; a[3] = 1; } return s + a.join(); }
console.log(al());
function alb() { var a = [1, 2], n = 0; for (var i = 0; i < 3000; i++) { try { a.length = i % 2 ? -1 : 3; } catch (e) { n++; } } return n; }
console.log(alb());
var fa = Object.freeze([1, 2]);
function als() { "use strict"; var n = 0; for (var i = 0; i < 3000; i++) { try { fa.length = 0; } catch (e) { n++; } } return n + fa.length; }
function alq() { for (var i = 0; i < 3000; i++) fa.length = 0; return fa.length; }
console.log(als(), alq());
// Writes: primitives, strict and sloppy; non-writable; inherited setter; proxies.
function wp(o, v) { o.x = v; return o.x; }
hot(() => wp({ x: 1 }, 2));
console.log(wp(1, 2), wp("s", 2), wp(true, 2));
function wps(o, v) { "use strict"; o.x = v; return o.x; }
hot(() => wps({ x: 1 }, 2));
for (var p of [1, "s", true, null, undefined]) { try { console.log(wps(p, 2)); } catch (e) { console.log(e.name); } }
var ro = Object.freeze({ x: 1 });
function wr() { var n = 0; for (var i = 0; i < 3000; i++) { try { wps(ro, i); } catch (e) { n++; } } return n; }
console.log(wr(), ro.x);
var log = [];
var pr = new Proxy({ a: 1 }, { get(t, k, r) { log.push("g" + String(k)); return k === "boom" ? (() => { throw new RangeError("trap"); })() : t[k]; }, set(t, k, v, r) { log.push("s" + String(k)); t[k] = v; return k !== "no"; } });
function prx() { var s = 0; for (var i = 0; i < 2000; i++) { pr.b = i; s += pr.a + pr.b; } return s; }
console.log(prx(), log.length, log.slice(0, 3).join());
function prt() { var n = 0; for (var i = 0; i < 2000; i++) { try { pr.boom; } catch (e) { n += e instanceof RangeError ? 1 : 0; } } return n; }
console.log(prt());
function prf() { "use strict"; var n = 0; for (var i = 0; i < 2000; i++) { try { pr.no = 1; } catch (e) { n++; } } return n; }
console.log(prf());
var setlog = 0;
var base = { set v(x) { setlog += x; }, get v() { return setlog; } };
var kid = Object.create(base);
function inh() { var s = 0; for (var i = 0; i < 3000; i++) { kid.v = 1; s += kid.v; } return s; }
console.log(inh());
function cls() { class A { static n = 0; static get m() { return A.n + 1; } static set m(v) { A.n = v; } } var s = 0; for (var i = 0; i < 3000; i++) { A.m = i; s += A.m + A.n; } return s; }
console.log(cls());
function fnp() { function f() {} f.calls = 0; var s = 0; for (var i = 0; i < 3000; i++) { f.calls = f.calls + 1; s += f.length + f.name.length + f.calls; } return s; }
console.log(fnp());
// A getter that bails or throws in the loop, seen from a try.
var tg = { get g() { throw new TypeError("tg"); } };
function tgl() { var n = 0; for (var i = 0; i < 2000; i++) { try { tg.g; } catch (e) { n++; } } return n; }
console.log(tgl());
// for-of over everything, and what a throwing or returning iterator does.
function fo(x) { var s = ""; var n = 0; for (var v of x) { n++; s = v; } return n + ":" + String(s); }
hot(() => fo([1, 2]));
var mp = new Map([[1, 2], [3, 4]]), st = new Set([1, 2, 3]);
function* gn(n) { for (var i = 0; i < n; i++) yield i * 2; }
var ui = { [Symbol.iterator]() { var i = 0; return { next() { return { done: i >= 3, value: i++ }; } }; } };
console.log(fo("héllo😀"), fo(st), fo(mp), fo(new Uint8Array([1, 2, 3])), fo(gn(4)), fo(ui), fo(mp.keys()), fo(st.values()), fo([1, , 3]), fo([1, 2, 3].entries()), fo("a,b".matchAll(/\w/g)));
function foh(x, n) { var s = 0; for (var i = 0; i < n; i++) { for (var v of x()) s += typeof v === "number" ? v : v.length; } return s; }
console.log(foh(() => "abc", 2000), foh(() => st, 2000), foh(() => mp, 2000), foh(() => new Uint8Array(4).fill(2), 2000), foh(() => gn(3), 2000), foh(() => ui, 2000), foh(() => [1, 2, 3].map((x) => x | 0), 2000));
Array.prototype[1] = 7;
function hol() { var s = 0, a = [1, , 3]; for (var i = 0; i < 3000; i++) { for (var v of a) s += v; } return s; }
console.log(hol());
delete Array.prototype[1];
var closes = 0;
var ci = { [Symbol.iterator]() { var i = 0; return { next() { i++; return { done: false, value: i }; }, return() { closes++; return {}; } }; } };
function br() { var s = 0; for (var i = 0; i < 2000; i++) { for (var v of ci) { if (v === 2) break; s += v; } } return s; }
console.log(br(), closes);
var te = { [Symbol.iterator]() { return { next() { throw new EvalError("next"); }, return() { closes = -1; return {}; } }; } };
function tn() { var n = 0; for (var i = 0; i < 2000; i++) { try { for (var v of te) n--; } catch (e) { n += e.name === "EvalError" ? 1 : 0; } } return n; }
console.log(tn(), closes);
var bad = { [Symbol.iterator]() { return { next() { return 1; } }; } };
function bd() { var n = 0; for (var i = 0; i < 2000; i++) { try { for (var v of bad) n--; } catch (e) { n += e.name === "TypeError" ? 1 : 0; } } return n; }
console.log(bd());
var replaced = gn(0).__proto__.__proto__;
var rn = replaced.next;
function gr() { var s = 0; for (var i = 0; i < 2000; i++) { for (var v of gn(2)) s += v + 1; } return s; }
console.log(gr());
replaced.next = function () { var r = rn.call(this); if (!r.done) r.value += 100; return r; };
console.log(gr());
replaced.next = rn;
// A map changed during its own iteration; a detached view; a loop's iterator kept.
function mc() { var m = new Map([[1, 1]]), n = 0; for (var [k] of m) { if (n++ < 3000) { m.delete(k); m.set(k + 1, 1); } } return n; }
console.log(mc());
var ab = new ArrayBuffer(8), v8 = new Uint8Array(ab);
function dt() { var n = 0; try { for (var v of v8) { if (++n === 4) ab.transfer(); } } catch (e) { return e.name + n; } return n; }
console.log(dt());
// A compiled caller, through deep calls, to a primitive read.
function deep(n, x) { return n === 0 ? x.toFixed(1) : deep(n - 1, x); }
function dp() { var s = 0; for (var i = 0; i < 3000; i++) s += deep(i % 50, i / 3).length; return s; }
console.log(dp());
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
