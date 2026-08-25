//! Differential coverage for captured writes compiled by a loop region or a
//! leaf-call splice. `UpvalSet` is PutValue through an outer environment: a
//! captured `const`, named-function-expression self binding, and TDZ cell are
//! not equivalent to the declaring scope's unconditional `CellSet`.
//!
//! The mode sweep is process-based because JIT environment switches are
//! intentionally latched. Threshold 1 makes the native path take over almost
//! immediately; NOJIT is the interpreter oracle; NO_CALL_INLINE isolates the
//! per-op region helper from the leaf splice.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const HOT: usize = 3000;

#[test]
fn upvalset_parity_const_leaf_splice_throws_without_mutating() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        function make() {{
          const value = 7;
          return function write(next) {{ value = next; return value; }};
        }}
        const write = make();
        let caught = 0;
        for (let i = 0; i < {HOT}; i++) {{
          try {{ write(i); }} catch (e) {{
            if (e.constructor.name === "TypeError") caught++;
          }}
        }}
        console.log(caught);
        "#
    ));
    assert_eq!(out, vec![HOT.to_string()]);
}

#[test]
fn upvalset_parity_const_region_throws_without_mutating() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        function run() {{
          const value = 11;
          let caught = 0;
          function spin() {{
            for (let i = 0; i < {HOT}; i++) {{
              try {{ value = i; }} catch (e) {{
                if (e.constructor.name === "TypeError") caught++;
              }}
            }}
          }}
          spin();
          return caught + ":" + value;
        }}
        console.log(run());
        "#
    ));
    assert_eq!(out, vec![format!("{HOT}:11")]);
}

#[test]
fn upvalset_parity_named_function_binding_obeys_strictness() {
    let out = run_ok(&format!(
        r#"
        var sloppy = function self() {{
          var ok = 0;
          function spin() {{
            for (var i = 0; i < {HOT}; i++) {{
              self = 0;
              if (typeof self === "function") ok++;
            }}
          }}
          spin();
          return ok;
        }};
        var strict = function self() {{
          "use strict";
          var caught = 0;
          function spin() {{
            for (var i = 0; i < {HOT}; i++) {{
              try {{ self = 0; }} catch (e) {{
                if (e.constructor.name === "TypeError") caught++;
              }}
            }}
          }}
          spin();
          return caught;
        }};
        console.log(sloppy() + ":" + strict());
        "#
    ));
    assert_eq!(out, vec![format!("{HOT}:{HOT}")]);
}

#[test]
fn upvalset_parity_tdz_region_throws_without_initializing() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        function run() {{
          let caught = 0;
          function spin() {{
            for (let i = 0; i < {HOT}; i++) {{
              try {{ later = i; }} catch (e) {{
                if (e.constructor.name === "ReferenceError") caught++;
              }}
            }}
          }}
          spin();
          return caught;
          let later;
        }}
        console.log(run());
        "#
    ));
    assert_eq!(out, vec![HOT.to_string()]);
}

#[test]
fn upvalset_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 4] = [
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_NO_CALL_INLINE", "1")],
        &[("ZIPP_NO_FNJIT_MEM", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("upvalset_parity_");
        for (key, value) in mode {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn test binary");
        assert!(
            out.status.success(),
            "mode {mode:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "mode filter matched no tests: {mode:?}"
        );
    }
}
