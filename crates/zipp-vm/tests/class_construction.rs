//! Classes constructed many times.
//!
//! Construction keeps each class's constructor and field-thunk function
//! values on the class (`Vm::class_body_fn`) instead of making them again for
//! every `new`, and a class field is defined through `Vm::define_field`.
//! This program constructs each shape hundreds of times first, so the
//! constructors and fields run on those paths, and then checks what a program
//! can see: fields defined (not set) over a parent's setter, over a
//! configurable and a non-configurable own property and on a frozen or
//! non-extensible return override; field order with computed, symbol and
//! index keys; private fields, methods and `#x in o` across two evaluations
//! of one class; three-level chains with `new.target`, return overrides,
//! `this` before `super()`, a missing and a repeated `super()`; arrow fields;
//! classes evaluated in a loop over a `let`; constructor functions whose
//! prototype is replaced; static fields and blocks.
//!
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Differential probe: class construction, fields and brands, constructed many times.
var out = [];
function log(label, v) { out.push(label + ": " + v); }
function heat(f) { for (var i = 0; i < 300; i++) f(i); }

// 1. Fields are defined, not set: a parent's setter of the same name does not run.
var setterRuns = 0;
class P1 { set x(v) { setterRuns++; } get x() { return "getter"; } }
class C1 extends P1 { x = 5; }
heat(function () { new C1(); });
var c1 = new C1();
log("define not set", setterRuns + " " + c1.x + " " + Object.getOwnPropertyDescriptor(c1, "x").enumerable + Object.getOwnPropertyDescriptor(c1, "x").writable + Object.getOwnPropertyDescriptor(c1, "x").configurable);

// 2. A field over an own property the base constructor made: configurable is redefined, non-configurable throws.
class B2 { constructor(locked) { if (locked) Object.defineProperty(this, "f", { value: 1, configurable: false, enumerable: false }); else Object.defineProperty(this, "f", { value: 1, configurable: true, enumerable: false, writable: false }); } }
class D2 extends B2 { f = 2; constructor(locked) { super(locked); } }
heat(function () { new D2(false); });
var d2 = new D2(false);
var dd = Object.getOwnPropertyDescriptor(d2, "f");
log("redefine configurable", d2.f + " " + dd.writable + dd.enumerable + dd.configurable);
try { new D2(true); log("redefine locked", "no throw"); } catch (e) { log("redefine locked", e.name); }

// 3. A base that returns a frozen or non-extensible object: the derived class's field define throws.
class B3 { constructor(kind) { if (kind === 1) return Object.freeze({ z: 1 }); if (kind === 2) return Object.preventExtensions({ w: 1 }); } }
class D3 extends B3 { g = 7; constructor(kind) { super(kind); } }
heat(function () { new D3(0); });
log("override plain", JSON.stringify(new D3(0)));
try { new D3(1); log("override frozen", "no throw"); } catch (e) { log("override frozen", e.name); }
try { new D3(2); log("override non-extensible", "no throw"); } catch (e) { log("override non-extensible", e.name); }

// 4. Field order, computed keys, symbols, numeric keys, and initializer side effects in order.
var trace = [];
var sym = Symbol("s");
var k = 0;
class C4 { a = trace.push("a"); [sym] = 3; ["c" + (++k)] = 4; 2 = "two"; b = this.a + 1; static s = trace.push("static"); }
heat(function () { new C4(); });
var c4 = new C4();
log("field order", Object.keys(c4).join(",") + " " + c4[sym] + " " + c4.b + " k=" + k + " trace=" + trace.length + " " + JSON.stringify(c4));

// 5. Private fields, methods and brands across many constructions and a re-evaluated class.
function makeCounter(start) {
  return class Counter { #n = start; #step() { this.#n++; } static has(o) { return #n in o; } tick() { this.#step(); return this.#n; } get n() { return this.#n; } };
}
var Ca = makeCounter(10), Cb = makeCounter(100);
heat(function () { new Ca().tick(); new Cb().tick(); });
var ca = new Ca(), cb = new Cb();
ca.tick(); ca.tick(); cb.tick();
var cross;
try { Ca.prototype.tick.call(cb); cross = "no throw"; } catch (e) { cross = e.name; }
log("private", ca.n + " " + cb.n + " " + Ca.has(ca) + Ca.has(cb) + Cb.has(cb) + " " + cross);

// 6. Derived chains: super args, new.target, return override, implicit constructors, three levels.
class A6 { constructor(x) { this.x = x; this.nt = new.target.name; } }
class B6 extends A6 { y = 1; constructor(x) { super(x * 2); this.z = this.x + this.y; } }
class C6 extends B6 { w = this.z * 10; }
class D6 extends C6 { constructor() { super(5); return { overridden: true, base: this.w }; } }
heat(function (i) { new C6(i); new D6(); });
var c6 = new C6(3);
log("chain", JSON.stringify(c6) + " " + c6.nt + " " + (c6 instanceof A6) + " " + JSON.stringify(new D6()));
class E6 extends A6 { constructor() { var r; try { this.q = 1; } catch (e) { r = e.name; } super(1); this.r = r; } }
heat(function () { new E6(); });
log("this before super", new E6().r);
class F6 extends A6 { constructor() { } }
try { new F6(); log("no super", "no throw"); } catch (e) { log("no super", e.name); }
class G6 extends A6 { constructor() { super(1); try { super(2); } catch (e) { this.again = e.name; } } }
heat(function () { new G6(); });
log("super twice", new G6().again);

// 7. Arrow fields capture `this`; methods on the prototype; a field shadowing a method.
class C7 { v = 3; get = () => this.v; m() { return "proto"; } }
class D7 extends C7 { m = () => "own"; }
heat(function () { new D7(); });
var d7 = new D7(), other = { v: 99, get: d7.get };
log("arrow fields", d7.get() + " " + other.get() + " " + d7.m() + " " + C7.prototype.m.call(d7) + " " + Object.keys(d7).join(","));

// 8. Classes evaluated in a loop, closing over the iteration's binding.
var made = [];
for (let i = 0; i < 4; i++) { made.push(class { k = i; constructor() { this.j = i * 10; } }); }
var sums = made.map(function (K) { var s = 0; for (var n = 0; n < 200; n++) { var o = new K(); s += o.k + o.j; } return s; });
log("loop classes", sums.join(",") + " " + (made[0] !== made[1]));

// 9. Constructor functions (not classes) built many times, with prototypes changed midway.
function F9(a) { this.a = a; this.b = a + 1; }
F9.prototype.sum = function () { return this.a + this.b; };
var t9 = 0;
for (var i = 0; i < 500; i++) t9 += new F9(i).sum();
F9.prototype = { sum: function () { return -1; } };
var f9 = new F9(1);
Object.defineProperty(Object.prototype, "c", { set: function (v) { this._c = v; }, configurable: true });
function G9() { this.c = 5; }
var g9 = new G9();
delete Object.prototype.c;
log("function ctors", t9 + " " + f9.sum() + " " + Object.keys(g9).join(",") + " " + g9._c);

// 10. Static fields and blocks, and `static ['prototype']` rejected.
class S10 { static count = 0; static { S10.count += 5; } static inc() { return ++S10.count; } }
S10.inc();
var bad;
try { eval("(class { static ['proto' + 'type'] = 1; })"); bad = "no throw"; } catch (e) { bad = e.name; }
log("statics", S10.count + " " + bad);

console.log(out.join("\n"));
"####;

#[test]
fn classes_constructed_many_times_match_node() {
    let expected: &[&str] = &[
        r####"define not set: 0 5 truetruetrue"####,
        r####"redefine configurable: 2 truetruetrue"####,
        r####"redefine locked: TypeError"####,
        r####"override plain: {"g":7}"####,
        r####"override frozen: TypeError"####,
        r####"override non-extensible: TypeError"####,
        r####"field order: 2,a,c1,b 3 303 k=1 trace=302 {"2":"two","a":302,"c1":4,"b":303}"####,
        r####"private: 12 101 truefalsetrue TypeError"####,
        r####"chain: {"x":6,"nt":"C6","y":1,"z":7,"w":70} C6 true {"overridden":true,"base":110}"####,
        r####"this before super: ReferenceError"####,
        r####"no super: ReferenceError"####,
        r####"super twice: ReferenceError"####,
        r####"arrow fields: 3 3 own proto v,get,m"####,
        r####"loop classes: 0,2200,4400,6600 true"####,
        r####"function ctors: 250000 -1 _c 5"####,
        r####"statics: 6 TypeError"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
