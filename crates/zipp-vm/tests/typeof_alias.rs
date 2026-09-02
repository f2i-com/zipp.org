//! The `typeof` alias lane (`compile/typeof_alias.rs`): a declaration
//! `var|let|const t = typeof v` over two plain register locals lets a later
//! `t === "lit"` compile to `TypeOfIs { a: v }` — the fused classifier — and
//! when nothing else reads `t`, the `TypeOf` that produced it is rewritten to
//! `LoadUndefined`. On the JIT side (`emit_typeof_is`) a `TypeOfIs` over a
//! non-heap value answers from the NaN-box tag without the helper.
//!
//! The lane is a linear compile-order fact, so every case in the JS file is a
//! way the fact could go stale: a write to either name (plain, compound,
//! update, destructuring), a declaration inside a branch, every loop kind with
//! a write after the use, `switch` fall-through and direct entry, try/finally,
//! labelled breaks, closures, eval, `with`, mapped `arguments` (sloppy) versus
//! strict, `t` still read as a value, redeclaration, generators, TDZ,
//! undeclared names and a revoked Proxy — each run hot enough for the JIT and
//! demanded to give one answer. Expectations are node-oracled (v24.12.0).
//!
//! `ZIPP_NO_TYPEOF_ALIAS=1` (compiler) and `ZIPP_NO_TYPEOF_IS_INLINE=1` (JIT)
//! restore the old paths; the children below run the file under each latch
//! and each execution mode.

const SRC: &str = include_str!("typeof_alias.js");

const EXPECTED: &str = concat!(
    "1:NNNNSSBBU0OOFFYIOOOOFNNOO\n",
    "2:truefalsetruetruefalsefalsetruefalse|falsetruefalsefalsetruefalsetruefalse|",
    "falsetruetruefalsefalsefalsetruefalse\n",
    "3:true,true,true,true,true,true\n",
    "5:false,true,false,true\n",
    "6:true,true,true|true,false,false|2|true,true|true,true|true,true\n",
    "8:true,false,-,true,false\n",
    "9:true,1,true,true,false,-,false,true\n",
    "10:true,true,true,false,false,true,true\n",
    "11:true,false,true\n",
    "12:true:number|6|true|true,false|true|false\n",
    "13:true,true,true,true,truetrue\n",
    "14:6.411034856395333e+35",
);

#[test]
fn typeof_alias_answers_match_node() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}

/// The lowering itself: the walk shape fuses every comparison and loses its
/// `TypeOf`; a `t` that is still read as a value keeps it; the latch restores
/// the unfused pair.
#[test]
fn typeof_alias_bytecode_shape() {
    let walk = zipp_vm::compile_to_text(
        "function classify(v) { var t = typeof v; if (t === \"number\") return 1; \
         if (t !== \"string\") return 2; if (\"boolean\" == t) return 3; return 0; } classify(1);",
        false,
    )
    .expect("source compiles");
    let used = zipp_vm::compile_to_text(
        "function used(v) { var t = typeof v; return (t === \"number\") + \":\" + t; } used(1);",
        false,
    )
    .expect("source compiles");
    let written = zipp_vm::compile_to_text(
        "function w(v) { var t = typeof v; v = 1; return t === \"number\"; } w(1);",
        false,
    )
    .expect("source compiles");
    if std::env::var_os("ZIPP_NO_TYPEOF_ALIAS").is_none() {
        assert_eq!(
            walk.matches("TypeOfIs").count(),
            3,
            "every aliased comparison fuses:\n{walk}"
        );
        assert!(
            !walk.contains("TypeOf {") && walk.contains("LoadUndefined"),
            "a TypeOf nothing reads is rewritten to LoadUndefined:\n{walk}"
        );
        assert!(
            used.contains("TypeOfIs") && used.contains("TypeOf {"),
            "a t that is still read keeps its TypeOf:\n{used}"
        );
        assert!(
            !written.contains("TypeOfIs") && written.contains("TypeOf {"),
            "a write to v kills the alias:\n{written}"
        );
    } else {
        for text in [&walk, &used, &written] {
            assert!(
                !text.contains("TypeOfIs") && text.contains("TypeOf {"),
                "latch ignored:\n{text}"
            );
        }
    }
}

/// The same answers under each latch and each execution mode, in a child
/// process (the latches are read once per process).
#[test]
fn typeof_alias_matches_in_every_mode_and_with_the_latches_off() {
    if std::env::var_os("ZIPP_TYPEOF_ALIAS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    const LATCHES: [&str; 6] = [
        "ZIPP_NO_TYPEOF_ALIAS",
        "ZIPP_NO_TYPEOF_IS_INLINE",
        "ZIPP_NO_TYPEOF_SAME",
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
    ];
    for (mode, env) in [
        ("default", None),
        ("no-typeof-alias", Some(("ZIPP_NO_TYPEOF_ALIAS", "1"))),
        ("no-typeof-is-inline", Some(("ZIPP_NO_TYPEOF_IS_INLINE", "1"))),
        ("no-typeof-same", Some(("ZIPP_NO_TYPEOF_SAME", "1"))),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("no-nursery", Some(("ZIPP_NO_NURSERY", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["typeof_alias_", "--nocapture"])
            .env("ZIPP_TYPEOF_ALIAS_CHILD", "1");
        for l in LATCHES {
            cmd.env_remove(l);
        }
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
