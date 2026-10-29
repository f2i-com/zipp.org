//! The WebAssembly runtime on [Wasmtime]: modules compiled to machine code by
//! Cranelift, where the default runtime (`interp.rs`) interprets them with
//! wasmi. The bridge is the same; what differs is Wasmtime's.
//!
//! - **One engine for the process**, and a module compiled once is compiled
//!   for every VM: each Web Worker has a VM of its own, and the zipp.org
//!   playground starts a fresh engine worker for every run, each compiling the
//!   same 8.6 MB.
//! - **`externref`s** are Wasmtime GC references. One is kept (owned) per
//!   distinct JavaScript value for the VM's life, as wasmi's are; the
//!   short-lived roots an operation makes to pass them are let go when it ends
//!   (each operation runs in a `RootScope`, and an import's are let go when it
//!   returns), so passing references does not grow the store's roots.
//!
//! [Wasmtime]: https://github.com/bytecodealliance/wasmtime

use std::collections::HashMap;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use crate::heap::{ExternalBytes, HeapObj};
use crate::value::Value;
use crate::vm::helpers_num2::to_int32;
use crate::vm::{Thrown, Vm};

use super::{stats, Failure, Op};

use wasmtime::{
    AsContext, AsContextMut, Caller, Config, Engine, Extern, ExternRef, ExternType, Func, Global, GlobalType,
    Instance, Memory, MemoryType, Module, Mutability, OwnedRooted, Ref, RefType, RootScope, Rooted, Store,
    StoreContextMut, Table, TableType, Trap, Val, ValType,
};

/// How much native stack WebAssembly may use. The VM's threads (the page's,
/// each worker's) have 64 MiB; Wasmtime's own default of 512 KiB is less than
/// a browser allows a deeply recursive program.
const MAX_WASM_STACK: usize = 8 * 1024 * 1024;

/// The process's engine.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.max_wasm_stack(MAX_WASM_STACK);
        // No backtrace for a trap: a JavaScript exception thrown through
        // WebAssembly frames (Emscripten's `invoke_*` do this on purpose)
        // would pay for one every time.
        config.wasm_backtrace_max_frames(None);
        Engine::new(&config).expect("the WebAssembly engine's configuration is valid")
    })
}

/// A compiled module, and its custom sections (which Wasmtime does not keep).
struct Compiled {
    module: Module,
    custom: Vec<(String, Vec<u8>)>,
}

/// `bytes` compiled: the module compiled before from the same bytes, if any.
fn compile(bytes: &[u8]) -> wasmtime::Result<Arc<Compiled>> {
    static MODULES: OnceLock<Mutex<HashMap<(usize, u64), Arc<Compiled>>>> = OnceLock::new();
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let key = (bytes.len(), hasher.finish());
    let modules = MODULES.get_or_init(Mutex::default);
    if let Some(compiled) = modules.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(compiled.clone());
    }
    let module = Module::new(engine(), bytes)?;
    let custom = wasmparser::Parser::new(0)
        .parse_all(bytes)
        .filter_map(|payload| match payload {
            Ok(wasmparser::Payload::CustomSection(s)) => Some((s.name().to_string(), s.data().to_vec())),
            _ => None,
        })
        .collect();
    let compiled = Arc::new(Compiled { module, custom });
    modules.lock().unwrap_or_else(|e| e.into_inner()).insert(key, compiled.clone());
    Ok(compiled)
}

/// The store's data: the VM, for an import to call back into. Set before
/// every entry into WebAssembly, as the VM may have moved since.
#[derive(Default)]
pub(crate) struct WasmHost {
    vm: usize,
}

/// A memory and the `ArrayBuffer` last handed out for it, with the bytes it
/// was made over.
struct WasmMemory {
    memory: Memory,
    buffer: Option<(u32, ExternalBytes)>,
}

/// A store context an operation runs in, by address (see `WasmRuntime::ctx`).
#[derive(Clone, Copy)]
enum Context {
    /// A running import's `Caller`.
    Caller(usize),
    /// The `RootScope` an operation runs in.
    Scope(usize),
}

type ScopeCtx = RootScope<StoreContextMut<'static, WasmHost>>;

/// Everything WebAssembly in one VM: the store and what is numbered in it.
pub(crate) struct WasmRuntime {
    store: Store<WasmHost>,
    modules: Vec<Arc<Compiled>>,
    instances: Vec<Instance>,
    funcs: Vec<Func>,
    /// Each entity's number, by its identity (its `Debug` form: the store and
    /// the address or index within it).
    func_ids: HashMap<String, u32>,
    memories: Vec<WasmMemory>,
    tables: Vec<Table>,
    globals: Vec<Global>,
    /// The JavaScript functions instances import (GC roots).
    pub(crate) imports: Vec<Value>,
    /// The JavaScript values WebAssembly has been given as `externref`s (GC
    /// roots), each with its reference, and by value, to find them again.
    pub(crate) externs: Vec<Value>,
    extern_refs: Vec<OwnedRooted<ExternRef>>,
    extern_ids: HashMap<u64, u32>,
    /// The contexts of the operations and imports running, innermost last: a
    /// call into WebAssembly from inside an import goes through the import's.
    contexts: Vec<Context>,
}

impl WasmRuntime {
    /// The store context for the next operation: the innermost running
    /// import's or operation's, else the store's.
    fn ctx(&mut self) -> StoreContextMut<'_, WasmHost> {
        match self.contexts.last() {
            // SAFETY: the address is of a `Caller` that `js_import` pushed and
            // pops before returning; while it is on the stack that frame is
            // suspended, so nothing else uses the caller.
            Some(&Context::Caller(caller)) => unsafe { (*(caller as *mut Caller<'static, WasmHost>)).as_context_mut() },
            // SAFETY: as for a caller: the scope is `wasm_op`'s, popped before
            // it is dropped.
            Some(&Context::Scope(scope)) => unsafe { (*(scope as *mut ScopeCtx)).as_context_mut() },
            None => self.store.as_context_mut(),
        }
    }

    fn func_id(&mut self, f: Func) -> u32 {
        let key = format!("{f:?}");
        if let Some(&i) = self.func_ids.get(&key) {
            return i;
        }
        self.funcs.push(f);
        let i = (self.funcs.len() - 1) as u32;
        self.func_ids.insert(key, i);
        i
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
    fn extern_ref(&mut self, v: Value) -> Op<Rooted<ExternRef>> {
        if let Some(&i) = self.extern_ids.get(&v.bits()) {
            let owned = self.extern_refs[i as usize].clone();
            return Ok(owned.to_rooted(self.ctx()));
        }
        let i = self.externs.len() as u32;
        let mut ctx = self.ctx();
        let r = ExternRef::new(&mut ctx, i).map_err(|e| Failure::Record("RangeError", e.to_string()))?;
        let owned = r.to_owned_rooted(&mut ctx).map_err(|e| Failure::Record("RangeError", e.to_string()))?;
        self.externs.push(v);
        self.extern_refs.push(owned);
        self.extern_ids.insert(v.bits(), i);
        Ok(r)
    }

    /// The JavaScript value an `externref` stands for.
    fn extern_value(&mut self, r: Rooted<ExternRef>) -> Value {
        let ctx = self.ctx();
        let i = r.data(&ctx).ok().flatten().and_then(|d| d.downcast_ref::<u32>()).copied();
        i.and_then(|i| self.externs.get(i as usize).copied()).unwrap_or(Value::UNDEFINED)
    }

    /// A table element from what the layer sends: `[value, functionId]`,
    /// the value for an `externref` table, the function for a `funcref` one.
    fn table_element(&mut self, element: &RefType, given: &[Value]) -> Op<Ref> {
        let value = given.first().copied().unwrap_or(Value::UNDEFINED);
        if element.heap_type().top().is_extern() {
            return Ok(if value.is_null() { Ref::Extern(None) } else { Ref::Extern(Some(self.extern_ref(value)?)) });
        }
        Ok(match given.get(1).copied() {
            Some(id) if id.is_number() => {
                let f = *self.funcs.get(id.as_f64() as usize).ok_or_else(|| Failure::Record("TypeError", "no such function".into()))?;
                Ref::Func(Some(f))
            }
            _ if value.is_null() || value.is_undefined() => Ref::Func(None),
            _ => {
                return Err(Failure::Record(
                    "TypeError",
                    "WebAssembly.Table: value must be null or an exported WebAssembly function".into(),
                ));
            }
        })
    }

    /// The buffers handed out over memories (GC roots).
    pub(crate) fn buffer_roots(&self) -> impl Iterator<Item = u32> + '_ {
        self.memories.iter().filter_map(|m| m.buffer.map(|(ab, _)| ab))
    }

    /// The parameter count of a function.
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

impl std::error::Error for JsException {}

/// The body of every JavaScript import: convert the arguments, call the
/// function, convert its result.
fn js_import(
    mut caller: Caller<'_, WasmHost>,
    index: usize,
    result_types: &[ValType],
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    // SAFETY: `vm` was set to the live VM by the entry into WebAssembly that
    // is running this import (see `WasmHost`), and the VM is suspended in
    // that entry, inside `wasm_host_op`.
    let vm = unsafe { &mut *(caller.data().vm as *mut Vm<'static>) };
    let rt = vm.wasm_runtime();
    // SAFETY: the runtime is boxed in the VM and outlives this call.
    unsafe { (*rt).contexts.push(Context::Caller(&mut caller as *mut Caller<'_, WasmHost> as usize)) };
    let outcome = stats::time(2, || vm.wasm_import_call(rt, index, result_types, params, results));
    unsafe { (*rt).contexts.pop() };
    outcome.map_err(|thrown| wasmtime::Error::new(JsException(thrown.0)))
}

impl<'p> Vm<'p> {
    /// The VM's runtime, made on first use.
    pub(crate) fn wasm_runtime(&mut self) -> *mut WasmRuntime {
        if self.wasm.is_none() {
            let store = Store::new(engine(), WasmHost::default());
            self.wasm = Some(Box::new(WasmRuntime {
                store,
                modules: Vec::new(),
                instances: Vec::new(),
                funcs: Vec::new(),
                func_ids: HashMap::new(),
                memories: Vec::new(),
                tables: Vec::new(),
                globals: Vec::new(),
                imports: Vec::new(),
                externs: Vec::new(),
                extern_refs: Vec::new(),
                extern_ids: HashMap::new(),
                contexts: Vec::new(),
            }));
        }
        &mut **self.wasm.as_mut().unwrap()
    }

    /// One operation, in a root scope of its own: the references it makes to
    /// pass `externref`s are let go when it ends.
    pub(super) fn wasm_op(&mut self, rt: *mut WasmRuntime, name: &str, a: Value, b: Value, c: Value) -> Op<Value> {
        // SAFETY: the runtime is boxed in the VM and is never dropped while
        // the VM lives; an import re-entering here gets the same box.
        let runtime = unsafe { &mut *rt };
        // SAFETY: the scope is used only through `contexts` while this frame
        // runs, and is popped before it is dropped.
        let ctx: StoreContextMut<'static, WasmHost> = unsafe { std::mem::transmute(runtime.ctx()) };
        let mut scope: ScopeCtx = RootScope::new(ctx);
        runtime.contexts.push(Context::Scope(&mut scope as *mut ScopeCtx as usize));
        let outcome = self.wasm_op_in(rt, name, a, b, c);
        // SAFETY: as above.
        unsafe { (*rt).contexts.pop() };
        drop(scope);
        outcome
    }

    fn wasm_op_in(&mut self, rt: *mut WasmRuntime, name: &str, a: Value, b: Value, c: Value) -> Op<Value> {
        // SAFETY: as in `wasm_op`.
        let rt = unsafe { &mut *rt };
        let index = |v: Value| -> usize {
            if v.is_number() && v.as_f64() >= 0.0 { v.as_f64() as usize } else { usize::MAX }
        };
        Ok(match name {
            "validate" => {
                let bytes = self.wasm_bytes(a)?;
                Value::bool(Module::validate(engine(), &bytes).is_ok())
            }
            "compile" => {
                let bytes = self.wasm_bytes(a)?;
                let compiled = compile(&bytes).map_err(|e| Failure::Record("CompileError", format!("{e:#}")))?;
                rt.modules.push(compiled);
                Value::num((rt.modules.len() - 1) as f64)
            }
            "moduleImports" => {
                let compiled = self.wasm_module(rt, index(a))?;
                let mut out = Vec::new();
                for import in compiled.module.imports() {
                    let (m, n) = (import.module().to_string(), import.name().to_string());
                    let kind = extern_kind(&import.ty());
                    let row = vec![self.alloc_str(m), self.alloc_str(n), self.alloc_str(kind.to_string())];
                    out.push(self.wasm_array(row));
                }
                self.wasm_array(out)
            }
            "moduleExports" => {
                let compiled = self.wasm_module(rt, index(a))?;
                let mut out = Vec::new();
                for export in compiled.module.exports() {
                    let n = export.name().to_string();
                    let kind = extern_kind(&export.ty());
                    let row = vec![self.alloc_str(n), self.alloc_str(kind.to_string())];
                    out.push(self.wasm_array(row));
                }
                self.wasm_array(out)
            }
            "customSections" => {
                let compiled = self.wasm_module(rt, index(a))?;
                let wanted = self.display(b);
                let mut out = Vec::new();
                for (_, data) in compiled.custom.iter().filter(|(n, _)| *n == wanted) {
                    let idx = self.alloc_array_buffer(data.len())?;
                    if let HeapObj::ArrayBuffer { data: d, .. } = self.heap.get_mut(idx) {
                        d.copy_from_slice(data);
                    }
                    out.push(Value::heap(idx));
                }
                self.wasm_array(out)
            }
            "instantiate" => self.wasm_instantiate(rt, index(a), b)?,
            "instanceExports" => {
                let instance = *rt
                    .instances
                    .get(index(a))
                    .ok_or_else(|| Failure::Record("TypeError", "no such instance".into()))?;
                let mut ctx = rt.ctx();
                let exports: Vec<(String, Extern)> = instance
                    .exports(&mut ctx)
                    .map(|e| (e.name().to_string(), e.into_extern()))
                    .collect();
                let mut out = Vec::new();
                for (n, ext) in exports {
                    let (kind, id, arity) = match ext {
                        Extern::Func(f) => ("function", rt.func_id(f), rt.arity(f)),
                        Extern::Memory(m) => ("memory", rt.memory_id(m), 0),
                        Extern::Table(t) => ("table", rt.table_id(t), 0),
                        Extern::Global(g) => ("global", rt.global_id(g), 0),
                        // Shared memories and exception tags: not offered.
                        _ => continue,
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
            "call" => self.wasm_call(rt, index(a), b)?,
            "memoryNew" => {
                let min = u32::try_from(index(a)).map_err(|_| Failure::Record("RangeError", "WebAssembly.Memory: initial is too large".into()))?;
                let max = if b.is_undefined() {
                    None
                } else {
                    Some(u32::try_from(index(b)).map_err(|_| Failure::Record("RangeError", "WebAssembly.Memory: maximum is too large".into()))?)
                };
                let memory = Memory::new(rt.ctx(), MemoryType::new(min, max))
                    .map_err(|e| Failure::Record("RangeError", format!("WebAssembly.Memory: {e}")))?;
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
                    .map_err(|_| Failure::Record("RangeError", "WebAssembly.Memory.grow: failed to grow memory".into()))?;
                // Growing always detaches the old buffer, moved or not.
                if let Some((ab, _)) = rt.memories[id].buffer.take() {
                    self.wasm_detach(ab);
                }
                Value::num(old as f64)
            }
            "tableNew" => {
                let max = if b.is_undefined() { None } else { Some(index(b) as u32) };
                let externs = c.is_bool() && c.as_bool();
                let (element, null) =
                    if externs { (RefType::EXTERNREF, Ref::Extern(None)) } else { (RefType::FUNCREF, Ref::Func(None)) };
                let ty = TableType::new(element, index(a) as u32, max);
                let table = Table::new(rt.ctx(), ty, null)
                    .map_err(|e| Failure::Record("RangeError", format!("WebAssembly.Table: {e}")))?;
                Value::num(rt.table_id(table) as f64)
            }
            "tableSize" => {
                let table = self.wasm_table(rt, index(a))?;
                Value::num(table.size(rt.ctx()) as f64)
            }
            "tableGet" => {
                let table = self.wasm_table(rt, index(a))?;
                match table.get(rt.ctx(), index(b) as u64) {
                    Some(Ref::Func(Some(f))) => {
                        let id = rt.func_id(f);
                        let arity = rt.arity(f);
                        self.wasm_array(vec![Value::num(id as f64), Value::num(arity as f64)])
                    }
                    // An `externref` table's value, alone in its array.
                    Some(Ref::Extern(Some(r))) => {
                        let v = rt.extern_value(r);
                        self.wasm_array(vec![v])
                    }
                    Some(_) => Value::NULL,
                    None => return Err(Failure::Record("RangeError", "WebAssembly.Table.get: index out of bounds".into())),
                }
            }
            "tableSet" => {
                let table = self.wasm_table(rt, index(a))?;
                let given = self.wasm_items(c);
                let element = table.ty(rt.ctx()).element().clone();
                let value = rt.table_element(&element, &given)?;
                table
                    .set(rt.ctx(), index(b) as u64, value)
                    .map_err(|_| Failure::Record("RangeError", "WebAssembly.Table.set: index out of bounds".into()))?;
                Value::UNDEFINED
            }
            "tableGrow" => {
                let table = self.wasm_table(rt, index(a))?;
                let given = self.wasm_items(c);
                let element = table.ty(rt.ctx()).element().clone();
                let init = rt.table_element(&element, &given)?;
                let old = table
                    .grow(rt.ctx(), index(b) as u64, init)
                    .map_err(|_| Failure::Record("RangeError", "WebAssembly.Table.grow: failed to grow table".into()))?;
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
                let value = self.wasm_from_js(rt, c, &ty)?;
                let mutability = if b.is_bool() && b.as_bool() { Mutability::Var } else { Mutability::Const };
                let global = Global::new(rt.ctx(), GlobalType::new(ty, mutability), value)
                    .map_err(|e| Failure::Record("TypeError", format!("WebAssembly.Global: {e}")))?;
                Value::num(rt.global_id(global) as f64)
            }
            "globalGet" => {
                let global = self.wasm_global(rt, index(a))?;
                let value = global.get(rt.ctx());
                self.wasm_to_js(rt, &value)?
            }
            "globalSet" => {
                let global = self.wasm_global(rt, index(a))?;
                let ty = global.ty(rt.ctx());
                if ty.mutability() == Mutability::Const {
                    return Err(Failure::Record("TypeError", "WebAssembly.Global: cannot set an immutable global".into()));
                }
                let value = self.wasm_from_js(rt, b, ty.content())?;
                global
                    .set(rt.ctx(), value)
                    .map_err(|e| Failure::Record("TypeError", format!("WebAssembly.Global: {e}")))?;
                Value::UNDEFINED
            }
            other => return Err(Failure::Record("TypeError", format!("WebAssembly: unknown operation {other}"))),
        })
    }

    fn wasm_instantiate(&mut self, rt: &mut WasmRuntime, module_id: usize, resolved: Value) -> Op<Value> {
        let compiled = self.wasm_module(rt, module_id)?;
        let types: Vec<ExternType> = compiled.module.imports().map(|i| i.ty()).collect();
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
                    let result_types: Vec<ValType> = ft.results().collect();
                    Extern::Func(Func::new(rt.ctx(), ft.clone(), move |caller, params, results| {
                        js_import(caller, index, &result_types, params, results)
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
                    let v = self.wasm_from_js(rt, value, gt.content())?;
                    let global = Global::new(rt.ctx(), GlobalType::new(gt.content().clone(), Mutability::Const), v)
                        .map_err(|e| Failure::Record("LinkError", format!("WebAssembly.Instance: import #{i}: {e}")))?;
                    Extern::Global(global)
                }
                _ => return Err(link("does not match the kind the module expects")),
            };
            externs.push(ext);
        }
        rt.ctx().data_mut().vm = self as *mut Vm<'p> as usize;
        let instance =
            Instance::new(rt.ctx(), &compiled.module, &externs).map_err(|e| self.wasm_failure(e, "LinkError"))?;
        rt.instances.push(instance);
        Ok(Value::num((rt.instances.len() - 1) as f64))
    }

    fn wasm_call(&mut self, rt: &mut WasmRuntime, id: usize, args: Value) -> Op<Value> {
        let func = *rt.funcs.get(id).ok_or_else(|| Failure::Record("TypeError", "no such function".into()))?;
        let ty = func.ty(rt.ctx());
        let given = self.wasm_items(args);
        let mut params = Vec::with_capacity(ty.params().len());
        for (i, t) in ty.params().enumerate() {
            let v = given.get(i).copied().unwrap_or(Value::UNDEFINED);
            params.push(self.wasm_from_js(rt, v, &t)?);
        }
        let mut results: Vec<Val> = ty.results().map(|t| Val::default_for_ty(&t).unwrap_or(Val::I32(0))).collect();
        rt.ctx().data_mut().vm = self as *mut Vm<'p> as usize;
        stats::time(1, || func.call(rt.ctx(), &params, &mut results)).map_err(|e| self.wasm_failure(e, "RuntimeError"))?;
        match results.len() {
            0 => Ok(Value::UNDEFINED),
            1 => self.wasm_to_js(rt, &results[0]),
            _ => {
                let mut out = Vec::with_capacity(results.len());
                for r in &results {
                    out.push(self.wasm_to_js(rt, r)?);
                }
                Ok(self.wasm_array(out))
            }
        }
    }

    /// An import's call: arguments in, the JavaScript function, result out.
    fn wasm_import_call(
        &mut self,
        rt: *mut WasmRuntime,
        index: usize,
        result_types: &[ValType],
        params: &[Val],
        results: &mut [Val],
    ) -> Result<(), Thrown> {
        // SAFETY: as in `wasm_op`.
        let rt = unsafe { &mut *rt };
        // The memory may have grown in the WebAssembly code that called this.
        self.wasm_detach_stale(rt);
        let to_js = |vm: &mut Self, rt: &mut WasmRuntime, p: &Val| match vm.wasm_to_js(rt, p) {
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
        let from_js = |vm: &mut Self, rt: &mut WasmRuntime, v: Value, t: &ValType| match vm.wasm_from_js(rt, v, t) {
            Ok(v) => Ok(v),
            Err(Failure::Thrown(t)) => Err(t),
            Err(Failure::Record(kind, message)) => Err(Thrown(format!("{kind}: {message}"))),
        };
        match results.len() {
            0 => {}
            1 => results[0] = from_js(self, rt, r, &result_types[0])?,
            n => {
                let items = self.wasm_items(r);
                for i in 0..n {
                    results[i] = from_js(self, rt, items.get(i).copied().unwrap_or(Value::UNDEFINED), &result_types[i])?;
                }
            }
        }
        Ok(())
    }

    /// What an error from instantiating or calling becomes: a JavaScript
    /// exception from an import stays itself (still pending in the VM); a
    /// trap is a `RuntimeError`; anything else is `otherwise`.
    fn wasm_failure(&mut self, e: wasmtime::Error, otherwise: &'static str) -> Failure {
        if let Some(js) = e.downcast_ref::<JsException>() {
            return Failure::Thrown(Thrown(js.0.clone()));
        }
        if let Some(trap) = e.downcast_ref::<Trap>() {
            // "integer divide by zero", without Wasmtime's "wasm trap: ".
            let message = trap.to_string();
            return Failure::Record("RuntimeError", message.trim_start_matches("wasm trap: ").to_string());
        }
        Failure::Record(otherwise, format!("{e:#}"))
    }

    /// A wasm value as JavaScript sees it.
    fn wasm_to_js(&mut self, rt: &mut WasmRuntime, v: &Val) -> Op<Value> {
        Ok(match v {
            Val::I32(i) => Value::num(*i as f64),
            Val::I64(i) => self.make_bigint(*i as i128),
            Val::F32(bits) => Value::num(f32::from_bits(*bits) as f64),
            Val::F64(bits) => Value::num(f64::from_bits(*bits)),
            Val::FuncRef(None) => Value::NULL,
            // A function crosses as its number; the layer's wrappers are made
            // from numbers on the JavaScript side.
            Val::FuncRef(Some(f)) => Value::num(rt.func_id(*f) as f64),
            Val::ExternRef(None) => Value::NULL,
            Val::ExternRef(Some(r)) => rt.extern_value(*r),
            _ => return Err(Failure::Record("TypeError", "WebAssembly: this value type cannot cross to JavaScript".into())),
        })
    }

    /// A JavaScript value as a wasm value of type `t`.
    fn wasm_from_js(&mut self, rt: &mut WasmRuntime, v: Value, t: &ValType) -> Op<Val> {
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
            ValType::F32 => Val::F32((self.to_number_strict(v)? as f32).to_bits()),
            ValType::F64 => Val::F64(self.to_number_strict(v)?.to_bits()),
            ValType::Ref(r) if r.heap_type().top().is_func() => {
                if v.is_null() {
                    Val::FuncRef(None)
                } else if v.is_number() {
                    let f = *rt.funcs.get(v.as_f64() as usize).ok_or_else(|| Failure::Record("TypeError", "not a WebAssembly function".into()))?;
                    Val::FuncRef(Some(f))
                } else {
                    return Err(Failure::Record("TypeError", "WebAssembly: a funcref must be null or a WebAssembly function".into()));
                }
            }
            ValType::Ref(r) if r.heap_type().top().is_extern() => {
                if v.is_null() { Val::ExternRef(None) } else { Val::ExternRef(Some(rt.extern_ref(v)?)) }
            }
            _ => return Err(Failure::Record("TypeError", "WebAssembly: this value type cannot cross from JavaScript".into())),
        })
    }

    /// Detach the buffers of the memories whose bytes moved or resized.
    pub(super) fn wasm_detach_stale(&mut self, rt: *mut WasmRuntime) {
        // SAFETY: as in `wasm_op`.
        let rt = unsafe { &mut *rt };
        for (i, ab) in rt.stale_buffers() {
            self.wasm_detach(ab);
            rt.memories[i].buffer = None;
        }
    }

    fn wasm_module(&mut self, rt: &mut WasmRuntime, id: usize) -> Op<Arc<Compiled>> {
        rt.modules
            .get(id)
            .cloned()
            .ok_or_else(|| Failure::Record("TypeError", "WebAssembly: not a WebAssembly.Module".into()))
    }

    fn wasm_table(&mut self, rt: &mut WasmRuntime, id: usize) -> Op<Table> {
        rt.tables.get(id).copied().ok_or_else(|| Failure::Record("TypeError", "no such table".into()))
    }

    fn wasm_global(&mut self, rt: &mut WasmRuntime, id: usize) -> Op<Global> {
        rt.globals.get(id).copied().ok_or_else(|| Failure::Record("TypeError", "no such global".into()))
    }
}

/// Whether two Wasmtime handles name the same entity. They have no
/// `PartialEq`; their `Debug` form is the store and the address or index
/// within it, which is the identity. Used so that a memory, table or global
/// exported twice is the same JavaScript object.
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
        ExternType::Tag(_) => "tag",
    }
}
