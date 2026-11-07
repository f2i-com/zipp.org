//! `arguments.length` in a function that uses `arguments` no other way reads the
//! count the call stored instead of an object, and any other use of the name,
//! any rebinding and any shadowing keeps what the language says. Node's output.

const EXPECTED: &str = r####"0 1 3
0 6 8
2
200000
5000050000
7
[ 2, 7 ]
12
1object
[ 2, undefined ]
2
[ 1, 99 ]
[ 1, '[object Arguments]' ]
[ 2, 4, 5 ]
3 2
2
3
4
5
[ 3, 2 ]
function
5
2
4
3
6
[ 1, 3 ]
[ 0, 0 ] [ 2, 2 ] [ 9, 1 ]
[ 3, 2 ]
2 1 3
2 0 1
2
3
2 3 3
[ 3, 3 ]
3
10
2
2
2
2
[ 1, 2 ]
[ 1, 3 ]
[ 1, 3 ]
[ 1, 4 ]
[ 1, undefined ]
[ 1, 19 ]
5
3
2
4
6
5
2
3
200109998
100000
3"####;

#[test]
fn arguments_length_only_child() {
    if std::env::var_os("ZIPP_ARGUMENTS_LENGTH_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn arguments_length_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_ARGUMENTS_LENGTH_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("jit", None),
        ("nojit", Some(("ZIPP_NOJIT", "1"))),
        ("forced", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gcstress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "arguments_length_only_child", "--nocapture"])
            .env("ZIPP_ARGUMENTS_LENGTH_CHILD", "1")
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

const PROGRAM: &str = r####"// arguments.length in every shape the compiler may treat alone or not.
function count() { return arguments.length; }
console.log(count(), count(1), count(1, 2, 3));
function withParams(a, b) { return arguments.length + (a | 0); }
console.log(withParams(), withParams(5), withParams(5, 6, 7));
function strictCount() { "use strict"; return arguments.length; }
console.log(strictCount(1, 2));
function hot(n) { var s = 0; for (var i = 0; i < n; i++) s += count(i, i); return s; }
console.log(hot(100000));
function hotA(n) { var s = 0; for (var i = 0; i < n; i++) s += withParams(i); return s; }
console.log(hotA(100000));

// Another use: the real object, its length a program may change.
function setLen() { arguments.length = 7; return arguments.length; }
console.log(setLen(1, 2));
function setLenLater() { var a = arguments.length; arguments.length = 7; return [a, arguments.length]; }
console.log(setLenLater(1, 2));
function readLater() { var a = arguments.length; return a + arguments[0]; }
console.log(readLater(10, 20));
function typeofLater() { var a = arguments.length; return a + typeof arguments; }
console.log(typeofLater(1));
function del() { var a = arguments.length; delete arguments.length; return [a, arguments.length]; }
console.log(del(1, 2));
function inc() { arguments.length++; return arguments.length; }
console.log(inc(1));
function assign() { var a = arguments.length; arguments = { length: 99 }; return [a, arguments.length]; }
console.log(assign(1));
function escapes() { var a = arguments.length; return [a, Object.prototype.toString.call(arguments)]; }
console.log(escapes(1));
function spread() { return [arguments.length, ...arguments]; }
console.log(spread(4, 5));

// Shadowing.
function param(arguments) { return arguments.length; }
console.log(param("abc"), param([1, 2]));
function local() { var arguments = "xy"; return arguments.length; }
console.log(local(1, 2, 3));
function localBare() { var arguments; return arguments.length; }
console.log(localBare(1, 2, 3));
function letIn() { { let arguments = "wxyz"; return arguments.length; } }
console.log(letIn(1));
function fnDecl() { function arguments(a, b, c, d, e) {} return arguments.length; }
console.log(fnDecl(1));
function blockFn() { var before = arguments.length; { function arguments(a, b) {} } return [before, arguments.length]; }
console.log(blockFn(1, 2, 3));
function afterBlock() { { function arguments(a, b) {} } return typeof arguments; }
console.log(afterBlock(1));
function withObj() { with ({ arguments: "abcde" }) { return arguments.length; } }
console.log(withObj(1));

// Captures and eval: the real object.
function arrow() { return (() => arguments.length)(); }
console.log(arrow(1, 2));
function arrowMix() { var a = arguments.length; return a + (() => arguments.length)(); }
console.log(arrowMix(1, 2));
function ev() { return eval("arguments.length"); }
console.log(ev(1, 2, 3));
function evMix() { var a = arguments.length; return a + eval("arguments.length"); }
console.log(evMix(1, 2, 3));
function inner() { function g() { return arguments.length; } return [arguments.length, g(1, 2, 3)]; }
console.log(inner(1));

// Default parameters, rest, classes, methods, constructors, generators, async.
function dflt(a = arguments.length) { return [a, arguments.length]; }
console.log(dflt(), dflt(undefined, 2), dflt(9));
function rest(a, ...r) { return [arguments.length, r.length]; }
console.log(rest(1, 2, 3));
class C { constructor() { this.n = arguments.length; } m() { return arguments.length; } static s() { return arguments.length; } }
console.log(new C(1, 2).n, new C().m(1), C.s(1, 2, 3));
var o = { m() { return arguments.length; }, get g() { return arguments.length; }, set s(v) { this.v = arguments.length; } };
o.s = 1;
console.log(o.m(1, 2), o.g, o.v);
function* gen() { yield arguments.length; }
console.log(gen(1, 2).next().value);
async function asy() { return arguments.length; }
asy(1, 2, 3).then(console.log);
function tail(a) { return count(a, a, a); }
console.log(tail(1));
function viaCall() { return arguments.length; }
console.log(viaCall.call(null, 1, 2), viaCall.apply(null, [1, 2, 3]), viaCall.bind(null, 1)(2, 3));
console.log([1, 2].map(function () { return arguments.length; }));
function ctorLen() { this.n = arguments.length; }
console.log(new ctorLen(1, 2, 3).n);
function recur(n) { return n === 0 ? arguments.length : recur(n - 1, 1, 2) + arguments.length; }
console.log(recur(3));
function fnLength(a, b) { return arguments.length; }
console.log(fnLength.length);
function optional() { return arguments?.length; }
console.log(optional(1, 2));
function computed() { return arguments["length"]; }
console.log(computed(1, 2));
function destruct() { const { length } = arguments; return length; }
console.log(destruct(1, 2));

// Writes that reach the register other ways.
function viaForOf() { var a = arguments.length; for (arguments of ["ab"]); return [a, arguments.length]; }
console.log(viaForOf(1));
function viaForIn() { var a = arguments.length; for (arguments in { abc: 1 }); return [a, arguments.length]; }
console.log(viaForIn(1));
function viaArrayPattern() { var a = arguments.length; [arguments] = ["abc"]; return [a, arguments.length]; }
console.log(viaArrayPattern(1));
function viaObjectPattern() { var a = arguments.length; ({ q: arguments } = { q: "abcd" }); return [a, arguments.length]; }
console.log(viaObjectPattern(1));
function viaUpdate() { var a = arguments.length; arguments++; return [a, arguments.length]; }
console.log(viaUpdate(1));
function viaCompound() { var a = arguments.length; arguments += "x"; return [a, arguments.length]; }
console.log(viaCompound(1));
function viaAssign() { arguments = "abcde"; return arguments.length; }
console.log(viaAssign(1));
function viaBlockVar() { { var arguments = "abc"; } return arguments.length; }
console.log(viaBlockVar(1));
function viaCatch() { try { throw "ab"; } catch (arguments) { return arguments.length; } }
console.log(viaCatch(1));
function viaForVar() { for (var arguments = "abcd"; ; ) return arguments.length; }
console.log(viaForVar(1));
function viaForVarOf() { for (var arguments of ["abcdef"]) return arguments.length; }
console.log(viaForVarOf(1));
function viaLetBlock() { var a = arguments.length; { let arguments = "x"; a += arguments.length; } return a + arguments.length; }
console.log(viaLetBlock(1, 2));
function viaNamedFn() { return (function arguments() { return arguments.length; })(1, 2); }
console.log(viaNamedFn());
var named = function arguments() { return arguments.length; };
console.log(named(1, 2, 3));

// Inlined at sites that each know their own count.
function clamp(a, b) { if (arguments.length < 2) return a; return a < b ? a : b; }
function hotSites(n) {
  var s = 0;
  for (var i = 0; i < n; i++) { s += clamp(i); s += clamp(i, 1); s += clamp(i, 1, 2); s += count(); s += count(i, i, i, i); }
  return s;
}
console.log(hotSites(20000));
var om = { m(a) { return arguments.length + (a | 0); } };
function hotMethod(n) {
  var s = 0;
  for (var i = 0; i < n; i++) { s += om.m(); s += om.m(1); s += om.m(1, 2); }
  return s;
}
console.log(hotMethod(20000));
"####;
