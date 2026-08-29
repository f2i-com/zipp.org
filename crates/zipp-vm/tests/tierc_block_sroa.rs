#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use std::process::{Command, Output};

const CHILD_ENV: &str = "ZIPP_TIERC_BLOCK_SROA_CHILD";
const DECLINE_CHILD_ENV: &str = "ZIPP_TIERC_BLOCK_SROA_DECLINE_CHILD";
#[cfg(feature = "instrument")]
const METER_CHILD_ENV: &str = "ZIPP_TIERC_BLOCK_SROA_METER_CHILD";
const MARKER: &str = "Tier-C block-SROA";

const SOURCE: &str = r#"
"use strict";
function project(value, which) {
  if (which) {
    const object = { number: value, text: "xx" };
    const alias = object;
    return (alias.number + alias.text.length) | 0;
  }
  const array = [value, 3];
  const alias = array;
  return (alias[0] + alias[1]) | 0;
}
let checksum = 0;
for (let i = 0; i < 4096; i++) checksum = (checksum + project(10, i & 1)) | 0;
console.log("tierc-block-sroa", checksum, 4096);
"#;

// Each allocation resembles an eligible site but violates a different closed
// proof: aggregate escape, intervening global effect, internal branch, and
// dynamic array index. None may be scalar-replaced.
const DECLINE_SOURCE: &str = r#"
"use strict";
var sink = null;
var ticks = 0;
function escape(value) {
  const array = [value, 3];
  sink = array;
  return array[0] | 0;
}
function effect(value) {
  const object = { number: value };
  ticks = (ticks + 1) | 0;
  return object.number | 0;
}
function control(value, which) {
  const object = { number: value };
  if (which) return object.number | 0;
  return (object.number + 1) | 0;
}
function dynamic(value, key) {
  const array = [value, 3];
  return array[key] | 0;
}
let checksum = 0;
for (let i = 0; i < 64; i++) {
  checksum = (checksum + escape(i) + effect(i) + control(i, i & 1) + dynamic(i, i & 1)) | 0;
}
console.log("tierc-block-sroa-decline", checksum, ticks, sink[0]);
"#;

fn assert_child(mode: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "Tier-C block-SROA {mode} child failed:\n--- stdout ---\n{}\n--- stderr ---\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    stderr
}

fn command_for(test: &str, child_env: &str) -> Command {
    let exe = std::env::current_exe().expect("test binary path");
    let mut command = Command::new(exe);
    command
        .args(["--exact", test, "--nocapture"])
        .env(child_env, "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env_remove("ZIPP_NO_TIERC_BLOCK_SROA")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NURSERY_VERIFY");
    command
}

#[test]
fn tierc_block_sroa_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let outcome = zipp_vm::run(SOURCE).expect("compile/run eligible source");
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.output, ["tierc-block-sroa 51200 4096"]);
}

#[test]
fn tierc_block_sroa_decline_child() {
    if std::env::var_os(DECLINE_CHILD_ENV).is_none() {
        return;
    }
    let outcome = zipp_vm::run(DECLINE_SOURCE).expect("compile/run decline source");
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.output, ["tierc-block-sroa-decline 7168 64 63"]);
}

#[test]
fn exact_lane_latch_nojit_and_gc_stress_paths() {
    for (mode, env, expected_marker) in [
        ("default", None, true),
        ("off", Some(("ZIPP_NO_TIERC_BLOCK_SROA", "1")), false),
        ("nojit", Some(("ZIPP_NOJIT", "1")), false),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1")), false),
    ] {
        let mut command = command_for("tierc_block_sroa_child", CHILD_ENV);
        if let Some((key, value)) = env {
            command.env(key, value);
        }
        if mode == "gc-stress" {
            command.env("ZIPP_NURSERY_VERIFY", "1");
        }
        let output = command.output().expect("run isolated Tier-C SROA mode");
        let stderr = assert_child(mode, &output);
        assert_eq!(
            stderr.contains(MARKER),
            expected_marker,
            "unexpected mechanism state in {mode}:\n{stderr}"
        );
        if expected_marker {
            assert!(
                stderr.contains("finalized=1 arrays=1 reads=4"),
                "mechanism counters do not describe both exact sites:\n{stderr}"
            );
        }
    }
}

#[test]
fn escape_effect_control_and_dynamic_index_decline() {
    let output = command_for("tierc_block_sroa_decline_child", DECLINE_CHILD_ENV)
        .output()
        .expect("run isolated decline source");
    let stderr = assert_child("decline", &output);
    assert!(
        !stderr.contains(MARKER),
        "a hostile site escaped the closed proof:\n{stderr}"
    );
}

#[cfg(feature = "instrument")]
#[test]
fn tierc_block_sroa_meter_child() {
    if std::env::var_os(METER_CHILD_ENV).is_none() {
        return;
    }
    let mut state = zipp_vm::embed::compile_script(SOURCE).expect("compile metered source");
    state.set_limits(u64::MAX, None);
    state.run_init().expect("run metered source");
    assert_eq!(state.take_output(), ["tierc-block-sroa 51200 4096"]);
}

#[cfg(feature = "instrument")]
#[test]
fn metered_vm_declines_block_sroa() {
    let output = command_for("tierc_block_sroa_meter_child", METER_CHILD_ENV)
        .output()
        .expect("run isolated metered source");
    let stderr = assert_child("metered", &output);
    assert!(
        !stderr.contains(MARKER),
        "metered VM installed an uncharged SROA plan:\n{stderr}"
    );
}
