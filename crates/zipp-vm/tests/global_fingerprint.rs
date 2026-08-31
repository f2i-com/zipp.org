//! `getGlobalsFingerprint` exists so an embedder can skip reading globals that
//! have not moved. That is only safe if one property holds: equal digests mean
//! equal marshalled values. These tests pin both directions of it.
//!
//! The direction that matters is the dangerous one. A digest that changes when
//! nothing did costs a needless copy. A digest that stays put when the value
//! moved makes the host stop updating, and the failure shows up as a frozen
//! UI far from here — so most of what follows is mutation that a naive
//! implementation would miss.

use zipp_vm::embed::{compile_script, HostValue, ScriptState};

fn prepare(src: &str) -> ScriptState {
    let mut st = compile_script(src).expect("source compiles");
    st.run_init().expect("top level runs");
    st
}

fn slot_of(st: &ScriptState, name: &str) -> u32 {
    st.symbols()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no global named {name}"))
        .index
}

/// Digest and value, read together, so a test can assert they agree.
fn probe(st: &mut ScriptState, slot: u32) -> (Option<u64>, HostValue) {
    let fp = st.fingerprint_slot(slot);
    let value = st.try_get_slot(slot).expect("slot reads");
    (fp, value)
}

#[test]
fn stable_value_keeps_its_digest() {
    let mut st = prepare(
        r#"
        let scenery = []
        let i = 0
        while (i < 200) {
            scenery.push({ id: "wall" + i, x: i * 1.5, tags: ["fixed", "opaque"] })
            i = i + 1
        }
        function untouched() { return scenery.length }
        "#,
    );
    let slot = slot_of(&st, "scenery");
    let first = st.fingerprint_slot(slot);
    assert!(
        first.is_some(),
        "a 200-element array is well inside the walk budget"
    );

    // Running unrelated code must not disturb it: the digest is of the value,
    // not of when it was taken.
    st.call_global("untouched", &[]).expect("call runs");
    assert_eq!(
        first,
        st.fingerprint_slot(slot),
        "digest moved with no mutation"
    );
}

#[test]
fn in_place_mutation_changes_the_digest() {
    // The case a write-generation counter gets wrong: the slot is never
    // assigned, so `global_gens` would not move, but the value the host reads
    // back is different.
    let mut st = prepare(
        r#"
        let items = [1, 2, 3]
        function push() { items.push(4) }
        function edit() { items[0] = 99 }
        function nest() { items[1] = { deep: [7, 8] } }
        function deeper() { items[1].deep.push(9) }
        "#,
    );
    let slot = slot_of(&st, "items");

    for step in ["push", "edit", "nest", "deeper"] {
        let (before_fp, before_val) = probe(&mut st, slot);
        st.call_global(step, &[]).expect("call runs");
        let (after_fp, after_val) = probe(&mut st, slot);
        assert_ne!(
            format!("{before_val:?}"),
            format!("{after_val:?}"),
            "{step} was supposed to change the value"
        );
        assert_ne!(
            before_fp, after_fp,
            "{step} changed the value but not the digest"
        );
    }
}

#[test]
fn reassignment_changes_the_digest() {
    let mut st = prepare(
        r#"
        let scene = ["a", "b"]
        function swap() { scene = ["a", "c"] }
        function restore() { scene = ["a", "b"] }
        "#,
    );
    let slot = slot_of(&st, "scene");
    let original = st.fingerprint_slot(slot);

    st.call_global("swap", &[]).expect("call runs");
    assert_ne!(
        original,
        st.fingerprint_slot(slot),
        "a different value kept its digest"
    );

    // An equal value is an equal digest even though it is a different array:
    // the host compares what it would read, not object identity, so rebuilding
    // an identical value must not force a copy.
    st.call_global("restore", &[]).expect("call runs");
    assert_eq!(
        original,
        st.fingerprint_slot(slot),
        "an equal value changed its digest"
    );
}

#[test]
fn distinguishes_values_that_are_easy_to_conflate() {
    // Each pair marshals differently, so each pair must digest differently.
    // Written as one script so every case shares an engine and a heap.
    let mut st = prepare(
        r#"
        let a = 0
        let b = -0
        let c = "1"
        let d = 1
        let e = [1, 2]
        let f = [2, 1]
        let g = { x: 1, y: 2 }
        let h = { x: 2, y: 1 }
        let i = []
        let j = {}
        let k = null
        let l = undefined
        let m = [[1], 2]
        let n = [1, [2]]
        "#,
    );
    let pairs = [
        ("a", "b"),
        ("c", "d"),
        ("e", "f"),
        ("g", "h"),
        ("i", "j"),
        ("k", "l"),
        ("m", "n"),
    ];
    for (left, right) in pairs {
        let ls = slot_of(&st, left);
        let rs = slot_of(&st, right);
        let lf = st.fingerprint_slot(ls);
        let rf = st.fingerprint_slot(rs);
        assert_ne!(lf, rf, "{left} and {right} share a digest but not a value");
    }
}

#[test]
fn key_order_and_key_names_are_part_of_the_digest() {
    let mut st = prepare(
        r#"
        let one = { alpha: 1, beta: 2 }
        let two = { beta: 2, alpha: 1 }
        let three = { alpha: 1, gamma: 2 }
        "#,
    );
    let a = st.fingerprint_slot(slot_of(&st, "one"));
    let b = st.fingerprint_slot(slot_of(&st, "two"));
    let c = st.fingerprint_slot(slot_of(&st, "three"));
    // host_out emits keys in insertion order, so a reordered object is a
    // different marshalled value and must be a different digest.
    assert_ne!(a, b, "key order is invisible to the digest");
    assert_ne!(a, c, "key names are invisible to the digest");
}

#[test]
fn a_cycle_terminates() {
    // host_out breaks cycles rather than failing, so the digest must too:
    // hanging here would hang the host's every sync.
    let mut st = prepare(
        r#"
        let loop_ = { name: "root" }
        function tie() { loop_.self = loop_ }
        "#,
    );
    let slot = slot_of(&st, "loop_");
    let before = st.fingerprint_slot(slot);
    st.call_global("tie", &[]).expect("call runs");
    let after = st.fingerprint_slot(slot);
    assert!(
        after.is_some(),
        "a cycle must produce a digest, not a hang or a None"
    );
    assert_ne!(before, after, "adding a self-reference changed the value");
}

#[test]
fn a_missing_slot_is_not_an_error() {
    let mut st = prepare("let only = 1");
    let fp = st.fingerprint_slot(9_999);
    assert!(
        fp.is_some(),
        "an out-of-range slot should digest, not panic"
    );
}

#[test]
fn host_writes_are_visible_to_the_digest() {
    // The host can write globals too, and must not then believe its own stale
    // digest afterwards.
    let mut st = prepare("let value = 1");
    let slot = slot_of(&st, "value");
    let before = st.fingerprint_slot(slot);
    assert!(st.set_slot(slot, &HostValue::Number(2.0)), "write accepted");
    assert_ne!(
        before,
        st.fingerprint_slot(slot),
        "a host write left the digest alone"
    );
}
