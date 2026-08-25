//! Collection name-dispatch must observe ordinary own/prototype overrides in
//! every tier; receiver kind alone never proves that a builtin was selected.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const SOURCE: &str = r#"
  "use strict";

  const ownMap = new Map([["x", 1]]);
  ownMap.get = function (key) { return key === "x" ? 7 : -1; };

  const accessorMap = new Map([["x", 2]]);
  let getterRuns = 0;
  Object.defineProperty(accessorMap, "get", {
    configurable: true,
    get() {
      getterRuns++;
      return function () { return 9; };
    }
  });

  const customProtoMap = new Map([["x", 3]]);
  Object.setPrototypeOf(customProtoMap, {
    get() { return 13; }
  });

  const ownSet = new Set([1]);
  ownSet.has = function (value) { return value === 99; };

  const ownWriter = new Map();
  let writes = 0;
  ownWriter.set = function (key, value) {
    writes += key.length + value;
    return this;
  };

  function readMap(map) {
    let sum = 0;
    for (let i = 0; i < 12000; i++) sum += map.get("x");
    return sum;
  }
  function readSet(set) {
    let sum = 0;
    for (let i = 0; i < 12000; i++) sum += set.has(99) ? 1 : 0;
    return sum;
  }
  function writeMap(map) {
    for (let i = 0; i < 12000; i++) map.set("k", 2);
  }

  const own = readMap(ownMap);
  const accessor = readMap(accessorMap);
  const custom = readMap(customProtoMap);
  const setOwn = readSet(ownSet);
  writeMap(ownWriter);

  Map.prototype.get = function () { return 11; };
  const protoMap = new Map([["x", 4]]);
  const proto = readMap(protoMap);

  Set.prototype.has = function () { return false; };
  const protoSet = new Set([99]);
  const setProto = readSet(protoSet);

  console.log(
    "collection-overrides:" + own + ":" + accessor + ":" + getterRuns +
    ":" + custom + ":" + setOwn + ":" + writes + ":" + proto + ":" + setProto
  );
"#;

const WANT: &str = "collection-overrides:84000:108000:12000:156000:12000:36000:132000:0";

#[test]
fn collection_method_overrides_are_observed() {
    assert_eq!(run_ok(SOURCE), [WANT]);
}

#[test]
fn collection_method_override_modes_are_identical() {
    let exe = std::env::current_exe().expect("test binary path");
    for (name, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "collection_method_overrides_are_observed",
            "--exact",
            "--nocapture",
        ])
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_GC_STRESS");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} mode failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
