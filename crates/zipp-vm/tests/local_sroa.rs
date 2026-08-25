#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const SRC: &str = r#"
"use strict";
(function main() {
  const rounds = 50000;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    const point = { x: i & 1023, y: (i * 3) & 2047, tag: "p" + (i & 31) };
    const pair = [point.x + point.y, point.tag.length];
    checksum = (checksum + pair[0] + pair[1]) | 0;
  }
  console.log("local-sroa", checksum, rounds);
})();
"#;

// The huge finite Number is valid input to JS ToInt32 but deliberately outside
// the region emitter's guarded i32 conversion lane. The guard sits after the
// virtual concat/object/array construction, forcing the internal-bail path to
// recreate those values before the original Bitwise instruction resumes.
const DEOPT_SRC: &str = r#"
"use strict";
(function main() {
  const rounds = 1000;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    const point = { x: i & 1023, y: (i * 3) & 2047, tag: "p" + (i & 31) };
    const pair = [point.x + point.y, point.tag.length];
    checksum = (1e100 + pair[0] + pair[1]) | 0;
  }
  console.log("local-sroa-deopt", checksum, rounds);
})();
"#;

#[test]
fn local_sroa_child() {
    if std::env::var_os("ZIPP_LOCAL_SROA_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SRC).expect("compile/run");
    assert_eq!(out.error, None);
    assert_eq!(out.output, ["local-sroa 76681282 50000"]);
}

#[test]
fn local_sroa_deopt_child() {
    if std::env::var_os("ZIPP_LOCAL_SROA_DEOPT_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(DEOPT_SRC).expect("compile/run");
    assert_eq!(out.error, None);
    assert_eq!(out.output, ["local-sroa-deopt 0 1000"]);
}

#[test]
fn optimized_and_off_switch_paths_are_exact_and_the_lane_engages() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, local_off, concat_off, expected_concat) in [
        ("default", false, false, Some(1)),
        ("concat-off", false, true, Some(0)),
        ("local-off", true, false, None),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "local_sroa_child", "--nocapture"])
            .env("ZIPP_LOCAL_SROA_CHILD", "1")
            .env("ZIPP_JITLOG", "1")
            .env_remove("ZIPP_NO_LOCAL_SROA")
            .env_remove("ZIPP_NO_LOCAL_CONCAT_LEN");
        if local_off {
            cmd.env("ZIPP_NO_LOCAL_SROA", "1");
        }
        if concat_off {
            cmd.env("ZIPP_NO_LOCAL_CONCAT_LEN", "1");
        }
        let out = cmd.output().expect("run isolated JIT mode");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "local-SROA {mode} mode failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            stderr
        );
        match expected_concat {
            Some(count) => assert!(
                stderr.contains(&format!("concat_lens={count}")),
                "unexpected LOCAL-SROA log in {mode} mode:\n{stderr}"
            ),
            None => assert!(
                !stderr.contains("LOCAL-SROA region"),
                "local kill switch still installed the lane:\n{stderr}"
            ),
        }
    }
}

#[test]
fn virtual_concat_is_materialized_exactly_on_an_internal_guard_bail() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "local_sroa_deopt_child", "--nocapture"])
        .env("ZIPP_LOCAL_SROA_DEOPT_CHILD", "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_LOCAL_SROA")
        .env_remove("ZIPP_NO_LOCAL_CONCAT_LEN")
        .output()
        .expect("run isolated deopt case");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "virtual-concat deopt failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(
        stderr.contains("concat_lens=1"),
        "virtual concat lane did not engage:\n{stderr}"
    );
    assert!(
        stderr.contains("deopt at ip"),
        "the post-construction guard did not exercise materialization:\n{stderr}"
    );
}
