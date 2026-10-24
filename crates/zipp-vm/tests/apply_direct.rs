//! `f.apply(t, list)` on a plain function, while `apply` is the pristine
//! `%Function.prototype.apply%` and the list reads without running code
//! (nullish, a dense array, an arguments object as it was made), enters `f` as
//! a frame of the dispatch loop (`interp_apply_list`, `setup_call_slice`),
//! from `CallMethod` and from the spec-order `GetProp; CallWithThis` pair. The
//! forwarded arguments must be what [[Get]] reads: mapped formals assigned
//! before the call, elements written, `length` shortened, lengthened or made a
//! string, deleted and redefined indices, captured formals, default
//! parameters, an arguments object that escaped its frame; arrays with holes,
//! past their dense storage, array-likes, typed arrays, and non-object lists
//! that throw. `this` binds as a call binds it (sloppy, strict, arrows), rest
//! parameters and the callee's own `arguments` are built, generators, async
//! functions, classes, bound functions and natives keep the general call,
//! recursion through `apply` is frames, throws propagate, and an own,
//! replaced or accessor `apply` is honoured. The expected lines are Node's.

const SOURCE: &str = r##"
// Behaviours the direct `f.apply(t, list)` must keep (sloppy script: the
// mapped-arguments cases need it). Each function runs hot first.
var out = [];
function log() { out.push(Array.prototype.join.call(arguments, " ")); }
function heat(f) { var r; for (var i = 0; i < 300; i++) r = f(i); return r; }
function attempt(f) { try { return String(f()); } catch (e) { return "threw " + e.constructor.name; } }

function sum() { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s + "/" + arguments.length; }
function two(a, b) { return [this === globalThis ? "global" : typeof this + ":" + String(this), a, b, arguments.length].join(","); }
function strictTwo(a, b) { "use strict"; return [this === undefined ? "undef" : typeof this + ":" + String(this), a, b].join(","); }

log("forward", heat(function () { return (function () { return sum.apply(this, arguments); })(1, 2, 3); }));
log("forward none", heat(function () { return (function () { return sum.apply(this, arguments); })(); }));
log("forward many", heat(function () { return (function () { return sum.apply(this, arguments); })(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12); }));
log("mapped formal changed", heat(function () { return (function (a, b) { a = 10; return sum.apply(null, arguments); })(1, 2, 3); }));
log("strict formal changed", heat(function () { return (function (a, b) { "use strict"; a = 10; return sum.apply(null, arguments); })(1, 2, 3); }));
log("element written", heat(function () { return (function (a) { arguments[0] = 7; arguments[2] = 9; return sum.apply(null, arguments); })(1, 2, 3); }));
log("length shorter", heat(function () { return (function () { arguments.length = 1; return sum.apply(null, arguments); })(4, 5, 6); }));
log("length longer", heat(function () { return (function () { arguments.length = 4; return sum.apply(null, arguments); })(4, 5); }));
log("length string", heat(function () { return (function () { arguments.length = "2"; return sum.apply(null, arguments); })(4, 5, 6); }));
log("deleted index", heat(function () { return (function (a, b) { delete arguments[0]; return two.apply(null, arguments); })(1, 2); }));
log("getter index", heat(function () { return (function (a, b) { Object.defineProperty(arguments, "1", { get: function () { return "got"; } }); return two.apply(null, arguments); })(1, 2); }));
log("captured formal", heat(function () { return (function (a, b) { var f = function () { return a; }; a = 5; return two.apply(null, arguments) + f(); })(1, 2); }));
log("default params", heat(function () { return (function (a, b = 9) { a = 3; return two.apply(null, arguments); })(1); }));
log("escaped arguments", heat(function () { var saved = (function (a) { return arguments; })(1, 2); return sum.apply(null, saved); }));

log("array", heat(function (i) { return two.apply({ t: 1 }, [i, "x"]); }));
log("array holes", heat(function () { return two.apply(null, [1, , 3]); }));
var holeProto = [];
log("array long", heat(function () { var a = [1]; a.length = 3; return two.apply(null, a); }));
log("array-like", heat(function () { return two.apply(null, { length: 2, 0: "a", 1: "b" }); }));
log("typed", heat(function () { return two.apply(null, new Int8Array([3, 4])); }));
log("null list", heat(function () { return two.apply(null, null); }), heat(function () { return two.apply(null, undefined); }));
log("string list", attempt(function () { return two.apply(null, "ab"); }));
log("number list", attempt(function () { return two.apply(null, 5); }));
log("no args", heat(function () { return two.apply(); }), heat(function () { return two.apply({ k: 1 }); }));
log("extra args", heat(function () { return two.apply(null, [1, 2], [3]); }));
log("this sloppy", heat(function () { return two.apply(5, [1]); }), heat(function () { return two.apply("s", [1]); }), heat(function () { return two.apply(true, []); }));
log("this strict", heat(function () { return strictTwo.apply(null, [1, 2]); }), heat(function () { return strictTwo.apply(5, [1, 2]); }));
var arrowThis = (function () { return () => this.tag; }).call({ tag: "lexical" });
log("arrow", heat(function () { return arrowThis.apply({ tag: "ignored" }, []); }));
function rest(a, ...more) { return a + ":" + more.join("/") + ":" + arguments.length; }
log("rest", heat(function () { return rest.apply(null, [1, 2, 3]); }), heat(function () { return rest.apply(null, []); }));
function usesArgs() { return arguments.length + ":" + Array.prototype.slice.call(arguments).join(); }
log("callee arguments", heat(function () { return usesArgs.apply(null, [1, 2]); }));
function* gen(a) { yield a; }
log("generator", heat(function () { return gen.apply(null, [5]).next().value; }));
async function af(a) { return a; }
log("async", heat(function () { return typeof af.apply(null, [1]).then; }));
class K { constructor(a) { this.a = a; } }
log("class", attempt(function () { return K.apply(null, [1]); }));
var bound = two.bind({ b: 1 }, "x");
log("bound", heat(function () { return bound.apply(null, ["y"]); }));
log("native", heat(function () { return Math.max.apply(null, [1, 9, 3]); }), heat(function () { return String.fromCharCode.apply(null, [65, 66]); }));
function deep(n) { return n === 0 ? 0 : 1 + deep.apply(null, [n - 1]); }
log("deep", deep(2500));
function deepArgs(n) { return n === 0 ? 0 : 1 + deepArgs.apply(this, [n - 1]); }
log("deep this", deepArgs.call({}, 2000));
function thrower() { throw new RangeError("bad"); }
log("throws", attempt(function () { return thrower.apply(null, []); }));
log("caller arguments", heat(function () { return (function f() { return f.arguments === null ? "null" : typeof f.arguments; })(); }));
var ownApply = function () { return "real"; };
ownApply.apply = function () { return "own apply"; };
log("own apply", heat(function () { return ownApply.apply(null, []); }));
var savedApply = Function.prototype.apply;
Function.prototype.apply = function () { return "replaced"; };
log("replaced", heat(function () { return two.apply(null, [1]); }));
Function.prototype.apply = savedApply;
log("restored", heat(function () { return two.apply(null, [1]); }));
Object.defineProperty(Function.prototype, "apply", { get: function () { return function () { return "from getter"; }; }, configurable: true });
log("accessor", heat(function () { return two.apply(null, [1]); }));
Object.defineProperty(Function.prototype, "apply", { value: savedApply, writable: true, configurable: true });
log("again", heat(function () { return two.apply(null, [2, 3]); }));
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn zipp_lines() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n").lines().map(str::to_owned).collect()
}

#[test]
fn apply_direct_worker() {
    if std::env::var_os("ZIPP_APPLY_DIRECT_WORKER").is_none() {
        return;
    }
    // The harness has printed `test apply_direct_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn apply_direct_match_node() {
    if std::env::var_os("ZIPP_APPLY_DIRECT_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["apply_direct_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_APPLY_DIRECT_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn the worker");
    assert!(out.status.success(), "worker failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_owned))
        .collect();
    assert_eq!(got, want, "interpreter-only run");
}
