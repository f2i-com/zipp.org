//! The 11 September 2026 audit's ZIPP-04: a fingerprint has a real work
//! budget. Every counter below is deterministic — nodes visited (holes and
//! properties included) and key/string bytes hashed — so the bounds are
//! facts about the walk rather than timings, and a graph the batched read
//! would refuse is one the digest reports unknown without walking it.

use zipp_vm::embed::{
    compile_script, FingerprintBudget, HostValue, ScriptState, DEFAULT_HOST_VALUE_MAX_NODES,
    DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
};

fn prepare(src: &str) -> ScriptState {
    let mut st = compile_script(src).expect("compiles");
    st.run_init().expect("runs");
    st
}

fn slot_of(st: &ScriptState, name: &str) -> u32 {
    st.symbols()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no global named {name}"))
        .index
}

/// Digest under a fresh default budget, returning the digest and the work.
fn digest(st: &mut ScriptState, slot: u32) -> (Option<u64>, FingerprintBudget) {
    let mut budget = FingerprintBudget::default();
    let fp = st.fingerprint_slot_bounded(slot, &mut budget);
    (fp, budget)
}

#[test]
fn a_hole_array_the_read_refuses_is_unknown_without_a_walk() {
    // A million holes — dense in every profile (the profiles differ in where
    // `new Array(n)` turns sparse, so the ceiling is the budget's, not the
    // default's) — under a budget that cannot hold them: refused before a
    // single hole is visited.
    let mut st = prepare("var sparse = new Array(1000000);");
    let slot = slot_of(&st, "sparse");
    let mut budget = FingerprintBudget::new(500_000, DEFAULT_HOST_VALUE_MAX_STRING_BYTES);
    assert_eq!(
        st.fingerprint_slot_bounded(slot, &mut budget),
        None,
        "the digest must not claim to know it"
    );
    assert!(
        budget.nodes_used() <= 1,
        "refused up front, not after walking {} holes",
        budget.nodes_used()
    );
    // Under the default budget the read and the digest agree: both accept
    // it, and the digest charges every hole.
    assert!(st.try_get_slot(slot).is_ok());
    let (fp, work) = digest(&mut st, slot);
    assert!(fp.is_some());
    assert_eq!(work.nodes_used(), 1 + 1_000_000);

    // Holes are charged like elements: a hole array that fits is walked hole
    // by hole and reports its exact size in the counter.
    let mut st = prepare("var holes = new Array(1000); holes[999] = 1;");
    let slot = slot_of(&st, "holes");
    let (fp, work) = digest(&mut st, slot);
    assert!(fp.is_some());
    assert_eq!(work.nodes_used(), 1 + 1000, "the array plus every element");
}

#[test]
fn repeated_long_strings_share_one_aggregate_byte_budget() {
    // 300 references to one 65,536-character string: 19.6 MB of hashing for
    // 64 KB of heap, past the read's 16 MiB string ceiling.
    let mut st = prepare(
        "var chunk = 'x'.repeat(65536); var strings = []; for (var i = 0; i < 300; i++) strings.push(chunk);",
    );
    let slot = slot_of(&st, "strings");
    assert!(st.try_get_slot(slot).is_err(), "the read refuses it");
    let (fp, work) = digest(&mut st, slot);
    assert_eq!(fp, None);
    assert!(
        work.string_bytes_used() <= DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
        "hashed {} bytes past the ceiling",
        work.string_bytes_used()
    );

    // Under the ceiling the exact byte count is reported.
    let mut st = prepare("var few = ['ab', 'cde', 'é'];");
    let slot = slot_of(&st, "few");
    let (fp, work) = digest(&mut st, slot);
    assert!(fp.is_some());
    assert_eq!(work.string_bytes_used(), 2 + 3 + 2);
    assert_eq!(work.nodes_used(), 4);
}

#[test]
fn broad_objects_charge_keys_and_values() {
    let mut st = prepare("var wide = {}; for (var i = 0; i < 10000; i++) wide['key' + i] = i;");
    let slot = slot_of(&st, "wide");
    let (fp, work) = digest(&mut st, slot);
    assert!(fp.is_some());
    assert_eq!(work.nodes_used(), 1 + 10000);
    let key_bytes: usize = (0..10000).map(|i| format!("key{i}").len()).sum();
    assert_eq!(work.string_bytes_used(), key_bytes);

    // A budget too small for the object refuses it before the walk.
    let mut small = FingerprintBudget::new(100, 1 << 20);
    assert_eq!(st.fingerprint_slot_bounded(slot, &mut small), None);
    assert!(
        small.nodes_used() <= 1,
        "walked {} nodes",
        small.nodes_used()
    );
}

#[test]
fn cycles_depth_and_dags_stay_bounded() {
    let mut st = prepare(
        "var cyc = { name: 'root' }; cyc.self = cyc; cyc.list = [cyc, cyc]; \
         var deep = {}; var cur = deep; for (var i = 0; i < 200; i++) { cur.next = {}; cur = cur.next; } \
         var dag = 0; for (var i = 0; i < 32; i++) dag = [dag, dag];",
    );
    let cyc = slot_of(&st, "cyc");
    let (fp, work) = digest(&mut st, cyc);
    assert!(fp.is_some(), "a back-edge is a marker, not a loop");
    assert!(work.nodes_used() < 16);

    let deep = slot_of(&st, "deep");
    let (fp, work) = digest(&mut st, deep);
    assert!(fp.is_some(), "depth is capped, the digest still answers");
    assert!(work.nodes_used() <= 70, "{}", work.nodes_used());

    // The shared DAG expands to 2^32 nodes as a tree: refused at the node
    // ceiling, not walked forever.
    let dag = slot_of(&st, "dag");
    assert!(st.try_get_slot(dag).is_err());
    let (fp, work) = digest(&mut st, dag);
    assert_eq!(fp, None);
    assert!(work.nodes_used() <= DEFAULT_HOST_VALUE_MAX_NODES);
}

#[test]
fn a_batch_threads_one_budget_through_duplicate_slots() {
    let mut st = prepare("var a = []; for (var i = 0; i < 1000; i++) a.push(i);");
    let slot = slot_of(&st, "a");
    let mut shared = FingerprintBudget::new(2500, 1 << 20);
    let first = st.fingerprint_slot_bounded(slot, &mut shared);
    let second = st.fingerprint_slot_bounded(slot, &mut shared);
    let third = st.fingerprint_slot_bounded(slot, &mut shared);
    assert!(first.is_some());
    assert_eq!(first, second, "the same value digests the same");
    assert_eq!(third, None, "the third copy does not fit the shared budget");
    assert_eq!(shared.nodes_used(), 2 * 1001 + 1, "refused before walking");
}

#[test]
fn unknown_is_never_equality_and_known_digests_track_content() {
    let mut st = prepare("var v = { items: [1, 2, 3], s: 'abc' };");
    let slot = slot_of(&st, "v");
    let (before, _) = digest(&mut st, slot);
    assert!(before.is_some());
    assert!(st.set_slot(
        slot,
        &HostValue::Object(vec![
            (
                "items".into(),
                HostValue::Array(vec![
                    HostValue::Number(1.0),
                    HostValue::Number(2.0),
                    HostValue::Number(4.0)
                ])
            ),
            ("s".into(), HostValue::String("abc".into())),
        ])
    ));
    let (after, _) = digest(&mut st, slot);
    assert_ne!(before, after);
    // Two unknowns are not equal to each other or to anything: `None` is
    // never a digest value.
    let mut st = prepare("var dag = 0; for (var i = 0; i < 32; i++) dag = [dag, dag];");
    let slot = slot_of(&st, "dag");
    assert_eq!(digest(&mut st, slot).0, None);
    assert_eq!(digest(&mut st, slot).0, None);
}
