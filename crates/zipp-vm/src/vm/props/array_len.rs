// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// IsExtensible(O): a Proxy delegates to its trap; an ordinary object reads its
    /// `extensible` flag; an exotic object reads the arr_props side-table flag
    /// (default extensible). Used by the Proxy trap-invariant checks.
    pub(crate) fn is_extensible(&mut self, obj: Value) -> Result<bool, Thrown> {
        if let Some(b) = self.proxy_is_extensible(obj)? {
            return Ok(b);
        }
        if !obj.is_heap() {
            return Ok(false);
        }
        Ok(match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.extensible,
            _ => self.arr_props.get(&obj.heap_index()).map_or(true, |m| m.extensible),
        })
    }

    /// The per-index override for an array element, if `defineProperty` gave index
    /// `i` non-default attributes or an accessor. Stored in the `arr_props` side
    /// table keyed by the canonical decimal string; when present it is authoritative
    /// for that index (the dense slot is a placeholder kept only so `length` counts
    /// it). Returns `(attrs, stored)` — `stored` is the value, or the getter for an
    /// accessor. Cheap miss: arrays with no side table return `None` after one
    /// HashMap probe.
    pub(crate) fn array_index_override(&self, arr_idx: u32, i: usize) -> Option<(PropAttr, Value)> {
        let m = self.arr_props.get(&arr_idx)?;
        let mut buf = [0u8; 20];
        let p = m.pos(crate::heap::index_key(&mut buf, i))?;
        Some((m.attrs[p], m.vals[p]))
    }

    /// The JS `length` of the array at `arr_idx`: the dense element count, unless
    /// the array is SPARSE — its virtual length (always larger) is then recorded
    /// in the `array_js_len` side table. Result is `0..=2^32-1`.
    pub(crate) fn js_array_len(&self, arr_idx: u32) -> usize {
        if let Some(&n) = self.array_js_len.get(&arr_idx) {
            return n as usize;
        }
        match self.heap.get(arr_idx) {
            HeapObj::Array(items) => items.len(),
            _ => 0,
        }
    }

    /// Record that array index `i` now exists as an own property (a sparse write
    /// past the dense prefix): grow the virtual JS length to `i + 1` when that
    /// exceeds the current length. `i` is a valid array index (< 2^32-1), so the
    /// new length fits u32.
    pub(crate) fn array_grow_js_len(&mut self, arr_idx: u32, i: usize) {
        if i + 1 > self.js_array_len(arr_idx) {
            self.array_js_len.insert(arr_idx, (i + 1) as u32);
        }
    }

    /// ArraySetLength truncation blocker: the highest NON-configurable own array
    /// index `>= new_len` (it survives the shrink and the final length becomes
    /// blocker + 1). Only a defineProperty'd override in `arr_props` can be
    /// non-configurable, so a scan of its keys covers every candidate — including
    /// sparse-overlay indices far past the dense prefix (never walk the integer
    /// RANGE here: a virtual length can be 2^32-1).
    pub(crate) fn array_shrink_blocker(&self, arr_idx: u32, new_len: usize) -> Option<usize> {
        let m = self.arr_props.get(&arr_idx)?;
        m.keys
            .iter()
            .enumerate()
            .filter_map(|(i, k)| {
                // A spec array index is < 2^32-1: "4294967295"/"4294967296"
                // are ORDINARY named properties — they never block a shrink.
                canonical_index_str(k)
                    .filter(|ki| *ki < 4_294_967_295 && *ki >= new_len && !m.attrs[i].configurable)
            })
            .max()
    }

    /// Apply a VALIDATED new `length` to a real array: drop the (configurable)
    /// arr_props index entries `>= n`, resize the dense store, and keep the
    /// virtual-length side table consistent (entry present iff the JS length
    /// exceeds the dense element count). The caller has already run the ToUint32
    /// coercion, the writability check, and resolved any non-configurable
    /// blocker into `n`.
    pub(crate) fn array_apply_length(&mut self, arr_idx: u32, n: usize) {
        if let Some(m) = self.arr_props.get_mut(&arr_idx) {
            let doomed: Vec<String> = m
                .keys
                .iter()
                // Only true array indices (< 2^32-1) are swept by truncation;
                // "4294967295"/"4294967296" are ordinary named props and stay.
                .filter(|k| canonical_index_str(k).is_some_and(|i| i < 4_294_967_295 && i >= n))
                .cloned()
                .collect();
            for k in doomed {
                m.remove(&k);
            }
        }
        let dense_len = match self.heap.get(arr_idx) {
            HeapObj::Array(items) => items.len(),
            _ => 0,
        };
        if n <= dense_len || n <= crate::vm::MAX_DENSE_ARRAY_LEN {
            // Materialized: truncate, or extend with HOLES (absent elements).
            if let HeapObj::Array(items) = self.heap.get_mut(arr_idx) {
                items.resize(n, Value::HOLE);
            }
            self.array_js_len.remove(&arr_idx);
        } else {
            // Keep the dense prefix; the JS length lives in the side table.
            self.array_js_len.insert(arr_idx, n as u32);
        }
        self.heap.bump_version(arr_idx);
    }

    /// `Object.defineProperties(obj, props)` — define each own enumerable key of
    /// `props` as a descriptor on `obj`.
    pub(crate) fn object_define_properties(&mut self, obj: Value, props: Value) -> Result<(), Thrown> {
        // ObjectDefineProperties: props = ToObject(Properties) — null/undefined
        // throw (to_object boxes them like Object(), so guard first); other
        // primitives box (a String's index chars then fail ToPropertyDescriptor).
        self.require_object_coercible(props)?;
        let props = self.to_object(props)?;
        let pidx = props.heap_index();
        // ObjectDefineProperties step 3 is `props.[[OwnPropertyKeys]]()`, so the
        // walk is in OWN-KEY order — array indices ascending, then the string
        // keys in insertion order, then the symbol keys — NOT the raw insertion
        // order, in which zipp interleaves its `@@`-prefixed symbol keys with
        // the string ones. The difference is observable as the order of the
        // [[DefineOwnProperty]] calls on a Proxy target.
        let enum_keys = |m: &ObjMap| -> Vec<String> {
            let ordered = spec_key_order(&m.keys);
            let live = |i: &usize| m.attrs[*i].enumerable;
            ordered
                .iter()
                .filter(|i| live(i) && !is_hidden_key(&m.keys[**i]))
                .chain(ordered.iter().filter(|i| live(i) && is_hidden_key(&m.keys[**i])))
                .map(|i| m.keys[*i].clone())
                .collect()
        };
        // A PROXY descriptor bag follows the spec protocol exactly:
        // props.[[OwnPropertyKeys]]() (the ownKeys trap, or the target's
        // ordinary integer-first key order when absent), then per key
        // [[GetOwnProperty]] (the gopd trap — an undefined or non-enumerable
        // result skips the key), then Get + ToPropertyDescriptor, with ALL
        // descriptors collected/validated BEFORE any define lands on `obj`
        // (defineProperties/proxy-no-ownkeys-returned-keys-order).
        if self.proxy_parts(pidx).is_some() {
            // `keys`/`pending` hold Values across user trap calls — hold GC off.
            let _gc = self.gc_lock_guard();
            let keys_arr = self.object_own_keys(props)?;
            let keys = self.array_snapshot(keys_arr.heap_index());
            let mut pending: Vec<(String, Value)> = Vec::new();
            for kv in keys {
                let k = self.key_of(kv);
                let desc = self.proxy_gopd(props, &k)?.unwrap_or(Value::UNDEFINED);
                if desc == Value::UNDEFINED {
                    continue;
                }
                let en = self.get_prop(desc, "enumerable")?;
                if !self.truthy(en) {
                    continue;
                }
                let desc_obj = self.get_prop(props, &k)?;
                self.read_descriptor(desc_obj)?; // ToPropertyDescriptor (validation)
                pending.push((k, desc_obj));
            }
            for (k, d) in pending {
                self.object_define_property(obj, &k, d)?;
            }
            return Ok(());
        }
        // OwnPropertyKeys(ToObject(props)) filtered to enumerable. The descriptor
        // bag may be any object — a function (own props in fn_props) or an exotic
        // object (arr_props) — not only a plain Object.
        let keys: Vec<String> = match self.heap.get(pidx) {
            HeapObj::Object(m) => enum_keys(m),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => {
                self.fn_props.get(&pidx).map(enum_keys).unwrap_or_default()
            }
            // A boxed String (e.g. `Object.create(O, "abc")` → ToObject) exposes its
            // char indices as own enumerable keys; reading each yields a char string,
            // which then fails ToPropertyDescriptor (a non-object) → TypeError.
            HeapObj::Boxed { kind: 0, value } => {
                let v = *value;
                let n = self.heap.str_units(v.heap_index()).unwrap_or(0);
                let mut ks: Vec<String> = (0..n).map(|i| i.to_string()).collect();
                // Plus any own enumerable properties added to the String wrapper.
                if let Some(m) = self.arr_props.get(&pidx) {
                    ks.extend(enum_keys(m));
                }
                ks
            }
            _ => self.arr_props.get(&pidx).map(enum_keys).unwrap_or_default(),
        };
        for k in keys {
            let desc = self.get_prop(props, &k)?;
            self.object_define_property(obj, &k, desc)?;
        }
        Ok(())
    }

    /// The `(name, length)` of a callable value (function, closure, or class) for
    /// its `.name`/`.length` properties — `None` for non-callables. A synthetic
    /// proto name (`<arrow>`, `<script>`, …) reads as the empty string (anonymous).
    /// `globalThis.<name>`: the value of the reserved global slot named `name`
    /// (or None if there is no such global). Backs property access on globalThis.
    /// True when `name` is a top-level LEXICAL binding (let/const/class) of
    /// the main script or a $262.evalScript: a realm binding readable as a
    /// bare identifier, but NOT a global-object property.
    pub(crate) fn global_name_is_lexical(&self, name: &str) -> bool {
        if self.program.lexical_globals.is_empty() && self.eval_lexical_globals.is_empty() {
            return false;
        }
        let slot = if let Some(i) = self.program.global_names.iter().position(|n| n == name) {
            i as u32
        } else if let Some(&s) = self.eval_global_map.get(name) {
            s
        } else {
            return false;
        };
        self.program.lexical_globals.contains(&slot) || self.eval_lexical_globals.contains(&slot)
    }

    pub(crate) fn global_by_name(&self, name: &str) -> Option<Value> {
        // A built-in global removed via `delete globalThis.X` reads as absent
        // everywhere (get / has-own / descriptor all consult this).
        if self.deleted_globals.contains(name) {
            return None;
        }
        // A global LEXICAL is not a global-object property: invisible to
        // property reflection (gopd / hasOwnProperty / `globalThis.x` reads),
        // though the bare identifier still reads its slot. A PRE-EXISTING
        // builtin property of the same name (`let Array` shadowing %Array%)
        // remains an own property of the global object, so fall through to the
        // builtin table for those.
        if self.global_name_is_lexical(name) {
            if let Some(&idx) = self.builtin_globals.get(name) {
                return Some(Value::heap(idx));
            }
            return None;
        }
        if let Some(slot) = self.program.global_names.iter().position(|n| n == name) {
            // A never-declared slot reads as "absent"; fall through to the
            // built-in table below rather than exposing the internal sentinel.
            match self.globals.get(slot).copied() {
                Some(v) if v.is_uninitialized() => {}
                Some(v) => return Some(v),
                None => {}
            }
        }
        // Standard built-in globals (Object, Array, Math, eval, parseInt, …) are
        // own properties of the global object even when the running program never
        // referenced them as a bare identifier (so no compiler slot was reserved).
        if let Some(&idx) = self.builtin_globals.get(name) {
            return Some(Value::heap(idx));
        }
        // The value-properties of the global object (non-heap globals).
        match name {
            "NaN" => Some(Value::num(f64::NAN)),
            "Infinity" => Some(Value::num(f64::INFINITY)),
            "undefined" => Some(Value::UNDEFINED),
            _ => None,
        }
    }

    /// Look up `key` on a built-in prototype object (`arr_proto`/`str_proto`),
    /// returning the method value (or undefined). Lets primitive array/string
    /// values expose their methods as first-class values.
    /// The effective prototype for an Array's method/inherited-index resolution: a
    /// `class extends Array` instance records its own (subclass) prototype in proto_of
    /// (which chains to %Array.prototype%); a plain array has no entry → %Array.prototype%.
    pub(crate) fn array_eff_proto(&self, idx: u32) -> u32 {
        self.proto_of
            .get(&idx)
            .and_then(|p| p.is_heap().then(|| p.heap_index()))
            .unwrap_or(self.arr_proto)
    }

    /// Flag that `Array.prototype` / `Object.prototype` now carries an integer
    /// index, so array index assignment must consult the prototype chain (the
    /// `array_proto_has_index` perf guard, read by `set_index`).
    /// A module-namespace TDZ guard for FALLIBLE reflective entry points:
    /// reading the descriptor/value of an UNINITIALIZED export throws.
    pub(crate) fn ns_tdz_check(&self, obj: Value, key: &str) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Ok(());
        }
        if let Some(slot) =
            self.module_namespaces.get(&obj.heap_index()).and_then(|m| m.get(key)).copied()
        {
            let live = self.globals.get(slot as usize).copied().unwrap_or(Value::UNDEFINED);
            if live.is_uninitialized() {
                return Err(Thrown(format!(
                    "ReferenceError: Cannot access '{key}' before initialization"
                )));
            }
        }
        Ok(())
    }

    /// EnumerableOwnProperties / EnumerateObjectProperties over a namespace
    /// call [[GetOwnProperty]] per key — the FIRST uninitialized export (in
    /// sorted ownKeys order) throws.
    pub(crate) fn ns_tdz_check_all(&self, obj: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Ok(());
        }
        if let Some(m) = self.module_namespaces.get(&obj.heap_index()) {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                let live =
                    self.globals.get(m[k] as usize).copied().unwrap_or(Value::UNDEFINED);
                if live.is_uninitialized() {
                    return Err(Thrown(format!(
                        "ReferenceError: Cannot access '{k}' before initialization"
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn note_array_proto_index(&mut self, obj_idx: u32, key: &str) {
        if (obj_idx == self.arr_proto || obj_idx == self.obj_proto)
            && canonical_index_str(key).is_some()
        {
            self.array_proto_has_index = true;
            // %Array.prototype% is an Array exotic: an index definition on it
            // grows its own `length` (ArraySetLength step for index defines).
            if obj_idx == self.arr_proto {
                if let Some(i) = canonical_index_str(key) {
                    let want = (i as u64 + 1).min(u32::MAX as u64) as u32;
                    if want > self.arr_proto_len {
                        self.arr_proto_len = want;
                    }
                }
            }
        }
    }

    /// OrdinarySet's prototype step for an array index `i` absent as an own property:
    /// walk the array's prototype chain (incl. Object.prototype). A prototype own
    /// ACCESSOR at `i` → invoke its setter with `obj` as receiver, return Ok(true)
    /// (handled, no own property created). A prototype non-writable own DATA prop at
    /// `i` → reject (Ok(true)). Otherwise Ok(false) → the caller creates the own data
    /// property. Only called when `array_proto_has_index` is set.
    pub(crate) fn array_proto_set_step(
        &mut self,
        obj: Value,
        i: usize,
        val: Value,
        strict: bool,
    ) -> Result<bool, Thrown> {
        let key = i.to_string();
        let mut cur = match self.object_get_prototype_of(obj) {
            p if p.is_heap() => p.heap_index(),
            _ => return Ok(false),
        };
        let mut guard = 0u32;
        let mut saw_obj_proto = false;
        while cur != 0 && guard < 64 {
            guard += 1;
            saw_obj_proto |= cur == self.obj_proto;
            // A Proxy chain node: parent.[[Set]](P, V, Receiver) — its `set`
            // trap fires with the ORIGINAL receiver (call-parameters-
            // prototype-index); with no trap, [[Set]] forwards to the proxy's
            // target — continue the walk from there with the same receiver.
            if self.proxy_parts(cur).is_some() {
                match self.proxy_set_bool(Value::heap(cur), &key, val, obj)? {
                    Some(true) => return Ok(true),
                    Some(false) => {
                        self.reject_write(&key, strict)?;
                        return Ok(true);
                    }
                    None => {
                        cur = match self.proxy_parts(cur).map(|(t, _, _)| t) {
                            Some(t) if t.is_heap() => t.heap_index(),
                            _ => 0,
                        };
                        continue;
                    }
                }
            }
            // A TypedArray chain node absorbs an INVALID index silently (report
            // handled — no write, no coercion, no reject); a valid index is a
            // plain writable data prop, so the caller writes the own element.
            if matches!(self.heap.get(cur), HeapObj::TypedArray { .. }) {
                return Ok(self.ta_valid_index(cur, &key).is_none());
            }
            if let Some((attr, raw)) = self.own_member(cur, &key) {
                return self.apply_proto_set(attr, raw, obj, &key, val, strict);
            }
            cur = match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => 0,
            };
        }
        // Type prototypes may not record proto_of -> Object.prototype explicitly.
        if !saw_obj_proto && self.obj_proto != 0 {
            if let Some((attr, raw)) = self.own_member(self.obj_proto, &key) {
                return self.apply_proto_set(attr, raw, obj, &key, val, strict);
            }
        }
        Ok(false)
    }

    pub(crate) fn apply_proto_set(
        &mut self,
        attr: PropAttr,
        _raw: Value,
        obj: Value,
        key: &str,
        val: Value,
        strict: bool,
    ) -> Result<bool, Thrown> {
        if attr.accessor {
            // For a SET, the setter is `attr.setter` (the `raw` value is the getter).
            if attr.setter == Value::UNDEFINED {
                self.reject_write(key, strict)?;
            } else {
                self.call_value(attr.setter, obj, &[val])?;
            }
            Ok(true)
        } else if !attr.writable {
            self.reject_write(key, strict)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

}
