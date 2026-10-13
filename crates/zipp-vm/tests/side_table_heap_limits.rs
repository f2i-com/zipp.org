//! Safe-profile regressions for guest-growing VM side tables.
//!
//! Heap objects already charge their own Vec/String payloads.  These cases
//! target allocations that live beside the heap: cloned private-field names
//! and the lazy SameValueZero index maintained for a large Map.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::embed::{self, JsValue, ScriptState};

fn ready(source: &str) -> ScriptState {
    let mut state = embed::compile_script(source).expect("script compiles");
    state.run_init().expect("script initializes");
    state
}

fn assert_memory_stop(
    state: &mut ScriptState,
    function: &str,
    argument: f64,
    headroom: usize,
) -> usize {
    state.set_limits(100_000_000, None);
    let baseline = state.heap_bytes();
    let ceiling = baseline.saturating_add(headroom);
    state.set_heap_limit(ceiling);

    // A short call can finish between periodic meter polls.  The explicit
    // typed-status read is the embedder boundary check and performs the same
    // full audit before reporting success/failure.
    let _ = state.call_global(function, &[JsValue::Number(argument)]);
    let status = state
        .resource_limit_error()
        .expect("side-table growth must exhaust the memory budget");
    assert!(
        status.contains("memory budget"),
        "unexpected status: {status}"
    );

    let reported = state.heap_bytes();
    assert!(
        reported > ceiling,
        "public heap report ({reported}) must expose the audited figure above {ceiling}"
    );
    reported.saturating_sub(baseline)
}

#[test]
fn private_names_are_interned_not_cloned_per_instance() {
    // PrivateFieldAdd used to own `(brand, key.to_string())` per instance, so
    // rotating 32 references to instances of a class with a 4 KiB private
    // name amplified into about 4 MiB of cloned names the meter had to catch.
    // The name is now interned once (`Vm::private_name_ids`) and an instance
    // keeps only `(brand, name id, value)`: the same churn stays far inside
    // the budget that used to be crossed, while the field values themselves
    // are still reported.
    let private_name = "p".repeat(4 * 1024);
    let source = format!(
        "var hold = [];\
         class Box {{ #{private_name} = 1; }}\
         function churn(n) {{\
           for (let i = 0; i < n; i++) hold[i & 31] = new Box();\
           return hold.length;\
         }}"
    );
    let mut state = ready(&source);
    state.set_limits(100_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline.saturating_add(700 * 1024));
    let r = state.call_global("churn", &[JsValue::Number(1024.0)]);
    assert!(r.is_ok(), "churn must complete within the budget: {r:?}");
    assert!(
        state.resource_limit_error().is_none(),
        "no memory stop expected: {:?}",
        state.resource_limit_error()
    );
    let growth = state.heap_bytes().saturating_sub(baseline);
    assert!(
        growth < 1024 * 1024,
        "1,024 instances must not carry a copy of the name each; growth was {growth}"
    );
}

#[test]
fn large_map_index_is_reported_and_rejected() {
    const SOURCE: &str = "var hold;\
         function fill(n) {\
           let m = new Map();\
           for (let i = 0; i < n; i++) m.set(i, i);\
           hold = m;\
           return m.size;\
         }";

    // 32K integer pairs reserve roughly 512 KiB in the Map's authoritative
    // key/value Vecs.  Its lazy index reserves another 512 KiB in a 64K-slot
    // flat table.  First pin public observability without a meter stopping the
    // build at its first over-budget audit.
    let mut reported = ready(SOURCE);
    let baseline = reported.heap_bytes();
    reported
        .call_global("fill", &[JsValue::Number(32_768.0)])
        .expect("unlimited Map build completes");
    let growth = reported.heap_bytes().saturating_sub(baseline);
    assert!(
        growth > 900 * 1024,
        "the Map backing vectors plus side index must be reported; growth was {growth}"
    );

    // This ceiling intentionally fits the authoritative Vecs but not the Vecs
    // plus side index.
    let mut limited = ready(SOURCE);
    assert_memory_stop(&mut limited, "fill", 32_768.0, 768 * 1024);
}
