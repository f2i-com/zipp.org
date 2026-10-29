//! `WebAssembly`: the JavaScript API for running WebAssembly modules.
//!
//! The modules themselves are decoded, validated and run by a runtime: [wasmi],
//! an interpreter written in Rust (`interp.rs`), or with the `wasm-jit`
//! feature [Wasmtime], which compiles them to machine code (`jit.rs`) and runs
//! a heavy module some ten times faster. This module is the bridge, the same
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

#[cfg(not(feature = "wasm-jit"))]
mod interp;
#[cfg(not(feature = "wasm-jit"))]
pub(crate) use interp::WasmRuntime;
#[cfg(feature = "wasm-jit")]
mod jit;
#[cfg(feature = "wasm-jit")]
pub(crate) use jit::WasmRuntime;

impl<'p> Vm<'p> {
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
        let rt = self.wasm_runtime();
        let outcome = stats::time(0, || self.wasm_op(rt, &name, arg(1), arg(2), arg(3)));
        // Whatever ran may have grown a memory.
        self.wasm_detach_stale(rt);
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
