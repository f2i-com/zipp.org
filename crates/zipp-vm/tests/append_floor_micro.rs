//! B219 follow-up: price the DYNAMIC-key append's remaining terms before
//! optimising any of them. Ignored by default; run explicitly on a quiet box:
//!   cargo test --release -p zipp-vm --test append_floor_micro -- --ignored --nocapture
#![cfg(not(feature = "safe-sandbox"))]

#[test]
#[ignore]
fn decompose_dynamic_append() {
    for line in zipp_vm::bench_support::append_decompose(30_000, 60) {
        println!("rust {line}");
    }
}
