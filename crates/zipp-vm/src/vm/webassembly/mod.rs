//! `WebAssembly`: the JavaScript API for running WebAssembly modules.
//!
//! The modules themselves are decoded, validated and run by a runtime: [wasmi],
//! an interpreter written in Rust (`interp.rs`), and with the `wasm-jit`
//! feature also [Wasmtime], which compiles them to machine code (`jit.rs`) and
//! runs a heavy module some ten times faster; each VM uses one of them
//! ([`WasmRuntime`]). This module is the bridge, the same
//! for both: the runtime keeps one store per VM and numbers the modules,
//! functions, memories, tables and globals in it, and the API's classes
//! (`webassembly.js`, JavaScript like Node's layer) carry those numbers and
//! call in through one native, [`Vm::wasm_host_op`].
//!
//! Nothing is paid by a program that does not use it: `WebAssembly` is a
//! namespace object whose members are accessors, and the first read of one
//! loads the layer. They stay accessors, each returning its member: turning
//! them into data properties in place could leave an inline cache that saw
//! the accessor calling the member as its getter.
//!
//! - **Memory** is shared, not copied: `Memory.prototype.buffer` is an
//!   `ArrayBuffer` over the wasm memory's own bytes ([`AbData::External`]).
//!   Growing the memory may move them; the buffer is then detached and a new
//!   one made on the next read, as the JS API specifies for `grow`.
//! - **Imports** call back into JavaScript. A JavaScript exception thrown by
//!   one unwinds through the WebAssembly frames as itself; a trap becomes a
//!   `WebAssembly.RuntimeError`.
//! - **Calls into WebAssembly from inside an import** (Emscripten's `invoke_*`
//!   do this) use the import's own store context, as both runtimes require:
//!   the callers of the running imports are kept on a stack.
//!
//! Values cross as the JS API says: `i32`, `f32` and `f64` as Numbers, `i64`
//! as BigInt, `funcref` as an exported function or null, `externref` as the
//! JavaScript value itself (null as a null reference). SIMD values are not
//! supported.
//!
//! - **`externref`** values are kept in the runtime, one `ExternRef` per
//!   distinct JavaScript value (GC roots), for as long as the VM lives: what
//!   WebAssembly holds a reference to cannot be known to have gone. This is
//!   what wasm-bindgen's reference-types output (its `externref` table) needs.
//!
//! [wasmi]: https://github.com/wasmi-labs/wasmi
//! [Wasmtime]: https://github.com/bytecodealliance/wasmtime
//! [`AbData::External`]: crate::heap::AbData::External

use std::collections::HashMap;
use std::fmt;

use crate::heap::{AbData, ExternalBytes, HeapObj, ObjMap, PropAttr};
use crate::value::Value;
use crate::vm::helpers_num2::to_int32;
use crate::vm::native::{self, WASM_HOST_OP, WASM_LAZY_GET};
use crate::vm::{Thrown, Vm};

/// `ZIPP_WASM_STATS=1`: where the time goes, reported every half second --
/// in WebAssembly, in the JavaScript imports it calls, and how often each
/// side crosses to the other.
mod stats {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    thread_local! {
        static ON: bool = std::env::var_os("ZIPP_WASM_STATS").is_some();
        static OPS: Cell<(u64, Duration)> = const { Cell::new((0, Duration::ZERO)) };
        static CALLS: Cell<(u64, Duration)> = const { Cell::new((0, Duration::ZERO)) };
        static IMPORTS: Cell<(u64, Duration)> = const { Cell::new((0, Duration::ZERO)) };
        static DEPTH: Cell<(u32, u32, u32)> = const { Cell::new((0, 0, 0)) };
        static SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    }
    pub(super) fn on() -> bool {
        ON.with(|o| *o)
    }
    fn add(c: &'static std::thread::LocalKey<Cell<(u64, Duration)>>, outer: bool, d: Duration) {
        c.with(|c| {
            let (n, t) = c.get();
            c.set((n + 1, if outer { t + d } else { t }));
        });
    }
    /// Time `f`, counting it as an op, a call or an import (0, 1, 2); only
    /// the outermost of each kind adds time, so nesting is not counted twice.
    pub(super) fn time<R>(kind: usize, f: impl FnOnce() -> R) -> R {
        if !on() {
            return f();
        }
        let outer = DEPTH.with(|d| {
            let mut v = d.get();
            let slot = match kind { 0 => &mut v.0, 1 => &mut v.1, _ => &mut v.2 };
            *slot += 1;
            let outer = *slot == 1;
            d.set(v);
            outer
        });
        let t = Instant::now();
        let r = f();
        let e = t.elapsed();
        DEPTH.with(|d| {
            let mut v = d.get();
            match kind { 0 => v.0 -= 1, 1 => v.1 -= 1, _ => v.2 -= 1 }
            d.set(v);
        });
        add(match kind { 0 => &OPS, 1 => &CALLS, _ => &IMPORTS }, outer, e);
        if kind == 0 && outer {
            report();
        }
        r
    }
    fn report() {
        let now = Instant::now();
        let since = SINCE.with(|s| *s.get().get_or_insert(now));
        SINCE.with(|s| s.set(Some(since)));
        if now.duration_since(since) < Duration::from_millis(500) {
            return;
        }
        let (ops, ops_t) = OPS.with(|c| c.replace((0, Duration::ZERO)));
        let (calls, calls_t) = CALLS.with(|c| c.replace((0, Duration::ZERO)));
        let (imports, imports_t) = IMPORTS.with(|c| c.replace((0, Duration::ZERO)));
        SINCE.with(|s| s.set(Some(now)));
        eprintln!(
            "[WASM] {:?}: ops {ops} ({}ms), calls into wasm {calls} ({}ms, of which imports {}ms), imports to JS {imports}",
            std::thread::current().name().unwrap_or("?"),
            ops_t.as_millis(),
            calls_t.as_millis(),
            imports_t.as_millis(),
        );
    }
}

/// The layer: a function of the native `op`, returning the members.
const LAYER: &str = include_str!("webassembly.js");

/// The namespace's members, each an accessor that loads the layer.
pub(crate) const MEMBERS: &[&str] = &[
    "Module",
    "Instance",
    "Memory",
    "Table",
    "Global",
    "CompileError",
    "LinkError",
    "RuntimeError",
    "validate",
    "compile",
    "instantiate",
    "compileStreaming",
    "instantiateStreaming",
];

/// Marks an error record (`[ERR, kind, message]`), which the layer throws as
/// the named class.
const ERR: &str = "\u{0}wasm-error";

/// What an operation on the runtime ended with.
enum Failure {
    /// A JavaScript exception, already pending in the VM.
    Thrown(Thrown),
    /// An error to report as the named `WebAssembly` (or JS) error class.
    Record(&'static str, String),
}

impl From<Thrown> for Failure {
    fn from(t: Thrown) -> Self {
        Failure::Thrown(t)
    }
}

type Op<T> = Result<T, Failure>;

mod interp;
#[cfg(feature = "wasm-jit")]
mod jit;

/// The runtime one VM's WebAssembly runs on, chosen when the VM first uses
/// WebAssembly (`Vm::wasm_runtime`): Wasmtime in a `wasm-jit` build whose
/// JIT is on, else wasmi. A VM with its JIT off (`ZIPP_NOJIT=1`, as the
/// sandbox's child sets it) thus runs no generated code for WebAssembly
/// either; `ZIPP_WASM_INTERP=1` picks wasmi with the JIT on.
pub(crate) enum WasmRuntime {
    Interp(interp::Runtime),
    #[cfg(feature = "wasm-jit")]
    Jit(jit::Runtime),
}

impl WasmRuntime {
    /// Every Value the runtime holds for the collector.
    pub(crate) fn roots(&self, f: &mut dyn FnMut(Value)) {
        match self {
            WasmRuntime::Interp(rt) => rt.roots(f),
            #[cfg(feature = "wasm-jit")]
            WasmRuntime::Jit(rt) => rt.roots(f),
        }
    }
}

/// What a WebAssembly store made before its VM's step budget says, when the
/// budget is set later: the store counts no fuel, so its code cannot run
/// under the budget.
const UNMETERED_MSG: &str = "WebAssembly made before this VM's step budget was set cannot run under it";

/// Fuel standing for no budget: the runtimes count fuel down in 64 bits, and
/// `i64::MAX` steps is past any run.
const UNLIMITED_FUEL: u64 = i64::MAX as u64;

/// The most fuel wasmi is given at once in a call it can resume: running
/// out of a slice hands control back, the steps are charged, and the meter
/// (a host's abort request among its states) decides whether it goes on.
/// About a few milliseconds of WebAssembly.
const FUEL_SLICE: u64 = 1 << 22;

/// Whether the VM at address `vm` must stop running WebAssembly now: its
/// meter has stopped the script, or the host asked it to abort
/// (`Vm::wasm_meter_fuel`). Wasmtime's epoch callback asks this.
#[cfg(feature = "wasm-jit")]
fn stop_requested(vm: usize) -> bool {
    if vm == 0 {
        return false;
    }
    // SAFETY: as in `grow`.
    let vm = unsafe { &*(vm as *const Vm<'static>) };
    #[cfg(feature = "instrument")]
    {
        vm.wasm_meter_fuel().is_err()
    }
    #[cfg(not(feature = "instrument"))]
    {
        let _ = vm;
        false
    }
}

/// A runtime limiter's growth by `bytes` (memory, or table elements at 8
/// bytes each) in the VM at address `vm`: admitted against the VM's heap
/// ceiling and counted in `Vm::wasm_resident`. `Err` with the meter's
/// message when the ceiling refuses it; the growth then fails, and the VM
/// has stopped the script.
fn grow(vm: usize, bytes: usize) -> Result<(), &'static str> {
    if vm == 0 {
        return Ok(());
    }
    // SAFETY: `vm` is the live VM, set by the operation running this growth
    // (`Host::vm`); the VM is suspended in that operation, as for an import.
    let vm = unsafe { &mut *(vm as *mut Vm<'static>) };
    #[cfg(feature = "instrument")]
    vm.instrument_preflight_external_heap_growth(bytes)?;
    vm.wasm_resident = vm.wasm_resident.saturating_add(bytes);
    Ok(())
}

/// A failure as the text of the error an import raises with it, unwinding
/// through the WebAssembly frames (the exception itself stays pending).
fn failure_message(failure: Failure) -> String {
    match failure {
        Failure::Thrown(thrown) => thrown.0,
        Failure::Record(kind, message) => format!("{kind}: {message}"),
    }
}

/// The bytes a table of `elements` takes, as the limiters count them.
fn table_bytes(elements: usize) -> usize {
    elements.saturating_mul(8)
}

/// `ZIPP_WASM_INTERP=1`: WebAssembly on wasmi whatever the JIT.
#[cfg(all(feature = "wasm-jit", feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn wasm_interp_forced() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ZIPP_WASM_INTERP").is_some())
}

impl<'p> Vm<'p> {
    /// The VM's runtime, made on first use (see [`WasmRuntime`]).
    pub(crate) fn wasm_runtime(&mut self) -> &mut WasmRuntime {
        if self.wasm.is_none() {
            #[cfg(all(feature = "wasm-jit", feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
            let compiled = self.jit_enabled && !wasm_interp_forced();
            #[cfg(not(all(feature = "wasm-jit", feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64"))))]
            let compiled = false;
            // A VM with a meter gets a store that counts fuel, for good.
            let metered = self.wasm_metered();
            let rt = if compiled {
                #[cfg(feature = "wasm-jit")]
                {
                    WasmRuntime::Jit(jit::Runtime::new(metered))
                }
                #[cfg(not(feature = "wasm-jit"))]
                unreachable!("no compiling runtime in this build")
            } else {
                WasmRuntime::Interp(interp::Runtime::new(metered))
            };
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            if crate::jitlog_enabled() {
                let name = if compiled { "wasmtime" } else { "wasmi" };
                eprintln!("[wasm] runtime {name}{}", if metered { ", metered" } else { "" });
            }
            self.wasm = Some(Box::new(rt));
        }
        self.wasm.as_mut().expect("made above")
    }

    /// Whether a runtime made now must count fuel: the VM has a meter.
    fn wasm_metered(&self) -> bool {
        #[cfg(feature = "instrument")]
        {
            self.wasm_meter_attached()
        }
        #[cfg(not(feature = "instrument"))]
        false
    }

    /// The fuel for WebAssembly about to run in a store that counts it
    /// (`metered`): what the VM's meter allows, or [`UNLIMITED_FUEL`]. A
    /// store that does not count fuel may run only while the VM has no
    /// finite budget. `Err` when nothing may run (`Vm::wasm_meter_fuel`).
    fn wasm_fuel(&self, metered: bool) -> Op<Option<u64>> {
        #[cfg(feature = "instrument")]
        let fuel = self.wasm_meter_fuel().map_err(|m| Failure::Thrown(Thrown(m.into())))?;
        #[cfg(not(feature = "instrument"))]
        let fuel: Option<u64> = None;
        match (metered, fuel) {
            (true, fuel) => Ok(Some(fuel.unwrap_or(UNLIMITED_FUEL))),
            (false, None) => Ok(None),
            (false, Some(_)) => Err(Failure::Record("RangeError", UNMETERED_MSG.into())),
        }
    }

    /// Charge the steps WebAssembly took; see `Vm::wasm_meter_charge`.
    fn wasm_charge(&mut self, used: u64, ran_out: bool) -> Op<()> {
        #[cfg(feature = "instrument")]
        self.wasm_meter_charge(used, ran_out)
            .map_err(|m| Failure::Thrown(Thrown(m.into())))?;
        #[cfg(not(feature = "instrument"))]
        let _ = (used, ran_out);
        Ok(())
    }

    /// `otherwise`, unless the VM's meter has stopped the script (its fuel
    /// or the heap ceiling ran out inside WebAssembly) or the host asked it
    /// to abort: then the meter's message, which no `catch` gets past.
    fn wasm_stopped(&self, otherwise: Failure) -> Failure {
        #[cfg(feature = "instrument")]
        if let Some(message) = self.wasm_meter_terminal() {
            return Failure::Thrown(Thrown(message.into()));
        }
        #[cfg(feature = "instrument")]
        if let Err(message) = self.wasm_meter_fuel() {
            return Failure::Thrown(Thrown(message.into()));
        }
        otherwise
    }

    /// The wasmi runtime, for its imports to call back through. Only that
    /// runtime's code asks, so only a VM running it.
    fn wasmi_runtime(&mut self) -> *mut interp::Runtime {
        match self.wasm_runtime() {
            WasmRuntime::Interp(rt) => rt,
            #[cfg(feature = "wasm-jit")]
            WasmRuntime::Jit(_) => unreachable!("a wasmi import in a VM running Wasmtime"),
        }
    }

    /// The Wasmtime runtime, as [`Vm::wasmi_runtime`].
    #[cfg(feature = "wasm-jit")]
    fn wasmtime_runtime(&mut self) -> *mut jit::Runtime {
        match self.wasm_runtime() {
            WasmRuntime::Jit(rt) => rt,
            WasmRuntime::Interp(_) => unreachable!("a Wasmtime import in a VM running wasmi"),
        }
    }

    /// The `WebAssembly` namespace object, made at setup with one lazy
    /// accessor per member, keyed by `names` (setup's interned strings for
    /// [`MEMBERS`]).
    pub(crate) fn wasm_namespace(&mut self, obj_proto: u32, names: &[Value]) -> u32 {
        let mut m = ObjMap::new();
        for (name, &key) in MEMBERS.iter().zip(names) {
            let getter = Value::heap(self.heap.alloc(HeapObj::NativeClosure {
                id: WASM_LAZY_GET,
                state: vec![key],
                name: "get",
                length: 0,
            }));
            m.define(
                name,
                getter,
                PropAttr {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: true,
                    setter: Value::UNDEFINED,
                },
            );
        }
        let idx = self.heap.alloc(HeapObj::Object(Box::new(m)));
        self.proto_of.insert(idx, Value::heap(obj_proto));
        self.wasm_ns = Value::heap(idx);
        idx
    }

    /// A member's accessor: load the layer on the first read, then return the
    /// member.
    pub(crate) fn wasm_lazy_get(&mut self, state: &[Value]) -> Result<Value, Thrown> {
        let name = self.display(state.first().copied().unwrap_or(Value::UNDEFINED));
        if self.wasm_layer.is_undefined() {
            let install = self.eval_script(LAYER)?;
            let op = Value::heap(self.heap.alloc(HeapObj::Native(WASM_HOST_OP)));
            self.wasm_layer = self.call_value(install, Value::UNDEFINED, &[op])?;
        }
        Ok(match self.heap.get(self.wasm_layer.heap_index()) {
            HeapObj::Object(m) => m
                .iter()
                .find(|(k, _, _)| *k == name)
                .map(|(_, v, _)| v)
                .unwrap_or(Value::UNDEFINED),
            _ => Value::UNDEFINED,
        })
    }

    /// `op(name, ...args)`, the layer's one native.
    pub(crate) fn wasm_host_op(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let name = self.display(arg(0));
        let (a, b, c) = (arg(1), arg(2), arg(3));
        // Whatever ran may have grown a memory: stale buffers are detached.
        let outcome = match self.wasm_runtime() {
            WasmRuntime::Interp(rt) => {
                let rt: *mut interp::Runtime = rt;
                let outcome = stats::time(0, || self.wasmi_op(rt, &name, a, b, c));
                self.wasmi_detach_stale(rt);
                outcome
            }
            #[cfg(feature = "wasm-jit")]
            WasmRuntime::Jit(rt) => {
                let rt: *mut jit::Runtime = rt;
                let outcome = stats::time(0, || self.wasmtime_op(rt, &name, a, b, c));
                self.wasmtime_detach_stale(rt);
                outcome
            }
        };
        match outcome {
            Ok(v) => Ok(v),
            Err(Failure::Thrown(t)) => Err(t),
            Err(Failure::Record(kind, message)) => {
                let marker = self.alloc_str(ERR.to_string());
                let kind = self.alloc_str(kind.to_string());
                let message = self.alloc_str(message);
                Ok(self.wasm_array(vec![marker, kind, message]))
            }
        }
    }

    fn wasm_detach(&mut self, ab: u32) {
        if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(ab) {
            *data = AbData::Local(Vec::new());
            *detached = true;
        }
    }

    fn wasm_is_detached(&self, ab: u32) -> bool {
        !matches!(self.heap.get(ab), HeapObj::ArrayBuffer { detached: false, .. })
    }

    /// An `ArrayBuffer` over a memory's bytes, with the current realm's
    /// prototype (as `alloc_array_buffer` gives one).
    fn wasm_external_buffer(&mut self, bytes: ExternalBytes) -> u32 {
        let idx = self.heap.alloc(HeapObj::ArrayBuffer {
            data: AbData::External(bytes),
            detached: false,
        });
        if self.arraybuffer_proto != 0 {
            let p = self.native_home(self.arraybuffer_proto);
            self.proto_of.insert(idx, Value::heap(p));
        }
        idx
    }

    /// The bytes of a BufferSource: an ArrayBuffer, a typed array or a DataView.
    fn wasm_bytes(&mut self, v: Value) -> Op<Vec<u8>> {
        let bad = || Failure::Record("TypeError", "WebAssembly: argument must be an ArrayBuffer or a typed array".into());
        if !v.is_heap() {
            return Err(bad());
        }
        let idx = v.heap_index();
        let (buffer, start, len) = match self.heap.get(idx) {
            HeapObj::ArrayBuffer { data, detached } => (idx, 0, if *detached { 0 } else { data.len() }),
            HeapObj::TypedArray { buffer, kind, byte_offset, .. } => {
                let (buffer, kind, byte_offset) = (*buffer, *kind, *byte_offset);
                let len = self.ta_effective_len(idx).unwrap_or(0) * native::TA_KINDS[kind as usize].1;
                (buffer, byte_offset, len)
            }
            HeapObj::DataView { buffer, byte_offset, byte_length, .. } => (*buffer, *byte_offset, *byte_length),
            _ => return Err(bad()),
        };
        Ok(match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, detached: false } if start + len <= data.len() => data[start..start + len].to_vec(),
            _ => Vec::new(),
        })
    }

    fn wasm_items(&self, v: Value) -> Vec<Value> {
        if v.is_heap() {
            if let HeapObj::Array(items) = self.heap.get(v.heap_index()) {
                return items.clone();
            }
        }
        Vec::new()
    }

    fn wasm_array(&mut self, items: Vec<Value>) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Array(items)))
    }
}
