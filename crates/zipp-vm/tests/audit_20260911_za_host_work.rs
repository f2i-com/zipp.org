//! The 11 September 2026 close audit's ZA-07: inspected property entries are
//! work, charged whether or not they are visible.
//!
//! The export walk and the digest scan every entry of an object twice — once
//! to count the visible data properties, once to walk them — and skipped
//! hidden and accessor entries without charging anything, so an object whose
//! exported view is empty cost the host the whole scan for free. Both budgets
//! now carry a deterministic work counter: `2 × entries` per object visited.
//! Every figure below is an exact count, not a timing.

use zipp_vm::embed::{
    compile_script, FingerprintBudget, HostValue, HostValueBudget, ScriptState,
    DEFAULT_HOST_VALUE_MAX_NODES, DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
    DEFAULT_HOST_VALUE_WORK_PER_NODE,
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

const HIDDEN: &str = r#"
    var hidden = {};
    for (var i = 0; i < 20000; i++) Object.defineProperty(hidden, "h" + i, { value: i, enumerable: false });
    var gets = 0;
    var accessors = {};
    for (var j = 0; j < 20000; j++) Object.defineProperty(accessors, "a" + j, { get: function () { gets++; return 1; }, enumerable: true });
    var visible = { a: 1, b: 2, c: 3 };
    var mixed = { shown: 1 };
    Object.defineProperty(mixed, "kept", { value: 2, enumerable: false });
    Object.defineProperty(mixed, "got", { get: function () { gets++; return 3; }, enumerable: true });
    function reads() { return gets; }
"#;

#[test]
fn hidden_entries_are_charged_as_work_by_the_read() {
    let mut st = prepare(HIDDEN);
    let hidden = slot_of(&st, "hidden");
    let mut budget = HostValueBudget::default();
    let value = st.try_get_slot_bounded(hidden, &mut budget).expect("fits");
    assert_eq!(value, HostValue::Object(Vec::new()), "nothing visible");
    assert_eq!(budget.nodes_used(), 1, "one node: the object itself");
    assert_eq!(budget.work_used(), 2 * 20_000, "every entry, twice");

    // A ceiling below that stops the walk before the scan, with the
    // documented recoverable error and no output.
    let mut small = HostValueBudget::default().with_work_limit(39_999);
    let err = st.try_get_slot_bounded(hidden, &mut small).unwrap_err();
    assert!(err.contains("inspection work limit"), "{err}");
    assert_eq!(small.nodes_used(), 1);
    assert_eq!(small.work_used(), 0, "refused before scanning, not after");
}

#[test]
fn accessor_entries_are_charged_and_never_invoked() {
    let mut st = prepare(HIDDEN);
    let accessors = slot_of(&st, "accessors");
    let mut budget = HostValueBudget::default();
    assert_eq!(
        st.try_get_slot_bounded(accessors, &mut budget)
            .expect("fits"),
        HostValue::Object(Vec::new())
    );
    assert_eq!(budget.work_used(), 2 * 20_000);
    let mut fp = FingerprintBudget::default();
    assert!(st.fingerprint_slot_bounded(accessors, &mut fp).is_some());
    assert_eq!(fp.work_used(), 2 * 20_000);
    assert_eq!(fp.nodes_used(), 1);
    let reads = slot_of(&st, "reads");
    assert_eq!(
        st.call_slot(reads, &[]),
        Ok(HostValue::Number(0.0)),
        "no getter ran"
    );
}

#[test]
fn the_digest_is_unknown_at_the_work_ceiling_and_charges_before_scanning() {
    let mut st = prepare(HIDDEN);
    let hidden = slot_of(&st, "hidden");
    let mut fp = FingerprintBudget::default();
    assert!(st.fingerprint_slot_bounded(hidden, &mut fp).is_some());
    assert_eq!(fp.work_used(), 40_000);
    let mut small = FingerprintBudget::default().with_work_limit(39_999);
    assert_eq!(
        st.fingerprint_slot_bounded(hidden, &mut small),
        None,
        "unknown, never a partial digest"
    );
    assert_eq!(small.work_used(), 0);
}

#[test]
fn visible_and_mixed_objects_charge_their_entries_too() {
    let mut st = prepare(HIDDEN);
    let visible = slot_of(&st, "visible");
    let mixed = slot_of(&st, "mixed");
    let mut budget = HostValueBudget::default();
    st.try_get_slot_bounded(visible, &mut budget).expect("fits");
    assert_eq!((budget.nodes_used(), budget.work_used()), (4, 6));
    let mut budget = HostValueBudget::default();
    assert_eq!(
        st.try_get_slot_bounded(mixed, &mut budget).expect("fits"),
        HostValue::Object(vec![("shown".into(), HostValue::Number(1.0))])
    );
    assert_eq!((budget.nodes_used(), budget.work_used()), (2, 6));
    // Arrays and strings are charged as nodes and bytes, as before: no work.
    let mut st2 = prepare("var arr = [1, [2, 3], 'x'];");
    let arr = slot_of(&st2, "arr");
    let mut budget = HostValueBudget::default();
    st2.try_get_slot_bounded(arr, &mut budget).expect("fits");
    assert_eq!((budget.nodes_used(), budget.work_used()), (6, 0));
}

#[test]
fn a_batch_shares_one_work_allowance_across_repeated_slots() {
    let mut st = prepare(HIDDEN);
    let hidden = slot_of(&st, "hidden");
    // Under the default ceiling (8 × the node ceiling) the 20,000-entry
    // object fits 400 times; the 401st slot of a batch is refused, and the
    // digest of that batch is unknown rather than partial.
    let per_slot = 40_000;
    let ceiling = DEFAULT_HOST_VALUE_MAX_NODES * DEFAULT_HOST_VALUE_WORK_PER_NODE;
    let fits = ceiling / per_slot;
    let mut budget = HostValueBudget::default();
    for n in 0..fits {
        st.try_get_slot_bounded(hidden, &mut budget)
            .unwrap_or_else(|e| panic!("slot {n}: {e}"));
    }
    assert_eq!(budget.work_used(), ceiling);
    let err = st.try_get_slot_bounded(hidden, &mut budget).unwrap_err();
    assert!(err.contains("inspection work limit"), "{err}");
    let mut fp = FingerprintBudget::new(
        DEFAULT_HOST_VALUE_MAX_NODES,
        DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
    );
    for _ in 0..fits {
        assert!(st.fingerprint_slot_bounded(hidden, &mut fp).is_some());
    }
    assert_eq!(st.fingerprint_slot_bounded(hidden, &mut fp), None);
}
