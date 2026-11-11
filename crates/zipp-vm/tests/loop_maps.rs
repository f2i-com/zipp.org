//! Map and WeakMap methods in hot loops (`vm/loop_tier.rs`: the method
//! handler's Map lane) against Node: `get`, `set`, `has` and `delete` with
//! keys of every kind (numbers, `-0`, `NaN`, strings, objects, Symbols,
//! `undefined`, `null`, booleans); a session map read in a called function
//! and written when a key is missing; a subclass that overrides `get`, a map
//! with an own `get`, and `Map.prototype.get` replaced and put back under
//! the loop; WeakMap methods with object keys and a key that is not an
//! object; a map that grows to sixty thousand entries, collections falling
//! due under the loop, and a cache cleared now and then; and Map methods
//! called through `call`, on a Map and on receivers that are not one. In the
//! interpreter with the runner entering as it does by itself, from every
//! loop's first back-edge (`ZIPP_LOOP_RUNNER_EAGER`) with and without the
//! lite loop and without frames of its own (`ZIPP_NO_LOOP_FRAMES`), and
//! switched off; by default; and with every function compiled at once.

const SOURCE: &str = r##"// Map and WeakMap methods in hot loops, in the loop's own frame and in functions it calls: get/set/has/delete,
// keys of every kind, a subclass that overrides get, an own `get` on one map, Map.prototype.get replaced and
// put back under the loop, a map that grows until a collection is due, and string building around them.
var out = [];

// 1. get/set/has/delete with numbers, -0, NaN, strings, objects, symbols and undefined as keys.
(function () {
  var m = new Map(), o1 = {}, o2 = {}, sy = Symbol("k");
  var keys = [1, -0, NaN, "a", "1", o1, o2, sy, undefined, null, true, 2.5];
  var t = 0;
  for (var i = 0; i < 6000; i++) {
    var k = keys[i % keys.length];
    m.set(k, (m.get(k) | 0) + 1);
    if (m.has(0)) t++;
    if ((i & 63) === 0) m.delete(keys[(i >> 6) % keys.length]);
  }
  out.push("kinds " + m.size + " " + t + " " + m.get(1) + " " + m.get(0) + " " + m.get(NaN) + " " + m.get(o2) + " " + m.get(sy));
})();

// 2. Sessions: a map read in a called function, written when missing; the value an object whose fields change.
(function () {
  var sessions = new Map(), made = 0;
  function sessionFor(id) { var s = sessions.get(id); if (s === undefined) { s = { id: id, hits: 0, last: "" }; sessions.set(id, s); made++; } return s; }
  var t = 0;
  for (var i = 0; i < 20000; i++) { var s = sessionFor(i & 1023); s.hits++; s.last = "p" + (i & 7); t = (t + s.hits + s.last.length) | 0; }
  out.push("sessions " + made + " " + sessions.size + " " + t);
})();

// 3. A subclass overriding get, a map with an own `get`, and Map.prototype.get replaced and put back.
(function () {
  class Counting extends Map { get(k) { this.reads = (this.reads | 0) + 1; return super.get(k); } }
  var c = new Counting([[1, 10], [2, 20]]);
  var own = new Map([[1, 100]]);
  own.get = function (k) { return -1; };
  var plain = new Map([[1, 1000]]);
  var maps = [c, own, plain];
  var t = 0;
  var realGet = Map.prototype.get;
  for (var i = 0; i < 6000; i++) {
    if (i === 3000) Map.prototype.get = function (k) { return 7; };
    if (i === 4500) Map.prototype.get = realGet;
    t = (t + maps[i % 3].get(1)) | 0;
  }
  out.push("overrides " + t + " " + c.reads);
})();

// 4. WeakMap: get/set/has/delete with object keys, a key that is not an object.
(function () {
  var wm = new WeakMap(), objs = [];
  for (var k = 0; k < 64; k++) objs.push({ k: k });
  var t = 0, bad = 0;
  for (var i = 0; i < 8000; i++) {
    var o = objs[i & 63];
    if (!wm.has(o)) wm.set(o, i);
    t = (t + wm.get(o)) | 0;
    if ((i & 255) === 0) wm.delete(objs[(i >> 8) & 63]);
    if (wm.get(i) !== undefined) bad++;
  }
  var threw = "none";
  try { wm.set(1, 1); } catch (e) { threw = e.constructor.name; }
  out.push("weak " + t + " " + bad + " " + threw);
})();

// 5. A map that grows to many entries (collections fall due under the loop), and a cache cleared now and then.
(function () {
  var m = new Map(), cache = new Map(), t = 0;
  for (var i = 0; i < 60000; i++) {
    m.set("k" + i, { v: i, s: "x" + (i & 15) });
    var key = "q" + (i & 127) + ":" + (i & 7);
    var body = cache.get(key);
    if (body === undefined) { body = key.toUpperCase() + "@" + (i & 7); cache.set(key, body); }
    t = (t + body.length) | 0;
    if ((i & 4095) === 0 && cache.size > 512) cache.clear();
  }
  out.push("grow " + m.size + " " + m.get("k59999").v + " " + m.get("k123").s + " " + cache.size + " " + t);
})();

// 6. Map methods called through a variable (`fn.call`), and a Map receiver that is not a Map.
(function () {
  var m = new Map([["a", 1]]), get = Map.prototype.get, t = 0, errs = 0;
  for (var i = 0; i < 4000; i++) {
    t += get.call(m, "a");
    try { get.call(i & 1 ? {} : new Set(), "a"); } catch (e) { errs += e instanceof TypeError ? 1 : 100; }
  }
  out.push("indirect " + t + " " + errs);
})();

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "kinds 12 5935 51 46 41 20 15\nsessions 1024 1024 245440\noverrides 1520000 2000\nweak 5465952 0 TypeError\ngrow 60000 59999 x11 128 428414\nindirect 4000 4000";

/// Every switch a mode below may set, cleared before each run.
const SWITCHES: [&str; 6] = [
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_LOOP_RUNNER_EAGER",
    "ZIPP_NO_LOOP_RUNNER",
    "ZIPP_NO_LOOP_FRAMES",
    "ZIPP_NO_LITE",
];

#[test]
fn loop_maps_worker() {
    if std::env::var_os("ZIPP_LOOP_MAPS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn loop_maps_match_node() {
    if std::env::var_os("ZIPP_LOOP_MAPS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let modes: [(&str, &[&str]); 7] = [
        ("default", &[]),
        ("interpreter", &["ZIPP_NOJIT"]),
        ("interpreter, runner from the first back-edge", &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER"]),
        (
            "interpreter, runner from the first back-edge, no lite loop",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LITE"],
        ),
        (
            "interpreter, runner from the first back-edge, no frame of its own",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LOOP_FRAMES"],
        ),
        ("interpreter, no runner", &["ZIPP_NOJIT", "ZIPP_NO_LOOP_RUNNER"]),
        ("compile-at-once", &["ZIPP_JIT_THRESHOLD"]),
    ];
    for (mode, vars) in modes {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["loop_maps_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_MAPS_WORKER", "1");
        for v in SWITCHES {
            command.env_remove(v);
        }
        for v in vars {
            command.env(v, "1");
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
