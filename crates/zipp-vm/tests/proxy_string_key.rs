//! `p[k]` and `p[k] = v` on a Proxy with a string key hand the key to the
//! `get` and `set` traps as it stands (`proxy_get_value_key`,
//! `proxy_set_value_key`), and spell it only for the invariant checks.
//! Every key and trap outcome must still match: plain, concatenated (rope),
//! long (past the 64-byte buffer), `@@`-spelt, non-ASCII and lone-surrogate
//! string keys, symbols and numbers (the general path), no trap (the
//! target's [[Get]] and [[Set]] with the proxy as receiver), revoked
//! proxies, both invariants, falsish `set` results in sloppy and strict
//! code, a throwing trap, and a default-value dictionary in a hot loop.
//! The expected lines are Node's.

const SOURCE: &str = r##"
(function () {
  var out = [];
  var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
  function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
  var seen = [];
  var p = new Proxy({ a: 1, long: 2 }, {
    get(t, k, r) { seen.push(typeof k === "symbol" ? "sym" : typeof k + ":" + k.length); return Reflect.get(t, k, r); },
    set(t, k, v, r) { seen.push("set " + (typeof k === "symbol" ? "sym" : k.length)); return Reflect.set(t, k, v, r); },
  });
  var long = "k".repeat(70), rope = "a" + String(seen.length * 0);
  rope = rope.slice(0, 1);
  var keys = ["a", "x", "@@iterator", "@@x", "été", "\ud800", long, "", "0", "1.5"];
  for (var i = 0; i < keys.length; i++) p[keys[i]] = i;
  for (var i = 0; i < keys.length; i++) log("get", JSON.stringify(keys[i]).length, p[keys[i]]);
  var built = "lo" + "ng"; log("rope", p[built], p["a" + ""], p[rope]);
  var sym = Symbol("s"); p[sym] = "symval"; log("symbol", p[sym], p[0], p[1.5]);
  log("seen", seen.join(","));
  var target = { get g() { return this === fwd ? "receiver is proxy" : "other"; }, set s(v) { this.sv = v; }, d: 5 };
  var fwd = new Proxy(target, {});
  var gk = "g", dk = "d", sk = "s";
  fwd[sk] = 7;
  log("no trap", fwd[dk], fwd[gk], fwd["nope"], target.sv, Object.keys(target).join(","));
  var r = Proxy.revocable({ a: 1 }, {}); r.revoke();
  attempt("revoked get", function () { var k = "a"; return r.proxy[k]; });
  attempt("revoked set", function () { var k = "a"; r.proxy[k] = 1; return "set"; });
  var fixed = {}; Object.defineProperty(fixed, "k", { value: 1, writable: false, configurable: false });
  var kk = "k";
  attempt("get invariant same", function () { return new Proxy(fixed, { get() { return 1; } })[kk]; });
  attempt("get invariant differs", function () { return new Proxy(fixed, { get() { return 2; } })[kk]; });
  attempt("set invariant", function () { "use strict"; var q = new Proxy(fixed, { set() { return true; } }); q[kk] = 5; return "set"; });
  attempt("set invariant same", function () { "use strict"; var q = new Proxy(fixed, { set() { return true; } }); q[kk] = 1; return "set"; });
  attempt("set falsish sloppy", function () { var q = new Proxy({}, { set() { return false; } }); q[kk] = 1; return "set"; });
  attempt("set falsish strict", function () { "use strict"; var q = new Proxy({}, { set() { return false; } }); q[kk] = 1; return "set"; });
  attempt("throwing get", function () { return new Proxy({}, { get() { throw new RangeError("no"); } })[kk]; });
  attempt("throwing set", function () { new Proxy({}, { set() { throw new TypeError("no"); } })[kk] = 1; return "set"; });
  var words = [];
  for (var i = 0; i < 64; i++) words.push("w" + (i * 7919 % 100));
  var dict = new Proxy({}, {
    get(t, k) { return k in t ? t[k] : 0; },
    set(t, k, v) { t[k] = v; return true; },
  });
  for (var i = 0; i < 3000; i++) { var w = words[i & 63]; dict[w] = dict[w] + 1; }
  var ks = Object.keys(dict);
  log("dict", ks.length, dict[words[3]], dict["w0"], dict["absent"]);
  out.forEach(function (l) { console.log(l); });
})();
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
fn proxy_string_key_worker() {
    if std::env::var_os("ZIPP_PROXY_STRING_KEY_WORKER").is_none() {
        return;
    }
    // The harness has printed `test proxy_string_key_worker ... ` unterminated.
    println!();
    for line in zipp_lines() {
        println!("LINE {line}");
    }
}

#[test]
fn proxy_string_key_matches_node() {
    if std::env::var_os("ZIPP_PROXY_STRING_KEY_WORKER").is_some() {
        return;
    }
    let want = node_output(SOURCE);
    assert_eq!(zipp_lines(), want, "in-process run");
    let out = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["proxy_string_key_worker", "--exact", "--nocapture", "--test-threads=1"])
        .env("ZIPP_PROXY_STRING_KEY_WORKER", "1")
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
