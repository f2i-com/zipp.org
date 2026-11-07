//! Lazy brackets holding lazy brackets: a return resuming at an inner
//! `finally`'s end goes on to the outer one, and a throw or a bail from any
//! depth finds every handler the interpreter would hold. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"9165 4 5 6004
1111 in1010 fin11011 mid1011 fin21111 1111,1010,1011,1011,1111
NaN 5 NaN 18
29990 -1 20 30 4
1f12f2f3 a f1f2f3 1f1 1f1f2f3 1f12f2 1f12f2f3 1f12f2f3 1f12f2f3
e2999 e4 112 112 -1 -1
1 RangeError TypeError TypeError
21162 zNaN 27 zNaN
1111 fin1 fin1
3110 NaN 109 109
2999 -1 -1
top fin1 5 1011
top2 NaN
number true"####;

const PROGRAM: &str = r####"// Nested lazy brackets: every way out of an inner and an outer body.
const HOT = 3000;
var fin = 0, log = [];
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function t(m) { throw new Error(m); }
// The bench's nested shape: a return captured before both finallys run.
function n1(x) { let s = x; try { try { s = (s + 1) | 0; if (s & 4) return s; } finally { s ^= 0x55; fin++; } s = (s + 2) | 0; } finally { s = (s * 3) | 0; fin++; } return s; }
console.log(hot(n1), n1(3), n1(4), fin);
// Throws from the inner body, the inner finally, between, and the outer finally, after warm-up.
function n2(x, k) { var s = 0; try { try { if (k === 1) t("in"); s = 1; } finally { s += 10; if (k === 2) t("fin1"); } if (k === 3) t("mid"); s += 100; } finally { s += 1000; log.push(s); if (k === 4) t("fin2"); } return s; }
function c2(k) { try { return n2(1, k); } catch (e) { return e.message + log[log.length - 1]; } }
hot(() => c2(0)); log = [];
console.log(c2(0), c2(1), c2(2), c2(3), c2(4), log.join());
// A guard bail mid-body: a different type after warm-up.
function n3(o) { var s = 0; try { try { s = o.a + o.b; } finally { s = s * 2; } s = s + 1; } finally { s = s - 3; } return s; }
hot((i) => n3({ a: i, b: 1 }));
console.log(n3({ a: "x", b: 2 }), n3({ a: 1.5, b: 2 }), n3({ b: 1 }), n3({ a: { valueOf() { return 9; } }, b: 1 }));
// Overrides: an inner finally's return, an outer finally's return, both.
function n4(x) { try { try { return x; } finally { if (x & 1) return -x; } } finally { if (x & 2) return x * 10; } }
console.log(hot(n4), n4(1), n4(2), n4(3), n4(4));
// Three deep, and a return from each level.
function n5(x) { var s = ""; try { try { try { if (x === 0) return "a"; s += "1"; } finally { s += "f1"; } if (x === 1) return s; s += "2"; } finally { s += "f2"; } if (x === 2) return s; } finally { s += "f3"; log = [s]; } return s; }
console.log(hot((i) => n5(i % 4)), n5(0), log.join(), n5(1), log.join(), n5(2), log.join(), n5(3), log.join());
// A catch around a lazy finally, and a finally around a catch.
function n6(x) { try { try { if (x % 5 === 4) t("e" + x); return x; } finally { fin++; } } catch (e) { return e.message; } }
function n7(x) { var s = 0; try { try { if (x % 5 === 4) t("q"); s = 1; } catch (e) { s = 2; } s += 10; } finally { s += 100; } return s; }
function n8(x) { try { try { if (x % 5 === 4) t("q"); } catch (e) { return -1; } return x; } finally { fin++; } }
console.log(hot(n6), n6(4), hot(n7), n7(4), hot(n8), n8(4));
// A catch's body that returns (no finally around it), and a throw from a callee in it.
function n9(o) { try { return o.f(); } catch (e) { return e.name; } }
var okf = { f() { return 1; } }, badf = { f() { throw new RangeError("r"); } };
console.log(hot(() => n9(okf)), n9(badf), n9({}), n9(null));
// Called from inside a caller's try/finally and try/catch: interpreted and compiled callers.
function r1(x) { try { if (x & 1) return x * 3; } finally { fin++; } return x; }
function r2(x) { try { return x; } finally { if ((x & 7) === 0) return -x; } }
function outer1(x) { var s = 0; try { s = r1(x) + r2(x) + n1(x); } finally { s += 1; } return s; }
function outer2(x) { try { if (x === HOT - 1) return r1("z") + n3({ a: "q", b: 1 }); return r1(x) + r2(x); } catch (e) { return -9; } finally { fin++; } }
console.log(hot(outer1), hot(outer2), outer1(5), outer2(HOT - 1));
function outer3(x) { try { return n2(x, x % 7 === 6 ? 2 : 0); } catch (e) { return e.message; } }
console.log(hot(outer3), outer3(6), outer3(13));
// The exit stub's pushes, seen by a throw past a nested bracket's catch.
function n10(x) { var s = 0; try { try { try { s = x.y.z; } finally { s += 1; } } catch (e) { s = -1; } s += 10; } finally { s += 100; } return s; }
console.log(hot((i) => n10({ y: { z: i } })), n10({ y: {} }), n10({}), n10(null));
// A catch nested in a lazy finally's body whose own body returns into it.
function n11(x) { var s = 0; try { try { try { if (x % 9 === 8) t("z"); return x; } finally { s = 1; } } catch (e) { return -s; } } finally { fin++; } }
console.log(hot(n11), n11(8), n11(17));
// Compiled callees throwing out of their nested brackets into a top-level try.
var top = 0;
try { try { top = n2(1, 2); } finally { top += 5; } } catch (e) { console.log("top", e.message, top, log[log.length - 1]); }
try { top = n10(null) + n3({ a: "s", b: 1 }); } finally { console.log("top2", top); }
console.log(typeof fin, fin > 0);
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
