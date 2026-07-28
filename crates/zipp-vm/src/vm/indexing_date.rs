#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// ToPropertyKey for a computed index: a Symbol or already-flat string keys
    /// as-is and primitives (numbers/bool/null) fall through unchanged, but an
    /// object key must be ToString-coerced (invoking its `toString`/`valueOf`)
    /// rather than rendered "[object Object]" by `key_of`'s `display`. Returns the
    /// key Value to use downstream (a fresh heap string for the coerced case).
    pub(crate) fn coerce_index_key(&mut self, key: Value) -> Result<Value, Thrown> {
        if key.is_heap()
            && !matches!(
                self.heap.get(key.heap_index()),
                HeapObj::Symbol { .. } | HeapObj::Str(_) | HeapObj::Cons { .. }
            )
        {
            // ToPropertyKey: ToPrimitive(key, hint String). A Symbol result (e.g. an
            // object key whose @@toPrimitive/toString returns a Symbol) stays a
            // Symbol Value — `key_of` maps it to its "@@…" form — rather than wrongly
            // throwing on a stringify; any other primitive is ToString'd.
            let prim = self.to_primitive_string(key)?;
            if prim.is_heap() && matches!(self.heap.get(prim.heap_index()), HeapObj::Symbol { .. }) {
                return Ok(prim);
            }
            let s = self.to_js_string(prim)?;
            return Ok(self.alloc_str(s));
        }
        Ok(key)
    }

    pub(crate) fn get_index(&mut self, obj: Value, key: Value) -> Result<Value, Thrown> {
        // RequireObjectCoercible(base) precedes ToPropertyKey(key): `null[k]` must
        // throw TypeError BEFORE evaluating k's toString (sec-evaluate-property-
        // access-with-expression-key + GetValue). coerce_index_key runs the key's
        // ToString, so the nullish guard has to come first.
        if obj.is_nullish() {
            return Err(Thrown(format!(
                "TypeError: cannot read property of {}",
                self.display(obj)
            )));
        }
        // A rope must be materialized before random access; no-op (one tag
        // check) for arrays, objects, and already-flat strings.
        if obj.is_heap() {
            self.heap.flatten(obj.heap_index());
        }
        let key = self.coerce_index_key(key)?;
        if !obj.is_heap() {
            // null/undefined throw; a number/boolean primitive resolves method-as-value
            // through its prototype (`(5)["toFixed"]`, `true["toString"]`).
            if obj.is_nullish() {
                return Err(Thrown(format!(
                    "TypeError: cannot read property of {}",
                    self.display(obj)
                )));
            }
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        // FAST PATH: a plain-object computed read whose flat string key hits an
        // own DATA property answers straight from the ObjMap, with the key's
        // bytes viewed in place — key_of below materializes a fresh String per
        // call, which parse-/JSON-shaped workloads pay millions of times.
        // Exotic own-key carriers are other HeapObj variants; the slot-backed
        // global and module namespaces (live bindings, defer triggers) stay
        // generic, as do accessor hits and misses (proto/class chain).
        if key.is_heap() {
            let oidx = obj.heap_index();
            if !(oidx == self.global_this && self.global_this != 0)
                && !(!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&oidx))
                && !(!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&oidx))
            {
                if let Some(std::borrow::Cow::Borrowed(b)) =
                    self.heap.str_wtf8_cow(key.heap_index())
                {
                    if let (Ok(k), HeapObj::Object(m)) =
                        (std::str::from_utf8(b), self.heap.get(oidx))
                    {
                        if let Some(i) = m.pos(k) {
                            if !m.attrs[i].accessor {
                                let v = m.vals[i];
                                if !v.is_uninitialized() {
                                    return Ok(v);
                                }
                            }
                        }
                    }
                }
            }
        }
        // A boxed String indexes its wrapped string (chars / length); a boxed
        // Number/Boolean has no index, so computed access goes through the prototype.
        if let HeapObj::Boxed { kind, .. } = self.heap.get(obj.heap_index()) {
            let k = *kind;
            if k == 0 {
                // A String wrapper is a String exotic: an in-range char index
                // (numeric Value or canonical numeric-string) reads the wrapped
                // string; any other key (an assigned/defineProperty'd own prop in
                // arr_props, `length`, or a method) resolves on the WRAPPER so own
                // properties aren't lost to the primitive.
                let ks = self.key_of(key);
                if let Some((sval, len)) = self.string_exotic_chars(obj) {
                    if let Some(i) = array_index(key).or_else(|| canonical_index_str(&ks)) {
                        if i < len {
                            return self.get_index(sval, Value::int(i as i32));
                        }
                    }
                }
                return self.get_prop(obj, &ks);
            }
            // A boxed Number/Boolean has no index — go through the prototype.
            let key_s = self.key_of(key);
            return self.get_prop(obj, &key_s);
        }
        // Object / callable / class index access is property access: delegate to
        // `get_prop` so a computed key reaches inherited methods/getters (e.g. a
        // class instance's `obj[Symbol.iterator]`), a callable's `fn["name"]`, and
        // static members (`C["m"]`) — not just own data properties. The built-in
        // instance types (Date/Promise/Weak*) have no integer-index meaning, so all
        // their computed access delegates here too.
        // A TypedArray: a canonical numeric index reads the element; everything
        // else (length/byteLength/methods) delegates to get_prop.
        if matches!(self.heap.get(obj.heap_index()), HeapObj::TypedArray { .. }) {
            if let Some(i) = array_index(key) {
                return Ok(self.ta_element_get(obj.heap_index(), i));
            }
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        if matches!(
            self.heap.get(obj.heap_index()),
            HeapObj::Object(_)
                | HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Wrapped { .. }
                | HeapObj::BoundResolver { .. }
                | HeapObj::CombinatorResolver { .. }
                | HeapObj::Native(_)
                | HeapObj::Iterator { .. }
                | HeapObj::IterHelper { .. }
                | HeapObj::Generator { .. }
                | HeapObj::AsyncGenerator(_)
                | HeapObj::Date(_)
                | HeapObj::Temporal { .. }
                // An Intl service instance has no integer-index meaning either.
                // Omitting it dropped `nf["formatRange"]` (and every other
                // COMPUTED read on an Intl object) into the `_ => undefined`
                // arm below, so the prototype method resolved to undefined
                // while `nf.formatRange` worked — what the `invoked-as-func`
                // tests observe as `typeof f === "undefined"`.
                | HeapObj::Intl { .. }
                | HeapObj::Promise { .. }
                | HeapObj::WeakMap { .. }
                | HeapObj::WeakSet(_)
                | HeapObj::WeakRef(_)
                | HeapObj::FinalizationRegistry { .. }
                | HeapObj::RegExp { .. }
                | HeapObj::Symbol { .. }
                | HeapObj::BigInt(_)
                | HeapObj::BigIntBig(_)
                | HeapObj::ArrayBuffer { .. }
                | HeapObj::DataView { .. }
                | HeapObj::Proxy { .. }
        ) {
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(_) => {
                let aidx = obj.heap_index();
                // Numeric key (incl. an integral double like 1.0 — the JIT region
                // produces f64 indices): a defineProperty'd special index (accessor
                // or non-default-attribute data) in arr_props overrides the dense
                // slot; else direct element access, else undefined.
                if let Some(i) = array_index(key) {
                    // A LIVE-mapped arguments index reads the formal's register
                    // (a still-mapped index is always a plain data property, so
                    // this wins over any attribute-only override).
                    if let Some(v) = self.args_mapped_get(aidx, i) {
                        return Ok(v);
                    }
                    if let Some((a, v)) = self.array_index_override(aidx, i) {
                        if a.accessor {
                            return if v == Value::UNDEFINED {
                                Ok(Value::UNDEFINED)
                            } else {
                                self.call_value(v, obj, &[])
                            };
                        }
                        return Ok(v);
                    }
                    if let HeapObj::Array(items) = self.heap.get(aidx) {
                        if i < items.len() && !items[i].is_hole() {
                            return Ok(items[i]);
                        }
                    }
                    // Out of range OR a hole → not an own element; [[Get]] continues up
                    // the ACTUAL prototype chain (a setPrototypeOf custom proto can
                    // carry inherited indices/accessors), else %Array.prototype%.
                    let k = self.key_of(key);
                    let proto = match self.proto_of.get(&aidx) {
                        Some(&p) => p,
                        None if self.arr_proto != 0 => Value::heap(self.arr_proto),
                        None => Value::NULL,
                    };
                    if proto.is_heap() {
                        return self.get_member(proto, &k, obj);
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-int key on an array: "length", else resolve via the prototype
                // (a computed method name / `@@iterator`, mirroring dot access).
                let k = self.key_of(key);
                if k == "length" && !self.arguments_objs.contains_key(&aidx) {
                    if matches!(self.heap.get(aidx), HeapObj::Array(_)) {
                        // A sparse array's JS length lives in the side table.
                        return Ok(len_value(self.js_array_len(aidx)));
                    }
                }
                self.get_prop(obj, &k)
            }
            HeapObj::Object(map) => {
                let k = self.key_of(key);
                Ok(map.get(&k).unwrap_or(Value::UNDEFINED))
            }
            HeapObj::Str(s) => {
                // A numeric Value key (the hot path — incl. an integral double, since
                // a JIT region produces f64 indices and a deopted string index must
                // agree), OR a canonical numeric-STRING key (`"abc"["0"]`). The
                // numeric form is checked FIRST so `s[i]` never pays the string parse.
                let i_opt = match array_index(key) {
                    Some(i) => Some(i),
                    None => canonical_index_str(&self.key_of(key)),
                };
                if let Some(i) = i_opt {
                    // A single ASCII char is interned at heap index == its byte
                    // (see Heap::new), so return that slot DIRECTLY — no temporary
                    // 1-char String + re-intern per access (that alloc dominated
                    // `s[i]` scans). O(1) for ASCII (i-th unit == i-th byte); a
                    // multi-byte string decodes the UTF-16 unit at `i` (O(i)) —
                    // a surrogate half is a REAL 1-unit lone-surrogate string.
                    if s.is_ascii() {
                        return Ok(match s.as_bytes().get(i) {
                            Some(&b) => Value::heap(b as u32),
                            None => Value::UNDEFINED,
                        });
                    }
                    match s.unit_at(i) {
                        Some(u) => return Ok(self.str_from_unit(u)),
                        None => return Ok(Value::UNDEFINED),
                    }
                }
                // Non-index key: `s["length"]`, else resolve via String.prototype
                // (a computed method name / `@@iterator`), mirroring dot access.
                let units = s.units();
                let k = self.key_of(key);
                if k == "length" {
                    return Ok(len_value(units));
                }
                self.get_prop(obj, &k)
            }
            // Positional access drives for-of / spread over a Map (the i-th
            // [key, value] entry) and a Set (the i-th value). Insertion order.
            HeapObj::Map { keys, vals } => {
                if let Some(i) = array_index(key) {
                    if i < keys.len() {
                        let (k, v) = (keys[i], vals[i]);
                        return Ok(Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))));
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-numeric key (`map[Symbol.iterator]`, `map["set"]`): via prototype.
                let k = self.key_of(key);
                self.get_prop(obj, &k)
            }
            HeapObj::Set(items) => {
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                let k = self.key_of(key);
                self.get_prop(obj, &k)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// Whether creating a NEW own data property `key` on the plain object
    /// `start` is provably unobservable through its prototype chain — per
    /// OrdinarySet, an inherited ACCESSOR must fire its setter (or reject)
    /// and an inherited NON-WRITABLE data property must reject, while an
    /// inherited writable data property just gets shadowed. Walks only
    /// plain class-less object levels (mirroring object_get_prototype_of's
    /// resolution for them); anything exotic — Proxy, class chain, the
    /// global, a namespace, a constructor object — answers `false` and the
    /// caller falls back to the generic set_prop. `&self` only: no traps run.
    fn plain_add_chain_clear(&self, start: u32, key: &str) -> bool {
        let mut lidx = start;
        for _ in 0..1000 {
            let proto = match self.proto_of.get(&lidx) {
                Some(p) => *p,
                None if lidx == self.obj_proto => Value::NULL,
                None if self.obj_proto != 0 => Value::heap(self.obj_proto),
                None => Value::NULL,
            };
            if !proto.is_heap() {
                return true; // chain bottom: nothing intercepts the add
            }
            let pidx = proto.heap_index();
            if (pidx == self.global_this && self.global_this != 0)
                || (!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&pidx))
                || (!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&pidx))
            {
                return false;
            }
            match self.heap.get(pidx) {
                HeapObj::Object(pm) => {
                    if pm.class.is_some() || pm.is_ctor {
                        return false; // class accessors / constructor exotica
                    }
                    if let Some(j) = pm.pos(key) {
                        // A writable data property on the chain is merely
                        // shadowed; an accessor or non-writable data blocks.
                        return !pm.attrs[j].accessor && pm.attrs[j].writable;
                    }
                }
                _ => return false, // exotic level: let set_prop decide
            }
            lidx = pidx;
        }
        false
    }

    pub(crate) fn set_index(
        &mut self,
        obj: Value,
        key: Value,
        val: Value,
        strict: bool,
    ) -> Result<(), Thrown> {
        if !obj.is_heap() {
            if obj.is_nullish() {
                return Err(Thrown("TypeError: cannot set property of non-object".into()));
            }
            // PutValue 6: a non-nullish PRIMITIVE base (a number or a boolean —
            // the heap primitives are heap values and fall through) is ToObject'd
            // and the write runs OrdinarySet against the wrapper, so it is a
            // silent no-op in sloppy code and a TypeError in strict.
            // `set_prop` has always done this; only the COMPUTED spelling landed
            // here, so `n.x = 5` was a no-op while `n[k] = 5` threw in SLOPPY
            // code (staging/sm/strict/primitive-assignment.js).
            let k = self.coerce_index_key(key)?;
            let ks = self.key_of(k);
            self.primitive_base_set(obj, &ks, val, strict)?;
            return Ok(());
        }
        let key = self.coerce_index_key(key)?;
        let idx = obj.heap_index();
        // FAST PATH: a plain-object computed write whose flat string key hits an
        // own WRITABLE DATA property stores straight into the ObjMap slot — the
        // twin of get_index's fast read (no key String materialization, no shape
        // change, so no IC/version traffic). An own MISS on an extensible
        // class-less object whose prototype chain provably can't intercept the
        // write (no same-named accessor / non-writable data / exotic level —
        // see plain_add_chain_clear) appends the new data property directly,
        // skipping set_prop's special-case gauntlet. Object.prototype is
        // excluded so its index-key bookkeeping (note_array_proto_index)
        // always runs; everything else stays generic.
        if key.is_heap()
            && !(idx == self.global_this && self.global_this != 0)
            && idx != self.obj_proto
            && self.realm_global_objs.is_empty()
            && !(!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&idx))
            && !(!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&idx))
        {
            // hit: own writable data slot; add: proven-clean new key.
            let (hit, add) = match self.heap.str_wtf8_cow(key.heap_index()) {
                Some(std::borrow::Cow::Borrowed(b)) => {
                    match (std::str::from_utf8(b), self.heap.get(idx)) {
                        (Ok(k), HeapObj::Object(m)) if k != "__proto__" => match m.pos(k) {
                            Some(i) if !m.attrs[i].accessor && m.attrs[i].writable => {
                                (Some(i), false)
                            }
                            Some(_) => (None, false),
                            None => (
                                None,
                                m.extensible
                                    && m.class.is_none()
                                    && !m.is_ctor
                                    && self.plain_add_chain_clear(idx, k),
                            ),
                        },
                        _ => (None, false),
                    }
                }
                _ => (None, false),
            };
            if let Some(i) = hit {
                if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                    m.vals[i] = val;
                    return Ok(());
                }
            }
            if add {
                let ks: Option<String> = match self.heap.str_wtf8_cow(key.heap_index()) {
                    Some(std::borrow::Cow::Borrowed(b)) => {
                        std::str::from_utf8(b).ok().map(|s| s.to_string())
                    }
                    _ => None,
                };
                if let Some(ks) = ks {
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.push_data(ks, val);
                        self.heap.bump_version(idx); // key add reallocs vals (IC)
                        return Ok(());
                    }
                }
            }
        }
        // A TypedArray: a canonical numeric index writes the element (coerced +
        // out-of-bounds is a silent no-op); other keys go to set_prop.
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            if let Some(i) = array_index(key) {
                return self.ta_element_set(idx, i, val);
            }
            let k = self.key_of(key);
            // A valid integer index given as a STRING ("0") still writes the element.
            if let Some(i) = self.ta_valid_index(idx, &k) {
                return self.ta_element_set(idx, i, val);
            }
            // A CanonicalNumericIndexString that isn't a valid integer index
            // (non-integer, "-0", out of range, or any key on a detached view)
            // still runs the VALUE COERCION (observable; abrupt propagates),
            // then drops the write without ever reaching the prototype —
            // IntegerIndexedExotic [[Set]] (10.4.5.5) / TypedArraySetElement.
            if self.is_canonical_numeric_index(&k) {
                self.ta_coerce_for_set(idx, val)?;
                return Ok(());
            }
            self.set_prop(obj, &k, val, strict)?;
            return Ok(());
        }
        // Callable / class computed assignment (`fn["x"] = v`, `C["s"] = v`) is
        // property assignment: route through `set_prop` (honours non-writable
        // `name`/`length`, static setters, function own props).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Wrapped { .. }
                | HeapObj::Native(_)
                | HeapObj::Proxy { .. }
        ) {
            let k = self.key_of(key);
            self.set_prop(obj, &k, val, strict)?;
            return Ok(());
        }
        // An Array with a non-index key (`arr["foo"] = v`, `arr.length` is handled
        // in set_prop) is a NAMED property → route through set_prop (-> arr_props),
        // so bracket and dot assignment agree. Numeric indices stay in the Vec.
        if matches!(self.heap.get(idx), HeapObj::Array(_)) && array_index(key).is_none() {
            let k = self.key_of(key);
            // A canonical numeric index passed as a STRING key ("0") reaches here
            // (array_index only matches numeric Value keys). A frozen array's
            // elements are non-writable, so reject such a write the same as the
            // numeric-key fast path below does (set_prop would otherwise overwrite
            // the dense slot). Sealed arrays keep writable elements, so frozen only.
            if canonical_index_str(&k).is_some()
                && self.arr_props.get(&idx).map_or(false, |m| m.frozen)
            {
                self.reject_write(&k, strict)?;
                return Ok(());
            }
            self.set_prop(obj, &k, val, strict)?;
            return Ok(());
        }
        // A defineProperty'd special index (accessor / non-writable / arr_props
        // value) is handled by set_prop's own-property path (override-aware) — its
        // setter must fire, a non-writable write must no-op, and a writable write
        // updates arr_props rather than the dense placeholder. The dense fast path
        // below handles the common plain element.
        if let Some(i) = array_index(key) {
            if matches!(self.heap.get(idx), HeapObj::Array(_))
                && self.array_index_override(idx, i).is_some()
            {
                let k = self.key_of(key);
                self.set_prop(obj, &k, val, strict)?;
                return Ok(());
            }
        }
        // OrdinarySet prototype step: assigning an array index that is ABSENT as an
        // own property (a new index past length, or a hole) must consult the
        // prototype chain — a prototype setter at that index runs (receiver = the
        // array) and a non-writable prototype data prop rejects, INSTEAD of silently
        // creating an own element. Gated on `array_proto_has_index` so the common
        // case (no integer props on Array/Object.prototype) keeps the fast path.
        // Also taken when THIS array has a custom [[Prototype]] (proto_of entry):
        // a TypedArray in its chain must absorb integer indices even though the
        // global Array.prototype carries none (plain arrays skip the lookup).
        if self.array_proto_has_index || self.proto_of.contains_key(&idx) {
            if let Some(i) = array_index(key) {
                let absent = matches!(
                    self.heap.get(idx),
                    HeapObj::Array(items) if i >= items.len() || items[i].is_hole()
                );
                if absent && self.array_proto_set_step(obj, i, val, strict)? {
                    return Ok(());
                }
            }
        }
        // A FROZEN array has non-writable elements: ANY index write (even to an
        // existing element) is rejected (sloppy no-op / strict TypeError). A sealed
        // (not frozen) array keeps writable elements — only NEW indices are blocked
        // by the non-extensible check below.
        if array_index(key).is_some()
            && matches!(self.heap.get(idx), HeapObj::Array(_))
            && self.arr_props.get(&idx).map_or(false, |m| m.frozen)
        {
            self.reject_write(&self.key_of(key), strict)?;
            return Ok(());
        }
        // A NEW index (past the current length) on a non-extensible array adds an own
        // property → rejected (sloppy no-op / strict TypeError). An in-range index is
        // already present and stays writable. Likewise, extending past the current
        // length grows `length`, so a non-writable `length` (defineProperty / freeze)
        // rejects it — Array [[DefineOwnProperty]]: index >= oldLen && length
        // non-writable → false. (Checked before the &mut borrow below.)
        if let Some(i) = array_index(key) {
            let present = matches!(self.heap.get(idx), HeapObj::Array(items) if i < items.len());
            if !present
                && matches!(self.heap.get(idx), HeapObj::Array(_))
                && (self.arr_props.get(&idx).map_or(false, |m| !m.extensible)
                    // Growing the JS length needs a writable `length`; an index
                    // below a sparse array's VIRTUAL length doesn't grow it.
                    || (self.array_length_nonwritable.contains(&idx)
                        && i >= self.js_array_len(idx)))
            {
                self.reject_write(&self.key_of(key), strict)?;
            return Ok(());
            }
        }
        // An ARGUMENTS object's `length` is a fixed own data property (argc):
        // an index store past the dense window creates an ordinary named own
        // property in the side table instead of growing the Vec (which would
        // grow `length`).
        if let Some(i) = array_index(key) {
            if self.arguments_objs.contains_key(&idx)
                && matches!(self.heap.get(idx), HeapObj::Array(items) if i >= items.len())
            {
                let k = self.key_of(key);
                self.arr_props.entry(idx).or_insert_with(ObjMap::new_side_table).set(&k, val);
                self.heap.bump_version(idx);
                return Ok(());
            }
        }
        // A LIVE-mapped arguments index also writes the formal's register
        // ([[ParameterMap]] [[Set]] companion); the dense store below stays in
        // sync as the escape/descriptor store.
        if let Some(i) = array_index(key) {
            if matches!(self.heap.get(idx), HeapObj::Array(_)) {
                self.args_mapped_set(idx, i, val);
            }
        }
        // A write past the eager-materialization cap goes to the SPARSE overlay:
        // the element lives in arr_props under its canonical index key (default
        // data attributes) and the JS length in the virtual-length side table —
        // the dense Vec is never resized to billions of holes. (An overridden /
        // defineProperty'd index was routed to set_prop above, and an arguments
        // object's past-the-end write returned above, so this index is NEW.)
        if let Some(i) = array_index(key) {
            if i >= crate::vm::MAX_DENSE_ARRAY_LEN
                && matches!(self.heap.get(idx), HeapObj::Array(items) if i >= items.len())
            {
                self.arr_props
                    .entry(idx)
                    .or_insert_with(ObjMap::new_side_table)
                    .set(&i.to_string(), val);
                self.array_grow_js_len(idx, i);
                self.heap.bump_version(idx);
                return Ok(());
            }
        }
        match self.heap.get_mut(idx) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double — the JIT region produces
                // f64 indices): store, growing with `undefined` holes past the end.
                if let Some(i) = array_index(key) {
                    if i >= items.len() {
                        // Slots between the old length and the new index are
                        // HOLES (absent), not present undefineds.
                        items.resize(i + 1, Value::HOLE);
                    }
                    items[i] = val;
                }
                Ok(())
            }
            // Plain objects AND every other heap receiver (Date / boxed primitive /
            // Map / Set / Promise / Weak* / RegExp / …) route through set_prop so a
            // computed write `obj[k] = v` is stored (in the object map or the
            // arr_props side table) and read back symmetrically by get_index — and
            // honours property attributes / accessors / extensibility. (Previously a
            // numeric-index write on these exotic receivers was silently dropped, so
            // `Array.prototype.<m>.call(dateLike, …)` saw zero elements.)
            _ => {
                let k = self.key_of(key);
                self.set_prop(obj, &k, val, strict)?;
                Ok(())
            }
        }
    }

    /// `dst = obj[<string-const `name`> + key]` — the `GetIndexConcat` op. When
    /// `key` is an int and `obj` is a plain object with an own non-accessor data
    /// slot for `"<prefix><int>"`, the key is assembled into the reusable scratch
    /// buffer (NO throwaway heap string) and answered from the ObjMap — the twin
    /// of `get_index`'s borrowed fast path. Any miss / accessor / exotic shape
    /// falls back to materialising `prefix + key` and running `get_index`
    /// (byte-identical to the unfused `obj["prefix" + key]`).
    pub(crate) fn get_index_concat(
        &mut self,
        obj: Value,
        name: u32,
        key: Value,
        func_id: u32,
    ) -> Result<Value, Thrown> {
        if key.is_int() && obj.is_heap() {
            let oidx = obj.heap_index();
            if !(oidx == self.global_this && self.global_this != 0)
                && !(!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&oidx))
                && !(!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&oidx))
                && matches!(self.heap.get(oidx), HeapObj::Object(_))
            {
                let mut scratch = std::mem::take(&mut self.idx_key_scratch);
                self.build_concat_key(&mut scratch, name, key.as_int(), func_id);
                let hit = match self.heap.get(oidx) {
                    HeapObj::Object(m) => match m.pos(&scratch) {
                        Some(i) if !m.attrs[i].accessor && !m.vals[i].is_uninitialized() => {
                            Some(m.vals[i])
                        }
                        _ => None,
                    },
                    _ => None,
                };
                self.idx_key_scratch = scratch;
                if let Some(v) = hit {
                    return Ok(v);
                }
            }
        }
        // SLOW PATH: materialise the key exactly as `+` would and run the ordinary
        // computed read (proto chain, accessors, arrays, typed arrays, …).
        let full = self.concat_key_value(name, key, func_id)?;
        self.get_index(obj, full)
    }

    /// `obj[<string-const `name`> + key] = val` — the `SetIndexConcat` op; the
    /// `set_index` twin of `get_index_concat`. The fast path handles an own
    /// writable-data overwrite and a proven-clean new-key append without ever
    /// allocating the concat key on the heap (only the unavoidable owned ObjMap
    /// key String is cloned, exactly as `set_index` does). Everything else falls
    /// back to materialise + `set_index`.
    pub(crate) fn set_index_concat(
        &mut self,
        obj: Value,
        name: u32,
        key: Value,
        val: Value,
        strict: bool,
        func_id: u32,
    ) -> Result<(), Thrown> {
        if key.is_int() && obj.is_heap() {
            let idx = obj.heap_index();
            if !(idx == self.global_this && self.global_this != 0)
                && idx != self.obj_proto
                && self.realm_global_objs.is_empty()
                && !(!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&idx))
                && !(!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&idx))
                && matches!(self.heap.get(idx), HeapObj::Object(_))
            {
                let mut scratch = std::mem::take(&mut self.idx_key_scratch);
                self.build_concat_key(&mut scratch, name, key.as_int(), func_id);
                // hit: own writable data slot; add: proven-clean new key.
                let (hit, add) = match self.heap.get(idx) {
                    HeapObj::Object(m) if scratch != "__proto__" => match m.pos(&scratch) {
                        Some(i) if !m.attrs[i].accessor && m.attrs[i].writable => (Some(i), false),
                        Some(_) => (None, false),
                        None => (
                            None,
                            m.extensible
                                && m.class.is_none()
                                && !m.is_ctor
                                && self.plain_add_chain_clear(idx, &scratch),
                        ),
                    },
                    _ => (None, false),
                };
                if let Some(i) = hit {
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.vals[i] = val;
                        self.idx_key_scratch = scratch;
                        return Ok(());
                    }
                }
                if add {
                    let ks = scratch.clone(); // the unavoidable owned ObjMap key
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.push_data(ks, val);
                        self.heap.bump_version(idx); // key add reallocs vals (IC)
                        self.idx_key_scratch = scratch;
                        return Ok(());
                    }
                }
                self.idx_key_scratch = scratch;
            }
        }
        // SLOW PATH: materialise + ordinary computed write.
        let full = self.concat_key_value(name, key, func_id)?;
        self.set_index(obj, full, val, strict)
    }

    /// `dst = delete obj[<string-const `name`> + key]` — the `DeleteIndexConcat`
    /// op. An int key on a plain object deletes by `&str` from the scratch buffer
    /// (no concat alloc); a heap object is always object-coercible and
    /// ToPropertyKey of a string is identity, so both are skipped on the fast
    /// path. Anything else materialises the key and mirrors `DeleteIndex`.
    pub(crate) fn delete_index_concat(
        &mut self,
        obj: Value,
        name: u32,
        key: Value,
        strict: bool,
        func_id: u32,
    ) -> Result<Value, Thrown> {
        if key.is_int() && obj.is_heap() {
            let oidx = obj.heap_index();
            if !(oidx == self.global_this && self.global_this != 0)
                && !(!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&oidx))
                && !(!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&oidx))
                && matches!(self.heap.get(oidx), HeapObj::Object(_))
            {
                let mut scratch = std::mem::take(&mut self.idx_key_scratch);
                self.build_concat_key(&mut scratch, name, key.as_int(), func_id);
                let r = self.delete_property(obj, &scratch);
                let out = match r {
                    Ok(v) if strict && v == Value::bool(false) => {
                        Err(Thrown(format!("TypeError: Cannot delete property '{scratch}'")))
                    }
                    other => other,
                };
                self.idx_key_scratch = scratch;
                return out;
            }
        }
        // SLOW PATH: materialise + mirror the ordinary DeleteIndex op.
        let full = self.concat_key_value(name, key, func_id)?;
        let ks = self.to_property_key(full)?;
        self.require_object_coercible(obj)?;
        let r = self.delete_property(obj, &ks)?;
        if strict && r == Value::bool(false) {
            return Err(Thrown(format!("TypeError: Cannot delete property '{ks}'")));
        }
        Ok(r)
    }

    /// Assemble `"<string_constants[name]><int>"` into `buf` (cleared first) — the
    /// fused-key fast path's no-alloc key builder. The int's decimal form is
    /// pure ASCII, so the result is well-formed UTF-8.
    #[inline]
    pub(crate) fn build_concat_key(&self, buf: &mut String, name: u32, key: i32, func_id: u32) {
        buf.clear();
        buf.push_str(&self.func(func_id as usize).string_constants[name as usize]);
        let (digits, start) = crate::vm::coerce::fmt_i32_buf(key);
        // SAFETY: fmt_i32_buf yields only ASCII '-' and '0'..='9'.
        buf.push_str(unsafe { std::str::from_utf8_unchecked(&digits[start..]) });
    }

    /// Materialise the fused key as a real `prefix + key` Value, byte-identical to
    /// the unfused `"prefix" + key` (`Add`): the prefix constant is interned and
    /// `add_values` runs the standard `+` (so a non-int `key` gets full ToString /
    /// ToPrimitive semantics). Used by both fused ops' slow paths.
    #[inline]
    fn concat_key_value(&mut self, name: u32, key: Value, func_id: u32) -> Result<Value, Thrown> {
        let prefix = self.resolve_const(
            func_id,
            Value::heap(crate::vm::helpers_misc::STRING_CONST_BIT | name),
        );
        self.add_values(prefix, key)
    }

    /// `new Date(...)` → epoch ms. 0 args = now; 1 number = ms (time-clipped);
    /// 1 Date = copy; 1 string = parsed; ≥2 = UTC components (month0-based).
    pub(crate) fn date_new_ms(&mut self, args: &[Value]) -> Result<f64, Thrown> {
        match args.len() {
            0 => Ok(crate::vm::clock::now_epoch_ms()),
            1 => {
                let a = args[0];
                // `new Date(aDate)` copies its time value directly.
                if a.is_heap() {
                    if let HeapObj::Date(ms) = self.heap.get(a.heap_index()) {
                        return Ok(*ms);
                    }
                }
                // Otherwise ToPrimitive(default): a String is parsed; anything else
                // is ToNumber'd (so `new Date({valueOf:()=>1000})` works).
                let prim = self.to_primitive_default(a)?;
                if prim.is_heap()
                    && matches!(self.heap.get(prim.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. })
                {
                    let s = self.heap.str_cow(prim.heap_index()).unwrap().into_owned();
                    return Ok(parse_date(&s));
                }
                Ok(time_clip(self.to_number(prim)?))
            }
            _ => {
                let comp = self.date_components(args)?;
                Ok(match comp {
                    Some(mut c) => {
                        c[0] = legacy_year_f64(c[0]);
                        time_clip(ms_from_utc_f64(c[0], c[1], c[2], c[3], c[4], c[5], c[6]))
                    }
                    None => f64::NAN,
                })
            }
        }
    }

    /// Coerce up to 7 Date component args (y, mo0, day, h, mi, s, ms) via ToNumber
    /// — invoking each arg's `valueOf` in ORDER (all coerced even if an earlier one
    /// is NaN, so side effects match spec). Returns `None` if any component is NaN.
    /// Components stay in the Number domain (ToInteger-truncated f64) — MakeTime/
    /// MakeDay arithmetic is IEEE f64, and a value like 8e10 hours must not be
    /// squeezed through i64 day/ms math.
    fn date_components(&mut self, args: &[Value]) -> Result<Option<[f64; 7]>, Thrown> {
        let mut comp = [0.0f64, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let mut any_non_finite = false;
        for (i, &v) in args.iter().enumerate().take(7) {
            let n = self.to_number_coerce(v)?;
            // MakeDay/MakeTime/MakeDate require every component to be finite — an
            // Infinity / -Infinity / NaN field (e.g. `new Date(Infinity, 1, 70)`)
            // makes the whole time value NaN (an Invalid Date), not a clamped 0.
            if !n.is_finite() {
                any_non_finite = true;
            }
            comp[i] = if n.is_finite() { n.trunc() } else { 0.0 };
        }
        Ok(if any_non_finite { None } else { Some(comp) })
    }

    /// `Date.UTC(year, month0, …)` → epoch ms (NaN with no args / a NaN field).
    pub(crate) fn date_utc_ms(&mut self, args: &[Value]) -> Result<f64, Thrown> {
        if args.is_empty() {
            return Ok(f64::NAN);
        }
        Ok(match self.date_components(args)? {
            Some(mut c) => {
                c[0] = legacy_year_f64(c[0]);
                time_clip(ms_from_utc_f64(c[0], c[1], c[2], c[3], c[4], c[5], c[6]))
            }
            None => f64::NAN,
        })
    }

    /// Dispatch a method on a `Date` receiver (`idx` is its heap index). All
    /// getters/setters are UTC. Returns `Ok(None)` if `name` isn't a Date method.
    pub(crate) fn date_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let ms = match self.heap.get(idx) {
            HeapObj::Date(m) => *m,
            // thisTimeValue brand check: every Date.prototype method requires a
            // Date receiver (reached here only via the method-as-value path with a
            // non-Date `this`; the direct dispatch already matched HeapObj::Date).
            _ => return Err(Thrown("TypeError: this is not a Date object".into())),
        };
        let p = date_parts(ms); // (year, month0, day, hour, min, sec, ms, weekday)
        let field = |v: i64| if ms.is_nan() { Value::num(f64::NAN) } else { Value::num(v as f64) };
        let r = match name {
            "getTime" | "valueOf" => Value::num(ms),
            // `Date.prototype.toTemporalInstant()` → a Temporal.Instant at the date's
            // epoch nanoseconds (ms × 1e6); an invalid Date is a RangeError.
            "toTemporalInstant" => {
                if ms.is_nan() {
                    return Err(Thrown("RangeError: Invalid time value".into()));
                }
                return Ok(Some(self.make_instant((ms as i128) * 1_000_000)?));
            }
            "getFullYear" | "getUTCFullYear" => field(p.0),
            "getMonth" | "getUTCMonth" => field(p.1),
            "getDate" | "getUTCDate" => field(p.2),
            "getHours" | "getUTCHours" => field(p.3),
            "getMinutes" | "getUTCMinutes" => field(p.4),
            "getSeconds" | "getUTCSeconds" => field(p.5),
            "getMilliseconds" | "getUTCMilliseconds" => field(p.6),
            "getDay" | "getUTCDay" => field(p.7),
            "getTimezoneOffset" => Value::num(if ms.is_nan() { f64::NAN } else { 0.0 }),
            "toISOString" => {
                if ms.is_nan() {
                    return Err(Thrown("RangeError: Invalid time value".into()));
                }
                self.alloc_str(date_to_iso(ms))
            }
            "toJSON" => {
                if ms.is_nan() {
                    Value::NULL
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            // The human/RFC date string forms (the engine is UTC-only, so the
            // local `toString`/`toTimeString` zone is always GMT+0000). toGMTString
            // is a legacy (Annex B) alias of toUTCString; the toLocale* forms reuse
            // the corresponding non-locale formatter (no Intl locale data).
            "toString" | "toUTCString" | "toGMTString" | "toDateString" | "toTimeString"
            | "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" => {
                if ms.is_nan() {
                    self.alloc_str("Invalid Date".to_string())
                } else {
                    let s = match name {
                        "toDateString" | "toLocaleDateString" => date_to_date_string(ms),
                        "toTimeString" | "toLocaleTimeString" => date_to_time_string(ms),
                        "toUTCString" | "toGMTString" => date_to_utc_string(ms),
                        _ => date_to_string(ms), // toString | toLocaleString
                    };
                    self.alloc_str(s)
                }
            }
            // Legacy (Annex B): getYear = full year - 1900; setYear maps 0..99 to 19xx.
            "getYear" => field(p.0 - 1900),
            "setYear" => {
                let y = match args.first() {
                    Some(&v) => self.to_number_coerce(v)?,
                    None => f64::NAN,
                };
                if y.is_nan() {
                    if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                        *m = f64::NAN;
                    }
                    Value::num(f64::NAN)
                } else {
                    let yi = y as i64;
                    let full = if (0..=99).contains(&yi) { 1900 + yi } else { yi };
                    self.date_set(idx, &p, &[Value::num(full as f64)], 0)?
                }
            }
            "setTime" => {
                let n = match args.first() {
                    Some(&v) => time_clip(self.to_number_coerce(v)?),
                    None => f64::NAN,
                };
                if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                    *m = n;
                }
                Value::num(n)
            }
            "setFullYear" | "setUTCFullYear" => self.date_set(idx, &p, args, 0)?,
            "setMonth" | "setUTCMonth" => self.date_set(idx, &p, args, 1)?,
            "setDate" | "setUTCDate" => self.date_set(idx, &p, args, 2)?,
            "setHours" | "setUTCHours" => self.date_set(idx, &p, args, 3)?,
            "setMinutes" | "setUTCMinutes" => self.date_set(idx, &p, args, 4)?,
            "setSeconds" | "setUTCSeconds" => self.date_set(idx, &p, args, 5)?,
            "setMilliseconds" | "setUTCMilliseconds" => self.date_set(idx, &p, args, 6)?,
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    /// A Date setter starting at component `start` (0=year … 6=ms): overwrite that
    /// field and the following ones from `args`, recompute, store, return the new ms.
    pub(crate) fn date_set(
        &mut self,
        idx: u32,
        p: &(i64, i64, i64, i64, i64, i64, i64, i64),
        args: &[Value],
        start: usize,
    ) -> Result<Value, Thrown> {
        let orig_ms = match self.heap.get(idx) {
            HeapObj::Date(m) => *m,
            _ => f64::NAN,
        };
        let mut comp = [p.0, p.1, p.2, p.3, p.4, p.5, p.6];
        // Every component setter (setFullYear, setMonth, … setMilliseconds) has at
        // least one REQUIRED argument — the field at `start`. Calling it with no
        // args reads ToNumber(undefined) = NaN for that field, so the result is
        // NaN (Invalid Date). (Trailing optional args still default to the current
        // component value, handled by `comp` starting from the present parts.)
        let mut any_nan = args.is_empty();
        // Coerce ALL args (ToNumber, invoking valueOf in order) before deciding.
        for (i, &v) in args.iter().enumerate() {
            if start + i >= 7 {
                break;
            }
            let n = self.to_number_coerce(v)?;
            if n.is_nan() {
                any_nan = true;
            }
            comp[start + i] = if n.is_finite() { n as i64 } else { 0 };
        }
        // A component setter (setMonth..setMilliseconds, start>=1) on an Invalid
        // Date returns NaN — but per spec [[DateValue]] is read BEFORE ToNumber
        // and the method returns early WITHOUT writing, so a side effect the
        // argument's valueOf had on this Date (e.g. setTime) persists. setFullYear
        // (start==0) revives an invalid Date (t treated as +0), so no short-circuit.
        if orig_ms.is_nan() && start != 0 {
            return Ok(Value::num(f64::NAN));
        }
        let ms = if any_nan {
            f64::NAN
        } else {
            time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6]))
        };
        if let HeapObj::Date(m) = self.heap.get_mut(idx) {
            *m = ms;
        }
        Ok(Value::num(ms))
    }

}
