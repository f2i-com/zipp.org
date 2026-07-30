// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// CreateDataPropertyOrThrow for a class FIELD: an own {value,
    /// writable:true, enumerable:true, configurable:true} data property on the
    /// receiver, never consulting prototype setters; a Proxy receiver's
    /// defineProperty trap fires (and its throw propagates).
    pub(crate) fn define_field(
        &mut self,
        target: Value,
        key: &str,
        value: Value,
    ) -> Result<(), Thrown> {
        self.defer_check(target, key)?; // CreateDataProperty = [[DefineOwnProperty]]
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::TRUE);
        m.set("enumerable", Value::TRUE);
        m.set("configurable", Value::TRUE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
        self.object_define_property(target, key, desc)
    }

    /// `Object.defineProperty(obj, key, descriptor)` — define/redefine an own
    /// property with explicit attributes (unspecified attrs default to false on a
    /// new property; an existing non-configurable property rejects most changes).
    pub(crate) fn object_define_property(&mut self, obj: Value, key: &str, desc: Value) -> Result<(), Thrown> {
        self.defer_check(obj, key)?;
        if !obj.is_heap() {
            return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
        }
        self.note_array_proto_index(obj.heap_index(), key);
        // …and the sibling protector: a real own descriptor on the GLOBAL object
        // shadowing a slot-backed binding turns every store to that slot into an
        // OrdinarySet, which a compiled raw slot write would bypass.
        self.note_global_own_property_change(obj.heap_index(), key);
        // ── materialize a slot-only global binding before redefining it ──
        // The global object's properties and the global slots are two views of one
        // set of bindings, but only the slot view exists for `foo = 1` / `var foo`.
        // So a define here was a CREATE, not a REDEFINE: it built a fresh property
        // from the incoming descriptor alone, and a descriptor that omits `value`
        // (`{writable: false}`) produced a property holding `undefined` and lost the
        // binding's enumerability.
        //
        // Writing the current descriptor into the ObjMap first makes the ordinary
        // redefine path below correct by construction — ValidateAndApplyProperty-
        // Descriptor keeps every field the incoming descriptor omits, which is
        // exactly the rule that was missing. Guarded on there being no entry yet, so
        // a second define is an ordinary redefine and this is a no-op.
        if self.global_this != 0 && obj.heap_index() == self.global_this {
            let absent =
                matches!(self.heap.get(self.global_this), HeapObj::Object(m) if m.pos(key).is_none());
            if absent {
                if let Some((v, a)) = self.global_slot_binding_descriptor(key) {
                    if let HeapObj::Object(m) = self.heap.get_mut(self.global_this) {
                        m.define(key, v, a);
                    }
                }
            }
        }
        // Module Namespace exotic [[DefineOwnProperty]]: true ONLY when the
        // descriptor is compatible with the live binding ({value: current,
        // writable:true, enumerable:true, configurable:false}, data); any
        // change request is rejected (Object.defineProperty throws;
        // Reflect.defineProperty maps the throw to false).
        if self.module_namespaces.contains_key(&obj.heap_index()) {
            let nsidx = obj.heap_index();
            let (value, get, set, wr, en, cf) = self.read_descriptor(desc)?;
            let reject = |k: &str| -> Result<(), Thrown> {
                Err(Thrown(format!(
                    "TypeError: Cannot redefine property: '{k}' of a module namespace"
                )))
            };
            if get.is_some() || set.is_some() || cf == Some(true) {
                return reject(key);
            }
            if key == "@@toStringTag" {
                // Own {value:"Module", writable:false, enumerable:false,
                // configurable:false}: only matching/absent fields pass.
                if wr == Some(true) || en == Some(true) {
                    return reject(key);
                }
                if let Some(v) = value {
                    let cur = match self.heap.get(nsidx) {
                        HeapObj::Object(m) => m.get("@@toStringTag").unwrap_or(Value::UNDEFINED),
                        _ => Value::UNDEFINED,
                    };
                    if !self.same_value(v, cur) {
                        return reject(key);
                    }
                }
                return Ok(());
            }
            // An exported string key: {live value, writable:true,
            // enumerable:true, configurable:false}; anything absent rejects.
            let key_slot =
                self.module_namespaces.get(&nsidx).and_then(|m| m.get(key)).copied();
            let Some(slot) = key_slot else {
                return reject(key);
            };
            if wr == Some(false) || en == Some(false) {
                return reject(key);
            }
            if let Some(v) = value {
                let live =
                    self.globals.get(slot as usize).copied().unwrap_or(Value::UNDEFINED);
                if !self.same_value(v, live) {
                    return reject(key);
                }
            }
            return Ok(());
        }
        // Proxy defineProperty trap: pass the trap a FromPropertyDescriptor of the
        // attributes (only the specified fields); a falsy result means the define
        // failed (Object.defineProperty throws, Reflect.defineProperty -> false).
        if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
            if revoked {
                return Err(Thrown(
                    "TypeError: Cannot perform 'defineProperty' on a revoked proxy".into(),
                ));
            }
            return match self.proxy_trap(handler, "defineProperty")? {
                None => self.object_define_property(target, key, desc),
                Some(trap) => {
                    let (value, get, set, wr, en, cf) = self.read_descriptor(desc)?;
                    let mut m = ObjMap::new();
                    if let Some(v) = value {
                        m.set("value", v);
                    }
                    if let Some(w) = wr {
                        m.set("writable", Value::bool(w));
                    }
                    if let Some(g) = get {
                        m.set("get", g);
                    }
                    if let Some(s) = set {
                        m.set("set", s);
                    }
                    if let Some(e) = en {
                        m.set("enumerable", Value::bool(e));
                    }
                    if let Some(c) = cf {
                        m.set("configurable", Value::bool(c));
                    }
                    let desc_obj = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
                    let kv = self.key_to_value(key);
                    let r = self.call_value(trap, handler, &[target, kv, desc_obj])?;
                    if !self.truthy(r) {
                        return Err(Thrown(format!(
                            "TypeError: proxy 'defineProperty' trap returned falsish for property '{key}'"
                        )));
                    }
                    // [[DefineOwnProperty]] invariant (10.5.6 steps 16-21): validate
                    // the truthy trap result against the target's descriptor —
                    // ? target.[[GetOwnProperty]](P), so a Proxy target recurses
                    // through ITS trap rather than reading past it.
                    let target_desc = match self.proxy_gopd(target, key)? {
                        Some(d) => d,
                        None => self.object_get_own_property_descriptor(target, key),
                    };
                    let extensible = self.is_extensible(target)?;
                    let setting_config_false = cf == Some(false);
                    if target_desc == Value::UNDEFINED {
                        if !extensible {
                            return Err(Thrown(
                                "TypeError: proxy can't define a property on a non-extensible target".into(),
                            ));
                        }
                        if setting_config_false {
                            return Err(Thrown(
                                "TypeError: proxy can't define a non-configurable property absent from the target".into(),
                            ));
                        }
                    } else {
                        let t_cfg = self.get_prop(target_desc, "configurable")?;
                        let t_configurable = self.truthy(t_cfg);
                        if setting_config_false && t_configurable {
                            return Err(Thrown(
                                "TypeError: proxy can't redefine a configurable target property as non-configurable".into(),
                            ));
                        }
                        if !t_configurable {
                            if cf == Some(true) {
                                return Err(Thrown(
                                    "TypeError: proxy can't redefine a non-configurable target property as configurable".into(),
                                ));
                            }
                            let t_wr = self.get_prop(target_desc, "writable")?;
                            let t_writable = self.truthy(t_wr);
                            // Step 16.c: a non-configurable WRITABLE data prop
                            // cannot be reported non-writable by the trap.
                            let t_is_data = self.has_property_str(target_desc, "writable");
                            if t_is_data && t_writable && wr == Some(false) {
                                return Err(Thrown(
                                    "TypeError: proxy can't report a non-configurable writable target property as non-writable".into(),
                                ));
                            }
                            if !t_writable {
                                if wr == Some(true) {
                                    return Err(Thrown(
                                        "TypeError: proxy can't make a non-configurable non-writable target property writable".into(),
                                    ));
                                }
                                if let Some(v) = value {
                                    let t_val = self.get_prop(target_desc, "value")?;
                                    if !self.same_value(v, t_val) {
                                        return Err(Thrown(
                                            "TypeError: proxy can't change the value of a non-configurable non-writable target property".into(),
                                        ));
                                    }
                                }
                            }
                        }
                        // Step 17.a proper: the clauses above are the frequently-hit
                        // subset (they keep their precise messages); the general
                        // IsCompatiblePropertyDescriptor catches what they miss — an
                        // enumerable flip, a data/accessor kind change, and a
                        // getter/setter swap on a non-configurable target property.
                        if !self.is_compatible_property_descriptor(
                            extensible,
                            (value, get, set, wr, en, cf),
                            target_desc,
                        )? {
                            return Err(Thrown(
                                "TypeError: proxy 'defineProperty' reported a descriptor incompatible with the target's".into(),
                            ));
                        }
                    }
                    Ok(())
                }
            };
        }
        let idx = obj.heap_index();
        // Array: a numeric index honours the FULL descriptor (attributes +
        // accessors), `length` resizes, a named key falls through to the generic
        // arr_props path. A fully-default DATA descriptor lives in the dense Vec
        // (the fast common case); any non-default attribute / accessor is stored in
        // the arr_props side table keyed by the index, which then overrides the
        // dense slot for reads/writes/descriptors (the dense slot is kept as a
        // placeholder so `length` still counts the index).
        if let HeapObj::Array(_) = self.heap.get(idx) {
            // Only a CANONICAL decimal (`"0"`, `"10"`) is an array index; `"00"` /
            // `" 1"` are ordinary named properties → generic path.
            if let Ok(i) = key.parse::<usize>() {
                // A canonical ARRAY index is 0..2^32-2; `"4294967295"` (2^32-1) and
                // beyond are ORDINARY named properties → fall through to the generic
                // arr_props path (not a dense slot, no dense-limit error).
                if i.to_string() == key && i < 0xFFFF_FFFF {
                    let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
                    let key_i = i.to_string();
                    // Current descriptor of index i: a special arr_props entry wins;
                    // else the dense slot (a default data property) if in range.
                    let (dense_len, dense_val) = match self.heap.get(idx) {
                        HeapObj::Array(items) => (items.len(), items.get(i).copied()),
                        _ => (0, None),
                    };
                    // Array [[DefineOwnProperty]] for an array index P=n: if n >= the
                    // current JS length and `length` is non-writable (defineProperty /
                    // freeze), the define fails — it would grow `length` (ArraySetLength
                    // forbidden). A sparse array's length is the VIRTUAL one (an index
                    // below it doesn't grow anything). A TypeError from
                    // DefinePropertyOrThrow; the array is left unchanged.
                    if i >= self.js_array_len(idx) && self.array_length_nonwritable.contains(&idx)
                    {
                        return Err(Thrown(format!(
                            "TypeError: Cannot define property {i}: array length is not writable"
                        )));
                    }
                    let plain = PropAttr {
                        writable: true,
                        enumerable: true,
                        configurable: true,
                        accessor: false,
                        setter: Value::UNDEFINED,
                    };
                    let existing = self
                        .array_index_override(idx, i)
                        .or_else(|| dense_val.map(|v| (plain, v)));
                    // ArgumentsObject [[DefineOwnProperty]] on a LIVE-mapped
                    // index: the formal's register is the property's CURRENT
                    // value (validation against a non-configurable existing
                    // must compare against it), and a data redefine to
                    // writable:false with NO [[Value]] freezes AT that live
                    // value (newArgDesc.[[Value]] = map.Get(P)).
                    let mapped_v = self.args_mapped_get(idx, i);
                    let existing = match (existing, mapped_v) {
                        (Some((a, _)), Some(mv)) => Some((a, mv)),
                        (e, _) => e,
                    };
                    let desc_value = value;
                    let value = if mapped_v.is_some()
                        && get.is_none()
                        && set.is_none()
                        && value.is_none()
                        && d_wr == Some(false)
                    {
                        mapped_v
                    } else {
                        value
                    };
                    let extensible = self.arr_props.get(&idx).map_or(true, |m| m.extensible);
                    let (attr, stored) = self.merge_property_descriptor(
                        &key_i, existing, extensible, value, get, set, d_wr, d_en, d_cf,
                    )?;
                    if mapped_v.is_some() {
                        if get.is_some() || set.is_some() {
                            // An accessor redefine severs the alias (no flush —
                            // the accessor replaces the data value entirely).
                            self.args_unmap(idx, i, false);
                        } else {
                            // map.Set BEFORE a writable:false sever, so the
                            // formal observes the final defined value.
                            if let Some(v) = desc_value {
                                self.args_mapped_set(idx, i, v);
                            }
                            if d_wr == Some(false) {
                                // `stored` (merged above) already carries the
                                // correct frozen value into the ordinary store.
                                self.args_unmap(idx, i, false);
                            }
                        }
                    }
                    let is_default_data = !attr.accessor
                        && attr.writable
                        && attr.enumerable
                        && attr.configurable;
                    // An ARGUMENTS object: an index define past the dense
                    // window is an ordinary named own property — `length`
                    // (items.len()) must stay argc, so never grow the Vec.
                    let args_past = i >= dense_len && self.arguments_objs.contains_key(&idx);
                    // Past the eager-materialization cap AND the dense prefix:
                    // a SPARSE define — the entry lives in arr_props with NO
                    // dense placeholder (the Vec is never resized to billions
                    // of holes); `length` comes from the virtual side table.
                    let sparse_target =
                        i >= crate::vm::MAX_DENSE_ARRAY_LEN && i >= dense_len && !args_past;
                    if is_default_data && !args_past && !sparse_target {
                        // Lives in the dense Vec; drop any stale special override.
                        if let Some(m) = self.arr_props.get_mut(&idx) {
                            m.remove(&key_i);
                        }
                        if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                            if i >= items.len() {
                                // Intervening slots are HOLES (absent), per spec.
                                items.resize(i + 1, Value::HOLE);
                            }
                            items[i] = stored;
                        }
                    } else {
                        // Special: store in arr_props and keep a dense placeholder so
                        // `length` counts the index (NOT for arguments or a sparse
                        // index — see above).
                        if i >= dense_len && !args_past && !sparse_target {
                            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                                // Intervening slots are HOLES; the slot at `i`
                                // itself is only a placeholder (presence comes
                                // from the arr_props override entry).
                                items.resize(i + 1, Value::HOLE);
                            }
                        }
                        self.arr_props
                            .entry(idx)
                            .or_insert_with(ObjMap::new_side_table)
                            .define(&key_i, stored, attr);
                        if sparse_target {
                            self.array_grow_js_len(idx, i);
                        }
                    }
                    self.heap.bump_version(idx);
                    return Ok(());
                }
            }
            if key == "length" && self.arguments_objs.contains_key(&idx) {
                // An ARGUMENTS object's `length`: ORDINARY define on the
                // arr_props prop; a plain numeric [[Value]] also resizes the
                // dense store so iteration/concat (which read it) follow.
                let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
                let cur = self
                    .arr_props
                    .get(&idx)
                    .and_then(|m| m.pos("length").map(|i| (m.attrs[i], m.vals[i])));
                if let Some((a, curv)) = cur {
                    if !a.configurable {
                        let changes = d_cf == Some(true)
                            || d_en.is_some_and(|e| e != a.enumerable)
                            || get.is_some()
                            || set.is_some()
                            || (!a.writable
                                && (d_wr == Some(true)
                                    || value.is_some_and(|v| !self.same_value(v, curv))));
                        if changes {
                            return Err(Thrown(
                                "TypeError: Cannot redefine property: length".into(),
                            ));
                        }
                    }
                }
                let new_attr = PropAttr {
                    writable: d_wr.unwrap_or(cur.map_or(false, |(a, _)| a.writable)),
                    enumerable: d_en.unwrap_or(cur.map_or(false, |(a, _)| a.enumerable)),
                    configurable: d_cf.unwrap_or(cur.map_or(false, |(a, _)| a.configurable)),
                    accessor: get.is_some() || set.is_some(),
                    setter: set.unwrap_or(Value::UNDEFINED),
                };
                let stored = if new_attr.accessor {
                    get.unwrap_or(Value::UNDEFINED)
                } else {
                    value.unwrap_or(cur.map_or(Value::UNDEFINED, |(_, v)| v))
                };
                let m = self.arr_props.entry(idx).or_insert_with(ObjMap::new_side_table);
                m.define("length", stored, new_attr);
                if !new_attr.accessor {
                    if let Some(v) = value {
                        if v.is_number() {
                            let n = v.as_f64();
                            if n >= 0.0
                                && n.fract() == 0.0
                                && (n as usize) <= crate::vm::MAX_DENSE_ARRAY_LEN
                            {
                                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                                    items.resize(n as usize, Value::HOLE);
                                }
                            }
                        }
                    }
                }
                self.heap.bump_version(idx);
                return Ok(());
            }
            if key == "length" && !self.arguments_objs.contains_key(&idx) {
                // `length` is a non-configurable, non-enumerable data property
                // (ArraySetLength, 15.4.5.1) — writable by default.
                let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
                // ArraySetLength steps 2-5: with a [[Value]], newLen = ToUint32(value)
                // and numberLen = ToNumber(value) are BOTH computed (so valueOf runs
                // TWICE), and `newLen != numberLen` (a non-uint32 length like -1 / NaN
                // / >=2^32) is a RangeError — BEFORE the attribute / writability checks.
                let new_len: Option<usize> = if let Some(v) = value {
                    let nu = self.to_number_coerce(v)?; // ToNumber inside ToUint32
                    let u = if nu.is_finite() { (nu.trunc() as i64 as u32) as f64 } else { 0.0 };
                    let number_len = self.to_number_coerce(v)?; // numberLen = ToNumber(value)
                    if u != number_len {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    Some(u as usize)
                } else {
                    None
                };
                // Reject making `length` configurable or enumerable, or an accessor.
                if get.is_some() || set.is_some() || d_cf == Some(true) || d_en == Some(true) {
                    return Err(Thrown("TypeError: Cannot redefine property: length".into()));
                }
                let cur_writable = !self.array_length_nonwritable.contains(&idx);
                let cur_len = self.js_array_len(idx);
                // A non-configurable, NON-writable `length`: it can't be made writable
                // again, and its value can only be "redefined" to the SAME length.
                if !cur_writable {
                    if d_wr == Some(true) {
                        return Err(Thrown("TypeError: Cannot redefine property: length".into()));
                    }
                    if let Some(nl) = new_len {
                        if nl != cur_len {
                            return Err(Thrown(
                                "TypeError: Cannot redefine property: length".into(),
                            ));
                        }
                    }
                    return Ok(());
                }
                if let Some(new_len) = new_len {
                    // Shrinking past a NON-configurable index does a PARTIAL shrink:
                    // delete the deletable indices above it, stop the length at
                    // blocker+1, set the length non-writable if requested, THEN throw
                    // (ArraySetLength steps 16-17). `effective_len` is where the length
                    // actually lands; `blocked` records that a TypeError is due.
                    // The blocker scan walks arr_props KEYS (never the integer range —
                    // a sparse array's length can be 2^32-1).
                    let mut effective_len = new_len;
                    let mut blocked = false;
                    if new_len < cur_len {
                        if let Some(ki) = self.array_shrink_blocker(idx, new_len) {
                            effective_len = ki + 1;
                            blocked = true;
                        }
                    }
                    // Sweeps the doomed arr_props index entries, resizes the dense
                    // store, and keeps the virtual length (a sparse `new_len` past
                    // the dense cap) consistent.
                    self.array_apply_length(idx, effective_len);
                    if blocked {
                        // A partial shrink still applies `writable:false` (newWritable)
                        // before throwing.
                        if d_wr == Some(false) {
                            self.array_length_nonwritable.insert(idx);
                        }
                        return Err(Thrown(
                            "TypeError: Cannot redefine property: length".into(),
                        ));
                    }
                }
                // Record a newly non-writable length (writable was true above) so
                // future writes / mutators / the descriptor honour it.
                if d_wr == Some(false) {
                    self.array_length_nonwritable.insert(idx);
                }
                return Ok(());
            }
            // else: named key → generic path.
        }
        // TypedArray integer-indexed [[DefineOwnProperty]] (ES 10.4.5.3). A
        // CanonicalNumericIndexString key is absorbed by the exotic behaviour:
        //   * read the descriptor FIRST (ToPropertyDescriptor may run getters that
        //     detach/resize the buffer), THEN re-check IsValidIntegerIndex;
        //   * an out-of-range / non-integer index (or detached buffer) -> false;
        //   * the slot is configurable/enumerable/writable data only, so an
        //     accessor or a {configurable|enumerable|writable: false} descriptor
        //     is rejected; a present `value` is written via IntegerIndexedElementSet.
        // A NON-numeric key (`ta.foo`) falls through to the generic named-property
        // path below (arr_props).
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. })
            && self.is_canonical_numeric_index(key)
        {
            let (value, get, set, wr, en, cf) = self.read_descriptor(desc)?;
            let valid_i = self.ta_valid_index(idx, key);
            if valid_i.is_none()
                || get.is_some()
                || set.is_some()
                || cf == Some(false)
                || en == Some(false)
                || wr == Some(false)
            {
                return Err(Thrown(format!(
                    "TypeError: cannot define property '{key}' on a TypedArray"
                )));
            }
            if let Some(v) = value {
                self.ta_element_set(idx, valid_i.unwrap(), v)?;
            }
            return Ok(());
        }
        // A String WRAPPER's char indices and `length` are exotic NON-WRITABLE,
        // NON-CONFIGURABLE data properties: a redefinition must satisfy
        // ValidateAndApplyPropertyDescriptor against that descriptor (same
        // value, no accessor, no flag escalation) — else TypeError.
        if matches!(self.heap.get(idx), HeapObj::Boxed { kind: 0, .. }) {
            if let Some((sval, len)) = self.string_exotic_chars(obj) {
                let is_char =
                    key.parse::<usize>().is_ok_and(|i| i.to_string() == key && i < len);
                if is_char || key == "length" {
                    let cur = if is_char {
                        let i: usize = key.parse().unwrap();
                        self.get_index(sval, Value::num(i as f64)).unwrap_or(Value::UNDEFINED)
                    } else {
                        Value::num(len as f64)
                    };
                    let (value, get, set, wr, en, cf) = self.read_descriptor(desc)?;
                    let bad = get.is_some()
                        || set.is_some()
                        || cf == Some(true)
                        || wr == Some(true)
                        || (is_char && en == Some(false))
                        || (!is_char && en == Some(true))
                        || value.is_some_and(|v| !self.same_value(v, cur));
                    if bad {
                        return Err(Thrown(format!(
                            "TypeError: Cannot redefine property: {key}"
                        )));
                    }
                    return Ok(());
                }
            }
        }
        // 0 = plain object, 1 = class (own props live in `statics`), 2 = callable
        // (own props live in `fn_props`), 3 = the generic side table `arr_props`
        // (array named props + every other exotic object: Map/Set/Date/Promise/
        // RegExp/Weak*/…). String/Symbol/BigInt are PRIMITIVES → "non-object".
        let target = match self.heap.get(idx) {
            HeapObj::Object(_) => 0u8,
            HeapObj::Class(_) => 1,
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => 2,
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
            }
            _ => 3, // Array named prop + exotic objects -> arr_props side table
        };
        // A callable's/class's `prototype` is synthesized; accept the redefinition
        // but don't shadow it (full `prototype` redefinition isn't modelled). `name`
        // and `length` ARE redefinable: they fall through and are stored as explicit
        // own props (fn_props for a function, class statics), seeding `existing` from
        // the synthesized intrinsic below so a value-only redefine keeps the
        // {writable:false, enumerable:false, configurable:true} attrs and counts as a
        // redefinition (not a brand-new property the extensible check could block).
        // Only a callable/class that OWNS the synthesized `prototype` (ordinary
        // function / class) ignores a redefinition. A bound function / arrow / async
        // / method has no intrinsic `prototype`, so an explicit one is a real own
        // property (stored in fn_props below).
        if target != 0 && key == "prototype" && self.callable_has_prototype(obj) {
            // The synthesized `prototype` is {writable:true, enumerable:false,
            // configurable:false}: converting it to an ACCESSOR — or making it
            // configurable/enumerable — is an illegal non-configurable
            // redefinition (a data-value redefine stays accepted-but-unmodelled).
            let (_v, get, set, _wr, d_en, d_cf) = self.read_descriptor(desc)?;
            if get.is_some() || set.is_some() || d_cf == Some(true) || d_en == Some(true) {
                return Err(Thrown(
                    "TypeError: Cannot redefine property: prototype".into(),
                ));
            }
            return Ok(());
        }
        let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
        // The existing descriptor lives wherever `target` writes (below).
        let mut existing = match target {
            0 => match self.heap.get(idx) {
                HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
                _ => None,
            },
            1 => match self.heap.get(idx) {
                HeapObj::Class(c) => c.statics.pos(key).map(|i| (c.statics.attrs[i], c.statics.vals[i])),
                _ => None,
            },
            3 => self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
            _ => self.fn_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
        };
        // A RegExp's `lastIndex` is a struct-backed own data property (default
        // {writable:true, enumerable:false, configurable:false}; only attr
        // overrides live in arr_props, the VALUE's source of truth is the slot).
        // Seed `existing` from the slot so the merge validates against the real
        // descriptor; the merged value is written back through to the slot below.
        let regexp_last_index =
            key == "lastIndex" && matches!(self.heap.get(idx), HeapObj::RegExp { .. });
        if regexp_last_index {
            if let HeapObj::RegExp { last_index, .. } = self.heap.get(idx) {
                let li = *last_index;
                let attrs = existing.map(|(a, _)| a).unwrap_or(PropAttr {
                    writable: true,
                    enumerable: false,
                    configurable: false,
                    accessor: false,
                    setter: Value::UNDEFINED,
                });
                existing = Some((attrs, li));
            }
        }
        // A first `name`/`length` redefine on a callable/class: seed `existing` from
        // the synthesized intrinsic (a configurable data property) so the merge
        // treats it as a redefinition that preserves writable:false/enumerable:false.
        if existing.is_none()
            && (target == 1 || target == 2)
            && matches!(key, "name" | "length")
            && self.callable_has_intrinsic(obj, key)
        {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                existing = Some((
                    PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: false,
                        setter: Value::UNDEFINED,
                    },
                    v,
                ));
            }
        }
        let extensible = if target == 3 {
            // Exotic objects (TypedArray / Map / Set / Date / RegExp / … whose named
            // own props live in the arr_props side table) keep their extensible flag
            // there — set by Object.preventExtensions/seal/freeze; default true.
            self.arr_props.get(&idx).map_or(true, |m| m.extensible)
        } else {
            match self.heap.get(idx) {
                HeapObj::Object(m) => m.extensible,
                // A CALLABLE (or class) keeps its own props in fn_props, but
                // preventExtensions/seal/freeze record its [[Extensible]] flag in
                // the arr_props side table — which is exactly where
                // `is_extensible` reads it. Answering an unconditional `true`
                // here let `Object.preventExtensions(f);
                // Object.defineProperty(f, "x", {value: 1})` succeed
                // (staging/sm/object/extensibility-01.js).
                _ => self.arr_props.get(&idx).map_or(true, |m| m.extensible),
            }
        };
        let (attr, stored) = self
            .merge_property_descriptor(key, existing, extensible, value, get, set, d_wr, d_en, d_cf)?;
        match target {
            0 => {
                if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                    m.define(key, stored, attr);
                }
            }
            1 => {
                if let HeapObj::Class(c) = self.heap.get_mut(idx) {
                    c.statics.define(key, stored, attr);
                }
            }
            3 => {
                self.arr_props.entry(idx).or_insert_with(ObjMap::new_side_table).define(key, stored, attr);
            }
            _ => {
                self.fn_props.entry(idx).or_insert_with(ObjMap::new_side_table).define(key, stored, attr);
            }
        }
        // Write a defined `lastIndex` VALUE through to the RegExp slot (the
        // getter and `exec` read the slot; arr_props only carries the attrs).
        if regexp_last_index && !attr.accessor {
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(idx) {
                *last_index = stored;
            }
        }
        // A redefined callable/class `name`/`length` now lives as an explicit own
        // prop; suppress the synthesized intrinsic so it neither double-counts in
        // own-key enumeration nor reappears after a later `delete`.
        if (target == 1 || target == 2) && matches!(key, "name" | "length") {
            self.deleted_callable_intrinsics
                .insert((idx, if key == "name" { 0 } else { 1 }));
        }
        self.heap.bump_version(idx);
        Ok(())
    }

    /// ValidateAndApplyPropertyDescriptor: merge a (partial) descriptor over the
    /// current property `existing` (its attrs + stored value; for an accessor the
    /// value is the getter and `attrs.setter` the setter), applying the spec's
    /// non-configurable-redefinition checks. Returns the (attrs, stored-value) to
    /// write, or a TypeError for an illegal redefinition / a new property on a
    /// non-extensible object. Shared by every defineProperty target (plain object,
    /// class static, arr_props, fn_props, and array indices).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn merge_property_descriptor(
        &mut self,
        key: &str,
        existing: Option<(PropAttr, Value)>,
        extensible: bool,
        value: Option<Value>,
        get: Option<Value>,
        set: Option<Value>,
        d_wr: Option<bool>,
        d_en: Option<bool>,
        d_cf: Option<bool>,
    ) -> Result<(PropAttr, Value), Thrown> {
        let is_accessor = get.is_some() || set.is_some();
        // The RESULT kind: a GENERIC descriptor (only enumerable/configurable) over
        // an existing accessor PRESERVES the accessor — it must not collapse to a
        // data property holding the getter VALUE. Only a real data descriptor
        // (value/writable) converts an accessor to data; an accessor descriptor
        // (get/set) always yields an accessor.
        let desc_is_data = value.is_some() || d_wr.is_some();
        let existing_is_accessor = existing.map_or(false, |(a, _)| a.accessor);
        let result_accessor = is_accessor || (!desc_is_data && existing_is_accessor);
        // A KIND conversion (accessor -> data; data and accessor fields are
        // mutually exclusive in a descriptor, so the reverse is `is_accessor`
        // over a data property) does NOT inherit the other kind's fields:
        // ValidateAndApplyPropertyDescriptor converts with [[Value]] undefined
        // and [[Writable]] false unless the descriptor specifies them.
        let convert_to_data = desc_is_data && existing_is_accessor;
        // Start from the existing attrs (redefine) or all-false (new property).
        let (mut wr, mut en, mut cf) = match existing {
            Some((a, _)) if !convert_to_data => (a.writable, a.enumerable, a.configurable),
            Some((a, _)) => (false, a.enumerable, a.configurable),
            None => (false, false, false),
        };
        if let Some(b) = d_wr {
            wr = b;
        }
        if let Some(b) = d_en {
            en = b;
        }
        if let Some(b) = d_cf {
            cf = b;
        }
        // A non-configurable existing property rejects illegal redefinitions.
        if let Some((a, oldv)) = existing {
            if !a.configurable {
                let make_cfg = d_cf == Some(true);
                let change_enum = d_en.is_some_and(|b| b != a.enumerable);
                // A kind change only happens when the descriptor actually specifies
                // the OTHER kind: an accessor descriptor over a data property, or a
                // data descriptor (value/writable) over an accessor. A generic
                // descriptor (only enumerable/configurable) changes neither.
                let is_data_desc = value.is_some() || d_wr.is_some();
                let change_kind = (is_accessor && !a.accessor) || (is_data_desc && a.accessor);
                let make_writable = !a.writable && d_wr == Some(true);
                // A non-writable data property may only be "redefined" to the same
                // value — compared with SameValue (so -0 vs +0 and NaN vs NaN are
                // handled per spec, unlike `==`).
                let change_frozen_value =
                    !a.accessor && !a.writable && value.is_some_and(|v| !self.same_value(v, oldv));
                // A non-configurable accessor's get/set may not be changed (an
                // accessor stores its getter in `oldv`, its setter in `a.setter`).
                let change_accessor = a.accessor
                    && is_accessor
                    && ((get.is_some() && get != Some(oldv))
                        || (set.is_some() && set != Some(a.setter)));
                if make_cfg
                    || change_enum
                    || change_kind
                    || make_writable
                    || change_frozen_value
                    || change_accessor
                {
                    return Err(Thrown(format!("TypeError: Cannot redefine property: {key}")));
                }
            }
        }
        // Defining a brand-new property requires the object to be extensible.
        if existing.is_none() && !extensible {
            return Err(Thrown(format!(
                "TypeError: Cannot define property {key}, object is not extensible"
            )));
        }
        // When redefining an existing accessor with only one half present, the
        // missing half is preserved (spec keeps fields absent from the new desc).
        let existing_get = existing.and_then(|(a, v)| if a.accessor { Some(v) } else { None });
        let existing_set = existing.and_then(|(a, _)| if a.accessor { Some(a.setter) } else { None });
        let attr = PropAttr {
            writable: wr,
            enumerable: en,
            configurable: cf,
            accessor: result_accessor,
            setter: if result_accessor {
                set.or(existing_set).unwrap_or(Value::UNDEFINED)
            } else {
                Value::UNDEFINED
            },
        };
        let stored = if result_accessor {
            get.or(existing_get).unwrap_or(Value::UNDEFINED)
        } else if convert_to_data {
            // Accessor -> data: the getter is NOT carried over as the value —
            // it defaults to undefined unless the descriptor specifies one.
            value.unwrap_or(Value::UNDEFINED)
        } else {
            value.or(existing.map(|(_, v)| v)).unwrap_or(Value::UNDEFINED)
        };
        Ok((attr, stored))
    }

}
