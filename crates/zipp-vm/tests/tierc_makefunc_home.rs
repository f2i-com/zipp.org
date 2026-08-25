//! Exactness, inherited EvalScope, home-object/`super`, GC and same-binary
//! ablation coverage for Tier-C capture-free `MakeFunc` + `SetHomeObject`.

use std::process::Command;

const SOURCE: &str = r#"
    const base = { read() { return this.serial + 5; } };
    function make(serial) {
      return {
        serial,
        method() { return super.read(); }
      };
    }

    let checksum = 0;
    let hold;
    for (let i = 0; i < 1800; i++) {
      hold = make(i);
      checksum = (checksum + hold.serial) | 0;
    }
    Object.setPrototypeOf(hold, base);
    console.log("home", checksum, hold.method(),
                Object.getPrototypeOf(hold.method) === Function.prototype);

    // `factory` is compile-time/main-program code, but its callable is born in
    // an activation carrying a sloppy direct-eval scope. Its native MakeFunc
    // must copy that exact scope onto every returned `read` method.
    function outer(seed) {
      function factory(i) {
        return { i, read() { return hidden + this.i; } };
      }
      eval("var hidden = seed + 40;");
      return factory;
    }
    const factory = outer(2);
    let evalSum = 0;
    let evalHold;
    for (let i = 0; i < 1000; i++) {
      evalHold = factory(i);
      evalSum = (evalSum + evalHold.i) | 0;
    }
    console.log("eval-scope", evalSum, evalHold.read());

    // The new lane must never absorb MakeArrow: lexical new.target remains an
    // interpreter-owned construction path.
    function C(value) {
      this.value = value;
      this.arrow = () => new.target === C;
    }
    const instance = new C(9);
    console.log("lexical", instance.value, instance.arrow());
"#;

const EXPECTED: [&str; 3] = [
    "home 1619100 1804 true",
    "eval-scope 499500 1041",
    "lexical 9 true",
];

fn run_child(env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["execution_child", "--exact", "--nocapture"])
        .env("ZIPP_TIERC_MAKEFUNC_CHILD", "1")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_JITLOG")
        .env_remove("ZIPP_NO_TIERC_MAKEFUNC_HOME")
        .env_remove("ZIPP_NO_DENSE_CLOSURE_HOME")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_GC_STRESS");
    cmd.envs(env.iter().copied());
    cmd.output().expect("spawn mode child")
}

#[test]
fn execution_child() {
    if std::env::var_os("ZIPP_TIERC_MAKEFUNC_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, EXPECTED);
}

#[test]
fn optimized_ablation_nojit_and_gc_modes_match() {
    for (mode, env) in [
        (
            "hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        (
            "lane_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_MAKEFUNC_HOME", "1"),
            ][..],
        ),
        (
            "dense_home_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_DENSE_CLOSURE_HOME", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = run_child(env);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{mode} child failed:\n{stdout}\n{stderr}"
        );
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if mode == "hot" {
            assert!(
                stderr.matches("Tier C").count() >= 2,
                "literal factories did not compile:\n{stderr}"
            );
            assert!(
                !stderr.contains("[tierC-reject] op MakeFunc")
                    && !stderr.contains("[tierC-reject] op SetHomeObject"),
                "enabled lane still rejected a literal op:\n{stderr}"
            );
        } else if mode == "lane_off" {
            assert!(
                stderr.contains("MakeFunc/SetHomeObject (disabled)"),
                "off switch did not reject the lane:\n{stderr}"
            );
        }
    }
}

/// The helper allocation path never bypasses the embedding heap ceiling. A
/// metered VM declines this whole Tier-C body, so instruction and memory limits
/// remain owned by the exact interpreter route.
#[cfg(feature = "instrument")]
#[test]
fn metered_literal_methods_remain_resource_bounded() {
    use zipp_vm::embed;

    let mut state = embed::compile_script("var ready = true;").expect("bootstrap compiles");
    state.run_init().expect("bootstrap runs");
    state.set_limits(50_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 400_000);

    let err = state
        .eval_in_context(
            "(function(){function make(i){return {i,m(){return this.i}}}var a=[];for(var i=0;i<1000000;i++)a.push(make(i));return a.length})()",
        )
        .expect_err("literal methods must hit the memory budget");
    assert!(err.contains("memory budget"), "unexpected error: {err}");
    assert!(
        state.steps_remaining() > 0,
        "memory, not steps, must stop it"
    );
}
