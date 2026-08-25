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
fn cloned_private_name_churn_is_reported_and_rejected() {
    // PrivateFieldAdd owns `(brand, key.to_string())` per instance.  Rotating
    // only 32 guest references makes this an amplification/churn case rather
    // than a deliberately retained public object graph: the VM still owns the
    // not-yet-collected instances and all of their cloned 4 KiB field names.
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

    // Heap slots plus the rotating dense array stay well below this.  The
    // per-instance cloned names (about 4 MiB) are what must cross the ceiling.
    let growth = assert_memory_stop(&mut state, "churn", 1024.0, 700 * 1024);
    assert!(
        growth > 3 * 1024 * 1024,
        "the cloned private-name buffers must appear in the public report; growth was {growth}"
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
