//! B187 scouting: attribute the per-object BUILD floor. Ignored by default —
//! run explicitly for the numbers, on a quiet machine:
//!   cargo test --release -p zipp-vm --test build_floor_micro -- --ignored --nocapture

use std::time::Instant;

#[test]
#[ignore]
fn attribute_object_build_floor() {
    use zipp_vm::run;
    // (a) whole-pipeline reference: the bench's own build loop, per object.
    let n = 2_000_000u32;
    let src = format!(
        r#"
        function makeObject(value, kind) {{
          return {{ value: value, kind: kind, left: value ^ 85, right: value + 3 }};
        }}
        var sink = null;
        var t0 = Date.now();
        for (var i = 0; i < {n}; i++) sink = makeObject(i | 0, i & 15);
        var t1 = Date.now();
        console.log("full-pipeline " + (t1 - t0) + "ms for {n}");
        if (sink.value !== {last}) throw new Error("sink");
        "#,
        n = n,
        last = n - 1
    );
    let out = run(&src).expect("runs");
    assert!(out.error.is_none(), "{:?}", out.error);
    for line in out.output {
        println!("js: {line}");
    }
    // (b) inline literal (no call): isolates the cross-call share.
    let src2 = format!(
        r#"
        var sink = null;
        var t0 = Date.now();
        for (var i = 0; i < {n}; i++) {{
          sink = {{ value: i | 0, kind: i & 15, left: (i | 0) ^ 85, right: (i | 0) + 3 }};
        }}
        var t1 = Date.now();
        console.log("inline-literal " + (t1 - t0) + "ms for {n}");
        if (sink.value !== {last}) throw new Error("sink");
        "#,
        n = n,
        last = n - 1
    );
    let out2 = run(&src2).expect("runs");
    assert!(out2.error.is_none(), "{:?}", out2.error);
    for line in out2.output {
        println!("js: {line}");
    }
    // (c) pure Rust floor: ObjMap construction + Box, no heap slot.
    let plan = zipp_vm::bench_support::make_plan(&["value", "kind", "left", "right"]);
    let vals = [1u64, 2, 3, 4];
    let t = Instant::now();
    let mut keep = 0usize;
    for i in 0..n {
        let m = zipp_vm::bench_support::finalized_box(&plan, &vals, i);
        keep = keep.wrapping_add(m);
    }
    let dt = t.elapsed();
    println!(
        "rust finalized+box: {:?} for {n} = {:.1}ns/obj (keep {keep})",
        dt,
        dt.as_nanos() as f64 / n as f64
    );
}
