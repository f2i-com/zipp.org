//! Exactness and override coverage for hot Map mutation and primitive-string
//! case conversion through capture-first member calls.

use std::process::Command;

const SOURCE: &str = r#"
  "use strict";

  function mutate(map, serial) {
    let score = serial + 1;
    score = score + 2;
    score = score + 3;
    score = score - 6;
    const returned = map.set("k", serial);
    if (returned !== map) score = -999;
    if ((serial & 255) === 255) {
      const cleared = map.clear();
      if (cleared !== undefined) score = -998;
    }
    score = score + 4;
    score = score - 4;
    return score;
  }

  function upper(holder) {
    const text = holder.text.toUpperCase();
    let size = text.length;
    size = size + 1;
    size = size + 2;
    size = size + 3;
    size = size - 6;
    return text + ":" + size;
  }

  const map = new Map();
  const holder = { text: "ab" };
  let checksum = 0;
  let upperBytes = 0;
  for (let i = 0; i < 800; i++) {
    checksum = (checksum + mutate(map, i)) | 0;
    upperBytes += upper(holder).length;
  }
  console.log("base", checksum, upperBytes, map.size, map.get("k"), upper(holder));

  let ownSet = 0;
  let ownClear = 0;
  map.set = function () { ownSet++; return this; };
  map.clear = function () { ownClear++; return undefined; };
  const ownScore = mutate(map, 1) + mutate(map, 255);
  console.log("own", ownScore, ownSet, ownClear, map.get("k"));

  const savedSet = Map.prototype.set;
  const savedClear = Map.prototype.clear;
  let protoSet = 0;
  let protoClear = 0;
  const protoMap = new Map();
  Map.prototype.set = function () { protoSet++; return this; };
  const protoScoreA = mutate(protoMap, 2);
  Map.prototype.set = savedSet;
  Map.prototype.clear = function () { protoClear++; return undefined; };
  const protoScoreB = mutate(protoMap, 255);
  Map.prototype.clear = savedClear;
  console.log("proto", protoScoreA + protoScoreB, protoSet, protoClear, protoMap.get("k"));

  const upperDesc = Object.getOwnPropertyDescriptor(String.prototype, "toUpperCase");
  const lowerDesc = Object.getOwnPropertyDescriptor(String.prototype, "toLowerCase");
  let upperRuns = 0;
  String.prototype.toUpperCase = function () { upperRuns++; return "OVR"; };
  const overridden = upper(holder);
  Object.defineProperty(String.prototype, "toUpperCase", {
    configurable: true,
    get() {
      upperRuns++;
      return function () { return "GET"; };
    }
  });
  const accessor = upper(holder);
  String.prototype.toLowerCase = function () { return "low"; };
  const lower = "AB".toLowerCase();
  Object.defineProperty(String.prototype, "toUpperCase", upperDesc);
  Object.defineProperty(String.prototype, "toLowerCase", lowerDesc);
  console.log("string-overrides", overridden, accessor, upperRuns, lower);

  const realm = $262.createRealm().global;
  realm.eval(`
    let caseRuns = 0;
    String.prototype.toUpperCase = function () { caseRuns++; return "CHILD"; };
    function childUpper(holder) { return holder.text.toUpperCase(); }
    let last = "";
    for (let i = 0; i < 40; i++) last = childUpper({ text: "x" });
    this.caseResult = last + ":" + caseRuns;

    let setRuns = 0;
    Map.prototype.set = function () { setRuns++; return this; };
    const childMap = new Map();
    function childSet(map, i) { map.set("x", i); return i; }
    for (let i = 0; i < 40; i++) childSet(childMap, i);
    this.mapResult = setRuns + ":" + childMap.size;
  `);
  console.log("realm", realm.caseResult, realm.mapResult);
"#;

const EXPECTED: &[&str] = &[
    "base 319600 3200 1 799 AB:2",
    "own 256 2 1 799",
    "proto 257 1 1 255",
    "string-overrides OVR:3 GET:3 2 low",
    // The createRealm Map-constructor facade now installs the child realm's
    // Map prototype in every execution mode, so its live override is observed
    // and the child map remains empty.
    "realm CHILD:40 40:0",
];

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_TIERC_COLLECTION_STRING_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, EXPECTED);
}

#[test]
fn optimized_nojit_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        (
            "hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = Command::new(&exe)
            .args(["execution_mode_child", "--exact", "--nocapture"])
            .env("ZIPP_TIERC_COLLECTION_STRING_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_JITLOG")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .envs(env.iter().copied())
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{mode} child failed:\n{stdout}\n{stderr}"
        );
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if mode == "hot" {
            assert!(
                stderr.matches("Tier C").count() >= 2,
                "hot bodies did not compile:\n{stderr}"
            );
        }
    }
}
