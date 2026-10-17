//! A dictionary-mode object keeps a deleted property's slot as a hole
//! (`ObjMap::delete_at`) and drops its holes once they are half the slots.
//! Every way of reading the object back must see exactly its properties, in
//! spec order: enumeration (keys, values, entries, own names, Reflect.ownKeys,
//! for-in, JSON), copying (assign, spread, fromEntries), descriptors, freezing
//! and sealing, presence checks, re-adding deleted keys (they go last),
//! deleting during for-in, integer and symbol keys, `"length"`, accessors,
//! proxies over such an object, and churn across many rounds. The expected
//! lines are Node's. The same source runs under `ZIPP_PROP_HOLES=all`, which
//! leaves a hole for every delete from any object with owned keys.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function make(n, tag) {
  var d = {};
  for (var i = 0; i < n; i++) {
    if (i % 9 === 4) d[i * 7] = tag + i;
    else if (i % 13 === 6) d[String(1000 + i)] = tag + i;
    else d["k" + i + tag] = tag + i;
  }
  return d;
}
function hash(s) { var h = 7; for (var i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0; return h; }
var sym = Symbol("s"), sym2 = Symbol("t");
var d = make(80, "a");
d[sym] = "sym";
d.length = 3;
for (var i = 0; i < 80; i += 3) delete d["k" + i + "a"];
for (var i = 0; i < 80; i += 5) delete d[i * 7];
d[sym2] = "sym2";
log("keys", Object.keys(d).length, hash(Object.keys(d).join()));
log("first", Object.keys(d).slice(0, 8).join());
log("last", Object.keys(d).slice(-6).join());
log("values", Object.values(d).length, hash(Object.values(d).join()));
log("entries", Object.entries(d).length, Object.entries(d)[3].join("="), Object.entries(d)[40].join("="));
log("names", hash(Object.getOwnPropertyNames(d).join()));
log("ownKeys", Reflect.ownKeys(d).length, String(Reflect.ownKeys(d).slice(-2)[0]), String(Reflect.ownKeys(d).slice(-1)[0]));
log("symbols", Object.getOwnPropertySymbols(d).map(String).join());
log("json", hash(JSON.stringify(d)), JSON.stringify(d).length);
var fi = []; for (var k in d) fi.push(k);
log("forin", fi.length, hash(fi.join()));
log("in", "k1a" in d, "k3a" in d, "k0a" in d, 28 in d, 35 in d, "length" in d, d.length);
log("hasOwn", Object.hasOwn(d, "k2a"), d.hasOwnProperty("k6a"), Object.prototype.hasOwnProperty.call(d, 7 * 13));
var c = Object.assign({}, d);
log("assign", Object.keys(c).length, hash(Object.keys(c).join()), c[sym], c[sym2]);
var s = { ...d, extra: 1 };
log("spread", Object.keys(s).length, Object.keys(s).slice(-2).join(), s[sym]);
var fe = Object.fromEntries(Object.entries(d));
log("fromEntries", hash(JSON.stringify(fe)));
var desc = Object.getOwnPropertyDescriptors(d);
log("descriptors", Object.keys(desc).length, desc.k1a.value, desc.k1a.writable);
// re-adding a deleted key appends it
d.k3a = "back"; d[0] = "zero"; d.k6a = "back6";
log("readd", Object.keys(d).slice(-3).join(), Object.keys(d)[0], Object.keys(d)[1]);
// accessors and redefinition on a holey object
Object.defineProperty(d, "acc", { get: function () { return "got"; }, enumerable: true, configurable: true });
Object.defineProperty(d, "k1a", { value: "fixed", writable: false, enumerable: false, configurable: true });
log("define", d.acc, d.k1a, Object.keys(d).indexOf("k1a"), Object.keys(d).indexOf("acc") === Object.keys(d).length - 1);
log("delete results", delete d.k2a, delete d.nope, delete d[sym]);
Object.defineProperty(d, "fixedKey", { value: 1, configurable: false, enumerable: true });
log("non-configurable", delete d.fixedKey, "fixedKey" in d);
// delete during for-in: deleted keys not yet visited are skipped
var e = make(60, "b");
var seen = [];
for (var k in e) {
  seen.push(k);
  if (k === "k10b") { delete e.k20b; delete e.k30b; delete e.k5b; e.late = 1; }
}
log("forin delete", seen.length, seen.indexOf("k20b"), seen.indexOf("k5b") >= 0, seen.indexOf("late"));
// past half holes: compaction, then more of everything
var f = make(100, "c");
var fk = Object.keys(f);
for (var i = 0; i < fk.length; i++) if (i % 4 !== 1) delete f[fk[i]];
log("compacted", Object.keys(f).length, hash(Object.keys(f).join()), JSON.stringify(f).length);
f.z1 = 1; f[5] = "five"; delete f[fk[1]];
log("after", Object.keys(f).slice(0, 3).join(), Object.keys(f).slice(-2).join(), Object.keys(f).length);
// freeze and seal a holey object
var g = make(64, "d");
for (var i = 0; i < 64; i += 2) delete g["k" + i + "d"];
Object.freeze(g);
g.k1d = "changed"; delete g.k3d;
log("frozen", Object.isFrozen(g), g.k1d, "k3d" in g, Object.keys(g).length);
var h = make(64, "e");
for (var i = 1; i < 64; i += 2) delete h["k" + i + "e"];
Object.seal(h);
h.k0e = "still"; delete h.k2e; h.added = 1;
log("sealed", Object.isSealed(h), h.k0e, "k2e" in h, "added" in h, Object.keys(h).length);
// a proxy with no traps over a holey target
var t = make(50, "f");
for (var i = 0; i < 50; i += 2) delete t["k" + i + "f"];
var p = new Proxy(t, {});
log("proxy", Object.keys(p).length, hash(Object.keys(p).join()), "k1f" in p, "k2f" in p, p.k3f);
delete p.k5f;
log("proxy delete", "k5f" in t, Object.keys(t).length);
// emptied completely, then refilled
var u = make(40, "g");
Object.keys(u).forEach(function (k) { delete u[k]; });
log("emptied", Object.keys(u).length, JSON.stringify(u));
u.a = 1; u[2] = 2; u.b = 3;
log("refilled", Object.keys(u).join(), JSON.stringify(u));
// churn: many rounds of deletes and re-adds on one object
var w = {}, total = 0;
for (var r = 0; r < 30; r++) {
  for (var i = 0; i < 120; i++) w["w" + ((i * 7 + r) % 150)] = r * 1000 + i;
  for (var i = 0; i < 150; i += 1 + (r % 3)) delete w["w" + i];
  var ks = Object.keys(w);
  total = (total * 31 + ks.length + hash(ks.join())) | 0;
}
log("churn", total, Object.keys(w).length);
// JSON.parse reviver deleting properties of a big holder
var big = {}; for (var i = 0; i < 60; i++) big["p" + i] = i;
var revived = JSON.parse(JSON.stringify(big), function (k, v) { return k !== "" && +k.slice(1) % 3 === 0 ? undefined : v; });
log("reviver", Object.keys(revived).length, Object.keys(revived).slice(0, 4).join());
// Object.defineProperties takes its properties from a holey object
var props = {}; for (var i = 0; i < 40; i++) props["q" + i] = { value: i, enumerable: true };
for (var i = 0; i < 40; i += 2) delete props["q" + i];
var target = Object.defineProperties({}, props);
log("defineProperties", Object.keys(target).length, Object.keys(target).slice(0, 3).join());
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

#[test]
fn dict_delete_holes_worker() {
    if std::env::var_os("ZIPP_DICT_HOLES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The policy is latched per process, so each setting runs in a child.
#[test]
fn dict_delete_holes_match_node() {
    if std::env::var_os("ZIPP_DICT_HOLES_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, holes, nojit) in [
        ("interpreter", None, true),
        ("interpreter-holes-all", Some("all"), true),
        ("interpreter-holes-off", Some("0"), true),
        ("default-tiers-holes-all", Some("all"), false),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["dict_delete_holes_worker", "--exact", "--nocapture"])
            .env("ZIPP_DICT_HOLES_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_PROP_HOLES");
        if let Some(h) = holes {
            command.env("ZIPP_PROP_HOLES", h);
        }
        if nojit {
            command.env("ZIPP_NOJIT", "1");
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
