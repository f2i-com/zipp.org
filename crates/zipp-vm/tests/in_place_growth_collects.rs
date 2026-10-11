//! Memory that grows in place has to reach the collector's schedule.
//!
//! An array is born empty and pushed, or index-stored, to a megabyte; a Map
//! fills. None of that allocates an object, so neither the count of objects
//! nor the size at birth moves, and a loop that built and dropped such things
//! ran no collection: a native run held 1.2 GB where Node peaked at 121 MB,
//! and under a heap ceiling the WebAssembly modules ended a script whose live
//! set was a megabyte (`RangeError: script exceeded its memory budget` on the
//! 512th call of a function that builds one 80,000-element array), or reached
//! the end of the linear memory inside one call and trapped the instance.
//!
//! The engine reads what its allocator has outstanding, which `zipp`
//! (`zipp_vm::note_large_block`) and the WebAssembly modules
//! (`zipp_vm::note_every_block`) report. This binary installs the same kind of
//! allocator, so the tests see what those builds see; it also keeps the peak
//! of the large blocks, which is what they assert.
//! One test function: the figure is process-wide, and tests of one binary
//! run on parallel threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Reporting;

static NET: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Every block, whatever the engine is told of: what a process holds.
static ALL_NET: AtomicUsize = AtomicUsize::new(0);
static ALL_PEAK: AtomicUsize = AtomicUsize::new(0);

fn note(old: usize, new: usize) {
    // As the builds do: every block in the checked profile (the WebAssembly
    // modules), the large ones otherwise (`zipp`).
    #[cfg(feature = "safe-sandbox")]
    zipp_vm::note_every_block(old, new);
    #[cfg(not(feature = "safe-sandbox"))]
    zipp_vm::note_large_block(old, new);
    let all = if new >= old {
        ALL_NET.fetch_add(new - old, Ordering::Relaxed) + (new - old)
    } else {
        ALL_NET.fetch_sub(old - new, Ordering::Relaxed) - (old - new)
    };
    ALL_PEAK.fetch_max(all, Ordering::Relaxed);
    let old = if old >= zipp_vm::LARGE_BLOCK_BYTES { old } else { 0 };
    let new = if new >= zipp_vm::LARGE_BLOCK_BYTES { new } else { 0 };
    let now = if new >= old {
        NET.fetch_add(new - old, Ordering::Relaxed) + (new - old)
    } else {
        NET.fetch_sub(old - new, Ordering::Relaxed) - (old - new)
    };
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards its arguments unchanged to the system
// allocator; the bookkeeping touches only atomics.
unsafe impl GlobalAlloc for Reporting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            note(0, layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            note(0, layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note(layout.size(), 0);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            note(layout.size(), new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Reporting = Reporting;

const MIB: usize = 1 << 20;

/// Run `f` and return how far the large blocks outstanding rose above where
/// they stood when it started.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = NET.load(Ordering::Relaxed);
    PEAK.store(start, Ordering::Relaxed);
    let value = f();
    (value, PEAK.load(Ordering::Relaxed).saturating_sub(start))
}

/// As [`peak_growth`], over every block the allocator handed out.
#[cfg(not(feature = "safe-sandbox"))]
fn peak_growth_of_all<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = ALL_NET.load(Ordering::Relaxed);
    ALL_PEAK.store(start, Ordering::Relaxed);
    let value = f();
    (value, ALL_PEAK.load(Ordering::Relaxed).saturating_sub(start))
}

/// Each iteration makes one array and stores at index 131,071, which fills a
/// megabyte in place, then drops it. Nothing else is allocated.
const DROPPED_ARRAYS: &str = r#"
function w(n) {
  var s = 0;
  for (var i = 0; i < n; i++) { var a = []; a[131071] = 1; s += a.length; }
  return s;
}
"#;

#[cfg(not(feature = "safe-sandbox"))]
fn native_tiers() {
    // 1,200 megabytes of arrays, each dead at the next iteration. Before, no
    // collection ran and all of it was resident at the end.
    let src = format!("{DROPPED_ARRAYS}\nconsole.log(w(1200));");
    let (out, grew) = peak_growth(|| zipp_vm::run(&src).expect("source compiles"));
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    assert_eq!(out.output, vec!["157286400".to_string()]);
    assert!(
        grew < 256 * MIB,
        "1,200 dropped megabyte arrays left {} MiB outstanding at the peak",
        grew / MIB
    );

    // The same through `push`, the route compiled code takes: its lane
    // allocates the array without the general allocation path.
    let src = r#"
function w(n) { var a = []; for (var i = 0; i < n; i++) a.push(i); return a.length; }
var t = 0;
for (var c = 0; c < 300; c++) t += w(131072);
console.log(t);
"#;
    let (out, grew) = peak_growth(|| zipp_vm::run(src).expect("source compiles"));
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    assert_eq!(out.output, vec!["39321600".to_string()]);
    assert!(
        grew < 256 * MIB,
        "300 dropped pushed arrays left {} MiB outstanding at the peak",
        grew / MIB
    );

    // Garbage in blocks under a page. An object given sixty properties
    // keeps some 4 KB in blocks of half a kilobyte to a kilobyte and a half,
    // and the loop allocates nothing but the object: the count schedule sees
    // 30,000 small allocations where there are 120 MB. While only blocks of
    // a page were reported no collection ran (bench/real/polymorphic-objects
    // peaked at 150 MB, Node at 47). Measured over every block, so the bound
    // does not move with what the engine is told of.
    let src = r#"
var keys = [];
for (var j = 0; j < 60; j++) keys.push("k" + j);
function fill(n) {
  var s = 0;
  for (var d = 0; d < n; d++) {
    var o = {};
    for (var j = 0; j < 60; j++) o[keys[j]] = d + j;
    s = (s + o.k59) | 0;
  }
  return s;
}
console.log(fill(30000));
"#;
    let (out, grew) = peak_growth_of_all(|| zipp_vm::run(src).expect("source compiles"));
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    assert_eq!(out.output, vec!["451755000".to_string()]);
    // 38 MiB with the collections; 112 MiB without them.
    assert!(
        grew < 80 * MIB,
        "30,000 dropped sixty-key objects left {} MiB outstanding at the peak",
        grew / MIB
    );

    // Growth that is live is not collected away, and a program that grows by
    // its objects is left to the count schedule: a Map of strings.
    let src = r#"
var m = new Map();
for (var i = 0; i < 200000; i++) m.set("k" + i, i);
console.log(m.size);
"#;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
    assert_eq!(out.output, vec!["200000".to_string()]);
}

#[cfg(feature = "safe-sandbox")]
mod checked {
    use super::{peak_growth, DROPPED_ARRAYS, MIB};
    use zipp_vm::embed::{self, HostValue, ScriptState};

    fn slot(state: &ScriptState, name: &str) -> u32 {
        state
            .symbols()
            .into_iter()
            .find(|symbol| symbol.name == name)
            .unwrap_or_else(|| panic!("missing global function {name}"))
            .index
    }

    fn engine(src: &str, ceiling: usize) -> ScriptState {
        let mut state = embed::compile_script(src).expect("script compiles");
        state.disable_vm_jit();
        state.run_init().expect("script initializes");
        state.set_limits(u64::MAX, None);
        state.set_heap_limit(ceiling);
        state
    }

    pub(super) fn run() {
        const CEILING: usize = 64 * MIB;

        // Garbage inside one call: 400 megabyte arrays under a 64 MiB
        // ceiling. Before, the ceiling convicted the script.
        let mut state = engine(DROPPED_ARRAYS, CEILING);
        let w = slot(&state, "w");
        let (result, grew) = peak_growth(|| state.call_slot(w, &[HostValue::Number(400.0)]));
        assert_eq!(result.as_ref().ok(), Some(&HostValue::Number(52_428_800.0)), "{result:?}");
        assert!(state.heap_bytes() < CEILING, "heap figure {}", state.heap_bytes());
        assert!(grew < CEILING, "{} MiB outstanding at the peak", grew / MIB);

        // Garbage across calls: each call sets a length and returns.
        let mut state = engine(
            "function one() { var a = []; a.length = 131072; a.fill(0); return a.length }",
            CEILING,
        );
        let one = slot(&state, "one");
        for call in 0..300 {
            state.renew_step_budget(u64::MAX);
            let result = state.call_slot(one, &[]);
            assert_eq!(result.as_ref().ok(), Some(&HostValue::Number(131_072.0)), "call {call}: {result:?}");
        }
        assert!(state.heap_bytes() < CEILING, "heap figure {}", state.heap_bytes());

        // A Map filled with numbers allocates no object per entry, and what
        // this profile keeps for one is in small blocks: 200 of them are
        // 400 MiB by the heap figure.
        let mut state = engine(
            "function maps(n) { var s = 0; for (var i = 0; i < n; i++) { var m = new Map(); \
             for (var j = 0; j < 8192; j++) m.set(j, j); s += m.size } return s }",
            CEILING,
        );
        let maps = slot(&state, "maps");
        let result = state.call_slot(maps, &[HostValue::Number(200.0)]);
        assert_eq!(result.as_ref().ok(), Some(&HostValue::Number(1_638_400.0)), "{result:?}");

        // Growth that is LIVE meets the ceiling as the catchable error, while
        // the growth is still a step: before, the 200 arrays were all built
        // (200 MiB against a 64 MiB ceiling) before any poll looked.
        let mut state = engine(
            "var keep = []\n\
             function grow(n) { for (var i = 0; i < n; i++) { var a = []; a[131071] = 1; keep.push(a) } return keep.length }",
            CEILING,
        );
        let grow = slot(&state, "grow");
        let (result, grew) = peak_growth(|| state.call_slot(grow, &[HostValue::Number(200.0)]));
        let message = result.expect_err("200 live megabyte arrays pass a 64 MiB ceiling");
        assert!(message.contains("memory budget"), "{message}");
        assert_eq!(
            state.resource_limit_error(),
            Some("RangeError: script exceeded its memory budget")
        );
        assert!(grew < CEILING + 32 * MIB, "{} MiB outstanding before the ceiling convicted", grew / MIB);

        // The same with nothing allocated while it grows: the arrays exist
        // already, so only the collector's poll at the loop's back-edge sees
        // the footprint.
        let mut state = engine(
            "var arrs = []\n\
             for (var k = 0; k < 200; k++) arrs.push([])\n\
             function fill(n) { for (var i = 0; i < n; i++) arrs[i][131071] = 1; return n }",
            CEILING,
        );
        let fill = slot(&state, "fill");
        let (result, grew) = peak_growth(|| state.call_slot(fill, &[HostValue::Number(200.0)]));
        let message = result.expect_err("200 live megabyte arrays pass a 64 MiB ceiling");
        assert!(message.contains("memory budget"), "{message}");
        assert!(grew < CEILING + 32 * MIB, "{} MiB outstanding before the ceiling convicted", grew / MIB);
    }
}

#[test]
fn memory_grown_in_place_is_collected_and_meets_the_ceiling() {
    #[cfg(not(feature = "safe-sandbox"))]
    native_tiers();
    #[cfg(feature = "safe-sandbox")]
    checked::run();
}
