//! Tier-C whole-function returns normally use the direct `NO_BAIL` epilogue.
//! A Return reached with a structured `PushFinally` still active is different:
//! the interpreter must create a return completion, run every iterator-close /
//! user-finally handler, and only then pop the frame. These cases pin explicit
//! value returns, ReturnUndefined, a catch return under an outer finally, and
//! an inner-finally return overriding a pending completion while an outer
//! finally remains active, across Tier C, the iterator-lane comparator, the
//! interpreter, and GC stress.

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
use std::process::{Command, Output};

const CHILD_ENV: &str = "ZIPP_TIERC_PROTECTED_RETURN_CHILD";
const RESULT_MARKER: &str = "tierc-protected-return-result:";

const SOURCE: &str = r#"
  "use strict";

  let finalized = 0;
  function protectedValue(n) {
    try {
      for (const value of [1]) {
        return n === 0 ? value : protectedValue(n - 1) + value;
      }
      return -1000;
    } finally {
      finalized++;
    }
  }
  let total = 0;
  for (let i = 0; i < 1000; i++) total += protectedValue(3);
  console.log("protected", total, finalized);

  let undefinedFinalized = 0;
  function protectedUndefined(n) {
    try {
      if (n === 0) return;
      return protectedUndefined(n - 1);
    } finally {
      undefinedFinalized++;
    }
  }
  let undefinedReturns = 0;
  for (let i = 0; i < 1000; i++) {
    if (protectedUndefined(3) === undefined) undefinedReturns++;
  }
  console.log("undefined", undefinedReturns, undefinedFinalized);

  let innerFinalized = 0;
  let outerFinalized = 0;
  function nestedFinally(n) {
    try {
      try {
        if (n === 0) return 1;
        return nestedFinally(n - 1) + 1;
      } finally {
        innerFinalized++;
      }
    } finally {
      outerFinalized++;
    }
  }
  let nestedTotal = 0;
  for (let i = 0; i < 1000; i++) nestedTotal += nestedFinally(3);
  console.log("nested", nestedTotal, innerFinalized, outerFinalized);

  let catchFinalized = 0;
  function returnFromCatch(shouldThrow) {
    try {
      try {
        if (shouldThrow) throw "caught";
        return 3;
      } catch (error) {
        return error === "caught" ? 7 : -100;
      }
    } finally {
      catchFinalized++;
    }
  }
  let catchTotal = 0;
  for (let i = 0; i < 1000; i++) {
    catchTotal += returnFromCatch((i & 1) === 0);
  }
  console.log("catch", catchTotal, catchFinalized);

  let overrideInner = 0;
  let overrideOuter = 0;
  function overridePending(pending) {
    try {
      try {
        if (pending) return 7;
      } finally {
        overrideInner++;
        return pending ? 11 : 13;
      }
    } finally {
      overrideOuter++;
    }
  }
  let overrideTotal = 0;
  for (let i = 0; i < 1000; i++) {
    overrideTotal += overridePending((i & 1) === 0);
  }
  console.log("override", overrideTotal, overrideInner, overrideOuter);
"#;

const EXPECTED: [&str; 5] = [
    "protected 4000 4000",
    "undefined 1000 4000",
    "nested 4000 4000 4000",
    "catch 5000 1000",
    "override 12000 1000 1000",
];

fn execute_source() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, EXPECTED);
    out.output
}

#[test]
fn tierc_protected_return_child() {
    if std::env::var_os(CHILD_ENV).is_some() {
        println!("{RESULT_MARKER}{}", execute_source().join("|"));
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn run_child(mode: &str, env: &[(&str, &str)]) -> (String, String) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args([
        "tierc_protected_return_child",
        "--exact",
        "--nocapture",
    ])
    .env(CHILD_ENV, mode)
    .env("ZIPP_JIT_THRESHOLD", "8")
    .env("ZIPP_JITLOG", "1");
    for key in [
        "ZIPP_NO_TIERC_ITER",
        "ZIPP_NOJIT",
        "ZIPP_GC_STRESS",
        "ZIPP_NURSERY_VERIFY",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(env.iter().copied());
    let out = cmd.output().expect("spawn protected-return child");
    child_result(mode, out)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child_result(mode: &str, out: Output) -> (String, String) {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "{mode} child failed:\n{stdout}\n{stderr}"
    );
    let result = stdout
        .lines()
        .find_map(|line| line.strip_prefix(RESULT_MARKER))
        .unwrap_or_else(|| panic!("{mode} emitted no result marker:\n{stdout}\n{stderr}"))
        .to_owned();
    (result, stderr)
}

#[test]
fn protected_returns_match_iterator_off_nojit_and_gc_stress() {
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    {
        let expected = EXPECTED.join("|");
        let (default, jit_log) = run_child("default", &[]);
        assert_eq!(default, expected);
        // Function ids are source-order stable here: fn4 contains PushHandler
        // and is intentionally interpreter-owned today; the four handler-only
        // protected-return bodies must themselves reach Tier C.
        for func_id in [1, 2, 3, 5] {
            let marker = format!("Tier C fn{func_id} compiled");
            assert!(
                jit_log.contains(&marker),
                "protected-return fn{func_id} never compiled through Tier C:\n{jit_log}"
            );
        }

        let (iterator_off, _) = run_child("iterator-off", &[("ZIPP_NO_TIERC_ITER", "1")]);
        let (nojit, _) = run_child("nojit", &[("ZIPP_NOJIT", "1")]);
        let (gc_stress, _) = run_child(
            "gc-stress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NURSERY_VERIFY", "1")],
        );
        assert_eq!(iterator_off, default);
        assert_eq!(nojit, default);
        assert_eq!(gc_stress, default);
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    execute_source();
}
