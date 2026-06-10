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
    /// For the Proxy get/set trap invariants: the TARGET's own property descriptor
    /// reduced to `(is_data, value, writable, has_getter, has_setter)`. Returns
    /// `None` when the target has no own property OR the property is configurable
    /// (in which case no invariant applies and the trap result stands).
    pub(crate) fn proxy_target_desc(
        &mut self,
        target: Value,
        key: &str,
    ) -> Result<Option<(bool, Value, bool, bool, bool)>, Thrown> {
        let desc = self.object_get_own_property_descriptor(target, key);
        if desc == Value::UNDEFINED {
            return Ok(None);
        }
        let cfg = self.get_prop(desc, "configurable")?;
        if self.truthy(cfg) {
            return Ok(None);
        }
        // A data descriptor carries an own "value" key; an accessor carries get/set.
        let is_data =
            matches!(self.heap.get(desc.heap_index()), HeapObj::Object(m) if m.pos("value").is_some());
        if is_data {
            let wv = self.get_prop(desc, "writable")?;
            let writable = self.truthy(wv);
            let value = self.get_prop(desc, "value")?;
            Ok(Some((true, value, writable, false, false)))
        } else {
            let g = self.get_prop(desc, "get")?;
            let s = self.get_prop(desc, "set")?;
            Ok(Some((false, Value::UNDEFINED, false, g != Value::UNDEFINED, s != Value::UNDEFINED)))
        }
    }

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
                if !self.truthy(r) {
                    return Ok(Some(false));
                }
                // Invariant: if the target is non-extensible, the proxy's prototype
                // must match the target's actual prototype.
                if !self.is_extensible(target)? {
                    let target_proto = self.object_get_prototype_of(target);
                    if !self.same_value(proto, target_proto) {
                        return Err(Thrown(
                            "TypeError: 'setPrototypeOf' on proxy: trap returned truish for setting a new prototype on the non-extensible proxy target".into(),
                        ));
                    }
                }
                Ok(Some(true))
            }
            None => {
                // No setPrototypeOf trap: forward to the target's [[SetPrototypeOf]]
                // — OrdinarySetPrototypeOf re-enters the proxy path for a Proxy target
                // (firing its own trap) and applies the cyclic-chain / non-extensible
                // checks for an ordinary target.
                Ok(Some(self.ordinary_set_prototype_of(target, proto)?))
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
                let result = self.truthy(r);
                // Invariant: the trap result must equal IsExtensible(target).
                if result != self.is_extensible(target)? {
                    return Err(Thrown(
                        "TypeError: 'isExtensible' on proxy: trap result does not reflect extensibility of proxy target".into(),
                    ));
                }
                Ok(Some(result))
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
                let result = self.truthy(r);
                // Invariant: a true result requires the target to be non-extensible.
                if result && self.is_extensible(target)? {
                    return Err(Thrown(
                        "TypeError: 'preventExtensions' on proxy: trap returned truish but the proxy target is extensible".into(),
                    ));
                }
                Ok(Some(result))
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
                // Target-key invariants (10.5.11 steps 10-22). Partition the target's
                // own keys into configurable / non-configurable, keyed by the same
                // identity scheme as `seen` (string content / symbol heap index).
                let extensible = self.is_extensible(target)?;
                let tkeys_v = self.object_own_keys(target)?;
                let tkeys = self.array_snapshot(tkeys_v.heap_index());
                let (mut config, mut nonconfig): (Vec<String>, Vec<String>) =
                    (Vec::new(), Vec::new());
                for tk in &tkeys {
                    let id = if tk.is_heap()
                        && matches!(self.heap.get(tk.heap_index()), HeapObj::Symbol { .. })
                    {
                        format!("y:{}", tk.heap_index())
                    } else {
                        format!("s:{}", self.display(*tk))
                    };
                    let ks = self.key_of(*tk);
                    let desc = self.object_get_own_property_descriptor(target, &ks);
                    let cfg = if desc.is_undefined() {
                        true
                    } else {
                        let c = self.get_prop(desc, "configurable")?;
                        self.truthy(c)
                    };
                    if cfg {
                        config.push(id);
                    } else {
                        nonconfig.push(id);
                    }
                }
                // Fast path: an extensible target with no non-configurable keys
                // imposes no further constraint on the trap result.
                if !(extensible && nonconfig.is_empty()) {
                    let mut unchecked = seen.clone();
                    // Every non-configurable own key MUST appear in the trap result.
                    for key in &nonconfig {
                        match unchecked.iter().position(|u| u == key) {
                            Some(p) => {
                                unchecked.remove(p);
                            }
                            None => {
                                return Err(Thrown(
                                    "TypeError: proxy [[OwnPropertyKeys]] must include all non-configurable keys of the target".into(),
                                ))
                            }
                        }
                    }
                    // A non-extensible target: the trap result must contain EXACTLY
                    // the target's own keys (every configurable key present, none extra).
                    if !extensible {
                        for key in &config {
                            match unchecked.iter().position(|u| u == key) {
                                Some(p) => {
                                    unchecked.remove(p);
                                }
                                None => {
                                    return Err(Thrown(
                                        "TypeError: proxy [[OwnPropertyKeys]] of a non-extensible target must contain all of its own keys".into(),
                                    ))
                                }
                            }
                        }
                        if !unchecked.is_empty() {
                            return Err(Thrown(
                                "TypeError: proxy [[OwnPropertyKeys]] of a non-extensible target must not contain extra keys".into(),
                            ));
                        }
                    }
                }
                Ok(Some(items))
            }
            None => {
                // No trap: forward the target's full own-key list (Strings AND
                // Symbols, so getOwnPropertySymbols/Reflect.ownKeys see symbol keys).
                let keys = self.object_own_keys(target)?;
                Ok(Some(self.array_snapshot(keys.heap_index())))
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
                let overridden = self.array_index_override(idx, i);
                // A hole (an absent element) with no defineProperty'd override is not
                // an own property — skip it.
                if overridden.is_none()
                    && matches!(self.heap.get(idx), HeapObj::Array(items) if items[i].is_hole())
                {
                    continue;
                }
                if overridden.map_or(true, |(a, _)| a.enumerable) {
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
        // A String exotic (boxed `new String(s)` or a string primitive): its own
        // ENUMERABLE keys are the character indices `0..length` (the exotic chars;
        // `length` is non-enumerable so it is excluded), then any enumerable assigned
        // own prop on the wrapper. Handled before the generic match because reading a
        // character needs `&mut self` (get_index).
        if let Some((sval, len)) = self.string_exotic_chars(obj) {
            let mut out: Vec<Value> = Vec::with_capacity(len);
            for i in 0..len {
                let kv = self.alloc_str(i.to_string());
                match what {
                    EnumWhat::Keys => out.push(kv),
                    EnumWhat::Values => {
                        let ch = self.get_index(sval, Value::num(i as f64))?;
                        out.push(ch);
                    }
                    EnumWhat::Entries => {
                        let ch = self.get_index(sval, Value::num(i as f64))?;
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, ch]))));
                    }
                }
            }
            // Enumerable named own props assigned to the wrapper (`s.foo = …`).
            let extra: Vec<String> = match self.arr_props.get(&obj.heap_index()) {
                Some(m) => m
                    .keys
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| m.attrs[*i].enumerable && !is_hidden_key(k))
                    .map(|(_, k)| k.clone())
                    .collect(),
                None => Vec::new(),
            };
            for k in extra {
                let v = self.get_member(obj, &k, obj)?;
                let kv = self.alloc_str(k);
                match what {
                    EnumWhat::Keys => out.push(kv),
                    EnumWhat::Values => out.push(v),
                    EnumWhat::Entries => {
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))))
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // A plain Object: EnumerableOwnPropertyNames — snapshot the ordered own string
        // keys ([[OwnPropertyKeys]]) ONCE, then per key re-read the LIVE descriptor (so
        // a key a prior getter deleted or made non-enumerable is skipped) and, for
        // values/entries, read the value via Get (an accessor's getter runs and its
        // mutations are observed; a thrown getter propagates). Object.keys reads no
        // value, so it never triggers a getter.
        if obj.is_heap() {
            if let HeapObj::Object(m) = self.heap.get(obj.heap_index()) {
                let names: Vec<String> = spec_key_order(&m.keys)
                    .into_iter()
                    .map(|i| m.keys[i].clone())
                    .filter(|k| !is_hidden_key(k))
                    .collect();
                let mut out: Vec<Value> = Vec::with_capacity(names.len());
                for k in names {
                    let enumerable = match self.heap.get(obj.heap_index()) {
                        HeapObj::Object(m) => m.pos(&k).map_or(false, |i| m.attrs[i].enumerable),
                        _ => false,
                    };
                    if !enumerable {
                        continue;
                    }
                    match what {
                        EnumWhat::Keys => {
                            let kv = self.alloc_str(k);
                            out.push(kv);
                        }
                        EnumWhat::Values => {
                            let v = self.get_member(obj, &k, obj)?;
                            out.push(v);
                        }
                        EnumWhat::Entries => {
                            let v = self.get_member(obj, &k, obj)?;
                            let kv = self.alloc_str(k);
                            out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                        }
                    }
                }
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
            }
        }
        // A callable (function / closure / bound / native): enumerate its own keys in
        // canonical [[OwnPropertyKeys]] order — length/name/prototype keep their
        // chronological-first slot even after a defineProperty override (see
        // object_own_property_names) — keeping only the enumerable ones. An intrinsic
        // length/name/prototype is non-enumerable unless a fn_props override made it
        // enumerable, so only fn_props-backed keys can appear; their order follows the
        // canonical key order, NOT raw fn_props insertion order.
        if obj.is_heap()
            && matches!(
                self.heap.get(obj.heap_index()),
                HeapObj::Func(_)
                    | HeapObj::Closure { .. }
                    | HeapObj::Bound { .. }
                    | HeapObj::Native(_)
            )
        {
            let idx = obj.heap_index();
            let names_v = self.object_own_property_names(obj)?;
            let names: Vec<String> = self
                .array_snapshot(names_v.heap_index())
                .into_iter()
                .filter_map(|k| {
                    (k.is_heap() && self.heap.is_str_like(k.heap_index())).then(|| self.display(k))
                })
                .collect();
            let mut out: Vec<Value> = Vec::new();
            for k in names {
                let enumerable = self
                    .fn_props
                    .get(&idx)
                    .and_then(|m| m.pos(&k).map(|i| m.attrs[i].enumerable))
                    .unwrap_or(false);
                if !enumerable {
                    continue;
                }
                match what {
                    EnumWhat::Keys => {
                        let kv = self.alloc_str(k);
                        out.push(kv);
                    }
                    EnumWhat::Values => {
                        let v = self.get_member(obj, &k, obj)?;
                        out.push(v);
                    }
                    EnumWhat::Entries => {
                        let v = self.get_member(obj, &k, obj)?;
                        let kv = self.alloc_str(k);
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        let pairs: Vec<(String, Value)> = if obj.is_heap() {
            match self.heap.get(obj.heap_index()) {
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
                // A function's assigned own properties live in the `fn_props` side
                // table (e.g. `fn.x = 1`); enumerate the enumerable ones (for
                // Object.keys/values/entries + for-in), like the getOwnPropertyNames
                // path already reads them.
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Bound { .. }
                | HeapObj::Native(_) => match self.fn_props.get(&obj.heap_index()) {
                    Some(m) => spec_key_order(&m.keys)
                        .into_iter()
                        .filter(|&i| m.attrs[i].enumerable && !is_hidden_key(&m.keys[i]))
                        .map(|i| (m.keys[i].clone(), m.vals[i]))
                        .collect(),
                    None => Vec::new(),
                },
                // A class's own (static) properties live in `ClassData.statics`
                // (static methods are non-enumerable; static fields / `Cls.s = …`
                // assignments are enumerable).
                HeapObj::Class(c) => spec_key_order(&c.statics.keys)
                    .into_iter()
                    .filter(|&i| c.statics.attrs[i].enumerable && !is_hidden_key(&c.statics.keys[i]))
                    .map(|i| (c.statics.keys[i].clone(), c.statics.vals[i]))
                    .collect(),
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

    /// EnumerateObjectProperties (for-in): the own + INHERITED enumerable string
    /// keys, walking the [[Prototype]] chain, with shadowing dedup — a key seen at a
    /// nearer level (enumerable OR not) hides the same key on farther prototypes.
    /// Symbol keys are excluded; built-in prototype methods are non-enumerable so
    /// they never appear. Returns a HeapObj::Array of string keys. (Object.keys/
    /// values/entries stay own-only via object_enum_own — only for-in walks up.)
    pub(crate) fn for_in_keys(&mut self, obj: Value) -> Result<Value, Thrown> {
        // `out` holds heap key strings while object_enum_own / object_own_property_names
        // re-enter and allocate — suspend GC for the scope.
        let _gc = self.gc_lock_guard();
        let mut out: Vec<Value> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut cur = obj;
        for _ in 0..100_000 {
            if !cur.is_heap() {
                break;
            }
            // Emit this level's enumerable string keys not already shadowed
            // (object_enum_own is Proxy-aware: ownKeys trap + per-key gopd check).
            let enum_keys = self.object_enum_own(cur, EnumWhat::Keys)?;
            let enum_snap = self.array_snapshot(enum_keys.heap_index());
            for k in &enum_snap {
                let ks = self.display(*k);
                if seen.insert(ks) {
                    out.push(*k);
                }
            }
            // Record EVERY own string key at this level (incl. non-enumerable) so it
            // shadows the same key on farther prototypes.
            let all_names = self.object_own_property_names(cur)?;
            let all_snap = self.array_snapshot(all_names.heap_index());
            for k in &all_snap {
                seen.insert(self.display(*k));
            }
            cur = self.object_get_prototype_of(cur);
            if cur == Value::NULL {
                break;
            }
        }
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
            // No trap: forward to the target's [[GetOwnProperty]] — which, when the
            // target is itself a Proxy, must recurse through ITS trap/target rather
            // than falling to the ordinary path (which ignores proxies).
            None => match self.proxy_gopd(target, key)? {
                Some(d) => Ok(Some(d)),
                None => Ok(Some(self.object_get_own_property_descriptor(target, key))),
            },
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
    /// If `obj` is a primitive String value or a boxed String (`new String(s)`),
    /// return `(wrapped_string_value, char_len)` — the source of a String exotic's
    /// own integer-index character properties and its non-writable `length`. The
    /// reflective `Object.*` methods use this so a string (boxed by ToObject) reports
    /// those exotic own props. `None` for any non-string value.
    pub(crate) fn string_exotic_chars(&self, obj: Value) -> Option<(Value, usize)> {
        if !obj.is_heap() {
            return None;
        }
        let idx = obj.heap_index();
        if let HeapObj::Boxed { kind: 0, value } = self.heap.get(idx) {
            let v = *value;
            return self.heap.str_char_len(v.heap_index()).map(|n| (v, n));
        }
        self.heap.str_char_len(idx).map(|n| (obj, n))
    }

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
        // data property (a `defineProperty` may have cleared its writable flag).
        if key == "lastIndex" {
            if let HeapObj::RegExp { last_index, .. } = self.heap.get(idx) {
                let v = *last_index;
                let writable = self
                    .arr_props
                    .get(&idx)
                    .map_or(true, |m| m.pos("lastIndex").map_or(true, |i| m.attrs[i].writable));
                return self.make_data_descriptor(v, writable, false, false);
            }
        }
        let own = match self.heap.get(idx) {
            HeapObj::Object(m) => {
                if let Some(i) = m.pos(key) {
                    Some((m.attrs[i], m.vals[i]))
                } else if idx == self.global_this && self.global_this != 0 {
                    // globalThis own properties: built-in globals are
                    // { writable, enumerable:false, configurable }; the value
                    // globals NaN/Infinity/undefined are { false, false, false }.
                    if let Some(v) = self.global_by_name(key) {
                        return match key {
                            "NaN" | "Infinity" | "undefined" => {
                                self.make_data_descriptor(v, false, false, false)
                            }
                            _ => self.make_data_descriptor(v, true, false, true),
                        };
                    }
                    None
                } else {
                    None
                }
            }
            HeapObj::Array(items) => {
                if key == "length" {
                    let len = len_value(items.len());
                    let writable = !self.array_length_nonwritable.contains(&idx);
                    return self.make_data_descriptor(len, writable, false, false);
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
                        // A hole has no own property descriptor (falls to undefined).
                        Ok(i) if i.to_string() == key && i < dense_len && !items[i].is_hole() => {
                            let v = items[i];
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
                        // A hole is an absent element — not a reflectable own key.
                        if !items[i].is_hole() {
                            keys.push(i.to_string());
                        }
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
                    // Spec order: the constructor's `length`, `name`, then
                    // `prototype` are created FIRST. A static element named one of
                    // them overwrites the VALUE but keeps that early position (it was
                    // already defined), so emit these three first and skip them when
                    // listing the remaining static elements.
                    // A static element named length/name (a method, a computed key,
                    // a generator, OR a get/set accessor) overrides the intrinsic but
                    // keeps its position — check all three stores.
                    let static_has = |k: &str| {
                        c.statics.pos(k).is_some()
                            || c.static_getters.iter().any(|(n, _)| n == k)
                            || c.static_setters.iter().any(|(n, _)| n == k)
                    };
                    if has_length || static_has("length") {
                        keys.push("length".to_string());
                    }
                    if has_name || static_has("name") {
                        keys.push("name".to_string());
                    }
                    if self.callable_has_prototype(obj) || c.statics.pos("prototype").is_some() {
                        keys.push("prototype".to_string());
                    }
                    let is_intrinsic_key =
                        |k: &str| matches!(k, "length" | "name" | "prototype");
                    keys.extend(
                        c.statics
                            .keys
                            .iter()
                            .filter(|k| !is_hidden_key(k) && !is_intrinsic_key(k))
                            .cloned(),
                    );
                    for (n, _) in &c.static_getters {
                        if !is_hidden_key(n) && !is_intrinsic_key(n) && !keys.iter().any(|k| k == n)
                        {
                            keys.push(n.clone());
                        }
                    }
                    for (n, _) in &c.static_setters {
                        if !is_hidden_key(n) && !is_intrinsic_key(n) && !keys.iter().any(|k| k == n)
                        {
                            keys.push(n.clone());
                        }
                    }
                }
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Bound { .. }
                | HeapObj::BoundResolver { .. }
                | HeapObj::CombinatorResolver { .. }
                | HeapObj::Native(_) => {
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
                // A String exotic (boxed `new String(s)` or a raw string): the
                // character indices `0..length` first, then `length`, then any named
                // own prop assigned to the wrapper.
                HeapObj::Boxed { kind: 0, .. } | HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    if let Some((_, len)) = self.string_exotic_chars(obj) {
                        for i in 0..len {
                            keys.push(i.to_string());
                        }
                        keys.push("length".to_string());
                    }
                    if let Some(m) = self.arr_props.get(&idx) {
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
                // A boxed Number/Boolean/Symbol/BigInt wrapper (kind != 0; kind 0 =
                // String is handled above) has no exotic own properties — only the
                // ones assigned to the wrapper, in the arr_props side table. (e.g.
                // `Object.assign(12, "ab")` boxes the target and copies "0"/"1".)
                HeapObj::Boxed { .. } => {
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
                _ => {}
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
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => self
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

    /// `__lookupGetter__`/`__lookupSetter__`'s chain walk using the SPEC abstract
    /// operations: each step is `[[GetOwnProperty]](key)` then `[[GetPrototypeOf]]()`,
    /// both Proxy-trap-aware and throwing — so a trap that raises an abrupt completion
    /// propagates (unlike `lookup_accessor`, which returns Value and skips Proxy nodes).
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
        let extensible = match self.heap.get(o.heap_index()) {
            HeapObj::Object(m) => m.extensible,
            _ => true,
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
                let idesc = Value::heap(self.heap.alloc(HeapObj::Object(m)));
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
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::TRUE);
        m.set("enumerable", Value::TRUE);
        m.set("configurable", Value::TRUE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(m)));
        self.object_define_property(target, key, desc)
    }

    /// `Object.defineProperty(obj, key, descriptor)` — define/redefine an own
    /// property with explicit attributes (unspecified attrs default to false on a
    /// new property; an existing non-configurable property rejects most changes).
    pub(crate) fn object_define_property(&mut self, obj: Value, key: &str, desc: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
        }
        self.note_array_proto_index(obj.heap_index(), key);
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
                // A canonical ARRAY index is 0..2^32-2; `"4294967295"` (2^32-1) and
                // beyond are ORDINARY named properties → fall through to the generic
                // arr_props path (not a dense slot, no dense-limit error).
                if i.to_string() == key && i < 0xFFFF_FFFF {
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
                    // Array [[DefineOwnProperty]] for an array index P=n: if n >= the
                    // current length and `length` is non-writable (defineProperty /
                    // freeze), the define fails — it would grow `length` (ArraySetLength
                    // forbidden). `n >= dense_len` already implies the index is absent
                    // (a special override keeps a dense placeholder). A TypeError from
                    // DefinePropertyOrThrow; the array is left unchanged.
                    if i >= dense_len && self.array_length_nonwritable.contains(&idx) {
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
                let cur_len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
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
                    if new_len > crate::vm::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: array length exceeds the engine's dense-array limit".into(),
                        ));
                    }
                    let cur_len = match self.heap.get(idx) {
                        HeapObj::Array(items) => items.len(),
                        _ => 0,
                    };
                    // Shrinking past a NON-configurable index does a PARTIAL shrink:
                    // delete the deletable indices above it, stop the length at
                    // blocker+1, set the length non-writable if requested, THEN throw
                    // (ArraySetLength steps 16-17). `effective_len` is where the length
                    // actually lands; `blocked` records that a TypeError is due.
                    let mut effective_len = new_len;
                    let mut blocked = false;
                    if new_len < cur_len {
                        // Walk DOWN from the top; the highest non-configurable
                        // (non-deletable) index in [new_len, cur_len) is the blocker.
                        let mut i = cur_len;
                        while i > new_len {
                            i -= 1;
                            if self
                                .array_index_override(idx, i)
                                .map_or(false, |(a, _)| !a.configurable)
                            {
                                effective_len = i + 1;
                                blocked = true;
                                break;
                            }
                        }
                        // Drop any (configurable) special overrides being truncated.
                        if let Some(m) = self.arr_props.get_mut(&idx) {
                            for i in effective_len..cur_len {
                                m.remove(&i.to_string());
                            }
                        }
                    }
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        items.resize(effective_len, Value::UNDEFINED);
                    }
                    self.heap.bump_version(idx);
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
                _ => true,
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
                self.arr_props.entry(idx).or_insert_with(ObjMap::new).define(key, stored, attr);
            }
            _ => {
                self.fn_props.entry(idx).or_insert_with(ObjMap::new).define(key, stored, attr);
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
            accessor: result_accessor,
            setter: if result_accessor {
                set.or(existing_set).unwrap_or(Value::UNDEFINED)
            } else {
                Value::UNDEFINED
            },
        };
        let stored = if result_accessor {
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
            // A boxed String (e.g. `Object.create(O, "abc")` → ToObject) exposes its
            // char indices as own enumerable keys; reading each yields a char string,
            // which then fails ToPropertyDescriptor (a non-object) → TypeError.
            HeapObj::Boxed { kind: 0, value } => {
                let v = *value;
                let n = self.heap.str_cow(v.heap_index()).map(|s| s.chars().count()).unwrap_or(0);
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
    pub(crate) fn global_by_name(&self, name: &str) -> Option<Value> {
        // A built-in global removed via `delete globalThis.X` reads as absent
        // everywhere (get / has-own / descriptor all consult this).
        if self.deleted_globals.contains(name) {
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
    pub(crate) fn note_array_proto_index(&mut self, obj_idx: u32, key: &str) {
        if (obj_idx == self.arr_proto || obj_idx == self.obj_proto)
            && canonical_index_str(key).is_some()
        {
            self.array_proto_has_index = true;
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

    fn apply_proto_set(
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

    /// Like `proto_member`, but ACCESSOR-AWARE: when the property found on the
    /// chain is a getter (e.g. a user `Object.defineProperty(TA.prototype,
    /// "constructor", {get})`), it is invoked with `receiver`. Used by the
    /// TypedArray/DataView instance get path so a user-installed accessor on a
    /// type prototype is honoured (SpeciesConstructor reads `this.constructor`).
    /// Mirrors `proto_member`'s walk, including the Object.prototype fallback.
    pub(crate) fn proto_member_get(
        &mut self,
        proto: u32,
        key: &str,
        receiver: Value,
    ) -> Result<Value, Thrown> {
        let mut cur = proto;
        let mut guard = 0u32;
        while cur != 0 && guard < 64 {
            guard += 1;
            if let Some((attr, raw)) = self.own_member(cur, key) {
                return if attr.accessor {
                    if raw == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { self.call_value(raw, receiver, &[]) }
                } else {
                    Ok(raw)
                };
            }
            match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => cur = p.heap_index(),
                _ => break,
            }
        }
        if self.obj_proto != 0 && proto != self.obj_proto {
            if let Some((attr, raw)) = self.own_member(self.obj_proto, key) {
                return if attr.accessor {
                    if raw == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { self.call_value(raw, receiver, &[]) }
                } else {
                    Ok(raw)
                };
            }
        }
        Ok(Value::UNDEFINED)
    }

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

    pub(crate) fn callable_name_length(&self, obj: Value) -> Option<(String, f64)> {
        let clean = |n: &str| -> String {
            if n.starts_with('<') { String::new() } else { n.to_string() }
        };
        match self.heap.get(obj.heap_index()) {
            HeapObj::Func(fid) => {
                let p = self.func(*fid as usize);
                Some((clean(&p.name), p.length as f64))
            }
            HeapObj::Closure { func, .. } => {
                let p = self.func(*func as usize);
                Some((clean(&p.name), p.length as f64))
            }
            // The resolve/reject functions of `new Promise(executor)`, and the
            // Promise.all/allSettled/any resolve/reject ELEMENT functions: anonymous
            // (name ""), length 1, with %Function.prototype% as [[Prototype]].
            HeapObj::BoundResolver { .. } | HeapObj::CombinatorResolver { .. } => {
                Some((String::new(), 1.0))
            }
            HeapObj::Class(c) => {
                let len = c
                    .ctor
                    .map(|f| self.func(f as usize).length as f64)
                    .unwrap_or(0.0);
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
                    return Some((format!("get {name}"), 0.0));
                }
                if (native::SAB_GETTER_BASE
                    ..native::SAB_GETTER_BASE + native::SAB_GETTERS.len() as u16)
                    .contains(&id)
                {
                    let name = native::SAB_GETTERS[(id - native::SAB_GETTER_BASE) as usize];
                    return Some((format!("get {name}"), 0.0));
                }
                native::proto_method(id)
                    .map(|(n, _, l)| (n.to_string(), l as f64))
                    .or_else(|| native::math_method(id).map(|(n, _, l)| (n.to_string(), l as f64)))
                    .or_else(|| native::static_name_length(id).map(|(n, l)| (n.to_string(), l as f64)))
            }
            HeapObj::Bound { target, args, .. } if target.is_heap() => {
                // The anonymous functions returned by the Intl format/compare
                // getters have name "" and length 1 (format) / 2 (compare).
                if let HeapObj::Native(tid) = self.heap.get(target.heap_index()) {
                    match *tid {
                        native::INTL_NF_FORMAT | native::INTL_DTF_FORMAT => {
                            return Some((String::new(), 1.0));
                        }
                        native::INTL_COLLATOR_COMPARE => return Some((String::new(), 2.0)),
                        _ => {}
                    }
                }
                // A bound function F: name is "bound " + target.name, and length is
                // max(0, target.length - boundArgsCount) when the target has a
                // numeric length (BoundFunctionCreate / SetFunctionLength+Name). Read
                // the target's EFFECTIVE name/length so a `defineProperty`-redefined
                // value flows through to the bound function.
                let nbound = args.len() as f64;
                let (tname, tlen) =
                    self.effective_name_length(*target).unwrap_or((String::new(), 0.0));
                // L = max(0, ToIntegerOrInfinity(target.length) − boundArgs): a +Inf
                // target length stays +Inf; the f64 channel avoids the i32 overflow
                // a 2^31 length would hit.
                Some((format!("bound {tname}"), (tlen - nbound).max(0.0)))
            }
            _ => None,
        }
    }

    /// A callable's EFFECTIVE name/length: its synthesized intrinsic overlaid with
    /// any value redefined via `Object.defineProperty(fn, "name"|"length", …)`
    /// (stored in fn_props for a function, statics for a class). Used by `bind` so a
    /// bound function's name/length derive from the target's CURRENT values, not the
    /// frozen intrinsic. A non-string redefined `name` yields "" (per SetFunctionName,
    /// which only adopts a String target name); a non-finite/non-numeric `length`
    /// yields 0.
    pub(crate) fn effective_name_length(&self, obj: Value) -> Option<(String, f64)> {
        let (mut name, mut len) = self.callable_name_length(obj)?;
        if obj.is_heap() {
            let idx = obj.heap_index();
            let name_ovr = match self.heap.get(idx) {
                HeapObj::Class(c) => c.statics.pos("name").map(|i| c.statics.vals[i]),
                _ => self.fn_props.get(&idx).and_then(|m| m.pos("name").map(|i| m.vals[i])),
            };
            let len_ovr = match self.heap.get(idx) {
                HeapObj::Class(c) => c.statics.pos("length").map(|i| c.statics.vals[i]),
                _ => self.fn_props.get(&idx).and_then(|m| m.pos("length").map(|i| m.vals[i])),
            };
            if let Some(v) = name_ovr {
                name = if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
                    self.display(v)
                } else {
                    String::new()
                };
            }
            if let Some(v) = len_ovr {
                // ToIntegerOrInfinity(value) for the bound-length computation:
                // a NaN/non-number → 0, +Inf stays +Inf, else truncate toward zero.
                len = if v.is_int() {
                    v.as_int() as f64
                } else if v.is_double() {
                    let d = v.as_f64();
                    if d.is_nan() { 0.0 } else { d.trunc() }
                } else {
                    0.0
                };
            }
        }
        Some((name, len))
    }

    /// SetFunctionName for an object-literal accessor / computed member whose name
    /// is only known at runtime: `name = prefix + (Symbol key → "[description]" or
    /// "", else ToString(key))`, with prefix 0=none / 1="get " / 2="set ". Written
    /// as a non-writable, non-enumerable, configurable own `name` (overriding the
    /// synthesized intrinsic via callable_has_intrinsic); a Class sets its name
    /// field. Only a function/class value is named.
    pub(crate) fn set_fn_name_from_key(&mut self, func: Value, key: Value, prefix: u8) {
        if !func.is_heap() {
            return;
        }
        let fi = func.heap_index();
        let is_class = matches!(self.heap.get(fi), HeapObj::Class(_));
        let is_callable = matches!(
            self.heap.get(fi),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
        );
        if !is_class && !is_callable {
            return;
        }
        let base_name = if key.is_heap() {
            match self.heap.get(key.heap_index()) {
                HeapObj::Symbol { desc, .. } => {
                    let d = *desc;
                    if d == Value::UNDEFINED {
                        String::new()
                    } else {
                        format!("[{}]", self.display(d))
                    }
                }
                _ => self.display(key),
            }
        } else {
            self.display(key)
        };
        let name = match prefix {
            1 => format!("get {base_name}"),
            2 => format!("set {base_name}"),
            _ => base_name,
        };
        if is_class {
            if let HeapObj::Class(c) = self.heap.get_mut(fi) {
                c.name = name;
            }
            return;
        }
        let nv = self.alloc_str(name);
        self.fn_props.entry(fi).or_insert_with(ObjMap::new).define(
            "name",
            nv,
            PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        // The explicit `name` IS the function's name — suppress the synthesized
        // intrinsic so it doesn't reappear if the explicit one is deleted (the
        // property is a single configurable own `name`, per SetFunctionName).
        self.deleted_callable_intrinsics.insert((fi, 0));
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

    /// Whether a callable owns a `prototype` data property. Per spec only some
    /// callables do: a `class`, an ordinary `function` declaration/expression,
    /// and any generator (sync OR async) function. Arrow functions, concise
    /// methods/accessors, and plain `async function`s have NO `prototype`. Bound
    /// and built-in/native functions also have none here. Mirrors the cases for
    /// which `prototype_of` would synthesize one, so the `prototype` own-property
    /// reporting (descriptor / hasOwnProperty / `in` / ownKeys) stays consistent.
    pub(crate) fn callable_has_prototype(&self, obj: Value) -> bool {
        if !obj.is_heap() {
            return false;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Class(_) => true,
            HeapObj::Func(id) => self.func_has_prototype(*id),
            HeapObj::Closure { func, .. } => self.func_has_prototype(*func),
            _ => false,
        }
    }

    fn func_has_prototype(&self, id: u32) -> bool {
        let f = self.func(id as usize);
        // Generator and async-generator functions/methods always own `prototype`.
        if f.is_generator {
            return true;
        }
        // Arrows (lexical_this), concise methods/accessors (non_constructable),
        // and plain async functions do not.
        !(f.lexical_this || f.non_constructable || f.is_async)
    }

    /// The current value of a callable's `name`/`length` own property (allocating
    /// the name string), or None if absent/deleted.
    pub(crate) fn callable_intrinsic_value(&mut self, obj: Value, key: &str) -> Option<Value> {
        if !self.callable_has_intrinsic(obj, key) {
            return None;
        }
        let (nm, len) = self.callable_name_length(obj)?;
        Some(if key == "name" {
            self.alloc_str(nm)
        } else if len.is_finite() && len >= 0.0 && len <= i32::MAX as f64 && len.fract() == 0.0 {
            // Keep the common case an integer Value (no representation change);
            // only +Infinity / a length past i32 range needs a double.
            Value::int(len as i32)
        } else {
            Value::num(len)
        })
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
        // A SUBCLASS instance (`class X extends Map/Set/Promise/Date/…`) is the builtin
        // variant re-branded with its own prototype recorded in `proto_of` (which chains
        // to the builtin's prototype), so resolve through it — subclass methods AND the
        // inherited builtin methods both resolve. A plain builtin instance has no
        // `proto_of` entry → the builtin's default prototype.
        let eff = self
            .proto_of
            .get(&obj.heap_index())
            .and_then(|p| p.is_heap().then(|| p.heap_index()))
            .unwrap_or(proto);
        // Accessor-aware so an inherited getter on the type's prototype (e.g. a
        // user-redefined `set`/`add`) is INVOKED with `obj` as the receiver, not
        // returned as the raw getter function.
        self.proto_member_get(eff, key, obj)
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
                        let r = self.call_value(trap, handler, &[target, kv, obj])?;
                        // Invariant: a non-configurable, non-writable target data
                        // property must be reported with its actual value; a
                        // non-configurable accessor with no getter must report
                        // undefined.
                        if let Some((is_data, value, writable, has_get, _)) =
                            self.proxy_target_desc(target, key)?
                        {
                            if is_data && !writable && !self.same_value(r, value) {
                                return Err(Thrown("TypeError: 'get' on proxy: property is a read-only and non-configurable data property on the proxy target but the proxy did not return its actual value".into()));
                            }
                            if !is_data && !has_get && r != Value::UNDEFINED {
                                return Err(Thrown("TypeError: 'get' on proxy: property is a non-configurable accessor property on the proxy target and does not have a getter function".into()));
                            }
                        }
                        Ok(r)
                    }
                    None => self.get_member(target, key, receiver),
                };
            }
            // Module Namespace exotic [[Get]]: an exported name reads its LIVE
            // per-module slot (so a re-assignment inside the module is observed),
            // not the snapshot in the ObjMap. Non-export keys (e.g. @@toStringTag)
            // fall through to the ordinary lookup below.
            if let Some(slot_map) = self.module_namespaces.get(&obj.heap_index()) {
                if let Some(&slot) = slot_map.get(key) {
                    return Ok(self
                        .globals
                        .get(slot as usize)
                        .copied()
                        .unwrap_or(Value::UNDEFINED));
                }
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
        // A String exotic object reports each in-range char index as an own data
        // property, so OrdinaryGet (this path) must read the wrapped char. Bracket
        // access (`s["0"]`) is handled in get_index; this covers the INDIRECT entry
        // points — a Proxy with no `get` trap forwarding to a boxed-String target,
        // or Reflect.get(boxedString, "0", receiver). Gated on a canonical numeric
        // key so non-index gets skip the boxed-String check.
        if let Some(i) = canonical_index_str(key) {
            if let Some((sval, len)) = self.string_exotic_chars(obj) {
                if i < len {
                    return self.get_index(sval, Value::int(i as i32));
                }
            }
        }
        // A function's / class's `.name` and `.length` — synthesized own data
        // properties (configurable, so a prior `delete` suppresses them).
        if key == "name" || key == "length" {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return Ok(v);
            }
        }
        // A function's / class's `.prototype` (a lazily-created, stable object). An
        // explicit `fn.prototype = value` (incl. a non-object) is returned verbatim.
        if key == "prototype" {
            if let Some(&v) = self.fn_proto_override.get(&obj.heap_index()) {
                return Ok(v);
            }
            if let Some(p) = self.prototype_of(obj) {
                return Ok(p);
            }
        }
        // A RegExp's accessor-like own properties (source/flags/lastIndex + the
        // flag booleans) and its match-result Array's `.index`/`.input`/`.groups`.
        // Cloned out of the heap borrow before any allocation.
        if let HeapObj::RegExp { source, flags, last_index, .. } = self.heap.get(obj.heap_index()) {
            let (s, f, li) = (source.clone(), flags.clone(), *last_index);
            // A custom own property (`re.exec = fn`, `re.x = …`, or an
            // Object.defineProperty'd `flags`/`source`/flag-boolean) in the side
            // table shadows the prototype AND the synthesized intrinsic accessor —
            // an own property is more specific than the `%RegExp.prototype%` getter.
            // `lastIndex` is the exception: it is a struct-backed own data property
            // (the single source of truth shared with `exec`), so it always resolves
            // through `regexp_get_prop`, never a side-table entry.
            if key != "lastIndex" {
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
            // `get RegExp.prototype.flags` (no own override): build the string by
            // reading each per-flag accessor off the RECEIVER in canonical order — so
            // a throwing `global`/`unicode`/… getter or a per-flag own override is
            // observed (e.g. by `@@match`/`@@replace`, which read Get(rx,"flags")),
            // rather than synthesizing from the internal flag string.
            if key == "flags" {
                let mut out = String::new();
                for (prop, ch) in [
                    ("hasIndices", 'd'),
                    ("global", 'g'),
                    ("ignoreCase", 'i'),
                    ("multiline", 'm'),
                    ("dotAll", 's'),
                    ("unicode", 'u'),
                    ("unicodeSets", 'v'),
                    ("sticky", 'y'),
                ] {
                    let v = self.get_prop(receiver, prop)?;
                    if self.truthy(v) {
                        out.push(ch);
                    }
                }
                return Ok(self.alloc_str(out));
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
            // A CANONICAL numeric string index reads the element. usize's FromStr
            // accepts a leading '+' ("+1" parses to 1), but "+1" is not a
            // CanonicalNumericIndexString — it must be an ordinary (possibly
            // inherited) property, so gate on the round-trip.
            if let Some(i) = canonical_index_str(key) {
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
                // A CanonicalNumericIndexString that is NOT a valid integer index
                // (non-integer "1.1", "-0", or out of range) is an absent own
                // property of an integer-indexed exotic: return undefined WITHOUT
                // consulting the prototype (10.4.5.4 [[Get]]).
                _ if self.is_canonical_numeric_index(key) => Value::UNDEFINED,
                // Accessor-aware so a user getter on the type prototype fires with
                // the TA instance as receiver (SpeciesConstructor's this.constructor).
                // Consult the instance's REAL [[Prototype]] (a custom proto from
                // Reflect.construct's newTarget / Object.setPrototypeOf), falling
                // back to the intrinsic %TypedArray.prototype% when unset.
                _ => {
                    let proto = self
                        .proto_of
                        .get(&obj.heap_index())
                        .copied()
                        .filter(|p| p.is_heap())
                        .map(|p| p.heap_index())
                        .unwrap_or(self.ta_protos[kind as usize]);
                    return self.proto_member_get(proto, key, obj);
                }
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
                    let default = if shared { self.sab_proto } else { self.arraybuffer_proto };
                    let proto = self
                        .proto_of
                        .get(&ai)
                        .copied()
                        .filter(|p| p.is_heap())
                        .map(|p| p.heap_index())
                        .unwrap_or(default);
                    // Accessor-aware (mirrors the TypedArray arm): an inherited
                    // getter like Object.prototype.__proto__ is INVOKED with the
                    // buffer as receiver, not returned as a raw function value.
                    return self.proto_member_get(proto, key, obj);
                }
            });
        }
        if let HeapObj::DataView { buffer, byte_offset, byte_length } = self.heap.get(obj.heap_index()) {
            let (buffer, byte_offset, byte_length) = (*buffer, *byte_offset, *byte_length);
            // IsViewOutOfBounds: byteLength / byteOffset throw a TypeError when the
            // viewed buffer is detached, or when a resizable buffer has shrunk so the
            // view no longer fits (fixed-length: offset+length > current size;
            // length-tracking: offset > current size). `buffer` stays readable.
            let detached =
                matches!(self.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. });
            let tracking = self.dv_tracking.contains(&obj.heap_index());
            let cur = self.array_buffer_len(buffer);
            let oob = detached
                || if tracking { byte_offset > cur } else { byte_offset + byte_length > cur };
            return Ok(match key {
                "byteLength" | "byteOffset" if oob => {
                    return Err(Thrown(format!(
                        "TypeError: get DataView.prototype.{key}: the viewed buffer is out of bounds"
                    )))
                }
                // A length-tracking view reports the buffer's current remaining size.
                "byteLength" if tracking => Value::num((cur - byte_offset) as f64),
                "byteLength" => Value::num(byte_length as f64),
                "byteOffset" => Value::num(byte_offset as f64),
                "buffer" => Value::heap(buffer),
                _ => {
                    let proto = self
                        .proto_of
                        .get(&obj.heap_index())
                        .copied()
                        .filter(|p| p.is_heap())
                        .map(|p| p.heap_index())
                        .unwrap_or(self.dataview_proto);
                    self.proto_member(proto, key)
                }
            });
        }
        // Temporal.Duration: field getters + sign/blank; methods via the prototype.
        if let HeapObj::Temporal { kind: 0, .. } = self.heap.get(obj.heap_index()) {
            let f = self.duration_fields(obj.heap_index()).unwrap_or([0.0; 10]);
            if let Some(i) = native::DURATION_FIELDS.iter().position(|n| *n == key) {
                return Ok(Value::num(f[i]));
            }
            return Ok(match key {
                "sign" => Value::num(
                    f.iter()
                        .map(|&x| {
                            if x > 0.0 {
                                1.0
                            } else if x < 0.0 {
                                -1.0
                            } else {
                                0.0
                            }
                        })
                        .find(|&s| s != 0.0)
                        .unwrap_or(0.0),
                ),
                "blank" => Value::bool(f.iter().all(|&x| x == 0.0)),
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
                "yearOfWeek" => Value::num(iso_year_of_week(y, m, d) as f64),
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
                "yearOfWeek" => Value::num(iso_year_of_week(y, m, d) as f64),
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
                // FLOOR division (toward −∞), not truncation toward zero, so a
                // pre-epoch (negative) instant rounds down: ns=-…543211 → -…877 ms.
                "epochMilliseconds" => Value::num(ns.div_euclid(1_000_000) as f64),
                "epochNanoseconds" => self.make_bigint(ns),
                "epochSeconds" => Value::num(ns.div_euclid(1_000_000_000) as f64),
                "epochMicroseconds" => self.make_bigint(ns.div_euclid(1_000)),
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
                "yearOfWeek" => Value::num(iso_year_of_week(y, m, d) as f64),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "daysInWeek" => Value::num(7.0),
                "monthsInYear" => Value::num(12.0),
                "hoursInDay" => {
                    // (startOfDay(tomorrow) − startOfDay(today)) / 1h. A fixed-offset
                    // (incl. UTC) zone's day is exactly 24h, but the next day's start
                    // can fall outside the representable instant range at the max
                    // boundary — a RangeError.
                    const DAY_NS_I: i128 = 86_400_000_000_000;
                    const NS_MAX: i128 = 8_640_000_000_000_000_000_000;
                    let today_start = iso_to_epoch_days(y, m, d) as i128 * DAY_NS_I - off as i128;
                    let tomorrow_start = today_start + DAY_NS_I;
                    // GetStartOfDay throws for BOTH boundaries: a nonzero offset can
                    // push today's local midnight itself past the instant range.
                    if today_start.abs() > NS_MAX || tomorrow_start.abs() > NS_MAX {
                        return Err(Thrown(
                            "RangeError: ZonedDateTime hoursInDay is outside the representable range"
                                .into(),
                        ));
                    }
                    Value::num(((tomorrow_start - today_start) / 3_600_000_000_000) as f64)
                }
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                "era" | "eraYear" => Value::UNDEFINED,
                // FLOOR division (toward −∞), not truncation, for negative epochs.
                "epochSeconds" => Value::num(epoch.div_euclid(1_000_000_000) as f64),
                "epochMilliseconds" => Value::num(epoch.div_euclid(1_000_000) as f64),
                "epochMicroseconds" => self.make_bigint(epoch.div_euclid(1_000)),
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
                    // the element, like GetIndex. A present own element wins; a hole or
                    // out-of-range index is not own, so [[Get]] continues to the
                    // prototype (an inherited `Array.prototype[i]` is visited, and the
                    // internal HOLE sentinel never leaks).
                    let present = matches!(items.get(i as usize), Some(v) if !v.is_hole());
                    let own = items.get(i as usize).copied();
                    if present {
                        Ok(own.unwrap())
                    } else {
                        // Not an own element → [[Get]] continues up the prototype chain.
                        // A subclass-of-Array instance records its own prototype in
                        // proto_of (chains to Array.prototype); else the default arr_proto.
                        let eff = self.array_eff_proto(obj.heap_index());
                        if eff != 0 {
                            self.get_member(Value::heap(eff), key, receiver)
                        } else {
                            Ok(Value::UNDEFINED)
                        }
                    }
                } else if key == "raw" {
                    // A tagged-template strings array's `.raw` (side table).
                    Ok(self.template_raws.get(&obj.heap_index()).copied().unwrap_or(Value::UNDEFINED))
                } else {
                    // A method as a VALUE (`arr.map`, `arr.slice`, …) → Array.prototype,
                    // or the subclass prototype for a `class extends Array` instance.
                    let eff = self.array_eff_proto(obj.heap_index());
                    Ok(self.proto_member(eff, key))
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
                // A setter-only own static accessor (`static set name(_)`) is an own
                // property: reading it returns undefined and does NOT fall through to
                // %Function.prototype% (e.g. so it doesn't pick up Fp's "" name / 0
                // length).
                if c.static_setters.iter().any(|(k, _)| k == key) {
                    return Ok(Value::UNDEFINED);
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
                            if pc.static_setters.iter().any(|(k, _)| k == key) {
                                return Ok(Value::UNDEFINED);
                            }
                            cur = pc.parent;
                        }
                        // A non-Class parent (a built-in constructor or a plain
                        // function) is the subclass constructor's [[Prototype]]:
                        // delegate the static read up its prototype chain (so
                        // `class X extends Temporal.Y {}` inherits `Y.from` etc.).
                        _ => return self.get_member(Value::heap(pidx), key, obj),
                    }
                }
                // A class is a function: keys not found as a static fall back to
                // Function.prototype (so `C.toString()` → the class source via
                // FN_TO_STRING, and `C.call`/`apply`/`bind` resolve). Accessor-aware
                // so an inherited getter on the chain is invoked, not returned raw.
                self.proto_member_get(self.fn_proto, key, receiver)
            }
            // `map.size` / `set.size` — an accessor property, not a method.
            // Deleted entries are tombstoned (Value::HOLE) without shifting indices
            // (so live iterators/forEach stay valid), so size counts only live slots.
            HeapObj::Map { keys, .. } if key == "size" => {
                Ok(len_value(keys.iter().filter(|k| !k.is_hole()).count()))
            }
            HeapObj::Set(items) if key == "size" => {
                Ok(len_value(items.iter().filter(|v| !v.is_hole()).count()))
            }
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
                // An own property in the fn_props bag (incl. an explicitly defined
                // `prototype` on a bound/arrow/async fn) shadows the inherited chain;
                // an accessor invokes its getter with the receiver.
                if let Some((accessor, raw)) = self
                    .fn_props
                    .get(&obj.heap_index())
                    .and_then(|m| m.pos(key).map(|i| (m.attrs[i].accessor, m.vals[i])))
                {
                    if accessor {
                        return if raw == Value::UNDEFINED {
                            Ok(Value::UNDEFINED)
                        } else {
                            self.call_value(raw, receiver, &[])
                        };
                    }
                    return Ok(raw);
                }
                // Poison-pill: `caller`/`arguments` on a STRICT or BOUND function are
                // the %ThrowTypeError% accessors (AddRestrictedFunctionProperties).
                // A sloppy function reads `undefined` here (zipp exposes no legacy own
                // caller/arguments) — handled explicitly so the inherited throwing
                // accessor on Function.prototype is not leaked as a value by the
                // proto-chain walk below.
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
                    return Ok(Value::UNDEFINED);
                }
                // Inherited methods: a generator/async function starts at its
                // dynamic-function intrinsic prototype (so `gen.constructor` is
                // %GeneratorFunction%), else %Function.prototype% (call/apply/bind),
                // then up to Object.prototype (toString/valueOf/hasOwnProperty/…).
                let start = self
                    .callable_dynfn_proto(obj.heap_index())
                    .unwrap_or(self.fn_proto);
                // Accessor-aware so an inherited getter on Function.prototype (or a
                // dynamic-function intrinsic) is invoked with this = receiver.
                self.proto_member_get(start, key, receiver)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

}
