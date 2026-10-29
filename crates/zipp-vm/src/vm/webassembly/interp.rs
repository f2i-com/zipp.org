//! The WebAssembly runtime on [wasmi], an interpreter: see the module above.
//!
//! [wasmi]: https://github.com/wasmi-labs/wasmi

use std::collections::HashMap;
use std::fmt;

use crate::heap::{ExternalBytes, HeapObj};
use crate::value::Value;
use crate::vm::helpers_num2::to_int32;
use crate::vm::{Thrown, Vm};

use super::{grow, stats, table_bytes, Failure, Op, FUEL_SLICE};

use wasmi::{
    AsContext, AsContextMut, Caller, Config, Engine, Extern, ExternRef, ExternType, Func, Global, Instance,
    Memory, MemoryType, Module, Mutability, Nullable, Ref, RefType, ResourceLimiter, ResumableCall, Store,
    StoreContextMut, Table, TableType, TrapCode, Val, ValType,
};
use wasmi_core::LimiterError;

/// The store's data: the VM, for an import to call back into and for the
/// limiter to admit growth against, set by every operation as the VM may
/// have moved since; the fuel the store held when last set or charged for
/// (`Vm::wasmi_fuel_settle`); and whether the running call can be resumed
/// when it runs out (a call, not an instantiation's start function), so
/// that its fuel comes in slices (`FUEL_SLICE`).
#[derive(Default)]
pub(crate) struct Host {
    vm: usize,
    fuel_mark: u64,
    sliced: bool,
}

/// Every memory's and table's growth, the initial size included, is admitted
/// against the VM's heap ceiling (`super::grow`); a refusal traps, as the
/// script has been stopped. A growth past the entity's own maximum fails
/// without taking anything.
impl ResourceLimiter for Host {
    fn memory_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> Result<bool, LimiterError> {
        if maximum.is_some_and(|max| desired > max) {
            return Ok(false);
        }
        grow(self.vm, desired.saturating_sub(current))
            .map(|()| true)
            .map_err(|_| LimiterError::ResourceLimiterDeniedAllocation)
    }

    fn table_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> Result<bool, LimiterError> {
        if maximum.is_some_and(|max| desired > max) {
            return Ok(false);
        }
        grow(self.vm, table_bytes(desired.saturating_sub(current)))
            .map(|()| true)
            .map_err(|_| LimiterError::ResourceLimiterDeniedAllocation)
    }

    // wasmi's own defaults (`StoreLimits`).
    fn instances(&self) -> usize {
        10_000
    }

    fn tables(&self) -> usize {
        10_000
    }

    fn memories(&self) -> usize {
        10_000
    }
}

/// A memory and the `ArrayBuffer` last handed out for it, with the bytes it
/// was made over.
struct WasmMemory {
    memory: Memory,
    buffer: Option<(u32, ExternalBytes)>,
}

/// Everything WebAssembly in one VM: the store and what is numbered in it.
pub(crate) struct Runtime {
    engine: Engine,
    store: Store<Host>,
    /// Whether the engine counts fuel: the VM had a meter when the runtime
    /// was made (`Vm::wasm_fuel`).
    metered: bool,
    modules: Vec<Module>,
    instances: Vec<Instance>,
    funcs: Vec<Func>,
    memories: Vec<WasmMemory>,
    tables: Vec<Table>,
    globals: Vec<Global>,
    /// The JavaScript functions instances import (GC roots).
    pub(crate) imports: Vec<Value>,
    /// The JavaScript values WebAssembly has been given as `externref`s (GC
    /// roots), each with its wasmi reference, and by value, to find them again.
    pub(crate) externs: Vec<Value>,
    extern_refs: Vec<ExternRef>,
    extern_ids: HashMap<u64, u32>,
    /// The running imports' `Caller`s, innermost last, as addresses: a call
    /// into WebAssembly from inside an import goes through the innermost.
    callers: Vec<usize>,
}

impl Runtime {
    /// The store context for the next operation: the innermost running
    /// import's when there is one, else the store's.
    fn ctx(&mut self) -> StoreContextMut<'_, Host> {
        match self.callers.last() {
            // SAFETY: the address is of a `Caller` that `js_import` pushed and
            // pops before returning; while it is on the stack that frame is
            // suspended in `call_value`, so nothing else uses the caller.
            Some(&caller) => unsafe { (*(caller as *mut Caller<'static, Host>)).as_context_mut() },
            None => self.store.as_context_mut(),
        }
    }

    fn func_id(&mut self, f: Func) -> u32 {
        match self.funcs.iter().position(|g| same(g, &f)) {
            Some(i) => i as u32,
            None => {
                self.funcs.push(f);
                (self.funcs.len() - 1) as u32
            }
        }
    }

    fn memory_id(&mut self, m: Memory) -> u32 {
        match self.memories.iter().position(|e| same(&e.memory, &m)) {
            Some(i) => i as u32,
            None => {
                self.memories.push(WasmMemory { memory: m, buffer: None });
                (self.memories.len() - 1) as u32
            }
        }
    }

    fn table_id(&mut self, t: Table) -> u32 {
        match self.tables.iter().position(|e| same(e, &t)) {
            Some(i) => i as u32,
            None => {
                self.tables.push(t);
                (self.tables.len() - 1) as u32
            }
        }
    }

    fn global_id(&mut self, g: Global) -> u32 {
        match self.globals.iter().position(|e| same(e, &g)) {
            Some(i) => i as u32,
            None => {
                self.globals.push(g);
                (self.globals.len() - 1) as u32
            }
        }
    }

    /// `v` as an `externref`: the same reference for the same value.
    fn extern_ref(&mut self, v: Value) -> ExternRef {
        if let Some(&i) = self.extern_ids.get(&v.bits()) {
            return self.extern_refs[i as usize];
        }
        let i = self.externs.len() as u32;
        let r = ExternRef::new(self.ctx(), i);
        self.externs.push(v);
        self.extern_refs.push(r);
        self.extern_ids.insert(v.bits(), i);
        r
    }

    /// The JavaScript value an `externref` stands for.
    fn extern_value(&mut self, r: ExternRef) -> Value {
        let ctx = self.ctx();
        let i = r.data(ctx.as_context()).downcast_ref::<u32>().copied();
        i.and_then(|i| self.externs.get(i as usize).copied()).unwrap_or(Value::UNDEFINED)
    }

    /// A table element from what the layer sends: `[value, functionId]`,
    /// the value for an `externref` table, the function for a `funcref` one.
    fn table_element(&mut self, element: RefType, given: &[Value]) -> Op<Ref> {
        let value = given.first().copied().unwrap_or(Value::UNDEFINED);
        Ok(match element {
            RefType::Extern if value.is_null() => Ref::Extern(Nullable::Null),
            RefType::Extern => Ref::Extern(Nullable::Val(self.extern_ref(value))),
            RefType::Func => match given.get(1).copied() {
                Some(id) if id.is_number() => {
                    let f = *self.funcs.get(id.as_f64() as usize).ok_or_else(|| Failure::Record("TypeError", "no such function".into()))?;
                    Ref::Func(Nullable::Val(f))
                }
                _ if value.is_null() || value.is_undefined() => Ref::Func(Nullable::Null),
                _ => {
                    return Err(Failure::Record(
                        "TypeError",
                        "WebAssembly.Table: value must be null or an exported WebAssembly function".into(),
                    ));
                }
            },
        })
    }

    /// The buffers handed out over memories (GC roots).
    pub(crate) fn buffer_roots(&self) -> impl Iterator<Item = u32> + '_ {
        self.memories.iter().filter_map(|m| m.buffer.map(|(ab, _)| ab))
    }

    /// The parameter count of function `id`.
    fn arity(&mut self, f: Func) -> usize {
        f.ty(self.ctx()).params().len()
    }

    /// The memories whose cached buffer no longer matches their bytes.
    fn stale_buffers(&mut self) -> Vec<(usize, u32)> {
        let mut stale = Vec::new();
        for i in 0..self.memories.len() {
            let Some((ab, bytes)) = self.memories[i].buffer else {
                continue;
            };
            let memory = self.memories[i].memory;
            let ctx = self.ctx();
            let (ptr, len) = (memory.data_ptr(&ctx) as usize, memory.data_size(&ctx));
            if ptr != bytes.ptr || len != bytes.len {
                stale.push((i, ab));
            }
        }
        stale
    }
}

/// A JavaScript exception thrown by an import, on its way out through the
/// WebAssembly frames. The thrown value itself stays in the VM's
/// `pending_throw`; this carries its message.
#[derive(Debug)]
struct JsException(String);

impl fmt::Display for JsException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl wasmi::errors::HostError for JsException {}

/// The body of every JavaScript import: convert the arguments, call the
/// function, convert its result.
fn js_import(mut caller: Caller<'_, Host>, index: usize, params: &[Val], results: &mut [Val]) -> Result<(), wasmi::Error> {
    // SAFETY: `vm` was set to the live VM by the entry into WebAssembly that
    // is running this import (see `Host`), and the VM is suspended in
    // that entry, inside `wasm_host_op`.
    let vm = unsafe { &mut *(caller.data().vm as *mut Vm<'static>) };
    let rt = vm.wasmi_runtime();
    // SAFETY: the runtime is boxed in the VM and outlives this call.
    unsafe { (*rt).callers.push(&mut caller as *mut Caller<'_, Host> as usize) };
    // The WebAssembly that called is charged for its work so far, and the
    // JavaScript charges its own; returning, WebAssembly gets what is left.
    // SAFETY (each `&mut *rt`): as above; no other reference is live.
    let outcome = vm
        .wasmi_fuel_settle(unsafe { &mut *rt }, false)
        .and_then(|()| stats::time(2, || vm.wasmi_import_call(rt, index, params, results)).map_err(Failure::Thrown))
        .and_then(|()| vm.wasmi_fuel_open(unsafe { &mut *rt }));
    unsafe { (*rt).callers.pop() };
    outcome.map_err(|failure| wasmi::Error::host(JsException(super::failure_message(failure))))
}

impl Runtime {
    /// An empty runtime: its own engine, counting fuel when `metered`, and
    /// a store whose growth the limiter admits.
    pub(super) fn new(metered: bool) -> Self {
        let mut config = Config::default();
        config.consume_fuel(metered);
        let engine = Engine::new(&config);
        let mut store = Store::new(&engine, Host::default());
        store.limiter(|host| host);
        Runtime {
            engine,
            store,
            metered,
            modules: Vec::new(),
            instances: Vec::new(),
            funcs: Vec::new(),
            memories: Vec::new(),
            tables: Vec::new(),
            globals: Vec::new(),
            imports: Vec::new(),
            externs: Vec::new(),
            extern_refs: Vec::new(),
            extern_ids: HashMap::new(),
            callers: Vec::new(),
        }
    }

    /// Every Value the runtime holds for the collector: the imported
    /// functions, the `externref` values and the buffers over memories.
    pub(super) fn roots(&self, f: &mut dyn FnMut(Value)) {
        self.imports.iter().chain(&self.externs).for_each(|&v| f(v));
        self.buffer_roots().for_each(|ab| f(Value::heap(ab)));
    }
}

impl<'p> Vm<'p> {
    pub(super) fn wasmi_op(&mut self, rt: *mut Runtime, name: &str, a: Value, b: Value, c: Value) -> Op<Value> {
        // SAFETY: the runtime is boxed in the VM and is never dropped while
        // the VM lives; an import re-entering here gets the same box.
        let rt = unsafe { &mut *rt };
        // The limiter admits growth in this VM, whatever the operation.
        rt.ctx().data_mut().vm = self as *mut Vm<'p> as usize;
        let index = |v: Value| -> usize {
            if v.is_number() && v.as_f64() >= 0.0 { v.as_f64() as usize } else { usize::MAX }
        };
        Ok(match name {
            "validate" => {
                let bytes = self.wasm_bytes(a)?;
                Value::bool(Module::validate(&rt.engine, &bytes).is_ok())
            }
            "compile" => {
                let bytes = self.wasm_bytes(a)?;
                let module = Module::new(&rt.engine, &bytes)
                    .map_err(|e| Failure::Record("CompileError", e.to_string()))?;
                rt.modules.push(module);
                Value::num((rt.modules.len() - 1) as f64)
            }
            "moduleImports" => {
                let module = self.wasmi_module(rt, index(a))?;
                let mut out = Vec::new();
                for import in module.imports() {
                    let (m, n) = (import.module().to_string(), import.name().to_string());
                    let kind = extern_kind(import.ty());
                    let row = vec![self.alloc_str(m), self.alloc_str(n), self.alloc_str(kind.to_string())];
                    out.push(self.wasm_array(row));
                }
                self.wasm_array(out)
            }
            "moduleExports" => {
                let module = self.wasmi_module(rt, index(a))?;
                let mut out = Vec::new();
                for export in module.exports() {
                    let n = export.name().to_string();
                    let kind = extern_kind(export.ty());
                    let row = vec![self.alloc_str(n), self.alloc_str(kind.to_string())];
                    out.push(self.wasm_array(row));
                }
                self.wasm_array(out)
            }
            "customSections" => {
                let module = self.wasmi_module(rt, index(a))?;
                let wanted = self.display(b);
                let sections: Vec<Vec<u8>> = module
                    .custom_sections()
                    .filter(|s| s.name() == wanted)
                    .map(|s| s.data().to_vec())
                    .collect();
                let mut out = Vec::new();
                for data in sections {
                    let idx = self.alloc_array_buffer(data.len())?;
                    if let HeapObj::ArrayBuffer { data: d, .. } = self.heap.get_mut(idx) {
                        d.copy_from_slice(&data);
                    }
                    out.push(Value::heap(idx));
                }
                self.wasm_array(out)
            }
            "instantiate" => self.wasmi_instantiate(rt, index(a), b)?,
            "instanceExports" => {
                let instance = *rt
                    .instances
                    .get(index(a))
                    .ok_or_else(|| Failure::Record("TypeError", "no such instance".into()))?;
                let ctx = rt.ctx();
                let exports: Vec<(String, Extern)> = instance
                    .exports(&ctx)
                    .map(|e| (e.name().to_string(), e.into_extern()))
                    .collect();
                let mut out = Vec::new();
                for (n, ext) in exports {
                    let (kind, id, arity) = match ext {
                        Extern::Func(f) => ("function", rt.func_id(f), rt.arity(f)),
                        Extern::Memory(m) => ("memory", rt.memory_id(m), 0),
                        Extern::Table(t) => ("table", rt.table_id(t), 0),
                        Extern::Global(g) => ("global", rt.global_id(g), 0),
                    };
                    let row = vec![
                        self.alloc_str(n),
                        self.alloc_str(kind.to_string()),
                        Value::num(id as f64),
                        Value::num(arity as f64),
                    ];
                    out.push(self.wasm_array(row));
                }
                self.wasm_array(out)
            }
            "call" => self.wasmi_call(rt, index(a), b)?,
            "memoryNew" => {
                let mut builder = MemoryType::builder();
                builder.min(index(a) as u64);
                builder.max(if b.is_undefined() { None } else { Some(index(b) as u64) });
                let ty = builder
                    .build()
                    .map_err(|e| Failure::Record("RangeError", format!("WebAssembly.Memory: {e}")))?;
                let memory = Memory::new(rt.ctx(), ty)
                    .map_err(|e| self.wasm_stopped(Failure::Record("RangeError", format!("WebAssembly.Memory: {e}"))))?;
                Value::num(rt.memory_id(memory) as f64)
            }
            "memoryBuffer" => {
                let id = index(a);
                let entry = rt.memories.get(id).ok_or_else(|| Failure::Record("TypeError", "no such memory".into()))?;
                let memory = entry.memory;
                let cached = entry.buffer;
                let ctx = rt.ctx();
                let bytes = ExternalBytes {
                    ptr: memory.data_ptr(&ctx) as usize,
                    len: memory.data_size(&ctx),
                };
                if let Some((ab, old)) = cached {
                    if old.ptr == bytes.ptr && old.len == bytes.len && !self.wasm_is_detached(ab) {
                        return Ok(Value::heap(ab));
                    }
                    self.wasm_detach(ab);
                }
                let ab = self.wasm_external_buffer(bytes);
                rt.memories[id].buffer = Some((ab, bytes));
                Value::heap(ab)
            }
            "memoryGrow" => {
                let id = index(a);
                let memory = rt.memories.get(id).ok_or_else(|| Failure::Record("TypeError", "no such memory".into()))?.memory;
                let old = memory
                    .grow(rt.ctx(), index(b) as u64)
                    .map_err(|_| self.wasm_stopped(Failure::Record("RangeError", "WebAssembly.Memory.grow: failed to grow memory".into())))?;
                // Growing always detaches the old buffer, moved or not.
                if let Some((ab, _)) = rt.memories[id].buffer.take() {
                    self.wasm_detach(ab);
                }
                Value::num(old as f64)
            }
            "tableNew" => {
                let max = if b.is_undefined() { None } else { Some(index(b) as u32) };
                let externs = c.is_bool() && c.as_bool();
                let (element, null) = if externs {
                    (RefType::Extern, Ref::Extern(Nullable::Null))
                } else {
                    (RefType::Func, Ref::Func(Nullable::Null))
                };
                let ty = TableType::new(element, index(a) as u32, max);
                let table = Table::new(rt.ctx(), ty, null)
                    .map_err(|e| self.wasm_stopped(Failure::Record("RangeError", format!("WebAssembly.Table: {e}"))))?;
                Value::num(rt.table_id(table) as f64)
            }
            "tableSize" => {
                let table = self.wasmi_table(rt, index(a))?;
                Value::num(table.size(rt.ctx()) as f64)
            }
            "tableGet" => {
                let table = self.wasmi_table(rt, index(a))?;
                match table.get(rt.ctx(), index(b) as u64) {
                    Some(Ref::Func(Nullable::Val(f))) => {
                        let id = rt.func_id(f);
                        let arity = rt.arity(f);
                        self.wasm_array(vec![Value::num(id as f64), Value::num(arity as f64)])
                    }
                    // An `externref` table's value, alone in its array.
                    Some(Ref::Extern(Nullable::Val(r))) => {
                        let v = rt.extern_value(r);
                        self.wasm_array(vec![v])
                    }
                    Some(_) => Value::NULL,
                    None => return Err(Failure::Record("RangeError", "WebAssembly.Table.get: index out of bounds".into())),
                }
            }
            "tableSet" => {
                let table = self.wasmi_table(rt, index(a))?;
                let given = self.wasm_items(c);
                let element = table.ty(rt.ctx()).element();
                let value = rt.table_element(element, &given)?;
                table
                    .set(rt.ctx(), index(b) as u64, value)
                    .map_err(|_| Failure::Record("RangeError", "WebAssembly.Table.set: index out of bounds".into()))?;
                Value::UNDEFINED
            }
            "tableGrow" => {
                let table = self.wasmi_table(rt, index(a))?;
                let given = self.wasm_items(c);
                let element = table.ty(rt.ctx()).element();
                let init = rt.table_element(element, &given)?;
                let old = table
                    .grow(rt.ctx(), index(b) as u64, init)
                    .map_err(|_| self.wasm_stopped(Failure::Record("RangeError", "WebAssembly.Table.grow: failed to grow table".into())))?;
                Value::num(old as f64)
            }
            "globalNew" => {
                let ty = match self.display(a).as_str() {
                    "i32" => ValType::I32,
                    "i64" => ValType::I64,
                    "f32" => ValType::F32,
                    "f64" => ValType::F64,
                    other => return Err(Failure::Record("TypeError", format!("WebAssembly.Global: unsupported type {other}"))),
                };
                let value = self.wasmi_from_js(rt, c, ty)?;
                let mutability = if b.is_bool() && b.as_bool() { Mutability::Var } else { Mutability::Const };
                let global = Global::new(rt.ctx(), value, mutability);
                Value::num(rt.global_id(global) as f64)
            }
            "globalGet" => {
                let global = self.wasmi_global(rt, index(a))?;
                let value = global.get(rt.ctx());
                self.wasmi_to_js(rt, &value)?
            }
            "globalSet" => {
                let global = self.wasmi_global(rt, index(a))?;
                let ty = global.ty(rt.ctx());
                if ty.mutability() == Mutability::Const {
                    return Err(Failure::Record("TypeError", "WebAssembly.Global: cannot set an immutable global".into()));
                }
                let value = self.wasmi_from_js(rt, b, ty.content())?;
                global
                    .set(rt.ctx(), value)
                    .map_err(|e| Failure::Record("TypeError", format!("WebAssembly.Global: {e}")))?;
                Value::UNDEFINED
            }
            other => return Err(Failure::Record("TypeError", format!("WebAssembly: unknown operation {other}"))),
        })
    }

    fn wasmi_instantiate(&mut self, rt: &mut Runtime, module_id: usize, resolved: Value) -> Op<Value> {
        let module = self.wasmi_module(rt, module_id)?;
        let types: Vec<ExternType> = module.imports().map(|i| i.ty().clone()).collect();
        let items = self.wasm_items(resolved);
        if items.len() != types.len() {
            return Err(Failure::Record("LinkError", "WebAssembly.Instance: wrong number of imports".into()));
        }
        let mut externs = Vec::with_capacity(items.len());
        for (i, (item, ty)) in items.iter().zip(types).enumerate() {
            let pair = self.wasm_items(*item);
            let tag = self.display(pair.first().copied().unwrap_or(Value::UNDEFINED));
            let value = pair.get(1).copied().unwrap_or(Value::UNDEFINED);
            let link = |what: &str| Failure::Record("LinkError", format!("WebAssembly.Instance: import #{i} {what}"));
            let ext = match (tag.as_str(), &ty) {
                ("jsfunc", ExternType::Func(ft)) => {
                    let index = rt.imports.len();
                    rt.imports.push(value);
                    Extern::Func(Func::new(rt.ctx(), ft.clone(), move |caller, params, results| {
                        js_import(caller, index, params, results)
                    }))
                }
                ("wasmfunc", ExternType::Func(_)) => {
                    Extern::Func(*rt.funcs.get(value.as_f64() as usize).ok_or_else(|| link("is not a function"))?)
                }
                ("memory", ExternType::Memory(_)) => {
                    Extern::Memory(rt.memories.get(value.as_f64() as usize).ok_or_else(|| link("is not a memory"))?.memory)
                }
                ("table", ExternType::Table(_)) => {
                    Extern::Table(*rt.tables.get(value.as_f64() as usize).ok_or_else(|| link("is not a table"))?)
                }
                ("global", ExternType::Global(_)) => {
                    Extern::Global(*rt.globals.get(value.as_f64() as usize).ok_or_else(|| link("is not a global"))?)
                }
                ("value", ExternType::Global(gt)) => {
                    if gt.mutability() == Mutability::Var {
                        return Err(link("must be a WebAssembly.Global to be mutable"));
                    }
                    let v = self.wasmi_from_js(rt, value, gt.content())?;
                    Extern::Global(Global::new(rt.ctx(), v, Mutability::Const))
                }
                _ => return Err(link("does not match the kind the module expects")),
            };
            externs.push(ext);
        }
        rt.ctx().data_mut().vm = self as *mut Vm<'p> as usize;
        // Instantiating runs the start function, which wasmi calls in a way
        // that cannot be resumed: its fuel is all the meter allows, not a
        // slice, so a host's abort request reaches it only at its imports.
        let outer = std::mem::replace(&mut rt.ctx().data_mut().sliced, false);
        let made = self.wasmi_fuel_open(rt).map(|()| Instance::new(rt.ctx(), &module, &externs));
        rt.ctx().data_mut().sliced = outer;
        let made = made?;
        self.wasmi_fuel_settle(rt, matches!(&made, Err(e) if e.as_trap_code() == Some(TrapCode::OutOfFuel)))?;
        let instance = made.map_err(|e| self.wasmi_failure(e, "LinkError"))?;
        rt.instances.push(instance);
        Ok(Value::num((rt.instances.len() - 1) as f64))
    }

    /// Before WebAssembly runs: the store's fuel set to what the VM's meter
    /// allows (`Vm::wasm_fuel`), at most a slice in a call that can resume,
    /// and marked. `Err` when nothing may run.
    fn wasmi_fuel_open(&mut self, rt: &mut Runtime) -> Op<()> {
        if let Some(fuel) = self.wasm_fuel(rt.metered)? {
            let mut ctx = rt.ctx();
            let fuel = if ctx.data().sliced { fuel.min(FUEL_SLICE) } else { fuel };
            // A store made metered counts fuel: setting it cannot fail.
            let _ = ctx.set_fuel(fuel);
            ctx.data_mut().fuel_mark = fuel;
        }
        Ok(())
    }

    /// `func` called: in a metered store resumably, its fuel in slices
    /// (`wasmi_fuel_next`), each charged as it is spent, so a host's abort
    /// request stops it within a slice; else as one plain call. The outer
    /// `Err` is the meter's (the budget spent, the script stopped or
    /// aborted), the inner the call's own.
    fn wasmi_run(&mut self, rt: &mut Runtime, func: Func, params: &[Val], results: &mut [Val]) -> Op<Result<(), wasmi::Error>> {
        self.wasmi_fuel_open(rt)?;
        if !rt.metered {
            return Ok(stats::time(1, || func.call(rt.ctx(), params, results)));
        }
        let mut step = stats::time(1, || func.call_resumable(rt.ctx(), params, results));
        loop {
            match step {
                Ok(ResumableCall::Finished) => {
                    self.wasmi_fuel_settle(rt, false)?;
                    return Ok(Ok(()));
                }
                // A JavaScript import's exception: not resumed, it unwinds.
                Ok(ResumableCall::HostTrap(trap)) => {
                    self.wasmi_fuel_settle(rt, false)?;
                    return Ok(Err(trap.into_host_error()));
                }
                Ok(ResumableCall::OutOfFuel(call)) => {
                    self.wasmi_fuel_next(rt, call.required_fuel())?;
                    step = stats::time(1, || call.resume(rt.ctx(), results));
                }
                Err(e) => {
                    self.wasmi_fuel_settle(rt, e.as_trap_code() == Some(TrapCode::OutOfFuel))?;
                    return Ok(Err(e));
                }
            }
        }
    }

    /// A resumable call ran out of its slice, needing `required` fuel to
    /// take its next step: the slice is charged, and the meter decides --
    /// stopped or aborted, it ends here; with fewer steps left than the step
    /// needs, the budget is spent; else the next slice (or what the step
    /// needs, if more) is given.
    fn wasmi_fuel_next(&mut self, rt: &mut Runtime, required: u64) -> Op<()> {
        self.wasmi_fuel_settle(rt, false)?;
        let left = self.wasm_fuel(rt.metered)?.unwrap_or(super::UNLIMITED_FUEL);
        if left < required.max(1) {
            return self.wasm_charge(0, true);
        }
        let fuel = left.min(FUEL_SLICE.max(required));
        let mut ctx = rt.ctx();
        // As in `wasmi_fuel_open`.
        let _ = ctx.set_fuel(fuel);
        ctx.data_mut().fuel_mark = fuel;
        Ok(())
    }

    /// After WebAssembly ran, or as it calls out: the fuel it spent since the
    /// mark is charged to the VM's meter, and the mark moved to what is left.
    /// `ran_out`: it stopped for want of fuel. `Err` with the meter's message
    /// when the budget is spent.
    fn wasmi_fuel_settle(&mut self, rt: &mut Runtime, ran_out: bool) -> Op<()> {
        if !rt.metered {
            return Ok(());
        }
        let mut ctx = rt.ctx();
        let left = ctx.get_fuel().unwrap_or(0);
        let mark = std::mem::replace(&mut ctx.data_mut().fuel_mark, left);
        self.wasm_charge(mark.saturating_sub(left), ran_out)
    }

    fn wasmi_call(&mut self, rt: &mut Runtime, id: usize, args: Value) -> Op<Value> {
        let func = *rt.funcs.get(id).ok_or_else(|| Failure::Record("TypeError", "no such function".into()))?;
        let ty = func.ty(rt.ctx());
        let given = self.wasm_items(args);
        let mut params = Vec::with_capacity(ty.params().len());
        for (i, &t) in ty.params().iter().enumerate() {
            let v = given.get(i).copied().unwrap_or(Value::UNDEFINED);
            params.push(self.wasmi_from_js(rt, v, t)?);
        }
        let mut results: Vec<Val> = ty.results().iter().map(|&t| Val::default_for_ty(t)).collect();
        rt.ctx().data_mut().vm = self as *mut Vm<'p> as usize;
        let metered = rt.metered;
        let outer = std::mem::replace(&mut rt.ctx().data_mut().sliced, metered);
        let ran = self.wasmi_run(rt, func, &params, &mut results);
        rt.ctx().data_mut().sliced = outer;
        ran?.map_err(|e| self.wasmi_failure(e, "RuntimeError"))?;
        match results.len() {
            0 => Ok(Value::UNDEFINED),
            1 => self.wasmi_to_js(rt, &results[0]),
            _ => {
                let mut out = Vec::with_capacity(results.len());
                for r in &results {
                    out.push(self.wasmi_to_js(rt, r)?);
                }
                Ok(self.wasm_array(out))
            }
        }
    }

    /// An import's call: arguments in, the JavaScript function, result out.
    fn wasmi_import_call(&mut self, rt: *mut Runtime, index: usize, params: &[Val], results: &mut [Val]) -> Result<(), Thrown> {
        // SAFETY: as in `wasmi_op`.
        let rt = unsafe { &mut *rt };
        // The memory may have grown in the WebAssembly code that called this.
        self.wasmi_detach_stale(rt);
        let to_js = |vm: &mut Self, rt: &mut Runtime, p: &Val| match vm.wasmi_to_js(rt, p) {
            Ok(v) => Ok(v),
            Err(Failure::Thrown(t)) => Err(t),
            Err(Failure::Record(kind, message)) => Err(Thrown(format!("{kind}: {message}"))),
        };
        let mut args = Vec::with_capacity(params.len());
        for p in params {
            args.push(to_js(self, rt, p)?);
        }
        let f = rt.imports[index];
        let r = self.call_value(f, Value::UNDEFINED, &args)?;
        let from_js = |vm: &mut Self, rt: &mut Runtime, v: Value, t: ValType| match vm.wasmi_from_js(rt, v, t) {
            Ok(v) => Ok(v),
            Err(Failure::Thrown(t)) => Err(t),
            Err(Failure::Record(kind, message)) => Err(Thrown(format!("{kind}: {message}"))),
        };
        match results.len() {
            0 => {}
            1 => {
                let t = results[0].ty();
                results[0] = from_js(self, rt, r, t)?;
            }
            n => {
                let items = self.wasm_items(r);
                for i in 0..n {
                    let t = results[i].ty();
                    results[i] = from_js(self, rt, items.get(i).copied().unwrap_or(Value::UNDEFINED), t)?;
                }
            }
        }
        Ok(())
    }

    /// What a wasmi error from instantiating or calling becomes: a JavaScript
    /// exception from an import stays itself (still pending in the VM); a
    /// trap is a `RuntimeError`; anything else is `otherwise`.
    fn wasmi_failure(&mut self, e: wasmi::Error, otherwise: &'static str) -> Failure {
        if let Some(js) = e.downcast_ref::<JsException>() {
            return Failure::Thrown(Thrown(js.0.clone()));
        }
        if let Some(code) = e.as_trap_code() {
            return self.wasm_stopped(Failure::Record("RuntimeError", code.to_string()));
        }
        self.wasm_stopped(Failure::Record(otherwise, e.to_string()))
    }

    /// A wasm value as JavaScript sees it.
    fn wasmi_to_js(&mut self, rt: &mut Runtime, v: &Val) -> Op<Value> {
        Ok(match v {
            Val::I32(i) => Value::num(*i as f64),
            Val::I64(i) => self.make_bigint(*i as i128),
            Val::F32(f) => Value::num(f32::from(*f) as f64),
            Val::F64(f) => Value::num(f64::from(*f)),
            Val::FuncRef(Nullable::Null) => Value::NULL,
            // A function crosses as its number; the layer's wrappers are made
            // from numbers on the JavaScript side.
            Val::FuncRef(Nullable::Val(f)) => Value::num(rt.func_id(*f) as f64),
            Val::ExternRef(Nullable::Null) => Value::NULL,
            Val::ExternRef(Nullable::Val(r)) => rt.extern_value(*r),
            _ => return Err(Failure::Record("TypeError", "WebAssembly: this value type cannot cross to JavaScript".into())),
        })
    }

    /// A JavaScript value as a wasm value of type `t`.
    fn wasmi_from_js(&mut self, rt: &mut Runtime, v: Value, t: ValType) -> Op<Val> {
        Ok(match t {
            ValType::I32 => Val::I32(to_int32(self.to_number_strict(v)?)),
            ValType::I64 => Val::I64(match self.to_bigint(v)? {
                crate::vm::bigint::BigVal::Small(i) => i as i64,
                crate::vm::bigint::BigVal::Big(n) => {
                    // ToBigInt64: the value modulo 2^64.
                    let bytes = n.to_signed_bytes_le();
                    let mut low = [if bytes.last().is_some_and(|b| b & 0x80 != 0) { 0xff } else { 0 }; 8];
                    for (i, b) in bytes.iter().take(8).enumerate() {
                        low[i] = *b;
                    }
                    i64::from_le_bytes(low)
                }
            }),
            ValType::F32 => Val::F32((self.to_number_strict(v)? as f32).into()),
            ValType::F64 => Val::F64(self.to_number_strict(v)?.into()),
            ValType::FuncRef if v.is_null() => Val::FuncRef(Nullable::Null),
            ValType::FuncRef if v.is_number() => {
                let f = *rt.funcs.get(v.as_f64() as usize).ok_or_else(|| Failure::Record("TypeError", "not a WebAssembly function".into()))?;
                Val::FuncRef(Nullable::Val(f))
            }
            ValType::ExternRef if v.is_null() => Val::ExternRef(Nullable::Null),
            ValType::ExternRef => Val::ExternRef(Nullable::Val(rt.extern_ref(v))),
            _ => return Err(Failure::Record("TypeError", "WebAssembly: this value type cannot cross from JavaScript".into())),
        })
    }

    /// Detach the buffers of the memories whose bytes moved or resized.
    pub(super) fn wasmi_detach_stale(&mut self, rt: *mut Runtime) {
        // SAFETY: as in `wasmi_op`.
        let rt = unsafe { &mut *rt };
        for (i, ab) in rt.stale_buffers() {
            self.wasm_detach(ab);
            rt.memories[i].buffer = None;
        }
    }

    fn wasmi_module(&mut self, rt: &mut Runtime, id: usize) -> Op<Module> {
        rt.modules
            .get(id)
            .cloned()
            .ok_or_else(|| Failure::Record("TypeError", "WebAssembly: not a WebAssembly.Module".into()))
    }

    fn wasmi_table(&mut self, rt: &mut Runtime, id: usize) -> Op<Table> {
        rt.tables.get(id).copied().ok_or_else(|| Failure::Record("TypeError", "no such table".into()))
    }

    fn wasmi_global(&mut self, rt: &mut Runtime, id: usize) -> Op<Global> {
        rt.globals.get(id).copied().ok_or_else(|| Failure::Record("TypeError", "no such global".into()))
    }
}


/// Whether two wasmi handles name the same entity. They have no `PartialEq`;
/// their `Debug` form is the store and the index within it, which is the
/// identity. Used so that a function, memory, table or global exported twice
/// (or read back from a table) is the same JavaScript object.
fn same<T: fmt::Debug>(a: &T, b: &T) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

/// The JS API's name for an import's or export's kind.
fn extern_kind(ty: &ExternType) -> &'static str {
    match ty {
        ExternType::Func(_) => "function",
        ExternType::Memory(_) => "memory",
        ExternType::Table(_) => "table",
        ExternType::Global(_) => "global",
    }
}

