//! A class declaration is lexically scoped to its block, against Node.
//!
//! `class_decl` bound to whatever binding of the name it found first, outer
//! scopes and globals included, so `class C {} { class C {} }` replaced the
//! outer `C` and `const C = 1; { class C {} }` overwrote a const; a class in
//! a script-level block became a global. It now binds only in the current
//! scope, as `let` does (kangax compat-table "class is block-scoped"). The
//! probe covers function and script level, `const`/`let` outer bindings,
//! nested blocks, a switch case, a loop body, closures over the inner and
//! outer binding, the block's TDZ, a named function expression's own name,
//! direct eval and a heritage. The expected lines are Node's (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
const kind = (v) => (typeof v === "function" ? "class " + v.name : JSON.stringify(v));

function fnLevel() { class C {} const c1 = C; { class C {} } return C === c1; }
function fnLevelUsed() { class C {} const c1 = C; let inner; { class C {} inner = C; } return [C === c1, inner !== c1, inner.name].join(); }
function constOuter() { const C = 1; { class C {} } return kind(C); }
function letOuter() { let C = "x"; { class C {} } C = "y"; return kind(C); }
function nested() { class C { m() { return 1; } } { class C { m() { return 2; } } { class C { m() { return 3; } } } } return new C().m(); }
function inSwitch(k) { const C = "outer"; switch (k) { case 1: class C {} return kind(C); default: try { return kind(C); } catch (e) { return e.constructor.name; } } }
function afterSwitch() { const C = "outer"; switch (1) { case 1: class C {} } return kind(C); }
function inLoop() { const C = "outer"; const seen = []; for (let i = 0; i < 3; i++) { class C {} seen.push(C); } return [kind(C), seen[0] !== seen[1], seen.length].join(); }
function captured() { const C = "outer"; let get; { class C {} get = () => C; } return [kind(C), kind(get())].join(); }
function tdz() { const C = "outer"; try { { C; class C {} } return "no tdz"; } catch (e) { return e.constructor.name; } }
function closureBefore() { const C = "outer"; { const f = () => C; class C {} return [kind(f()), kind(C)].join(); } }
const fe = function C() { { class C {} } return C === fe; };
function viaEval() { const C = "outer"; const r = eval("{ class C {} } typeof C"); return [r, kind(C)].join(); }
function heritage() { class B { who() { return "B"; } } { class B extends Object {} } return new B().who(); }

log("fn-level", fnLevel());
log("fn-level-used", fnLevelUsed());
log("const-outer", constOuter());
log("let-outer", letOuter());
log("nested", nested());
log("switch", inSwitch(1), inSwitch(2), afterSwitch());
log("loop", inLoop());
log("captured", captured());
log("tdz", tdz());
log("closure-before", closureBefore());
log("fn-expr-name", fe());
log("eval", viaEval());
log("heritage", heritage());
class TopC {}
const topC = TopC;
{ class TopC {} }
{ class OnlyInBlock {} }
log("script-block", TopC === topC, typeof OnlyInBlock);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"fn-level true
fn-level-used true,true,C
const-outer 1
let-outer "y"
nested 1
switch class C ReferenceError "outer"
loop "outer",true,3
captured "outer",class C
tdz ReferenceError
closure-before class C,class C
fn-expr-name true
eval string,"outer"
heritage B
script-block true undefined"###;

fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn class_declarations_are_block_scoped_like_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
