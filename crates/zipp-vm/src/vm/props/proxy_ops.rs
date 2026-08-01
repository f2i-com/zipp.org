// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

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
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => false,
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
                self.materialize_regexp_result_props(ti);
                if matches!(self.heap.get(ti), HeapObj::Object(_)) {
                    if let HeapObj::Object(m) = self.heap.get_mut(ti) {
                        m.extensible = false;
                    }
                    // In-place attrs change: invalidate JIT inline caches.
                    self.heap.bump_version(ti);
                } else if !matches!(
                    self.heap.get(ti),
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                ) {
                    self.arr_props.entry(ti).or_insert_with(ObjMap::new_side_table).extensible = false;
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
                // CreateListFromArrayLike: ANY object with a length is accepted
                // (its indexed Gets — possibly getters — are observable); only a
                // non-object trap result is a TypeError.
                if !self.is_object_value(r) {
                    return Err(Thrown(
                        "TypeError: proxy [[OwnPropertyKeys]] must return an array-like object"
                            .into(),
                    ));
                }
                let items = self.create_list_from_array_like(r)?;
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

}
