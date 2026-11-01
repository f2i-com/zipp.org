//! `new` of a constructor that computes what it stores: the body inlined at the
//! site with its stores staged, the instance made once at the end. Operands the
//! inline cannot take leave it with nothing done, so `valueOf` and getters run
//! once; reads of `this`, branches, calls and captures keep the body's path; a
//! proof that lapses takes the site back to it. Node's output.

const EXPECTED: &str = r####"599990000 599970006
{"a":5,"b":6,"c":10} {"s":7,"d":3,"m":5} true true true
q=3.5,r=1,p=14,n=-7 q=1,r=0,p=4611686014132420600,n=-2147483647 q=Infinity,r=NaN,p=0,n=-1 q=-0,r=0,p=-0,n=-0 q=-Infinity,r=NaN,p=-0,n=1
{"a":1,"b":2,"c":2} {"a":"s","b":"s1","c":null} {"b":null,"c":null} {"a":null,"b":1,"c":0} {"a":true,"b":2,"c":2}
{"a":{},"b":11,"c":20} 2
{"a":{},"b":11,"c":20} 10
TypeError
TypeError
{"v":1,"w":2}
{"v":3,"w":2} 3
{"v":"v!","w":"w!"}
{} {}
TypeError
TypeError
{"a":40000,"b":20000,"c":40000} [ 'a', 'b', 'c' ]
{"n":null,"t":true,"f":false,"i":7,"d":1.5,"a":20000,"n2":null} n,u,t,f,i,d,a,n2
{"x":39998,"y":39999} true
{"b":"inherited","c":20000}
{"small":2} {"big":9}
{"list":[20000],"o":{"a":20001}} 2
{"v":20000}
{"other":20001}
{"v":20000}
{"v":20002}
{"v":"1x"}
{"v":20006}
[ 'v', 'w' ]
[ 'v' ] [ 0, 1 ]
1 [ 'v', 'w' ]
[ 'v' ]
[] [ 'k1' ]
{"z":1,"y":2,"w":20000}
{"a":19999,"b":20000,"c":39998,"e":20002}
{"v":20000,"t":true}
1s0,5001s5000,10001s10000,15001s15000,20001s20000,25001s25000"####;

#[test]
fn new_leaf_lanes_child() {
    if std::env::var_os("ZIPP_NEW_LEAF_LANES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn new_leaf_lanes_match_node_in_every_mode() {
    if std::env::var_os("ZIPP_NEW_LEAF_LANES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("jit", None),
        ("nojit", Some(("ZIPP_NOJIT", "1"))),
        ("forced", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gcstress", Some(("ZIPP_GC_STRESS", "1"))),
        ("verify", Some(("ZIPP_NURSERY_VERIFY", "1"))),
        ("no_ready", Some(("ZIPP_NO_READY_OBJECTS", "1"))),
        ("no_leaf", Some(("ZIPP_NO_TIERC_LEAF", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "new_leaf_lanes_child", "--nocapture"])
            .env("ZIPP_NEW_LEAF_LANES_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// `new` of constructors that compute what they store, inlined at the site, with
// everything that must leave the inline or run exactly once.
var N = 20000;
function Ar(a) { this.a = a; this.b = a + 1; this.c = a * 2; }
class Cl { constructor(a, b) { this.s = a + b; this.d = a - b; this.m = Math.max(a, b); } get t() { return this.s + this.d; } }
function hot(n) { var s = 0; for (var i = 0; i < n; i++) { var o = new Ar(i); s = (s + o.b + o.c) | 0; } return s; }
function hotC(n) { var s = 0; for (var i = 0; i < n; i++) { var o = new Cl(i, 3); s = (s + o.t + o.m) | 0; } return s; }
console.log(hot(N), hotC(N));
var p = new Ar(5), q = new Cl(5, 2);
console.log(JSON.stringify(p), JSON.stringify(q), p instanceof Ar, q instanceof Cl, Object.getPrototypeOf(p) === Ar.prototype);

// Doubles, overflow, negative zero, NaN and infinities through the body.
function Nums(a, b) { this.q = a / b; this.r = a % b; this.p = a * b; this.n = -a; }
function mkNums(a, b, n) { var o; for (var i = 0; i < n; i++) o = new Nums(a, b); return o; }
function show(o) { return Object.keys(o).map((k) => k + "=" + (Object.is(o[k], -0) ? "-0" : String(o[k]))).join(); }
console.log(show(mkNums(7, 2, N)), show(mkNums(2147483647, 2147483647, 10)), show(mkNums(1, 0, 10)), show(mkNums(0, -5, 10)), show(mkNums(-1, 0, 10)));

// Arguments that are not numbers, after the site compiled: the body leaves the
// inline with nothing done, and the operators run as the language says, once.
function viaAdd(x, n) { var o; for (var i = 0; i < n; i++) o = new Ar(x); return o; }
console.log(JSON.stringify(viaAdd(1, N)), JSON.stringify(viaAdd("s", 3)), JSON.stringify(viaAdd(undefined, 3)), JSON.stringify(viaAdd(null, 3)), JSON.stringify(viaAdd(true, 3)));
var calls = 0;
var obj = { valueOf() { calls++; return 10; } };
console.log(JSON.stringify(viaAdd(obj, 1)), calls);
calls = 0;
console.log(JSON.stringify(viaAdd(obj, 5)), calls);
try { viaAdd(Symbol("x"), 1); } catch (e) { console.log(e.constructor.name); }
try { viaAdd(1n, 1); } catch (e) { console.log(e.constructor.name); }

// Reads in the body: an own one, a getter, a Proxy, a missing object.
function Rd(o) { this.v = o.v; this.w = o.w; }
function mkRd(o, n) { var r; for (var i = 0; i < n; i++) r = new Rd(o); return r; }
console.log(JSON.stringify(mkRd({ v: 1, w: 2 }, N)));
var g = 0;
console.log(JSON.stringify(mkRd({ get v() { return ++g; }, w: 2 }, 3)), g);
console.log(JSON.stringify(mkRd(new Proxy({}, { get(t, k) { return k + "!"; } }), 2)));
console.log(JSON.stringify(mkRd("str", 2)), JSON.stringify(mkRd([1], 2)));
try { mkRd(undefined, 1); } catch (e) { console.log(e.constructor.name); }
try { mkRd(null, 1); } catch (e) { console.log(e.constructor.name); }

// Reading back what the body stored, storing twice, constants of each kind.
function Back(a) { this.a = a; this.b = this.a + 1; this.a = this.b * 2; this.c = this.a; }
function mkBack(n) { var o; for (var i = 0; i < n; i++) o = new Back(i); return o; }
console.log(JSON.stringify(mkBack(N)), Object.keys(mkBack(1)));
function Kon(a) { this.n = null; this.u = undefined; this.t = true; this.f = false; this.i = 7; this.d = 1.5; this.a = a + 1; this.n2 = this.n; }
function mkKon(n) { var o; for (var i = 0; i < n; i++) o = new Kon(i); return o; }
console.log(JSON.stringify(mkKon(N)), Object.keys(mkKon(1)).join());
function Loc(a) { var x = a * 2, y = x + 1; this.x = x; this.y = y; var z; this.z = z; }
function mkLoc(n) { var o; for (var i = 0; i < n; i++) o = new Loc(i); return o; }
console.log(JSON.stringify(mkLoc(N)), "z" in mkLoc(1));

// A read of `this` before the store, a branch, a call, an allocation: the body's path.
function Before(a) { this.b = this.inh; this.c = a + 1; }
Before.prototype.inh = "inherited";
function mkBefore(n) { var o; for (var i = 0; i < n; i++) o = new Before(i); return o; }
console.log(JSON.stringify(mkBefore(N)));
function Br(a) { if (a > 5) { this.big = a; } else { this.small = a + 1; } }
function mkBr(a, n) { var o; for (var i = 0; i < n; i++) o = new Br(a); return o; }
console.log(JSON.stringify(mkBr(1, N)), JSON.stringify(mkBr(9, 10)));
function Alloc(a) { this.list = [a + 1]; this.o = { a: a + 2 }; this.f = function () { return a; }; }
function mkAlloc(n) { var o; for (var i = 0; i < n; i++) o = new Alloc(i); return o; }
console.log(JSON.stringify(mkAlloc(N)), mkAlloc(3).f());
function Ca(a) { this.v = helper(a); }
function helper(x) { return x + 1; }
function mkCa(n) { var o; for (var i = 0; i < n; i++) o = new Ca(i); return o; }
console.log(JSON.stringify(mkCa(N)));
function Ret(a) { this.v = a + 1; return { other: a + 2 }; }
function mkRet(n) { var o; for (var i = 0; i < n; i++) o = new Ret(i); return o; }
console.log(JSON.stringify(mkRet(N)));
function RetPrim(a) { this.v = a + 1; return 5; }
function mkRetPrim(n) { var o; for (var i = 0; i < n; i++) o = new RetPrim(i); return o; }
console.log(JSON.stringify(mkRetPrim(N)));
function Glo(a) { this.v = a + gv; }
var gv = 3;
function mkGlo(n) { var o; for (var i = 0; i < n; i++) o = new Glo(i); return o; }
console.log(JSON.stringify(mkGlo(N)));
gv = "x";
console.log(JSON.stringify(mkGlo(2)));
var cap = 2;
var Up = (function () { var k = 5; return function Up(a) { this.v = a + k + cap; }; })();
function mkUp(n) { var o; for (var i = 0; i < n; i++) o = new Up(i); return o; }
console.log(JSON.stringify(mkUp(N)));

// Proofs that lapse after the site compiled.
function St(v) { this.v = v + 1; this.w = v; }
function mkSt(n) { var o; for (var i = 0; i < n; i++) o = new St(i); return o; }
console.log(Object.keys(mkSt(N)));
var log = [];
Object.defineProperty(St.prototype, "w", { set(x) { log.push(x); }, configurable: true });
console.log(Object.keys(mkSt(2)), log);
delete St.prototype.w;
St.prototype = { tag: 1 };
console.log(mkSt(2).tag, Object.keys(mkSt(1)));
class K2 { constructor(a) { this.v = a + 1; } }
function mkK2(n) { var o; for (var i = 0; i < n; i++) o = new K2(i); return o; }
console.log(Object.keys(mkK2(N)));
Object.defineProperty(K2.prototype, "v", { set(x) { log.push("k" + x); }, configurable: true });
console.log(Object.keys(mkK2(1)), log.slice(-1));

// Class fields with a computing constructor; a subclass; new.target.
class Fld { z = 1; y = 2; constructor(a) { this.w = a + 1; } }
function mkFld(n) { var o; for (var i = 0; i < n; i++) o = new Fld(i); return o; }
console.log(JSON.stringify(mkFld(N)));
class Sub extends Ar { constructor(a) { super(a); this.e = a + 3; } }
function mkSub(n) { var o; for (var i = 0; i < n; i++) o = new Sub(i); return o; }
console.log(JSON.stringify(mkSub(N)));
function Nt(a) { this.v = a + 1; this.t = new.target === Nt; }
function mkNt(n) { var o; for (var i = 0; i < n; i++) o = new Nt(i); return o; }
console.log(JSON.stringify(mkNt(N)));

// Instances through collections, kept in old holders.
var keep = [];
function Pair(a, b) { this.k = a + 1; this.v = b; }
for (var i = 0; i < 30000; i++) { var pr = new Pair(i, "s" + i); if (i % 5000 === 0) keep.push(pr); }
console.log(keep.map((x) => x.k + x.v).join());
"####;
