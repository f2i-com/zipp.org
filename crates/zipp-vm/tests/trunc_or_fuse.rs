//! B267: in Tier C a truncation-only `Add`/`AddInt` (B260's wrapping add)
//! followed by `LoadInt 0; Bitwise Or` -- the `x | 0` idiom -- stores its
//! wrapped Int straight into the Or's destination and skips the two following
//! ops on the Int path (`codegen/proto_mem.rs`, `fuse_or`;
//! `ZIPP_NO_TRUNC_OR_FUSE=1` restores the three-op emission). The skipped
//! registers are still written, so a later deopt sees the same frame. The JS
//! file enters the shape with Int operands that overflow, both operand orders,
//! doubles (the f64 path), strings/objects/undefined (the concat and coercion
//! paths), a deopt right after the triple and a loop-carried chain. Output is
//! node-oracled (v24.12.0).

const SRC: &str = include_str!("trunc_or_fuse.js");
const EXPECTED: &str = "wrap:1207964498|orders:-2147433642,-2147433642,-2147483640,50001|\
double:1950247377,6,1,0,0,2147483647|mixed:1812865408,123,0,0,12|deopt:1802004000,1000|chain:-165673968";

#[test]
fn trunc_or_fuse_output_matches_node() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}

#[test]
fn trunc_or_fuse_matches_in_every_mode() {
    if std::env::var_os("ZIPP_TRUNC_OR_FUSE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    const LATCHES: [&str; 5] = [
        "ZIPP_NO_TRUNC_OR_FUSE",
        "ZIPP_NO_INT32_TRUNC_ADD",
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
    ];
    for (mode, env) in [
        ("default", vec![]),
        ("no-or-fuse", vec![("ZIPP_NO_TRUNC_OR_FUSE", "1")]),
        ("no-trunc-add", vec![("ZIPP_NO_INT32_TRUNC_ADD", "1")]),
        ("interpreter", vec![("ZIPP_NOJIT", "1")]),
        ("forced-jit", vec![("ZIPP_JIT_THRESHOLD", "1")]),
        ("no-nursery", vec![("ZIPP_NO_NURSERY", "1")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["trunc_or_fuse_output_matches_node", "--nocapture"])
            .env("ZIPP_TRUNC_OR_FUSE_CHILD", "1");
        for l in LATCHES {
            cmd.env_remove(l);
        }
        for (key, value) in env {
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
