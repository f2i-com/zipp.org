//! The fused string-index append must stay native from an interned seed while
//! retaining exact fallback behavior when the first-builder path is disabled.

use std::process::Command;

const SOURCE: &str = r#"
    "use strict";
    const copy = (input) => {
      let out = "";
      let i = 0;
      while (i < input.length) {
        out += input[i];
        i++;
      }
      return out;
    };
    let hash = 0;
    let last = "";
    for (let i = 0; i < 6000; i++) {
      last = copy((i & 1) === 0 ? "zipp" : "sandbox");
      hash = (Math.imul(hash ^ last.charCodeAt(i % last.length), 33) + last.length) | 0;
    }
    console.log("copy", hash, last);
"#;

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_APPEND_INDEX_FIRST_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, ["copy 453474347 sandbox"]);
}

#[test]
fn optimized_ablation_nojit_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("hot", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        (
            "first_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_NO_STR_APPEND_INDEX_FIRST", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = Command::new(&exe)
            .args(["execution_mode_child", "--exact"])
            .env("ZIPP_APPEND_INDEX_FIRST_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_STR_APPEND_INDEX_FIRST")
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
