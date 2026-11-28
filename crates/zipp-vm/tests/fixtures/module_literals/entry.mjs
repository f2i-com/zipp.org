import * as L from "./lib.mjs";
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 2000;
const j = JSON.stringify;
function hot(fn) { let last; for (let i = 0; i < N; i++) last = fn(i); return last; }
function err(fn) { try { fn(); return "no error"; } catch (e) { return e.constructor.name + ":" + e.message; } }
log("point", j(hot(L.point)), Object.keys(L.point(1)).join(""));
log("record", hot((i) => { const r = L.record(i, null); const { node, parent } = r; return node + ":" + parent + ":" + Object.keys(r); }));
log("shorthand", j(hot((i) => L.shorthand(i, 2))), Object.keys(L.shorthand(1, 2)).join("|"));
log("nested", hot((i) => { const o = L.nested(i); return j(o) + o.fn(); }));
log("wide", hot((i) => { const o = L.wide(i); return o.k0 + o.k15 + Object.keys(o).length; }), hot((i) => { const o = L.wider(i); return o.k0 + o.k17 + Object.keys(o).length; }));
log("duplicate", j(hot(L.duplicate)));
const proto = { inherited: "yes" };
log("proto", hot((i) => { const o = L.withProto(proto, i); return o.own + o.inherited + Object.keys(o) + (Object.getPrototypeOf(o) === proto); }), j(L.withProto(null, 1)), Object.getPrototypeOf(L.withProto(5, 1)) === Object.prototype);
log("computed", j(hot((i) => L.computed(i % 3 === 0 ? "first" : i % 3 === 1 ? "mid" : "last", i))), j(L.computed("first", 1)), j(L.computed(Symbol.iterator, 1)));
log("spread", j(hot((i) => L.spreadEnd({ id: "over", extra: i }, i))), j(hot((i) => L.spreadStart({ id: "under", extra: i }, i))), j(L.spreadEnd(null, 1)));
log("accessors", hot((i) => { const o = L.accessors(i); const before = o.twice; o.twice = 10; return before + ":" + o.base + ":" + Object.keys(o) + ":" + typeof Object.getOwnPropertyDescriptor(o, "twice").get; }));
log("methods", hot((i) => { const o = L.methods(i); return o.hello() + o.add(1) + [...o.gen()] + Object.keys(o); }));
let caught = ""; hot((i) => { try { return L.throwing(i).b; } catch (e) { caught += e.message + ";"; } });
log("throwing", caught);
log("reentrant", j(hot((i) => L.reentrant(i % 4))));
log("this", hot(() => { const o = L.withThis.call(proto); return (o.self === proto) + ":" + (o.arrow() === proto) + ":" + o.kind + ":" + L.withThis().kind; }));
log("defaults", j(hot(() => L.defaults())), j(L.defaults({ a: 5, b: 6 })), j(L.defaults(undefined, { a: 7, b: 8 })));
log("generated", j([...L.generated(4)]), hot(() => { let s = 0; for (const o of L.generated(5)) s += o.sq; return s; }));
log("eval", j(hot(L.viaEval)), j(hot(L.viaFunction)), j(hot(L.viaIndirect)));
log("grown", j(hot(L.grown)), Object.keys(L.grown(1)).join(""));
log("frozen", hot((i) => { const o = L.frozenish(i); return j(o) + Object.isFrozen(o) + Object.keys(o) + err(() => { "use strict"; o.a = 1; }).split(":")[0]; }));
log("descriptor", j(Object.getOwnPropertyDescriptor(L.point(3), "x")), j(Object.getOwnPropertyDescriptor(L.shorthand(1, 2), "7")), j(Object.entries(L.shorthand(1, 2))));
log("class", hot(() => { const h = new L.Holder(); h.bump(); h.bump(); return j(h.state) + j(L.Holder.config); }));
// Kept across collections, and distinct objects every time.
const keep = []; hot((i) => { const p = L.point(i); if (i % 250 === 0) keep.push(p, L.nested(i), L.record(p, keep)); return p; });
log("kept", keep.length, keep.filter((o) => "x" in o).map((o) => o.x).join(","), keep[1].pos.y, keep[2].node === keep[0], L.point(1) !== L.point(1));
const v = await L.awaited(Promise.resolve(41));
log("awaited", j(v), j(await Promise.all([L.awaited(1), L.awaited(2)])));
console.log(out.join("\n"));
