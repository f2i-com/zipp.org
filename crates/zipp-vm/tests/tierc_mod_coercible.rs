//! Exactness and same-binary ablation coverage for Tier-C `%` and
//! RequireObjectCoercible.

use std::process::Command;

const SOURCE: &str = r#"
    "use strict";
    let leftCoercions = 0;
    let rightCoercions = 0;

    function remainder(a, b) {
      let pad = 1;
      pad = pad + 2;
      pad = pad * 3;
      pad = pad - 4;
      pad = pad / 5;
      pad = pad + 6;
      pad = pad * 7;
      pad = pad - 8;
      const result = a % b;
      return pad === 41 ? result : 12345;
    }

    function readX(value) {
      // A named property below already throws on nullish input through GetProp.
      // Keep an empty pattern too so this probe deliberately exercises the
      // standalone RequireObjectCoercible bytecode and its Tier-C gate.
      const {} = value;
      const { x } = value;
      const code = value.text.charCodeAt(0);
      let out = x + code - 65 + 1;
      out = out + 2;
      out = out + 3;
      out = out + 4;
      out = out + 5;
      out = out + 6;
      out = out + 7;
      out = out + 8;
      return out;
    }

    let checksum = 0;
    for (let i = 0; i < 12000; i++) {
      checksum = (checksum + remainder(i, 4) + readX({ x: i & 31, text: "A" })) | 0;
    }

    const left = { valueOf() { leftCoercions++; return 19; } };
    const right = { valueOf() { rightCoercions++; return 5; } };
    let nullError = "none";
    let undefinedError = "none";
    try { readX({ x: 1, text: null }); } catch (error) { nullError = error.name; }
    try { readX({ x: 1, text: undefined }); } catch (error) { undefinedError = error.name; }

    console.log(
      "tierc-primitives",
      checksum,
      remainder(left, right), leftCoercions, rightCoercions,
      remainder(7.5, 2),
      Number.isNaN(remainder(1, 0)),
      Object.is(remainder(-4, 2), -0),
      Object.is(remainder(-0, 3), -0),
      remainder(9007199254740991, 2147483659),
      readX({ x: 9, text: "A" }),
      nullError, undefinedError
    );
"#;

const EXPECTED: &str =
    "tierc-primitives 636000 4 1 1 1.5 true true true 2101346314 45 TypeError TypeError";

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_TIERC_PRIMITIVES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, [EXPECTED]);
}

#[test]
fn optimized_ablation_nojit_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        (
            "hot",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")][..],
        ),
        (
            "mod_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_MOD", "1"),
            ][..],
        ),
        (
            "coercible_off",
            &[
                ("ZIPP_JIT_THRESHOLD", "1"),
                ("ZIPP_JITLOG", "1"),
                ("ZIPP_NO_TIERC_CHECK_COERCIBLE", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let out = Command::new(&exe)
            .args(["execution_mode_child", "--exact", "--nocapture"])
            .env("ZIPP_TIERC_PRIMITIVES_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_JITLOG")
            .env_remove("ZIPP_NO_TIERC_MOD")
            .env_remove("ZIPP_NO_TIERC_CHECK_COERCIBLE")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .envs(env.iter().copied())
            .output()
            .expect("spawn mode child");
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
                "hot bodies did not compile:\n{stderr}"
            );
        } else if mode == "mod_off" {
            assert!(
                stderr.contains("op Mod (disabled)"),
                "mod switch did not reject:\n{stderr}"
            );
        } else if mode == "coercible_off" {
            assert!(
                stderr.contains("op CheckCoercible (disabled)"),
                "coercible switch did not reject:\n{stderr}"
            );
        }
    }
}
