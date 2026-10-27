// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// `Vm::proxy_trap_front`'s size: a direct-mapped memo of where Proxy
/// handlers keep their traps (`Vm::trap_front_get`).
pub(crate) const TRAP_FRONT_SLOTS: usize = 64;

/// One entry of `Vm::proxy_trap_front`: a handler of `shape` keeps trap
/// `trap` (`trap_id`) in own slot `slot`, or (`ABSENT`) neither it nor
/// %Object.prototype% at version `obj_proto_ver` has it. `EMPTY`'s shape is
/// `DICT`, which no described handler has.
#[derive(Clone, Copy)]
pub(crate) struct TrapFront {
    shape: u32,
    trap: u32,
    slot: u32,
    obj_proto_ver: u32,
}

impl TrapFront {
    pub(crate) const EMPTY: TrapFront =
        TrapFront { shape: crate::shape::DICT, trap: u32::MAX, slot: 0, obj_proto_ver: 0 };
    const ABSENT: u32 = u32::MAX;
}

/// A Proxy trap's name as a small number, `None` for any other name.
#[inline(always)]
fn trap_id(name: &str) -> Option<u32> {
    Some(match name {
        "get" => 0,
        "set" => 1,
        "has" => 2,
        "deleteProperty" => 3,
        "defineProperty" => 4,
        "getOwnPropertyDescriptor" => 5,
        "ownKeys" => 6,
        "apply" => 7,
        "construct" => 8,
        "getPrototypeOf" => 9,
        "setPrototypeOf" => 10,
        "isExtensible" => 11,
        "preventExtensions" => 12,
        _ => return None,
    })
}

#[inline(always)]
fn trap_front_index(shape: u32, trap: u32) -> usize {
    (shape.wrapping_mul(13).wrapping_add(trap) as usize) & (TRAP_FRONT_SLOTS - 1)
}

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
        if let Some(reduced) = self.plain_target_desc(target, key) {
            return Ok(reduced);
        }
        let desc = self.object_get_own_property_descriptor(target, key);
        if desc == Value::UNDEFINED {
            return Ok(None);
        }
        let cfg = self.get_prop(desc, "configurable")?;
        if self.truthy(cfg) {
            return Ok(None);
        }
        // A data descriptor carries an own "value" key; an accessor carries get/set.
        let is_data = matches!(self.heap.get(desc.heap_index()), HeapObj::Object(m) if m.pos("value").is_some());
        if is_data {
            let wv = self.get_prop(desc, "writable")?;
            let writable = self.truthy(wv);
            let value = self.get_prop(desc, "value")?;
            Ok(Some((true, value, writable, false, false)))
        } else {
            let g = self.get_prop(desc, "get")?;
            let s = self.get_prop(desc, "set")?;
            Ok(Some((
                false,
                Value::UNDEFINED,
                false,
                g != Value::UNDEFINED,
                s != Value::UNDEFINED,
            )))
        }
    }

    /// [`Self::proxy_target_desc`] for an ordinary object target, read from
    /// its map: the attributes `object_get_own_property_descriptor` would put
    /// in the descriptor it builds. Building that object (four properties)
    /// and reading three of them back was most of the cost of a `get` or
    /// `set` trap. `None` (use the descriptor) for a target whose own
    /// properties are not all in its map: a non-object, a constructor (its
    /// synthesized `name`/`length`/`prototype`), the global object (its slot
    /// bindings), `%Array.prototype%` (its `length`), a module namespace (its
    /// live bindings), and every exotic object.
    fn plain_target_desc(
        &self,
        target: Value,
        key: &str,
    ) -> Option<Option<(bool, Value, bool, bool, bool)>> {
        if !target.is_heap() {
            return None;
        }
        let idx = target.heap_index();
        if (idx == self.global_this && self.global_this != 0)
            || (idx == self.arr_proto && self.arr_proto != 0)
            || (!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&idx))
        {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return self.plain_array_target_desc(idx, key);
        };
        if m.is_ctor {
            return None;
        }
        // Every property of an all-default map is a configurable data
        // property: no invariant applies, present or not, and no lookup is
        // needed to say so.
        if !m.may_deviate_attrs() {
            return Some(None);
        }
        let Some(i) = m.pos(key) else {
            return Some(None);
        };
        let a = m.attr_at(i);
        if a.configurable {
            return Some(None);
        }
        let raw = m.val_at(i);
        Some(Some(if a.accessor {
            (
                false,
                Value::UNDEFINED,
                false,
                raw != Value::UNDEFINED,
                a.setter != Value::UNDEFINED,
            )
        } else {
            (true, raw, a.writable, false, false)
        }))
    }

    /// [`Self::plain_target_desc`] for an array target with nothing in a side
    /// table: its elements are configurable (no invariant applies, present
    /// or not), and its `length` is the non-configurable data property whose
    /// writability `array_length_nonwritable` records. `None` for anything
    /// else: an arguments object (configurable `length`, mapped elements),
    /// any other kind of object, and any other key. Out of line, so the
    /// ordinary-object path compiles as it did.
    #[inline(never)]
    fn plain_array_target_desc(&self, idx: u32, key: &str) -> Option<Option<(bool, Value, bool, bool, bool)>> {
        if !matches!(self.heap.get(idx), HeapObj::Array(_))
            || self.arguments_objs.contains_key(&idx)
            || (!self.arr_props.is_empty() && self.arr_props.contains_key(&idx))
        {
            return None;
        }
        if key == "length" {
            let writable = !self.array_length_nonwritable.contains(&idx);
            return Some(Some((true, len_value(self.js_array_len(idx)), writable, false, false)));
        }
        crate::heap::canonical_array_index_key(key).map(|_| None)
    }

    /// Whether the Proxy `handler` certainly has no `name` trap, found without
    /// running code: every object on its chain is an ordinary object that
    /// does not hold `name`. GetMethod(handler, name) would then be
    /// undefined, and skipping it unobservable. `false` (look it up) for
    /// anything else.
    pub(crate) fn proxy_handler_lacks(&self, handler: Value, name: &str) -> bool {
        if !self.deferred_ns_state.is_empty() {
            return false;
        }
        if let Some(t) = self.trap_front_get(handler, name) {
            return t.is_none();
        }
        self.trap_front_fill(handler, name);
        let mut cur = handler;
        for _ in 0..64 {
            if !cur.is_heap() {
                return true;
            }
            let idx = cur.heap_index();
            if (idx == self.global_this && self.global_this != 0)
                || (!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&idx))
            {
                return false;
            }
            let HeapObj::Object(m) = self.heap.get(idx) else {
                return false;
            };
            if m.is_ctor || m.class.is_some() || m.pos(name).is_some() {
                return false;
            }
            cur = match self.proto_of.get_copied(idx) {
                Some(p) => p,
                None if self.obj_proto != 0 && idx != self.obj_proto => Value::heap(self.obj_proto),
                None => return true,
            };
        }
        false
    }

    /// Where a Proxy handler keeps its `name` trap, from `proxy_trap_front`:
    /// `Some(Some(v))` for an own data property, `Some(None)` when neither
    /// the handler nor %Object.prototype% has the name, `None` (look it up)
    /// for a memo miss and every handler the memo does not describe. Every
    /// operation on a proxy found its trap by name: the handler's own keys,
    /// then %Object.prototype%'s hashed ones when the handler leaves the trap
    /// out (a forwarding `{}` handler on every access).
    ///
    /// The handler's shape fixes its key sequence and attributes, and shapes
    /// are never reissued, so a (shape, trap) answer about the handler holds
    /// for any object of that shape. An absent answer also needs the chain
    /// past it: the default link to %Object.prototype% (whose own
    /// [[Prototype]] is null for good) at the version the memo recorded,
    /// which every key add, delete or redefinition there moves.
    #[inline]
    pub(crate) fn trap_front_get(&self, handler: Value, name: &str) -> Option<Option<Value>> {
        let id = trap_id(name)?;
        if !handler.is_heap() {
            return None;
        }
        let idx = handler.heap_index();
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let sh = m.shape();
        let e = self.proxy_trap_front[trap_front_index(sh, id)].get();
        if e.shape != sh
            || e.trap != id
            || sh == crate::shape::DICT
            || m.is_ctor
            || m.class.is_some()
            || self.active_realm.is_some()
            || !self.realm_global_objs.is_empty()
            || !self.deferred_ns_state.is_empty()
        {
            return None;
        }
        if e.slot == TrapFront::ABSENT {
            return (self.proto_of.link(idx) == crate::slot_table::ProtoTable::NONE
                && self.obj_proto != 0
                && self.heap.version_of(self.obj_proto) == e.obj_proto_ver)
                .then_some(None);
        }
        let v = m.val_at(e.slot as usize);
        (!v.is_uninitialized()).then_some(Some(v))
    }

    /// Record in `proxy_trap_front` where `handler` keeps its `name` trap,
    /// when [`Self::trap_front_get`] can answer it: an own data property of a
    /// shaped ordinary handler, or a name that neither such a handler (with
    /// the default prototype) nor %Object.prototype% holds.
    #[inline(never)]
    pub(crate) fn trap_front_fill(&self, handler: Value, name: &str) {
        let Some(id) = trap_id(name) else {
            return;
        };
        if !handler.is_heap()
            || self.active_realm.is_some()
            || !self.realm_global_objs.is_empty()
            || !self.deferred_ns_state.is_empty()
            || self.obj_proto == 0
        {
            return;
        }
        let idx = handler.heap_index();
        if idx == self.global_this && self.global_this != 0 {
            return;
        }
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return;
        };
        let sh = m.shape();
        if sh == crate::shape::DICT || m.is_ctor || m.class.is_some() {
            return;
        }
        let slot = match m.pos(name) {
            Some(i) => {
                if m.attr_at(i).accessor || m.val_at(i).is_uninitialized() {
                    return;
                }
                i as u32
            }
            None => {
                if self.proto_of.link(idx) != crate::slot_table::ProtoTable::NONE {
                    return;
                }
                let HeapObj::Object(op) = self.heap.get(self.obj_proto) else {
                    return;
                };
                if op.is_ctor || op.class.is_some() || op.pos(name).is_some() {
                    return;
                }
                TrapFront::ABSENT
            }
        };
        self.proxy_trap_front[trap_front_index(sh, id)].set(TrapFront {
            shape: sh,
            trap: id,
            slot,
            obj_proto_ver: self.heap.version_of(self.obj_proto),
        });
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
            return Err(Thrown::from_static(
                "TypeError: Cannot perform 'setPrototypeOf' on a revoked proxy",
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
                if !self.with_native_recursion_guard(|vm| vm.is_extensible(target))? {
                    let target_proto = self.object_get_prototype_of(target);
                    if !self.same_value(proto, target_proto) {
                        return Err(Thrown::from_static(
                            "TypeError: 'setPrototypeOf' on proxy: trap returned truish for setting a new prototype on the non-extensible proxy target",
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
                Ok(Some(self.with_native_recursion_guard(|vm| {
                    vm.ordinary_set_prototype_of(target, proto)
                })?))
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
            return Err(Thrown::from_static(
                "TypeError: Cannot perform 'isExtensible' on a revoked proxy",
            ));
        }
        match self.proxy_trap(handler, "isExtensible")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target])?;
                let result = self.truthy(r);
                // Invariant: the trap result must equal IsExtensible(target).
                if result != self.with_native_recursion_guard(|vm| vm.is_extensible(target))? {
                    return Err(Thrown::from_static(
                        "TypeError: 'isExtensible' on proxy: trap result does not reflect extensibility of proxy target",
                    ));
                }
                Ok(Some(result))
            }
            None => {
                if let Some(b) =
                    self.with_native_recursion_guard(|vm| vm.proxy_is_extensible(target))?
                {
                    return Ok(Some(b)); // nested proxy target
                }
                let ext = match self.heap.get(target.heap_index()) {
                    HeapObj::Object(m) => m.extensible,
                    HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::Symbol { .. }
                    | HeapObj::BigInt(_)
                    | HeapObj::BigIntBig(_) => false,
                    _ => self
                        .arr_props
                        .get(&target.heap_index())
                        .map_or(true, |m| m.extensible),
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
            return Err(Thrown::from_static(
                "TypeError: Cannot perform 'preventExtensions' on a revoked proxy",
            ));
        }
        match self.proxy_trap(handler, "preventExtensions")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target])?;
                let result = self.truthy(r);
                // Invariant: a true result requires the target to be non-extensible.
                if result && self.with_native_recursion_guard(|vm| vm.is_extensible(target))? {
                    return Err(Thrown::from_static(
                        "TypeError: 'preventExtensions' on proxy: trap returned truish but the proxy target is extensible",
                    ));
                }
                Ok(Some(result))
            }
            None => {
                if let Some(b) =
                    self.with_native_recursion_guard(|vm| vm.proxy_prevent_extensions(target))?
                {
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
                    HeapObj::Str(_)
                        | HeapObj::Cons { .. }
                        | HeapObj::Symbol { .. }
                        | HeapObj::BigInt(_)
                        | HeapObj::BigIntBig(_)
                ) {
                    self.arr_props
                        .entry(ti)
                        .or_insert_with(ObjMap::new_side_table)
                        .extensible = false;
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
            return Err(Thrown::from_static(
                "TypeError: Cannot perform 'ownKeys' on a revoked proxy",
            ));
        }
        match self.proxy_trap(handler, "ownKeys")? {
            Some(trap) => {
                let r = self.call_value(trap, handler, &[target])?;
                // CreateListFromArrayLike: ANY object with a length is accepted
                // (its indexed Gets — possibly getters — are observable); only a
                // non-object trap result is a TypeError.
                if !self.is_object_value(r) {
                    return Err(Thrown::from_static(
                        "TypeError: proxy [[OwnPropertyKeys]] must return an array-like object",
                    ));
                }
                let items = self.create_list_from_array_like(r)?;
                // The key strings live only in `items` while a Proxy target's
                // isExtensible / ownKeys traps run below; the caller receives
                // them after the scope closes, with no safe point in between.
                self.with_host_roots(&items, |vm| vm.proxy_own_keys_validate(target, &items))?;
                Ok(Some(items))
            }
            None => {
                // No trap: forward the target's full own-key list (Strings AND
                // Symbols, so getOwnPropertySymbols/Reflect.ownKeys see symbol keys).
                let keys = self.with_native_recursion_guard(|vm| vm.object_own_keys(target))?;
                Ok(Some(self.array_snapshot(keys.heap_index())))
            }
        }
    }

    /// [[OwnPropertyKeys]] steps 8-22 for a trap result `items` (already a
    /// rooted list): the element-type and duplicate checks, then the target
    /// key invariants.
    fn proxy_own_keys_validate(&mut self, target: Value, items: &[Value]) -> Result<(), Thrown> {
        // CreateListFromArrayLike with «String, Symbol» element-type check
        // and the no-duplicate-entries invariant (spec 10.5.11 steps 8-9).
        let mut seen_set: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        seen_set.try_reserve(items.len()).map_err(|_| {
            Thrown::from_static("RangeError: proxy ownKeys validation allocation failed")
        })?;
        for k in items {
            let is_str = k.is_heap() && self.heap.is_str_like(k.heap_index());
            let is_sym = k.is_heap()
                && matches!(self.heap.get(k.heap_index()), HeapObj::Symbol { .. });
            if !is_str && !is_sym {
                return Err(Thrown::from_static(
                    "TypeError: ownKeys trap result must contain only Strings and Symbols",
                ));
            }
            let id = if is_str {
                format!("s:{}", self.display(*k))
            } else {
                format!("y:{}", k.heap_index())
            };
            if !seen_set.insert(id.clone()) {
                return Err(Thrown::from_static(
                    "TypeError: ownKeys trap result must not contain duplicate entries",
                ));
            }
        }
        // Target-key invariants (10.5.11 steps 10-22). Partition the target's
        // own keys into configurable / non-configurable, keyed by the same
        // identity scheme as `seen_set` (string content / symbol heap index).
        let extensible = self.with_native_recursion_guard(|vm| vm.is_extensible(target))?;
        let tkeys_v = self.with_native_recursion_guard(|vm| vm.object_own_keys(target))?;
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
            let mut unchecked = seen_set;
            // Every non-configurable own key MUST appear in the trap result.
            for key in &nonconfig {
                if !unchecked.remove(key) {
                    return Err(Thrown::from_static(
                        "TypeError: proxy [[OwnPropertyKeys]] must include all non-configurable keys of the target",
                    ));
                }
            }
            // A non-extensible target: the trap result must contain EXACTLY
            // the target's own keys (every configurable key present, none extra).
            if !extensible {
                for key in &config {
                    if !unchecked.remove(key) {
                        return Err(Thrown::from_static(
                            "TypeError: proxy [[OwnPropertyKeys]] of a non-extensible target must contain all of its own keys",
                        ));
                    }
                }
                if !unchecked.is_empty() {
                    return Err(Thrown::from_static(
                        "TypeError: proxy [[OwnPropertyKeys]] of a non-extensible target must not contain extra keys",
                    ));
                }
            }
        }
        Ok(())
    }
}
