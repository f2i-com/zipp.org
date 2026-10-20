//! `$262.evalScript` returns the script's completion value, as
//! ScriptEvaluation does: the value of the last expression statement that
//! produced one, with the statement rules eval uses (an `if` or loop that
//! completes empty, `break`, `try`/`finally`, a directive). It returned
//! undefined for every script. Expected output is Node's (v24,
//! `vm.runInThisContext` standing in for `$262.evalScript`).

#[test]
fn eval_script_returns_the_completion_value() {
    let src = r###"if (typeof $262 === "undefined") globalThis.$262 = { evalScript: (s) => require("vm").runInThisContext(s) };
const out = [];
const show = (v) => (typeof v === "string" ? JSON.stringify(v) : typeof v === "object" && v !== null ? JSON.stringify(v) : String(v));
const t = (src) => { let r; try { r = show($262.evalScript(src)); } catch (e) { r = "threw " + e.name; } out.push(JSON.stringify(src) + " -> " + r); };
t("1 + 1");
t("1; var x;");
t("var y = 5;");
t("3; function f() {}");
t("let z = 1; z + 1");
t("const q = 4;");
t("if (true) { 'a' } else { 'b' }");
t("if (false) { 'a' }");
t("for (var i = 0; i < 3; i++) { i * 10; if (i == 1) break; }");
t("var n = 0; while (n < 3) { n++; }");
t("do { 7 } while (false)");
t("try { 'try' } finally { 'fin' }");
t("try { throw 1 } catch (e) { 'caught' }");
t("'use strict'");
t("'use strict'; 9");
t("switch (2) { case 2: 'two' }");
t("label: { 'in'; break label; }");
t("({ a: 1 })");
t("[1, 2, 3].map(v => v * 2)");
t("class K {}");
t("5; class K2 {}");
t("throw new TypeError('x')");
t("8; { }");
t("'s' + 'u' + 'm'");
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###""1 + 1" -> 2
"1; var x;" -> 1
"var y = 5;" -> undefined
"3; function f() {}" -> 3
"let z = 1; z + 1" -> 2
"const q = 4;" -> undefined
"if (true) { 'a' } else { 'b' }" -> "a"
"if (false) { 'a' }" -> undefined
"for (var i = 0; i < 3; i++) { i * 10; if (i == 1) break; }" -> undefined
"var n = 0; while (n < 3) { n++; }" -> 2
"do { 7 } while (false)" -> 7
"try { 'try' } finally { 'fin' }" -> "try"
"try { throw 1 } catch (e) { 'caught' }" -> "caught"
"'use strict'" -> "use strict"
"'use strict'; 9" -> 9
"switch (2) { case 2: 'two' }" -> "two"
"label: { 'in'; break label; }" -> "in"
"({ a: 1 })" -> {"a":1}
"[1, 2, 3].map(v => v * 2)" -> [2,4,6]
"class K {}" -> undefined
"5; class K2 {}" -> 5
"throw new TypeError('x')" -> threw TypeError
"8; { }" -> 8
"'s' + 'u' + 'm'" -> "sum""###);
}
