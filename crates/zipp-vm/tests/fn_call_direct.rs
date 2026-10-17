//! `f.call(t, ...args)` on a plain function enters `f` directly as the call
//! with `this` = `t` that it is, while a Get of `call` finds the pristine
//! `%Function.prototype.call%`. Every `this` binding must match a real call
//! (sloppy nullish and primitive `this`, strict, arrows), every argument
//! count (none, `this` only, past eight), `arguments` and its aliasing, and
//! every case that must not take the direct entry still behaves: generators,
//! async functions, classes, bound functions, natives, an own `call` shadow,
//! a replaced or accessor `Function.prototype.call`, throws and recursion;
//! and the interpreter's `.call` recursion is frames of one loop, not a
//! native-stack frame per level.
//! The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
function sloppyThis() { return this === globalThis ? "global" : typeof this + ":" + String(this); }
function strictThis() { "use strict"; return this === undefined ? "undefined" : typeof this + ":" + String(this); }
log("sloppy", sloppyThis.call(null), sloppyThis.call(undefined), sloppyThis.call(5), sloppyThis.call("s"), sloppyThis.call(true), sloppyThis.call());
log("strict", strictThis.call(null), strictThis.call(undefined), strictThis.call(5), strictThis.call("s"), strictThis.call());
var o = { tag: "o" };
function getTag() { return this.tag; }
log("object", getTag.call(o), getTag.call({ tag: "p" }));
var arrow = () => typeof this;
log("arrow", arrow.call(o), arrow.call(null));
function sum() { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s + ":" + arguments.length; }
log("args", sum.call(null), sum.call(null, 1), sum.call(null, 1, 2, 3), sum.call(null, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11));
function named(a, b, c) { return [a, b, c, arguments.length].join(","); }
log("params", named.call(o), named.call(o, 1), named.call(o, 1, 2, 3, 4));
function alias(a, b) { arguments[0] = "changed"; b = "b2"; return a + "|" + arguments[1]; }
log("alias", alias.call(null, "a", "b"));
function strictAlias(a) { "use strict"; arguments[0] = "changed"; return a; }
log("strict alias", strictAlias.call(null, "kept"));
function rest(first, ...more) { return first + ":" + more.join("/"); }
log("rest", rest.call(o, 1, 2, 3), rest.call(o));
function defaults(a = 10, b = a + 1) { return a + b; }
log("defaults", defaults.call(null), defaults.call(null, 1), defaults.call(null, undefined, 5));
function fact(n) { return n <= 1 ? 1 : n * fact.call(this, n - 1); }
log("recursion", fact.call(null, 10));
function deep(n) { return n === 0 ? 0 : 1 + deep.call(null, n - 1); }
function deepMethod(n) { return n === 0 ? 0 : 1 + deepMethod.call(this, n - 1); }
log("deep", deep.call(null, 20), deepMethod.call(o, 20));
function nt() { return new.target === undefined; }
log("new.target", nt.call(o));
function thrower() { throw new RangeError("x"); }
attempt("throws", function () { return thrower.call(o); });
function* gen(a) { yield this.tag; yield a; }
log("generator", Array.from(gen.call(o, 7)).join());
async function af(x) { return this.tag + x; }
var pending = af.call(o, "!");
log("async", typeof pending.then);
class K { constructor() { this.v = 1; } }
attempt("class", function () { return K.call(o); });
var bound = getTag.bind({ tag: "bound" });
log("bound", bound.call(o));
log("native", Object.prototype.hasOwnProperty.call(o, "tag"), Array.prototype.slice.call([1, 2, 3], 1).join(), Math.max.call(null, 3, 9, 4));
log("natives this", Object.prototype.toString.call(null), Object.prototype.toString.call([]), String.prototype.toUpperCase.call("abc"), Number.prototype.toFixed.call(1.25, 1), Symbol.prototype.toString.call(Symbol("x")));
log("call.call", Function.prototype.call.call(getTag, o), Function.prototype.call.call(Math.max, null, 1, 5));
var like = { length: 0 }; Array.prototype.push.call(like, "a", "b"); log("array-like", like.length, like[1]);
attempt("native throws", function () { return Array.prototype.forEach.call(null, function () {}); });
attempt("strict native", function () { "use strict"; return Object.prototype.valueOf.call(undefined); });
var hasOwn = Object.prototype.hasOwnProperty;
var hits = 0; for (var i = 0; i < 50; i++) if (hasOwn.call(o, i & 1 ? "tag" : "nope")) hits++;
log("loop", hits);
function shadowed() { return "real"; }
shadowed.call = function () { return "shadow"; };
log("own shadow", shadowed.call(o));
var closureCount = 0;
function makeAdder(n) { return function (x) { closureCount++; return x + n + (this === o ? 100 : 0); }; }
var add5 = makeAdder(5);
var acc = 0; for (var i = 0; i < 20; i++) acc += add5.call(i & 1 ? o : null, i);
log("closure", acc, closureCount);
var method = { m: function (x) { return this === method ? "self" : "other:" + x; } };
log("method", method.m.call(method, 1), method.m.call(o, 2));
var savedCall = Function.prototype.call;
Function.prototype.call = function (t) { return "replaced"; };
log("replaced", getTag.call(o), sum.call(null, 1, 2));
Function.prototype.call = savedCall;
log("restored", getTag.call(o));
Object.defineProperty(Function.prototype, "call", { get: function () { return function () { return "from getter"; }; }, configurable: true });
log("accessor", getTag.call(o));
Object.defineProperty(Function.prototype, "call", { value: savedCall, writable: true, configurable: true });
log("again", getTag.call(o), named.call(null, "x"));
pending.then(function (v) { log("async result", v); console.log(out.join("\n")); });
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

/// Recursion through `.call` deep enough that a nested run loop per level
/// would overflow a debug build's native stack. Interpreter only, in a child:
/// the compiled tiers' calls are native frames.
const DEEP: &str = r##"
var o = {};
function deep(n) { return n === 0 ? 0 : 1 + deep.call(null, n - 1); }
function deepMethod(n) { return n === 0 ? 0 : 1 + deepMethod.call(this, n - 1); }
console.log(deep.call(null, 3000), deepMethod.call(o, 3000));
"##;

#[test]
fn fn_call_deep_worker() {
    if std::env::var_os("ZIPP_FN_CALL_DEEP_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(DEEP).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n").trim(), "3000 3000");
}

#[test]
fn fn_call_deep_recursion_is_frames() {
    if std::env::var_os("ZIPP_FN_CALL_DEEP_WORKER").is_some() {
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["fn_call_deep_worker", "--exact", "--nocapture"])
        .env("ZIPP_FN_CALL_DEEP_WORKER", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn the worker");
    assert!(
        out.status.success(),
        "deep .call recursion failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn fn_call_direct_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
