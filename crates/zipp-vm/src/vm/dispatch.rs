#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Drives execution from the current frame until the frame that was current
    /// on entry returns (frames drops to `stop_depth`), catching thrown values
    /// at `try` handlers along the way. `run()` passes 0 (drain everything);
    /// `call_value` passes the pre-call depth (run one nested call).
    ///
    /// On a throw, [`Self::dispatch_body`] returns `Err`; we look up the thrown
    /// value and unwind to the nearest handler at or above `stop_depth`. If one
    /// exists, execution resumes at its catch target; otherwise the throw
    /// propagates out (with `pending_throw` left set so an enclosing `run_loop`
    /// â€” e.g. the caller of a builtin callback â€” can still catch it).
    pub(crate) fn run_loop(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            match self.dispatch_body(stop_depth) {
                Ok(v) => return Ok(v),
                Err(t) => {
                    let tv = match self.pending_throw {
                        Some(v) => v,
                        None => {
                            // Internal error (TypeError/RangeError/â€¦) with no
                            // explicit thrown value: synthesise a real Error
                            // object so `catch (e)` sees `e.name`/`e.message` and
                            // `e instanceof TypeError`, matching JS.
                            let v = self.alloc_error_from_message(&t.0);
                            self.pending_throw = Some(v);
                            v
                        }
                    };
                    if self.unwind_to_handler(tv, stop_depth) {
                        self.pending_throw = None; // caught â€” resume at catch
                        continue;
                    }
                    // Uncaught here; propagate. If the carried message is empty
                    // (e.g. a JIT-bail unwind that signalled via pending_throw
                    // with no text), recompute it from the thrown value so the
                    // top-level report shows the real error, not "".
                    if t.0.is_empty() {
                        return Err(Thrown(self.throw_message(tv)));
                    }
                    return Err(t); // pending_throw stays set for an outer catch
                }
            }
        }
    }

    /// Pop frames from the top down to (but not below) `stop_depth`, looking for
    /// a `try` handler. A `Catch` deposits `tv` in its register and resumes at the
    /// catch target. A `Finally` deposits a throw completion (kind 2 + the reason)
    /// into its registers and resumes at the finally target â€” `EndFinally`
    /// re-throws after the finally runs. Either way execution resumes (`true`). If
    /// the boundary is reached with no handler, return `false` (propagate).
    pub(crate) fn unwind_to_handler(&mut self, tv: Value, stop_depth: usize) -> bool {
        while self.frames.len() > stop_depth {
            let top = self.frames.len() - 1;
            if let Some(h) = self.frames[top].handlers.pop() {
                let base = self.frames[top].base;
                match h {
                    Handler::Catch { target, reg } => {
                        self.regs[base + reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                    Handler::Finally { target, kind_reg, val_reg } => {
                        self.regs[base + kind_reg as usize] = Value::int(2); // throw
                        self.regs[base + val_reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                }
                return true;
            }
            // No handler in this frame: discard it and its register window.
            let f = self.frames.pop().unwrap();
            self.regs.truncate(f.base);
        }
        false
    }

    /// On a non-throw leave of the top frame (`return`, and later break/continue),
    /// run any pending `finally` first. Discards `Catch` handlers we are exiting;
    /// on the innermost `Finally`, deposits the completion (`kind` 1=return + the
    /// `value`) into its registers and returns its target so the caller resumes
    /// there (`EndFinally` later re-leaves). Returns `None` when no finally is
    /// pending â€” the caller performs the real leave (pop the frame).
    pub(crate) fn route_through_finally(&mut self, kind: i32, value: Value) -> Option<u32> {
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        while let Some(h) = self.frames[top].handlers.last().copied() {
            match h {
                Handler::Finally { target, kind_reg, val_reg } => {
                    self.frames[top].handlers.pop();
                    self.regs[base + kind_reg as usize] = Value::int(kind);
                    self.regs[base + val_reg as usize] = value;
                    return Some(target);
                }
                Handler::Catch { .. } => {
                    self.frames[top].handlers.pop();
                }
            }
        }
        None
    }

    /// Route a `break`/`continue` (`JumpFinally`) that exits one or more `try`
    /// blocks. Pops handlers in the current frame until the stack is back to
    /// `floor` (the handler depth at the target loop): a `Catch` being exited is
    /// discarded, and the first `Finally` encountered is run first â€” a kind-3
    /// (jump) completion is deposited (the `floor` packed into the kind word's
    /// upper bits, the jump `target` in the value register) so `EndFinally`
    /// resumes the unwind toward `target`. Returns the finally body to run, or
    /// `None` when the stack is already at `floor` (jump straight to `target`).
    pub(crate) fn route_jump_through_finally(&mut self, target: u32, floor: usize) -> Option<u32> {
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        while self.frames[top].handlers.len() > floor {
            let h = self.frames[top].handlers.pop().unwrap();
            if let Handler::Finally { target: ftarget, kind_reg, val_reg } = h {
                self.regs[base + kind_reg as usize] = Value::int(3 | ((floor as i32) << 2));
                self.regs[base + val_reg as usize] = Value::int(target as i32);
                return Some(ftarget);
            }
            // A `Catch` handler being exited has no body to run â€” discard it.
        }
        None
    }

    /// The inner execution loop: runs ops in the current frame until a frame
    /// transition (a call pushes / a return pops) or a throw. Returns the value
    /// when the `stop_depth` frame returns, or `Err` to begin unwinding.
    pub(crate) fn dispatch_body(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            // GC safe point on every frame transition (call/return): no native
            // built-in is mid-flight holding an un-rooted Vec here, and all live
            // Values are in regs/frames/globals/side-tables (the GC root set).
            self.maybe_gc();
            // Snapshot the current frame's coordinates. `ip` is advanced as a
            // local and written back only on frame transitions / loops.
            let frame_idx = self.frames.len() - 1;
            let func_id = self.frames[frame_idx].func;
            let base = self.frames[frame_idx].base;
            let mut ip = self.frames[frame_idx].ip;
            let cur_closure = self.frames[frame_idx].closure;
            let code: *const Vec<Instr> = &self.func(func_id as usize).code;
            // SAFETY: `code` borrows immutable program data that outlives the
            // loop; we never mutate program functions during execution.
            let code: &Vec<Instr> = unsafe { &*code };

            // â”€â”€ JIT tier â”€â”€
            // On fresh frame entry (ip == 0), if this function has compiled
            // native code, run it over the frame's register window. The native
            // code shares `self.regs`, so on a bail the interpreter resumes with
            // consistent state. Only entered at ip==0: a bail sets `ip` to the
            // resume point and falls into the interpreter for the rest of this
            // activation (never re-enters native mid-function). We also count
            // entries here and compile on crossing the threshold.
            // Only enter native code from a NON-recursive interpreter context
            // (`jit_recurse_depth == 0`). Once a native self-call has deopted and
            // we're finishing it on the interpreter, re-entering the JIT for the
            // continuation would livelock: native recurses 256, deopts, the
            // interpreter re-enters native, recurses 256, deoptsâ€¦ forever,
            // because the per-call native depth counter resets each return and
            // interpreter frames never reach MAX_FRAMES. Staying interpreted in
            // that subtree lets frames accumulate monotonically â†’ RangeError.
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            if ip == 0
                && self.jit_enabled
                && self.jit_recurse_depth == 0
                // Runtime `eval`/`new Function` functions live past the program's
                // function table (leaked boxes addressed via `func()`); the JIT
                // indexes `program.functions` directly, so never JIT them â€” they
                // always interpret.
                && (func_id as usize) < self.main_func_count
                && !self.func(func_id as usize).is_generator
                && !self.func(func_id as usize).is_async
            {
                if let Some((result, bail)) = self.try_run_jit(func_id, base) {
                    if bail == crate::codegen::NO_BAIL {
                        // Native code returned: behave like a `Return`.
                        if self.pop_frame_with(result, stop_depth) {
                            return Ok(result);
                        }
                        continue; // re-enter outer loop with caller frame
                    }
                    // A bail can mean two things:
                    // (a) a normal deopt (non-int operand, overflow): resume the
                    //     interpreter at the recorded ip with consistent regs.
                    // (b) a self-recursive call threw (e.g. RangeError) and the
                    //     helper signalled deopt with `pending_throw` set â€” the
                    //     whole native chain must UNWIND, not resume. Detect (b)
                    //     by the pending throw and return Err so `run_loop`
                    //     dispatches it to the nearest handler / propagates it.
                    if self.pending_throw.is_some() {
                        // Persist ip for coherence, then unwind. The message is
                        // recomputed by run_loop from pending_throw.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = bail as usize;
                        return Err(Thrown(String::new()));
                    }
                    // (a): resume the interpreter at the recorded ip.
                    ip = bail as usize;
                } else if self.jit.record_and_should_compile(func_id) {
                    let proto: *const crate::bytecode::FuncProto =
                        self.func(func_id as usize);
                    // SAFETY: program functions are immutable during execution.
                    let proto_ref = unsafe { &*proto };
                    // The self-function's current global Value (a heap Func),
                    // stable since hoist_functions ran at startup. Embedded so a
                    // JIT'd `LoadGlobal(self_slot)` stores the REAL function (not
                    // a placeholder) â€” required for a deopted self-Call to
                    // resolve the callee correctly in the interpreter.
                    let self_val = proto_ref
                        .name_global
                        .and_then(|s| self.globals.get(s as usize).copied())
                        .unwrap_or(Value::UNDEFINED)
                        .bits();
                    self.jit.compile(
                        func_id,
                        proto_ref,
                        jit_self_call_at as usize,
                        self_val,
                    );
                }
            }

            // Inner loop: execute within the current frame until a call pushes
            // a new frame or a return pops this one.
            loop {
                let instr = &code[ip];
                match *instr {
                    Instr::LoadConst { dst, idx } => {
                        let v = self.func(func_id as usize).constants[idx as usize];
                        // String constants are stored with a sentinel; resolve
                        // to a freshly-interned heap string the first time.
                        let resolved = self.resolve_const(func_id, v);
                        self.set(base, dst, resolved);
                        ip += 1;
                    }
                    Instr::LoadInt { dst, val } => {
                        self.set(base, dst, Value::int(val));
                        ip += 1;
                    }
                    Instr::LoadUndefined { dst } => {
                        self.set(base, dst, Value::UNDEFINED);
                        ip += 1;
                    }
                    Instr::LoadNewTarget { dst } => {
                        let nt = self.frames.last().map(|f| f.new_target).unwrap_or(Value::UNDEFINED);
                        self.set(base, dst, nt);
                        ip += 1;
                    }
                    Instr::LoadCallee { dst } => {
                        // The function value of the running frame. Prefer the actual
                        // callee the caller invoked (so a named function expression's
                        // own name === the outer reference); fall back to the Closure
                        // object, or a fresh Func for a plain function whose caller is
                        // unknown (generator/async resume, top-level).
                        let (callee, clo, fid) = {
                            let fr = self.frames.last().unwrap();
                            (fr.callee, fr.closure, fr.func)
                        };
                        let v = if !callee.is_undefined() {
                            callee
                        } else if clo != NO_CLOSURE {
                            Value::heap(clo)
                        } else {
                            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadClassValue { dst, class_id } => {
                        // The inner class-name binding (used by methods/ctor/static
                        // blocks + arrows within them). None = accessed before the
                        // class value is initialized (e.g. `class C extends C`) → TDZ.
                        match self.class_values.get(class_id as usize).copied().flatten() {
                            Some(v) => self.set(base, dst, v),
                            None => {
                                return Err(Thrown(
                                    "ReferenceError: class binding accessed before initialization"
                                        .into(),
                                ))
                            }
                        }
                        ip += 1;
                    }
                    Instr::LoadHole { dst } => {
                        // The HOLE sentinel for an elided array-literal element; the
                        // following NewArray/ArrayAppend copies it into the array.
                        self.set(base, dst, Value::HOLE);
                        ip += 1;
                    }
                    Instr::LoadNull { dst } => {
                        self.set(base, dst, Value::NULL);
                        ip += 1;
                    }
                    Instr::LoadBool { dst, val } => {
                        self.set(base, dst, Value::bool(val));
                        ip += 1;
                    }
                    Instr::Move { dst, src } => {
                        let v = self.get(base, src);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobal { dst, idx } => {
                        let v = self.globals[idx as usize];
                        if v.is_uninitialized() {
                            // Referenced but never declared â†’ ReferenceError. The
                            // name is in `program.global_names`, or â€” for a slot an
                            // `eval` drew from the EVAL_POOL â€” in `eval_global_map`.
                            let name = self
                                .program
                                .global_names
                                .get(idx as usize)
                                .map(|s| s.as_str())
                                .or_else(|| {
                                    self.eval_global_map
                                        .iter()
                                        .find(|(_, &v)| v == idx)
                                        .map(|(k, _)| k.as_str())
                                })
                                .unwrap_or("?")
                                .to_string();
                            // A binding created on the global OBJECT in sloppy code
                            // (`this.x = v` / `globalThis.x = v`) lives as an own
                            // property there, not in this slot. The global object's
                            // own properties ARE global bindings, so a bare read
                            // resolves to it. Own-only: inherited Object.prototype
                            // members (`toString`, â€¦) are NOT global bindings, so an
                            // undeclared name still ReferenceErrors. Slot-only names
                            // (`global_by_name`) are excluded by checking the ObjMap
                            // directly, preserving their uninitialized ReferenceError.
                            let has_own = self.global_this != 0
                                && matches!(
                                    self.heap.get(self.global_this),
                                    HeapObj::Object(m) if m.pos(&name).is_some()
                                );
                            if !has_own {
                                return Err(Thrown(format!("ReferenceError: {name} is not defined")));
                            }
                            let gobj = Value::heap(self.global_this);
                            let val = self.get_prop(gobj, &name)?;
                            self.set(base, dst, val);
                            ip += 1;
                        } else {
                            self.set(base, dst, v);
                            ip += 1;
                        }
                    }
                    Instr::LoadGlobalOrUndefined { dst, idx } => {
                        let v = self.globals[idx as usize];
                        let v = if v.is_uninitialized() {
                            // The binding may be own-prop-backed on the global
                            // object (eval-created vars / `this.x = v`).
                            match self.global_slot_name(idx) {
                                Some(name)
                                    if self.global_this != 0
                                        && matches!(
                                            self.heap.get(self.global_this),
                                            HeapObj::Object(m) if m.pos(&name).is_some()
                                        ) =>
                                {
                                    let gobj = Value::heap(self.global_this);
                                    self.get_prop(gobj, &name)?
                                }
                                _ => Value::UNDEFINED,
                            }
                        } else {
                            v
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobalDyn { dst, idx } => {
                        // Dynamic-first: the activation's EvalScope may bind
                        // this slot's NAME (a sloppy fn-context eval var).
                        if let Some(v) = self.eval_scope_lookup(idx) {
                            self.set(base, dst, v);
                            ip += 1;
                            continue;
                        }
                        let v = self.globals[idx as usize];
                        if v.is_uninitialized() {
                            let name = self.global_slot_name(idx).unwrap_or_default();
                            let has_own = self.global_this != 0
                                && matches!(
                                    self.heap.get(self.global_this),
                                    HeapObj::Object(m) if m.pos(&name).is_some()
                                );
                            if !has_own {
                                return Err(Thrown(format!(
                                    "ReferenceError: {name} is not defined"
                                )));
                            }
                            let gobj = Value::heap(self.global_this);
                            let val = self.get_prop(gobj, &name)?;
                            self.set(base, dst, val);
                        } else {
                            self.set(base, dst, v);
                        }
                        ip += 1;
                    }
                    Instr::LoadGlobalOrUndefinedDyn { dst, idx } => {
                        if let Some(v) = self.eval_scope_lookup(idx) {
                            self.set(base, dst, v);
                            ip += 1;
                            continue;
                        }
                        let v = self.globals[idx as usize];
                        let v = if v.is_uninitialized() {
                            match self.global_slot_name(idx) {
                                Some(name)
                                    if self.global_this != 0
                                        && matches!(
                                            self.heap.get(self.global_this),
                                            HeapObj::Object(m) if m.pos(&name).is_some()
                                        ) =>
                                {
                                    let gobj = Value::heap(self.global_this);
                                    self.get_prop(gobj, &name)?
                                }
                                _ => Value::UNDEFINED,
                            }
                        } else {
                            v
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::StoreGlobalDyn { idx, src } => {
                        let v = self.get(base, src);
                        if self.eval_scope_store(idx, v) {
                            ip += 1;
                            continue;
                        }
                        if self.globals[idx as usize].is_uninitialized() {
                            if let Some(name) = self.global_slot_name(idx) {
                                let has_own = self.global_this != 0
                                    && matches!(
                                        self.heap.get(self.global_this),
                                        HeapObj::Object(m) if m.pos(&name).is_some()
                                    );
                                if has_own {
                                    let gobj = Value::heap(self.global_this);
                                    self.set_prop(gobj, &name, v, false)?;
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::StoreGlobal { idx, src } => {
                        let v = self.get(base, src);
                        if self.globals[idx as usize].is_uninitialized() {
                            // An own-prop-backed binding (eval-created /
                            // `this.x = v`) is the live binding: write through
                            // it and leave the slot uninitialized.
                            if let Some(name) = self.global_slot_name(idx) {
                                let has_own = self.global_this != 0
                                    && matches!(
                                        self.heap.get(self.global_this),
                                        HeapObj::Object(m) if m.pos(&name).is_some()
                                    );
                                if has_own {
                                    let gobj = Value::heap(self.global_this);
                                    self.set_prop(gobj, &name, v, false)?;
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::StoreGlobalStrict { idx, src } => {
                        // Strict assignment to an unresolvable reference (a global slot
                        // never declared/initialized) is a ReferenceError, not a global
                        // creation. (No own-prop fallback here: the reference's
                        // unresolvable-ness was fixed when the LHS was evaluated —
                        // a property the RHS created meanwhile must not resolve it.)
                        if self.globals[idx as usize].is_uninitialized() {
                            let name = self.global_slot_name(idx).unwrap_or_else(|| "?".into());
                            return Err(Thrown(format!("ReferenceError: {name} is not defined")));
                        }
                        let v = self.get(base, src);
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::Now { dst, epoch } => {
                        let ms = if epoch {
                            // Date.now(): integer ms since the Unix epoch.
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as f64)
                                .unwrap_or(0.0)
                        } else {
                            // performance.now(): fractional ms since VM start.
                            self.start.elapsed().as_secs_f64() * 1000.0
                        };
                        self.set(base, dst, Value::num(ms));
                        ip += 1;
                    }

                    Instr::Add { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // Identical to `Add` â€” a JIT routing hint only (see bytecode).
                    Instr::StrConcat { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // In-place string append (emitter proved `a` uniquely owned).
                    Instr::StrAppendInPlace { dst, a, b } => {
                        let av = self.get(base, a);
                        let bv = self.get(base, b);
                        let r = self.str_append_inplace(av, bv);
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // ToString(a) with the STRING hint (toString before valueOf),
                    // for template-literal substitutions.
                    Instr::ToStr { dst, a } => {
                        let av = self.get(base, a);
                        let s = self.to_js_string(av)?;
                        let r = self.alloc_str(s);
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Sub { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            match va.as_int().checked_sub(vb.as_int()) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                            }
                        } else if let Some(bv) = self.bigint_binop(BigOp::Sub, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number_coerce(va)? - self.to_number_coerce(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mul { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            match va.as_int().checked_mul(vb.as_int()) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 * vb.as_int() as f64),
                            }
                        } else if let Some(bv) = self.bigint_binop(BigOp::Mul, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number_coerce(va)? * self.to_number_coerce(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Div { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if let Some(bv) = self.bigint_binop(BigOp::Div, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number_coerce(va)? / self.to_number_coerce(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mod { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if let Some(bv) = self.bigint_binop(BigOp::Mod, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number_coerce(va)? % self.to_number_coerce(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ToNum { dst, a } => {
                        let va = self.get(base, a);
                        // `+x`: numbers pass through (keep Int tag); `+bigint` throws
                        // (unary plus is not defined on BigInt); else ToNumber STRICT â€”
                        // a BigInt reached via ToPrimitive (a boxed `Object(1n)`, or an
                        // object whose valueOf returns a BigInt) is also a TypeError.
                        let r = if va.is_number() {
                            va
                        } else if self.bigint_value(va).is_some() {
                            return Err(Thrown("TypeError: Cannot convert a BigInt value to a number".into()));
                        } else {
                            Value::num(self.to_number_strict(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Neg { dst, a } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            let i = va.as_int();
                            if i == 0 {
                                // `-0` is the negative-zero DOUBLE, not integer 0
                                // (so `1/-0` is -Infinity, `Object.is(-0,+0)` false).
                                Value::num(-0.0)
                            } else {
                                match i.checked_neg() {
                                    Some(v) => Value::int(v),
                                    None => Value::num(-(i as f64)),
                                }
                            }
                        } else if let Some(n) = self.bigint_value(va) {
                            self.make_bigint(n.wrapping_neg())
                        } else {
                            Value::num(-self.to_number_coerce(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Bitwise { dst, a, b, op } => {
                        use crate::bytecode::BitwiseOp as B;
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        // BigInt bitwise: &/|/^/<</>> on two BigInts (incl. wrapper
                        // objects, via bigint_binop's ToNumeric); `>>>` is not defined
                        // for BigInt (TypeError); mixing â†’ TypeError.
                        match op {
                            B::Ushr => {
                                if self.this_bigint_value(va).is_some()
                                    || self.this_bigint_value(vb).is_some()
                                {
                                    return Err(Thrown(
                                        "TypeError: BigInts have no unsigned right shift, use >> instead"
                                            .into(),
                                    ));
                                }
                            }
                            _ => {
                                let bop = match op {
                                    B::And => BigOp::And,
                                    B::Or => BigOp::Or,
                                    B::Xor => BigOp::Xor,
                                    B::Shl => BigOp::Shl,
                                    B::Shr => BigOp::Shr,
                                    B::Ushr => unreachable!(),
                                };
                                if let Some(bv) = self.bigint_binop(bop, va, vb)? {
                                    self.set(base, dst, bv);
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        let x = to_int32(self.to_number_coerce(va)?);
                        // Shift counts use the low 5 bits per the JS spec.
                        let r = match op {
                            B::And => Value::int(x & to_int32(self.to_number_coerce(vb)?)),
                            B::Or => Value::int(x | to_int32(self.to_number_coerce(vb)?)),
                            B::Xor => Value::int(x ^ to_int32(self.to_number_coerce(vb)?)),
                            B::Shl => {
                                let s = to_uint32(self.to_number_coerce(vb)?) & 31;
                                Value::int(x.wrapping_shl(s))
                            }
                            B::Shr => {
                                let s = to_uint32(self.to_number_coerce(vb)?) & 31;
                                Value::int(x >> s)
                            }
                            B::Ushr => {
                                let s = to_uint32(self.to_number_coerce(vb)?) & 31;
                                let u = to_uint32(self.to_number_coerce(va)?) >> s;
                                // u32 may exceed i32::MAX â†’ keep numeric range.
                                if u <= i32::MAX as u32 {
                                    Value::int(u as i32)
                                } else {
                                    Value::num(u as f64)
                                }
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Pow { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if let Some(bv) = self.bigint_binop(BigOp::Pow, va, vb)? {
                            bv
                        } else {
                            let bb = self.to_number_coerce(va)?;
                            let ee = self.to_number_coerce(vb)?;
                            // Spec: |base|==1 with a NaN/±Infinity exponent is NaN
                            // (C/Rust powf returns 1 — a deliberate deviation).
                            let p = if (bb == 1.0 || bb == -1.0) && (ee.is_nan() || ee.is_infinite()) {
                                f64::NAN
                            } else {
                                bb.powf(ee)
                            };
                            Value::num(p)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::BitNot { dst, a } => {
                        let va = self.get(base, a);
                        if let Some(n) = self.bigint_value(va) {
                            let r = self.make_bigint(!n);
                            self.set(base, dst, r);
                        } else {
                            let r = !to_int32(self.to_number_coerce(va)?);
                            self.set(base, dst, Value::int(r));
                        }
                        ip += 1;
                    }
                    Instr::AddInt { dst, a, imm } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_add(imm) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + imm as f64),
                            }
                        } else if let Some(b) = self.bigint_value(va) {
                            // `++`/`--` (this op backs every UpdateExpression) on a
                            // BigInt operand stays a BigInt â€” ToNumeric keeps the type
                            // (so `n++` yields `n + 1n`, not the Number coercion).
                            self.make_bigint(b.wrapping_add(imm as i128))
                        } else {
                            Value::num(self.to_number_coerce(va)? + imm as f64)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Lt { dst, a, b } => {
                        let r = self.cmp_lt(base, a, b, true)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Le { dst, a, b } => {
                        let r = self.cmp_le(base, a, b, true)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Gt { dst, a, b } => {
                        // `a > b` ≡ IsLessThan(b, a, LeftFirst=false): swap registers,
                        // but the source-left operand `a` must still coerce first.
                        let r = self.cmp_lt(base, b, a, false)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ge { dst, a, b } => {
                        let r = self.cmp_le(base, b, a, false)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseEq { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.loose_eq(va, vb)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseNe { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.loose_eq(va, vb)?;
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Eq { dst, a, b } => {
                        let r = self.strict_eq(base, a, b);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ne { dst, a, b } => {
                        let r = self.strict_eq(base, a, b);
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Not { dst, a } => {
                        let va = self.get(base, a);
                        let t = self.truthy(va);
                        self.set(base, dst, Value::bool(!t));
                        ip += 1;
                    }
                    Instr::TypeOf { dst, a } => {
                        let va = self.get(base, a);
                        let t = self.type_of(va);
                        let v = self.alloc_str(t.to_string());
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::IsArray { dst, a } => {
                        let v = self.get(base, a);
                        let is_arr = self.value_is_array_throwing(v)?;
                        self.set(base, dst, Value::bool(is_arr));
                        ip += 1;
                    }
                    Instr::JsonStringify { dst, val, space } => {
                        let v = self.get(base, val);
                        let indent = self.json_indent(self.get(base, space));
                        // `JSON.stringify(undefined)` (and of a function) is undefined.
                        // This op is the single-arg form: no replacer / allowlist.
                        let _gc = self.gc_lock_guard();
                        let mut m = crate::heap::ObjMap::new();
                        m.set("", v);
                        let wrapper = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                        let mut visited = Vec::new();
                        let result = match self.json_value(
                            wrapper,
                            "",
                            v,
                            &indent,
                            0,
                            &mut visited,
                            Value::UNDEFINED,
                            None,
                        )? {
                            Some(s) => self.alloc_str(s),
                            None => Value::UNDEFINED,
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::JsonParse { dst, a } => {
                        let arg = self.get(base, a);
                        // ToString (invokes toString/valueOf; throws TypeError for a Symbol).
                        let s = self.to_js_string(arg)?;
                        let v = self.json_parse(&s)?; // propagates SyntaxError as a throw
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayAppend { arr, val, spread } => {
                        let aidx = self.get(base, arr).heap_index();
                        let vv = self.get(base, val);
                        if spread {
                            // An array whose Array.prototype[Symbol.iterator] was
                            // replaced spreads via the iterator protocol (the inline
                            // fast path below assumes the default iterator).
                            if vv.is_heap()
                                && matches!(self.heap.get(vv.heap_index()), HeapObj::Array(_))
                            {
                                let m = self.get_prop(vv, "@@iterator")?;
                                if m.bits() != self.default_array_iter.bits()
                                    && self.is_callable(m)
                                {
                                    let elems = self.iterate_to_vec(vv)?;
                                    if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                        dst_items.extend(elems);
                                    }
                                    ip += 1;
                                    continue;
                                }
                            }
                            // A generator or a custom iterable (object) is drained
                            // via the iterator protocol (iterate_to_vec also errors
                            // for a plain, non-iterable object, as a spread should).
                            if vv.is_heap()
                                && matches!(
                                    self.heap.get(vv.heap_index()),
                                    HeapObj::Generator { .. }
                                        | HeapObj::Object(_)
                                        | HeapObj::Iterator { .. }
                                        | HeapObj::IterHelper { .. }
                                        | HeapObj::TypedArray { .. }
                                )
                            {
                                let elems = self.iterate_to_vec(vv)?;
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                                ip += 1;
                                continue;
                            }
                            // Materialize the spread source's elements (array/set â†’
                            // elements; string â†’ chars; map â†’ [k,v] entries) WITHOUT
                            // holding a heap borrow across the fresh allocations.
                            let mut chars: Option<Vec<char>> = None;
                            let mut map_pairs: Option<Vec<(Value, Value)>> = None;
                            if vv.is_heap() {
                                match self.heap.get(vv.heap_index()) {
                                    HeapObj::Array(items) => {
                                        let elems = items.clone();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Set(items) => {
                                        // Skip tombstoned (deleted) slots.
                                        let elems: Vec<Value> =
                                            items.iter().copied().filter(|v| !v.is_hole()).collect();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Str(_) | HeapObj::Cons { .. } => {
                                        chars = Some(self.heap.str_cow(vv.heap_index()).unwrap().chars().collect());
                                    }
                                    HeapObj::Map { keys, vals } => {
                                        // Skip tombstoned (deleted) entries.
                                        map_pairs = Some(
                                            keys.iter()
                                                .copied()
                                                .zip(vals.iter().copied())
                                                .filter(|(k, _)| !k.is_hole())
                                                .collect(),
                                        );
                                    }
                                    _ => return Err(Thrown("TypeError: spread value is not iterable".into())),
                                }
                            } else {
                                return Err(Thrown("TypeError: spread value is not iterable".into()));
                            }
                            if let Some(chars) = chars {
                                let elems: Vec<Value> =
                                    chars.into_iter().map(|c| self.alloc_str(c.to_string())).collect();
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                            }
                            if let Some(pairs) = map_pairs {
                                let elems: Vec<Value> = pairs
                                    .into_iter()
                                    .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                                    .collect();
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                            }
                        } else if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                            dst_items.push(vv);
                        }
                        ip += 1;
                    }
                    Instr::ArrayRest { dst, src, start } => {
                        let sv = self.get(base, src);
                        let mut elems = self.iterate_to_vec(sv)?;
                        let start = (start as usize).min(elems.len());
                        let rest = elems.split_off(start);
                        let arr = Value::heap(self.heap.alloc(HeapObj::Array(rest)));
                        self.set(base, dst, arr);
                        ip += 1;
                    }
                    Instr::ObjectSpread { target, src } => {
                        let t = self.get(base, target);
                        let s = self.get(base, src);
                        self.object_assign(&[t, s])?; // mutates target in place
                        ip += 1;
                    }
                    Instr::ObjectRest { dst, src, exclude_start, exclude_count } => {
                        let s = self.get(base, src);
                        let consts = &self.func(func_id as usize).string_constants;
                        let excluded =
                            &consts[exclude_start as usize..exclude_start as usize + exclude_count as usize];
                        // Copy src's own enumerable keys except the destructured
                        // siblings â€” Getting each (CopyDataProperties), so a getter's
                        // VALUE is copied (not the accessor) and a throw propagates.
                        let keys: Vec<String> = if s.is_heap() {
                            match self.heap.get(s.heap_index()) {
                                HeapObj::Object(map) => spec_key_order(&map.keys)
                                    .into_iter()
                                    .filter(|&i| map.attrs[i].enumerable)
                                    .map(|i| map.keys[i].clone())
                                    .filter(|k| !excluded.iter().any(|e| e == k))
                                    .collect(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut m = ObjMap::new();
                        for k in keys {
                            let v = self.get_prop(s, &k)?;
                            m.set(&k, v);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectRestDyn { dst, src, keys_base, n } => {
                        let s = self.get(base, src);
                        // Resolve the excluded sibling keys (ToPropertyKey) from regs.
                        let mut excluded: Vec<String> = Vec::with_capacity(n as usize);
                        for i in 0..n {
                            let kv = self.get(base, keys_base + i);
                            excluded.push(self.to_property_key(kv)?);
                        }
                        let keys: Vec<String> = if s.is_heap() {
                            match self.heap.get(s.heap_index()) {
                                HeapObj::Object(map) => spec_key_order(&map.keys)
                                    .into_iter()
                                    .filter(|&i| map.attrs[i].enumerable)
                                    .map(|i| map.keys[i].clone())
                                    .filter(|k| !excluded.iter().any(|e| e == k))
                                    .collect(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut m = ObjMap::new();
                        for k in keys {
                            let v = self.get_prop(s, &k)?;
                            m.set(&k, v);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClass { dst, class_id, parent } => {
                        let cd = self.class_def(class_id as usize).clone();
                        // A STATIC member named "prototype" is a TypeError at class
                        // definition (a literal `static prototype` is an early
                        // SyntaxError; this catches the constant-computed
                        // `static ['prototype'](){}` / `get`/`set`, which fold to a
                        // named static. The dynamic `static [expr]` form is caught in
                        // ClassAddMember.)
                        if cd.statics.iter().any(|(n, _)| n == "prototype")
                            || cd.static_getters.iter().any(|(n, _)| n == "prototype")
                            || cd.static_setters.iter().any(|(n, _)| n == "prototype")
                        {
                            return Err(Thrown(
                                "TypeError: Classes may not have a static property named 'prototype'"
                                    .into(),
                            ));
                        }
                        // `extends superclass`: the superclass must be `null` (proto
                        // parent null) or a constructor â€” anything else (a plain
                        // object, a number, â€¦) is a TypeError per ClassDefinition-
                        // Evaluation, thrown here at class creation.
                        let mut extends_null = false;
                        let parent_idx = match parent {
                            Some(p) => {
                                let pv = self.get(base, p);
                                // Symbol/BigInt HAVE a [[Construct]] (it throws on a
                                // `super()` call) so they ARE valid extends values
                                // even though `new Symbol()` throws â€” IsConstructor is
                                // true for them, unlike e.g. `parseInt`.
                                let ctor_like = self.is_constructor(pv)
                                    || (pv.is_heap()
                                        && (pv.heap_index() == self.symbol_ctor
                                            || pv.heap_index() == self.bigint_ctor));
                                if pv == Value::NULL {
                                    extends_null = true;
                                    None
                                } else if !ctor_like {
                                    return Err(Thrown(
                                        "TypeError: Class extends value is not a constructor or null"
                                            .into(),
                                    ));
                                } else {
                                    // protoParent = Get(superclass, "prototype") â€” runs
                                    // an accessor `prototype` (exactly once) and must be
                                    // an Object or null; anything else (a bound
                                    // function's absent prototype, a getter returning a
                                    // primitive, a setter-only `prototype`) is a
                                    // TypeError per ClassDefinitionEvaluation.
                                    let proto_parent = self.get_prop(pv, "prototype")?;
                                    if proto_parent != Value::NULL
                                        && !self.is_object_value(proto_parent)
                                    {
                                        return Err(Thrown(
                                            "TypeError: Class extends value does not have valid prototype property"
                                                .into(),
                                        ));
                                    }
                                    Some(pv.heap_index())
                                }
                            }
                            None => None,
                        };
                        // Materialize each method as a callable value once
                        // (instances share these): a plain Func, or a Closure over
                        // this frame when the method closes over an enclosing local.
                        let materialize =
                            |vm: &mut Self, defs: &[(String, u32)]| -> Vec<(String, Value)> {
                                defs.iter()
                                    .map(|(n, fid)| {
                                        (n.clone(), vm.materialize_callable(*fid, base, cur_closure))
                                    })
                                    .collect()
                            };
                        let methods = materialize(self, &cd.methods);
                        let getters = materialize(self, &cd.getters);
                        let setters = materialize(self, &cd.setters);
                        let static_getters = materialize(self, &cd.static_getters);
                        let static_setters = materialize(self, &cd.static_setters);
                        // Mint a fresh per-evaluation private brand + build the ORDERED
                        // lexical brand CHAIN: this class's own brand first, then every
                        // brand of the class body minting it (the running frame), so a
                        // private access threaded with lexical DEPTH d resolves
                        // chain[d] = the specific declaring class's brand.
                        let private_brand = self.next_private_brand;
                        self.next_private_brand += 1;
                        let enclosing = self.current_private_brands().cloned();
                        let mut lex_brands = vec![private_brand];
                        if let Some(e) = enclosing {
                            lex_brands.extend(e);
                        }
                        // Record the private NAMES this class declares, keyed by its
                        // brand, so a private access can resolve "#x" to the SPECIFIC
                        // declaring class in the lexical chain (precise, shadow-aware)
                        // instead of accepting any brand in the chain. Instance field
                        // names come from cd.instance_field_names; methods/accessors
                        // carry the "#" prefix already.
                        // KIND bits per declared private name: 1 = method,
                        // 2 = getter, 4 = setter (same-name get/set pairs merge),
                        // 0 = field — drives kind-aware private access.
                        let mut declared: Vec<(String, u8)> = Vec::new();
                        fn add_declared(declared: &mut Vec<(String, u8)>, n: &str, k: u8) {
                            if !n.starts_with('#') {
                                return;
                            }
                            if let Some(e) = declared.iter_mut().find(|(dn, _)| dn == n) {
                                e.1 |= k;
                            } else {
                                declared.push((n.to_string(), k));
                            }
                        }
                        for (n, _) in &cd.methods {
                            add_declared(&mut declared, n, 1);
                        }
                        for (n, _) in &cd.getters {
                            add_declared(&mut declared, n, 2);
                        }
                        for (n, _) in &cd.setters {
                            add_declared(&mut declared, n, 4);
                        }
                        // STATIC members carry bit 8: their brand lives on the
                        // class VALUE itself, not on constructed instances.
                        for (n, _) in &cd.statics {
                            add_declared(&mut declared, n, 1 | 8);
                        }
                        for (n, _) in &cd.static_getters {
                            add_declared(&mut declared, n, 2 | 8);
                        }
                        for (n, _) in &cd.static_setters {
                            add_declared(&mut declared, n, 4 | 8);
                        }
                        for n in &cd.instance_field_names {
                            add_declared(&mut declared, n, 0);
                        }
                        for n in &cd.static_field_names {
                            add_declared(&mut declared, n, 8);
                        }
                        if !declared.is_empty() {
                            self.brand_private_names.insert(private_brand, declared);
                        }
                        // EVERY class-body callable carries the lexical brand chain —
                        // including STATIC methods and static accessors (a static
                        // method's private access must resolve THIS evaluation's
                        // brand, so an instance of another evaluation of the same
                        // source fails its brand check with TypeError).
                        for (_, mv) in methods
                            .iter()
                            .chain(getters.iter())
                            .chain(setters.iter())
                            .chain(static_getters.iter())
                            .chain(static_setters.iter())
                        {
                            if mv.is_heap() {
                                self.method_brand.insert(mv.heap_index(), lex_brands.clone());
                            }
                        }
                        let mut statics = ObjMap::new();
                        // Static methods are non-enumerable (writable + configurable),
                        // like instance methods. Static *fields* are added later via
                        // SetProp and stay enumerable, as ES requires.
                        let method_attr = PropAttr {
                            writable: true,
                            enumerable: false,
                            configurable: true,
                            accessor: false,
                            setter: Value::UNDEFINED,
                        };
                        for (n, fid) in &cd.statics {
                            let fv = self.materialize_callable(*fid, base, cur_closure);
                            if fv.is_heap() {
                                self.method_brand.insert(fv.heap_index(), lex_brands.clone());
                            }
                            statics.define(n, fv, method_attr);
                        }
                        // The constructor (incl. field initializers) captures its
                        // upvalues from this frame now; `new` supplies them later.
                        let ctor_upvalues = match cd.ctor {
                            Some(fid) => {
                                let sources = self.func(fid as usize).upvalues.clone();
                                self.capture_upvalue_cells(&sources, base, cur_closure)
                            }
                            None => Vec::new(),
                        };
                        // The deferred fields thunk (derived + explicit ctor)
                        // captures the same defining frame.
                        let field_thunk_upvalues = match cd.field_thunk {
                            Some(fid) => {
                                let sources = self.func(fid as usize).upvalues.clone();
                                self.capture_upvalue_cells(&sources, base, cur_closure)
                            }
                            None => Vec::new(),
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Class(Box::new(ClassData {
                            name: cd.name,
                            ctor: cd.ctor,
                            has_explicit_ctor: cd.has_explicit_ctor,
                            methods,
                            getters,
                            setters,
                            statics,
                            static_getters,
                            static_setters,
                            parent: parent_idx,
                            extends_null,
                            computed_field_keys: Vec::new(),
                            source: cd.source,
                            ctor_upvalues,
                            field_thunk: cd.field_thunk,
                            field_thunk_upvalues,
                            private_brand,
                        }))));
                        self.brand_owner.insert(private_brand, v.heap_index());
                        // Remember it so `super` in a derived class can reach it.
                        self.class_values[class_id as usize] = Some(v);
                        // The class value itself carries the lexical brand chain so the
                        // ctor / field initializers / static blocks (frame.callee = the
                        // class value) resolve the same brands.
                        self.method_brand.insert(v.heap_index(), lex_brands);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ClassAddMember { class, key, func, kind } => {
                        let cv = self.get(base, class);
                        // ToPropertyKey the computed key the SAME way get_index/set_index
                        // do (ToPrimitive string-hint, Symbols kept, real ToString for a
                        // function/object) â€” `display` produced a debug string (e.g.
                        // "function" for a function-expression key), so the member was
                        // stored under a key the access could never recompute.
                        let kraw = self.get(base, key);
                        let k = self.coerce_index_key(kraw)?;
                        let kstr = self.key_of(k);
                        // A STATIC element (method/getter/setter) whose computed key is
                        // "prototype" is a TypeError at class definition (a literal
                        // `static prototype(){}` is an early SyntaxError caught by the
                        // parser; this guards the computed form `static ['prototype']`).
                        if matches!(kind, 3 | 4 | 5) && kstr == "prototype" {
                            return Err(Thrown(
                                "TypeError: Classes may not have a static property named 'prototype'"
                                    .into(),
                            ));
                        }
                        let fv = self.materialize_callable(func, base, cur_closure);
                        // SetFunctionName from the evaluated key (NamedEvaluation):
                        // the compile-time proto carried only a "<class>.[computed]"
                        // placeholder. A Symbol key â†’ "[description]" (or "" when it
                        // has none); a getter/setter gets the "get "/"set " prefix.
                        let name_prefix = match kind {
                            1 | 4 => 1, // getter / static getter
                            2 | 5 => 2, // setter / static setter
                            _ => 0,     // (static) method
                        };
                        self.set_fn_name_from_key(fv, k, name_prefix);
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            if kind == 3 {
                                // Static method â€” non-enumerable (like a named one).
                                let attr = PropAttr {
                                    writable: true,
                                    enumerable: false,
                                    configurable: true,
                                    accessor: false,
                                    setter: Value::UNDEFINED,
                                };
                                c.statics.define(&kstr, fv, attr);
                            } else {
                                // kind: 1=getter 2=setter 4=static getter 5=static
                                // setter, else instance method.
                                let list = match kind {
                                    1 => &mut c.getters,
                                    2 => &mut c.setters,
                                    4 => &mut c.static_getters,
                                    5 => &mut c.static_setters,
                                    _ => &mut c.methods,
                                };
                                // Replace a same-key member, else append.
                                if let Some(slot) = list.iter_mut().find(|(n, _)| *n == kstr) {
                                    slot.1 = fv;
                                } else {
                                    list.push((kstr, fv));
                                }
                            }
                        }
                        ip += 1;
                    }
                    Instr::New { dst, callee, arg_base, argc } => {
                        let cv = self.get(base, callee);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let result = self.construct(cv, &args)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::NewSpread { dst, callee, args } => {
                        let cv = self.get(base, callee);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let result = self.construct(cv, &arg_vec)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::PushFieldKey { class, key } => {
                        let cv = self.get(base, class);
                        let kv = self.get(base, key);
                        // ToPropertyKey at class-DEFINITION time (spec: a ClassElement-
                        // Name is evaluated once when the class is defined). Running the
                        // key's toString/@@toPrimitive now means a throwing or
                        // non-callable @@toPrimitive surfaces at definition (not at the
                        // first `new`), and the resolved key is reused per instance.
                        let k = self.coerce_index_key(kv)?;
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            c.computed_field_keys.push(k);
                        }
                        ip += 1;
                    }
                    Instr::FieldInit { key_index, val } => {
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        // The computed key was evaluated once at class definition and
                        // stored on this instance's class.
                        let key = match self.heap.get(this.heap_index()) {
                            HeapObj::Object(m) => m.class.and_then(|cidx| {
                                match self.heap.get(cidx) {
                                    HeapObj::Class(c) => c.computed_field_keys.get(key_index as usize).copied(),
                                    _ => None,
                                }
                            }),
                            _ => None,
                        };
                        if let Some(key) = key {
                            // CreateDataPropertyOrThrow (an own define, prototype
                            // setters never consulted; Proxy defineProperty fires).
                            let ks = self.key_of(key);
                            self.define_field(this, &ks, v)?;
                        }
                        ip += 1;
                    }
                    Instr::ThisCheck { src } => {
                        let v = self.get(base, src);
                        if v.is_heap() && self.this_tdz.contains(&v.heap_index()) {
                            return Err(Thrown(
                                "ReferenceError: must call super constructor before accessing 'this' in a derived class constructor".into(),
                            ));
                        }
                        ip += 1;
                    }
                    Instr::SuperCtor { home_class_id, arg_base, argc } => {
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: superclass is not a constructor".into()))?;
                        let this = self.get(base, 0);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        // `super(...)` keeps the derived activation's new.target.
                        let nt = self.frames.last().map(|f| f.new_target).unwrap_or(Value::UNDEFINED);
                        // super() PRODUCES `this`; completion enforces the once-only
                        // rule, rebinds reg 0 (return-override), lifts the this-TDZ
                        // and runs this class's deferred field initializers.
                        let produced = self.run_class_ctor(parent, this, &args, nt)?;
                        self.super_ctor_complete(base, this, produced, home_class_id)?;
                        ip += 1;
                    }
                    Instr::SuperCtorSpread { home_class_id, args } => {
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: superclass is not a constructor".into()))?;
                        let this = self.get(base, 0);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let nt = self.frames.last().map(|f| f.new_target).unwrap_or(Value::UNDEFINED);
                        let produced = self.run_class_ctor(parent, this, &arg_vec, nt)?;
                        self.super_ctor_complete(base, this, produced, home_class_id)?;
                        ip += 1;
                    }
                    Instr::SuperMethod { dst, home_class_id, name, arg_base, argc } => {
                        // `func()` returns `&'p`, so the interned name key outlives
                        // any `&mut self` below â€” and resolves eval functions too.
                        let key: &'p str =
                            &self.func(func_id as usize).string_constants[name as usize];
                        // super.m() resolves m via the super base (the home object's
                        // [[Prototype]]) with `this` = the receiver â€” like a normal
                        // property get + call (and like SuperMethodComputed). This
                        // reaches inherited methods, accessors, and base-class super
                        // (â†’ %Object.prototype%), not just own parent-class methods.
                        let proto = self.super_base(home_class_id, self.func(func_id as usize).super_static);
                        // MakeSuperPropertyReference: RequireObjectCoercible(base).
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let m = self.get_member(proto, key, this)?;
                        if !self.is_callable(m) {
                            return Err(Thrown(format!("TypeError: super.{key} is not a function")));
                        }
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperMethodSpread { dst, home_class_id, name, args } => {
                        // `super.name(...args)` â€” like SuperMethod but the arguments come
                        // from a spread array; `this` = the current receiver.
                        let key: &'p str =
                            &self.func(func_id as usize).string_constants[name as usize];
                        let proto = self.super_base(home_class_id, self.func(func_id as usize).super_static);
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let m = self.get_member(proto, key, this)?;
                        if !self.is_callable(m) {
                            return Err(Thrown(format!("TypeError: super.{key} is not a function")));
                        }
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let r = self.call_value(m, this, &arg_vec)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperGet { dst, home_class_id, name } => {
                        // `super.name` read: resolve on the super base (the home
                        // object's [[Prototype]]) with `this` = the current receiver
                        // (so a getter sees it). For a base class the base is
                        // %Object.prototype%.
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].clone();
                        let proto = self.super_base(home_class_id, self.func(func_id as usize).super_static);
                        // MakeSuperPropertyReference: RequireObjectCoercible(base).
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &key, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperGetComputed { dst, home_class_id, key } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let proto = self.super_base(home_class_id, self.func(func_id as usize).super_static);
                        // MakeSuperPropertyReference: RequireObjectCoercible(base).
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &ks, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperMethodComputed { dst, home_class_id, key, arg_base, argc } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let proto = self.super_base(home_class_id, self.func(func_id as usize).super_static);
                        // MakeSuperPropertyReference: RequireObjectCoercible(base).
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let m = self.get_member(proto, &ks, this)?;
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperSet { home_class_id, name, val } => {
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].clone();
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        let is_static = self.func(func_id as usize).super_static;
                        self.super_set(home_class_id, &key, this, v, is_static)?;
                        ip += 1;
                    }
                    Instr::SuperSetComputed { home_class_id, key, val } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        let is_static = self.func(func_id as usize).super_static;
                        self.super_set(home_class_id, &ks, this, v, is_static)?;
                        ip += 1;
                    }
                    Instr::SetHomeObject { method, home } => {
                        let m = self.get(base, method);
                        if m.is_heap() {
                            let h = self.get(base, home);
                            self.closure_home.insert(m.heap_index(), h);
                        }
                        ip += 1;
                    }
                    Instr::SuperGetObj { dst, name } => {
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].clone();
                        let proto = self.obj_super_base(self.frames[frame_idx].callee);
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &key, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperGetObjComputed { dst, key } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let proto = self.obj_super_base(self.frames[frame_idx].callee);
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &ks, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperSetObj { name, val } => {
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].clone();
                        let proto = self.obj_super_base(self.frames[frame_idx].callee);
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        self.super_set_obj(proto, &key, this, v)?;
                        ip += 1;
                    }
                    Instr::SuperSetObjComputed { key, val } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let proto = self.obj_super_base(self.frames[frame_idx].callee);
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        self.super_set_obj(proto, &ks, this, v)?;
                        ip += 1;
                    }
                    Instr::SuperMethodObj { dst, name, arg_base, argc } => {
                        let key =
                            self.func(func_id as usize).string_constants[name as usize].clone();
                        let proto = self.obj_super_base(self.frames[frame_idx].callee);
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let m = self.get_member(proto, &key, this)?;
                        if !self.is_callable(m) {
                            return Err(Thrown(format!("TypeError: super.{key} is not a function")));
                        }
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperMethodObjComputed { dst, key, arg_base, argc } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let proto = self.obj_super_base(self.frames[frame_idx].callee);
                        self.require_object_coercible(proto)?;
                        let this = self.get(base, 0);
                        let m = self.get_member(proto, &ks, this)?;
                        if !self.is_callable(m) {
                            return Err(Thrown(format!("TypeError: super[{ks}] is not a function")));
                        }
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ArrayCtor { dst, arg_base, argc } => {
                        let arr = if argc == 1 && self.get(base, arg_base).is_number() {
                            // `Array(n)` â†’ n HOLES (absent elements), not n undefineds.
                            let n = self.get(base, arg_base).as_f64();
                            if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                                return Err(Thrown("RangeError: Invalid array length".into()));
                            }
                            if n as usize > super::MAX_DENSE_ARRAY_LEN {
                                return Err(Thrown(
                                    "RangeError: array length exceeds the engine's dense-array limit".into(),
                                ));
                            }
                            vec![Value::HOLE; n as usize]
                        } else {
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect()
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewMap { dst, src } => {
                        // Entries are added through the `set` adder resolved off the
                        // new map, so an overridden `Map.prototype.set` is honoured
                        // (the builtin set inserts + normalizes -0 itself).
                        let m = Value::heap(self.heap.alloc(HeapObj::Map {
                            keys: Vec::new(),
                            vals: Vec::new(),
                        }));
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                let adder = self.get_member(m, "set", m)?;
                                if !self.is_callable(adder) {
                                    return Err(Thrown("TypeError: Map.prototype.set is not callable".into()));
                                }
                                // AddEntriesFromIterable: step the iterator lazily;
                                // each entry must be an Object; read k/v and call the
                                // adder; an abrupt completion runs IteratorClose.
                                let iter = self.get_iterator_object(sv)?;
                                loop {
                                    let e = match self.iterator_step(iter)? {
                                        Some(v) => v,
                                        None => break,
                                    };
                                    if !self.is_object_value(e) {
                                        let _ = self.iterator_close(iter);
                                        return Err(Thrown(
                                            "TypeError: Map iterable entry is not an object".into(),
                                        ));
                                    }
                                    let k = match self.get_index(e, Value::int(0)) {
                                        Ok(k) => k,
                                        Err(err) => { let _ = self.iterator_close(iter); return Err(err); }
                                    };
                                    let v = match self.get_index(e, Value::int(1)) {
                                        Ok(v) => v,
                                        Err(err) => { let _ = self.iterator_close(iter); return Err(err); }
                                    };
                                    if let Err(err) = self.call_value(adder, m, &[k, v]) {
                                        let _ = self.iterator_close(iter);
                                        return Err(err);
                                    }
                                }
                            }
                        }
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewSet { dst, src } => {
                        let set_v = Value::heap(self.heap.alloc(HeapObj::Set(Vec::new())));
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                let adder = self.get_member(set_v, "add", set_v)?;
                                if !self.is_callable(adder) {
                                    return Err(Thrown("TypeError: Set.prototype.add is not callable".into()));
                                }
                                // Lazy iteration + IteratorClose on an abrupt adder.
                                let iter = self.get_iterator_object(sv)?;
                                loop {
                                    let e = match self.iterator_step(iter)? {
                                        Some(v) => v,
                                        None => break,
                                    };
                                    if let Err(err) = self.call_value(adder, set_v, &[e]) {
                                        let _ = self.iterator_close(iter);
                                        return Err(err);
                                    }
                                }
                            }
                        }
                        self.set(base, dst, set_v);
                        ip += 1;
                    }
                    Instr::NewWeakMap { dst, src } => {
                        // Build empty, then AddEntriesFromIterable via the observable
                        // `set` adder (so non-registered symbol keys validate via
                        // CanBeHeldWeakly, the adder is observably called, and an
                        // abrupt closes the iterator).
                        let wm = Value::heap(
                            self.heap.alloc(HeapObj::WeakMap { keys: Vec::new(), vals: Vec::new() }),
                        );
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                self.add_entries_via_adder(wm, sv, true)?;
                            }
                        }
                        self.set(base, dst, wm);
                        ip += 1;
                    }
                    Instr::NewWeakSet { dst, src } => {
                        let ws = Value::heap(self.heap.alloc(HeapObj::WeakSet(Vec::new())));
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                self.add_entries_via_adder(ws, sv, false)?;
                            }
                        }
                        self.set(base, dst, ws);
                        ip += 1;
                    }
                    Instr::NewWeakRef { dst, target } => {
                        let t = self.get(base, target);
                        if !self.is_object_value(t) {
                            return Err(Thrown(
                                "TypeError: WeakRef: target must be an object".into(),
                            ));
                        }
                        let wr = Value::heap(self.heap.alloc(HeapObj::WeakRef(t)));
                        self.set(base, dst, wr);
                        ip += 1;
                    }
                    Instr::NewBox { dst, kind, arg } => {
                        let value = match kind {
                            0 => {
                                // String box: ToString(arg) (no arg -> "").
                                let s = match arg {
                                    Some(a) => self.to_js_string(self.get(base, a))?,
                                    None => String::new(),
                                };
                                self.alloc_str(s)
                            }
                            1 => {
                                // Number box: ToNumber(arg) (no arg -> +0) — observable
                                // (a user valueOf/toString runs) and abrupt; plain
                                // `to_number` would return NaN for an object.
                                let n = match arg {
                                    Some(a) => {
                                        let v = self.get(base, a);
                                        self.to_number_coerce(v)?
                                    }
                                    None => 0.0,
                                };
                                Value::num(n)
                            }
                            _ => {
                                // Boolean box: ToBoolean(arg) (no arg -> false).
                                Value::bool(arg.map(|a| self.truthy(self.get(base, a))).unwrap_or(false))
                            }
                        };
                        let b = Value::heap(self.heap.alloc(HeapObj::Boxed { kind, value }));
                        self.set(base, dst, b);
                        ip += 1;
                    }
                    Instr::NewFinalizationRegistry { dst, cleanup } => {
                        let cb = self.get(base, cleanup);
                        if self.type_of(cb) != "function" {
                            return Err(Thrown(
                                "TypeError: FinalizationRegistry: cleanup callback must be callable".into(),
                            ));
                        }
                        let fr = Value::heap(
                            self.heap.alloc(HeapObj::FinalizationRegistry { cleanup: cb, tokens: Vec::new() }),
                        );
                        self.set(base, dst, fr);
                        ip += 1;
                    }
                    Instr::NewPromise { dst, executor } => {
                        let exec = self.get(base, executor);
                        // The Promise constructor throws synchronously when the
                        // executor is not callable (spec step 2) — it does NOT
                        // produce a rejected promise.
                        if !self.is_callable(exec) {
                            return Err(Thrown(
                                "TypeError: Promise resolver is not a function".into(),
                            ));
                        }
                        let p = self.alloc_promise();
                        let res = Value::heap(
                            self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: false }),
                        );
                        let rej = Value::heap(
                            self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: true }),
                        );
                        // A throwing executor rejects the promise.
                        if self.call_value(exec, Value::UNDEFINED, &[res, rej]).is_err() {
                            let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                            self.reject(p, reason);
                        }
                        self.set(base, dst, Value::heap(p));
                        ip += 1;
                    }
                    Instr::CallSpread { dst, callee, args } => {
                        let callee_v = self.get(base, callee);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let result = self.call_value(callee_v, Value::UNDEFINED, &arg_vec)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::CallMethodSpread { dst, obj, name, args } => {
                        let recv = self.get(base, obj);
                        // `func()` returns `&'p`, so the interned name key outlives
                        // any `&mut self` below â€” and resolves eval functions too.
                        let key: &'p str =
                            &self.func(func_id as usize).string_constants[name as usize];
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        // Builtin (array/string/number) method, else a user method
                        // resolved off the receiver and called with `this = recv`.
                        let result = match self.dispatch_builtin_method(recv, key, &arg_vec)? {
                            Some(r) => r,
                            None => {
                                let prop = self.get_prop(recv, key)?;
                                self.call_value(prop, recv, &arg_vec)?
                            }
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::CallMethodComputedSpread { dst, obj, key, args } => {
                        // `obj[key](...args)` â€” bind `this` = obj (unlike CallSpread on
                        // the GET result). Builtin method first, else resolve off the
                        // receiver via the computed key and call with `this = recv`.
                        let recv = self.get(base, obj);
                        let k = self.get(base, key);
                        let kstr = self.display(k);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let result = match self.dispatch_builtin_method(recv, &kstr, &arg_vec)? {
                            Some(r) => r,
                            None => {
                                let method = self.get_index(recv, k)?;
                                self.call_value(method, recv, &arg_vec)?
                            }
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::MathOp { dst, op, arg_base, argc } => {
                        let r = self.eval_math(op, base, arg_base, argc)?;
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }
                    Instr::GlobalFn { dst, op, arg_base, argc } => {
                        use crate::bytecode::GlobalFn as G;
                        let a0 = if argc >= 1 { self.get(base, arg_base) } else { Value::UNDEFINED };
                        let v = match op {
                            G::Number => {
                                if argc == 0 { Value::num(0.0) } else { Value::num(self.to_number_coerce(a0)?) }
                            }
                            G::String => {
                                if argc == 0 {
                                    self.alloc_str(String::new())
                                } else if a0.is_heap()
                                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Symbol { .. })
                                {
                                    // `String(symbol)` is allowed (unlike ToString,
                                    // which throws) and yields "Symbol(desc)".
                                    let s = self.display(a0);
                                    self.alloc_str(s)
                                } else {
                                    // Proper ToString: routes objects/functions
                                    // through their `toString` (so a function yields
                                    // its real source via Function.prototype.toString).
                                    let s = self.to_js_string(a0)?;
                                    self.alloc_str(s)
                                }
                            }
                            G::Boolean => Value::bool(argc >= 1 && self.truthy(a0)),
                            G::ParseInt => {
                                // ToString(string) FIRST (observable toString, abrupt),
                                // then radix = ToInt32(ToNumber(radix)) (NaN/±Infinity
                                // → 0 = default base).
                                let s = self.to_js_string(a0)?;
                                let radix = if argc >= 2 {
                                    let rv = self.get(base, arg_base + 1);
                                    let r = self.to_number_coerce(rv)?;
                                    if r.is_finite() { r as i64 as i32 } else { 0 }
                                } else {
                                    0
                                };
                                Value::num(parse_int(&s, radix))
                            }
                            G::ParseFloat => {
                                let s = self.to_js_string(a0)?;
                                Value::num(parse_float(&s))
                            }
                            // isNaN/isFinite are `Number::isNaN/isFinite(? ToNumber(x))`:
                            // ToNumber coerces objects (@@toPrimitive/valueOf/toString)
                            // and propagates abrupt completions (a throwing valueOf, a
                            // Symbol arg â†’ TypeError), so route through to_number_coerce.
                            G::IsNaN => Value::bool(self.to_number_coerce(a0)?.is_nan()),
                            G::IsFinite => {
                                Value::bool(self.to_number_coerce(a0)?.is_finite())
                            }
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::InstanceOf { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let r = self.eval_instanceof(v, ctor);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::HasProp { dst, key, obj, brand } => {
                        let k = self.get(base, key);
                        let o = self.get(base, obj);
                        // The `in` operator (and `#x in`) require an Object right
                        // operand â€” a primitive RHS is a TypeError (checked before
                        // ToPropertyKey on the key, per spec order).
                        if !self.is_object_value(o) {
                            let kd = self.display(k);
                            return Err(Thrown(format!(
                                "TypeError: Cannot use 'in' operator to search for '{kd}' in a non-object"
                            )));
                        }
                        // ToPropertyKey: an object key is ToString-coerced (toString/
                        // valueOf), not rendered "[object Object]".
                        let k = self.coerce_index_key(k)?;
                        // A private name (`#x`) is not a string property key: a
                        // regular `in` (and Reflect.has) reports it absent. The
                        // ergonomic brand check `#x in obj` sets `brand` and skips
                        // this filter so it still observes the private element.
                        let r = if brand {
                            // `#x in obj` ergonomic brand check: a FIELD is
                            // present iff the side table has the entry; a
                            // method/accessor iff the receiver carries the
                            // declaring brand (textual fallback when none
                            // resolvable).
                            let key = self.key_of(k);
                            if let Some((b2, kind, owner)) = self.resolve_private(&key) {
                                if kind & 7 == 0 {
                                    self.private_field_get(o, b2, &key).is_some()
                                } else {
                                    self.private_receiver_ok(o, b2, kind, owner)
                                }
                            } else {
                                let textual = self.has_property_str(o, &key)
                                    || self.private_field_scan_has(o, &key);
                                match self.private_brand_ok(o, &key) {
                                    Some(b) => textual && b,
                                    None => textual,
                                }
                            }
                        } else {
                            // (Real private fields live in the side table, so an
                            // ordinary `in` never sees them; a PUBLIC "#..."
                            // string key is an ordinary property.)
                            // Proxy-aware [[HasProperty]]: dispatches a `has` trap
                            // when the proxy is the receiver OR is in the proto chain.
                            self.has_property_dyn(o, k)?
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::WithHas { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        // HasBindingFor a with environment: [[HasProperty]] (own or
                        // inherited), then the @@unscopables filter â€” an own/inherited
                        // `@@unscopables` object whose `key` entry is truthy hides the
                        // binding (so e.g. `with([]) { values }` reaches the outer
                        // binding, not Array.prototype.values).
                        // [[HasProperty]] must dispatch a Proxy `has` trap
                        // (and propagate its abrupt completion).
                        let kv = self.key_to_value(&key);
                        let mut found =
                            self.is_object_value(o) && self.has_property_dyn(o, kv)?;
                        if found {
                            let unsc = self.get_prop(o, "@@unscopables")?;
                            if self.is_object_value(unsc) {
                                let blocked = self.get_prop(unsc, &key)?;
                                if self.truthy(blocked) {
                                    found = false;
                                }
                            }
                        }
                        self.set(base, dst, Value::bool(found));
                        ip += 1;
                    }
                    Instr::WithGet { dst, obj, name, strict } => {
                        let o = self.get(base, obj);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        // GetBindingValue: HasProperty AGAIN (the WithHas
                        // @@unscopables getter may have deleted the binding).
                        let kv = self.key_to_value(&key);
                        if !(self.is_object_value(o) && self.has_property_dyn(o, kv)?) {
                            if strict {
                                return Err(Thrown(format!(
                                    "ReferenceError: {key} is not defined"
                                )));
                            }
                            self.set(base, dst, Value::UNDEFINED);
                        } else {
                            let v = self.get_prop(o, &key)?;
                            self.set(base, dst, v);
                        }
                        ip += 1;
                    }
                    Instr::WithSet { obj, name, val, strict } => {
                        let o = self.get(base, obj);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        let kv = self.key_to_value(&key);
                        if strict && !(self.is_object_value(o) && self.has_property_dyn(o, kv)?)
                        {
                            return Err(Thrown(format!(
                                "ReferenceError: {key} is not defined"
                            )));
                        }
                        let v = self.get(base, val);
                        self.set_prop(o, &key, v, strict)?;
                        ip += 1;
                    }
                    Instr::InstanceOfDyn { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let c = self.get(base, ctor);
                        // `Symbol.hasInstance`: if the RHS defines a callable
                        // @@hasInstance, it fully governs `instanceof` â€” invoke it
                        // with the LHS and coerce the result to boolean. (Ordinary
                        // functions/classes have no own @@hasInstance here, so they
                        // fall through to the prototype-chain check below.)
                        if c.is_heap() {
                            let hi = self.get_prop(c, "@@hasInstance")?;
                            // The built-in Function.prototype[@@hasInstance] is
                            // OrdinaryHasInstance, already implemented by the kind
                            // dispatch below (which also handles classes / built-in
                            // constructors); skip it so only a USER-overridden
                            // @@hasInstance intercepts here.
                            let is_builtin = hi.is_heap()
                                && matches!(self.heap.get(hi.heap_index()),
                                    HeapObj::Native(n) if *n == native::FN_HAS_INSTANCE);
                            if self.is_callable(hi) && !is_builtin {
                                let res = self.call_value(hi, c, &[v])?;
                                let b = self.truthy(res);
                                self.set(base, dst, Value::bool(b));
                                ip += 1;
                                continue;
                            }
                        }
                        // A class uses its `extends` chain; a constructor FUNCTION
                        // checks whether `F.prototype` is in `v`'s prototype chain.
                        // Any other CALLABLE right operand (%Function.prototype%, a
                        // native function, â€¦) still uses OrdinaryHasInstance.
                        // `Symbol`/`BigInt` are callable globals (typeof "function")
                        // but not constructors (is_ctor false), so `is_callable` skips
                        // them; for `instanceof` they ARE valid right operands â€”
                        // OrdinaryHasInstance reads their .prototype and yields false
                        // for a non-wrapper LHS (e.g. `x instanceof Symbol`), never a
                        // "not callable" TypeError. (deepEqual.js guards these with
                        // `typeof X === 'function'`, then does `v instanceof X`.)
                        let c_callable = c.is_heap()
                            && (self.is_callable(c)
                                || (self.symbol_ctor != 0 && c.heap_index() == self.symbol_ctor)
                                || (self.bigint_ctor != 0 && c.heap_index() == self.bigint_ctor));
                        let kind = if c.is_heap() {
                            match self.heap.get(c.heap_index()) {
                                HeapObj::Class(_) => 1u8,
                                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } => 2,
                                // Built-in constructor globals (Map/Set/Date/WeakMap/â€¦)
                                // are objects but constructable: use prototype-chain check.
                                HeapObj::Object(m) if m.is_ctor => 2,
                                _ if c_callable => 3,
                                _ => 0,
                            }
                        } else {
                            0
                        };
                        let r = match kind {
                            // A class instance: the fast map.class-lineage check, then
                            // the spec prototype-chain check (which also covers a
                            // subclass-of-builtin instance re-branded to the builtin
                            // variant, whose map.class link is gone).
                            1 => {
                                v.is_heap()
                                    && (self.instance_of_class(v, c.heap_index())
                                        || self.instanceof_via_proto(v, c))
                            }
                            2 => self.instanceof_via_proto(v, c),
                            // A plain callable RHS: spec OrdinaryHasInstance â€” reads
                            // `C.prototype` via [[Get]] (a getter runs; a non-object
                            // prototype throws), returns false for a primitive LHS.
                            3 => self.ordinary_has_instance(c, v)?,
                            // RHS is neither callable nor has @@hasInstance: TypeError
                            // (`x instanceof {}`, `x instanceof 5`, `x instanceof null`).
                            _ => {
                                return Err(Thrown(
                                    "TypeError: Right-hand side of 'instanceof' is not callable".into(),
                                ))
                            }
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::StaticFn { dst, op, arg_base, argc } => {
                        use crate::bytecode::StaticFn as S;
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let v = match op {
                            S::ArrayOf => Value::heap(self.heap.alloc(HeapObj::Array(args))),
                            S::NumberIsInteger => Value::bool(num_is_integer(a0)),
                            S::NumberIsNaN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
                            S::NumberIsFinite => Value::bool(num_is_finite(a0)),
                            S::NumberIsSafeInteger => Value::bool(num_is_safe_integer(a0)),
                            S::StringFromCharCode => {
                                // ToUint16(ToNumber(v)) per arg â€” strict ToNumber
                                // (ToPrimitive-aware, BigInt/Symbol â†’ TypeError, a
                                // throwing valueOf propagates rather than being
                                // swallowed to 0).
                                let mut s = String::new();
                                for &v in &args {
                                    let u = to_uint32(self.to_number_strict(v)?) as u16;
                                    s.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
                                }
                                self.alloc_str(s)
                            }
                            S::ObjectAssign => self.object_assign(&args)?,
                            S::ObjectFromEntries => self.object_from_entries(a0)?,
                            S::PromiseResolve => {
                                // Promise.resolve(p) of an existing Promise is identity.
                                if a0.is_heap()
                                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Promise { .. })
                                {
                                    a0
                                } else {
                                    let p = self.alloc_promise();
                                    self.resolve(p, a0);
                                    Value::heap(p)
                                }
                            }
                            S::PromiseReject => {
                                let p = self.alloc_promise();
                                self.reject(p, a0);
                                Value::heap(p)
                            }
                            S::PromiseAll => {
                                let c = self.promise_ctor_value();
                                self.promise_combine(crate::heap::CombKind::All, a0, c)?
                            }
                            S::PromiseAllSettled => {
                                let c = self.promise_ctor_value();
                                self.promise_combine(crate::heap::CombKind::AllSettled, a0, c)?
                            }
                            S::PromiseRace => {
                                let c = self.promise_ctor_value();
                                self.promise_combine(crate::heap::CombKind::Race, a0, c)?
                            }
                            S::PromiseAny => {
                                let c = self.promise_ctor_value();
                                self.promise_combine(crate::heap::CombKind::Any, a0, c)?
                            }
                            S::ObjectDefineProperty => {
                                self.require_object_coercible(a0)?; // Type(O) must be Object
                                let key =
                                    self.to_property_key(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_property(a0, &key, desc)?;
                                a0
                            }
                            S::ObjectDefineProperties => {
                                if !self.is_object_value(a0) {
                                    return Err(Thrown(
                                        "TypeError: Object.defineProperties called on non-object".into(),
                                    ));
                                }
                                let props = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_properties(a0, props)?;
                                a0
                            }
                            S::ObjectGetOwnPropertyDescriptor => {
                                self.require_object_coercible(a0)?; // ToObject(O)
                                let o = self.to_object(a0)?;
                                let key =
                                    self.to_property_key(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                                self.ns_tdz_check(o, &key)?; // uninit export throws
                                match self.proxy_gopd(o, &key)? {
                                    Some(d) => d,
                                    None => self.object_get_own_property_descriptor(o, &key),
                                }
                            }
                            S::ObjectGetOwnPropertyNames => {
                                self.require_object_coercible(a0)?; // ToObject(O)
                                let o = self.to_object(a0)?;
                                self.object_own_property_names(o)?
                            }
                            S::ObjectGetPrototypeOf => {
                                self.require_object_coercible(a0)?; // ToObject(O)
                                let o = self.to_object(a0)?;
                                self.get_prototype_of_checked(o)?
                            }
                            S::ObjectCreate => {
                                if a0 != Value::NULL && !self.is_object_value(a0) {
                                    return Err(Thrown(
                                        "TypeError: Object prototype may only be an Object or null".into(),
                                    ));
                                }
                                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                                if a0 != Value::UNDEFINED {
                                    self.proto_of.insert(o.heap_index(), a0);
                                }
                                if let Some(props) = args.get(1).copied() {
                                    if props != Value::UNDEFINED {
                                        self.object_define_properties(o, props)?;
                                    }
                                }
                                o
                            }
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayFrom { dst, src, mapfn } => {
                        let sv = self.get(base, src);
                        let fnv = self.get(base, mapfn);
                        let out = self.array_from(Value::UNDEFINED, sv, fnv, Value::UNDEFINED)?;
                        self.set(base, dst, out);
                        ip += 1;
                    }
                    Instr::MathSpread { dst, op, args } => {
                        use crate::bytecode::MathFn as M;
                        let av = self.get(base, args);
                        let elems = self.array_snapshot(av.heap_index());
                        let nums: Vec<f64> =
                            elems.iter().map(|&v| self.to_number(v)).collect::<Result<_, _>>()?;
                        let r = match op {
                            M::Max => nums.iter().fold(f64::NEG_INFINITY, |a, &b| {
                                if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
                            }),
                            M::Min => nums.iter().fold(f64::INFINITY, |a, &b| {
                                if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
                            }),
                            M::Hypot => nums.iter().map(|&v| v * v).sum::<f64>().sqrt(),
                            // A non-variadic Math fn spread is unusual; apply to elem 0.
                            _ => self.eval_math_one(op, nums.first().copied().unwrap_or(f64::NAN)),
                        };
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }

                    Instr::Jump { target } => {
                        let t = target as usize;
                        // A backward jump is a loop back-edge â€” poll the GC here so
                        // a tight allocating loop (which never leaves this inner
                        // loop) still gets collected. Safe: all live Values are in
                        // regs, and gc_lock guards any native built-in up-stack.
                        if t < ip {
                            self.maybe_gc();
                        }
                        // â”€â”€ OSR tier â”€â”€ a backward jump is a loop back-edge. After
                        // the region heats up, compile `[target, ip]` (the loop
                        // body, headed at `target`) and run it natively; the
                        // native code returns the ip to resume at (a clean loop
                        // exit or a guard bail). Gated like the function JIT:
                        // enabled, and not inside a native self-recursion.
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        if self.jit_enabled
                            && self.jit_recurse_depth == 0
                            && (func_id as usize) < self.main_func_count
                            && t < ip
                        {
                            if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                ip = resume;
                                continue;
                            }
                            // A global op whose slot is still UNINITIALIZED may
                            // be own-prop-backed (eval-created / `this.x` bindings):
                            // raw JIT slot accesses would bypass the own property.
                            // Don't record/compile yet — once the slot holds a real
                            // value it can never go back, so a later attempt is safe.
                            let region_globals_ok = {
                                let proto = self.func(func_id as usize);
                                let s = t as usize;
                                let e = (ip as usize).min(proto.code.len() - 1);
                                proto.code[s..=e].iter().all(|ins| {
                                    let slot = match *ins {
                                        Instr::LoadGlobal { idx, .. } => Some(idx),
                                        Instr::LoadGlobalOrUndefined { idx, .. } => Some(idx),
                                        Instr::StoreGlobal { idx, .. } => Some(idx),
                                        Instr::StoreGlobalStrict { idx, .. } => Some(idx),
                                        _ => None,
                                    };
                                    slot.map_or(true, |i| !self.globals[i as usize].is_uninitialized())
                                })
                            };
                            if region_globals_ok && self.jit.record_region(func_id, t as u32) {
                                let proto: *const crate::bytecode::FuncProto =
                                    self.func(func_id as usize);
                                // SAFETY: program functions are immutable during run.
                                let proto_ref = unsafe { &*proto };
                                self.jit.compile_region(
                                    func_id,
                                    proto_ref,
                                    t as u32,
                                    ip as u32,
                                    jit_globals_base as usize,
                                    crate::codegen::HeapHelperAddrs {
                                        get_prop_miss: jit_get_prop_miss as usize,
                                        set_prop_miss: jit_set_prop_miss as usize,
                                        versions_base: jit_heap_versions_base as usize,
                                        ic_base: jit_ic_base as usize,
                                        get_index: jit_get_index as usize,
                                        set_index: jit_set_index as usize,
                                        array_push: jit_array_push as usize,
                                        char_code_at: jit_char_code_at as usize,
                                        concat: jit_concat as usize,
                                        str_append: jit_str_append as usize,
                                    },
                                    self.program.global_count, // field-global pool base
                                    FIELD_POOL as u32,
                                );
                                if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                    ip = resume;
                                    continue;
                                }
                            }
                        }
                        ip = t;
                    }
                    Instr::JumpIfFalse { cond, target } => {
                        let v = self.get(base, cond);
                        if !self.truthy(v) {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfTrue { cond, target } => {
                        let v = self.get(base, cond);
                        if self.truthy(v) {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLt { a, b, target } => {
                        let r = self.cmp_lt(base, a, b, true)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLe { a, b, target } => {
                        let r = self.cmp_le(base, a, b, true)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }

                    Instr::Print { arg_base, argc, to_stderr } => {
                        let mut parts = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            let v = self.get(base, arg_base + i);
                            parts.push(self.inspect(v));
                        }
                        let line = parts.join(" ");
                        if to_stderr {
                            self.errput.push(line);
                        } else {
                            self.output.push(line);
                        }
                        ip += 1;
                    }

                    Instr::MakeFunc { dst, func_id } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Func(func_id)));
                        // A function created under a dynamic EvalScope keeps
                        // resolving its bindings (Dyn global ops consult it).
                        if let Some(sc) = self.ensure_frame_eval_scope(frame_idx) {
                            self.closure_eval_scope.insert(v.heap_index(), sc);
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectKeys { dst, obj } => {
                        let o = self.get(base, obj);
                        self.require_object_coercible(o)?; // ToObject(O)
                        let o = self.to_object(o)?;
                        let v = self.object_enum_own(o, EnumWhat::Keys)?;
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ForInKeys { dst, obj } => {
                        let o = self.get(base, obj);
                        // ForIn/OfHeadEvaluation: a null/undefined receiver iterates
                        // nothing (no ToObject error), so yield an empty key list.
                        let v = if o.is_nullish() {
                            Value::heap(self.heap.alloc(HeapObj::Array(Vec::new())))
                        } else {
                            let o = self.to_object(o)?;
                            self.for_in_keys(o)?
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectValues { dst, obj } => {
                        let o = self.get(base, obj);
                        self.require_object_coercible(o)?;
                        let o = self.to_object(o)?;
                        let v = self.object_enum_own(o, EnumWhat::Values)?;
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectEntries { dst, obj } => {
                        let o = self.get(base, obj);
                        self.require_object_coercible(o)?;
                        let o = self.to_object(o)?;
                        let v = self.object_enum_own(o, EnumWhat::Entries)?;
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LenOf { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Array(items) => len_value(items.len()),
                                HeapObj::Str(s) => len_value(s.char_len),
                                HeapObj::Cons { len, .. } => len_value(*len),
                                // for-of over a Map/Set iterates `size` slots (a
                                // tombstoned/deleted entry doesn't count).
                                HeapObj::Map { keys, .. } => {
                                    len_value(keys.iter().filter(|k| !k.is_hole()).count())
                                }
                                HeapObj::Set(items) => {
                                    len_value(items.iter().filter(|v| !v.is_hole()).count())
                                }
                                _ => Value::int(0),
                            }
                        } else {
                            Value::int(0)
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClosure { dst, func_id } => {
                        // Capture each upvalue's cell index, resolved in THIS
                        // (defining) frame: a ParentLocal source reads the cell
                        // index from a local register (the local was boxed via
                        // MakeCell); a ParentUpval source forwards one of this
                        // frame's own captured cells.
                        let sources = &self.func(func_id as usize).upvalues;
                        let mut cells = Vec::with_capacity(sources.len());
                        for src in sources {
                            let cell = match *src {
                                UpvalSource::ParentLocal(reg) => {
                                    self.get(base, reg).heap_index()
                                }
                                UpvalSource::ParentUpval(idx) => {
                                    self.closure_upvalue(cur_closure, idx)
                                }
                            };
                            cells.push(cell);
                        }
                        let v = Value::heap(
                            self.heap.alloc(HeapObj::Closure { func: func_id, upvalues: cells, this_val: Value::UNDEFINED }),
                        );
                        if let Some(sc) = self.ensure_frame_eval_scope(frame_idx) {
                            self.closure_eval_scope.insert(v.heap_index(), sc);
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeArrow { dst, func_id, this_reg } => {
                        // Like MakeClosure, but the resulting closure also captures the
                        // defining frame's effective `this` (register `this_reg` =
                        // `this_override.unwrap_or(0)` at the definition site â€” usually
                        // reg 0, but the class value inside a static field initializer)
                        // so a later call binds it lexically (FuncProto::lexical_this).
                        let sources = &self.func(func_id as usize).upvalues;
                        let mut cells = Vec::with_capacity(sources.len());
                        for src in sources {
                            let cell = match *src {
                                UpvalSource::ParentLocal(reg) => self.get(base, reg).heap_index(),
                                UpvalSource::ParentUpval(idx) => self.closure_upvalue(cur_closure, idx),
                            };
                            cells.push(cell);
                        }
                        let this_val = self.get(base, this_reg);
                        let v = Value::heap(
                            self.heap.alloc(HeapObj::Closure { func: func_id, upvalues: cells, this_val }),
                        );
                        // An arrow inside an object method inherits that method's
                        // [[HomeObject]] lexically (so `super.x` in the arrow resolves).
                        let callee = self.frames[frame_idx].callee;
                        if callee.is_heap() {
                            if let Some(&home) = self.closure_home.get(&callee.heap_index()) {
                                self.closure_home.insert(v.heap_index(), home);
                            }
                        }
                        // ... and any dynamic EvalScope of the defining frame.
                        if let Some(sc) = self.ensure_frame_eval_scope(frame_idx) {
                            self.closure_eval_scope.insert(v.heap_index(), sc);
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeCell { reg } => {
                        let v = self.get(base, reg);
                        let cell = self.heap.alloc(HeapObj::Cell(v));
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::MakeCellTdz { reg } => {
                        // A captured lexical pre-created at entry: the cell starts in
                        // its TDZ (UNINITIALIZED) until the textual declaration runs.
                        let cell = self.heap.alloc(HeapObj::Cell(Value::UNINITIALIZED));
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::CellGet { dst, cell } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.heap.cell_get(cell_idx);
                        if v.is_uninitialized() {
                            return Err(Thrown(
                                "ReferenceError: cannot access a lexical binding before initialization"
                                    .to_string(),
                            ));
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::CellSet { cell, src } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.get(base, src);
                        self.heap.cell_set(cell_idx, v);
                        ip += 1;
                    }
                    Instr::UpvalGet { dst, idx } => {
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.heap.cell_get(cell);
                        if v.is_uninitialized() {
                            return Err(Thrown(
                                "ReferenceError: cannot access a lexical binding before initialization"
                                    .to_string(),
                            ));
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::UpvalSet { idx, src } => {
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.get(base, src);
                        self.heap.cell_set(cell, v);
                        ip += 1;
                    }
                    Instr::NewArray { dst, arg_base, argc } => {
                        let mut items = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            items.push(self.get(base, arg_base + i));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(items)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewObject { dst } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ToObject { dst, src } => {
                        let v = self.get(base, src);
                        let o = self.to_object(v)?;
                        self.set(base, dst, o);
                        ip += 1;
                    }
                    Instr::CheckCoercible { src } => {
                        let v = self.get(base, src);
                        self.require_object_coercible(v)?;
                        ip += 1;
                    }
                    Instr::NewError { dst, kind, arg, opts, errors } => {
                        // The message is coerced with a real ToString (observable user
                        // `toString` / `@@toPrimitive`, abrupt completion) for EVERY
                        // error kind: a Symbol message throws TypeError and a throwing
                        // coercion propagates — before the error is allocated (and, for
                        // AggregateError, before the errors are iterated).
                        let msg = match arg {
                            Some(r) => {
                                let m = self.get(base, r);
                                if m == Value::UNDEFINED {
                                    None
                                } else {
                                    let s = self.to_js_string(m)?;
                                    Some(self.alloc_str(s))
                                }
                            }
                            None => None,
                        };
                        let v = self.make_error(kind, msg);
                        // InstallErrorCause (ES2022): an options object with a `cause`
                        // gives the error a non-enumerable own `cause` property.
                        if let Some(or) = opts {
                            let options = self.get(base, or);
                            if self.is_object_value(options) && self.has_own_property(options, "cause") {
                                let cause = self.get_prop(options, "cause")?;
                                if let HeapObj::Object(m) = self.heap.get_mut(v.heap_index()) {
                                    m.define(
                                        "cause",
                                        cause,
                                        PropAttr {
                                            writable: true,
                                            enumerable: false,
                                            configurable: true,
                                            accessor: false,
                                            setter: Value::UNDEFINED,
                                        },
                                    );
                                }
                            }
                        }
                        // AggregateError installs `errors` LAST (after message + cause):
                        // a non-enumerable own array of IterableToList(firstArg). It runs
                        // even when the arg is absent (`new AggregateError()`) so that
                        // IterableToList(undefined) throws the required TypeError.
                        if kind == 7 {
                            let errors_arg =
                                errors.map(|er| self.get(base, er)).unwrap_or(Value::UNDEFINED);
                            self.install_agg_errors(v, errors_arg)?;
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeSymbol { dst, desc } => {
                        // `Symbol(desc)`: description is ToString(desc) unless absent/undefined.
                        let d = match desc {
                            Some(r) => {
                                let v = self.get(base, r);
                                if v == Value::UNDEFINED {
                                    Value::UNDEFINED
                                } else {
                                    let s = self.to_js_string(v)?;
                                    self.alloc_str(s)
                                }
                            }
                            None => Value::UNDEFINED,
                        };
                        let sym = self.make_symbol(d);
                        self.set(base, dst, sym);
                        ip += 1;
                    }
                    Instr::LoadBigInt { dst, value } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::BigInt(value)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::BigIntFrom { dst, arg } => {
                        let a = self.get(base, arg);
                        let n = self.bigint_from(a)?;
                        let v = self.make_bigint(n);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewRegExp { dst, pattern, flags, is_construct } => {
                        let p = self.get(base, pattern);
                        let f = self.get(base, flags);
                        // `RegExp(re)` (NOT `new`) with no flags returns `re`
                        // unchanged when re's `constructor` is RegExp (ctor step 2.b).
                        let mut short = None;
                        if !is_construct && f.is_undefined() && self.regexp_ctor != 0 && self.is_regexp(p)? {
                            let c = self.get_prop(p, "constructor")?;
                            if self.same_value(c, Value::heap(self.regexp_ctor)) {
                                short = Some(p);
                            }
                        }
                        let v = match short {
                            Some(v) => v,
                            None => self.build_regexp(p, f)?,
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::GetIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let r = self.get_index(o, k)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ToPropKey { dst, obj, src } => {
                        // RequireObjectCoercible(base) precedes ToPropertyKey: a
                        // null/undefined base throws BEFORE the key's toString runs
                        // (mirrors get_index). Coercing once here makes the later
                        // GetIndex/SetIndex coercion a no-op (single key evaluation).
                        let o = self.get(base, obj);
                        if o.is_nullish() {
                            return Err(Thrown(format!(
                                "TypeError: cannot read property of {}",
                                self.display(o)
                            )));
                        }
                        let k = self.get(base, src);
                        let pk = self.coerce_index_key(k)?;
                        self.set(base, dst, pk);
                        ip += 1;
                    }
                    Instr::SetIndex { obj, key, val } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let v = self.get(base, val);
                        let strict = self.func(func_id as usize).is_strict;
                        self.set_index(o, k, v, strict)?;
                        ip += 1;
                    }
                    Instr::ImportCall { dst, spec, phase, opts } => {
                        // import(spec [, opts]) / import.defer / import.source.
                        // Spec order: ToString(spec); then a non-undefined non-object
                        // `opts` â†’ TypeError; `import.source` â†’ SyntaxError (source
                        // phase unavailable for a text module); otherwise resolve the
                        // specifier against the script's dir, load + evaluate the
                        // module ONCE (cached by path so re-import yields the SAME
                        // namespace), and resolve with its (snapshot) namespace. A
                        // missing file / no base dir â†’ TypeError; a throw during
                        // ToString or evaluation rejects with that value. import()
                        // never throws synchronously. Everything that may GC runs
                        // BEFORE the promise is allocated; the settle value is rooted
                        // in `dst` across alloc_promise (the iter-169 GC invariant).
                        let spec_val = self.get(base, spec);
                        let settle: Result<Value, Value> = match self.to_js_string(spec_val) {
                            Err(_) => Err(self
                                .pending_throw
                                .take()
                                .unwrap_or_else(|| self.make_error(1, None))),
                            Ok(spec_str) => {
                                let mut mtype: Option<String> = None;
                                let opt_err: Option<Value> = match opts {
                                    Some(r) => {
                                        let ov = self.get(base, r);
                                        if ov == Value::UNDEFINED {
                                            None
                                        } else {
                                            match self.validate_import_options(ov) {
                                                Ok(t) => {
                                                    mtype = t;
                                                    None
                                                }
                                                Err(e) => Some(e),
                                            }
                                        }
                                    }
                                    None => None,
                                };
                                if let Some(e) = opt_err {
                                    Err(e) // bad options / import attributes
                                } else if phase == 2 {
                                    Err(self.make_error(3, None)) // SyntaxError: source phase
                                } else {
                                    match self.module_base_dir.as_ref().map(|d| d.join(&spec_str)) {
                                        None => Err(self.make_error(1, None)),
                                        // import_module canonicalizes, caches, runs, and
                                        // recursively links re-exports; the returned value
                                        // IS the fully-linked namespace.
                                        Some(p) => match self.import_module(&p, mtype.as_deref()) {
                                            Ok(ns) => Ok(ns),
                                            // A loader Thrown carries its error type in
                                            // the message prefix ("SyntaxError: …") —
                                            // reject with the MATCHING error object, not
                                            // an empty TypeError.
                                            Err(Thrown(msg)) => Err(self
                                                .pending_throw
                                                .take()
                                                .unwrap_or_else(|| self.error_from_thrown(&msg))),
                                        },
                                    }
                                }
                            }
                        };
                        match settle {
                            Ok(v) => {
                                self.set(base, dst, v);
                                let p = self.alloc_promise();
                                let r = self.get(base, dst);
                                self.resolve(p, r);
                                self.set(base, dst, Value::heap(p));
                            }
                            Err(e) => {
                                self.set(base, dst, e);
                                let p = self.alloc_promise();
                                let r = self.get(base, dst);
                                self.reject(p, r);
                                self.set(base, dst, Value::heap(p));
                            }
                        }
                        ip += 1;
                    }
                    Instr::ClassStaticField { class, key, val } => {
                        let cv = self.get(base, class);
                        let kv = self.get(base, key);
                        let vv = self.get(base, val);
                        // ToPropertyKey ONCE (the key expression was already evaluated).
                        let k = self.coerce_index_key(kv)?;
                        if self.key_of(k) == "prototype" {
                            return Err(Thrown(
                                "TypeError: Classes may not have a static property named 'prototype'"
                                    .into(),
                            ));
                        }
                        // The resolved key is a string/symbol, so set_index's own
                        // ToPropertyKey is idempotent (no user code runs twice).
                        self.set_index(cv, k, vv, true)?;
                        ip += 1;
                    }
                    Instr::DefineAccessor { obj, key, func, is_setter } => {
                        let o = self.get(base, obj);
                        let kv = self.get(base, key);
                        let f = self.get(base, func);
                        let k = self.to_property_key(kv)?;
                        self.define_object_accessor(o, &k, f, is_setter);
                        ip += 1;
                    }
                    Instr::SetFnNameFromKey { func, key, prefix } => {
                        let f = self.get(base, func);
                        let k = self.get(base, key);
                        self.set_fn_name_from_key(f, k, prefix);
                        ip += 1;
                    }
                    Instr::GetProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        // PrivateFieldGet brand check: reading a private member
                        // (`obj.#x`) from an object whose class did not declare it
                        // is a TypeError (has_property_str walks instance own fields
                        // + private methods/getters on the class chain).
                        // PrivateFieldGet present iff the element exists textually AND
                        // (when the accessing class's brand chain is resolvable) the
                        // receiver carries the declaring class's brand — the textual
                        // half rejects a wholly-absent member, the brand half rejects a
                        // same-named member of a DIFFERENT class evaluation.
                        if is_private_key(&key) {
                            if let Some((b, kind, owner)) = self.resolve_private(&key) {
                                // Declaring-class-resolved, KIND-aware access
                                // (spec PrivateGet): brand first, then dispatch
                                // on the DECLARING class's member kind.
                                if !self.private_receiver_ok(o, b, kind, owner) {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot read private member {key} from an object whose class did not declare it"
                                    )));
                                }
                                if kind & 7 != 0 {
                                    let r = if kind & 1 != 0 {
                                        self.private_member_from_owner(owner, &key, (kind & 8) | 1)
                                            .unwrap_or(Value::UNDEFINED)
                                    } else if kind & 2 != 0 {
                                        let g = self
                                            .private_member_from_owner(owner, &key, (kind & 8) | 2)
                                            .unwrap_or(Value::UNDEFINED);
                                        self.call_value(g, o, &[])?
                                    } else {
                                        // Setter-only accessor: no [[Get]].
                                        return Err(Thrown(format!(
                                            "TypeError: '{key}' was defined without a getter"
                                        )));
                                    };
                                    self.set(base, dst, r);
                                    ip += 1;
                                    continue;
                                }
                                // Private FIELD: PrivateFieldFind in the
                                // side table (empty -> TypeError).
                                match self.private_field_get(o, b, &key) {
                                    Some(v) => {
                                        self.set(base, dst, v);
                                        ip += 1;
                                        continue;
                                    }
                                    None => {
                                        return Err(Thrown(format!(
                                            "TypeError: Cannot read private member {key} from an object whose class did not declare it"
                                        )));
                                    }
                                }
                            } else {
                                let textual = self.has_property_str(o, &key)
                                    || self.private_field_scan_has(o, &key);
                                let present = match self.private_brand_ok(o, &key) {
                                    Some(b) => textual && b,
                                    None => textual,
                                };
                                if !present {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot read private member {key} from an object whose class did not declare it"
                                    )));
                                }
                                if let Some(vv) = self.private_field_scan(o, &key) {
                                    self.set(base, dst, vv);
                                    ip += 1;
                                    continue;
                                }
                            }
                        }
                        let r = self.get_prop(o, &key)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DefineField { obj, name, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        self.define_field(o, &key, v)?;
                        ip += 1;
                    }
                    Instr::SetProp { obj, name, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        // Private stores route to the side table / accessors
                        // (field-init emission, compound/update writes). One
                        // byte-compare on the hot path.
                        if key.as_bytes().first() == Some(&b'#') {
                            if let Some((b2, kind, owner)) = self.resolve_private(&key) {
                                if !self.private_receiver_ok(o, b2, kind, owner) {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot write private member {key} to an object whose class did not declare it"
                                    )));
                                }
                                if kind & 7 == 0 {
                                    // PrivateFieldAdd/Set: upsert (the add case
                                    // is the field initializer's first store).
                                    self.private_field_set(o, b2, &key, v, true);
                                } else if kind & 4 != 0 {
                                    let s = self
                                        .private_member_from_owner(owner, &key, (kind & 8) | 4)
                                        .unwrap_or(Value::UNDEFINED);
                                    self.call_value(s, o, &[v])?;
                                } else if kind & 2 != 0 {
                                    return Err(Thrown(format!(
                                        "TypeError: '{key}' was defined without a setter"
                                    )));
                                } else {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot assign to private method {key}"
                                    )));
                                }
                                ip += 1;
                                continue;
                            }
                            // A STATIC private field initializer runs inline
                            // in the DEFINING frame (this_override), so the chain
                            // is unresolvable there — derive the brand from the
                            // receiver class itself.
                            if o.is_heap()
                                && matches!(self.heap.get(o.heap_index()), HeapObj::Class(_))
                            {
                                if let Some(own) = self
                                    .method_brand
                                    .get(&o.heap_index())
                                    .and_then(|c| c.first())
                                    .copied()
                                {
                                    let declares = self
                                        .brand_private_names
                                        .get(&own)
                                        .is_some_and(|ns| ns.iter().any(|(n, k)| n == &key && *k == 8));
                                    if declares {
                                        self.private_field_set(o, own, &key, v, true);
                                        ip += 1;
                                        continue;
                                    }
                                }
                            }
                            // Unresolvable chain: write an EXISTING table entry
                            // (any brand) before falling back to a textual prop.
                            if self.private_field_scan_set(o, &key, v) {
                                ip += 1;
                                continue;
                            }
                        }
                        let strict = self.func(func_id as usize).is_strict;
                        self.set_prop(o, &key, v, strict)?;
                        ip += 1;
                    }
                    Instr::SetPrivate { obj, name, val } => {
                        // PrivateSet (user `this.#x = v`): brand-check first â€” the
                        // private element must be present on the receiver, else a
                        // TypeError (mirrors the GetProp brand check). Private writes
                        // are always strict.
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        if let Some((b, kind, owner)) = self.resolve_private(&key) {
                            // Declaring-class-resolved, KIND-aware write (spec
                            // PrivateSet).
                            if !self.private_receiver_ok(o, b, kind, owner) {
                                return Err(Thrown(format!(
                                    "TypeError: Cannot write private member {key} to an object whose class did not declare it"
                                )));
                            }
                            if kind & 7 == 0 {
                                // PrivateFieldSet: the entry must EXIST.
                                if !self.private_field_set(o, b, &key, v, false) {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot write private member {key} to an object whose class did not declare it"
                                    )));
                                }
                            } else if kind & 4 != 0 {
                                let s = self
                                    .private_member_from_owner(owner, &key, (kind & 8) | 4)
                                    .unwrap_or(Value::UNDEFINED);
                                self.call_value(s, o, &[v])?;
                            } else if kind & 2 != 0 {
                                // Getter-only accessor: no [[Set]].
                                return Err(Thrown(format!(
                                    "TypeError: '{key}' was defined without a setter"
                                )));
                            } else {
                                // A private METHOD is not writable.
                                return Err(Thrown(format!(
                                    "TypeError: Cannot assign to private method {key}"
                                )));
                            }
                        } else {
                            let textual = self.has_property_str(o, &key)
                                || self.private_field_scan_has(o, &key);
                            let present = match self.private_brand_ok(o, &key) {
                                Some(b) => textual && b,
                                None => textual,
                            };
                            if !present {
                                return Err(Thrown(format!(
                                    "TypeError: Cannot write private member {key} to an object whose class did not declare it"
                                )));
                            }
                            if !self.private_field_scan_set(o, &key, v) {
                                self.set_prop(o, &key, v, true)?;
                            }
                        }
                        ip += 1;
                    }
                    Instr::InitDataProp { obj, name, val } => {
                        // CreateDataProperty on a fresh object-literal object: a plain
                        // own w/e/c data property, ignoring the prototype chain.
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key: &'p str =
                            &self.func(func_id as usize).string_constants[name as usize];
                        if o.is_heap() {
                            let oi = o.heap_index();
                            if let HeapObj::Object(m) = self.heap.get_mut(oi) {
                                m.define(key, v, crate::heap::PropAttr::data());
                            } else {
                                let k = key.to_string();
                                self.set_prop(o, &k, v, false)?;
                            }
                        }
                        ip += 1;
                    }
                    Instr::DeleteProp { dst, obj, name, strict } => {
                        let o = self.get(base, obj);
                        // `delete obj.x` does ToObject(base) â€” null/undefined throw a
                        // TypeError (RequireObjectCoercible); other primitives box and
                        // delete returns true.
                        self.require_object_coercible(o)?;
                        let key = self.func(func_id as usize)
                            .string_constants[name as usize]
                            .clone();
                        let r = self.delete_property(o, &key)?;
                        // Strict mode: a delete that evaluates to false throws.
                        if strict && r == Value::bool(false) {
                            return Err(Thrown(format!("TypeError: Cannot delete property '{key}'")));
                        }
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DeleteIndex { dst, obj, key, strict } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let ks = self.to_property_key(k)?; // ToPropertyKey (symbol â†’ prop_key, object â†’ ToString)
                        // `delete obj[k]` does ToObject(base) after ToPropertyKey â€”
                        // null/undefined throw a TypeError (other primitives box and
                        // delete returns true).
                        self.require_object_coercible(o)?;
                        let r = self.delete_property(o, &ks)?;
                        if strict && r == Value::bool(false) {
                            return Err(Thrown(format!("TypeError: Cannot delete property '{ks}'")));
                        }
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    // Direct eval from strict code: the evaluated string inherits
                    // strict mode. Mirrors the `GLOBAL_EVAL` native but forces strict;
                    // a non-string argument is returned unchanged (spec 19.2.1).
                    Instr::ImportMeta { dst } => {
                        if self.import_meta == 0 {
                            let idx = self.heap.alloc(HeapObj::Object(ObjMap::new()));
                            self.proto_of.insert(idx, Value::NULL);
                            self.import_meta = idx;
                        }
                        self.set(base, dst, Value::heap(self.import_meta));
                        ip += 1;
                    }
                    Instr::DirectEval { dst, arg, new_target_ok, this_reg, home_class, super_static, ban_arguments, strict_caller, super_home_obj, var_env_is_global, site } => {
                        let a0 = self.get(base, arg);
                        let is_str = a0.is_heap()
                            && matches!(
                                self.heap.get(a0.heap_index()),
                                HeapObj::Str(_) | HeapObj::Cons { .. }
                            );
                        // Runtime identity check: direct-eval semantics apply
                        // only while the global `eval` binding still IS %eval%;
                        // a rebound `eval` gets an ordinary call of that value.
                        let live = self.global_by_name("eval").unwrap_or(Value::UNDEFINED);
                        if !(live.is_heap() && live.heap_index() == self.eval_fn_idx) {
                            let r = self.call_value(live, Value::UNDEFINED, &[a0])?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let r = if is_str {
                            let code = self.display(a0);
                            // A direct eval inherits the caller's `this` (reg 0, or
                            // the static-field-initializer's `this_reg`) and the
                            // caller's strictness.
                            let caller_this = self.get(base, this_reg);
                            let inherit =
                                (home_class != u32::MAX).then_some((home_class, super_static));
                            // The caller activation's new.target and (for an
                            // object-literal method) its [[HomeObject]].
                            let caller_nt = self
                                .frames
                                .last()
                                .map(|f| f.new_target)
                                .unwrap_or(Value::UNDEFINED);
                            let caller_home = if super_home_obj {
                                self.frames
                                    .last()
                                    .filter(|f| f.callee.is_heap())
                                    .and_then(|f| self.closure_home.get(&f.callee.heap_index()))
                                    .copied()
                            } else {
                                None
                            };
                            // Sloppy FUNCTION-context eval: its var/function
                            // declarations live in this activation's dynamic
                            // EvalScope (created lazily here).
                            let eval_scope_idx = if !var_env_is_global && !strict_caller {
                                let fi = self.frames.len() - 1;
                                if self.frames[fi].eval_scope == u32::MAX {
                                    let s = self.heap.alloc(HeapObj::EvalScope(
                                        std::collections::HashMap::new(),
                                    ));
                                    self.frames[fi].eval_scope = s;
                                }
                                Some(self.frames[fi].eval_scope)
                            } else {
                                None
                            };
                            self.do_eval(
                                &code,
                                strict_caller,
                                new_target_ok,
                                Some(caller_this),
                                inherit,
                                ban_arguments,
                                true,
                                caller_nt,
                                caller_home,
                                var_env_is_global,
                                if site != u16::MAX {
                                    self.func(func_id as usize).eval_sites[site as usize]
                                        .1
                                        .clone()
                                } else {
                                    None
                                },
                                {
                                    // The site's visible caller bindings: their
                                    // boxed cells become the eval closure's
                                    // upvalues.
                                    if site != u16::MAX {
                                        let (map, _) = self.func(func_id as usize).eval_sites
                                            [site as usize]
                                            .clone();
                                        let mut names = Vec::with_capacity(map.len());
                                        let mut cells = Vec::with_capacity(map.len());
                                        for (n, kind, idx) in map {
                                            let cv = if kind == 0 {
                                                self.get(base, idx)
                                            } else {
                                                // kind 1: this frame's closure
                                                // upvalue cell (an eval root
                                                // forwarding its caller scope).
                                                let cl = self
                                                    .frames
                                                    .last()
                                                    .map(|f| f.closure)
                                                    .unwrap_or(NO_CLOSURE);
                                                if cl != NO_CLOSURE {
                                                    match self.heap.get(cl) {
                                                        HeapObj::Closure { upvalues, .. } => {
                                                            upvalues
                                                                .get(idx as usize)
                                                                .map(|&c| Value::heap(c))
                                                                .unwrap_or(Value::UNDEFINED)
                                                        }
                                                        _ => Value::UNDEFINED,
                                                    }
                                                } else {
                                                    Value::UNDEFINED
                                                }
                                            };
                                            if cv.is_heap() {
                                                names.push(n);
                                                cells.push(cv);
                                            }
                                        }
                                        Some((names, cells))
                                    } else {
                                        None
                                    }
                                },
                                eval_scope_idx,
                            )?
                        } else {
                            a0
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Call { dst, callee, arg_base, argc } => {
                        let callee_v = self.get(base, callee);
                        // A callable Proxy: route through call_value (apply trap).
                        if callee_v.is_heap()
                            && matches!(self.heap.get(callee_v.heap_index()), HeapObj::Proxy { .. })
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A native resolve/reject function (from `new Promise`).
                        if callee_v.is_heap() {
                            if let HeapObj::BoundResolver { promise, is_reject } =
                                self.heap.get(callee_v.heap_index())
                            {
                                let (p, isr) = (*promise, *is_reject);
                                let arg = if argc >= 1 {
                                    self.get(base, arg_base)
                                } else {
                                    Value::UNDEFINED
                                };
                                if isr {
                                    self.reject(p, arg);
                                } else {
                                    self.resolve(p, arg);
                                }
                                self.set(base, dst, Value::UNDEFINED);
                                ip += 1;
                                continue;
                            }
                            // A bound or native function: run via call_value (fixes
                            // `this`/prepends bound args, or dispatches the builtin).
                            // A CombinatorResolver (a Promise.all/race/â€¦ resolve/reject
                            // element) is callable too: a userland thenable invokes it
                            // directly as `onFulfilled(v)` â€” route it through call_value
                            // so it performs its combinator step instead of throwing
                            // "not a function". %Function.prototype% is also a callable.
                            if matches!(
                                self.heap.get(callee_v.heap_index()),
                                HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::CombinatorResolver { .. }
                            ) || (self.fn_proto != 0 && callee_v.heap_index() == self.fn_proto)
                            {
                                let argv: Vec<Value> =
                                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                                let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        // A built-in constructor object invoked as a function
                        // (e.g. an Intl service ctor without `new`), or an
                        // [[IsHTMLDDA]] exotic called directly (â†’ undefined).
                        if callee_v.is_heap()
                            && (matches!(self.heap.get(callee_v.heap_index()), HeapObj::Object(m) if m.is_ctor)
                                || self.is_htmldda.contains(&callee_v.heap_index()))
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable(callee_v)?;
                        // An `async function*` returns an AsyncGenerator (checked
                        // before the plain-generator/async cases since it is both).
                        if self.func(fid as usize).is_generator
                            && self.func(fid as usize).is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator function returns a Generator object, unrun.
                        if self.func(fid as usize).is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async function runs to its first `await` then returns
                        // its result Promise.
                        if self.func(fid as usize).is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(
                            fid,
                            closure,
                            Value::UNDEFINED,
                            base,
                            arg_base,
                            argc,
                            dst,
                            ip + 1,
                            callee_v,
                        )?;
                        break;
                    }

                    Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        // `program` outlives the VM, so borrow the method name
                        // with the program's lifetime (NOT self's) â€” avoids
                        // cloning the name string on every method call (a heap
                        // alloc per `a.push(i)` / `a.map(cb)` etc.).
                        // `func()` returns `&'p`, so the interned name key outlives
                        // any `&mut self` below â€” and resolves eval functions too.
                        let key: &'p str =
                            &self.func(func_id as usize).string_constants[name as usize];
                        // PrivateMethodCall brand check (`obj.#m()`): the receiver must
                        // carry the declaring class's brand, else a TypeError. Gated on
                        // is_private_key so ordinary calls (push/map/…) only pay a
                        // leading-'#' test; textual fallback when no brand resolvable.
                        let mut private_callee: Option<Value> = None;
                        if is_private_key(key) {
                            if let Some((b, kind, owner)) = self.resolve_private(key) {
                                // Declaring-class-resolved, KIND-aware (FIX-3).
                                if !self.private_receiver_ok(recv, b, kind, owner) {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot invoke private method {key} on an object whose class did not declare it"
                                    )));
                                }
                                let f = if kind & 1 != 0 {
                                    self.private_member_from_owner(owner, key, (kind & 8) | 1)
                                        .unwrap_or(Value::UNDEFINED)
                                } else if kind & 2 != 0 {
                                    let g = self
                                        .private_member_from_owner(owner, key, (kind & 8) | 2)
                                        .unwrap_or(Value::UNDEFINED);
                                    self.call_value(g, recv, &[])?
                                } else if kind & 4 != 0 {
                                    return Err(Thrown(format!(
                                        "TypeError: '{key}' was defined without a getter"
                                    )));
                                } else {
                                    match self.private_field_get(recv, b, key) {
                                        Some(v) => v,
                                        None => {
                                            return Err(Thrown(format!(
                                                "TypeError: Cannot invoke private method {key} on an object whose class did not declare it"
                                            )));
                                        }
                                    }
                                };
                                private_callee = Some(f);
                            } else {
                                let textual = self.has_property_str(recv, key)
                                    || self.private_field_scan_has(recv, key);
                                let present = match self.private_brand_ok(recv, key) {
                                    Some(b) => textual && b,
                                    None => textual,
                                };
                                if !present {
                                    return Err(Thrown(format!(
                                        "TypeError: Cannot invoke private method {key} on an object whose class did not declare it"
                                    )));
                                }
                                private_callee = self.private_field_scan(recv, key);
                            }
                        }
                        // Hot fast path: `arr.push(x)` â€” the most common
                        // per-element array idiom. Append directly, skipping the
                        // try_builtin_method â†’ dispatch_builtin_method â†’ array_method
                        // layering (and the args-gather), then return the new length.
                        // A FROZEN array (or one whose `length` was made non-writable)
                        // must throw â€” skip the fast path so it routes through
                        // array_method's guard. The side tables are empty for an
                        // ordinary array, so this is a no-op in the build-a-list case.
                        if argc == 1
                            && key == "push"
                            && recv.is_heap()
                            // A prototype carrying integer indices means push's new
                            // index may resolve to a prototype setter (OrdinarySet) â€”
                            // route through array_method's proto-aware path.
                            && !self.array_proto_has_index
                            && !(!self.arr_props.is_empty()
                                && self
                                    .arr_props
                                    .get(&recv.heap_index())
                                    .map_or(false, |m| m.is_frozen()))
                            && !(!self.array_length_nonwritable.is_empty()
                                && self.array_length_nonwritable.contains(&recv.heap_index()))
                        {
                            let v = self.get(base, arg_base);
                            let len = if let HeapObj::Array(items) =
                                self.heap.get_mut(recv.heap_index())
                            {
                                items.push(v);
                                Some(items.len() as i32)
                            } else {
                                None
                            };
                            if let Some(len) = len {
                                self.set(base, dst, Value::int(len));
                                ip += 1;
                                continue;
                            }
                        }
                        // Builtin methods (array/string) execute inline and
                        // produce a result without pushing a frame.
                        if let Some(result) = self.try_builtin_method(recv, key, base, arg_base, argc)? {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Otherwise the property must resolve to a function; call it
                        // with `this = recv`.
                        let prop = match private_callee {
                            Some(f) => f,
                            None => self.get_prop(recv, key)?,
                        };
                        // A built-in constructor object used as a method value
                        // (e.g. `Intl.NumberFormat(...)` without `new`): route via
                        // call_value, which handles construct-without-new / throws.
                        if prop.is_heap()
                            && matches!(self.heap.get(prop.heap_index()), HeapObj::Object(m) if m.is_ctor)
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A built-in constructor object used as a computed method
                        // value (e.g. `Intl["NumberFormat"](...)` without `new`).
                        if prop.is_heap()
                            && matches!(self.heap.get(prop.heap_index()), HeapObj::Object(m) if m.is_ctor)
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A native or bound method value (e.g. inherited from a
                        // prototype) is invoked via call_value with this = recv.
                        if prop.is_heap()
                            && (matches!(
                                self.heap.get(prop.heap_index()),
                                HeapObj::Native(_) | HeapObj::Bound { .. } | HeapObj::BoundResolver { .. }
                            ) || (self.fn_proto != 0 && prop.heap_index() == self.fn_proto))
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, recv, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable(prop)?;
                        // An `async function*` method returns an AsyncGenerator.
                        if self.func(fid as usize).is_generator
                            && self.func(fid as usize).is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv)?;
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator method returns a Generator object, unrun.
                        if self.func(fid as usize).is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv)?;
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async method runs to its first `await` then returns
                        // its result Promise.
                        if self.func(fid as usize).is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        // Pass the resolved method VALUE as the callee (so LoadCallee and
                        // object-method `super` â€” which reads [[HomeObject]] keyed by the
                        // executing function value â€” find it, even for a no-upvalue method).
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1, prop)?;
                        break;
                    }

                    Instr::CallMethodComputed { dst, obj, key, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        let k = self.get(base, key);
                        // `obj["push"](x)` etc: a builtin array/string method first.
                        let kstr = self.display(k);
                        if let Some(result) =
                            self.try_builtin_method(recv, &kstr, base, arg_base, argc)?
                        {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Else resolve the method off the receiver (own/inherited)
                        // and call it with `this = recv`.
                        let method = self.get_index(recv, k)?;
                        // A native / bound / resolver method value runs via call_value.
                        if method.is_heap()
                            && (matches!(
                                self.heap.get(method.heap_index()),
                                HeapObj::Native(_) | HeapObj::Bound { .. } | HeapObj::BoundResolver { .. }
                            ) || (self.fn_proto != 0 && method.heap_index() == self.fn_proto))
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(method, recv, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable(method)?;
                        if self.func(fid as usize).is_generator
                            && self.func(fid as usize).is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv)?;
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        if self.func(fid as usize).is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv)?;
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        if self.func(fid as usize).is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1, method)?;
                        break;
                    }

                    Instr::Throw { src } => {
                        let v = self.get(base, src);
                        let msg = self.throw_message(v);
                        // Persist ip so the (unused) frame state is coherent,
                        // then signal unwinding via pending_throw + Err.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = ip;
                        self.pending_throw = Some(v);
                        return Err(Thrown(msg));
                    }
                    Instr::PushHandler { catch_target, catch_reg } => {
                        let top = self.frames.len() - 1;
                        self.frames[top]
                            .handlers
                            .push(Handler::Catch { target: catch_target, reg: catch_reg });
                        ip += 1;
                    }
                    Instr::PopHandler => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::PushFinally { target, kind_reg, val_reg } => {
                        let top = self.frames.len() - 1;
                        self.frames[top]
                            .handlers
                            .push(Handler::Finally { target, kind_reg, val_reg });
                        ip += 1;
                    }
                    Instr::PopFinally => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::EndFinally { kind_reg, val_reg } => {
                        // Resume the completion deposited when this finally was
                        // entered. The low 2 bits are the kind: 1 = return (re-leave
                        // through any outer finally, else return), 2 = throw
                        // (re-raise), 3 = break/continue jump (resume the unwind
                        // toward the jump target, `floor` in the upper bits), else
                        // 0 = normal.
                        let raw = self.regs[base + kind_reg as usize].as_int();
                        match raw & 3 {
                            1 => {
                                let v = self.regs[base + val_reg as usize];
                                if let Some(target) = self.route_through_finally(1, v) {
                                    ip = target as usize;
                                    continue;
                                }
                                if self.pop_frame_with(v, stop_depth) {
                                    return Ok(v);
                                }
                                break;
                            }
                            2 => {
                                let v = self.regs[base + val_reg as usize];
                                let top = self.frames.len() - 1;
                                self.frames[top].ip = ip;
                                self.pending_throw = Some(v);
                                return Err(Thrown(self.throw_message(v)));
                            }
                            3 => {
                                let jump_target = self.regs[base + val_reg as usize].as_int() as u32;
                                let floor = (raw >> 2) as usize;
                                match self.route_jump_through_finally(jump_target, floor) {
                                    Some(target) => ip = target as usize,
                                    None => ip = jump_target as usize,
                                }
                            }
                            _ => {
                                ip += 1;
                            }
                        }
                    }
                    Instr::OpenUsingScope { dst } => {
                        // Allocate a fresh `using` resource scope; its id (in a
                        // register, so it rides the frame across suspensions) keys
                        // the disposer list in `using_resources`.
                        let id = self.using_next_id;
                        self.using_next_id = self.using_next_id.wrapping_add(1);
                        self.using_resources.insert(id, Vec::new());
                        self.set(base, dst, Value::int(id as i32));
                        ip += 1;
                    }
                    Instr::RegisterDisposable { scope, val } => {
                        // CreateDisposableResource (sync hint): null/undefined adds
                        // nothing; a non-object, or an object whose @@dispose is
                        // absent/non-callable, throws a TypeError AT the declaration
                        // (which unwinds through the enclosing finally so already-
                        // registered resources are still disposed).
                        let v = self.get(base, val);
                        if v.is_nullish() {
                            ip += 1;
                        } else {
                            if !self.is_object_value(v) {
                                return Err(Thrown(
                                    "TypeError: a 'using' declaration value is not an object".into(),
                                ));
                            }
                            let method = self.get_member(v, "@@dispose", v)?;
                            if !self.is_callable(method) {
                                return Err(Thrown(
                                    "TypeError: the 'using' value's [Symbol.dispose] is not a function"
                                        .into(),
                                ));
                            }
                            let disposer = Value::heap(self.heap.alloc(HeapObj::Bound {
                                target: method,
                                this: v,
                                args: Vec::new(),
                            }));
                            let id = self.get(base, scope).as_int() as u32;
                            if let Some(d) = self.using_resources.get_mut(&id) {
                                d.push(disposer);
                            }
                            ip += 1;
                        }
                    }
                    Instr::DisposeScope { scope, kind_reg, val_reg } => {
                        // DisposeResources: run this scope's disposers LIFO, merging
                        // any throw with the incoming completion (kind&3==2 â‡’ the
                        // block already threw) into a SuppressedError chain; rewrite
                        // kind/val so the following EndFinally re-raises the merge.
                        let id = self.get(base, scope).as_int() as u32;
                        let disposers =
                            self.using_resources.remove(&id).unwrap_or_default();
                        let raw = self.regs[base + kind_reg as usize].as_int();
                        let incoming = if raw & 3 == 2 {
                            Some(self.regs[base + val_reg as usize])
                        } else {
                            None
                        };
                        if let Some(v) = self.dispose_resource_list(disposers, incoming)? {
                            self.regs[base + kind_reg as usize] = Value::int(2);
                            self.regs[base + val_reg as usize] = v;
                        }
                        ip += 1;
                    }
                    Instr::RegisterAsyncDisposable { scope, val } => {
                        // CreateDisposableResource (async hint): null/undefined still
                        // pushes an INERT record (so disposal performs one Await); a
                        // non-object â†’ TypeError; else GetDisposeMethod(async): read
                        // @@asyncDispose FIRST (once), fall back to @@dispose only when
                        // it is nullish; both absent/non-callable â†’ TypeError.
                        let v = self.get(base, val);
                        let id = self.get(base, scope).as_int() as u32;
                        if v.is_nullish() {
                            if let Some(d) = self.using_resources.get_mut(&id) {
                                d.push(Value::UNDEFINED); // inert: awaited, not called
                            }
                            ip += 1;
                        } else {
                            if !self.is_object_value(v) {
                                return Err(Thrown(
                                    "TypeError: an 'await using' declaration value is not an object".into(),
                                ));
                            }
                            let mut method = self.get_member(v, "@@asyncDispose", v)?;
                            if !self.is_callable(method) {
                                method = self.get_member(v, "@@dispose", v)?;
                            }
                            if !self.is_callable(method) {
                                return Err(Thrown(
                                    "TypeError: the 'await using' value has no callable [Symbol.asyncDispose] or [Symbol.dispose]"
                                        .into(),
                                ));
                            }
                            let disposer = Value::heap(self.heap.alloc(HeapObj::Bound {
                                target: method,
                                this: v,
                                args: Vec::new(),
                            }));
                            if let Some(d) = self.using_resources.get_mut(&id) {
                                d.push(disposer);
                            }
                            ip += 1;
                        }
                    }
                    Instr::AsyncDisposeNext { scope, res, done } => {
                        // Pop the LAST (LIFO) disposer of the scope. Empty â†’ done. An
                        // inert `undefined` entry yields res=undefined (nothing called).
                        // A real bound disposer is CALLED here (carrying its `this`);
                        // its result is left in `res` for the caller to Await. A sync
                        // throw propagates (caught by the loop's handler).
                        let id = self.get(base, scope).as_int() as u32;
                        let entry = self
                            .using_resources
                            .get_mut(&id)
                            .and_then(|d| d.pop());
                        match entry {
                            None => {
                                self.set(base, done, Value::bool(true));
                                self.set(base, res, Value::UNDEFINED);
                                ip += 1;
                            }
                            Some(d) => {
                                self.set(base, done, Value::bool(false));
                                let r = if d.is_nullish() {
                                    Value::UNDEFINED
                                } else {
                                    self.call_value(d, Value::UNDEFINED, &[])?
                                };
                                self.set(base, res, r);
                                ip += 1;
                            }
                        }
                    }
                    Instr::MergeDispose { kind_reg, val_reg, err } => {
                        // DisposeResources error chaining for the async loop's catch
                        // arm: chain into a SuppressedError if a throw is already
                        // pending, else make the completion a throw of `err`.
                        let e = self.get(base, err);
                        let raw = self.regs[base + kind_reg as usize].as_int();
                        if raw & 3 == 2 {
                            let prior = self.regs[base + val_reg as usize];
                            let merged = self.build_suppressed_error(&[e, prior, Value::UNDEFINED])?;
                            self.regs[base + val_reg as usize] = merged;
                        } else {
                            self.regs[base + kind_reg as usize] = Value::int(2);
                            self.regs[base + val_reg as usize] = e;
                        }
                        ip += 1;
                    }
                    Instr::JumpFinally { target, floor } => {
                        // A `break`/`continue` exiting one or more `try` blocks:
                        // run each intervening `finally` first, popping any
                        // intervening `catch`, then land at `target`.
                        match self.route_jump_through_finally(target, floor as usize) {
                            Some(t) => ip = t as usize,
                            None => ip = target as usize,
                        }
                    }
                    Instr::SetRaw { arr, raw } => {
                        // GetTemplateObject finalization: `.raw` is a frozen,
                        // non-enumerable, non-writable, non-configurable own data
                        // property of the cooked array, and BOTH the cooked and raw
                        // arrays are frozen (their indices/length non-writable &
                        // non-configurable). Define `.raw` before freezing the cooked
                        // array (a frozen object rejects new properties).
                        let a = self.get(base, arr);
                        let r = self.get(base, raw);
                        if a.is_heap() && r.is_heap() {
                            let cooked = a.heap_index();
                            let raw_idx = r.heap_index();
                            let attr = PropAttr {
                                writable: false,
                                enumerable: false,
                                configurable: false,
                                accessor: false,
                                setter: Value::UNDEFINED,
                            };
                            self.arr_props.entry(cooked).or_insert_with(ObjMap::new).define("raw", r, attr);
                            for idx in [raw_idx, cooked] {
                                self.arr_props.entry(idx).or_insert_with(ObjMap::new).freeze();
                                self.array_length_nonwritable.insert(idx);
                            }
                        }
                        ip += 1;
                    }
                    Instr::TemplateGetCached { dst, site } => {
                        let v = self
                            .template_cache
                            .get(&(func_id, site))
                            .copied()
                            .unwrap_or(Value::UNDEFINED);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::TemplateSetCached { site, src } => {
                        let v = self.get(base, src);
                        self.template_cache.insert((func_id, site), v);
                        ip += 1;
                    }
                    Instr::GetIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::GetIteratorObj { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator_direct(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::GetAsyncIterator { dst, src, sync_dst } => {
                        let s = self.get(base, src);
                        let (it, is_sync) = self.get_async_iterator(s)?;
                        self.set(base, dst, it);
                        self.set(base, sync_dst, Value::bool(is_sync));
                        ip += 1;
                    }
                    Instr::IterToArray { dst, src, count } => {
                        let s = self.get(base, src);
                        let a = self.iter_to_array(s, count)?;
                        self.set(base, dst, a);
                        ip += 1;
                    }
                    Instr::Random { dst } => {
                        // xorshift64* â†’ a uniform double in [0, 1) (top 53 bits).
                        let mut x = self.rng_state;
                        x ^= x >> 12;
                        x ^= x << 25;
                        x ^= x >> 27;
                        self.rng_state = x;
                        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                        let f = (r >> 11) as f64 / (1u64 << 53) as f64;
                        self.set(base, dst, Value::num(f));
                        ip += 1;
                    }
                    Instr::DateNew { dst, arg_base, argc } => {
                        let args: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let ms = self.date_new_ms(&args)?;
                        let v = Value::heap(self.heap.alloc(HeapObj::Date(ms)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::DateUTC { dst, arg_base, argc } => {
                        let args: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let ms = self.date_utc_ms(&args)?;
                        self.set(base, dst, Value::num(ms));
                        ip += 1;
                    }
                    Instr::DateParse { dst, src } => {
                        let s = self.get(base, src);
                        let str = self.display(s);
                        self.set(base, dst, Value::num(parse_date(&str)));
                        ip += 1;
                    }
                    Instr::Return { src } => {
                        let v = self.regs[base + src as usize];
                        // Run any pending `finally` in this frame first.
                        if let Some(target) = self.route_through_finally(1, v) {
                            ip = target as usize;
                            continue;
                        }
                        if self.pop_frame_with(v, stop_depth) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::ReturnUndefined => {
                        if let Some(target) = self.route_through_finally(1, Value::UNDEFINED) {
                            ip = target as usize;
                            continue;
                        }
                        if self.pop_frame_with(Value::UNDEFINED, stop_depth) {
                            return Ok(Value::UNDEFINED);
                        }
                        break;
                    }
                    Instr::Yield { val, .. } => {
                        // Suspend the generator: pop the frame ENTRY but leave its
                        // register window live at the top of `self.regs` so the
                        // resumer (generator_method) can copy it back into the heap
                        // Generator. The generator frame is always the top (and the
                        // run_loop's stop frame) at a yield, so popping returns to
                        // the resumer. `pending_yield` carries the value + this ip.
                        let v = self.get(base, val);
                        // Capture this frame's `try` handlers so a SYNC generator can
                        // park them and `gen.throw(e)`/`gen.return(v)` resume into the
                        // body's try/catch/finally. (drive_async_gen ignores this.)
                        let f = self.frames.pop().unwrap();
                        self.pending_yield_eval_scope = f.eval_scope;
                        self.pending_yield_handlers = f.handlers;
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::YieldDelegate { val, .. } => {
                        // A `yield*` suspension: yield `val` exactly like `Yield`. The
                        // resume MODE + value are delivered into mode_dst/val_dst by
                        // gen_resume (it detects the resume op is a YieldDelegate), so
                        // here we only suspend.
                        let v = self.get(base, val);
                        let f = self.frames.pop().unwrap();
                        self.pending_yield_eval_scope = f.eval_scope;
                        self.pending_yield_handlers = f.handlers;
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::AsyncYieldDelegate { val, .. } => {
                        // An async `yield*` step: suspend exactly like `Yield`. On resume
                        // drive_async_gen delivers (mode, value) into mode_dst/val_dst.
                        // The distinct op lets the driver recognise a delegating
                        // suspension for `.throw()`/`.return()` handling.
                        let v = self.get(base, val);
                        let f = self.frames.pop().unwrap();
                        self.pending_yield_eval_scope = f.eval_scope;
                        self.pending_yield_handlers = f.handlers;
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::RequireObject { val } => {
                        let v = self.get(base, val);
                        if !self.is_object_value(v) {
                            return Err(Thrown(
                                "TypeError: iterator result is not an object".into(),
                            ));
                        }
                        ip += 1;
                    }
                    Instr::AsyncIterNextStep { dst, iter, idx, sent, next_fn } => {
                        // Like ForAwaitNext, but CALLS the cached next method (next_fn,
                        // captured once at GetIterator time) with the `.next(v)` sent
                        // value (yield* sent-value forwarding + no re-get of `next`).
                        let it = self.get(base, iter);
                        let sent_v = self.get(base, sent);
                        let nf = self.get(base, next_fn);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        let result = match self.heap.get(it.heap_index()) {
                            HeapObj::AsyncGenerator(_) => self
                                .async_generator_method(it.heap_index(), "next", &[sent_v])
                                .unwrap_or(Value::UNDEFINED),
                            HeapObj::Generator { .. } => self
                                .generator_method(it.heap_index(), "next", &[sent_v])?
                                .unwrap_or(Value::UNDEFINED),
                            HeapObj::Object(_) => {
                                if self.is_callable(nf) {
                                    self.call_value(nf, it, &[sent_v])?
                                } else {
                                    return Err(Thrown(format!(
                                        "TypeError: {} is not iterable",
                                        self.display(it)
                                    )));
                                }
                            }
                            _ => {
                                let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
                                // A Set's / Map's tombstoned (deleted) slots are skipped.
                                while match self.heap.get(it.heap_index()) {
                                    HeapObj::Set(items) => cursor < items.len() && items[cursor].is_hole(),
                                    HeapObj::Map { keys, .. } => cursor < keys.len() && keys[cursor].is_hole(),
                                    _ => false,
                                } {
                                    cursor += 1;
                                }
                                let len = match self.heap.get(it.heap_index()) {
                                    HeapObj::Array(items) => items.len(),
                                    HeapObj::Set(items) => items.len(),
                                    HeapObj::Str(s) => s.char_len,
                                    HeapObj::Cons { len, .. } => *len,
                                    HeapObj::Map { keys, .. } => keys.len(),
                                    _ => {
                                        return Err(Thrown(format!(
                                            "TypeError: {} is not iterable",
                                            self.display(it)
                                        )))
                                    }
                                };
                                if cursor < len {
                                    let val = self.get_index(it, Value::int(cursor as i32))?;
                                    self.set(base, idx, Value::int((cursor + 1) as i32));
                                    self.iter_result(val, false)
                                } else {
                                    self.iter_result(Value::UNDEFINED, true)
                                }
                            }
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::AsyncIterReturnStep { dst, has_dst, iter, ret } => {
                        let it = self.get(base, iter);
                        let r = self.get(base, ret);
                        let m = self.get_member(it, "return", it)?;
                        if m.is_nullish() || !self.is_callable(m) {
                            // No `return` method â†’ the outer generator just returns `ret`.
                            self.set(base, has_dst, Value::bool(false));
                        } else {
                            let res = self.call_value(m, it, &[r])?;
                            self.set(base, dst, res);
                            self.set(base, has_dst, Value::bool(true));
                        }
                        ip += 1;
                    }
                    Instr::AsyncIterThrowStep { dst, iter, exc } => {
                        let it = self.get(base, iter);
                        let e = self.get(base, exc);
                        let m = self.get_member(it, "throw", it)?;
                        if m.is_nullish() || !self.is_callable(m) {
                            // No usable `throw` on the delegated iterator â†’ TypeError.
                            return Err(Thrown(
                                "TypeError: the iterator does not provide a 'throw' method".into(),
                            ));
                        }
                        let res = self.call_value(m, it, &[e])?;
                        self.set(base, dst, res);
                        ip += 1;
                    }
                    Instr::IterDelegate { value_dst, done_dst, ret_dst, iter, mode, sent } => {
                        let iter_v = self.get(base, iter);
                        let mode_code = self.get(base, mode).as_int();
                        let sent_v = self.get(base, sent);
                        let (val, done_b, ret_b) =
                            self.iter_delegate_step(iter_v, mode_code, sent_v)?;
                        self.set(base, value_dst, val);
                        self.set(base, done_dst, Value::bool(done_b));
                        self.set(base, ret_dst, Value::bool(ret_b));
                        ip += 1;
                    }
                    Instr::GenStart => {
                        // Body-entry marker reached during the eager call-time run of
                        // a generator's parameter prologue. Suspend exactly like a
                        // valueless yield: pop the frame, leave the window live for
                        // `alloc_generator` to park, and record this ip so the first
                        // `.next()` resumes just past it (the resume path delivers no
                        // sent value here because this is not a `Yield`).
                        let f = self.frames.pop().unwrap();
                        self.pending_yield_eval_scope = f.eval_scope;
                        self.pending_yield_handlers = f.handlers; // empty at GenStart
                        self.pending_yield = Some((Value::UNDEFINED, ip));
                        return Ok(Value::UNDEFINED);
                    }
                    Instr::Await { val, .. } => {
                        // Suspend the async activation: pop the frame ENTRY but
                        // leave its register window live at the top of `self.regs`
                        // for `drive_async` to park into the heap AsyncState. Unlike
                        // a generator yield, we CAPTURE the frame's `try` handlers
                        // (carried in `pending_await`) so they can be restored on
                        // resume â€” letting `try { await p } catch (e)` see a
                        // rejection thrown back in at the await point. The async
                        // frame is always the top (and the run_loop stop frame) at
                        // an await, so popping returns to `drive_async`.
                        let v = self.get(base, val);
                        let f = self.frames.pop().unwrap();
                        self.pending_await = Some((v, ip, f.handlers));
                        return Ok(v);
                    }
                    Instr::IterClose { iter } => {
                        let it = self.get(base, iter);
                        self.iterator_close(it)?;
                        ip += 1;
                    }
                    Instr::IterCloseQuiet { iter } => {
                        // Error-context close: a throwing/non-object return() is
                        // ignored so the original abrupt completion is preserved.
                        let it = self.get(base, iter);
                        let _ = self.iterator_close(it);
                        ip += 1;
                    }
                    Instr::IterNext { value_dst, done_dst, iter, idx } => {
                        let it = self.get(base, iter);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        // A generator is driven by `.next()`; the cursor is unused.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Generator { .. }) {
                            let res = self
                                .generator_method(it.heap_index(), "next", &[])?
                                .unwrap_or(Value::UNDEFINED);
                            let done = self.get_prop(res, "done")?;
                            let val = self.get_prop(res, "value")?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, done);
                            ip += 1;
                            continue;
                        }
                        // A user iterator object (`@@iterator` already resolved by
                        // GetIterator): pull the next result via `.next()`. Lazy â€”
                        // a `break` simply stops calling it.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Object(_) | HeapObj::Iterator { .. } | HeapObj::IterHelper { .. }) {
                            let next = self.get_prop(it, "next")?;
                            if self.is_callable(next) {
                                let res = self.call_value(next, it, &[])?;
                                // IteratorNext step 3: a non-Object result is a TypeError.
                                if !self.is_object_value(res) {
                                    return Err(Thrown(
                                        "TypeError: iterator result is not an object".into(),
                                    ));
                                }
                                let done = self.get_prop(res, "done")?;
                                let val = self.get_prop(res, "value")?;
                                self.set(base, value_dst, val);
                                self.set(base, done_dst, done);
                                ip += 1;
                                continue;
                            }
                        }
                        // Array/Set element, string char, or Map [k,v] at the cursor.
                        let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        // A Set's / Map's tombstoned (deleted) slots are skipped.
                        while match self.heap.get(it.heap_index()) {
                            HeapObj::Set(items) => cursor < items.len() && items[cursor].is_hole(),
                            HeapObj::Map { keys, .. } => cursor < keys.len() && keys[cursor].is_hole(),
                            _ => false,
                        } {
                            cursor += 1;
                        }
                        let len = match self.heap.get(it.heap_index()) {
                            HeapObj::Array(items) => items.len(),
                            HeapObj::Set(items) => items.len(),
                            HeapObj::Str(s) => s.char_len,
                            HeapObj::Cons { len, .. } => *len,
                            HeapObj::Map { keys, .. } => keys.len(),
                            // The LIVE length each step: a tracking view follows its
                            // resizable buffer (shrink ends early, grow yields more);
                            // a detached/out-of-bounds view mid-iteration throws.
                            HeapObj::TypedArray { .. } => {
                                match self.ta_effective_len(it.heap_index()) {
                                    Some(n) => n,
                                    None => {
                                        return Err(Thrown(
                                            "TypeError: TypedArray iterator: the viewed buffer is detached or out of bounds".into(),
                                        ))
                                    }
                                }
                            }
                            _ => {
                                return Err(Thrown(format!(
                                    "TypeError: {} is not iterable",
                                    self.display(it)
                                )))
                            }
                        };
                        if cursor < len {
                            let val = self.get_index(it, Value::int(cursor as i32))?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, Value::bool(false));
                            self.set(base, idx, Value::int((cursor + 1) as i32));
                        } else {
                            self.set(base, done_dst, Value::bool(true));
                        }
                        ip += 1;
                    }
                    Instr::ForAwaitNext { dst, iter, idx } => {
                        let it = self.get(base, iter);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        let result = match self.heap.get(it.heap_index()) {
                            // Async iterator: `.next()` returns a Promise the loop awaits.
                            HeapObj::AsyncGenerator(_) => self
                                .async_generator_method(it.heap_index(), "next", &[])
                                .unwrap_or(Value::UNDEFINED),
                            // Sync generator: `.next()` returns {value,done} (awaited = no-op tick).
                            HeapObj::Generator { .. } => self
                                .generator_method(it.heap_index(), "next", &[])?
                                .unwrap_or(Value::UNDEFINED),
                            // A user iterator object (sync or async) with `.next()`.
                            HeapObj::Object(_) => {
                                let next = self.get_prop(it, "next")?;
                                if self.is_callable(next) {
                                    self.call_value(next, it, &[])?
                                } else {
                                    return Err(Thrown(format!(
                                        "TypeError: {} is not iterable",
                                        self.display(it)
                                    )));
                                }
                            }
                            // Array/Set element, string char, Map [k,v] â€” positional,
                            // wrapped in a {value, done} the loop awaits (a tick).
                            _ => {
                                let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
                                // A Set's / Map's tombstoned (deleted) slots are skipped.
                                while match self.heap.get(it.heap_index()) {
                                    HeapObj::Set(items) => cursor < items.len() && items[cursor].is_hole(),
                                    HeapObj::Map { keys, .. } => cursor < keys.len() && keys[cursor].is_hole(),
                                    _ => false,
                                } {
                                    cursor += 1;
                                }
                                let len = match self.heap.get(it.heap_index()) {
                                    HeapObj::Array(items) => items.len(),
                                    HeapObj::Set(items) => items.len(),
                                    HeapObj::Str(s) => s.char_len,
                                    HeapObj::Cons { len, .. } => *len,
                                    HeapObj::Map { keys, .. } => keys.len(),
                                    _ => {
                                        return Err(Thrown(format!(
                                            "TypeError: {} is not iterable",
                                            self.display(it)
                                        )))
                                    }
                                };
                                if cursor < len {
                                    let val = self.get_index(it, Value::int(cursor as i32))?;
                                    self.set(base, idx, Value::int((cursor + 1) as i32));
                                    self.iter_result(val, false)
                                } else {
                                    self.iter_result(Value::UNDEFINED, true)
                                }
                            }
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                }
            }
        }
    }

    /// If `func_id` has compiled native code, run it over the register window
    /// at `base` and return `(result_bits_as_Value, bail_ip)`. `None` if there
    /// is no compiled code for this function.
    ///
    /// The native code reads/writes `self.regs[base..]` directly via a raw
    /// pointer taken here and used ONLY for the duration of the call â€” nothing
    /// in between can resize `self.regs` (the JIT subset issues no calls/allocs).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_run_jit(&mut self, func_id: u32, base: usize) -> Option<(Value, u32)> {
        let jitfn = self.jit.get(func_id)? as *const crate::codegen::JitFn;
        // SAFETY: `jitfn` points into self.jit.compiled (stable for the call).
        // `regs_ptr` is valid for the frame's reg_count slots. A self-call op
        // routes through `jit_self_call` (passed the `vm` pointer below) which
        // may resize self.regs for the recursive frame â€” but it RESTORES regs to
        // this length before returning, and the native code re-reads its window
        // base from the callee-saved register only relative to `regs_ptr`, which
        // stays valid because jit_self_call uses a SEPARATE save/restore of the
        // regs Vec around the recursion (see its safety note).
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        let (bits, bail) = unsafe { (*jitfn).run(regs_ptr, vm_ptr) };
        Some((Value::from_bits(bits), bail))
    }

    /// Run the compiled OSR region for the loop headed at `entry_ip` (in
    /// `func_id`) over the frame's register window at `base`, returning the ip to
    /// resume interpreting at. `None` if no region is compiled for this header.
    ///
    /// The region's native code reads/writes `self.regs[base..]` and
    /// `self.globals` directly (the latter via a base pointer it fetches in its
    /// prologue). The numeric region issues NO calls that push frames or grow
    /// `self.regs`/`self.globals`, so the raw pointers stay valid for the call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_run_osr(&mut self, func_id: u32, entry_ip: u32, base: usize) -> Option<usize> {
        let region = self.jit.get_region(func_id, entry_ip)? as *const crate::codegen::Region;
        // Object scalar-replacement (SROA): clone the sync plan so no region
        // borrow is held while the sync mutates globals/heap below.
        let field_plan = unsafe { (*region).field_plan().cloned() };

        // â”€â”€ pre-run sync â”€â”€ load the promoted object's fields into the scratch
        // pool globals the native code reads as ordinary globals.
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.func(p.func_id as usize).string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.get_prop(obj, &key).unwrap_or(Value::UNDEFINED);
                self.globals[slot as usize] = v;
            }
        }

        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `region` is stable for the call (we don't mutate self.jit until
        // after); regs/globals do not move during a region run.
        let resume = unsafe { (*region).run(regs_ptr, vm_ptr) };

        // â”€â”€ post-run sync â”€â”€ flush the pool globals back to the object's fields,
        // so the interpreter (which resumes on the ORIGINAL bytecode, reading the
        // object) sees consistent values. Runs on EVERY exit (clean or bail).
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.func(p.func_id as usize).string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.globals[slot as usize];
                let _ = self.set_prop(obj, &key, v, false);
            }
        }
        // Bookkeeping: a resume INSIDE the region is a deopt; evict if chronic.
        self.jit.note_region_resume(func_id, entry_ip, resume);
        Some(resume as usize)
    }

    /// Pop the current frame. If this returns control to `stop_depth` (the
    /// frame the active `run_loop` was asked to run), report `true` so the loop
    /// returns `ret`. Otherwise deliver `ret` into the caller's `ret_dst` and
    /// report `false` to keep executing the caller.
    #[inline]
    pub(crate) fn pop_frame_with(&mut self, ret: Value, stop_depth: usize) -> bool {
        let finished = self.frames.pop().expect("frame underflow");
        // Shrink the register file back to the caller's window top.
        self.regs.truncate(finished.base);
        if self.frames.len() == stop_depth {
            return true;
        }
        let caller_base = self.frames.last().unwrap().base;
        self.regs[caller_base + finished.ret_dst as usize] = ret;
        false
    }

    /// Render a thrown value for the UNCAUGHT-throw message (the `Outcome.error`
    /// string). An Error-like object (`{message,â€¦}` or one with a `.message`)
    /// prints `name: message`; otherwise the value's string form. Catchable
    /// throws bind the real `Value`, so this is only the top-level report.
    pub(crate) fn throw_message(&self, v: Value) -> String {
        if v.is_heap() {
            if let HeapObj::Object(_) = self.heap.get(v.heap_index()) {
                let idx = v.heap_index();
                // `name`: own/inherited "name", else the constructor's name — so a
                // nameless user error (e.g. the harness Test262Error, which sets only
                // `.message` but has constructor.name "Test262Error") reports
                // "Test262Error: …" rather than "Error: …", matching V8/Node (and
                // letting a negative-test type substring-match its stderr).
                let name = self
                    .read_data_prop(idx, "name")
                    .map(|n| self.display(n))
                    .or_else(|| {
                        self.read_data_prop(idx, "constructor")
                            .map(|c| self.callable_name(c))
                            .filter(|s| !s.is_empty())
                    });
                let msg = self.read_data_prop(idx, "message").map(|m| self.display(m));
                return match (name, msg) {
                    (Some(n), Some(m)) => format!("{n}: {m}"),
                    (Some(n), None) => n,
                    (None, Some(m)) => format!("Error: {m}"),
                    _ => self.display(v),
                };
            }
        }
        format!("Uncaught {}", self.display(v))
    }

    // â”€â”€ register access â”€â”€
    //
    // Unchecked: the compiler allocates `reg_count` registers per function and
    // never emits a register index â‰¥ `reg_count` (it tracks a `max_reg`
    // high-water mark), and every frame resizes `self.regs` to
    // `base + reg_count` on entry â€” so `base + r` is always in bounds. We index
    // `self.regs` freshly each call (no cached pointer), so a reallocation of
    // the register Vec by a re-entrant call/alloc is handled correctly. The
    // `debug_assert!` turns any compiler bug into a loud test failure in debug
    // builds while release elides the bounds check.
    #[inline(always)]
    pub(crate) fn get(&self, base: usize, r: u16) -> Value {
        debug_assert!((base + r as usize) < self.regs.len(), "reg read out of bounds");
        unsafe { *self.regs.get_unchecked(base + r as usize) }
    }
    #[inline(always)]
    pub(crate) fn set(&mut self, base: usize, r: u16, v: Value) {
        debug_assert!((base + r as usize) < self.regs.len(), "reg write out of bounds");
        unsafe {
            *self.regs.get_unchecked_mut(base + r as usize) = v;
        }
    }

    // â”€â”€ call setup â”€â”€

    /// Resolve a value to a callable function id, or throw a TypeError.
    /// The cell heap-index captured at upvalue slot `idx` of the closure heap
    /// object `closure`. Panics only on a miscompiled program (an UpvalGet in a
    /// frame with no closure, or an out-of-range slot), which the compiler must
    /// not emit.
    #[inline]
    pub(crate) fn closure_upvalue(&self, closure: u32, idx: u16) -> u32 {
        match self.heap.get(closure) {
            HeapObj::Closure { upvalues, .. } => upvalues[idx as usize],
            _ => panic!("UpvalGet/Set in a frame without a closure"),
        }
    }

    /// Capture each upvalue cell from the defining frame: a ParentLocal reads the
    /// boxed cell from a local register; a ParentUpval forwards one of the current
    /// closure's own cells. Mirrors the `MakeClosure` op.
    pub(crate) fn capture_upvalue_cells(
        &self,
        sources: &[UpvalSource],
        base: usize,
        cur_closure: u32,
    ) -> Vec<u32> {
        sources
            .iter()
            .map(|src| match *src {
                UpvalSource::ParentLocal(reg) => self.get(base, reg).heap_index(),
                UpvalSource::ParentUpval(idx) => self.closure_upvalue(cur_closure, idx),
            })
            .collect()
    }

    /// Materialize a class member function as a callable value: a plain `Func`
    /// when it captures nothing, else a `Closure` over the defining frame's cells.
    pub(crate) fn materialize_callable(&mut self, fid: u32, base: usize, cur_closure: u32) -> Value {
        let sources = self.func(fid as usize).upvalues.clone();
        if sources.is_empty() {
            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
        } else {
            let cells = self.capture_upvalue_cells(&sources, base, cur_closure);
            Value::heap(self.heap.alloc(HeapObj::Closure { func: fid, upvalues: cells, this_val: Value::UNDEFINED }))
        }
    }

    /// Resolve a value to `(func_id, closure_heap_idx)`. `closure_heap_idx` is
    /// the value's heap index when it is a `Closure` (so the frame can reach its
    /// captured cells), or `NO_CLOSURE` for a plain `Func`.
    pub(crate) fn resolve_callable(&self, v: Value) -> Result<(u32, u32), Thrown> {
        if v.is_heap() {
            let idx = v.heap_index();
            match self.heap.get(idx) {
                HeapObj::Func(id) => return Ok((*id, NO_CLOSURE)),
                HeapObj::Closure { func, .. } => return Ok((*func, idx)),
                _ => {}
            }
        }
        Err(Thrown(format!("TypeError: {} is not a function", self.display(v))))
    }

    /// Push a new frame for `func_id`, binding `this_val` to register 0 and the
    /// `argc` arguments (staged at `caller_base + arg_base ..`) into registers
    /// `1..`. Records the caller's resume ip and result register.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn setup_call(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
        callee_val: Value,
    ) -> Result<(), Thrown> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = self.func(func_id as usize);
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;
        let is_strict = proto.is_strict;
        let lexical_this = proto.lexical_this;
        // An arrow binds the `this` it captured lexically (ignoring the supplied
        // one) and skips OrdinaryCallBindThis. Otherwise OrdinaryCallBindThis:
        // a sloppy callee invoked with a nullish `this` (e.g. a bare `f()`) binds
        // the global object instead.
        let this_val = if lexical_this && closure != NO_CLOSURE {
            self.rebind_arrow_this(func_id, closure, this_val)
        } else if !is_strict && this_val.is_nullish() && self.global_this != 0 {
            Value::heap(self.global_this)
        } else if !is_strict && !self.is_object_value(this_val) && self.global_this != 0 {
            // OrdinaryCallBindThis: a sloppy function boxes a primitive `this`.
            self.to_object(this_val)?
        } else {
            this_val
        };

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) â€” throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);

        // Register 0 = `this`; parameters at registers 1..1+param_count.
        self.regs[new_base] = this_val;
        let n = (argc as usize).min(callee_params);
        for i in 0..n {
            let v = self.regs[caller_base + arg_base as usize + i];
            self.regs[new_base + 1 + i] = v;
        }
        // Rest parameter: collect args beyond the fixed params into a fresh array.
        if let Some(rreg) = self.func(func_id as usize).rest_reg {
            let extra: Vec<Value> = ((arg_base as usize + callee_params)
                ..(arg_base as usize + argc as usize))
                .map(|i| self.regs[caller_base + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: an array of ALL actual args (a function that references it).
        if let Some(areg) = self.func(func_id as usize).arguments_reg {
            let argsv: Vec<Value> = (0..argc as usize)
                .map(|i| self.regs[caller_base + arg_base as usize + i])
                .collect();
            let is_strict = self.func(func_id as usize).is_strict;
            let arr = self.build_arguments_object(argsv, callee_val, is_strict);
            self.regs[new_base + areg as usize] = arr;
        }

        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame { super_done: false, eval_scope: u32::MAX, func: func_id, base: new_base, ip: 0, ret_dst: dst, closure, handlers: Vec::new(), new_target, callee: callee_val });
        Ok(())
    }

}
