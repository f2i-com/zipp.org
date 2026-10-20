//! A top-level lexical destructuring declaration in a script run with script
//! semantics (`$262.evalScript`, ScriptEvaluation) binds its names: each leaf
//! is initialized by the extraction, once, as a simple `const x = 1` is. Its
//! leaves used to be set to `undefined` first, so the extraction was a
//! `const`'s second write and threw "Assignment to constant variable"
//! (f2i-com/zipp.org#75), and a leaf whose extraction never ran had left its
//! dead zone anyway. The expected lines are Node's (v24, `vm.runInThisContext`
//! standing in for `$262.evalScript`).

const PROBE: &str = r#"
if (typeof $262 === "undefined") globalThis.$262 = { evalScript: (s) => require("vm").runInThisContext(s) };
const out = [];
const t = (label, f) => { try { out.push(label + ": " + f()); } catch (e) { out.push(label + ": " + e.name); } };
const { mp1, mp2: [mp3, ...mp4] } = { mp1: 1, mp2: [2, 3, 4] };
t("main program", () => JSON.stringify([mp1, mp3, mp4]));
t("main program const stays const", () => { mp1 = 5; return mp1; });
t("script const object and array", () => {
  $262.evalScript("const {a, b = a + 1, c: {d}} = {a: 1, c: {d: 4}}; const [x, y] = [5, 6];");
  return JSON.stringify([a, b, d, x, y]);
});
t("script const stays const", () => { a = 9; return a; });
t("a later script reads them", () => { $262.evalScript("var later = a + b + d + x + y;"); return later; });
t("script let leaves", () => {
  $262.evalScript("let {p, q: [r, ...s]} = {p: 1, q: [2, 3, 4]};");
  p = 10;
  return JSON.stringify([p, r, s]);
});
t("strict script const", () => { $262.evalScript('"use strict"; const {m, n = m * 2} = {m: 3};'); return m + n; });
t("script const from an iterator", () => {
  $262.evalScript("const [g1, g2 = g1 * 10] = (function* () { yield 4; })();");
  return g1 + g2;
});
t("partial initialization", () => {
  try {
    $262.evalScript('const {u, v = (() => { throw new Error("boom"); })()} = {u: 7};');
  } catch (e) {
    out.push("  threw " + e.message);
  }
  return u;
});
t("the leaf never initialized stays in its dead zone", () => v);
t("a default reading a later leaf", () => $262.evalScript("let {e1 = e2, e2} = {}; e1"));
t("which leaves both uninitialized", () => { try { return e1; } catch (e) { return e.name + " " + (() => { try { return e2; } catch (f) { return f.name; } })(); } });
console.log(out.join("\n"));
"#;

const EXPECTED: &str = "main program: [1,2,[3,4]]
main program const stays const: TypeError
script const object and array: [1,2,4,5,6]
script const stays const: TypeError
a later script reads them: 18
script let leaves: [10,2,[3,4]]
strict script const: 9
script const from an iterator: 44
  threw boom
partial initialization: 7
the leaf never initialized stays in its dead zone: ReferenceError
a default reading a later leaf: ReferenceError
which leaves both uninitialized: ReferenceError ReferenceError";

#[test]
fn script_lexical_destructuring_initializes_each_leaf_once() {
    let out = zipp_vm::run(PROBE).expect("probe compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}; output: {:?}",
        out.error,
        out.output
    );
    assert_eq!(out.output.join("\n"), EXPECTED);
}
