#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Own ENUMERABLE keys / values / [k,v] entries of `obj` as an array (the
    /// shared core of `Object.keys`/`values`/`entries`).
    /// A Proxy's `setPrototypeOf` trap. `Some(success)` for a proxy (the trap's
    /// boolean, or the target update when no trap); `None` for a non-proxy.
    pub(crate) fn proxy_set_prototype_of(
        &mut self,
        obj: Value,
        proto: Value,
    ) -> Result<Option<bool>, Thrown> {
        if !obj.is_heap() {
            return Ok(None);
        }
        let (target, handler, revoked) = match self.proxy_parts(obj.heap_index()) {
            Some(p) => p,
            None => return Ok(None),
        };
        if revoked {
            return Err(Thrown(
                "TypeError: Cannot perform 'setPrototypeOf' on a revoked proxy".into(),
            ));
        }
        match self.proxy_trap(handler, "setPrototypeOf")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target, proto])?;
                Ok(Some(self.truthy(r)))
            }
            None => {
                if target.is_heap() {
                    self.proto_of.insert(target.heap_index(), proto);
                }
                Ok(Some(true))
            }
        }
    }

    /// A Proxy's `isExtensible` trap. `Some(result)` for a proxy (the trap boolean,
    /// or the target's extensibility when no trap); `None` for a non-proxy.
    pub(crate) fn proxy_is_extensible(&mut self, obj: Value) -> Result<Option<bool>, Thrown> {
        if !obj.is_heap() {
            return Ok(None);
        }
        let (target, handler, revoked) = match self.proxy_parts(obj.heap_index()) {
            Some(p) => p,
            None => return Ok(None),
        };
        if revoked {
            return Err(Thrown("TypeError: Cannot perform 'isExtensible' on a revoked proxy".into()));
        }
        match self.proxy_trap(handler, "isExtensible")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target])?;
                Ok(Some(self.truthy(r)))
            }
            None => {
                if let Some(b) = self.proxy_is_extensible(target)? {
                    return Ok(Some(b)); // nested proxy target
                }
                let ext = match self.heap.get(target.heap_index()) {
                    HeapObj::Object(m) => m.extensible,
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) => false,
                    _ => self.arr_props.get(&target.heap_index()).map_or(true, |m| m.extensible),
                };
                Ok(Some(ext))
            }
        }
    }

    /// A Proxy's `preventExtensions` trap. `Some(success)` for a proxy; `None` for
    /// a non-proxy. With no trap, marks the target non-extensible.
    pub(crate) fn proxy_prevent_extensions(&mut self, obj: Value) -> Result<Option<bool>, Thrown> {
        if !obj.is_heap() {
            return Ok(None);
        }
        let (target, handler, revoked) = match self.proxy_parts(obj.heap_index()) {
            Some(p) => p,
            None => return Ok(None),
        };
        if revoked {
            return Err(Thrown(
                "TypeError: Cannot perform 'preventExtensions' on a revoked proxy".into(),
            ));
        }
        match self.proxy_trap(handler, "preventExtensions")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target])?;
                Ok(Some(self.truthy(r)))
            }
            None => {
                if let Some(b) = self.proxy_prevent_extensions(target)? {
                    return Ok(Some(b)); // nested proxy target
                }
                let ti = target.heap_index();
                if matches!(self.heap.get(ti), HeapObj::Object(_)) {
                    if let HeapObj::Object(m) = self.heap.get_mut(ti) {
                        m.extensible = false;
                    }
                } else if !matches!(
                    self.heap.get(ti),
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_)
                ) {
                    self.arr_props.entry(ti).or_insert_with(ObjMap::new).extensible = false;
                }
                Ok(Some(true))
            }
        }
    }

    /// A Proxy's `ownKeys` trap result as a list of property-key Values, or None
    /// for a non-proxy. With no trap, delegates to the target's own (string) keys.
    /// The trap result must be an Array.
    pub(crate) fn proxy_own_keys(&mut self, obj: Value) -> Result<Option<Vec<Value>>, Thrown> {
        if !obj.is_heap() {
            return Ok(None);
        }
        let (target, handler, revoked) = match self.proxy_parts(obj.heap_index()) {
            Some(p) => p,
            None => return Ok(None),
        };
        if revoked {
            return Err(Thrown("TypeError: Cannot perform 'ownKeys' on a revoked proxy".into()));
        }
        match self.proxy_trap(handler, "ownKeys")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target])?;
                let items = match r.is_heap().then(|| self.heap.get(r.heap_index())) {
                    Some(HeapObj::Array(items)) => items.clone(),
                    _ => {
                        return Err(Thrown(
                            "TypeError: proxy [[OwnPropertyKeys]] must return an Array".into(),
                        ))
                    }
                };
                // CreateListFromArrayLike with «String, Symbol» element-type check
                // and the no-duplicate-entries invariant (spec 10.5.11 steps 8-9).
                let mut seen: Vec<String> = Vec::with_capacity(items.len());
                for k in &items {
                    let is_str = k.is_heap() && self.heap.is_str_like(k.heap_index());
                    let is_sym =
                        k.is_heap() && matches!(self.heap.get(k.heap_index()), HeapObj::Symbol { .. });
                    if !is_str && !is_sym {
                        return Err(Thrown(
                            "TypeError: ownKeys trap result must contain only Strings and Symbols"
                                .into(),
                        ));
                    }
                    let id = if is_str {
                        format!("s:{}", self.display(*k))
                    } else {
                        format!("y:{}", k.heap_index())
                    };
                    if seen.contains(&id) {
                        return Err(Thrown(
                            "TypeError: ownKeys trap result must not contain duplicate entries".into(),
                        ));
                    }
                    seen.push(id);
                }
                Ok(Some(items))
            }
            None => {
                let names = self.object_own_property_names(target)?;
                Ok(Some(self.array_snapshot(names.heap_index())))
            }
        }
    }

    pub(crate) fn object_enum_own(&mut self, obj: Value, what: EnumWhat) -> Result<Value, Thrown> {
        // A Proxy enumerates via its ownKeys trap, keeping the STRING keys whose
        // [[GetOwnProperty]] (the gopd trap) reports enumerable.
        if let Some(keys) = self.proxy_own_keys(obj)? {
            let mut out: Vec<Value> = Vec::new();
            for k in keys {
                if !(k.is_heap() && self.heap.is_str_like(k.heap_index())) {
                    continue; // Object.keys/values/entries skip Symbol keys
                }
                let ks = self.display(k);
                let desc = match self.proxy_gopd(obj, &ks)? {
                    Some(d) => d,
                    None => Value::UNDEFINED,
                };
                if desc.is_undefined() {
                    continue;
                }
                let en = self.get_prop(desc, "enumerable")?;
                if !self.truthy(en) {
                    continue;
                }
                match what {
                    EnumWhat::Keys => out.push(k),
                    EnumWhat::Values => {
                        let v = self.get_member(obj, &ks, obj)?;
                        out.push(v);
                    }
                    EnumWhat::Entries => {
                        let v = self.get_member(obj, &ks, obj)?;
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))));
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // A TypedArray enumerates its integer indices `0..length` (all enumerable),
        // then any enumerable named own prop. Handled before the generic match
        // because reading each element needs `&mut self` (ta_element_get).
        if obj.is_heap() && matches!(self.heap.get(obj.heap_index()), HeapObj::TypedArray { .. }) {
            let idx = obj.heap_index();
            let len = self.ta_len_kind(idx).0;
            let mut pairs: Vec<(String, Value)> = Vec::with_capacity(len);
            for i in 0..len {
                let v = self.ta_element_get(idx, i);
                pairs.push((i.to_string(), v));
            }
            if let Some(m) = self.arr_props.get(&idx) {
                for (i, k) in m.keys.iter().enumerate() {
                    if m.attrs[i].enumerable && !is_hidden_key(k) {
                        pairs.push((k.clone(), m.vals[i]));
                    }
                }
            }
            let out: Vec<Value> = pairs
                .into_iter()
                .map(|(k, v)| match what {
                    EnumWhat::Keys => self.alloc_str(k),
                    EnumWhat::Values => v,
                    EnumWhat::Entries => {
                        let ks = self.alloc_str(k);
                        Value::heap(self.heap.alloc(HeapObj::Array(vec![ks, v])))
                    }
                })
                .collect();
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // An Array enumerates its dense indices `0..length` (skipping any special
        // index defineProperty made non-enumerable), then its enumerable named own
        // props. Handled before the generic match because reading an accessor index
        // or a special value needs `&mut self` (get_index / get_member).
        if obj.is_heap() && matches!(self.heap.get(obj.heap_index()), HeapObj::Array(_)) {
            let idx = obj.heap_index();
            let len = match self.heap.get(idx) {
                HeapObj::Array(items) => items.len(),
                _ => 0,
            };
            let mut ks: Vec<String> = Vec::new();
            for i in 0..len {
                if self.array_index_override(idx, i).map_or(true, |(a, _)| a.enumerable) {
                    ks.push(i.to_string());
                }
            }
            if let Some(m) = self.arr_props.get(&idx) {
                for (j, k) in m.keys.iter().enumerate() {
                    if !m.attrs[j].enumerable || is_hidden_key(k) {
                        continue;
                    }
                    // A special index key is already covered by the dense range.
                    if let Ok(n) = k.parse::<usize>() {
                        if n.to_string() == k.as_str() && n < len {
                            continue;
                        }
                    }
                    ks.push(k.clone());
                }
            }
            let mut out: Vec<Value> = Vec::with_capacity(ks.len());
            for k in ks {
                if matches!(what, EnumWhat::Keys) {
                    let kv = self.alloc_str(k);
                    out.push(kv);
                    continue;
                }
                let v = match k.parse::<usize>() {
                    Ok(n) if n.to_string() == k.as_str() => {
                        self.get_index(obj, Value::num(n as f64))?
                    }
                    _ => self.get_member(obj, &k, obj)?,
                };
                match what {
                    EnumWhat::Values => out.push(v),
                    EnumWhat::Entries => {
                        let kv = self.alloc_str(k);
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                    }
                    EnumWhat::Keys => {}
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        let pairs: Vec<(String, Value)> = if obj.is_heap() {
            match self.heap.get(obj.heap_index()) {
                HeapObj::Object(m) => spec_key_order(&m.keys)
                    .into_iter()
                    .filter(|&i| m.attrs[i].enumerable && !is_hidden_key(&m.keys[i]))
                    .map(|i| (m.keys[i].clone(), m.vals[i]))
                    .collect(),
                HeapObj::Array(items) => {
                    let mut v: Vec<(String, Value)> =
                        items.iter().enumerate().map(|(i, x)| (i.to_string(), *x)).collect();
                    // Enumerable named own properties (arr.foo / match-result fields).
                    if let Some(m) = self.arr_props.get(&obj.heap_index()) {
                        for (i, k) in m.keys.iter().enumerate() {
                            if m.attrs[i].enumerable && !is_hidden_key(k) {
                                v.push((k.clone(), m.vals[i]));
                            }
                        }
                    }
                    v
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let out: Vec<Value> = pairs
            .into_iter()
            .map(|(k, v)| match what {
                EnumWhat::Keys => self.alloc_str(k),
                EnumWhat::Values => v,
                EnumWhat::Entries => {
                    let ks = self.alloc_str(k);
                    Value::heap(self.heap.alloc(HeapObj::Array(vec![ks, v])))
                }
            })
            .collect();
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// Build a data property descriptor object `{value, writable, enumerable,
    /// configurable}` (for `Object.getOwnPropertyDescriptor`).
    pub(crate) fn make_data_descriptor(&mut self, value: Value, w: bool, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::bool(w));
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(m)))
    }

    /// Build an accessor descriptor object `{get, set, enumerable, configurable}`.
    pub(crate) fn make_accessor_descriptor(&mut self, get: Value, set: Value, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("get", get);
        m.set("set", set);
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(m)))
    }

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
            None => Ok(Some(self.object_get_own_property_descriptor(target, key))),
            Some(trap) => {
                let kv = self.key_to_value(key);
                let r = self.call_value(trap, handler, &[target, kv])?;
                // The target's own property + extensibility drive the [[GetOwnProperty]]
                // invariants. (Only checked for an ordinary Object target; an exotic
                // target skips them, matching the prior lenient behavior.)
                let (ordinary, t_own, t_cfg, t_wr, t_acc, t_ext) =
                    match self.heap.get(target.heap_index()) {
                        HeapObj::Object(m) => {
                            let ext = m.extensible;
                            match m.pos(key) {
                                Some(i) => (
                                    true,
                                    true,
                                    m.attrs[i].configurable,
                                    m.attrs[i].writable,
                                    m.attrs[i].accessor,
                                    ext,
                                ),
                                None => (true, false, false, false, false, ext),
                            }
                        }
                        _ => (false, false, false, false, false, true),
                    };
                if r.is_undefined() {
                    // Can't report a non-configurable own prop (or any own prop of a
                    // non-extensible target) as non-existent.
                    if ordinary && t_own && (!t_cfg || !t_ext) {
                        return Err(Thrown(
                            "TypeError: proxy getOwnPropertyDescriptor cannot report an existing non-configurable or non-extensible-target property as undefined".into(),
                        ));
                    }
                    return Ok(Some(Value::UNDEFINED));
                }
                // ToPropertyDescriptor (read_descriptor) requires an object, then we
                // re-emit a COMPLETE descriptor (missing fields take their defaults).
                let (value, get, set, wr, en, cf) = self.read_descriptor(r)?;
                // A non-configurable reported descriptor requires a matching
                // non-configurable target property (and, for a non-writable data
                // descriptor, a non-writable target).
                if ordinary && !cf.unwrap_or(false) {
                    if !t_own || t_cfg {
                        return Err(Thrown(
                            "TypeError: proxy getOwnPropertyDescriptor reported a non-configurable descriptor for a configurable or non-existent property".into(),
                        ));
                    }
                    let is_accessor = get.is_some() || set.is_some();
                    if !is_accessor && !wr.unwrap_or(false) && !t_acc && t_wr {
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
    pub(crate) fn object_get_own_property_descriptor(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() || is_private_key(key) {
            return Value::UNDEFINED; // private names aren't reflectable
        }
        let idx = obj.heap_index();
        // A callable's `name`/`length`: non-writable, non-enumerable, configurable.
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return self.make_data_descriptor(v, false, false, true);
            }
        }
        let own = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            HeapObj::Array(items) => {
                if key == "length" {
                    let len = len_value(items.len());
                    return self.make_data_descriptor(len, true, false, false);
                }
                let dense_len = items.len();
                // A special index override (defineProperty'd attrs/accessor) OR a
                // named own property in arr_props wins; else a dense in-range index
                // is a default { writable, enumerable, configurable } data property.
                let ovr =
                    self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|p| (m.attrs[p], m.vals[p])));
                if ovr.is_some() {
                    ovr
                } else {
                    match key.parse::<usize>() {
                        Ok(i) if i.to_string() == key && i < dense_len => {
                            let v = items[i];
                            return self.make_data_descriptor(v, true, true, true);
                        }
                        _ => None,
                    }
                }
            }
            // Class static members: data props, plus `static get`/`set` rendered
            // as an accessor descriptor (raw = getter, attr.setter = setter).
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
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
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
                HeapObj::Object(m) => keys.extend(
                    spec_key_order(&m.keys)
                        .into_iter()
                        .map(|i| &m.keys[i])
                        .filter(|k| !is_hidden_key(k))
                        .cloned(),
                ),
                HeapObj::Array(items) => {
                    let dense_len = items.len();
                    for i in 0..dense_len {
                        keys.push(i.to_string());
                    }
                    keys.push("length".to_string());
                    if let Some(m) = self.arr_props.get(&idx) {
                        // Named own props only — a special index key in arr_props is
                        // already covered by the dense `0..len` range above.
                        keys.extend(
                            m.keys
                                .iter()
                                .filter(|k| {
                                    if is_hidden_key(k) {
                                        return false;
                                    }
                                    if let Ok(n) = k.parse::<usize>() {
                                        if n.to_string() == k.as_str() && n < dense_len {
                                            return false;
                                        }
                                    }
                                    true
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
                    if has_length {
                        keys.push("length".to_string());
                    }
                    if has_name {
                        keys.push("name".to_string());
                    }
                    keys.extend(c.statics.keys.iter().filter(|k| !is_hidden_key(k)).cloned());
                    for (n, _) in &c.static_getters {
                        if !is_hidden_key(n) && !keys.iter().any(|k| k == n) {
                            keys.push(n.clone());
                        }
                    }
                    for (n, _) in &c.static_setters {
                        if !is_hidden_key(n) && !keys.iter().any(|k| k == n) {
                            keys.push(n.clone());
                        }
                    }
                }
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Bound { .. }
                | HeapObj::BoundResolver { .. }
                | HeapObj::Native(_) => {
                    if has_length {
                        keys.push("length".to_string());
                    }
                    if has_name {
                        keys.push("name".to_string());
                    }
                    if let Some(m) = self.fn_props.get(&idx) {
                        keys.extend(m.keys.iter().filter(|k| !is_hidden_key(k)).cloned());
                    }
                }
                _ => {}
            }
        }
        let names: Vec<Value> = keys.into_iter().map(|k| self.alloc_str(k)).collect();
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(names))))
    }

    /// `Object.getPrototypeOf(obj)` — the prototype: a class instance's is its
    /// class's `.prototype`; an `Object.create`d object's is the recorded proto;
    /// otherwise `null` (a plain object's real `Object.prototype` isn't modelled).
    /// Walk `this`'s prototype chain for an own accessor property `key`, returning
    /// its getter (or setter if `want_setter`). Stops at the first own property
    /// (returning undefined for a data property). Backs `__lookupGetter__`/Setter.
    pub(crate) fn lookup_accessor(&mut self, this: Value, key: &str, want_setter: bool) -> Value {
        let mut cur = this;
        for _ in 0..10_000 {
            if !cur.is_heap() {
                break;
            }
            if let HeapObj::Object(m) = self.heap.get(cur.heap_index()) {
                if let Some(i) = m.pos(key) {
                    let attr = m.attrs[i];
                    if attr.accessor {
                        return if want_setter { attr.setter } else { m.vals[i] };
                    }
                    return Value::UNDEFINED;
                }
            }
            cur = self.object_get_prototype_of(cur);
            if cur == Value::NULL {
                break;
            }
        }
        Value::UNDEFINED
    }

    /// If `idx` is a generator / async / async-generator FUNCTION, the matching
    /// dynamic-function intrinsic prototype (%GeneratorFunction.prototype% etc.) —
    /// its [[Prototype]] and the target for its method/`.constructor` lookups.
    /// `None` for plain functions (which use %Function.prototype%) and non-callables.
    pub(crate) fn callable_dynfn_proto(&self, idx: u32) -> Option<u32> {
        let fid = match self.heap.get(idx) {
            HeapObj::Func(f) => *f,
            HeapObj::Closure { func, .. } => *func,
            _ => return None,
        };
        let p = self.func(fid as usize);
        match (p.is_generator, p.is_async) {
            (true, true) => (self.asyncgen_fn_proto != 0).then_some(self.asyncgen_fn_proto),
            (true, false) => (self.gen_fn_proto != 0).then_some(self.gen_fn_proto),
            (false, true) => (self.async_fn_proto != 0).then_some(self.async_fn_proto),
            (false, false) => None,
        }
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
            | HeapObj::Native(_) => (None, false, 1),
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
                    let target_proto = self.object_get_prototype_of(target);
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
        // getters). Gather presence first (so the &mut get_prop calls don't clash).
        let p_value = self.has_property_str(desc, "value");
        let p_get = self.has_property_str(desc, "get");
        let p_set = self.has_property_str(desc, "set");
        let p_writable = self.has_property_str(desc, "writable");
        let p_enumerable = self.has_property_str(desc, "enumerable");
        let p_configurable = self.has_property_str(desc, "configurable");
        let value = if p_value { Some(self.get_prop(desc, "value")?) } else { None };
        let get = if p_get { Some(self.get_prop(desc, "get")?) } else { None };
        let set = if p_set { Some(self.get_prop(desc, "set")?) } else { None };
        let writable = if p_writable {
            let v = self.get_prop(desc, "writable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let enumerable = if p_enumerable {
            let v = self.get_prop(desc, "enumerable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let configurable = if p_configurable {
            let v = self.get_prop(desc, "configurable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        // ToPropertyDescriptor validation (6.2.5.5 steps 13-21): a present getter
        // or setter must be callable (or `undefined`), and an accessor descriptor
        // may not also carry data fields (`value`/`writable`).
        if let Some(g) = get {
            if g != Value::UNDEFINED && !self.is_callable(g) {
                return Err(Thrown("TypeError: Getter must be a function".into()));
            }
        }
        if let Some(s) = set {
            if s != Value::UNDEFINED && !self.is_callable(s) {
                return Err(Thrown("TypeError: Setter must be a function".into()));
            }
        }
        if (get.is_some() || set.is_some()) && (value.is_some() || writable.is_some()) {
            return Err(Thrown(
                "TypeError: Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"
                    .into(),
            ));
        }
        Ok((value, get, set, writable, enumerable, configurable))
    }

    /// `Object.defineProperty(obj, key, descriptor)` — define/redefine an own
    /// property with explicit attributes (unspecified attrs default to false on a
    /// new property; an existing non-configurable property rejects most changes).
    pub(crate) fn object_define_property(&mut self, obj: Value, key: &str, desc: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
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
                    let desc_obj = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                    let kv = self.key_to_value(key);
                    let r = self.call_value(trap, handler, &[target, kv, desc_obj])?;
                    if !self.truthy(r) {
                        return Err(Thrown(format!(
                            "TypeError: proxy 'defineProperty' trap returned falsish for property '{key}'"
                        )));
                    }
                    // [[DefineOwnProperty]] invariant (10.5.6 steps 16-21): validate
                    // the truthy trap result against the target's descriptor.
                    let target_desc = self.object_get_own_property_descriptor(target, key);
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
                if i.to_string() == key {
                    if i >= crate::vm::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: array index exceeds the engine's dense-array limit".into(),
                        ));
                    }
                    let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
                    let key_i = i.to_string();
                    // Current descriptor of index i: a special arr_props entry wins;
                    // else the dense slot (a default data property) if in range.
                    let (dense_len, dense_val) = match self.heap.get(idx) {
                        HeapObj::Array(items) => (items.len(), items.get(i).copied()),
                        _ => (0, None),
                    };
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
                    let extensible = self.arr_props.get(&idx).map_or(true, |m| m.extensible);
                    let (attr, stored) = self.merge_property_descriptor(
                        &key_i, existing, extensible, value, get, set, d_wr, d_en, d_cf,
                    )?;
                    let is_default_data = !attr.accessor
                        && attr.writable
                        && attr.enumerable
                        && attr.configurable;
                    if is_default_data {
                        // Lives in the dense Vec; drop any stale special override.
                        if let Some(m) = self.arr_props.get_mut(&idx) {
                            m.remove(&key_i);
                        }
                        if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                            if i >= items.len() {
                                items.resize(i + 1, Value::UNDEFINED);
                            }
                            items[i] = stored;
                        }
                    } else {
                        // Special: store in arr_props and keep a dense placeholder so
                        // `length` counts the index.
                        if i >= dense_len {
                            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                                items.resize(i + 1, Value::UNDEFINED);
                            }
                        }
                        self.arr_props
                            .entry(idx)
                            .or_insert_with(ObjMap::new)
                            .define(&key_i, stored, attr);
                    }
                    self.heap.bump_version(idx);
                    return Ok(());
                }
            }
            if key == "length" {
                // `length` is a non-configurable, non-enumerable, WRITABLE data
                // property (ArraySetLength, 15.4.5.1). Reject making it configurable
                // or enumerable, or turning it into an accessor.
                let (value, get, set, _d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
                if get.is_some() || set.is_some() || d_cf == Some(true) || d_en == Some(true) {
                    return Err(Thrown("TypeError: Cannot redefine property: length".into()));
                }
                if let Some(v) = value {
                    let n = self.to_number_coerce(v)?;
                    if !(n >= 0.0 && n.fract() == 0.0 && n < 4_294_967_296.0) {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    if n as usize > crate::vm::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: array length exceeds the engine's dense-array limit".into(),
                        ));
                    }
                    let new_len = n as usize;
                    let cur_len = match self.heap.get(idx) {
                        HeapObj::Array(items) => items.len(),
                        _ => 0,
                    };
                    // Shrinking past a NON-configurable index is forbidden: the
                    // element can't be deleted, so length stops there and a
                    // TypeError is thrown (ArraySetLength steps 16-17).
                    if new_len < cur_len {
                        for i in new_len..cur_len {
                            if let Some((a, _)) = self.array_index_override(idx, i) {
                                if !a.configurable {
                                    return Err(Thrown(
                                        "TypeError: Cannot redefine property: length".into(),
                                    ));
                                }
                            }
                        }
                        // Drop any (configurable) special overrides being truncated.
                        if let Some(m) = self.arr_props.get_mut(&idx) {
                            for i in new_len..cur_len {
                                m.remove(&i.to_string());
                            }
                        }
                    }
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        items.resize(new_len, Value::UNDEFINED);
                    }
                    self.heap.bump_version(idx);
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
        // 0 = plain object, 1 = class (own props live in `statics`), 2 = callable
        // (own props live in `fn_props`), 3 = the generic side table `arr_props`
        // (array named props + every other exotic object: Map/Set/Date/Promise/
        // RegExp/Weak*/…). String/Symbol/BigInt are PRIMITIVES → "non-object".
        let target = match self.heap.get(idx) {
            HeapObj::Object(_) => 0u8,
            HeapObj::Class(_) => 1,
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => 2,
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) => {
                return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
            }
            _ => 3, // Array named prop + exotic objects -> arr_props side table
        };
        // A callable's/class's `name`/`length`/`prototype` are synthesized; accept
        // the call but don't shadow them (full redefinition isn't modelled).
        if target != 0 && matches!(key, "name" | "length" | "prototype") {
            return Ok(());
        }
        let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
        // The existing descriptor lives wherever `target` writes (below).
        let existing = match target {
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
        let extensible = match self.heap.get(idx) {
            HeapObj::Object(m) => m.extensible,
            _ => true,
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
                self.arr_props.entry(idx).or_insert_with(ObjMap::new).define(key, stored, attr);
            }
            _ => {
                self.fn_props.entry(idx).or_insert_with(ObjMap::new).define(key, stored, attr);
            }
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
        // Start from the existing attrs (redefine) or all-false (new property).
        let (mut wr, mut en, mut cf) = match existing {
            Some((a, _)) => (a.writable, a.enumerable, a.configurable),
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
            accessor: is_accessor,
            setter: if is_accessor {
                set.or(existing_set).unwrap_or(Value::UNDEFINED)
            } else {
                Value::UNDEFINED
            },
        };
        let stored = if is_accessor {
            get.or(existing_get).unwrap_or(Value::UNDEFINED)
        } else {
            value.or(existing.map(|(_, v)| v)).unwrap_or(Value::UNDEFINED)
        };
        Ok((attr, stored))
    }

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
        let p = m.pos(&i.to_string())?;
        Some((m.attrs[p], m.vals[p]))
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
        let enum_keys = |m: &ObjMap| -> Vec<String> {
            m.keys
                .iter()
                .zip(m.attrs.iter())
                .filter(|(_, a)| a.enumerable)
                .map(|(k, _)| k.clone())
                .collect()
        };
        // OwnPropertyKeys(ToObject(props)) filtered to enumerable. The descriptor
        // bag may be any object — a function (own props in fn_props) or an exotic
        // object (arr_props) — not only a plain Object.
        let keys: Vec<String> = match self.heap.get(pidx) {
            HeapObj::Object(m) => enum_keys(m),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                self.fn_props.get(&pidx).map(enum_keys).unwrap_or_default()
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
    pub(crate) fn global_by_name(&self, name: &str) -> Option<Value> {
        let slot = self.program.global_names.iter().position(|n| n == name)?;
        // A never-declared slot reads as "absent" for `globalThis.x` (→ undefined),
        // not the internal sentinel.
        match self.globals.get(slot).copied() {
            Some(v) if v.is_uninitialized() => None,
            other => other,
        }
    }

    /// Look up `key` on a built-in prototype object (`arr_proto`/`str_proto`),
    /// returning the method value (or undefined). Lets primitive array/string
    /// values expose their methods as first-class values.
    pub(crate) fn proto_member(&self, proto: u32, key: &str) -> Value {
        // Walk the full prototype chain via `proto_of`. Most type prototypes chain
        // directly to Object.prototype, but a TypedArray instance's prototype chain
        // has an intermediate level (`Int8Array.prototype` -> `%TypedArray%.prototype`
        // -> `Object.prototype`), so a 2-level lookup missed the shared methods.
        let mut cur = proto;
        let mut guard = 0u32;
        while cur != 0 && guard < 64 {
            guard += 1;
            if let HeapObj::Object(m) = self.heap.get(cur) {
                if let Some(v) = m.get(key) {
                    return v;
                }
            }
            match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => cur = p.heap_index(),
                _ => break,
            }
        }
        // Type prototypes that don't explicitly record `proto_of` -> Object.prototype
        // still inherit from it (`[].hasOwnProperty`, `(5).isPrototypeOf`, etc.).
        if self.obj_proto != 0 && proto != self.obj_proto {
            if let HeapObj::Object(m) = self.heap.get(self.obj_proto) {
                if let Some(v) = m.get(key) {
                    return v;
                }
            }
        }
        Value::UNDEFINED
    }

    pub(crate) fn callable_name_length(&self, obj: Value) -> Option<(String, i32)> {
        let clean = |n: &str| -> String {
            if n.starts_with('<') { String::new() } else { n.to_string() }
        };
        match self.heap.get(obj.heap_index()) {
            HeapObj::Func(fid) => {
                let p = self.func(*fid as usize);
                Some((clean(&p.name), p.length as i32))
            }
            HeapObj::Closure { func, .. } => {
                let p = self.func(*func as usize);
                Some((clean(&p.name), p.length as i32))
            }
            // The resolve/reject functions of `new Promise(executor)`, and the
            // Promise.all/allSettled/any resolve/reject ELEMENT functions: anonymous
            // (name ""), length 1, with %Function.prototype% as [[Prototype]].
            HeapObj::BoundResolver { .. } | HeapObj::CombinatorResolver { .. } => {
                Some((String::new(), 1))
            }
            HeapObj::Class(c) => {
                let len = c
                    .ctor
                    .map(|f| self.func(f as usize).length as i32)
                    .unwrap_or(0);
                Some((clean(&c.name), len))
            }
            // A native value's `name`/`length`: a prototype method
            // (`Array.prototype.map.name === "map"`, length 1) or a static/namespace
            // method (`Object.keys.name === "keys"`, `Reflect.get.length === 2`).
            HeapObj::Native(id) => {
                let id = *id;
                // Accessor getters carry name "get <prop>" and length 0.
                if (native::BUFFER_GETTER_BASE
                    ..native::BUFFER_GETTER_BASE + native::BUFFER_GETTERS.len() as u16)
                    .contains(&id)
                {
                    let (name, _) = native::BUFFER_GETTERS[(id - native::BUFFER_GETTER_BASE) as usize];
                    return Some((format!("get {name}"), 0));
                }
                if (native::SAB_GETTER_BASE
                    ..native::SAB_GETTER_BASE + native::SAB_GETTERS.len() as u16)
                    .contains(&id)
                {
                    let name = native::SAB_GETTERS[(id - native::SAB_GETTER_BASE) as usize];
                    return Some((format!("get {name}"), 0));
                }
                native::proto_method(id)
                    .map(|(n, _, l)| (n.to_string(), l as i32))
                    .or_else(|| native::math_method(id).map(|(n, _, l)| (n.to_string(), l as i32)))
                    .or_else(|| native::static_name_length(id).map(|(n, l)| (n.to_string(), l as i32)))
            }
            HeapObj::Bound { target, args, .. } if target.is_heap() => {
                // The anonymous functions returned by the Intl format/compare
                // getters have name "" and length 1 (format) / 2 (compare).
                if let HeapObj::Native(tid) = self.heap.get(target.heap_index()) {
                    match *tid {
                        native::INTL_NF_FORMAT | native::INTL_DTF_FORMAT => {
                            return Some((String::new(), 1));
                        }
                        native::INTL_COLLATOR_COMPARE => return Some((String::new(), 2)),
                        _ => {}
                    }
                }
                // A bound function F: name is "bound " + target.name, and length is
                // max(0, target.length - boundArgsCount) when the target has a
                // numeric length (BoundFunctionCreate / SetFunctionLength+Name).
                let nbound = args.len() as i32;
                let (tname, tlen) =
                    self.callable_name_length(*target).unwrap_or((String::new(), 0));
                Some((format!("bound {tname}"), (tlen - nbound).max(0)))
            }
            _ => None,
        }
    }

    /// Does this callable expose `key` (`name`/`length`) as an own property right
    /// now? True for any named callable unless that intrinsic was `delete`d.
    pub(crate) fn callable_has_intrinsic(&self, obj: Value, key: &str) -> bool {
        let bit = match key {
            "name" => 0u8,
            "length" => 1u8,
            _ => return false,
        };
        if !obj.is_heap() || self.deleted_callable_intrinsics.contains(&(obj.heap_index(), bit)) {
            return false;
        }
        // An EXPLICIT own `name`/`length` overrides the synthesized intrinsic: a
        // class with `static name(){}` / `static name = …` / `static get name()`
        // (or a function carrying an assigned `name`) keeps that real property.
        // Per spec SetFunctionName is skipped when the object already has `name`,
        // so NamedEvaluation must not clobber it.
        let idx = obj.heap_index();
        let has_explicit_own = match self.heap.get(idx) {
            HeapObj::Class(c) => {
                c.statics.pos(key).is_some()
                    || c.static_getters.iter().any(|(k, _)| k == key)
                    || c.static_setters.iter().any(|(k, _)| k == key)
            }
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                self.fn_props.get(&idx).is_some_and(|m| m.pos(key).is_some())
            }
            _ => false,
        };
        if has_explicit_own {
            return false;
        }
        self.callable_name_length(obj).is_some()
    }

    /// The current value of a callable's `name`/`length` own property (allocating
    /// the name string), or None if absent/deleted.
    pub(crate) fn callable_intrinsic_value(&mut self, obj: Value, key: &str) -> Option<Value> {
        if !self.callable_has_intrinsic(obj, key) {
            return None;
        }
        let (nm, len) = self.callable_name_length(obj)?;
        Some(if key == "name" { self.alloc_str(nm) } else { Value::int(len) })
    }

    #[inline]
    pub(crate) fn get_prop(&mut self, obj: Value, key: &str) -> Result<Value, Thrown> {
        self.get_member(obj, key, obj)
    }

    /// Read `key` from an exotic object (Map/Set/Date/Promise/…): a defineProperty
    /// own property in the generic `arr_props` side table (invoking a getter with
    /// `this = obj`), else delegate to the type's prototype. Lets these objects
    /// carry own properties defined via `Object.defineProperty`.
    fn exotic_own_or_proto(&mut self, obj: Value, proto: u32, key: &str) -> Result<Value, Thrown> {
        let found = self
            .arr_props
            .get(&obj.heap_index())
            .and_then(|m| m.pos(key).map(|i| (m.attrs[i].accessor, m.vals[i])));
        if let Some((is_accessor, v)) = found {
            if is_accessor {
                return if v == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { self.call_value(v, obj, &[]) };
            }
            return Ok(v);
        }
        Ok(self.proto_member(proto, key))
    }

    /// Property GET with an explicit `receiver` — the original object a lookup
    /// started from. It equals `obj` at the top level; during prototype-chain
    /// delegation `obj` advances up the chain while `receiver` stays the original,
    /// so an INHERITED accessor's getter is invoked with the correct `this`.
    pub(crate) fn get_member(&mut self, obj: Value, key: &str, receiver: Value) -> Result<Value, Thrown> {
        // Proxy `get` trap (or fall through to the target).
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'get' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "get")? {
                    Some(trap) => {
                        let kv = self.key_to_value(key);
                        self.call_value(trap, handler, &[target, kv, obj])
                    }
                    None => self.get_member(target, key, receiver),
                };
            }
        }
        if !obj.is_heap() {
            // Reading a property of null/undefined throws a TypeError (matches
            // JS); other primitives (number/bool) have no own props here → undef.
            if obj.is_nullish() {
                return Err(Thrown(format!(
                    "TypeError: Cannot read properties of {} (reading '{key}')",
                    if obj.is_null() { "null" } else { "undefined" }
                )));
            }
            // A number/boolean PRIMITIVE delegates method-as-value access to
            // Number/Boolean.prototype (`(5).toFixed`, `true.valueOf`).
            if obj.is_number() {
                return Ok(self.proto_member(self.num_proto, key));
            }
            if obj.is_bool() {
                return Ok(self.proto_member(self.bool_proto, key));
            }
            return Ok(Value::UNDEFINED);
        }
        // A function's / class's `.name` and `.length` — synthesized own data
        // properties (configurable, so a prior `delete` suppresses them).
        if key == "name" || key == "length" {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return Ok(v);
            }
        }
        // A function's / class's `.prototype` (a lazily-created, stable object).
        if key == "prototype" {
            if let Some(p) = self.prototype_of(obj) {
                return Ok(p);
            }
        }
        // A RegExp's accessor-like own properties (source/flags/lastIndex + the
        // flag booleans) and its match-result Array's `.index`/`.input`/`.groups`.
        // Cloned out of the heap borrow before any allocation.
        if let HeapObj::RegExp { source, flags, last_index, .. } = self.heap.get(obj.heap_index()) {
            let (s, f, li) = (source.clone(), flags.clone(), *last_index);
            // A custom own property (`re.exec = fn`, `re.x = …`) in the side table
            // shadows the prototype. The regexp's own/accessor keys
            // (lastIndex/source/flags/flag-booleans) always come from
            // regexp_get_prop, so a stray side-table entry for one is ignored.
            if !is_regexp_own_key(key) {
                let entry = self
                    .arr_props
                    .get(&obj.heap_index())
                    .and_then(|m| m.pos(key).map(|i| (m.vals[i], m.attrs[i])));
                if let Some((raw, attr)) = entry {
                    if attr.accessor {
                        return if raw == Value::UNDEFINED {
                            Ok(Value::UNDEFINED)
                        } else {
                            self.call_value(raw, receiver, &[])
                        };
                    }
                    return Ok(raw);
                }
            }
            return self.regexp_get_prop(&s, &f, li, key);
        }
        // An Array's named (non-index) own properties (arr.foo, and a match
        // result's index/input/groups) live in arr_props and shadow the prototype.
        let arr_entry =
            self.arr_props.get(&obj.heap_index()).and_then(|m| m.pos(key).map(|i| (m.vals[i], m.attrs[i])));
        if let Some((raw, attr)) = arr_entry {
            if attr.accessor {
                return if raw == Value::UNDEFINED {
                    Ok(Value::UNDEFINED)
                } else {
                    self.call_value(raw, receiver, &[])
                };
            }
            return Ok(raw);
        }
        // TypedArray / ArrayBuffer / DataView instance properties.
        if let HeapObj::TypedArray { buffer, kind, byte_offset, .. } = self.heap.get(obj.heap_index()) {
            let (buffer, kind, byte_offset) = (*buffer, *kind, *byte_offset);
            let size = native::TA_KINDS[kind as usize].1;
            // A canonical numeric string index reads the element.
            if let Ok(i) = key.parse::<usize>() {
                return Ok(self.ta_element_get(obj.heap_index(), i));
            }
            // Out of bounds (detached, or shrunk past this view) reports
            // length/byteLength/byteOffset as 0; a length-tracking view reflects
            // the buffer's current size.
            let eff = self.ta_effective_len(obj.heap_index());
            return Ok(match key {
                "length" => Value::num(eff.unwrap_or(0) as f64),
                "byteLength" => Value::num((eff.unwrap_or(0) * size) as f64),
                "byteOffset" => Value::num(if eff.is_none() { 0.0 } else { byte_offset as f64 }),
                "BYTES_PER_ELEMENT" => Value::num(size as f64),
                "buffer" => Value::heap(buffer),
                "@@toStringTag" => self.alloc_str(native::TA_KINDS[kind as usize].0.to_string()),
                _ => self.proto_member(self.ta_protos[kind as usize], key),
            });
        }
        if let HeapObj::ArrayBuffer { data, .. } = self.heap.get(obj.heap_index()) {
            let len = data.len();
            let ai = obj.heap_index();
            let max = self.ab_max.get(&ai).copied();
            // A SharedArrayBuffer: `growable` (not `resizable`), never `detached`,
            // methods/@@toStringTag from %SharedArrayBuffer.prototype%.
            let shared = self.shared_buffers.contains(&ai);
            let immut = self.immutable_buffers.contains(&ai);
            return Ok(match key {
                "byteLength" => Value::num(len as f64),
                // An immutable buffer is fixed-size and never resizable.
                "maxByteLength" => Value::num(max.unwrap_or(len) as f64),
                "immutable" if !shared => Value::bool(immut),
                "growable" if shared => Value::bool(max.is_some()),
                "resizable" if !shared => Value::bool(max.is_some() && !immut),
                "detached" if !shared => Value::bool(
                    matches!(self.heap.get(ai), HeapObj::ArrayBuffer { detached: true, .. }),
                ),
                _ => {
                    let proto = if shared { self.sab_proto } else { self.arraybuffer_proto };
                    self.proto_member(proto, key)
                }
            });
        }
        if let HeapObj::DataView { buffer, byte_offset, byte_length } = self.heap.get(obj.heap_index()) {
            let (buffer, byte_offset, byte_length) = (*buffer, *byte_offset, *byte_length);
            return Ok(match key {
                "byteLength" => Value::num(byte_length as f64),
                "byteOffset" => Value::num(byte_offset as f64),
                "buffer" => Value::heap(buffer),
                _ => self.proto_member(self.dataview_proto, key),
            });
        }
        // Temporal.Duration: field getters + sign/blank; methods via the prototype.
        if let HeapObj::Temporal { kind: 0, .. } = self.heap.get(obj.heap_index()) {
            let f = self.duration_fields(obj.heap_index()).unwrap_or([0; 10]);
            if let Some(i) = native::DURATION_FIELDS.iter().position(|n| *n == key) {
                return Ok(Value::num(f[i] as f64));
            }
            return Ok(match key {
                "sign" => Value::num(Self::duration_sign(&f) as f64),
                "blank" => Value::bool(f.iter().all(|&x| x == 0)),
                _ => self.proto_member(self.duration_proto, key),
            });
        }
        // Temporal.PlainDate getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 1, .. } = self.heap.get(obj.heap_index()) {
            let (y, m, d) = self.plain_date_fields(obj.heap_index()).unwrap_or((0, 0, 0));
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "day" => Value::num(d as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "dayOfYear" => {
                    Value::num((iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1) as f64)
                }
                "weekOfYear" => Value::num(iso_week_of_year(y, m, d) as f64),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "daysInWeek" => Value::num(7.0),
                "monthsInYear" => Value::num(12.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                "era" | "eraYear" => Value::UNDEFINED,
                _ => self.proto_member(self.plaindate_proto, key),
            });
        }
        // Temporal.PlainTime getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 2, .. } = self.heap.get(obj.heap_index()) {
            let f = self.plain_time_fields(obj.heap_index()).unwrap_or([0; 6]);
            return Ok(match key {
                "hour" => Value::num(f[0] as f64),
                "minute" => Value::num(f[1] as f64),
                "second" => Value::num(f[2] as f64),
                "millisecond" => Value::num(f[3] as f64),
                "microsecond" => Value::num(f[4] as f64),
                "nanosecond" => Value::num(f[5] as f64),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plaintime_proto, key),
            });
        }
        // Temporal.PlainDateTime getters (date + time); methods via the prototype.
        if let HeapObj::Temporal { kind: 3, .. } = self.heap.get(obj.heap_index()) {
            let f = self.pdt_fields(obj.heap_index()).unwrap_or([0; 9]);
            let (y, m, d) = (f[0], f[1], f[2]);
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "day" => Value::num(d as f64),
                "hour" => Value::num(f[3] as f64),
                "minute" => Value::num(f[4] as f64),
                "second" => Value::num(f[5] as f64),
                "millisecond" => Value::num(f[6] as f64),
                "microsecond" => Value::num(f[7] as f64),
                "nanosecond" => Value::num(f[8] as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "dayOfYear" => {
                    Value::num((iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1) as f64)
                }
                "weekOfYear" => Value::num(iso_week_of_year(y, m, d) as f64),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "daysInWeek" => Value::num(7.0),
                "monthsInYear" => Value::num(12.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                "era" | "eraYear" => Value::UNDEFINED,
                _ => self.proto_member(self.plaindatetime_proto, key),
            });
        }
        // Temporal.Instant getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 4, .. } = self.heap.get(obj.heap_index()) {
            let ns = self.instant_ns(obj.heap_index()).unwrap_or(0);
            return Ok(match key {
                "epochMilliseconds" => Value::num((ns / 1_000_000) as f64),
                "epochNanoseconds" => self.make_bigint(ns),
                "epochSeconds" => Value::num((ns / 1_000_000_000) as f64),
                "epochMicroseconds" => self.make_bigint(ns / 1_000),
                _ => self.proto_member(self.instant_proto, key),
            });
        }
        // Temporal.PlainYearMonth getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 5, fields } = self.heap.get(obj.heap_index()) {
            let (y, m) = (fields[0], fields[1]);
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "monthsInYear" => Value::num(12.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "era" | "eraYear" => Value::UNDEFINED,
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plainyearmonth_proto, key),
            });
        }
        // Temporal.PlainMonthDay getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 6, fields } = self.heap.get(obj.heap_index()) {
            let (m, d) = (fields[1], fields[2]);
            return Ok(match key {
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "day" => Value::num(d as f64),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plainmonthday_proto, key),
            });
        }
        // Temporal.ZonedDateTime getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 7, .. } = self.heap.get(obj.heap_index()) {
            let idx = obj.heap_index();
            let f = self.zdt_local(idx); // [y,mo,d,h,mi,s,ms,us,ns]
            let (y, m, d) = (f[0], f[1], f[2]);
            let epoch = self.zdt_epoch_ns(idx).unwrap_or(0);
            let off = self.zdt_offset_ns(idx);
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "day" => Value::num(d as f64),
                "hour" => Value::num(f[3] as f64),
                "minute" => Value::num(f[4] as f64),
                "second" => Value::num(f[5] as f64),
                "millisecond" => Value::num(f[6] as f64),
                "microsecond" => Value::num(f[7] as f64),
                "nanosecond" => Value::num(f[8] as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "dayOfYear" => {
                    Value::num((iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1) as f64)
                }
                "weekOfYear" => Value::num(iso_week_of_year(y, m, d) as f64),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "daysInWeek" => Value::num(7.0),
                "monthsInYear" => Value::num(12.0),
                "hoursInDay" => Value::num(24.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                "era" | "eraYear" => Value::UNDEFINED,
                "epochSeconds" => Value::num((epoch / 1_000_000_000) as f64),
                "epochMilliseconds" => Value::num((epoch / 1_000_000) as f64),
                "epochMicroseconds" => self.make_bigint(epoch / 1_000),
                "epochNanoseconds" => self.make_bigint(epoch),
                "offsetNanoseconds" => Value::num(off as f64),
                "offset" => {
                    let sign = if off < 0 { '-' } else { '+' };
                    let tot = off.abs() / 1_000_000_000;
                    let (h, mi, s) = (tot / 3600, (tot % 3600) / 60, tot % 60);
                    let str = if s == 0 {
                        format!("{sign}{h:02}:{mi:02}")
                    } else {
                        format!("{sign}{h:02}:{mi:02}:{s:02}")
                    };
                    self.alloc_str(str)
                }
                "timeZoneId" => self
                    .zdt_tz
                    .get(&idx)
                    .copied()
                    .unwrap_or_else(|| Value::UNDEFINED),
                _ => self.proto_member(self.zoneddatetime_proto, key),
            });
        }
        // Intl.* instance: resolve the key on its prototype chain (service proto →
        // Object.prototype), invoking accessor getters (Locale subtags, format/
        // compare) with this = the instance.
        if let HeapObj::Intl { kind, .. } = self.heap.get(obj.heap_index()) {
            let proto = self.intl_protos[*kind as usize];
            let found = self.own_member(proto, key).or_else(|| {
                if self.obj_proto != 0 {
                    self.own_member(self.obj_proto, key)
                } else {
                    None
                }
            });
            if let Some((attr, raw)) = found {
                if attr.accessor {
                    return if raw == Value::UNDEFINED {
                        Ok(Value::UNDEFINED)
                    } else {
                        self.call_value(raw, receiver, &[])
                    };
                }
                return Ok(raw);
            }
            return Ok(Value::UNDEFINED);
        }
        // Own data/accessor property on a plain object. Extracted BEFORE the type
        // match so an accessor's getter can be invoked outside the heap borrow.
        let own = match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            _ => None,
        };
        if let Some((a, raw)) = own {
            if a.accessor {
                // `raw` is the getter (UNDEFINED ⇒ no getter ⇒ read is undefined).
                return if raw == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { self.call_value(raw, receiver, &[]) };
            }
            return Ok(raw);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                if key == "length" {
                    Ok(len_value(items.len()))
                } else if let Some(i) = key.parse::<u32>().ok().filter(|i| i.to_string() == key) {
                    // Element access via a canonical numeric STRING key (`arr["0"]`,
                    // object-pattern destructuring `{0: x} = arr`): GetProp must read
                    // the element, like GetIndex.
                    Ok(items.get(i as usize).copied().unwrap_or(Value::UNDEFINED))
                } else if key == "raw" {
                    // A tagged-template strings array's `.raw` (side table).
                    Ok(self.template_raws.get(&obj.heap_index()).copied().unwrap_or(Value::UNDEFINED))
                } else {
                    // A method as a VALUE (`arr.map`, `arr.slice`, …) → Array.prototype.
                    Ok(self.proto_member(self.arr_proto, key))
                }
            }
            HeapObj::Str(s) => {
                if key == "length" {
                    Ok(len_value(s.char_len))
                } else {
                    Ok(self.proto_member(self.str_proto, key))
                }
            }
            HeapObj::Cons { len, .. } => {
                if key == "length" {
                    Ok(len_value(*len))
                } else {
                    Ok(self.proto_member(self.str_proto, key))
                }
            }
            HeapObj::Object(map) => {
                if let Some(v) = map.get(key) {
                    return Ok(v);
                }
                // `globalThis.X` → the reserved global slot named X.
                if obj.heap_index() == self.global_this && self.global_this != 0 {
                    if let Some(v) = self.global_by_name(key) {
                        return Ok(v);
                    }
                }
                // Own-property miss: walk the class chain for an inherited method
                // (return its func) or getter (invoke it with this = obj).
                let class = map.class;
                let is_ctor = map.is_ctor;
                let (mut method, mut getter) = (None, None);
                let mut cur = class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                                method = Some(*v);
                                break;
                            }
                            if let Some((_, v)) = c.getters.iter().find(|(k, _)| k == key) {
                                getter = Some(*v);
                                break;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                if let Some(m) = method {
                    return Ok(m);
                }
                if let Some(g) = getter {
                    return self.call_value(g, receiver, &[]);
                }
                // Own + class miss: delegate up the prototype chain — an explicit
                // `Object.create` proto, else a class instance's `C.prototype`
                // (carries `constructor` + inherited methods, and itself chains to
                // Object.prototype), else the base Object.prototype.
                let proto = if let Some(&p) = self.proto_of.get(&obj.heap_index()) {
                    p.is_heap().then_some(p)
                } else if let Some(cidx) = class {
                    self.prototype_of(Value::heap(cidx))
                } else if is_ctor && self.fn_proto != 0 {
                    // A constructor object (Array/Object/…) inherits Function.prototype
                    // methods (`Array.bind`, `Object.call`), matching its [[Prototype]].
                    Some(Value::heap(self.fn_proto))
                } else if self.obj_proto != 0 && obj.heap_index() != self.obj_proto {
                    Some(Value::heap(self.obj_proto))
                } else {
                    None
                };
                match proto {
                    Some(p) => self.get_member(p, key, receiver),
                    None => Ok(Value::UNDEFINED),
                }
            }
            // Static members are own properties of the class value; statics are
            // inherited, so walk the `extends` chain (`C.method`, `Sub.parentStatic`).
            // A `static get name()` is invoked with `this` = the class value.
            HeapObj::Class(c) => {
                if let Some(v) = c.statics.get(key) {
                    return Ok(v);
                }
                if let Some((_, g)) = c.static_getters.iter().find(|(k, _)| k == key) {
                    let g = *g;
                    return self.call_value(g, obj, &[]);
                }
                let mut cur = c.parent;
                while let Some(pidx) = cur {
                    match self.heap.get(pidx) {
                        HeapObj::Class(pc) => {
                            if let Some(v) = pc.statics.get(key) {
                                return Ok(v);
                            }
                            if let Some((_, g)) = pc.static_getters.iter().find(|(k, _)| k == key) {
                                let g = *g;
                                return self.call_value(g, obj, &[]);
                            }
                            cur = pc.parent;
                        }
                        _ => break,
                    }
                }
                // A class is a function: keys not found as a static fall back to
                // Function.prototype (so `C.toString()` → the class source via
                // FN_TO_STRING, and `C.call`/`apply`/`bind` resolve).
                Ok(self.proto_member(self.fn_proto, key))
            }
            // `map.size` / `set.size` — an accessor property, not a method.
            HeapObj::Map { keys, .. } if key == "size" => Ok(len_value(keys.len())),
            HeapObj::Set(items) if key == "size" => Ok(len_value(items.len())),
            // A method as a VALUE on a Map/Set/Date/Promise instance
            // (`new Map().set`, `d.getHours`) → the corresponding prototype.
            HeapObj::Map { .. } => self.exotic_own_or_proto(obj, self.map_proto, key),
            HeapObj::Set(_) => self.exotic_own_or_proto(obj, self.set_proto, key),
            HeapObj::WeakMap { .. } => self.exotic_own_or_proto(obj, self.weakmap_proto, key),
            HeapObj::WeakSet(_) => self.exotic_own_or_proto(obj, self.weakset_proto, key),
            HeapObj::WeakRef(_) => self.exotic_own_or_proto(obj, self.weakref_proto, key),
            HeapObj::FinalizationRegistry { .. } => self.exotic_own_or_proto(obj, self.finreg_proto, key),
            HeapObj::Iterator { proto, .. } => {
                let p = *proto;
                self.proto_chain_get(p, key, obj)
            }
            HeapObj::IterHelper { .. } => {
                let p = self.iterator_helper_proto;
                self.proto_chain_get(p, key, obj)
            }
            // A generator instance delegates to %GeneratorPrototype% (next/return/
            // throw + @@iterator) which chains to %Iterator.prototype% (the helper
            // methods). So `g().next`, `g().map`, `g()[Symbol.iterator]` all resolve.
            HeapObj::Generator { .. } => {
                let p = self.gen_proto;
                self.proto_chain_get(p, key, obj)
            }
            HeapObj::AsyncGenerator(_) => {
                let p = self.asyncgen_proto;
                self.proto_chain_get(p, key, obj)
            }
            // A boxed primitive: `length` (String box) reads the wrapped string;
            // everything else resolves through the wrapped type's prototype.
            HeapObj::Boxed { kind, value } => {
                let (k, v) = (*kind, *value);
                if k == 0 && key == "length" {
                    return self.get_prop(v, "length");
                }
                let proto = match k {
                    0 => self.str_proto,
                    1 => self.num_proto,
                    2 => self.bool_proto,
                    3 => self.symbol_proto,
                    _ => self.bigint_proto,
                };
                // An assigned/defined own property (`new Object(42).charAt = …`)
                // wins over the wrapped type's prototype.
                self.exotic_own_or_proto(obj, proto, key)
            }
            HeapObj::Date(_) => self.exotic_own_or_proto(obj, self.date_proto, key),
            HeapObj::Promise { .. } => self.exotic_own_or_proto(obj, self.promise_proto, key),
            // A Symbol: `.description` reads the wrapped description; methods
            // (toString/valueOf/constructor) resolve through Symbol.prototype.
            HeapObj::Symbol { desc, .. } => {
                if key == "description" {
                    return Ok(*desc);
                }
                Ok(self.proto_member(self.symbol_proto, key))
            }
            // A BigInt: methods (toString/valueOf/constructor) via BigInt.prototype.
            HeapObj::BigInt(_) => Ok(self.proto_member(self.bigint_proto, key)),
            // Functions / natives / bound functions: own props set on them
            // (`assert.sameValue`), then Function.prototype (`call`/`apply`/`bind`).
            _ if matches!(
                self.heap.get(obj.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
            ) =>
            {
                if let Some(m) = self.fn_props.get(&obj.heap_index()) {
                    if let Some(v) = m.get(key) {
                        return Ok(v);
                    }
                }
                // Poison-pill: `caller`/`arguments` on a STRICT or BOUND function are
                // %ThrowTypeError% accessors (AddRestrictedFunctionProperties); a
                // sloppy function inherits neither and reads undefined (no throw).
                if key == "caller" || key == "arguments" {
                    let poison = match self.heap.get(obj.heap_index()) {
                        HeapObj::Bound { .. } => true,
                        HeapObj::Func(fid) => self.func(*fid as usize).is_strict,
                        HeapObj::Closure { func, .. } => self.func(*func as usize).is_strict,
                        _ => false,
                    };
                    if poison {
                        return Err(Thrown(format!(
                            "TypeError: '{key}' may not be accessed on strict-mode or bound functions"
                        )));
                    }
                }
                // Inherited methods: a generator/async function starts at its
                // dynamic-function intrinsic prototype (so `gen.constructor` is
                // %GeneratorFunction%), else %Function.prototype% (call/apply/bind),
                // then up to Object.prototype (toString/valueOf/hasOwnProperty/…).
                let start = self
                    .callable_dynfn_proto(obj.heap_index())
                    .unwrap_or(self.fn_proto);
                Ok(self.proto_member(start, key))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

}
