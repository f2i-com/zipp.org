//! Guarded same-FuncProto leaf inlining with exact default-parameter fallback.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const PROBE: &str = r#"
  "use strict";
  let defaultRuns = 0;

  function makeInvoker() {
    let target = function rotate(value, amount = (defaultRuns++, 5)) {
      return (value << amount) | (value >>> (32 - amount));
    };
    const original = target;
    function invoke(value, amount) {
      let pad = (value ^ value) | 0;
      pad = (pad + 3) | 0;
      pad = (pad * 5) | 0;
      pad = (pad - 15) | 0;
      pad = (pad ^ 91) | 0;
      pad = (pad ^ 91) | 0;
      const result = target((value + pad) | 0, amount);
      return (result - pad) | 0;
    }
    return [invoke, function (next) { target = next; }, original];
  }

  const pairs = [];
  for (let i = 0; i < 16; i++) pairs.push(makeInvoker());
  let checksum = 0;
  for (let i = 0; i < 80000; i++) {
    checksum = (checksum + pairs[i & 15][0](i, (i & 7) + 1)) | 0;
  }

  const pair = pairs[0];
  const invoke = pair[0];
  const set = pair[1];
  const original = pair[2];
  const numeric = invoke(0x12345678, 7);
  // Both reach the SAME inner Call bytecode as the hot path, with its second
  // argument undefined. The inline guard must run the real default prologue.
  const explicitUndefined = invoke(3, undefined);
  const missing = invoke(3);

  function other(value, amount = 9) { return (value + amount) | 0; }
  set(other);
  const crossProto = invoke(10, 2);
  set(original.bind(null));
  const bound = invoke(9, 1);
  const boundDefault = invoke(3, undefined);
  set(Math.abs);
  const native = invoke(-7, 2);
  // A sloppy child-realm target must receive that realm's global object. It is
  // deliberately routed through the same hot call bytecode after planning;
  // the same-prototype guard additionally rejects any child-realm live target
  // before using its baked main-realm `this`.
  const realm = $262.createRealm().global;
  realm.key = 700;
  const realmTarget = realm.eval("(function(value, amount) { return (this.key + value + amount) | 0; })");
  set(realmTarget);
  const realmThis = invoke(2, 3);
  set(17);
  let nonCallableThrew = false;
  try { invoke(1, 2); }
  catch (e) { nonCallableThrew = e instanceof TypeError; }

  // Distinct arrows share a FuncProto too, but their lexical-this Values are
  // instance state. The polymorphic lane must remain excluded for this shape.
  function Owner(key) {
    this.key = key;
    this.make = function () {
      const arrow = (value, amount = 5) => (this.key + value + amount) | 0;
      return function (value, amount) {
        let pad = (value ^ value) | 0;
        pad = (pad + 1) | 0;
        pad = (pad - 1) | 0;
        pad = (pad ^ 7) | 0;
        pad = (pad ^ 7) | 0;
        return (arrow(value + pad, amount) - pad) | 0;
      };
    };
  }
  const arrows = [];
  for (let i = 0; i < 8; i++) arrows.push(new Owner(100 + i).make());
  let arrowSum = 0;
  for (let i = 0; i < 20000; i++) {
    arrowSum = (arrowSum + arrows[i & 7](i & 31, 2)) | 0;
  }
  const arrowDefault = arrows[7](1, undefined);

  console.log(
    "poly-leaf:" + checksum + ":" + numeric + ":" + explicitUndefined +
    ":" + missing + ":" + crossProto + ":" + bound + ":" +
    boundDefault + ":" + native + ":" + realmThis + ":" + nonCallableThrew + ":" +
    defaultRuns + ":" + arrowSum + ":" + arrowDefault
  );
"#;

const WANT: &str = "poly-leaf:2146897088:439041033:96:96:12:18:96:7:705:true:3:2420000:113";

#[test]
fn guarded_poly_leaf_defaults_and_fallbacks_are_exact() {
    assert_eq!(run_ok(PROBE), [WANT]);
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn guarded_poly_leaf_mechanism_child() {
    if std::env::var_os("ZIPP_POLY_LEAF_CHILD").is_some() {
        assert_eq!(run_ok(PROBE), [WANT]);
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn guarded_poly_leaf_mechanism_engages() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args([
            "guarded_poly_leaf_mechanism_child",
            "--exact",
            "--nocapture",
        ])
        .env("ZIPP_POLY_LEAF_CHILD", "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_POLY_LEAF_INLINE")
        .output()
        .expect("spawn mechanism child");
    assert!(
        out.status.success(),
        "mechanism child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("SAME-PROTO-INLINE (default_mask=0x2)"),
        "same-proto/default leaf did not engage:\n{stderr}"
    );
}

#[test]
fn guarded_poly_leaf_modes_are_identical() {
    let exe = std::env::current_exe().expect("test binary path");
    for (name, env) in [
        ("default", None),
        ("off", Some(("ZIPP_NO_POLY_LEAF_INLINE", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "guarded_poly_leaf_defaults_and_fallbacks_are_exact",
            "--exact",
            "--nocapture",
        ])
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_POLY_LEAF_INLINE")
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
