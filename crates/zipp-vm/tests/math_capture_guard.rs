//! The call-free `Math.imul` JIT identity guard is only a replacement for the
//! post-capture Rust check. Every source lookup must still run, and any miss
//! must call the exact callee/receiver captured before argument evaluation.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const SEMANTICS: &str = r#"
  function eq(actual, expected, label) {
    if (actual !== expected) {
      throw new Error(label + ": " + actual + " != " + expected);
    }
  }

  var mainMath = Math;
  var original = Math.imul;
  function hot(a, b) { return Math.imul(a, b); }

  // Compile the pristine exact pair before any mutation below.
  var warm = 0;
  for (var i = 0; i < 12000; i++) warm = (warm ^ hot(i, 33)) | 0;
  eq(hot(7, 9), 63, "warm result");

  // A guard baked while pristine must not hoist or skip later [[Get]] calls.
  // Returning the original native is still eligible after the getter ran.
  var gets = 0;
  Object.defineProperty(mainMath, "imul", {
    configurable: true,
    get: function () { gets++; return original; }
  });
  var accessorSum = 0;
  for (var j = 0; j < 9000; j++) accessorSum = (accessorSum + hot(j, 5)) | 0;
  eq(gets, 9000, "one accessor Get per call");
  Object.defineProperty(mainMath, "imul", {
    configurable: true,
    enumerable: false,
    writable: true,
    value: original
  });

  // The reference is captured before its arguments. Replacing the live
  // property while evaluating an argument must not redirect this call, but it
  // must affect the next call. The fallback must retain this === mainMath.
  function replacement(a, b) {
    return this === mainMath ? 700 + a + b : -1;
  }
  var mutate = false;
  function captureDuringArgs(a, b) {
    return Math.imul((mutate && (mainMath.imul = replacement), a), b);
  }
  for (var k = 0; k < 12000; k++) captureDuringArgs(2, 4);
  mutate = true;
  eq(captureDuringArgs(3, 4), 12, "captured original callee");
  mutate = false;
  eq(captureDuringArgs(3, 4), 707, "next call sees replacement");
  mainMath.imul = original;

  // The global/receiver is also live per activation. A same-name method on a
  // rebound namespace must receive that exact namespace as `this`.
  var customMath = {
    marker: 80,
    imul: function (a, b) { return this.marker + a + b; }
  };
  Math = customMath;
  eq(hot(2, 3), 85, "rebound namespace and this");
  Math = mainMath;
  eq(hot(6, 7), 42, "restored namespace");

  // Child-realm native ids intentionally match main-realm ids. Exact receiver
  // and function-realm guards must still decline; an abrupt completion exposes
  // which realm's native was actually called.
  var foreign = $262.createRealm().global;
  Math = foreign.Math;
  var foreignError = false;
  try {
    hot(Symbol("x"), 2);
  } catch (error) {
    foreignError = error.constructor === foreign.TypeError &&
                   error.constructor !== TypeError;
  }
  Math = mainMath;
  eq(foreignError, true, "foreign intrinsic realm");

  console.log("math-capture-guard:ok:" + (warm | 0) + ":" + (accessorSum | 0));
"#;

#[test]
fn math_capture_guard_child() {
    if std::env::var_os("ZIPP_MATH_CAPTURE_CHILD").is_none() {
        return;
    }
    let output = run_ok(SEMANTICS);
    assert_eq!(output.len(), 1);
    assert!(output[0].starts_with("math-capture-guard:ok:"));
}

#[test]
fn math_capture_guard_matches_in_all_modes() {
    if std::env::var_os("ZIPP_MATH_CAPTURE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("direct-guard-off", Some(("ZIPP_NO_DIRECT_MATH_GUARD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "math_capture_guard_child", "--nocapture"])
            .env("ZIPP_MATH_CAPTURE_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_DIRECT_MATH_GUARD");
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

#[test]
fn math_capture_guard_jitlog_proves_engagement_and_ablation() {
    if std::env::var_os("ZIPP_MATH_CAPTURE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let run = |direct_guard: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "math_capture_guard_child", "--nocapture"])
            .env("ZIPP_MATH_CAPTURE_CHILD", "1")
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_JIT_THRESHOLD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_DIRECT_MATH_GUARD");
        if !direct_guard {
            cmd.env("ZIPP_NO_DIRECT_MATH_GUARD", "1");
        }
        let out = cmd.output().expect("spawn JITLOG child");
        assert!(
            out.status.success(),
            "direct_guard={direct_guard} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    };

    let on = run(true);
    assert!(
        on.contains("TYPED-LANE (ops="),
        "captured Math fast path did not engage:\n{on}"
    );

    let off = run(false);
    assert!(
        off.contains("typed-lane=DECLINED(math-guard-missing)"),
        "Math guard ablation did not reach its explicit decline:\n{off}"
    );
    assert!(
        !off.contains("TYPED-LANE (ops="),
        "captured Math typed lane remained active under ablation:\n{off}"
    );
    assert!(
        off.contains("MEM region"),
        "ordinary fallback did not compile under Math guard ablation:\n{off}"
    );
}
