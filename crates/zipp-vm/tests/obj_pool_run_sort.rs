//! Run-aware address ordering for the object-shell and dense-array pools.
//!
//! The optimization is an ordering implementation detail: every accepted
//! run proof and the kill-switch fallback must leave the pools in the same
//! ascending-address order and therefore produce identical JS behavior.

use std::process::{Command, Output};

const CHILD_ENV: &str = "ZIPP_OBJ_POOL_RUN_SORT_CHILD";

const SOURCE: &str = r#"
  "use strict";
  (function () {
    const epochs = 90;
    const width = 5000;
    let survivors = [];
    let checksum = 0;

    function makeNode(serial) {
      const bias = serial & 255;
      return {
        serial,
        label: "node-" + (serial & 1023),
        values: [bias, bias ^ 85, (bias + 17) & 255],
        apply(delta) {
          return (this.serial + this.values[delta % 3] + delta) | 0;
        }
      };
    }

    for (let epoch = 0; epoch < epochs; epoch++) {
      const fresh = new Array(width);
      for (let i = 0; i < width; i++) {
        const serial = epoch * width + i;
        const node = makeNode(serial);
        fresh[i] = node;
        checksum = (checksum ^ node.apply(epoch + i)) | 0;
        if ((serial % 11) === 0) survivors.push(node);
      }

      for (let i = epoch & 7; i < survivors.length; i += 29) {
        checksum = (checksum + survivors[i].apply(epoch)) | 0;
      }

      if (survivors.length > 24000) {
        survivors = survivors.slice(survivors.length >> 1);
      }
    }

    for (let i = 0; i < survivors.length; i += 7) {
      checksum = (checksum ^ survivors[i].serial ^ survivors[i].label.length) | 0;
    }
    console.log("obj-pool-run-sort", checksum, survivors.length, epochs * width);
  })();
"#;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SortStats {
    obj_run: u64,
    obj_full: u64,
    arr_run: u64,
    arr_full: u64,
}

fn run_child(env: &[(&str, &str)]) -> Output {
    let exe = std::env::current_exe().expect("test executable path");
    let mut cmd = Command::new(exe);
    cmd.args(["obj_pool_run_sort_child", "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("ZIPP_GCSTATS", "1")
        .env_remove("ZIPP_NO_OBJ_POOL_RUN_SORT")
        .env_remove("ZIPP_NO_OBJ_POOL_SORT")
        .env_remove("ZIPP_NO_OBJ_POOL")
        .env_remove("ZIPP_NO_OBJ_POOL_MAJOR")
        .env_remove("ZIPP_NO_VAL_SLAB")
        .env_remove("ZIPP_NO_SHELL_REFIT")
        .env_remove("ZIPP_NO_OBJECT_FINALIZE")
        .env_remove("ZIPP_NO_STATIC_KEY_PLANS")
        .env_remove("ZIPP_NO_NURSERY")
        .env_remove("ZIPP_NO_NURSERY_ADAPT")
        .env_remove("ZIPP_NURSERY_YOUNG_BUDGET")
        .env_remove("ZIPP_NURSERY_MAX_MINORS")
        .env_remove("ZIPP_NO_PRETENURE")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD");
    cmd.envs(env.iter().copied());
    cmd.output().expect("pool-sort child runs")
}

fn assert_child(label: &str, out: &Output) -> (String, String) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "{label} child failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    (stdout.into_owned(), stderr.into_owned())
}

fn js_output(stdout: &str) -> &str {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("OBJ_POOL_RUN_SORT_OUTPUT "))
        .unwrap_or_else(|| panic!("missing JS output marker in:\n{stdout}"))
}

fn sort_stats(stderr: &str) -> SortStats {
    let line = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("[poolsort] "))
        .unwrap_or_else(|| panic!("missing pool-sort counters in:\n{stderr}"));
    let mut stats = SortStats::default();
    for field in line.split_whitespace() {
        let Some((name, raw)) = field.split_once('=') else {
            continue;
        };
        let value = raw
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("invalid pool-sort counter {field:?}"));
        match name {
            "obj_run" => stats.obj_run = value,
            "obj_full" => stats.obj_full = value,
            "arr_run" => stats.arr_run = value,
            "arr_full" => stats.arr_full = value,
            _ => {}
        }
    }
    stats
}

#[test]
fn obj_pool_run_sort_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("pool-sort source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(
        out.output.len(),
        1,
        "unexpected JS output: {:?}",
        out.output
    );
    println!("OBJ_POOL_RUN_SORT_OUTPUT {}", out.output[0]);
}

#[test]
fn run_sort_matches_full_sort_and_proves_both_pool_routes() {
    let (default_stdout, default_stderr) = assert_child("run sort", &run_child(&[]));
    let (fallback_stdout, fallback_stderr) = assert_child(
        "full-sort fallback",
        &run_child(&[("ZIPP_NO_OBJ_POOL_RUN_SORT", "1")]),
    );
    let (unsorted_stdout, unsorted_stderr) = assert_child(
        "address-sort fallback",
        &run_child(&[("ZIPP_NO_OBJ_POOL_SORT", "1")]),
    );

    let expected = js_output(&default_stdout);
    assert_eq!(js_output(&fallback_stdout), expected);
    assert_eq!(js_output(&unsorted_stdout), expected);

    let enabled = sort_stats(&default_stderr);
    assert!(
        enabled.obj_run > 0 && enabled.arr_run > 0,
        "the survival-shaped workload did not exercise both run proofs: {enabled:?}"
    );

    let disabled = sort_stats(&fallback_stderr);
    assert_eq!(disabled.obj_run, 0, "kill switch used object run sort");
    assert_eq!(disabled.arr_run, 0, "kill switch used array run sort");
    assert!(
        disabled.obj_full > 0 && disabled.arr_full > 0,
        "kill switch did not restore both full sorts: {disabled:?}"
    );

    assert_eq!(
        sort_stats(&unsorted_stderr),
        SortStats::default(),
        "disabling address ordering should bypass both sort implementations"
    );
}
