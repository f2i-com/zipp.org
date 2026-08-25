//! Exactness and ablation coverage for Tier-C unary numeric negation.

use std::process::Command;

const SOURCE: &str = r#"
    "use strict";
    let coercions = 0;
    function hot(value, index) {
      let out = -(value + index);
      out = out + 1;
      out = out - 1;
      out = out * 1;
      out = out / 1;
      out = out + 2;
      out = out - 2;
      out = out * 1;
      out = out / 1;
      out = out + 3;
      out = out - 3;
      return out;
    }
    let sum = 0;
    for (let i = 0; i < 12000; i++) sum += hot(i & 255, i & 31);
    const negZero = hot(0, 0) - hot(0, 0);
    const object = { valueOf() { coercions++; return 9; } };
    const coerced = hot(object, 1);
    console.log("neg", sum, 1 / -0, 1 / negZero, coerced, coercions);
"#;

fn expected() -> Vec<String> {
    vec!["neg -1712416 -Infinity Infinity -10 1".into()]
}

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_TIERC_NEG_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, expected());
}

#[test]
fn optimized_ablation_nojit_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("hot", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        (
            "neg_off",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_TIERC_NEG", "1")][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = Command::new(&exe)
            .args(["execution_mode_child", "--exact"])
            .env("ZIPP_TIERC_NEG_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_TIERC_NEG")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .envs(env.iter().copied())
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
