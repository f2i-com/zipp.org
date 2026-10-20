//! `ScriptState::resource_usage` is documented as cheap enough to read after
//! every entry, so its `heap_bytes` is the O(1) estimate a heap ceiling is
//! checked against, not the audit walk `ScriptState::heap_bytes` performs
//! (f2i-com/zipp.org#74). It must still track the exact figure.

use std::time::Instant;
use zipp_vm::embed;

fn filled_state() -> embed::ScriptState {
    let mut state = embed::compile_script(
        "var keep = []; for (let i = 0; i < 2e5; i++) keep.push({ a: ['x' + i, i], n: i });",
    )
    .expect("script compiles");
    state.run_init().expect("script fills its heap");
    state
}

#[test]
fn resource_usage_reads_without_walking_the_heap() {
    let state = filled_state();
    // One exact read, which walks every heap slot...
    let walk = Instant::now();
    let exact = state.heap_bytes();
    let walk = walk.elapsed();
    // ...against a hundred `resource_usage` reads. The same walk each time,
    // they would cost a hundred walks; the estimate costs a few loads apiece.
    let reads = Instant::now();
    let mut estimate = 0;
    for _ in 0..100 {
        estimate = std::hint::black_box(state.resource_usage()).heap_bytes;
    }
    let reads = reads.elapsed();
    assert!(
        reads < walk * 10,
        "100 resource_usage() reads took {reads:?}, one heap walk {walk:?}"
    );
    assert!(exact > 8 << 20, "the script retains a large heap: {exact}");
    // The estimate and the audit count the same objects.
    let (lo, hi) = (exact / 2, exact.saturating_mul(2));
    assert!(
        (lo..=hi).contains(&estimate),
        "estimate {estimate} strays from the exact {exact}"
    );
}
