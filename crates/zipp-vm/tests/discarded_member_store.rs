//! A member store whose result a statement discards (`this.x = x;`) stores
//! the value's own register instead of a copy (`assign_discarded`): stored
//! values, setters and proxies, evaluation order, read-only targets and the
//! result of a store that IS used are unchanged. The expected lines are
//! Node's.

const SOURCE: &str = r##"// Member stores whose result is discarded store the value's own register:
// the stored value, setter calls, evaluation order and the result of a
// store that IS used must all stay exactly as before.
var out = [];
function t(name, f) { try { out.push(name + " " + f()); } catch (e) { out.push(name + " threw " + e.constructor.name); } }
class V { constructor(x, y) { this.x = x; this.y = y; } }
t("ctor", () => { var v = new V(1, 2); return v.x + "," + v.y; });
t("self", () => { var o = {}; o.self = o; return o.self === o; });
t("index self", () => { var a = [0, 1, 2]; var i = 1; a[i] = i; return a.join(); });
t("chain", () => { var a = {}, b = {}; var v = 7; a.x = b.y = v; return a.x + b.y; });
t("used", () => { var o = {}; var v = 3; var r = (o.p = v); return r + o.p; });
t("seq", () => { var o = {}; var v = 4; var r = (o.p = v, o.q = v + 1); return r + o.p + o.q; });
t("setter", () => { var log = []; var o = { set p(v) { log.push("set " + v); } }; var v = 5; o.p = v; return log.join(); });
t("setter result", () => { var o = { set p(v) { } }; var v = 6; return (o.p = v); });
t("receiver order", () => { var log = []; var o = { a: {} }; function k() { log.push("k"); return "x"; } function val() { log.push("v"); return 1; } o.a[k()] = val(); return log.join() + " " + o.a.x; });
t("reassign obj", () => { var o = { n: 1 }, keep = o; var v = 9; o.n = (o = { n: 2 }, v); return keep.n + " " + o.n; });
t("key then value", () => { var a = []; var i = 0; a[i] = i++; return JSON.stringify(a) + " " + i; });
t("frozen sloppy", () => { var o = Object.freeze({ p: 1 }); var v = 2; o.p = v; return o.p; });
t("frozen strict", () => { "use strict"; var o = Object.freeze({ p: 1 }); var v = 2; o.p = v; return o.p; });
t("private", () => { class P { #x = 0; set(v) { this.#x = v; return this.#x; } } return new P().set(8); });
t("concat key", () => { var o = {}; var v = 3; var i = 2; o["k" + i] = v; return o.k2; });
t("typed", () => { var ta = new Float64Array(2); var v = 1.5; ta[1] = v; return ta[1]; });
t("proxy", () => { var log = []; var p = new Proxy({}, { set(t, k, v) { log.push(k + "=" + v); t[k] = v; return true; } }); var v = 3; p.a = v; p["b"] = v; return log.join(); });
t("string prim", () => { "use strict"; var s = "abc"; var v = 1; s.x = v; return "no"; });
t("loop", () => { var a = []; for (var i = 0; i < 3000; i++) { var o = { v: 0 }; o.v = i; a[i & 63] = o; a.length; } return a[5].v + a[63].v; });
t("class fields", () => { class Q { x; y; constructor(x, y) { this.x = x; this.y = y; } } var s = 0; for (var i = 0; i < 2000; i++) { var q = new Q(i, i + 1); s += q.x + q.y; } return s; });
console.log(out.join("\n"));
"##;

const NODE: &str = r##"ctor 1,2
self true
index self 0,1,2
chain 14
used 6
seq 14
setter set 5
setter result 6
receiver order k,v 1
reassign obj 9 2
key then value [0] 1
frozen sloppy 1
frozen strict threw TypeError
private 8
concat key 3
typed 1.5
proxy a=3,b=3
string prim threw TypeError
loop 5892
class fields 4000000
"##;

#[test]
fn discarded_member_store_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    let want: Vec<String> = NODE.trim_start_matches('\n').lines().map(str::to_owned).collect();
    assert_eq!(got, want);
}

/// The corpus again with the store copy back, eager JIT, GC stress and the
/// interpreter, each in a fresh process.
#[test]
fn discarded_member_store_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_DISCARDED_STORE", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["discarded_member_store_parity", "--exact"])
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