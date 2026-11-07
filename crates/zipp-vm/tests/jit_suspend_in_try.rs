//! A suspension inside a `try` body in hot generators and async functions: the
//! handler is gone once the body ends. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"after
4
1
9 3000
3 6000
3RangeError 15000
3
3001
after
3
rej"####;

const PROGRAM: &str = r####"// A suspension inside a `try` body, in hot generators and async functions: the handler
// must be gone once the body ends, however many times it suspended.
const HOT = 3000;
// A throw after the body must not land in its catch.
function* a() { try { yield 1; } catch (e) { yield "caught"; } throw new Error("after"); }
function runA() { var g = a(); g.next(); try { var r = g.next(); return r.value; } catch (e) { return e.message; } }
var r; for (var i = 0; i < HOT; i++) r = runA(); console.log(r);
function* b(n) { var s = 0; for (var i = 0; i < n; i++) { try { s += yield i; } catch (e) { s = -1; } } if (n > 100) throw 1; return s; }
function runB() { var g = b(4), v = g.next(); while (!v.done) v = g.next(1); return v.value; }
for (var i = 0; i < HOT; i++) r = runB(); console.log(r);
// A throw while suspended lands in the catch; `return` runs the finally.
function runC() { var g = b(2); g.next(); var v = g.throw(new Error("in")); return v.value; }
for (var i = 0; i < HOT; i++) r = runC(); console.log(r);
var fin = 0;
function* c() { try { yield 1; yield 2; } finally { fin++; } return 3; }
function runD() { var g = c(); g.next(); var v = g.return(9); return v.value; }
for (var i = 0; i < HOT; i++) r = runD(); console.log(r, fin);
function runE() { var s = 0; for (var v of c()) s += v; return s; }
for (var i = 0; i < HOT; i++) r = runE(); console.log(r, fin);
function* d() { for (var i = 0; i < 3; i++) { try { yield i; } finally { fin++; } } throw new RangeError("end"); }
function runF() { var s = 0; try { for (var v of d()) s += v; } catch (e) { return s + e.name; } return s; }
for (var i = 0; i < HOT; i++) r = runF(); console.log(r, fin);
// Nested generators, yield*, and a try with no suspension in a generator.
function* e() { try { yield* [1, 2]; } catch (x) { yield "no"; } }
function runG() { var s = 0; for (var v of e()) s += v; return s; }
for (var i = 0; i < HOT; i++) r = runG(); console.log(r);
function* f(x) { var s = 0; try { s = x + 1; } catch (e) { s = -1; } yield s; try { s = s + 1; } catch (e) { s = -2; } return s; }
function runH(i) { var g = f(i); g.next(); return g.next().value; }
for (var i = 0; i < HOT; i++) r = runH(i); console.log(r);
// Async: after the await the handler is gone too.
async function g1() { try { await null; } catch (e) { return "caught"; } throw new Error("after"); }
async function g2(n) { var s = 0; for (var i = 0; i < n; i++) { try { s += await i; } catch (e) { s = -1; } } return s; }
async function g3() { try { await Promise.reject(new Error("rej")); } catch (e) { return e.message; } return "no"; }
async function main() {
  var r = "";
  for (var i = 0; i < 1500; i++) { try { await g1(); } catch (e) { r = e.message; } }
  console.log(r);
  for (var i = 0; i < 1500; i++) r = await g2(3);
  console.log(r);
  for (var i = 0; i < 1500; i++) r = await g3();
  console.log(r);
}
main();
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
