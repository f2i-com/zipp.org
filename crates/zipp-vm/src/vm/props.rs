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
    pub(crate) fn object_enum_own(&mut self, obj: Value, what: EnumWhat) -> Value {
        let pairs: Vec<(String, Value)> = if obj.is_heap() {
            match self.heap.get(obj.heap_index()) {
                HeapObj::Object(m) => m
                    .keys
                    .iter()
                    .cloned()
                    .zip(m.vals.iter().copied())
                    .zip(m.attrs.iter())
                    .filter(|((k, _), a)| a.enumerable && !is_hidden_key(k))
                    .map(|(kv, _)| kv)
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
        Value::heap(self.heap.alloc(HeapObj::Array(out)))
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
                match key.parse::<usize>() {
                    Ok(i) if i < items.len() => {
                        let v = items[i];
                        return self.make_data_descriptor(v, true, true, true);
                    }
                    // A named (non-index) own property lives in arr_props; let the
                    // shared tail render it as a data/accessor descriptor.
                    _ => self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
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
            _ => None,
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
    pub(crate) fn object_own_property_names(&mut self, obj: Value) -> Value {
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
                    keys.extend(m.keys.iter().filter(|k| !is_hidden_key(k)).cloned())
                }
                HeapObj::Array(items) => {
                    for i in 0..items.len() {
                        keys.push(i.to_string());
                    }
                    keys.push("length".to_string());
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
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
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
        Value::heap(self.heap.alloc(HeapObj::Array(names)))
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
            HeapObj::Boxed { kind, .. } => match kind {
                0 => self.str_proto,
                1 => self.num_proto,
                _ => self.bool_proto,
            },
            HeapObj::Date(_) => self.date_proto,
            HeapObj::Promise { .. } => self.promise_proto,
            _ => 0,
        };
        if builtin_proto != 0 {
            return Value::heap(builtin_proto);
        }
        // kind: 0=plain/instance object, 1=callable, 2=array, 3=other.
        let (class, kind) = match self.heap.get(idx) {
            HeapObj::Object(m) => (m.class, 0u8),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                (None, 1)
            }
            HeapObj::Array(_) => (None, 2),
            _ => (None, 3),
        };
        match kind {
            0 => {
                if let Some(cidx) = class {
                    if let Some(p) = self.prototype_of(Value::heap(cidx)) {
                        return p;
                    }
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
        // Presence uses [[HasProperty]]-on-own across object/class/function bags.
        let present = |vm: &Self, k: &str| -> bool { vm.has_own_property(desc, k) };
        let value = if present(self, "value") { Some(self.get_prop(desc, "value")?) } else { None };
        let get = if present(self, "get") { Some(self.get_prop(desc, "get")?) } else { None };
        let set = if present(self, "set") { Some(self.get_prop(desc, "set")?) } else { None };
        let writable = if present(self, "writable") {
            let v = self.get_prop(desc, "writable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let enumerable = if present(self, "enumerable") {
            let v = self.get_prop(desc, "enumerable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let configurable = if present(self, "configurable") {
            let v = self.get_prop(desc, "configurable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        Ok((value, get, set, writable, enumerable, configurable))
    }

    /// `Object.defineProperty(obj, key, descriptor)` — define/redefine an own
    /// property with explicit attributes (unspecified attrs default to false on a
    /// new property; an existing non-configurable property rejects most changes).
    pub(crate) fn object_define_property(&mut self, obj: Value, key: &str, desc: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
        }
        let idx = obj.heap_index();
        // Array: a numeric-index data descriptor sets the element; `length` resizes.
        // (Index accessors / extra named props aren't modeled — accepted as a no-op
        // so the definition doesn't abort the program, matching common test setup.)
        if let HeapObj::Array(_) = self.heap.get(idx) {
            // A numeric-index data descriptor sets the element; `length` resizes.
            // A named (non-index) property falls through to the generic path below
            // (target 3 -> arr_props) with full descriptor semantics. read_descriptor
            // (which may run getters) is therefore invoked exactly once per path.
            if let Ok(i) = key.parse::<usize>() {
                if i >= crate::vm::MAX_DENSE_ARRAY_LEN {
                    return Err(Thrown(
                        "RangeError: array index exceeds the engine's dense-array limit".into(),
                    ));
                }
                let (value, get, set, ..) = self.read_descriptor(desc)?;
                if get.is_none() && set.is_none() {
                    let v = value.unwrap_or(Value::UNDEFINED);
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        if i >= items.len() {
                            items.resize(i + 1, Value::UNDEFINED);
                        }
                        items[i] = v;
                    }
                    self.heap.bump_version(idx);
                }
                return Ok(());
            }
            if key == "length" {
                let (value, ..) = self.read_descriptor(desc)?;
                if let Some(v) = value {
                    let n = self.to_number(v)?;
                    if !(n >= 0.0 && n.fract() == 0.0 && n < 4_294_967_296.0) {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    if n as usize > crate::vm::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: array length exceeds the engine's dense-array limit".into(),
                        ));
                    }
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        items.resize(n as usize, Value::UNDEFINED);
                    }
                    self.heap.bump_version(idx);
                }
                return Ok(());
            }
            // else: named key → generic path.
        }
        // TypedArray: a numeric-index data descriptor writes the element.
        if let HeapObj::TypedArray { .. } = self.heap.get(idx) {
            let (value, get, set, ..) = self.read_descriptor(desc)?;
            if get.is_none() && set.is_none() {
                if let Ok(i) = key.parse::<usize>() {
                    self.ta_element_set(idx, i, value.unwrap_or(Value::UNDEFINED))?;
                }
            }
            return Ok(());
        }
        // 0 = plain object, 1 = class (own props live in `statics`), 2 = callable
        // (own props live in `fn_props`).
        let target = match self.heap.get(idx) {
            HeapObj::Object(_) => 0u8,
            HeapObj::Class(_) => 1,
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => 2,
            HeapObj::Array(_) => 3, // named (non-index) own prop -> arr_props
            _ => return Err(Thrown("TypeError: Object.defineProperty called on non-object".into())),
        };
        // A callable's/class's `name`/`length`/`prototype` are synthesized; accept
        // the call but don't shadow them (full redefinition isn't modelled).
        if target != 0 && matches!(key, "name" | "length" | "prototype") {
            return Ok(());
        }
        let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
        let existing = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            HeapObj::Class(c) => c.statics.pos(key).map(|i| (c.statics.attrs[i], c.statics.vals[i])),
            HeapObj::Array(_) => self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
            _ => self.fn_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
        };
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
                let change_kind = is_accessor != a.accessor;
                let make_writable = !a.writable && d_wr == Some(true);
                let change_frozen_value =
                    !a.accessor && !a.writable && value.is_some_and(|v| v != oldv);
                if make_cfg || change_enum || change_kind || make_writable || change_frozen_value {
                    return Err(Thrown(format!("TypeError: Cannot redefine property: {key}")));
                }
            }
        }
        // Defining a brand-new property requires the object to be extensible.
        if existing.is_none() {
            let extensible = match self.heap.get(idx) {
                HeapObj::Object(m) => m.extensible,
                _ => true,
            };
            if !extensible {
                return Err(Thrown(format!(
                    "TypeError: Cannot define property {key}, object is not extensible"
                )));
            }
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

    /// `Object.defineProperties(obj, props)` — define each own enumerable key of
    /// `props` as a descriptor on `obj`.
    pub(crate) fn object_define_properties(&mut self, obj: Value, props: Value) -> Result<(), Thrown> {
        if !props.is_heap() {
            return Ok(());
        }
        let keys: Vec<String> = match self.heap.get(props.heap_index()) {
            HeapObj::Object(m) => m
                .keys
                .iter()
                .zip(m.attrs.iter())
                .filter(|(_, a)| a.enumerable)
                .map(|(k, _)| k.clone())
                .collect(),
            _ => Vec::new(),
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
                let p = &self.program.functions[*fid as usize];
                Some((clean(&p.name), p.param_count as i32))
            }
            HeapObj::Closure { func, .. } => {
                let p = &self.program.functions[*func as usize];
                Some((clean(&p.name), p.param_count as i32))
            }
            HeapObj::Class(c) => {
                let len = c
                    .ctor
                    .map(|f| self.program.functions[f as usize].param_count as i32)
                    .unwrap_or(0);
                Some((clean(&c.name), len))
            }
            // A native value's `name`/`length`: a prototype method
            // (`Array.prototype.map.name === "map"`, length 1) or a static/namespace
            // method (`Object.keys.name === "keys"`, `Reflect.get.length === 2`).
            HeapObj::Native(id) => native::proto_method(*id)
                .map(|(n, _, l)| (n.to_string(), l as i32))
                .or_else(|| native::math_method(*id).map(|(n, _, l)| (n.to_string(), l as i32)))
                .or_else(|| native::static_name_length(*id).map(|(n, l)| (n.to_string(), l as i32))),
            // The anonymous functions returned by the Intl format/compare getters
            // have name "" and length 1 (format) / 2 (compare).
            HeapObj::Bound { target, .. } if target.is_heap() => {
                if let HeapObj::Native(tid) = self.heap.get(target.heap_index()) {
                    match *tid {
                        native::INTL_NF_FORMAT | native::INTL_DTF_FORMAT => {
                            Some((String::new(), 1))
                        }
                        native::INTL_COLLATOR_COMPARE => Some((String::new(), 2)),
                        _ => None,
                    }
                } else {
                    None
                }
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
        if let HeapObj::TypedArray { buffer, kind, byte_offset, length } = self.heap.get(obj.heap_index()) {
            let (buffer, kind, byte_offset, length) = (*buffer, *kind, *byte_offset, *length);
            let size = native::TA_KINDS[kind as usize].1;
            // A canonical numeric string index reads the element.
            if let Ok(i) = key.parse::<usize>() {
                return Ok(self.ta_element_get(obj.heap_index(), i));
            }
            // A TypedArray over a detached buffer reports length/byteLength/
            // byteOffset as 0.
            let detached = matches!(self.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. });
            return Ok(match key {
                "length" => Value::num(if detached { 0.0 } else { length as f64 }),
                "byteLength" => Value::num(if detached { 0.0 } else { (length * size) as f64 }),
                "byteOffset" => Value::num(if detached { 0.0 } else { byte_offset as f64 }),
                "BYTES_PER_ELEMENT" => Value::num(size as f64),
                "buffer" => Value::heap(buffer),
                "@@toStringTag" => self.alloc_str(native::TA_KINDS[kind as usize].0.to_string()),
                _ => self.proto_member(self.ta_protos[kind as usize], key),
            });
        }
        if let HeapObj::ArrayBuffer { data, .. } = self.heap.get(obj.heap_index()) {
            let len = data.len();
            return Ok(match key {
                "byteLength" => Value::num(len as f64),
                _ => self.proto_member(self.arraybuffer_proto, key),
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
                Ok(Value::UNDEFINED)
            }
            // `map.size` / `set.size` — an accessor property, not a method.
            HeapObj::Map { keys, .. } if key == "size" => Ok(len_value(keys.len())),
            HeapObj::Set(items) if key == "size" => Ok(len_value(items.len())),
            // A method as a VALUE on a Map/Set/Date/Promise instance
            // (`new Map().set`, `d.getHours`) → the corresponding prototype.
            HeapObj::Map { .. } => Ok(self.proto_member(self.map_proto, key)),
            HeapObj::Set(_) => Ok(self.proto_member(self.set_proto, key)),
            HeapObj::WeakMap { .. } => Ok(self.proto_member(self.weakmap_proto, key)),
            HeapObj::WeakSet(_) => Ok(self.proto_member(self.weakset_proto, key)),
            HeapObj::WeakRef(_) => Ok(self.proto_member(self.weakref_proto, key)),
            HeapObj::FinalizationRegistry { .. } => Ok(self.proto_member(self.finreg_proto, key)),
            HeapObj::Iterator { proto, .. } => {
                let p = *proto;
                self.proto_chain_get(p, key, obj)
            }
            HeapObj::IterHelper { .. } => {
                let p = self.iterator_helper_proto;
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
                    _ => self.bool_proto,
                };
                Ok(self.proto_member(proto, key))
            }
            HeapObj::Date(_) => Ok(self.proto_member(self.date_proto, key)),
            HeapObj::Promise { .. } => Ok(self.proto_member(self.promise_proto, key)),
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
                // Inherited methods: Function.prototype (call/apply/bind) then up
                // the chain to Object.prototype (toString/valueOf/hasOwnProperty/…),
                // so `fn.toString`, `fn.hasOwnProperty`, `fn + ''` (ToPrimitive) work.
                Ok(self.proto_member(self.fn_proto, key))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

}
