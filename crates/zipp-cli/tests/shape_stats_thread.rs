//! `ZIPP_SHAPESTATS=1` must report the transition tree the ENGINE built.
//!
//! The tree lives in a `thread_local!` in `zipp_vm::shape`, and the CLI runs the
//! engine on a spawned 256 MiB-stack worker. Sampling it after `join()` read the
//! main thread's untouched table, so every program on earth printed
//! `nodes=2 max_fanout=0 edges=0` — a freshly constructed table holding nothing
//! but DICT and EMPTY. Nothing else about the engine was wrong, which is exactly
//! why it survived: the number is plausible-looking and constant.

use std::process::Command;

/// Two objects built through the SAME key sequence, then a third through a
/// different one — so the tree must hold more than its two sentinels and must
/// have real edges, and `count()`'s reuse property stays visible from here.
#[test]
fn shape_stats_report_the_worker_thread_table() {
    let dir = std::env::temp_dir().join("zipp_shape_stats_cli");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let js = dir.join("shape_stats_probe.js");
    std::fs::write(
        &js,
        r#""use strict";
        function mk(a, b, c) { var o = {}; o.alpha = a; o.beta = b; o.gamma = c; return o; }
        var one = mk(1, 2, 3), two = mk(4, 5, 6);
        var other = {}; other.zeta = 7; other.eta = 8;
        console.log(one.alpha + two.beta + other.zeta + other.eta);
        "#,
    )
    .expect("write probe");

    let out = Command::new(env!("CARGO_BIN_EXE_zipp"))
        .args(["js", js.to_str().expect("utf-8 path")])
        .env("ZIPP_SHAPESTATS", "1")
        .output()
        .expect("run zipp");
    assert!(out.status.success(), "zipp failed: {out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "21");

    let err = String::from_utf8_lossy(&out.stderr);
    let line = err
        .lines()
        .find(|l| l.starts_with("[shape] "))
        .unwrap_or_else(|| panic!("no [shape] line; stderr:\n{err}"));
    let field = |name: &str| -> usize {
        line.split_whitespace()
            .find_map(|f| f.strip_prefix(name))
            .unwrap_or_else(|| panic!("no {name} in {line:?}"))
            .parse()
            .expect("a number")
    };
    // DICT and EMPTY are the two sentinels a never-touched table already holds.
    assert!(field("nodes=") > 2, "reported an untouched table: {line:?}");
    assert!(field("edges=") > 0, "reported an untouched table: {line:?}");
    assert!(
        field("max_fanout=") > 0,
        "reported an untouched table: {line:?}"
    );
}
