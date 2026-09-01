//! Multi-word Tier-C cross-call window-fill masks. These fixtures deliberately
//! use 70 parameters: `p69` lives beyond the first `u64`, so alternating full
//! and short calls catch stale high-word arguments while branches, native
//! bailouts, throws, nested calls, allocation/GC, and metering exercise every
//! transition around the selective fill.

use std::process::Command;

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
use std::process::Output;

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const CHILD_ENV: &str = "ZIPP_WIDE_CROSS_MASK_CHILD";
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const BUDGET_CHILD_ENV: &str = "ZIPP_WIDE_CROSS_BUDGET_CHILD";
const CORE_EXPECTED: &str = "wide-core:18908160:4110";
const NESTED_EXPECTED: &str = "wide-nested:2048:29317";
const DEOPT_EXPECTED: &str = "wide-deopt:5803:3:TypeError";

fn params70() -> String {
    (0..70)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn full_args(first: &str, second: &str, p2: &str, p3: &str, last: &str) -> String {
    let mut args = vec![
        first.to_owned(),
        second.to_owned(),
        p2.to_owned(),
        p3.to_owned(),
    ];
    args.extend((4..69).map(|_| "0".to_owned()));
    args.push(last.to_owned());
    assert_eq!(args.len(), 70);
    args.join(",")
}

fn core_expr() -> String {
    r#"
      (function(){
        "use strict";
        function wide(__PARAMS__) {
          var stale;
          let x = p0 | 0;
          if ((p1 & 1) === 0) x = (x + p2) | 0;
          else x = (x - p3) | 0;
          if ((p1 & 1) === 0) stale = p69;
          else if (stale !== undefined) return -1234567;
          if ((p1 & 7) === 3) x = (x ^ 23130) | 0;
          if (p69 === undefined) return (x ^ 85) | 0;
          return (x + p69) | 0;
        }
        let sum = 0, last = 0;
        for (let i = 0; i < 4096; i++) {
          if ((i & 4) === 0) last = wide(i, i, 3, 2);
          else last = wide(__FULL_ARGS__);
          sum = (sum + last) | 0;
        }
        return "wide-core:" + sum + ":" + last;
      })()
    "#
    .replace("__PARAMS__", &params70())
    .replace("__FULL_ARGS__", &full_args("i", "i", "3", "2", "17"))
}

fn nested_expr() -> String {
    let params = params70();
    r#"
      (function(){
        "use strict";
        function inner(__PARAMS__) {
          let x = (p0 + p2) | 0;
          if ((p1 & 2) !== 0) x = (x ^ 31337) | 0;
          return p69 === undefined ? (x ^ 17) | 0 : (x + p69) | 0;
        }
        function outer(__PARAMS__) {
          let y;
          if ((p1 & 1) === 0) y = inner(p0, p1, p2, p3);
          else y = inner(__FORWARD_ARGS__);
          return (y + 1) | 0;
        }
        let sum = 0, last = 0;
        for (let i = 0; i < 2048; i++) {
          if ((i & 4) === 0) last = outer(i, i, 5, 2);
          else last = outer(__FULL_ARGS__);
          sum = (sum ^ last) | 0;
        }
        return "wide-nested:" + sum + ":" + last;
      })()
    "#
    .replace("__PARAMS__", &params)
    .replace("__FORWARD_ARGS__", &params)
    .replace("__FULL_ARGS__", &full_args("i", "i", "5", "2", "23"))
}

fn deopt_throw_expr() -> String {
    r#"
      (function(){
        "use strict";
        function wide(__PARAMS__) {
          let object = { value: p0 };
          let array = [object.value, p1, p69];
          if ((p1 & 255) === 127) return null.missing;
          let mixed = object.value + p69;
          if ((p1 & 3) === 0) mixed = "v" + mixed;
          else if ((p1 & 3) === 1) mixed = mixed + 0.5;
          return (String(mixed).length + array.length) | 0;
        }
        let sum = 0, caught = 0, name = "";
        for (let i = 0; i < 768; i++) {
          let seed = (i % 3) === 0 ? i : ((i % 3) === 1 ? ("s" + (i & 7)) : (i + 0.25));
          try { sum = (sum + wide(__FULL_ARGS__)) | 0; }
          catch (error) { caught++; name = error.constructor.name; }
        }
        return "wide-deopt:" + sum + ":" + caught + ":" + name;
      })()
    "#
    .replace("__PARAMS__", &params70())
    .replace("__FULL_ARGS__", &full_args("seed", "i", "3", "2", "7"))
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn over_budget_expr() -> String {
    // Deliberately exceeds the analyzer's 1,024-register cap while remaining
    // a tiny, valid Tier-C body. The function should still compile and execute;
    // only selective filling must decline to the bounded full-fill fallback.
    let params = (0..1_030)
        .map(|i| format!("p{i}"))
        .collect::<Vec<_>>()
        .join(",");
    r#"
      (function(){
        "use strict";
        function capped(__PARAMS__) { return (p0 + 1) | 0; }
        let sum = 0;
        for (let i = 0; i < 512; i++) sum = (sum + capped(i)) | 0;
        return "wide-budget:" + sum;
      })()
    "#
    .replace("__PARAMS__", &params)
}

fn run_expr(expr: &str) -> String {
    let out = zipp_vm::run(&format!("console.log({expr});")).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output.len(), 1, "unexpected output: {:?}", out.output);
    out.output[0].clone()
}

#[test]
fn wide_mask_semantics_branch_and_high_missing_arg() {
    // A full argc=70 call leaves p69=17 in a reused window; the next argc=4
    // call must explicitly restore high-word p69 to undefined. `stale` is a
    // post-parameter register (>64): an even call writes it and the next odd
    // call reads it without a write, pinning a real bit in mask word two.
    assert_eq!(run_expr(&core_expr()), CORE_EXPECTED);
}

#[test]
fn wide_mask_semantics_nested_wide_calls() {
    // Both functions have >64-register windows. The inner site alternates a
    // four-argument call and a full forwarded call while the outer site does
    // the same, exercising stacked selective windows and their unwind order.
    assert_eq!(run_expr(&nested_expr()), NESTED_EXPECTED);
}

#[test]
fn wide_mask_semantics_deopt_throw_and_allocation() {
    // Mixed int/double/string Add inputs force native guards to bail and resume
    // in the interpreter; object/array/string allocations supply GC pressure;
    // null property reads unwind throws across the frame-free call helper.
    assert_eq!(run_expr(&deopt_throw_expr()), DEOPT_EXPECTED);
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn wide_mask_mechanism_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(run_expr(&core_expr()), CORE_EXPECTED);
    let (fast, full) = zipp_vm::cross_fill_stats();
    eprintln!("[wide-mask-test] fast={fast} full={full}");
    if std::env::var_os("ZIPP_NO_CROSSCALL_WIDE_MASK").is_some() {
        assert!(
            full > 3_000 && fast == 0,
            "wide off-switch did not restore full fills: fast={fast}, full={full}"
        );
    } else {
        assert!(
            fast > 3_000 && fast > full.saturating_mul(100),
            "wide selective fill did not engage: fast={fast}, full={full}"
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn wide_mask_budget_child() {
    if std::env::var_os(BUDGET_CHILD_ENV).is_none() {
        return;
    }
    assert_eq!(run_expr(&over_budget_expr()), "wide-budget:131328");
    let (fast, full) = zipp_vm::cross_fill_stats();
    eprintln!("[wide-mask-budget-test] fast={fast} full={full}");
    assert!(
        fast == 0 && full > 400,
        "over-budget wide analysis did not fail closed: fast={fast}, full={full}"
    );
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn mechanism_child(disabled: bool) -> Output {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["wide_mask_mechanism_child", "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("ZIPP_ICSTATS", "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "8")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_CROSSCALL")
        .env_remove("ZIPP_NO_CROSSCALL2")
        .env_remove("ZIPP_NO_CROSSCALL_WIDE_MASK");
    if disabled {
        cmd.env("ZIPP_NO_CROSSCALL_WIDE_MASK", "1");
    }
    cmd.output().expect("spawn wide-mask mechanism child")
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn assert_child(label: &str, out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "{label} child failed:\n{stdout}\n{stderr}"
    );
    stderr.into_owned()
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn wide_mask_mechanism_and_switch_are_pinned() {
    let enabled = assert_child("enabled", &mechanism_child(false));
    assert!(
        enabled.contains("wide cross-uninit mask regs=") && enabled.contains("words=2 marked=1"),
        "wide multi-word metadata was not installed:\n{enabled}"
    );

    let disabled = assert_child("disabled", &mechanism_child(true));
    assert!(
        !disabled.contains("wide cross-uninit mask regs="),
        "wide metadata survived its off-switch:\n{disabled}"
    );
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn wide_mask_budget_declines_before_install() {
    let out = Command::new(std::env::current_exe().expect("test binary path"))
        .args(["wide_mask_budget_child", "--exact", "--nocapture"])
        .env(BUDGET_CHILD_ENV, "1")
        .env("ZIPP_ICSTATS", "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JIT_THRESHOLD", "8")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_CROSSCALL")
        .env_remove("ZIPP_NO_CROSSCALL2")
        .env_remove("ZIPP_NO_CROSSCALL_WIDE_MASK")
        .output()
        .expect("spawn wide-mask budget child");
    let stderr = assert_child("budget", &out);
    assert!(
        stderr.contains("Tier C fn1 compiled"),
        "over-budget callee did not compile Tier C, making the cap test vacuous:\n{stderr}"
    );
    assert!(
        !stderr.contains("wide cross-uninit mask regs="),
        "over-budget callee installed wide metadata:\n{stderr}"
    );
}

#[test]
fn wide_mask_modes_remain_identical() {
    if std::env::var_os("ZIPP_WIDE_CROSS_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let modes: &[(&str, &[(&str, &str)])] = &[
        ("default", &[("ZIPP_JIT_THRESHOLD", "8")]),
        (
            "wide-off",
            &[
                ("ZIPP_JIT_THRESHOLD", "8"),
                ("ZIPP_NO_CROSSCALL_WIDE_MASK", "1"),
            ],
        ),
        (
            "w7-off",
            &[("ZIPP_JIT_THRESHOLD", "8"), ("ZIPP_NO_CROSSCALL2", "1")],
        ),
        ("cross-off", &[("ZIPP_NO_CROSSCALL", "1")]),
        ("no-inline", &[("ZIPP_NO_CALL_INLINE", "1")]),
        (
            "gc-stress",
            &[("ZIPP_JIT_THRESHOLD", "8"), ("ZIPP_GC_STRESS", "1")],
        ),
        ("interpreter", &[("ZIPP_NOJIT", "1")]),
    ];
    for (label, env) in modes {
        let mut cmd = Command::new(&exe);
        cmd.args(["wide_mask_semantics_", "--nocapture"])
            .env("ZIPP_WIDE_CROSS_MODE_CHILD", "1");
        for key in [
            "ZIPP_NOJIT",
            "ZIPP_NO_CROSSCALL",
            "ZIPP_NO_CROSSCALL2",
            "ZIPP_NO_CROSSCALL_WIDE_MASK",
            "ZIPP_NO_CALL_INLINE",
            "ZIPP_GC_STRESS",
            "ZIPP_JIT_THRESHOLD",
        ] {
            cmd.env_remove(key);
        }
        cmd.envs(env.iter().copied());
        let out = cmd.output().expect("spawn wide-mask mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{label} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(feature = "instrument")]
#[test]
fn wide_mask_meter_matches_interpreter_exactly() {
    use zipp_vm::embed;

    fn js_string(source: &str) -> String {
        format!("{source:?}")
    }
    fn run(interpreter_only: bool) -> (u64, String) {
        const BIG: u64 = 1_000_000_000;
        let mut state =
            embed::compile_script("void globalThis; void eval;").expect("meter bootstrap compiles");
        state.set_limits(BIG, None);
        if interpreter_only {
            state.disable_vm_jit();
        }
        state.run_init().expect("meter bootstrap runs");
        let before = state.steps_remaining();
        state
            .eval_in_context(&format!(
                "globalThis.__wide_mask_result=(0,eval)({})",
                js_string(&core_expr())
            ))
            .expect("wide meter source runs");
        let used = before - state.steps_remaining();
        let result = state
            .eval_in_context("String(globalThis.__wide_mask_result)")
            .expect("read wide meter result")
            .as_str()
            .expect("wide meter result is a string")
            .to_owned();
        (used, result)
    }

    let native = run(false);
    let interpreter = run(true);
    assert_eq!(native.1, CORE_EXPECTED);
    assert_eq!(native, interpreter);
}
