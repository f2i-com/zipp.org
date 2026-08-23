//! Focused parity and mechanism tests for the narrow exact Array+Int
//! `DeleteIndex` MEM lane. Every hot fixture crosses the normal OSR threshold.
//! Child-process probes prove the switch changes engagement while stdout stays
//! identical.

use std::process::Command;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = Command::new("node")
        .args(["-e", src])
        .output()
        .expect("node is available");
    assert!(
        out.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect()
}

const DELETE_HIT: &str = r#"
    var n = 2048;
    var a = new Array(n);
    for (var f = 0; f < n; f++) a[f] = f;
    var alias = a, sum = 0, truth = 0, bad = 0;
    for (var i = 0; i < n; i++) {
      sum = (sum + alias[i]) | 0;
      if (delete a[i]) truth++;
      if (alias[i] !== undefined) bad++;
      if (i in alias) bad++;
      if (alias.length !== n) bad++;
    }
    console.log(sum + "," + truth + "," + bad + "," + a.length + "," + Object.keys(a).length);
"#;

const DELETE_MATRIX: &str = r#"
    "use strict";
    var hits = 0;
    var proto = Object.create(Array.prototype);
    Object.defineProperty(proto, "100", {
      get: function () { hits++; return "P100"; }, configurable: true
    });
    var a = new Array(300);
    for (var f = 0; f < a.length; f++) a[f] = f;
    Object.setPrototypeOf(a, proto);
    var deleted = 0;
    for (var i = 0; i < a.length; i++) if (delete a[i]) deleted++;
    var before = hits;
    var inherited = a[100];

    var h = [1, , 3], vacuous = 0;
    for (var o = 0; o < 200; o++) if (delete h[1000 + o]) vacuous++;
    var hole = delete h[1];

    var sealed = [1, 2, 3], sealedThrows = 0;
    Object.seal(sealed);
    for (var s = 0; s < 60; s++) {
      try { delete sealed[s % 3]; }
      catch (e) { if (e instanceof TypeError) sealedThrows++; else throw e; }
    }

    var target = [1, 2, 3], traps = 0;
    var proxy = new Proxy(target, {
      deleteProperty: function (t, k) { traps++; return Reflect.deleteProperty(t, k); }
    });
    for (var p = 0; p < 80; p++) delete proxy[p % 3];

    var ta = new Uint8Array([1, 2, 3, 4]), taThrows = 0;
    for (var t = 0; t < 40; t++) {
      try { delete ta[t & 3]; }
      catch (e) { if (e instanceof TypeError) taThrows++; else throw e; }
    }

    var c = [10, 20], coercions = 0;
    var objectKey = {
      toString: function () { coercions++; return String(coercions & 1); }
    };
    for (var q = 0; q < 40; q++) {
      c[0] = 10; c[1] = 20;
      delete c[objectKey];
    }

    var neg = [1, 2];
    neg[-1] = 9;
    var negResult = delete neg[-1];
    var minus = [3, 4], minusZero = -0;
    var minusResult = delete minus[minusZero];
    var largeResult = delete minus[2147483648];

    function deleteMapped(x) {
      var args = arguments, ok = 0;
      for (var m = 0; m < 40; m++) {
        if (delete args[0]) ok++;
        x = m;
      }
      return ok + ":" + String(args[0]) + ":" + x + ":" + (0 in args);
    }

    console.log([
      "own=" + deleted + ":" + before + ":" + inherited + ":" + hits + ":" + (100 in a) + ":" + a.length,
      "vac=" + vacuous + ":" + hole + ":" + h.length,
      "seal=" + sealedThrows + ":" + sealed.join("|"),
      "proxy=" + traps + ":" + Object.keys(target).length,
      "ta=" + taThrows + ":" + (ta[0] + ta[1] + ta[2] + ta[3]),
      "coerce=" + coercions + ":" + String(c[0]) + ":" + String(c[1]),
      "neg=" + negResult + ":" + Object.prototype.hasOwnProperty.call(neg, "-1"),
      "minus0=" + minusResult + ":" + String(minus[0]) + ":" + minus.length + ":" + largeResult,
      "args=" + deleteMapped(1)
    ].join(","));
"#;

#[test]
fn array_delete_semantics_match_node() {
    let hit = run_ok(DELETE_HIT);
    assert_eq!(hit, node_output(DELETE_HIT));
    assert_eq!(hit, ["2096128,2048,0,2048,0"]);

    let matrix = run_ok(DELETE_MATRIX);
    assert_eq!(matrix, node_output(DELETE_MATRIX));
    assert_eq!(
        matrix,
        ["own=300:0:P100:1:true:300,vac=200:true:3,seal=60:1|2|3,proxy=80:0,ta=40:10,coerce=40:undefined:20,neg=true:false,minus0=true:undefined:2:true,args=40:undefined:39:false"]
    );
}

fn probe_child(test: &str, marker: &str, off: Option<&str>) -> String {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test, "--nocapture"])
        .env(marker, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_NO_JIT_ARRAY_DELETE");
    if let Some(flag) = off {
        cmd.env(flag, "1");
    }
    let out = cmd.output().expect("mechanism child runs");
    assert!(
        out.status.success(),
        "{test} ({off:?}) failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn mechanism_delete_probe() {
    if std::env::var_os("ZIPP_ARRAY_DELETE_PROBE").is_none() {
        return;
    }
    assert_eq!(run_ok(DELETE_HIT), ["2096128,2048,0,2048,0"]);
}

#[test]
fn delete_jitlog_proves_engagement_and_switch_restoration() {
    let on = probe_child("mechanism_delete_probe", "ZIPP_ARRAY_DELETE_PROBE", None);
    assert!(
        on.contains("MEM array DeleteIndex helper emitted"),
        "delete helper was not emitted:\n{on}"
    );
    let off = probe_child(
        "mechanism_delete_probe",
        "ZIPP_ARRAY_DELETE_PROBE",
        Some("ZIPP_NO_JIT_ARRAY_DELETE"),
    );
    assert!(
        !off.contains("MEM array DeleteIndex helper emitted"),
        "delete helper emitted with its switch off:\n{off}"
    );
    assert!(
        off.contains("DECLINED"),
        "switch off did not restore decline:\n{off}"
    );
}

fn rerun_semantics_with(env: &[(&str, &str)]) {
    if std::env::var_os("ZIPP_SPARSE_PATHS_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args([
        "--exact",
        "array_delete_semantics_match_node",
        "--nocapture",
    ])
    .env("ZIPP_SPARSE_PATHS_MODE_CHILD", "1")
    .env_remove("ZIPP_NO_JIT_ARRAY_DELETE")
    .env_remove("ZIPP_NOJIT")
    .env_remove("ZIPP_JIT_THRESHOLD")
    .env_remove("ZIPP_GC_STRESS");
    for &(k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("delete mode child runs");
    assert!(
        out.status.success(),
        "delete mode {env:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn zz_switches_threshold_interpreter_and_gc_stress_agree() {
    for mode in [
        vec![("ZIPP_NO_JIT_ARRAY_DELETE", "1")],
        vec![("ZIPP_NOJIT", "1")],
        vec![("ZIPP_JIT_THRESHOLD", "1")],
        vec![("ZIPP_GC_STRESS", "1")],
    ] {
        rerun_semantics_with(&mode);
    }
}
