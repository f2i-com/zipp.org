//! A split member call (the method read before an argument that may run
//! code) whose captured callee is a built-in Array or String method runs that
//! callee by its own identity (`captured_own_method`): the captured function,
//! whatever the site's name and the prototype say, with the method's own
//! checks (frozen arrays, read-only `length`, receivers of the other kind).
//! The expected lines are Node's.

const SOURCE: &str = r##"// Split member calls (`obj.m(arg)` with an argument the compiler cannot prove
// effect-free, so the method is read before the arguments) whose captured
// callee is a built-in Array/String method: the call runs the CAPTURED function,
// whatever the site's name says and whatever the prototype holds afterwards.
var out = [];
function t(name, f) { try { out.push(name + " " + f()); } catch (e) { out.push(name + " threw " + e.constructor.name); } }
var o = { p: 1, q: [1, 2, 3] };
function arg() { return o.p; }          // a member read: not order-transparent
t("charCodeAt", () => { var s = "hello", acc = 0; for (var k = 0; k < 2000; k++) acc += s.charCodeAt(o.p + (k & 3)); return acc; });
t("push", () => { var a = []; for (var k = 0; k < 2000; k++) a.push(o.q[k % 3]); return a.length + ":" + a[1999]; });
t("indexOf", () => { var s = "abcabc", acc = 0; for (var k = 0; k < 500; k++) acc += s.indexOf("c", o.p + (k & 1)); return acc; });
t("slice", () => { var a = [1, 2, 3, 4], acc = 0; for (var k = 0; k < 500; k++) acc += a.slice(o.p).length; return acc; });
var swapped = String.prototype.charCodeAt;
String.prototype.charCodeAt = String.prototype.charAt;
t("swapped charAt", () => { var s = "hey", r = ""; for (var k = 0; k < 300; k++) r = s.charCodeAt(o.p); return r; });
String.prototype.charCodeAt = swapped;
t("restored", () => "hey".charCodeAt(o.p));
var origPush = Array.prototype.push;
Array.prototype.push = function (v) { return "mine " + v; };
t("replaced push", () => { var a = [], r; for (var k = 0; k < 300; k++) r = a.push(o.p); return r + " " + a.length; });
Array.prototype.push = origPush;
t("own push", () => { var a = [1]; a.push = function (v) { return "own " + v; }; return a.push(o.p); });
t("borrowed slice on string", () => { String.prototype.sl = Array.prototype.slice; var r = "abc".sl(o.p); delete String.prototype.sl; return JSON.stringify(r); });
t("borrowed charAt on array", () => { Array.prototype.ch = String.prototype.charAt; var r = [7, 8, 9].ch(o.p); delete Array.prototype.ch; return r; });
t("push on frozen", () => { var a = Object.freeze([1]); return a.push(o.p); });
t("push on string via call", () => { String.prototype.pu = Array.prototype.push; try { return "ab".pu(o.p); } finally { delete String.prototype.pu; } });
t("length nonwritable", () => { var a = [1, 2]; Object.defineProperty(a, "length", { writable: false }); return a.push(o.p); });
t("getter method", () => { var calls = 0; Object.defineProperty(String.prototype, "cc2", { get() { calls++; return String.prototype.charCodeAt; }, configurable: true }); var acc = 0; for (var k = 0; k < 200; k++) acc += "xyz".cc2(o.p); delete String.prototype.cc2; return acc + " " + calls; });
t("valueOf order", () => { var log = []; var key = { valueOf() { log.push("valueOf"); return 1; } }; var s = "abc"; Object.defineProperty(String.prototype, "cx", { get() { log.push("get"); return String.prototype.charCodeAt; }, configurable: true }); var r = s.cx(key); delete String.prototype.cx; return r + " " + log.join(","); });
console.log(out.join("\n"));
"##;

const NODE: &str = r##"charCodeAt 214000
push 2000:2
indexOf 1000
slice 1500
swapped charAt e
restored 101
own push own 1
borrowed slice on string ["b","c"]
borrowed charAt on array ,
push on frozen threw TypeError
push on string via call threw TypeError
length nonwritable threw TypeError
getter method 24200 200
valueOf order 98 get,valueOf
"##;

#[test]
fn captured_own_method_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    let want: Vec<String> = NODE.trim_start_matches('\n').lines().map(str::to_owned).collect();
    assert_eq!(got, want);
}

/// The corpus again with the lane off, eager JIT, GC stress and the
/// interpreter, each in a fresh process.
#[test]
fn captured_own_method_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_CAPTURED_OWN", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["captured_own_method_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}