#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Outcome of walking the prototype chain for an OrdinarySet `[[Set]]` whose
/// receiver has no own data property for the key.
enum ProtoSet {
    /// An inherited accessor with a setter — invoke it with the receiver.
    Setter(Value),
    /// An inherited getter-only accessor — assignment is a no-op (sloppy) / throw.
    GetterOnly,
    /// A Proxy in the chain handled the write via its `set` trap (`true` = ok,
    /// `false` = trap returned falsish → no-op/throw).
    Proxy(bool),
    /// A TypedArray in the chain ABSORBED a canonical-numeric key that is not a
    /// valid index: its [[Set]] reports success with no write and no coercion
    /// (the prototype beyond it is never consulted).
    Absorbed,
    /// No accessor or proxy governs the write — write an own data property on the
    /// receiver (shadowing any inherited WRITABLE data property).
    DataWrite,
    /// An inherited NON-WRITABLE data property — the write is rejected (a sloppy
    /// no-op / strict TypeError); it does NOT create a shadowing own property.
    NonWritable,
}

impl<'p> Vm<'p> {
    /// JS `typeof` type-name. `null` is `"object"` (a historic quirk); functions
    /// and closures are `"function"`; arrays and objects are `"object"`.
    pub(crate) fn type_of(&self, v: Value) -> &'static str {
        if v.is_int() || v.is_double() {
            "number"
        } else if v.is_bool() {
            "boolean"
        } else if v.is_undefined() {
            "undefined"
        } else if v.is_null() {
            "object"
        } else if v.is_heap() {
            // An [[IsHTMLDDA]] exotic (`document.all`): `typeof` is "undefined".
            if self.is_htmldda.contains(&v.heap_index()) {
                return "undefined";
            }
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "string",
                // A class is callable (with `new`), so `typeof C === "function"`.
                // Native builtins and bound functions are callable too.
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Native(_)
                | HeapObj::Bound { .. }
                | HeapObj::BoundResolver { .. }
                | HeapObj::CombinatorResolver { .. } => "function",
                HeapObj::Cell(inner) => self.type_of(*inner), // see through an upvalue cell
                HeapObj::Proxy { target, .. } => self.type_of(*target), // typeof = target's
                HeapObj::Symbol { .. } => "symbol",
                HeapObj::BigInt(_) => "bigint",
                // The built-in constructor globals (Object/Array/Map/…) are callable.
                HeapObj::Object(m) if m.is_ctor => "function",
                // %Function.prototype% is itself a (no-op) callable function.
                HeapObj::Object(_) if v.heap_index() == self.fn_proto && self.fn_proto != 0 => "function",
                // `Symbol` is callable (typeof "function") but NOT a constructor
                // (so `new Symbol()` throws and IsConstructor is false).
                HeapObj::Object(_) if v.heap_index() == self.symbol_ctor && self.symbol_ctor != 0 => "function",
                HeapObj::Object(_) if v.heap_index() == self.bigint_ctor && self.bigint_ctor != 0 => "function",
                _ => "object", // Array, ordinary Object, namespace globals
            }
        } else {
            "undefined"
        }
    }

    /// `delete obj[key]` — remove an own property, returning the boolean result.
    /// Without property descriptors every own property is configurable, so this
    /// yields `true` (matching `delete` on a missing property / non-object too).
    /// An array element delete leaves a hole (reads as `undefined`), length kept.
    /// `delete obj[key]` honoring a Proxy `deleteProperty` trap (Result-returning
    /// so the trap can throw); else delegates to `delete_prop`.
    pub(crate) fn delete_property(&mut self, obj: Value, key: &str) -> Result<Value, Thrown> {
        if obj.is_heap()
            && key == "length"
            && self.arr_proto != 0
            && obj.heap_index() == self.arr_proto
        {
            // %Array.prototype%'s exotic `length` is non-configurable.
            return Ok(Value::bool(false));
        }
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'deleteProperty' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "deleteProperty")? {
                    Some(trap) => {
                        let kv = self.key_to_value(key);
                        let r = self.call_value(trap, handler, &[target, kv])?;
                        if !self.truthy(r) {
                            return Ok(Value::bool(false));
                        }
                        // Invariant: a deletion reported successful must be allowed
                        // by the target — the property must not be non-configurable,
                        // and (if it exists) the target must be extensible.
                        let desc = self.object_get_own_property_descriptor(target, key);
                        if desc != Value::UNDEFINED {
                            let cfg = self.get_prop(desc, "configurable")?;
                            if !self.truthy(cfg) {
                                return Err(Thrown("TypeError: 'deleteProperty' on proxy: trap returned truish for property which is non-configurable in the proxy target".into()));
                            }
                            if !self.is_extensible(target)? {
                                return Err(Thrown("TypeError: 'deleteProperty' on proxy: trap returned truish for property but the proxy target is non-extensible".into()));
                            }
                        }
                        Ok(Value::bool(true))
                    }
                    // No deleteProperty trap: forward to the target's [[Delete]] —
                    // re-enter the proxy-aware path so a Proxy target's own trap fires.
                    None => self.delete_property(target, key),
                };
            }
        }
        Ok(self.delete_prop(obj, key))
    }

    pub(crate) fn delete_prop(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() {
            return Value::bool(true);
        }
        let idx = obj.heap_index();
        // A non-configurable own property cannot be deleted (`delete` yields false).
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if let Some(i) = m.pos(key) {
                if !m.attrs[i].configurable {
                    return Value::bool(false);
                }
            }
        }
        // `delete globalThis.X` for a built-in global (Number/Date/…): these live in
        // builtin_globals/globals and resolve via global_by_name, NOT as own
        // `global_this` entries — but a prior assignment (e.g. propertyHelper's
        // writable probe does `global.X = tmp`) may have left a shadowing own entry.
        // Remove any such entry AND record the deletion so global_by_name (consulted
        // by get / has-own / descriptor) agrees the property is gone. The value
        // globals NaN/Infinity/undefined are non-configurable, so they can't be
        // deleted (any configurable own entry was already removed by the path above
        // for a user prop; the non-configurable own case returned false earlier).
        if idx == self.global_this
            && self.global_this != 0
            && self.global_by_name(key).is_some()
        {
            if matches!(key, "NaN" | "Infinity" | "undefined") {
                return Value::bool(false);
            }
            // SCRIPT-declared var/function globals are non-configurable
            // bindings — `delete` returns false and removes nothing.
            if self
                .program
                .global_names
                .iter()
                .position(|n| n == key)
                .is_some_and(|i| {
                    self.program.hoisted_globals.contains(&(i as u32))
                        || self.program.decl_globals.contains(&(i as u32))
                })
            {
                return Value::bool(false);
            }
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                m.remove(key);
            }
            self.deleted_globals.insert(key.to_string());
            self.heap.bump_version(idx);
            return Value::bool(true);
        }
        // The same for a non-configurable NAMED (non-index) own property stored in
        // the arr_props side table — Array/Arguments/TypedArray/Map/Set/Date/…
        // `defineProperty`'d named props. Canonical integer-index keys are excluded
        // (they route through the array-index-override path below, which carries
        // its own configurable check); without this, `delete obj.x` on such a prop
        // removed it unconditionally, so verifyProperty's deletion probe wrongly
        // reported a non-configurable property as configurable.
        if key.parse::<usize>().map_or(true, |i| i.to_string() != key) {
            if let Some(m) = self.arr_props.get(&idx) {
                if let Some(p) = m.pos(key) {
                    if !m.attrs[p].configurable {
                        return Value::bool(false);
                    }
                }
            }
        }
        // A String wrapper's `length` and in-range char indices are non-configurable
        // exotic own props — `delete` fails (false). A named own prop (`s.foo`) falls
        // through to the generic deletion below.
        if matches!(self.heap.get(idx), HeapObj::Boxed { kind: 0, .. }) {
            let blocked = key == "length"
                || self
                    .string_exotic_chars(obj)
                    .and_then(|(_, len)| canonical_index_str(key).map(|i| i < len))
                    .unwrap_or(false);
            if blocked {
                return Value::bool(false);
            }
        }
        // A TypedArray's in-bounds integer index is a non-configurable exotic own
        // property: `delete ta[0]` fails (false). An out-of-range / non-index key
        // falls through to the named-property (arr_props) deletion below.
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. })
            && self.ta_valid_index(idx, key).is_some()
        {
            return Value::bool(false);
        }
        // An Array's `length` is a non-configurable own property — `delete arr.length`
        // fails (false), leaving the length intact. An `arguments` exotic is
        // Array-backed but its `length` IS configurable, so exclude it.
        if key == "length"
            && matches!(self.heap.get(idx), HeapObj::Array(_))
            && !self.arguments_objs.contains(&idx)
        {
            return Value::bool(false);
        }
        // A callable's intrinsic `prototype` (normal function, class constructor,
        // generator) is a non-configurable own property — `delete f.prototype`
        // fails (false), even after the value was reassigned. (Arrow / bound / most
        // native functions have no `prototype`, so callable_has_prototype is false
        // and a missing-property delete still returns true below.)
        if key == "prototype" && self.callable_has_prototype(obj) {
            return Value::bool(false);
        }
        // A callable's `name`/`length` are configurable: record the deletion so
        // the synthesized property stops appearing (own-property queries + reads).
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            self.deleted_callable_intrinsics
                .insert((idx, if key == "name" { 0 } else { 1 }));
            return Value::bool(true);
        }
        // Deleting a canonical array index: a non-configurable special override
        // (defineProperty'd) refuses deletion; otherwise drop the override (if any)
        // and clear the dense slot.
        if let HeapObj::Array(_) = self.heap.get(idx) {
            if let Ok(i) = key.parse::<usize>() {
                if i.to_string() == key {
                    // A sealed/frozen array's existing elements are non-configurable,
                    // so an in-bounds index can't be deleted (an out-of-range index is
                    // not an own property and deletes vacuously).
                    let in_bounds =
                        matches!(self.heap.get(idx), HeapObj::Array(items) if i < items.len());
                    if in_bounds
                        && self.arr_props.get(&idx).map_or(false, |m| m.sealed || m.frozen)
                    {
                        return Value::bool(false);
                    }
                    if let Some((a, _)) = self.array_index_override(idx, i) {
                        if !a.configurable {
                            return Value::bool(false);
                        }
                        if let Some(m) = self.arr_props.get_mut(&idx) {
                            m.remove(key);
                        }
                    }
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        if i < items.len() {
                            items[i] = Value::HOLE;
                        }
                    }
                    self.heap.bump_version(idx);
                    return Value::bool(true);
                }
            }
        }
        let removed = match self.heap.get_mut(idx) {
            HeapObj::Object(map) => map.remove(key),
            HeapObj::Array(items) => {
                if let Ok(i) = key.parse::<usize>() {
                    if i < items.len() {
                        items[i] = Value::HOLE;
                    }
                    false // array slot stays (a hole); no version bump needed
                } else {
                    // A named (non-index) own property in arr_props.
                    self.arr_props.get_mut(&idx).map_or(false, |m| m.remove(key))
                }
            }
            HeapObj::Class(c) => {
                // Static methods/data live in `statics`; static accessors live in
                // the `static_getters`/`static_setters` side lists. A class's own
                // members are all configurable, so `delete C.x` must drop the key
                // from whichever holds it (a get/set pair shares one key).
                let mut removed = c.statics.remove(key);
                let gl = c.static_getters.len();
                c.static_getters.retain(|(n, _)| n != key);
                let sl = c.static_setters.len();
                c.static_setters.retain(|(n, _)| n != key);
                removed |= c.static_getters.len() != gl || c.static_setters.len() != sl;
                removed
            }
            // A function's assigned own property (`delete fn.x`).
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                self.fn_props.get_mut(&idx).map_or(false, |m| m.remove(key))
            }
            // Every other exotic heap kind (TypedArray, DataView, ArrayBuffer,
            // Map, Set, Date, RegExp, Promise, Boxed, ...) keeps its named/symbol
            // own properties in arr_props — where define_property put them.
            _ => self.arr_props.get_mut(&idx).map_or(false, |m| m.remove(key)),
        };
        if removed {
            self.heap.bump_version(idx); // a key was removed → slots shifted (IC)
        }
        Value::bool(true)
    }

    /// A [[Set]] that the receiver's descriptors rejected — a setter-less accessor,
    /// a non-writable data property, or a new property on a non-extensible object.
    /// Sloppy code ignores it (a no-op); strict-mode assignment throws a TypeError.
    pub(crate) fn reject_write(&self, key: &str, strict: bool) -> Result<(), Thrown> {
        if strict {
            return Err(Thrown(format!(
                "TypeError: Cannot assign to read only property '{key}' of object"
            )));
        }
        Ok(())
    }

    /// A Proxy's `[[Set]]` for `Reflect.set`, surfacing the trap's BOOLEAN result
    /// (which a plain assignment via `set_prop` swallows in non-strict mode).
    /// `Some(b)` when `obj` is a proxy with a `set` trap — `b` is the trap's
    /// truthiness after the [[Set]] invariants (a violation throws). `None` when
    /// `obj` is not a proxy or has no `set` trap (the caller forwards via its
    /// ordinary [[Set]] path). A revoked proxy throws.
    pub(crate) fn proxy_set_bool(
        &mut self,
        obj: Value,
        key: &str,
        val: Value,
        receiver: Value,
    ) -> Result<Option<bool>, Thrown> {
        let (target, handler, revoked) = match self.proxy_parts(obj.heap_index()) {
            Some(p) => p,
            None => return Ok(None),
        };
        if revoked {
            return Err(Thrown("TypeError: Cannot perform 'set' on a revoked proxy".into()));
        }
        let trap = match self.proxy_trap(handler, "set")? {
            Some(t) => t,
            None => return Ok(None),
        };
        let kv = self.key_to_value(key);
        let r = self.call_value(trap, handler, &[target, kv, val, receiver])?;
        if !self.truthy(r) {
            return Ok(Some(false));
        }
        // Same post-invariants as set_prop's proxy branch: a non-configurable,
        // non-writable target data property can't be reported set to a different
        // value; a non-configurable accessor with no setter can't be set at all.
        if let Some((is_data, value, writable, _, has_set)) = self.proxy_target_desc(target, key)? {
            if is_data && !writable && !self.same_value(val, value) {
                return Err(Thrown(format!(
                    "TypeError: 'set' on proxy: trap returned truish for property '{key}' which exists in the proxy target as a non-configurable and non-writable data property with a different value"
                )));
            }
            if !is_data && !has_set {
                return Err(Thrown(format!(
                    "TypeError: 'set' on proxy: trap returned truish for property '{key}' which exists in the proxy target as a non-configurable accessor property without a setter"
                )));
            }
        }
        Ok(Some(true))
    }

    /// OrdinarySet(target, P, V, Receiver) where the receiver is a PROXY (the
    /// no-set-trap forward): the governing descriptor comes from the TARGET,
    /// but the write lands through the RECEIVER — its own descriptor is read
    /// ([[GetOwnProperty]] → GOPD trap) and the value is defined on it
    /// ([[DefineOwnProperty]] → defineProperty trap), per
    /// OrdinarySetWithOwnDescriptor steps 2-3.
    fn ordinary_set_with_proxy_receiver(
        &mut self,
        target: Value,
        receiver: Value,
        key: &str,
        val: Value,
        strict: bool,
    ) -> Result<(), Thrown> {
        // With NO observable trap on the receiver's handler (no set / gopd /
        // defineProperty, and the handler isn't itself a proxy), the spec
        // receiver-define sequence is indistinguishable from a direct forward
        // — which also handles exotic targets (a function's `prototype`, a
        // RegExp's `lastIndex`) whose writes aren't expressible as defines.
        let observable = match self.proxy_parts(receiver.heap_index()) {
            Some((_, h, _)) if h.is_heap() => match self.heap.get(h.heap_index()) {
                HeapObj::Object(m) => {
                    // An explicitly-undefined/null trap is "no trap" per spec.
                    let live = |k: &str| m.get(k).is_some_and(|v| !v.is_nullish());
                    live("set") || live("getOwnPropertyDescriptor") || live("defineProperty")
                }
                _ => true,
            },
            _ => true,
        };
        if !observable {
            return self.set_prop(target, key, val, strict);
        }
        // The target may ITSELF be a proxy: [[Set]] forwards through its set
        // trap (with the ORIGINAL receiver) or recurses to its target.
        if let Some((t2, h2, revoked2)) = self.proxy_parts(target.heap_index()) {
            if revoked2 {
                return Err(Thrown("TypeError: Cannot perform 'set' on a revoked proxy".into()));
            }
            return match self.proxy_trap(h2, "set")? {
                Some(trap) => {
                    let kv = self.key_to_value(key);
                    let r = self.call_value(trap, h2, &[t2, kv, val, receiver])?;
                    if !self.truthy(r) && strict {
                        return Err(Thrown(format!(
                            "TypeError: 'set' on proxy: trap returned falsish for property '{key}'"
                        )));
                    }
                    Ok(())
                }
                None => self.ordinary_set_with_proxy_receiver(t2, receiver, key, val, strict),
            };
        }
        // Walk the TARGET's prototype chain for the governing descriptor
        // (mirrors Reflect.set): ordinary Object links carry inline
        // descriptors; a TypedArray in the chain ABSORBS canonical numeric
        // keys (an invalid index returns true with NO receiver define).
        let mut governing: Option<(bool, bool, Value)> = None;
        let mut cur = target;
        loop {
            let cidx = cur.heap_index();
            match self.heap.get(cidx) {
                HeapObj::Object(m) => {
                    if let Some(i) = m.pos(key) {
                        governing = Some((
                            m.attrs[i].accessor,
                            m.attrs[i].writable,
                            m.attrs[i].setter,
                        ));
                        break;
                    }
                    if m.class.is_some() {
                        break;
                    }
                }
                HeapObj::Array(_) if key == "length" => {
                    governing = Some((
                        false,
                        !self.array_length_nonwritable.contains(&cidx),
                        Value::UNDEFINED,
                    ));
                    break;
                }
                HeapObj::TypedArray { .. } if self.is_canonical_numeric_index(key) => {
                    // TA [[Set]] step 1 with Receiver != O: an INVALID integer
                    // index returns true — no coercion, no receiver define.
                    if self.ta_valid_index(cidx, key).is_none() {
                        return Ok(());
                    }
                    break;
                }
                _ => {
                    governing = self.arr_props.get(&cidx).and_then(|m| {
                        m.pos(key).map(|i| {
                            (m.attrs[i].accessor, m.attrs[i].writable, m.attrs[i].setter)
                        })
                    });
                    break;
                }
            }
            let p = self.object_get_prototype_of(cur);
            if !p.is_heap() {
                break;
            }
            cur = p;
        }
        match governing {
            // A target accessor: its setter runs with the RECEIVER as `this`.
            Some((true, _, setter)) => {
                if setter == Value::UNDEFINED {
                    return self.reject_write(key, strict);
                }
                self.call_value(setter, receiver, &[val])?;
                Ok(())
            }
            Some((false, false, _)) => self.reject_write(key, strict),
            // Writable data, or no own target descriptor: write through the
            // receiver — same shape as Reflect.set's proxy-receiver path.
            _ => {
                let existing = self.proxy_gopd(receiver, key)?.unwrap_or(Value::UNDEFINED);
                if self.is_object_value(existing) {
                    let g = self.get_prop(existing, "get")?;
                    let s = self.get_prop(existing, "set")?;
                    let w = self.get_prop(existing, "writable")?;
                    if g != Value::UNDEFINED || s != Value::UNDEFINED || !self.truthy(w) {
                        return self.reject_write(key, strict);
                    }
                    let mut m = ObjMap::new();
                    m.set("value", val);
                    let desc = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                    self.object_define_property(receiver, key, desc)
                } else {
                    let mut m = ObjMap::new();
                    m.set("value", val);
                    m.set("writable", Value::TRUE);
                    m.set("enumerable", Value::TRUE);
                    m.set("configurable", Value::TRUE);
                    let desc = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                    self.object_define_property(receiver, key, desc)
                }
            }
        }
    }

    pub(crate) fn set_prop(
        &mut self,
        obj: Value,
        key: &str,
        val: Value,
        strict: bool,
    ) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        // A plain assignment can put an integer index on Array/Object.prototype
        // just like defineProperty — flag it so index stores/mutators consult
        // the prototype chain (two integer compares for ordinary receivers).
        self.note_array_proto_index(obj.heap_index(), key);
        // %Array.prototype%.length = n: ArraySetLength on the exotic prototype
        // (double coercion, RangeError on non-uint32, truncation deletes its
        // own integer-index properties >= n).
        if key == "length" && obj.heap_index() == self.arr_proto && self.arr_proto != 0 {
            let nu = self.to_number_coerce(val)?;
            let u = if nu.is_finite() { (nu.trunc() as i64 as u32) as f64 } else { 0.0 };
            let number_len = self.to_number_coerce(val)?;
            if u != number_len {
                return Err(Thrown("RangeError: Invalid array length".into()));
            }
            let n = u as u32;
            if let HeapObj::Object(m) = self.heap.get_mut(self.arr_proto) {
                let doomed: Vec<String> = m
                    .keys
                    .iter()
                    .filter(|k| canonical_index_str(k).is_some_and(|i| i >= n as usize))
                    .cloned()
                    .collect();
                for k in doomed {
                    m.remove(&k);
                }
            }
            self.arr_proto_len = n;
            return Ok(());
        }
        // Proxy `set` trap (or fall through to the target).
        if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'set' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "set")? {
                Some(trap) => {
                    let kv = self.key_to_value(key);
                    let r = self.call_value(trap, handler, &[target, kv, val, obj])?;
                    if !self.truthy(r) {
                        if strict {
                            return Err(Thrown(format!(
                                "TypeError: 'set' on proxy: trap returned falsish for property '{key}'"
                            )));
                        }
                        return Ok(());
                    }
                    // Invariant: a non-configurable, non-writable target data
                    // property can't be reported set to a different value; a
                    // non-configurable accessor with no setter can't be set at all.
                    if let Some((is_data, value, writable, _, has_set)) =
                        self.proxy_target_desc(target, key)?
                    {
                        if is_data && !writable && !self.same_value(val, value) {
                            return Err(Thrown(format!(
                                "TypeError: 'set' on proxy: trap returned truish for property '{key}' which exists in the proxy target as a non-configurable and non-writable data property with a different value"
                            )));
                        }
                        if !is_data && !has_set {
                            return Err(Thrown(format!(
                                "TypeError: 'set' on proxy: trap returned truish for property '{key}' which exists in the proxy target as a non-configurable accessor property without a setter"
                            )));
                        }
                    }
                    Ok(())
                }
                // No set trap: target.[[Set]](P, V, Receiver = THE PROXY).
                // OrdinarySetWithOwnDescriptor consults the RECEIVER's own
                // descriptor and defines through it, so the proxy's
                // getOwnPropertyDescriptor and defineProperty traps fire.
                None => self.ordinary_set_with_proxy_receiver(target, obj, key, val, strict),
            };
        }
        let idx = obj.heap_index();
        // A Module Namespace exotic: [[Set]] ALWAYS returns false — strict
        // assignment TypeError, sloppy no-op, even for the SAME value.
        if self.module_namespaces.contains_key(&idx) {
            return self.reject_write(key, strict);
        }
        // A Symbol PRIMITIVE receiver: PutValue boxes it, but [[Set]]'s receiver is
        // the primitive itself, so the write can't create a property — a strict
        // TypeError, a sloppy no-op. (A boxed wrapper Object(sym) is a real object
        // and takes the normal path below.)
        if matches!(self.heap.get(idx), HeapObj::Symbol { .. }) {
            return self.reject_write(key, strict);
        }
        // `undefined` / `NaN` / `Infinity` on the global object are non-writable,
        // non-configurable data properties: assigning them is a sloppy no-op and a
        // strict TypeError — never a shadowing own property.
        if self.global_this != 0
            && idx == self.global_this
            && matches!(key, "undefined" | "NaN" | "Infinity")
        {
            return self.reject_write(key, strict);
        }
        // A String exotic — a `new String("ab")` wrapper OR a raw primitive string
        // value used as a receiver (`Array.prototype.shift.call("abc")` after
        // ToObject) — has `length` and its in-range char indices as non-writable,
        // non-configurable own data props, so assigning them is a sloppy no-op / a
        // strict TypeError. Other keys (`s.foo = 1`) fall through to the ordinary
        // named-property path below.
        if key != "__proto__" {
            if let Some((_, slen)) = self.string_exotic_chars(obj) {
                let blocked =
                    key == "length" || canonical_index_str(key).map(|i| i < slen).unwrap_or(false);
                if blocked {
                    return self.reject_write(key, strict);
                }
            }
        }
        // `o.__proto__ = v` invokes the inherited Object.prototype.__proto__
        // setter: an object/null value runs [[SetPrototypeOf]] (the setter throws a
        // TypeError if it is rejected — a non-extensible target, a cycle, or the
        // immutable %Object.prototype% — regardless of the assignment's strictness,
        // since the throw originates inside the accessor); a primitive value is a
        // silent no-op. Shared with Object.setPrototypeOf / Reflect.setPrototypeOf.
        if key == "__proto__" {
            if (self.is_object_value(val) || val == Value::NULL)
                && !self.ordinary_set_prototype_of(obj, val)?
            {
                return Err(Thrown(
                    "TypeError: cannot set prototype (target is non-extensible, the change is cyclic, or it has an immutable prototype)".into(),
                ));
            }
            return Ok(());
        }
        // `re.lastIndex = n` — a RegExp's one writable data property by default.
        if key == "lastIndex" && matches!(self.heap.get(idx), HeapObj::RegExp { .. }) {
            // `Object.defineProperty` can make it non-writable (recorded in
            // arr_props): then `Set(R,"lastIndex",..,true)` throws (strict) / is a
            // silent no-op (sloppy), per OrdinarySet — without touching the slot.
            let writable = self
                .arr_props
                .get(&idx)
                .map_or(true, |m| m.pos("lastIndex").map_or(true, |i| m.attrs[i].writable));
            if !writable {
                return self.reject_write(key, strict);
            }
            // Store the assigned Value AS-IS — ToLength is applied later by `exec`
            // / the @@-methods, invoking a user `valueOf`/`toString` then, not now.
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(idx) {
                *last_index = val;
            }
            return Ok(());
        }
        // `arr.length = n` truncates (n < len) or extends-with-holes (n > len) a
        // dense array — a very common idiom (`arr.length = 0` clears it). Per JS,
        // n must be a non-negative integer < 2^32, else a RangeError.
        if key == "length" && matches!(self.heap.get(idx), HeapObj::Array(_)) {
            // ArraySetLength steps 2-5: newLen = ToUint32(value) AND
            // numberLen = ToNumber(value) are BOTH computed (valueOf runs
            // TWICE), and newLen != numberLen (a non-uint32 length like -1 /
            // NaN / 1.5 / 2^32) is a RangeError — same model as the
            // defineProperty path in props.rs.
            let nu = self.to_number_coerce(val)?; // ToNumber inside ToUint32
            let u = if nu.is_finite() { (nu.trunc() as i64 as u32) as f64 } else { 0.0 };
            let number_len = self.to_number_coerce(val)?; // numberLen
            if u != number_len {
                return Err(Thrown("RangeError: Invalid array length".into()));
            }
            let n = u;
            // A `defineProperty`'d non-writable `length`, or a frozen array (freeze
            // makes `length` non-writable), rejects assignment (sloppy no-op / strict
            // TypeError) — the ToNumber/RangeError coercion above still runs first,
            // per OrdinarySet.
            // A frozen array's `length` is non-writable (freeze records it in
            // array_length_nonwritable); a merely sealed array keeps a writable
            // length, so use the explicit `frozen` flag, not the vacuous is_frozen().
            if self.array_length_nonwritable.contains(&idx)
                || self.arr_props.get(&idx).is_some_and(|m| m.frozen)
            {
                return self.reject_write("length", strict);
            }
            if n as usize > crate::vm::MAX_DENSE_ARRAY_LEN {
                return Err(Thrown(
                    "RangeError: array length exceeds the engine's dense-array limit".into(),
                ));
            }
            // Truncation deletes index properties >= newLen in DESCENDING
            // order, stopping at the first NON-CONFIGURABLE one: it survives
            // and the final length becomes thatIndex + 1 (ArraySetLength).
            let mut final_len = n as usize;
            if let Some(m) = self.arr_props.get(&idx) {
                let highest_noncfg = m
                    .keys
                    .iter()
                    .enumerate()
                    .filter_map(|(i, k)| {
                        canonical_index_str(k)
                            .filter(|ki| *ki >= n as usize && !m.attrs[i].configurable)
                    })
                    .max();
                if let Some(ki) = highest_noncfg {
                    final_len = ki + 1;
                }
            }
            if let Some(m) = self.arr_props.get_mut(&idx) {
                let doomed: Vec<String> = m
                    .keys
                    .iter()
                    .filter(|k| canonical_index_str(k).is_some_and(|i| i >= final_len))
                    .cloned()
                    .collect();
                for k in doomed {
                    m.remove(&k);
                }
            }
            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                // Extending past the current length adds HOLES (absent elements),
                // not present `undefined`s; truncating just drops the tail.
                items.resize(final_len, Value::HOLE);
            }
            self.heap.bump_version(idx);
            return Ok(());
        }
        // A callable's `name`/`length` are non-writable: assignment is a sloppy
        // no-op while the synthesized intrinsic is present. (Once `delete`d it
        // falls through and becomes an ordinary assigned property.)
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            return self.reject_write(key, strict);
        }
        // An OWN property's descriptor governs assignment: an accessor invokes its
        // setter; a non-writable data property silently ignores the write (sloppy).
        let own_attr = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| m.attrs[i]),
            // An Array's named props — and an exotic object's defineProperty'd own
            // props (Map/Set/Date/Promise/…) — live in the arr_props side table.
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) | HeapObj::Class(_) => None,
            _ => self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| m.attrs[i])),
        };
        if let Some(a) = own_attr {
            if a.accessor {
                if a.setter != Value::UNDEFINED {
                    self.call_value(a.setter, obj, &[val])?;
                    return Ok(()); // setter invoked ⇒ the write succeeds
                }
                return self.reject_write(key, strict); // accessor with no setter
            }
            if !a.writable {
                return self.reject_write(key, strict); // non-writable own data property
            }
            // writable own data property → fall through to overwrite its value.
        }
        // An inherited accessor on the prototype-OBJECT chain (`Object.create`
        // proto, `fn.prototype` via defineProperty) governs the write: invoke its
        // setter, or sloppy no-op for a getter-only accessor. (Class-instance
        // chains are handled just below via map.class; `__proto__` was handled
        // at the top.) Only when there's no own property with this key.
        let needs_proto_walk = (match self.heap.get(idx) {
            HeapObj::Object(m) => m.class.is_none() && m.pos(key).is_none(),
            // An EXOTIC receiver's inherited getter-only accessor (RegExp
            // global/source/flags, Map/Set size) must govern the write: assigning it
            // is a sloppy no-op / strict TypeError, NOT a new own data property. The
            // `re.lastIndex` data property is handled (and returns) above this point.
            HeapObj::RegExp { .. }
            | HeapObj::Map { .. }
            | HeapObj::Set(_)
            | HeapObj::WeakMap { .. }
            | HeapObj::WeakSet(_)
            | HeapObj::WeakRef(_)
            | HeapObj::FinalizationRegistry { .. }
            | HeapObj::Date(_)
            | HeapObj::Promise { .. }
            | HeapObj::Boxed { .. }
            | HeapObj::ArrayBuffer { .. }
            | HeapObj::DataView { .. } => true,
            // NOTE: Array is deliberately NOT here — several internal array
            // operations (concat result-building, arguments mapping) use
            // [[DefineOwnProperty]] semantics through this path and must create an
            // own property, not invoke an inherited Array.prototype setter.
            _ => false,
        } && self.arr_props.get(&idx).map_or(true, |m| m.pos(key).is_none()))
        // A function / bound function / native: an inherited accessor on
        // Function.prototype governs a write that is not an own property (own
        // function props live in fn_props, so check via has_own_property).
        || (matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
        ) && !self.has_own_property(obj, key));
        if needs_proto_walk {
            match self.proto_chain_set(idx, key, val, obj)? {
                ProtoSet::Setter(setter) => {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
                ProtoSet::GetterOnly => return self.reject_write(key, strict),
                ProtoSet::Proxy(true) => return Ok(()), // chain proxy's set trap handled it
                ProtoSet::Absorbed => return Ok(()), // TA chain node absorbed the index
                ProtoSet::Proxy(false) => return self.reject_write(key, strict),
                ProtoSet::NonWritable => return self.reject_write(key, strict),
                ProtoSet::DataWrite => {} // no inherited accessor/proxy ⇒ own-data write
            }
        }
        // A class instance with an inherited `set x(v)` accessor: assigning a
        // property that is NOT an own data property invokes the setter (own data
        // properties shadow an inherited accessor, per JS [[Set]]).
        if let HeapObj::Object(map) = self.heap.get(idx) {
            if map.class.is_some() && map.get(key).is_none() {
                if let Some(setter) = self.lookup_setter(map.class, key) {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
                // A PRIVATE method or getter-only accessor is not assignable:
                // `this.#m = v` / `this.#g = v` (incl. compound assignment) throws
                // TypeError. Gated on a private key — a public method is a writable
                // prototype data property and stays shadowable, and a private FIELD
                // is an own data property (so map.get(key) is Some and this branch
                // is skipped, leaving it writable).
                if is_private_key(key)
                    && self.lookup_instance_method_or_getter(map.class, key)
                {
                    return Err(Thrown(format!(
                        "TypeError: Cannot write to private member '{key}': it is a method or a getter-only accessor"
                    )));
                }
            }
        }
        // A function value's own property (`fn.x = …`, e.g. `assert.sameValue`)
        // lives in a side table (functions carry no inline property map).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
        ) {
            // `caller`/`arguments` on a STRICT or BOUND function are the inherited
            // %ThrowTypeError% accessors — assigning either throws, mirroring the
            // read poison (props.rs). A sloppy function keeps its ordinary write.
            if key == "caller" || key == "arguments" {
                let restricted = match self.heap.get(idx) {
                    HeapObj::Bound { .. } => true,
                    HeapObj::Func(fid) => self.func(*fid as usize).is_strict,
                    HeapObj::Closure { func, .. } => self.func(*func as usize).is_strict,
                    _ => false,
                };
                if restricted {
                    return Err(Thrown(format!(
                        "TypeError: '{key}' may not be assigned on strict-mode or bound functions"
                    )));
                }
            }
            // Reassigning `fn.prototype = value` redirects what `new fn()` / the
            // `.prototype` getter see. ANY value is honoured (incl. a non-object —
            // undefined/null/primitive — which the heap-only `prototypes` cache can't
            // hold), via `fn_proto_override` which the reads consult first.
            if key == "prototype" {
                self.fn_proto_override.insert(idx, val);
                if val.is_heap() {
                    self.prototypes.insert(idx, val.heap_index());
                }
            } else {
                // An existing NON-WRITABLE own data property (e.g. a function `name`
                // set by NamedEvaluation/SetFunctionName) rejects assignment.
                if let Some(m) = self.fn_props.get(&idx) {
                    if let Some(i) = m.pos(key) {
                        if !m.attrs[i].accessor && !m.attrs[i].writable {
                            return self.reject_write(key, strict);
                        }
                    }
                }
                // A NEW own property on a non-extensible function is rejected (the
                // extensibility flag lives in the arr_props side table). The intrinsic
                // name/length/prototype already exist, so they are not "new".
                let present = matches!(key, "name" | "length" | "prototype")
                    || self.fn_props.get(&idx).map_or(false, |m| m.pos(key).is_some());
                if !present && self.arr_props.get(&idx).map_or(false, |m| !m.extensible) {
                    return self.reject_write(key, strict);
                }
                self.fn_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
            }
            return Ok(());
        }
        // A `static set name(v)` (or getter-only accessor) on the class chain
        // intercepts the write before it becomes a static data property.
        if matches!(self.heap.get(idx), HeapObj::Class(_)) {
            match self.lookup_static_accessor(Some(idx), key) {
                Some(Some(setter)) => {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
                Some(None) => return self.reject_write(key, strict), // getter-only
                None => {
                    // Restricted properties: with no OWN static accessor of that
                    // name (a class may legitimately declare `static set caller`),
                    // assignment to caller/arguments hits the inherited
                    // %ThrowTypeError% setter — classes are strict functions.
                    if key == "caller" || key == "arguments" {
                        return Err(Thrown(format!(
                            "TypeError: '{key}' may not be assigned on a class constructor"
                        )));
                    }
                }
            }
        }
        // An Array's named (non-index) own property — `arr.foo = 1`, a match
        // result's `index`/`input`/`groups` — lives in the arr_props side table
        // (numeric indices + `length` were handled above). Mirrors fn_props.
        if matches!(self.heap.get(idx), HeapObj::Array(_)) {
            // A canonical numeric-string key is an array INDEX (`arr["0"] = v` is
            // `arr[0] = v`) — write to the dense elements, extending with holes.
            // (A huge index past the dense limit, or a non-canonical key, falls
            // through to the arr_props side table as a named property.)
            if let Ok(n) = key.parse::<usize>() {
                if n.to_string() == key && n < crate::vm::MAX_DENSE_ARRAY_LEN {
                    // A special (defineProperty'd) index lives in arr_props and
                    // overrides the dense slot. Its accessor / non-writable cases
                    // were already handled by the own_attr block above, so only a
                    // writable special data index reaches here — update it in place
                    // (preserving its attributes).
                    if self.array_index_override(idx, n).is_some() {
                        self.arr_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
                        self.heap.bump_version(idx);
                        return Ok(());
                    }
                    // A NEW index (past the current length) on a non-extensible array
                    // adds an own property → rejected (sloppy no-op / strict TypeError).
                    // An in-range index is already present and stays writable.
                    let present =
                        matches!(self.heap.get(idx), HeapObj::Array(items) if n < items.len());
                    // Extending past the current length grows `length`; a non-writable
                    // `length` (defineProperty / freeze) rejects that — Array
                    // [[DefineOwnProperty]]: index >= oldLen && length non-writable → false.
                    if !present
                        && (self.arr_props.get(&idx).map_or(false, |m| !m.extensible)
                            || self.array_length_nonwritable.contains(&idx))
                    {
                        return self.reject_write(key, strict);
                    }
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        if n >= items.len() {
                            // Intervening slots are HOLES (absent), per spec.
                            items.resize(n + 1, Value::HOLE);
                        }
                        items[n] = val;
                    }
                    self.heap.bump_version(idx);
                    return Ok(());
                }
            }
            // A NEW named own prop on a non-extensible array is rejected.
            if self
                .arr_props
                .get(&idx)
                .map_or(false, |m| !m.extensible && m.pos(key).is_none())
            {
                return self.reject_write(key, strict);
            }
            let added = self.arr_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
            if added {
                self.heap.bump_version(idx);
            }
            return Ok(());
        }
        // A TypedArray's extra NAMED own property (`ta.constructor = {}`, used by
        // species lookup) lives in the arr_props side table; a canonical numeric
        // index still writes to the buffer (or is ignored when out of bounds).
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            // Any CanonicalNumericIndexString is absorbed by the integer-indexed
            // exotic [[Set]]: a valid index writes the element; an invalid one
            // (non-integer, "-0", out of range, detached view) still runs the
            // value coercion then drops the write — it is NEVER a named prop.
            if self.is_canonical_numeric_index(key) {
                match self.ta_valid_index(idx, key) {
                    Some(n) => self.ta_element_set(idx, n, val)?,
                    None => self.ta_coerce_for_set(idx, val)?,
                }
                return Ok(());
            }
            // A NEW named own prop on a non-extensible TypedArray is rejected (its
            // integer indices are exotic and handled above, so this is named-only).
            if self
                .arr_props
                .get(&idx)
                .map_or(false, |m| !m.extensible && m.pos(key).is_none())
            {
                return self.reject_write(key, strict);
            }
            let added = self.arr_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
            if added {
                self.heap.bump_version(idx);
            }
            return Ok(());
        }
        // An exotic object's extra own property (`mapInst.x = 1`) lives in the same
        // arr_props side table that get_member / defineProperty use for it, so the
        // write is readable (parity with the defineProperty path).
        if matches!(
            self.heap.get(idx),
            HeapObj::Map { .. }
                | HeapObj::Set(_)
                | HeapObj::WeakMap { .. }
                | HeapObj::WeakSet(_)
                | HeapObj::WeakRef(_)
                | HeapObj::FinalizationRegistry { .. }
                | HeapObj::Date(_)
                | HeapObj::Promise { .. }
                | HeapObj::Boxed { .. }
                | HeapObj::RegExp { .. }
                | HeapObj::ArrayBuffer { .. }
                | HeapObj::DataView { .. }
        ) {
            // (`re.lastIndex = …` was handled above; a `re.exec = fn` override or
            // any `re.x = …` lands in the side table. RegExp accessor keys
            // source/flags/… are read back via regexp_get_prop, not from here.)
            // A non-extensible exotic object rejects a NEW own property (sloppy
            // no-op), mirroring the plain-object arm below — its extensibility lives
            // in the arr_props side table's flag (set by Object.preventExtensions).
            if let Some(m) = self.arr_props.get(&idx) {
                if m.pos(key).is_none() && !m.extensible {
                    return self.reject_write(key, strict);
                }
            }
            let added = self.arr_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
            if added {
                self.heap.bump_version(idx);
            }
            return Ok(());
        }
        let mut added = false;
        match self.heap.get_mut(idx) {
            HeapObj::Object(map) => {
                // A non-extensible object rejects NEW own properties (sloppy no-op);
                // existing writable data properties still accept writes.
                if map.pos(key).is_none() && !map.extensible {
                    return self.reject_write(key, strict);
                }
                added = map.set(key, val);
            }
            // Static-member assignment on a class value (`C.x = …`).
            HeapObj::Class(c) => {
                c.statics.set(key, val);
            }
            _ => {}
        }
        if added {
            self.heap.bump_version(idx); // invalidate any JIT inline cache (vals realloc)
        }
        Ok(())
    }

    /// Walk `start_idx`'s prototype-object chain for an own accessor named `key`,
    /// for the [[Set]] algorithm. Returns:
    /// * `Some(Some(setter))` — an accessor with a setter (invoke it);
    /// * `Some(None)` — a getter-only accessor (assignment is a sloppy no-op);
    /// * `None` — no accessor reached (a data property shadows / the chain ends),
    ///   so the caller writes an own data property.
    fn proto_chain_set(&mut self, start_idx: u32, key: &str, val: Value, receiver: Value) -> Result<ProtoSet, Thrown> {
        let mut cur = self.object_get_prototype_of(Value::heap(start_idx));
        for _ in 0..1000 {
            if !cur.is_heap() {
                return Ok(ProtoSet::DataWrite);
            }
            let cidx = cur.heap_index();
            // A Proxy in the chain delegates the write to its [[Set]] with the
            // ORIGINAL receiver (OrdinarySet step: parent.[[Set]](P,V,Receiver)). A
            // `set` trap fires; with no trap it forwards to the target's [[Set]] —
            // continue the walk from the target with the same receiver.
            if self.proxy_parts(cidx).is_some() {
                match self.proxy_set_bool(cur, key, val, receiver)? {
                    Some(ok) => return Ok(ProtoSet::Proxy(ok)),
                    None => {
                        let target = self.proxy_parts(cidx).map(|(t, _, _)| t).unwrap_or(Value::NULL);
                        cur = target;
                        continue;
                    }
                }
            }
            // A TypedArray chain node ABSORBS canonical numeric keys: its [[Set]]
            // (with Receiver ≠ the TA) returns true for an invalid index — no
            // write, no coercion, prototype beyond it never consulted — while a
            // VALID index is a writable data prop, so OrdinarySet continues with
            // an own write on the receiver.
            if matches!(self.heap.get(cidx), HeapObj::TypedArray { .. })
                && self.is_canonical_numeric_index(key)
            {
                return Ok(if self.ta_valid_index(cidx, key).is_some() {
                    ProtoSet::DataWrite
                } else {
                    ProtoSet::Absorbed
                });
            }
            if let HeapObj::Object(m) = self.heap.get(cidx) {
                if let Some(i) = m.pos(key) {
                    let a = m.attrs[i];
                    if a.accessor {
                        return Ok(if a.setter != Value::UNDEFINED {
                            ProtoSet::Setter(a.setter)
                        } else {
                            ProtoSet::GetterOnly
                        });
                    }
                    // An inherited WRITABLE data property is shadowed by an own
                    // write; a NON-WRITABLE one rejects the write (OrdinarySet).
                    return Ok(if a.writable {
                        ProtoSet::DataWrite
                    } else {
                        ProtoSet::NonWritable
                    });
                }
            }
            cur = self.object_get_prototype_of(cur);
        }
        Ok(ProtoSet::DataWrite)
    }

    /// Install an object-literal accessor (`{ get k(){…} }` / `{ set k(v){…} }`)
    /// on a plain object, merging with an existing accessor for the same key (so a
    /// get+set pair becomes one get/set accessor). Object-literal accessors are
    /// enumerable + configurable. A getter is stored in `vals[i]`, a setter in
    /// `attrs[i].setter`.
    pub(crate) fn define_object_accessor(&mut self, obj: Value, key: &str, func: Value, is_setter: bool) {
        if !obj.is_heap() {
            return;
        }
        let idx = obj.heap_index();
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            if let Some(i) = m.pos(key) {
                if m.attrs[i].accessor {
                    if is_setter {
                        m.attrs[i].setter = func;
                    } else {
                        m.vals[i] = func;
                    }
                    return;
                }
            }
            let (getter, setter) = if is_setter {
                (Value::UNDEFINED, func)
            } else {
                (func, Value::UNDEFINED)
            };
            let attr = PropAttr {
                writable: false,
                enumerable: true,
                configurable: true,
                accessor: true,
                setter,
            };
            m.define(key, getter, attr);
            self.heap.bump_version(idx);
        }
    }

    /// Walk a class chain for a `set key(v)` accessor, returning the setter fn.
    pub(crate) fn lookup_setter(&self, class: Option<u32>, key: &str) -> Option<Value> {
        let mut cur = class;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if let Some((_, v)) = c.setters.iter().find(|(k, _)| k == key) {
                        return Some(*v);
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        None
    }

    /// True if `key` resolves to an instance METHOD or GETTER anywhere on the
    /// class chain. Used (with `lookup_setter` having already returned None) to
    /// reject a write to a private method / getter-only accessor.
    pub(crate) fn lookup_instance_method_or_getter(&self, class: Option<u32>, key: &str) -> bool {
        let mut cur = class;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if c.methods.iter().any(|(k, _)| k == key)
                        || c.getters.iter().any(|(k, _)| k == key)
                    {
                        return true;
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        false
    }

    /// Resolve a static-property write against the class chain starting at heap
    /// index `start`. The first chain level that owns the key decides:
    ///   `Some(Some(setter))` → invoke `setter`;
    ///   `Some(None)`         → a getter-only accessor shadows the write (no-op);
    ///   `None`               → no accessor shadows it → write a static data prop.
    pub(crate) fn lookup_static_accessor(&self, start: Option<u32>, key: &str) -> Option<Option<Value>> {
        let mut cur = start;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if let Some((_, s)) = c.static_setters.iter().find(|(k, _)| k == key) {
                        return Some(Some(*s));
                    }
                    if c.static_getters.iter().any(|(k, _)| k == key) {
                        return Some(None); // accessor with no setter ⇒ sloppy no-op
                    }
                    if c.statics.get(key).is_some() {
                        return None; // own data property shadows inherited accessors
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        None
    }

}
