//! `wm.get(k)`, `wm.set(k, v)`, `wm.has(k)` and `wm.delete(k)` on a
//! WeakMap run the WeakMap arm directly while %WeakMap.prototype%'s method
//! is the intrinsic and the receiver has neither an own shadow nor another
//! prototype. Each way to break that (an own method, a replaced prototype
//! method, a subclass, `setPrototypeOf`, a method restored) must be seen as
//! Node sees it, as must the arm's own answers: object, array, function
//! and symbol keys, keys that cannot be held weakly, deletes and re-adds,
//! and a collection between rounds. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }
var wm = new WeakMap();
var keys = [];
for (var i = 0; i < 300; i++) {
  var k = (i % 4 === 0) ? [i] : (i % 4 === 1) ? { w: i } : (i % 4 === 2) ? function () { return i; } : Symbol("s" + i);
  keys.push(k);
  wm.set(k, i * 2);
}
var h = 0, hits = 0;
for (var r = 0; r < 4; r++) {
  for (var j = 0; j < keys.length; j++) {
    var key = keys[(j * 7 + r) % keys.length];
    var v = wm.get(key);
    if (v !== undefined) { h = (h + v) | 0; hits++; }
    if (r === 1 && (j & 3) === 0) wm.delete(key);
    if (r === 2 && (j & 7) === 0) wm.set(key, j);
    if (wm.has(key)) hits++;
  }
  var junk = []; for (var g = 0; g < 3000; g++) junk.push({ g: g });
}
log("churn", h, hits, wm.get({}), wm.has({}), wm.delete({}));
log("bad-keys", catchName(function () { return wm.set(1, 2); }), catchName(function () { return wm.set("s", 2); }),
  catchName(function () { return wm.set(Symbol.for("reg"), 2); }), wm.get(1), wm.has("s"), wm.delete(null));
log("chain", wm.set(keys[0], 5) === wm, wm.get(keys[0]));

// An own `get` shadows the prototype's.
var own = new WeakMap([[keys[1], "a"]]);
own.get = function (k) { return "own:" + (k === keys[1]); };
log("own-shadow", own.get(keys[1]), own.has(keys[1]));
delete own.get;
log("own-removed", own.get(keys[1]));

// The prototype's method replaced, then restored.
var realHas = WeakMap.prototype.has;
WeakMap.prototype.has = function (k) { return "patched"; };
log("proto-patched", wm.has(keys[0]), own.has(keys[1]));
WeakMap.prototype.has = realHas;
log("proto-restored", wm.has(keys[0]), own.has(keys[1]));
var realSet = WeakMap.prototype.set;
var sets = 0;
WeakMap.prototype.set = function (k, v) { sets++; return realSet.call(this, k, v); };
for (var s = 0; s < 5; s++) wm.set(keys[s], s);
WeakMap.prototype.set = realSet;
log("set-counted", sets, wm.get(keys[3]));

// A subclass and a changed prototype.
class Counting extends WeakMap { get(k) { return "sub:" + super.get(k); } }
var sub = new Counting([[keys[2], 7]]);
log("subclass", sub.get(keys[2]), sub.has(keys[2]));
var moved = new WeakMap([[keys[3], 9]]);
Object.setPrototypeOf(moved, { get: function () { return "moved"; }, has: WeakMap.prototype.has });
log("set-proto", moved.get(keys[3]), moved.has(keys[3]));
log("generic-receiver", catchName(function () { return WeakMap.prototype.get.call(new Map(), keys[0]); }));
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
fn weakmap_method_direct_worker() {
    if std::env::var_os("ZIPP_WEAKMAP_DIRECT_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn weakmap_method_direct_matches_node() {
    if std::env::var_os("ZIPP_WEAKMAP_DIRECT_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc_stress) in [("default", false, false), ("interpreter", true, false), ("interpreter-gc-stress", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["weakmap_method_direct_worker", "--exact", "--nocapture"])
            .env("ZIPP_WEAKMAP_DIRECT_WORKER", "1")
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
