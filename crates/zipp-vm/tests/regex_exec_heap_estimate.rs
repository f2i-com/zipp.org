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

// Node v24.19.0 answers "108000:191650" and 4000 for these.
const ROPE_SOURCE: &str = r#"
function tokens(n) {
  var src = '';
  for (var i = 0; i < n; i++) src += 'let x' + (i % 97) + ' = ' + i + ';\n';
  var re = /\s+|[A-Za-z_]\w*|\d+|[=;]/y, count = 0;
  re.lastIndex = 0;
  while (re.lastIndex < src.length) { if (!re.exec(src)) break; count++; }
  return count + ':' + src.length;
}
function accents(n) {
  var text = '';
  for (var i = 0; i < n; i++) text += 'café ' + (i % 10) + ' ';
  var re = /é/g, count = 0;
  while (re.exec(text)) count++;
  return count;
}
"#;

/// A string built by `+=` is a rope, and an exec used to walk a rope into a
/// fresh UTF-16 copy every time instead of flattening it once: the tokenizer
/// below made 108,000 copies of 190 KB (17 s in the WebAssembly build). The
/// exec now flattens a rope that fits the RegExp working allowance.
#[test]
fn regex_exec_over_a_concatenated_string_is_not_quadratic_in_its_length() {
    let mut state = embed::compile_script(ROPE_SOURCE).expect("script compiles");
    state.disable_vm_jit();
    state.run_init().expect("script initializes");
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(CEILING);

    let started = Instant::now();
    let tokens = state
        .call_slot(slot(&state, "tokens"), &[HostValue::Number(12000.0)])
        .expect("tokenizer completes");
    let elapsed = started.elapsed();
    assert!(matches!(&tokens, HostValue::String(text) if text == "108000:191650"), "{tokens:?}");
    assert!(elapsed.as_secs() < 30, "sticky tokenizer over a rope took {elapsed:?}");

    // A rope that is not ASCII is flattened too, and its UTF-16 units are
    // kept from one exec to the next instead of being encoded for each:
    // 40,000 execs over 280,000 units.
    let started = Instant::now();
    let accents = number(
        state
            .call_slot(slot(&state, "accents"), &[HostValue::Number(40000.0)])
            .expect("accent scan completes"),
    );
    let elapsed = started.elapsed();
    assert_eq!(accents, 40000.0);
    assert!(elapsed.as_secs() < 30, "exec loop over text that is not ASCII took {elapsed:?}");
    assert!(state.resource_limit_error().is_none());
}

/// The kept buffer is working storage under the RegExp allowance, not a
/// second allowance: an exec over another subject, an ASCII one included,
/// gives it back, and a subject past the kept size is encoded each time as
/// before. Results are Node's.
#[test]
fn a_kept_subject_buffer_never_outlives_its_subject_or_its_allowance() {
    let source = r#"
        function mixed(n) {
            var a = '', b = '';
            for (var i = 0; i < n; i++) { a += 'café ' + (i % 10) + ' '; b += 'naïve ' + (i % 7) + ' '; }
            var ra = /é/g, rb = /ï/g, rc = /x/g, hits = 0, ma, mb;
            do {
                ma = ra.exec(a); mb = rb.exec(b);
                if (ma) hits++;
                if (mb) hits++;
                rc.lastIndex = 0; rc.exec('plain ascii x');
            } while (ma || mb);
            a += '!';
            ra.lastIndex = 0;
            while (ra.exec(a)) hits++;
            return hits;
        }
        function large(n) {
            var text = 'é';
            while (text.length < n) text += text;
            var re = /é/g, hits = 0;
            for (var i = 0; i < 3; i++) if (re.exec(text)) hits++;
            return hits + ':' + text.length;
        }
    "#;
    let mut state = embed::compile_script(source).expect("script compiles");
    state.disable_vm_jit();
    state.run_init().expect("script initializes");
    state.set_limits(u64::MAX, None);
    state.set_heap_limit(CEILING);

    let mixed = number(
        state
            .call_slot(slot(&state, "mixed"), &[HostValue::Number(500.0)])
            .expect("alternating subjects complete"),
    );
    assert_eq!(mixed, 1500.0);
    // 2,097,152 units: over the kept size, under the working allowance.
    let large = state
        .call_slot(slot(&state, "large"), &[HostValue::Number(2_000_000.0)])
        .expect("a subject past the kept size still matches");
    assert!(matches!(&large, HostValue::String(text) if text == "3:2097152"), "{large:?}");
    assert!(state.resource_limit_error().is_none());
}
