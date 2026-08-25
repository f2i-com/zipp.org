//! Int/Int fast head for memory-tier `JumpIfNotLt` / `JumpIfNotLe`.
//!
//! The fast head is representation-only: signed i32 payloads branch directly,
//! while doubles, mixed numeric pairs, NaN and non-numbers enter the unchanged
//! generic path. Fresh child processes cover the default and off-switch modes
//! because the emitter latch is process-wide.

const CHILD_ENV: &str = "ZIPP_MEM_INT_CMPJUMP_CHILD";
const OFF_ENV: &str = "ZIPP_NO_MEM_INT_CMPJUMP";
const MARKER: &str = "[mem-int-cmpjump-test] ";

const SOURCE: &str = r#"
"use strict";
(function main() {
  // Includes signed i32 extremes/negatives, mixed Int/double pairs, NaN,
  // infinities and both zero signs through the same hot relational sites.
  const left = [
    -2147483648, -9, -1, -0, 0, 1, 17, 2147483647,
    1.5, 2.0, NaN, Infinity, -Infinity
  ];
  const right = [
    2147483647, -1, -9, 0, -0, 1.5, 16.5, -2147483648,
    2, 1, 7, Infinity, -Infinity
  ];
  let hash = 0;
  const first = [];
  for (let r = 0; r < 12000; r++) {
    const a = left[r % left.length];
    const b = right[(r * 7 + 3) % right.length];
    let mask = 0;
    if (a < b) mask |= 1;
    if (a <= b) mask |= 2;
    if (b < a) mask |= 4;
    if (b <= a) mask |= 8;
    hash = (Math.imul(hash, 33) + mask + (r % 7)) | 0;
    if (r < 26) first.push(mask);
  }
  console.log(hash + "|" + first.join(","));

  // The loop OSR-compiles while `limit` is an Int. It later becomes an object.
  // The native numeric guard must bail at the comparison ip without coercing;
  // the interpreter then replays it and calls valueOf exactly once per test.
  let coercions = 0;
  const lateLimit = { valueOf() { coercions++; return 5001; } };
  let i = 0;
  let sum = 0;
  let limit = 6000;
  while (i < limit) {
    sum = (sum + (i % 7)) | 0;
    i++;
    if (i === 5000) limit = lateLimit;
  }
  console.log(i + "|" + sum + "|" + coercions);
})();
"#;

fn node_output() -> Vec<String> {
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(SOURCE)
        .output()
        .expect("node on PATH");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn run_child(off: bool) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("--exact")
        .arg("mem_int_cmpjump_matches_generic_path")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD");
    if off {
        command.env(OFF_ENV, "1");
    } else {
        command.env_remove(OFF_ENV);
    }
    command.output().expect("spawn mem-int-cmpjump child")
}

#[test]
fn mem_int_cmpjump_matches_generic_path() {
    let expected = node_output();
    if std::env::var_os(CHILD_ENV).is_some() {
        let outcome = zipp_vm::run(SOURCE).expect("comparison fixture compiles");
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.output, expected);
        assert_eq!(outcome.output[1], "5001|14997|2");
        eprintln!("{MARKER}{}", outcome.output.join("||"));
        return;
    }

    let expected_marker = format!("{MARKER}{}", expected.join("||"));
    for off in [false, true] {
        let output = run_child(off);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && !stdout.contains("running 0 tests"),
            "mode off={off} failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains(&expected_marker),
            "mode off={off} omitted exact output marker:\n{stderr}"
        );
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        assert!(
            stderr.contains("MEM region"),
            "mode off={off} never exercised the memory JIT:\n{stderr}"
        );
    }
}
