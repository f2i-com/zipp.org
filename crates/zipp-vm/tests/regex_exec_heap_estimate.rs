//! Under a heap ceiling, every RegExp exec sizes its match limits from the
//! remaining heap headroom. That figure used to come from the exact audit walk
//! over the whole heap, so a sticky `split` over a 400 KB string (~450,000
//! execs) took 208 s in the browser build against 44 ms natively, and the
//! landing page's text-processing example hit its deadline. The per-exec paths
//! now read `Vm::heap_bytes_estimate`: the heap's O(1) resident figure plus the
//! non-heap remainder cached by the last exact audit, so a ceiling set from the
//! exact figure still convicts at the same byte; the strided preflight audit
//! and host-boundary reads keep the exact total.
//!
//! The guard here is exact results plus a wall-clock bound generous enough for
//! a slow CI box: the walk-based path took minutes for this input natively, the
//! estimate takes well under a second in the dev profile, so a regression to
//! the quadratic path fails loudly rather than flaking on timing noise.
#![cfg(feature = "safe-sandbox")]

use std::time::Instant;
use zipp_vm::embed::{self, HostValue, ScriptState};

const CEILING: usize = 256 << 20;

fn slot(state: &ScriptState, name: &str) -> u32 {
    state
        .symbols()
        .into_iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing global function {name}"))
        .index
}

fn number(value: HostValue) -> f64 {
    match value {
        HostValue::Number(n) => n,
        other => panic!("expected a number, got {other:?}"),
    }
}

// Node v24.12.0 answers 186477 / 30001 / 3509 for the three functions.
const SOURCE: &str = r#"
const words = ["zipp", "engine", "rust", "sandbox", "script", "fast", "host", "plugin"];
const parts = [];
for (let i = 0; i < 30000; i++) parts.push(words[i % words.length] + (i % 11 === 10 ? ".\n" : " "));
const doc = parts.join("");
function docLength() { return doc.length; }
function splitCount() { return doc.toLowerCase().split(/[^a-z]+/).length; }
function stickyHits() {
  const re = /[^a-z]+/y;
  let hits = 0;
  for (let i = 0; i < 20000; i++) { re.lastIndex = (i * 7) % doc.length; if (re.exec(doc)) hits++; }
  return hits;
}
"#;

#[test]
fn regex_exec_under_a_heap_ceiling_is_not_quadratic_in_the_heap() {
    let mut state = embed::compile_script(SOURCE).expect("script compiles");
    state.disable_vm_jit();
    state.run_init().expect("script initializes");
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(CEILING);

    let length = number(state.call_slot(slot(&state, "docLength"), &[]).expect("length"));
    assert_eq!(length, 186477.0);

    let started = Instant::now();
    let count = number(state.call_slot(slot(&state, "splitCount"), &[]).expect("split completes"));
    let split_elapsed = started.elapsed();
    assert_eq!(count, 30001.0);

    let started = Instant::now();
    let hits = number(state.call_slot(slot(&state, "stickyHits"), &[]).expect("sticky loop completes"));
    let sticky_elapsed = started.elapsed();
    assert_eq!(hits, 3509.0);

    assert!(
        split_elapsed.as_secs() < 30 && sticky_elapsed.as_secs() < 30,
        "regex exec under a heap ceiling is too slow: split {split_elapsed:?}, sticky loop {sticky_elapsed:?}"
    );
}
