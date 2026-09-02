//! B258: a pool-bound literal shell keeps its slab cell at death and the thin
//! birth path fills that cell in place when the next literal is of the same
//! class. The retention is a value-store implementation detail: with
//! `ZIPP_NO_SHELL_CELL=1` the cell goes home at death exactly as before, and
//! every execution mode must print the same JS output either way.
//!
//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the
//! interpreter-only and sandbox profiles never emit; compiled only where that
//! tier and the slab exist, like the other tier-pinning suites.
#![cfg(all(not(feature = "safe-sandbox"), feature = "jit", target_arch = "x86_64"))]

use std::process::{Command, Output};

const CHILD_ENV: &str = "ZIPP_SHELL_CELL_CHILD";

/// One literal site, one plan, one slab class: every pooled shell carries a
/// class-0 cell and every pooled birth can fill it in place.
const STABLE: &str = r#"
  "use strict";
  (function () {
    const epochs = 200;
    const width = 384;
    const survivors = [];

    function makeObject(value, kind) {
      return { value: value, kind: kind, left: value ^ 85, right: value + 3 };
    }

    function touch(object, delta) {
      object.value = (object.value + delta) | 0;
      return (object.value ^ object.left ^ object.right) | 0;
    }

    let checksum = 0;
    for (let epoch = 0; epoch < epochs; epoch++) {
      const batch = new Array(width);
      for (let i = 0; i < width; i++) {
        batch[i] = makeObject((epoch * width + i) | 0, i & 15);
      }
      for (let i = 0; i < width; i++) {
        checksum = (checksum + touch(batch[i], (epoch + i) & 7)) | 0;
        if (((epoch * width + i) % 97) === 0) survivors.push(batch[i]);
      }
      if (survivors.length > 900 && (epoch & 31) === 0) {
        survivors.splice(0, 300);
      }
    }

    for (let i = 0; i < survivors.length; i++) {
      checksum = (checksum ^ survivors[i].value ^ survivors[i].kind) | 0;
    }
    console.log("shell-cell-stable", checksum, survivors.length, epochs * width);
  })();
"#;

/// Five literal sites across four slab classes at one churn loop, so pooled
/// shells meet literals of another class (the cell goes home at the pop),
/// plus spilled stores (`o.extra = i` moves a slab store to a `Vec`, and the
/// shell still pools) and dictionary-mode maps (`delete o.a`, never pooled).
/// Every field of every object is read back, so a wrong cell is a wrong sum.
const MIXED: &str = r#"
  "use strict";
  (function () {
    const rounds = 24000;
    let checksum = 0;
    const keep = [];

    function two(i) { return { a: i, b: i ^ 7 }; }
    function three(i) { return { a: i, b: i + 1, c: i + 2 }; }
    function five(i) { return { a: i, b: i + 1, c: i + 2, d: i + 3, e: i + 4 }; }
    function six(i) { return { a: i, b: i + 1, c: i + 2, d: i + 3, e: i + 4, f: i + 5 }; }
    function nine(i) { return { a: i, b: 1, c: 2, d: 3, e: 4, f: 5, g: 6, h: 7, k: i + 8 }; }

    function sum(o) {
      let s = 0;
      for (const k in o) s = (s + o[k]) | 0;
      return s;
    }

    for (let i = 0; i < rounds; i++) {
      let o;
      switch (i % 5) {
        case 0: o = two(i); break;
        case 1: o = three(i); break;
        case 2: o = five(i); break;
        case 3: o = six(i); break;
        default: o = nine(i);
      }
      if ((i & 15) === 3) o.extra = i;
      if ((i & 63) === 9) delete o.a;
      checksum = (checksum + sum(o) + i) | 0;
      if ((i % 13) === 0) keep.push(o);
      if (keep.length > 400) keep.splice(0, 200);
    }

    for (let i = 0; i < keep.length; i++) checksum = (checksum ^ sum(keep[i])) | 0;
    console.log("shell-cell-mixed", checksum, keep.length);
  })();
"#;

/// Small enough to run under `ZIPP_GC_STRESS=1` (a collection per allocation),
/// with two classes and a spill.
const SMALL: &str = r#"
  "use strict";
  (function () {
    let checksum = 0;
    const keep = [];

    function four(i) { return { a: i, b: i ^ 3, c: i + 1, d: i - 1 }; }
    function five(i) { return { a: i, b: 1, c: 2, d: 3, e: i + 4 }; }

    for (let i = 0; i < 240; i++) {
      const o = (i & 1) ? four(i) : five(i);
      if ((i % 7) === 3) o.extra = i;
      checksum = (checksum + o.a + o.b + o.c + o.d + (o.e | 0) + (o.extra | 0)) | 0;
      if ((i % 5) === 0) keep.push(o);
      if (keep.length > 20) keep.splice(0, 10);
    }

    for (let i = 0; i < keep.length; i++) checksum = (checksum ^ keep[i].a) | 0;
    console.log("shell-cell-small", checksum, keep.length);
  })();
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellStats {
    on: bool,
    reused: u64,
    mismatched: u64,
}

const OFF: CellStats = CellStats {
    on: false,
    reused: 0,
    mismatched: 0,
};

fn run_child(program: &str, env: &[(&str, &str)]) -> Output {
    let exe = std::env::current_exe().expect("test executable path");
    let mut cmd = Command::new(exe);
    cmd.args(["shell_cell_child", "--exact", "--nocapture"])
        .env(CHILD_ENV, program)
        .env("ZIPP_GCSTATS", "1")
        .env_remove("ZIPP_NO_SHELL_CELL")
        .env_remove("ZIPP_NO_THIN_ALLOC")
        .env_remove("ZIPP_NO_SETTLED_ALLOC")
        .env_remove("ZIPP_STATIC_KEY_STATS")
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
    cmd.output().expect("shell-cell child runs")
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
        .find_map(|line| line.trim().strip_prefix("SHELL_CELL_OUTPUT "))
        .unwrap_or_else(|| panic!("missing JS output marker in:\n{stdout}"))
}

fn cell_stats(stderr: &str) -> CellStats {
    let line = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("[shellcell] "))
        .unwrap_or_else(|| panic!("missing shell-cell counters in:\n{stderr}"));
    let mut stats = OFF;
    for field in line.split_whitespace() {
        let Some((name, raw)) = field.split_once('=') else {
            continue;
        };
        match name {
            "on" => stats.on = raw == "true",
            "reused" => stats.reused = raw.parse().expect("reused counter"),
            "mismatched" => stats.mismatched = raw.parse().expect("mismatched counter"),
            _ => {}
        }
    }
    stats
}

/// Runs `program` under the child's environment and returns its JS output.
fn output_under(program: &str, env: &[(&str, &str)]) -> String {
    let label = format!("{program} {env:?}");
    let (stdout, _) = assert_child(&label, &run_child(program, env));
    js_output(&stdout).to_owned()
}

#[test]
fn shell_cell_child() {
    let Some(program) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let source = match program.to_str() {
        Some("stable") => STABLE,
        Some("mixed") => MIXED,
        Some("small") => SMALL,
        other => panic!("unknown shell-cell child program {other:?}"),
    };
    let out = zipp_vm::run(source).expect("shell-cell source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(
        out.output.len(),
        1,
        "unexpected JS output: {:?}",
        out.output
    );
    println!("SHELL_CELL_OUTPUT {}", out.output[0]);
}

#[test]
fn stable_churn_fills_cells_in_place_and_the_latch_returns_them_at_death() {
    let (on_stdout, on_stderr) = assert_child("default", &run_child("stable", &[]));
    let (off_stdout, off_stderr) = assert_child(
        "latched off",
        &run_child("stable", &[("ZIPP_NO_SHELL_CELL", "1")]),
    );
    assert_eq!(js_output(&on_stdout), js_output(&off_stdout));

    let on = cell_stats(&on_stderr);
    assert!(
        on.on && on.reused > 0,
        "the one-class churn did not fill any pooled cell in place: {on:?}"
    );
    assert_eq!(
        cell_stats(&off_stderr),
        OFF,
        "the latch must restore return-at-death and count nothing"
    );
}

#[test]
fn mixed_classes_fill_matching_cells_and_return_the_rest_at_the_pop() {
    let (on_stdout, on_stderr) = assert_child("default", &run_child("mixed", &[]));
    let (off_stdout, off_stderr) = assert_child(
        "latched off",
        &run_child("mixed", &[("ZIPP_NO_SHELL_CELL", "1")]),
    );
    assert_eq!(js_output(&on_stdout), js_output(&off_stdout));

    let on = cell_stats(&on_stderr);
    assert!(
        on.on && on.reused > 0 && on.mismatched > 0,
        "the four-class churn did not exercise both the in-place fill and the mismatch strip: {on:?}"
    );
    assert_eq!(cell_stats(&off_stderr), OFF);
}

#[test]
fn every_execution_mode_agrees_with_the_latch_on_and_off() {
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_NOJIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NO_NURSERY", "1")],
        &[("ZIPP_NO_THIN_ALLOC", "1")],
    ];
    for program in ["stable", "mixed"] {
        let reference = output_under(program, &[]);
        for mode in modes {
            for latch in [&[][..], &[("ZIPP_NO_SHELL_CELL", "1")][..]] {
                let mut env: Vec<(&str, &str)> = mode.to_vec();
                env.extend_from_slice(latch);
                assert_eq!(
                    output_under(program, &env),
                    reference,
                    "{program} under {env:?} diverged from the default run"
                );
            }
        }
    }
    // The fold: without the thin paths there is no in-place fill, so the
    // general birth path (which strips at the pop) is what the comparator
    // measures, and the counters read off.
    let (_, stderr) = assert_child(
        "thin off",
        &run_child("stable", &[("ZIPP_NO_THIN_ALLOC", "1")]),
    );
    assert_eq!(
        cell_stats(&stderr),
        OFF,
        "the retention must fold off with the thin paths"
    );
}

#[test]
fn gc_stress_agrees_on_a_small_program() {
    let reference = output_under("small", &[]);
    let (stress_stdout, stress_stderr) =
        assert_child("gc stress", &run_child("small", &[("ZIPP_GC_STRESS", "1")]));
    assert_eq!(js_output(&stress_stdout), reference);
    let stress = cell_stats(&stress_stderr);
    assert!(
        stress.on && stress.reused > 0,
        "a collection per allocation did not exercise the in-place fill: {stress:?}"
    );
    for env in [
        &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NO_SHELL_CELL", "1")][..],
        &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..],
        &[
            ("ZIPP_GC_STRESS", "1"),
            ("ZIPP_NOJIT", "1"),
            ("ZIPP_NO_SHELL_CELL", "1"),
        ][..],
        &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NO_NURSERY", "1")][..],
    ] {
        assert_eq!(
            output_under("small", env),
            reference,
            "small program under {env:?} diverged from the default run"
        );
    }
}
