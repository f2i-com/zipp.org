//! Property-model audit, 15 September 2026 (track E-props).
//!
//! Class instances used to resolve declared methods and accessors from the
//! class's member tables (`ClassData.methods`/`getters`/`setters`) without ever
//! consulting the live `C.prototype`, so `C.prototype.m = f`, redefinition,
//! deletion, `Object.assign` mixins, `setPrototypeOf` and base-class patching
//! had no effect on instances (spies and mocks silently did nothing), and a
//! getter-only accessor was silently shadowed by an assignment. The tables are
//! now a cache that stays authoritative only while it still describes the
//! prototype objects; the first divergence retires it and advances
//! `class_proto_epoch`, which the interpreter's `Class*` inline caches and the
//! JIT's class method/accessor inline arms guard on. Accessors defined on a
//! class CONSTRUCTOR with `defineProperty` are now real accessors (reads call
//! the getter, writes the setter), which is what `Symbol.species` getters need.
//!
//! Array integrity levels: a hole is not an own property, so a sealed or
//! non-extensible array rejects filling it; `defineProperty` on a frozen or
//! sealed array's element validates against the non-writable/non-configurable
//! descriptor the flags imply; a sealed array's `length` cannot delete its
//! elements; pop/shift/splice on a merely non-extensible array succeed, and
//! push onto one fails; and a compiled dense store no longer reuses a snapshot
//! taken before the array was frozen.
//!
//! Smaller items from the same audit: StrWhiteSpace (U+FEFF yes, U+0085 no)
//! in StringToBigInt/StringToNumber/trim; long `parseInt`/`Number("0x…")`/
//! non-decimal literal digit strings rounded once; `JSON.rawJSON` rejecting
//! object/array text; `Object.defineProperty`'s object check before the key's
//! coercion; `Array.prototype.length` ignoring the non-index key
//! "4294967295"; a function with a null `[[Prototype]]` inheriting nothing;
//! `@@matchAll` reading `constructor` before `flags`; `groupBy` stepping,
//! calling and coercing per element (closing the iterator on an abrupt
//! completion); `Object.defineProperties` validating every descriptor, reading
//! each once, before defining any; `Reflect.set`/`super.x = v` consulting an
//! exotic receiver's real own descriptor (Array `length`, function
//! `name`/`prototype`, RegExp `lastIndex`, String indices); RegExp flag and
//! source getters answering from the slots only for the RegExp itself; English
//! ordinal/cardinal plural rules over the formatted operands; mapped
//! `arguments` aliasing past formal 63; a built-in constructor's `toString`
//! keeping its initial name; `defineProperty` of a function's `prototype`
//! value; and Error instances inheriting `name` instead of owning it.
//!
//! Symbol-keyed properties are stored under reserved "@@…" string keys. A
//! guest STRING key spelled that way used to share that space: it vanished
//! from `Object.keys`/`for-in`/JSON (a JSON round trip lost it), and
//! `{"@@toStringTag": …}` / `o["@@sym:1"]` / `o["@@for:k"]` spoofed or forged
//! symbol-keyed properties. Such a key is now stored one '@' longer and reads
//! back unchanged. And `Function.prototype.bind` builds "bound " + name as a
//! rope over the target's name, so a long bind chain is linear.
//!
//! Every probe's expected output is node v24's. Each runs in a clean child
//! process under the default, interpreter and forced-JIT modes (the hot-loop
//! probe is the one that exercises compiled inline arms).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    out.output
}

/// `(name, source, node's output lines)`.
const CASES: &[(&str, &str, &[&str])] = &[
    (
        "class_prototype_is_live",
        r#"
// Class members resolve through the live prototype objects: assignment,
// defineProperty, delete, Object.assign, Reflect, setPrototypeOf and
// base-class patching are all visible to instances.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name); } }
t('assign', () => { class R { m() { return 1; } } const r = new R(); R.prototype.m = function () { return 2; }; return [r.m(), R.prototype.m === r.m]; });
t('bracket', () => { class P { m() { return 1; } } const o = new P(); const k = 'm'; P.prototype[k] = function () { return 2; }; return [o.m(), o[k]()]; });
t('patch-before-new', () => { class P { m() { return 1; } } P.prototype.m = () => 9; return new P().m(); });
t('define-value', () => { class X { m() { return 1; } } Object.defineProperty(X.prototype, 'm', { value: () => 'dp' }); return new X().m(); });
t('define-getter', () => { class R { get g() { return 'g1'; } } const r = new R(); Object.defineProperty(R.prototype, 'g', { get() { return 'g2'; } }); return r.g; });
t('defineGetter', () => { class P { get g() { return 1; } } const o = new P(); P.prototype.__defineGetter__('g', function () { return 2; }); return o.g; });
t('defineSetter', () => { class P { set s(v) { this.a = v; } } const o = new P(); P.prototype.__defineSetter__('s', function (v) { this.b = v; }); o.s = 3; return [o.a === undefined, o.b]; });
t('delete', () => { class R { m() { return 1; } } const r = new R(); delete R.prototype.m; return [typeof r.m, 'm' in r, Reflect.has(r, 'm')]; });
t('reflect-set', () => { class P { m() { return 1; } } Reflect.set(P.prototype, 'm', () => 2); return new P().m(); });
t('reflect-define', () => { class P { m() { return 1; } } Reflect.defineProperty(P.prototype, 'm', { value: () => 6 }); return new P().m(); });
t('reflect-delete', () => { class P { m() { return 1; } } Reflect.deleteProperty(P.prototype, 'm'); return typeof new P().m; });
t('assign-mixin', () => { class P { m() { return 1; } } Object.assign(P.prototype, { m() { return 2; } }); return new P().m(); });
t('spy', () => { class Svc { fetch() { return 'real'; } } const orig = Svc.prototype.fetch; let calls = 0; Svc.prototype.fetch = function () { calls++; return 'mock:' + orig.call(this); }; return [new Svc().fetch(), calls]; });
t('base-patch', () => { class Base { m() { return 'base'; } } class D extends Base { } Base.prototype.m = function () { return 'patched'; }; return new D().m(); });
t('derived-shadow-add', () => { class B { m() { return 'b'; } } class D extends B {} const d = new D(); D.prototype.m = function () { return 'd'; }; return d.m(); });
t('inherited-getter-define', () => { class B { get v() { return 1; } } class D extends B {} const d = new D(); Object.defineProperty(B.prototype, 'v', { get() { return 2; } }); return d.v; });
t('super-after-patch', () => { class B { m() { return 'b'; } } class D extends B { m() { return 'd' + super.m(); } } const d = new D(); B.prototype.m = function () { return 'B2'; }; return d.m(); });
t('setproto-instance', () => { class Q { m() { return 1; } } const q = new Q(); Object.setPrototypeOf(q, { m() { return 'plain'; } }); return [q.m(), new Q().m()]; });
t('proto-setter', () => { class A { m() { return 'A'; } } const a = new A(); a.__proto__ = { m() { return 'X'; } }; return a.m(); });
t('setproto-classproto', () => { class A { m() { return 'A'; } } class B { m() { return 'B'; } } Object.setPrototypeOf(A.prototype, B.prototype); delete A.prototype.m; return new A().m(); });
t('instanceof', () => { class A {} class B {} const a = new A(); Object.setPrototypeOf(a, B.prototype); return [a instanceof A, a instanceof B]; });
t('toString', () => { class H { toString() { return 'a'; } } const h = new H(); H.prototype.toString = function () { return 'b'; }; return ['' + h, String(h)]; });
t('toStringTag', () => { class T { get [Symbol.toStringTag]() { return 'T1'; } } const x = new T(); Object.defineProperty(T.prototype, Symbol.toStringTag, { get() { return 'T2'; } }); return Object.prototype.toString.call(x); });
t('iterator', () => { class I { *[Symbol.iterator]() { yield 1; } } const i = new I(); I.prototype[Symbol.iterator] = function* () { yield 2; yield 3; }; return [...i]; });
t('in-added', () => { class K { m() {} } const k = new K(); K.prototype.z = 1; Object.defineProperty(K.prototype, 'q', { get() { return 'q'; }, configurable: true }); return ['z' in k, 'q' in k, k.q, Reflect.has(k, 'z')]; });
t('for-in', () => { class B { } B.prototype.bz = 2; class D extends B { } const d = new D(); D.prototype.dz = 3; d.own = 1; const r = []; for (const x in d) r.push(x); return r; });
t('extends-fn', () => { function F() {} F.prototype.q = 1; class D extends F { m() { return 1; } } const d = new D(); return ['q' in d, d.q, 'constructor' in d]; });
t('getter-only-sloppy', () => { class T { get c() { return 0; } } const x = new T(); x.c = 30; return [x.c, Object.keys(x)]; });
t('getter-only-strict', () => { 'use strict'; class T { get c() { return 0; } } const x = new T(); try { x.c = 30; return 'no throw'; } catch (e) { return [e.name, x.c]; } });
t('getter-only-reflect', () => { class T { get c() { return 0; } } const x = new T(); return [Reflect.set(x, 'c', 6), x.c]; });
t('nonwritable-proto', () => { 'use strict'; class F { m() { } } Object.defineProperty(F.prototype, 'm', { writable: false }); const f = new F(); try { f.m = 5; return 'wrote'; } catch (e) { return e.name; } });
t('frozen-proto', () => { class Fz { m() { } } Object.freeze(Fz.prototype); const f = new Fz(); f.m = 5; return [typeof f.m, Object.hasOwn(f, 'm')]; });
t('setter-deleted', () => { class S { set x(v) { this._x = v; } } const s = new S(); delete S.prototype.x; s.x = 5; return [s._x === undefined, s.x, Object.hasOwn(s, 'x')]; });
t('setter-reproto', () => { class S { set x(v) { this._x = v; } } const s = new S(); Object.setPrototypeOf(s, {}); s.x = 5; return [s._x === undefined, s.x]; });
t('private-unaffected', () => { class P { #x = 1; #m() { return this.#x; } get #g() { return 2; } run() { return this.#m() + this.#g; } } const p = new P(); Object.setPrototypeOf(P.prototype, {}); P.prototype.run2 = 0; return P.prototype.run.call(p); });
t('own-shadow', () => { class W { m() { return 1; } } const w = new W(); w.m = () => 'own'; return w.m(); });
console.log(out.join('\n'));"#,
        &[
            "assign: [2,true]",
            "bracket: [2,2]",
            "patch-before-new: 9",
            "define-value: \"dp\"",
            "define-getter: \"g2\"",
            "defineGetter: 2",
            "defineSetter: [true,3]",
            "delete: [\"undefined\",false,false]",
            "reflect-set: 2",
            "reflect-define: 6",
            "reflect-delete: \"undefined\"",
            "assign-mixin: 2",
            "spy: [\"mock:real\",1]",
            "base-patch: \"patched\"",
            "derived-shadow-add: \"d\"",
            "inherited-getter-define: 2",
            "super-after-patch: \"dB2\"",
            "setproto-instance: [\"plain\",1]",
            "proto-setter: \"X\"",
            "setproto-classproto: \"B\"",
            "instanceof: [false,true]",
            "toString: [\"b\",\"b\"]",
            "toStringTag: \"[object T2]\"",
            "iterator: [2,3]",
            "in-added: [true,true,\"q\",true]",
            "for-in: [\"own\",\"dz\",\"bz\"]",
            "extends-fn: [true,1,true]",
            "getter-only-sloppy: [0,[]]",
            "getter-only-strict: [\"TypeError\",0]",
            "getter-only-reflect: [false,0]",
            "nonwritable-proto: \"TypeError\"",
            "frozen-proto: [\"function\",false]",
            "setter-deleted: [true,5,true]",
            "setter-reproto: [true,5]",
            "private-unaffected: 3",
            "own-shadow: \"own\"",
        ],
    ),
    (
        "class_member_arms_follow_the_prototype",
        r#"
// Hot class-member sites compiled with inline arms, then the prototype changes
// underneath them: every arm must notice (method, getter, setter, inherited
// method, re-prototyped instance, deleted method).
var out = [];
class A { constructor() { this.v = 1; } m() { return this.v; } get g() { return this.v; } set st(x) { this.v = x; } }
class D extends A { }
function runM(o) { var s = 0; for (var i = 0; i < 30000; i++) s += o.m(); return s; }
function runG(o) { var s = 0; for (var i = 0; i < 30000; i++) s += o.g; return s; }
function runS(o) { var s = 0; for (var i = 0; i < 30000; i++) { o.st = 1; s += o.v; } return s; }
var a = new A(), d = new D();
out.push(runM(a), runM(d), runG(a), runS(a));
out.push(runM(a), runM(d), runG(a), runS(a));
A.prototype.m = function () { return this.v * 3; };
Object.defineProperty(A.prototype, 'g', { get: function () { return this.v * 3; }, configurable: true });
Object.defineProperty(A.prototype, 'st', { set: function (x) { this.v = x * 3; }, configurable: true });
out.push(runM(a), runM(d), runG(a), runS(a));
class E { constructor() { this.v = 1; } m() { return this.v; } }
var e = new E();
out.push(runM(e), runM(e));
Object.setPrototypeOf(e, { m() { return 9; } });
out.push(runM(e));
class F { constructor() { this.v = 2; } m() { return this.v; } }
var f = new F();
out.push(runM(f), runM(f));
delete F.prototype.m;
try { runM(f); out.push('no throw'); } catch (x) { out.push(x.name); }
console.log(out.join('|'));"#,
        &[
            "30000|30000|30000|30000|30000|30000|30000|30000|90000|90000|90000|90000|30000|30000|270000|60000|60000|TypeError",
        ],
    ),
    (
        "class_static_accessors",
        r#"
// Accessors installed on a class constructor with defineProperty are real
// accessors: reads call the getter, writes call the setter (or reject), and
// Symbol.species accessors drive the species protocol.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name); } }
t('get-set', () => { class K {} Object.defineProperty(K, 'a', { get() { return 'A'; }, set(v) { this._a = v; }, configurable: true }); const r = [K.a]; K.a = 1; r.push(K._a, K.a); return r; });
t('inherited', () => { class K {} Object.defineProperty(K, 'a', { get() { return this.name; } }); class Sub extends K {} return [Sub.a, Reflect.get(K, 'a'), K['a'], Object.create(K).a]; });
t('strict-getter-only', () => { 'use strict'; class K {} Object.defineProperty(K, 'g', { get() { return 1; } }); try { K.g = 2; return 'no throw'; } catch (e) { return e.name; } });
t('reflect-set-getter-only', () => { class K {} Object.defineProperty(K, 'g', { get() { return 1; } }); return [Reflect.set(K, 'g', 2), K.g]; });
t('static-block', () => { class K { static { Object.defineProperty(this, 'x', { get() { return 42; } }); } } return K.x; });
t('redefine-syntax-getter', () => { class K { static get s() { return 1; } } Object.defineProperty(K, 's', { get() { return 2; }, configurable: true }); return K.s; });
t('defineProperties', () => { const K = class {}; Object.defineProperties(K, { p: { get() { return 'p'; } } }); return K.p; });
t('species-array', () => { class A extends Array {} Object.defineProperty(A, Symbol.species, { get() { return Array; } }); const a = new A(); a.push(1, 2, 3); const m = a.map(x => x); return [m instanceof A, m.constructor === Array, a.filter(Boolean).length]; });
t('species-promise', () => { class P extends Promise {} Object.defineProperty(P, Symbol.species, { get() { return Promise; } }); const q = P.resolve(1).then(() => 2); q.finally(() => {}); return [q instanceof P, q instanceof Promise]; });
console.log(out.join('\n'));"#,
        &[
            "get-set: [\"A\",1,\"A\"]",
            "inherited: [\"Sub\",\"K\",\"K\",\"K\"]",
            "strict-getter-only: \"TypeError\"",
            "reflect-set-getter-only: [false,1]",
            "static-block: 42",
            "redefine-syntax-getter: 2",
            "defineProperties: \"p\"",
            "species-array: [false,true,3]",
            "species-promise: [false,true]",
        ],
    ),
    (
        "array_integrity_levels",
        r#"
// Array integrity levels: hole fills, element redefinition, length shrink and
// the mutators on frozen, sealed and non-extensible arrays.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name + ' ' + e.message); } }
// JIT stores through a frozen array (hot loop, frozen mid-way / before).
t('jit-frozen-store', () => { const a = [1, 2, 3, 4]; function w(a) { for (let i = 0; i < 20000; i++) a[i & 3] = i; } w(a); Object.freeze(a); w(a); return a; });
t('jit-frozen-store2', () => { const a = new Array(8).fill(0); function w(a, v) { for (let i = 0; i < 50000; i++) a[i & 7] = v; } w(a, 1); Object.freeze(a); w(a, 2); return [a.join(''), Object.isFrozen(a)]; });
t('jit-frozen-strict', () => { 'use strict'; const a = [1, 2, 3, 4]; function w(a) { for (let i = 0; i < 20000; i++) a[i & 3] = i; } w(a); Object.freeze(a); try { w(a); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('jit-sealed-store', () => { const a = [1, 2, 3, 4]; function w(a) { for (let i = 0; i < 20000; i++) a[i & 3] = i; } w(a); Object.seal(a); w(a); return a; });
t('jit-sealed-push', () => { const a = [1, 2]; function w(a) { for (let i = 0; i < 2000; i++) { try { a.push(i); } catch (e) { return e.name; } } return a.length; } Object.seal(a); return [w(a), a.length]; });
t('jit-nonext-hole', () => { const a = [1, , 3]; Object.preventExtensions(a); function w(a) { for (let i = 0; i < 20000; i++) a[1] = i; } w(a); return [a[1], 1 in a, a.length]; });
t('sealed-hole-fill', () => { const a = [1, , 3]; Object.seal(a); a[1] = 2; return [a[1], 1 in a]; });
t('sealed-hole-fill-strict', () => { 'use strict'; const a = [1, , 3]; Object.seal(a); try { a[1] = 2; return 'no throw'; } catch (e) { return [e.name, 1 in a]; } });
t('nonext-hole-fill', () => { const a = [1, , 3]; Object.preventExtensions(a); a[1] = 2; return [a[1], 1 in a]; });
t('frozen-hole-fill', () => { const a = [1, , 3]; Object.freeze(a); a[1] = 2; return [a[1], 1 in a]; });
t('define-frozen-elem', () => { const a = Object.freeze([1, 2, 3]); try { Object.defineProperty(a, 0, { value: 9 }); return ['no throw', a[0]]; } catch (e) { return [e.name, a[0]]; } });
t('define-frozen-elem-same', () => { const a = Object.freeze([1, 2, 3]); Object.defineProperty(a, 0, { value: 1 }); return a[0]; });
t('reflect-define-frozen', () => { const a = Object.freeze([1, 2, 3]); return [Reflect.defineProperty(a, 1, { value: 9 }), a[1], Reflect.defineProperty(a, 1, { writable: true }), Reflect.defineProperty(a, 1, { enumerable: false })]; });
t('define-sealed-elem', () => { const a = Object.seal([1, 2, 3]); Object.defineProperty(a, 0, { value: 9 }); const r = [a[0]]; try { Object.defineProperty(a, 1, { enumerable: false }); r.push('no throw'); } catch (e) { r.push(e.name); } try { Object.defineProperty(a, 5, { value: 1 }); r.push('no throw'); } catch (e) { r.push(e.name); } return r; });
t('sealed-length0', () => { const a = Object.seal([1, 2, 3]); a.length = 0; return [a.length, a]; });
t('sealed-length0-strict', () => { 'use strict'; const a = Object.seal([1, 2, 3]); try { a.length = 0; return 'no throw'; } catch (e) { return [e.name, a.length]; } });
t('sealed-define-length', () => { const a = Object.seal([1, 2, 3]); return [Reflect.defineProperty(a, 'length', { value: 1 }), a.length]; });
t('sealed-reflect-set-length', () => { const a = Object.seal([1, 2, 3]); return [Reflect.set(a, 'length', 1), a.length]; });
t('sealed-sparse-length', () => { const a = [1, , , 4, , ]; Object.seal(a); a.length = 2; return [a.length, 3 in a]; });
t('sealed-grow-length', () => { const a = Object.seal([1, 2]); a.length = 5; return a.length; });
t('pe-then-seal', () => { const a = [1, 2, 3]; Object.seal(a); a.length = 0; return [a.length, Object.isSealed(a)]; });
t('frozen-pop', () => { const a = Object.freeze([1, 2]); try { a.pop(); return 'no throw'; } catch (e) { return [e.name, a.length]; } });
t('sealed-pop', () => { const a = Object.seal([1, 2]); try { a.pop(); return 'no throw'; } catch (e) { return [e.name, a.length]; } });
t('frozen-sort', () => { const a = Object.freeze([3, 1, 2]); try { a.sort(); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('frozen-fill', () => { const a = Object.freeze([3, 1, 2]); try { a.fill(0); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('frozen-reverse', () => { const a = Object.freeze([3, 1, 2]); try { a.reverse(); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('frozen-copyWithin', () => { const a = Object.freeze([3, 1, 2]); try { a.copyWithin(0, 1); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('frozen-splice', () => { const a = Object.freeze([3, 1, 2]); try { a.splice(0, 1); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('frozen-unshift', () => { const a = Object.freeze([3, 1, 2]); try { a.unshift(0); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('frozen-shift', () => { const a = Object.freeze([3, 1, 2]); try { a.shift(); return 'no throw'; } catch (e) { return [e.name, a]; } });
t('sealed-splice-grow', () => { const a = Object.seal([3, 1, 2]); try { a.splice(0, 0, 9); return 'no throw'; } catch (e) { return [e.name, a]; } });
console.log(out.join('\n'));
const out2 = [];
function t2(name, f) { try { out2.push(name + ': ' + JSON.stringify(f())); } catch (e) { out2.push(name + ': THROW ' + e.name); } }
t2('pop-nonext', () => { const q = [1, 2, 3]; Object.preventExtensions(q); q.pop(); return q.length; });
t2('pop-sealed-trailing-hole', () => { const q = [1, 2, ,]; Object.seal(q); q.pop(); return q.length; });
t2('shift-nonext', () => { const q = [1, 2, 3]; Object.preventExtensions(q); q.shift(); return [q.length, q]; });
t2('fill-nonext-holey', () => { const q = [1, , 3]; Object.preventExtensions(q); q.fill(7); return q; });
t2('reflect-define-hole-nonext', () => { const q = Object.preventExtensions([1, , 3]); return [Reflect.defineProperty(q, 1, { value: 2 }), 1 in q]; });
t2('frozen-args-define', () => { function f(a, b) { Object.freeze(arguments); return [Reflect.defineProperty(arguments, 0, { value: 9 }), arguments[0]]; } return f(1, 2); });
t2('frozen-fn-name', () => { function f() {} Object.freeze(f); return [Reflect.defineProperty(f, 'name', { value: 'x' }), f.name, Reflect.defineProperty(f, 'length', { configurable: true })]; });
t2('sealed-fn-name', () => { function f() {} Object.seal(f); return [Reflect.defineProperty(f, 'name', { value: 'x' }), f.name]; });
t2('sealed-elem-accessor', () => { const s = Object.seal([1, 2]); return [Reflect.defineProperty(s, 0, { get() { return 7; } }), s[0]]; });
t2('frozen-writable-true', () => { const a = Object.freeze([1]); return [Reflect.defineProperty(a, 0, { writable: true }), Object.getOwnPropertyDescriptor(a, 0).writable]; });
t2('sealed-keep-desc', () => { const a = Object.seal([1, 2]); Object.defineProperty(a, 0, { value: 5 }); const d = Object.getOwnPropertyDescriptor(a, 0); return [a[0], d.writable, d.enumerable, d.configurable, Object.isSealed(a), Object.isFrozen(a)]; });
t2('sealed-make-readonly', () => { const a = Object.seal([1, 2]); Object.defineProperty(a, 0, { writable: false }); a[0] = 9; const d = Object.getOwnPropertyDescriptor(a, 0); return [a[0], d.writable, d.configurable]; });
t2('isSealed-per-elem', () => { const a = [1]; Object.defineProperty(a, 0, { configurable: false }); Object.preventExtensions(a); return [Object.isSealed(a), Object.isFrozen(a)]; });
t2('isFrozen-per-elem', () => { const a = [1, , 3]; Object.defineProperty(a, 0, { configurable: false, writable: false }); Object.defineProperty(a, 2, { configurable: false, writable: false }); Object.defineProperty(a, 'length', { writable: false }); Object.preventExtensions(a); return [Object.isSealed(a), Object.isFrozen(a)]; });
t2('isSealed-some-elem', () => { const a = [1, 2]; Object.defineProperty(a, 0, { configurable: false }); Object.preventExtensions(a); return [Object.isSealed(a), Object.isFrozen(a)]; });
console.log(out2.join('\n'));
const out3 = [];
function t3(name, f) { try { out3.push(name + ': ' + JSON.stringify(f())); } catch (e) { out3.push(name + ': THROW ' + e.name); } }
t3('push-sealed-named', () => { const a = [1]; a.foo = 1; Object.seal(a); try { a.push(2); return 'no throw'; } catch (e) { return [e.name, a.length]; } });
t3('push-nonext-named', () => { const a = [1]; a.foo = 1; Object.preventExtensions(a); try { a.push(2); return 'no throw'; } catch (e) { return [e.name, a.length]; } });
t3('push-hot-sealed', () => { const a = [1]; a.foo = 1; Object.seal(a); let n = 0; for (let i = 0; i < 5000; i++) { try { a.push(i); } catch (e) { n++; } } return [n, a.length]; });
t3('unshift-nonext', () => { const a = [1, 2]; Object.preventExtensions(a); try { a.unshift(0); return 'no throw'; } catch (e) { return [e.name, a]; } });
t3('splice-nonext-shrink', () => { const a = [1, 2, 3]; Object.preventExtensions(a); a.splice(0, 1); return a; });
t3('reverse-sealed', () => { const a = [1, 2, 3]; Object.seal(a); a.reverse(); return a; });
t3('sort-sealed', () => { const a = [3, 1, 2]; Object.seal(a); a.sort(); return a; });
console.log(out3.join('\n'));"#,
        &[
            "jit-frozen-store: [19996,19997,19998,19999]",
            "jit-frozen-store2: [\"11111111\",true]",
            "jit-frozen-strict: [\"TypeError\",[19996,19997,19998,19999]]",
            "jit-sealed-store: [19996,19997,19998,19999]",
            "jit-sealed-push: [\"TypeError\",2]",
            "jit-nonext-hole: [null,false,3]",
            "sealed-hole-fill: [null,false]",
            "sealed-hole-fill-strict: [\"TypeError\",false]",
            "nonext-hole-fill: [null,false]",
            "frozen-hole-fill: [null,false]",
            "define-frozen-elem: [\"TypeError\",1]",
            "define-frozen-elem-same: 1",
            "reflect-define-frozen: [false,2,false,false]",
            "define-sealed-elem: [9,\"TypeError\",\"TypeError\"]",
            "sealed-length0: [3,[1,2,3]]",
            "sealed-length0-strict: [\"TypeError\",3]",
            "sealed-define-length: [false,3]",
            "sealed-reflect-set-length: [false,3]",
            "sealed-sparse-length: [4,true]",
            "sealed-grow-length: 5",
            "pe-then-seal: [3,true]",
            "frozen-pop: [\"TypeError\",2]",
            "sealed-pop: [\"TypeError\",2]",
            "frozen-sort: [\"TypeError\",[3,1,2]]",
            "frozen-fill: [\"TypeError\",[3,1,2]]",
            "frozen-reverse: [\"TypeError\",[3,1,2]]",
            "frozen-copyWithin: [\"TypeError\",[3,1,2]]",
            "frozen-splice: [\"TypeError\",[3,1,2]]",
            "frozen-unshift: [\"TypeError\",[3,1,2]]",
            "frozen-shift: [\"TypeError\",[3,1,2]]",
            "sealed-splice-grow: [\"TypeError\",[3,1,2]]",
            "pop-nonext: 2",
            "pop-sealed-trailing-hole: 2",
            "shift-nonext: [2,[2,3]]",
            "fill-nonext-holey: THROW TypeError",
            "reflect-define-hole-nonext: [false,false]",
            "frozen-args-define: [false,1]",
            "frozen-fn-name: [false,\"f\",false]",
            "sealed-fn-name: [false,\"f\"]",
            "sealed-elem-accessor: [false,1]",
            "frozen-writable-true: [false,false]",
            "sealed-keep-desc: [5,true,true,false,true,false]",
            "sealed-make-readonly: [1,false,false]",
            "isSealed-per-elem: [true,false]",
            "isFrozen-per-elem: [true,true]",
            "isSealed-some-elem: [false,false]",
            "push-sealed-named: [\"TypeError\",1]",
            "push-nonext-named: [\"TypeError\",1]",
            "push-hot-sealed: [5000,1]",
            "unshift-nonext: [\"TypeError\",[1,2]]",
            "splice-nonext-shrink: [2,3]",
            "reverse-sealed: [3,2,1]",
            "sort-sealed: [1,2,3]",
        ],
    ),
    (
        "frozen_array_compiled_store",
        r#"
// A hot loop keeps storing into an array that gets frozen mid-loop: the
// compiled dense-store lane must not reuse its pre-freeze snapshot (the arrow
// call gives the function a cross-call site, which enables snapshot reuse).
const fmt = (a) => a.join(',');
for (let k = 0; k < 20000; k++) fmt([k]);
function strictRun(n) {
  'use strict';
  const a = [1, 2, 3, 4, 5, 6, 7, 8];
  let err = 'none';
  try {
    for (let i = 0; i < n; i++) {
      if (i === 20000) Object.freeze(a);
      a[i & 7] = i;
    }
  } catch (e) { err = e.constructor.name; }
  return err + ' ' + Object.isFrozen(a) + ' ' + fmt(a);
}
function sloppyRun(n, integrity) {
  const a = [1, 2, 3, 4, 5, 6, 7, 8];
  for (let i = 0; i < n; i++) {
    if (i === 20000) integrity(a);
    a[i & 7] = i;
  }
  return fmt(a);
}
console.log(strictRun(30000));
console.log(sloppyRun(30000, Object.freeze));
console.log(sloppyRun(30000, Object.seal));
console.log(sloppyRun(30000, Object.preventExtensions));"#,
        &[
            "TypeError true 19992,19993,19994,19995,19996,19997,19998,19999",
            "19992,19993,19994,19995,19996,19997,19998,19999",
            "29992,29993,29994,29995,29996,29997,29998,29999",
            "29992,29993,29994,29995,29996,29997,29998,29999",
        ],
    ),
    (
        "numbers_and_builtin_order",
        r#"
// StrWhiteSpace in StringToBigInt/StringToNumber/trim, exact rounding of
// long parseInt/Number/literal digit strings, JSON.rawJSON primitives only,
// defineProperty's argument order, Array.prototype's length, functions with
// a null [[Prototype]], and RegExp.prototype[@@matchAll]'s Get order.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name); } }
const F = '\uFEFF', N = '\u0085';
t('bigint-feff', () => String(BigInt(F + '12' + F)));
t('bigint-nel', () => String(BigInt(N + '12')));
t('bigint-eq-nel', () => 12n == N + '12');
t('bigint-lt-nel', () => 12n < N + '13');
t('bigint-lt-feff', () => 12n < F + '13');
t('bigint-eq-feff', () => 12n == F + '12');
t('asIntN-nel', () => String(BigInt.asIntN(8, N + '12')));
t('number-nel', () => Number(N + '12'));
t('number-feff', () => Number(F + '12' + F));
t('trim-nel', () => (' ' + N + '1' + N).trim().length);
t('trimStart-feff', () => (F + 'a').trimStart());
t('hot-bigint-eq', () => { let n = 0; for (let i = 0; i < 20000; i++) if (12n == N + '12') n++; return n; });
t('parseInt-19', () => parseInt('1234567890123456789'));
t('parseInt-eq', () => ['12345678901234567890', '900719925474099267', '9223372036854775807', '123456789012345678', '1000000000000000128', '90284280488608042'].map(s => parseInt(s) === Number(s)));
t('parseInt-hex', () => [parseInt('20000000000001F', 16), parseInt('8000000000000401', 16), parseInt('1' + '0'.repeat(52) + '11', 2), parseInt('-20000000000001F', 16)]);
t('number-hex', () => [Number('0x20000000000001F'), Number('0b' + '1' + '0'.repeat(52) + '11'), Number('0o7777777777777777777777')]);
t('literal-hex', () => [0x20000000000001F, 0b100000000000000000000000000000000000000000000000000011, 0o7777777777777777777777]);
t('parseInt-big', () => [parseInt('f'.repeat(300), 16), parseInt('1'.repeat(40)), parseInt('zzzzzzzzzzzzzzzz', 36), parseInt('1'.repeat(40), 3), parseInt('v'.repeat(20), 32), parseInt('3'.repeat(30), 4)]);
t('parseInt-fuzz', () => { let bad = 0; let seed = 7; const rnd = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648; for (let i = 0; i < 2000; i++) { let n = 16 + Math.floor(rnd() * 5); let s = String(1 + Math.floor(rnd() * 9)); for (let j = 1; j < n; j++) s += Math.floor(rnd() * 10); if (parseInt(s) !== Number(s)) bad++; } return bad; });
t('rawJSON', () => ['{}', '[1,2]', '{"x":1}', '1', '"a"', 'null'].map(s => { try { return JSON.stringify({ v: JSON.rawJSON(s) }); } catch (e) { return e.name; } }));
t('defineProperty-order', () => { const log = []; try { Object.defineProperty(1, { toString() { log.push('ts'); return 'k'; } }, {}); } catch (e) { return [e.name, log]; } });
t('arrproto-bigidx', () => { Object.defineProperty(Array.prototype, '4294967295', { value: 1, configurable: true }); const a = Array.prototype.length; delete Array.prototype['4294967295']; return [a, Array.prototype.length]; });
t('null-proto-fn', () => { function f() { return 1; } Object.setPrototypeOf(f, null); let c; try { c = f.call(null); } catch (e) { c = e.name; } return [typeof f.call, typeof f.apply, typeof f.bind, typeof f.toString, typeof f.constructor, 'call' in f, c, Reflect.get(f, 'call') === undefined]; });
t('null-proto-class', () => { class C {} Object.setPrototypeOf(C, null); return [typeof C.call, typeof C.constructor]; });
t('null-proto-arrow', () => { const a = () => 1; Object.setPrototypeOf(a, null); return typeof a.call; });
t('null-proto-native', () => { const m = Math.max; Object.setPrototypeOf(m, null); const r = typeof m.call; Object.setPrototypeOf(m, Function.prototype); return r; });
t('create-null-fn', () => { function f() {} Object.setPrototypeOf(f, null); return typeof Object.create(f).call; });
t('hot-null-proto', () => { function f() { return 1; } Object.setPrototypeOf(f, null); let n = 0; for (let i = 0; i < 20000; i++) if (f.call !== undefined) n++; return n; });
t('matchAll-order', () => { const log = []; const o = { get constructor() { log.push('constructor'); return RegExp; }, get flags() { log.push('flags'); return 'g'; } }; RegExp.prototype[Symbol.matchAll].call(o, 'x'); return log.slice(0, 2); });
console.log(out.join('\n'));"#,
        &[
            "bigint-feff: \"12\"",
            "bigint-nel: THROW SyntaxError",
            "bigint-eq-nel: false",
            "bigint-lt-nel: false",
            "bigint-lt-feff: true",
            "bigint-eq-feff: true",
            "asIntN-nel: THROW SyntaxError",
            "number-nel: null",
            "number-feff: 12",
            "trim-nel: 3",
            "trimStart-feff: \"a\"",
            "hot-bigint-eq: 0",
            "parseInt-19: 1234567890123456800",
            "parseInt-eq: [true,true,true,true,true,true]",
            "parseInt-hex: [144115188075855900,9223372036854778000,18014398509481988,-144115188075855900]",
            "number-hex: [144115188075855900,18014398509481988,73786976294838210000]",
            "literal-hex: [144115188075855900,9007199254740996,73786976294838210000]",
            "parseInt-big: [null,1.1111111111111112e+39,7.958661109946401e+24,6078832729528464000,1.2676506002282294e+30,1152921504606847000]",
            "parseInt-fuzz: 0",
            "rawJSON: [\"SyntaxError\",\"SyntaxError\",\"SyntaxError\",\"{\\\"v\\\":1}\",\"{\\\"v\\\":\\\"a\\\"}\",\"{\\\"v\\\":null}\"]",
            "defineProperty-order: [\"TypeError\",[]]",
            "arrproto-bigidx: [0,0]",
            "null-proto-fn: [\"undefined\",\"undefined\",\"undefined\",\"undefined\",\"undefined\",false,\"TypeError\",true]",
            "null-proto-class: [\"undefined\",\"undefined\"]",
            "null-proto-arrow: \"undefined\"",
            "null-proto-native: \"undefined\"",
            "create-null-fn: \"undefined\"",
            "hot-null-proto: 0",
            "matchAll-order: [\"constructor\",\"flags\"]",
        ],
    ),
    (
        "group_by_and_define_properties",
        r#"
// Object.groupBy/Map.groupBy step, call and coerce one element at a time
// (closing the iterator on an abrupt callback); Object.defineProperties
// reads every descriptor, once, before defining any.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name + ':' + e.message); } }
t('groupBy-order', () => { const log = []; function* g() { log.push('n0'); yield 1; log.push('n1'); yield 2; log.push('n2'); } Object.groupBy(g(), x => { log.push('cb' + x); return 'k'; }); const l2 = []; function* g2() { l2.push('n0'); yield 1; l2.push('n1'); yield 2; l2.push('n2'); } Map.groupBy(g2(), x => { l2.push('cb' + x); return 'k'; }); return [log, l2]; });
t('groupBy-close', () => { let closed = false; const it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { closed = true; return {}; } }; } }; try { Object.groupBy(it, () => { throw new Error('boom'); }); } catch (e) { return [e.message, closed]; } });
t('groupBy-keyerr-close', () => { let closed = false; const it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { closed = true; return {}; } }; } }; try { Object.groupBy(it, () => ({ toString() { throw new Error('key'); } })); } catch (e) { return [e.message, closed]; } });
t('groupBy-inf', () => { function* g() { let i = 0; while (true) yield i++; } try { Object.groupBy(g(), x => { if (x === 3) throw new Error('stop'); return 'k'; }); } catch (e) { return e.message; } });
t('map-groupBy-inf', () => { function* g() { let i = 0; while (true) yield i++; } try { Map.groupBy(g(), x => { if (x === 3) throw new Error('stop'); return 'k'; }); } catch (e) { return e.message; } });
t('groupBy-array', () => { const r = Object.groupBy([1, 2, 3, 4, 5], x => x % 2 ? 'odd' : 'even'); return [Object.getPrototypeOf(r), r.odd, r.even]; });
t('groupBy-array-mutate', () => { const a = [1, 2, 3]; const r = Object.groupBy(a, (x, i) => { if (i === 0) a.push(4); return 'k'; }); return r.k; });
t('groupBy-holes', () => Object.groupBy([1, , 3], x => String(x)));
t('map-groupBy', () => [...Map.groupBy([1, 2, 3, -0, 0], x => x > 1 ? 'big' : x)].map(([k, v]) => [Object.is(k, -0) ? '-0' : k, v]));
t('groupBy-string', () => Object.groupBy('abca', c => c));
t('groupBy-set', () => Object.groupBy(new Set([1, 2, 3]), x => x & 1));
t('groupBy-index', () => { const idx = []; Object.groupBy(['a', 'b'], (x, i) => { idx.push(i); return x; }); return idx; });
t('defineProperties-atomic', () => { const o = {}; try { Object.defineProperties(o, { a: { value: 1 }, b: { get: 1 } }); } catch (e) { return [e.name, Object.getOwnPropertyNames(o)]; } });
t('defineProperties-throwing-getter', () => { const o = {}; try { Object.defineProperties(o, { a: { value: 1 }, b: { get enumerable() { throw 3; } } }); } catch (e) { return [e, Object.getOwnPropertyNames(o)]; } });
t('defineProperties-deleted', () => { const bag = { get a() { delete bag.b; return { value: 1 }; }, b: { value: 2 } }; return Object.getOwnPropertyNames(Object.defineProperties({}, bag)); });
t('create-atomic', () => { try { Object.create({}, { a: { value: 1 }, b: { get: 2 } }); return 'no throw'; } catch (e) { return e.name; } });
t('defineProperties-getter-once', () => { let n = 0; const d = { get value() { n++; return 5; } }; const o = Object.defineProperties({}, { x: d }); return [o.x, n]; });
t('defineProperties-order', () => Object.keys(Object.defineProperties({}, { b: { value: 1, enumerable: true }, a: { value: 2, enumerable: true }, 1: { value: 3, enumerable: true } })));
console.log(out.join('\n'));"#,
        &[
            "groupBy-order: [[\"n0\",\"cb1\",\"n1\",\"cb2\",\"n2\"],[\"n0\",\"cb1\",\"n1\",\"cb2\",\"n2\"]]",
            "groupBy-close: [\"boom\",true]",
            "groupBy-keyerr-close: [\"key\",true]",
            "groupBy-inf: \"stop\"",
            "map-groupBy-inf: \"stop\"",
            "groupBy-array: [null,[1,3,5],[2,4]]",
            "groupBy-array-mutate: [1,2,3,4]",
            "groupBy-holes: {\"1\":[1],\"3\":[3],\"undefined\":[null]}",
            "map-groupBy: [[1,[1]],[\"big\",[2,3]],[0,[0,0]]]",
            "groupBy-string: {\"a\":[\"a\",\"a\"],\"b\":[\"b\"],\"c\":[\"c\"]}",
            "groupBy-set: {\"0\":[2],\"1\":[1,3]}",
            "groupBy-index: [0,1]",
            "defineProperties-atomic: [\"TypeError\",[]]",
            "defineProperties-throwing-getter: [3,[]]",
            "defineProperties-deleted: [\"a\"]",
            "create-atomic: \"TypeError\"",
            "defineProperties-getter-once: [5,1]",
            "defineProperties-order: [\"1\",\"b\",\"a\"]",
        ],
    ),
    (
        "receivers_regexp_plural_arguments",
        r#"
// Reflect.set/super.x = v with an exotic receiver use its real own
// descriptor; RegExp accessors answer from slots only for the RegExp itself;
// English plural rules; mapped arguments past formal 63; a constructor's
// toString keeps its initial name; a function's prototype redefinition.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name); } }
t('rs-arr-len', () => { const arr = [1, 2, 3]; return [Reflect.set({}, 'length', 1, arr), arr.length, arr]; });
t('rs-fn-proto', () => { function f() {} return [Reflect.set({}, 'prototype', 1, f), f.prototype]; });
t('rs-fn-name', () => { function f() {} const r = Reflect.set({}, 'name', 'z', f); const d = Object.getOwnPropertyDescriptor(f, 'name'); return [r, f.name, d.writable, d.enumerable, d.configurable]; });
t('rs-fn-length', () => { function f(a) {} return [Reflect.set({}, 'length', 9, f), f.length]; });
t('rs-re-lastIndex', () => { const re = /a/g; return [Reflect.set({}, 'lastIndex', 5, re), re.lastIndex]; });
t('rs-str-len', () => { const s = new String('abc'); return [Reflect.set({}, 'length', 1, s), s.length]; });
t('rs-str-idx', () => { const s = new String('abc'); return [Reflect.set({}, '0', 'y', s), s[0]]; });
t('super-length', () => { class A extends Array { setLen(n) { super.length = n; return this.length; } } const x = new A(); x.push(1, 2, 3); return x.setLen(1); });
t('rs-arr-sealed-len', () => { const arr = Object.seal([1, 2, 3]); return [Reflect.set({}, 'length', 1, arr), arr.length]; });
t('rs-ordinary', () => { const r = { x: 1 }; return [Reflect.set({}, 'x', 2, r), r.x]; });
t('rs-arr-elem-frozen', () => [Reflect.set({}, '0', 9, Object.freeze([1])), Reflect.set({}, '0', 9, Object.seal([1]))]);
t('rs-len-coerce-throws', () => { const arr = [1, 2]; try { Reflect.set({}, 'length', { valueOf() { throw new Error('vo'); } }, arr); return 'no throw'; } catch (e) { return e.message; } });
t('re-proxy-source', () => { try { return new Proxy(/a/g, {}).source; } catch (e) { return e.name; } });
t('re-proxy-global', () => { try { return new Proxy(/a/g, {}).global; } catch (e) { return e.name; } });
t('re-proxy-flags', () => { try { return new Proxy(/a/g, {}).flags; } catch (e) { return e.name; } });
t('re-create', () => { const o = Object.create(/a/g); const r = []; for (const k of ['source', 'global', 'flags']) { try { r.push(o[k]); } catch (e) { r.push(e.name); } } r.push(Object.create(Object.assign(/a/g, { lastIndex: 3 })).lastIndex); return r; });
t('re-reflect-get', () => { try { return Reflect.get(/a/g, 'source', {}); } catch (e) { return e.name; } });
t('re-string-proxy', () => { try { return String(new Proxy(/a/g, {})); } catch (e) { return e.name; } });
t('re-new-proxy', () => { try { const r = new RegExp(new Proxy(/a/g, {})); return [r.source, r.flags]; } catch (e) { return e.name; } });
t('re-proto-source', () => [RegExp.prototype.source, RegExp.prototype.flags, RegExp.prototype.global]);
t('re-subclass', () => { class R extends RegExp { get global() { return true; } } const r = new R('a'); return [r.global, r.flags, r.source]; });
t('re-normal', () => { const r = /ab+c/gi; let n = 0; for (let i = 0; i < 20000; i++) if (r.global && r.source.length === 4) n++; return [n, r.flags]; });
t('plural-ordinal', () => { const p = new Intl.PluralRules('en', { type: 'ordinal' }); const s = { one: 'st', two: 'nd', few: 'rd', other: 'th' }; return [[1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 102, 111, 112].map(n => n + s[p.select(n)]).join(' '), p.resolvedOptions().pluralCategories]; });
t('plural-cardinal', () => { const p = (o) => new Intl.PluralRules('en', o); return [p({ maximumFractionDigits: 0 }).select(1.2), p().select(-1), p().select(1.0000001), p({ minimumFractionDigits: 1 }).select(1), p().select(0), p().select(2), p().select('1'), p({ notation: 'compact' }).select(1000), p({ notation: 'scientific' }).select(1), p().select(Infinity), p().select(NaN), p({ minimumSignificantDigits: 2 }).select(1), p({ maximumSignificantDigits: 1 }).select(1.4), p().select(1e21), p().resolvedOptions().pluralCategories]; });
t('plural-ordinal-edge', () => { const p = new Intl.PluralRules('en', { type: 'ordinal' }); return [p.select(21.0), p.select(1.5), new Intl.PluralRules('en', { type: 'ordinal', minimumFractionDigits: 1 }).select(21), p.select(-1), p.select(1e21)]; });
t('args-70', () => { const params = []; for (let i = 0; i < 70; i++) params.push('p' + i); const f = new Function(...params, 'arguments[65] = "A"; p66 = "B"; arguments[3] = "C"; p63 = "D"; arguments[64] = "E"; return [p65, arguments[66], p3, arguments[63], p64];'); return f(...Array.from({ length: 70 }, (_, i) => i)); });
t('args-70-delete', () => { const params = []; for (let i = 0; i < 70; i++) params.push('p' + i); const f = new Function(...params, 'delete arguments[65]; arguments[65] = "X"; p66 = 5; delete arguments[66]; return [p65, arguments[65], arguments[66], p66];'); return f(...Array.from({ length: 70 }, (_, i) => i)); });
t('args-delete-huge', () => { function f(a) { delete arguments[4294967294]; arguments[0] = 2; return a; } return f(1); });
t('ctor-name-spoof', () => { const saved = Object.getOwnPropertyDescriptor(Map, 'name'); Object.defineProperty(Map, 'name', { value: 'x() { return 1 } function y' }); const s = Map.toString(); Object.defineProperty(Map, 'name', saved); return [s, Map.name]; });
t('ctor-name-delete', () => { const saved = Object.getOwnPropertyDescriptor(Promise, 'name'); delete Promise.name; const s = String(Promise); Object.defineProperty(Promise, 'name', saved); return s; });
t('ctor-name-array', () => { Object.defineProperty(Array, 'name', { value: 'Q', configurable: true }); const s = Function.prototype.toString.call(Array); Object.defineProperty(Array, 'name', { value: 'Array' }); return s; });
console.log(out.join('\n'));
const out4 = [];
function t4(name, f) { try { out4.push(name + ': ' + JSON.stringify(f())); } catch (e) { out4.push(name + ': THROW ' + e.name); } }
t4('define-fn-proto', () => { function f() {} Object.defineProperty(f, 'prototype', { value: 1 }); function g() {} g.prototype; const p = {}; Object.defineProperty(g, 'prototype', { value: p }); return [f.prototype, Object.getOwnPropertyDescriptor(f, 'prototype').value, g.prototype === p, Object.getPrototypeOf(new g()) === p]; });
t4('define-class-proto', () => { class C {} const r = [Reflect.defineProperty(C, 'prototype', { value: {} }), Reflect.defineProperty(C, 'prototype', { value: C.prototype }), Reflect.defineProperty(C, 'prototype', { writable: true })]; return r; });
console.log(out4.join('\n'));"#,
        &[
            "rs-arr-len: [true,1,[1]]",
            "rs-fn-proto: [true,1]",
            "rs-fn-name: [false,\"f\",false,false,true]",
            "rs-fn-length: [false,1]",
            "rs-re-lastIndex: [true,5]",
            "rs-str-len: [false,3]",
            "rs-str-idx: [false,\"a\"]",
            "super-length: 1",
            "rs-arr-sealed-len: [false,3]",
            "rs-ordinary: [true,2]",
            "rs-arr-elem-frozen: [false,true]",
            "rs-len-coerce-throws: \"vo\"",
            "re-proxy-source: \"TypeError\"",
            "re-proxy-global: \"TypeError\"",
            "re-proxy-flags: \"TypeError\"",
            "re-create: [\"TypeError\",\"TypeError\",\"TypeError\",3]",
            "re-reflect-get: \"TypeError\"",
            "re-string-proxy: \"TypeError\"",
            "re-new-proxy: \"TypeError\"",
            "re-proto-source: [\"(?:)\",\"\",null]",
            "re-subclass: [true,\"g\",\"a\"]",
            "re-normal: [20000,\"gi\"]",
            "plural-ordinal: [\"1st 2nd 3rd 4th 11th 12th 13th 21st 22nd 23rd 101st 102nd 111th 112th\",[\"one\",\"two\",\"few\",\"other\"]]",
            "plural-cardinal: [\"one\",\"one\",\"one\",\"other\",\"other\",\"other\",\"one\",\"other\",\"one\",\"other\",\"other\",\"other\",\"one\",\"other\",[\"one\",\"other\"]]",
            "plural-ordinal-edge: [\"one\",\"other\",\"one\",\"one\",\"other\"]",
            "args-70: [\"A\",\"B\",\"C\",\"D\",\"E\"]",
            "args-70-delete: [65,\"X\",null,5]",
            "args-delete-huge: 2",
            "ctor-name-spoof: [\"function Map() { [native code] }\",\"Map\"]",
            "ctor-name-delete: \"function Promise() { [native code] }\"",
            "ctor-name-array: \"function Array() { [native code] }\"",
            "define-fn-proto: [1,1,true,true]",
            "define-class-proto: [false,true,false]",
        ],
    ),
    (
        "error_name_is_inherited",
        r#"
// Error instances inherit `name` from their prototype ([[ErrorData]], not
// an own name, marks them), so Error.prototype.name and newTarget
// prototypes are observed.
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name); } }
t('own-names', () => [Object.getOwnPropertyNames(new TypeError('m')).filter(k => k !== 'stack'), Object.hasOwn(new Error('y'), 'name'), Object.hasOwn(new RangeError('y'), 'name')]);
t('agg-desc', () => Object.getOwnPropertyDescriptor(new AggregateError([]), 'name'));
t('custom-proto', () => { function P() {} P.prototype = Object.create(TypeError.prototype, { name: { value: 'Custom' } }); const e = Reflect.construct(TypeError, ['m'], P); return [e.name, String(e)]; });
t('subclass', () => { class MyErr extends Error {} MyErr.prototype.name = 'MyErr'; const e = new MyErr('z'); return [e.name, String(e), e instanceof Error]; });
t('legacy', () => { function Legacy(m) { var e = new Error(m); Object.setPrototypeOf(e, Legacy.prototype); return e; } Legacy.prototype = Object.create(Error.prototype); Legacy.prototype.name = 'Legacy'; const e = new Legacy('boom'); return [e.name, String(e), e instanceof Error]; });
t('tag', () => [Object.prototype.toString.call(new TypeError()), Object.prototype.toString.call({ name: 'TypeError' }), Object.prototype.toString.call(TypeError.prototype), Object.prototype.toString.call(Object.create(Error.prototype))]);
t('internal', () => { try { null.x; } catch (e) { return [e.name, e instanceof TypeError, String(e).slice(0, 10), Object.hasOwn(e, 'name')]; } });
t('internal-range', () => { try { new Array(-1); } catch (e) { return [e.name, e.constructor === RangeError]; } });
t('promise-any', async () => 0);
t('isError', () => [Error.isError(new Error()), Error.isError({ name: 'Error' })]);
t('json', () => JSON.stringify(new Error('x')));
t('keys', () => Object.keys(new TypeError('x')));
t('assign-name', () => { const e = new Error('x'); e.name = 'Foo'; return [String(e), Object.hasOwn(e, 'name')]; });
t('proto-changed', () => { const old = Error.prototype.name; Error.prototype.name = 'Changed'; const r = [String(new Error('x')), new Error('y').name]; Error.prototype.name = old; return r; });
t('uri', () => { try { decodeURIComponent('%'); } catch (e) { return [e.name, e instanceof URIError]; } });
t('eval-err', () => [new EvalError('a').name, new SyntaxError('b').name, new ReferenceError('c').name]);
console.log(out.join('\n'));
Promise.any([Promise.reject(1)]).catch(e => console.log('any:', e.name, e.constructor === AggregateError, Object.hasOwn(e, 'name'), String(e)));
console.log(String(new TypeError('boom')));"#,
        &[
            "own-names: [[\"message\"],false,false]",
            "agg-desc: undefined",
            "custom-proto: [\"Custom\",\"Custom: m\"]",
            "subclass: [\"MyErr\",\"MyErr: z\",true]",
            "legacy: [\"Legacy\",\"Legacy: boom\",true]",
            "tag: [\"[object Error]\",\"[object Object]\",\"[object Object]\",\"[object Object]\"]",
            "internal: [\"TypeError\",true,\"TypeError:\",false]",
            "internal-range: [\"RangeError\",true]",
            "promise-any: {}",
            "isError: [true,false]",
            "json: \"{}\"",
            "keys: []",
            "assign-name: [\"Foo: x\",true]",
            "proto-changed: [\"Changed: x\",\"Changed\"]",
            "uri: [\"URIError\",true]",
            "eval-err: [\"EvalError\",\"SyntaxError\",\"ReferenceError\"]",
            "TypeError: boom",
            "any: AggregateError true false AggregateError: All promises were rejected",
        ],
    ),
    (
        "string_keys_spelled_like_symbols",
        r#"
// String property keys that begin with "@@" are ordinary string keys: they
// survive enumeration and JSON, and never alias a Symbol-keyed property
// (well-known, registered or unique).
const out = [];
function t(name, f) { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': THROW ' + e.name); } }
t('json-roundtrip', () => JSON.stringify(JSON.parse('{"@@toStringTag":"Array","@@user":1,"n":3}')));
t('json-keys', () => Object.keys(JSON.parse('{"@@a":1,"b":2}')));
t('tostring-spoof', () => Object.prototype.toString.call(JSON.parse('{"@@toStringTag":"Evil"}')));
t('concat-spoof', () => [].concat(JSON.parse('{"@@isConcatSpreadable":true,"length":2,"0":"a","1":"b"}')).length);
t('toPrimitive-spoof', () => `${JSON.parse('{"@@toPrimitive":1}')}`);
t('iterator-spoof', () => { try { return [...{ '@@iterator': function* () { yield 1; } }]; } catch (e) { return e.name; } });
t('iterator-alias', () => { const o2 = {}; o2['@@iterator'] = 5; return [o2[Symbol.iterator], Object.keys(o2)]; });
t('symbol-alias', () => { const o3 = {}; o3[Symbol.iterator] = 7; return [o3['@@iterator'], '@@iterator' in o3, Object.hasOwn(o3, '@@iterator')]; });
t('entries', () => Object.entries(JSON.parse('{"@@species":1,"ok":2}')));
t('forged-sym', () => { const secret = Symbol('secret'); const vault = { [secret]: 's3cr3t' }; for (let n = 0; n < 200; n++) if (vault['@@sym:' + n] !== undefined) return 'forged ' + vault['@@sym:' + n]; return 'none'; });
t('forged-for', () => { const o = {}; o[Symbol.for('k')] = 1; o['@@for:k'] = 2; return [o[Symbol.for('k')], o['@@for:k'], Object.keys(o), Object.getOwnPropertySymbols(o).length]; });
t('spread-assign', () => { const p = { '@@a': 1, b: 2 }; return [JSON.stringify({ ...p }), JSON.stringify(Object.assign({}, p)), Object.getOwnPropertyNames(p)]; });
t('for-in', () => { const r = []; for (const k in { '@@iterator': 1, '@@transducer/step': 2, a: 3 }) r.push(k); return r; });
t('ownKeys', () => Reflect.ownKeys({ [Symbol.iterator]: 1, '@@iterator': 2 }).map(String));
t('symbols', () => Object.getOwnPropertySymbols({ '@@x': 1, [Symbol('y')]: 2 }).map(String));
t('in-hasown', () => { const d = {}; d['@@x'] = 5; return ['@@x' in d, d.hasOwnProperty('@@x'), Object.getOwnPropertyNames(d), Object.getOwnPropertySymbols(d).length]; });
t('array-proto-iter', () => typeof Array.prototype['@@iterator']);
t('computed-literal', () => { const o = { ['@@k']: 1, '@@j': 2 }; const { '@@k': k, ['@@j']: j } = o; return [k, j, o['@@k'], Object.keys(o)]; });
t('class-literal-key', () => { class C { '@@m'() { return 1; } static '@@s' = 2; } return [new C()['@@m'](), C['@@s'], Object.getOwnPropertyNames(C.prototype)]; });
t('define', () => { const o = {}; Object.defineProperty(o, '@@d', { value: 1, enumerable: true }); return [o['@@d'], Object.getOwnPropertyDescriptor(o, '@@d').value, Object.keys(o), JSON.stringify(Object.getOwnPropertyDescriptors(o))]; });
t('delete', () => { const o = { '@@z': 1 }; delete o['@@z']; return Object.keys(o); });
t('proxy-trap-key', () => { const seen = []; const p = new Proxy({}, { get(t, k) { seen.push(typeof k === 'symbol' ? 'sym' : k); return 1; } }); p['@@x']; p[Symbol.iterator]; return seen; });
t('reviver', () => { const keys = []; JSON.parse('{"@@r":1}', function (k, v) { keys.push(k); return v; }); return keys; });
t('groupBy', () => Object.keys(Object.groupBy(['a', 'b'], x => '@@' + x)));
t('fromEntries', () => Object.keys(Object.fromEntries([['@@e', 1]])));
t('hot-keys', () => { let n = 0; for (let i = 0; i < 20000; i++) { const o = { ['@@k' + (i & 7)]: i, x: 1 }; n += Object.keys(o).length; } return n; });
t('hot-index', () => { const o = {}; const k = '@@hot'; let s = 0; for (let i = 0; i < 20000; i++) { o[k] = i; s += o[k]; } return [s, Object.keys(o)]; });
t('at-single', () => { const o = { '@x': 1, '@': 2, '@@': 3, '@@@': 4 }; return [Object.keys(o), JSON.stringify(o), o['@@'], o['@@@']]; });
t('well-known-still', () => { const o = { [Symbol.toStringTag]: 'T', *[Symbol.iterator]() { yield 9; } }; return [Object.prototype.toString.call(o), [...o]]; });
console.log(out.join('\n'));"#,
        &[
            "json-roundtrip: \"{\\\"@@toStringTag\\\":\\\"Array\\\",\\\"@@user\\\":1,\\\"n\\\":3}\"",
            "json-keys: [\"@@a\",\"b\"]",
            "tostring-spoof: \"[object Object]\"",
            "concat-spoof: 1",
            "toPrimitive-spoof: \"[object Object]\"",
            "iterator-spoof: \"TypeError\"",
            "iterator-alias: [null,[\"@@iterator\"]]",
            "symbol-alias: [null,false,false]",
            "entries: [[\"@@species\",1],[\"ok\",2]]",
            "forged-sym: \"none\"",
            "forged-for: [1,2,[\"@@for:k\"],1]",
            "spread-assign: [\"{\\\"@@a\\\":1,\\\"b\\\":2}\",\"{\\\"@@a\\\":1,\\\"b\\\":2}\",[\"@@a\",\"b\"]]",
            "for-in: [\"@@iterator\",\"@@transducer/step\",\"a\"]",
            "ownKeys: [\"@@iterator\",\"Symbol(Symbol.iterator)\"]",
            "symbols: [\"Symbol(y)\"]",
            "in-hasown: [true,true,[\"@@x\"],0]",
            "array-proto-iter: \"undefined\"",
            "computed-literal: [1,2,1,[\"@@k\",\"@@j\"]]",
            "class-literal-key: [1,2,[\"constructor\",\"@@m\"]]",
            "define: [1,1,[\"@@d\"],\"{\\\"@@d\\\":{\\\"value\\\":1,\\\"writable\\\":false,\\\"enumerable\\\":true,\\\"configurable\\\":false}}\"]",
            "delete: []",
            "proxy-trap-key: [\"@@x\",\"sym\"]",
            "reviver: [\"@@r\",\"\"]",
            "groupBy: [\"@@a\",\"@@b\"]",
            "fromEntries: [\"@@e\"]",
            "hot-keys: 40000",
            "hot-index: [199990000,[\"@@hot\"]]",
            "at-single: [[\"@x\",\"@\",\"@@\",\"@@@\"],\"{\\\"@x\\\":1,\\\"@\\\":2,\\\"@@\\\":3,\\\"@@@\\\":4}\",3,4]",
            "well-known-still: [\"[object T]\",[9]]",
        ],
    ),
    (
        "bind_chain_name_is_linear",
        r#"
// Function.prototype.bind's "bound " + name no longer copies the target's
// name on every bind: a 40000-deep bind chain is linear, and its name is the
// same string it always was.
let f = function base() { return 1; };
const t0 = Date.now();
for (let i = 0; i < 40000; i++) f = f.bind(null);
const elapsed = Date.now() - t0;
console.log(f.name.length, f.name.slice(0, 12), f.name.slice(-10), elapsed < 5000);
let g = function () { return 7; };
for (let i = 0; i < 300; i++) g = g.bind(null, i);
console.log(g(), g.name === 'bound '.repeat(300) + 'g', g.length);
const h = Object.defineProperty(function () {}, 'name', { value: 42 }).bind(null);
console.log(JSON.stringify(h.name));"#,
        &[
            "240004 bound bound  bound base true",
            "7 true 0",
            "\"bound \"",
        ],
    ),
    (
        "symbol_spelled_keys_json_and_rest",
        r#"
// "@@…" string keys through JSON (replacer, allowlist, toJSON, reviver),
// callable and exotic own properties, object rest and descriptor maps.
const o = { '@@a': 1, b: 2, get '@@g'() { return 3; } };
console.log(JSON.stringify(o), JSON.stringify(o, (k, v) => v), JSON.stringify(o, ['@@a', 'b']), JSON.stringify({ x: { toJSON(k) { return k; } }, '@@y': { toJSON(k) { return k; } } }));
console.log(JSON.stringify(JSON.parse('{"@@r":{"@@s":1}}', function (k, v) { return v; })), JSON.stringify(JSON.parse('{"@@r":1}', (k, v) => typeof v === 'number' ? v + 1 : v)));
function f() {} f['@@p'] = 1; f.q = 2; console.log(Object.keys(f), Object.entries(f));
const m = new Map(); m['@@m'] = 1; console.log(Object.keys(m));
const { '@@a': a1, ...rest } = { '@@a': 1, '@@b': 2, c: 3 }; console.log(a1, JSON.stringify(rest));
console.log(JSON.stringify(Object.getOwnPropertyDescriptors({ '@@d': 1 })));"#,
        &[
            "{\"@@a\":1,\"b\":2,\"@@g\":3} {\"@@a\":1,\"b\":2,\"@@g\":3} {\"@@a\":1,\"b\":2} {\"x\":\"x\",\"@@y\":\"@@y\"}",
            "{\"@@r\":{\"@@s\":1}} {\"@@r\":2}",
            "[ '@@p', 'q' ] [ [ '@@p', 1 ], [ 'q', 2 ] ]",
            "[ '@@m' ]",
            "1 {\"@@b\":2,\"c\":3}",
            "{\"@@d\":{\"value\":1,\"writable\":true,\"enumerable\":true,\"configurable\":true}}",
        ],
    ),
    (
        "class_prototype_nearer_levels",
        r#"
// A farther class level whose prototype diverged must not hide what a nearer
// level's prototype carries: the live walk resumes at the nearest existing
// prototype (found by differential fuzzing of prototype mutations).
const out = [];
class A { m() { return 'Am'; } get g() { return 'Ag'; } }
class B extends A { m() { return 'Bm'; } }
class C extends B { }
const c = new C();
C.prototype.x = function () { return 'Cx'; };
B.prototype.m = function () { return 'B2'; };
out.push(c.x(), 'x' in c, c.m(), c.g);
const keys = []; for (const k in c) keys.push(k); out.push(keys.join(','));
Object.defineProperty(C.prototype, 'y', { get() { return 'Cy'; }, configurable: true });
Object.defineProperty(A.prototype, 'g', { get() { return 'A2'; }, configurable: true });
out.push(c.y, c.g, Reflect.has(c, 'y'));
function hot(o) { let r; for (let i = 0; i < 3000; i++) r = o.x() + o.m() + o.g; return r; }
out.push(hot(c), hot(new C()));
console.log(out.join('|'));"#,
        &[
            "Cx|true|B2|Ag|x|Cy|A2|true|CxB2A2|CxB2A2",
        ],
    ),
];

#[test]
fn props_child() {
    if std::env::var_os("ZIPP_PROPS_AUDIT_CHILD").is_none() {
        return;
    }
    let mut failures = Vec::new();
    for (name, src, expected) in CASES {
        // One console.log may print several lines; compare line by line.
        let got = run_ok(src).join("\n");
        let got: Vec<&str> = got.split('\n').collect();
        if got != *expected {
            failures.push(format!(
                "{name}:\n  expected {expected:?}\n  got      {got:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn props_modes_match_node() {
    if std::env::var_os("ZIPP_PROPS_AUDIT_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "props_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env("ZIPP_PROPS_AUDIT_CHILD", "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
