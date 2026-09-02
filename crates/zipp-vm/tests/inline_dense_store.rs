//! B264: the inline pinned dense-Array `SetIndex` lane in MEM regions
//! (`region_mem.rs`, `ZIPP_NO_INLINE_DENSE_STORE=1` restores the helper-only
//! route). The lane stores directly when the receiver matches its pin
//! snapshot, the key is an in-range integer, the holder is YOUNG (no barrier
//! needed) and the element is present or the hole-fill is licensed by the
//! snapshot flags; every other shape reaches `jit_set_index` exactly as
//! before. The JS file exercises each routing edge -- hole fills, appends, an
//! OLD holder across minors, a non-writable `length`, an indexed setter on
//! `Array.prototype`, a custom prototype, fractional/negative/string keys and
//! non-number values -- and its output is node-oracled (v24.12.0).

const SRC: &str = include_str!("inline_dense_store.js");
const EXPECTED: &str = "holes:-1943535616,4096,true|grow:2000,2057000|retained:335104,400|\
fixed:0,64,true|fixed2:64096000,8|setter:500,proto3,502|setproto:x,4,16|\
keys:32,half,neg,31,69|mixed:11842560";

#[test]
fn inline_dense_store_output_matches_node() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}

/// Exact output in every execution mode, with the latch off, and under the
/// collector torture modes (a barrier hole panics at the minor where it
/// opens under `ZIPP_NURSERY_VERIFY=1`). Each child is a fresh process.
#[test]
fn inline_dense_store_matches_in_every_mode() {
    if std::env::var_os("ZIPP_INLINE_DENSE_STORE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    const LATCHES: [&str; 7] = [
        "ZIPP_NO_INLINE_DENSE_STORE",
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
        "ZIPP_GC_STRESS",
        "ZIPP_NURSERY_VERIFY",
        "ZIPP_GCSTATS",
    ];
    for (mode, env) in [
        ("default", vec![]),
        ("no-inline-dense-store", vec![("ZIPP_NO_INLINE_DENSE_STORE", "1")]),
        ("interpreter", vec![("ZIPP_NOJIT", "1")]),
        ("forced-jit", vec![("ZIPP_JIT_THRESHOLD", "1")]),
        ("no-nursery", vec![("ZIPP_NO_NURSERY", "1")]),
        ("gc-stress", vec![("ZIPP_GC_STRESS", "1")]),
        ("nursery-verify", vec![("ZIPP_NURSERY_VERIFY", "1")]),
        ("nursery-verify+forced-jit", vec![("ZIPP_NURSERY_VERIFY", "1"), ("ZIPP_JIT_THRESHOLD", "1")]),
        ("gcstats-oracle", vec![("ZIPP_GCSTATS", "1")]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["inline_dense_store_output_matches_node", "--nocapture"])
            .env("ZIPP_INLINE_DENSE_STORE_CHILD", "1");
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
