//! WebAssembly is held to the VM's step budget and heap ceiling, as the
//! script calling it is: its runtime's fuel is the budget, charged back when
//! it returns or calls out (`Vm::wasm_meter_fuel`, `Vm::wasm_meter_charge`),
//! and its store's limiter admits every memory and table growth, the initial
//! sizes included, against the ceiling (`webassembly::grow`). Before this an
//! infinite WebAssembly loop ran to `zipp sandbox`'s wall-clock timeout past
//! a 1M-step budget, and a sandboxed script grew a 1 GiB memory under a 64 MB
//! cap.
//!
//! A host's abort request stops WebAssembly too: Wasmtime's metered engine
//! checks its epoch, and wasmi runs a call in resumable slices of fuel.
//!
//! Every case runs with the JIT on (Wasmtime, in a `wasm-jit` build) and off
//! (wasmi, as the sandbox's child runs it).
#![cfg(all(feature = "instrument", feature = "webassembly"))]

use std::time::{Duration, Instant};
use zipp_vm::embed::{self, HostValue, ScriptState};

const SOURCE: &str = r#"
const bytes = (a) => new Uint8Array(a);
const head = [0, 97, 115, 109, 1, 0, 0, 0];
// (func (export "f") (loop (br 0)))
const LOOP = bytes([...head, 1, 4, 1, 96, 0, 0, 3, 2, 1, 0, 7, 5, 1, 1, 102, 0, 0,
  10, 9, 1, 7, 0, 3, 64, 12, 0, 11, 11]);
// (func (export "count") (param i32) (result i32)): counts down to 0, then 7.
const COUNT = bytes([...head, 1, 6, 1, 96, 1, 127, 1, 127, 3, 2, 1, 0,
  7, 9, 1, 5, 99, 111, 117, 110, 116, 0, 0,
  10, 26, 1, 24, 0, 2, 64, 3, 64, 32, 0, 69, 13, 1, 32, 0, 65, 1, 107, 33, 0, 12, 0, 11, 11, 65, 7, 11]);
// (import "env" "tick" (func)) and "run": COUNT's loop calling tick each turn.
const RUN = bytes([...head, 1, 9, 2, 96, 0, 0, 96, 1, 127, 1, 127,
  2, 12, 1, 3, 101, 110, 118, 4, 116, 105, 99, 107, 0, 0, 3, 2, 1, 1,
  7, 7, 1, 3, 114, 117, 110, 0, 1,
  10, 28, 1, 26, 0, 2, 64, 3, 64, 32, 0, 69, 13, 1, 16, 0, 32, 0, 65, 1, 107, 33, 0, 12, 0, 11, 11, 65, 7, 11]);
// (memory 1) and "g": memory.grow 16 pages at a time, 2000 times or to -1.
const GROW = bytes([...head, 1, 5, 1, 96, 0, 1, 127, 3, 2, 1, 0, 5, 3, 1, 0, 1,
  7, 5, 1, 1, 103, 0, 0,
  10, 36, 1, 34, 1, 1, 127, 2, 64, 3, 64, 65, 16, 64, 0, 65, 127, 70, 13, 1,
  32, 0, 65, 1, 106, 34, 0, 65, 208, 15, 73, 13, 0, 11, 11, 32, 0, 11]);
// (memory 16384): 1 GiB from the start.
const BIGMEM = bytes([...head, 5, 5, 1, 0, 128, 128, 1]);

const instance = (b, imports) => new WebAssembly.Instance(new WebAssembly.Module(b), imports);
function loopForever() { instance(LOOP).exports.f(); return "returned"; }
function catchLoop() { let caught = 0; try { instance(LOOP).exports.f(); } catch (e) { caught++; } return "continued " + caught; }
function count(n) { return instance(COUNT).exports.count(n); }
let ticks = 0;
function withImports(n) { return instance(RUN, { env: { tick() { ticks++; } } }).exports.run(n) + ":" + ticks; }
function bigMemory() { return new WebAssembly.Memory({ initial: 16384 }).buffer.byteLength; }
function growMemory() { const m = new WebAssembly.Memory({ initial: 1 }); m.grow(16383); return m.buffer.byteLength; }
function growInWasm() { return instance(GROW).exports.g(); }
function bigInitial() { instance(BIGMEM); return "made"; }
function smallMemory() { const m = new WebAssembly.Memory({ initial: 16 }); const u = new Uint8Array(m.buffer); u[100] = 42; return u[100] + ":" + m.buffer.byteLength; }
"#;

/// A fresh state: compiled, the JIT off when `jit` is false, initialized.
fn state(jit: bool) -> ScriptState {
    let mut state = embed::compile_script(SOURCE).expect("script compiles");
    if !jit {
        state.disable_vm_jit();
    }
    state.run_init().expect("script initializes");
    state
}

fn call(state: &mut ScriptState, name: &str, args: &[HostValue]) -> Result<HostValue, String> {
    let slot = state
        .symbols()
        .into_iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing global function {name}"))
        .index;
    state.call_slot(slot, args)
}

fn n(x: f64) -> HostValue {
    HostValue::Number(x)
}

fn expect_err(result: Result<HostValue, String>, needle: &str, what: &str) {
    match result {
        Err(e) => assert!(e.contains(needle), "{what}: expected an error containing {needle:?}, got {e:?}"),
        Ok(v) => panic!("{what}: expected an error containing {needle:?}, got {v:?}"),
    }
}

const BUDGET: &str = "instruction budget";
const MEMORY: &str = "memory budget";

#[test]
fn the_step_budget_stops_webassembly() {
    for jit in [true, false] {
        let mut s = state(jit);
        s.set_limits(2_000_000, None);
        let started = Instant::now();
        expect_err(call(&mut s, "loopForever", &[]), BUDGET, "an infinite WebAssembly loop");
        assert!(started.elapsed() < Duration::from_secs(20), "the loop ran {:?} (jit {jit})", started.elapsed());
        assert!(s.resource_limit_error().is_some(), "the budget's exhaustion is the run's (jit {jit})");

        // A `catch` around the call does not let the script go on.
        let mut s = state(jit);
        s.set_limits(2_000_000, None);
        expect_err(call(&mut s, "catchLoop", &[]), BUDGET, "a caught WebAssembly loop");
    }
}

#[test]
fn webassembly_work_is_charged_to_the_budget() {
    for jit in [true, false] {
        const MAX: u64 = 200_000_000;
        let mut s = state(jit);
        s.set_limits(MAX, None);
        let before = s.steps_used();
        let r = call(&mut s, "count", &[n(1_000_000.0)]).expect("a countdown under its budget completes");
        assert!(matches!(r, HostValue::Number(x) if x == 7.0), "{r:?}");
        let used = s.steps_used() - before;
        assert!(used >= 1_000_000, "a million WebAssembly iterations charged {used} steps (jit {jit})");
        assert_eq!(s.steps_used() + s.steps_remaining(), MAX, "jit {jit}");

        // Re-entry through an import each turn: both sides charged, once.
        let before = s.steps_used();
        let r = call(&mut s, "withImports", &[n(100_000.0)]).expect("an import loop under its budget completes");
        assert!(matches!(&r, HostValue::String(t) if t == "7:100000"), "{r:?}");
        assert!(s.steps_used() - before >= 100_000, "jit {jit}");
        assert_eq!(s.steps_used() + s.steps_remaining(), MAX, "jit {jit}");

        // The same countdown cannot finish on a budget too small for it.
        let mut s = state(jit);
        s.set_limits(100_000, None);
        expect_err(call(&mut s, "count", &[n(1_000_000.0)]), BUDGET, "a countdown past its budget");
    }
}

#[test]
fn the_heap_ceiling_holds_webassembly_memory() {
    for jit in [true, false] {
        for (name, what) in [
            ("bigMemory", "a 1 GiB WebAssembly.Memory"),
            ("growMemory", "WebAssembly.Memory.grow to 1 GiB"),
            ("growInWasm", "memory.grow in a loop"),
            ("bigInitial", "a module whose memory starts at 1 GiB"),
        ] {
            let mut s = state(jit);
            s.set_limits(u64::MAX, None);
            s.set_heap_limit(64 << 20);
            expect_err(call(&mut s, name, &[]), MEMORY, &format!("{what} (jit {jit})"));
            assert!(s.resource_limit_error().is_some(), "{what} (jit {jit})");
        }
        let mut s = state(jit);
        s.set_limits(u64::MAX, None);
        s.set_heap_limit(64 << 20);
        let r = call(&mut s, "smallMemory", &[]).expect("a 1 MiB memory fits a 64 MB ceiling");
        assert!(matches!(&r, HostValue::String(t) if t == "42:1048576"), "{r:?} (jit {jit})");
    }
}

/// A host's abort request stops WebAssembly that would run forever, with no
/// step budget to end it: Wasmtime at its next epoch tick, wasmi at the end
/// of its fuel slice.
#[test]
fn a_host_abort_stops_webassembly() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    for jit in [true, false] {
        let mut s = state(jit);
        let flag = Arc::new(AtomicBool::new(false));
        s.set_limits(u64::MAX, Some(flag.clone()));
        let setter = {
            let flag = flag.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                flag.store(true, Ordering::SeqCst);
            })
        };
        let started = Instant::now();
        expect_err(call(&mut s, "loopForever", &[]), "aborted", &format!("an aborted WebAssembly loop (jit {jit})"));
        assert!(started.elapsed() < Duration::from_secs(10), "the abort took {:?} (jit {jit})", started.elapsed());
        setter.join().expect("the abort thread");
    }
}

#[test]
fn unlimited_runs_and_late_budgets() {
    for jit in [true, false] {
        // No meter at all: nothing counts, everything runs.
        let mut s = state(jit);
        let r = call(&mut s, "count", &[n(1000.0)]).expect("a countdown without limits completes");
        assert!(matches!(r, HostValue::Number(x) if x == 7.0), "{r:?}");
        let r = call(&mut s, "smallMemory", &[]).expect("a memory without limits");
        assert!(matches!(&r, HostValue::String(t) if t == "42:1048576"), "{r:?}");

        // A budget set after WebAssembly ran unmetered: that store counts no
        // fuel, so its code does not run under the budget.
        s.set_limits(10_000_000, None);
        expect_err(call(&mut s, "count", &[n(10.0)]), "cannot run under it", &format!("a late budget (jit {jit})"));
    }
}
