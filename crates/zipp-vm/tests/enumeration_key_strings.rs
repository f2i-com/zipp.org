//! Enumeration hands out one heap string per property key.
//!
//! `for (k in o)`, `Object.keys` and their kin used to allocate a string for
//! every key of every object on every pass. They now share one string per
//! key text (`Vm::key_str_cache`), which is only sound if nothing can tell:
//! a key string must never be grown in place by a loop that starts from it,
//! escaped keys must read back as they did, mutation during a loop must be
//! seen as before, and keys past the cache's bounds must still enumerate.
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    // The checked profile meters, and the 9,000-key case needs room.
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(2_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"var out = [];
function t(label, f) { try { out.push(label + "=" + f()); } catch (e) { out.push(label + "!" + e.name + ":" + e.message); } }

// The same keys enumerated many times, by every route that yields key strings.
var o = { alpha: 1, beta: 2, "@@x": 3, "@@@y": 4, "#p": 5, "": 6, 7: 8, "long key with spaces": 9 };
t("for-in", () => { var s = []; for (var r = 0; r < 3; r++) for (var k in o) s.push(k); return s.join("|"); });
t("keys", () => Object.keys(o).join("|") + "/" + Object.keys(o).join("|"));
t("entries", () => Object.entries(o).map(e => e[0] + "=" + e[1]).join("|"));
t("names", () => Object.getOwnPropertyNames(o).join("|"));
t("reflect", () => Reflect.ownKeys(o).join("|"));
t("json", () => JSON.stringify(o) + JSON.stringify(Object.keys(o)));

// A key string is nobody's accumulator: appending to one must not change the next enumeration.
t("append-to-key", () => {
  var acc = [];
  for (var r = 0; r < 3; r++) {
    for (var k in o) { var s = k; for (var i = 0; i < 3; i++) s += i; acc.push(s); }
  }
  return acc.slice(0, 3).join("|") + "/" + Object.keys(o).join("|");
});
t("append-keys0", () => {
  var res = [];
  for (var r = 0; r < 3; r++) { var s = Object.keys(o)[1]; for (var i = 0; i < 2; i++) s += "!" + i; res.push(s); }
  return res.join("|") + "/" + Object.keys(o)[1];
});
t("concat-chain", () => { var s = ""; for (var k in o) s += k + ","; var u = ""; for (var k2 in o) u += k2 + ","; return (s === u) + ":" + s; });

// Mutation while enumerating.
t("delete-unvisited", () => { var p = { a: 1, b: 2, c: 3, d: 4 }; var s = []; for (var k in p) { s.push(k); if (k === "a") delete p.c; } return s.join(""); });
t("add-during", () => { var p = { a: 1, b: 2 }; var s = []; for (var k in p) { s.push(k); p["n" + k] = 1; } return s.join(""); });
t("proto-key-added", () => {
  var p = { a: 1, b: 2 }; var s = [];
  for (var k in p) { s.push(k); if (k === "a") Object.prototype.zzInherited = 1; }
  delete Object.prototype.zzInherited;
  return s.join(",");
});
t("non-enumerable-mid-loop", () => { var p = { a: 1, b: 2, c: 3 }; var s = []; for (var k in p) { s.push(k); if (k === "a") Object.defineProperty(p, "b", { enumerable: false }); } return s.join(""); });
t("shadowed", () => { var base = { a: 1, b: 2 }; var p = Object.create(base); p.b = 3; p.c = 4; var s = []; for (var k in p) s.push(k); return s.join(""); });

// Many distinct keys: past the cache's entry bound and its key-length bound.
t("many-keys", () => {
  var big = {}; for (var i = 0; i < 9000; i++) big["k" + i] = i;
  var n = 0, last = ""; for (var r = 0; r < 2; r++) for (var k in big) { n++; last = k; }
  var longKey = "x".repeat(200); var q = {}; q[longKey] = 1; q.short = 2;
  var s = []; for (var r2 = 0; r2 < 2; r2++) for (var k2 in q) s.push(k2.length);
  return n + ":" + last + ":" + s.join(",") + ":" + Object.keys(big).length;
});
t("types", () => { var s = []; for (var k in o) s.push(typeof k); return [...new Set(s)].join(","); });
for (var line of out) console.log(line);
"####;

#[test]
fn shared_key_strings_are_not_observable() {
    let out = run(PROGRAM);
    let expected: &[&str] = &[
        r####"for-in=7|alpha|beta|@@x|@@@y|#p||long key with spaces|7|alpha|beta|@@x|@@@y|#p||long key with spaces|7|alpha|beta|@@x|@@@y|#p||long key with spaces"####,
        r####"keys=7|alpha|beta|@@x|@@@y|#p||long key with spaces/7|alpha|beta|@@x|@@@y|#p||long key with spaces"####,
        r####"entries=7=8|alpha=1|beta=2|@@x=3|@@@y=4|#p=5|=6|long key with spaces=9"####,
        r####"names=7|alpha|beta|@@x|@@@y|#p||long key with spaces"####,
        r####"reflect=7|alpha|beta|@@x|@@@y|#p||long key with spaces"####,
        r####"json={"7":8,"alpha":1,"beta":2,"@@x":3,"@@@y":4,"#p":5,"":6,"long key with spaces":9}["7","alpha","beta","@@x","@@@y","#p","","long key with spaces"]"####,
        r####"append-to-key=7012|alpha012|beta012/7|alpha|beta|@@x|@@@y|#p||long key with spaces"####,
        r####"append-keys0=alpha!0!1|alpha!0!1|alpha!0!1/alpha"####,
        r####"concat-chain=true:7,alpha,beta,@@x,@@@y,#p,,long key with spaces,"####,
        r####"delete-unvisited=abd"####,
        r####"add-during=ab"####,
        r####"proto-key-added=a,b"####,
        r####"non-enumerable-mid-loop=abc"####,
        r####"shadowed=bca"####,
        r####"many-keys=18000:k8999:200,5,200,5:9000"####,
        r####"types=string"####,
    ];
    assert_eq!(out.len(), expected.len(), "{out:#?}");
    for (got, want) in out.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
