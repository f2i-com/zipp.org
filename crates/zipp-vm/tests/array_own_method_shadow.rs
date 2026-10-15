//! An array's OWN method property must win over %Array.prototype%'s intrinsic
//! in compiled code as it does in the interpreter: `arr.push = fn; arr.push(x)`
//! calls `fn`. The JIT's `push` helper (`jit_array_push`) checked element
//! overrides and integrity levels but not a named own `push` in the array's
//! side table, so a hot `b.push(i)` appended to the array instead of calling
//! the shadow. Every common Array.prototype method is exercised here, hot, with
//! the shadow installed before the loop and halfway through it; the expected
//! output is Node's.

const SOURCE: &str = r#"
"use strict";
// An array's OWN method property must win over %Array.prototype%'s intrinsic in compiled code too:
// `arr.push = fn; arr.push(x)` calls fn. Each method is called hot (so the JIT compiles the call),
// once on a plain array and once on an array with an own shadow, and the shadow is installed both
// before the loop and in the middle of it.
var out = [];
var names = ["push", "pop", "shift", "unshift", "indexOf", "includes", "join", "slice", "concat",
  "map", "filter", "forEach", "reduce", "some", "every", "find", "findIndex", "at", "fill", "reverse",
  "sort", "splice", "lastIndexOf", "keys", "flat"];
function run(name, shadowAt) {
  var a = [3, 1, 2];
  var b = [3, 1, 2];
  var got = 0, own = 0;
  if (shadowAt === 0) b[name] = function () { own++; return "own"; };
  for (var i = 0; i < 2000; i++) {
    if (i === shadowAt) b[name] = function () { own++; return "own"; };
    var r;
    switch (name) {
      case "push": r = b.push(i); if (a.length < 10) a.push(i); break;
      case "pop": r = b.pop(); a.pop(); a.push(i); break;
      case "shift": r = b.shift(); a.shift(); a.push(i); break;
      case "unshift": r = b.unshift(i); if (a.length < 10) a.unshift(i); break;
      case "indexOf": r = b.indexOf(2); a.indexOf(2); break;
      case "includes": r = b.includes(2); a.includes(2); break;
      case "join": r = b.join("-"); a.join("-"); break;
      case "slice": r = b.slice(1); a.slice(1); break;
      case "concat": r = b.concat([i]); a.concat([i]); break;
      case "map": r = b.map(function (x) { return x + 1; }); a.map(function (x) { return x; }); break;
      case "filter": r = b.filter(function (x) { return x > 1; }); a.filter(Boolean); break;
      case "forEach": r = b.forEach(function () {}); a.forEach(function () {}); break;
      case "reduce": r = b.reduce(function (p, c) { return p + c; }, 0); a.reduce(function (p, c) { return p + c; }, 0); break;
      case "some": r = b.some(function (x) { return x > 2; }); a.some(Boolean); break;
      case "every": r = b.every(function (x) { return x > 0; }); a.every(Boolean); break;
      case "find": r = b.find(function (x) { return x === 2; }); a.find(Boolean); break;
      case "findIndex": r = b.findIndex(function (x) { return x === 2; }); a.findIndex(Boolean); break;
      case "at": r = b.at(-1); a.at(-1); break;
      case "fill": r = b.fill(0, 5); a.fill(0, 5); break;
      case "reverse": r = b.reverse(); a.reverse(); break;
      case "sort": r = b.sort(); a.sort(); break;
      case "splice": r = b.splice(5, 0); a.splice(5, 0); break;
      case "lastIndexOf": r = b.lastIndexOf(2); a.lastIndexOf(2); break;
      case "keys": r = b.keys(); a.keys(); break;
      case "flat": r = b.flat(); a.flat(); break;
    }
    if (typeof r === "number") got = (got + r) | 0;
    else if (typeof r === "string") got = (got + r.length) | 0;
    else if (r && typeof r === "object" && "length" in r) got = (got + r.length) | 0;
  }
  return name + "@" + shadowAt + " own=" + own + " got=" + got + " a=" + a.length + " b=" + (Array.isArray(b) ? b.length : "?");
}
for (var n = 0; n < names.length; n++) {
  out.push(run(names[n], 0));
  out.push(run(names[n], 1000));
}
for (var q = 0; q < out.length; q++) console.log(out[q]);
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
fn array_own_method_shadow_worker() {
    if std::env::var_os("ZIPP_ARRAY_OWN_METHOD_SHADOW_WORKER").is_none() {
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
fn array_own_method_shadow_matches_node() {
    if std::env::var_os("ZIPP_ARRAY_OWN_METHOD_SHADOW_WORKER").is_some() {
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
            .args(["array_own_method_shadow_worker", "--exact", "--nocapture"])
            .env("ZIPP_ARRAY_OWN_METHOD_SHADOW_WORKER", "1")
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
