// Object literals of an imported module: every form, made from hot code.
export const point = (i) => ({ x: i, y: i + 1, z: i + 2, tag: "p" });
export function record(node, parent) { return { node, parent }; }
export function shorthand(a, b) { const c = a + b; return { a, b, c, "quoted key": c, 7: "seven", 0: "zero" }; }
export function nested(i) { return { id: i, pos: { x: i, y: -i }, tags: ["a", { deep: { deeper: i } }], fn: () => i * 2 }; }
export function wide(i) { return { k0: i, k1: 1, k2: 2, k3: 3, k4: 4, k5: 5, k6: 6, k7: 7, k8: 8, k9: 9, k10: 10, k11: 11, k12: 12, k13: 13, k14: 14, k15: 15 }; }
export function wider(i) { return { k0: i, k1: 1, k2: 2, k3: 3, k4: 4, k5: 5, k6: 6, k7: 7, k8: 8, k9: 9, k10: 10, k11: 11, k12: 12, k13: 13, k14: 14, k15: 15, k16: 16, k17: 17 }; }
export function duplicate(i) { return { a: i, b: 2, a: i + 100 }; }
export function withProto(p, i) { return { __proto__: p, own: i }; }
export function computed(k, i) { return { first: i, [k]: i + 1, last: i + 2 }; }
export function spreadEnd(extra, i) { return { id: i, name: "n" + i, ...extra }; }
export function spreadStart(extra, i) { return { ...extra, id: i, name: "n" + i }; }
export function accessors(i) { return { base: i, get twice() { return this.base * 2; }, set twice(v) { this.base = v / 2; }, plain: true }; }
const parentProto = { hello() { return "parent"; } };
export function methods(i) { const o = { n: i, hello() { return "child>" + super.hello(); }, add(v) { return this.n + v; }, *gen() { yield this.n; yield this.n + 1; }, async later() { return this.n; } }; Object.setPrototypeOf(o, parentProto); return o; }
export function throwing(i) { return { a: i, b: i % 500 === 499 ? (() => { throw new RangeError("mid" + i); })() : i, c: 3 }; }
export function reentrant(d) { return { depth: d, child: d > 0 ? reentrant(d - 1) : null, sibling: d > 1 ? reentrant(d - 2) : null }; }
export function withThis() { return { self: this, arrow: () => this, kind: typeof this }; }
export function defaults(o = { a: 1, b: 2 }, { a, b } = o) { return { sum: a + b, o }; }
export function* generated(n) { for (let i = 0; i < n; i++) yield { i, sq: i * i }; }
export async function awaited(i) { const v = await i; return { v, done: true }; }
export function viaEval(i) { return eval("({ e: i, f: i * 2, g: { h: i } })"); }
export const viaFunction = new Function("i", "return { fa: i, fb: [i], fc: { fd: i + 1 } };");
export const viaIndirect = (0, eval)("(function (i) { return { ia: i, ib: `x${i}` }; })");
export function grown(i) { const o = { a: i, b: 2 }; o.c = 3; delete o.a; o.d = i; return o; }
export function frozenish(i) { const o = { a: i, b: 2 }; Object.defineProperty(o, "b", { enumerable: false }); return Object.freeze(o); }
export class Holder { state = { count: 0, items: [] }; static config = { mode: "static", level: 3 }; bump() { this.state = { count: this.state.count + 1, items: this.state.items }; return this.state; } }
