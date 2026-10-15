//! A builtin global the program replaced, redefined or deleted through the
//! global object (`globalThis.print = function () {}`) is what `eval`,
//! indirect eval, `new Function` and later scripts see, even when the program
//! never named that builtin as an identifier. Eval code used to get a fresh
//! global slot seeded with the ORIGINAL builtin whenever the main program had
//! no slot for the name, so a silenced `print` still printed from eval code,
//! and a deleted builtin was still callable there. Expected lines are Node
//! 24's.

const SOURCE: &str = r#"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
var ev = function (s) { try { return (0, eval)(s); } catch (e) { return e.constructor.name; } };
var de = function (s) { try { return eval(s); } catch (e) { return e.constructor.name; } };
var nf = function (s) { try { return new Function("return " + s)(); } catch (e) { return e.constructor.name; } };
var desc = function (n) {
  var d = Object.getOwnPropertyDescriptor(globalThis, n);
  return d ? [("value" in d ? typeof d.value : "accessor"), d.writable, d.enumerable, d.configurable].join("/") : "absent";
};

// Silenced before any eval names it: nothing reaches the real one.
var said = [];
globalThis["print"] = function (s) { said.push(s); };
eval('print("direct eval")');
(0, eval)('print("indirect eval")');
new Function('print("new Function")')();
Function('print("Function")')();
log("print", said.join("|"));

// Replaced, replaced again, assigned by eval code, deleted.
globalThis.parseInt = function () { return "r1"; };
log("1a", ev("parseInt('7')"), de("parseInt('7')"), nf("parseInt('7')"), desc("parseInt"));
globalThis.parseInt = function () { return "r2"; };
log("1b", ev("parseInt('7')"), nf("parseInt('7')"), globalThis.parseInt(), desc("parseInt"));
ev("parseInt = function () { return 'r3'; }");
log("1c", ev("parseInt('7')"), nf("parseInt('7')"), globalThis.parseInt(), desc("parseInt"));
delete globalThis.parseInt;
log("1d", ev("typeof parseInt"), nf("typeof parseInt"), "parseInt" in globalThis, ev("parseInt"), desc("parseInt"));
log("1e", Object.keys(globalThis).indexOf("parseInt"));

// Deleted before any eval names it, then created again.
delete globalThis.escape;
log("2a", ev("typeof escape"), nf("typeof escape"), ev("escape"), desc("escape"));
globalThis.escape = function () { return "e2"; };
log("2b", ev("escape()"), nf("escape()"));

// A getter over a builtin, then a non-writable value.
var gets = 0;
Object.defineProperty(globalThis, "isFinite", { get: function () { gets++; return function () { return "g"; }; }, configurable: true });
log("3a", ev("isFinite(1)"), nf("isFinite(1)"), gets, desc("isFinite"));
Object.defineProperty(globalThis, "isFinite", { value: function () { return "v"; }, writable: false, configurable: true, enumerable: false });
log("3b", ev("isFinite(1)"), nf("isFinite(1)"), desc("isFinite"));
ev("isFinite = function () { return 'w'; }");
log("3c", ev("isFinite(1)"), globalThis.isFinite(1));

// Untouched builtins resolve, and a later replacement is seen.
log("4a", ev("unescape('%41')"), nf("decodeURI('%41')"), desc("unescape"));
globalThis.unescape = function () { return "u"; };
log("4b", ev("unescape('%41')"), nf("unescape('%41')"), desc("unescape"));
log("4c", Object.keys(globalThis).indexOf("unescape"));

// A var declaration in indirect eval code, a namespace object, a hot caller.
ev("var encodeURI = function () { return 'v5'; }");
log("5a", ev("encodeURI('x')"), nf("encodeURI('x')"), globalThis.encodeURI("x"), desc("encodeURI"));
globalThis.Reflect = { ownKeys: function () { return ["R"]; } };
log("6a", ev("Reflect.ownKeys({})"), nf("Reflect.ownKeys({})"));
var calls = 0;
globalThis.encodeURIComponent = function () { calls++; return "c"; };
var hot = new Function("n", "var s = ''; for (var i = 0; i < n; i++) s = encodeURIComponent('x'); return s;");
log("7a", hot(5000), calls);
globalThis.encodeURIComponent = function () { return "c2"; };
log("7b", hot(3), ev("encodeURIComponent('x')"));

console.log(out.join("\n"));
"#;

/// Node's output for `source`, run from a file: `node -e` assigns
/// `globalThis.module` after the script, which a frozen global refuses.
fn node_output(source: &str) -> Vec<String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "zipp-eval-replaced-builtin-{}-{}.js",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, source).expect("write the node script");
    let out = std::process::Command::new("node").arg(&path).output();
    let _ = std::fs::remove_file(&path);
    let out = out.expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn eval_sees_replaced_builtins_like_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone, the JIT
/// compiling everything at once, and under GC stress.
#[test]
fn eval_sees_replaced_builtins_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("nojit", "ZIPP_NOJIT", "1"),
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("gc", "ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["eval_sees_replaced_builtins_like_node", "--exact"])
            .env(key, value)
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A first write of a builtin through the global object updates an existing
/// property: it keeps the builtin's attributes and is not refused by a
/// non-extensible global, while freezing and sealing (which make the
/// builtins real own properties with their new attributes) still govern it.
/// Each case runs in a fresh VM, against Node.
#[test]
fn builtin_writes_on_a_non_extensible_frozen_or_sealed_global() {
    const BODY: &str = r#"
var d = function (n) { var x = Object.getOwnPropertyDescriptor(globalThis, n); return x ? [typeof x.value, x.writable, x.enumerable, x.configurable].join("/") : "absent"; };
var f = function () { return "f"; };
function trySloppy(name) { try { globalThis[name] = f; return globalThis[name] === f ? "replaced" : "kept"; } catch (e) { return e.constructor.name; } }
function tryStrict(name) { "use strict"; try { globalThis[name] = f; return globalThis[name] === f ? "replaced" : "kept"; } catch (e) { return e.constructor.name; } }
function evalSees(name) { return (0, eval)(name) === f ? "eval:f" : "eval:orig"; }
SETUP;
console.log([trySloppy("parseInt"), d("parseInt"), evalSees("parseInt"), tryStrict("isNaN"), d("isNaN"), evalSees("isNaN"), trySloppy("newGlobalName"), d("newGlobalName")].join(" "));
"#;
    for setup in ["", "Object.preventExtensions(globalThis)", "Object.freeze(globalThis)", "Object.seal(globalThis)"] {
        let src = BODY.replace("SETUP", setup);
        let out = zipp_vm::run(&src).expect("source compiles");
        assert!(out.error.is_none(), "{setup}: unexpected runtime error: {:?}", out.error);
        assert_eq!(out.output, node_output(&src), "{setup}");
    }
}

/// An embedder's later scripts are compiled after the first one ran: a
/// `print` the first script silenced stays silent in them, directly and
/// through eval.
#[test]
fn later_scripts_see_a_silenced_print() {
    use zipp_vm::embed;
    let mut st = embed::compile_script("globalThis.print = function () {};").expect("compiles");
    st.run_init().expect("init runs");
    for src in [
        "print('LEAK direct')",
        "eval(\"print('LEAK eval')\")",
        "new Function(\"print('LEAK fn')\")()",
        "(0, eval)(\"print('LEAK indirect')\")",
    ] {
        st.eval_in_context(src).expect("runs");
    }
    let leaked: Vec<String> = st.take_output().into_iter().filter(|l| l.contains("LEAK")).collect();
    assert!(leaked.is_empty(), "the silenced print still printed: {leaked:?}");
}
