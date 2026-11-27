//! The interpreter's cold instructions, out of line.
//!
//! `dispatch_body` was 234 KB of machine code, and more than half of it was
//! arms no measured workload executes: class and decorator construction,
//! direct `eval`, dynamic `import`, async iteration, the spread forms of
//! `super` calls and the like. Inline, they shared one function, and so one
//! register allocation and one instruction-cache footprint, with the dozen
//! arms every loop runs. They now run here, reached from the loop's `_` arm.
//!
//! Each arm is the loop's former arm verbatim, with its exits spelled for a
//! function: falling through or `continue` returns [`ColdFlow::Next`] with the
//! instruction to run next, `break` (a frame was pushed or popped) returns
//! [`ColdFlow::Frame`], and `return Ok(v)` returns [`ColdFlow::Done`]. The
//! loop's locals arrive by value, so none of them has its address taken in
//! the loop.
#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// What a cold arm hands back to the dispatch loop.
pub(crate) enum ColdFlow {
    /// Go on in the same frame at this instruction.
    Next(usize),
    /// A frame was pushed or popped: re-read the current frame (the arm's
    /// `break`).
    Frame,
    /// Leave the loop with this value (the arm's `return Ok`): a `yield*`
    /// suspending its generator.
    Done(Value),
}

/// The longest direct-eval source `Vm::direct_eval_cache` keeps, and how
/// many sources it keeps.
const DIRECT_EVAL_CACHE_MAX_SOURCE: usize = 1024;
const DIRECT_EVAL_CACHE_MAX_ENTRIES: usize = 4096;

impl<'p> Vm<'p> {
    /// `ObjectRest`'s value (`let {a, ...rest} = src`) over the window at
    /// `base`: the interpreter's arm, and Tier C's (`jit_cold_op_ic`).
    pub(crate) fn object_rest_value(
        &mut self,
        base: usize,
        func_id: u32,
        src: u16,
        exclude_start: u32,
        exclude_count: u32,
    ) -> Result<Value, Thrown> {
        let s = self.get(base, src);
        let consts = &self.func(func_id as usize).string_constants;
        let excluded = &consts[exclude_start as usize..exclude_start as usize + exclude_count as usize];
        // Only an ORDINARY object (or a string, handled below) can be walked
        // straight off its ObjMap. Every other source — a Proxy's traps, but
        // equally an Array's or a TypedArray's own INDEX properties, which live
        // outside any ObjMap — needs the general CopyDataProperties (ownKeys →
        // gopd → get per key, excluded keys never probed). `let {...r} =
        // ['x','y']` produced `{}`.
        if s.is_heap()
            && !matches!(
                self.heap.get(s.heap_index()),
                HeapObj::Object(_) | HeapObj::Str(_) | HeapObj::Cons { .. }
            )
        {
            let m = self.copy_data_properties_rest(s, excluded)?;
            return Ok(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m)))));
        }
        // Copy src's own enumerable keys except the destructured siblings —
        // Getting each (CopyDataProperties), so a getter's VALUE is copied (not
        // the accessor) and a throw propagates.
        let keys: Vec<String> = if s.is_heap() {
            match self.heap.get(s.heap_index()) {
                HeapObj::Object(map) => spec_key_order_map(&map)
                    .into_iter()
                    .filter(|&i| map.attr_at(i).enumerable)
                    .map(|i| map.key_at(i).to_owned())
                    .filter(|k| !excluded.iter().any(|e| e == k))
                    .collect(),
                // A STRING source: its UNITS are own enumerable index
                // properties (CopyDataProperties copies them).
                HeapObj::Str(st) => (0..st.units())
                    .map(|i| i.to_string())
                    .filter(|k| !excluded.iter().any(|e| e == k))
                    .collect(),
                HeapObj::Cons { len, .. } => (0..*len)
                    .map(|i| i.to_string())
                    .filter(|k| !excluded.iter().any(|e| e == k))
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let m = self.object_rest_plain(s, keys)?;
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m)))))
    }

    /// `MathSpread`'s value (`Math.max(...xs)`) over the window at `base`:
    /// the interpreter's arm, and Tier C's (`jit_cold_op_ic`).
    pub(crate) fn math_spread_value(
        &mut self,
        base: usize,
        op: crate::bytecode::MathFn,
        callee: u16,
        this_v: u16,
        args: u16,
    ) -> Result<Value, Thrown> {
        use crate::bytecode::MathFn as M;
        let av = self.get(base, args);
        let elems = self.array_snapshot(av.heap_index());
        let callee = self.get(base, callee);
        let this_v = self.get(base, this_v);
        let native_id = native::MATH_METHOD_BASE + op as u16;
        if !self.static_builtin_is_intrinsic("Math", native_id, callee, this_v) {
            return self.call_value(callee, this_v, &elems);
        }
        // The variadic ops reduce exactly like a direct call (`eval_math_args`:
        // -0 < +0 ordering, a ±Infinity hypot argument beating NaN);
        // f64::max/min leave the sign of a zero tie unspecified.
        let r = match op {
            // `eval_math_args` is the ONE reduction: Hypot goes through
            // `hypot_scaled` there, as every other tier computes it.
            M::Max | M::Min | M::Hypot => self.eval_math_args(op, &elems)?,
            // A non-variadic Math fn spread is unusual; apply to elem 0.
            _ => {
                let nums: Vec<f64> = elems.iter().map(|&v| self.to_number_strict(v)).collect::<Result<_, _>>()?;
                self.eval_math_one(op, nums.first().copied().unwrap_or(f64::NAN))
            }
        };
        Ok(Value::num(r))
    }

    /// Whether `it` is a generator whose `next` is the intrinsic: a primed
    /// loop decided at `IterPrime` (its record's [[NextMethod]] is read once:
    /// undefined there means the intrinsic), an unprimed step asks now.
    #[inline]
    pub(crate) fn iter_generator_direct(&self, base: usize, it: Value, next: u16) -> bool {
        matches!(self.heap.get(it.heap_index()), HeapObj::Generator(_))
            && if next != u16::MAX {
                self.get(base, next).is_undefined()
            } else {
                self.gen_method_pristine(it.heap_index(), "next")
            }
    }

    /// One `IterNext` past the dense Array read, for the interpreter and for
    /// compiled code (`jit_iter_next`) alike: a generator or a user iterator
    /// driven by `.next()`, an intrinsic one stepped without its result
    /// object, and the positional walk of a string, a Set, a Map, a typed
    /// array or a holey Array. `it` is a heap value. Writes `value_dst`,
    /// `done_dst` and the cursor `idx` of the window at `base`.
    #[inline(never)]
    /// A user iterator's `next()` result `res` taken apart for a for-of
    /// step: IteratorNext's object check, IteratorComplete (the `done` Get)
    /// and, only while not done, IteratorValue (the `value` Get, observable
    /// through a getter). `done_dst` receives `done` as read, `value_dst` the
    /// value or, once done, `undefined`. A result that is exactly
    /// `{value, done}` as two own data properties (what `return { value: v,
    /// done: false }` makes) is read where it lies (`iter_result_unwrap`):
    /// neither Get can run code there, so reading both is the same.
    pub(crate) fn iter_step_result(
        &mut self,
        base: usize,
        res: Value,
        value_dst: u16,
        done_dst: u16,
    ) -> Result<(), Thrown> {
        if !self.is_object_value(res) {
            return Err(Thrown::from_static("TypeError: iterator result is not an object"));
        }
        let (val, done) = match self.iter_result_unwrap(res) {
            Some((val, done)) => (if self.truthy(done) { Value::UNDEFINED } else { val }, done),
            None => {
                let done = self.get_prop(res, "done")?;
                let val = if self.truthy(done) {
                    Value::UNDEFINED
                } else {
                    self.get_prop(res, "value")?
                };
                (val, done)
            }
        };
        self.set(base, value_dst, val);
        self.set(base, done_dst, done);
        Ok(())
    }

    pub(crate) fn iter_next_step(
        &mut self,
        base: usize,
        it: Value,
        value_dst: u16,
        done_dst: u16,
        idx: u16,
        next: u16,
    ) -> Result<(), Thrown> {
        // A generator is driven by `.next()`; the cursor is unused.
        // The step is taken without its `{value, done}` result
        // object, which nothing could see: built, read back and
        // dropped, it was a third of a generator-driven loop.
        // A `yield*` step still hands over the inner result
        // object itself, which is read below as before.
        if self.iter_generator_direct(base, it, next) {
            let res = match self.generator_next_step(it.heap_index())? {
                Some(super::async_runtime::GenStep::Yield(v)) => {
                    self.set(base, value_dst, v);
                    self.set(base, done_dst, Value::FALSE);
                    return Ok(());
                }
                Some(super::async_runtime::GenStep::Done(v)) => {
                    self.set(base, value_dst, v);
                    self.set(base, done_dst, Value::TRUE);
                    return Ok(());
                }
                Some(super::async_runtime::GenStep::Raw(r)) => r,
                None => Value::UNDEFINED,
            };
            // `iter_result` builds `{value, done}` in that order
            // and nothing else, so when the result is exactly
            // that shape its slots can be read directly instead
            // of through two full `get_prop`s. Verified against
            // the map rather than assumed, because `yield*`
            // returns the INNER result verbatim and that may be
            // any user object.
            if let Some((val, done)) = self.iter_result_unwrap(res) {
                self.set(base, value_dst, val);
                self.set(base, done_dst, done);
                return Ok(());
            }
            let done = self.get_prop(res, "done")?;
            let val = self.get_prop(res, "value")?;
            self.set(base, value_dst, val);
            self.set(base, done_dst, done);
            return Ok(());
        }
        // A user iterator object (`@@iterator` already resolved by
        // GetIterator): pull the next result via `.next()`. Lazy:
        // a `break` simply stops calling it. The cached prologue
        // [[NextMethod]] (IterPrime) is used when present; the
        // uncached (destructuring) sites re-fetch per step.
        // `HeapObj::Intl` is in the list for %SegmentIterator%:
        // it is an ordinary object with a `next` method, and
        // without it `for (v of segmenter.segment(s))` fell
        // through to the positional walk below and reported the
        // iterator itself as "not iterable".
        // A generator lands here when its `next` was replaced
        // (the arm above runs it directly only while it is the
        // intrinsic).
        if matches!(
            self.heap.get(it.heap_index()),
            HeapObj::Object(_)
                | HeapObj::Proxy { .. }
                | HeapObj::Iterator { .. }
                | HeapObj::IterHelper(_)
                | HeapObj::Intl { .. }
                | HeapObj::Generator(_)
        ) {
            let next = if next != u16::MAX {
                self.get(base, next)
            } else {
                self.get_prop(it, "next")?
            };
            if self.is_callable(next) {
                // A %RegExpStringIterator% stepped by the PRISTINE
                // intrinsic `next`: run the step inline. The
                // {value, done} result object the intrinsic would
                // build is engine-internal and its `done`/`value`
                // Gets are unobservable — skip both.
                if next.is_heap()
                    && matches!(self.heap.get(next.heap_index()),
                                HeapObj::Native(n) if *n == crate::vm::native::ITER_NEXT)
                {
                    if let Some(step) =
                        self.regexp_string_iter_step(it.heap_index())
                    {
                        let (val, done) = step?;
                        self.set(base, value_dst, val);
                        self.set(base, done_dst, Value::bool(done));
                        return Ok(());
                    }
                    // Same argument, same intrinsic `next`: a
                    // live %ArrayIteratorPrototype% iterator
                    // (`a.values()`, `a.keys()`, `a.entries()`,
                    // and every iterator-helper chain over an
                    // array) is stepped inline, and the
                    // {value, done} object it would have built
                    // — 90ns of a 143ns step — is never made.
                    if let Some(step) = self.array_iter_step(it.heap_index()) {
                        let (val, done) = step?;
                        self.set(base, value_dst, val);
                        self.set(base, done_dst, Value::bool(done));
                        return Ok(());
                    }
                    // Map/Set (`map.keys()`, `set.values()`,
                    // `map.entries()`) and snapshot iterators.
                    // `None` leaves the TypedArray-backed case,
                    // whose per-step bounds check can throw, on
                    // the general path below.
                    if let Some((val, done)) =
                        self.collection_iter_step(it.heap_index())?
                    {
                        self.set(base, value_dst, val);
                        self.set(base, done_dst, Value::bool(done));
                        return Ok(());
                    }
                }
                let res = self.call_value(next, it, &[])?;
                return self.iter_step_result(base, res, value_dst, done_dst);
            }
        }
        // Array/Set element, string char, or Map [k,v] at the cursor.
        let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
        // A Set's / Map's tombstoned (deleted) slots are skipped.
        while match self.heap.get(it.heap_index()) {
            HeapObj::Set(items) => cursor < items.len() && items[cursor].is_hole(),
            HeapObj::Map { keys, .. } => {
                cursor < keys.len() && keys[cursor].is_hole()
            }
            _ => false,
        } {
            cursor += 1;
        }
        // A STRING iterates by CODE POINT (one step per scalar; an
        // astral char is ONE step) while the cursor advances in
        // UTF-16 units (matching `.length`). Flatten a rope first
        // (no-op tag check for everything else).
        self.heap.flatten(it.heap_index());
        let str_step = match self.heap.get(it.heap_index()) {
            HeapObj::Str(s) => Some(s.cp_step(cursor)),
            _ => None,
        };
        if let Some(step) = str_step {
            match step {
                Some((cp, next)) => {
                    // `cp` may be a lone surrogate — a real 1-unit
                    // surrogate string, not U+FFFD.
                    let val = self.str_from_cp(cp);
                    self.set(base, value_dst, val);
                    self.set(base, done_dst, Value::bool(false));
                    self.set(base, idx, Value::int(next as i32));
                }
                None => self.set(base, done_dst, Value::bool(true)),
            }
            return Ok(());
        }
        // `coll`: a Map/Set step reads its entry positionally
        // (`collection_entry_at`); everything else is an ordinary index.
        let (len, coll) = match self.heap.get(it.heap_index()) {
            // The JS length: past the dense store for a
            // virtual array (the fast path above declined it).
            HeapObj::Array(_) => (self.js_array_len(it.heap_index()), false),
            HeapObj::Set(items) => (items.len(), true),
            HeapObj::Str(s) => (s.units(), false),
            HeapObj::Cons { len, .. } => (*len, false),
            HeapObj::Map { keys, .. } => (keys.len(), true),
            // The LIVE length each step: a tracking view follows its
            // resizable buffer (shrink ends early, grow yields more);
            // a detached/out-of-bounds view mid-iteration throws.
            HeapObj::TypedArray { .. } => {
                match self.ta_effective_len(it.heap_index()) {
                    Some(n) => (n, false),
                    None => {
                        return Err(Thrown(
                            "TypeError: TypedArray iterator: the viewed buffer is detached or out of bounds".into(),
                        ))
                    }
                }
            }
            _ => {
                return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
            }
        };
        if cursor < len {
            let val = if coll {
                self.collection_entry_at(it, cursor)
            } else {
                self.get_index(it, Value::int(cursor as i32))?
            };
            self.set(base, value_dst, val);
            self.set(base, done_dst, Value::bool(false));
            self.set(base, idx, len_value(cursor + 1));
        } else {
            self.set(base, done_dst, Value::bool(true));
        }
        Ok(())
    }

    /// The cell of a direct-eval site's caller binding `(kind, idx)` (see
    /// `FuncProto::eval_sites`): kind 0 is a cell of this frame in register
    /// `idx`, and so is kind 3, a catch parameter (an eval'd `var` of its name
    /// still goes to the function's varEnv, Annex B.3.5, but cannot shadow
    /// the parameter's cell); kinds 1 and 2 are this frame's closure upvalue
    /// `idx` (an eval root forwarding its caller scope, or a binding of a
    /// function enclosing the caller). Not a heap value when the binding has
    /// no cell yet.
    fn eval_site_cell(&self, base: usize, kind: u8, idx: u16) -> Value {
        if kind == 0 || kind == 3 {
            return self.get(base, idx);
        }
        let cl = self.frames.last().map(|f| f.closure).unwrap_or(NO_CLOSURE);
        if cl == NO_CLOSURE {
            return Value::UNDEFINED;
        }
        match self.heap.get(cl) {
            HeapObj::Closure { upvalues, .. } => upvalues
                .as_cells()
                .and_then(|s| s.get(idx as usize))
                .map(|&c| Value::heap(c))
                .unwrap_or(Value::UNDEFINED),
            _ => Value::UNDEFINED,
        }
    }

    /// Whether nothing outside a direct eval's own site could change what it
    /// compiles to: no realm, `$262.evalScript` or prelude compile in
    /// progress, and no private names in the running class body.
    fn direct_eval_cacheable_now(&self) -> bool {
        self.active_realm.is_none()
            && self.realm_global_objs.is_empty()
            && !self.eval_script_gdi
            && !self.eval_prelude_mode
            && self.current_private_brands().is_none()
    }

    /// Run one cold instruction for `dispatch_body` (see the module docs).
    #[inline(never)]
    #[allow(clippy::too_many_arguments, unused_variables, unused_mut)]
    pub(crate) fn dispatch_cold(
        &mut self,
        instr: &Instr,
        mut ip: usize,
        frame_idx: usize,
        func_id: u32,
        base: usize,
        cur_closure: u32,
        running: &'p crate::bytecode::FuncProto,
        code: &'p [Instr],
        stop_depth: usize,
    ) -> Result<ColdFlow, Thrown> {
        match *instr {
            // The stub of a function whose body has not compiled yet: compile
            // it, and run it from its start in this frame.
            Instr::LazyCompile => {
                #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
                {
                    self.lazy_compile_frame(func_id)?;
                    return Ok(ColdFlow::Frame);
                }
                #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
                {
                    return Err(Thrown::from_static(
                        "InternalError: lazy compilation is disabled in this build",
                    ));
                }
            }
            // A function with more locals than the register file addresses
            // keeps the rest in a spill vector (see `Instr::NewSpill`). It is
            // an ordinary heap value held in a register, so the collector
            // traces it and a suspended generator keeps it.
            Instr::NewSpill { dst, count } => {
                let slots = vec![Value::UNDEFINED; count as usize];
                let v = Value::heap(self.heap.alloc(HeapObj::Array(slots)));
                self.set(base, dst, v);
                ip += 1;
            }
            // A `switch` over string literals: here rather than in
            // `dispatch_body`, where an arm moved rows that never run one by
            // 4-7% (ephemeral, throw-catch); the call is a sliver of the
            // compares it replaces.
            #[cfg(not(feature = "meter-only"))]
            Instr::FuseStrSwitch { ldst, a, dst, n } => {
                ip = self.str_switch(running, func_id, base, ip, ldst, a, dst, n);
            }
            #[cfg(not(feature = "meter-only"))]
            Instr::FuseIntSwitch { ldst, a, dst, n } => {
                ip = self.int_switch(running, base, ip, ldst, a, dst, n);
            }
            Instr::LoadSpill { dst, vec, idx } => {
                let holder = self.get(base, vec).heap_index();
                let v = match self.heap.get(holder) {
                    HeapObj::Array(slots) => slots[idx as usize],
                    _ => unreachable!("a spill register holds its spill vector"),
                };
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::StoreSpill { vec, idx, src } => {
                let holder = self.get(base, vec).heap_index();
                let v = self.get(base, src);
                match self.heap.get_mut(holder) {
                    HeapObj::Array(slots) => slots[idx as usize] = v,
                    _ => unreachable!("a spill register holds its spill vector"),
                }
                // An old vector holding a young value: the next minor must see it.
                self.heap.write_barrier_val(holder, v);
                ip += 1;
            }
            Instr::LoadGlobalDyn { dst, idx } => {
                // Dynamic-first: the activation's EvalScope may bind
                // this slot's NAME (a sloppy fn-context eval var).
                if let Some(v) = self.eval_scope_lookup(idx) {
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                let v = self.globals[idx as usize];
                if v.is_uninitialized() {
                    let name = self.global_slot_name(idx).unwrap_or_default();
                    let lexical = self.global_name_is_lexical(&name);
                    let has_own = !lexical
                        && self.global_this != 0
                        && matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                    if !has_own {
                        // Chain fallback, as in LoadGlobal: a Proxy
                        // prototype's `has` trap / an inherited property
                        // resolves the name (HasBinding = HasProperty).
                        let gobj = Value::heap(self.global_this);
                        let proto = if self.global_this != 0 {
                            self.object_get_prototype_of(gobj)
                        } else {
                            Value::NULL
                        };
                        if !lexical && proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                            let val = self.get_prop(gobj, &name)?;
                            self.set(base, dst, val);
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        // `undefined`, `NaN` and `Infinity` are virtual
                        // value-properties rather than heap-backed builtin
                        // slots.  A dynamic EvalScope lookup has already
                        // had first refusal above; only now use that global
                        // fallback (global_by_name also respects deletion).
                        if let Some(val) = self.global_by_name(&name) {
                            self.set(base, dst, val);
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        return Err(Thrown::join3("ReferenceError: ", &*name, " is not defined"));
                    }
                    let gobj = Value::heap(self.global_this);
                    let val = self.get_prop(gobj, &name)?;
                    self.set(base, dst, val);
                } else {
                    self.set(base, dst, v);
                }
                ip += 1;
            }
            Instr::StoreGlobalDyn { idx, src } => {
                let v = self.get(base, src);
                if self.eval_scope_store(idx, v) {
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                // A $262.evalScript `const`: writes after the
                // initializing one (which sees UNINITIALIZED) throw.
                if !self.eval_const_globals.is_empty()
                    && self.eval_const_globals.contains(&idx)
                    && !self.globals[idx as usize].is_uninitialized()
                {
                    return Err(Thrown(
                        "TypeError: Assignment to constant variable.".into(),
                    ));
                }
                if self.globals[idx as usize].is_uninitialized() {
                    // A script lexical's slot is in its TDZ: this store is
                    // its initialization, which a same-named own property
                    // of the global object (a configurable global the
                    // lexical shadows) or the global's chain must not
                    // absorb.
                    if let Some(name) = self
                        .global_slot_name(idx)
                        .filter(|n| !self.global_name_is_lexical(n))
                    {
                        let has_own = self.global_this != 0
                            && matches!(
                                self.heap.get(self.global_this),
                                HeapObj::Object(m) if m.pos(&name).is_some()
                            );
                        if has_own {
                            let gobj = Value::heap(self.global_this);
                            self.set_prop(gobj, &name, v, false)?;
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        // A name the global object's PROTO CHAIN resolves
                        // (an inherited accessor / non-writable data
                        // property, or a Proxy prototype's `has` trap) is
                        // assigned THROUGH [[Set]] on the global object,
                        // not as a shadowing slot binding.
                        let gobj = Value::heap(self.global_this);
                        let proto = if self.global_this != 0 {
                            self.object_get_prototype_of(gobj)
                        } else {
                            Value::NULL
                        };
                        if proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                            self.set_prop(gobj, &name, v, false)?;
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        // See StoreGlobal: an unresolvable name with a
                        // Proxy on the global's chain is [[Set]].
                        if self.global_proto_chain_has_proxy() {
                            self.set_prop(gobj, &name, v, false)?;
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        // See StoreGlobal: re-creating a deleted binding
                        // lifts DeleteGlobal's deletion record.
                        if !self.deleted_globals.is_empty() {
                            self.deleted_globals.remove(&name);
                        }
                    }
                } else if self.global_real_own_route(idx) {
                    // A defineProperty'd REAL own property of the global
                    // object (its attributes live on the object, not in
                    // the slot) governs the write — route through [[Set]]
                    // (staging/sm/Proxy/regress-bug1037770).
                    let name = self.global_slot_name(idx).unwrap_or_default();
                    if !self.global_name_is_lexical(&name) {
                        let gobj = Value::heap(self.global_this);
                        self.set_prop(gobj, &name, v, false)?;
                        ip += 1;
                        return Ok(ColdFlow::Next(ip));
                    }
                }
                self.globals[idx as usize] = v;
                ip += 1;
            }
            Instr::JsonStringify {
                dst,
                val,
                space,
                callee,
                this_v,
            } => {
                let v = self.get(base, val);
                let space_v = self.get(base, space);
                let callee = self.get(base, callee);
                let this_v = self.get(base, this_v);
                let result = self.exec_json_stringify(v, space_v, callee, this_v)?;
                self.set(base, dst, result);
                ip += 1;
            }
            Instr::JsonParse {
                dst,
                a,
                callee,
                this_v,
            } => {
                let arg = self.get(base, a);
                let callee = self.get(base, callee);
                let this_v = self.get(base, this_v);
                let v = self.exec_json_parse(arg, callee, this_v)?;
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::ObjectRest {
                dst,
                src,
                exclude_start,
                exclude_count,
            } => {
                let v = self.object_rest_value(base, func_id, src, exclude_start as u32, exclude_count as u32)?;
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::ObjectRestDyn {
                dst,
                src,
                keys_base,
                n,
            } => {
                let s = self.get(base, src);
                // Resolve the excluded sibling keys (ToPropertyKey) from regs.
                let mut excluded: Vec<String> = Vec::with_capacity(n as usize);
                for i in 0..n {
                    let kv = self.get(base, keys_base + i);
                    excluded.push(self.to_property_key(kv)?);
                }
                // See `ObjectRest`: anything but an ordinary object or a
                // string goes through the general CopyDataProperties.
                if s.is_heap()
                    && !matches!(
                        self.heap.get(s.heap_index()),
                        HeapObj::Object(_) | HeapObj::Str(_) | HeapObj::Cons { .. }
                    )
                {
                    let m = self.copy_data_properties_rest(s, &excluded)?;
                    let v = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                let keys: Vec<String> = if s.is_heap() {
                    match self.heap.get(s.heap_index()) {
                        HeapObj::Object(map) => spec_key_order_map(&map)
                            .into_iter()
                            .filter(|&i| map.attr_at(i).enumerable)
                            .map(|i| map.key_at(i).to_owned())
                            .filter(|k| !excluded.iter().any(|e| e == k))
                            .collect(),
                        HeapObj::Str(st) => (0..st.units())
                            .map(|i| i.to_string())
                            .filter(|k| !excluded.iter().any(|e| e == k))
                            .collect(),
                        HeapObj::Cons { len, .. } => (0..*len)
                            .map(|i| i.to_string())
                            .filter(|k| !excluded.iter().any(|e| e == k))
                            .collect(),
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                let m = self.object_rest_plain(s, keys)?;
                let v = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::MakeClass {
                dst,
                class_id,
                parent,
            } => {
                let cd = self.class_def(class_id as usize).clone();
                // MakeClass materializes and indexes several parallel
                // member lists without returning to bytecode dispatch.
                // Charge a conservative four visits per declaration
                // before any of those native loops begin.
                let class_member_work = [
                    cd.methods.len(),
                    cd.getters.len(),
                    cd.setters.len(),
                    cd.statics.len(),
                    cd.static_getters.len(),
                    cd.static_setters.len(),
                    cd.instance_field_names.len(),
                    cd.static_field_names.len(),
                ]
                .into_iter()
                .fold(0u64, |sum, len| sum.saturating_add(len as u64))
                .saturating_mul(4);
                self.preflight_native_iteration_work(class_member_work)?;
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
                let materialize = |vm: &mut Self,
                                   defs: &[(String, u32)]|
                 -> Vec<(String, Value)> {
                    defs.iter()
                        .map(|(n, fid)| {
                            // A computed-key PLACEHOLDER (position-only:
                            // "\u{1}cm{fid}", no compiled function of its
                            // own) — ClassAddMember renames it in place
                            // with the materialized member.
                            if *fid == u32::MAX {
                                return (n.clone(), Value::UNDEFINED);
                            }
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
                // Keep the retained Vec's established representation and
                // declaration order, but index it with RandomState while
                // building. Repeated Vec::find made N unique private names
                // quadratic inside this single instruction.
                let mut declared_index: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                fn add_declared<'a>(
                    declared: &mut Vec<(String, u8)>,
                    declared_index: &mut std::collections::HashMap<&'a str, usize>,
                    n: &'a str,
                    k: u8,
                ) -> Result<(), Thrown> {
                    if !n.starts_with('#') {
                        return Ok(());
                    }
                    if let Some(&index) = declared_index.get(n) {
                        declared[index].1 |= k;
                    } else {
                        declared_index.try_reserve(1).map_err(|_| {
                            Thrown::from_static("RangeError: private-name index allocation failed")
                        })?;
                        declared.try_reserve(1).map_err(|_| {
                            Thrown::from_static("RangeError: private-name allocation failed")
                        })?;
                        let mut owned = String::new();
                        owned.try_reserve_exact(n.len()).map_err(|_| {
                            Thrown::from_static("RangeError: private-name allocation failed")
                        })?;
                        owned.push_str(n);
                        let index = declared.len();
                        declared.push((owned, k));
                        declared_index.insert(n, index);
                    }
                    Ok(())
                }
                for (n, _) in &cd.methods {
                    add_declared(&mut declared, &mut declared_index, n, 1)?;
                }
                for (n, _) in &cd.getters {
                    add_declared(&mut declared, &mut declared_index, n, 2)?;
                }
                for (n, _) in &cd.setters {
                    add_declared(&mut declared, &mut declared_index, n, 4)?;
                }
                // STATIC members carry bit 8: their brand lives on the
                // class VALUE itself, not on constructed instances.
                for (n, _) in &cd.statics {
                    add_declared(&mut declared, &mut declared_index, n, 1 | 8)?;
                }
                for (n, _) in &cd.static_getters {
                    add_declared(&mut declared, &mut declared_index, n, 2 | 8)?;
                }
                for (n, _) in &cd.static_setters {
                    add_declared(&mut declared, &mut declared_index, n, 4 | 8)?;
                }
                for n in &cd.instance_field_names {
                    add_declared(&mut declared, &mut declared_index, n, 0)?;
                }
                for n in &cd.static_field_names {
                    add_declared(&mut declared, &mut declared_index, n, 8)?;
                }
                let has_private_names = !declared.is_empty();
                if has_private_names {
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
                        self.method_brand
                            .insert(mv.heap_index(), lex_brands.clone());
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
                        self.method_brand
                            .insert(fv.heap_index(), lex_brands.clone());
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
                    has_private_names,
                    methods,
                    getters,
                    setters,
                    proto_order: cd.proto_order,
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
                    ctor_fn: Value::UNDEFINED,
                    thunk_fn: Value::UNDEFINED,
                    private_brand,
                    class_id,
                    dec: None,
                    proto_dirty: false,
                    reproto_instances: false,
                    no_has_instance_shape: std::cell::Cell::new(crate::shape::DICT),
                    private_slots: std::cell::Cell::new(0),
                }))));
                self.brand_owner.insert(private_brand, v.heap_index());
                // Remember it so `super` in a derived class can reach it, and
                // whether an earlier evaluation's code may still run
                // (`class_value_for`).
                let slot = &mut self.class_values[class_id as usize];
                let again = slot.latest.is_some();
                slot.repeated |= again;
                slot.latest = Some(v);
                // A second evaluation mints a second brand for the same
                // declaration, so the compiled private-field lanes that baked
                // the first one stand down here, before this evaluation's
                // static blocks, computed keys or initializers can run the
                // shared compiled methods (`codegen::PrivLane::flag`).
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                if again {
                    if let Some(flag) = self.jit_private_class_flags.get_mut(&class_id) {
                        **flag = 0;
                    }
                }
                // A re-executed class declaration swaps this slot to a NEW
                // class value (+ new prototype chain) without mutating the
                // OLD prototype objects a JIT method-inline `super` arm's
                // hop guards watch — so bump the epoch the inline re-checks.
                self.mi_class_epoch = self.mi_class_epoch.wrapping_add(1);
                // The class value itself carries the lexical brand chain so the
                // ctor / field initializers / static blocks (frame.callee = the
                // class value) resolve the same brands.
                self.method_brand.insert(v.heap_index(), lex_brands);
                // A class evaluated by createRealm-child code carries the
                // child's realm tag (its brand-check TypeErrors etc. then
                // adopt the child's constructor identity).
                self.realm_tag_new(v.heap_index());
                // A DECORATED class gets its per-evaluation decoration
                // state (and its `[Symbol.metadata]` object, whose
                // prototype is the superclass's) here — before any
                // decorator can run, since `context.metadata` is handed
                // to the very first one.
                let n_elems = self
                    .class_def(class_id as usize)
                    .dec_plan
                    .as_ref()
                    .map(|p| p.elements.len());
                if let Some(n) = n_elems {
                    self.dec_init_state(v, n, parent_idx);
                }
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::DecElem {
                class,
                elem,
                arg_base,
                argc,
                class_id,
            } => {
                let cv = self.get(base, class);
                // The register block is (decorator, receiver) PAIRS: a
                // `@a.b` decorator is called as a method of `a`.
                let decs: Vec<(Value, Value)> = (0..argc)
                    .map(|i| {
                        (
                            self.get(base, arg_base + i * 2),
                            self.get(base, arg_base + i * 2 + 1),
                        )
                    })
                    .collect();
                self.dec_apply_element(cv, class_id, elem as usize, &decs)?;
                ip += 1;
            }
            Instr::DecClass {
                class,
                arg_base,
                argc,
            } => {
                let cv = self.get(base, class);
                let decs: Vec<(Value, Value)> = (0..argc)
                    .map(|i| {
                        (
                            self.get(base, arg_base + i * 2),
                            self.get(base, arg_base + i * 2 + 1),
                        )
                    })
                    .collect();
                let out = self.dec_apply_class(cv, &decs)?;
                self.set(base, class, out);
                ip += 1;
            }
            Instr::ClassAddMember {
                class,
                key,
                func,
                kind,
            } => {
                let write_back = kind & crate::bytecode::KEY_WRITEBACK != 0;
                let kind = kind & !crate::bytecode::KEY_WRITEBACK;
                let cv = self.get(base, class);
                // ToPropertyKey the computed key the SAME way get_index/set_index
                // do (ToPrimitive string-hint, Symbols kept, real ToString for a
                // function/object) â€” `display` produced a debug string (e.g.
                // "function" for a function-expression key), so the member was
                // stored under a key the access could never recompute.
                let kraw = self.get(base, key);
                let k = self.coerce_index_key(kraw)?;
                // An auto-accessor's setter instruction follows on the same
                // key register: hand it the already-coerced key so the
                // element's ToPropertyKey runs exactly once. The compiler
                // guarantees the register is a temp when it sets this.
                if write_back {
                    self.set(base, key, k);
                }
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
                // A class-body callable carries the class's lexical brand
                // chain, as MakeClass gave every named member: without it a
                // computed-key member's private accesses fell back to the
                // textual path, which accepted another evaluation's
                // same-named private and cost a key spelling per access.
                if fv.is_heap() {
                    if let Some(chain) = self.method_brand.get(&cv.heap_index()).cloned() {
                        self.method_brand.insert(fv.heap_index(), chain);
                    }
                }
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
                // A computed-key expression can call user code before
                // this instruction, promoting the class while the
                // callable materialized above is still young. Record
                // only the value this instruction actually adds to the
                // class: the p < d duplicate arm discards `fv` and
                // merely moves an already-present later definition.
                let mut stored_member = None;
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
                        stored_member = Some(fv);
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
                        // The compile pass parked a position-keeping
                        // PLACEHOLDER ("\u{1}cm{func_id}") at this member's
                        // source position; rename it in place — unless an
                        // earlier same-key member exists (a duplicate keeps
                        // its ORIGINAL position, last definition wins), in
                        // which case overwrite that one and drop the parked
                        // placeholder.
                        let ph = format!("\u{1}cm{func}");
                        let dup = list.iter().position(|(n, _)| *n == kstr);
                        let parked = list.iter().position(|(n, _)| *n == ph);
                        match (dup, parked) {
                            // Defined twice in one class body: the
                            // property keeps the FIRST definition's
                            // position and the LAST definition's value.
                            // Which came first is the entry ORDER — the
                            // placeholder sits at the computed member's
                            // source position — and this arm is the case
                            // where the computed one is EARLIER, so the
                            // already-installed named member is the
                            // later definition and its value wins.
                            // Overwriting unconditionally instead made a
                            // computed member beat a later same-named one
                            // (`class C { [k](){return 1} m(){return 2} }`
                            // answered 1), which is also what let an
                            // `accessor [k]` clobber a `get k` below it.
                            (Some(d), Some(p)) if p < d => {
                                let v = list[d].1;
                                list[p] = (kstr.clone(), v);
                                list.remove(d);
                            }
                            (Some(d), p) => {
                                list[d].1 = fv;
                                stored_member = Some(fv);
                                if let Some(p) = p {
                                    list.remove(p);
                                }
                            }
                            (None, Some(p)) => {
                                list[p] = (kstr.clone(), fv);
                                stored_member = Some(fv);
                            }
                            (None, None) => {
                                list.push((kstr.clone(), fv));
                                stored_member = Some(fv);
                            }
                        }
                        // Mirror the rename into the cross-kind source
                        // order that builds `C.prototype` — the same
                        // "first definition keeps the position" rule,
                        // but measured across methods AND accessors
                        // (each kind's own list can only order within
                        // itself). Static members do not participate:
                        // their order comes from the `statics` ObjMap.
                        if matches!(kind, 0 | 1 | 2) {
                            let ord = &mut c.proto_order;
                            let dup = ord.iter().position(|n| *n == kstr);
                            let parked = ord.iter().position(|n| *n == ph);
                            match (dup, parked) {
                                (Some(d), Some(p)) if p < d => {
                                    ord[p] = kstr;
                                    ord.remove(d);
                                }
                                (Some(_), Some(p)) => {
                                    ord.remove(p);
                                }
                                (None, Some(p)) => ord[p] = kstr,
                                (Some(_), None) => {}
                                (None, None) => ord.push(kstr),
                            }
                        }
                    }
                }
                if matches!(kind, 0 | 1 | 2) {
                    self.class_proto_epoch = self.class_proto_epoch.wrapping_add(1);
                }
                if let Some(stored) = stored_member {
                    // The store is committed and the mutable ClassData
                    // borrow is over; the barrier is infallible and no
                    // collection can intervene inside this instruction.
                    self.store_barrier(
                        crate::heap::gcoracle::CLASS_MEMBER,
                        cv.heap_index(),
                        stored,
                    );
                }
                ip += 1;
            }
            Instr::SuperCtorSpread {
                ctor,
                home_class_id,
                args,
            } => {
                self.super_ctor_spread_call(base, ip, ctor, home_class_id, args)?;
                ip += 1;
            }
            Instr::SuperMethodSpread {
                dst,
                home_class_id,
                name,
                args,
            } => {
                // `super.name(...args)` â€” like SuperMethod but the arguments come
                // from a spread array; `this` = the current receiver.
                let key: &'p str =
                    &self.func(func_id as usize).string_constants[name as usize];
                let proto = self
                    .super_base(home_class_id, self.func(func_id as usize).super_static);
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let m = self.get_member(proto, key, this)?;
                if !self.is_callable(m) {
                    return Err(Thrown::join3("TypeError: super.", &*key, " is not a function"));
                }
                let args_v = self.get(base, args);
                let arg_vec = self.array_snapshot(args_v.heap_index());
                let r = self.call_value(m, this, &arg_vec)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperMethodComputedSpread {
                dst,
                home_class_id,
                key,
                args,
            } => {
                // `super[key](...args)` — computed SuperMethodSpread:
                // ToPropertyKey(key), resolve on the super base with
                // `this` = the current receiver, spread-call.
                let kv = self.get(base, key);
                // GetSuperBase BEFORE ToPropertyKey (see SuperGetComputed).
                let proto = self
                    .super_base(home_class_id, self.func(func_id as usize).super_static);
                let ks = self.coerce_index_key(kv)?;
                let ks = self.key_of(ks);
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let m = self.get_member(proto, &ks, this)?;
                if !self.is_callable(m) {
                    return Err(Thrown::join3("TypeError: super[", &*ks, "] is not a function"));
                }
                let args_v = self.get(base, args);
                let arg_vec = self.array_snapshot(args_v.heap_index());
                let r = self.call_value(m, this, &arg_vec)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperMethodObj {
                dst,
                name,
                arg_base,
                argc,
            } => {
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                let proto = self.obj_super_base(self.frames[frame_idx].callee);
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let m = self.get_member(proto, &key, this)?;
                if !self.is_callable(m) {
                    return Err(Thrown::join3("TypeError: super.", &*key, " is not a function"));
                }
                let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                for i in 0..argc {
                    args.push(self.get(base, arg_base + i));
                }
                let r = self.call_value(m, this, &args)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperMethodObjComputed {
                dst,
                key,
                arg_base,
                argc,
            } => {
                let kv = self.get(base, key);
                // GetSuperBase BEFORE ToPropertyKey (see SuperGetComputed).
                let proto = self.obj_super_base(self.frames[frame_idx].callee);
                let ks = self.to_property_key(kv)?;
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let m = self.get_member(proto, &ks, this)?;
                if !self.is_callable(m) {
                    return Err(Thrown::join3("TypeError: super[", &*ks, "] is not a function"));
                }
                let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                for i in 0..argc {
                    args.push(self.get(base, arg_base + i));
                }
                let r = self.call_value(m, this, &args)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::CallMethodComputedSpread {
                dst,
                obj,
                key,
                args,
            } => {
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
            Instr::MathSpread {
                dst,
                op,
                callee,
                this_v,
                args,
            } => {
                let v = self.math_spread_value(base, op, callee, this_v, args)?;
                self.set(base, dst, v);
                ip += 1;
            }

            Instr::NewError {
                dst,
                kind,
                arg,
                opts,
                errors,
            } => {
                // The message is coerced with a real ToString (observable user
                // `toString` / `@@toPrimitive`, abrupt completion) for EVERY
                // error kind: a Symbol message throws TypeError and a throwing
                // coercion propagates — before the error is allocated (and, for
                // AggregateError, before the errors are iterated).
                let msg = match arg.get() {
                    Some(r) => {
                        let m = self.get(base, r);
                        if m == Value::UNDEFINED {
                            None
                        } else {
                            Some(self.to_str_value(m)?)
                        }
                    }
                    None => None,
                };
                let v = self.make_error(kind, msg);
                // InstallErrorCause (ES2022): an options object with a `cause`
                // gives the error a non-enumerable own `cause` property.
                // HasProperty walks the proto chain AND fires a Proxy
                // `has` trap (observable; its throw propagates). The
                // error is rooted across that guest code.
                if let Some(or) = opts.get() {
                    let options = self.get(base, or);
                    self.install_error_cause(v, options)?;
                }
                // AggregateError installs `errors` LAST (after message + cause):
                // a non-enumerable own array of IterableToList(firstArg). It runs
                // even when the arg is absent (`new AggregateError()`) so that
                // IterableToList(undefined) throws the required TypeError.
                if kind == 7 {
                    let errors_arg = errors
                        .get()
                        .map(|er| self.get(base, er))
                        .unwrap_or(Value::UNDEFINED);
                    self.with_host_roots(&[v], |vm| vm.install_agg_errors(v, errors_arg))?;
                }
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::NewRegExp {
                dst,
                pattern,
                flags,
                is_construct,
            } => {
                let p = self.get(base, pattern);
                let f = self.get(base, flags);
                // Every `NewRegExp` the compiler emits is a literal's.
                let v = if is_construct {
                    self.new_regexp_literal((u64::from(func_id) << 32) | ip as u64, p, f)?
                } else {
                    self.new_regexp_value(p, f, is_construct)?
                };
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::RegExpLitProbe {
                dst,
                pattern,
                flags,
            } => {
                let yes = self.regexp_literal_test_probe(func_id, pattern, flags)?;
                self.set(base, dst, Value::bool(yes));
                ip += 1;
            }
            Instr::RegExpLitTest {
                dst,
                arg,
                pattern,
                flags,
            } => {
                let a = self.get(base, arg);
                let v = self.regexp_literal_test(func_id, pattern, flags, a)?;
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::ImportCall {
                dst,
                spec,
                phase,
                opts,
            } => {
                #[cfg(feature = "wasm-no-fs-loader")]
                {
                    // The dedicated WebAssembly embedder cannot install a
                    // filesystem loader. Preserve every observable operation
                    // before resolution: ToString(spec), import-options
                    // validation (including Proxy/getter effects), error
                    // precedence, and the always-asynchronous Promise result.
                    let spec_val = self.get(base, spec);
                    let reason = match self.to_js_string(spec_val) {
                        Err(_) => self
                            .pending_throw
                            .take()
                            .unwrap_or_else(|| self.make_error(1, None)),
                        Ok(_spec_str) => {
                            let opt_err = match opts {
                                Some(r) => {
                                    let ov = self.get(base, r);
                                    if ov == Value::UNDEFINED {
                                        None
                                    } else {
                                        self.validate_import_options(ov).err()
                                    }
                                }
                                None => None,
                            };
                            match opt_err {
                                Some(e) => e,
                                // Source phase is unavailable for a
                                // SourceTextModule; this takes precedence
                                // over the absent-loader error.
                                None if phase == 2 => self.make_error(3, None),
                                None => self.make_error(1, None),
                            }
                        }
                    };
                    // Root the reason in the destination across Promise
                    // allocation, exactly like the general loader path.
                    self.set(base, dst, reason);
                    let p = self.alloc_promise();
                    let reason = self.get(base, dst);
                    self.reject(p, reason);
                    self.set(base, dst, Value::heap(p));
                }
                // import(spec [, opts]) / import.defer / import.source.
                // Spec order: ToString(spec); then a non-undefined non-object
                // `opts` â†’ TypeError; `import.source` â†’ SyntaxError (source
                // phase unavailable for a text module); otherwise resolve the
                // specifier against the referrer's dir, load + evaluate the
                // module ONCE in a later job (cached by path so re-import
                // yields the SAME namespace), and resolve with its
                // (snapshot) namespace. A
                // missing file / no base dir â†’ TypeError; a throw during
                // ToString or evaluation rejects with that value. import()
                // never throws synchronously. Everything that may GC runs
                // BEFORE the promise is allocated; the settle value is rooted
                // in `dst` across alloc_promise (the iter-169 GC invariant).
                #[cfg(not(feature = "wasm-no-fs-loader"))]
                let spec_val = self.get(base, spec);
                #[cfg(not(feature = "wasm-no-fs-loader"))]
                let mut deferred = false;
                #[cfg(not(feature = "wasm-no-fs-loader"))]
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
                            // import.source: a source text module has no
                            // source phase (SyntaxError); the test262 host's
                            // `<module source>` names one that has, the same
                            // shared object a static `import source` binds.
                            if spec_str == "<module source>" {
                                match self.module_source_slot(std::path::Path::new("<module source>")) {
                                    Ok(s) => Ok(self.globals[s as usize]),
                                    Err(Thrown(msg)) => Err(self.error_from_thrown(&msg)),
                                }
                            } else {
                                Err(self.make_error(3, None)) // SyntaxError: source phase
                            }
                        } else if phase == 1 {
                            // import.defer(): the DEFERRED namespace —
                            // loaded, not evaluated. An ASYNC target
                            // (top-level await) is the exception: the
                            // proposal evaluates a deferred graph's
                            // async modules EAGERLY, with the returned
                            // promise waiting on them.
                            match self
                                .dynamic_import_base_dir(func_id)
                                .map(|d| crate::vm::node::resolve::specifier_path(Some(&d), &spec_str))
                            {
                                None => Err(self.make_error(1, None)),
                                // A fresh Evaluate() for the eager
                                // async subgraph: its own DFS stack
                                // segment, never an edge of a body
                                // that happens to be executing.
                                Some(p) => match self.with_fresh_dfs_segment(|vm| {
                                    vm.deferred_namespace_for(&p, mtype.as_deref())
                                }) {
                                    Ok(ns) => {
                                        let canon = std::fs::canonicalize(&p).unwrap_or(p);
                                        if ns.is_heap()
                                            && self
                                                .deferred_ns_state
                                                .contains_key(&ns.heap_index())
                                            && self.module_has_tla(&canon)
                                        {
                                            match self.import_module(&canon, None) {
                                                Ok(real) => {
                                                    // pending_module_body
                                                    // (if TLA suspended)
                                                    // flows to the
                                                    // promise wiring.
                                                    self.defer_ns_adopt(
                                                        ns.heap_index(),
                                                        real,
                                                    );
                                                    Ok(ns)
                                                }
                                                Err(Thrown(msg)) => Err(self
                                                    .pending_throw
                                                    .take()
                                                    .unwrap_or_else(|| {
                                                        self.error_from_thrown(&msg)
                                                    })),
                                            }
                                        } else {
                                            Ok(ns)
                                        }
                                    }
                                    Err(Thrown(msg)) => Err(self
                                        .pending_throw
                                        .take()
                                        .unwrap_or_else(|| self.error_from_thrown(&msg))),
                                },
                            }
                        } else {
                            // ContinueDynamicImport links and evaluates
                            // in a LATER job: the target's body never
                            // runs inside the import() call, before the
                            // rest of the calling code or jobs queued
                            // ahead of it. The same deferral keeps an
                            // import issued while a static link DFS is
                            // in flight (a dependency body evaluating
                            // inside an ancestor's import loop) from
                            // preempting the DFS evaluation order
                            // (verify-dfs). The specifier resolves
                            // against the REFERRER now; the job gets
                            // the joined path.
                            match self
                                .dynamic_import_base_dir(func_id)
                                .map(|d| crate::vm::node::resolve::specifier_path(Some(&d), &spec_str))
                            {
                                None => Err(self.make_error(1, None)),
                                Some(path) => {
                                    let p = self.alloc_promise();
                                    // Root the promise in dst across the
                                    // allocations below.
                                    self.set(base, dst, Value::heap(p));
                                    let _gc = self.gc_lock_guard();
                                    let sv = self
                                        .alloc_str(path.to_string_lossy().into_owned());
                                    let mut bargs = vec![sv];
                                    if let Some(t) = &mtype {
                                        let tv = self.alloc_str(t.clone());
                                        bargs.push(tv);
                                    }
                                    let tgt =
                                        Value::heap(self.heap.alloc(HeapObj::Native(
                                            native::MODULE_DYN_IMPORT,
                                        )));
                                    let cb = Value::heap(self.heap.alloc(HeapObj::Bound {
                                        target: tgt,
                                        this: Value::heap(p),
                                        args: bargs,
                                    }));
                                    // The reaction's dependent is a fresh
                                    // dummy — the native settles the REAL
                                    // promise itself (it may need to chain
                                    // on a TLA body promise).
                                    let dummy = self.alloc_promise();
                                    self.microtasks.push_back(Microtask::Reaction {
                                        callback: cb,
                                        arg: Value::UNDEFINED,
                                        dependent: dummy,
                                        kind: ReactionKind::Fulfill,
                                        finally: false,
                                    });
                                    deferred = true;
                                    Ok(Value::heap(p))
                                }
                            }
                        }
                    }
                };
                // A DEFERRED load already parked its promise in dst;
                // the microtask settles it.
                #[cfg(not(feature = "wasm-no-fs-loader"))]
                match settle {
                    _ if deferred => {}
                    Ok(v) => {
                        // A TLA module still evaluating published its
                        // body promise: the import() promise settles
                        // FROM it (fulfill → the namespace, reject →
                        // the body's reason) instead of immediately.
                        let body = self.pending_module_body.take();
                        self.set(base, dst, v);
                        let p = self.alloc_promise();
                        let r = self.get(base, dst);
                        match body {
                            Some(bp) if bp.is_heap() => {
                                let _gc = self.gc_lock_guard();
                                let ret_ns = Value::heap(
                                    self.heap.alloc(HeapObj::Native(native::SPECIES_GET)),
                                );
                                // `this`-returning native bound to the
                                // namespace: the fulfill callback's
                                // return value settles the dependent.
                                let cb = Value::heap(self.heap.alloc(HeapObj::Bound {
                                    target: ret_ns,
                                    this: r,
                                    args: Vec::new(),
                                }));
                                self.then_internal(
                                    bp.heap_index(),
                                    cb,
                                    Value::UNDEFINED,
                                    Some(p),
                                );
                            }
                            _ => self.resolve_fresh_promise(p, r),
                        }
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
            Instr::DefineAccessor {
                obj,
                key,
                func,
                is_setter,
            } => {
                let o = self.get(base, obj);
                let kv = self.get(base, key);
                let f = self.get(base, func);
                let k = self.to_property_key(kv)?;
                self.define_object_accessor(o, &k, f, is_setter);
                ip += 1;
            }
            Instr::DeleteGlobal { dst, slot } => {
                // Dynamic-first, like the LoadGlobalDyn family: a sloppy
                // FUNCTION-context eval's `var` lives in the caller
                // activation's EvalScope, and 19.2.1.3 step 9.d.ii.1
                // creates it with deletable = true — so `delete` must
                // REMOVE it and let the name fall back to the outer
                // binding, not answer false against a global slot that
                // was never the binding
                // (staging/sm/eval/exhaustive-fun-*: `eval('delete y')`
                // inside a function the eval itself declared).
                if let Some(name) = self.global_slot_name(slot) {
                    if let Some(fi) = self.frames.len().checked_sub(1) {
                        // Innermost binding of the name, as the read does.
                        let owner = self.eval_scope_chain(fi).find(|&sc| {
                            matches!(self.heap.get(sc), HeapObj::EvalScope(m) if m.contains_key(&name))
                        });
                        if let Some(sc) = owner {
                            let removed = match self.heap.get_mut(sc) {
                                HeapObj::EvalScope(m) => m.remove(&name).is_some(),
                                _ => false,
                            };
                            if removed {
                                self.set(base, dst, Value::bool(true));
                                ip += 1;
                                return Ok(ColdFlow::Next(ip));
                            }
                        }
                    }
                }
                // Sloppy `delete <global>`: a DECLARED global binding
                // (program var/function/class/let/const slot) is
                // non-configurable → false; everything else (implicit
                // `x = 1` global, builtin, eval-made var) is REMOVED —
                // its slot returns to the never-declared sentinel — and
                // yields true. Checked at RUNTIME against the program's
                // decl lists so eval-compiled deletes agree with the
                // program's bindings.
                let immutable_value_global =
                    self.global_slot_name(slot).is_some_and(|name| {
                        matches!(name.as_str(), "NaN" | "Infinity" | "undefined")
                    });
                let nonconfig = immutable_value_global
                    || self.program.hoisted_globals.contains(&slot)
                    || self.program.decl_globals.contains(&slot)
                    || self.program.lexical_globals.contains(&slot)
                    // A later SCRIPT's var/function declaration is as
                    // non-configurable as the main program's (an eval
                    // `var` is not recorded here, and deletes).
                    || self.eval_var_globals.contains(&slot)
                    // An evalScript-created LEXICAL binding lives in the
                    // declarative record — DeleteBinding returns false
                    // and the binding persists (only eval VARs delete).
                    || self.eval_lexical_globals.contains(&slot)
                    || self.eval_const_globals.contains(&slot);
                // A binding may exist as an own property of the global
                // OBJECT without any compile-time declaration behind it
                // (`Object.defineProperty(globalThis, …)`, an implicit
                // `x = 1`, an eval `var`). The object environment record
                // delegates to [[Delete]], so the property's own
                // attributes decide: a non-configurable one refuses
                // (`delete nonconfigurable` is false, not true), and a
                // configurable one must actually be REMOVED — clearing
                // only the slot left the property behind, still visible
                // to `in` and getOwnPropertyDescriptor.
                let mut ok = !nonconfig;
                if ok && self.global_this != 0 {
                    if let Some(name) = self.global_slot_name(slot) {
                        let g = self.global_this;
                        let has_own = matches!(
                            self.heap.get(g),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                        if has_own {
                            ok = self.delete_property(Value::heap(g), &name)?
                                == Value::bool(true);
                        }
                    }
                }
                if ok {
                    // Through `uninitialize_global`, not a bare write: the
                    // slot going backwards is what invalidates every
                    // compile-time "this slot is live" finding, and the epoch
                    // bump is how already-compiled code learns.
                    self.uninitialize_global(slot);
                    // Clearing the SLOT is not enough for a BUILT-IN name:
                    // `global_by_name` falls back to the builtin table, so
                    // %Function% stayed visible to `"Function" in this` /
                    // `globalThis.Function` after `delete Function`
                    // (staging/sm/regress/regress-569306.js). Record the
                    // removal the same way `delete globalThis.X` does.
                    if let Some(name) = self.global_slot_name(slot) {
                        if self.builtin_globals.contains_key(&name) {
                            self.deleted_globals.insert(name);
                        }
                    }
                }
                self.set(base, dst, Value::bool(ok));
                ip += 1;
            }
            // Direct eval from strict code: the evaluated string inherits
            // strict mode. Mirrors the `GLOBAL_EVAL` native but forces strict;
            // a non-string argument is returned unchanged (spec 19.2.1).
            Instr::ImportMeta { dst } => {
                #[cfg(feature = "wasm-no-fs-loader")]
                let idx = {
                    // No loader-installed function ranges can exist in
                    // this artifact, so every reachable module uses the
                    // same entry/direct-module singleton as the general
                    // path's `mkey == None` case.
                    if self.import_meta == 0 {
                        let idx = self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
                        self.proto_of.insert(idx, Value::NULL);
                        self.import_meta = idx;
                    }
                    self.import_meta
                };
                #[cfg(not(feature = "wasm-no-fs-loader"))]
                let idx = {
                    // import.meta is DISTINCT per module: the executing
                    // frame's func id falls in exactly one loader module's
                    // installed range (closures keep their module's ids);
                    // outside every range (entry script / run_module /
                    // eval) the Vm-wide singleton applies.
                    let fid = func_id;
                    let mkey = self
                        .module_func_ranges
                        .iter()
                        .find(|&&(s, e, _)| fid >= s && fid < e)
                        .map(|&(_, _, k)| k);
                    match mkey {
                        Some(k) => match self.module_metas.get(&k) {
                            Some(&m) => m,
                            None => {
                                // Lazily created, host-defined: ordinary
                                // extensible null-proto object (GC-rooted
                                // via module_metas), whose `url` is the
                                // module file's (HostGetImportMetaProperties,
                                // as Node, Deno and browsers give it). No
                                // collection runs between the two allocations.
                                let mut meta = ObjMap::new();
                                if let Some(url) = self.module_url(k) {
                                    let url = self.alloc_str(url);
                                    meta.set("url", url);
                                    let resolve = Value::heap(self.heap.alloc(
                                        HeapObj::NativeClosure {
                                            id: native::IMPORT_META_RESOLVE,
                                            state: vec![Value::num(k as f64)],
                                            name: "resolve",
                                            length: 1,
                                        },
                                    ));
                                    meta.set("resolve", resolve);
                                }
                                let m = self.heap.alloc(HeapObj::Object(Box::new(meta)));
                                self.proto_of.insert(m, Value::NULL);
                                self.module_metas.insert(k, m);
                                m
                            }
                        },
                        None => {
                            if self.import_meta == 0 {
                                let idx = self
                                    .heap
                                    .alloc(HeapObj::Object(Box::new(ObjMap::new())));
                                self.proto_of.insert(idx, Value::NULL);
                                self.import_meta = idx;
                            }
                            self.import_meta
                        }
                    }
                };
                self.set(base, dst, Value::heap(idx));
                ip += 1;
            }
            Instr::DirectEval {
                dst,
                callee,
                this_v,
                arg_base,
                argc,
                this_reg,
                call,
            } => {
                use crate::bytecode::EvalFlags as F;
                let crate::bytecode::EvalCall { home_class, flags, site } =
                    self.func(func_id as usize).eval_calls[call as usize];
                let args_array = flags.has(F::ARGS_ARRAY);
                let new_target_ok = flags.has(F::NEW_TARGET_OK);
                let super_static = flags.has(F::SUPER_STATIC);
                let derived_ctor = flags.has(F::DERIVED_CTOR);
                let class_name_ok = flags.has(F::CLASS_NAME_OK);
                let ban_arguments = flags.has(F::BAN_ARGUMENTS);
                let strict_caller = flags.has(F::STRICT_CALLER);
                let super_home_obj = flags.has(F::SUPER_HOME_OBJ);
                let var_env_is_global = flags.has(F::VAR_ENV_IS_GLOBAL);
                let tail = flags.has(F::TAIL);
                // Both the reference and every argument were evaluated
                // before this op. Never re-resolve `eval` here: argument
                // code is allowed to rebind it without redirecting the
                // already captured call reference.
                let live = self.get(base, callee);
                let call_this = self.get(base, this_v);
                // The eval code's frames sit above this one at this call.
                self.sync_top_ip(ip);
                // Direct semantics depend on the captured VALUE's
                // identity, not on where the named reference resolved.
                // A miss ordinary-calls the exact captured value with
                // the complete evaluated argument list.
                let is_intrinsic_eval = if let Some(realm) = self.current_realm_id() {
                    // createRealm gives every child its own %eval%
                    // identity. Require the canonical kind-0 realm-eval
                    // entry as well as its Native id + realm tag: this
                    // cannot confuse another same-id native or a foreign
                    // realm's eval with the current realm intrinsic.
                    live.is_heap()
                        && matches!(
                            self.heap.get(live.heap_index()),
                            HeapObj::Native(native::GLOBAL_EVAL)
                        )
                        && self.get_function_realm(live) == realm
                        && matches!(self.realm_fns.get(&live.heap_index()), Some((_, 0)))
                } else {
                    // Main-realm identity is a single dedicated heap
                    // object; same-native-id lookalikes are not enough.
                    live.is_heap() && live.heap_index() == self.eval_fn_idx
                };
                if !is_intrinsic_eval {
                    let argv: Vec<Value> = if args_array {
                        let args_v = self.get(base, arg_base);
                        self.array_snapshot(args_v.heap_index())
                    } else {
                        (0..argc).map(|i| self.get(base, arg_base + i)).collect()
                    };
                    if tail && self.try_tail_reuse(base, live, call_this, &argv)? {
                        return Ok(ColdFlow::Frame); // re-snapshot the frame coordinates
                    }
                    // A rebound `eval` that is a PLAIN user function takes
                    // the flat call path (push a frame, re-snapshot) — a
                    // nested `call_value` run_loop per level would exhaust
                    // the NATIVE stack long before MAX_FRAMES on runaway
                    // recursion (`function eval(){ eval(); }`), turning a
                    // catchable RangeError into a hard stack overflow.
                    if !args_array {
                        if let Ok((fid, closure)) = self.resolve_callable(live) {
                            if !self.func(fid as usize).is_generator
                                && !self.func(fid as usize).is_async
                            {
                                self.setup_call(
                                    fid,
                                    closure,
                                    call_this,
                                    base,
                                    arg_base,
                                    argc,
                                    dst,
                                    ip + 1,
                                    live,
                                )?;
                                return Ok(ColdFlow::Frame);
                            }
                        }
                    }
                    // A main-realm %eval% invoked from child code is an
                    // INDIRECT eval in the CALLEE's realm. The generic
                    // Native path inherits `active_realm` from the
                    // surrounding child eval, so clear it just for this
                    // exact foreign intrinsic (child eval identities
                    // route themselves through `realm_eval_call`).
                    let saved_realm = self.active_realm;
                    if live.is_heap() && live.heap_index() == self.eval_fn_idx {
                        self.active_realm = None;
                    }
                    let called = self.call_value(live, call_this, &argv);
                    self.active_realm = saved_realm;
                    let r = called?;
                    self.set(base, dst, r);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                // Genuine eval consumes only arg 0. Avoid allocating a
                // Vec on the common non-spread intrinsic path; extras
                // have already been evaluated into the register window.
                let a0 = if args_array {
                    let args_v = self.get(base, arg_base);
                    let spread_argv = self.array_snapshot(args_v.heap_index());
                    spread_argv.first().copied().unwrap_or(Value::UNDEFINED)
                } else if argc == 0 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base)
                };
                let is_str = a0.is_heap()
                    && matches!(
                        self.heap.get(a0.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. }
                    );
                let r = if is_str {
                    let code = self.display(a0);
                    // EXACT WTF-8 bytes when the code string holds lone
                    // surrogates: the lossy `code` aligns with them
                    // byte-for-byte (U+FFFD and a WTF-8 surrogate are both
                    // 3 bytes), letting regex literals recover their exact
                    // pattern text. None (O(1)) for well-formed sources.
                    let exact = self.heap.str_exact_if_not_wellformed(a0.heap_index());
                    // A direct eval inherits the caller's `this` (reg 0, or
                    // the static-field-initializer's `this_reg`) and the
                    // caller's strictness.
                    let caller_this = self.get(base, this_reg);
                    let inherit = (home_class != u32::MAX).then_some((
                        home_class,
                        super_static,
                        derived_ctor,
                        class_name_ok,
                    ));
                    // The caller activation's new.target and (for an
                    // object-literal method) its [[HomeObject]]. Read it
                    // through frame_new_target so an eval in an ARROW body
                    // sees the arrow's lexically-captured new.target rather
                    // than the arrow frame's own (always undefined) slot.
                    let caller_nt = self.frame_new_target();
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
                            // A closure stamped with its CREATOR's eval
                            // scope keeps seeing it: the fresh scope this
                            // activation's own eval declares into is
                            // NESTED inside it, not a replacement.
                            // EXCEPTION: an eval-program frame's stamped
                            // scope IS its variable environment (sloppy
                            // direct eval in function code), so a nested
                            // direct eval declares into that SAME scope —
                            // `eval("eval('var x=1'); typeof x")` sees
                            // the `x`, and nothing leaks to the global.
                            let outer = self.frame_eval_scope(fi);
                            if self.frames[fi].is_eval && outer.is_some() {
                                outer
                            } else {
                                let s = self.heap.alloc(HeapObj::EvalScope(
                                    std::collections::HashMap::new(),
                                ));
                                self.frames[fi].eval_scope = s;
                                if let Some(p) = outer {
                                    self.eval_scope_parent.insert(s, p);
                                }
                                Some(s)
                            }
                        } else {
                            Some(self.frames[fi].eval_scope)
                        }
                    } else {
                        None
                    };
                    // A declaration-free eval this site has run before, with
                    // the same caller bindings live, re-runs the program it
                    // installed then (`direct_eval_cache`). Not for a caller
                    // with a home class or object, private names or a realm
                    // in play, a source with lone surrogates, or one with a
                    // template literal (each eval's tagged templates are
                    // their own objects, and template objects are cached by
                    // function).
                    let flags = (strict_caller as u8)
                        | (new_target_ok as u8) << 1
                        | (ban_arguments as u8) << 2
                        | (var_env_is_global as u8) << 3
                        | (eval_scope_idx.is_some() as u8) << 4;
                    let cacheable = site != u16::MAX
                        && inherit.is_none()
                        && caller_home.is_none()
                        && exact.is_none()
                        && code.len() <= DIRECT_EVAL_CACHE_MAX_SOURCE
                        && !code.contains('`')
                        && self.direct_eval_cacheable_now();
                    let live_cells = if cacheable {
                        let map = &self.func(func_id as usize).eval_sites[site as usize].0;
                        (map.len() <= 64).then(|| {
                            let mut mask = 0u64;
                            let mut cells = Vec::with_capacity(map.len());
                            for (i, &(_, kind, idx)) in map.iter().enumerate() {
                                let cv = self.eval_site_cell(base, kind, idx);
                                if cv.is_heap() {
                                    mask |= 1 << i;
                                    cells.push(cv);
                                }
                            }
                            (mask, cells)
                        })
                    } else {
                        None
                    };
                    let source_hash = {
                        use std::hash::{Hash, Hasher};
                        let mut h = rustc_hash::FxHasher::default();
                        code.hash(&mut h);
                        h.finish()
                    };
                    if let Some((mask, cells)) = live_cells.as_ref() {
                        let key = (func_id, site, *mask, flags, source_hash);
                        let hit = self
                            .direct_eval_cache
                            .get(&key)
                            .and_then(|v| v.iter().find(|(s, _)| **s == *code))
                            .map(|&(_, base_func)| base_func);
                        if let Some(base_func) = hit {
                            // What `do_eval` charges every runtime compile.
                            self.require_external_code_enabled()?;
                            #[cfg(feature = "instrument")]
                            self.instrument_dynamic_code_attempt(code.len())
                                .map_err(|message| Thrown(message.into()))?;
                            let r = self.execute_eval_program(
                                base_func,
                                Some(caller_this),
                                None,
                                caller_nt,
                                None,
                                Some(cells.clone()),
                                eval_scope_idx,
                            )?;
                            self.set(base, dst, r);
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        self.direct_eval_want = true;
                        self.direct_eval_installed = None;
                    }
                    let evaled = self.do_eval(
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
                        // Visible LEXICAL caller bindings: a sloppy
                        // eval's var/fn name colliding with one is a
                        // SyntaxError.
                        if site != u16::MAX {
                            self.func(func_id as usize).eval_sites[site as usize]
                                .2
                                .clone()
                        } else {
                            Vec::new()
                        },
                        {
                            // The site's visible caller bindings: their
                            // boxed cells become the eval closure's
                            // upvalues.
                            if site != u16::MAX {
                                let (map, _, _) = self.func(func_id as usize).eval_sites
                                    [site as usize]
                                    .clone();
                                let mut names = Vec::with_capacity(map.len());
                                let mut cells = Vec::with_capacity(map.len());
                                let mut outer = Vec::new();
                                let mut catch_params = Vec::new();
                                for (n, kind, idx) in map {
                                    let cv = self.eval_site_cell(base, kind, idx);
                                    if cv.is_heap() {
                                        if kind == 2 || kind == 3 {
                                            outer.push(n.clone());
                                        }
                                        if kind == 3 {
                                            catch_params.push(n.clone());
                                        }
                                        names.push(n);
                                        cells.push(cv);
                                    }
                                }
                                Some((names, cells, outer, catch_params))
                            } else {
                                None
                            }
                        },
                        eval_scope_idx,
                        exact.as_deref(),
                        None,
                    );
                    // `direct_eval_installed` is set only by the `do_eval` this
                    // arm asked, and taken here even when that eval threw: a
                    // nested eval's arm clears it before its own `do_eval`, so
                    // a stale report never names another eval's program.
                    let installed = self.direct_eval_installed.take();
                    let r = evaled?;
                    if let (Some((mask, _)), Some(base_func)) = (live_cells, installed) {
                        if self.direct_eval_cache_len
                            < DIRECT_EVAL_CACHE_MAX_ENTRIES
                        {
                            self.direct_eval_cache
                                .entry((func_id, site, mask, flags, source_hash))
                                .or_default()
                                .push((code.as_str().into(), base_func));
                            self.direct_eval_cache_len += 1;
                        }
                    }
                    r
                } else {
                    a0
                };
                self.set(base, dst, r);
                ip += 1;
            }

            Instr::DisposeScope {
                scope,
                kind_reg,
                val_reg,
            } => {
                // DisposeResources: run this scope's disposers LIFO, merging
                // any throw with the incoming completion (kind&3==2 â‡’ the
                // block already threw) into a SuppressedError chain; rewrite
                // kind/val so the following EndFinally re-raises the merge.
                let list = self.get(base, scope);
                let disposers = self.using_scope_take(list);
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
                let list = self.get(base, scope);
                if v.is_nullish() {
                    // Inert: sets needsAwait at disposal, calls nothing.
                    self.using_scope_push(list, Value::UNDEFINED, true);
                    ip += 1;
                } else {
                    if !self.is_object_value(v) {
                        return Err(Thrown(
                            "TypeError: an 'await using' declaration value is not an object".into(),
                        ));
                    }
                    let mut method = self.get_member(v, "@@asyncDispose", v)?;
                    // Only the @@dispose FALLBACK gets the result-discarding
                    // shim — see `sync_dispose_shim`.
                    let mut sync_fallback = false;
                    if !self.is_callable(method) {
                        method = self.get_member(v, "@@dispose", v)?;
                        sync_fallback = true;
                    }
                    if !self.is_callable(method) {
                        return Err(Thrown(
                            "TypeError: the 'await using' value has no callable [Symbol.asyncDispose] or [Symbol.dispose]"
                                .into(),
                        ));
                    }
                    let disposer = if sync_fallback {
                        // The first use compiles the shim with an eval;
                        // `method` may be a getter's fresh function.
                        let shim =
                            self.with_host_roots(&[method], Self::sync_dispose_shim)?;
                        Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: shim,
                            this: method,
                            args: vec![v],
                        }))
                    } else {
                        Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: method,
                            this: v,
                            args: Vec::new(),
                        }))
                    };
                    self.using_scope_push(list, disposer, true);
                    ip += 1;
                }
            }
            Instr::AsyncDisposeNext { scope, res, done } => {
                // Pop the LAST (LIFO) disposer of the scope. Empty â†’ done. An
                // inert `undefined` entry yields res=undefined (nothing called).
                // A real bound disposer is CALLED here (carrying its `this`);
                // its result is left in `res` for the caller to Await. A sync
                // throw propagates (caught by the loop's handler).
                //
                // DisposeResources' await economy: an inert entry only
                // sets needsAwait, a sync-hint entry's result is not
                // awaited, and the one Await(undefined) a nullish entry
                // owes is paid before the next sync call or at the end,
                // and only when no async result was awaited. Each step
                // runs until it has a value to Await (done=false) or the
                // list is spent (done=true). Awaiting every entry cost a
                // microtask tick per null and per sync `using`.
                let list = self.get(base, scope);
                let r = loop {
                    let (needs_await, has_awaited) = self.using_scope_flags(list);
                    let owed = needs_await && !has_awaited;
                    match self.using_scope_pop(list) {
                        None if owed => {
                            self.set_using_scope_flags(list, false, has_awaited);
                            break Some(Value::UNDEFINED);
                        }
                        None => break None,
                        Some((d, false)) if owed => {
                            // Await(undefined) BEFORE this sync disposer.
                            self.set_using_scope_flags(list, false, has_awaited);
                            self.using_scope_push(list, d, false);
                            break Some(Value::UNDEFINED);
                        }
                        Some((d, false)) => {
                            self.call_value(d, Value::UNDEFINED, &[])?;
                        }
                        Some((d, true)) if d.is_nullish() => {
                            self.set_using_scope_flags(list, true, has_awaited);
                        }
                        Some((d, true)) => {
                            let r = self.call_value(d, Value::UNDEFINED, &[])?;
                            let (needs_await, _) = self.using_scope_flags(list);
                            self.set_using_scope_flags(list, needs_await, true);
                            break Some(r);
                        }
                    }
                };
                self.set(base, done, Value::bool(r.is_none()));
                self.set(base, res, r.unwrap_or(Value::UNDEFINED));
                ip += 1;
            }
            Instr::GetAsyncIterator { dst, src, sync_dst } => {
                let s = self.get(base, src);
                let (it, is_sync) = self.get_async_iterator(s)?;
                self.set(base, dst, it);
                self.set(base, sync_dst, Value::bool(is_sync));
                ip += 1;
            }
            Instr::DateNew {
                dst,
                arg_base,
                argc,
            } => {
                let args: Vec<Value> =
                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                let ms = self.date_new_ms(&args)?;
                let v = Value::heap(self.heap.alloc(HeapObj::Date(ms)));
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::DateUTC {
                dst,
                arg_base,
                argc,
            } => {
                let args: Vec<Value> =
                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                let ms = self.date_utc_ms(&args)?;
                self.set(base, dst, Value::num(ms));
                ip += 1;
            }
            Instr::AsyncIterNextStep {
                dst,
                iter,
                idx,
                sent,
                next_fn,
            } => {
                // Like ForAwaitNext, but CALLS the cached next method (next_fn,
                // captured once at GetIterator time) with the `.next(v)` sent
                // value (yield* sent-value forwarding + no re-get of `next`).
                let it = self.get(base, iter);
                let sent_v = self.get(base, sent);
                let nf = self.get(base, next_fn);
                if !it.is_heap() {
                    return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
                }
                let gen_direct = matches!(self.heap.get(it.heap_index()), HeapObj::Generator(_))
                    && self.gen_method_pristine(it.heap_index(), "next");
                let result = match self.heap.get(it.heap_index()) {
                    HeapObj::AsyncGenerator(_) => self
                        .async_generator_method(it.heap_index(), "next", &[sent_v])
                        .unwrap_or(Value::UNDEFINED),
                    HeapObj::Generator(_) if gen_direct => self
                        .generator_method(it.heap_index(), "next", &[sent_v])?
                        .unwrap_or(Value::UNDEFINED),
                    // A generator whose `next` was replaced calls it.
                    HeapObj::Object(_) | HeapObj::Generator(_) => {
                        if self.is_callable(nf) {
                            self.call_value(nf, it, &[sent_v])?
                        } else {
                            return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
                        }
                    }
                    _ => {
                        let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        // A Set's / Map's tombstoned (deleted) slots are skipped.
                        while match self.heap.get(it.heap_index()) {
                            HeapObj::Set(items) => {
                                cursor < items.len() && items[cursor].is_hole()
                            }
                            HeapObj::Map { keys, .. } => {
                                cursor < keys.len() && keys[cursor].is_hole()
                            }
                            _ => false,
                        } {
                            cursor += 1;
                        }
                        // A STRING iterates by CODE POINT (one step per
                        // scalar) while the cursor advances in UTF-16
                        // units; flatten a rope first (no-op otherwise).
                        self.heap.flatten(it.heap_index());
                        let str_step = match self.heap.get(it.heap_index()) {
                            HeapObj::Str(s) => Some(s.cp_step(cursor)),
                            _ => None,
                        };
                        if let Some(step) = str_step {
                            match step {
                                Some((cp, next)) => {
                                    // `cp` may be a lone surrogate — a real
                                    // 1-unit surrogate string, not U+FFFD.
                                    let val = self.str_from_cp(cp);
                                    self.set(base, idx, Value::int(next as i32));
                                    self.iter_result(val, false)
                                }
                                None => self.iter_result(Value::UNDEFINED, true),
                            }
                        } else {
                            // `coll`: a Map/Set step reads its entry positionally
                            // (`collection_entry_at`); an Array is an ordinary index.
                            let (len, coll) = match self.heap.get(it.heap_index()) {
                                HeapObj::Array(items) => (items.len(), false),
                                HeapObj::Set(items) => (items.len(), true),
                                HeapObj::Map { keys, .. } => (keys.len(), true),
                                _ => {
                                    return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
                                }
                            };
                            if cursor < len {
                                let val = if coll {
                                    self.collection_entry_at(it, cursor)
                                } else {
                                    self.get_index(it, Value::int(cursor as i32))?
                                };
                                self.set(base, idx, Value::int((cursor + 1) as i32));
                                self.iter_result(val, false)
                            } else {
                                self.iter_result(Value::UNDEFINED, true)
                            }
                        }
                    }
                };
                self.set(base, dst, result);
                ip += 1;
            }
            Instr::IterDelegate {
                value_dst,
                done_dst,
                ret_dst,
                iter,
                mode,
                sent,
            } => {
                let iter_v = self.get(base, iter);
                let mode_code = self.get(base, mode).as_int();
                let sent_v = self.get(base, sent);
                // A `next` step into a generator with the intrinsic `next`
                // runs it directly; a plain yield comes back as its value,
                // which the YieldDelegate then yields as a value too.
                if mode_code == 0 {
                    // Run the step as a frame of this loop; its yield or
                    // return completes this instruction.
                    if iter_v.is_heap()
                        && matches!(self.heap.get(iter_v.heap_index()), HeapObj::Generator(_))
                        && self.gen_method_pristine(iter_v.heap_index(), "next")
                        && self.gen_enter_inline(iter_v.heap_index(), ip, value_dst, done_dst, sent_v, true)
                    {
                        return Ok(ColdFlow::Frame);
                    }
                    if let Some(step) = self.generator_delegate_step(iter_v, sent_v)? {
                        let (val, done_b, raw) = match step {
                            crate::vm::async_runtime::GenStep::Yield(y) => (y, false, false),
                            crate::vm::async_runtime::GenStep::Raw(r) => (r, false, true),
                            crate::vm::async_runtime::GenStep::Done(v) => (v, true, false),
                        };
                        self.pending_yield_raw = raw;
                        self.set(base, value_dst, val);
                        self.set(base, done_dst, Value::bool(done_b));
                        self.set(base, ret_dst, Value::bool(false));
                        ip += 1;
                        return Ok(ColdFlow::Next(ip));
                    }
                }
                let (val, done_b, ret_b) =
                    self.iter_delegate_step(iter_v, mode_code, sent_v)?;
                // Not done ⇒ `val` is the inner iterator's RAW result object
                // and the immediately-following `YieldDelegate` must forward
                // it verbatim — flag the suspension so `gen_resume` skips the
                // fresh `{value, done}` re-wrap. Nothing observable runs
                // between here and the YieldDelegate (two register jumps).
                self.pending_yield_raw = !done_b && !ret_b;
                self.set(base, value_dst, val);
                self.set(base, done_dst, Value::bool(done_b));
                self.set(base, ret_dst, Value::bool(ret_b));
                ip += 1;
            }
            Instr::AsyncFromSyncStep { dst, step, iter } => {
                // AsyncFromSyncIteratorContinuation(step): read done/
                // value from the SYNC result (IteratorComplete BEFORE
                // IteratorValue, spec steps 2/4), PromiseResolve the
                // value (observable constructor read for a native
                // promise, here in ordinary dispatch context, BEFORE
                // any job), and chain the unwrap reaction into a fresh
                // capability promise resolving { value: await value,
                // done }. IfAbruptRejectPromise: a throw from any step
                // here (done/value getters, the constructor read)
                // REJECTS the capability — next() returns it normally
                // and the following Await surfaces the rejection at
                // the await point (with the frame's handlers
                // restored). closeOnRejection (steps 7/13): with
                // done == false, an abrupt PromiseResolve closes the
                // sync iterator FIRST (errors swallowed, the original
                // reason wins), and a rejecting awaited value routes
                // through AFS_CLOSE_REJECT (close, then re-reject).
                let s = self.get(base, step);
                let it = self.get(base, iter);
                let cap = self.alloc_promise();
                // `cap` is not published to `dst` until every observable
                // done/value/constructor getter below has completed. Keep
                // it alive while those getters may re-enter and collect;
                // all labelled exits converge on the matching pop below.
                self.promise_resolution_roots.push(cap);
                let abrupt = |vm: &mut Self, cap: u32, msg: &str| {
                    let reason = vm
                        .pending_throw
                        .take()
                        .unwrap_or_else(|| vm.error_from_thrown(msg));
                    vm.reject(cap, reason);
                };
                'cont: {
                    let done_v = match self.get_prop(s, "done") {
                        Ok(v) => v,
                        Err(Thrown(msg)) => {
                            abrupt(self, cap, &msg);
                            break 'cont;
                        }
                    };
                    let done = self.truthy(done_v);
                    let value = match self.get_prop(s, "value") {
                        Ok(v) => v,
                        Err(Thrown(msg)) => {
                            abrupt(self, cap, &msg);
                            break 'cont;
                        }
                    };
                    let wrapper = if value.is_heap()
                        && matches!(
                            self.heap.get(value.heap_index()),
                            HeapObj::Promise { .. }
                        ) {
                        match self.get_prop(value, "constructor") {
                            Ok(c) => {
                                let intrinsic = self
                                    .global_by_name("Promise")
                                    .unwrap_or(Value::UNDEFINED);
                                if c == intrinsic {
                                    value.heap_index()
                                } else {
                                    let p = self.alloc_promise();
                                    self.resolve_fresh_promise(p, value);
                                    p
                                }
                            }
                            Err(Thrown(msg)) => {
                                // Step 7: IteratorClose(syncIterator,
                                // valueWrapper) — quiet, the original
                                // error rejects the capability.
                                if !done {
                                    let original = self.pending_throw.take();
                                    let _ = self.iterator_close(it);
                                    self.pending_throw = original;
                                }
                                abrupt(self, cap, &msg);
                                break 'cont;
                            }
                        }
                    } else {
                        let p = self.alloc_promise();
                        self.resolve_fresh_promise(p, value);
                        p
                    };
                    let target = Value::heap(
                        self.heap.alloc(HeapObj::Native(native::UNWRAP_ITER_RESULT)),
                    );
                    let unwrap = Value::heap(self.heap.alloc(HeapObj::Bound {
                        target,
                        this: Value::bool(done),
                        args: Vec::new(),
                    }));
                    // Step 13: onRejected closes the sync iterator only
                    // while iteration is incomplete.
                    let on_reject = if done {
                        Value::UNDEFINED
                    } else {
                        let t = Value::heap(
                            self.heap.alloc(HeapObj::Native(native::AFS_CLOSE_REJECT)),
                        );
                        Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: t,
                            this: Value::UNDEFINED,
                            args: vec![it],
                        }))
                    };
                    self.then_internal(wrapper, unwrap, on_reject, Some(cap));
                }
                self.set(base, dst, Value::heap(cap));
                let popped = self.promise_resolution_roots.pop();
                debug_assert_eq!(popped, Some(cap));
                ip += 1;
            }
            Instr::ForAwaitNext { dst, iter, idx } => {
                let it = self.get(base, iter);
                if !it.is_heap() {
                    return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
                }
                let result = match self.heap.get(it.heap_index()) {
                    // Async iterator: `.next()` returns a Promise the loop awaits.
                    HeapObj::AsyncGenerator(_) => self
                        .async_generator_method(it.heap_index(), "next", &[])
                        .unwrap_or(Value::UNDEFINED),
                    // Sync generator: `.next()` returns {value,done} (awaited = no-op tick).
                    HeapObj::Generator(_)
                        if self.gen_method_pristine(it.heap_index(), "next") =>
                    {
                        self.generator_method(it.heap_index(), "next", &[])?
                            .unwrap_or(Value::UNDEFINED)
                    }
                    // A user iterator object (sync or async) with `.next()` —
                    // including a BUILTIN iterator / iterator helper handed
                    // over by a custom @@asyncIterator (driven through its
                    // prototype `next`, same protocol), and a generator
                    // whose `next` was replaced.
                    HeapObj::Object(_)
                    | HeapObj::Proxy { .. }
                    | HeapObj::Iterator { .. }
                    | HeapObj::IterHelper(_)
                    | HeapObj::Generator(_) => {
                        let next = self.get_prop(it, "next")?;
                        if self.is_callable(next) {
                            self.call_value(next, it, &[])?
                        } else {
                            return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
                        }
                    }
                    // Array/Set element, string char, Map [k,v] â€” positional,
                    // wrapped in a {value, done} the loop awaits (a tick).
                    _ => {
                        let mut cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        // A Set's / Map's tombstoned (deleted) slots are skipped.
                        while match self.heap.get(it.heap_index()) {
                            HeapObj::Set(items) => {
                                cursor < items.len() && items[cursor].is_hole()
                            }
                            HeapObj::Map { keys, .. } => {
                                cursor < keys.len() && keys[cursor].is_hole()
                            }
                            _ => false,
                        } {
                            cursor += 1;
                        }
                        // A STRING iterates by CODE POINT (one step per
                        // scalar) while the cursor advances in UTF-16
                        // units; flatten a rope first (no-op otherwise).
                        self.heap.flatten(it.heap_index());
                        let str_step = match self.heap.get(it.heap_index()) {
                            HeapObj::Str(s) => Some(s.cp_step(cursor)),
                            _ => None,
                        };
                        if let Some(step) = str_step {
                            match step {
                                Some((cp, next)) => {
                                    // `cp` may be a lone surrogate — a real
                                    // 1-unit surrogate string, not U+FFFD.
                                    let val = self.str_from_cp(cp);
                                    self.set(base, idx, Value::int(next as i32));
                                    self.iter_result(val, false)
                                }
                                None => self.iter_result(Value::UNDEFINED, true),
                            }
                        } else {
                            // `coll`: a Map/Set step reads its entry positionally
                            // (`collection_entry_at`); an Array is an ordinary index.
                            let (len, coll) = match self.heap.get(it.heap_index()) {
                                HeapObj::Array(items) => (items.len(), false),
                                HeapObj::Set(items) => (items.len(), true),
                                HeapObj::Map { keys, .. } => (keys.len(), true),
                                _ => {
                                    return Err(Thrown::join3("TypeError: ", &self.display(it), " is not iterable"));
                                }
                            };
                            if cursor < len {
                                let val = if coll {
                                    self.collection_entry_at(it, cursor)
                                } else {
                                    self.get_index(it, Value::int(cursor as i32))?
                                };
                                self.set(base, idx, Value::int((cursor + 1) as i32));
                                self.iter_result(val, false)
                            } else {
                                self.iter_result(Value::UNDEFINED, true)
                            }
                        }
                    }
                };
                self.set(base, dst, result);
                ip += 1;
            }
            Instr::LoadClassValue { dst, class_id } => {
                // The inner class-name binding (used by methods/ctor/static
                // blocks + arrows within them). None = accessed before the
                // class value is initialized (e.g. `class C extends C`) → TDZ.
                match self.class_value_for(class_id) {
                    // A class DECORATOR may have replaced the class:
                    // ClassDefinitionEvaluation binds classBinding to the
                    // decorated value, so the inner name means the
                    // replacement. `class_values` keeps the original —
                    // that is what carries the DecState and the computed
                    // field keys — so the hop lives here and nowhere else.
                    Some(v) => {
                        let v = match self.heap.get(v.heap_index()) {
                            HeapObj::Class(c) => match &c.dec {
                                Some(d) if d.replacement.is_heap() => d.replacement,
                                _ => v,
                            },
                            _ => v,
                        };
                        self.set(base, dst, v)
                    }
                    None => {
                        return Err(Thrown(
                            "ReferenceError: class binding accessed before initialization"
                                .into(),
                        ));
                    }
                }
                ip += 1;
            }
            Instr::LoadGlobalOrUndefinedDyn { dst, idx } => {
                if let Some(v) = self.eval_scope_lookup(idx) {
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                let v = self.globals[idx as usize];
                let v = if v.is_uninitialized() {
                    match self.global_slot_name(idx) {
                        Some(name) if self.global_this != 0 => {
                            let has_own = matches!(
                                self.heap.get(self.global_this),
                                HeapObj::Object(m) if m.pos(&name).is_some()
                            );
                            let gobj = Value::heap(self.global_this);
                            let proto = self.object_get_prototype_of(gobj);
                            if has_own
                                || (proto.is_heap()
                                    && self.has_property_str_dyn(proto, &name)?)
                            {
                                self.get_prop(gobj, &name)?
                            } else {
                                Value::UNDEFINED
                            }
                        }
                        _ => Value::UNDEFINED,
                    }
                } else {
                    v
                };
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::EvalScopeHas { dst, idx } => {
                let has = self.eval_scope_lookup(idx).is_some();
                self.set(base, dst, Value::bool(has));
                ip += 1;
            }
            Instr::EvalScopeSet { idx, src } => {
                let v = self.get(base, src);
                if !self.eval_scope_store(idx, v) {
                    // The probed binding vanished (cannot normally
                    // happen): fall back to the global slot.
                    self.globals[idx as usize] = v;
                }
                ip += 1;
            }
            Instr::CheckGlobalResolvable { idx } => {
                // ResolveBinding for a strict `name = …`, performed BEFORE
                // the RHS evaluates (the compiler emits this ahead of the
                // RHS and stores with StoreGlobalResolved after it). The
                // reference is resolvable when the slot is live, when a
                // same-named own property of the global object backs it
                // (an eval-created var), when it is a builtin, or when
                // the global object's PROTO CHAIN has it. An own prop the
                // RHS itself creates must NOT resolve it
                // (`undeclared = (this.undeclared = 5)` still throws).
                // The ReferenceError itself is DEFERRED to the store:
                // PutValue runs after the RHS, so an RHS exception must
                // win (`seen = act()` where act() throws propagates the
                // act() throw — staging/sm/Proxy/getPrototypeOf).
                // An unnamed pool slot is a DECLARED module/realm
                // binding: the store itself raises its error.
                let name = if self.globals[idx as usize].is_uninitialized() {
                    self.global_slot_name(idx)
                } else {
                    None
                };
                if let Some(name) = name {
                    let own_backed = !self.global_name_is_lexical(&name)
                        && self.global_this != 0
                        && matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                    let mut resolvable = own_backed || self.global_by_name(&name).is_some();
                    if !resolvable {
                        let gobj = Value::heap(self.global_this);
                        let proto = if self.global_this != 0 {
                            self.object_get_prototype_of(gobj)
                        } else {
                            Value::NULL
                        };
                        resolvable =
                            proto.is_heap() && self.has_property_str_dyn(proto, &name)?;
                    }
                    if resolvable {
                        // Drop any stale entry a caught RHS throw left.
                        self.strict_unresolvable_globals.retain(|&i| i != idx);
                    } else {
                        self.strict_unresolvable_globals.push(idx);
                    }
                }
                ip += 1;
            }
            Instr::ArrayRest { dst, src, start } => {
                let sv = self.get(base, src);
                // `src` is IterToArray's normalized value (the only
                // producer of this op's operand), whose GetIterator
                // already ran: walk it without a second one.
                let mut elems = self.destructure_rest(sv)?;
                let start = (start as usize).min(elems.len());
                let rest = elems.split_off(start);
                let arr = Value::heap(self.heap.alloc(HeapObj::Array(rest)));
                self.set(base, dst, arr);
                ip += 1;
            }
            Instr::DecKey {
                class,
                elem,
                key,
                class_id: _,
            } => {
                let cv = self.get(base, class);
                let kraw = self.get(base, key);
                // ToPropertyKey HERE and write the result back, so the
                // member/field op that consumes the same register sees an
                // already-coerced key: the element's ClassElementName is
                // coerced exactly ONCE observably (a key object counting
                // its `toString` calls must see one), and `context.name`
                // is the resulting String/Symbol rather than the raw
                // object.
                let k = self.coerce_index_key(kraw)?;
                self.set(base, key, k);
                self.dec_record_key(cv, elem as usize, k);
                ip += 1;
            }
            Instr::DecInits {
                class_id,
                which,
                elem,
                recv,
            } => {
                let r = self.get(base, recv);
                self.dec_run_inits(class_id, which, elem as usize, r)?;
                ip += 1;
            }
            Instr::DecField {
                class_id,
                elem,
                val,
                recv,
            } => {
                // `v = init(v)` per initializer, innermost decorator
                // first. The running value goes back into its register
                // after every step so it stays a GC root across the next
                // call — this runs per decorated field per `new`, so
                // pinning the collector instead would be a real cost.
                for f in self.dec_field_inits(class_id, elem as usize) {
                    let v = self.get(base, val);
                    let r = self.get(base, recv);
                    let out = self.call_value(f, r, &[v])?;
                    self.set(base, val, out);
                }
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
                // Nursery barrier: the key coercion above can run user
                // code (@@toPrimitive), whose safe points can promote
                // the class mid-definition.
                self.heap.write_barrier_val(cv.heap_index(), k);
                if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                    c.computed_field_keys.push(k);
                }
                ip += 1;
            }
            Instr::SuperCtorFetch { dst, home_class_id } => {
                // GetSuperConstructor() BEFORE the argument list runs
                // (spec SuperCall order): the LIVE [[GetPrototypeOf]] of
                // the class object — Object.setPrototypeOf(C, X) in an
                // argument must NOT retarget THIS super() call, and a
                // non-constructor parent is only reported by the SuperCtor
                // op after the args (call-proto-not-ctor).
                let f = self.super_ctor_fetch(home_class_id);
                self.set(base, dst, f);
                ip += 1;
            }
            Instr::SuperMethod {
                dst,
                base: base_reg,
                home_class_id,
                name,
                arg_base,
                argc,
            } => {
                // `func()` returns `&'p`, so the interned name key outlives
                // any `&mut self` below â€” and resolves eval functions too.
                let key: &'p str =
                    &self.func(func_id as usize).string_constants[name as usize];
                // ── interpreter super-call IC ── `super.m()` is
                // receiver-independent (home class + its prototype
                // chain, version-guarded): a hit frame-calls the
                // resolved method with `this` = the current receiver,
                // skipping super_base + the chain walk + call_value's
                // nested run_loop (and its per-call args Vec).
                {
                    let is_static = self.func(func_id as usize).super_static;
                    if let Some((fid, closure, callee)) =
                        self.ic_super_method(func_id, ip, home_class_id, is_static, key)
                    {
                        let this = self.get(base, 0);
                        self.setup_call(
                            fid,
                            closure,
                            this,
                            base,
                            arg_base,
                            argc,
                            dst,
                            ip + 1,
                            callee,
                        )?;
                        return Ok(ColdFlow::Frame);
                    }
                }
                // super.m() resolves m via the super base CAPTURED before
                // the argument list ran (SuperBase) with `this` = the
                // receiver — like a normal property get + call (and like
                // SuperMethodComputed). This reaches inherited methods,
                // accessors, and base-class super (→ %Object.prototype%),
                // not just own parent-class methods. (The IC above is
                // version-guarded on the same chain, so a base retargeted
                // during argument evaluation misses it and lands here.)
                let proto = self.get(base, base_reg);
                // MakeSuperPropertyReference: RequireObjectCoercible(base).
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let m = self.get_member(proto, key, this)?;
                if !self.is_callable(m) {
                    return Err(Thrown::join3("TypeError: super.", &*key, " is not a function"));
                }
                let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                for i in 0..argc {
                    args.push(self.get(base, arg_base + i));
                }
                let r = self.call_value(m, this, &args)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperGetComputed {
                dst,
                home_class_id,
                key,
            } => {
                let kv = self.get(base, key);
                // GetSuperBase runs at MakeSuperPropertyReference time —
                // BEFORE GetValue's ToPropertyKey coercion (whose
                // toString may retarget the home object's prototype):
                // prop-expr-getsuperbase-before-topropertykey-*.
                let proto = self
                    .super_base(home_class_id, self.func(func_id as usize).super_static);
                let ks = self.to_property_key(kv)?;
                // MakeSuperPropertyReference: RequireObjectCoercible(base).
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let r = self.get_member(proto, &ks, this)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperGetRef {
                dst,
                home_class_id,
                name,
                receiver,
                is_static,
            } => {
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                // The call-reference form carries both context values
                // explicitly so an inline static field initializer is
                // independent of the enclosing frame's metadata/reg 0.
                match self.ic_super_get(func_id, ip, home_class_id, is_static, key) {
                    GetAct::Value(v) => {
                        self.set(base, dst, v);
                        ip += 1;
                        return Ok(ColdFlow::Next(ip));
                    }
                    GetAct::Accessor {
                        fid,
                        closure,
                        getter,
                    } => {
                        let this = self.get(base, receiver);
                        self.setup_call(
                            fid,
                            closure,
                            this,
                            base,
                            0,
                            0,
                            dst,
                            ip + 1,
                            getter,
                        )?;
                        return Ok(ColdFlow::Frame);
                    }
                    GetAct::None => {}
                }
                let proto = self.super_base(home_class_id, is_static);
                self.require_object_coercible(proto)?;
                let this = self.get(base, receiver);
                let r = self.get_member(proto, &key, this)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperGetRefComputed {
                dst,
                home_class_id,
                key,
                receiver,
                is_static,
            } => {
                let kv = self.get(base, key);
                let proto = self.super_base(home_class_id, is_static);
                let ks = self.to_property_key(kv)?;
                self.require_object_coercible(proto)?;
                let this = self.get(base, receiver);
                let r = self.get_member(proto, &ks, this)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperMethodComputed {
                dst,
                base: base_reg,
                home_class_id: _,
                key,
                arg_base,
                argc,
            } => {
                let kv = self.get(base, key);
                // The base was captured right after the key ran, BEFORE
                // the argument list (SuperBase).
                let proto = self.get(base, base_reg);
                let ks = self.to_property_key(kv)?;
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
            Instr::SuperSetComputed {
                base: base_reg,
                home_class_id: _,
                key,
                val,
            } => {
                let kv = self.get(base, key);
                // The base was captured right after the key ran, BEFORE
                // the RHS (SuperBase) — but still before ToPropertyKey's
                // coercion, matching GetSuperBase-before-ToPropertyKey.
                let proto = self.get(base, base_reg);
                let ks = self.to_property_key(kv)?;
                let this = self.get(base, 0);
                let v = self.get(base, val);
                let strict = self.func(func_id as usize).is_strict;
                self.super_set_obj(proto, &ks, this, v, strict)?;
                ip += 1;
            }
            Instr::SetHomeObject { method, home } => {
                let m = self.get(base, method);
                if m.is_heap() {
                    let h = self.get(base, home);
                    self.record_closure_home(m.heap_index(), h);
                }
                ip += 1;
            }
            Instr::SuperGetObj { dst, name } => {
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                let proto = self.obj_super_base(self.frames[frame_idx].callee);
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let r = self.get_member(proto, &key, this)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperGetObjComputed { dst, key } => {
                let kv = self.get(base, key);
                // GetSuperBase BEFORE ToPropertyKey (see SuperGetComputed).
                let proto = self.obj_super_base(self.frames[frame_idx].callee);
                let ks = self.to_property_key(kv)?;
                self.require_object_coercible(proto)?;
                let this = self.get(base, 0);
                let r = self.get_member(proto, &ks, this)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::SuperSetObj { name, val } => {
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                let proto = self.obj_super_base(self.frames[frame_idx].callee);
                let this = self.get(base, 0);
                let v = self.get(base, val);
                // A sloppy object-literal method's rejected super-write
                // is a silent no-op (prop-dot-obj-ref-non-strict).
                let strict = self.func(func_id as usize).is_strict;
                self.super_set_obj(proto, &key, this, v, strict)?;
                ip += 1;
            }
            Instr::SuperSetObjComputed { key, val } => {
                let kv = self.get(base, key);
                // GetSuperBase BEFORE ToPropertyKey (see SuperGetComputed).
                let proto = self.obj_super_base(self.frames[frame_idx].callee);
                let ks = self.to_property_key(kv)?;
                let this = self.get(base, 0);
                let v = self.get(base, val);
                let strict = self.func(func_id as usize).is_strict;
                self.super_set_obj(proto, &ks, this, v, strict)?;
                ip += 1;
            }
            Instr::NewWeakMap { dst, src } => {
                // Build empty, then AddEntriesFromIterable via the observable
                // `set` adder (so non-registered symbol keys validate via
                // CanBeHeldWeakly, the adder is observably called, and an
                // abrupt closes the iterator).
                let wm_idx = self.heap.alloc(HeapObj::WeakMap {
                    keys: Vec::new(),
                    vals: Vec::new(),
                });
                self.register_weak_container(wm_idx);
                let wm = Value::heap(wm_idx);
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
                let ws_idx = self.heap.alloc(HeapObj::WeakSet(Vec::new()));
                self.register_weak_container(ws_idx);
                let ws = Value::heap(ws_idx);
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
                // CanBeHeldWeakly: any object, or a non-registered Symbol.
                if !self.can_be_held_weakly(t) {
                    return Err(Thrown(
                        "TypeError: WeakRef: target cannot be held weakly".into(),
                    ));
                }
                self.keep_during_job(t);
                let wr_idx = self.heap.alloc(HeapObj::WeakRef(t));
                self.register_weak_container(wr_idx);
                let wr = Value::heap(wr_idx);
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
                        Value::bool(
                            arg.map(|a| self.truthy(self.get(base, a))).unwrap_or(false),
                        )
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
                let fr_idx = self.heap.alloc(HeapObj::FinalizationRegistry(Box::new(crate::heap::FinRegData {
                    cleanup: cb,
                    cells: Vec::new(),
                    cleared: Vec::new(),
                    cleanup_queued: false,
                })));
                self.register_weak_container(fr_idx);
                let fr = Value::heap(fr_idx);
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
                // Publish before invoking the executor. Besides matching
                // the eventual result, this register is the cheapest GC
                // root when a zero-parameter executor ignores both
                // resolving functions and allocates/re-enters anyway.
                self.set(base, dst, Value::heap(p));
                let pair = self.new_resolver_pair();
                let res = Value::heap(self.heap.alloc(HeapObj::BoundResolver {
                    promise: p,
                    is_reject: false,
                    pair,
                }));
                let rej = Value::heap(self.heap.alloc(HeapObj::BoundResolver {
                    promise: p,
                    is_reject: true,
                    pair,
                }));
                // A throwing executor rejects the promise — via the pair's
                // [[Reject]], a no-op once resolve/reject already fired.
                if self
                    .call_value(exec, Value::UNDEFINED, &[res, rej])
                    .is_err()
                {
                    let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                    if self.resolver_pair_fire(pair) {
                        self.reject(p, reason);
                    }
                }
                ip += 1;
            }
            Instr::CallMethodSpread {
                dst,
                obj,
                name,
                args,
            } => {
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
            Instr::WithHas { dst, obj, name } => {
                let o = self.get(base, obj);
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                // HasBindingFor a with environment: [[HasProperty]] (own or
                // inherited), then the @@unscopables filter â€” an own/inherited
                // `@@unscopables` object whose `key` entry is truthy hides the
                // binding (so e.g. `with([]) { values }` reaches the outer
                // binding, not Array.prototype.values).
                // [[HasProperty]] must dispatch a Proxy `has` trap
                // (and propagate its abrupt completion).
                let kv = self.key_to_value(&key);
                let mut found = self.is_object_value(o) && self.has_property_dyn(o, kv)?;
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
            Instr::WithGet {
                dst,
                obj,
                name,
                strict,
            } => {
                let o = self.get(base, obj);
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                // GetBindingValue: HasProperty AGAIN (the WithHas
                // @@unscopables getter may have deleted the binding).
                let kv = self.key_to_value(&key);
                if !(self.is_object_value(o) && self.has_property_dyn(o, kv)?) {
                    if strict {
                        return Err(Thrown::join3("ReferenceError: ", &*key, " is not defined"));
                    }
                    self.set(base, dst, Value::UNDEFINED);
                } else {
                    let v = self.get_prop(o, &key)?;
                    self.set(base, dst, v);
                }
                ip += 1;
            }
            Instr::WithSet {
                obj,
                name,
                val,
                strict,
            } => {
                let o = self.get(base, obj);
                let key =
                    self.func(func_id as usize).string_constants[name as usize].as_str();
                // SetMutableBinding step 2 re-checks HasProperty
                // UNCONDITIONALLY (observable through a Proxy `has`
                // trap, even in sloppy mode); strict mode then throws
                // when the binding vanished since the reference resolved.
                let kv = self.key_to_value(&key);
                let still_exists =
                    self.is_object_value(o) && self.has_property_dyn(o, kv)?;
                if strict && !still_exists {
                    return Err(Thrown::join3("ReferenceError: ", &*key, " is not defined"));
                }
                let v = self.get(base, val);
                self.set_prop(o, &key, v, strict)?;
                ip += 1;
            }
            Instr::ObjectValues {
                dst,
                obj,
                callee,
                this_v,
            } => {
                let o = self.get(base, obj);
                let callee = self.get(base, callee);
                let this_v = self.get(base, this_v);
                if !self.static_builtin_is_intrinsic(
                    "Object",
                    native::OBJ_VALUES,
                    callee,
                    this_v,
                ) {
                    let v = self.call_value(callee, this_v, &[o])?;
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                self.require_object_coercible(o)?;
                let o = self.to_object(o)?;
                let v = self.object_enum_own(o, EnumWhat::Values)?;
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::ObjectEntries {
                dst,
                obj,
                callee,
                this_v,
            } => {
                let o = self.get(base, obj);
                let callee = self.get(base, callee);
                let this_v = self.get(base, this_v);
                if !self.static_builtin_is_intrinsic(
                    "Object",
                    native::OBJ_ENTRIES,
                    callee,
                    this_v,
                ) {
                    let v = self.call_value(callee, this_v, &[o])?;
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                self.require_object_coercible(o)?;
                let o = self.to_object(o)?;
                let v = self.object_enum_own(o, EnumWhat::Entries)?;
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::MakeCellFnName { reg } => {
                // A named function expression's own-name cell: tagged
                // immutable so nested-closure / eval writes no-op
                // (sloppy) or throw (strict).
                let v = self.get(base, reg);
                let cell = self.heap.alloc_cell(v);
                self.fn_name_cells.insert(cell);
                self.fn_name_cells_nonempty = 1;
                self.set(base, reg, Value::heap(cell));
                ip += 1;
            }
            Instr::LoadUpvalDyn { dst, idx, name } => {
                // A sloppy direct eval may have shadowed the captured
                // name with a function-scoped binding — it wins.
                if let Some(v) = self.eval_scope_lookup(name) {
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
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
            Instr::StoreUpvalDyn { idx, src, name } => {
                let v = self.get(base, src);
                if self.eval_scope_store(name, v) {
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                let cell = self.closure_upvalue(cur_closure, idx);
                // Captured `const` (see UpvalSet) — throws in both modes.
                if !self.const_cells.is_empty() && self.const_cells.contains(&cell) {
                    return Err(Thrown(
                        "TypeError: Assignment to constant variable.".into(),
                    ));
                }
                // Immutable own-name binding (see UpvalSet).
                if !self.fn_name_cells.is_empty() && self.fn_name_cells.contains(&cell) {
                    let f = self.frames.last().unwrap().func;
                    if self.func(f as usize).is_strict {
                        return Err(Thrown(
                            "TypeError: Assignment to constant variable.".into(),
                        ));
                    }
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                // TDZ: a lexical cell still UNINITIALIZED rejects the
                // assignment (mirrors UpvalSet).
                if self.heap.cell_get(cell).is_uninitialized() {
                    return Err(Thrown(
                        "ReferenceError: cannot access a lexical binding before initialization"
                            .to_string(),
                    ));
                }
                self.heap.cell_set(cell, v);
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
                            self.to_str_value(v)?
                        }
                    }
                    None => Value::UNDEFINED,
                };
                let sym = self.make_symbol(d);
                self.set(base, dst, sym);
                ip += 1;
            }
            Instr::LoadBigInt { dst, value } => {
                let value = value.get();
                // A small literal is an immediate (value.rs).
                let v = self.make_bigint(value);
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::LoadBigIntBig { dst, idx } => {
                // A literal beyond i64 (`load_bigint_instr`): the
                // compile-time-parsed constant, demoted to the small form
                // when it is within i128, as the canonical form requires.
                let b = self.func(func_id as usize).bigint_consts[idx as usize].clone();
                let v = self.make_bigint_num(b);
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::BigIntFrom { dst, arg } => {
                let a = self.get(base, arg);
                let n = self.bigint_from(a)?;
                let v = self.make_bigint_val(n);
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::ClassStaticField { class, key, val } => {
                let cv = self.get(base, class);
                let kv = self.get(base, key);
                let vv = self.get(base, val);
                // ToPropertyKey ONCE (the key expression was already evaluated).
                let k = self.coerce_index_key(kv)?;
                let key = self.key_of(k);
                if key == "prototype" {
                    return Err(Thrown(
                        "TypeError: Classes may not have a static property named 'prototype'"
                            .into(),
                    ));
                }
                // DefineField (CreateDataPropertyOrThrow), not [[Set]]:
                // `static ['name'] = v` replaces the constructor's own
                // read-only `name`, and no inherited setter runs.
                self.define_field(cv, &key, vv)?;
                ip += 1;
            }
            Instr::DefineField { obj, name, val } => {
                let o = self.get(base, obj);
                let v = self.get(base, val);
                let key = running.string_constants[name as usize].as_str();
                #[cfg(not(feature = "safe-sandbox"))]
                let done = self.ic_define_field(func_id, ip, o, key, v);
                #[cfg(feature = "safe-sandbox")]
                let done = false;
                if !done {
                    self.define_field(o, &key, v)?;
                }
                ip += 1;
            }
            Instr::InitDataPropDyn { obj, key, val } => {
                // CreateDataProperty with a computed key: ordinary own
                // data property — never the __proto__ setter.
                let o = self.get(base, obj);
                let k = self.get(base, key);
                let v = self.get(base, val);
                if o.is_heap() {
                    let oi = o.heap_index();
                    if matches!(self.heap.get(oi), HeapObj::Object(_)) {
                        self.init_data_prop_dyn_object(oi, k, v);
                    } else {
                        let ks = self.key_of(k);
                        self.set_prop(o, &ks, v, false)?;
                    }
                }
                ip += 1;
            }
            Instr::OpenUsingScope { dst } => {
                // Allocate a fresh `using` resource scope (see
                // `using_scope_new`): it lives exactly as long as the
                // frame does, so a generator abandoned mid-block takes
                // its resources with it instead of rooting them forever.
                let list = self.using_scope_new();
                self.set(base, dst, list);
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
                            "TypeError: a 'using' declaration value is not an object"
                                .into(),
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
                    let list = self.get(base, scope);
                    self.using_scope_push(list, disposer, false);
                    ip += 1;
                }
            }
            Instr::MergeDispose {
                kind_reg,
                val_reg,
                err,
            } => {
                // DisposeResources error chaining for the async loop's catch
                // arm: chain into a SuppressedError if a throw is already
                // pending, else make the completion a throw of `err`.
                let e = self.get(base, err);
                let raw = self.regs[base + kind_reg as usize].as_int();
                if raw & 3 == 2 {
                    let prior = self.regs[base + val_reg as usize];
                    let merged =
                        self.build_suppressed_error(&[e, prior, Value::UNDEFINED])?;
                    self.regs[base + val_reg as usize] = merged;
                } else {
                    self.regs[base + kind_reg as usize] = Value::int(2);
                    self.regs[base + val_reg as usize] = e;
                }
                ip += 1;
            }
            Instr::DateParse { dst, src } => {
                let s = self.get(base, src);
                let str = self.to_js_string(s)?;
                self.set(base, dst, Value::num(parse_date(&str)));
                ip += 1;
            }
            Instr::YieldDelegate { val, .. } => {
                // A `yield*` suspension: yield `val` exactly like `Yield`. The
                // resume MODE + value are delivered into mode_dst/val_dst by
                // gen_resume (it detects the resume op is a YieldDelegate), so
                // here we only suspend.
                let v = self.get(base, val);
                // A step `gen_enter_inline` runs in this loop: park, and
                // continue the consumer.
                if self.frames[frame_idx].gen != u32::MAX {
                    let raw = std::mem::replace(&mut self.pending_yield_raw, false);
                    self.gen_yield_delegate_inline(v, ip, raw)?;
                    return Ok(ColdFlow::Frame);
                }
                let f = self.frames.pop().unwrap();
                self.pending_yield_eval_scope = f.eval_scope;
                self.pending_yield_handlers = f.handlers;
                self.pending_yield = Some((v, ip));
                return Ok(ColdFlow::Done(v));
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
                return Ok(ColdFlow::Done(v));
            }
            Instr::AsyncIterReturnStep {
                dst,
                has_dst,
                iter,
                ret,
            } => {
                let it = self.get(base, iter);
                let r = self.get(base, ret);
                let m = self.get_member(it, "return", it)?;
                if m.is_nullish() {
                    // No `return` method â†’ the outer generator just returns `ret`.
                    self.set(base, has_dst, Value::bool(false));
                } else if !self.is_callable(m) {
                    // GetMethod: present but not callable.
                    return Err(Thrown(
                        "TypeError: the iterator's 'return' is not a function".into(),
                    ));
                } else {
                    // `ret == dst`: AsyncIteratorClose's bare call.
                    let args: &[Value] = if ret == dst { &[] } else { &[r] };
                    let res = self.call_value(m, it, args)?;
                    self.set(base, dst, res);
                    self.set(base, has_dst, Value::bool(true));
                }
                ip += 1;
            }
            Instr::AsyncIterThrowStep { dst, iter, exc } => {
                let it = self.get(base, iter);
                let e = self.get(base, exc);
                // GetMethod(iterator, "throw"): a poisoned getter propagates
                // (rejecting the outer request with the thrown value, via the
                // generator's unwind).
                let m = self.get_member(it, "throw", it)?;
                if m.is_nullish() {
                    // %AsyncFromSyncIteratorPrototype%.throw steps 7.a-h: no
                    // `throw` method — IteratorClose(iterator, NORMAL) FIRST
                    // (the iterator gets a chance to clean up; a poisoned /
                    // throwing `return` or a non-object close result
                    // propagates INSTEAD of the TypeError), then a TypeError
                    // for the protocol violation.
                    self.iterator_close(it)?;
                    return Err(Thrown(
                        "TypeError: the iterator does not provide a 'throw' method".into(),
                    ));
                }
                if !self.is_callable(m) {
                    // GetMethod: present-but-not-callable is a TypeError
                    // (no close — the abrupt GetMethod rejects directly).
                    return Err(Thrown(
                        "TypeError: the iterator does not provide a 'throw' method".into(),
                    ));
                }
                let res = self.call_value(m, it, &[e])?;
                self.set(base, dst, res);
                ip += 1;
            }
            Instr::NewSpread { dst, callee, args } => {
                let cv = self.get(base, callee);
                let args_v = self.get(base, args);
                let arg_vec = self.array_snapshot(args_v.heap_index());
                self.sync_top_ip(ip);
                let result = self.construct(cv, &arg_vec)?;
                self.set(base, dst, result);
                ip += 1;
            }
            Instr::ArrayCtor {
                dst,
                callee,
                arg_base,
                argc,
                is_construct,
            } => {
                if let Some(callee_reg) = callee {
                    let live = self.get(base, callee_reg);
                    if !self.bare_builtin_is_intrinsic(self.array_ctor, live) {
                        if is_construct {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let out = self.construct(live, &argv)?;
                            self.set(base, dst, out);
                            ip += 1;
                            return Ok(ColdFlow::Next(ip));
                        }
                        // Match generic Call's flat-frame path for a
                        // replaced plain function; other callable kinds
                        // (native/bound/proxy/realm wrappers) use the
                        // ordinary value call below.
                        if let Ok((fid, closure)) = self.resolve_callable(live) {
                            if !self.func(fid as usize).is_generator
                                && !self.func(fid as usize).is_async
                            {
                                self.setup_call(
                                    fid,
                                    closure,
                                    Value::UNDEFINED,
                                    base,
                                    arg_base,
                                    argc,
                                    dst,
                                    ip + 1,
                                    live,
                                )?;
                                return Ok(ColdFlow::Frame);
                            }
                        }
                        let argv: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let out = self.call_value(live, Value::UNDEFINED, &argv)?;
                        self.set(base, dst, out);
                        ip += 1;
                        return Ok(ColdFlow::Next(ip));
                    }
                }
                let mut virtual_len: Option<u32> = None;
                let arr = if argc == 1 && self.get(base, arg_base).is_number() {
                    // `Array(n)` â†’ n HOLES (absent elements), not n undefineds.
                    let n = self.get(base, arg_base).as_f64();
                    if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                        return Err(Thrown::from_static("RangeError: Invalid array length"));
                    }
                    if n as usize > super::MAX_DENSE_ARRAY_LEN {
                        // Past the eager-materialization cap: a SPARSE
                        // array — no elements, just a virtual length.
                        virtual_len = Some(n as u32);
                        Vec::new()
                    } else {
                        vec![Value::HOLE; n as usize]
                    }
                } else {
                    (0..argc).map(|i| self.get(base, arg_base + i)).collect()
                };
                let v = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                if let Some(n) = virtual_len {
                    self.array_js_len.insert(v.heap_index(), n);
                }
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::CallSpread { dst, callee, args } => {
                let callee_v = self.get(base, callee);
                let args_v = self.get(base, args);
                if self.spread_call_frame(callee_v, Value::UNDEFINED, args_v, dst, ip + 1)? {
                    return Ok(ColdFlow::Frame);
                }
                let arg_vec = self.array_snapshot(args_v.heap_index());
                let result = self.call_value(callee_v, Value::UNDEFINED, &arg_vec)?;
                self.set(base, dst, result);
                ip += 1;
            }
            Instr::CallWithThisSpread {
                dst,
                callee,
                this_v,
                args,
            } => {
                let callee_v = self.get(base, callee);
                let this_v = self.get(base, this_v);
                let args_v = self.get(base, args);
                if self.spread_call_frame(callee_v, this_v, args_v, dst, ip + 1)? {
                    return Ok(ColdFlow::Frame);
                }
                let arg_vec = self.array_snapshot(args_v.heap_index());
                let result = self.call_value(callee_v, this_v, &arg_vec)?;
                self.set(base, dst, result);
                ip += 1;
            }
            Instr::ObjectKeys {
                dst,
                obj,
                callee,
                this_v,
            } => {
                let callee = self.get(base, callee);
                let this_v = self.get(base, this_v);
                let o = self.get(base, obj);
                if !self.static_builtin_is_intrinsic(
                    "Object",
                    native::OBJ_KEYS,
                    callee,
                    this_v,
                ) {
                    let v = self.call_value(callee, this_v, &[o])?;
                    self.set(base, dst, v);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                if let Some(exit) = self.try_object_keys_len_reduce(func_id, ip) {
                    ip = exit;
                    return Ok(ColdFlow::Next(ip));
                }
                self.require_object_coercible(o)?; // ToObject(O)
                let o = self.to_object(o)?;
                let v = self.object_enum_own(o, EnumWhat::Keys)?;
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::ForInKeys { dst, obj } => {
                if let Some(exit) = self.try_sparse_forin_fold_reduce(func_id, ip) {
                    ip = exit;
                    return Ok(ColdFlow::Next(ip));
                }
                if let Some(exit) = self.try_forin_count_reduce(func_id, ip) {
                    ip = exit;
                    return Ok(ColdFlow::Next(ip));
                }
                if let Some(exit) = self.try_forin_sum_reduce(func_id, ip) {
                    ip = exit;
                    return Ok(ColdFlow::Next(ip));
                }
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
            Instr::RegExpMethod {
                dst,
                op,
                callee,
                this_v,
                arg_base,
                argc,
            } => {
                use crate::bytecode::RegExpMethod as R;

                let callee_v = self.get(base, callee);
                let this_val = self.get(base, this_v);
                let arg0 = if argc == 0 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base)
                };
                let direct = match op {
                    R::Test | R::Exec if regexp_call_direct_enabled() => self
                        .regexp_call_direct(
                            callee_v,
                            this_val,
                            arg0,
                            op == R::Test,
                            false,
                        )?,
                    R::MatchAll | R::Replace | R::Match if string_regexp_call_direct_enabled() => {
                        let replacement = if argc >= 2 {
                            self.get(base, arg_base + 1)
                        } else {
                            Value::UNDEFINED
                        };
                        self.string_regexp_call_direct(
                            callee_v,
                            this_val,
                            arg0,
                            replacement,
                            op,
                            false,
                        )?
                    }
                    _ => None,
                };
                if let Some(result) = direct {
                    self.set(base, dst, result);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }

                // Exact ordinary-call fallback. The property Get and
                // every argument effect already happened; never resolve
                // the method name again here.
                let plain = if callee_v.is_heap() {
                    match self.heap.get(callee_v.heap_index()) {
                        HeapObj::Func(id) => Some((*id, NO_CLOSURE)),
                        HeapObj::Closure { func, .. } => {
                            Some((*func, callee_v.heap_index()))
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some((fid, closure)) = plain {
                    let p = self.func(fid as usize);
                    if !p.is_generator && !p.is_async {
                        self.setup_call(
                            fid,
                            closure,
                            this_val,
                            base,
                            arg_base,
                            argc,
                            dst,
                            ip + 1,
                            callee_v,
                        )?;
                        return Ok(ColdFlow::Frame);
                    }
                }
                let argv: Vec<Value> =
                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                let r = self.call_value(callee_v, this_val, &argv)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::CallMethodComputed {
                dst,
                obj,
                key,
                arg_base,
                argc,
            } => {
                let recv = self.get(base, obj);
                let k = self.get(base, key);
                // A NUMERIC key cannot name a builtin method, and finding
                // that out the long way is not cheap: `display` formats the
                // number into a fresh String, `try_builtin_method` copies the
                // arguments into a buffer, and the name is then matched
                // against every builtin there is so it can fail.
                //
                // `table[i]()` pays all of it on every call — an opcode table,
                // a jump table, any array of functions. Measured at 118.8ns
                // against 44.4ns for the identical call written out as
                // `let h = table[i]; h()`. Pocket's CPU core carries a comment
                // telling the next reader to hand-write the slow form in the
                // hot path, which is the engine asking a bundle to work around
                // it.
                let numeric_key = k.is_int() || k.is_double();
                // `obj["push"](x)` etc: a builtin array/string method first.
                let kstr = if numeric_key {
                    String::new()
                } else {
                    self.display(k)
                };
                // Only a PRIMITIVE key's display is its property key. An
                // object key (a String wrapper, anything with its own
                // toString / @@toPrimitive) is named by ToPropertyKey,
                // which runs user code — `get_index` below performs it.
                if !numeric_key && !self.is_object_value(k) {
                    if let Some(result) =
                        self.try_builtin_method(recv, &kstr, base, arg_base, argc)?
                    {
                        self.set(base, dst, result);
                        ip += 1;
                        return Ok(ColdFlow::Next(ip));
                    }
                }
                // Else resolve the method off the receiver (own/inherited)
                // and call it with `this = recv`.
                let method = self.get_index(recv, k)?;
                // A native / bound / resolver / Proxy method value runs via
                // call_value (resolve_callable knows only Func/Closure).
                if method.is_heap()
                    && (matches!(
                        self.heap.get(method.heap_index()),
                        HeapObj::Native(_)
                            | HeapObj::NativeClosure { .. }
                            | HeapObj::Bound { .. }
                            | HeapObj::BoundResolver { .. }
                            | HeapObj::Proxy { .. }
                    ) || (self.fn_proto != 0 && method.heap_index() == self.fn_proto))
                {
                    let argv: Vec<Value> =
                        (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                    let r = self.call_value(method, recv, &argv)?;
                    self.set(base, dst, r);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                // Only the failure path needs the key rendered, so a hot
                // numeric dispatch never formats one.
                let (fid, closure) = if numeric_key {
                    match self.resolve_callable(method) {
                        Ok(x) => x,
                        Err(Thrown(msg)) => {
                            let name = self.display(k);
                            return Err(Thrown(format!("{msg} (property \"{name}\")")));
                        }
                    }
                } else {
                    self.resolve_callable_named(method, &kstr)?
                };
                if self.func(fid as usize).is_generator && self.func(fid as usize).is_async
                {
                    let argv: Vec<Value> =
                        (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                    self.pending_gen_callee = method;
                    let ag = self.alloc_async_generator(fid, closure, recv, &argv)?;
                    self.set(base, dst, ag);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                if self.func(fid as usize).is_generator {
                    let argv: Vec<Value> =
                        (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                    self.pending_gen_callee = method;
                    let g = self.alloc_generator(fid, closure, recv, &argv)?;
                    self.set(base, dst, g);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                if self.func(fid as usize).is_async {
                    let argv: Vec<Value> =
                        (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                    self.pending_gen_callee = method;
                    let p = self.alloc_async(fid, closure, recv, &argv);
                    self.set(base, dst, p);
                    ip += 1;
                    return Ok(ColdFlow::Next(ip));
                }
                self.setup_call(
                    fid,
                    closure,
                    recv,
                    base,
                    arg_base,
                    argc,
                    dst,
                    ip + 1,
                    method,
                )?;
                return Ok(ColdFlow::Frame);
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
                    self.arr_props
                        .entry(cooked)
                        .or_insert_with(ObjMap::new_side_table)
                        .define("raw", r, attr);
                    for idx in [raw_idx, cooked] {
                        self.arr_props
                            .entry(idx)
                            .or_insert_with(ObjMap::new_side_table)
                            .freeze();
                        self.array_length_nonwritable.insert(idx);
                    }
                }
                ip += 1;
            }
            Instr::FieldInit {
                key_index,
                val,
                class_id,
            } => {
                self.exec_field_init(base, key_index, val, class_id)?;
                ip += 1;
            }
            Instr::ThisCheck { src } => {
                let v = self.get(base, src);
                if v.is_heap() && self.this_tdz.contains(&v.heap_index()) {
                    return Err(Thrown::from_static(
                        "ReferenceError: must call super constructor before accessing 'this' in a derived class constructor",
                    ));
                }
                if v.is_heap() {
                    if let Some(&produced) = self.super_this.get(&v.heap_index()) {
                        self.set(base, src, produced);
                    }
                }
                ip += 1;
            }
            Instr::SuperCtor {
                ctor,
                home_class_id,
                arg_base,
                argc,
            } => {
                self.super_ctor_call(base, ip, ctor, home_class_id, arg_base, argc)?;
                ip += 1;
            }
            Instr::SuperBase { dst, home_class_id } => {
                let v = self.super_base(home_class_id, running.super_static);
                self.set(base, dst, v);
                ip += 1;
            }
            Instr::SuperGet {
                dst,
                home_class_id,
                name,
            } => {
                return self.exec_super_get(func_id, ip, base, dst, home_class_id, name);
            }
            Instr::SuperSet {
                base: base_reg,
                home_class_id,
                name,
                val,
            } => {
                return self.exec_super_set(func_id, ip, base, base_reg, home_class_id, name, val);
            }
            Instr::StaticFn {
                dst,
                op,
                callee,
                this_v,
                arg_base,
                argc,
            } => {
                self.exec_static_fn(base, dst, op, callee, this_v, arg_base, argc)?;
                ip += 1;
            }
            Instr::ArrayFrom {
                dst,
                src,
                mapfn,
                callee,
                this_v,
                argc,
            } => {
                let sv = self.get(base, src);
                let fnv = self.get(base, mapfn);
                let callee = self.get(base, callee);
                let this_v = self.get(base, this_v);
                let out = if self.static_builtin_is_intrinsic(
                    "Array",
                    native::ARR_FROM,
                    callee,
                    this_v,
                ) {
                    self.array_from(this_v, sv, fnv, Value::UNDEFINED)?
                } else if argc == 1 {
                    self.call_value(callee, this_v, &[sv])?
                } else {
                    self.call_value(callee, this_v, &[sv, fnv])?
                };
                self.set(base, dst, out);
                ip += 1;
            }
            Instr::DeleteProp {
                dst,
                obj,
                name,
                strict,
            } => {
                let o = self.get(base, obj);
                self.require_object_coercible(o)?;
                let key = running.string_constants[name as usize].as_str();
                let r = self.delete_property(o, key)?;
                if strict && r == Value::bool(false) {
                    return Err(Thrown::join3("TypeError: Cannot delete property '", key, "'"));
                }
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::DeleteIndex {
                dst,
                obj,
                key,
                strict,
            } => {
                let o = self.get(base, obj);
                let k = self.get(base, key);
                let ks = self.to_property_key(k)?;
                self.require_object_coercible(o)?;
                let r = self.delete_property(o, &ks)?;
                if strict && r == Value::bool(false) {
                    return Err(Thrown::join3("TypeError: Cannot delete property '", &ks, "'"));
                }
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::DeleteIndexConcat {
                dst,
                obj,
                name,
                key,
                strict,
            } => {
                let o = self.get(base, obj);
                let k = self.get(base, key);
                let r = self.delete_index_concat(o, name, k, strict, func_id)?;
                self.set(base, dst, r);
                ip += 1;
            }
            Instr::TailCallWithThis {
                callee,
                this_v,
                arg_base,
                argc,
            } => {
                let callee_v = self.get(base, callee);
                let this_val = self.get(base, this_v);
                let argv = self.copy_args_vec(base, arg_base, argc);
                if self.try_tail_reuse(base, callee_v, this_val, &argv)? {
                    return Ok(ColdFlow::Frame);
                }
                ip += 1;
            }
            Instr::EndFinally { kind_reg, val_reg } => {
                return self.exec_end_finally(ip, base, kind_reg, val_reg, stop_depth);
            }
            Instr::JumpFinally { target, floor } => {
                let from = ip;
                match self.route_jump_through_finally(target, floor as usize) {
                    Some(t) => ip = t as usize,
                    None => ip = target as usize,
                }
                if ip < from {
                    self.maybe_gc();
                }
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
            Instr::Yield { val, .. } => {
                let v = self.get(base, val);
                if self.frames[frame_idx].gen != u32::MAX {
                    self.gen_yield_inline(v, ip);
                    return Ok(ColdFlow::Frame);
                }
                let f = self.frames.pop().unwrap();
                self.pending_yield_eval_scope = f.eval_scope;
                self.pending_yield_handlers = f.handlers;
                self.pending_yield = Some((v, ip));
                return Ok(ColdFlow::Done(v));
            }
            Instr::RequireObject { val } => {
                let v = self.get(base, val);
                if !self.is_object_value(v) {
                    return Err(Thrown::from_static(
                        "TypeError: iterator result is not an object",
                    ));
                }
                ip += 1;
            }
            Instr::GenStart => {
                let f = self.frames.pop().unwrap();
                self.pending_yield_eval_scope = f.eval_scope;
                self.pending_yield_handlers = f.handlers;
                self.pending_yield = Some((Value::UNDEFINED, ip));
                return Ok(ColdFlow::Done(Value::UNDEFINED));
            }
            Instr::IterClose { iter } => {
                let it = self.get(base, iter);
                self.iterator_close(it)?;
                ip += 1;
            }
            Instr::IterCloseQuiet { iter } => {
                let it = self.get(base, iter);
                let _ = self.iterator_close(it);
                ip += 1;
            }
            Instr::IterCloseFinally { iter, kind_reg } => {
                let it = self.get(base, iter);
                match self.regs[base + kind_reg as usize].as_int() & 3 {
                    1 => self.iterator_close(it)?,
                    2 => {
                        let _ = self.iterator_close(it);
                    }
                    _ => {}
                }
                ip += 1;
            }
            Instr::IterPrime { dst, iter } => {
                let it = self.get(base, iter);
                let nv = if it.is_heap()
                    && (matches!(
                        self.heap.get(it.heap_index()),
                        HeapObj::Object(_)
                            | HeapObj::Proxy { .. }
                            | HeapObj::Iterator { .. }
                            | HeapObj::IterHelper(_)
                            | HeapObj::Intl { .. }
                    ) || (matches!(self.heap.get(it.heap_index()), HeapObj::Generator(_))
                        && !self.gen_method_pristine(it.heap_index(), "next")))
                {
                    self.get_prop(it, "next")?
                } else {
                    Value::UNDEFINED
                };
                self.set(base, dst, nv);
                ip += 1;
            }
            _ => unreachable!("dispatch_cold reached for an instruction the loop runs inline"),
        }
        Ok(ColdFlow::Next(ip))
    }

    /// The JIT's compile trigger at a frame entry, out of `dispatch_body`'s
    /// prologue: it runs when a function's entry count crosses the threshold,
    /// about once per function, while the prologue runs on every frame
    /// transition. Inline it was most of the prologue's 14 KB.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[cold]
    #[inline(never)]
    pub(crate) fn jit_compile_on_entry(&mut self, func_id: u32, base: usize) {
        // Long `eval` / `new Function` code compiles after more calls.
        if self.jit.runtime_fn_wait(func_id) {
            return;
        }
        // W9: a live register-homed region already serves this
        // function's hot loop; a Tier C body would shadow it with
        // mem-homed loop code forever (its back-edge is a bare
        // `jmp` — no OSR check). Decline BEFORE any plan building
        // so the recurring trip costs one dense read, and defer so
        // the offer re-trips if the region is later evicted.
        // B206 yield-with-entry: a fn whose loops own reg-homed
        // regions still compiles — with an unconditional bail at
        // each region head, so calls get the native lane and
        // prologue while the regions keep the loops.
        let yield_heads: Vec<u32> = if self.jit.should_yield_to_region(func_id) {
            if crate::codegen::yield_entry_enabled() {
                self.jit.reg_region_heads(func_id)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        if self.jit.should_yield_to_region(func_id) && yield_heads.is_empty() {
            self.jit.compile_defer(func_id);
        } else {
            let proto: *const crate::bytecode::FuncProto = self.func(func_id as usize);
            // SAFETY: program functions are immutable during execution.
            let proto_ref = unsafe { &*proto };
            // A global op whose slot is still UNINITIALIZED may be
            // own-prop-backed — `this.x = v` / `globalThis.x = v` /
            // eval-created bindings live as own properties of the global
            // OBJECT, and the interpreter's LoadGlobal falls back to them.
            // The emitted `mov rax, [r12 + idx*8]` cannot: it reads the
            // uninitialized sentinel, so `x++` silently produced NaN.
            //
            // The region compiler (`region_globals_ok` below) and the
            // leaf-inline planner (`build_leaf_inline_plan`) already refuse
            // for exactly this reason. Tier C did not, which is why the
            // whole-function path had the bug and the loop path did not.
            //
            // "Once a slot holds a real value it can never go back" was the
            // stated justification for checking here and never again. It is
            // FALSE — `delete implicitG` puts the sentinel back — so
            // `try_run_jit` now revalidates at entry against the global-route
            // epoch. This stays as the cheap first filter.
            //
            // A STORE carries a second condition: a real own property on the
            // global object (non-writable, accessor, frozen) makes the write
            // an OrdinarySet the raw slot store would bypass. Both live in
            // `global_slot_directly_routable`, shared with the entry check so
            // the two cannot drift.
            let metered = self.jit.metered();
            let mut slow_global_loads = rustc_hash::FxHashSet::default();
            let mut globals_ok = true;
            for ins in &proto_ref.code {
                match *ins {
                    Instr::LoadGlobal { idx, .. }
                    | Instr::LoadGlobalOrUndefined { idx, .. } => {
                        if !self.global_slot_directly_routable(idx) {
                            if metered {
                                globals_ok = false;
                                break;
                            }
                            slow_global_loads.insert(idx);
                        }
                    }
                    Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. } => {
                        if !self.global_store_slot_directly_routable(idx) {
                            globals_ok = false;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if !globals_ok {
                self.jit.compile_defer_globals(func_id);
            } else {
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
                let mut heap_helper_addrs = self.jit_heap_helper_addrs();
                heap_helper_addrs.acc_lanes = self.build_acc_lane_plan(func_id);
                heap_helper_addrs.priv_lanes = self.build_priv_lane_plan(func_id);
                let const_strs = self.jit_build_const_strs(func_id);
                heap_helper_addrs.atoms_raw = self.heap.atoms_raw;
                heap_helper_addrs.atoms_words = self.heap.atoms_words;
                // Tier-C leaf-inline plan for the whole function (from the live
                // ICs). Built BEFORE the &mut self.jit borrow. Opt out with
                // ZIPP_NO_TIERC_LEAF (region leaf inlining is unaffected).
                let tierc_leaf_on = {
                    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    *ON.get_or_init(|| crate::env_var_os("ZIPP_NO_TIERC_LEAF").is_none())
                };
                let mut leaf_plan = if tierc_leaf_on {
                    self.build_leaf_inline_plan(
                        func_id,
                        0,
                        (proto_ref.code.len() - 1) as u32,
                    )
                } else {
                    rustc_hash::FxHashMap::default()
                };
                let method_plan =
                    if crate::codegen::tierc_method_global_inline_enabled() {
                        self.build_tierc_method_global_plan(
                            func_id,
                            0,
                            (proto_ref.code.len() - 1) as u32,
                            base,
                        )
                    } else {
                        rustc_hash::FxHashMap::default()
                    };
                // Tier-C cross-call plan (B83) — also built before &mut self.jit.
                let (mut cross_plan, cross_pending, cross_baked) =
                    self.build_cross_call_plan(func_id, Some(base), true);
                self.plan_new_lanes(func_id, &mut cross_plan, &mut leaf_plan);
                self.jit.note_cross_pending(func_id, &cross_pending);
                self.jit.note_cross_baked(func_id, &cross_baked);
                let random_fuse = self.build_random_fuse_plan(func_id, Some(base));
                let callable_make = self.build_callable_make_plan(func_id);
                let prop_shape = self.build_prop_shape_plan(func_id);
                let class_read = self.build_class_read_plan(func_id);
                let enum_switches = self.build_enum_switch_plan(func_id);
                // Tier O's inlinable callees, with their own property plans.
                let mut opt_callees = crate::codegen::OptCallees::default();
                if self.jit.opt_compiles_now(func_id) {
                    opt_callees.const_props = self.build_const_prop_plan(func_id);
                    opt_callees.keyed_get = self.build_keyed_get_plan(func_id);
                    let mut super_ctor_sites: Vec<(usize, u32)> = Vec::new();
                    for (ip, instr) in self.func(func_id as usize).code.iter().enumerate() {
                        match *instr {
                            Instr::GetProp { .. } => {
                                if let Some(lane) = self.ic_class_getter_read(func_id, ip) {
                                    opt_callees.getters.insert(ip, lane);
                                } else {
                                    // Ahead of a one-shape plan too: a cache that
                                    // has met two shapes would deopt that plan.
                                    let ways = self.ic_own_data_ways(func_id, ip);
                                    if ways.len() >= 2 {
                                        opt_callees.poly.insert(ip, ways);
                                    }
                                }
                            }
                            Instr::LoadClassValue { class_id, .. } => {
                                if let Some(v) = self.class_value_fixed(class_id) {
                                    opt_callees.class_values.insert(class_id, v.bits());
                                }
                            }
                            Instr::SuperCtorFetch { home_class_id, .. } => {
                                if let Some(v) = self.class_value_fixed(home_class_id) {
                                    let hi = v.heap_index();
                                    if let HeapObj::Class(c) = self.heap.get(hi) {
                                        if c.dec.is_none() {
                                            let pbits = c
                                                .parent
                                                .map_or(Value::UNDEFINED.bits(), |p| Value::heap(p).bits());
                                            opt_callees.super_parents.insert(home_class_id, (hi, pbits));
                                        }
                                    }
                                }
                            }
                            Instr::SuperCtor { home_class_id, argc, .. } if argc <= 255 => {
                                super_ctor_sites.push((ip, home_class_id));
                            }
                            Instr::New { callee, argc: 0, .. } if ip > 0 => {
                                let prev = self.func(func_id as usize).code[ip - 1];
                                if let Instr::LoadGlobal { dst, idx }
                                | Instr::LoadGlobalOrUndefined { dst, idx } = prev
                                {
                                    if dst == callee {
                                        let gv = self.globals.get(idx as usize).copied();
                                        if self.map_ctor != 0 && gv == Some(Value::heap(self.map_ctor)) {
                                            opt_callees
                                                .new_empty_colls
                                                .insert(ip, (Value::heap(self.map_ctor).bits(), false));
                                        } else if self.set_ctor != 0 && gv == Some(Value::heap(self.set_ctor)) {
                                            opt_callees
                                                .new_empty_colls
                                                .insert(ip, (Value::heap(self.set_ctor).bits(), true));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if !super_ctor_sites.is_empty() && self.ctor_derived_ff(func_id) == 1 {
                        for (ip, home_class_id) in super_ctor_sites {
                            let Some(home_v) = self.class_value_fixed(home_class_id) else {
                                continue;
                            };
                            let parent_ci = match self.heap.get(home_v.heap_index()) {
                                HeapObj::Class(dc)
                                    if dc.dec.is_none()
                                        && !dc.has_private_names
                                        && dc.field_thunk.is_none() =>
                                {
                                    match dc.parent {
                                        Some(p) => p,
                                        None => continue,
                                    }
                                }
                                _ => continue,
                            };
                            let Some((_, entry)) = self.resolve_ctor_chain_cached(parent_ci) else {
                                continue;
                            };
                            if (entry.kind != 1 && entry.kind != 2)
                                || entry.has_private_names
                                || entry.has_field_thunk
                                || !entry.ctor_fn.is_heap()
                            {
                                continue;
                            }
                            let parent_fid = entry.ctor_fid;
                            let parent_ok = {
                                let p = self.func(parent_fid as usize);
                                p.rest_reg.is_none() && p.arguments_reg.is_none()
                            };
                            if !parent_ok {
                                continue;
                            }
                            let is_derived = entry.kind == 2;
                            if is_derived {
                                if self.ctor_derived_ff(parent_fid) != 1 {
                                    continue;
                                }
                            } else if !self.ctor_body_framefree(parent_fid) {
                                continue;
                            }
                            let plan = Box::leak(Box::new(crate::codegen::SuperCtorPlan {
                                parent_bits: Value::heap(parent_ci).bits(),
                                target_val_bits: entry.target_val.bits(),
                                ctor_fn_bits: entry.ctor_fn.bits(),
                                parent_idx: parent_ci,
                                parent_ver: entry.start_ver,
                                chain_epoch: entry.chain_epoch,
                                parent_fid,
                                parent_closure: entry.closure,
                                parent_callee_idx: entry.ctor_fn.heap_index(),
                                is_derived: u8::from(is_derived),
                                _pad: [0; 7],
                            }));
                            opt_callees.super_ctors.insert(ip, plan);
                        }
                    }
                    opt_callees.class_epoch = self.mi_class_epoch;
                    let mut queue: Vec<(u32, u8)> =
                        crate::codegen::opt_plan_callee_ids(&cross_plan, &class_read, &opt_callees.getters)
                            .into_iter()
                            .map(|fid| (fid, 0u8))
                            .collect();
                    while let Some((fid, depth)) = queue.pop() {
                        if opt_callees.bodies.contains_key(&fid) {
                            continue;
                        }
                        if !(self.is_main_func(fid) || self.loader_module_func(fid)) {
                            continue;
                        }
                        let callee: &'p crate::bytecode::FuncProto = self.func(fid as usize);
                        if callee.code.len() <= 64
                            && !callee.is_generator
                            && !callee.is_async
                            && callee.rest_reg.is_none()
                            && callee.arguments_reg.is_none()
                            && callee.eval_sites.is_empty()
                            && callee.code.iter().all(|ci| match *ci {
                                Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                                    self.global_slot_directly_routable(idx)
                                }
                                _ => true,
                            })
                        {
                            let c_prop = self.build_prop_shape_plan(fid);
                            let c_strs = self.jit_build_const_strs(fid);
                            let c_const_props = self.build_const_prop_plan(fid);
                            let c_prop_key_ids = self.build_prop_key_ids(fid);
                            let c_keyed_get = self.build_keyed_get_plan(fid);
                            let mut c_poly = rustc_hash::FxHashMap::default();
                            let mut has_call = false;
                            for (cip, cinstr) in callee.code.iter().enumerate() {
                                match cinstr {
                                    Instr::GetProp { .. } => {
                                        let ways = self.ic_own_data_ways(fid, cip);
                                        if ways.len() >= 2 {
                                            c_poly.insert(cip, ways);
                                        }
                                    }
                                    Instr::Call { .. }
                                    | Instr::CallWithThis { .. }
                                    | Instr::CallMethod { .. } => has_call = true,
                                    _ => {}
                                }
                            }
                            let c_sites = if depth == 0 && has_call {
                                let (c_cross, _, _) = self.build_cross_call_plan(fid, None, false);
                                let c_class_read = self.build_class_read_plan(fid);
                                let (sites, sub_fids) = crate::codegen::opt_plan_nested_sites(
                                    callee,
                                    &c_cross,
                                    &c_class_read,
                                    fid,
                                    func_id,
                                );
                                for sub_fid in sub_fids {
                                    if !opt_callees.bodies.contains_key(&sub_fid) {
                                        queue.push((sub_fid, 1));
                                    }
                                }
                                sites
                            } else {
                                rustc_hash::FxHashMap::default()
                            };
                            opt_callees.bodies.insert(
                                fid,
                                crate::codegen::OptCallee {
                                    fid,
                                    proto: callee,
                                    prop_shape: c_prop,
                                    const_strs: c_strs,
                                    poly: c_poly,
                                    const_props: c_const_props,
                                    prop_key_ids: c_prop_key_ids,
                                    keyed_get: c_keyed_get,
                                    sites: c_sites,
                                },
                            );
                        }
                    }
                } else if crate::codegen::tierc_class_const_enabled() {
                    // Tier C bakes them too: a method naming its own class
                    // (`return new Vec(…)`) or fetching its superclass in a
                    // derived ctor reads a constant behind the epoch guard
                    // instead of calling the runtime helper.
                    for instr in self.func(func_id as usize).code.iter() {
                        match *instr {
                            Instr::LoadClassValue { class_id, .. } => {
                                if let Some(v) = self.class_value_fixed(class_id) {
                                    opt_callees.class_values.insert(class_id, v.bits());
                                }
                            }
                            Instr::SuperCtorFetch { home_class_id, .. } => {
                                if let Some(v) = self.class_value_fixed(home_class_id) {
                                    let hi = v.heap_index();
                                    if let HeapObj::Class(c) = self.heap.get(hi) {
                                        if c.dec.is_none() {
                                            let pbits = c
                                                .parent
                                                .map_or(Value::UNDEFINED.bits(), |p| Value::heap(p).bits());
                                            opt_callees.super_parents.insert(home_class_id, (hi, pbits));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    opt_callees.class_epoch = self.mi_class_epoch;
                }
                if !self.jit.metered() {
                    let direct_on = crate::codegen::direct_ic_miss_enabled();
                    for (ip, instr) in proto_ref.code.iter().enumerate() {
                        let is_set = match instr {
                            Instr::GetProp { .. } => false,
                            Instr::SetProp { .. } => true,
                            _ => continue,
                        };
                        if direct_on && self.ic_site_is_rotating_own(func_id, ip, is_set) {
                            self.jit.preseed_direct_miss_site(func_id, ip as u32);
                        } else if self.ic_site_has_accessor(func_id, ip) {
                            self.jit.preseed_acc_site(func_id, ip as u32);
                        }
                    }
                }
                let prop_key_ids = self.build_prop_key_ids(func_id);
                #[cfg(feature = "python")]
                if self.py_plans_skipped(func_id) {
                    self.py_attr_inline_plan(func_id);
                }
                self.jit.compile(
                    func_id,
                    proto_ref,
                    jit_self_call_at as *const () as usize,
                    self_val,
                    jit_globals_base as *const () as usize,
                    heap_helper_addrs,
                    &const_strs,
                    &leaf_plan,
                    &method_plan,
                    &cross_plan,
                    &yield_heads,
                    &random_fuse,
                    &callable_make,
                    &prop_shape,
                    &class_read,
                    &opt_callees,
                    &prop_key_ids,
                    &enum_switches,
                    &slow_global_loads,
                    self.global_route_epoch,
                );
            }
        }
    }

    /// The OSR loop-region compile trigger on a back-edge, out of `dispatch_body`:
    /// it runs once per loop header on crossing the threshold, while `dispatch_body`
    /// runs on every instruction. Inline, its ~25 heavy planning locals inflated
    /// `dispatch_body::<false, false>`'s stack frame and register spills across
    /// every opcode. Returns `Some(resume_ip)` if the newly compiled region ran.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[cold]
    #[inline(never)]
    pub(crate) fn jit_compile_osr_on_backedge(
        &mut self,
        func_id: u32,
        t: usize,
        ip: usize,
        base: usize,
    ) -> Option<usize> {
        let region_end = {
            let proto = self.func(func_id as usize);
            let last = proto.code.len() - 1;
            let mut e = ip.min(last);
            for p in (ip + 1)..=last {
                if matches!(proto.code[p], Instr::Jump { target } if target as usize == t) {
                    e = p;
                }
            }
            e
        };
        let region_globals_ok = {
            let proto = self.func(func_id as usize);
            let s = t;
            let e = region_end;
            proto.code[s..=e].iter().all(|ins| match *ins {
                Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                    self.global_slot_directly_routable(idx)
                }
                Instr::StoreGlobal { idx, .. }
                | Instr::StoreGlobalStrict { idx, .. }
                | Instr::StoreGlobalResolved { idx, .. } => {
                    self.global_store_slot_directly_routable(idx)
                }
                _ => true,
            })
        };
        if !region_globals_ok {
            self.jit.region_defer_globals(func_id, t as u32);
            return None;
        }
        let region_call_mix_ok = {
            let proto = self.func(func_id as usize);
            let s = t;
            let e = region_end;
            let mut fallback = 0usize;
            for (off, ins) in proto.code[s..=e].iter().enumerate() {
                match ins {
                    Instr::CallMethod { name, argc, .. } => {
                        let key = proto
                            .string_constants
                            .get(*name as usize)
                            .map(|x| x.as_str());
                        if *argc == 1
                            && matches!(
                                key,
                                Some("push")
                                    | Some("charCodeAt")
                                    | Some("indexOf")
                                    | Some("get")
                                    | Some("has")
                            )
                        {
                            continue;
                        }
                        if (*argc == 2
                            || (*argc == 1 && crate::codegen::substring1_intrinsic_enabled()))
                            && matches!(key, Some("substring") | Some("slice"))
                        {
                            continue;
                        }
                        if matches!(key, Some("set") | Some("delete") | Some("add")) {
                            continue;
                        }
                        if (*argc == 1 || *argc == 2)
                            && key.is_some_and(|k| crate::codegen::dv_get_kind(k).is_some())
                        {
                            continue;
                        }
                        if *argc == 0 && key == Some("next") {
                            continue;
                        }
                        if *argc == 0 && key == Some("pop") {
                            continue;
                        }
                        if crate::codegen::call_inline_enabled()
                            && ((*argc <= 4 && key == Some("call"))
                                || (*argc <= 2 && key == Some("apply")))
                        {
                            continue;
                        }
                    }
                    Instr::Call { .. } => {}
                    _ => continue,
                }
                let site = (func_id as usize)
                    .checked_sub(0)
                    .and_then(|f| self.site_ics.get(f))
                    .and_then(|x| x.as_ref())
                    .and_then(|slots| slots.get(s + off))
                    .and_then(super::ic::SiteCell::site_ref);
                if let Some(site) = site {
                    if site.n == 0 && site.misses >= 4 {
                        fallback += 1;
                    }
                }
            }
            fallback == 0 || (e - s + 1) >= fallback * 20
        };
        if !region_call_mix_ok {
            self.jit.blacklist_region(func_id, t as u32);
            return None;
        }
        let pending: Vec<u32> = {
            let proto = self.func(func_id as usize);
            proto.code[t..=region_end]
                .iter()
                .filter_map(|ins| match *ins {
                    Instr::LoadConst { idx, .. } => {
                        let c = proto.constants[idx as usize];
                        if c.is_heap() && (c.heap_index() & STRING_CONST_BIT) != 0 {
                            Some(idx)
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect()
        };
        let mut const_strs: rustc_hash::FxHashMap<u32, u64> = rustc_hash::FxHashMap::default();
        for idx in pending {
            if const_strs.contains_key(&idx) {
                continue;
            }
            const_strs.insert(idx, self.jit_const_str_bits(func_id, idx));
        }
        let (mut cross_plan, cross_pending, cross_baked) =
            self.build_cross_call_plan(func_id, Some(base), false);
        if !cross_pending.is_empty() && self.jit.cross_defer_allowed(func_id, t as u32) {
            self.jit.region_defer(func_id, t as u32);
            return None;
        }
        let ta_plan = self.build_ta_pin_plan(func_id, t as u32, region_end as u32, base);
        let mut leaf_plan = self.build_leaf_inline_plan(func_id, t as u32, region_end as u32);
        let computed_leaf_plan =
            self.build_dense_computed_leaf_plan(func_id, t as u32, region_end as u32, base);
        let method_plan =
            self.build_method_inline_plan(func_id, t as u32, region_end as u32, base, false);
        if self.heap.nursery_on() {
            for plan in method_plan.values() {
                for shape in &plan.shapes {
                    let stores = shape.body.iter().any(|op| {
                        matches!(
                            op,
                            crate::bytecode::Instr::SetProp { obj: 0, .. }
                                | crate::bytecode::Instr::SuperSet { .. }
                        )
                    });
                    if stores {
                        let v = crate::value::Value::from_bits(shape.recv_bits);
                        if v.is_heap() {
                            self.heap.register_scan_root(v.heap_index());
                        }
                    }
                }
            }
        }
        let proto: *const crate::bytecode::FuncProto = self.func(func_id as usize);
        // SAFETY: program functions are immutable during run.
        let proto_ref = unsafe { &*proto };
        let mut heap_helper_addrs = self.jit_heap_helper_addrs();
        heap_helper_addrs.acc_lanes = self.build_acc_lane_plan(func_id);
        self.plan_new_lanes(func_id, &mut cross_plan, &mut leaf_plan);
        self.jit.note_cross_baked(func_id, &cross_baked);
        self.jit.note_cross_pending(func_id, &cross_pending);
        #[cfg(feature = "python")]
        self.py_native_call_sites(func_id);
        let class_read = self.build_class_read_plan(func_id);
        let prop_key_ids = self.build_prop_key_ids(func_id);
        let enum_switches = self.build_enum_switch_plan(func_id);
        self.jit.compile_region(
            func_id,
            proto_ref,
            t as u32,
            region_end as u32,
            &self.globals,
            &self.heap,
            jit_globals_base as *const () as usize,
            heap_helper_addrs,
            self.program.global_count,
            FIELD_POOL as u32,
            &const_strs,
            &ta_plan,
            &leaf_plan,
            &computed_leaf_plan,
            &method_plan,
            &cross_plan,
            &class_read,
            &prop_key_ids,
            &enum_switches,
        );
        self.try_run_osr(func_id, t as u32, base)
    }

    /// Shared `Instr::NewMap` execution for interpreter dispatch and Tier C.
    pub(crate) fn exec_new_map(&mut self, src_val: Option<Value>) -> Result<Value, Thrown> {
        let m = Value::heap(self.heap.alloc_map_settled());
        if let Some(sv) = src_val {
            if !sv.is_nullish() {
                let adder = self.get_member(m, "set", m)?;
                if !self.is_callable(adder) {
                    return Err(Thrown(
                        "TypeError: Map.prototype.set is not callable".into(),
                    ));
                }
                let iter = self.get_iterator_object(sv)?;
                let mut native_work = 0u64;
                loop {
                    let e = match self.iterator_step(iter)? {
                        Some(v) => v,
                        None => break,
                    };
                    native_work = native_work.saturating_add(1);
                    if let Err(err) = self.preflight_native_iteration_work(native_work) {
                        self.iterator_close_quiet(iter);
                        return Err(err);
                    }
                    if !self.is_object_value(e) {
                        self.iterator_close_quiet(iter);
                        return Err(Thrown(
                            "TypeError: Map iterable entry is not an object".into(),
                        ));
                    }
                    let k = match self.get_index(e, Value::int(0)) {
                        Ok(k) => k,
                        Err(err) => {
                            self.iterator_close_quiet(iter);
                            return Err(err);
                        }
                    };
                    let v = match self.get_index(e, Value::int(1)) {
                        Ok(v) => v,
                        Err(err) => {
                            self.iterator_close_quiet(iter);
                            return Err(err);
                        }
                    };
                    if let Err(err) = self.call_value(adder, m, &[k, v]) {
                        self.iterator_close_quiet(iter);
                        return Err(err);
                    }
                }
            }
        }
        Ok(m)
    }

    /// Shared `Instr::NewSet` execution for interpreter dispatch and Tier C.
    pub(crate) fn exec_new_set(&mut self, src_val: Option<Value>) -> Result<Value, Thrown> {
        let set_v = Value::heap(self.heap.alloc_set_settled());
        if let Some(sv) = src_val {
            if !sv.is_nullish() {
                let adder = self.get_member(set_v, "add", set_v)?;
                if !self.is_callable(adder) {
                    return Err(Thrown(
                        "TypeError: Set.prototype.add is not callable".into(),
                    ));
                }
                let iter = self.get_iterator_object(sv)?;
                let mut native_work = 0u64;
                loop {
                    let e = match self.iterator_step(iter)? {
                        Some(v) => v,
                        None => break,
                    };
                    native_work = native_work.saturating_add(1);
                    if let Err(err) = self.preflight_native_iteration_work(native_work) {
                        self.iterator_close_quiet(iter);
                        return Err(err);
                    }
                    if let Err(err) = self.call_value(adder, set_v, &[e]) {
                        self.iterator_close_quiet(iter);
                        return Err(err);
                    }
                }
            }
        }
        Ok(set_v)
    }

    /// Shared `Instr::JsonStringify` execution for interpreter dispatch and Tier C.
    pub(crate) fn exec_json_stringify(
        &mut self,
        v: Value,
        space_v: Value,
        callee: Value,
        this_v: Value,
    ) -> Result<Value, Thrown> {
        if !self.static_builtin_is_intrinsic("JSON", native::JSON_STRINGIFY, callee, this_v) {
            return self.call_value(callee, this_v, &[v]);
        }
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JsonStringify);
        let indent = self.json_indent(space_v);
        let _gc = self.gc_lock_guard();
        let plain = self.json_plain_stringify(v, &indent);
        Ok(match plain {
            Some(s) => self.alloc_str(s),
            None => {
                let mut m = crate::heap::ObjMap::new();
                m.set("", v);
                let wrapper = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
                let mut visited = Vec::new();
                match self.json_value(
                    wrapper,
                    "",
                    v,
                    &indent,
                    0,
                    &mut visited,
                    Value::UNDEFINED,
                    None,
                )? {
                    Some(s) => self.json_stringify_result(s, &indent),
                    None => Value::UNDEFINED,
                }
            }
        })
    }

    /// Shared `Instr::JsonParse` execution for interpreter dispatch and Tier C.
    pub(crate) fn exec_json_parse(
        &mut self,
        arg: Value,
        callee: Value,
        this_v: Value,
    ) -> Result<Value, Thrown> {
        if !self.static_builtin_is_intrinsic("JSON", native::JSON_PARSE, callee, this_v) {
            return self.call_value(callee, this_v, &[arg]);
        }
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JsonParse);
        let s: Vec<u8> = if arg.is_heap() && self.heap.is_str_like(arg.heap_index()) {
            self.heap
                .str_wtf8_cow(arg.heap_index())
                .unwrap()
                .into_owned()
        } else {
            self.to_js_string(arg)?.into_bytes()
        };
        self.json_parse(&s)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn throw_in_fn(&self, func_id: u32, err: Thrown) -> Thrown {
        let f = self.func(func_id as usize);
        let name: &str = if f.name.is_empty() { "<anonymous>" } else { &f.name };
        Thrown(format!("{} (in {name})", err.0))
    }

    #[inline(never)]
    pub(crate) fn copy_args_vec(&self, base: usize, arg_base: u16, argc: u16) -> Vec<Value> {
        let mut out = Vec::with_capacity(argc as usize);
        for i in 0..argc {
            out.push(self.get(base, arg_base + i));
        }
        out
    }

    #[inline(never)]
    pub(crate) fn call_value_regs(
        &mut self,
        callee: Value,
        this_val: Value,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Value, Thrown> {
        self.with_argv(base, arg_base, argc, |vm, argv| {
            vm.call_value(callee, this_val, argv)
        })
    }

    #[inline(never)]
    pub(crate) fn call_native_regs(
        &mut self,
        id: u16,
        this_val: Value,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Value, Thrown> {
        self.with_argv(base, arg_base, argc, |vm, argv| {
            vm.call_native(id, this_val, argv)
        })
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn load_global_or_undef_slow(&mut self, idx: u32) -> Result<Value, Thrown> {
        match self.global_slot_name(idx) {
            Some(name) if self.global_this != 0 => {
                let has_own = matches!(
                    self.heap.get(self.global_this),
                    HeapObj::Object(m) if m.pos(&name).is_some()
                );
                let gobj = Value::heap(self.global_this);
                let proto = self.object_get_prototype_of(gobj);
                if has_own || (proto.is_heap() && self.has_property_str_dyn(proto, &name)?) {
                    self.get_prop(gobj, &name)
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    #[inline(always)]
    pub(crate) fn store_global_slow_reg(
        &mut self,
        base: usize,
        idx: u32,
        src: u16,
        mode: u8,
    ) -> Result<(), Thrown> {
        let v = self.get(base, src);
        self.store_global_slow(idx, v, mode)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn store_global_slow(
        &mut self,
        idx: u32,
        v: Value,
        mode: u8,
    ) -> Result<(), Thrown> {
        let strict = mode != 0;
        if mode == 2 && !self.strict_unresolvable_globals.is_empty() {
            if let Some(p) = self
                .strict_unresolvable_globals
                .iter()
                .rposition(|&i| i == idx)
            {
                self.strict_unresolvable_globals.remove(p);
                let Some(name) = self.global_slot_name(idx) else {
                    return Err(self.unnamed_global_slot_error(idx));
                };
                return Err(Thrown::join3("ReferenceError: ", &*name, " is not defined"));
            }
        }
        if mode == 0
            && !self.eval_const_globals.is_empty()
            && self.eval_const_globals.contains(&idx)
            && !self.globals[idx as usize].is_uninitialized()
        {
            return Err(Thrown::from_static(
                "TypeError: Assignment to constant variable.",
            ));
        }
        if self.globals[idx as usize].is_uninitialized() {
            if !strict {
                if let Some(name) = self
                    .global_slot_name(idx)
                    .filter(|n| !self.global_name_is_lexical(n))
                {
                    let has_own = self.global_this != 0
                        && matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                    if has_own {
                        let gobj = Value::heap(self.global_this);
                        self.set_prop(gobj, &name, v, false)?;
                        return Ok(());
                    }
                    let gobj = Value::heap(self.global_this);
                    let proto = if self.global_this != 0 {
                        self.object_get_prototype_of(gobj)
                    } else {
                        Value::NULL
                    };
                    if proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                        self.set_prop(gobj, &name, v, false)?;
                        return Ok(());
                    }
                    if self.global_proto_chain_has_proxy() {
                        self.set_prop(gobj, &name, v, false)?;
                        return Ok(());
                    }
                    if !self.deleted_globals.is_empty() {
                        self.deleted_globals.remove(&name);
                    }
                }
            } else {
                let Some(name) = self.global_slot_name(idx) else {
                    return Err(self.unnamed_global_slot_error(idx));
                };
                if matches!(name.as_str(), "undefined" | "NaN" | "Infinity") {
                    return Err(Thrown::join3(
                        "TypeError: Cannot assign to read only property '",
                        &*name,
                        "'",
                    ));
                }
                if mode == 2 {
                    let own_backed = !self.global_name_is_lexical(&name)
                        && self.global_this != 0
                        && matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                    if own_backed {
                        let gobj = Value::heap(self.global_this);
                        self.set_prop(gobj, &name, v, true)?;
                        return Ok(());
                    }
                }
                if self.global_by_name(&name).is_none() {
                    if mode == 1 && self.eval_var_globals.contains(&idx) {
                        let gobj = Value::heap(self.global_this);
                        self.set_prop(gobj, &name, v, true)?;
                        return Ok(());
                    }
                    let gobj = Value::heap(self.global_this);
                    let proto = if self.global_this != 0 {
                        self.object_get_prototype_of(gobj)
                    } else {
                        Value::NULL
                    };
                    if proto.is_heap() && self.has_property_str_dyn(proto, &name)? {
                        self.set_prop(gobj, &name, v, true)?;
                        return Ok(());
                    }
                    return Err(Thrown::join3("ReferenceError: ", &*name, " is not defined"));
                }
            }
        } else if self.global_route_epoch != 0 && self.global_real_own_route(idx) {
            let name = self.global_slot_name(idx).unwrap_or_default();
            if !self.global_name_is_lexical(&name) {
                let gobj = Value::heap(self.global_this);
                self.set_prop(gobj, &name, v, strict)?;
                return Ok(());
            }
        }
        if strict && !self.eval_const_globals.is_empty() && self.eval_const_globals.contains(&idx) {
            return Err(Thrown::from_static(
                "TypeError: Assignment to constant variable.",
            ));
        }
        self.globals[idx as usize] = v;
        Ok(())
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments, unused_variables)]
    pub(crate) fn interp_get_prop_slow(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        dst: u16,
        o: Value,
        name: u32,
        key: &'p str,
        lite: bool,
    ) -> Result<ColdFlow, Thrown> {
        if is_private_key(key) {
            if let Some(v) = self.private_get_slow(func_id, ip, name, o, key)? {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        if let Some(v) = self.ic_get_prop_proto_hit(func_id, ip, o, key) {
            #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
            self.ic_proto_hit_settle(o);
            self.set(base, dst, v);
            return Ok(ColdFlow::Next(ip + 1));
        }
        if o.is_heap() && !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_)) {
            if let Some((target, handler, revoked)) = self.proxy_parts(o.heap_index()) {
                let r = match self.proxy_get(target, handler, revoked, key, o) {
                    Ok(v) => v,
                    Err(err) => return Err(self.throw_in_fn(func_id, err)),
                };
                self.set(base, dst, r);
                return Ok(ColdFlow::Next(ip + 1));
            }
            if let Some(v) = self.shapeless_read(o, key) {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
        } else if o.is_heap()
            && matches!(self.heap.get(o.heap_index()), HeapObj::Object(m) if m.is_ctor)
        {
            if let Some(v) = self.ctor_own_data_read(o, key) {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
        } else if o.is_heap()
            && self.arr_proto != 0
            && (o.heap_index() == self.arr_proto || o.heap_index() == self.str_proto)
            && self.realm_global_objs.is_empty()
            && self.active_realm.is_none()
        {
            let is_arr = o.heap_index() == self.arr_proto;
            if let Some(bits) = self.proto_intrinsic_bits(key, is_arr) {
                self.set(base, dst, Value::from_bits(bits));
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        match self.ic_get_prop(func_id, ip, o, key) {
            GetAct::Value(v) => {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
            GetAct::Accessor {
                fid,
                closure,
                getter,
            } => {
                let this_is_object = self.is_object_value(o);
                #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                if lite {
                    if let Some(v) = self.leaf_call(fid, closure, o, this_is_object, base, 0, 0) {
                        self.set(base, dst, v);
                        return Ok(ColdFlow::Next(ip + 1));
                    }
                }
                self.setup_call_this_outlined(
                    fid,
                    closure,
                    o,
                    this_is_object,
                    base,
                    0,
                    0,
                    dst,
                    ip + 1,
                    getter,
                )?;
                return Ok(ColdFlow::Frame);
            }
            GetAct::None => {}
        }
        if o.is_heap() {
            if let Some(v) = self.ctor_own_data_read(o, key) {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        let r = match self.get_prop(o, key) {
            Ok(v) => v,
            Err(err) => return Err(self.throw_in_fn(func_id, err)),
        };
        self.set(base, dst, r);
        Ok(ColdFlow::Next(ip + 1))
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments, unused_variables)]
    pub(crate) fn interp_set_prop_slow(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        obj: u16,
        name: u32,
        val: u16,
        site_strict: bool,
        o: Value,
        v: Value,
        key: &'p str,
        code: &[Instr],
    ) -> Result<ColdFlow, Thrown> {
        if matches!(key.as_bytes().first(), Some(b'#'))
            && self.private_set_slow(func_id, ip, name, o, key, v)?
        {
            return Ok(ColdFlow::Next(ip + 1));
        }
        if o.is_heap() && !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_)) {
            if matches!(self.heap.get(o.heap_index()), HeapObj::Proxy { .. }) {
                let strict = self.func(func_id as usize).is_strict || site_strict;
                self.set_prop_proxy(o, key, v, strict)?;
                return Ok(ColdFlow::Next(ip + 1));
            }
            if self.class_static_data_write(o, key, v) || self.fn_own_data_write(o, key, v) {
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        match self.ic_set_prop(func_id, ip, o, key, v) {
            SetAct::Done => {
                #[cfg(feature = "meter-only")]
                if let Some(
                    next @ Instr::GetProp {
                        dst,
                        obj: get_obj,
                        name: get_name,
                    },
                ) = code.get(ip + 1)
                {
                    let same_key = name == *get_name
                        || self.func(func_id as usize).string_constants[*get_name as usize]
                            .as_str()
                            == key;
                    if *get_obj == obj && same_key {
                        if self.instr_rec.is_some() {
                            if let Err(msg) = self.instrument_step(base, ip + 1, next) {
                                let top = self.frames.len() - 1;
                                self.frames[top].ip = ip + 1;
                                return Err(Thrown(msg.to_string()));
                            }
                        }
                        self.set(base, *dst, v);
                        return Ok(ColdFlow::Next(ip + 2));
                    }
                }
                return Ok(ColdFlow::Next(ip + 1));
            }
            SetAct::Setter {
                fid,
                closure,
                setter,
            } => {
                let this_is_object = self.is_object_value(o);
                self.setup_call_this_outlined(
                    fid,
                    closure,
                    o,
                    this_is_object,
                    base,
                    val,
                    1,
                    RET_DISCARD,
                    ip + 1,
                    setter,
                )?;
                return Ok(ColdFlow::Frame);
            }
            SetAct::None => {}
        }
        let strict = self.func(func_id as usize).is_strict || site_strict;
        self.set_prop(o, key, v, strict)?;
        Ok(ColdFlow::Next(ip + 1))
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn interp_call_with_this_slow(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        dst: u16,
        callee_v: Value,
        this_val: Value,
        arg_base: u16,
        argc: u16,
        name: u32,
        not_plain: bool,
        running: &'p crate::bytecode::FuncProto,
    ) -> Result<ColdFlow, Thrown> {
        if not_plain {
            if name == crate::bytecode::NO_NAME
                || matches!(
                    running.string_constants[name as usize].as_str(),
                    "slice" | "concat"
                )
            {
                if let Some(method) = self.captured_array_copy_method(func_id, ip) {
                    if let Some(exit) = self.try_array_copy_len_reduce(
                        func_id,
                        ip,
                        base,
                        this_val,
                        method,
                        Some(callee_v),
                    ) {
                        return Ok(ColdFlow::Next(exit));
                    }
                }
            }
            if callee_v.is_heap() {
                if let HeapObj::Native(id) = self.heap.get(callee_v.heap_index()) {
                    let nid = *id;
                    if nid == crate::vm::native::FN_CALL {
                        let (t, at, n) = if argc == 0 {
                            (Value::UNDEFINED, arg_base, 0)
                        } else {
                            (self.get(base, arg_base), arg_base + 1, argc - 1)
                        };
                        if let Some((fid, closure)) = self.plain_fn_entry(this_val) {
                            self.setup_call(fid, closure, t, base, at, n, dst, ip + 1, this_val)?;
                            return Ok(ColdFlow::Frame);
                        }
                        if self.realm_global_objs.is_empty() && this_val.is_heap() {
                            if let HeapObj::Native(tid) = self.heap.get(this_val.heap_index()) {
                                let tid = *tid;
                                let r = self.call_native_regs(tid, t, base, at, n)?;
                                self.set(base, dst, r);
                                return Ok(ColdFlow::Next(ip + 1));
                            }
                        }
                    } else if nid == crate::vm::native::FN_APPLY
                        && super::builtins::apply_fast_enabled()
                    {
                        if let Some((fid, closure)) = self.plain_fn_entry(this_val) {
                            if self.realm_global_objs.is_empty()
                                || self.get_function_realm(this_val) == 0
                            {
                                let t = if argc == 0 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base)
                                };
                                let list = if argc < 2 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base + 1)
                                };
                                if self.setup_apply_frame(
                                    fid,
                                    closure,
                                    t,
                                    list,
                                    dst,
                                    ip + 1,
                                    this_val,
                                )? {
                                    return Ok(ColdFlow::Frame);
                                }
                            }
                        } else if self.realm_global_objs.is_empty() && this_val.is_heap() {
                            if let HeapObj::Native(tid) = self.heap.get(this_val.heap_index()) {
                                let tid = *tid;
                                let t = if argc == 0 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base)
                                };
                                let list = if argc < 2 {
                                    Value::UNDEFINED
                                } else {
                                    self.get(base, arg_base + 1)
                                };
                                if let Some(args) = self.interp_apply_list(list) {
                                    let r = self.call_native(tid, t, &args)?;
                                    self.set(base, dst, r);
                                    return Ok(ColdFlow::Next(ip + 1));
                                }
                            }
                        }
                    }
                }
            }
        }
        if callee_v.is_heap() && this_val.is_heap() && self.realm_global_objs.is_empty() {
            if let HeapObj::Native(id) = self.heap.get(callee_v.heap_index()) {
                let id = *id;
                let kind_ok = match crate::vm::native::proto_method(id) {
                    Some((_, 3, _)) => {
                        matches!(self.heap.get(this_val.heap_index()), HeapObj::Set(_))
                    }
                    Some((_, 4, _)) => {
                        matches!(self.heap.get(this_val.heap_index()), HeapObj::Map { .. })
                    }
                    _ => false,
                };
                if kind_ok {
                    let r = self.call_native_regs(id, this_val, base, arg_base, argc)?;
                    self.set(base, dst, r);
                    return Ok(ColdFlow::Next(ip + 1));
                }
            }
        }
        if name != crate::bytecode::NO_NAME {
            if let Some(result) = self.captured_intrinsic_lane(
                func_id, base, this_val, callee_v, name, arg_base, argc,
            )? {
                self.set(base, dst, result);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        if self.realm_global_objs.is_empty() && self.realm_fns.is_empty() && callee_v.is_heap() {
            if let HeapObj::Native(id) = self.heap.get(callee_v.heap_index()) {
                let id = *id;
                let r = self.call_native_regs(id, this_val, base, arg_base, argc)?;
                self.set(base, dst, r);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        let r = self.call_value_regs(callee_v, this_val, base, arg_base, argc)?;
        self.set(base, dst, r);
        Ok(ColdFlow::Next(ip + 1))
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn interp_call_slow(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        dst: u16,
        callee_v: Value,
        arg_base: u16,
        argc: u16,
    ) -> Result<ColdFlow, Thrown> {
        if callee_v.is_heap() {
            let cidx = callee_v.heap_index();
            if let HeapObj::BoundResolver {
                promise,
                is_reject,
                pair,
            } = self.heap.get(cidx)
            {
                let (p, isr, pid) = (*promise, *is_reject, *pair);
                let arg = if argc >= 1 {
                    self.get(base, arg_base)
                } else {
                    Value::UNDEFINED
                };
                if self.resolver_pair_fire(pid) {
                    if isr {
                        self.reject(p, arg);
                    } else {
                        self.resolve(p, arg);
                    }
                }
                self.set(base, dst, Value::UNDEFINED);
                return Ok(ColdFlow::Next(ip + 1));
            }
            if argc == 0 {
                if let Some(r) = self.bound_step_call(func_id, ip, base, dst, callee_v) {
                    let r = r?;
                    self.set(base, dst, r);
                    return Ok(ColdFlow::Next(ip + 1));
                }
            }
            let via_call_value = matches!(
                self.heap.get(cidx),
                HeapObj::Proxy { .. }
                    | HeapObj::Bound { .. }
                    | HeapObj::Wrapped { .. }
                    | HeapObj::Native(_)
                    | HeapObj::NativeClosure { .. }
                    | HeapObj::CombinatorResolver { .. }
            ) || (self.fn_proto != 0 && cidx == self.fn_proto)
                || matches!(self.heap.get(cidx), HeapObj::Object(m) if m.is_ctor)
                || self.is_htmldda_index(cidx);
            if via_call_value {
                let r = self.call_value_regs(callee_v, Value::UNDEFINED, base, arg_base, argc)?;
                self.set(base, dst, r);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        let (fid, closure) = match self.resolve_callable_realm(callee_v) {
            Ok(v) => v,
            Err(err) => {
                #[cfg(not(target_arch = "wasm32"))]
                if crate::env_var_os("ZIPP_TRACE_CALLS").is_some() {
                    let f = self.func(func_id as usize);
                    let name: &str = if f.name.is_empty() { "<anonymous>" } else { &f.name };
                    let sample: Vec<&str> = f
                        .string_constants
                        .iter()
                        .map(|s| s.as_str())
                        .filter(|s| s.len() > 3)
                        .take(8)
                        .collect();
                    return Err(Thrown(format!("{} (in {name}) strings={sample:?}", err.0)));
                }
                return Err(self.throw_in_fn(func_id, err));
            }
        };
        let fp = self.func(fid as usize);
        if fp.is_generator || fp.is_async {
            let (is_gen, is_async) = (fp.is_generator, fp.is_async);
            let argv = self.copy_args_vec(base, arg_base, argc);
            self.pending_gen_callee = callee_v;
            let res = if is_gen && is_async {
                self.alloc_async_generator(fid, closure, Value::UNDEFINED, &argv)?
            } else if is_gen {
                self.alloc_generator(fid, closure, Value::UNDEFINED, &argv)?
            } else {
                self.alloc_async(fid, closure, Value::UNDEFINED, &argv)
            };
            self.set(base, dst, res);
            return Ok(ColdFlow::Next(ip + 1));
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
        Ok(ColdFlow::Frame)
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn interp_call_method_slow(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        dst: u16,
        recv: Value,
        name: u32,
        key: &'p str,
        object_recv: bool,
        arg_base: u16,
        argc: u16,
    ) -> Result<ColdFlow, Thrown> {
        if recv.is_heap()
            && matches!(self.heap.get(recv.heap_index()), HeapObj::DataView { .. })
        {
            let position = if argc == 0 {
                Value::UNDEFINED
            } else {
                self.get(base, arg_base)
            };
            let little_endian = if argc < 2 {
                Value::UNDEFINED
            } else {
                self.get(base, arg_base + 1)
            };
            if let Some(result) = self.dataview_interp_get_fast(
                recv.heap_index(),
                key,
                position,
                little_endian,
            )? {
                self.set(base, dst, result);
                return Ok(ColdFlow::Next(ip + 1));
            }
            if argc >= 2 && key.as_bytes().first() == Some(&b's') {
                let little_endian = if argc < 3 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base + 2)
                };
                let value = self.get(base, arg_base + 1);
                if self
                    .dataview_interp_set_fast(
                        recv.heap_index(),
                        key,
                        position,
                        value,
                        little_endian,
                    )?
                    .is_some()
                {
                    self.set(base, dst, Value::UNDEFINED);
                    return Ok(ColdFlow::Next(ip + 1));
                }
            }
            if key.len() > 6 && key.as_bytes()[3..6] == *b"Big" {
                let a1 = if argc < 2 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base + 1)
                };
                let a2 = if argc < 3 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base + 2)
                };
                if let Some(result) =
                    self.dataview_interp_big_fast(recv.heap_index(), key, position, a1, a2)?
                {
                    self.set(base, dst, result);
                    return Ok(ColdFlow::Next(ip + 1));
                }
            }
        }
        if key == "next"
            && self.realm_global_objs.is_empty()
            && recv.is_heap()
            && matches!(self.heap.get(recv.heap_index()), HeapObj::Generator(_))
            && self.gen_method_pristine(recv.heap_index(), "next")
        {
            let sent = if argc == 0 {
                Value::UNDEFINED
            } else {
                self.get(base, arg_base)
            };
            super::builtins::builtin_stats_count(self, recv, "next");
            if self.gen_enter_inline(
                recv.heap_index(),
                ip,
                dst,
                super::async_runtime::GEN_STEP_OBJECT,
                sent,
                false,
            ) {
                return Ok(ColdFlow::Frame);
            }
            if let Some(result) = self.generator_method(recv.heap_index(), "next", &[sent])? {
                self.set(base, dst, result);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        if matches!(key, "slice" | "concat") {
            if let Some(exit) =
                self.try_array_copy_len_reduce(func_id, ip, base, recv, key, None)
            {
                return Ok(ColdFlow::Next(exit));
            }
        }
        #[cfg(not(feature = "meter-only"))]
        if recv.is_heap() && matches!(self.heap.get(recv.heap_index()), HeapObj::Array(_)) {
            if let Some(v) = self.interp_array_lane(recv, key, base, arg_base, argc) {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        let mut private_callee: Option<Value> = None;
        if is_private_key(key) {
            #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
            if let GetAct::Value(f) = self.ic_get_prop_way0(func_id, ip, recv, name) {
                if let Some((fid, closure)) = self.plain_fn_entry(f) {
                    self.setup_call_lean_this(
                        fid, closure, recv, true, base, arg_base, argc, dst, ip + 1, f,
                    )?;
                    return Ok(ColdFlow::Frame);
                }
            }
            let ctx = self.frames.last().map_or(Value::UNDEFINED, |f| f.callee);
            let site = super::values::private_site(func_id, name);
            private_callee =
                self.private_method_callee(ctx, recv, key, Some(site), Some((func_id, ip)))?;
            if let Some(f) = private_callee {
                if let Some((fid, closure)) = self.plain_fn_entry(f) {
                    self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1, f)?;
                    return Ok(ColdFlow::Frame);
                }
            }
        }
        if private_callee.is_none() && (argc == 1 || argc == 2) {
            if let Some(result) =
                self.interp_map_method_fast(recv, key, base, arg_base, argc)?
            {
                self.set(base, dst, result);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        if private_callee.is_none() && key == "call" {
            let (this_v, args_at, n) = if argc == 0 {
                (Value::UNDEFINED, arg_base, 0)
            } else {
                (self.get(base, arg_base), arg_base + 1, argc - 1)
            };
            if let Some((fid, closure)) = self.fn_call_plain_target(recv) {
                self.setup_call(fid, closure, this_v, base, args_at, n, dst, ip + 1, recv)?;
                return Ok(ColdFlow::Frame);
            }
            if let Some(id) = self.fn_call_native_target(recv) {
                if let Some(it) = self.interp_array_values_call(id, this_v) {
                    self.set(base, dst, it);
                    return Ok(ColdFlow::Next(ip + 1));
                }
                let r = self.call_native_regs(id, this_v, base, args_at, n)?;
                self.set(base, dst, r);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        if private_callee.is_none() && key == "bind" {
            if let Some(b) = self.interp_fn_bind_fast(recv, base, arg_base, argc) {
                self.set(base, dst, b);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        if private_callee.is_none() && key == "apply" {
            if let Some((fid, closure)) = self.fn_apply_plain_target(recv) {
                let this_v = if argc == 0 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base)
                };
                let list = if argc < 2 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base + 1)
                };
                if self.setup_apply_frame(fid, closure, this_v, list, dst, ip + 1, recv)? {
                    return Ok(ColdFlow::Frame);
                }
            } else if let Some(id) = self.fn_apply_native_target(recv) {
                let this_v = if argc == 0 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base)
                };
                let list = if argc < 2 {
                    Value::UNDEFINED
                } else {
                    self.get(base, arg_base + 1)
                };
                if let Some(args) = self.interp_apply_list(list) {
                    let r = self.call_native(id, this_v, &args)?;
                    self.set(base, dst, r);
                    return Ok(ColdFlow::Next(ip + 1));
                }
            }
        }
        if private_callee.is_none() && !object_recv {
            if let Some((fid, closure, callee)) = self.ic_call_method(func_id, ip, recv, key) {
                self.setup_call_lean(
                    fid, closure, recv, base, arg_base, argc, dst, ip + 1, callee,
                )?;
                return Ok(ColdFlow::Frame);
            }
        }
        if private_callee.is_none() && recv.is_heap() {
            if let Some(f) = self
                .class_static_data_read(recv, key)
                .or_else(|| self.fn_own_data_read(recv, key))
            {
                if let Some((fid, closure)) = self.plain_fn_entry(f) {
                    self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1, f)?;
                    return Ok(ColdFlow::Frame);
                }
            }
        }
        if let Some(result) = self.try_builtin_method(recv, key, base, arg_base, argc)? {
            self.set(base, dst, result);
            return Ok(ColdFlow::Next(ip + 1));
        }
        let prop = match private_callee {
            Some(f) => f,
            None => match self.get_prop(recv, key) {
                Ok(v) => v,
                Err(err) => return Err(self.throw_in_fn(func_id, err)),
            },
        };
        if prop.is_heap() {
            let pidx = prop.heap_index();
            if matches!(self.heap.get(pidx), HeapObj::Object(m) if m.is_ctor) {
                let r = self.call_value_regs(prop, Value::UNDEFINED, base, arg_base, argc)?;
                self.set(base, dst, r);
                return Ok(ColdFlow::Next(ip + 1));
            }
            if matches!(
                self.heap.get(pidx),
                HeapObj::Native(_)
                    | HeapObj::NativeClosure { .. }
                    | HeapObj::Bound { .. }
                    | HeapObj::BoundResolver { .. }
                    | HeapObj::Proxy { .. }
            ) || (self.fn_proto != 0 && pidx == self.fn_proto)
            {
                let r = self.call_value_regs(prop, recv, base, arg_base, argc)?;
                self.set(base, dst, r);
                return Ok(ColdFlow::Next(ip + 1));
            }
        }
        let (fid, closure) = self.resolve_callable_named(prop, key)?;
        let fp = self.func(fid as usize);
        if fp.is_generator || fp.is_async {
            let (is_gen, is_async) = (fp.is_generator, fp.is_async);
            let argv = self.copy_args_vec(base, arg_base, argc);
            self.pending_gen_callee = prop;
            let res = if is_gen && is_async {
                self.alloc_async_generator(fid, closure, recv, &argv)?
            } else if is_gen {
                self.alloc_generator(fid, closure, recv, &argv)?
            } else {
                self.alloc_async(fid, closure, recv, &argv)
            };
            self.set(base, dst, res);
            return Ok(ColdFlow::Next(ip + 1));
        }
        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1, prop)?;
        Ok(ColdFlow::Frame)
    }

    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn interp_math_op_slow(
        &mut self,
        func_id: u32,
        base: usize,
        op: crate::bytecode::MathFn,
        callee: u16,
        this_v: u16,
        arg_base: u16,
        argc: u16,
    ) -> Result<Value, Thrown> {
        if callee == crate::bytecode::NO_REG {
            let Some(gslot) = self.bare_math_slot(this_v as u32) else {
                return Err(Thrown::from_static("ReferenceError: Math is not defined"));
            };
            let recv = self.load_global_value(gslot, func_id)?;
            let name = native::MATH_METHODS[op as usize].0;
            let f = self.get_prop(recv, name)?;
            self.call_value_regs(f, recv, base, arg_base, argc)
        } else {
            let callee = self.get(base, callee);
            let this_v = self.get(base, this_v);
            self.call_value_regs(callee, this_v, base, arg_base, argc)
        }
    }

    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn interp_global_fn_slow(
        &mut self,
        ip: usize,
        base: usize,
        dst: u16,
        live: Value,
        arg_base: u16,
        argc: u16,
    ) -> Result<ColdFlow, Thrown> {
        if let Ok((fid, closure)) = self.resolve_callable(live) {
            let fp = self.func(fid as usize);
            if !fp.is_generator && !fp.is_async {
                self.setup_call(
                    fid,
                    closure,
                    Value::UNDEFINED,
                    base,
                    arg_base,
                    argc,
                    dst,
                    ip + 1,
                    live,
                )?;
                return Ok(ColdFlow::Frame);
            }
        }
        let out = self.call_value_regs(live, Value::UNDEFINED, base, arg_base, argc)?;
        self.set(base, dst, out);
        Ok(ColdFlow::Next(ip + 1))
    }

    #[inline(never)]
    pub(crate) fn interp_has_prop_slow(
        &mut self,
        func_id: u32,
        ip: usize,
        k: Value,
        o: Value,
        brand: bool,
    ) -> Result<bool, Thrown> {
        if !self.is_object_value(o) {
            let kd = self.display(k);
            return Err(Thrown(format!(
                "TypeError: Cannot use 'in' operator to search for '{kd}' in a non-object"
            )));
        }
        if brand {
            let ctx = self.frames.last().map_or(Value::UNDEFINED, |f| f.callee);
            let site = super::values::private_site(func_id, 0x8000_0000 | ip as u32);
            let resolved = match self.private_memo_hit(ctx, site) {
                Some(hit) => Some(hit),
                None => {
                    let key = self.key_of(k);
                    self.resolve_private_site(ctx, site, &key)
                }
            };
            if let Some((b2, kind, owner, pid)) = resolved {
                Ok(if kind & 7 == 0 {
                    self.private_field_get_id(o, b2, pid).is_some()
                } else {
                    self.private_receiver_ok(o, b2, kind, owner)
                })
            } else {
                let key = self.key_of(k);
                let textual = self.has_property_str(o, &key)
                    || self.private_field_scan_has(o, &key);
                Ok(match self.private_brand_ok(o, &key) {
                    Some(b) => textual && b,
                    None => textual,
                })
            }
        } else {
            let k = self.coerce_index_key(k)?;
            self.has_property_dyn(o, k)
        }
    }

    #[inline(never)]
    fn exec_field_init(
        &mut self,
        base: usize,
        key_index: u16,
        val: u16,
        class_id: u32,
    ) -> Result<(), Thrown> {
        let this = self.get(base, 0);
        let v = self.get(base, val);
        let key = if class_id != u32::MAX {
            self.class_value_for(class_id)
                .filter(|cv| cv.is_heap())
                .and_then(|cv| match self.heap.get(cv.heap_index()) {
                    HeapObj::Class(c) => c.computed_field_keys.get(key_index as usize).copied(),
                    _ => None,
                })
        } else {
            match self.heap.get(this.heap_index()) {
                HeapObj::Object(m) => m.class.and_then(|cidx| match self.heap.get(cidx) {
                    HeapObj::Class(c) => c.computed_field_keys.get(key_index as usize).copied(),
                    _ => None,
                }),
                _ => None,
            }
        };
        if let Some(key) = key {
            let ks = self.key_of(key);
            self.define_field(this, &ks, v)?;
        }
        Ok(())
    }

    #[inline(never)]
    fn exec_super_get(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        dst: u16,
        home_class_id: u32,
        name: u32,
    ) -> Result<ColdFlow, Thrown> {
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let stamped = self.ic_super_stamped(func_id, ip);
        #[cfg(not(all(feature = "jit", not(feature = "safe-sandbox"))))]
        let stamped = GetAct::None;
        let act = match stamped {
            GetAct::None => {
                let fp = self.func(func_id as usize);
                let key = fp.string_constants[name as usize].as_str();
                let is_static = fp.super_static;
                self.ic_super_get(func_id, ip, home_class_id, is_static, key)
            }
            act => act,
        };
        match act {
            GetAct::Value(v) => {
                self.set(base, dst, v);
                return Ok(ColdFlow::Next(ip + 1));
            }
            GetAct::Accessor {
                fid,
                closure,
                getter,
            } => {
                let this = self.get(base, 0);
                #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                if self.lite_entry_ok() {
                    let strict = self.func(fid as usize).is_strict;
                    if let Some(v) = self.leaf_call(fid, closure, this, strict, base, 0, 0) {
                        self.set(base, dst, v);
                        return Ok(ColdFlow::Next(ip + 1));
                    }
                }
                self.setup_call(fid, closure, this, base, 0, 0, dst, ip + 1, getter)?;
                return Ok(ColdFlow::Frame);
            }
            GetAct::None => {}
        }
        let fp = self.func(func_id as usize);
        let key = fp.string_constants[name as usize].as_str();
        let proto = self.super_base(home_class_id, fp.super_static);
        self.require_object_coercible(proto)?;
        let this = self.get(base, 0);
        let r = self.get_member(proto, key, this)?;
        self.set(base, dst, r);
        Ok(ColdFlow::Next(ip + 1))
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn exec_super_set(
        &mut self,
        func_id: u32,
        ip: usize,
        base: usize,
        base_reg: u16,
        home_class_id: u32,
        name: u32,
        val: u16,
    ) -> Result<ColdFlow, Thrown> {
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let stamped = self.ic_super_set_stamped(func_id, ip);
        #[cfg(not(all(feature = "jit", not(feature = "safe-sandbox"))))]
        let stamped = SetAct::None;
        let act = match stamped {
            SetAct::None => {
                let fp = self.func(func_id as usize);
                let key = fp.string_constants[name as usize].as_str();
                let is_static = fp.super_static;
                self.ic_super_set(func_id, ip, home_class_id, is_static, key)
            }
            act => act,
        };
        let this = self.get(base, 0);
        if let SetAct::Setter {
            fid,
            closure,
            setter,
        } = act
        {
            self.setup_call(
                fid,
                closure,
                this,
                base,
                val,
                1,
                RET_DISCARD,
                ip + 1,
                setter,
            )?;
            return Ok(ColdFlow::Frame);
        }
        let fp = self.func(func_id as usize);
        let key = fp.string_constants[name as usize].as_str();
        let strict = fp.is_strict;
        let v = self.get(base, val);
        let proto = self.get(base, base_reg);
        self.super_set_obj(proto, key, this, v, strict)?;
        Ok(ColdFlow::Next(ip + 1))
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn exec_static_fn(
        &mut self,
        base: usize,
        dst: u16,
        op: crate::bytecode::StaticFn,
        callee: u16,
        this_v: u16,
        arg_base: u16,
        argc: u16,
    ) -> Result<(), Thrown> {
        use crate::bytecode::StaticFn as S;
        let callee = self.get(base, callee);
        let this_v = self.get(base, this_v);
        if argc == 1 && self.static_fn_is_intrinsic(op, callee, this_v) {
            let a0 = self.get(base, arg_base);
            let fast = match op {
                S::StringFromCodePoint => Some(self.string_from_code_point_1(a0)?),
                S::StringFromCharCode => Some(self.string_from_char_codes_val(&[a0])?),
                S::NumberIsInteger => Some(Value::bool(num_is_integer(a0))),
                S::NumberIsNaN => Some(Value::bool(a0.is_double() && a0.as_f64().is_nan())),
                S::NumberIsFinite => Some(Value::bool(num_is_finite(a0))),
                S::NumberIsSafeInteger => Some(Value::bool(num_is_safe_integer(a0))),
                _ => None,
            };
            if let Some(v) = fast {
                self.set(base, dst, v);
                return Ok(());
            }
        }
        if !self.static_fn_is_intrinsic(op, callee, this_v) {
            let v = self.call_value_regs(callee, this_v, base, arg_base, argc)?;
            self.set(base, dst, v);
            return Ok(());
        }
        let args = self.copy_args_vec(base, arg_base, argc);
        let v = self.static_fn_exec(op, args)?;
        self.set(base, dst, v);
        Ok(())
    }

    #[inline(never)]
    fn exec_end_finally(
        &mut self,
        ip: usize,
        base: usize,
        kind_reg: u16,
        val_reg: u16,
        stop_depth: usize,
    ) -> Result<ColdFlow, Thrown> {
        let raw = self.regs[base + kind_reg as usize].as_int();
        match raw & 3 {
            1 => {
                let v = self.regs[base + val_reg as usize];
                if let Some(target) = self.route_through_finally(1, v) {
                    return Ok(ColdFlow::Next(target as usize));
                }
                if self.pop_frame_with(v, stop_depth) {
                    return Ok(ColdFlow::Done(v));
                }
                Ok(ColdFlow::Frame)
            }
            2 => {
                let v = self.regs[base + val_reg as usize];
                let top = self.frames.len() - 1;
                self.frames[top].ip = ip;
                self.pending_throw = Some(v);
                Err(Thrown(self.throw_message(v)))
            }
            3 => {
                let jump_target = self.regs[base + val_reg as usize].as_int() as u32;
                let floor = (raw >> 2) as usize;
                let from = ip;
                let next_ip = match self.route_jump_through_finally(jump_target, floor) {
                    Some(target) => target as usize,
                    None => jump_target as usize,
                };
                if next_ip < from {
                    self.maybe_gc();
                }
                Ok(ColdFlow::Next(next_ip))
            }
            _ => Ok(ColdFlow::Next(ip + 1)),
        }
    }
}
