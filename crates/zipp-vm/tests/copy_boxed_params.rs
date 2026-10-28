//! Parameters captured by function and arrow expressions are copied into each
//! closure's own cell when nothing writes them (`capture::copy_box_names`),
//! and boxed at entry as before when anything could: an assignment or update
//! in the body or a closure, a destructuring or for-in/of target, a
//! redeclaration, a sloppy `arguments` alias, a direct `eval`, a `with`, a
//! capture by a declaration, a method or a class, a default parameter. Closures
//! made in a loop, nested, kept past the call, and a recursive callback. Run
//! interpreted, at the default thresholds, with every function compiled at
//! once, under GC stress, with the copy off and with lazy compilation off; the
//! expected lines are Node 24's.

const EXPECTED: &str = r####"mapAdd 11,12,13
later 0,7,14
nested 104,105
keeps onetwo
assigns 5
closureWrites 9
updates 2
destructures yx
objDestructures new
forOf 5
forIn q
redeclares 3
hoisted function
sloppyArgs 42
viaEval evald
viaWith with
byDecl decl
byMethod method
byClass class
evaluate 181818
defaults def
shadowed inner
strictArgs 1"####;

#[test]
fn copy_boxed_params_child() {
    if std::env::var_os("ZIPP_COPY_BOX_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn copied_params_match_node() {
    if std::env::var_os("ZIPP_COPY_BOX_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("copy-off", Some(("ZIPP_NO_COPY_BOX", "1"))),
        ("eager", Some(("ZIPP_LAZY", "0"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "copy_boxed_params_child", "--nocapture"])
            .env("ZIPP_COPY_BOX_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Parameters captured by function and arrow expressions, copied into each
// closure when nothing writes them, and every way something could: an
// assignment or update in the body or in a closure, a destructuring or
// for-in/of target, a redeclaration (`var`, a function declaration), a
// sloppy `arguments` alias, a direct `eval`, a `with`. Closures made in a
// loop, nested closures, closures that outlive the call, a capture by a
// declaration or an object method, and a recursive callback.
const out = [];
function mapAdd(xs, k) { return xs.map((x) => x + k); }
out.push("mapAdd " + mapAdd([1, 2, 3], 10).join(","));
function later(v) { const fs = []; for (let i = 0; i < 3; i++) fs.push(() => v * i); return fs.map((f) => f()).join(","); }
out.push("later " + later(7));
function nested(a) { return [1, 2].map((x) => [3].map((y) => x + y + a)[0]); }
out.push("nested " + nested(100).join(","));
function keeps(p) { return function () { return p; }; }
const k1 = keeps("one"), k2 = keeps("two");
out.push("keeps " + k1() + k2());
function assigns(a) { const f = () => a; a = 5; return f(); }
out.push("assigns " + assigns(1));
function closureWrites(a) { const set = () => { a = 9; }; const get = () => a; set(); return get(); }
out.push("closureWrites " + closureWrites(1));
function updates(a) { const g = () => a; a++; return g(); }
out.push("updates " + updates(1));
function destructures(a, b) { const g = () => a + b; [a, b] = [b, a]; return g(); }
out.push("destructures " + destructures("x", "y"));
function objDestructures(a) { const g = () => a; ({ a } = { a: "new" }); return g(); }
out.push("objDestructures " + objDestructures("old"));
function forOf(a) { const g = () => a; for (a of [4, 5]); return g(); }
out.push("forOf " + forOf(1));
function forIn(a) { const g = () => a; for (a in { q: 1 }); return g(); }
out.push("forIn " + forIn(1));
function redeclares(a) { const g = () => a; var a = 3; return g(); }
out.push("redeclares " + redeclares(1));
function hoisted(a) { const g = () => typeof a; function a() {} return g(); }
out.push("hoisted " + hoisted(1));
function sloppyArgs(a) { const g = () => a; arguments[0] = 42; return g(); }
out.push("sloppyArgs " + sloppyArgs(1));
function viaEval(a) { const g = () => a; eval("a = 'evald'"); return g(); }
out.push("viaEval " + viaEval(1));
function viaWith(a) { const g = () => a; with ({}) { a = "with"; } return g(); }
out.push("viaWith " + viaWith(1));
function byDecl(a) { function inner() { return a; } return inner(); }
out.push("byDecl " + byDecl("decl"));
function byMethod(a) { const o = { m() { return a; } }; return o.m(); }
out.push("byMethod " + byMethod("method"));
function byClass(a) { class C { get v() { return a; } } return new C().v; }
out.push("byClass " + byClass("class"));
function evaluate(e, env) {
  if (typeof e === "number") return e;
  if (typeof e === "string") return env.get(e);
  return Math.max(...e.map((x) => evaluate(x, env)));
}
const env = new Map([["x", 3], ["y", 9]]);
let t = 0;
for (let i = 0; i < 20000; i++) t += evaluate([1, "x", [i % 7, "y", [2, i % 11]]], env);
out.push("evaluate " + t);
function defaults(a, b = () => a) { return b(); }
out.push("defaults " + defaults("def"));
function shadowed(a) { { let a = "inner"; return (() => a)(); } }
out.push("shadowed " + shadowed("outer"));
function strictArgs(a) { "use strict"; const g = () => a; arguments[0] = 42; return g(); }
out.push("strictArgs " + strictArgs(1));
console.log(out.join("\n"));
"####;
