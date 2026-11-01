//! `new` of a constructor that only stores its arguments, answered in compiled
//! code without a call: the instance is what the body would have made, and every
//! change to what the lane baked (the callee, its `prototype`, a setter or a
//! read-only property on the chain, a class's chain) takes the site back to the
//! body. Node's output.

const EXPECTED: &str = r####"2001000 2005000
[ 'x', 'y' ] true true true
[ 'a', 'b', 'c' ] true true true
{"x":1,"y":2} {"a":1,"b":2,"c":3}
{"a":9999,"k":7,"n":null,"f":false} {"a":9999,"b":2,"c":3,"k":7,"n":null,"f":false} true
{"v":9999} {"v":9999,"w":9999} true
{"other":true}
TypeError
true
true fresh true
true
defined
[ 'v' ]
[] [ 0, 1, 2 ]
[] 1
[] 10003
[ 'zz' ]
[ 'v' ]
[] [ 'cl0', 'cl1' ]
1
1
local9999 local9999
v0,v4000,v8000,v12000,v16000,v20000,v24000,v28000
12000
[true,[9999]]
{"z":1,"v":9999}
{"a":9999,"b":1,"c":2,"d":3} 3
true [ 'x', 'y' ]
9 true"####;

#[test]
fn new_store_lanes_child() {
    if std::env::var_os("ZIPP_NEW_STORE_LANES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn new_store_lanes_match_node_in_every_mode() {
    if std::env::var_os("ZIPP_NEW_STORE_LANES_CHILD").is_some() {
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
        ("no_store", Some(("ZIPP_NO_CTOR_STORE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "new_store_lanes_child", "--nocapture"])
            .env("ZIPP_NEW_STORE_LANES_CHILD", "1")
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

const PROGRAM: &str = r####"// `new` of constructors that only store their arguments, hot enough to compile,
// then every change that must take a compiled site off its proof.
function Pt(x, y) { this.x = x; this.y = y; }
Pt.prototype.sum = function () { return this.x + this.y; };
class Vec { constructor(a, b, c) { this.a = a; this.b = b; this.c = c; } dot() { return this.a + this.b + this.c; } }

function hotFn(n) { var s = 0; for (var i = 0; i < n; i++) { var p = new Pt(i, 1); s += p.sum(); } return s; }
function hotCls(n) { var s = 0; for (var i = 0; i < n; i++) { var v = new Vec(i, 1, 2); s += v.dot(); } return s; }
console.log(hotFn(2000), hotCls(2000));

// What the instance is.
var p = new Pt(1, 2), v = new Vec(1, 2, 3);
console.log(Object.keys(p), Object.getPrototypeOf(p) === Pt.prototype, p instanceof Pt, p.constructor === Pt);
console.log(Object.keys(v), Object.getPrototypeOf(v) === Vec.prototype, v instanceof Vec, v.constructor === Vec);
console.log(JSON.stringify(p), JSON.stringify(v));

// Fewer and more arguments than parameters; constants; an unused parameter.
function Three(a, b, c) { this.a = a; this.b = b; this.c = c; this.k = 7; this.n = null; this.f = false; }
function few(n) { var o; for (var i = 0; i < n; i++) o = new Three(i); return o; }
function many(n) { var o; for (var i = 0; i < n; i++) o = new Three(i, 2, 3, 4, 5); return o; }
console.log(JSON.stringify(few(10000)), JSON.stringify(many(10000)), "b" in few(1));

// A site that sees a second callee, and a callee that stops being one.
function A(v) { this.v = v; }
function B(v) { this.v = v; this.w = v; }
function pick(C, n) { var o; for (var i = 0; i < n; i++) o = new C(i); return o; }
console.log(JSON.stringify(pick(A, 10000)), JSON.stringify(pick(B, 10000)), pick(A, 1) instanceof A);
function NotCtor() { return { other: true }; }
console.log(JSON.stringify(pick(NotCtor, 10)));
var arrow = () => 1;
try { pick(arrow, 1); } catch (e) { console.log(e.constructor.name); }

// F.prototype replaced after the site compiled.
function Sw(v) { this.v = v; }
function mkSw(n) { var o; for (var i = 0; i < n; i++) o = new Sw(i); return o; }
console.log(mkSw(10000) instanceof Sw);
var oldP = Sw.prototype;
var fresh = { tag: "fresh" };
Sw.prototype = fresh;
var o2 = mkSw(10);
console.log(Object.getPrototypeOf(o2) === fresh, o2.tag, o2 instanceof Sw);
Sw.prototype = 5; // not an object: %Object.prototype%
console.log(Object.getPrototypeOf(mkSw(10)) === Object.prototype);
Object.defineProperty(Sw, "prototype", { value: { tag: "defined" } });
console.log(mkSw(10).tag);

// A setter or a read-only property appearing on the chain.
function St(v) { this.v = v; }
function mkSt(n) { var o; for (var i = 0; i < n; i++) o = new St(i); return o; }
console.log(Object.keys(mkSt(10000)));
var log = [];
Object.defineProperty(St.prototype, "v", { set(x) { log.push(x); }, configurable: true });
var o3 = mkSt(3);
console.log(Object.keys(o3), log);
delete St.prototype.v;
Object.defineProperty(St.prototype, "v", { value: 1, writable: false, configurable: true });
var o4 = mkSt(1);
console.log(Object.keys(o4), o4.v);
delete St.prototype.v;
Object.defineProperty(Object.prototype, "zz", { set(x) { log.push("zz" + x); }, configurable: true });
function Zz(v) { this.zz = v; }
function mkZz(n) { var o; for (var i = 0; i < n; i++) o = new Zz(i); return o; }
console.log(Object.keys(mkZz(10000)), log.length);
delete Object.prototype.zz;
console.log(Object.keys(mkZz(2)));

// The same through a class: its chain, and a method added or a parent swapped.
class Cl { constructor(v) { this.v = v; } }
function mkCl(n) { var o; for (var i = 0; i < n; i++) o = new Cl(i); return o; }
console.log(Object.keys(mkCl(10000)));
Object.defineProperty(Cl.prototype, "v", { set(x) { log.push("cl" + x); }, configurable: true });
console.log(Object.keys(mkCl(2)), log.slice(-2));
delete Cl.prototype.v;
Object.setPrototypeOf(Cl.prototype, { get inherited() { return 1; } });
console.log(mkCl(1).inherited);

// Prototype chains replaced underneath, and an object that outlives its function.
function Ch(v) { this.v = v; }
function mkCh(n) { var o; for (var i = 0; i < n; i++) o = new Ch(i); return o; }
mkCh(10000);
Object.setPrototypeOf(Ch.prototype, { up: 1 });
console.log(mkCh(1).up);
function outlive() {
  function Local(v) { this.v = v; }
  Local.prototype.who = function () { return "local" + this.v; };
  var keep;
  for (var i = 0; i < 10000; i++) keep = new Local(i);
  return keep;
}
var kept = [];
for (var r = 0; r < 6; r++) kept.push(outlive());
var junk = [];
for (var i = 0; i < 30000; i++) junk = [i];
console.log(kept[3].who(), kept[5].who());

// Fresh instances in old holders, and instances with young string values.
var holder = [];
function St2(s, n) { this.s = s; this.n = n; }
for (var i = 0; i < 30000; i++) {
  var o = new St2("v" + i, i);
  if (i % 4000 === 0) holder.push(o);
}
console.log(holder.map((h) => h.s).join());

// The instance grows, is frozen, loses keys: an ordinary object afterwards.
function Gr(a, b) { this.a = a; this.b = b; }
function grow(n) {
  var r = 0;
  for (var i = 0; i < n; i++) {
    var g = new Gr(i, i);
    g.c = 1; g.d = 2; g.e = 3; g.f = 4;
    delete g.a;
    r += Object.keys(g).length;
    var h = Object.freeze(new Gr(1, 2));
    h.a = 9;
    r += h.a;
  }
  return r;
}
console.log(grow(2000));

// new.target and `this` in a body that is not store-only keep the body's path.
function Nt(v) { this.v = v; this.t = new.target === Nt; }
function Ret(v) { this.v = v; return [v]; }
function Lit(n) { var a, b; for (var i = 0; i < n; i++) { a = new Nt(i); b = new Ret(i); } return [a.t, b]; }
console.log(JSON.stringify(Lit(10000)));
class Fld { z = 1; constructor(v) { this.v = v; } }
function mkFld(n) { var o; for (var i = 0; i < n; i++) o = new Fld(i); return o; }
console.log(JSON.stringify(mkFld(10000)));
class Sub extends Vec { constructor(a) { super(a, 1, 2); this.d = 3; } }
function mkSub(n) { var o; for (var i = 0; i < n; i++) o = new Sub(i); return o; }
console.log(JSON.stringify(mkSub(10000)), mkSub(1).dot());
console.log(Reflect.construct(Pt, [1, 2], Vec) instanceof Vec, Object.keys(Reflect.construct(Pt, [1, 2], Vec)));

// A subclass of a function, and the callee as a bound function.
function Bo(v) { this.v = v; }
var Bound = Bo.bind(null, 9);
function mkBound(n) { var o; for (var i = 0; i < n; i++) o = new Bound(); return o; }
console.log(mkBound(10000).v, mkBound(1) instanceof Bo);
"####;
