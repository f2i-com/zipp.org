use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use crate::config::ZippConfig;
use crate::error::ZippError;
use crate::object::{CompiledFunctionObject, Object};

/// ZK execution trace: (clock, pc, opcode, val_a, val_b, val_dst, const_val, aux) per step.
pub type ExecutionTrace = Vec<(u64, u64, u8, u64, u64, u64, u64, u64)>;
use crate::parser::parse_program_from_source;
use crate::rcompiler::RCompiler;
use crate::value::{obj_into_val, val_to_obj, Heap, Value};
use crate::vm::{ExecutionQuota, VM};

const BYTECODE_CACHE_CAPACITY: usize = 256;


#[derive(Clone)]
struct CachedBytecode {
    instructions: Rc<Vec<u8>>,
    constants: Rc<Vec<Object>>,
    num_cache_slots: u16,
    max_stack_depth: u16,
    register_count: u16,
}

pub struct ZippEngine {
    pub config: ZippConfig,
    bytecode_cache: RefCell<IndexMap<String, CachedBytecode>>,
    vm_pool: RefCell<Option<VM>>,
}

impl Default for ZippEngine {
    fn default() -> Self {
        Self {
            config: ZippConfig::default(),
            bytecode_cache: RefCell::new(IndexMap::new()),
            vm_pool: RefCell::new(None),
        }
    }
}

impl ZippEngine {
    pub fn with_config(config: ZippConfig) -> Self {
        Self {
            config,
            bytecode_cache: RefCell::new(IndexMap::new()),
            vm_pool: RefCell::new(None),
        }
    }

    /// Compile source (or retrieve from cache) and prepare a VM for execution.
    fn prepare_vm(&self, source: &str) -> Result<(CachedBytecode, VM), ZippError> {
        // True LRU: on a hit, move the entry to the back of the
        // IndexMap so the next eviction takes the *least* recently
        // used entry. Before this change the cache was FIFO regardless
        // of access pattern — frequently-used scripts could still get
        // evicted by an unrelated cold-call burst.
        let cached = {
            let mut cache = self.bytecode_cache.borrow_mut();
            if let Some(idx) = cache.get_index_of(source) {
                let last = cache.len() - 1;
                if idx != last {
                    cache.move_index(idx, last);
                }
                cache.get_index(last).map(|(_, v)| v.clone())
            } else {
                None
            }
        };
        let cached = if let Some(cached) = cached {
            cached
        } else {
            let (program, errors) = parse_program_from_source(source);
            if !errors.is_empty() {
                return Err(ZippError::Parse(errors.join(", ")));
            }
            let compiled = RCompiler::new()
                .compile_program(&program)
                .map_err(ZippError::Compile)?;
            crate::backend::validate::validate(&compiled).map_err(ZippError::Compile)?;
            let compiled = CachedBytecode {
                instructions: Rc::new(compiled.instructions),
                constants: Rc::new(compiled.constants),
                num_cache_slots: compiled.num_cache_slots,
                max_stack_depth: compiled.max_stack_depth,
                register_count: compiled.register_count,
            };
            {
                let mut cache = self.bytecode_cache.borrow_mut();
                if cache.len() >= BYTECODE_CACHE_CAPACITY {
                    cache.swap_remove_index(0);
                }
                cache.insert(source.to_string(), compiled.clone());
            }
            compiled
        };

        let mut vm = self.vm_pool.borrow_mut().take().unwrap_or_else(|| {
            VM::new_from_rc(
                Rc::clone(&cached.instructions),
                Rc::clone(&cached.constants),
                self.config.clone(),
                crate::vm::STACK_SIZE,
                cached.num_cache_slots,
                cached.max_stack_depth,
            )
        });
        // Re-sync the engine's current config onto the pooled VM. The
        // pool may have last seen a stale config from an earlier
        // `eval` — without this, an embedder that tightened limits
        // (e.g. `engine.config.max_instructions = Some(small)`) would
        // see the stricter value silently ignored on the next call
        // because the VM still held the lax pre-pool config. Mirrors
        // what `set_execution_limits` does for `ScriptState`.
        vm.config = self.config.clone();
        vm.enforce_limits = vm.config.requires_limit_checks();
        vm.reset_for_run(
            Rc::clone(&cached.instructions),
            Rc::clone(&cached.constants),
            cached.num_cache_slots,
            cached.max_stack_depth,
            cached.register_count,
        );
        Ok((cached, vm))
    }

    /// Return the VM to the pool for reuse.
    fn recycle_vm(&self, vm: VM) {
        *self.vm_pool.borrow_mut() = Some(vm);
    }

    pub fn eval(&self, source: &str) -> Result<Object, ZippError> {
        let (_cached, mut vm) = self.prepare_vm(source)?;
        let run_result = vm.run_register();
        // Drain queued microtasks before returning. queueMicrotask /
        // Promise then-chains push here while the script runs and only
        // execute at the boundary back to the host. Any error mid-drain
        // bubbles up the same way an in-script throw would.
        let drain_result = if run_result.is_ok() {
            vm.drain_microtasks().map(|_| ())
        } else {
            Ok(())
        };
        let last = vm.last_popped.take().unwrap_or(Value::UNDEFINED);
        let result = val_to_obj(last, &vm.heap);
        let err = run_result
            .and(drain_result)
            .err()
            .map(|e| ZippError::from_vm_error(&e, &vm.heap));
        self.recycle_vm(vm);
        if let Some(e) = err { return Err(e); }
        Ok(result)
    }

    /// Evaluate source code and return the result as a JSON string.
    /// Unlike `eval()` which returns an Object (with potential `[ref]` for nested heap values),
    /// this method serializes the result while the heap is still accessible, producing
    /// a complete JSON representation with all nested values fully resolved.
    ///
    /// Microtasks (`queueMicrotask`, Promise `.then` chains) are drained
    /// before serialisation, matching [`Self::eval`]. Without that drain,
    /// the pooled VM would carry the queue into the next `eval*` call.
    pub fn eval_to_json(&self, source: &str) -> Result<String, ZippError> {
        let (_cached, mut vm) = self.prepare_vm(source)?;
        let run_result = vm.run_register();
        let drain_result = if run_result.is_ok() {
            vm.drain_microtasks().map(|_| ())
        } else {
            Ok(())
        };
        let last = vm.last_popped.take().unwrap_or(Value::UNDEFINED);
        let json = Self::value_to_json(last, &vm.heap);
        let err = run_result
            .and(drain_result)
            .err()
            .map(|e| ZippError::from_vm_error(&e, &vm.heap));
        self.recycle_vm(vm);
        if let Some(e) = err { return Err(e); }
        Ok(json)
    }

    /// Evaluate source code, return JSON result + execution trace for ZK proving.
    /// The trace captures (clk, pc, opcode, val_a, val_b, val_dst, const_val, aux) per step.
    ///
    /// Like [`Self::eval_to_json`], drains microtasks after the script
    /// finishes so the pooled VM is left in the same shape `eval()` would.
    pub fn eval_with_trace(
        &self,
        source: &str,
    ) -> Result<(String, ExecutionTrace), ZippError> {
        let (_cached, mut vm) = self.prepare_vm(source)?;

        // Enable trace capture
        vm.trace_enabled = true;
        vm.trace_steps.clear();
        vm.trace_clk = 0;

        let run_result = vm.run_register();
        let drain_result = if run_result.is_ok() {
            vm.drain_microtasks().map(|_| ())
        } else {
            Ok(())
        };
        let last = vm.last_popped.take().unwrap_or(Value::UNDEFINED);
        let json = Self::value_to_json(last, &vm.heap);

        // Extract trace before returning VM to pool
        let trace = std::mem::take(&mut vm.trace_steps);
        vm.trace_enabled = false;

        let err = run_result
            .and(drain_result)
            .err()
            .map(|e| ZippError::from_vm_error(&e, &vm.heap));
        self.recycle_vm(vm);
        if let Some(e) = err { return Err(e); }
        Ok((json, trace))
    }

    /// Produce a correctly escaped JSON string literal using serde_json.
    /// Handles all control characters, unicode, backslashes, and quotes.
    fn json_string(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "null".to_string())
    }

    /// Convert a NaN-boxed Value to a JSON string, resolving all heap references.
    fn value_to_json(val: Value, heap: &Heap) -> String {
        let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
        Self::value_to_json_depth(val, heap, 0, &mut visited)
    }

    /// Maximum nesting depth for JSON serialization. The visited-set
    /// below catches true cycles (`a.self = a`) precisely; this cap is
    /// the secondary backstop for legitimately deep, non-cyclic
    /// structures (e.g. AST-shaped data) that would otherwise blow the
    /// Rust thread stack on a naive recursion.
    const JSON_MAX_DEPTH: u32 = 256;

    fn value_to_json_depth(
        val: Value,
        heap: &Heap,
        depth: u32,
        visited: &mut std::collections::HashSet<u32>,
    ) -> String {
        if val.is_i32() {
            return format!("{}", unsafe { val.as_i32_unchecked() });
        }
        if val.is_f64() {
            let f = val.as_f64();
            if f.is_nan() { return "null".to_string(); }
            if f.is_infinite() { return "null".to_string(); }
            if f.fract() == 0.0 && f.abs() < i64::MAX as f64 {
                return format!("{}", f as i64);
            }
            return format!("{}", f);
        }
        if val.is_bool() {
            return if unsafe { val.as_bool_unchecked() } { "true" } else { "false" }.to_string();
        }
        if val.is_null() || val.is_undefined() {
            return "null".to_string();
        }
        if val.is_inline_str() {
            let (buf, len) = val.inline_str_buf();
            let s = std::str::from_utf8(&buf[..len]).unwrap_or("");
            return Self::json_string(s);
        }
        if val.is_heap() {
            if depth >= Self::JSON_MAX_DEPTH {
                return "\"[Circular]\"".to_string();
            }
            let idx = val.heap_index();
            // True cycle detection: a heap_index is "circular" only if
            // it is an *ancestor* in the current traversal path. A
            // sibling that happens to share the same node renders
            // normally the second time. Compare with the previous
            // depth-only check, which would have falsely flagged
            // wide-but-shallow shared structures as Circular once
            // depth > 256 even when there was no actual cycle.
            if !visited.insert(idx) {
                return "\"[Circular]\"".to_string();
            }
            let out = Self::object_to_json_depth(
                heap.get(idx),
                heap,
                depth + 1,
                visited,
            );
            visited.remove(&idx);
            return out;
        }
        "null".to_string()
    }

    /// Convert an Object to a JSON string, resolving all nested heap
    /// references. The shared `visited` set tracks heap_index values
    /// active in the current path; a re-entry on the same index
    /// returns `"[Circular]"` without stack-overflowing the host.
    fn object_to_json_depth(
        obj: &Object,
        heap: &Heap,
        depth: u32,
        visited: &mut std::collections::HashSet<u32>,
    ) -> String {
        match obj {
            Object::Integer(v) => format!("{}", v),
            Object::Float(v) => {
                if v.is_nan() || v.is_infinite() { "null".to_string() }
                else if v.fract() == 0.0 && v.abs() < i64::MAX as f64 { format!("{}", *v as i64) }
                else { format!("{}", v) }
            }
            Object::Boolean(v) => format!("{}", v),
            Object::Null | Object::Undefined => "null".to_string(),
            Object::String(v) => Self::json_string(v),
            Object::Array(items) => {
                if depth >= Self::JSON_MAX_DEPTH {
                    return "\"[Circular]\"".to_string();
                }
                let borrowed = items.borrow();
                let elements: Vec<String> = borrowed.iter()
                    .map(|v| Self::value_to_json_depth(*v, heap, depth + 1, visited))
                    .collect();
                format!("[{}]", elements.join(", "))
            }
            Object::Hash(h) => {
                if depth >= Self::JSON_MAX_DEPTH {
                    return "\"[Circular]\"".to_string();
                }
                let h = h.borrow();
                let entries: Vec<String> = h.pairs.keys().enumerate()
                    .map(|(i, k)| {
                        let v = h.values.get(i)
                            .map(|v| Self::value_to_json_depth(*v, heap, depth + 1, visited))
                            .unwrap_or_else(|| "null".to_string());
                        format!("{}: {}", Self::json_string(&k.to_string()), v)
                    })
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
            Object::Instance(inst) => {
                if depth >= Self::JSON_MAX_DEPTH {
                    return "\"[Circular]\"".to_string();
                }
                let entries: Vec<String> = inst.fields.iter()
                    .map(|(k, v)| format!("{}: {}", Self::json_string(k), Self::value_to_json_depth(*v, heap, depth + 1, visited)))
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
            Object::Error(err) => Self::json_string(&format!("Error: {}", err.message)),
            Object::ReturnValue(v) => Self::object_to_json_depth(v, heap, depth, visited),
            _ => Self::json_string(&obj.inspect()),
        }
    }

    /// Parse, compile, and execute top-level code, keeping the VM alive for
    /// subsequent `call_function` / `get_global` / `set_global` calls.
    pub fn init_script(&self, source: &str) -> Result<ScriptState, ZippError> {
        let mut state = self.compile_script(source)?;
        state.run_init()?;
        Ok(state)
    }

    /// Parse and compile top-level code WITHOUT executing it.
    /// Returns a `ScriptState` with the VM ready to run. Call `run_init()` on the
    /// returned state after setting up bridges (db, localStorage, etc.).
    pub fn compile_script(&self, source: &str) -> Result<ScriptState, ZippError> {
        let (program, errors) = parse_program_from_source(source);
        if !errors.is_empty() {
            // Fail closed unless the embedder explicitly opted into
            // partial-parse execution. Running a prefix of a malformed
            // script is almost always worse than failing — the script
            // can stash partially-initialised state into globals and
            // mislead the next call.
            if program.statements.is_empty() || !self.config.allow_partial_parse {
                return Err(ZippError::Parse(errors.join(", ")));
            }
            // Opt-in path: show unique ROOT errors (not cascading) so the
            // embedder can audit what was skipped.
            let root_errors: Vec<&String> = errors.iter()
                .filter(|e| e.contains("(line") || (!e.starts_with("no prefix")))
                .collect();
            let preview: Vec<&str> = root_errors.iter().take(15).map(|s| s.as_str()).collect();
            eprintln!(
                "[Zipp] {} parser warnings ({} root causes, {} statements, allow_partial_parse=true). Roots: {}",
                errors.len(),
                root_errors.len(),
                program.statements.len(),
                preview.join(" | ")
            );
        }

        let compiled = RCompiler::new()
            .compile_program_persistent(&program)
            .map_err(ZippError::Compile)?;
        crate::backend::validate::validate(&compiled).map_err(ZippError::Compile)?;
        let globals_table = compiled.globals_table.clone();
        let next_global_slot = compiled.next_global_slot;
        let register_count = compiled.register_count;

        let instructions = Rc::new(compiled.instructions);
        let constants = Rc::new(compiled.constants);

        let mut vm = VM::new_from_rc(
            Rc::clone(&instructions),
            Rc::clone(&constants),
            self.config.clone(),
            crate::vm::STACK_SIZE,
            compiled.num_cache_slots,
            compiled.max_stack_depth,
        );
        vm.reset_for_run(
            Rc::clone(&instructions),
            Rc::clone(&constants),
            compiled.num_cache_slots,
            compiled.max_stack_depth,
            register_count,
        );

        Ok(ScriptState {
            vm,
            globals_table,
            next_global_slot,
            gc_threshold: ScriptState::GC_INITIAL_THRESHOLD,
            eval_cache: RefCell::new(IndexMap::new()),
        })
    }
}

/// Persistent script state: a VM with its globals still alive, plus the
/// name→slot mapping so callers can look up variables and functions by name.
pub struct ScriptState {
    pub(crate) vm: VM,
    pub(crate) globals_table: FxHashMap<String, u16>,
    /// One past the highest global slot the script (and its inner
    /// closures) has claimed. Used by [`Self::set_global`] when
    /// allocating a fresh slot for a runtime-defined global —
    /// `globals_table.len()` is **not** the right starting index
    /// because inner closures can reserve private slots not
    /// represented in `globals_table`.
    pub(crate) next_global_slot: u16,
    /// Dynamic GC threshold: scales to 2x live objects after each collection.
    /// Prevents the O(N²) GC storm where a static threshold triggers collection
    /// on every single function call once the heap has grown past it.
    gc_threshold: usize,
    /// Compiled-bytecode cache for [`Self::eval_in_context`] keyed on the
    /// source string. Spares the parse+compile round-trip when the same
    /// expression is eval'd repeatedly (event handlers, benchmark harnesses,
    /// REPL rerun of the last input, …). Bounded by `EVAL_CACHE_CAPACITY`
    /// to keep long-lived states from growing unbounded.
    eval_cache: RefCell<IndexMap<String, CachedBytecode>>,
}

const EVAL_CACHE_CAPACITY: usize = 128;

impl ScriptState {
    const GC_INITIAL_THRESHOLD: usize = 4096;

    /// Execute the compiled top-level code (the "init" phase).
    /// Call this after setting up bridges (db, localStorage, etc.) on the ScriptState.
    pub fn run_init(&mut self) -> Result<(), ZippError> {
        // Ensure built-in namespaces (Object, Array, etc.) are available as globals
        self.init_builtin_globals();
        self.vm
            .run_register()
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))
    }

    /// Set up built-in global namespaces (Object, Math, etc.) as global values.
    /// The stack VM creates these lazily, but the register VM needs them pre-set.
    fn init_builtin_globals(&mut self) {
        use crate::object::*;
        // Ensure "Object" exists in globals (may not if compiled as local-only).
        // Use `next_global_slot` rather than `globals.high_water_mark()` —
        // the high-water mark only reflects slots that have been
        // *written*, not slots the compiler has reserved for inner
        // closures. Picking a slot at the high-water mark could
        // collide with a closure-captured slot whose write hasn't
        // happened yet.
        if !self.globals_table.contains_key("Object") {
            let slot = self.next_global_slot;
            self.globals_table.insert("Object".to_string(), slot);
            self.next_global_slot = self.next_global_slot.saturating_add(1);
        }
        if let Some(&slot) = self.globals_table.get("Object") {
            let mut hash = HashObject::default();
            let fns: &[(&str, BuiltinFunction)] = &[
                ("keys", BuiltinFunction::ObjectKeys),
                ("values", BuiltinFunction::ObjectValues),
                ("entries", BuiltinFunction::ObjectEntries),
                ("fromEntries", BuiltinFunction::ObjectFromEntries),
                ("hasOwn", BuiltinFunction::ObjectHasOwn),
                ("is", BuiltinFunction::ObjectIs),
                ("assign", BuiltinFunction::ObjectAssign),
                ("freeze", BuiltinFunction::ObjectFreeze),
                ("create", BuiltinFunction::ObjectCreate),
                ("defineProperty", BuiltinFunction::ObjectDefineProperty),
                ("getPrototypeOf", BuiltinFunction::ObjectGetPrototypeOf),
                ("getOwnPropertyDescriptor", BuiltinFunction::ObjectGetOwnPropertyDescriptor),
                ("getOwnPropertyNames", BuiltinFunction::ObjectGetOwnPropertyNames),
            ];
            for (name, func) in fns {
                hash.insert_pair_obj(
                    HashKey::from_string(name),
                    Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                        function: func.clone(),
                        receiver: None,
                    })),
                );
            }
            // Also add prototype with hasOwnProperty
            let mut proto = HashObject::default();
            proto.insert_pair_obj(
                HashKey::from_string("hasOwnProperty"),
                Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::HashHasOwnProperty,
                    receiver: None,
                })),
            );
            hash.insert_pair_obj(HashKey::from_string("prototype"), make_hash(proto));

            let obj_val = obj_into_val(make_hash(hash), &mut self.vm.heap);
            unsafe { self.vm.globals.set_unchecked(slot as usize, obj_val) };
        }
    }

    /// Snapshot global slot values (up to high-water mark).
    /// Used to restore globals to post-init state between calls, preventing
    /// state bleed across requests on the same worker thread.
    pub fn snapshot_globals(&self) -> Vec<Value> {
        let hwm = self.vm.globals.high_water_mark();
        let mut snapshot = Vec::with_capacity(hwm);
        for i in 0..hwm {
            snapshot.push(unsafe { self.vm.globals.get_unchecked(i) });
        }
        snapshot
    }

    /// Restore global slot values from a snapshot taken after init.
    /// Any globals written since the snapshot are reverted, ensuring each
    /// handler call starts from a clean state.
    ///
    /// Earlier this only overwrote slots within the snapshot range — new
    /// globals the handler installed past `snapshot.len()` survived the
    /// "restore" and were visible to the next call. The `truncate_to`
    /// step rolls the high-water mark and clears those leaked slots so
    /// a restore really does return to the snapshot shape.
    pub fn restore_globals(&mut self, snapshot: &[Value]) {
        for (i, &val) in snapshot.iter().enumerate() {
            unsafe { self.vm.globals.set_unchecked(i, val) };
        }
        self.vm.globals.truncate_to(snapshot.len());
    }

    /// Return a reference to the globals table (name → slot index).
    pub fn globals_table(&self) -> &FxHashMap<String, u16> {
        &self.globals_table
    }

    /// Check if a global slot has been written since the last `clear_dirty()`.
    #[inline]
    pub fn is_global_dirty(&self, index: u16) -> bool {
        self.vm.globals.is_dirty(index as usize)
    }

    /// Clear all dirty bits (call after syncing state to React).
    #[inline]
    pub fn clear_dirty(&self) {
        self.vm.globals.clear_dirty();
    }

    /// Read a global variable by slot index. `index` is a `u16` and so
    /// always fits inside the `GLOBALS_SIZE` (65536) slot space. Slots
    /// the script never wrote to read back as `Object::Undefined`.
    pub fn get_global_by_index(&self, index: u16) -> Object {
        // u16 max (65_535) is always < GLOBALS_SIZE (65_536), so the
        // SharedGlobals::get_unchecked call is in bounds by construction.
        debug_assert!((index as usize) < crate::vm::GLOBALS_SIZE);
        let val = unsafe { self.vm.globals.get_unchecked(index as usize) };
        val_to_obj(val, &self.vm.heap)
    }

    /// Write a global variable by slot index. Same `u16`-bounded safety
    /// as [`Self::get_global_by_index`]. Writes to slots outside the
    /// `globals_table` are tolerated — embedders sometimes pre-stash
    /// values in slots the script will read later — but the slot index
    /// must still be < `GLOBALS_SIZE`, which `u16` enforces statically.
    pub fn set_global_by_index(&mut self, index: u16, value: Object) {
        debug_assert!((index as usize) < crate::vm::GLOBALS_SIZE);
        let val = obj_into_val(value, &mut self.vm.heap);
        unsafe { self.vm.globals.set_unchecked(index as usize, val) };
    }

    /// Call a named function defined in the script.
    /// Resets the execution quota so each call gets a fresh instruction/time budget.
    pub fn call_function(&mut self, name: &str, args: &[Object]) -> Result<Object, ZippError> {
        self.vm.quota = ExecutionQuota::default();

        let &slot = self
            .globals_table
            .get(name)
            .ok_or_else(|| ZippError::TypeError(format!("undefined function: {}", name)))?;

        // Read the function object from the global slot
        let val = unsafe { self.vm.globals.get_unchecked(slot as usize) };
        if !val.is_heap() {
            return Err(ZippError::TypeError(format!("{} is not a function", name)));
        }
        let func = match self.vm.heap.get(val.heap_index()) {
            Object::CompiledFunction(f) => f.clone(),
            _ => return Err(ZippError::TypeError(format!("{} is not a function", name))),
        };

        // Convert args to Values and place them on the stack
        let arg_start = self.vm.sp;
        // Reserve a dummy register for callee (Call opcode layout: base = callee, base+1.. = args)
        // call_register_direct expects args starting at arg_stack_start
        for arg in args {
            let v = obj_into_val(arg.clone(), &mut self.vm.heap);
            if self.vm.sp >= self.vm.stack.len() {
                self.vm.stack.push(v);
            } else {
                self.vm.stack[self.vm.sp] = v;
            }
            self.vm.sp += 1;
        }
        let nargs = args.len();

        // SAFETY: func pointers are derived from Rc-backed CompiledFunctionObject
        // fields that remain valid for the duration of the call.
        //
        // Restore `sp` *before* surfacing the error so that a thrown
        // exception inside `func` doesn't leave the just-pushed
        // arguments stuck on the VM stack — the next public call on
        // the persisted `ScriptState` would otherwise pick up the
        // stale args, GC them as roots, and either corrupt the call
        // it was meant to make or exhaust the stack.
        let call_result = unsafe {
            self.vm.call_register_direct(
                func.instructions.as_ptr(),
                func.instructions.len(),
                &*func.constants as *const std::vec::Vec<Object>,
                func.rest_parameter_index,
                func.takes_this,
                func.is_async,
                func.num_cache_slots,
                func.max_stack_depth,
                func.register_count,
                Rc::as_ptr(&func.inline_cache),
                arg_start,
                nargs,
                None,
            )
        };
        self.vm.sp = arg_start;
        let result_val = call_result
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;

        let result = val_to_obj(result_val, &self.vm.heap);

        // Drain queued microtasks before returning to the embedder.
        // queueMicrotask / Promise.then chains pushed during the call
        // would otherwise leak into a subsequent call and fire mid-
        // way through unrelated work — exactly the surprise `eval()`
        // already protects against. Mirrors the `eval` flow: drain on
        // success only; a thrown error has already preempted further
        // script work.
        self.vm
            .drain_microtasks()
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;

        // Trigger GC when live heap objects exceed dynamic threshold
        if self.vm.heap.allocated_count() > self.gc_threshold {
            self.gc_collect();
            // Scale threshold to 2x live objects, never below initial
            self.gc_threshold = std::cmp::max(
                Self::GC_INITIAL_THRESHOLD,
                self.vm.heap.allocated_count() * 2,
            );
        }

        Ok(result)
    }

    /// Call a Value (function closure / compiled function) with Object arguments.
    /// Used for dispatching event handlers stored as Values in event_listeners.
    pub fn call_value(&mut self, callee: Value, args: &[Object]) -> Result<Object, ZippError> {
        let arg_start = self.vm.sp;
        for arg in args {
            let v = obj_into_val(arg.clone(), &mut self.vm.heap);
            if self.vm.sp >= self.vm.stack.len() {
                self.vm.stack.push(v);
            } else {
                self.vm.stack[self.vm.sp] = v;
            }
            self.vm.sp += 1;
        }

        let arg_vals: Vec<Value> = (arg_start..self.vm.sp)
            .map(|i| self.vm.stack[i])
            .collect();

        // Restore `sp` before surfacing the error — a thrown exception
        // inside the callee must not leave the args parked on the
        // stack across the public-API boundary. See `call_function`
        // above for the failure shape this protects against.
        let call_result = self.vm.call_value_slice(callee, &arg_vals);
        self.vm.sp = arg_start;
        let result_val = call_result
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;
        let result = val_to_obj(result_val, &self.vm.heap);

        // Drain microtasks before returning — see `call_function`.
        self.vm
            .drain_microtasks()
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;

        if self.vm.heap.allocated_count() > self.gc_threshold {
            self.gc_collect();
            self.gc_threshold = std::cmp::max(
                Self::GC_INITIAL_THRESHOLD,
                self.vm.heap.allocated_count() * 2,
            );
        }

        Ok(result)
    }

    /// Call a CompiledFunctionObject directly (for functions stored in hash objects,
    /// e.g. component renderers registered via `registerComponent`).
    pub fn call_compiled_function(
        &mut self,
        func: &CompiledFunctionObject,
        args: &[Object],
    ) -> Result<Object, ZippError> {
        let arg_start = self.vm.sp;
        for arg in args {
            let v = obj_into_val(arg.clone(), &mut self.vm.heap);
            if self.vm.sp >= self.vm.stack.len() {
                self.vm.stack.push(v);
            } else {
                self.vm.stack[self.vm.sp] = v;
            }
            self.vm.sp += 1;
        }
        let nargs = args.len();

        // SAFETY: func pointers are derived from Rc-backed CompiledFunctionObject
        // fields that remain valid for the duration of the call.
        // Restore `sp` before propagating any error so the pushed
        // args don't outlive a thrown exception. See `call_function`.
        let call_result = unsafe {
            self.vm.call_register_direct(
                func.instructions.as_ptr(),
                func.instructions.len(),
                &*func.constants as *const std::vec::Vec<Object>,
                func.rest_parameter_index,
                func.takes_this,
                func.is_async,
                func.num_cache_slots,
                func.max_stack_depth,
                func.register_count,
                Rc::as_ptr(&func.inline_cache),
                arg_start,
                nargs,
                None,
            )
        };
        self.vm.sp = arg_start;
        let result_val = call_result
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;

        let result = val_to_obj(result_val, &self.vm.heap);

        // Drain microtasks before returning — see `call_function`.
        self.vm
            .drain_microtasks()
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;

        if self.vm.heap.allocated_count() > self.gc_threshold {
            self.gc_collect();
            self.gc_threshold = std::cmp::max(
                Self::GC_INITIAL_THRESHOLD,
                self.vm.heap.allocated_count() * 2,
            );
        }

        Ok(result)
    }

    /// Evaluate an expression in the script's global context.
    /// The expression has access to all script variables and functions via
    /// the existing globals table. Uses the script's own VM (heap, globals, etc.)
    /// with a temporary bytecode swap.
    ///
    /// Compiled bytecode is cached in `eval_cache` (LRU, capped at
    /// [`EVAL_CACHE_CAPACITY`]) so repeated calls with the same source skip
    /// the parse+compile round-trip. This is the hot path for event handlers
    /// and benchmark harnesses that invoke the same expression many times.
    pub fn eval_in_context(&mut self, source: &str) -> Result<Object, ZippError> {
        // ── Cache lookup (LRU) ──
        // Move the entry to the back of the IndexMap on a hit so the
        // next eviction takes the truly oldest item. The previous
        // implementation evicted in insertion order regardless of how
        // often a given expression was used.
        let cached = {
            let mut cache = self.eval_cache.borrow_mut();
            if let Some(idx) = cache.get_index_of(source) {
                let last = cache.len() - 1;
                if idx != last {
                    cache.move_index(idx, last);
                }
                cache.get_index(last).map(|(_, v)| v.clone())
            } else {
                None
            }
        };
        let compiled = if let Some(c) = cached {
            c
        } else {
            let (program, errors) = parse_program_from_source(source);
            if !errors.is_empty() {
                if program.statements.is_empty() || !self.vm.config.allow_partial_parse {
                    return Err(ZippError::Parse(errors.join(", ")));
                }
                eprintln!(
                    "[Zipp] eval_in_context: {} parser warnings (continuing with {} statements, allow_partial_parse=true)",
                    errors.len(),
                    program.statements.len()
                );
            }

            // Compile with the script's globals table so GetGlobal uses correct indices.
            // Use the *persistent* compile entry point so any top-level
            // `let`/`function` introduced by this snippet survives into
            // `globals_table` — the previous `compile_program` call
            // treated the snippet as a function scope and left top-level
            // bindings stuck in registers, so `> let x = 1` followed by
            // `> x + 1` saw `x` as undefined despite the first eval
            // appearing to succeed.
            let raw = RCompiler::with_globals(&self.globals_table)
                .compile_program_persistent(&program)
                .map_err(ZippError::Compile)?;
            crate::backend::validate::validate(&raw).map_err(ZippError::Compile)?;

            // Merge any newly-allocated slots (and the updated
            // high-water mark) back into the persistent state so a
            // later `set_global` / `get_global` / next eval sees
            // consistent slot indices. Without this merge, an
            // `eval_in_context("let y = 2")` would allocate a slot
            // for `y` that the parent `ScriptState` never learns
            // about — the next compile would silently re-allocate
            // the same slot for a different name. Bumping
            // `next_global_slot` here is also what stops the cached
            // bytecode from going stale relative to the parent.
            for (name, slot) in &raw.globals_table {
                self.globals_table.entry(name.clone()).or_insert(*slot);
            }
            if raw.next_global_slot > self.next_global_slot {
                self.next_global_slot = raw.next_global_slot;
            }

            let entry = CachedBytecode {
                instructions: Rc::new(raw.instructions),
                constants: Rc::new(raw.constants),
                num_cache_slots: raw.num_cache_slots,
                max_stack_depth: raw.max_stack_depth,
                register_count: raw.register_count,
            };
            {
                let mut cache = self.eval_cache.borrow_mut();
                if cache.len() >= EVAL_CACHE_CAPACITY {
                    cache.swap_remove_index(0);
                }
                cache.insert(source.to_string(), entry.clone());
            }
            entry
        };

        // Save VM state
        let saved_instructions =
            std::mem::replace(&mut self.vm.instructions, Rc::clone(&compiled.instructions));
        let saved_constants =
            std::mem::replace(&mut self.vm.constants, Rc::clone(&compiled.constants));
        let saved_ip = self.vm.ip;
        let saved_sp = self.vm.sp;
        let saved_register_count = self.vm.register_count;
        let saved_max_stack_depth = self.vm.max_stack_depth;
        let saved_inline_cache = std::mem::replace(
            &mut self.vm.inline_cache,
            vec![(0, 0); compiled.num_cache_slots as usize],
        );

        // Reset for expression evaluation
        self.vm.ip = 0;
        self.vm.inst_ptr = self.vm.instructions.as_ptr();
        self.vm.inst_len = self.vm.instructions.len();
        self.vm.register_count = compiled.register_count;
        self.vm.max_stack_depth = compiled.max_stack_depth as usize;

        // Run the expression
        let run_result = self.vm.run_register();
        // Drain microtasks before returning, mirroring the
        // top-level `eval` flow. Without this, an
        // `eval_in_context("queueMicrotask(f)")` would silently
        // park `f` until the next eval ran.
        let drain_result = if run_result.is_ok() {
            self.vm.drain_microtasks().map(|_| ())
        } else {
            Ok(())
        };

        // Get result
        let last = self.vm.last_popped.take().unwrap_or(Value::UNDEFINED);
        let result = val_to_obj(last, &self.vm.heap);

        // Restore VM state
        self.vm.instructions = saved_instructions;
        self.vm.constants = saved_constants;
        self.vm.ip = saved_ip;
        self.vm.sp = saved_sp;
        self.vm.register_count = saved_register_count;
        self.vm.max_stack_depth = saved_max_stack_depth;
        self.vm.inline_cache = saved_inline_cache;
        self.vm.inst_ptr = self.vm.instructions.as_ptr();
        self.vm.inst_len = self.vm.instructions.len();
        // Invalidate constants pointers — they'll be re-set on next run_register
        self.vm.constants_values_ptr = std::ptr::null();
        self.vm.constants_raw = &*self.vm.constants as *const Vec<Object>;

        run_result
            .and(drain_result)
            .map_err(|e| ZippError::from_vm_error(&e, &self.vm.heap))?;
        Ok(result)
    }

    /// Read a global variable by name.
    pub fn get_global(&self, name: &str) -> Result<Object, String> {
        let &slot = self
            .globals_table
            .get(name)
            .ok_or_else(|| format!("undefined variable: {}", name))?;
        let val = unsafe { self.vm.globals.get_unchecked(slot as usize) };
        Ok(val_to_obj(val, &self.vm.heap))
    }

    /// Get a reference to the VM heap (for converting Values to Objects).
    pub fn heap(&self) -> &crate::value::Heap {
        &self.vm.heap
    }

    /// Get a mutable reference to the VM heap (for allocating Objects as Values).
    pub fn heap_mut(&mut self) -> &mut crate::value::Heap {
        &mut self.vm.heap
    }

    /// Write a global variable by name.
    /// If the variable does not exist, it is created as a new runtime global.
    ///
    /// New runtime globals start at `self.next_global_slot`, **not**
    /// `self.globals_table.len()`. The compiler may have allocated
    /// private slots for inner closures (captured locals, IIFE
    /// parameter mirrors) that aren't surfaced in the user-visible
    /// `globals_table`; the previous version of this function used
    /// `globals_table.len()` and could happily clobber one of those
    /// slots, silently corrupting the script's persistent state.
    pub fn set_global(&mut self, name: &str, value: Object) -> Result<(), String> {
        let slot = if let Some(&s) = self.globals_table.get(name) {
            s
        } else {
            let next = self.next_global_slot;
            if (next as usize) >= crate::vm::GLOBALS_SIZE {
                return Err("too many global variables".to_string());
            }
            self.globals_table.insert(name.to_string(), next);
            self.next_global_slot = next
                .checked_add(1)
                .ok_or_else(|| "global slot counter overflow".to_string())?;
            next
        };
        let val = obj_into_val(value, &mut self.vm.heap);
        unsafe { self.vm.globals.set_unchecked(slot as usize, val) };
        Ok(())
    }

    /// Attach a localStorage backend to the VM.
    pub fn set_local_storage(
        &mut self,
        storage: Box<dyn crate::local_storage::LocalStorageBridge>,
    ) {
        self.vm.local_storage = Some(storage);
    }

    /// Attach a database backend to the VM.
    pub fn set_db(&mut self, db: Box<dyn crate::db_bridge::DbBridge>) {
        self.vm.db = Some(db);
    }

    /// Attach a 2D drawing backend to the VM.
    pub fn set_draw(&mut self, draw: Box<dyn crate::draw_bridge::DrawBridge>) {
        self.vm.draw = Some(draw);
    }

    /// Attach a layout engine backend to the VM.
    pub fn set_layout(&mut self, layout: Box<dyn crate::layout_bridge::LayoutBridge>) {
        self.vm.layout = Some(layout);
    }

    /// Attach an input/event state backend to the VM.
    pub fn set_input(&mut self, input: Box<dyn crate::input_bridge::InputBridge>) {
        self.vm.input = Some(input);
    }

    /// Attach an HTTP backend to the VM (server-side).
    pub fn set_http(&mut self, http: Box<dyn crate::http_bridge::HttpBridge>) {
        self.vm.http = Some(http);
    }

    /// Attach a file system backend to the VM (server-side, scoped).
    pub fn set_fs(&mut self, fs: Box<dyn crate::fs_bridge::FsBridge>) {
        self.vm.fs = Some(fs);
    }

    /// Attach an environment variable backend to the VM (server-side).
    pub fn set_env(&mut self, env: Box<dyn crate::env_bridge::EnvBridge>) {
        self.vm.env = Some(env);
    }

    /// Set execution limits for script calls. Useful for server environments
    /// where untrusted scripts must be bounded.
    ///
    /// Recomputes `enforce_limits` from *all* configured limit fields,
    /// not just the two passed here. The previous implementation only
    /// inspected `max_instructions` / `max_wall_time_ms`, so calling
    /// `state.set_execution_limits(None, None)` would also disable
    /// heap-object / heap-byte / abort-flag checks even if those were
    /// still set on the engine config.
    pub fn set_execution_limits(
        &mut self,
        max_instructions: Option<u64>,
        max_wall_time_ms: Option<u64>,
    ) {
        self.vm.config.max_instructions = max_instructions;
        self.vm.config.max_wall_time_ms = max_wall_time_ms;
        self.vm.enforce_limits = self.vm.config.requires_limit_checks();
    }

    /// Read-only access to the VM (for inspecting localStorage etc.).
    pub fn vm(&self) -> &VM {
        &self.vm
    }

    /// Mutable access to the VM (for localStorage mutations etc.).
    pub fn vm_mut(&mut self) -> &mut VM {
        &mut self.vm
    }

    /// Run garbage collection on the VM heap.
    ///
    /// The heap is append-only during normal execution — temporary objects from
    /// function calls accumulate and are never freed. This method performs a
    /// mark-sweep to identify heap objects still referenced by globals, then
    /// either nulls out unreachable slots or compacts the heap.
    ///
    /// Called automatically by `call_function` when the heap exceeds a
    /// threshold, or can be called manually.
    pub fn gc_collect(&mut self) {
        let heap_len = self.vm.heap.objects.len();
        if heap_len == 0 {
            return;
        }

        // Phase 1: Mark — find all heap indices reachable from globals.
        let mut reachable = vec![false; heap_len];

        // Scan only initialized global slots (up to high-water mark instead of all 65536).
        let globals_limit = self.vm.globals.high_water_mark();
        for i in 0..globals_limit {
            let val = unsafe { self.vm.globals.get_unchecked(i) };
            if val.is_heap() {
                let idx = val.heap_index() as usize;
                if idx < heap_len {
                    reachable[idx] = true;
                    // Recursively mark objects reachable from this heap object
                    mark_object_refs(&self.vm.heap.objects[idx], &mut reachable, &self.vm.heap);
                }
            }
        }

        // Also scan the VM stack (values below sp may still hold heap refs
        // from the just-completed function call, e.g. closures pushed as args).
        for i in 0..self.vm.sp {
            mark_value(&self.vm.stack[i], &mut reachable, &self.vm.heap);
        }

        // Scan current locals (may contain heap-referencing Objects).
        for obj in &self.vm.locals {
            mark_nested_object(obj, &mut reachable, &self.vm.heap);
        }

        // Scan call frames (each has locals and constants).
        for frame in &self.vm.frames {
            for obj in &frame.locals {
                mark_nested_object(obj, &mut reachable, &self.vm.heap);
            }
            for obj in frame.constants.iter() {
                mark_nested_object(obj, &mut reachable, &self.vm.heap);
            }
        }

        // Scan register call frames (rframes) — saved caller state for
        // iterative register dispatch. closure_saves hold Values that may
        // reference heap objects, and constants_raw points to a constants
        // pool whose objects may share Rc identity with heap entries.
        for rframe in &self.vm.rframes {
            for (_reg, val) in &rframe.closure_saves {
                mark_value(val, &mut reachable, &self.vm.heap);
            }
            if !rframe.constants_raw.is_null() {
                let constants = unsafe { &*rframe.constants_raw };
                for obj in constants.iter() {
                    mark_nested_object(obj, &mut reachable, &self.vm.heap);
                }
            }
        }

        // Scan current constants pool.
        for obj in self.vm.constants.iter() {
            mark_nested_object(obj, &mut reachable, &self.vm.heap);
        }

        // Scan last_popped and arg_buffer for stale heap refs.
        if let Some(ref val) = self.vm.last_popped {
            mark_value(val, &mut reachable, &self.vm.heap);
        }
        for val in &self.vm.arg_buffer {
            mark_value(val, &mut reachable, &self.vm.heap);
        }

        // Scan event listener handler Values (closures registered via addEventListener).
        for handlers in self.vm.event_listeners.values() {
            for val in handlers {
                mark_value(val, &mut reachable, &self.vm.heap);
            }
        }

        // Scan host call callback Values (closures pending async resolution).
        for val in self.vm.host_callbacks.values() {
            mark_value(val, &mut reachable, &self.vm.heap);
        }

        // Scan pre-converted constants cache — contains Values pointing to
        // cloned constants on the heap. Must be marked BEFORE clearing so that
        // any heap objects shared between the cache and globals/constants are
        // not incorrectly freed.
        for (_key, values) in &self.vm.constants_values_cache {
            for val in values {
                mark_value(val, &mut reachable, &self.vm.heap);
            }
        }
        // Also scan the scratch buffer (may contain stale Values from last build)
        for val in &self.vm.constants_values_buf {
            mark_value(val, &mut reachable, &self.vm.heap);
        }

        // Scan pooled locals — returned-to-pool Vec<Object> entries may hold
        // Rc-based types (Hash, Array) that share heap identity.
        for pool_entry in &self.vm.locals_pool {
            for obj in pool_entry {
                mark_nested_object(obj, &mut reachable, &self.vm.heap);
            }
        }

        // Scan new_target (constructor target, may be a heap reference).
        mark_value(&self.vm.new_target, &mut reachable, &self.vm.heap);

        // Scan cached typeof Values (lazily-initialized heap-allocated strings).
        mark_value(&self.vm.typeof_undefined, &mut reachable, &self.vm.heap);
        mark_value(&self.vm.typeof_number, &mut reachable, &self.vm.heap);
        mark_value(&self.vm.typeof_string, &mut reachable, &self.vm.heap);
        mark_value(&self.vm.typeof_boolean, &mut reachable, &self.vm.heap);
        mark_value(&self.vm.typeof_function, &mut reachable, &self.vm.heap);
        mark_value(&self.vm.typeof_object, &mut reachable, &self.vm.heap);

        // Clear pre-converted constants caches — they're rebuilt lazily on
        // next function call. The heap objects they reference are now marked
        // as reachable so they survive this GC cycle; they'll be freed on the
        // next cycle when they're no longer in the cache.
        self.vm.constants_values_cache.clear();
        self.vm.constants_values_ptr = std::ptr::null();
        self.vm.constants_syms_cache.clear();
        self.vm.constants_syms_ptr = std::ptr::null();
        self.vm.last_preconvert_key = usize::MAX;
        self.vm.last_preconvert_values_ptr = std::ptr::null();
        self.vm.last_preconvert_syms_ptr = std::ptr::null();

        // Phase 2: Sweep — null out unreachable heap slots, add to free list.
        // Clear existing free list since we're rebuilding it from scratch.
        self.vm.heap.clear_free_list();
        let mut freed = 0usize;
        for (i, &is_reachable) in reachable.iter().enumerate().take(heap_len) {
            if !is_reachable {
                let obj = &self.vm.heap.objects[i];
                // Don't null out cheap inline values (they cost nothing to keep)
                if matches!(obj, Object::Null | Object::Undefined
                    | Object::Integer(_) | Object::Float(_) | Object::Boolean(_))
                {
                    continue;
                }
                // Remove Rc pointer from index before nulling
                self.vm.heap.unregister_rc(i as u32);
                self.vm.heap.objects[i] = Object::Null;
                self.vm.heap.add_free(i as u32);
                freed += 1;
            }
        }

        // Phase 3: Truncate trailing nulls to reclaim Vec capacity from the end.
        while self.vm.heap.objects.last().is_some_and(|o| matches!(o, Object::Null)) {
            self.vm.heap.objects.pop();
        }
        // Remove any free-list entries that are now beyond the truncated length
        let new_len = self.vm.heap.objects.len();
        self.vm.heap.trim_free_list(new_len);

        if freed > 0 {
            // Shrink the backing Vec if we freed a lot (> 50% unreachable)
            if new_len < heap_len / 2 {
                self.vm.heap.objects.shrink_to(new_len + 256);
            }
        }
    }

    /// Drain all pending host calls queued by `host.call()` builtins.
    /// Returns the calls and clears the queue.
    pub fn drain_pending_host_calls(&mut self) -> Vec<crate::host_bridge::PendingHostCall> {
        std::mem::take(&mut self.vm.pending_host_calls)
    }

    /// Resolve a pending host callback: looks up the stored callback Value
    /// by call ID and invokes it with the given result object.
    pub fn resolve_host_callback(
        &mut self,
        id: u32,
        result: Object,
    ) -> Result<Object, ZippError> {
        let callback = self.vm.host_callbacks.remove(&id).ok_or_else(|| {
            ZippError::Runtime(format!("no pending callback for host call id {}", id))
        })?;
        self.call_value(callback, &[result])
    }
}

/// Recursively mark heap slots reachable from an Object's nested contents.
///
/// This must comprehensively handle ALL Object variants that can contain
/// Values (NaN-boxed heap references) or nested Objects (which may themselves
/// contain Values or share Rc identity with heap entries).
fn mark_object_refs(obj: &Object, reachable: &mut [bool], heap: &Heap) {
    match obj {
        Object::Hash(hash_rc) => {
            let h = hash_rc.borrow();
            for val in h.pairs.values() {
                mark_value(val, reachable, heap);
            }
            for val in &h.values {
                mark_value(val, reachable, heap);
            }
            // local_objects: backing store for heap-type Values created outside VM heap
            for obj in &h.local_objects {
                mark_nested_object(obj, reachable, heap);
            }
            // Getter/setter accessor functions may reference heap via constants
            if let Some(getters) = &h.getters {
                for func in getters.values() {
                    mark_compiled_fn_refs(func, reachable, heap);
                }
            }
            if let Some(setters) = &h.setters {
                for func in setters.values() {
                    mark_compiled_fn_refs(func, reachable, heap);
                }
            }
        }
        Object::Array(arr_rc) => {
            let arr = arr_rc.borrow();
            for val in arr.iter() {
                mark_value(val, reachable, heap);
            }
        }
        Object::CompiledFunction(func) => {
            mark_compiled_fn_refs(func, reachable, heap);
        }
        Object::Map(map) => {
            let entries = map.entries.borrow();
            for (_, val) in entries.iter() {
                mark_value(val, reachable, heap);
            }
        }
        Object::Generator(gen_rc) => {
            let gen = gen_rc.borrow();
            for obj in &gen.locals {
                mark_nested_object(obj, reachable, heap);
            }
            for val in &gen.args {
                mark_value(val, reachable, heap);
            }
            if let Some(recv) = &gen.receiver {
                mark_value(recv, reachable, heap);
            }
            mark_compiled_fn_refs(&gen.function, reachable, heap);
        }
        Object::Class(cls) => {
            if let Some(ctor) = &cls.constructor {
                mark_compiled_fn_refs(ctor, reachable, heap);
            }
            for func in cls.methods.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in cls.static_methods.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in cls.getters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in cls.setters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in cls.super_methods.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in cls.super_getters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in cls.super_setters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for (_, func) in &cls.field_initializers {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for init in &cls.static_initializers {
                match init {
                    crate::object::StaticInitializer::Field { thunk, .. }
                    | crate::object::StaticInitializer::Block { thunk } => {
                        mark_compiled_fn_refs(thunk, reachable, heap);
                    }
                }
            }
            for obj in cls.static_fields.values() {
                mark_nested_object(obj, reachable, heap);
            }
        }
        Object::Instance(inst) => {
            for val in inst.fields.values() {
                mark_value(val, reachable, heap);
            }
            for func in inst.methods.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in inst.getters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in inst.setters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in inst.super_methods.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in inst.super_getters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in inst.super_setters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
        }
        Object::BoundMethod(bm) => {
            mark_compiled_fn_refs(&bm.function, reachable, heap);
            mark_nested_object(&bm.receiver, reachable, heap);
        }
        Object::SuperRef(sr) => {
            mark_nested_object(&sr.receiver, reachable, heap);
            for func in sr.methods.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in sr.getters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
            for func in sr.setters.values() {
                mark_compiled_fn_refs(func, reachable, heap);
            }
        }
        Object::BuiltinFunction(bf) => {
            if let Some(recv) = &bf.receiver {
                mark_nested_object(recv, reachable, heap);
            }
        }
        Object::ReturnValue(inner) => {
            mark_nested_object(inner, reachable, heap);
        }
        Object::Promise(p) => {
            let borrowed = p.borrow();
            match &borrowed.settled {
                crate::object::PromiseState::Fulfilled(inner)
                | crate::object::PromiseState::Rejected(inner) => {
                    mark_nested_object(inner, reachable, heap);
                }
                crate::object::PromiseState::Pending => {}
            }
            // Chained promises + queued handlers also keep objects alive.
            for v in borrowed
                .then_chain
                .iter()
                .chain(borrowed.catch_chain.iter())
                .chain(borrowed.chained.iter())
            {
                mark_value(v, reachable, heap);
            }
        }
        // Integer, Float, Boolean, Null, Undefined, String, RegExp, Set,
        // Error — no heap references.
        _ => {}
    }
}

/// Mark a NaN-boxed Value if it references a heap slot.
fn mark_value(val: &Value, reachable: &mut [bool], heap: &Heap) {
    if val.is_heap() {
        let idx = val.heap_index() as usize;
        if idx < reachable.len() && !reachable[idx] {
            reachable[idx] = true;
            mark_object_refs(&heap.objects[idx], reachable, heap);
        }
    }
}

/// Mark heap references from a CompiledFunctionObject's constant pool.
/// Constants may share Rc identity with heap entries (Hash, Array, Generator).
fn mark_compiled_fn_refs(
    func: &crate::object::CompiledFunctionObject,
    reachable: &mut [bool],
    heap: &Heap,
) {
    for c in func.constants.iter() {
        mark_nested_object(c, reachable, heap);
    }
}

/// Mark an Object that is NOT a direct heap entry (e.g., a field value,
/// local_objects entry, or constant pool entry). If it's an Rc-based type
/// (Hash, Array, Generator), find and mark the matching heap slot. Then
/// recursively scan its contents for further heap references.
///
/// Uses Heap::rc_index for O(1) pointer-to-index lookup instead of a linear
/// heap scan, eliminating the O(N²) GC bottleneck for large heaps.
fn mark_nested_object(obj: &Object, reachable: &mut [bool], heap: &Heap) {
    let ptr = match obj {
        Object::Hash(rc) => Rc::as_ptr(rc) as usize,
        Object::Array(rc) => Rc::as_ptr(rc) as usize,
        Object::Generator(rc) => Rc::as_ptr(rc) as usize,
        _ => {
            // Non-Rc type: just scan contents for heap refs
            mark_object_refs(obj, reachable, heap);
            return;
        }
    };

    if let Some(idx) = heap.rc_lookup(ptr) {
        let i = idx as usize;
        if i < reachable.len() && !reachable[i] {
            reachable[i] = true;
            mark_object_refs(&heap.objects[i], reachable, heap);
        }
    } else {
        // Not on heap but its Values may still reference heap objects
        mark_object_refs(obj, reachable, heap);
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::ZippEngine;
    use crate::object::Object;

    #[test]
    fn test_null_not_equals_and_comma_operator() {
        let engine = ZippEngine::default();
        // React pattern: null !== (e = fn()) && (nc(e,...), Bo(e,...))
        let code = r#"
var __log = [];
function getRoot() { return {tag: 3}; }
function nc(root) { __log.push("nc:" + root.tag); }
function Bo(root) { __log.push("Bo:" + root.tag); }
var e = null;
null !== (e = getRoot()) && (nc(e), Bo(e));
__log.length;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 2, "both nc and Bo should be called, got {}", v),
            other => panic!("expected 2, got {:?}", other),
        }
    }

    #[test]
    fn test_side_effect_via_function_parameter() {
        let engine = ZippEngine::default();
        let code = r#"
var root = {pendingLanes: 0, current: {}};
function yt(e, t, n) { e.pendingLanes |= t; }
yt(root, 16, 0);
root.pendingLanes;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 16, "basic: got {}", v),
            other => panic!("expected 16, got {:?}", other),
        }
    }

    /// Test: var in arrow function should NOT be visible to sibling code
    #[test]
    fn test_var_scoping_in_arrow_callback() {
        let engine = ZippEngine::default();
        // This simulates the webpack pattern where module 730 (arrow function)
        // declares var Xe, and the entry code also declares var Xe.
        // Hc (inside module 730) should see module 730's Xe, not entry code's.
        let code = r#"
var __result = "FAIL";
(function() {
    var e = {
        730: (e, t, n) => {
            var Xe = "scheduler_time";
            function Hc() { return Xe; }
            t.Hc = Hc;
        }
    };
    var t = {};
    function n(r) {
        if (t[r]) return t[r].exports;
        var o = t[r] = {exports: {}};
        e[r].call(o.exports, o, o.exports, n);
        return o.exports;
    }
    var mod730 = n(730);
    // Entry code uses same var name
    var Xe = [{name: "ZIPP.org"}];
    // Hc should still see "scheduler_time", not the array
    var result = mod730.Hc();
    if (result === "scheduler_time") {
        __result = "OK";
    } else {
        __result = "FAIL: Hc returned " + typeof result + " = " + String(result).substring(0,30);
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "var scoping: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test with actual React bundle scheduler extracted.
    ///
    /// Marked `#[ignore]` because it depends on a hard-coded fixture
    /// at `$FORMLOGIC_REACT_BUNDLE` (or `C:\tmp\react_bundle.js` on
    /// Windows when the env var is unset). The previous version
    /// silently `return`-ed when the file was missing, which made the
    /// test look like green coverage on every CI run while never
    /// actually exercising anything. Run explicitly via:
    /// `cargo test test_real_scheduler_module -- --ignored`.
    #[test]
    #[ignore = "requires react_bundle.js fixture; set $FORMLOGIC_REACT_BUNDLE or run explicitly"]
    fn test_real_scheduler_module() {
        let engine = ZippEngine::default();
        let path = std::env::var("FORMLOGIC_REACT_BUNDLE")
            .unwrap_or_else(|_| r"C:\tmp\react_bundle.js".to_string());
        let bundle = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!(
                "react_bundle.js fixture missing at {}: {}. Set FORMLOGIC_REACT_BUNDLE \
                 to the path of a bundled React build to run this test.",
                path, e,
            ));
        if bundle.is_empty() {
            panic!("react_bundle.js at {} is empty", path);
        }
        // Extract module 234
        let mod234_start = bundle.find("234:(e,t)=>{").unwrap();
        // Find end by counting braces
        let mod234_src = &bundle[mod234_start..];
        let mut depth = 0;
        let mut end = 0;
        for (i, c) in mod234_src.chars().enumerate() {
            if c == '{' { depth += 1; }
            if c == '}' { depth -= 1; if depth == 0 { end = i + 1; break; } }
        }
        let mod234_str = mod234_src[..end].to_string();
        // Inject debug exports into module 234
        let mod234_patched = mod234_str.replace(
            "t.unstable_IdlePriority=5",
            "t.getN=function(){return N};t.getK=function(){return k};t.getM=function(){return m};t.unstable_IdlePriority=5"
        );
        let mod234_full = mod234_patched.as_str();
        let code = [
            "var __result = 'FAIL';",
            "var MessageChannel = undefined;",
            "var __stQueue = [];",
            "var setTimeout = function(fn, ms) { if(typeof fn==='function') __stQueue.push(fn); return 1; };",
            "var clearTimeout = function() {};",
            "var setImmediate = undefined;",
            "var navigator = {};",
            "var performance = { now: function() { return 0.01; } };",
            // Direct module execution without require wrapper
            "var __sched_exports = {};",
            "(", mod234_full.trim_start_matches("234:"), ")({}, __sched_exports);",
            "__sched_exports.unstable_scheduleCallback(3, function(){ __result = 'CB_OK'; return null; });",
            "var __K = __sched_exports.getK();",
            "__K(true, 0);",
            "__result;",
        ].join("\n");
        // First try: run and check result
        let result = engine.eval(&code);
        match &result {
            Ok(obj) => eprintln!("Result: {:?}", obj),
            Err(e) => eprintln!("Error: {}", e),
        }
        match result {
            Ok(Object::String(s)) => {
                let sv = s.to_string();
                assert!(sv == "CB_OK" || sv.starts_with("CB"),
                    "real scheduler: expected CB_OK, got {}", sv);
            }
            Ok(other) => panic!("unexpected: {:?}", other),
            Err(e) => panic!("eval error: {}", e),
        }
    }

    /// Test the FULL scheduler module pattern with task queue processing
    #[test]
    fn test_scheduler_full_task_queue() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var modules = {
        730: (e, t, n) => {
            "use strict";
            // Simulate react-dom with many function declarations including k
            function k(e) { return "react-dom-k"; }
            function D(e) { return "react-dom-D"; }
            function N(e) { return "react-dom-N"; }
            function T(e) { return "react-dom-T"; }
            var scheduler = n(234);
            t.createRoot = function(container) {
                scheduler.scheduleCallback(3, function(didTimeout) {
                    __result = "CALLBACK EXECUTED";
                    return null;
                });
            };
            t.getScheduler = function() { return scheduler; };
        },
        234: (e, t) => {
            "use strict";
            var c=[],u=[],d=1,f=null,p=3,h=false,m=false,g=false;
            var N=null,j=false;
            function r(e){return e.length>0?e[0]:null;}
            function x(e){for(var n=r(u);null!==n;){if(null===n.callback){u.shift();}else{if(!(n.startTime<=e))return;u.shift();n.sortIndex=n.expirationTime;c.push(n);}n=r(u);}}
            function k(e,n){m=false;h=true;var o2=p;try{for(x(n),f=r(c);null!==f;){var i=f.callback;if("function"===typeof i){f.callback=null;p=f.priorityLevel;i(f.expirationTime<=n);n=0;x(n);}else{c.shift();}f=r(c);}return null!==f;}finally{f=null;p=o2;h=false;}}
            var j2=false;
            function D(e){N=e,j2||(j2=true)}
            t.scheduleCallback=function(e2,a2){var l2=5000;var s2={id:d++,callback:a2,priorityLevel:e2,startTime:0,expirationTime:l2,sortIndex:l2};c.push(s2);if(!m&&!h){m=true;D(k);}return s2;};
            t.getN=function(){return N;};
        }
    };
    var cache = {};
    function require(id) {
        if (cache[id]) return cache[id].exports;
        var mod = cache[id] = {exports: {}};
        modules[id].call(mod.exports, mod, mod.exports, require);
        return mod.exports;
    }
    var reactDom = require(730);
    reactDom.createRoot({});
    var sched = reactDom.getScheduler();
    var N = sched.getN();
    if (typeof N === "function") {
        N(true, 0);
    } else {
        __result = "FAIL: N=" + typeof N;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => {
                assert!(s.to_string().starts_with("CALLBACK EXECUTED"),
                    "scheduler full: {}", s);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test scheduler pattern: hoisted function k() accessed from exported callback
    #[test]
    fn test_scheduler_k_hoisted_function_in_callback() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var e = {
        234: (e, t) => {
            var c=[],u=[],d=1,f=null,p=3,h=false,m=false,g=false;
            var N = null, j = false, E = -1;
            function a(e) { return e; }
            function r(e) { return e[0]; }
            function n(e,t) { e.push(t); }
            function x(e) { return; }
            function k(e, n) { return "flushWork"; }
            function D(e) { N = e; j || (j = true); }
            function S() { /* postMessage */ }
            function T() { /* performWorkUntilDeadline */ }
            t.scheduleCallback = function(priority, cb) {
                m = true;
                D(k);
            };
            t.getN = function() { return N; };
        }
    };
    var t = {};
    function n(r) {
        if (t[r]) return t[r].exports;
        var o = t[r] = {exports: {}};
        e[r].call(o.exports, o, o.exports, n);
        return o.exports;
    }
    var scheduler = n(234);
    scheduler.scheduleCallback(3, function(){});
    var N_val = scheduler.getN();
    if (typeof N_val === "function") {
        __result = "OK: " + N_val();
    } else {
        __result = "FAIL: N=" + typeof N_val;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => {
                assert!(s.to_string().starts_with("OK"), "scheduler k: {}", s);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Minimal reproduction: nested fast-path calls with task queue
    #[test]
    fn test_nested_fast_path_callback() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
function processQueue(tasks) {
    var h = true;
    try {
        for (var i = 0; i < tasks.length; i++) {
            var cb = tasks[i].callback;
            if (typeof cb === "function") {
                tasks[i].callback = null;
                var ret = cb(true);
            }
        }
        return tasks.length > 0;
    } finally {
        h = false;
    }
}
var queue = [{callback: function(x) { __result = "CB x=" + x; return null; }, id: 1}];
var result = processQueue(queue);
if (__result === "FAIL") __result = "processQueue returned " + result + " but cb not called";
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert!(s.starts_with("CB"), "nested fast: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: scheduler task processing with bound callback
    #[test]
    fn test_scheduler_task_callback_bound() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
function doWork(root, didTimeout) {
    __result = "WORK: root=" + typeof root + " dt=" + didTimeout;
    return null;
}
var task = {
    id: 1,
    callback: doWork.bind(null, {tag:3}),
    priorityLevel: 3,
    startTime: 0,
    expirationTime: 5000,
    sortIndex: 5000
};
var i = task.callback;
if ("function" === typeof i) {
    task.callback = null;
    var l = i(true);
} else {
    __result = "typeof i = " + typeof i;
}
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => {
                assert!(s.to_string().starts_with("WORK:"), "task cb: {}", s);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: typeof BoundMethod stored in hash should return "function"
    #[test]
    fn test_typeof_bound_method_in_hash() {
        let engine = ZippEngine::default();
        let code = r#"
function add(a, b) { return a + b; }
var bound = add.bind(null, 5);
var obj = {callback: bound};
var result = typeof obj.callback;
result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "function", "typeof bound in hash: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test Function.prototype.bind with partial application
    #[test]
    fn test_bind_partial_application() {
        let engine = ZippEngine::default();
        let code = r#"
function add(a, b) { return a + b; }
var add5 = add.bind(null, 5);
add5(3);
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 8, "5+3=8"),
            Object::Float(v) => assert_eq!(v as i64, 8),
            other => panic!("expected 8, got {:?}", other),
        }
    }

    /// Test webpack require pattern: module sets exports via .call()
    #[test]
    fn test_webpack_require_exports_persist() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var e = {
        234: (e, t) => {
            t.unstable_now = function() { return 42; };
        }
    };
    var t = {};
    function n(r) {
        if (t[r]) return t[r].exports;
        var o = t[r] = {exports: {}};
        e[r].call(o.exports, o, o.exports, n);
        return o.exports;
    }
    var scheduler = n(234);
    if (scheduler.unstable_now && scheduler.unstable_now() === 42) {
        __result = "OK";
    } else {
        __result = "FAIL: unstable_now=" + typeof scheduler.unstable_now;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "webpack exports: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Stress test: many function declarations in one scope (like webpack IIFE)
    #[test]
    fn test_many_hoisted_functions_call_each_other() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    // Simulate a large webpack module with many function declarations
    var a1=0,a2=0,a3=0,a4=0,a5=0,a6=0,a7=0,a8=0,a9=0,a10=0;
    var b1=0,b2=0,b3=0,b4=0,b5=0,b6=0,b7=0,b8=0,b9=0,b10=0;
    function f1() { return 1; }
    function f2() { return 2; }
    function f3(x) { return x + 3; }
    function f4(x, y) { return x + y; }
    function f5(e) { e.val |= 16; }
    function f6(e, t, n, r) {
        // Like nc: guard check then call f5
        if (50 < a1) throw "guard";
        f5(e, n, r);
    }
    var obj = {val: 0};
    f6(obj, {}, 16, 0);
    if (obj.val === 16) {
        __result = "OK";
    } else {
        __result = "FAIL: obj.val=" + obj.val;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "many functions: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test the EXACT React nc pattern: yt(e,n,r) in a comma expression
    #[test]
    fn test_call_in_comma_expression_after_if() {
        let engine = ZippEngine::default();
        // This reproduces the exact minified React pattern:
        // function nc(e,t,n,r){if(50<Gs)throw...;yt(e,n,r),otherExpr}
        let code = r#"
var __ytCalled = false;
var _s = 0;
(function() {
    var Gs = 0;
    var Cs = null;
    function yt(e, t, n) { __ytCalled = true; e.pendingLanes |= t; }
    function nc(e, t, n, r) {
        if (50 < Gs) throw "too many";
        yt(e, n, r), 0 !== (2 & _s) && e === Cs || (e === Cs && (0 === (2 & _s)), 1 === n && 0 === _s);
    }
    var root = {pendingLanes: 0};
    nc(root, {}, 16, 0);
    if (!__ytCalled) throw "yt was not called!";
})();
__ytCalled ? "OK" : "FAIL";
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "comma expr: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: function call where arguments might clobber callee register
    #[test]
    fn test_call_with_many_params_after_guard() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var Gs = 0;
    function yt(e, t, n) { __result = "yt called: t=" + t; }
    function nc(e, t, n, r) {
        if (50 < Gs) throw "too many";
        yt(e, n, r);
    }
    var root = {pendingLanes: 0};
    nc(root, {}, 16, 0);
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "yt called: t=16", "got: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Simulate React's render → updateContainer → markRootUpdated pattern
    #[test]
    fn test_closure_side_effect_react_pattern() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    function yt(e, t, n) { e.pendingLanes |= t; }
    function nc(e, t, n, r) { yt(e, n, r); }
    function Hc(e, t, n, r) {
        var a = t.current;
        var i = 16;
        nc(t, a, i, 0);
    }
    var fiberRoot = {pendingLanes: 0, current: {stateNode: null}, tag: 3};
    fiberRoot.current.stateNode = fiberRoot;
    Hc({}, fiberRoot, null, null);
    if (fiberRoot.pendingLanes === 16) {
        __result = "OK";
    } else {
        __result = "FAIL: pendingLanes=" + fiberRoot.pendingLanes;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "closure pattern: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_bitwise_or_assign_on_property() {
        let engine = ZippEngine::default();
        // Test with initialized property
        let code = "var o = {lanes: 0}; o.lanes |= 16; o.lanes;";
        let out = engine.eval(code).expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 16),
            Object::Float(v) => assert_eq!(v as i64, 16),
            other => panic!("expected 16, got {:?}", other),
        }
    }

    #[test]
    fn test_bitwise_or_assign_on_undefined_property() {
        let engine = ZippEngine::default();
        // Test with UNINITIALIZED property (like React's pendingLanes before markRootUpdated)
        let code = "var o = {}; o.lanes |= 16; o.lanes;";
        let out = engine.eval(code).expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 16, "undefined |= 16 should produce 16"),
            Object::Float(v) => assert_eq!(v as i64, 16),
            other => panic!("expected 16, got {:?}", other),
        }
    }

    #[test]
    fn test_typeof_undeclared_returns_undefined() {
        let engine = ZippEngine::default();
        // typeof undeclaredVariable should return "undefined" without throwing
        let code = r#"
var result = "FAIL";
(function() {
    var r = "function" == typeof Symbol && Symbol;
    if (r) {
        result = "HAS_SYMBOL";
    } else {
        result = "NO_SYMBOL";
    }
})();
result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "NO_SYMBOL", "typeof undeclared: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn evaluates_basic_arithmetic() {
        let engine = ZippEngine::default();
        let out = engine.eval("1 + 2;").expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 3),
            Object::Float(v) => assert!((v - 3.0).abs() < 1e-9),
            _ => panic!("expected numeric output"),
        }
    }

    #[test]
    fn evaluates_let_binding_and_reference() {
        let engine = ZippEngine::default();
        let out = engine.eval("let x = 7; x + 5;").expect("eval");
        match out {
            Object::Integer(v) => assert_eq!(v, 12),
            Object::Float(v) => assert!((v - 12.0).abs() < 1e-9),
            _ => panic!("expected numeric output"),
        }
    }

    fn assert_int(obj: Object, expected: i64) {
        match obj {
            Object::Integer(v) => assert_eq!(v, expected),
            Object::Float(v) => assert_eq!(v as i64, expected),
            other => panic!("expected Integer({}), got {:?}", expected, other),
        }
    }

    #[test]
    fn test_init_script_and_call() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script("let count = 0; function inc() { count = count + 1; }")
            .unwrap();
        assert_int(state.get_global("count").unwrap(), 0);
        state.call_function("inc", &[]).unwrap();
        assert_int(state.get_global("count").unwrap(), 1);
        state.call_function("inc", &[]).unwrap();
        assert_int(state.get_global("count").unwrap(), 2);
    }

    #[test]
    fn test_init_script_function_with_args() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script("let total = 0; function add(x) { total = total + x; }")
            .unwrap();
        state.call_function("add", &[Object::Integer(5)]).unwrap();
        assert_int(state.get_global("total").unwrap(), 5);
        state.call_function("add", &[Object::Integer(3)]).unwrap();
        assert_int(state.get_global("total").unwrap(), 8);
    }

    #[test]
    fn test_init_script_set_global() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script("let count = 0; function read() { return count; }")
            .unwrap();
        state.set_global("count", Object::Integer(42)).unwrap();
        assert_int(state.get_global("count").unwrap(), 42);
    }

    #[test]
    fn test_init_script_function_return_value() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script("let x = 10; function double() { return x * 2; }")
            .unwrap();
        let result = state.call_function("double", &[]).unwrap();
        assert_int(result, 20);
    }

    #[test]
    fn test_init_script_undefined_var_error() {
        let engine = ZippEngine::default();
        let state = engine.init_script("let x = 1;").unwrap();
        assert!(state.get_global("nonexistent").is_err());
    }

    /// Regression test: self-recursive function with a for-loop should
    /// correctly process all loop iterations at every recursion depth.
    /// The bug was that the self-recursion fast path in call_register_direct
    /// corrupted for-loop state, causing inner loops to terminate early.
    #[test]
    fn test_self_recursion_for_loop() {
        let engine = ZippEngine::default();
        // recurse(depth): at depth 0, returns 1. At depth > 0, iterates
        // over a 3-element array, recursively calling recurse(depth-1)
        // for each element, summing the results.
        // Expected: recurse(0) = 1
        //           recurse(1) = 3  (3 elements × 1)
        //           recurse(2) = 9  (3 elements × 3)
        //           recurse(3) = 27 (3 elements × 9)
        let out = engine
            .eval(
                r#"
                function recurse(depth) {
                    if (depth <= 0) return 1;
                    let items = [10, 20, 30];
                    let sum = 0;
                    for (let i = 0; i < items.length; i++) {
                        sum = sum + recurse(depth - 1);
                    }
                    return sum;
                }
                recurse(3);
                "#,
            )
            .expect("eval");
        assert_int(out, 27);
    }

    /// Variant using for-of loop (another common pattern).
    #[test]
    fn test_self_recursion_for_of_loop() {
        let engine = ZippEngine::default();
        let out = engine
            .eval(
                r#"
                function recurse(depth) {
                    if (depth <= 0) return 1;
                    let items = ["a", "b", "c"];
                    let sum = 0;
                    for (let item of items) {
                        sum = sum + recurse(depth - 1);
                    }
                    return sum;
                }
                recurse(3);
                "#,
            )
            .expect("eval");
        assert_int(out, 27);
    }

    /// Test with depth=1 to isolate minimal recursion case.
    #[test]
    fn test_self_recursion_depth_1() {
        let engine = ZippEngine::default();
        let out = engine
            .eval(
                r#"
                function recurse(depth) {
                    if (depth <= 0) return 1;
                    let items = [10, 20, 30];
                    let sum = 0;
                    for (let i = 0; i < items.length; i++) {
                        sum = sum + recurse(depth - 1);
                    }
                    return sum;
                }
                recurse(1);
                "#,
            )
            .expect("eval");
        assert_int(out, 3);
    }

    /// Test with while loop to see if it's for-specific.
    #[test]
    fn test_self_recursion_while_loop() {
        let engine = ZippEngine::default();
        let out = engine
            .eval(
                r#"
                function recurse(depth) {
                    if (depth <= 0) return 1;
                    let items = [10, 20, 30];
                    let sum = 0;
                    let i = 0;
                    while (i < items.length) {
                        sum = sum + recurse(depth - 1);
                        i = i + 1;
                    }
                    return sum;
                }
                recurse(3);
                "#,
            )
            .expect("eval");
        assert_int(out, 27);
    }

    /// Test with depth=2 to see what second level returns.
    #[test]
    fn test_self_recursion_depth_2() {
        let engine = ZippEngine::default();
        let out = engine
            .eval(
                r#"
                function recurse(depth) {
                    if (depth <= 0) return 1;
                    let sum = 0;
                    for (let i = 0; i < 3; i++) {
                        sum = sum + recurse(depth - 1);
                    }
                    return sum;
                }
                recurse(2);
                "#,
            )
            .expect("eval");
        assert_int(out, 9);
    }

    /// Test: is the issue the length property access or the numeric constant?
    #[test]
    fn test_self_recursion_hardcoded_bound() {
        let engine = ZippEngine::default();
        let out = engine
            .eval(
                r#"
                function recurse(depth) {
                    if (depth <= 0) return 1;
                    let sum = 0;
                    for (let i = 0; i < 3; i++) {
                        sum = sum + recurse(depth - 1);
                    }
                    return sum;
                }
                recurse(3);
                "#,
            )
            .expect("eval");
        assert_int(out, 27);
    }

    /// Test: init_script + call_function where A calls B, B defined after A.
    /// This reproduces the _render / _dispatchEvents pattern.
    #[test]
    fn test_init_script_forward_call() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script(
                r#"
                function A() {
                    return B();
                }
                function B() {
                    return 42;
                }
                "#,
            )
            .expect("init_script");
        let result = state.call_function("A", &[]).expect("call A");
        assert_int(result, 42);
    }

    /// Test: init_script + call_function where A calls B and reads a top-level let.
    #[test]
    fn test_init_script_forward_call_with_let() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script(
                r#"
                let counter = 10;
                function A() {
                    return B() + counter;
                }
                function B() {
                    return 5;
                }
                "#,
            )
            .expect("init_script");
        let result = state.call_function("A", &[]).expect("call A");
        assert_int(result, 15);
    }

    /// Test: init_script + call_function with multiple forward references
    /// (mimics runtime.logic with _render calling _dispatchEvents, buildLayout, etc.)
    #[test]
    fn test_init_script_multiple_forward_refs() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script(
                r#"
                let state = 0;
                function render() {
                    dispatchEvents();
                    let result = buildLayout();
                    return result + state;
                }
                function setBuilderFn(fn_ref) {
                    state = 100;
                }
                function dispatchEvents() {
                    state = state + 1;
                }
                function buildLayout() {
                    return 7;
                }
                setBuilderFn(null);
                "#,
            )
            .expect("init_script");
        let result = state.call_function("render", &[]).expect("call render");
        // dispatchEvents increments state from 100 to 101, buildLayout returns 7
        // result = 7 + 101 = 108
        assert_int(result, 108);
    }

    /// Test: mimics _render() calling _builderFn() which is a callback stored in a let.
    /// This is the actual pattern in runtime.logic.
    #[test]
    fn test_init_script_callback_in_let() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script(
                r#"
                let _builderFn = null;
                let _rootNode = null;
                
                function _render() {
                    if (_builderFn) {
                        _rootNode = _builderFn();
                    }
                    if (!_rootNode) return 0;
                    return _rootNode;
                }
                
                function setBuilderFn(fn) {
                    _builderFn = fn;
                }
                
                function buildApp() {
                    return 42;
                }
                
                setBuilderFn(buildApp);
                "#,
            )
            .expect("init_script");
        let result = state.call_function("_render", &[]).expect("call _render");
        assert_int(result, 42);
    }

    /// Test: mimics the pattern where imports inject code between function definitions.
    /// In runtime.logic, _render is defined before the imports, and _dispatchEvents
    /// is defined after the imports. The imports define lots of functions/variables.
    #[test]
    fn test_init_script_code_between_functions() {
        let engine = ZippEngine::default();
        let mut state = engine
            .init_script(
                r#"
                let _needsRebuild = true;
                let _rootNode = null;
                let _builderFn = null;
                
                function buildLayout(node) {
                    return 1;
                }
                
                function computeLayout(node, w, h) {
                    return 2;
                }
                
                function renderTree(node) {
                    return 3;
                }
                
                function _render() {
                    if (_builderFn) {
                        _rootNode = _builderFn();
                        _needsRebuild = true;
                    }
                    if (!_rootNode) return 0;
                    _dispatchEvents();
                    if (_needsRebuild) {
                        buildLayout(_rootNode);
                        _needsRebuild = false;
                    }
                    computeLayout(_rootNode, 100, 100);
                    renderTree(_rootNode);
                    return 99;
                }
                
                // Simulated imported code (goes between _render and _dispatchEvents)
                let importedVar1 = "hello";
                let importedVar2 = "world";
                function importedFn1() { return 10; }
                function importedFn2() { return 20; }
                let importedVar3 = importedFn1() + importedFn2();
                
                function _dispatchEvents() {
                    // does nothing for this test
                }
                
                function buildApp() {
                    return { type: "Box" };
                }
                
                function setBuilderFn(fn) {
                    _builderFn = fn;
                }
                
                setBuilderFn(buildApp);
                "#,
            )
            .expect("init_script");
        let result = state.call_function("_render", &[]).expect("call _render");
        assert_int(result, 99);
    }

    /// Integration test: compile and run the actual counter.logic resolved source.
    /// This tests the real-world scenario with ~4800 lines of code.
    #[test]
    fn test_counter_resolved_source() {
        use crate::draw_bridge::DrawBridge;
        use crate::input_bridge::InputBridge;
        use crate::layout_bridge::{LayoutBridge, LayoutStyle};

        // Mock draw bridge that returns non-zero viewport dimensions
        struct MockDraw;
        impl DrawBridge for MockDraw {
            fn draw_rect(
                &mut self,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: &str,
                _: f64,
                _: f64,
                _: &str,
                _: f64,
            ) {
            }
            fn draw_rounded_rect(
                &mut self,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: [f64; 4],
                _: &str,
                _: f64,
            ) {
            }
            fn draw_circle(&mut self, _: f64, _: f64, _: f64, _: &str, _: f64) {}
            fn draw_ellipse(&mut self, _: f64, _: f64, _: f64, _: f64, _: &str, _: f64) {}
            fn draw_line(&mut self, _: f64, _: f64, _: f64, _: f64, _: &str, _: f64) {}
            fn draw_path(&mut self, _: &str, _: &str, _: &str, _: f64, _: f64) {}
            fn draw_text(
                &mut self,
                _: &str,
                _: f64,
                _: f64,
                _: f64,
                _: &str,
                _: u32,
                _: &str,
                _: f64,
                _: f64,
            ) -> (f64, f64) {
                (0.0, 0.0)
            }
            fn draw_image(&mut self, _: &str, _: f64, _: f64, _: f64, _: f64, _: f64) {}
            fn draw_linear_gradient(
                &mut self,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: &[f64],
                _: f64,
            ) {
            }
            fn draw_radial_gradient(&mut self, _: f64, _: f64, _: f64, _: f64, _: &[f64], _: f64) {}
            fn draw_shadow(
                &mut self,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: f64,
                _: &str,
                _: f64,
                _: f64,
                _: f64,
            ) {
            }
            fn push_clip(&mut self, _: f64, _: f64, _: f64, _: f64, _: f64) {}
            fn pop_clip(&mut self) {}
            fn push_transform(&mut self, _: f64, _: f64, _: f64, _: f64, _: f64) {}
            fn pop_transform(&mut self) {}
            fn push_opacity(&mut self, _: f64) {}
            fn pop_opacity(&mut self) {}
            fn draw_arc(&mut self, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: &str) {}
            fn measure_text(&self, _: &str, _: f64, _: u32, _: &str, _: f64) -> (f64, f64) {
                (0.0, 0.0)
            }
            fn get_viewport_width(&self) -> f64 {
                800.0
            }
            fn get_viewport_height(&self) -> f64 {
                600.0
            }
        }

        // Mock layout bridge
        struct MockLayout;
        impl LayoutBridge for MockLayout {
            fn create_node(&mut self, _: LayoutStyle) -> u64 {
                0
            }
            fn update_style(&mut self, _: u64, _: LayoutStyle) {}
            fn set_children(&mut self, _: u64, _: &[u64]) {}
            fn compute_layout(&mut self, _: u64, _: f64, _: f64) {}
            fn get_layout(&self, _: u64) -> (f64, f64, f64, f64) {
                (0.0, 0.0, 100.0, 50.0)
            }
            fn remove_node(&mut self, _: u64) {}
            fn clear(&mut self) {}
        }

        // Mock input bridge
        struct MockInput;
        impl InputBridge for MockInput {
            fn get_mouse_x(&self) -> f64 {
                0.0
            }
            fn get_mouse_y(&self) -> f64 {
                0.0
            }
            fn is_mouse_down(&self) -> bool {
                false
            }
            fn is_mouse_pressed(&self) -> bool {
                false
            }
            fn is_mouse_released(&self) -> bool {
                false
            }
            fn get_scroll_y(&self) -> f64 {
                0.0
            }
            fn set_cursor(&mut self, _: &str) {}
            fn get_text_input(&self) -> String {
                String::new()
            }
            fn is_backspace_pressed(&self) -> bool {
                false
            }
            fn is_escape_pressed(&self) -> bool {
                false
            }
            fn request_redraw(&mut self) {}
            fn get_elapsed_secs(&self) -> f64 {
                0.0
            }
            fn get_page_elapsed_secs(&self) -> f64 {
                0.0
            }
            fn get_delta_time(&self) -> f64 {
                0.016
            }
            fn get_focused_input(&self) -> Option<String> {
                None
            }
            fn set_focused_input(&mut self, _: Option<&str>) {}
            fn is_key_down(&self, _: &str) -> bool {
                false
            }
        }

        let source = include_str!("../tests/counter_resolved.logic");
        let engine = ZippEngine::default();
        let mut state = engine.init_script(source).expect("init_script failed");

        // Attach mock bridges
        state.set_draw(Box::new(MockDraw));
        state.set_layout(Box::new(MockLayout));
        state.set_input(Box::new(MockInput));

        // Now call _render — this should exercise the full rendering pipeline
        match state.call_function("_render", &[]) {
            Ok(_) => eprintln!("_render succeeded!"),
            Err(e) => {
                // Print some diagnostics
                eprintln!("_render FAILED: {}", e);

                // Dump all global slots that are Undefined
                let mut undefined_globals = vec![];
                for (name, &slot) in &state.globals_table {
                    let val = unsafe { state.vm.globals.get_unchecked(slot as usize) };
                    if val.is_undefined() {
                        undefined_globals.push((name.clone(), slot));
                    }
                }
                undefined_globals.sort_by_key(|&(_, s)| s);
                eprintln!("Global slots that are Undefined:");
                for (name, slot) in &undefined_globals {
                    eprintln!("  slot {}: {}", slot, name);
                }

                panic!("_render failed: {}", e);
            }
        }
    }

    /// Core var shadowing test: a nested function's `var e` must not
    /// overwrite the outer function's `var e` when both are captured.
    #[test]
    fn test_var_shadow_does_not_overwrite_outer_closure() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var e = "original";
    function reader() { return e; }

    function callback() {
        var e = "shadow";
        var getter = function() { return e; };
        return getter();
    }

    var inner = callback();
    var outer = reader();
    if (outer === "original" && inner === "shadow") {
        __result = "OK";
    } else {
        __result = "FAIL: outer=" + outer + " inner=" + inner;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "var shadow: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Full webpack module pattern with require function and var shadowing
    #[test]
    fn test_webpack_module_map_not_corrupted() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var e = {};
    e[43] = function(module, exports, require) {
        var e = {};
        e.__esModule = true;
        var getter = function() { return e.__esModule; };
        exports.check = getter();
    };
    e[730] = function(module, exports, require) {
        var mod43 = require(43);
        exports.result = mod43.check;
    };
    var cache = {};
    function req(id) {
        if (cache[id]) return cache[id].exports;
        var m = cache[id] = { exports: {} };
        e[id].call(m.exports, m, m.exports, req);
        return m.exports;
    }
    var out = req(730);
    if (out.result === true) {
        __result = "OK";
    } else {
        __result = "FAIL: result=" + String(out.result) + " e43type=" + typeof e[43];
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "webpack pattern: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test function property storage (n.d = ..., n.r = ...)
    #[test]
    fn test_function_properties() {
        let engine = ZippEngine::default();
        let code = r#"
function f(x) { return x * 2; }
f.hello = "world";
f.num = 42;
var result = f.hello + ":" + f.num + ":" + f(5);
result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "world:42:10"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test n.d as function property inside IIFE (closer to webpack)
    #[test]
    fn test_nd_as_function_property() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    function n(r) { return r; }
    n.o = function(e, t) {
        return Object.prototype.hasOwnProperty.call(e, t);
    };
    n.d = function(e, t) {
        for (var r in t) {
            if (n.o(t, r) && !n.o(e, r)) {
                Object.defineProperty(e, r, { enumerable: true, get: t[r] });
            }
        }
    };
    n.r = function(e) {
        Object.defineProperty(e, "__esModule", { value: true });
    };
    var obj = {};
    n.r(obj);
    n.d(obj, { greeting: function() { return "hello"; } });
    if (obj.greeting === "hello" && obj.__esModule === true) {
        __result = "OK";
    } else {
        __result = "FAIL: greeting=" + String(obj.greeting) + " esModule=" + String(obj.__esModule);
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "n.d as func prop: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test webpack n.d (define exports) in isolation
    #[test]
    fn test_webpack_nd_function() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
function no(e, t) {
    return Object.prototype.hasOwnProperty.call(e, t);
}
function nd(e, t) {
    for (var r in t) {
        if (no(t, r) && !no(e, r)) {
            Object.defineProperty(e, r, { enumerable: true, get: t[r] });
        }
    }
}
var obj = {};
nd(obj, { greeting: function() { return "hello"; } });
if (obj.greeting === "hello") {
    __result = "OK";
} else {
    __result = "FAIL: obj.greeting=" + String(obj.greeting);
}
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "n.d test: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test Object.defineProperty with getter
    #[test]
    fn test_define_property_getter() {
        let engine = ZippEngine::default();
        let code = r#"
var obj = {};
var val = "hello";
Object.defineProperty(obj, "greeting", { enumerable: true, get: function() { return val; } });
obj.greeting;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "hello"),
            other => panic!("expected String('hello'), got {:?}", other),
        }
    }

    /// Realistic webpack pattern with n.d/n.r runtime extensions, minified names
    #[test]
    fn test_webpack_full_runtime_pattern() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    "use strict";
    var e = {
        43: function(e, t, n) {
            var r = {};
            n.r(r);
            n.d(r, { greeting: function() { return o } });
            var o = "Hello from module 43";
            e.exports = r;
        },
        730: function(e, t, n) {
            var r = n(43);
            e.exports = { msg: r.greeting };
        }
    };
    var t = {};
    function n(r) {
        if (t[r]) return t[r].exports;
        var o = t[r] = { exports: {} };
        e[r].call(o.exports, o, o.exports, n);
        return o.exports;
    }
    n.d = function(e, t) {
        for (var r in t) {
            if (n.o(t, r) && !n.o(e, r)) {
                Object.defineProperty(e, r, { enumerable: true, get: t[r] });
            }
        }
    };
    n.r = function(e) {
        Object.defineProperty(e, "__esModule", { value: true });
    };
    n.o = function(e, t) {
        return Object.prototype.hasOwnProperty.call(e, t);
    };
    var r = n(730);
    if (r.msg === "Hello from module 43") {
        __result = "OK";
    } else {
        __result = "FAIL: msg=" + String(r.msg);
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "webpack full runtime: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Parameter shadowing (minified: function(e, t, n) where outer var is also e)
    #[test]
    fn test_param_shadow_webpack_module_callback() {
        let engine = ZippEngine::default();
        let code = r#"
var __result = "FAIL";
(function() {
    var e = "module_map";
    function reader() { return e; }

    function callback(e) {
        var getter = function() { return e; };
        return getter();
    }

    var inner = callback("param_val");
    var outer = reader();
    if (outer === "module_map" && inner === "param_val") {
        __result = "OK";
    } else {
        __result = "FAIL: outer=" + outer + " inner=" + inner;
    }
})();
__result;
"#;
        let out = engine.eval(code).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "OK", "param shadow: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: code AFTER a fast-path function call must execute.
    /// Verifies that Call+Return properly restores the caller's IP.
    #[test]
    fn test_code_after_fastpath_call() {
        let engine = ZippEngine::default();
        // Basic: function returns, then more code executes
        let out = engine.eval(r#"
var result = "FAIL";
function inner() { return null; }
function outer(a, b) { inner(); return true; }
outer(true, 0);
42;
result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "FAIL", "basic: {}", s),
            other => panic!("expected String('FAIL'), got {:?}", other),
        }
    }

    /// Test: code after a call to a function that calls a callback
    #[test]
    fn test_code_after_callback_call() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var result = "FAIL";
function processQueue(flag, n) {
    var tasks = [function(){ return null; }];
    for (var i = 0; i < tasks.length; i++) {
        tasks[i]();
    }
    return true;
}
processQueue(true, 0);
42;
result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "FAIL", "callback: {}", s),
            other => panic!("expected String('FAIL'), got {:?}", other),
        }
    }

    /// Test: code after a call to a function with try-catch (which our engine skips try bodies)
    #[test]
    fn test_code_after_trycatch_call() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var result = "FAIL";
function flushWork(a, b) {
    var callback = function(){ return null; };
    try {
        callback();
    } finally {
        var x = 1;
    }
    return true;
}
flushWork(true, 0);
42;
result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "FAIL", "trycatch: {}", s),
            other => panic!("expected String('FAIL'), got {:?}", other),
        }
    }

    /// Test: calling a closure obtained from an IIFE (scheduler pattern)
    #[test]
    fn test_iife_closure_call_continuation() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var result = "FAIL";
var exports = {};
(function(e, t) {
    var c = [], f = null, h = false, m = false;
    function k(a, b) {
        h = true;
        var cb = c[0];
        if (typeof cb === "function") {
            cb();
        }
        h = false;
        return true;
    }
    t.schedule = function(cb) {
        c.push(cb);
        m = true;
    };
    t.getK = function() { return k; };
})({}, exports);
exports.schedule(function(){ return null; });
var K = exports.getK();
K(true, 0);
42;
result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "FAIL", "iife closure: {}", s),
            other => panic!("expected String('FAIL'), got {:?}", other),
        }
    }

    /// Test: calling a closure with try-finally obtained from an IIFE
    #[test]
    fn test_iife_closure_try_finally_call() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var result = "FAIL";
var exports = {};
(function(e, t) {
    var c = [], f = null, p = 3, h = false, m = false;
    function k(a, b) {
        h = true;
        var o2 = p;
        try {
            f = c[0];
            if (f && typeof f.callback === "function") {
                f.callback();
            }
        } finally {
            f = null;
            p = o2;
            h = false;
        }
        return true;
    }
    t.schedule = function(cb) {
        c.push({callback: cb, priorityLevel: 3});
        m = true;
    };
    t.getK = function() { return k; };
})({}, exports);
exports.schedule(function(){ return null; });
var K = exports.getK();
K(true, 0);
42;
result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "FAIL", "iife try-finally: {}", s),
            other => panic!("expected String('FAIL'), got {:?}", other),
        }
    }

    /// Test: try-catch catches errors from called functions
    #[test]
    fn test_try_catch_across_function_boundary() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var result = "FAIL";
function thrower() {
    throw new Error("test error");
}
try {
    thrower();
    result = "NOT_CAUGHT";
} catch(e) {
    result = "CAUGHT";
}
result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "CAUGHT", "cross-boundary: {}", s),
            other => panic!("expected String('CAUGHT'), got {:?}", other),
        }
    }

    /// Test: try-finally with error in called function
    #[test]
    fn test_try_finally_executes() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var result = "FAIL";
var finallyRan = false;
function thrower() {
    throw new Error("boom");
}
try {
    thrower();
} catch(e) {
    result = "CAUGHT";
} finally {
    finallyRan = true;
}
result + ":" + finallyRan;
"#).expect("eval");
        match out {
            Object::String(s) => {
                let sv = s.to_string();
                assert!(sv.starts_with("CAUGHT"), "try-finally: {}", sv);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: Object.defineProperty with lazy getter (webpack pattern)
    #[test]
    fn test_lazy_getter_webpack_pattern() {
        let engine = ZippEngine::default();
        // Webpack defines exports with lazy getters that read from module-scope vars
        // The variable is assigned AFTER the getter is defined
        let out = engine.eval(r#"
var exports = {};
var StrictMode;
Object.defineProperty(exports, "StrictMode", {
    enumerable: true,
    get: function() { return StrictMode; }
});
// Variable assigned after getter is defined
StrictMode = 42;
// Access should call the getter and return 42
exports.StrictMode;
"#).expect("eval");
        assert_int(out, 42);
    }

    /// Test: Object.defineProperty lazy getter via compile_script (browser path)
    #[test]
    fn test_lazy_getter_persistent() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var exports = {};
var StrictMode;
Object.defineProperty(exports, "StrictMode", {
    enumerable: true,
    get: function() { return StrictMode; }
});
StrictMode = 42;
"#).expect("compile");
        state.run_init().expect("init");
        let result = state.eval_in_context("exports.StrictMode").expect("eval");
        assert_int(result, 42);
    }

    /// Test: webpack n.d pattern with lazy getters for module exports
    #[test]
    fn test_webpack_nd_lazy_getters() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var n = {};
n.d = function(e, t) {
    for (var r in t) {
        if (!e[r]) {
            Object.defineProperty(e, r, {enumerable: true, get: t[r]});
        }
    }
};
var react = {};
var STRICT_MODE = undefined;
var FRAGMENT = undefined;
n.d(react, {
    StrictMode: function() { return STRICT_MODE; },
    Fragment: function() { return FRAGMENT; }
});
STRICT_MODE = 99;
FRAGMENT = 88;
"#).expect("compile");
        state.run_init().expect("init");
        let sm = state.eval_in_context("react.StrictMode").expect("sm");
        assert_int(sm, 99);
        let frag = state.eval_in_context("react.Fragment").expect("frag");
        assert_int(frag, 88);
    }

    /// Test: property access on undefined/null is currently lenient (returns undefined)
    /// TODO: Change to throw TypeError once webpack circular dep resolution is fixed
    #[test]
    fn test_property_access_on_undefined_lenient() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var x = undefined;
var y = x.property;
typeof y;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "undefined"),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: forEach callback captures parameter correctly in closures
    #[test]
    fn test_foreach_closure_capture() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var fns = [];
["a","b","c"].forEach(function(e) { fns.push(function(){ return e; }); });
fns[0]() + ":" + fns[1]() + ":" + fns[2]();
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "a:b:c",
                "forEach closure capture: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: forEach with arrow function closure capture (webpack n.t pattern)
    #[test]
    fn test_foreach_arrow_closure_capture() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var f0, f1, f2;
var idx = 0;
["a","b","c"].forEach((e) => {
    var fn = () => e;
    if(idx===0) f0=fn;
    if(idx===1) f1=fn;
    if(idx===2) f2=fn;
    idx++;
});
f0() + ":" + f1() + ":" + f2();
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "a:b:c",
                "arrow closure: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: n.t full pattern with Object.defineProperty getters
    #[test]
    fn test_webpack_nt_namespace_wrapper() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var source = {a: 1, b: 2, c: 3};
var getters = {};
Object.getOwnPropertyNames(source).forEach((e) => { getters[e] = () => source[e]; });
var wrapper = {};
for (var key in getters) {
    Object.defineProperty(wrapper, key, {enumerable: true, get: getters[key]});
}
Object.keys(wrapper).length + ":" + wrapper.a + ":" + wrapper.b;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "3:1:2",
                "n.t wrapper: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: Object.keys includes getter-defined enumerable properties
    #[test]
    fn test_object_keys_with_getters() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var obj = {};
Object.defineProperty(obj, "x", {enumerable: true, get: function(){ return 42; }});
Object.defineProperty(obj, "y", {enumerable: true, get: function(){ return 99; }});
Object.keys(obj).length + ":" + obj.x + ":" + obj.y;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "2:42:99",
                "Object.keys with getters: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: Object.getOwnPropertyNames returns own property names
    #[test]
    fn test_get_own_property_names() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var obj = {x: 1, y: 2, z: 3};
Object.getOwnPropertyNames(obj).join(",");
"#).expect("eval");
        match out {
            Object::String(s) => {
                let sv = s.to_string();
                assert!(sv.contains("x") && sv.contains("y") && sv.contains("z"),
                    "getOwnPropertyNames: {}", sv);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: n.t pattern - getOwnPropertyNames().forEach with arrow creating getters
    #[test]
    fn test_nt_getownpropertynames_foreach() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var r = {a:1, b:2, c:3};
var i = {};
Object.getOwnPropertyNames(r).forEach((e) => { i[e] = () => r[e]; });
Object.keys(i).length + ":" + i.a() + ":" + i.b() + ":" + i.c();
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "3:1:2:3",
                "n.t pattern: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: n.t with exports object from simulated webpack require
    #[test]
    fn test_nt_webpack_exports() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
// Simulate webpack module system
var modules = {
    202: (e, t) => {
        t.StrictMode = 42;
        t.Fragment = 99;
        t.Component = 7;
    },
    43: (e, t, n) => {
        e.exports = n(202);
    }
};
var cache = {};
function require(id) {
    if (cache[id]) return cache[id].exports;
    var m = cache[id] = {exports: {}};
    modules[id](m, m.exports, require);
    return m.exports;
}
var r = require(43);
var names = Object.getOwnPropertyNames(r);
names.length + ":" + names.sort().join(",");
"#).expect("eval");
        match out {
            Object::String(s) => {
                let sv = s.to_string();
                assert!(sv.starts_with("3:"), "webpack exports names: {}", sv);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: FULL webpack + n.t simulation (exactly matches React bundle pattern)
    #[test]
    fn test_webpack_nt_combined() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var __result = "FAIL";
(()=>{
    var e = {
        202: (e, t) => {
            var o = 42;
            var a = 99;
            t.StrictMode = o;
            t.Fragment = a;
            t.Component = function(){};
        },
        43: (e, t, n) => { "use strict"; e.exports = n(202); }
    };
    var t = {};
    function n(r) {
        var a = t[r];
        if (void 0 !== a) return a.exports;
        var o = t[r] = {exports: {}};
        return e[r].call(o.exports, o, o.exports, n), o.exports;
    }
    // n.t implementation (exact copy from webpack)
    var _e2, _t2 = Object.getPrototypeOf ? (e2) => Object.getPrototypeOf(e2) : (e2) => e2.__proto__;
    n.t = function(r, a) {
        if (1 & a && (r = this(r)), 8 & a) return r;
        if ("object" === typeof r && r) {
            if (4 & a && r.__esModule) return r;
        }
        var o = {};
        var i = {};
        _e2 = _e2 || [null, _t2({}), _t2([]), _t2(_t2)];
        for (var l = 2 & a && r; "object" == typeof l && !~_e2.indexOf(l); l = _t2(l))
            Object.getOwnPropertyNames(l).forEach((e3) => { i[e3] = () => r[e3]; });
        i.default = () => r;
        // Apply getters (simplified n.d)
        for (var k in i) {
            Object.defineProperty(o, k, {enumerable: true, get: i[k]});
        }
        return o;
    };
    var r = n(43);
    var a = n.t(r, 2);
    // a should have StrictMode, Fragment, Component, default
    __result = Object.keys(a).length + ":" + a.StrictMode + ":" + a.Fragment;
})();
__result;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "4:42:99",
                "webpack+n.t: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: n.t full simulation with prototype chain walk
    #[test]
    fn test_nt_full_simulation() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var t = Object.getPrototypeOf ? (e) => Object.getPrototypeOf(e) : (e) => e.__proto__;
var r = {StrictMode: 42, Fragment: 99, Component: 7};
var o = {};
var i = {};
var e = [null, t({}), t([]), t(t)];
for (var l = r; typeof l === "object" && !~e.indexOf(l); l = t(l)) {
    Object.getOwnPropertyNames(l).forEach((e) => { i[e] = () => r[e]; });
}
i.default = () => r;
// Apply getters
for (var k in i) {
    Object.defineProperty(o, k, {enumerable: true, get: i[k]});
}
Object.keys(o).length + ":" + o.StrictMode + ":" + o.Fragment;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "4:42:99",
                "n.t full: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: EXACT n.t code from the webpack bundle
    #[test]
    fn test_exact_webpack_nt() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var __result = "FAIL";
// Simulate the webpack runtime IIFE
var n = {};
n.d = (e, t) => { for (var r in t) { if (t[r] !== undefined) Object.defineProperty(e, r, {enumerable: true, get: t[r]}); } };
n.r = (e) => { Object.defineProperty(e, "__esModule", {value: true}); };
n.o = (e, t) => Object.prototype.hasOwnProperty.call(e, t);
(()=>{
    var e, t = Object.getPrototypeOf ? (e) => Object.getPrototypeOf(e) : (e) => e.__proto__;
    n.t = function(r, a) {
        if (1 & a && (r = this(r)), 8 & a) return r;
        if ("object" === typeof r && r) {
            if (4 & a && r.__esModule) return r;
        }
        var o = {};
        n.r(o);
        var i = {};
        e = e || [null, t({}), t([]), t(t)];
        for (var l = 2 & a && r; "object" == typeof l && !~e.indexOf(l); l = t(l))
            Object.getOwnPropertyNames(l).forEach((e) => { i[e] = () => r[e]; });
        return i.default = () => r, n.d(o, i), o;
    };
})();
// Test with a source object
var source = {StrictMode: 42, Fragment: 99, Component: 7};
var wrapper = n.t(source, 2);
__result = Object.keys(wrapper).length + ":" + wrapper.StrictMode + ":" + wrapper.Fragment;
__result;
"#).expect("eval");
        match out {
            Object::String(s) => {
                let sv = s.to_string();
                // Expect: __esModule + StrictMode + Fragment + Component + default = 5
                assert!(sv.contains("42") && sv.contains("99"),
                    "exact n.t: {}", sv);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: variable 'e' used as both outer seen-array AND forEach arrow param
    /// This is the EXACT pattern in webpack's n.t that fails in the real bundle
    #[test]
    fn test_nt_variable_e_shadowing() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var __result = "FAIL";
(function() {
    var e;
    function doWork(r) {
        var i = {};
        e = e || ["sentinel"];
        Object.getOwnPropertyNames(r).forEach((e) => { i[e] = () => r[e]; });
        if (Array.isArray(e)) {
            __result = Object.keys(i).length + ":" + i.a() + ":" + i.b();
        } else {
            __result = "FAIL: e=" + typeof e + " " + e;
        }
    }
    doWork({a: 1, b: 2, c: 3});
})();
__result;
"#).expect("eval");
        match out {
            Object::String(s) => {
                let sv = s.to_string();
                assert!(sv.starts_with("3:"), "e shadowing: {}", sv);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: var r survives method calls on other objects
    #[test]
    fn test_var_r_survives_method_calls() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
(()=>{
    var r = {StrictMode: 42};
    var arr = ["a","b","c"];
    // Method call that internally uses callbacks
    arr.forEach(function(x) { });
    arr.map(function(x) { return x.toUpperCase(); });
    // Object.keys, Object.defineProperty, etc.
    var obj = {};
    Object.defineProperty(obj, "test", {get: function(){ return 1; }});
    Object.keys(obj);
    // After all that, r should still have StrictMode
    __result = r.StrictMode;
})();
"#).expect("compile");
        state.run_init().expect("init");
        let result = state.eval_in_context("__result").expect("result");
        assert_int(result, 42);
    }

    /// Test: var r survives many function calls with reload_locals_from_globals
    #[test]
    fn test_var_r_survives_function_calls() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
(()=>{
    var r = {StrictMode: 42};
    // Many function calls that use 'r' as local parameter
    function f1(r) { return r + 1; }
    function f2(r) { return r * 2; }
    function f3() { const r = "local"; return r; }
    f1(10);
    f2(20);
    f3();
    // After all those calls, outer r should still have StrictMode
    __result = r.StrictMode;
})();
"#).expect("compile");
        state.run_init().expect("init");
        let result = state.eval_in_context("__result").expect("result");
        assert_int(result, 42);
    }

    /// Test: webpack e.exports=n(202) makes r have React keys (compile_script path)
    #[test]
    fn test_webpack_exports_replacement_compile_script() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
(()=>{
    var e = {
        202: (e, t) => { t.StrictMode = 42; t.Fragment = 99; },
        43: (e, t, n) => { e.exports = n(202); }
    };
    var t = {};
    function n(r) {
        var a = t[r];
        if (void 0 !== a) return a.exports;
        var o = t[r] = {exports: {}};
        return e[r].call(o.exports, o, o.exports, n), o.exports;
    }
    var r = n(43);
    __sm = r.StrictMode;
    __frag = r.Fragment;
    __keys = Object.keys(r).length;
})();
"#).expect("compile");
        state.run_init().expect("init");
        let sm = state.eval_in_context("__sm").expect("sm");
        assert_int(sm, 42);
        let frag = state.eval_in_context("__frag").expect("frag");
        assert_int(frag, 99);
    }

    /// Test: var r persists through nested const r (compile_script path)
    #[test]
    fn test_var_r_persists_compile_script() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var r = {StrictMode: 42, Fragment: 99};
function setup() {
    const r = "local";
    return r;
}
setup();
var __sm = r.StrictMode;
"#).expect("compile");
        state.run_init().expect("init");
        let sm = state.eval_in_context("__sm").expect("sm");
        assert_int(sm, 42);
    }

    /// Test: comma expression returns second value after function call modifies object
    /// This is the webpack require pattern: return fn.call(...), o.exports
    #[test]
    fn test_comma_expr_after_property_replace() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
function require() {
    var o = {exports: {}};
    // Function that replaces o.exports
    function modify(mod) { mod.exports = {StrictMode: 42, Fragment: 99}; }
    modify(o);
    // Comma: call then return o.exports
    return o.exports;
}
var r = require();
Object.keys(r).length;
"#).expect("eval");
        assert_int(out, 2);
    }

    /// Test: return A, B uses comma operator (returns B not A)
    #[test]
    fn test_return_comma_operator() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
function test() {
    var x = {a: 1};
    return x.a = 99, x;
}
var r = test();
r.a;
"#).expect("eval");
        assert_int(out, 99);
    }

    /// Test: return with .call() then comma (webpack pattern)
    #[test]
    fn test_return_call_comma() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
function test() {
    var fn = function(o) { o.exports = {val: 42}; };
    var o = {exports: {}};
    return fn.call(null, o), o.exports;
}
var r = test();
r.val;
"#).expect("eval");
        assert_int(out, 42);
    }

    /// Test: EXACT webpack require pattern with .call() + 4 args + comma return
    #[test]
    fn test_webpack_require_exact_call_pattern() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var e = {
    43: (e,t,n) => { "use strict"; e.exports = n(202); },
    202: (e,t) => { "use strict"; t.StrictMode = 42; t.Fragment = 99; }
};
var t = {};
function n(r) {
    var a = t[r];
    if (void 0 !== a) return a.exports;
    var o = t[r] = {exports: {}};
    return e[r].call(o.exports, o, o.exports, n), o.exports;
}
var result = n(43);
Object.keys(result).length + ":" + result.StrictMode;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "2:42",
                "exact require: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: exact webpack require comma expression pattern
    #[test]
    fn test_webpack_require_comma_return() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var modules = {
    202: function(e, t) { t.StrictMode = 42; t.Fragment = 99; },
    43: function(e, t, n) { e.exports = n(202); }
};
var cache = {};
function n(id) {
    var a = cache[id];
    if (void 0 !== a) return a.exports;
    var o = cache[id] = {exports: {}};
    return modules[id].call(o.exports, o, o.exports, n), o.exports;
}
var r = n(43);
Object.keys(r).length + ":" + r.StrictMode;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "2:42",
                "webpack require: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: class static method with 'function r(e)' inside — EXACT bundle pattern
    #[test]
    fn test_class_static_method_function_r_no_corruption() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var __result = "FAIL";
(()=>{
    var r = {StrictMode: 42, Fragment: 99};
    class Rn {
        constructor(e) { this.data = e; }
        set(e, t) { const r = this; r.data = e; return r; }
        static from(e) { return new Rn(e); }
        static accessor(e) {
            var t = {};
            function r(e) { t[e] = true; }
            ["get","set","has"].forEach((r) => { t[r] = r; });
            return t;
        }
    }
    Rn.accessor("test");
    __result = Object.keys(r).length + ":" + r.StrictMode;
})();
"#).expect("compile");
        state.run_init().expect("init");
        let result = state.eval_in_context("__result").expect("result");
        match result {
            Object::String(s) => assert_eq!(s.to_string(), "2:42",
                "class static func r: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: const/let inside function does NOT affect outer var
    #[test]
    fn test_const_let_scoping_no_leak() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var r = {StrictMode: 42};
function inner() {
    const r = "something_else";
    return r;
}
inner();
r.StrictMode;
"#).expect("eval");
        assert_int(out, 42);
    }

    /// Test: const inside arrow does NOT affect outer var
    #[test]
    fn test_const_in_arrow_no_leak() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var r = {StrictMode: 42};
var fn = () => { const r = "other"; return r; };
fn();
r.StrictMode;
"#).expect("eval");
        assert_int(out, 42);
    }

    /// Test: shared object property mutation visible across function scopes
    /// (React dispatcher pattern: Ns.current = hooks, then hook reads ai.current)
    #[test]
    fn test_shared_object_property_mutation() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var shared = {current: null};
var ref1 = shared;
var ref2 = shared;
function setDispatcher(d) { ref1.current = d; }
function getDispatcher() { return ref2.current; }
setDispatcher("HOOKS");
getDispatcher();
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "HOOKS"),
            other => panic!("expected String('HOOKS'), got {:?}", other),
        }
    }

    /// Test: React-like pattern — set property inside function, read in another
    #[test]
    fn test_react_dispatcher_pattern() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var R = {current: null};
var L = {ReactCurrentDispatcher: R};
// Simulate react-dom reading the ref
var Ns = L.ReactCurrentDispatcher;
// Simulate hooks reading the ref
var ai = L.ReactCurrentDispatcher;
// Set dispatcher (react-dom reconciler)
Ns.current = {useState: function(init) { return [init, function(){}]; }};
// Read dispatcher (hook call)
var dispatcher = ai.current;
typeof dispatcher + ":" + typeof dispatcher.useState;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "object:function",
                "dispatcher: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: object identity must be preserved through property set/get
    #[test]
    fn test_object_identity_through_property() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var doc = {nodeType: 9, createElement: function(t) { return {tag: t}; }};
var body = {nodeType: 1};
body.ownerDocument = doc;
body.ownerDocument === doc;
"#).expect("eval");
        match out {
            Object::Boolean(b) => assert!(b, "object identity lost: body.ownerDocument !== doc"),
            other => panic!("expected Boolean(true), got {:?}", other),
        }
    }

    /// Test: object identity with class instances
    /// Test: this property assignment in constructor functions
    #[test]
    fn test_constructor_this_properties() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
function Foo(x) { this.value = x; this.name = "test"; }
var obj = new Foo(42);
obj.value + ":" + obj.name;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "42:test",
                "constructor this: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: arguments object in regular functions
    #[test]
    fn test_arguments_object() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
function test(a, b) {
    var n = arguments.length;
    var extra = arguments[2];
    return n + ":" + extra;
}
test(1, 2, "third");
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "3:third",
                "arguments: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: arguments indexing works
    #[test]
    fn test_arguments_indexing() {
        let engine = ZippEngine::default();
        // Test via compile_script (browser path)
        let mut state = engine.compile_script(r#"
function f(a, b) {
    __result = arguments[0] + ":" + arguments[1] + ":" + arguments[2];
}
f("X", "Y", "Z");
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "X:Y:Z",
                "arguments indexing: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: arguments indexing works across multiple nested functions (browser path).
    /// Each function should get its own arguments, not share a global slot.
    #[test]
    fn test_arguments_nested_no_collision() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var results = [];
function outer(a, b) {
    function inner(x, y, z) {
        results.push("inner:" + arguments[0] + "," + arguments[1] + "," + arguments[2]);
    }
    results.push("outer:" + arguments[0] + "," + arguments[1]);
    inner("X", "Y", "Z");
    // After inner call, outer's arguments should still be correct
    results.push("outer_after:" + arguments[0] + "," + arguments[1]);
}
outer("A", "B");
__result = results.join("|");
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(),
                "outer:A,B|inner:X,Y,Z|outer_after:A,B",
                "arguments collision: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: Object.prototype.toString.call works for all types
    #[test]
    fn test_object_prototype_tostring_call() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var ts = Object.prototype.toString;
ts.call(undefined) + "|" + ts.call(null) + "|" + ts.call("hi") + "|" + ts.call(42) + "|" + ts.call({}) + "|" + ts.call([]);
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(),
                "[object Undefined]|[object Null]|[object String]|[object Number]|[object Object]|[object Array]",
                "toString.call: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_object_identity_class_instance() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
class Node { constructor(t) { this.nodeType = t; this.ownerDocument = null; } }
var doc = new Node(9);
var body = new Node(1);
body.ownerDocument = doc;
body.ownerDocument === doc;
"#).expect("eval");
        match out {
            Object::Boolean(b) => assert!(b, "class instance identity lost"),
            other => panic!("expected Boolean(true), got {:?}", other),
        }
    }

    /// Test: function identity through property set/get (React Router pattern)
    #[test]
    fn test_function_identity_through_property() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function Route(e) { return null; }
var element = {type: Route, props: {}};
__result = (element.type === Route) + "|" + (typeof element.type) + "|" + (typeof Route);
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "true|function|function",
                "function identity: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: forwardRef $$typeof scoping with webpack module concatenation
    #[test]
    fn test_forwardref_typeof_scoping() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
// Mimics webpack module concatenation with shared IIFE scope
(function() {
    // Module 202: jsx runtime
    var n = 65520;  // Symbol.for("react.element")
    var a = n;
    function c(e, t, n) {
        var r, o = {};
        for (r in t) o[r] = t[r];
        return {$$typeof: a, type: e, props: o};
    }
    var jsx = c;

    // React core module (same scope due to concatenation)
    var c2 = 65527;  // Symbol.for("react.forward_ref") — different var name to avoid collision
    var forwardRef = function(e) { return {$$typeof: c2, render: e}; };

    // Test
    var fwdObj = forwardRef(function(props) { return null; });
    __result = fwdObj.$$typeof + "|" + (fwdObj.$$typeof === 65527);
})();
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "65527|true",
                "forwardRef typeof: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: EXACT createElement pattern - var c in for loop shadows captured outer c
    #[test]
    fn test_createElement_var_c_shadow() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var c = 65527;
var S = {current: null};
function N(e, t, r) {
    var a, o = {};
    var s = arguments.length - 2;
    if (1 === s) o.children = r;
    else if (1 < s) { for (var c = Array(s), u = 0; u < s; u++) c[u] = arguments[u + 2]; o.children = c; }
    return {$$typeof: 65520, type: e, props: o, _owner: S.current};
}
// Call N to trigger the var c inside the for loop
var el = N("div", null, "child1", "child2");
// After N returns, outer c should still be 65527
var getForwardRef = function() { return c; };
__result = getForwardRef() + "|" + (getForwardRef() === 65527);
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "65527|true",
                "createElement shadow: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: same variable name 'c' in concatenated module scope (actual bundle pattern)
    #[test]
    fn test_same_varname_concatenated_scope() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
(function() {
    // jsx runtime module defines function c
    var c_jsx;
    (function() {
        var n = 65520;
        var a = n;
        function c(e, t, n) { return {$$typeof: a, type: e, props: {}}; }
        c_jsx = c;
    })();

    // React core module defines var c as a different value
    var c_fwd;
    (function() {
        var c = 65527;  // This should NOT be affected by the jsx module's c
        c_fwd = function(e) { return {$$typeof: c, render: e}; };
    })();

    var result = c_fwd(function(){});
    __result = result.$$typeof + "|" + (result.$$typeof === 65527);
})();
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "65527|true",
                "concatenated c scoping: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: webpack module variable scoping - sibling functions with same var name
    #[test]
    fn test_webpack_module_var_scoping() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var modules = {};
// Module 1: defines function c
modules[1] = function(e, t) {
    function c(a, b, n) { return {type: a, props: b}; }
    t.jsx = c;
};
// Module 2: defines var c as a number
modules[2] = function(e, t) {
    var c = 42;
    t.getC = function() { return c; };
};
// Execute modules
var exports1 = {}; modules[1](null, exports1);
var exports2 = {}; modules[2](null, exports2);
__result = typeof exports1.jsx + "|" + exports2.getC();
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "function|42",
                "module scoping: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: typeof for objects with function properties (forwardRef pattern)
    #[test]
    fn test_typeof_object_with_function() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var obj = {$$typeof: 65521, render: function(e, t) { return null; }};
__result = typeof obj;
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "object",
                "typeof object with fn prop: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: Symbol.for returns same value for same key (critical for React)
    #[test]
    fn test_symbol_for_identity() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var Symbol = function(desc) { return 0xfff0 + Symbol._c++; };
Symbol._c = 0;
Symbol._r = {};
Symbol.for = function(key) {
    if (Symbol._r[key] !== undefined) return Symbol._r[key];
    var id = 0xfff0 + Symbol._c++;
    Symbol._r[key] = id;
    return id;
};
// Test: same key returns same value
var a = Symbol.for("react.forward_ref");
var b = Symbol.for("react.forward_ref");
// Test: forwardRef object stores and retrieves $$typeof correctly
var obj = {$$typeof: a, render: function(){}};
__result = (a === b) + "|" + (obj.$$typeof === b) + "|" + a + "|" + b + "|" + obj.$$typeof;
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => {
                let r = s.to_string();
                assert!(r.starts_with("true|true|"), "Symbol.for identity: {}", r);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: function identity through JSX-like element creation
    #[test]
    fn test_function_identity_jsx_pattern() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function Route(e) { return null; }
// Simulate jsx(Route, {path: "/"})
function jsx(type, props) {
    var o = {};
    for (var k in props) o[k] = props[k];
    return {type: type, props: o};
}
var el = jsx(Route, {path: "/"});
__result = (el.type === Route) + "";
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "true",
                "jsx function identity: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: let destructuring with default values and renaming
    #[test]
    fn test_let_destructuring() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var obj = {name: "Alice", age: 30, city: "NYC"};
function test(e) {
    let {name: n, age: a = 25, missing: m = "default", city: c} = e;
    __result = n + "|" + a + "|" + m + "|" + c;
}
test(obj);
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "Alice|30|default|NYC",
                "destructuring: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: arguments.length with fewer args than params (createElement pattern)
    #[test]
    fn test_arguments_length_fewer_args() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var results = [];
function N(e, t, r) {
    var o = {};
    // Copy config properties
    for (var a in t) {
        o[a] = t[a];
    }
    // Override children from arguments
    var s = arguments.length - 2;
    results.push("args.len=" + arguments.length + " s=" + s);
    if (1 === s) o.children = r;
    else if (1 < s) {
        var c = [];
        for (var u = 0; u < s; u++) c[u] = arguments[u + 2];
        o.children = c;
    }
    return o;
}
// Test 1: 2 args (no extra children)
var r1 = N("Provider", {children: "original_child", value: "ctx_value"});
results.push("r1.children=" + r1.children + " r1.value=" + r1.value);

// Test 2: 3 args (1 extra child)
var r2 = N("Provider", {value: "ctx_value"}, "extra_child");
results.push("r2.children=" + r2.children + " r2.value=" + r2.value);

__result = results.join("|");
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(),
                "args.len=2 s=0|r1.children=original_child r1.value=ctx_value|args.len=3 s=1|r2.children=extra_child r2.value=ctx_value",
                "createElement pattern: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: comma operator in assignment context (void 0 === expr, fn_call)
    #[test]
    fn test_comma_operator_assignment() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var l;
function create() { return {ok: true}; }
var result = (void 0 === (l = {x:1}) && (l = {}), create());
__result = typeof result + "|" + result.ok;
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "object|true",
                "comma op: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: createBrowserHistory pattern (let g shadows fn name + getters)
    #[test]
    fn test_create_browser_history_pattern() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function g(e, n, r, a) {
    var d = "POP";
    var m = null;
    function y() { return null; }
    var g = y();
    null == g && (g = 0);
    var x = {
        get action() { return d; },
        get location() { return {pathname: "/"}; },
        listen: function(fn) { m = fn; return function() { m = null; }; }
    };
    return x;
}
var hist = g(function(){}, function(){}, null, {});
__result = typeof hist + "|" + hist.action + "|" + hist.location.pathname;
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "object|POP|/",
                "history pattern: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: object literal with getter properties (like createBrowserHistory's return)
    #[test]
    fn test_object_literal_getters() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function createHistory() {
    var d = "POP";
    var loc = {pathname: "/", search: "", hash: ""};
    var x = {
        get action() { return d; },
        get location() { return loc; },
        listen: function(fn) { return function(){}; },
        createHref: function(e) { return e; }
    };
    return x;
}
var h = createHistory();
__result = typeof h + "|" + h.action + "|" + h.location.pathname;
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "object|POP|/",
                "getter object: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: new Error() creates object with .message
    #[test]
    fn test_error_message() {
        let engine = ZippEngine::default();
        let out = engine.eval(r#"
var e = new Error("hello");
typeof e + "|" + e.message;
"#).expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "object|hello", "Error: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: React's minified error function pattern (o function)
    #[test]
    fn test_react_error_function() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function o(e) {
    var t = "https://reactjs.org/docs/error-decoder.html?invariant=" + e;
    var n = 1;
    for (; n < arguments.length; n++)
        t += "&args[]=" + arguments[n];
    return "Minified React error #" + e + "; visit " + t + " for the full message.";
}
__result = o(308);
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => {
                let r = s.to_string();
                assert!(r.starts_with("Minified React error #308"), "o(308): {}", r);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: simplest try-catch at top level
    #[test]
    fn test_try_catch_toplevel() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
try {
    throw new Error("boom");
} catch(e) {
    __result = "got:" + e.message;
}
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "got:boom", "toplevel try: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: try-catch across function calls (like React's render catch)
    #[test]
    fn test_try_catch_across_calls() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function inner() {
    throw new Error("test_error");
}
function outer() {
    try {
        inner();
        return "no_error";
    } catch(e) {
        return "caught:" + e.message;
    }
}
__result = outer();
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "caught:test_error",
                "try-catch across calls: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: try-catch with deeply nested calls
    #[test]
    fn test_try_catch_deep_nesting() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function c() { throw new Error("deep"); }
function b() { return c(); }
function a() { return b(); }
function top() {
    try {
        return a();
    } catch(e) {
        return "caught:" + e.message;
    }
}
__result = top();
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "caught:deep",
                "deep try-catch: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: for-in on null should not throw (JS spec: null is silently skipped)
    #[test]
    fn test_forin_null() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var result = {};
for (var k in null) { result[k] = true; }
for (var k in undefined) { result[k] = true; }
__result = Object.keys(result).length;
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::Integer(n) => assert_eq!(n, 0, "for-in on null should not iterate"),
            other => panic!("expected Integer(0), got {:?}", other),
        }
    }

    /// Reproduce React Router pattern with all the destructured variables
    #[test]
    fn test_router_createElement_pattern() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
var k = Object.prototype.hasOwnProperty;
var j = {key:true, ref:true};
function N(e, t, r) {
    var a, o = {};
    if (null != t) {
        for (a in t) {
            k.call(t, a) && !j.hasOwnProperty(a) && (o[a] = t[a]);
        }
    }
    var s = arguments.length - 2;
    if (1 === s) o.children = r;
    return {type: e, props: o};
}

function ge(e) {
    let {basename:n="/", children:a=null, location:o, navigationType:i="POP", navigator:l, static:s=false, future:c} = e;
    var d = (n || "/").replace("x", "x");
    var f = {basename: d, navigator: l, static: s};
    "string" === typeof o && (o = {pathname: o});
    let {pathname:p="/", search:h="", hash:g="", state:y=null, key:b="default"} = o;
    var v = {location: {pathname: p, search: h, hash: g, state: y, key: b}, navigationType: i};
    // Critical: createElement with both children:a and value:v
    var innerEl = N("J.Provider", {children: a, value: v});
    var outerEl = N("K.Provider", {value: f}, innerEl);
    __result = "a_type=" + typeof a + " a_$$=" + (a && a.type) +
        " inner_ch=" + (innerEl.props.children && innerEl.props.children.type) +
        " inner_val=" + (innerEl.props.value && Object.keys(innerEl.props.value).join(",")) +
        " outer_ch=" + (outerEl.props.children && outerEl.props.children.type);
}
ge({
    children: {type: "App", $$typeof: 65520},
    location: {pathname: "/", search: "", hash: "", state: null, key: "default"},
    navigationType: "POP",
    navigator: {},
    future: undefined
});
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => {
                let r = s.to_string();
                assert!(r.contains("a_type=object"), "a type: {}", r);
                assert!(r.contains("a_$$=App"), "a val: {}", r);
                assert!(r.contains("inner_ch=App"), "inner children: {}", r);
                assert!(r.contains("inner_val=location,navigationType"), "inner value: {}", r);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    /// Test: let destructuring in function parameter position (like React Router)
    #[test]
    fn test_destructuring_function_param_style() {
        let engine = ZippEngine::default();
        let mut state = engine.compile_script(r#"
function ge(e) {
    let {basename: n = "/", children: a = null, location: o, navigationType: i = "POP"} = e;
    __result = n + "|" + a + "|" + o + "|" + i;
}
ge({basename: "/app", children: "CHILD", location: "/home", navigationType: "PUSH"});
"#).expect("compile");
        state.run_init().expect("init");
        let out = state.eval_in_context("__result").expect("eval");
        match out {
            Object::String(s) => assert_eq!(s.to_string(), "/app|CHILD|/home|PUSH",
                "destructuring param: {}", s),
            other => panic!("expected String, got {:?}", other),
        }
    }
}
