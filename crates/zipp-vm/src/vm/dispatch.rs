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
    /// — e.g. the caller of a builtin callback — can still catch it).
    pub(crate) fn run_loop(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            match self.dispatch_body(stop_depth) {
                Ok(v) => return Ok(v),
                Err(t) => {
                    let tv = match self.pending_throw {
                        Some(v) => v,
                        None => {
                            // Internal error (TypeError/RangeError/…) with no
                            // explicit thrown value: synthesise a real Error
                            // object so `catch (e)` sees `e.name`/`e.message` and
                            // `e instanceof TypeError`, matching JS.
                            let v = self.alloc_error_from_message(&t.0);
                            self.pending_throw = Some(v);
                            v
                        }
                    };
                    if self.unwind_to_handler(tv, stop_depth) {
                        self.pending_throw = None; // caught — resume at catch
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
    /// into its registers and resumes at the finally target — `EndFinally`
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
    /// pending — the caller performs the real leave (pop the frame).
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
            let code: *const Vec<Instr> = &self.program.functions[func_id as usize].code;
            // SAFETY: `code` borrows immutable program data that outlives the
            // loop; we never mutate program functions during execution.
            let code: &Vec<Instr> = unsafe { &*code };

            // ── JIT tier ──
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
            // interpreter re-enters native, recurses 256, deopts… forever,
            // because the per-call native depth counter resets each return and
            // interpreter frames never reach MAX_FRAMES. Staying interpreted in
            // that subtree lets frames accumulate monotonically → RangeError.
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            if ip == 0
                && self.jit_enabled
                && self.jit_recurse_depth == 0
                && !self.program.functions[func_id as usize].is_generator
                && !self.program.functions[func_id as usize].is_async
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
                    //     helper signalled deopt with `pending_throw` set — the
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
                        &self.program.functions[func_id as usize];
                    // SAFETY: program functions are immutable during execution.
                    let proto_ref = unsafe { &*proto };
                    // The self-function's current global Value (a heap Func),
                    // stable since hoist_functions ran at startup. Embedded so a
                    // JIT'd `LoadGlobal(self_slot)` stores the REAL function (not
                    // a placeholder) — required for a deopted self-Call to
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
                        let v = self.program.functions[func_id as usize].constants[idx as usize];
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
                            // Referenced but never declared → ReferenceError.
                            let name = self
                                .program
                                .global_names
                                .get(idx as usize)
                                .map(|s| s.as_str())
                                .unwrap_or("?");
                            return Err(Thrown(format!("ReferenceError: {name} is not defined")));
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobalOrUndefined { dst, idx } => {
                        let v = self.globals[idx as usize];
                        let v = if v.is_uninitialized() { Value::UNDEFINED } else { v };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::StoreGlobal { idx, src } => {
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
                    // Identical to `Add` — a JIT routing hint only (see bytecode).
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
                        // (unary plus is not defined on BigInt); else ToNumber.
                        let r = if va.is_number() {
                            va
                        } else if self.bigint_value(va).is_some() {
                            return Err(Thrown("TypeError: Cannot convert a BigInt value to a number".into()));
                        } else {
                            Value::num(self.to_number_coerce(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Neg { dst, a } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(va.as_int() as f64)),
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
                        // BigInt bitwise: &/|/^/<</>> on two BigInts; `>>>` is not
                        // defined for BigInt (TypeError); mixing → TypeError.
                        if self.bigint_value(va).is_some() || self.bigint_value(vb).is_some() {
                            let bop = match op {
                                B::And => BigOp::And,
                                B::Or => BigOp::Or,
                                B::Xor => BigOp::Xor,
                                B::Shl => BigOp::Shl,
                                B::Shr => BigOp::Shr,
                                B::Ushr => {
                                    return Err(Thrown(
                                        "TypeError: BigInts have no unsigned right shift, use >> instead"
                                            .into(),
                                    ))
                                }
                            };
                            if let Some(bv) = self.bigint_binop(bop, va, vb)? {
                                self.set(base, dst, bv);
                                ip += 1;
                                continue;
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
                                // u32 may exceed i32::MAX → keep numeric range.
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
                            Value::num(self.to_number_coerce(va)?.powf(self.to_number_coerce(vb)?))
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
                        } else {
                            Value::num(self.to_number_coerce(va)? + imm as f64)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Lt { dst, a, b } => {
                        let r = self.cmp_lt(base, a, b)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Le { dst, a, b } => {
                        let r = self.cmp_le(base, a, b)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Gt { dst, a, b } => {
                        let r = self.cmp_lt(base, b, a)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ge { dst, a, b } => {
                        let r = self.cmp_le(base, b, a)?;
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
                        let is_arr = v.is_heap()
                            && matches!(self.heap.get(v.heap_index()), HeapObj::Array(_));
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
                            // A generator or a custom iterable (object) is drained
                            // via the iterator protocol (iterate_to_vec also errors
                            // for a plain, non-iterable object, as a spread should).
                            if vv.is_heap()
                                && matches!(
                                    self.heap.get(vv.heap_index()),
                                    HeapObj::Generator { .. }
                                        | HeapObj::Object(_)
                                        | HeapObj::Iterator { .. }
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
                            // Materialize the spread source's elements (array/set →
                            // elements; string → chars; map → [k,v] entries) WITHOUT
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
                                        let elems = items.clone();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Str(_) | HeapObj::Cons { .. } => {
                                        chars = Some(self.heap.str_cow(vv.heap_index()).unwrap().chars().collect());
                                    }
                                    HeapObj::Map { keys, vals } => {
                                        map_pairs = Some(keys.iter().copied().zip(vals.iter().copied()).collect());
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
                        let prog: &'p Program = self.program;
                        let consts = &prog.functions[func_id as usize].string_constants;
                        let excluded =
                            &consts[exclude_start as usize..exclude_start as usize + exclude_count as usize];
                        // Copy src's own keys except the destructured siblings.
                        let pairs: Vec<(String, Value)> = if s.is_heap() {
                            match self.heap.get(s.heap_index()) {
                                HeapObj::Object(map) => spec_key_order(&map.keys)
                                    .into_iter()
                                    .filter(|&i| map.attrs[i].enumerable)
                                    .map(|i| (map.keys[i].clone(), map.vals[i]))
                                    .filter(|(k, _)| !excluded.iter().any(|e| e == k))
                                    .collect(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut m = ObjMap::new();
                        for (k, v) in pairs {
                            m.set(&k, v);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClass { dst, class_id, parent } => {
                        let cd = self.program.classes[class_id as usize].clone();
                        let parent_idx = parent.and_then(|p| {
                            let pv = self.get(base, p);
                            pv.is_heap().then(|| pv.heap_index())
                        });
                        // Materialize each method as a Func value once; instances
                        // share these (no per-access alloc, no per-instance copy).
                        let mk = |heap: &mut Heap, defs: &[(String, u32)]| -> Vec<(String, Value)> {
                            defs.iter()
                                .map(|(n, fid)| {
                                    (n.clone(), Value::heap(heap.alloc(HeapObj::Func(*fid))))
                                })
                                .collect()
                        };
                        let methods = mk(&mut self.heap, &cd.methods);
                        let getters = mk(&mut self.heap, &cd.getters);
                        let setters = mk(&mut self.heap, &cd.setters);
                        let static_getters = mk(&mut self.heap, &cd.static_getters);
                        let static_setters = mk(&mut self.heap, &cd.static_setters);
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
                            let fv = Value::heap(self.heap.alloc(HeapObj::Func(*fid)));
                            statics.define(n, fv, method_attr);
                        }
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
                            computed_field_keys: Vec::new(),
                            source: cd.source,
                        }))));
                        // Remember it so `super` in a derived class can reach it.
                        self.class_values[class_id as usize] = Some(v);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ClassAddMember { class, key, func, kind } => {
                        let cv = self.get(base, class);
                        let k = self.get(base, key);
                        let kstr = self.display(k);
                        let fv = Value::heap(self.heap.alloc(HeapObj::Func(func)));
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            if kind == 3 {
                                // Static method — non-enumerable (like a named one).
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
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            c.computed_field_keys.push(kv);
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
                            self.set_index(this, key, v)?;
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
                        self.run_class_ctor(parent, this, &args)?;
                        ip += 1;
                    }
                    Instr::SuperCtorSpread { home_class_id, args } => {
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: superclass is not a constructor".into()))?;
                        let this = self.get(base, 0);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        self.run_class_ctor(parent, this, &arg_vec)?;
                        ip += 1;
                    }
                    Instr::SuperMethod { dst, home_class_id, name, arg_base, argc } => {
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
                        // Find the method up the parent's class chain.
                        let mut method = None;
                        let mut cur = parent.is_heap().then(|| parent.heap_index());
                        while let Some(cidx) = cur {
                            match self.heap.get(cidx) {
                                HeapObj::Class(c) => {
                                    if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                                        method = Some(*v);
                                        break;
                                    }
                                    cur = c.parent;
                                }
                                _ => break,
                            }
                        }
                        let m = method.ok_or_else(|| {
                            Thrown(format!("TypeError: super.{key} is not a function"))
                        })?;
                        let this = self.get(base, 0);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperGet { dst, home_class_id, name } => {
                        // `super.name` read: resolve on the superclass's prototype
                        // with `this` = the current receiver (so a getter sees it).
                        let key =
                            self.program.functions[func_id as usize].string_constants[name as usize].clone();
                        let parent = self
                            .super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
                        let proto = self.prototype_of(parent).unwrap_or(Value::UNDEFINED);
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &key, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperGetComputed { dst, home_class_id, key } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let parent = self
                            .super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
                        let proto = self.prototype_of(parent).unwrap_or(Value::UNDEFINED);
                        let this = self.get(base, 0);
                        let r = self.get_member(proto, &ks, this)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SuperMethodComputed { dst, home_class_id, key, arg_base, argc } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let parent = self
                            .super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
                        let proto = self.prototype_of(parent).unwrap_or(Value::UNDEFINED);
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
                            self.program.functions[func_id as usize].string_constants[name as usize].clone();
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        self.super_set(home_class_id, &key, this, v)?;
                        ip += 1;
                    }
                    Instr::SuperSetComputed { home_class_id, key, val } => {
                        let kv = self.get(base, key);
                        let ks = self.to_property_key(kv)?;
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        self.super_set(home_class_id, &ks, this, v)?;
                        ip += 1;
                    }
                    Instr::ArrayCtor { dst, arg_base, argc } => {
                        let arr = if argc == 1 && self.get(base, arg_base).is_number() {
                            // `Array(n)` → n empty slots (undefined).
                            let n = self.get(base, arg_base).as_f64();
                            if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                                return Err(Thrown("RangeError: Invalid array length".into()));
                            }
                            if n as usize > super::MAX_DENSE_ARRAY_LEN {
                                return Err(Thrown(
                                    "RangeError: array length exceeds the engine's dense-array limit".into(),
                                ));
                            }
                            vec![Value::UNDEFINED; n as usize]
                        } else {
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect()
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewMap { dst, src } => {
                        let (mut keys, mut vals): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                // Each iterated entry is a [key, value]-indexable.
                                for e in self.iterate_to_vec(sv)? {
                                    let k = normalize_zero(self.get_index(e, Value::int(0))?);
                                    let v = self.get_index(e, Value::int(1))?;
                                    match keys.iter().position(|kk| self.same_value_zero(*kk, k)) {
                                        Some(i) => vals[i] = v,
                                        None => {
                                            keys.push(k);
                                            vals.push(v);
                                        }
                                    }
                                }
                            }
                        }
                        let m = Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }));
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewSet { dst, src } => {
                        let mut items: Vec<Value> = Vec::new();
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    let v = normalize_zero(e);
                                    if !items.iter().any(|x| self.same_value_zero(*x, v)) {
                                        items.push(v);
                                    }
                                }
                            }
                        }
                        let s = Value::heap(self.heap.alloc(HeapObj::Set(items)));
                        self.set(base, dst, s);
                        ip += 1;
                    }
                    Instr::NewWeakMap { dst, src } => {
                        let (mut keys, mut vals): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    let k = self.get_index(e, Value::int(0))?;
                                    let v = self.get_index(e, Value::int(1))?;
                                    if !self.is_object_value(k) {
                                        return Err(Thrown(
                                            "TypeError: Invalid value used as weak map key".into(),
                                        ));
                                    }
                                    match keys.iter().position(|kk| self.same_value_zero(*kk, k)) {
                                        Some(i) => vals[i] = v,
                                        None => {
                                            keys.push(k);
                                            vals.push(v);
                                        }
                                    }
                                }
                            }
                        }
                        let m = Value::heap(self.heap.alloc(HeapObj::WeakMap { keys, vals }));
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewWeakSet { dst, src } => {
                        let mut items: Vec<Value> = Vec::new();
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    if !self.is_object_value(e) {
                                        return Err(Thrown(
                                            "TypeError: Invalid value used in weak set".into(),
                                        ));
                                    }
                                    if !items.iter().any(|x| self.same_value_zero(*x, e)) {
                                        items.push(e);
                                    }
                                }
                            }
                        }
                        let s = Value::heap(self.heap.alloc(HeapObj::WeakSet(items)));
                        self.set(base, dst, s);
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
                                // Number box: ToNumber(arg) (no arg -> +0).
                                let n = match arg {
                                    Some(a) => self.to_number(self.get(base, a))?,
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
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
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
                                let s = self.display(a0);
                                let radix = if argc >= 2 {
                                    self.to_number(self.get(base, arg_base + 1))? as i32
                                } else {
                                    0
                                };
                                Value::num(parse_int(&s, radix))
                            }
                            G::ParseFloat => Value::num(parse_float(&self.display(a0))),
                            // isNaN/isFinite coerce and never throw for the values
                            // in this subset; treat any coercion failure as NaN.
                            G::IsNaN => {
                                Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_nan())
                            }
                            G::IsFinite => {
                                Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_finite())
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
                    Instr::HasProp { dst, key, obj } => {
                        let k = self.get(base, key);
                        let o = self.get(base, obj);
                        // ToPropertyKey: an object key is ToString-coerced (toString/
                        // valueOf), not rendered "[object Object]".
                        let k = self.coerce_index_key(k)?;
                        // Proxy `has` trap (or fall through to the target).
                        let r = if let Some((target, handler, revoked)) =
                            o.is_heap().then(|| self.proxy_parts(o.heap_index())).flatten()
                        {
                            if revoked {
                                return Err(Thrown("TypeError: Cannot perform 'has' on a revoked proxy".into()));
                            }
                            match self.proxy_trap(handler, "has")? {
                                Some(trap) => {
                                    let ks = self.key_of(k);
                                    let kv = self.key_to_value(&ks);
                                    let res = self.call_value(trap, handler, &[target, kv])?;
                                    self.truthy(res)
                                }
                                None => self.has_property(target, k),
                            }
                        } else {
                            self.has_property(o, k)
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::InstanceOfDyn { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let c = self.get(base, ctor);
                        // A class uses its `extends` chain; a constructor FUNCTION
                        // checks whether `F.prototype` is in `v`'s prototype chain.
                        let kind = if c.is_heap() {
                            match self.heap.get(c.heap_index()) {
                                HeapObj::Class(_) => 1u8,
                                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } => 2,
                                // Built-in constructor globals (Map/Set/Date/WeakMap/…)
                                // are objects but constructable: use prototype-chain check.
                                HeapObj::Object(m) if m.is_ctor => 2,
                                _ => 0,
                            }
                        } else {
                            0
                        };
                        let r = match kind {
                            1 => v.is_heap() && self.instance_of_class(v, c.heap_index()),
                            2 => self.instanceof_via_proto(v, c),
                            _ => false,
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
                                let s: String = args
                                    .iter()
                                    .map(|&v| {
                                        // ToUint16 of each code unit.
                                        let u = to_uint32(self.to_number(v).unwrap_or(0.0)) as u16;
                                        char::from_u32(u as u32).unwrap_or('\u{FFFD}')
                                    })
                                    .collect();
                                self.alloc_str(s)
                            }
                            S::ObjectAssign => self.object_assign(&args)?,
                            S::ObjectFromEntries => {
                                let entries = self.iterate_to_vec(a0)?;
                                let mut map = ObjMap::new();
                                for e in entries {
                                    let kv = self.get_index(e, Value::int(0))?;
                                    let k = self.display(kv);
                                    let v = self.get_index(e, Value::int(1))?;
                                    map.set(&k, v);
                                }
                                Value::heap(self.heap.alloc(HeapObj::Object(map)))
                            }
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
                            S::PromiseAll => self.promise_combine(crate::heap::CombKind::All, a0)?,
                            S::PromiseAllSettled => {
                                self.promise_combine(crate::heap::CombKind::AllSettled, a0)?
                            }
                            S::PromiseRace => self.promise_combine(crate::heap::CombKind::Race, a0)?,
                            S::PromiseAny => self.promise_combine(crate::heap::CombKind::Any, a0)?,
                            S::ObjectDefineProperty => {
                                let key =
                                    self.to_property_key(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_property(a0, &key, desc)?;
                                a0
                            }
                            S::ObjectDefineProperties => {
                                let props = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_properties(a0, props)?;
                                a0
                            }
                            S::ObjectGetOwnPropertyDescriptor => {
                                let key =
                                    self.to_property_key(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                                self.object_get_own_property_descriptor(a0, &key)
                            }
                            S::ObjectGetOwnPropertyNames => self.object_own_property_names(a0),
                            S::ObjectGetPrototypeOf => self.object_get_prototype_of(a0),
                            S::ObjectCreate => {
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
                        let out = self.array_from(sv, fnv)?;
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
                        // A backward jump is a loop back-edge — poll the GC here so
                        // a tight allocating loop (which never leaves this inner
                        // loop) still gets collected. Safe: all live Values are in
                        // regs, and gc_lock guards any native built-in up-stack.
                        if t < ip {
                            self.maybe_gc();
                        }
                        // ── OSR tier ── a backward jump is a loop back-edge. After
                        // the region heats up, compile `[target, ip]` (the loop
                        // body, headed at `target`) and run it natively; the
                        // native code returns the ip to resume at (a clean loop
                        // exit or a guard bail). Gated like the function JIT:
                        // enabled, and not inside a native self-recursion.
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        if self.jit_enabled && self.jit_recurse_depth == 0 && t < ip {
                            if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                ip = resume;
                                continue;
                            }
                            if self.jit.record_region(func_id, t as u32) {
                                let proto: *const crate::bytecode::FuncProto =
                                    &self.program.functions[func_id as usize];
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
                        let r = self.cmp_lt(base, a, b)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLe { a, b, target } => {
                        let r = self.cmp_le(base, a, b)?;
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
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectKeys { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = self.object_enum_own(o, EnumWhat::Keys);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectValues { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = self.object_enum_own(o, EnumWhat::Values);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectEntries { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = self.object_enum_own(o, EnumWhat::Entries);
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
                                // for-of over a Map/Set iterates `size` slots.
                                HeapObj::Map { keys, .. } => len_value(keys.len()),
                                HeapObj::Set(items) => len_value(items.len()),
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
                        let sources = &self.program.functions[func_id as usize].upvalues;
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
                            self.heap.alloc(HeapObj::Closure { func: func_id, upvalues: cells }),
                        );
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeCell { reg } => {
                        let v = self.get(base, reg);
                        let cell = self.heap.alloc(HeapObj::Cell(v));
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::CellGet { dst, cell } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.heap.cell_get(cell_idx);
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
                    Instr::NewError { dst, kind, arg } => {
                        let msg = arg.map(|r| self.get(base, r));
                        let v = self.make_error(kind, msg);
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
                        let n = self.to_bigint(a)?;
                        let v = self.make_bigint(n);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewRegExp { dst, pattern, flags } => {
                        let p = self.get(base, pattern);
                        let f = self.get(base, flags);
                        let v = self.build_regexp(p, f)?;
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
                    Instr::SetIndex { obj, key, val } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let v = self.get(base, val);
                        self.set_index(o, k, v)?;
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
                    Instr::GetProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        let r = self.get_prop(o, &key)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SetProp { obj, name, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        self.set_prop(o, &key, v)?;
                        ip += 1;
                    }
                    Instr::DeleteProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        let r = self.delete_property(o, &key)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DeleteIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let ks = self.to_property_key(k)?; // ToPropertyKey (symbol → prop_key, object → ToString)
                        let r = self.delete_property(o, &ks)?;
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
                            // %Function.prototype% is also a callable (returns undefined).
                            if matches!(
                                self.heap.get(callee_v.heap_index()),
                                HeapObj::Bound { .. } | HeapObj::Native(_)
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
                        // (e.g. an Intl service ctor without `new`).
                        if callee_v.is_heap()
                            && matches!(self.heap.get(callee_v.heap_index()), HeapObj::Object(m) if m.is_ctor)
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
                        if self.program.functions[fid as usize].is_generator
                            && self.program.functions[fid as usize].is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator function returns a Generator object, unrun.
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async function runs to its first `await` then returns
                        // its result Promise.
                        if self.program.functions[fid as usize].is_async {
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
                        )?;
                        break;
                    }

                    Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        // `program` outlives the VM, so borrow the method name
                        // with the program's lifetime (NOT self's) — avoids
                        // cloning the name string on every method call (a heap
                        // alloc per `a.push(i)` / `a.map(cb)` etc.).
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        // Hot fast path: `arr.push(x)` — the most common
                        // per-element array idiom. Append directly, skipping the
                        // try_builtin_method → dispatch_builtin_method → array_method
                        // layering (and the args-gather), then return the new length.
                        if argc == 1 && key == "push" && recv.is_heap() {
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
                        let prop = self.get_prop(recv, key)?;
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
                        if self.program.functions[fid as usize].is_generator
                            && self.program.functions[fid as usize].is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv);
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator method returns a Generator object, unrun.
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async method runs to its first `await` then returns
                        // its result Promise.
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
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
                        if self.program.functions[fid as usize].is_generator
                            && self.program.functions[fid as usize].is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv);
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
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
                        // entered: 1 = return (re-leave through any outer finally,
                        // else return), 2 = throw (re-raise), else 0 = normal.
                        match self.regs[base + kind_reg as usize].as_int() {
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
                            _ => {
                                ip += 1;
                            }
                        }
                    }
                    Instr::SetRaw { arr, raw } => {
                        let a = self.get(base, arr);
                        let r = self.get(base, raw);
                        if a.is_heap() {
                            self.template_raws.insert(a.heap_index(), r);
                        }
                        ip += 1;
                    }
                    Instr::GetIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::GetAsyncIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_async_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::IterToArray { dst, src, count } => {
                        let s = self.get(base, src);
                        let a = self.iter_to_array(s, count)?;
                        self.set(base, dst, a);
                        ip += 1;
                    }
                    Instr::Random { dst } => {
                        // xorshift64* → a uniform double in [0, 1) (top 53 bits).
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
                        self.frames.pop();
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::Await { val, .. } => {
                        // Suspend the async activation: pop the frame ENTRY but
                        // leave its register window live at the top of `self.regs`
                        // for `drive_async` to park into the heap AsyncState. Unlike
                        // a generator yield, we CAPTURE the frame's `try` handlers
                        // (carried in `pending_await`) so they can be restored on
                        // resume — letting `try { await p } catch (e)` see a
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
                        // GetIterator): pull the next result via `.next()`. Lazy —
                        // a `break` simply stops calling it.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Object(_) | HeapObj::Iterator { .. } | HeapObj::IterHelper { .. }) {
                            let next = self.get_prop(it, "next")?;
                            if self.is_callable(next) {
                                let res = self.call_value(next, it, &[])?;
                                let done = self.get_prop(res, "done")?;
                                let val = self.get_prop(res, "value")?;
                                self.set(base, value_dst, val);
                                self.set(base, done_dst, done);
                                ip += 1;
                                continue;
                            }
                        }
                        // Array/Set element, string char, or Map [k,v] at the cursor.
                        let cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        let len = match self.heap.get(it.heap_index()) {
                            HeapObj::Array(items) => items.len(),
                            HeapObj::Set(items) => items.len(),
                            HeapObj::Str(s) => s.char_len,
                            HeapObj::Cons { len, .. } => *len,
                            HeapObj::Map { keys, .. } => keys.len(),
                            HeapObj::TypedArray { length, .. } => *length,
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
                            // Array/Set element, string char, Map [k,v] — positional,
                            // wrapped in a {value, done} the loop awaits (a tick).
                            _ => {
                                let cursor = array_index(self.get(base, idx)).unwrap_or(0);
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
    /// pointer taken here and used ONLY for the duration of the call — nothing
    /// in between can resize `self.regs` (the JIT subset issues no calls/allocs).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_run_jit(&mut self, func_id: u32, base: usize) -> Option<(Value, u32)> {
        let jitfn = self.jit.get(func_id)? as *const crate::codegen::JitFn;
        // SAFETY: `jitfn` points into self.jit.compiled (stable for the call).
        // `regs_ptr` is valid for the frame's reg_count slots. A self-call op
        // routes through `jit_self_call` (passed the `vm` pointer below) which
        // may resize self.regs for the recursive frame — but it RESTORES regs to
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

        // ── pre-run sync ── load the promoted object's fields into the scratch
        // pool globals the native code reads as ordinary globals.
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.program.functions[p.func_id as usize].string_constants
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

        // ── post-run sync ── flush the pool globals back to the object's fields,
        // so the interpreter (which resumes on the ORIGINAL bytecode, reading the
        // object) sees consistent values. Runs on EVERY exit (clean or bail).
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.program.functions[p.func_id as usize].string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.globals[slot as usize];
                let _ = self.set_prop(obj, &key, v);
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
    /// string). An Error-like object (`{message,…}` or one with a `.message`)
    /// prints `name: message`; otherwise the value's string form. Catchable
    /// throws bind the real `Value`, so this is only the top-level report.
    pub(crate) fn throw_message(&self, v: Value) -> String {
        if v.is_heap() {
            if let HeapObj::Object(map) = self.heap.get(v.heap_index()) {
                let name = map.get("name").map(|n| self.display(n));
                let msg = map.get("message").map(|m| self.display(m));
                return match (name, msg) {
                    (Some(n), Some(m)) => format!("{n}: {m}"),
                    (None, Some(m)) => format!("Error: {m}"),
                    _ => self.display(v),
                };
            }
        }
        format!("Uncaught {}", self.display(v))
    }

    // ── register access ──
    //
    // Unchecked: the compiler allocates `reg_count` registers per function and
    // never emits a register index ≥ `reg_count` (it tracks a `max_reg`
    // high-water mark), and every frame resizes `self.regs` to
    // `base + reg_count` on entry — so `base + r` is always in bounds. We index
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

    // ── call setup ──

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
    ) -> Result<(), Thrown> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = &self.program.functions[func_id as usize];
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
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
        if let Some(rreg) = self.program.functions[func_id as usize].rest_reg {
            let extra: Vec<Value> = ((arg_base as usize + callee_params)
                ..(arg_base as usize + argc as usize))
                .map(|i| self.regs[caller_base + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: an array of ALL actual args (a function that references it).
        if let Some(areg) = self.program.functions[func_id as usize].arguments_reg {
            let argsv: Vec<Value> = (0..argc as usize)
                .map(|i| self.regs[caller_base + arg_base as usize + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(argsv)));
            self.regs[new_base + areg as usize] = arr;
        }

        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: dst, closure, handlers: Vec::new() });
        Ok(())
    }

}
