//! Process-isolated coverage for the global hidden-shape cap.
//!
//! This lives in its own integration-test binary because exhausting the
//! process-global shape registry would otherwise make sibling IC tests
//! order-dependent.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn set_prop_ic_falls_back_after_shape_cap_and_heap_slot_reuse() {
    let output = run_ok(
        r#"
        function put(o, v) { o.x = v; }
        let warm = { x: 0 };
        put(warm, 1);
        put(warm, 2);

        // Under ZIPP_GC_STRESS=1 this repeatedly frees/reuses heap slots while
        // the same SetProp site retains its shape entry. The next receiver has
        // the cached layout but a fresh identity, so stale receiver indices
        // must be impossible.
        let checksum = 0;
        for (let i = 0; i < 512; i++) {
            let transient = { x: i };
            checksum += transient.x;
        }
        let reused = { x: 9 };
        put(reused, 10);
        checksum += reused.x;

        // Exhaust the process-local shape budget with unrelated one-property
        // layouts. A fresh transition after this point is deliberately DICT;
        // the earlier EMPTY -> x transition would still be reusable.
        for (let i = 0; i < 4300; i++) {
            let transient = {};
            transient["unique_shape_" + i] = i;
        }
        let dict = {};
        dict.post_cap_seed = 1;
        dict.x = 3;
        put(dict, 4);
        put(dict, 5);

        console.log(warm.x, reused.x, dict.x, checksum);
        "#,
    );

    assert_eq!(output, ["2 10 5 130826"]);
}
