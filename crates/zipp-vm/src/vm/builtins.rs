#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Try a builtin method on an array or string receiver. Returns
    /// `Ok(Some(result))` when `name` is a recognised builtin, `Ok(None)` when
    /// it isn't (the caller then treats it as a user-defined method/property).
    ///
    /// Dispatch is split by receiver type into focused helpers so each stays
    /// readable. Methods that take a JS callback (`map`/`filter`/`reduce`/
    /// `sort`) clone the element snapshot out of the heap BEFORE invoking the
    /// callback, because a callback can mutate the same array (which would
    /// reallocate its `Vec` and invalidate any borrow held across the call).
    pub(crate) fn try_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Option<Value>, Thrown> {
        // Gather args into a stack buffer for the common small-arity case (1-2
        // args for push/map/filter/…), avoiding a heap Vec alloc per call; only
        // a rare >8-arg call falls back to the heap.
        let mut stackbuf = [Value::UNDEFINED; 8];
        let heapbuf: Vec<Value>;
        let n = arg_base as usize;
        let args: &[Value] = if argc as usize <= stackbuf.len() {
            for i in 0..argc as usize {
                stackbuf[i] = self.regs[base + n + i];
            }
            &stackbuf[..argc as usize]
        } else {
            heapbuf = (0..argc as usize).map(|i| self.regs[base + n + i]).collect();
            &heapbuf
        };
        self.dispatch_builtin_method(recv, name, args)
    }

    /// Dispatch a builtin method on `recv` with an already-materialized args
    /// slice. Shared by `try_builtin_method` (args gathered from registers) and
    /// the spread method-call path (args taken from an array). `Ok(None)` means
    /// no builtin matched the receiver kind.
    pub(crate) fn dispatch_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // Number receivers (Int or double) support a small method set.
        if recv.is_number() {
            return self.number_method(recv, name, args);
        }
        if !recv.is_heap() {
            return Ok(None);
        }
        let idx = recv.heap_index();
        // Temporal receivers route to their own dispatch (so valueOf throws and
        // toString gives the ISO string, not the generic Object behavior).
        if matches!(self.heap.get(idx), HeapObj::Temporal { .. }) {
            return self.temporal_method(idx, name, args);
        }
        // ── Function.prototype.call / apply / bind (callable receivers) ──
        if self.is_callable(recv) {
            match name {
                "call" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                    return Ok(Some(self.call_value(recv, this, rest)?));
                }
                "apply" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let arr = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    let callargs = if arr.is_heap() { self.iterate_to_vec(arr)? } else { Vec::new() };
                    return Ok(Some(self.call_value(recv, this, &callargs)?));
                }
                "bind" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                    let b = self.heap.alloc(HeapObj::Bound { target: recv, this, args: bound });
                    return Ok(Some(Value::heap(b)));
                }
                _ => {}
            }
        }
        // ── Boxed primitive: dispatch on the wrapped value (so new Number(5).
        // toFixed(), new String("x").charAt(), and valueOf/toString unwrap) — this
        // must precede the generic Object.prototype valueOf/toString below.
        if let HeapObj::Boxed { kind, value } = self.heap.get(idx) {
            let (k, v) = (*kind, *value);
            return match k {
                0 => self.string_method(v.heap_index(), name, args),
                1 => self.number_method(v, name, args),
                _ => match name {
                    "toString" | "valueOf" => Ok(Some(self.boolean_method(v, name))),
                    _ => Ok(None),
                },
            };
        }
        // ── Object.prototype methods (available on every object) ──
        match name {
            "hasOwnProperty" => {
                let key = self.to_property_key(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                return Ok(Some(Value::bool(self.has_own_property(recv, &key))));
            }
            "propertyIsEnumerable" => {
                let key = self.to_property_key(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                return Ok(Some(Value::bool(self.own_is_enumerable(recv, &key))));
            }
            "isPrototypeOf" => {
                let target = args.first().copied().unwrap_or(Value::UNDEFINED);
                return Ok(Some(Value::bool(self.is_prototype_of(recv, target))));
            }
            "valueOf" => return Ok(Some(recv)), // default valueOf returns the object
            "toString" => {
                // Generic `Object.prototype.toString` for a plain object; arrays /
                // numbers / dates etc. have their own toString in the type dispatch.
                if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                    // An error instance inherits Error.prototype.toString ("name: message").
                    if self.is_error_instance(idx) {
                        return self.call_native(native::ERROR_TO_STRING, recv, args).map(Some);
                    }
                    return Ok(Some(self.alloc_str("[object Object]".to_string())));
                }
            }
            _ => {}
        }
        match self.heap.get(idx) {
            HeapObj::Array(_) => self.array_method(idx, name, args),
            HeapObj::Str(_) | HeapObj::Cons { .. } => self.string_method(idx, name, args),
            HeapObj::Map { .. } => self.map_method(idx, name, args),
            HeapObj::Set(_) => self.set_method(idx, name, args),
            HeapObj::Generator { .. } => self.generator_method(idx, name, args),
            HeapObj::AsyncGenerator(_) => Ok(self.async_generator_method(idx, name, args)),
            HeapObj::Promise { .. } => self.promise_method(idx, name, args),
            HeapObj::Date(_) => self.date_method(idx, name, args),
            HeapObj::TypedArray { .. } => self.typed_array_method(idx, name, args),
            HeapObj::DataView { .. } => self.dataview_method(idx, name, args),
            HeapObj::ArrayBuffer { .. } => self.arraybuffer_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// Infallible ToNumber (Symbol/etc. → NaN) — for index/length args in the
    /// TypedArray/DataView methods, where a closure can't propagate `?`.
    pub(crate) fn value_num(&self, v: Value) -> f64 {
        self.to_number(v).unwrap_or(f64::NAN)
    }

    /// `Object.prototype.toString`'s tag: the builtin tag (Array/Function/Error/…),
    /// overridden by a string `@@toStringTag` if present. (`[object <tag>]`.)
    pub(crate) fn object_to_string_tag(&mut self, this: Value) -> Result<String, Thrown> {
        if this.is_undefined() {
            return Ok("Undefined".to_string());
        }
        if this.is_null() {
            return Ok("Null".to_string());
        }
        let builtin = if this.is_heap() {
            match self.heap.get(this.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "String",
                HeapObj::Array(_) => "Array",
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Native(_) | HeapObj::Bound { .. } => {
                    "Function"
                }
                HeapObj::Boxed { kind: 0, .. } => "String",
                HeapObj::Boxed { kind: 1, .. } => "Number",
                HeapObj::Boxed { kind: 2, .. } => "Boolean",
                // Date/RegExp have built-in tags ([[DateValue]]/[[RegExpMatcher]]);
                // Map/Set/Promise/etc instead carry a @@toStringTag (handled below).
                HeapObj::Date(_) => "Date",
                HeapObj::RegExp { .. } => "RegExp",
                _ if self.error_name(this.heap_index()).is_some() => "Error",
                _ => "Object",
            }
        } else if this.is_number() {
            "Number"
        } else if this.is_bool() {
            "Boolean"
        } else {
            "Object"
        };
        // A string @@toStringTag overrides the builtin tag.
        if this.is_heap() {
            let tag = self.get_prop(this, "@@toStringTag")?;
            if tag.is_heap() && self.heap.is_str_like(tag.heap_index()) {
                return Ok(self.display(tag));
            }
        }
        Ok(builtin.to_string())
    }

    pub(crate) fn ta_len_kind(&self, idx: u32) -> (usize, u8) {
        match self.heap.get(idx) {
            // Effective length (0 if out of bounds on a resized buffer).
            HeapObj::TypedArray { kind, .. } => (self.ta_effective_len(idx).unwrap_or(0), *kind),
            _ => (0, 0),
        }
    }
    /// Snapshot a TypedArray's elements as Values (numbers / BigInts).
    pub(crate) fn ta_snapshot(&mut self, idx: u32) -> Vec<Value> {
        let len = self.ta_len_kind(idx).0;
        (0..len).map(|i| self.ta_element_get(idx, i)).collect()
    }
    /// Build a fresh TypedArray of `kind` from element Values (coerced/encoded).
    pub(crate) fn ta_build_from(&mut self, kind: u8, vals: &[Value]) -> Result<Value, Thrown> {
        let size = native::TA_KINDS[kind as usize].1;
        let buf = self.alloc_array_buffer(vals.len() * size);
        let ta = self.alloc_typed_array(buf, kind, 0, vals.len());
        for (i, v) in vals.iter().enumerate() {
            self.ta_element_set(ta.heap_index(), i, *v)?;
        }
        Ok(ta)
    }

    /// `%TypedArray%.prototype` methods (most mirror Array.prototype, but map/filter/
    /// slice/etc. return TypedArrays and `sort` is numeric by default). `idx` is the
    /// receiver TypedArray's heap index.
    /// Resolve a TypedArray relative index argument (negative counts from the
    /// end) into [0,len], via ToInteger — which throws on a Symbol and honours a
    /// valueOf (abrupt completion); `undefined` yields `def`.
    fn ta_rel_index(&mut self, v: Value, def: usize, len: usize) -> Result<usize, Thrown> {
        if v == Value::UNDEFINED {
            return Ok(def);
        }
        let n = self.to_integer_or_zero(v)?;
        Ok(if n < 0 {
            ((len as i64) + n).max(0) as usize
        } else {
            (n as usize).min(len)
        })
    }

    pub(crate) fn typed_array_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        if !matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            return Ok(None);
        }
        // ValidateTypedArray: nearly every TypedArray prototype method throws a
        // TypeError when the view is out of bounds — a detached buffer, or (on a
        // resizable buffer that shrank) an offset/length that no longer fits.
        // subarray is the one exception (it just builds another view).
        if name != "subarray" && self.ta_effective_len(idx).is_none() {
            return Err(Thrown(format!(
                "TypeError: Cannot perform {name} on an out-of-bounds or detached TypedArray"
            )));
        }
        let (len, kind) = self.ta_len_kind(idx);
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        match name {
            "at" => {
                // ToInteger(index): throws on a Symbol, honours a valueOf.
                let n = self.to_integer_or_zero(a0)?;
                let i = if n < 0 { len as i64 + n } else { n };
                Ok(Some(if i >= 0 && (i as usize) < len {
                    self.ta_element_get(idx, i as usize)
                } else {
                    Value::UNDEFINED
                }))
            }
            "join" => {
                let sep = if a0 == Value::UNDEFINED { ",".to_string() } else { self.to_js_string(a0)? };
                let parts: Vec<String> = (0..len).map(|i| self.ta_elem_string(idx, i)).collect();
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            "toString" => {
                let parts: Vec<String> = (0..len).map(|i| self.ta_elem_string(idx, i)).collect();
                Ok(Some(self.alloc_str(parts.join(","))))
            }
            "indexOf" | "lastIndexOf" | "includes" => {
                let snap = self.ta_snapshot(idx);
                let len = snap.len() as i64;
                // fromIndex (ToInteger). lastIndexOf defaults to len-1 and counts
                // negatives from the end; indexOf/includes clamp to [0, len].
                let from = if args.len() >= 2 {
                    self.to_integer_or_zero(a1)?
                } else if name == "lastIndexOf" {
                    len - 1
                } else {
                    0
                };
                let mut found: i64 = -1;
                if name == "lastIndexOf" {
                    let hi = if from < 0 { len + from } else { from.min(len - 1) };
                    if hi >= 0 {
                        for i in (0..=(hi as usize).min(snap.len().saturating_sub(1))).rev() {
                            if self.values_strict_eq(snap[i], a0) {
                                found = i as i64;
                                break;
                            }
                        }
                    }
                } else {
                    let lo = if from < 0 { (len + from).max(0) } else { from.min(len) } as usize;
                    for i in lo..snap.len() {
                        let eq = if name == "includes" {
                            self.same_value_zero(snap[i], a0)
                        } else {
                            self.values_strict_eq(snap[i], a0)
                        };
                        if eq {
                            found = i as i64;
                            break;
                        }
                    }
                }
                Ok(Some(if name == "includes" {
                    Value::bool(found >= 0)
                } else {
                    Value::num(found as f64)
                }))
            }
            "forEach" | "map" | "filter" | "find" | "findIndex" | "findLast" | "findLastIndex"
            | "every" | "some" => {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!("TypeError: {name} callback is not a function")));
                }
                let mut mapped: Vec<Value> = Vec::new();
                let order: Vec<usize> = if name == "findLast" || name == "findLastIndex" {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                for &i in &order {
                    // Read each element fresh (the spec re-Gets per iteration, so a
                    // callback that mutates the TypedArray is observed — values are
                    // not cached before iteration).
                    let e = self.ta_element_get(idx, i);
                    let r = self.call_value(a0, a1, &[e, Value::num(i as f64), recv])?;
                    match name {
                        "forEach" => {}
                        "map" => mapped.push(r),
                        "filter" => {
                            if self.truthy(r) {
                                mapped.push(e);
                            }
                        }
                        "find" => {
                            if self.truthy(r) {
                                return Ok(Some(e));
                            }
                        }
                        "findLast" => {
                            if self.truthy(r) {
                                return Ok(Some(e));
                            }
                        }
                        "findIndex" | "findLastIndex" => {
                            if self.truthy(r) {
                                return Ok(Some(Value::num(i as f64)));
                            }
                        }
                        "every" => {
                            if !self.truthy(r) {
                                return Ok(Some(Value::bool(false)));
                            }
                        }
                        "some" => {
                            if self.truthy(r) {
                                return Ok(Some(Value::bool(true)));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Some(match name {
                    "map" => self.ta_build_from(kind, &mapped)?,
                    "filter" => self.ta_build_from(kind, &mapped)?,
                    "find" | "findLast" => Value::UNDEFINED,
                    "findIndex" | "findLastIndex" => Value::num(-1.0),
                    "every" => Value::bool(true),
                    "some" => Value::bool(false),
                    _ => Value::UNDEFINED, // forEach
                }))
            }
            "reduce" | "reduceRight" => {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!("TypeError: {name} callback is not a function")));
                }
                let order: Vec<usize> = if name == "reduceRight" {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                let mut acc;
                let mut start = 0;
                if args.len() >= 2 {
                    acc = a1;
                } else {
                    if order.is_empty() {
                        return Err(Thrown("TypeError: Reduce of empty array with no initial value".into()));
                    }
                    acc = self.ta_element_get(idx, order[0]);
                    start = 1;
                }
                for &i in &order[start..] {
                    // Read each element fresh (not cached) per the spec.
                    let e = self.ta_element_get(idx, i);
                    acc = self.call_value(a0, Value::UNDEFINED, &[acc, e, Value::num(i as f64), recv])?;
                }
                Ok(Some(acc))
            }
            "fill" => {
                let start = self.ta_rel_index(a1, 0, len)?;
                let end = self.ta_rel_index(args.get(2).copied().unwrap_or(Value::UNDEFINED), len, len)?;
                for i in start..end {
                    self.ta_element_set(idx, i, a0)?;
                }
                Ok(Some(recv))
            }
            "reverse" => {
                let mut snap = self.ta_snapshot(idx);
                snap.reverse();
                for (i, v) in snap.into_iter().enumerate() {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            // ES2023 change-array-by-copy: build a NEW typed array of the same kind.
            "toReversed" => {
                let mut snap = self.ta_snapshot(idx);
                snap.reverse();
                Ok(Some(self.ta_build_from(kind, &snap)?))
            }
            "toSorted" => {
                let cmp = a0;
                if cmp != Value::UNDEFINED && !self.is_callable(cmp) {
                    return Err(Thrown(
                        "TypeError: the comparator argument must be a function or undefined".into(),
                    ));
                }
                let mut snap = self.ta_snapshot(idx);
                if self.is_callable(cmp) {
                    let n = snap.len();
                    for i in 1..n {
                        let mut j = i;
                        while j > 0 {
                            let r = self.call_value(cmp, Value::UNDEFINED, &[snap[j - 1], snap[j]])?;
                            if self.value_num(r) > 0.0 {
                                snap.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    snap.sort_by(|a, b| {
                        let (x, y) = (self.value_num(*a), self.value_num(*b));
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                Ok(Some(self.ta_build_from(kind, &snap)?))
            }
            "with" => {
                // %TypedArray%.prototype.with(index, value): a copy with one element
                // replaced. A relative (negative = from end) index out of range is a
                // RangeError; the value is coerced by ta_build_from.
                let n = self.value_num(a0);
                let n = if n.is_nan() { 0.0 } else { n.trunc() };
                let actual = if n < 0.0 { len as f64 + n } else { n };
                if actual < 0.0 || actual >= len as f64 {
                    return Err(Thrown("RangeError: invalid typed array index".into()));
                }
                let mut snap = self.ta_snapshot(idx);
                snap[actual as usize] = a1;
                Ok(Some(self.ta_build_from(kind, &snap)?))
            }
            "slice" => {
                let start = self.ta_rel_index(a0, 0, len)?;
                let end = self.ta_rel_index(a1, len, len)?;
                let vals: Vec<Value> = (start..end.max(start)).map(|i| self.ta_element_get(idx, i)).collect();
                Ok(Some(self.ta_build_from(kind, &vals)?))
            }
            "subarray" => {
                let start = self.ta_rel_index(a0, 0, len)?;
                let end = self.ta_rel_index(a1, len, len)?;
                let (buffer, byte_offset) = match self.heap.get(idx) {
                    HeapObj::TypedArray { buffer, byte_offset, .. } => (*buffer, *byte_offset),
                    _ => return Ok(None),
                };
                let size = native::TA_KINDS[kind as usize].1;
                let new_len = end.saturating_sub(start);
                Ok(Some(self.alloc_typed_array(buffer, kind, byte_offset + start * size, new_len)))
            }
            "sort" => {
                let cmp = a0;
                let mut snap = self.ta_snapshot(idx);
                if self.is_callable(cmp) {
                    // Comparator sort (stable insertion to allow VM re-entry).
                    let n = snap.len();
                    for i in 1..n {
                        let mut j = i;
                        while j > 0 {
                            let r = self.call_value(cmp, Value::UNDEFINED, &[snap[j - 1], snap[j]])?;
                            if self.value_num(r) > 0.0 {
                                snap.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    snap.sort_by(|a, b| {
                        let (x, y) = (self.value_num(*a), self.value_num(*b));
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                for (i, v) in snap.into_iter().enumerate() {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            "copyWithin" => {
                let target = self.ta_rel_index(a0, 0, len)?;
                let start = self.ta_rel_index(a1, 0, len)?;
                let end = self.ta_rel_index(args.get(2).copied().unwrap_or(Value::UNDEFINED), len, len)?;
                let src: Vec<Value> = (start..end.max(start)).map(|i| self.ta_element_get(idx, i)).collect();
                for (k, v) in src.into_iter().enumerate() {
                    if target + k < len {
                        self.ta_element_set(idx, target + k, v)?;
                    }
                }
                Ok(Some(recv))
            }
            "set" => {
                let offset = if a1 == Value::UNDEFINED { 0 } else { self.value_num(a1) as usize };
                let src = self.iterate_or_arraylike(a0)?;
                for (k, v) in src.into_iter().enumerate() {
                    self.ta_element_set(idx, offset + k, v)?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            "keys" => {
                let items: Vec<Value> = (0..len).map(|i| Value::num(i as f64)).collect();
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "values" | "@@iterator" => {
                let items = self.ta_snapshot(idx);
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "entries" => {
                let mut items = Vec::with_capacity(len);
                for i in 0..len {
                    let e = self.ta_element_get(idx, i);
                    items.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![Value::num(i as f64), e]))));
                }
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            _ => Ok(None),
        }
    }

    /// Array-like or iterable → Vec of element Values (for `TypedArray.prototype.set`
    /// and TypedArray construction).
    pub(crate) fn iterate_or_arraylike(&mut self, v: Value) -> Result<Vec<Value>, Thrown> {
        if let Some(ta) = self.as_typed_array(v) {
            return Ok(self.ta_snapshot(ta));
        }
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(_)
                | HeapObj::Set(_)
                | HeapObj::Map { .. }
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
                | HeapObj::Generator { .. }
                | HeapObj::Iterator { .. } => return self.iterate_to_vec(v),
                _ => {}
            }
        }
        // Array-like object: read length + indices 0..length.
        let lv = self.get_prop(v, "length")?;
        let n = self.value_num(lv);
        let n = if n.is_finite() && n > 0.0 { n as usize } else { 0 };
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(self.get_index(v, Value::num(i as f64))?);
        }
        Ok(out)
    }

    /// `DataView.prototype.get/setInt8 … getFloat64` (+ `byteLength`/`byteOffset`/
    /// `buffer` are getters in get_prop). `name` is e.g. "getInt32".
    pub(crate) fn dataview_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let (buffer, byte_offset, byte_length) = match self.heap.get(idx) {
            HeapObj::DataView { buffer, byte_offset, byte_length } => (*buffer, *byte_offset, *byte_length),
            _ => return Ok(None),
        };
        let (op, ty) = if let Some(t) = name.strip_prefix("get") {
            (0u8, t)
        } else if let Some(t) = name.strip_prefix("set") {
            (1u8, t)
        } else {
            return Ok(None);
        };
        // Element kind index for the suffix (Int8..Float64 / BigInt64 / BigUint64).
        let kind = match ty {
            "Int8" => 0,
            "Uint8" => 1,
            "Int16" => 3,
            "Uint16" => 4,
            "Int32" => 5,
            "Uint32" => 6,
            "Float32" => 7,
            "Float64" => 8,
            "BigInt64" => 9,
            "BigUint64" => 10,
            _ => return Ok(None),
        };
        let size = native::TA_KINDS[kind as usize].1;
        let pos = self.value_num(args.first().copied().unwrap_or(Value::UNDEFINED)) as usize;
        // get(pos, littleEndian?) / set(pos, value, littleEndian?)
        let little_endian = if op == 0 {
            self.truthy(args.get(1).copied().unwrap_or(Value::UNDEFINED))
        } else {
            self.truthy(args.get(2).copied().unwrap_or(Value::UNDEFINED))
        };
        if pos + size > byte_length {
            return Err(Thrown("RangeError: Offset is outside the bounds of the DataView".into()));
        }
        let abs = byte_offset + pos;
        if op == 0 {
            // read
            let mut b = [0u8; 8];
            {
                let data = match self.heap.get(buffer) {
                    HeapObj::ArrayBuffer { data, .. } => data,
                    _ => return Ok(Some(Value::UNDEFINED)),
                };
                if abs + size > data.len() {
                    return Err(Thrown("RangeError: DataView out of bounds".into()));
                }
                b[..size].copy_from_slice(&data[abs..abs + size]);
            }
            if !little_endian {
                b[..size].reverse();
            }
            Ok(Some(match kind {
                0 => Value::num(b[0] as i8 as f64),
                1 => Value::num(b[0] as f64),
                3 => Value::num(i16::from_le_bytes([b[0], b[1]]) as f64),
                4 => Value::num(u16::from_le_bytes([b[0], b[1]]) as f64),
                5 => Value::num(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                6 => Value::num(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                7 => Value::num(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                8 => Value::num(f64::from_le_bytes(b)),
                9 => self.make_bigint(i64::from_le_bytes(b) as i128),
                _ => self.make_bigint(u64::from_le_bytes(b) as i128),
            }))
        } else {
            // write
            let v = args.get(1).copied().unwrap_or(Value::UNDEFINED);
            let mut bytes = if kind >= 9 {
                let n = self.to_bigint(v)?;
                if kind == 9 {
                    (n as i64).to_le_bytes()
                } else {
                    (n as u64).to_le_bytes()
                }
            } else {
                let f = self.to_number_coerce(v)?;
                ta_encode(kind, f)
            };
            if !little_endian {
                bytes[..size].reverse();
            }
            if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buffer) {
                if abs + size <= data.len() {
                    data[abs..abs + size].copy_from_slice(&bytes[..size]);
                }
            }
            Ok(Some(Value::UNDEFINED))
        }
    }

    /// `ArrayBuffer.prototype.slice(begin?, end?)` → a new ArrayBuffer copy.
    pub(crate) fn arraybuffer_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let len = self.array_buffer_len(idx);
        match name {
            // `ArrayBuffer.prototype.resize(newLength)` — only for a resizable
            // buffer (created with maxByteLength); grows with zero-fill, shrinks
            // by truncation, within [0, maxByteLength].
            "resize" => {
                let max = match self.ab_max.get(&idx) {
                    Some(&m) => m,
                    None => return Err(Thrown("TypeError: ArrayBuffer is not resizable".into())),
                };
                if matches!(self.heap.get(idx), HeapObj::ArrayBuffer { detached: true, .. }) {
                    return Err(Thrown("TypeError: Cannot resize a detached ArrayBuffer".into()));
                }
                let n = self.to_integer_or_zero(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                if n < 0 || n as usize > max {
                    return Err(Thrown("RangeError: ArrayBuffer resize length out of range".into()));
                }
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(idx) {
                    data.resize(n as usize, 0u8);
                }
                Ok(Some(Value::UNDEFINED))
            }
            "slice" => {
                let start = self.ta_rel_index(args.first().copied().unwrap_or(Value::UNDEFINED), 0, len)?;
                let end = self.ta_rel_index(args.get(1).copied().unwrap_or(Value::UNDEFINED), len, len)?;
                let slice: Vec<u8> = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => data[start..end.max(start)].to_vec(),
                    _ => Vec::new(),
                };
                let new_idx = self.alloc_array_buffer(slice.len());
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(new_idx) {
                    data.copy_from_slice(&slice);
                }
                Ok(Some(Value::heap(new_idx)))
            }
            _ => Ok(None),
        }
    }

    /// `Promise.prototype.then/catch/finally`. Returns a NEW dependent promise.
    /// All handlers run as microtasks (never synchronously). `idx` is the
    /// receiver promise's heap index.
    pub(crate) fn promise_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "then" => {
                let on_r = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let dep = self.then_internal(idx, a0, on_r, None);
                Ok(Some(Value::heap(dep)))
            }
            "catch" => {
                let dep = self.then_internal(idx, Value::UNDEFINED, a0, None);
                Ok(Some(Value::heap(dep)))
            }
            "finally" => {
                // `cb` runs (no args) on both settle paths; the original value /
                // reason forwards (FinallyReaction handles the value pass-through).
                let dep = self.finally_internal(idx, a0);
                Ok(Some(Value::heap(dep)))
            }
            _ => Ok(None),
        }
    }

    /// `Map.prototype.*`. `idx` is the Map's heap index. Returns `Ok(None)` for an
    /// unknown method (→ TypeError at the call site). `forEach` snapshots the
    /// entries before invoking the callback (which may mutate the map).
    pub(crate) fn map_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        // Brand check: `Map.prototype.<m>.call(x)` requires x to have [[MapData]].
        if !matches!(self.heap.get(idx), HeapObj::Map { .. }) {
            return Err(Thrown(format!("TypeError: Map.prototype.{name} called on incompatible receiver")));
        }
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => keys
                        .iter()
                        .position(|k| self.same_value_zero(*k, a0))
                        .map(|i| vals[i]),
                    _ => None,
                };
                Ok(Some(v.unwrap_or(Value::UNDEFINED)))
            }
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().any(|k| self.same_value_zero(*k, a0)),
                    _ => false,
                };
                Ok(Some(Value::bool(found)))
            }
            "set" => {
                let key = normalize_zero(a0);
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, key)),
                    _ => None,
                };
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val, // update in place, keep position
                        None => {
                            keys.push(key);
                            vals.push(val);
                        }
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::Map { keys, vals }) = (pos, self.heap.get_mut(idx)) {
                    keys.remove(i);
                    vals.remove(i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    keys.clear();
                    vals.clear();
                }
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (ks, vs) = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => (keys.clone(), vals.clone()),
                    _ => (Vec::new(), Vec::new()),
                };
                for (k, v) in ks.into_iter().zip(vs) {
                    // callback(value, key, map)
                    self.call_value(cb, this_arg, &[v, k, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // Real iterators over %MapIteratorPrototype% (snapshot semantics).
            "keys" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(self.make_iterator(v, self.map_iter_proto)))
            }
            "values" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { vals, .. } => vals.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(self.make_iterator(v, self.map_iter_proto)))
            }
            "entries" => {
                let pairs: Vec<(Value, Value)> = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => {
                        keys.iter().copied().zip(vals.iter().copied()).collect()
                    }
                    _ => Vec::new(),
                };
                let entries: Vec<Value> = pairs
                    .into_iter()
                    .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                    .collect();
                Ok(Some(self.make_iterator(entries, self.map_iter_proto)))
            }
            _ => Ok(None),
        }
    }

    /// `WeakMap.prototype.{get,set,has,delete}`. Brand-checked (the receiver must be
    /// a WeakMap, so `WeakMap.prototype.set.call(aMap)` throws) and keys must be
    /// objects. No GC, so entries are held strongly (unobservable without GC).
    pub(crate) fn weakmap_method(&mut self, this: Value, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::WeakMap { .. }) {
            return Err(Thrown(format!(
                "TypeError: WeakMap.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, vals } => {
                        keys.iter().position(|k| self.same_value_zero(*k, a0)).map(|i| vals[i])
                    }
                    _ => None,
                };
                Ok(v.unwrap_or(Value::UNDEFINED))
            }
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, .. } => keys.iter().any(|k| self.same_value_zero(*k, a0)),
                    _ => false,
                };
                Ok(Value::bool(found))
            }
            "set" => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Invalid value used as weak map key".into()));
                }
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let HeapObj::WeakMap { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val,
                        None => {
                            keys.push(a0);
                            vals.push(val);
                        }
                    }
                }
                Ok(this) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::WeakMap { keys, vals }) = (pos, self.heap.get_mut(idx)) {
                    keys.remove(i);
                    vals.remove(i);
                    return Ok(Value::bool(true));
                }
                Ok(Value::bool(false))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `WeakSet.prototype.{add,has,delete}`. Brand-checked; values must be objects.
    pub(crate) fn weakset_method(&mut self, this: Value, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::WeakSet(_)) {
            return Err(Thrown(format!(
                "TypeError: WeakSet.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::WeakSet(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => false,
                };
                Ok(Value::bool(found))
            }
            "add" => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Invalid value used in weak set".into()));
                }
                let present = match self.heap.get(idx) {
                    HeapObj::WeakSet(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => true,
                };
                if !present {
                    if let HeapObj::WeakSet(items) = self.heap.get_mut(idx) {
                        items.push(a0);
                    }
                }
                Ok(this) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::WeakSet(items) => items.iter().position(|v| self.same_value_zero(*v, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::WeakSet(items)) = (pos, self.heap.get_mut(idx)) {
                    items.remove(i);
                    return Ok(Value::bool(true));
                }
                Ok(Value::bool(false))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `FinalizationRegistry.prototype.{register,unregister}`. No GC, so cleanup
    /// never fires; only the register/unregister bookkeeping (+ arg validation) is
    /// observable. `tokens` tracks live unregister tokens for `unregister`.
    pub(crate) fn finreg_method(&mut self, this: Value, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::FinalizationRegistry { .. }) {
            return Err(Thrown(format!(
                "TypeError: FinalizationRegistry.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "register" => {
                let held = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let token = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: FinalizationRegistry.register: target must be an object".into()));
                }
                if self.same_value(a0, held) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: target and held value must not be the same".into(),
                    ));
                }
                if token != Value::UNDEFINED && !self.is_object_value(token) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: unregister token must be an object".into(),
                    ));
                }
                if self.is_object_value(token) {
                    if let HeapObj::FinalizationRegistry { tokens, .. } = self.heap.get_mut(idx) {
                        tokens.push(token);
                    }
                }
                Ok(Value::UNDEFINED)
            }
            "unregister" => {
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.unregister: token must be an object".into(),
                    ));
                }
                let mut removed = false;
                if let HeapObj::FinalizationRegistry { tokens, .. } = self.heap.get_mut(idx) {
                    let before = tokens.len();
                    tokens.retain(|t| *t != a0); // object identity = Value bit-equality
                    removed = tokens.len() != before;
                }
                Ok(Value::bool(removed))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `Set.prototype.*`. `idx` is the Set's heap index. `keys`/`values`/`entries`
    /// return arrays (the iterator approximation).
    pub(crate) fn set_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        // Brand check: `Set.prototype.<m>.call(x)` requires x to have [[SetData]].
        if !matches!(self.heap.get(idx), HeapObj::Set(_)) {
            return Err(Thrown(format!("TypeError: Set.prototype.{name} called on incompatible receiver")));
        }
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => false,
                };
                Ok(Some(Value::bool(found)))
            }
            "add" => {
                let val = normalize_zero(a0);
                let present = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().any(|v| self.same_value_zero(*v, val)),
                    _ => true,
                };
                if !present {
                    if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                        items.push(val);
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().position(|v| self.same_value_zero(*v, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::Set(items)) = (pos, self.heap.get_mut(idx)) {
                    items.remove(i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                    items.clear();
                }
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                for v in items {
                    // callback(value, value, set) — value passed twice, mirroring Map.
                    self.call_value(cb, this_arg, &[v, v, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // keys() === values() for a Set; both yield the values (real iterator).
            "keys" | "values" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(self.make_iterator(v, self.set_iter_proto)))
            }
            "entries" => {
                let items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                let entries: Vec<Value> = items
                    .into_iter()
                    .map(|v| Value::heap(self.heap.alloc(HeapObj::Array(vec![v, v]))))
                    .collect();
                Ok(Some(self.make_iterator(entries, self.set_iter_proto)))
            }
            // ES2025 set methods. `other` must be set-like; the common (and tested)
            // case is a real Set, whose elements we read directly.
            "union" | "intersection" | "difference" | "symmetricDifference"
            | "isSubsetOf" | "isSupersetOf" | "isDisjointFrom" => {
                // Calls user has()/keys() (Set-like arg), so suspend GC for the scope.
                let _gc = self.gc_lock_guard();
                let this_items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                // GetSetRecord: a real Set uses its elements directly; any other
                // value must be a Set-like object ({size: number, has, keys}) —
                // read size (ToNumber, observable) / has / keys in spec order, then
                // materialize its elements via keys().
                let other_items: Vec<Value> = match a0.is_heap().then(|| self.heap.get(a0.heap_index())) {
                    Some(HeapObj::Set(items)) => items.clone(),
                    _ => {
                        if !self.is_object_value(a0) {
                            return Err(Thrown(
                                "TypeError: Set.prototype set method called with a non-object".into(),
                            ));
                        }
                        let raw_size = self.get_prop(a0, "size")?;
                        if raw_size.is_heap()
                            && matches!(self.heap.get(raw_size.heap_index()), HeapObj::BigInt(_))
                        {
                            return Err(Thrown(
                                "TypeError: Set-like 'size' cannot be a BigInt".into(),
                            ));
                        }
                        let num_size = self.to_number_coerce(raw_size)?;
                        if num_size.is_nan() {
                            return Err(Thrown("TypeError: Set-like 'size' is NaN".into()));
                        }
                        let has = self.get_prop(a0, "has")?;
                        if !self.is_callable(has) {
                            return Err(Thrown("TypeError: Set-like 'has' is not callable".into()));
                        }
                        let keys = self.get_prop(a0, "keys")?;
                        if !self.is_callable(keys) {
                            return Err(Thrown("TypeError: Set-like 'keys' is not callable".into()));
                        }
                        let kiter = self.call_value(keys, a0, &[])?;
                        // -0 from keys() normalizes to +0 (SameValueZero).
                        self.iterate_to_vec(kiter)?
                            .into_iter()
                            .map(|v| if v.is_number() && v.as_f64() == 0.0 { Value::int(0) } else { v })
                            .collect()
                    }
                };
                let has = |hay: &[Value], v: Value, vm: &Self| hay.iter().any(|x| vm.same_value_zero(*x, v));
                let result = match name {
                    "union" => {
                        let mut r = this_items.clone();
                        for &v in &other_items {
                            if !has(&r, v, self) {
                                r.push(v);
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "intersection" => {
                        let r: Vec<Value> =
                            this_items.iter().copied().filter(|&v| has(&other_items, v, self)).collect();
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "difference" => {
                        let r: Vec<Value> =
                            this_items.iter().copied().filter(|&v| !has(&other_items, v, self)).collect();
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "symmetricDifference" => {
                        let mut r: Vec<Value> =
                            this_items.iter().copied().filter(|&v| !has(&other_items, v, self)).collect();
                        for &v in &other_items {
                            if !has(&this_items, v, self) && !has(&r, v, self) {
                                r.push(v);
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "isSubsetOf" => Value::bool(this_items.iter().all(|&v| has(&other_items, v, self))),
                    "isSupersetOf" => Value::bool(other_items.iter().all(|&v| has(&this_items, v, self))),
                    _ => Value::bool(!this_items.iter().any(|&v| has(&other_items, v, self))), // isDisjointFrom
                };
                Ok(Some(result))
            }
            _ => Ok(None),
        }
    }

}
