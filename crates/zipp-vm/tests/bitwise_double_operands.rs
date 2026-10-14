//! Bitwise operators take a finite double on their int path (ToInt32 as a
//! truncate and a 32-bit wrap) instead of the full ToNumeric coercion. This
//! battery pins every operator over doubles at the ToInt32 edges (2^31,
//! 2^32, 2^53, 2^63 and beyond, -0, NaN, the infinities, denormals and shift
//! counts past 31) to Node's answers, in the interpreter, under GC stress
//! and with the default tiers.

const SOURCE: &str = r#"
"use strict";
// Bitwise operators over doubles at the ToInt32 edges, compared with Node.
var vals = [0, -0, 1.5, -1.5, 2.7, -2.7, 0.999, -0.999, 2147483647.9, 2147483648, -2147483648.5,
  -2147483649, 4294967295, 4294967296, 4294967297.25, -4294967297.75, 1e10 + 0.5, -1e10 - 0.5,
  9007199254740991, -9007199254740991, 9007199254740993, 2 ** 62, -(2 ** 62), 2 ** 63, -(2 ** 63),
  2 ** 64, 1e300, -1e300, NaN, Infinity, -Infinity, 5e-324, 123456789.123, 3, 31, 32, 33, -1];
var ops = [
  function (a, b) { return a | b; }, function (a, b) { return a & b; }, function (a, b) { return a ^ b; },
  function (a, b) { return a << b; }, function (a, b) { return a >> b; }, function (a, b) { return a >>> b; }
];
var out = [];
var h = 0;
for (var o = 0; o < ops.length; o++) {
  for (var i = 0; i < vals.length; i++) {
    for (var j = 0; j < vals.length; j++) {
      var r = ops[o](vals[i], vals[j]);
      h = (h * 31 + (r | 0)) | 0;
      if (j % 9 === 0) out.push(r);
    }
  }
}
console.log(h, out.length);
console.log(out.slice(0, 60).join(","));
console.log(out.slice(60).join(","));
// The rnd shape from regex-log-scan.
function mulberry32(seed) {
  var a = seed | 0;
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(12345), acc = 0;
for (var k = 0; k < 20000; k++) acc = (acc + ((rnd() * 1000) | 0)) | 0;
console.log(acc);
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
fn bitwise_double_operands_worker() {
    if std::env::var_os("ZIPP_BITWISE_DOUBLE_OPERANDS_WORKER").is_none() {
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
fn bitwise_double_operands_matches_node() {
    if std::env::var_os("ZIPP_BITWISE_DOUBLE_OPERANDS_WORKER").is_some() {
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
            .args(["bitwise_double_operands_worker", "--exact", "--nocapture"])
            .env("ZIPP_BITWISE_DOUBLE_OPERANDS_WORKER", "1")
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
