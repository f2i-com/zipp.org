//! Shortcuts that stand in for a built-in getter or `@@iterator` call must
//! give way once that method is replaced. A generator is its own iterator
//! for `for-of`, destructuring and `yield*` only while its `@@iterator` is
//! still %Iterator.prototype%'s: an own one, or one on the generator
//! function's prototype, %GeneratorPrototype% or %Iterator.prototype% (a
//! getter, a non-callable value) is read and called. A Map or Set's `size`
//! is its live count only while the prototype's `size` is the intrinsic
//! getter: a redefined or deleted one is what the read sees. Expected
//! output is node's.

#[test]
fn generator_iteration_honours_a_replaced_iterator_method() {
    let src = r###"const out = [];
function* leaf(n) { for (let i = 0; i < n; i++) yield i; return "r" + n; }
function* outer(g) { const r = yield* g; yield "ret:" + r; }
const run = (g) => { const xs = []; for (const v of outer(g)) xs.push(v); return xs.join(","); };
out.push("1 " + run(leaf(3)));
// an own @@iterator on the generator object
const g1 = leaf(2);
Object.defineProperty(g1, Symbol.iterator, { value() { return leaf(1); }, configurable: true });
out.push("2 " + run(g1));
// on the generator function's prototype
function* other(n) { yield n; yield n + 1; }
other.prototype[Symbol.iterator] = function () { return leaf(4); };
out.push("3 " + run(other(10)));
delete other.prototype[Symbol.iterator];
out.push("4 " + run(other(10)));
// on %GeneratorPrototype%
const GenProto = Object.getPrototypeOf(leaf.prototype);
let calls = 0;
GenProto[Symbol.iterator] = function () { calls++; return this; };
out.push("5 " + run(leaf(2)) + " calls=" + calls);
const [d0, d1] = leaf(5);
out.push("5d " + d0 + d1 + " calls=" + calls + " spread=" + [...leaf(3)].join("") + " calls=" + calls);
GenProto[Symbol.iterator] = function () { calls++; return leaf(2); };
const [e0, e1, e2] = other(7);
out.push("5e " + e0 + e1 + e2 + " " + [...other(7)].join("") + " " + Array.from(other(7)).join("") + " calls=" + calls);
delete GenProto[Symbol.iterator];
const [f0, f1] = other(7);
out.push("5f " + f0 + f1 + " calls=" + calls);
// on %Iterator.prototype%: a getter, then a non-callable value
const IterProto = Object.getPrototypeOf(GenProto);
const orig = Object.getOwnPropertyDescriptor(IterProto, Symbol.iterator);
let gets = 0;
Object.defineProperty(IterProto, Symbol.iterator, { get() { gets++; return orig.value; }, configurable: true });
out.push("6 " + run(leaf(2)) + " gets=" + gets);
Object.defineProperty(IterProto, Symbol.iterator, { value: 5, configurable: true, writable: true });
try { run(leaf(2)); out.push("7 no throw"); } catch (e) { out.push("7 " + e.constructor.name); }
Object.defineProperty(IterProto, Symbol.iterator, orig);
out.push("8 " + run(leaf(3)));
// a generator whose prototype is swapped
const g2 = leaf(2);
Object.setPrototypeOf(g2, { [Symbol.iterator]() { return leaf(5); }, next: GenProto.next });
out.push("9 " + run(g2));
// a recursive yield* over a tree, then again after patching mid-way
function* walk(t) { if (!t) return 0; const l = yield* walk(t.l); yield t.v; const r = yield* walk(t.r); return l + r + 1; }
const tree = { v: 4, l: { v: 2, l: { v: 1 }, r: { v: 3 } }, r: { v: 6, l: { v: 5 }, r: { v: 7 } } };
out.push("10 " + [...walk(tree)].join(""));
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"1 0,1,2,ret:r3
2 0,ret:r1
3 0,1,2,3,ret:r4
4 10,11,ret:undefined
5 0,1,ret:r2 calls=2
5d 01 calls=3 spread=012 calls=4
5e 01undefined 01 01 calls=7
5f 78 calls=7
6 0,1,ret:r2 gets=2
7 TypeError
8 0,1,2,ret:r3
9 0,1,2,3,4,ret:r5
10 1234567"###);
}

#[test]
fn collection_size_honours_a_replaced_getter() {
    let src = r###"const out = [];
const m = new Map([[1, 2], [3, 4]]);
const s = new Set([1, 2, 3]);
out.push("1 " + m.size + " " + s.size);
const md = Object.getOwnPropertyDescriptor(Map.prototype, "size");
const sd = Object.getOwnPropertyDescriptor(Set.prototype, "size");
Object.defineProperty(Map.prototype, "size", { get() { return "patched:" + md.get.call(this); }, configurable: true });
Object.defineProperty(Set.prototype, "size", { value: 99, configurable: true });
out.push("2 " + m.size + " " + s.size);
Object.defineProperty(Map.prototype, "size", md);
Object.defineProperty(Set.prototype, "size", sd);
out.push("3 " + m.size + " " + s.size);
delete Map.prototype.size;
out.push("4 " + m.size);
Object.defineProperty(Map.prototype, "size", md);
out.push("5 " + m.size);
Object.setPrototypeOf(Map.prototype, { get size() { return "never"; } });
out.push("6 " + m.size);
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"1 2 3
2 patched:2 99
3 2 3
4 undefined
5 2
6 2"###);
}
