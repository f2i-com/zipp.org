#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Shared driver for `map`/`filter`/`forEach` (callback args = [element,
    /// index]). Uses the native callback fast path when the callback is a
    /// compiled non-capturing function: a single reused register window, a direct
    /// native call per element. Falls back to `call_value` per element otherwise.
    /// The window is always released (truncate) before returning — including on a
    /// callback error — so a thrown callback never leaks register slots.
    pub(crate) fn array_each(&mut self, idx: u32, cb: Value, mode: EachMode, this_arg: Value) -> Result<Option<Value>, Thrown> {
        // IsCallable(callback) precedes iteration: map/filter/forEach on an EMPTY
        // array with a non-callable callback must still throw TypeError.
        if !self.is_callable(cb) {
            let m = match mode {
                EachMode::Map => "map",
                EachMode::Filter => "filter",
                EachMode::ForEach => "forEach",
            };
            return Err(Thrown(format!("TypeError: {m} callback is not a function")));
        }
        // `out` (and the snapshot) hold values not reachable from the GC roots
        // while the callback re-enters the interpreter — suspend GC for the scope.
        let _gc = self.gc_lock_guard();
        let snapshot = self.array_snapshot(idx);
        // The receiver passed to the callback as its 3rd argument.
        let receiver = Value::heap(idx);
        // The fused kernels inline the callback over (element, index) only and run
        // with `this`=undefined, so they cannot honour a thisArg, a 3rd "array"
        // parameter, or `arguments`. Disable them when the callback could observe
        // any of those (the per-element path below handles every case correctly).
        let kernel_ok = this_arg.is_undefined();
        let collect = matches!(mode, EachMode::Map | EachMode::Filter);
        let mut out: Vec<Value> =
            if collect { Vec::with_capacity(snapshot.len()) } else { Vec::new() };

        // Fused native map kernel: inline the callback into a native loop over
        // the snapshot for the leading run of integer elements — eliminating the
        // per-element call boundary (the gap to V8, which inlines callbacks). Map
        // only (dense, ordered store). On a type-guard bail the kernel returns
        // the index it reached, having written results `[0, start)`; the
        // per-element loop below finishes `[start, len)` correctly (handling
        // doubles/strings/etc.), so a mixed array can never give a wrong answer.
        let mut start = 0usize;
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Map)
            && kernel_ok
            && self.jit_enabled
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot.len() <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto =
                        self.func(fid as usize);
                    // SAFETY: program functions are immutable during execution;
                    // the raw ptr dodges the self.jit (&mut) vs self.program (&)
                    // borrow conflict (same pattern as native_cb_entry).
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    // A callback that declares the 3rd (array) param or uses
                    // `arguments` must see the receiver — not the kernel's path.
                    let kernel_entry = if proto_ref.param_count >= 3
                        || proto_ref.arguments_reg.is_some()
                    {
                        None
                    } else {
                        self.jit.map_kernel(fid, proto_ref)
                    };
                    if let Some(entry) = kernel_entry {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot.len();
                            let window_ptr =
                                unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                            let snap_ptr = snapshot.as_ptr() as *const u64;
                            let out_ptr = out.as_mut_ptr() as *mut u64;
                            // SAFETY: `entry` is a valid win64 map kernel; the
                            // window holds `reg_count` slots; `out` has capacity
                            // `len` ≥ the returned count; the kernel is call-free
                            // so none of these pointers move during the call.
                            let kernel: extern "win64" fn(
                                *mut u64,
                                *const u64,
                                usize,
                                *mut u64,
                            ) -> usize = unsafe { core::mem::transmute(entry) };
                            let processed = kernel(window_ptr, snap_ptr, len, out_ptr);
                            // The kernel wrote `out[0..processed]` densely.
                            unsafe { out.set_len(processed) };
                            self.regs.truncate(win);
                            start = processed;
                        }
                    }
                }
            }
        }

        // Fused native filter kernel: inline the predicate over the snapshot for
        // the leading numeric run, compacting kept elements into `out`. The
        // predicate result must be a Bool (a comparison); a non-Bool result bails
        // that element to the per-element tail (which evaluates JS truthiness).
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Filter)
            && kernel_ok
            && self.jit_enabled
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot.len() <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto =
                        self.func(fid as usize);
                    // SAFETY: as the map branch above.
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    // Skip the kernel when the predicate could observe the 3rd
                    // (array) param or `arguments` (see the map branch).
                    let kernel_entry = if proto_ref.param_count >= 3
                        || proto_ref.arguments_reg.is_some()
                    {
                        None
                    } else {
                        self.jit.filter_kernel(fid, proto_ref)
                    };
                    if let Some(entry) = kernel_entry {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot.len();
                            let window_ptr =
                                unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                            let snap_ptr = snapshot.as_ptr() as *const u64;
                            let out_ptr = out.as_mut_ptr() as *mut u64;
                            let mut kept: usize = 0;
                            // SAFETY: valid win64 filter kernel; window has
                            // reg_count slots; `out` capacity `len` ≥ kept; the
                            // kernel is call-free so the pointers don't move.
                            let kernel: extern "win64" fn(
                                *mut u64,
                                *const u64,
                                usize,
                                *mut u64,
                                *mut usize,
                            ) -> usize = unsafe { core::mem::transmute(entry) };
                            let scanned =
                                kernel(window_ptr, snap_ptr, len, out_ptr, &mut kept as *mut usize);
                            // The kernel wrote `kept` elements into `out[0..kept]`.
                            unsafe { out.set_len(kept) };
                            self.regs.truncate(win);
                            start = scanned;
                        }
                    }
                }
            }
        }

        // Per-element path for `[start, len)` — the whole array when no kernel
        // ran, or just the tail after a kernel bail (or nothing if it completed).
        let run_tail = start < snapshot.len();
        let mut native = if run_tail { self.native_cb_entry(cb) } else { None };
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None; // can't fit a window → interpreter path
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }

        let mut err = None;
        for i in start..snapshot.len() {
            let v = snapshot[i];
            let args = [v, Value::int(i as i32), receiver];
            match self.run_cb_elem(native, win, cb, &args, this_arg) {
                Ok(r) => match mode {
                    EachMode::Map => out.push(r),
                    EachMode::Filter => {
                        if self.truthy(r) {
                            out.push(v);
                        }
                    }
                    EachMode::ForEach => {}
                },
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        match mode {
            EachMode::ForEach => Ok(Some(Value::UNDEFINED)),
            _ => Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out))))),
        }
    }

    /// Allocate a built-in iterator over a snapshot of `items` with prototype `proto`.
    pub(crate) fn make_iterator(&mut self, items: Vec<Value>, proto: u32) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Iterator { items, index: 0, proto }))
    }

    /// The hole-skipping iteration methods (forEach/map/filter/some/every/reduce/
    /// reduceRight) run against an array-like *object* by visiting only indices
    /// where HasProperty is true — unlike the dense-snapshot fast path, this
    /// honours absent indices (own or inherited holes), per the spec.
    pub(crate) fn array_like_iterate(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_coerce(lv)?;
        let len: usize = if lenf.is_finite() && lenf > 0.0 {
            (lenf as usize).min(crate::vm::MAX_DENSE_ARRAY_LEN)
        } else {
            0
        };
        let cb = args.first().copied().unwrap_or(Value::UNDEFINED);
        if !self.is_callable(cb) {
            return Err(Thrown(format!("TypeError: {name} callback is not a function")));
        }
        let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        let idxv = |k: usize| Value::num(k as f64);

        match name {
            "forEach" => {
                for k in 0..len {
                    if self.has_property(this, idxv(k)) {
                        let val = self.get_index(this, idxv(k))?;
                        self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                    }
                }
                Ok(Some(Value::UNDEFINED))
            }
            "map" => {
                // map does ArraySpeciesCreate(O, len); for a non-array O that is
                // ArrayCreate(len), which throws RangeError when len > 2^32-1 — and
                // it happens BEFORE any element is visited. (forEach/some/every/
                // reduce create no array, and filter creates length 0, so only map
                // validates here.)
                if lenf.is_finite() && lenf.floor() > 4_294_967_295.0 {
                    return Err(Thrown("RangeError: Invalid array length".into()));
                }
                let mut out = vec![Value::UNDEFINED; len];
                for k in 0..len {
                    if self.has_property(this, idxv(k)) {
                        let val = self.get_index(this, idxv(k))?;
                        out[k] = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "filter" => {
                let mut out = Vec::new();
                for k in 0..len {
                    if self.has_property(this, idxv(k)) {
                        let val = self.get_index(this, idxv(k))?;
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if self.truthy(r) {
                            out.push(val);
                        }
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "some" => {
                for k in 0..len {
                    if self.has_property(this, idxv(k)) {
                        let val = self.get_index(this, idxv(k))?;
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if self.truthy(r) {
                            return Ok(Some(Value::bool(true)));
                        }
                    }
                }
                Ok(Some(Value::bool(false)))
            }
            "every" => {
                for k in 0..len {
                    if self.has_property(this, idxv(k)) {
                        let val = self.get_index(this, idxv(k))?;
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if !self.truthy(r) {
                            return Ok(Some(Value::bool(false)));
                        }
                    }
                }
                Ok(Some(Value::bool(true)))
            }
            "reduce" | "reduceRight" => {
                let right = name == "reduceRight";
                let order: Vec<usize> =
                    if right { (0..len).rev().collect() } else { (0..len).collect() };
                let mut acc = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let mut started = args.len() >= 2;
                for k in order {
                    if !self.has_property(this, idxv(k)) {
                        continue;
                    }
                    let val = self.get_index(this, idxv(k))?;
                    if !started {
                        acc = val;
                        started = true;
                    } else {
                        acc = self.call_value(
                            cb,
                            Value::UNDEFINED,
                            &[acc, val, idxv(k), this],
                        )?;
                    }
                }
                if !started {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                }
                Ok(Some(acc))
            }
            _ => Ok(None),
        }
    }

    /// `Array.prototype.copyWithin` against an array-like *object* via the generic
    /// Get/Set/HasProperty/DeletePropertyOrThrow protocol, so it propagates abrupt
    /// completions (a throwing length/index coercion, or a non-configurable target
    /// that can't be deleted → TypeError). Real arrays use the dense fast path.
    pub(crate) fn array_like_copy_within(
        &mut self,
        this: Value,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_coerce(lv)?;
        // ToLength: clamp to [0, 2^53-1].
        let len: i64 = if lenf.is_nan() || lenf <= 0.0 {
            0
        } else {
            lenf.floor().min(9_007_199_254_740_991.0) as i64
        };
        let rel = |i: i64| -> i64 { if i < 0 { (len + i).max(0) } else { i.min(len) } };
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let mut to = rel(self.to_integer_or_zero(arg0)?);
        let s0 = if args.len() >= 2 { self.to_integer_or_zero(args[1])? } else { 0 };
        let mut from = rel(s0);
        let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
            self.to_integer_or_zero(args[2])?
        } else {
            len
        };
        let mut count = (rel(e0) - from).min(len - to).max(0);
        let mut dir = 1i64;
        if from < to && to < from + count {
            dir = -1;
            from += count - 1;
            to += count - 1;
        }
        while count > 0 {
            let fk = Value::num(from as f64);
            if self.has_property(this, fk) {
                let v = self.get_index(this, fk)?;
                self.set_index(this, Value::num(to as f64), v, false)?;
            } else {
                let deleted = self.delete_property(this, &to.to_string())?;
                if !self.truthy(deleted) {
                    return Err(Thrown(format!("TypeError: cannot delete property '{to}'")));
                }
            }
            from += dir;
            to += dir;
            count -= 1;
        }
        Ok(Some(this))
    }

    /// `Array.prototype.fill` against an array-like *object* via Set, so a
    /// throwing setter, a non-writable/frozen slot, a symbol length, or a
    /// throwing index coercion propagates (abrupt completion). Real arrays use
    /// the dense fast path.
    pub(crate) fn array_like_fill(
        &mut self,
        this: Value,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_coerce(lv)?;
        let len: i64 = if lenf.is_nan() || lenf <= 0.0 {
            0
        } else {
            lenf.floor().min(9_007_199_254_740_991.0) as i64
        };
        let rel = |i: i64| -> i64 { if i < 0 { (len + i).max(0) } else { i.min(len) } };
        let value = args.first().copied().unwrap_or(Value::UNDEFINED);
        let s0 = if args.len() >= 2 { self.to_integer_or_zero(args[1])? } else { 0 };
        let mut k = rel(s0);
        let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
            self.to_integer_or_zero(args[2])?
        } else {
            len
        };
        let end = rel(e0);
        while k < end {
            self.set_index(this, Value::num(k as f64), value, false)?;
            k += 1;
        }
        Ok(Some(this))
    }

    pub(crate) fn array_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        // Suspend GC for the whole method: callback-driven arms (map/filter/
        // reduce/sort/…) hold un-rooted working sets across interpreter re-entry,
        // and the array-like path builds an un-rooted temp array. Non-callback
        // arms never reach a GC safe point, so the lock is free for them.
        let _gc = self.gc_lock_guard();
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Generic array methods accept an array-like `this`
        // (`Array.prototype.map.call({length:2, 0:'a', 1:'b'}, cb)`, or on a string).
        // For a non-array receiver, snapshot its `length` + indexed elements into a
        // temp array and run the (read-only) method against that. Mutating methods
        // still require a real array (they fall through to their HeapObj::Array arms).
        if !matches!(self.heap.get(idx), HeapObj::Array(_)) {
            // Hole-skipping callback methods iterate the array-like object with
            // HasProperty per index (a dense snapshot would treat holes as
            // present-undefined and wrongly invoke the callback on them).
            if matches!(
                name,
                "map" | "filter" | "forEach" | "every" | "some" | "reduce" | "reduceRight"
            ) {
                return self.array_like_iterate(Value::heap(idx), name, args);
            }
            // copyWithin mutates an array-like in place via the generic protocol
            // (Get/Set/HasProperty/DeletePropertyOrThrow), propagating abrupt
            // completions a dense snapshot would swallow.
            if name == "copyWithin" {
                return self.array_like_copy_within(Value::heap(idx), args);
            }
            if name == "fill" {
                return self.array_like_fill(Value::heap(idx), args);
            }
            // Read-only methods that treat a hole as undefined snapshot to a dense
            // temp array and run against that.
            if matches!(
                name,
                "find" | "findIndex" | "findLast" | "findLastIndex" | "indexOf"
                    | "lastIndexOf" | "includes" | "join" | "toString" | "slice" | "at"
                    | "concat" | "flat" | "flatMap" | "with" | "toReversed" | "toSorted"
                    | "toSpliced" | "entries" | "keys" | "values" | "toLocaleString"
            ) {
                // with/toReversed/toSorted/toSpliced build a result of the source
                // length via ArrayCreate(len), which throws RangeError for
                // len > 2^32-1 — BEFORE reading any element (a throwing index getter
                // must not run first).
                if matches!(name, "with" | "toReversed" | "toSorted" | "toSpliced") {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let n = self.to_number_coerce(lv)?;
                    let len = if n.is_finite() && n > 0.0 { n.floor() } else { 0.0 };
                    if len > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                }
                let elems = self.array_like_read(idx)?;
                let tmp = self.heap.alloc(HeapObj::Array(elems));
                return self.array_method(tmp, name, args);
            }
        }
        // A REAL array that carries an arr_props side table may hold a
        // defineProperty'd index ACCESSOR — its getter lives in arr_props while the
        // dense slot is only an undefined placeholder. The dense fast paths below
        // read that placeholder and never invoke the getter, so route the callback
        // methods through the generic HasProperty/Get protocol (which calls
        // get_index → array_index_override → the getter). Arrays without a side
        // table keep the fast snapshot path (zero perf impact on the common case).
        if self.arr_props.contains_key(&idx)
            && matches!(
                name,
                "map" | "filter" | "forEach" | "every" | "some" | "reduce" | "reduceRight"
            )
        {
            return self.array_like_iterate(Value::heap(idx), name, args);
        }
        match name {
            "push" => {
                let mut last = Value::UNDEFINED;
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for a in args {
                        items.push(*a);
                    }
                    last = Value::int(items.len() as i32);
                }
                Ok(Some(last))
            }
            "pop" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    return Ok(Some(items.pop().unwrap_or(Value::UNDEFINED)));
                }
                Ok(Some(Value::UNDEFINED))
            }
            "shift" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    if items.is_empty() {
                        return Ok(Some(Value::UNDEFINED));
                    }
                    return Ok(Some(items.remove(0)));
                }
                Ok(Some(Value::UNDEFINED))
            }
            "unshift" => {
                // Prepend all args (preserving order) and return the new length.
                let len = if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for (i, &v) in args.iter().enumerate() {
                        items.insert(i, v);
                    }
                    items.len()
                } else {
                    0
                };
                self.heap.bump_version(idx);
                Ok(Some(len_value(len)))
            }
            // `Array.prototype.toString()` is `join()` with the default "," sep.
            "join" | "toString" => {
                // ToString the separator (undefined -> ","), and ToString each
                // element — invoking a custom `toString`/`@@toPrimitive`, not the
                // infallible `display`. (to_js_string short-circuits primitives to
                // `display`, so a numeric/string array join stays on the fast path.)
                let sep = if name == "toString" || arg0 == Value::UNDEFINED {
                    ",".to_string()
                } else {
                    self.to_js_string(arg0)?
                };
                let snapshot = self.array_snapshot(idx);
                let mut parts: Vec<String> = Vec::with_capacity(snapshot.len());
                for v in snapshot {
                    parts.push(if v.is_nullish() { String::new() } else { self.to_js_string(v)? });
                }
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            "at" => {
                // Negative index counts from the end; out of range → undefined.
                let i = self.to_integer_or_zero(arg0)?;
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let abs = if i < 0 { i + len as i64 } else { i };
                let v = if abs >= 0 && (abs as usize) < len {
                    match self.heap.get(idx) {
                        HeapObj::Array(items) => items[abs as usize],
                        _ => Value::UNDEFINED,
                    }
                } else {
                    Value::UNDEFINED
                };
                Ok(Some(v))
            }
            "indexOf" => {
                let snapshot = self.array_snapshot(idx);
                let len = snapshot.len() as i64;
                // Optional fromIndex (ToInteger; negative counts from the end).
                let from = if args.len() >= 2 {
                    let f = self.to_integer_or_zero(args[1])?;
                    if f < 0 { (len + f).max(0) } else { f.min(len) }
                } else {
                    0
                } as usize;
                let pos = (from..snapshot.len()).find(|&i| self.values_strict_eq(snapshot[i], arg0));
                Ok(Some(Value::int(pos.map(|p| p as i32).unwrap_or(-1))))
            }
            "includes" => {
                let snapshot = self.array_snapshot(idx);
                let found = snapshot.iter().any(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::bool(found)))
            }
            "lastIndexOf" => {
                let snapshot = self.array_snapshot(idx);
                let len = snapshot.len() as i64;
                // fromIndex defaults to len-1 (search from the end); negative
                // counts from the end. ToInteger.
                let from = if args.len() >= 2 {
                    let f = self.to_integer_or_zero(args[1])?;
                    if f < 0 { len + f } else { f.min(len - 1) }
                } else {
                    len - 1
                };
                let mut result = -1i32;
                if from >= 0 && !snapshot.is_empty() {
                    let hi = (from as usize).min(snapshot.len() - 1);
                    for i in (0..=hi).rev() {
                        if self.values_strict_eq(snapshot[i], arg0) {
                            result = i as i32;
                            break;
                        }
                    }
                }
                Ok(Some(Value::int(result)))
            }
            "reverse" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    items.reverse();
                }
                Ok(Some(Value::heap(idx))) // reverses in place, returns the array
            }
            "concat" => {
                // New array = `this` ++ each arg. An element is spread one level
                // iff IsConcatSpreadable (a `Symbol.isConcatSpreadable` flag, else
                // IsArray) — so an array-like with the flag spreads, and an array
                // with the flag cleared is added whole. Both `this` and the args
                // are subject to the check.
                let this_val = Value::heap(idx);
                let mut out: Vec<Value> = Vec::new();
                for e in std::iter::once(this_val).chain(args.iter().copied()) {
                    if self.is_concat_spreadable(e)? {
                        // A real array spreads via its dense storage (fast); any
                        // other spreadable (array-like) reads ToLength(length) and
                        // each index — coercion/getters here may throw, per spec.
                        if e.is_heap() && matches!(self.heap.get(e.heap_index()), HeapObj::Array(_)) {
                            let snap = self.array_snapshot(e.heap_index());
                            out.extend(snap);
                        } else {
                            let len_v = self.get_prop(e, "length")?;
                            let len = self.to_integer_or_zero(len_v)?.clamp(0, (1i64 << 53) - 1);
                            for k in 0..len {
                                let el = self.get_prop(e, &k.to_string())?;
                                out.push(el);
                            }
                        }
                    } else {
                        out.push(e);
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "flat" => {
                let depth = if args.is_empty() {
                    1
                } else {
                    // ToInteger (Infinity saturates to i64::MAX -> deep flatten).
                    let n = self.to_integer_or_zero(arg0)?;
                    if n < 0 {
                        0
                    } else {
                        n.min(i32::MAX as i64) as i32
                    }
                };
                let snapshot = self.array_snapshot(idx);
                let out = self.flatten_array(&snapshot, depth);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "fill" => {
                let val = arg0;
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let s0 = if args.len() >= 2 { self.to_integer_or_zero(args[1])? } else { 0 };
                let e0 = if args.len() >= 3 { self.to_integer_or_zero(args[2])? } else { len as i64 };
                let start = norm_index(s0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                let end = norm_index(e0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    let n = items.len() as i32; // re-clamp (a coercion valueOf may have resized)
                    for i in start..end.min(n) {
                        items[i as usize] = val;
                    }
                }
                Ok(Some(Value::heap(idx)))
            }
            "slice" => {
                let s0 = if args.is_empty() { 0 } else { self.to_integer_or_zero(arg0)? };
                let e0 = if args.len() < 2 { None } else { Some(self.to_integer_or_zero(args[1])?) };
                let snapshot = self.array_snapshot(idx);
                let len = snapshot.len() as i32;
                let start = norm_index(s0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                let end = match e0 {
                    None => len,
                    Some(e) => norm_index(e.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len),
                };
                let slice: Vec<Value> = if start < end {
                    snapshot[start as usize..end as usize].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(slice)))))
            }
            "map" => {
                self.array_each(idx, arg0, EachMode::Map, args.get(1).copied().unwrap_or(Value::UNDEFINED))
            }
            "filter" => {
                self.array_each(idx, arg0, EachMode::Filter, args.get(1).copied().unwrap_or(Value::UNDEFINED))
            }
            "forEach" => {
                self.array_each(idx, arg0, EachMode::ForEach, args.get(1).copied().unwrap_or(Value::UNDEFINED))
            }
            // Short-circuiting callback searches. They stop at the first match, so
            // they use call_value directly (the all-elements array_each driver
            // doesn't fit); the callback receives (element, index).
            "find" | "findIndex" | "some" | "every" => {
                let cb = arg0;
                // IsCallable(callback) is checked before any iteration, so an empty
                // array with a non-callable predicate still throws (spec step 3/4).
                if !self.is_callable(cb) {
                    return Err(Thrown(format!("TypeError: {name} predicate is not a function")));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let receiver = Value::heap(idx);
                let snapshot = self.array_snapshot(idx);
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, this_arg, &[*v, Value::int(i as i32), receiver])?;
                    let t = self.truthy(r);
                    match name {
                        "find" if t => return Ok(Some(*v)),
                        "findIndex" if t => return Ok(Some(Value::int(i as i32))),
                        "some" if t => return Ok(Some(Value::bool(true))),
                        "every" if !t => return Ok(Some(Value::bool(false))),
                        _ => {}
                    }
                }
                Ok(Some(match name {
                    "find" => Value::UNDEFINED,
                    "findIndex" => Value::int(-1),
                    "some" => Value::bool(false),
                    _ => Value::bool(true), // every: all matched (or empty)
                }))
            }
            "reduce" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown("TypeError: Reduce callback is not a function".into()));
                }
                let snapshot = self.array_snapshot(idx);
                let has_init = args.len() >= 2;
                // Seed + first index to process: with an initial value, start at
                // element 0; otherwise the first element seeds and we start at 1.
                let mut start = if has_init { 0 } else { 1 };
                let mut acc = if has_init {
                    args[1]
                } else if !snapshot.is_empty() {
                    snapshot[0]
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };

                // Fused native reduce kernel: inline the `(acc, element)`
                // callback into a native loop over the leading numeric run — no
                // per-element call. On a guard bail it returns the index reached
                // and the accumulated value (via the in/out acc pointer); the
                // per-element tail below finishes `[start, len)` correctly.
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                if self.jit_enabled
                    && self.jit_recurse_depth == 0
                    && cb.is_heap()
                    && start < snapshot.len()
                {
                    if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                        if ups.is_empty() {
                            let proto: *const crate::bytecode::FuncProto =
                                self.func(fid as usize);
                            // SAFETY: immutable program functions; raw ptr dodges
                            // the jit-vs-program borrow conflict (as elsewhere).
                            let proto_ref = unsafe { &*proto };
                            let reg_count = (proto_ref.reg_count as usize).max(3);
                            // The reduce kernel passes only (acc, element); a
                            // callback that declares the 3rd (index) / 4th (array)
                            // param or uses `arguments` must take the per-element
                            // path so those args are supplied.
                            let kernel_entry = if proto_ref.param_count >= 3
                                || proto_ref.arguments_reg.is_some()
                            {
                                None
                            } else {
                                self.jit.reduce_kernel(fid, proto_ref)
                            };
                            if let Some(entry) = kernel_entry {
                                let win = self.regs.len();
                                if !self.regs_would_overflow(win + reg_count) {
                                    self.regs.resize(win + reg_count, Value::UNDEFINED);
                                    let count = snapshot.len() - start;
                                    let window_ptr =
                                        unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                                    let snap_ptr =
                                        unsafe { snapshot.as_ptr().add(start) } as *const u64;
                                    let mut acc_bits = acc.bits();
                                    // SAFETY: valid win64 reduce kernel; window has
                                    // reg_count slots; acc_bits is a live u64;
                                    // call-free ⇒ none of these pointers move.
                                    let kernel: extern "win64" fn(
                                        *mut u64,
                                        *const u64,
                                        usize,
                                        *mut u64,
                                    ) -> usize = unsafe { core::mem::transmute(entry) };
                                    let processed =
                                        kernel(window_ptr, snap_ptr, count, &mut acc_bits as *mut u64);
                                    acc = Value::from_bits(acc_bits);
                                    self.regs.truncate(win);
                                    start += processed;
                                }
                            }
                        }
                    }
                }

                // Per-element tail: the whole array if no kernel ran, or just the
                // remainder after a kernel bail (nothing if it completed).
                let run_tail = start < snapshot.len();
                let mut native = if run_tail { self.native_cb_entry(cb) } else { None };
                let win = self.regs.len();
                if let Some((_, callee_regs, _)) = native {
                    if self.regs_would_overflow(win + callee_regs) {
                        native = None;
                    } else {
                        self.regs.resize(win + callee_regs, Value::UNDEFINED);
                    }
                }
                let mut err = None;
                let receiver = Value::heap(idx);
                for i in start..snapshot.len() {
                    let cbargs = [acc, snapshot[i], Value::int(i as i32), receiver];
                    match self.run_cb_elem(native, win, cb, &cbargs, Value::UNDEFINED) {
                        Ok(r) => acc = r,
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if native.is_some() {
                    self.regs.truncate(win);
                }
                if let Some(e) = err {
                    return Err(e);
                }
                Ok(Some(acc))
            }
            "sort" => {
                let cmp = arg0;
                // A non-undefined, non-callable comparator is a TypeError.
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: The comparison function must be either a function or undefined"
                            .into(),
                    ));
                }
                let mut snapshot = self.array_snapshot(idx);
                if self.is_callable(cmp) {
                    // Comparator sort: stable O(n log n) bottom-up merge sort,
                    // re-entering the VM for each comparison.
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    // Default sort: by string coercion (JS spec default).
                    snapshot.sort_by(|a, b| self.display(*a).cmp(&self.display(*b)));
                }
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    *items = snapshot;
                }
                Ok(Some(Value::heap(idx)))
            }
            "reduceRight" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown("TypeError: Reduce callback is not a function".into()));
                }
                let snapshot = self.array_snapshot(idx);
                let mut i = snapshot.len();
                let mut acc = if args.len() >= 2 {
                    args[1]
                } else if i > 0 {
                    i -= 1;
                    snapshot[i]
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };
                let receiver = Value::heap(idx);
                while i > 0 {
                    i -= 1;
                    acc = self.call_value(
                        cb,
                        Value::UNDEFINED,
                        &[acc, snapshot[i], Value::int(i as i32), receiver],
                    )?;
                }
                Ok(Some(acc))
            }
            "flatMap" => {
                // map(cb) then flatten one level (array results spliced in).
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown("TypeError: flatMap mapper is not a function".into()));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let receiver = Value::heap(idx);
                let snapshot = self.array_snapshot(idx);
                let mut out: Vec<Value> = Vec::new();
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, this_arg, &[*v, Value::int(i as i32), receiver])?;
                    if r.is_heap() {
                        if let HeapObj::Array(items) = self.heap.get(r.heap_index()) {
                            out.extend(items.iter().copied());
                            continue;
                        }
                    }
                    out.push(r);
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "findLast" | "findLastIndex" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown(format!("TypeError: {name} predicate is not a function")));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let receiver = Value::heap(idx);
                let snapshot = self.array_snapshot(idx);
                for i in (0..snapshot.len()).rev() {
                    let v = snapshot[i];
                    let r = self.call_value(cb, this_arg, &[v, Value::int(i as i32), receiver])?;
                    if self.truthy(r) {
                        return Ok(Some(if name == "findLast" {
                            v
                        } else {
                            Value::int(i as i32)
                        }));
                    }
                }
                Ok(Some(if name == "findLast" { Value::UNDEFINED } else { Value::int(-1) }))
            }
            "toSorted" => {
                // Like sort() but returns a NEW array; the receiver is unchanged.
                let cmp = arg0;
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: The comparison function must be either a function or undefined"
                            .into(),
                    ));
                }
                let mut snapshot = self.array_snapshot(idx);
                if self.is_callable(cmp) {
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    snapshot.sort_by(|a, b| self.display(*a).cmp(&self.display(*b)));
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "toReversed" => {
                let mut snapshot = self.array_snapshot(idx);
                snapshot.reverse();
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "splice" => {
                // splice(start, deleteCount?, ...items): mutate in place, return
                // the removed elements (start may be negative).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let s = self.to_integer_or_zero(arg0)?;
                let start = if s < 0 { (len as i64 + s).max(0) as usize } else { (s as usize).min(len) };
                // deleteCount: 0 args → 0; 1 arg → len-start; else ToInteger(arg1).
                let del = if args.is_empty() {
                    0
                } else if args.len() < 2 {
                    len - start
                } else {
                    let d = self.to_integer_or_zero(args[1])?;
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                let removed: Vec<Value> = match self.heap.get_mut(idx) {
                    HeapObj::Array(items) => {
                        // Re-clamp to the current length (a coercion valueOf may have resized).
                        let n = items.len();
                        let st = start.min(n);
                        let en = (start + del).min(n);
                        items.splice(st..en, insert).collect()
                    }
                    _ => Vec::new(),
                };
                self.heap.bump_version(idx); // length/contents changed
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(removed)))))
            }
            // Array iterators (real iterator objects with .next(), proto =
            // %ArrayIteratorPrototype%). values() is also the default @@iterator.
            "values" => {
                let items = self.array_snapshot(idx);
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "keys" => {
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let items: Vec<Value> = (0..len).map(|i| Value::int(i as i32)).collect();
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "entries" => {
                let snap = self.array_snapshot(idx);
                let items: Vec<Value> = snap
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![Value::int(i as i32), v]))))
                    .collect();
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "toLocaleString" => {
                // Join each element's own toLocaleString() with ","; nullish → "".
                let snapshot = self.array_snapshot(idx);
                let mut parts: Vec<String> = Vec::with_capacity(snapshot.len());
                for v in snapshot {
                    if v.is_nullish() {
                        parts.push(String::new());
                    } else {
                        let f = self.get_prop(v, "toLocaleString")?;
                        let s = if self.is_callable(f) {
                            let r = self.call_value(f, v, &[])?;
                            self.display(r)
                        } else {
                            self.display(v)
                        };
                        parts.push(s);
                    }
                }
                Ok(Some(self.alloc_str(parts.join(","))))
            }
            "with" => {
                // with(index, value): a COPY with one index replaced. The index is
                // relative (negative from the end) and NOT clamped — an out-of-range
                // index throws a RangeError.
                let mut out = self.array_snapshot(idx);
                let len = out.len() as i64;
                let n = self.to_number_coerce(arg0)?;
                let rel = if n.is_nan() { 0 } else { n.trunc() as i64 };
                let actual = if rel >= 0 { rel } else { len + rel };
                if actual < 0 || actual >= len {
                    return Err(Thrown("RangeError: Invalid index".into()));
                }
                out[actual as usize] = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "toSpliced" => {
                // Like splice() but returns the modified COPY; receiver unchanged.
                let mut out = self.array_snapshot(idx);
                let len = out.len();
                let s = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let start = if s < 0 { (len as i64 + s).max(0) as usize } else { (s as usize).min(len) };
                let del = if args.len() < 2 {
                    len - start
                } else {
                    let d = if args[1].is_number() { args[1].as_f64() as i64 } else { 0 };
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                out.splice(start..start + del, insert);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "copyWithin" => {
                // copyWithin(target, start, end?): copy the [start,end) slice over the
                // run beginning at target, in place. Reads from a snapshot so
                // overlapping ranges behave as if copied from the original.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let i32c = |n: i64| n.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                let t0 = self.to_integer_or_zero(arg0)?;
                let s0 = if args.len() >= 2 { self.to_integer_or_zero(args[1])? } else { 0 };
                let e0 = if args.len() >= 3 { self.to_integer_or_zero(args[2])? } else { len as i64 };
                let target = norm_index(i32c(t0), len);
                let start = norm_index(i32c(s0), len);
                let end = norm_index(i32c(e0), len);
                let count = (end - start).min(len - target).max(0);
                if count > 0 {
                    // A coerced arg's valueOf may have resized the array between
                    // capturing `len` and here, so guard every index against the
                    // CURRENT length (don't panic on a shrunk array).
                    let snapshot = self.array_snapshot(idx);
                    let snap_len = snapshot.len();
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        let cur = items.len();
                        for k in 0..count {
                            let (ti, si) = ((target + k) as usize, (start + k) as usize);
                            if ti < cur && si < snap_len {
                                items[ti] = snapshot[si];
                            }
                        }
                    }
                    self.heap.bump_version(idx);
                }
                Ok(Some(Value::heap(idx)))
            }
            _ => Ok(None),
        }
    }

    /// Stable bottom-up merge sort driven by a JS comparator (`cmp(a,b) < 0` ⇒
    /// `a` before `b`). O(n log n) comparisons — vs the old insertion sort's
    /// O(n²), which dominated `Array.sort` for non-trivial sizes. Stable: on a tie
    /// (and on `<= 0`) the LEFT run's element wins, preserving original order. The
    /// comparator re-enters the VM (`call_value`) and may throw (propagated).
    pub(crate) fn comparator_sort(&mut self, items: &mut [Value], cmp: Value) -> Result<(), Thrown> {
        let n = items.len();
        if n < 2 {
            return Ok(());
        }
        // Native-callback fast path: a compiled non-capturing comparator is called
        // directly over one reused register window (skipping a per-comparison frame
        // build + run_loop re-entry). `native = None` falls back to call_value.
        let mut native = self.native_cb_entry(cmp);
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None;
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }
        // Ping-pong between two local buffers (not self.regs/heap, so a comparator
        // that re-enters the VM and allocates can't invalidate them).
        let mut a: Vec<Value> = items.to_vec();
        let mut b: Vec<Value> = vec![Value::UNDEFINED; n];
        let mut width = 1;
        let mut err: Option<Thrown> = None;
        'outer: while width < n {
            let mut lo = 0;
            while lo < n {
                let mid = (lo + width).min(n);
                let hi = (lo + 2 * width).min(n);
                // Merge a[lo..mid] and a[mid..hi] into b[lo..hi], stably.
                let (mut l, mut r, mut k) = (lo, mid, lo);
                while l < mid && r < hi {
                    let c = match self.run_cb_elem(native, win, cmp, &[a[l], a[r]], Value::UNDEFINED) {
                        Ok(c) => c,
                        Err(e) => {
                            err = Some(e);
                            break 'outer;
                        }
                    };
                    if c.as_f64() <= 0.0 {
                        b[k] = a[l];
                        l += 1;
                    } else {
                        b[k] = a[r];
                        r += 1;
                    }
                    k += 1;
                }
                while l < mid {
                    b[k] = a[l];
                    l += 1;
                    k += 1;
                }
                while r < hi {
                    b[k] = a[r];
                    r += 1;
                    k += 1;
                }
                lo += 2 * width;
            }
            std::mem::swap(&mut a, &mut b);
            width *= 2;
        }
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        items.copy_from_slice(&a);
        Ok(())
    }

}
