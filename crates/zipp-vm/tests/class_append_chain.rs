//! Appends to a class instance through its `AddClass` cache entry, for
//! classes several `extends` deep: constructing an instance materializes
//! every level's prototype, so the chain the entry guards is long. A setter
//! or read-only property added to any of those prototypes (or to
//! `%Object.prototype%`) after the entry filled must govern the write, as
//! must a class setter up the chain and a swapped-out prototype. Expected
//! output is node's.

#[test]
fn derived_class_appends_respect_their_whole_prototype_chain() {
    let src = r###""use strict";
const out = [];
const t = (f) => { try { return String(f()); } catch (e) { return e.constructor.name; } };
class A { constructor() { this.a = 1; } }
class B extends A { constructor() { super(); this.b = 2; } }
class C extends B { constructor() { super(); this.c = 3; } }
class D extends C { constructor(v) { super(); this.d = v; this.x = v * 2; } }
const mk = (n) => { const r = []; for (let i = 0; i < n; i++) r.push(new D(i)); return r; };
let xs = mk(50);
out.push("1 " + JSON.stringify(xs[7]) + " " + Object.keys(xs[49]).join(","));
// a setter appears on an intermediate prototype after the cache filled
let seen = 0;
Object.defineProperty(B.prototype, "x", { set(v) { seen += v; }, get() { return "getter"; }, configurable: true });
xs = mk(20);
out.push("2 " + seen + " " + xs[5].x + " " + Object.keys(xs[5]).join(","));
delete B.prototype.x;
xs = mk(10);
out.push("3 " + xs[3].x + " " + Object.keys(xs[3]).join(","));
// a read-only property on the farthest prototype: a strict-mode TypeError
Object.defineProperty(A.prototype, "d", { value: 0, writable: false, configurable: true });
out.push("4 " + t(() => new D(5).d));
delete A.prototype.d;
out.push("5 " + new D(6).d);
// Object.prototype gets a setter
Object.defineProperty(Object.prototype, "c", { set(v) { seen += 1000; }, configurable: true });
const e = new D(1);
out.push("6 " + seen + " " + Object.prototype.hasOwnProperty.call(e, "c"));
delete Object.prototype.c;
// a class setter declared up the chain, and a prototype swapped out
class S { set y(v) { this._y = v * 10; } }
class S2 extends S { constructor() { super(); this.y = 4; } }
class S3 extends S2 { constructor() { super(); this.z = 1; } }
let s3; for (let i = 0; i < 30; i++) s3 = new S3();
out.push("7 " + JSON.stringify(s3) + " " + ("y" in s3) + " " + Object.prototype.hasOwnProperty.call(s3, "y"));
Object.setPrototypeOf(S2.prototype, Object.prototype);
const s4 = new S3();
out.push("8 " + JSON.stringify(s4) + " " + Object.prototype.hasOwnProperty.call(s4, "y"));
// a frozen prototype in the chain
Object.freeze(C.prototype);
out.push("9 " + JSON.stringify(new D(9)));
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"1 {"a":1,"b":2,"c":3,"d":7,"x":14} a,b,c,d,x
2 380 getter a,b,c,d
3 6 a,b,c,d,x
4 TypeError
5 6
6 1380 false
7 {"_y":40,"z":1} true false
8 {"y":4,"z":1} true
9 {"a":1,"b":2,"c":3,"d":9,"x":18}"###);
}
