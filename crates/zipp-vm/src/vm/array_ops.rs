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
            // Live read (the callback may have mutated this element or shortened the
            // array): a present index uses its current value; an index now past the
            // live length is absent — `map` keeps a placeholder so its result length
            // stays the original, `filter`/`forEach` skip it (HasProperty is false).
            let v = match self.array_dense_or_proto_get(idx, i)? {
                Some(v) => v,
                None => {
                    if matches!(mode, EachMode::Map) {
                        out.push(Value::UNDEFINED);
                    }
                    continue;
                }
            };
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
            // map does ArraySpeciesCreate(O, len) — `out.len()` is the source length
            // in the dense path; filter does ArraySpeciesCreate(O, 0).
            EachMode::Map => {
                let n = out.len();
                Ok(Some(self.array_from_species(receiver, out, n)?))
            }
            EachMode::Filter => Ok(Some(self.array_from_species(receiver, out, 0)?)),
        }
    }

    /// Allocate a built-in iterator over a snapshot of `items` with prototype `proto`.
    pub(crate) fn make_iterator(&mut self, items: Vec<Value>, proto: u32) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Iterator { items, index: 0, proto, live: None }))
    }

    /// Allocate a LIVE Map/Set iterator over the backing collection `coll` (its heap
    /// index) with prototype `proto`. `kind`: 0 = keys, 1 = values, 2 = entries. Each
    /// `.next()` steps the live collection (skipping deleted/tombstoned slots), so a
    /// delete/add performed after the iterator is created is reflected.
    pub(crate) fn make_live_iterator(&mut self, coll: u32, kind: u8, proto: u32) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Iterator {
            items: Vec::new(),
            index: 0,
            proto,
            live: Some((coll, kind)),
        }))
    }

    /// ArraySpeciesCreate(originalArray, length), but returns `None` to signal that
    /// the caller should keep its existing fast dense-array path — the constructor is
    /// the intrinsic `%Array%`, is absent/undefined, or carries no custom `@@species`.
    /// `Some(target)` is a species-constructed object the caller must populate with
    /// CreateDataPropertyOrThrow. A non-object (non-undefined) constructor, or a
    /// non-constructor `@@species`, throws a TypeError (matching the spec step
    /// "If IsConstructor(C) is false, throw a TypeError exception").
    pub(crate) fn array_species_create(
        &mut self,
        original: Value,
        len: usize,
    ) -> Result<Option<Value>, Thrown> {
        // ArraySpeciesCreate step 1-2: if IsArray(originalArray) is false, return
        // ArrayCreate(length) — `constructor`/`@@species` are NOT consulted for a
        // non-array receiver (e.g. `Array.prototype.map.call(typedArray | plainObj)`).
        if !self.value_is_array_throwing(original)? {
            return Ok(None);
        }
        let ctor = self.get_prop(original, "constructor")?;
        let species = if ctor == Value::UNDEFINED {
            return Ok(None);
        } else if !self.is_object_value(ctor) {
            // A non-object, non-undefined `constructor` can never be a constructor,
            // so ArraySpeciesCreate reaches the IsConstructor(C)-false throw.
            return Err(Thrown(
                "TypeError: Array species constructor is not an object".into(),
            ));
        } else {
            // ArraySpeciesCreate step 6.c.i: a constructor that is ANOTHER realm's
            // %Array% intrinsic is treated as undefined — so its @@species getter is
            // NOT consulted (cross-realm).
            if self.is_constructor(ctor)
                && self.get_function_realm(ctor) != 0
                && self.realm_ctor_main.get(&ctor.heap_index()) == Some(&self.array_ctor)
            {
                return Ok(None);
            }
            let s = self.get_prop(ctor, "@@species")?;
            if s == Value::NULL {
                Value::UNDEFINED
            } else {
                s
            }
        };
        if species == Value::UNDEFINED {
            return Ok(None);
        }
        // `%Array%` itself as the species is observably identical to ArrayCreate(len),
        // so keep the fast dense path (and avoid running the Array constructor).
        if species.is_heap() && self.array_ctor != 0 && species.heap_index() == self.array_ctor {
            return Ok(None);
        }
        if !self.is_constructor(species) {
            return Err(Thrown(
                "TypeError: Array species constructor is not a constructor".into(),
            ));
        }
        Ok(Some(self.construct(species, &[Value::num(len as f64)])?))
    }

    /// CreateDataPropertyOrThrow(O, ToString(index), value): install a fresh
    /// enumerable, writable, configurable data property (overwriting a configurable
    /// existing one), throwing a TypeError when the define fails — a non-extensible
    /// target, or a non-configurable existing property.
    pub(crate) fn create_data_property_or_throw(
        &mut self,
        target: Value,
        index: usize,
        value: Value,
    ) -> Result<(), Thrown> {
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::TRUE);
        m.set("enumerable", Value::TRUE);
        m.set("configurable", Value::TRUE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(m)));
        let key = index.to_string();
        self.object_define_property(target, &key, desc)
    }

    /// Finish a species-aware `Array.prototype` method: build the result array from
    /// `out`, honouring a custom `@@species` constructor. `species_len` is the length
    /// ArraySpeciesCreate is invoked with (0 for filter/concat/flat/flatMap, the
    /// source length for map, the element count for slice/splice). The common
    /// ordinary-array case takes the fast dense path unchanged; only a custom species
    /// constructs a target and receives each element via CreateDataPropertyOrThrow.
    /// GC is suspended for the scope so `out`'s values survive the species ctor call.
    pub(crate) fn array_from_species(
        &mut self,
        original: Value,
        out: Vec<Value>,
        species_len: usize,
    ) -> Result<Value, Thrown> {
        self.array_from_species_len(original, out, species_len, false)
    }

    /// `set_length`: slice/splice end with Set(A,'length',n,true) per spec;
    /// map/filter/flat/flatMap only define elements.
    pub(crate) fn array_from_species_len(
        &mut self,
        original: Value,
        out: Vec<Value>,
        species_len: usize,
        set_length: bool,
    ) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        match self.array_species_create(original, species_len)? {
            None => Ok(Value::heap(self.heap.alloc(HeapObj::Array(out)))),
            Some(target) => {
                let n = out.len();
                for (i, v) in out.into_iter().enumerate() {
                    // A HOLE marks an ABSENT source index: it is skipped (not
                    // defined as undefined) — the result keeps the gap.
                    if !v.is_hole() {
                        self.create_data_property_or_throw(target, i, v)?;
                    }
                }
                if set_length {
                    self.set_prop(target, "length", Value::num(n as f64), true)?;
                }
                Ok(target)
            }
        }
    }

    /// FlattenIntoArray (23.1.3.13.1): walk `source` per index with the spec
    /// HasProperty+Get protocol, applying `mapper` at the TOP level only
    /// (flatMap), spreading array elements (proxy-piercing IsArray) up to
    /// `depth` levels. Absent indices are SKIPPED (the mapper never runs on a
    /// hole and nothing is appended for it).
    fn flatten_into_array(
        &mut self,
        out: &mut Vec<Value>,
        source: Value,
        source_len: usize,
        depth: i64,
        mapper: Option<(Value, Value)>,
    ) -> Result<(), Thrown> {
        for k in 0..source_len {
            let Some(got) = self.array_iter_get(source, k)? else {
                continue;
            };
            let v = match mapper {
                Some((cb, ta)) => {
                    self.call_value(cb, ta, &[got, Value::num(k as f64), source])?
                }
                None => got,
            };
            if depth > 0 && self.value_is_array_throwing(v)? {
                let lv = self.get_prop(v, "length")?;
                let lf = self.to_number_coerce(lv)?;
                let n = if lf.is_nan() || lf <= 0.0 {
                    0usize
                } else {
                    (lf.trunc().min(9_007_199_254_740_991.0) as usize)
                        .min(crate::vm::MAX_DENSE_ARRAY_LEN)
                };
                self.flatten_into_array(out, v, n, depth - 1, None)?;
            } else {
                if out.len() >= crate::vm::MAX_DENSE_ARRAY_LEN {
                    return Err(Thrown(
                        "RangeError: array length exceeds the engine's dense-array limit"
                            .into(),
                    ));
                }
                out.push(v);
            }
        }
        Ok(())
    }

    /// Live read of index `k` for the iteration protocol: `Some(value)` if the
    /// index is PRESENT (HasProperty), `None` if absent (a hole / out of range).
    /// Re-reads the receiver each call so a mutation during a callback (a deleted
    /// index, a shrunk length, a changed element) is observed. The common case — a
    /// real array with no side table — reads the dense slot directly (no get_index/
    /// has_property dispatch), keeping the iterator methods at dense-snapshot speed;
    /// an array-like object, or an array with accessor/override indices, falls back
    /// to the general HasProperty + Get protocol (invoking inherited/accessor getters).
    pub(crate) fn array_iter_get(&mut self, this: Value, k: usize) -> Result<Option<Value>, Thrown> {
        // Fast path: only a PRESENT (non-hole, in-range) own element of a real array
        // with no side table. A hole or out-of-range index is NOT resolved here — it
        // falls through to the general HasProperty+Get protocol below, which walks the
        // prototype chain (a prototype-inherited index at a hole must still be visited).
        if this.is_heap() && !self.arr_props.contains_key(&this.heap_index()) {
            if let HeapObj::Array(items) = self.heap.get(this.heap_index()) {
                if let Some(v) = items.get(k) {
                    if !v.is_hole() {
                        return Ok(Some(*v));
                    }
                }
            }
        }
        let kv = Value::num(k as f64);
        // Proxy-aware HasProperty (a has trap must dispatch and may throw).
        if self.has_property_dyn(this, kv)? {
            Ok(Some(self.get_index(this, kv)?))
        } else {
            Ok(None)
        }
    }

    /// A snapshot that resolves every index through the `[[Get]]` protocol, so a hole
    /// reads its PROTOTYPE-inherited value (or `undefined` when truly absent) exactly as
    /// `fromValue = Get(O, from)` requires. The change-by-copy methods (toReversed /
    /// toSorted / toSpliced / with) build a dense result this way, so a hole over a
    /// `Array.prototype[k]` is not silently dropped to `undefined`. The fast path inside
    /// `array_iter_get` keeps a dense, side-table-free array at plain-snapshot speed; the
    /// length is read once up front (per LengthOfArrayLike) so a getter that mutates the
    /// array mid-read still yields exactly `len` elements.
    pub(crate) fn array_snapshot_get(&mut self, idx: u32) -> Result<Vec<Value>, Thrown> {
        let len = match self.heap.get(idx) {
            HeapObj::Array(items) => items.len(),
            _ => return Ok(Vec::new()),
        };
        let this = Value::heap(idx);
        let mut out = Vec::with_capacity(len);
        for k in 0..len {
            out.push(self.array_iter_get(this, k)?.unwrap_or(Value::UNDEFINED));
        }
        Ok(out)
    }

    /// Live per-index read for the DENSE callback arms (a real array known to have no
    /// side table at dispatch): a present (non-hole, in-range) element is returned
    /// directly — no per-element side-table lookup, so the hot path stays at snapshot
    /// speed — while a hole or out-of-range index defers to the proto-aware
    /// `array_iter_get` (which visits a prototype-inherited index). Re-reads the heap
    /// each call, so a callback's mid-iteration mutation (delete / length change) is
    /// observed.
    pub(crate) fn array_dense_or_proto_get(&mut self, idx: u32, i: usize) -> Result<Option<Value>, Thrown> {
        if let HeapObj::Array(items) = self.heap.get(idx) {
            if let Some(v) = items.get(i) {
                if !v.is_hole() {
                    return Ok(Some(*v));
                }
            }
        }
        self.array_iter_get(Value::heap(idx), i)
    }

    /// The hole-skipping iteration methods (forEach/map/filter/some/every/reduce/
    /// reduceRight) run against an array-like *object* OR a real array by visiting
    /// only indices where HasProperty is true (via `array_iter_get`) — unlike the
    /// dense-snapshot path, this honours absent indices (own or inherited holes) and
    /// observes mid-iteration mutation, per the spec.
    pub(crate) fn array_like_iterate(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        // O = ToObject(this value): a primitive receiver (e.g. a string passed via
        // `Array.prototype.forEach.call("abc", …)`) is boxed, so iteration reads the
        // wrapper's indexed properties AND the callback's 3rd argument is the object
        // (`obj instanceof String`), per every method's step 1.
        let this = self.to_object(this)?;
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_coerce(lv)?;
        // ToLength: a positive length (incl. +Infinity / "Infinity" / a huge finite)
        // clamps to MAX_DENSE_ARRAY_LEN; NaN and ≤0 (incl. -Infinity) → 0.
        let len: usize = if lenf > 0.0 {
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
                    if let Some(val) = self.array_iter_get(this, k)? {
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
                // ArrayCreate(len) requires len <= 2^32-1; a larger finite length OR a
                // non-finite one (Infinity, via ToLength → 2^53-1) is a RangeError.
                if lenf > 4_294_967_295.0 {
                    return Err(Thrown("RangeError: Invalid array length".into()));
                }
                let mut out = vec![Value::UNDEFINED; len];
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        out[k] = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                    }
                }
                Ok(Some(self.array_from_species(this, out, len)?))
            }
            "filter" => {
                let mut out = Vec::new();
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if self.truthy(r) {
                            out.push(val);
                        }
                    }
                }
                Ok(Some(self.array_from_species(this, out, 0)?))
            }
            "some" => {
                for k in 0..len {
                    if let Some(val) = self.array_iter_get(this, k)? {
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
                    if let Some(val) = self.array_iter_get(this, k)? {
                        let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                        if !self.truthy(r) {
                            return Ok(Some(Value::bool(false)));
                        }
                    }
                }
                Ok(Some(Value::bool(true)))
            }
            "find" | "findIndex" | "findLast" | "findLastIndex" => {
                // The find family visits EVERY index with Get (no HasProperty skip), so a
                // throwing index getter on an array-like propagates and an absent index is
                // undefined. find/findIndex go forward; findLast/findLastIndex backward.
                let backward = name == "findLast" || name == "findLastIndex";
                let order: Vec<usize> =
                    if backward { (0..len).rev().collect() } else { (0..len).collect() };
                for k in order {
                    let val = self.array_iter_get(this, k)?.unwrap_or(Value::UNDEFINED);
                    let r = self.call_value(cb, this_arg, &[val, idxv(k), this])?;
                    if self.truthy(r) {
                        return Ok(Some(match name {
                            "find" | "findLast" => val,
                            _ => idxv(k),
                        }));
                    }
                }
                Ok(Some(match name {
                    "find" | "findLast" => Value::UNDEFINED,
                    _ => Value::num(-1.0),
                }))
            }
            "reduce" | "reduceRight" => {
                let right = name == "reduceRight";
                let order: Vec<usize> =
                    if right { (0..len).rev().collect() } else { (0..len).collect() };
                let mut acc = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let mut started = args.len() >= 2;
                for k in order {
                    let val = match self.array_iter_get(this, k)? {
                        Some(v) => v,
                        None => continue,
                    };
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

    /// `indexOf` / `lastIndexOf` / `includes` over the generic [[Get]]/[[HasProperty]]
    /// protocol (ES 23.1.3.x), used whenever the receiver is an array-like object OR
    /// a real array carrying an `arr_props` side table (a defineProperty'd index
    /// accessor, or a prototype-inherited index). Unlike the dense snapshot fast path
    /// this: invokes accessor getters, walks the prototype chain, never materialises
    /// an absent index (HasProperty is consulted for indexOf/lastIndexOf), reads
    /// `length` live and coerces `fromIndex` AFTER it, and propagates a throwing index
    /// getter. `includes` reads EVERY index via Get (no HasProperty — a hole counts as
    /// undefined) and compares with SameValueZero; indexOf/lastIndexOf use HasProperty
    /// and strict equality.
    pub(crate) fn array_like_search(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let _gc = self.gc_lock_guard();
        let search = args.first().copied().unwrap_or(Value::UNDEFINED);
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_coerce(lv)?;
        // ToLength: clamp to 2^53-1 (NOT the dense-array ceiling) — search is per-index
        // via Get/HasProperty, so a fromIndex near a huge `length` reads only the few
        // indices in range (indexOf/lastIndexOf/includes on `{length: 2**53, ...}`).
        let len: i64 = if lenf > 0.0 {
            lenf.min(9_007_199_254_740_991.0) as i64
        } else {
            0
        };
        let idxv = |k: i64| Value::num(k as f64);
        if len == 0 {
            return Ok(Some(if name == "includes" {
                Value::bool(false)
            } else {
                Value::int(-1)
            }));
        }
        // fromIndex (ToIntegerOrInfinity), coerced AFTER reading length so its
        // valueOf side effects observe the current length.
        let has_from = args.len() >= 2;
        let from_raw = if has_from { self.to_integer_or_zero(args[1])? } else { 0 };
        match name {
            "lastIndexOf" => {
                // Default search start is len-1; n>=0 → min(n, len-1); n<0 → len+n.
                let mut k = if has_from {
                    if from_raw >= 0 { from_raw.min(len - 1) } else { len + from_raw }
                } else {
                    len - 1
                };
                while k >= 0 {
                    if self.has_property(this, idxv(k)) {
                        let v = self.get_index(this, idxv(k))?;
                        if self.values_strict_eq(v, search) {
                            return Ok(Some(Value::num(k as f64))); // index may exceed i32 (length up to 2^53-1)
                        }
                    }
                    k -= 1;
                }
                Ok(Some(Value::int(-1)))
            }
            // indexOf / includes share the forward start: n>=0 → n; n<0 → len+n (≥0).
            _ => {
                let mut k = if from_raw >= 0 { from_raw } else { (len + from_raw).max(0) };
                let is_includes = name == "includes";
                while k < len {
                    // includes visits every index (a hole reads as undefined);
                    // indexOf skips holes via HasProperty.
                    if is_includes {
                        let v = self.get_index(this, idxv(k))?;
                        if self.same_value_zero(v, search) {
                            return Ok(Some(Value::bool(true)));
                        }
                    } else if self.has_property(this, idxv(k)) {
                        let v = self.get_index(this, idxv(k))?;
                        if self.values_strict_eq(v, search) {
                            return Ok(Some(Value::num(k as f64))); // index may exceed i32 (length up to 2^53-1)
                        }
                    }
                    k += 1;
                }
                Ok(Some(if is_includes { Value::bool(false) } else { Value::int(-1) }))
            }
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
            // HasProperty must dispatch a Proxy `has` trap and propagate its abrupt
            // completion (the &self has_property swallows both).
            if self.has_property_dyn(this, fk)? {
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

    // ── generic (array-like) mutators: the abstract Get/Set/HasProperty/
    // DeletePropertyOrThrow + ToLength(length)/Set(length) protocol, so
    // `Array.prototype.<m>.call({0:…, length:n}, …)` mutates a plain object. Real
    // arrays use the dense fast paths in `array_method`; these run only for a
    // non-array `this`. ──

    /// ToLength(Get(O, "length")) — clamped to [0, 2^53-1].
    fn al_len(&mut self, this: Value) -> Result<i64, Thrown> {
        let lv = self.get_prop(this, "length")?;
        let lenf = self.to_number_coerce(lv)?;
        Ok(if lenf.is_nan() || lenf <= 0.0 {
            0
        } else {
            lenf.floor().min(9_007_199_254_740_991.0) as i64
        })
    }
    /// Set(O, "length", n, true).
    fn al_set_len(&mut self, this: Value, n: i64) -> Result<(), Thrown> {
        self.set_prop(this, "length", Value::num(n as f64), true)
    }
    /// HasProperty(O, i) — proxy-aware (dispatches a Proxy `has` trap).
    fn al_has(&mut self, this: Value, i: i64) -> Result<bool, Thrown> {
        self.has_property_dyn(this, Value::num(i as f64))
    }
    fn al_get(&mut self, this: Value, i: i64) -> Result<Value, Thrown> {
        self.get_index(this, Value::num(i as f64))
    }
    fn al_set(&mut self, this: Value, i: i64, v: Value) -> Result<(), Thrown> {
        self.set_index(this, Value::num(i as f64), v, true)
    }
    /// DeletePropertyOrThrow(O, i).
    fn al_del(&mut self, this: Value, i: i64) -> Result<(), Thrown> {
        let r = self.delete_property(this, &i.to_string())?;
        if !self.truthy(r) {
            return Err(Thrown(format!(
                "TypeError: Cannot delete property '{i}' of an array-like object"
            )));
        }
        Ok(())
    }

    pub(crate) fn array_like_mutate(
        &mut self,
        this: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        const MAX_SAFE: i64 = 9_007_199_254_740_991;
        let _gc = self.gc_lock_guard();
        let len = self.al_len(this)?;
        let r = match name {
            "push" => {
                let argc = args.len() as i64;
                if len + argc > MAX_SAFE {
                    return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                }
                let mut n = len;
                for &item in args {
                    self.al_set(this, n, item)?;
                    n += 1;
                }
                self.al_set_len(this, n)?;
                Value::num(n as f64)
            }
            "pop" => {
                if len == 0 {
                    self.al_set_len(this, 0)?;
                    Value::UNDEFINED
                } else {
                    let i = len - 1;
                    let el = self.al_get(this, i)?;
                    self.al_del(this, i)?;
                    self.al_set_len(this, i)?;
                    el
                }
            }
            "shift" => {
                if len == 0 {
                    self.al_set_len(this, 0)?;
                    Value::UNDEFINED
                } else {
                    let first = self.al_get(this, 0)?;
                    let mut k = 1;
                    while k < len {
                        if self.al_has(this, k)? {
                            let v = self.al_get(this, k)?;
                            self.al_set(this, k - 1, v)?;
                        } else {
                            self.al_del(this, k - 1)?;
                        }
                        k += 1;
                    }
                    self.al_del(this, len - 1)?;
                    self.al_set_len(this, len - 1)?;
                    first
                }
            }
            "unshift" => {
                let argc = args.len() as i64;
                if argc > 0 {
                    if len + argc > MAX_SAFE {
                        return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                    }
                    let mut k = len;
                    while k > 0 {
                        let from = k - 1;
                        let to = k + argc - 1;
                        if self.al_has(this, from)? {
                            let v = self.al_get(this, from)?;
                            self.al_set(this, to, v)?;
                        } else {
                            self.al_del(this, to)?;
                        }
                        k -= 1;
                    }
                    let mut j = 0i64;
                    for &item in args {
                        self.al_set(this, j, item)?;
                        j += 1;
                    }
                }
                let newlen = len + argc;
                self.al_set_len(this, newlen)?;
                Value::num(newlen as f64)
            }
            "reverse" => {
                let middle = len / 2;
                let mut lower = 0;
                while lower != middle {
                    let upper = len - lower - 1;
                    let lower_exists = self.al_has(this, lower)?;
                    let lower_val =
                        if lower_exists { self.al_get(this, lower)? } else { Value::UNDEFINED };
                    let upper_exists = self.al_has(this, upper)?;
                    let upper_val =
                        if upper_exists { self.al_get(this, upper)? } else { Value::UNDEFINED };
                    match (lower_exists, upper_exists) {
                        (true, true) => {
                            self.al_set(this, lower, upper_val)?;
                            self.al_set(this, upper, lower_val)?;
                        }
                        (false, true) => {
                            self.al_set(this, lower, upper_val)?;
                            self.al_del(this, upper)?;
                        }
                        (true, false) => {
                            self.al_del(this, lower)?;
                            self.al_set(this, upper, lower_val)?;
                        }
                        (false, false) => {}
                    }
                    lower += 1;
                }
                this
            }
            "splice" => {
                let relative_start =
                    self.to_integer_or_zero(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let actual_start = if relative_start < 0 {
                    len.saturating_add(relative_start).max(0)
                } else {
                    relative_start.min(len)
                };
                let (insert_count, actual_delete) = if args.is_empty() {
                    (0i64, 0i64)
                } else if args.len() == 1 {
                    (0, len - actual_start)
                } else {
                    let dc = self.to_integer_or_zero(args[1])?;
                    (args.len() as i64 - 2, dc.max(0).min(len - actual_start))
                };
                if len - actual_delete + insert_count > MAX_SAFE {
                    return Err(Thrown("TypeError: Array length exceeds the maximum".into()));
                }
                // Step 9: ArraySpeciesCreate(O, actualDeleteCount) runs BEFORE any
                // element read; the no-species ArrayCreate path rejects > 2^32-1
                // immediately (a 2^32-length receiver must not loop 4e9 reads).
                let species_target =
                    self.array_species_create(this, actual_delete.max(0) as usize)?;
                if species_target.is_none() && actual_delete > 4_294_967_295 {
                    return Err(Thrown("RangeError: Invalid array length".into()));
                }
                let a = match species_target {
                    Some(a) => {
                        // STREAM the deleted elements: Has/Get then DEFINE on A
                        // per element (absent indices stay absent on A).
                        let mut k = 0;
                        while k < actual_delete {
                            let from = actual_start + k;
                            if self.al_has(this, from)? {
                                let v = self.al_get(this, from)?;
                                self.create_data_property_or_throw(a, k as usize, v)?;
                            }
                            k += 1;
                        }
                        self.set_prop(
                            a,
                            "length",
                            Value::num(actual_delete.max(0) as f64),
                            true,
                        )?;
                        a
                    }
                    None => {
                        if actual_delete.max(0) as usize > crate::vm::MAX_DENSE_ARRAY_LEN {
                            return Err(Thrown(
                                "RangeError: array length exceeds the engine's dense-array limit"
                                    .into(),
                            ));
                        }
                        let mut deleted: Vec<Value> =
                            Vec::with_capacity((actual_delete.max(0) as usize).min(4096));
                        let mut k = 0;
                        while k < actual_delete {
                            let from = actual_start + k;
                            deleted.push(if self.al_has(this, from)? {
                                self.al_get(this, from)?
                            } else {
                                Value::HOLE
                            });
                            k += 1;
                        }
                        Value::heap(self.heap.alloc(HeapObj::Array(deleted)))
                    }
                };
                // Shift the tail to make room for the inserted items.
                if insert_count < actual_delete {
                    let mut k = actual_start;
                    while k < len - actual_delete {
                        let from = k + actual_delete;
                        let to = k + insert_count;
                        if self.al_has(this, from)? {
                            let v = self.al_get(this, from)?;
                            self.al_set(this, to, v)?;
                        } else {
                            self.al_del(this, to)?;
                        }
                        k += 1;
                    }
                    let mut k = len;
                    while k > len - actual_delete + insert_count {
                        self.al_del(this, k - 1)?;
                        k -= 1;
                    }
                } else if insert_count > actual_delete {
                    let mut k = len - actual_delete;
                    while k > actual_start {
                        let from = k + actual_delete - 1;
                        let to = k + insert_count - 1;
                        if self.al_has(this, from)? {
                            let v = self.al_get(this, from)?;
                            self.al_set(this, to, v)?;
                        } else {
                            self.al_del(this, to)?;
                        }
                        k -= 1;
                    }
                }
                // Insert the new items (args[2..]).
                let items: &[Value] = if args.len() > 2 { &args[2..] } else { &[] };
                let mut k = actual_start;
                for &item in items {
                    self.al_set(this, k, item)?;
                    k += 1;
                }
                self.al_set_len(this, len - actual_delete + insert_count)?;
                a
            }
            _ => return Ok(None),
        };
        Ok(Some(r))
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
            // The in-place mutators are generic over an array-like object: they
            // operate via the abstract ToLength(Get(O,"length")) + Get/Set/
            // HasProperty/DeletePropertyOrThrow + Set(O,"length",…) protocol, so a
            // plain `{0:…, length:n}` receiver is mutated correctly (real arrays use
            // the dense fast paths in the match below).
            if matches!(name, "pop" | "push" | "shift" | "unshift" | "reverse" | "splice") {
                return self.array_like_mutate(Value::heap(idx), name, args);
            }
            // indexOf/lastIndexOf/includes via the generic HasProperty/Get protocol —
            // invokes inherited/accessor getters, never materialises an absent index,
            // and propagates a throwing getter (a dense snapshot would do none of these).
            if matches!(name, "indexOf" | "lastIndexOf" | "includes") {
                return self.array_like_search(Value::heap(idx), name, args);
            }
            // The find family iterates via the generic Get protocol (every index visited,
            // accessor getters invoked, a throwing getter propagated) rather than
            // materialising a dense snapshot that swallows those side effects.
            if matches!(name, "find" | "findIndex" | "findLast" | "findLastIndex") {
                return self.array_like_iterate(Value::heap(idx), name, args);
            }
            // Read-only methods that treat a hole as undefined snapshot to a dense
            // temp array and run against that. (concat is NOT here: it must check
            // IsConcatSpreadable on the receiver itself — a non-array array-like is
            // appended WHOLE, not spread — so it runs on the object directly below.)
            // keys/values/entries on ANY receiver return a LIVE iterator over
            // the original object (the spec iterator re-reads length/elements
            // per step; a TypedArray receiver hits the live-TA next() branch,
            // which also throws its out-of-bounds TypeError per step).
            if matches!(name, "keys" | "values" | "entries") {
                let kind = match name {
                    "keys" => 0u8,
                    "values" => 1,
                    _ => 2,
                };
                return Ok(Some(self.make_live_iterator(idx, kind, self.array_iter_proto)));
            }
            if matches!(
                name,
                "join" | "toString" | "slice" | "at"
                    | "flat" | "flatMap" | "with" | "toReversed" | "toSorted"
                    | "toSpliced" | "toLocaleString"
            ) {
                // toSorted: IsCallable(comparefn) precedes ANY length / element read
                // (a non-callable comparator is a TypeError before the length getter).
                if name == "toSorted" {
                    let cmp = args.first().copied().unwrap_or(Value::UNDEFINED);
                    if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                        return Err(Thrown("TypeError: the comparator is not a function".into()));
                    }
                }
                // toReversed reads the live array-like in DESCENDING index order and
                // builds the reversed result directly (one length read, ArrayCreate
                // RangeError, then the descending element Gets — array_like_read reads
                // ascending, which is the wrong observable order here).
                if name == "toReversed" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_coerce(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0usize
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0) as usize
                    };
                    if len as f64 > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    let mut out = Vec::with_capacity(len);
                    for k in 0..len {
                        let v = self.get_index(Value::heap(idx), Value::num((len - 1 - k) as f64))?;
                        out.push(v);
                    }
                    return Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))));
                }
                // at: length is read ONCE, THEN the index argument is coerced (its
                // valueOf may mutate the receiver, e.g. shrink a resizable buffer),
                // then a single LIVE Get — never a snapshot of stale elements.
                if name == "at" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_coerce(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0.0
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0)
                    };
                    let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rel = self.to_number_coerce(a0)?;
                    let rel = if rel.is_nan() { 0.0 } else { rel.trunc() };
                    let k = if rel >= 0.0 { rel } else { len + rel };
                    if k < 0.0 || k >= len {
                        return Ok(Some(Value::UNDEFINED));
                    }
                    let v = self.get_index(Value::heap(idx), Value::num(k))?;
                    return Ok(Some(v));
                }
                // flat/flatMap run FlattenIntoArray against the ORIGINAL
                // receiver in spec order: length Get, (flatMap) mapper
                // IsCallable, (flat) depth coercion, ArraySpeciesCreate(O, 0)
                // — its constructor Get is observable — then the HasProperty+
                // Get walk (absent indices skipped; the mapper never runs on
                // a hole).
                if matches!(name, "flat" | "flatMap") {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lf = self.to_number_coerce(lv)?;
                    let source_len = if lf.is_nan() || lf <= 0.0 {
                        0usize
                    } else {
                        (lf.trunc().min(9_007_199_254_740_991.0) as usize)
                            .min(crate::vm::MAX_DENSE_ARRAY_LEN)
                    };
                    let (depth, mapper) = if name == "flatMap" {
                        if !self.is_callable(arg0) {
                            return Err(Thrown(
                                "TypeError: flatMap mapper is not a function".into(),
                            ));
                        }
                        (1i64, Some((arg0, args.get(1).copied().unwrap_or(Value::UNDEFINED))))
                    } else if args.is_empty() || arg0 == Value::UNDEFINED {
                        (1i64, None)
                    } else {
                        (self.to_integer_or_zero(arg0)?.max(0), None)
                    };
                    let target = self.array_species_create(Value::heap(idx), 0)?;
                    let mut out = Vec::new();
                    self.flatten_into_array(
                        &mut out,
                        Value::heap(idx),
                        source_len,
                        depth,
                        mapper,
                    )?;
                    return match target {
                        Some(a) => {
                            for (i, v) in out.into_iter().enumerate() {
                                self.create_data_property_or_throw(a, i, v)?;
                            }
                            Ok(Some(a))
                        }
                        None => Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out))))),
                    };
                }
                // join/toString/toLocaleString run LIVE against the receiver:
                // len = ToLength(Get(O,'length')) FIRST, then (join) the
                // separator coerces, then ONE Get per index — a separator
                // toString or an element toLocaleString that resizes the
                // receiver (resizable-buffer TA) is observed per element.
                if matches!(name, "join" | "toString" | "toLocaleString") {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_coerce(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0usize
                    } else {
                        (lenf.trunc().min(9_007_199_254_740_991.0) as usize)
                            .min(crate::vm::MAX_DENSE_ARRAY_LEN)
                    };
                    let sep = if name == "join" && arg0 != Value::UNDEFINED {
                        self.to_js_string(arg0)?
                    } else {
                        ",".to_string()
                    };
                    let mut parts: Vec<String> = Vec::with_capacity(len.min(4096));
                    for k in 0..len {
                        let v = self.get_index(Value::heap(idx), Value::num(k as f64))?;
                        if v.is_nullish() {
                            parts.push(String::new());
                        } else if name == "toLocaleString" {
                            let f = self.get_prop(v, "toLocaleString")?;
                            let s = if self.is_callable(f) {
                                let r = self.call_value(f, v, &[])?;
                                self.display(r)
                            } else {
                                self.display(v)
                            };
                            parts.push(s);
                        } else {
                            parts.push(self.to_js_string(v)?);
                        }
                    }
                    return Ok(Some(self.alloc_str(parts.join(&sep))));
                }
                // toSpliced runs the spec copy loops directly: a DISCARDED element
                // (actualStart..actualStart+actualDeleteCount) is never read — the
                // snapshot path would invoke its getter.
                if name == "toSpliced" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_coerce(lv)?;
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0i64
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0) as i64
                    };
                    let toii = |v: f64| if v.is_nan() { 0.0 } else { v.trunc() };
                    let (start, del) = if args.is_empty() {
                        (0i64, 0i64)
                    } else {
                        let s_raw = toii(self.to_number_coerce(args[0])?);
                        let s = if s_raw < 0.0 {
                            ((len as f64) + s_raw).max(0.0)
                        } else {
                            s_raw.min(len as f64)
                        } as i64;
                        let d = if args.len() < 2 {
                            len - s
                        } else {
                            let d_raw = toii(self.to_number_coerce(args[1])?);
                            (d_raw.max(0.0) as i64).min(len - s)
                        };
                        (s, d)
                    };
                    let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                    let new_len = len - del + insert.len() as i64;
                    // Step 12: newLen > 2^53-1 is a TypeError; step 13 ArrayCreate
                    // rejects > 2^32-1 with a RangeError.
                    if new_len > 9_007_199_254_740_991 {
                        return Err(Thrown(
                            "TypeError: Array length exceeds the maximum".into(),
                        ));
                    }
                    if new_len > 4_294_967_295 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    let mut out = Vec::with_capacity((new_len.max(0) as usize).min(4096));
                    for k in 0..start {
                        out.push(self.get_index(Value::heap(idx), Value::num(k as f64))?);
                    }
                    out.extend(insert);
                    let mut r = start + del;
                    while (out.len() as i64) < new_len {
                        out.push(self.get_index(Value::heap(idx), Value::num(r as f64))?);
                        r += 1;
                    }
                    return Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))));
                }
                // with/toSorted build a result of the source length via
                // ArrayCreate(len), which throws RangeError for len > 2^32-1 — BEFORE
                // reading any element (a throwing index getter must not run first).
                if matches!(name, "with" | "toSorted") {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let n = self.to_number_coerce(lv)?;
                    // ArrayCreate(len) requires len <= 2^32-1; a larger finite length OR
                    // a non-finite one (Infinity, via ToLength → 2^53-1) is a RangeError.
                    if n > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                }
                // slice runs the spec directly: length is read ONCE, start/end
                // coerce ONCE, ArraySpeciesCreate(O, count) uses the ORIGINAL
                // receiver (count <= 2^32-1 validated BEFORE any element read),
                // then live per-index HasProperty+Get (proxy/TA-correct).
                if name == "slice" {
                    let lv = self.get_prop(Value::heap(idx), "length")?;
                    let lenf = self.to_number_coerce(lv)?;
                    // ToLength(lenf) → clamp to [0, 2^53-1].
                    let len = if lenf.is_nan() || lenf <= 0.0 {
                        0.0
                    } else {
                        lenf.trunc().min(9_007_199_254_740_991.0)
                    };
                    // relativeStart/relativeEnd = ToIntegerOrInfinity(arg) (Infinity-aware).
                    let toii = |raw: f64| if raw.is_nan() { 0.0 } else { raw.trunc() };
                    let s_arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rel_start = toii(self.to_number_coerce(s_arg)?);
                    let k0 = if rel_start < 0.0 { (len + rel_start).max(0.0) } else { rel_start.min(len) };
                    let e_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    let rel_end =
                        if e_arg == Value::UNDEFINED { len } else { toii(self.to_number_coerce(e_arg)?) };
                    let fin = if rel_end < 0.0 { (len + rel_end).max(0.0) } else { rel_end.min(len) };
                    let count = (fin - k0).max(0.0);
                    if count > 4_294_967_295.0 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    let target = self.array_species_create(Value::heap(idx), count as usize)?;
                    return match target {
                        Some(a) => {
                            let mut n = 0usize;
                            let mut kf = k0;
                            while kf < fin {
                                if let Some(v) =
                                    self.array_iter_get(Value::heap(idx), kf as usize)?
                                {
                                    self.create_data_property_or_throw(a, n, v)?;
                                }
                                n += 1;
                                kf += 1.0;
                            }
                            self.set_prop(a, "length", Value::num(n as f64), true)?;
                            Ok(Some(a))
                        }
                        None => {
                            if count as usize > crate::vm::MAX_DENSE_ARRAY_LEN {
                                return Err(Thrown(
                                    "RangeError: array length exceeds the engine's dense-array limit"
                                        .into(),
                                ));
                            }
                            let mut out = Vec::with_capacity((count as usize).min(4096));
                            let mut kf = k0;
                            while kf < fin {
                                match self.array_iter_get(Value::heap(idx), kf as usize)? {
                                    Some(v) => out.push(v),
                                    None => out.push(Value::HOLE),
                                }
                                kf += 1.0;
                            }
                            Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
                        }
                    };
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
        // A side table (defineProperty'd index accessor) OR a HOLE makes the dense
        // placeholder unreliable: route the callback methods to the live HasProperty+
        // Get protocol (skips absent indices, invokes accessor getters). A hole-free,
        // side-table-free array keeps the fast dense path below — whose general
        // (non-native) JS-callback branch reads each element live, so a callback's
        // mid-iteration mutation is still observed; only the non-mutating native
        // numeric kernel snapshots.
        if (self.arr_props.contains_key(&idx) || self.array_has_holes(idx))
            && matches!(
                name,
                "map" | "filter" | "forEach" | "every" | "some" | "reduce" | "reduceRight"
            )
        {
            return self.array_like_iterate(Value::heap(idx), name, args);
        }
        // Likewise route the SEARCH methods off the dense fast path when the array
        // carries a side table (a defineProperty'd index accessor must have its getter
        // invoked) OR has holes (indexOf/lastIndexOf skip holes via HasProperty).
        if (self.arr_props.contains_key(&idx) || self.array_has_holes(idx))
            && matches!(name, "indexOf" | "lastIndexOf" | "includes")
        {
            return self.array_like_search(Value::heap(idx), name, args);
        }
        // push/pop/shift/unshift/splice end with Set(O,"length",…,true); on a FROZEN
        // array `length` is non-writable, so they throw a TypeError — even when no
        // element changes (pop/shift on an empty array, push/unshift with no args,
        // splice() with no args still set `length`). (A SEALED-but-not-frozen array
        // keeps `length` writable, so it is not gated here; its add/delete failures
        // are a separate concern.)
        if matches!(name, "push" | "pop" | "shift" | "unshift" | "splice")
            && (self.arr_props.get(&idx).map_or(false, |m| m.is_frozen())
                || self.array_length_nonwritable.contains(&idx))
        {
            return Err(Thrown(
                "TypeError: Cannot assign to read only property 'length' of object '[object Array]'".into(),
            ));
        }
        // pop/shift read an element via the spec Get. When that element is a HOLE in
        // the array's own storage, Get defers to the prototype chain — a prototype
        // accessor there can run arbitrary code (e.g. freeze the array mid-operation),
        // which the fast Vec path would miss. Route such cases to the abstract path.
        if name == "pop" || name == "shift" {
            if let HeapObj::Array(items) = self.heap.get(idx) {
                let probe = if name == "pop" { items.len().checked_sub(1) } else { Some(0) };
                if probe.map_or(false, |p| items.get(p).is_some_and(|v| v.is_hole())) {
                    return self.array_like_mutate(Value::heap(idx), name, args);
                }
            }
        }
        // shift/reverse mutate via the raw Vec — correct only when every slot is
        // a plain own data element. A side table (accessor/attribute overrides),
        // or holes that an inherited prototype index could cover, must run the
        // spec HasProperty/Get/Set/Delete protocol (same gating as the callback
        // and search families above).
        if matches!(name, "shift" | "reverse")
            && (self.arr_props.contains_key(&idx)
                || (self.array_has_holes(idx)
                    && (self.array_proto_has_index || self.proto_of.contains_key(&idx))))
        {
            return self.array_like_mutate(Value::heap(idx), name, args);
        }
        // push/unshift Set a NEW index; when a prototype carries integer indices, that
        // Set may hit a prototype setter (OrdinarySet, handled by set_index via the
        // abstract al_set path). The fast Vec append below bypasses set_index, so route
        // to the abstract path then. Gated on the flag, so the common fast path stands.
        if (name == "push" || name == "unshift") && self.array_proto_has_index {
            return self.array_like_mutate(Value::heap(idx), name, args);
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
                // A TypedArray receiver (TypedArray.prototype.toString IS
                // Array.prototype.toString) goes through this.join, i.e.
                // %TypedArray%.prototype.join, whose ValidateTypedArray rejects a
                // detached/out-of-bounds view before any element reads.
                if matches!(self.heap.get(idx), HeapObj::TypedArray { .. })
                    && self.ta_effective_len(idx).is_none()
                {
                    return Err(Thrown(
                        "TypeError: TypedArray is detached or out of bounds".into(),
                    ));
                }
                // ToString the separator (undefined -> ","), and ToString each
                // element — invoking a custom `toString`/`@@toPrimitive`, not the
                // infallible `display`. (to_js_string short-circuits primitives to
                // `display`, so a numeric/string array join stays on the fast path.)
                let sep = if name == "toString" || arg0 == Value::UNDEFINED {
                    ",".to_string()
                } else {
                    self.to_js_string(arg0)?
                };
                // Non-clean (side table / holes): per-index proto-aware Get —
                // an inherited Array.prototype[k] at a hole joins its VALUE.
                let snapshot = if self.arr_props.contains_key(&idx) || self.array_has_holes(idx)
                {
                    self.array_snapshot_get(idx)?
                } else {
                    self.array_snapshot(idx)
                };
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
                        // A hole reads as undefined (never leak the sentinel).
                        HeapObj::Array(items) => {
                            let el = items[abs as usize];
                            if el.is_hole() { Value::UNDEFINED } else { el }
                        }
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
                // len === 0 short-circuits to -1 BEFORE ToIntegerOrInfinity(fromIndex),
                // so a throwing fromIndex.valueOf must not run (spec step 2).
                if len == 0 {
                    return Ok(Some(Value::int(-1)));
                }
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
                let len = snapshot.len() as i64;
                // len === 0 short-circuits to false BEFORE ToIntegerOrInfinity(fromIndex),
                // so a throwing fromIndex.valueOf must not run (spec step 2).
                if len == 0 {
                    return Ok(Some(Value::bool(false)));
                }
                // fromIndex (ToIntegerOrInfinity): negative counts from the end
                // (clamped to 0); +Infinity → past the end (never found); -Infinity → 0.
                let from = if args.len() >= 2 {
                    let n = self.to_integer_or_zero(args[1])?;
                    if n >= 0 { n } else { len.saturating_add(n).max(0) }
                } else {
                    0
                };
                // SameValueZero (NaN matches NaN; +0/-0 equal) — not strict `===`.
                let mut found = false;
                let mut k = from;
                while k < len {
                    if self.same_value_zero(snapshot[k as usize], arg0) {
                        found = true;
                        break;
                    }
                    k += 1;
                }
                Ok(Some(Value::bool(found)))
            }
            "lastIndexOf" => {
                let snapshot = self.array_snapshot(idx);
                let len = snapshot.len() as i64;
                // len === 0 short-circuits to -1 BEFORE ToIntegerOrInfinity(fromIndex),
                // so a throwing fromIndex.valueOf must not run (spec step 2).
                if len == 0 {
                    return Ok(Some(Value::int(-1)));
                }
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
                // ArraySpeciesCreate(O, 0) is step 2: its constructor/@@species
                // Gets precede every @@isConcatSpreadable Get below.
                let species_target = self.array_species_create(this_val, 0)?;
                let mut out: Vec<Value> = Vec::new();
                for e in std::iter::once(this_val).chain(args.iter().copied()) {
                    if self.is_concat_spreadable(e)? {
                        // A CLEAN real array spreads via its dense storage (fast).
                        // One with a side table (accessors), an arguments object,
                        // or holes runs the spec HasProperty+Get per index —
                        // accessors fire and ABSENT indices stay absent (HOLE).
                        let arr_n = if e.is_heap() {
                            match self.heap.get(e.heap_index()) {
                                HeapObj::Array(items) => Some(items.len()),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        if let Some(n) = arr_n {
                            let eidx = e.heap_index();
                            if !self.arr_props.contains_key(&eidx)
                                && !self.arguments_objs.contains(&eidx)
                                && !self.array_has_holes(eidx)
                            {
                                let snap = self.array_snapshot(eidx);
                                out.extend(snap);
                            } else {
                                for k in 0..n {
                                    match self.array_iter_get(e, k)? {
                                        Some(v) => out.push(v),
                                        None => out.push(Value::HOLE),
                                    }
                                }
                            }
                        } else {
                            let len_v = self.get_prop(e, "length")?;
                            let len = self.to_integer_or_zero(len_v)?.clamp(0, (1i64 << 53) - 1);
                            // Step 5.c.iii: n + len > 2^53-1 is a TypeError BEFORE
                            // any element read (a MAX_SAFE_INTEGER-length spreadable
                            // must not loop 9e15 Gets).
                            if out.len() as i64 + len > (1i64 << 53) - 1 {
                                return Err(Thrown(
                                    "TypeError: concat result length exceeds 2**53 - 1".into(),
                                ));
                            }
                            for k in 0..len {
                                let el = self.get_prop(e, &k.to_string())?;
                                out.push(el);
                            }
                        }
                    } else {
                        out.push(e);
                    }
                }
                match species_target {
                    None => Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out))))),
                    Some(a) => {
                        let n = out.len();
                        for (i, v) in out.into_iter().enumerate() {
                            if !v.is_hole() {
                                self.create_data_property_or_throw(a, i, v)?;
                            }
                        }
                        self.set_prop(a, "length", Value::num(n as f64), true)?;
                        Ok(Some(a))
                    }
                }
            }
            "flat" => {
                // An absent OR explicitly-`undefined` depth defaults to 1
                // (ToIntegerOrInfinity is only applied to a provided depth).
                let depth = if args.is_empty() || arg0 == Value::UNDEFINED {
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
                // flat builds the result via ArraySpeciesCreate(O, 0).
                Ok(Some(self.array_from_species(Value::heap(idx), out, 0)?))
            }
            "fill" => {
                let val = arg0;
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let s0 = if args.len() >= 2 { self.to_integer_or_zero(args[1])? } else { 0 };
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
                    self.to_integer_or_zero(args[2])?
                } else {
                    len as i64
                };
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
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    None
                } else {
                    Some(self.to_integer_or_zero(args[1])?)
                };
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let start = norm_index(s0.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len);
                let end = match e0 {
                    None => len,
                    Some(e) => norm_index(e.clamp(i32::MIN as i64, i32::MAX as i64) as i32, len),
                };
                let clean =
                    !self.arr_props.contains_key(&idx) && !self.array_has_holes(idx);
                let slice: Vec<Value> = if start < end {
                    if clean {
                        match self.heap.get(idx) {
                            HeapObj::Array(items) => {
                                items[start as usize..end as usize].to_vec()
                            }
                            _ => Vec::new(),
                        }
                    } else {
                        // Spec copy: HasProperty(k) (proto-aware, accessors fire)
                        // then Get; an ABSENT index stays absent in the result.
                        let mut v = Vec::with_capacity((end - start) as usize);
                        for k in start..end {
                            match self.array_iter_get(Value::heap(idx), k as usize)? {
                                Some(x) => v.push(x),
                                None => v.push(Value::HOLE),
                            }
                        }
                        v
                    }
                } else {
                    Vec::new()
                };
                // slice does ArraySpeciesCreate(O, count) where count == slice.len().
                let n = slice.len();
                Ok(Some(self.array_from_species_len(Value::heap(idx), slice, n, true)?))
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
                // `len` is captured once; each element is read LIVE (a callback may
                // mutate it or shorten the array). For an index now past the live
                // length: `find`/`findIndex` still visit it with `undefined` (they do
                // not HasProperty-skip), while `some`/`every` skip it.
                let len = self.array_snapshot(idx).len();
                for i in 0..len {
                    let v = match self.array_dense_or_proto_get(idx, i)? {
                        Some(v) => v,
                        None => {
                            if name == "some" || name == "every" {
                                continue;
                            }
                            Value::UNDEFINED
                        }
                    };
                    let r = self.call_value(cb, this_arg, &[v, Value::int(i as i32), receiver])?;
                    let t = self.truthy(r);
                    match name {
                        "find" if t => return Ok(Some(v)),
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
                    // Live read; skip an index now past the live length (the callback
                    // shortened the array) — reduce HasProperty-skips absent indices.
                    let v = match self.array_dense_or_proto_get(idx, i)? {
                        Some(v) => v,
                        None => continue,
                    };
                    let cbargs = [acc, v, Value::int(i as i32), receiver];
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
                let receiver = Value::heap(idx);
                // Fast path: a plain dense array (no side table, no holes) — every
                // index 0..len is an own present element, so the raw backing slice is
                // observably identical to the [[Get]]/[[Set]] protocol. Keeps the hot
                // path at snapshot speed.
                let fast = match self.heap.get(idx) {
                    HeapObj::Array(items) => {
                        !self.arr_props.contains_key(&idx) && items.iter().all(|v| !v.is_hole())
                    }
                    _ => false,
                };
                if fast {
                    let mut snapshot = match self.heap.get(idx) {
                        HeapObj::Array(items) => items.clone(),
                        _ => Vec::new(),
                    };
                    self.sort_values(&mut snapshot, cmp)?;
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        *items = snapshot;
                    }
                    return Ok(Some(receiver));
                }
                // SortIndexedProperties via the [[Get]]/[[Set]]/[[Delete]] protocol:
                // own/inherited accessor INDICES fire their getters/setters, holes read
                // their prototype value, and a getter that mutates the array mid-sort is
                // observed. `len` is read ONCE up front (LengthOfArrayLike).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => {
                        let lv = self.get_prop(receiver, "length")?;
                        let n = self.to_number_coerce(lv)?;
                        if n.is_nan() || n <= 0.0 {
                            0
                        } else {
                            n.min((u32::MAX as f64) - 1.0) as usize
                        }
                    }
                };
                let mut gathered = Vec::new();
                for i in 0..len {
                    // array_iter_get = ? HasProperty(O,i) ? ? Get(O,i) : skip.
                    if let Some(v) = self.array_iter_get(receiver, i)? {
                        gathered.push(v);
                    }
                }
                let item_count = gathered.len();
                self.sort_values(&mut gathered, cmp)?;
                for (j, v) in gathered.into_iter().enumerate() {
                    self.set_index(receiver, Value::num(j as f64), v, true)?;
                }
                for j in item_count..len {
                    self.delete_property(receiver, &j.to_string())?;
                }
                Ok(Some(receiver))
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
                    // Live read; skip an index now past the live length (a callback
                    // shortened the array) — reduceRight HasProperty-skips absent ones.
                    let v = match self.array_dense_or_proto_get(idx, i)? {
                        Some(v) => v,
                        None => continue,
                    };
                    acc = self.call_value(
                        cb,
                        Value::UNDEFINED,
                        &[acc, v, Value::int(i as i32), receiver],
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
                // flatMap builds the result via ArraySpeciesCreate(O, 0).
                Ok(Some(self.array_from_species(receiver, out, 0)?))
            }
            "findLast" | "findLastIndex" => {
                let cb = arg0;
                if !self.is_callable(cb) {
                    return Err(Thrown(format!("TypeError: {name} predicate is not a function")));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let receiver = Value::heap(idx);
                // `len` is captured once; each element is read LIVE (a callback may mutate
                // the array). findLast/findLastIndex visit EVERY index (no HasProperty
                // skip), so an absent/hole index is the inherited value or undefined.
                let len = self.array_snapshot(idx).len();
                for i in (0..len).rev() {
                    let v = self.array_dense_or_proto_get(idx, i)?.unwrap_or(Value::UNDEFINED);
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
                let mut snapshot = self.array_snapshot_get(idx)?;
                if self.is_callable(cmp) {
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    // Default SortCompare: ToString each element (undefined last).
                    let mut keyed: Vec<(Option<String>, Value)> =
                        Vec::with_capacity(snapshot.len());
                    for v in std::mem::take(&mut snapshot) {
                        let key = if v == Value::UNDEFINED { None } else { Some(self.to_js_string(v)?) };
                        keyed.push((key, v));
                    }
                    keyed.sort_by(|(ka, _), (kb, _)| match (ka, kb) {
                        (Some(a), Some(b)) => a.cmp(b),
                        (None, None) => std::cmp::Ordering::Equal,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (Some(_), None) => std::cmp::Ordering::Less,
                    });
                    snapshot = keyed.into_iter().map(|(_, v)| v).collect();
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "toReversed" => {
                // Read in SPEC order: out[k] = Get(O, len-k-1). A snapshot-then-reverse
                // would read indices ascending, but a getter's side effect (e.g. it
                // shrinks the array) makes the read order observable, so the descending
                // `from` sequence must be honoured.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let this = Value::heap(idx);
                let mut out = Vec::with_capacity(len);
                for k in 0..len {
                    let from = len - k - 1;
                    out.push(self.array_iter_get(this, from)?.unwrap_or(Value::UNDEFINED));
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
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
                // splice returns the removed elements via
                // ArraySpeciesCreate(O, actualDeleteCount).
                let n = removed.len();
                Ok(Some(self.array_from_species_len(Value::heap(idx), removed, n, true)?))
            }
            // Array iterators (real iterator objects with .next(), proto =
            // %ArrayIteratorPrototype%). values() is also the default @@iterator.
            // LIVE: each next() re-reads the array, so mutations made during
            // iteration are observed (the spec iterator is a generator over O).
            "values" => Ok(Some(self.make_live_iterator(idx, 1, self.array_iter_proto))),
            "keys" => Ok(Some(self.make_live_iterator(idx, 0, self.array_iter_proto))),
            "entries" => {
                Ok(Some(self.make_live_iterator(idx, 2, self.array_iter_proto)))
            }
            "toLocaleString" => {
                // Join each element's own toLocaleString() with ","; nullish → "".
                let snapshot = if self.arr_props.contains_key(&idx) || self.array_has_holes(idx)
                {
                    self.array_snapshot_get(idx)?
                } else {
                    self.array_snapshot(idx)
                };
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
                // index throws a RangeError. Per spec the replaced index is set to
                // `value` WITHOUT a [[Get]]; every OTHER index is read via Get (so an
                // inherited `Array.prototype[k]` at a hole is visited, but the replaced
                // slot's getter is NOT invoked).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                } as i64;
                let n = self.to_number_coerce(arg0)?;
                let rel = if n.is_nan() { 0 } else { n.trunc() as i64 };
                let actual = if rel >= 0 { rel } else { len + rel };
                if actual < 0 || actual >= len {
                    return Err(Thrown("RangeError: Invalid index".into()));
                }
                let value = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let this = Value::heap(idx);
                let mut out = Vec::with_capacity(len as usize);
                for k in 0..len as usize {
                    if k as i64 == actual {
                        out.push(value);
                    } else {
                        out.push(self.array_iter_get(this, k)?.unwrap_or(Value::UNDEFINED));
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "toSpliced" => {
                // Like splice() but returns the modified COPY; receiver unchanged.
                let mut out = self.array_snapshot_get(idx)?;
                let len = out.len();
                let s = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let start = if s < 0 { (len as i64 + s).max(0) as usize } else { (s as usize).min(len) };
                let del = if args.is_empty() {
                    // No start argument: skipCount/actualSkipCount are 0 — the
                    // result is an unchanged copy, NOT a delete-everything.
                    0
                } else if args.len() < 2 {
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
                // A prototype index / accessor side table makes the per-index
                // Has/Get/Set/Delete protocol observable — route abstract.
                if self.arr_props.contains_key(&idx)
                    || self.array_proto_has_index
                    || self.proto_of.contains_key(&idx)
                {
                    return self.array_like_copy_within(Value::heap(idx), args);
                }
                // copyWithin(target, start, end?): copy the [start,end) slice over the
                // run beginning at target, in place. Reads from a raw snapshot
                // (HOLEs preserved) so overlapping ranges behave as if copied
                // from the original; a hole copies as a hole (delete).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let i32c = |n: i64| n.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                let t0 = self.to_integer_or_zero(arg0)?;
                let s0 = if args.len() >= 2 { self.to_integer_or_zero(args[1])? } else { 0 };
                // An absent OR explicitly-`undefined` end defaults to the length.
                let e0 = if args.len() >= 3 && args[2] != Value::UNDEFINED {
                    self.to_integer_or_zero(args[2])?
                } else {
                    len as i64
                };
                let target = norm_index(i32c(t0), len);
                let start = norm_index(i32c(s0), len);
                let end = norm_index(i32c(e0), len);
                let count = (end - start).min(len - target).max(0);
                if count > 0 {
                    // A coerced arg's valueOf may have resized the array between
                    // capturing `len` and here: guard targets against the CURRENT
                    // length, and a now-out-of-range SOURCE deletes its target
                    // (HasProperty false → DeletePropertyOrThrow, = HOLE here).
                    let raw: Vec<Value> = match self.heap.get(idx) {
                        HeapObj::Array(items) => items.clone(),
                        _ => Vec::new(),
                    };
                    let snap_len = raw.len();
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        for k in 0..count {
                            let (ti, si) = ((target + k) as usize, (start + k) as usize);
                            if si < snap_len {
                                // Set(O, to, v): grows past a shrunk length.
                                if ti >= items.len() {
                                    items.resize(ti + 1, Value::HOLE);
                                }
                                items[ti] = raw[si];
                            } else if ti < items.len() {
                                // Absent source → DeletePropertyOrThrow(to).
                                items[ti] = Value::HOLE;
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

    /// SortIndexedProperties' compare step over the gathered present values: the user
    /// comparator if callable (a throwing compare propagates), else the default sort
    /// (SortCompare): ToString each element by code units, `undefined` last. Default
    /// keys are precomputed because ToString runs JS (which can't happen inside the
    /// comparator).
    fn sort_values(&mut self, items: &mut Vec<Value>, cmp: Value) -> Result<(), Thrown> {
        if self.is_callable(cmp) {
            // SortCompare always orders `undefined` elements AFTER every defined
            // value and NEVER passes them to the comparator. Partition them out,
            // sort the rest, then re-append (the default-comparator branch below
            // does the equivalent via its None-is-Greater key ordering).
            let undef = items.iter().filter(|&&v| v == Value::UNDEFINED).count();
            if undef > 0 {
                items.retain(|&v| v != Value::UNDEFINED);
            }
            self.comparator_sort(items, cmp)?;
            for _ in 0..undef {
                items.push(Value::UNDEFINED);
            }
        } else {
            let mut keyed: Vec<(Option<String>, Value)> = Vec::with_capacity(items.len());
            for v in std::mem::take(items) {
                let key = if v == Value::UNDEFINED { None } else { Some(self.to_js_string(v)?) };
                keyed.push((key, v));
            }
            keyed.sort_by(|(ka, _), (kb, _)| match (ka, kb) {
                (Some(a), Some(b)) => a.cmp(b),
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
            });
            *items = keyed.into_iter().map(|(_, v)| v).collect();
        }
        Ok(())
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
