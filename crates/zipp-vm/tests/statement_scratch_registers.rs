//! A statement's scratch registers die with it (GitHub #56). Every statement
//! used to keep the scratch registers it evaluated into until its scope ended,
//! and `pop_scope` subtracts only the scope's own locals, so a flat body of
//! about 33,000 statements failed with "function needs more than 32768
//! registers". Bundlers that flatten every module into one function (esbuild's
//! IIFE output, for one) produce bodies that size.
//!
//! The second test is the other half of the contract: the reclaim must never
//! free a register that still holds a binding, so declarations, closures,
//! `with`, Annex B function hoisting and eval completion values interleaved
//! with ordinary statements keep their values.

const MANY: &str = r#"
var out = [];
var kinds = [
  "a;", "1;", "a = a + 1;", "f();", "x.y;", "a < 1;", "a ? 1 : 2;", "`t${a}`;",
  "[a, 1];", "({ a });", "a, f();", "if (a) {}", "if (a) a; else f();",
  "while (0) {}", "do {} while (0);", "switch (a) {}", "for (;0;) {}", "{ a; }",
  "label: a;", "try {} catch {}"
];
kinds.forEach(function (stmt) {
  var body = "var a = 0, x = { y: 1 }, n = 0; function f() { n++; }" + stmt.repeat(40000) + "return a + ',' + n;";
  try { out.push(JSON.stringify(stmt) + " " + new Function(body)()); }
  catch (e) { out.push(JSON.stringify(stmt) + " " + e.message); }
});
console.log(out.join("\n"));
"#;

#[test]
fn flat_bodies_of_many_statements_compile() {
    let out = zipp_vm::run(MANY).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let text = out.output.join("\n");
    let expected = [
        r#""a;" 0,0"#,
        r#""1;" 0,0"#,
        r#""a = a + 1;" 40000,0"#,
        r#""f();" 0,40000"#,
        r#""x.y;" 0,0"#,
        r#""a < 1;" 0,0"#,
        r#""a ? 1 : 2;" 0,0"#,
        r#""`t${a}`;" 0,0"#,
        r#""[a, 1];" 0,0"#,
        r#""({ a });" 0,0"#,
        r#""a, f();" 0,40000"#,
        r#""if (a) {}" 0,0"#,
        r#""if (a) a; else f();" 0,40000"#,
        r#""while (0) {}" 0,0"#,
        r#""do {} while (0);" 0,0"#,
        r#""switch (a) {}" 0,0"#,
        r#""for (;0;) {}" 0,0"#,
        r#""{ a; }" 0,0"#,
        r#""label: a;" 0,0"#,
        r#""try {} catch {}" 0,0"#,
    ];
    assert_eq!(text.lines().collect::<Vec<_>>(), expected);
}

const BINDINGS: &str = r#"
function g() {
  var acc = [];
  { let p = 1; acc.push(p); p++; acc.push(p); }
  let q = 10; q++; acc.push(q);
  const h = () => q + 1; acc.push(h());
  for (let i = 0; i < 3; i++) { let j = i * 2; acc.push(j); }
  if (true) { function hoisted() { return 5; } }
  acc.push(hoisted());
  try { throw 6; } catch (e) { acc.push(e); }
  class K { m() { return 7; } }
  acc.push(new K().m());
  label: { acc.push(8); break label; }
  switch (q) { case 11: let z = 9; acc.push(z); }
  with ({ w: 10 }) { acc.push(w); }
  var later = [1, 2, 3].map(function (v) { return v + q; });
  acc.push(later.join("+"));
  let r = 0; for (const v of [1, 2, 3]) { r += v; } acc.push(r);
  acc.push(eval("1; 2; if (true) 3;"), eval("var e1 = 4; e1 + 1;"), eval("x1: { 12; break x1; }"));
  acc.push(q, h(), typeof p);
  return acc.join(",");
}
console.log(g());
function* gen() { var s = 0; s += 1; yield s; let t = s + 1; t; yield t; }
console.log([...gen()].join(","));
"#;

#[test]
fn bindings_survive_the_reclaim() {
    let out = zipp_vm::run(BINDINGS).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(
        out.output.join("\n"),
        "1,2,11,12,0,2,4,5,6,7,8,9,10,12+13+14,6,3,5,12,11,12,undefined\n1,2"
    );
}
