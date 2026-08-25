//! Enumerating a sparse array stops probing the overlay per dense slot (B80).
//!
//! `object_enum_own`'s array arm walked `0..dense_len` calling
//! `array_index_override(idx, i)` — a `pos()` HASH PROBE on the overlay map —
//! for every slot. A sparse array makes that pathological: writing `a[i] = v`
//! on a stride extends the DENSE vector until it stops growing, and the rest of
//! the writes go to the overlay. Measured on `a.length = 50e6` with 5,000
//! strided keys: **dense_len = 1,040,001 with 105 elements in it**, so
//! `Object.keys(a)` paid 1.04 MILLION hash probes to discover 105 elements.
//! 25ms, against node's 0ms, and — the giveaway — completely independent of how
//! many keys the array actually had.
//!
//! When no overlay key parses to an index below `dense_len`, every one of those
//! probes returns `None` by construction, so the question is now asked ONCE over
//! the overlay's own keys. A `defineProperty` on a dense index is what puts a
//! key below the prefix, and that case keeps the old per-slot probe exactly.
//!
//!     for-in, length 50e6      before    after    node
//!       5,000 keys              26ms      2ms      0ms
//!      20,000 keys              31ms      5ms      3ms
//!      80,000 keys              47ms     22ms     17ms
//!     Object.keys, 5,000 keys   25ms      1ms      0ms
//!
//! Every expectation below was executed in node as a SCRIPT and diffs
//! byte-identical, and each was confirmed identical with `ZIPP_NO_ENUM_HOIST=1`
//! (the old per-slot path) — the two produce byte-identical output on the whole
//! case set.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// `Object.keys(a)` and a `for…in` walk, joined — they must agree.
const HARNESS: &str = r#"
    function keys(a) { return Object.keys(a).join(","); }
    function forin(a) { var r = []; for (var k in a) r.push(k); return r.join(","); }
"#;

#[test]
fn a_sparse_array_enumerates_indices_ascending_then_named() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var a = []; a.length = 100;
        a[50] = 1; a[10] = 2; a[99] = 3; a.zz = 4;
        console.log(keys(a) + "|" + forin(a));
        "#
    ));
    assert_eq!(out[0], "10,50,99,zz|10,50,99,zz");
}

#[test]
fn a_non_enumerable_override_on_a_dense_index_is_hidden() {
    // The case that KEEPS the per-slot probe: an overlay key below `dense_len`.
    // If the hoist got its condition wrong, `2` would reappear here.
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var a = [1, 2, 3, 4];
        Object.defineProperty(a, "2", {{ value: 99, enumerable: false, configurable: true }});
        console.log(keys(a) + "|" + forin(a) + "|" + a[2]);
        "#
    ));
    assert_eq!(out[0], "0,1,3|0,1,3|99");
}

#[test]
fn a_non_enumerable_override_above_the_dense_prefix_is_hidden() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var a = []; a.length = 1000;
        a[0] = 1; a[500] = 2;
        Object.defineProperty(a, "700", {{ value: 7, enumerable: false, configurable: true }});
        console.log(keys(a) + "|" + forin(a) + "|" + a[700]);
        "#
    ));
    assert_eq!(out[0], "0,500|0,500|7");
}

#[test]
fn an_override_on_a_hole_below_the_dense_length_is_enumerated() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var a = [1, 2, 3];
        delete a[1];
        Object.defineProperty(a, "1", {{ value: 42, enumerable: true, configurable: true }});
        console.log(keys(a) + "|" + forin(a) + "|" + a[1]);
        "#
    ));
    assert_eq!(out[0], "0,1,2|0,1,2|42");
}

#[test]
fn delete_punched_holes_are_skipped() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var a = [1, 2, 3, 4, 5];
        delete a[1]; delete a[3];
        console.log(keys(a) + "|" + forin(a));
        "#
    ));
    assert_eq!(out[0], "0,2,4|0,2,4");
}

#[test]
fn a_strided_sparse_array_enumerates_in_numeric_order() {
    // The shape the fix targets: writes on a stride, so the dense prefix grows
    // far past the populated count and the rest lands in the overlay. Order
    // must be numeric-ascending across BOTH storages, then named.
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var a = []; a.length = 5000000;
        for (var i = 0; i < 20; i++) a[i * 100000] = i;
        a.tail = "T";
        console.log(keys(a) + "|" + forin(a));
        "#
    ));
    let expect: Vec<String> = (0..20).map(|i| (i * 100000).to_string()).collect();
    let idx = expect.join(",");
    assert_eq!(out[0], format!("{idx},tail|{idx},tail"));
}

#[test]
fn inherited_enumerables_appear_in_for_in_but_not_object_keys() {
    let out = run_ok(&format!(
        r#"
        "use strict";
        {HARNESS}
        var proto = {{ inherited: 1 }};
        var a = []; a.length = 50; a[7] = 1;
        Object.setPrototypeOf(a, proto);
        console.log(keys(a) + "|" + forin(a));
        "#
    ));
    assert_eq!(out[0], "7|7,inherited");
}

#[test]
fn a_large_strided_array_enumerates_exactly_its_populated_keys() {
    // 5,000 keys over a 50e6 length — the measured shape, where the dense
    // vector reaches 1,040,001 slots holding 105 of them. The COUNT is the
    // assertion that would catch a hoist that skipped real elements.
    let out = run_ok(
        r#"
        "use strict";
        var a = []; a.length = 50000000;
        for (var n = 0; n < 5000; n++) a[n * 10000] = n;
        var viaKeys = Object.keys(a).length;
        var viaForIn = 0, sum = 0;
        for (var k in a) { viaForIn++; sum = (sum + a[k]) | 0; }
        console.log(viaKeys + "," + viaForIn + "," + sum);
        "#,
    );
    // 5000 keys; sum 0..4999 = 12497500.
    assert_eq!(out[0], "5000,5000,12497500");
}
