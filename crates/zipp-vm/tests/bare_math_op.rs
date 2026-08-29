//! The BARE `MathOp` (B249): `Math.<op>(args…)` whose arguments are all
//! order-transparent compiles with NO captured `Math` receiver/callee pair —
//! the pre-hardening register layout — and validates the live `Math` global
//! slot and the live `Math.<op>` own data slot at execution instead, so a
//! replaced method, a rebound `Math`, an accessor, or a deleted slot are all
//! observed exactly as the captured form observes them.

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
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

/// Every observable replacement of `Math` / `Math.imul` / `Math.floor`, hit
/// after a JIT-warm loop so every tier's guard is exercised, compared with
/// node line by line.
const SEMANTICS: &str = r#"
  function imul3(a, b) { return Math.imul(a, b) | 0; }
  function fl(x) { return Math.floor(x / 7); }
  var acc = 0;
  for (var i = 0; i < 30000; i++) acc = (acc + imul3(i, 3) + fl(i)) | 0;
  console.log("warm:" + acc);
  var saved = Math.imul;
  Math.imul = function (a, b) { return 42 + a + b; };
  console.log("replaced:" + imul3(5, 6));
  Math.imul = saved;
  console.log("restored:" + imul3(5, 6));
  Object.defineProperty(Math, "imul", {
    get: function () { return function () { return 7; }; },
    configurable: true
  });
  console.log("accessor:" + imul3(5, 6));
  Object.defineProperty(Math, "imul", { value: saved, writable: true, configurable: true });
  console.log("data-again:" + imul3(5, 6));
  var savedMath = Math;
  Math = { imul: function () { return 99; }, floor: function () { return -1; } };
  console.log("rebound:" + imul3(5, 6) + ":" + fl(70));
  Math = savedMath;
  console.log("rebound-back:" + imul3(5, 6) + ":" + fl(70));
  delete Math.imul;
  var threw = false;
  try { imul3(1, 2); } catch (e) { threw = e instanceof TypeError; }
  console.log("deleted:" + threw);
  Math.imul = saved;
  console.log("final:" + imul3(5, 6));
  Math.floor = function (x) { return 1000; };
  console.log("floor-replaced:" + fl(70));
  var g = (function () { var Math = { imul: function () { return "local"; } }; return Math.imul(1, 2); })();
  console.log("shadowed:" + g);
  for (var j = 0; j < 30000; j++) acc = (acc + imul3(j, 5)) | 0;
  console.log("after:" + acc);
"#;

#[test]
fn bare_math_op_matches_node() {
    if std::env::var_os("ZIPP_BARE_MATH_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(SEMANTICS), node_output(SEMANTICS));
}

/// The compiled shape: transparent arguments take the bare form (no captured
/// pair), a call argument keeps the captured pair.
#[test]
fn bare_math_op_bytecode_shape() {
    let bare = zipp_vm::compile_to_text(
        "function f(a, b, o) { return Math.imul(a, b) + Math.floor(o.x * 2); } console.log(f(1,2,{x:3}));",
        false,
    )
    .expect("compiles");
    assert!(
        bare.contains("callee: 65535"),
        "transparent arguments should compile to the bare MathOp:\n{bare}"
    );
    let captured = zipp_vm::compile_to_text(
        "function g() { return 2; } function f(a) { return Math.imul(a, g()); } console.log(f(1));",
        false,
    )
    .expect("compiles");
    assert!(
        !captured.contains("callee: 65535"),
        "a call argument must keep the captured MathOp:\n{captured}"
    );
}

#[test]
fn bare_math_op_modes_match() {
    if std::env::var_os("ZIPP_BARE_MATH_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("strict-order", Some(("ZIPP_STRICT_CALL_ORDER", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "bare_math_op_matches_node", "--nocapture"])
            .env("ZIPP_BARE_MATH_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_STRICT_CALL_ORDER");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
