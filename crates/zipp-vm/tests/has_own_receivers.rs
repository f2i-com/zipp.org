//! `hasOwnProperty` answers an ordinary object and a flat string key in
//! place. This battery pins the receivers that must stay off that path to
//! Node's answers: %Array.prototype%'s exotic `length`, the global object,
//! boxed primitives, arrays, mapped and strict arguments, a Proxy with a
//! `getOwnPropertyDescriptor` trap, RegExp `lastIndex` and match results,
//! classes, accessors, deleted keys and an `@@`-spelled string key, in the
//! interpreter, under GC stress and with the default tiers.

const SOURCE: &str = r#"
// hasOwnProperty edge receivers, compared with Node.
var hop = Object.prototype.hasOwnProperty;
console.log(Array.prototype.hasOwnProperty("length"), hop.call(Array.prototype, "length"), hop.call(Array.prototype, "push"));
console.log(Object.prototype.hasOwnProperty("__proto__"), ({}).hasOwnProperty("__proto__"), hop.call({ __proto__: null }, "__proto__"));
var o = { a: 1 };
console.log(o.hasOwnProperty("a"), o.hasOwnProperty("b"), o.hasOwnProperty("toString"));
console.log(globalThis.hasOwnProperty("Array"), hop.call(Math, "PI"), JSON.hasOwnProperty("parse"));
console.log(Reflect.hasOwnProperty("apply"), Function.prototype.hasOwnProperty("length"), Function.prototype.hasOwnProperty("name"));
console.log(String.prototype.hasOwnProperty("length"), Number.prototype.hasOwnProperty("toFixed"), hop.call(new String("ab"), "1"));
console.log(hop.call(new Boolean(true), "valueOf"), hop.call(Object(Symbol.iterator), "description"), hop.call(new Date(0), "getTime"));
var a = [1, 2]; a.x = 3;
console.log(hop.call(a, "x"), hop.call(a, "length"), hop.call(a, "0"), hop.call(a, "2"));
function f(p, q) { return arguments; }
var args = f(1, 2);
console.log(hop.call(args, "length"), hop.call(args, "callee"), hop.call(args, "0"), hop.call(args, "2"));
var sargs = (function () { "use strict"; return arguments; })(1);
console.log(hop.call(sargs, "callee"), hop.call(sargs, "length"));
var p = new Proxy({ z: 1 }, { getOwnPropertyDescriptor: function (t, k) { return k === "y" ? { value: 1, configurable: true } : undefined; } });
console.log(hop.call(p, "y"), hop.call(p, "z"));
var e = new Error("m");
console.log(hop.call(e, "message"), hop.call(Error.prototype, "name"));
var re = /x/g;
console.log(hop.call(re, "lastIndex"), hop.call(RegExp.prototype, "flags"), hop.call(/a(b)/.exec("ab"), "index"));
class C { static s = 1; m() {} }
console.log(hop.call(C, "s"), hop.call(C.prototype, "m"), hop.call(C.prototype, "constructor"), hop.call(new C(), "m"));
var acc = {}; Object.defineProperty(acc, "g", { get: function () { return 1; } });
console.log(hop.call(acc, "g"), hop.call(Object.create(acc), "g"));
var del = { k: 1 }; delete del.k;
console.log(hop.call(del, "k"), hop.call({ "@@iterator": 1 }, "@@iterator"));
"#;

fn output_from_node(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn has_own_receivers_worker() {
    if std::env::var_os("ZIPP_HAS_OWN_RECEIVERS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, output_from_node(SOURCE));
}

#[test]
fn has_own_receivers_matches_node() {
    if std::env::var_os("ZIPP_HAS_OWN_RECEIVERS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc_stress) in [
        ("interpreter", true, false),
        ("interpreter-gc-stress", true, true),
        ("default-tiers", false, false),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["has_own_receivers_worker", "--exact", "--nocapture"])
            .env("ZIPP_HAS_OWN_RECEIVERS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
