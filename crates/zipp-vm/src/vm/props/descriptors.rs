// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// A Proxy's `getOwnPropertyDescriptor` trap (ES 10.5.5). Returns:
    /// * `Some(descriptor)` — `obj` is a proxy: the trap's (normalized) result, or
    ///   the target's descriptor when the handler defines no trap;
    /// * `None` — `obj` is not a proxy (the caller uses the ordinary path).
    /// Callers try this first so `Object.getOwnPropertyDescriptor(proxy, k)` (and
    /// the descriptor consumers) observe the trap.
    pub(crate) fn proxy_gopd(&mut self, obj: Value, key: &str) -> Result<Option<Value>, Thrown> {
        if !obj.is_heap() {
            return Ok(None);
        }
        let (target, handler, revoked) = match self.proxy_parts(obj.heap_index()) {
            Some(p) => p,
            None => return Ok(None),
        };
        if revoked {
            return Err(Thrown(
                "TypeError: Cannot perform 'getOwnPropertyDescriptor' on a revoked proxy".into(),
            ));
        }
        match self.proxy_trap(handler, "getOwnPropertyDescriptor")? {
            // No trap: forward to the target's [[GetOwnProperty]] — which, when the
            // target is itself a Proxy, must recurse through ITS trap/target rather
            // than falling to the ordinary path (which ignores proxies).
            None => match self.proxy_gopd(target, key)? {
                Some(d) => Ok(Some(d)),
                None => Ok(Some(self.object_get_own_property_descriptor(target, key))),
            },
            Some(trap) => {
                // `r` / `target_desc` are un-rooted heap Values held across the
                // trap call and ToPropertyDescriptor's own re-entries.
                let _gc = self.gc_lock_guard();
                let kv = self.key_to_value(key);
                let r = self.call_value(trap, handler, &[target, kv])?;
                // Step 10: the trap result must be an Object or undefined — checked
                // BEFORE the target's [[GetOwnProperty]], which a Proxy target makes
                // observable.
                if !r.is_undefined() && !self.is_object_value(r) {
                    return Err(Thrown(
                        "TypeError: proxy 'getOwnPropertyDescriptor' trap must return an object or undefined".into(),
                    ));
                }
                // Step 11: targetDesc = ? target.[[GetOwnProperty]](P). A Proxy
                // target recurses through ITS trap; every other kind — Array,
                // TypedArray, a callable, an exotic — answers from the ordinary
                // path. Reading only `HeapObj::Object`'s map (as this did) meant an
                // exotic target skipped EVERY invariant below.
                let target_desc = match self.proxy_gopd(target, key)? {
                    Some(d) => d,
                    None => self.object_get_own_property_descriptor(target, key),
                };
                let t_own = !target_desc.is_undefined();
                let t_cfg = t_own && {
                    let v = self.get_prop(target_desc, "configurable")?;
                    self.truthy(v)
                };
                if r.is_undefined() {
                    // Step 12: can't report a non-configurable own prop (or any own
                    // prop of a non-extensible target) as non-existent. IsExtensible
                    // runs only once the configurable test has passed — on a Proxy
                    // target that is an observable trap.
                    if t_own && (!t_cfg || !self.is_extensible(target)?) {
                        return Err(Thrown(
                            "TypeError: proxy getOwnPropertyDescriptor cannot report an existing non-configurable or non-extensible-target property as undefined".into(),
                        ));
                    }
                    return Ok(Some(Value::UNDEFINED));
                }
                let t_ext = self.is_extensible(target)?;
                // ToPropertyDescriptor (read_descriptor) requires an object, then we
                // re-emit a COMPLETE descriptor (missing fields take their defaults).
                let (value, get, set, wr, en, cf) = self.read_descriptor(r)?;
                // IsCompatiblePropertyDescriptor's extensibility clause: the trap
                // cannot report a property the target lacks when the target is
                // non-extensible (10.5.5 steps 20-21 with targetDesc undefined).
                if !t_own && !t_ext {
                    return Err(Thrown(
                        "TypeError: proxy getOwnPropertyDescriptor reported a new property on a non-extensible target".into(),
                    ));
                }
                let is_accessor = get.is_some() || set.is_some();
                // Step 18: IsCompatiblePropertyDescriptor against the COMPLETED
                // result descriptor (step 17 gives every absent field its default),
                // so an enumerable / data-vs-accessor / value / getter mismatch on a
                // non-configurable target property is rejected — none of which the
                // hand-rolled clauses below cover.
                let completed = if is_accessor {
                    (
                        None,
                        Some(get.unwrap_or(Value::UNDEFINED)),
                        Some(set.unwrap_or(Value::UNDEFINED)),
                        None,
                        Some(en.unwrap_or(false)),
                        Some(cf.unwrap_or(false)),
                    )
                } else {
                    (
                        Some(value.unwrap_or(Value::UNDEFINED)),
                        None,
                        None,
                        Some(wr.unwrap_or(false)),
                        Some(en.unwrap_or(false)),
                        Some(cf.unwrap_or(false)),
                    )
                };
                if !self.is_compatible_property_descriptor(t_ext, completed, target_desc)? {
                    return Err(Thrown(
                        "TypeError: proxy getOwnPropertyDescriptor reported a descriptor incompatible with the target's".into(),
                    ));
                }
                // Step 20: a non-configurable reported descriptor requires a matching
                // non-configurable target property (and, for a non-writable data
                // descriptor, a non-writable target).
                if !cf.unwrap_or(false) {
                    if !t_own || t_cfg {
                        return Err(Thrown(
                            "TypeError: proxy getOwnPropertyDescriptor reported a non-configurable descriptor for a configurable or non-existent property".into(),
                        ));
                    }
                    let t_wr = {
                        let v = self.get_prop(target_desc, "writable")?;
                        self.truthy(v)
                    };
                    if !is_accessor && !wr.unwrap_or(false) && t_wr {
                        return Err(Thrown(
                            "TypeError: proxy getOwnPropertyDescriptor reported a non-writable descriptor for a writable property".into(),
                        ));
                    }
                }
                let normalized = if get.is_some() || set.is_some() {
                    self.make_accessor_descriptor(
                        get.unwrap_or(Value::UNDEFINED),
                        set.unwrap_or(Value::UNDEFINED),
                        en.unwrap_or(false),
                        cf.unwrap_or(false),
                    )
                } else {
                    self.make_data_descriptor(
                        value.unwrap_or(Value::UNDEFINED),
                        wr.unwrap_or(false),
                        en.unwrap_or(false),
                        cf.unwrap_or(false),
                    )
                };
                Ok(Some(normalized))
            }
        }
    }

    /// `Object.getOwnPropertyDescriptor(obj, key)` — the property's descriptor, or
    /// undefined for a missing own property / non-object.
    /// If `obj` is a primitive String value or a boxed String (`new String(s)`),
    /// return `(wrapped_string_value, unit_len)` — the source of a String exotic's
    /// own integer-index (UTF-16 unit) properties and its non-writable `length`.
    /// The reflective `Object.*` methods use this so a string (boxed by ToObject)
    /// reports those exotic own props. `None` for any non-string value.
    pub(crate) fn string_exotic_chars(&self, obj: Value) -> Option<(Value, usize)> {
        if !obj.is_heap() {
            return None;
        }
        let idx = obj.heap_index();
        if let HeapObj::Boxed { kind: 0, value } = self.heap.get(idx) {
            let v = *value;
            return self.heap.str_units(v.heap_index()).map(|n| (v, n));
        }
        self.heap.str_units(idx).map(|n| (obj, n))
    }

    pub(crate) fn object_get_own_property_descriptor(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() {
            return Value::UNDEFINED;
        }
        let idx = obj.heap_index();
        // Descriptor reflection observes the standard match-result properties
        // as fully ordinary data properties. Move the compact pristine record
        // into the general side table before inspecting attributes.
        self.materialize_regexp_result_prop_for_key(idx, key);
        // Module Namespace exotic [[GetOwnProperty]]: the descriptor's value is
        // the LIVE binding (the ObjMap snapshot may be stale).
        if let Some(slot) = self.module_namespaces.get(&idx).and_then(|m| m.get(key)).copied()
        {
            let live = self.globals.get(slot as usize).copied().unwrap_or(Value::UNDEFINED);
            // A TDZ binding: this infallible path reports absent; the fallible
            // reflective entry points (Object/Reflect.getOwnPropertyDescriptor)
            // throw ReferenceError via ns_tdz_check BEFORE calling here.
            if live.is_uninitialized() {
                return Value::UNDEFINED;
            }
            return self.make_data_descriptor(live, true, true, false);
        }
        // %Array.prototype%'s exotic own `length`.
        if idx == self.arr_proto && key == "length" {
            let v = Value::num(self.arr_proto_len as f64);
            return self.make_data_descriptor(v, true, false, false);
        }
        // A callable's `name`/`length`: non-writable, non-enumerable, configurable —
        // unless the function was frozen/sealed (the arr_props integrity flags;
        // %ThrowTypeError% is born frozen), which makes them non-configurable.
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                let cfg = !self.arr_props.get(&idx).map_or(false, |m| m.frozen || m.sealed);
                return self.make_data_descriptor(v, false, false, cfg);
            }
        }
        // A function's / class's `prototype` is a non-enumerable, non-configurable
        // own data property. It is non-writable for a class, writable for an
        // ordinary constructor function. (Only synthesized when no explicit own
        // `prototype` was assigned — the generic match below handles that case.)
        if key == "prototype"
            && self.callable_has_prototype(obj)
            && !matches!(self.heap.get(idx), HeapObj::Object(m) if m.pos("prototype").is_some())
        {
            // An explicit `fn.prototype = value` (incl. a non-object) is reported
            // verbatim as a writable, non-enumerable, non-configurable data property.
            if let Some(&v) = self.fn_proto_override.get(&idx) {
                return self.make_data_descriptor(v, true, false, false);
            }
            let is_class = matches!(self.heap.get(idx), HeapObj::Class(_));
            if let Some(p) = self.prototype_of(obj) {
                return self.make_data_descriptor(p, !is_class, false, false);
            }
        }
        // A String exotic (boxed `new String(s)` or a raw string value): an in-range
        // integer index is a character data prop { value, writable:false,
        // enumerable:true, configurable:false }; `length` is a data prop with all
        // three flags false. Other keys fall through to the wrapper's assigned own
        // props (the arr_props side table) via the generic match below.
        if let Some((sval, len)) = self.string_exotic_chars(obj) {
            if key == "length" {
                return self.make_data_descriptor(len_value(len), false, false, false);
            }
            if let Ok(i) = key.parse::<usize>() {
                if i.to_string() == key && i < len {
                    let ch = self.get_index(sval, Value::num(i as f64)).unwrap_or(Value::UNDEFINED);
                    return self.make_data_descriptor(ch, false, true, false);
                }
            }
        }
        // A RegExp's `lastIndex` is a writable, non-enumerable, non-configurable own
        // data property (a `defineProperty` or an `Object.freeze` may have cleared
        // its writable flag).
        if key == "lastIndex" {
            if let HeapObj::RegExp { last_index, .. } = self.heap.get(idx) {
                let v = *last_index;
                let writable = self.regexp_last_index_writable(idx);
                return self.make_data_descriptor(v, writable, false, false);
            }
        }
        let own = match self.heap.get(idx) {
            HeapObj::Object(m) => {
                if let Some(i) = m.pos(key) {
                    Some((m.attrs[i], m.vals[i]))
                } else if idx == self.global_this && self.global_this != 0 {
                    // A binding that lives only in a global SLOT still presents as an
                    // own property of the global object. `global_slot_binding_descriptor`
                    // owns that classification — shared with `object_define_property`,
                    // which materializes the same descriptor before applying a partial
                    // one over it.
                    if let Some((v, a)) = self.global_slot_binding_descriptor(key) {
                        return self.make_data_descriptor(v, a.writable, a.enumerable, a.configurable);
                    }
                    None
                } else {
                    None
                }
            }
            HeapObj::Array(items) => {
                if key == "length" && !self.arguments_objs.contains_key(&idx) {
                    // A sparse array's JS length lives in the side table.
                    let len = len_value(self.js_array_len(idx));
                    let writable = !self.array_length_nonwritable.contains(&idx);
                    return self.make_data_descriptor(len, writable, false, false);
                }
                let dense_len = items.len();
                // A LIVE-mapped arguments index reports the formal's register
                // as the descriptor's [[Value]] (with the stored attributes).
                let mapped_v = key
                    .parse::<usize>()
                    .ok()
                    .filter(|i| i.to_string() == key)
                    .and_then(|i| self.args_mapped_get(idx, i));
                // A special index override (defineProperty'd attrs/accessor) OR a
                // named own property in arr_props wins; else a dense in-range index
                // is a default { writable, enumerable, configurable } data property.
                let ovr =
                    self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|p| (m.attrs[p], m.vals[p])));
                if ovr.is_some() {
                    ovr.map(|(a, v)| (a, mapped_v.unwrap_or(v)))
                } else {
                    match key.parse::<usize>() {
                        // A hole has no own property descriptor (falls to undefined).
                        Ok(i) if i.to_string() == key && i < dense_len && !items[i].is_hole() => {
                            let v = mapped_v.unwrap_or(items[i]);
                            // A frozen array's elements are non-writable AND
                            // non-configurable; a sealed (not frozen) array's are
                            // non-configurable but still writable.
                            let (frozen, sealed) = self
                                .arr_props
                                .get(&idx)
                                .map_or((false, false), |m| (m.frozen, m.sealed));
                            return self.make_data_descriptor(v, !frozen, true, !(frozen || sealed));
                        }
                        _ => None,
                    }
                }
            }
            // Class static members: data props, plus `static get`/`set` rendered
            // as an accessor descriptor (raw = getter, attr.setter = setter).
            HeapObj::Class(c) if is_private_key(key) => None,
            HeapObj::Class(c) => {
                if let Some(i) = c.statics.pos(key) {
                    Some((c.statics.attrs[i], c.statics.vals[i]))
                } else if let Some((_, g)) = c.static_getters.iter().find(|(n, _)| n == key) {
                    let setter = c
                        .static_setters
                        .iter()
                        .find(|(n, _)| n == key)
                        .map(|(_, s)| *s)
                        .unwrap_or(Value::UNDEFINED);
                    let attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: true,
                        setter,
                    };
                    Some((attr, *g))
                } else if let Some((_, s)) = c.static_setters.iter().find(|(n, _)| n == key) {
                    let attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: true,
                        setter: *s,
                    };
                    Some((attr, Value::UNDEFINED))
                } else {
                    None
                }
            }
            // A function's assigned own properties (`fn.x = y`).
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => {
                // A legacy sloppy function's own `caller`/`arguments`: a LIVE
                // value read from the stack, reported with the fixed attributes
                // engines give them (nothing about them is writable or
                // configurable), so hasOwnProperty/gOPD agree with the read.
                if self.fn_has_legacy_caller_prop(idx, key)
                    && self.fn_props.get(&idx).map_or(true, |m| m.pos(key).is_none())
                {
                    let v = if key == "caller" {
                        self.legacy_fn_caller(idx)
                    } else {
                        self.legacy_fn_arguments(idx)
                    };
                    return self.make_data_descriptor(v, false, false, false);
                }
                self.fn_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i])))
            }
            // A TypedArray's integer-indexed element: a data descriptor
            // { writable:true, enumerable:true, configurable:true }. A named own
            // prop (constructor override) still comes from arr_props (the tail).
            HeapObj::TypedArray { .. } => {
                if let Some(i) = self.ta_valid_index(idx, key) {
                    let v = self.ta_element_get(idx, i);
                    return self.make_data_descriptor(v, true, true, true);
                }
                self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i])))
            }
            // Exotic objects (Map/Set/Date/Promise/…) keep defineProperty'd own
            // properties in the generic arr_props side table.
            _ => self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
        };
        match own {
            Some((a, raw)) if a.accessor => {
                self.make_accessor_descriptor(raw, a.setter, a.enumerable, a.configurable)
            }
            Some((a, raw)) => self.make_data_descriptor(raw, a.writable, a.enumerable, a.configurable),
            None => Value::UNDEFINED,
        }
    }

    /// `Object.getOwnPropertyNames(obj)` — all own string keys (enumerable or not).
    pub(crate) fn object_own_property_names(&mut self, obj: Value) -> Result<Value, Thrown> {
        self.defer_check_all(obj)?;
        if obj.is_heap() {
            // Own-key reflection observes ordering and descriptors, so this is
            // one of the intentionally cold materialisation boundaries.
            self.materialize_regexp_result_props(obj.heap_index());
        }
        // A Proxy reports its keys via the ownKeys trap; getOwnPropertyNames keeps
        // the STRING keys.
        if let Some(keys) = self.proxy_own_keys(obj)? {
            let strs: Vec<Value> = keys
                .into_iter()
                .filter(|k| k.is_heap() && self.heap.is_str_like(k.heap_index()))
                .collect();
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(strs))));
        }
        // Collect the key strings under the (immutable) heap borrow, then allocate
        // the result strings afterwards (alloc needs `&mut self`).
        let mut keys: Vec<String> = Vec::new();
        if obj.is_heap() {
            let idx = obj.heap_index();
            // `length`, then `name` — the spec order for ordinary callables.
            let has_length = self.callable_has_intrinsic(obj, "length");
            let has_name = self.callable_has_intrinsic(obj, "name");
            match self.heap.get(idx) {
                // Private names (stored as "#x") are not reflectable own properties.
                HeapObj::Object(m) => {
                    // %Array.prototype% is an Array exotic object, so its `length`
                    // is a real own property — modelled here as the virtual
                    // `arr_proto_len` slot rather than an ObjMap entry (see the
                    // getOwnPropertyDescriptor arm above). Emit it in its spec
                    // position: after the integer indices, before the methods
                    // (`length` is created by ArrayCreate, i.e. first of the string
                    // keys). Without this, ownKeys contradicted the
                    // getOwnPropertyDescriptor/hasOwnProperty answers.
                    let mut virtual_len =
                        self.arr_proto != 0 && idx == self.arr_proto && m.pos("length").is_none();
                    for k in spec_key_order(&m.keys)
                        .into_iter()
                        .map(|i| &m.keys[i])
                        .filter(|k| !is_hidden_key(k))
                    {
                        if virtual_len
                            && canonical_index_str(k).map_or(true, |n| n >= 4_294_967_295)
                        {
                            keys.push("length".to_string());
                            virtual_len = false;
                        }
                        keys.push(k.clone());
                    }
                    if virtual_len {
                        keys.push("length".to_string());
                    }
                    // The GLOBAL object: builtin globals (Object, Math, NaN, …)
                    // and the program's INITIALIZED var/function globals are
                    // slot-backed routed own properties, not ObjMap entries —
                    // surface them for getOwnPropertyNames/Reflect.ownKeys.
                    // Top-level let/const are NOT global-object properties, a
                    // never-initialized slot does not exist, and a
                    // `delete globalThis.X`'d builtin stays gone.
                    // (Skipped inside a child-realm evaluation: the realm's
                    // global surface is its OWN binding table, and a
                    // ShadowRealm's globalThis must expose only configurable
                    // properties.)
                    if idx == self.global_this && self.global_this != 0 && self.active_realm.is_none() {
                        let mut seen: std::collections::HashSet<String> =
                            keys.iter().cloned().collect();
                        for n in ["NaN", "Infinity", "undefined"] {
                            if !self.deleted_globals.contains(n) && seen.insert(n.to_string()) {
                                keys.push(n.to_string());
                            }
                        }
                        for (i, name) in self.program.global_names.iter().enumerate() {
                            let v =
                                self.globals.get(i).copied().unwrap_or(Value::UNINITIALIZED);
                            if v.is_uninitialized()
                                || is_hidden_key(name)
                                || name == crate::vm::native::ANNEXB_REF_ERROR_NAME
                                || name == crate::vm::native::HOST_CALL_NAME
                                || self.program.lexical_globals.contains(&(i as u32))
                                || self.deleted_globals.contains(name)
                            {
                                continue;
                            }
                            if seen.insert(name.clone()) {
                                keys.push(name.clone());
                            }
                        }
                        // Builtins the program never referenced (no slot). The
                        // Annex-B internal helper is reserved, not a spec
                        // global — it never reflects.
                        let mut extra: Vec<String> = self
                            .builtin_globals
                            .keys()
                            .filter(|n| {
                                !self.deleted_globals.contains(*n)
                                    && !seen.contains(*n)
                                    && *n != crate::vm::native::ANNEXB_REF_ERROR_NAME
                                    && *n != crate::vm::native::HOST_CALL_NAME
                            })
                            .cloned()
                            .collect();
                        extra.sort_unstable();
                        keys.extend(extra);
                    }
                }
                HeapObj::Array(items) => {
                    let dense_len = items.len();
                    for i in 0..dense_len {
                        // A hole is an absent element — not a reflectable own key.
                        if !items[i].is_hole() {
                            keys.push(i.to_string());
                        }
                    }
                    if let Some(m) = self.arr_props.get(&idx) {
                        // SPARSE-overlay index keys (>= the dense prefix) continue
                        // the ascending index run before "length"/named keys.
                        // ("4294967295"+ are NOT array indices — named below.)
                        let mut sparse: Vec<usize> = m
                            .keys
                            .iter()
                            .filter(|k| !is_hidden_key(k))
                            .filter_map(|k| {
                                canonical_index_str(k)
                                    .filter(|n| *n < 4_294_967_295 && *n >= dense_len)
                            })
                            .collect();
                        sparse.sort_unstable();
                        keys.extend(sparse.into_iter().map(|n| n.to_string()));
                    }
                    keys.push("length".to_string());
                    if let Some(m) = self.arr_props.get(&idx) {
                        // Named own props only — index keys in arr_props were
                        // covered by the dense range / sparse run above.
                        keys.extend(
                            m.keys
                                .iter()
                                .filter(|k| {
                                    if is_hidden_key(k) {
                                        return false;
                                    }
                                    canonical_index_str(k).map_or(true, |n| n >= 4_294_967_295)
                                })
                                .cloned(),
                        );
                    }
                }
                // A TypedArray's own keys: its integer indices `0..length` first
                // (the exotic own properties; `length`/`buffer`/… live on the
                // prototype), then any named own props in the arr_props side table.
                HeapObj::TypedArray { .. } => {
                    let len = self.ta_len_kind(idx).0;
                    for i in 0..len {
                        keys.push(i.to_string());
                    }
                    if let Some(m) = self.arr_props.get(&idx) {
                        keys.extend(m.keys.iter().filter(|k| !is_hidden_key(k)).cloned());
                    }
                }
                HeapObj::Class(c) => {
                    // Spec order (OrdinaryOwnPropertyKeys): INTEGER-keyed static
                    // elements first, ascending; then strings in creation order —
                    // the constructor's `length`, `name`, then `prototype` are
                    // created FIRST. A static element named one of those
                    // overwrites the VALUE but keeps that early position (it was
                    // already defined), so emit those three before the remaining
                    // static elements.
                    let static_has = |k: &str| {
                        !is_private_key(k)
                            && (c.statics.pos(k).is_some()
                                || c.static_getters.iter().any(|(n, _)| n == k)
                                || c.static_setters.iter().any(|(n, _)| n == k))
                    };
                    let mut ints: Vec<usize> = Vec::new();
                    let mut named: Vec<String> = Vec::new();
                    {
                        let mut push_key = |k: &str| {
                            if is_hidden_key(k)
                                || is_private_key(k)
                                || matches!(k, "length" | "name" | "prototype")
                            {
                                return;
                            }
                            if let Some(i) = canonical_index_str(k) {
                                if !ints.contains(&i) {
                                    ints.push(i);
                                }
                            } else if !named.iter().any(|n| n == k) {
                                named.push(k.to_string());
                            }
                        };
                        for k in &c.statics.keys {
                            push_key(k);
                        }
                        for (n, _) in &c.static_getters {
                            push_key(n);
                        }
                        for (n, _) in &c.static_setters {
                            push_key(n);
                        }
                    }
                    ints.sort_unstable();
                    keys.extend(ints.into_iter().map(|i| i.to_string()));
                    if has_length || static_has("length") {
                        keys.push("length".to_string());
                    }
                    if has_name || static_has("name") {
                        keys.push("name".to_string());
                    }
                    if self.callable_has_prototype(obj) || c.statics.pos("prototype").is_some() {
                        keys.push("prototype".to_string());
                    }
                    keys.extend(named);
                }
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Bound { .. }
                | HeapObj::BoundResolver { .. }
                | HeapObj::CombinatorResolver { .. }
                | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => {
                    // length, name, then prototype are created at function-definition
                    // time, so they keep their chronological-FIRST position even after
                    // a defineProperty moves the override into the fn_props bag (which
                    // clears the synthesized-intrinsic flag) — and are excluded from
                    // the fn_props tail below. `prototype` is intrinsic only for real
                    // functions (callable_has_prototype): an arrow's later-assigned
                    // `prototype` is an ordinary property and stays in chronological
                    // order. (Matters for Object.keys once one is made enumerable.)
                    let fp_has = |k: &str| {
                        self.fn_props.get(&idx).map_or(false, |m| m.pos(k).is_some())
                    };
                    let has_proto_early = self.callable_has_prototype(obj);
                    if has_length || fp_has("length") {
                        keys.push("length".to_string());
                    }
                    if has_name || fp_has("name") {
                        keys.push("name".to_string());
                    }
                    // A legacy sloppy function's own `arguments`/`caller` are
                    // created with the function, right after `name` and before
                    // `prototype` — the order engines report.
                    if self.fn_has_legacy_caller_prop(idx, "arguments") && !fp_has("arguments") {
                        keys.push("arguments".to_string());
                        keys.push("caller".to_string());
                    }
                    if has_proto_early {
                        keys.push("prototype".to_string());
                    }
                    if let Some(m) = self.fn_props.get(&idx) {
                        keys.extend(
                            m.keys
                                .iter()
                                .filter(|k| {
                                    if is_hidden_key(k) {
                                        return false;
                                    }
                                    match k.as_str() {
                                        // Always emitted early on a callable.
                                        "length" | "name" => false,
                                        // Early only for real functions; an arrow's
                                        // assigned `prototype` stays chronological.
                                        "prototype" => !has_proto_early,
                                        _ => true,
                                    }
                                })
                                .cloned(),
                        );
                    }
                }
                // A String exotic (boxed `new String(s)` or a raw string): ALL
                // integer-index keys first ascending — the character indices
                // `0..length` plus any ASSIGNED index past them — then `length`,
                // then the named own props in creation order.
                HeapObj::Boxed { kind: 0, .. } | HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    if let Some((_, len)) = self.string_exotic_chars(obj) {
                        for i in 0..len {
                            keys.push(i.to_string());
                        }
                        if let Some(m) = self.arr_props.get(&idx) {
                            let mut extra: Vec<usize> = m
                                .keys
                                .iter()
                                .filter(|k| !is_hidden_key(k))
                                .filter_map(|k| {
                                    canonical_index_str(k)
                                        .filter(|n| *n < 4_294_967_295 && *n >= len)
                                })
                                .collect();
                            extra.sort_unstable();
                            keys.extend(extra.into_iter().map(|n| n.to_string()));
                        }
                        keys.push("length".to_string());
                        if let Some(m) = self.arr_props.get(&idx) {
                            keys.extend(
                                m.keys
                                    .iter()
                                    .filter(|k| {
                                        !is_hidden_key(k)
                                            && canonical_index_str(k)
                                                .map_or(true, |n| n >= 4_294_967_295)
                                    })
                                    .cloned(),
                            );
                        }
                    } else if let Some(m) = self.arr_props.get(&idx) {
                        keys.extend(m.keys.iter().filter(|k| !is_hidden_key(k)).cloned());
                    }
                }
                // A RegExp's only own property is `lastIndex` (plus any assigned).
                HeapObj::RegExp { .. } => {
                    keys.push("lastIndex".to_string());
                    if let Some(m) = self.arr_props.get(&idx) {
                        keys.extend(
                            m.keys
                                .iter()
                                .filter(|k| !is_hidden_key(k) && k.as_str() != "lastIndex")
                                .cloned(),
                        );
                    }
                }
                // Every REMAINING heap kind — a boxed Number/Boolean/Symbol/BigInt
                // wrapper (kind != 0; kind 0 = String is handled above), Map, Set,
                // Date, Promise, ArrayBuffer, DataView, a generator, … — has no
                // exotic own properties of its own: the only ones it can have are
                // those ASSIGNED to it, which every such kind keeps in the generic
                // arr_props side table (the same store getOwnPropertyDescriptor and
                // hasOwnProperty read). Without this arm the catch-all reported no
                // keys at all, so `var m = new Map(); m.x = 1` was invisible to
                // getOwnPropertyNames / Object.keys / for-in / JSON.stringify while
                // `m.hasOwnProperty("x")` said true. (`Object.assign(12, "ab")`
                // boxes the target and copies "0"/"1" the same way.)
                _ => {
                    if let Some(m) = self.arr_props.get(&idx) {
                        keys.extend(
                            spec_key_order(&m.keys)
                                .into_iter()
                                .map(|i| &m.keys[i])
                                .filter(|k| !is_hidden_key(k))
                                .cloned(),
                        );
                    }
                }
            }
        }
        let names: Vec<Value> = keys.into_iter().map(|k| self.alloc_str(k)).collect();
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(names))))
    }

    /// `Reflect.ownKeys(obj)` — every own property key: the String keys (in
    /// `[[OwnPropertyKeys]]` order, integer-index-first then creation order)
    /// followed by the Symbol keys in creation order. A Proxy's ownKeys trap
    /// already returns the full Strings+Symbols list, so pass it through.
    pub(crate) fn object_own_keys(&mut self, obj: Value) -> Result<Value, Thrown> {
        if let Some(keys) = self.proxy_own_keys(obj)? {
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(keys))));
        }
        // String keys (reuses the [[OwnPropertyKeys]] string ordering).
        let names = self.object_own_property_names(obj)?;
        let mut out = self.array_snapshot(names.heap_index());
        // Symbol keys: the `@@`-prefixed own keys mapped back to their Symbols,
        // in property-creation (insertion) order.
        if obj.is_heap() {
            let sym_keys: Vec<String> = match self.heap.get(obj.heap_index()) {
                HeapObj::Object(m) => {
                    m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect()
                }
                // Callables keep their own props in fn_props; every other exotic
                // heap kind (TypedArray, DataView, Map, Date, ...) keeps them in
                // arr_props — surface their "@@" symbol keys too.
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => self
                    .fn_props
                    .get(&obj.heap_index())
                    .map_or(Vec::new(), |m| {
                        m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect()
                    }),
                _ => self
                    .arr_props
                    .get(&obj.heap_index())
                    .map_or(Vec::new(), |m| {
                        m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect()
                    }),
            };
            for k in sym_keys {
                if let Some(&sym) = self.symbol_keys.get(&k) {
                    out.push(sym);
                }
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// `__lookupGetter__`/`__lookupSetter__`'s chain walk using the SPEC abstract
    /// operations: each step is `[[GetOwnProperty]](key)` then `[[GetPrototypeOf]]()`,
    /// both Proxy-trap-aware and throwing — so a trap that raises an abrupt completion
    /// propagates.
    pub(crate) fn lookup_accessor_checked(
        &mut self,
        this: Value,
        key: &str,
        want_setter: bool,
    ) -> Result<Value, Thrown> {
        let mut cur = this;
        for _ in 0..10_000 {
            if !cur.is_heap() {
                break;
            }
            if self.proxy_parts(cur.heap_index()).is_some() {
                // [[GetOwnProperty]] via the getOwnPropertyDescriptor trap (may throw).
                if let Some(desc) = self.proxy_gopd(cur, key)? {
                    if desc != Value::UNDEFINED {
                        // An accessor descriptor carries get/set; a data descriptor → undefined.
                        let is_accessor = self.has_property_str(desc, "get")
                            || self.has_property_str(desc, "set");
                        if is_accessor {
                            let which = if want_setter { "set" } else { "get" };
                            return self.get_prop(desc, which);
                        }
                        return Ok(Value::UNDEFINED);
                    }
                }
            } else if let HeapObj::Object(m) = self.heap.get(cur.heap_index()) {
                if let Some(i) = m.pos(key) {
                    let attr = m.attrs[i];
                    if attr.accessor {
                        return Ok(if want_setter { attr.setter } else { m.vals[i] });
                    }
                    return Ok(Value::UNDEFINED);
                }
            }
            // [[GetPrototypeOf]] via the getPrototypeOf trap (may throw).
            cur = self.get_prototype_of_checked(cur)?;
            if cur == Value::NULL {
                break;
            }
        }
        Ok(Value::UNDEFINED)
    }

    /// If `idx` is a generator / async / async-generator FUNCTION, the matching
    /// dynamic-function intrinsic prototype (%GeneratorFunction.prototype% etc.) —
    /// its [[Prototype]] and the target for its method/`.constructor` lookups.
    /// `None` for plain functions (which use %Function.prototype%) and non-callables.
    /// A REALM-BORN function starts at its realm's image of the intrinsic (a
    /// hidden per-realm copy built by create_realm), so `.constructor` reaches
    /// the realm's %GeneratorFunction% facade.
    pub(crate) fn callable_dynfn_proto(&self, idx: u32) -> Option<u32> {
        let fid = match self.heap.get(idx) {
            HeapObj::Func(f) => *f,
            HeapObj::Closure { func, .. } => *func,
            _ => return None,
        };
        let p = self.func(fid as usize);
        let main = match (p.is_generator, p.is_async) {
            (true, true) => (self.asyncgen_fn_proto != 0).then_some(self.asyncgen_fn_proto),
            (true, false) => (self.gen_fn_proto != 0).then_some(self.gen_fn_proto),
            (false, true) => (self.async_fn_proto != 0).then_some(self.async_fn_proto),
            (false, false) => None,
        }?;
        if !self.realm_global_objs.is_empty() {
            let r = self.get_function_realm(Value::heap(idx));
            if r != 0 {
                if let Some(&rp) = self.realms.get(r as usize).and_then(|m| m.get(&main)) {
                    return Some(rp);
                }
            }
        }
        Some(main)
    }

    pub(crate) fn object_get_prototype_of(&mut self, obj: Value) -> Value {
        if !obj.is_heap() {
            return Value::NULL;
        }
        let idx = obj.heap_index();
        // Proxy `getPrototypeOf` trap (errors degrade to null — this signature is
        // infallible; the throwing path is rare and used internally by instanceof).
        if let Some((target, handler, revoked)) = self.proxy_parts(idx) {
            if revoked {
                return Value::NULL;
            }
            if let Ok(Some(trap)) = self.proxy_trap(handler, "getPrototypeOf") {
                return self.call_value(trap, handler, &[target]).unwrap_or(Value::NULL);
            }
            return self.object_get_prototype_of(target);
        }
        if let Some(&p) = self.proto_of.get(&idx) {
            return p;
        }
        if idx == self.obj_proto {
            return Value::NULL; // Object.prototype's [[Prototype]] is null
        }
        // A class value's [[Prototype]] is its PARENT CLASS (`class C extends B` →
        // B), or %Function.prototype% for a base class — so `getPrototypeOf(C)===B`,
        // `getPrototypeOf(class{})===Function.prototype`, and `C instanceof Function`
        // all hold (the chain reaches %Function.prototype% then %Object.prototype%).
        let class_parent = match self.heap.get(idx) {
            HeapObj::Class(c) => Some(c.parent),
            _ => None,
        };
        if let Some(parent) = class_parent {
            return match parent {
                Some(p) => Value::heap(p),
                None if self.fn_proto != 0 => Value::heap(self.fn_proto),
                None => Value::NULL,
            };
        }
        // Built-in instance types delegate to their respective prototype (so
        // `Object.getPrototypeOf(new Map()) === Map.prototype` and `m instanceof Map`).
        let builtin_proto = match self.heap.get(idx) {
            HeapObj::Map { .. } => self.map_proto,
            HeapObj::Set(_) => self.set_proto,
            HeapObj::WeakMap { .. } => self.weakmap_proto,
            HeapObj::WeakSet(_) => self.weakset_proto,
            HeapObj::WeakRef(_) => self.weakref_proto,
            HeapObj::FinalizationRegistry { .. } => self.finreg_proto,
            HeapObj::Iterator { proto, .. } => *proto,
            HeapObj::IterHelper { .. } => self.iterator_helper_proto,
            HeapObj::Generator { .. } => self.gen_proto,
            HeapObj::AsyncGenerator(_) => self.asyncgen_proto,
            HeapObj::Boxed { kind, .. } => match kind {
                0 => self.str_proto,
                1 => self.num_proto,
                2 => self.bool_proto,
                3 => self.symbol_proto,
                _ => self.bigint_proto,
            },
            HeapObj::Date(_) => self.date_proto,
            HeapObj::Promise { .. } => self.promise_proto,
            _ => 0,
        };
        if builtin_proto != 0 {
            return Value::heap(builtin_proto);
        }
        // kind: 0=plain/instance object, 1=callable, 2=array, 3=other.
        let (class, is_ctor, kind) = match self.heap.get(idx) {
            HeapObj::Object(m) => (m.class, m.is_ctor, 0u8),
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::BoundResolver { .. }
            | HeapObj::CombinatorResolver { .. }
            | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => (None, false, 1),
            HeapObj::Array(_) => (None, false, 2),
            _ => (None, false, 3),
        };
        // A generator/async/async-generator function's [[Prototype]] is the
        // matching dynamic-function intrinsic prototype, not %Function.prototype%.
        if kind == 1 {
            if let Some(p) = self.callable_dynfn_proto(idx) {
                return Value::heap(p);
            }
        }
        match kind {
            0 => {
                if let Some(cidx) = class {
                    if let Some(p) = self.prototype_of(Value::heap(cidx)) {
                        return p;
                    }
                }
                // A constructor object (Array/Object/Map/… built as a callable
                // Object with no explicit [[Prototype]] and no class link) IS a
                // function, so its [[Prototype]] is %Function.prototype% — making
                // `Array instanceof Function` true and
                // `getPrototypeOf(Array) === Function.prototype`.
                if is_ctor && self.fn_proto != 0 {
                    return Value::heap(self.fn_proto);
                }
                if self.obj_proto != 0 {
                    Value::heap(self.obj_proto)
                } else {
                    Value::NULL
                }
            }
            1 if self.fn_proto != 0 => Value::heap(self.fn_proto),
            2 if self.arr_proto != 0 => Value::heap(self.arr_proto),
            _ => Value::NULL,
        }
    }

    /// OrdinarySetPrototypeOf (+ the Proxy `setPrototypeOf` trap). Returns whether
    /// the change succeeded (per spec, a boolean — the caller throws if a strict/
    /// reflective entry point requires it). Shared by `Object.setPrototypeOf`,
    /// `Reflect.setPrototypeOf`, and the `Object.prototype.__proto__` setter, so the
    /// failure conditions are enforced uniformly: a same-proto change is a no-op
    /// success; the immutable-prototype exotic %Object.prototype% rejects any real
    /// change; a non-extensible target rejects a real change; and a new prototype
    /// chain that loops back to the target (a cycle) is rejected.
    pub(crate) fn ordinary_set_prototype_of(&mut self, o: Value, proto: Value) -> Result<bool, Thrown> {
        // A Proxy routes through its [[SetPrototypeOf]] trap.
        if let Some(b) = self.proxy_set_prototype_of(o, proto)? {
            return Ok(b);
        }
        let cur = self.object_get_prototype_of(o);
        if cur == proto {
            return Ok(true); // SameValue: no-op success (even if non-extensible)
        }
        // %Object.prototype% is an immutable-prototype exotic — any real change fails.
        if o.is_heap() && o.heap_index() == self.obj_proto {
            return Ok(false);
        }
        // Non-Object heap kinds (Array/TypedArray/Date/…) keep their extensible
        // flag in the arr_props side table; primitive-like heap objects are never
        // extensible targets here.
        let extensible = match self.heap.get(o.heap_index()) {
            HeapObj::Object(m) => m.extensible,
            HeapObj::Str(_)
            | HeapObj::Cons { .. }
            | HeapObj::Symbol { .. }
            | HeapObj::BigInt(_)
            | HeapObj::BigIntBig(_) => false,
            _ => self.arr_props.get(&o.heap_index()).map_or(true, |m| m.extensible),
        };
        if !extensible {
            return Ok(false);
        }
        let mut p = proto;
        while p.is_heap() {
            if p.heap_index() == o.heap_index() {
                return Ok(false); // cycle
            }
            // A Proxy's [[GetPrototypeOf]] may be exotic — stop the static walk here.
            if self.proxy_parts(p.heap_index()).is_some() {
                break;
            }
            let next = self.object_get_prototype_of(p);
            if !next.is_heap() {
                break;
            }
            p = next;
        }
        self.proto_of.insert(o.heap_index(), proto);
        // Replacing a live object's [[Prototype]] changes what every chain
        // lookup THROUGH it resolves — bump its version so version-guarded
        // caches (the interpreter property/call ICs' chain hops) miss.
        self.heap.bump_version(o.heap_index());
        // Re-prototyping %Array.prototype% / %Object.prototype% splices an entire
        // new chain into every plain array's lookup path. A version bump does NOT
        // cover it: the hole/OOB/append fast paths are gated on the indexed-proto
        // PROTECTOR, not on versions, and the protector until now only watched for
        // an integer key being DEFINED on those two objects. So
        // `setPrototypeOf(Array.prototype, {5: "M5"})` left a hot `a[5]` answering
        // `undefined` where the interpreter and node answer `"M5"` (B66's third
        // open tier divergence).
        //
        // Invalidate unconditionally — do NOT try to prove the new chain carries no
        // index. `x` can be index-free here and gain `x[5] = …` a line later, and
        // that later write lands on an ordinary object no protector is watching.
        if self.is_indexed_proto_anchor(o.heap_index()) {
            self.invalidate_indexed_proto_protector();
        }
        Ok(true)
    }

    /// FALLIBLE [[GetPrototypeOf]] for the PUBLIC reflective entry points
    /// (`Object.getPrototypeOf`, `Reflect.getPrototypeOf`, the `__proto__`
    /// getter). Identical to `object_get_prototype_of` for ordinary objects, but
    /// a Proxy enforces its trap invariants — a revoked handler, a non-callable
    /// trap, a non-Object/Null result, or a result that disagrees with a
    /// non-extensible target's real prototype all throw TypeError (the infallible
    /// path silently degraded these to null, which internal proto-chain walks
    /// such as `instanceof` still use).
    pub(crate) fn get_prototype_of_checked(&mut self, obj: Value) -> Result<Value, Thrown> {
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown(
                        "TypeError: Cannot perform 'getPrototypeOf' on a proxy that has been revoked"
                            .into(),
                    ));
                }
                let trap = match self.proxy_trap(handler, "getPrototypeOf")? {
                    Some(t) => t,
                    None => return self.get_prototype_of_checked(target),
                };
                let handler_proto = self.call_value(trap, handler, &[target])?;
                if handler_proto != Value::NULL && !self.is_object_value(handler_proto) {
                    return Err(Thrown(
                        "TypeError: proxy 'getPrototypeOf' trap must return an object or null".into(),
                    ));
                }
                // Non-extensible target: the trap result must equal the target's
                // actual [[Prototype]] (SameValue).
                let ext = match self.proxy_is_extensible(target)? {
                    Some(b) => b,
                    None => match self.heap.get(target.heap_index()) {
                        HeapObj::Object(m) => m.extensible,
                        _ => self
                            .arr_props
                            .get(&target.heap_index())
                            .map_or(true, |m| m.extensible),
                    },
                };
                if !ext {
                    // Step 11 is `? target.[[GetPrototypeOf]]()` — ABRUPT. The
                    // infallible walker swallows a throwing getPrototypeOf trap on
                    // the target (it returns null), so a nested proxy target whose
                    // own trap throws reported "no exception" and, worse, compared
                    // against a bogus null (staging/sm/Proxy/getPrototypeOf.js).
                    let target_proto = self.get_prototype_of_checked(target)?;
                    if !self.same_value(handler_proto, target_proto) {
                        return Err(Thrown(
                            "TypeError: proxy 'getPrototypeOf' must return the target's prototype when the target is not extensible"
                                .into(),
                        ));
                    }
                }
                return Ok(handler_proto);
            }
        }
        Ok(self.object_get_prototype_of(obj))
    }

    /// IsCompatiblePropertyDescriptor(extensible, Desc, current) — 6.2.6.4, i.e.
    /// ValidateAndApplyPropertyDescriptor with O = undefined (validate only, apply
    /// nothing). `desc` is the candidate's read_descriptor tuple (a field is
    /// "present" iff it is `Some`); `current` is the target's own descriptor
    /// OBJECT, `undefined` when it has none.
    ///
    /// Only the Proxy [[GetOwnProperty]] / [[DefineOwnProperty]] invariants need
    /// it, so the answer is a plain bool and the caller raises the trap-specific
    /// TypeError. The ordinary define path validates inline against its own
    /// storage instead.
    #[allow(clippy::type_complexity)]
    pub(crate) fn is_compatible_property_descriptor(
        &mut self,
        extensible: bool,
        desc: (Option<Value>, Option<Value>, Option<Value>, Option<bool>, Option<bool>, Option<bool>),
        current: Value,
    ) -> Result<bool, Thrown> {
        let (value, get, set, writable, enumerable, configurable) = desc;
        // No existing property: only extensibility decides.
        if current.is_undefined() {
            return Ok(extensible);
        }
        let c_cfg = {
            let v = self.get_prop(current, "configurable")?;
            self.truthy(v)
        };
        // A configurable existing property accepts any redefinition.
        if c_cfg {
            return Ok(true);
        }
        if configurable == Some(true) {
            return Ok(false);
        }
        let c_en = {
            let v = self.get_prop(current, "enumerable")?;
            self.truthy(v)
        };
        if enumerable.is_some_and(|e| e != c_en) {
            return Ok(false);
        }
        let d_accessor = get.is_some() || set.is_some();
        let d_data = value.is_some() || writable.is_some();
        let c_accessor = self.has_own_property(current, "get") || self.has_own_property(current, "set");
        // A GENERIC descriptor (neither data nor accessor fields) may narrow
        // either kind; anything else must keep the existing kind.
        if (d_accessor || d_data) && d_accessor != c_accessor {
            return Ok(false);
        }
        if c_accessor {
            if let Some(g) = get {
                let cg = self.get_prop(current, "get")?;
                if !self.same_value(g, cg) {
                    return Ok(false);
                }
            }
            if let Some(s) = set {
                let cs = self.get_prop(current, "set")?;
                if !self.same_value(s, cs) {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        let c_wr = {
            let v = self.get_prop(current, "writable")?;
            self.truthy(v)
        };
        if !c_wr {
            if writable == Some(true) {
                return Ok(false);
            }
            if let Some(v) = value {
                let cv = self.get_prop(current, "value")?;
                if !self.same_value(v, cv) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Read a property-descriptor object's fields (present-or-absent) for
    /// `Object.defineProperty`. Throws if `desc` is not an object.
    pub(crate) fn read_descriptor(
        &mut self,
        desc: Value,
    ) -> Result<(Option<Value>, Option<Value>, Option<Value>, Option<bool>, Option<bool>, Option<bool>), Thrown>
    {
        // ToPropertyDescriptor only requires Type(Obj) is Object — a Function (or
        // any other object) carrying value/get/set/... own props is a valid
        // descriptor, so accept any object, not just a plain HeapObj::Object.
        if !self.is_object_value(desc) {
            return Err(Thrown("TypeError: Property description must be an object".into()));
        }
        // Presence uses [[HasProperty]] (walks the prototype chain), so a
        // descriptor whose value/writable/enumerable/configurable/get/set is
        // INHERITED (or an accessor on the prototype) is recognized. Each field's
        // value is then fetched with get_prop (which also walks the chain + runs
        // getters).
        //
        // The reads follow ToPropertyDescriptor (6.2.6.5) EXACTLY: enumerable,
        // configurable, value, writable, get, set — each [[HasProperty]]
        // immediately followed by its [[Get]], and each accessor's callability
        // checked before the next field is probed. Batching the six Has calls
        // ahead of the six Gets (and validating at the end) is unobservable for a
        // plain descriptor but wrong for one whose fields are traps or getters.
        // The Has must also be the FALLIBLE, proxy-aware operation: with the
        // infallible walk a Proxy descriptor read as entirely empty, so
        // `Object.defineProperty({}, "x", proxyDesc)` performed no observable
        // operation at all.
        let enumerable = if self.has_property_str_dyn(desc, "enumerable")? {
            let v = self.get_prop(desc, "enumerable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let configurable = if self.has_property_str_dyn(desc, "configurable")? {
            let v = self.get_prop(desc, "configurable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let value =
            if self.has_property_str_dyn(desc, "value")? { Some(self.get_prop(desc, "value")?) } else { None };
        let writable = if self.has_property_str_dyn(desc, "writable")? {
            let v = self.get_prop(desc, "writable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        // Steps 12.b / 14.b: a present getter or setter must be callable (or
        // `undefined`), rejected before the NEXT field is read.
        let get = if self.has_property_str_dyn(desc, "get")? {
            let g = self.get_prop(desc, "get")?;
            if g != Value::UNDEFINED && !self.is_callable(g) {
                return Err(Thrown("TypeError: Getter must be a function".into()));
            }
            Some(g)
        } else {
            None
        };
        let set = if self.has_property_str_dyn(desc, "set")? {
            let s = self.get_prop(desc, "set")?;
            if s != Value::UNDEFINED && !self.is_callable(s) {
                return Err(Thrown("TypeError: Setter must be a function".into()));
            }
            Some(s)
        } else {
            None
        };
        // Step 15: an accessor descriptor may not also carry data fields.
        if (get.is_some() || set.is_some()) && (value.is_some() || writable.is_some()) {
            return Err(Thrown(
                "TypeError: Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"
                    .into(),
            ));
        }
        Ok((value, get, set, writable, enumerable, configurable))
    }

    /// SetIntegrityLevel's per-key walk for a Proxy `proxy`, after a successful
    /// [[PreventExtensions]]: O.[[OwnPropertyKeys]](), then for each key whose
    /// [[GetOwnProperty]] is not undefined, DefinePropertyOrThrow a PARTIAL
    /// integrity descriptor — `{configurable:false}` for an accessor or under
    /// `seal`, `{configurable:false, writable:false}` for a data property under
    /// `freeze`. Drives the proxy ownKeys / getOwnPropertyDescriptor /
    /// defineProperty traps (in spec key order) via the existing helpers.
    pub(crate) fn proxy_set_integrity(&mut self, proxy: Value, freeze: bool) -> Result<(), Thrown> {
        // The keys Vec holds un-rooted Values across the trap re-entries.
        let _gc = self.gc_lock_guard();
        let keys = match self.proxy_own_keys(proxy)? {
            Some(k) => k,
            None => return Ok(()),
        };
        for kv in keys {
            let key = self.key_of(kv);
            let cur = self.proxy_gopd(proxy, &key)?;
            if let Some(desc) = cur {
                if desc == Value::UNDEFINED {
                    continue;
                }
                let is_accessor =
                    self.has_own_property(desc, "get") || self.has_own_property(desc, "set");
                let mut m = ObjMap::new();
                m.set("configurable", Value::bool(false));
                if freeze && !is_accessor {
                    m.set("writable", Value::bool(false));
                }
                let idesc = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))));
                self.object_define_property(proxy, &key, idesc)?;
            }
        }
        Ok(())
    }

    /// TestIntegrityLevel for a Proxy (`Object.isFrozen`/`isSealed`): an extensible
    /// proxy is neither; otherwise [[OwnPropertyKeys]]() and check each present
    /// property is non-configurable (and, for `freeze`, that a data property is
    /// non-writable). Drives the isExtensible / ownKeys / getOwnPropertyDescriptor
    /// traps via the existing helpers.
    pub(crate) fn proxy_test_integrity(&mut self, proxy: Value, freeze: bool) -> Result<bool, Thrown> {
        if self.proxy_is_extensible(proxy)? == Some(true) {
            return Ok(false);
        }
        let _gc = self.gc_lock_guard();
        let keys = match self.proxy_own_keys(proxy)? {
            Some(k) => k,
            None => return Ok(true),
        };
        for kv in keys {
            let key = self.key_of(kv);
            if let Some(desc) = self.proxy_gopd(proxy, &key)? {
                if desc == Value::UNDEFINED {
                    continue;
                }
                let cfg = self.get_prop(desc, "configurable")?;
                if self.truthy(cfg) {
                    return Ok(false);
                }
                if freeze {
                    let is_data =
                        self.has_own_property(desc, "value") || self.has_own_property(desc, "writable");
                    let wr = self.get_prop(desc, "writable")?;
                    if is_data && self.truthy(wr) {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

}
