#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

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
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "string",
                // A class is callable (with `new`), so `typeof C === "function"`.
                // Native builtins and bound functions are callable too.
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Native(_)
                | HeapObj::Bound { .. }
                | HeapObj::BoundResolver { .. } => "function",
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
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'deleteProperty' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "deleteProperty")? {
                    Some(trap) => {
                        let kv = self.key_to_value(key);
                        let r = self.call_value(trap, handler, &[target, kv])?;
                        Ok(Value::bool(self.truthy(r)))
                    }
                    None => Ok(self.delete_prop(target, key)),
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
        // A callable's `name`/`length` are configurable: record the deletion so
        // the synthesized property stops appearing (own-property queries + reads).
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            self.deleted_callable_intrinsics
                .insert((idx, if key == "name" { 0 } else { 1 }));
            return Value::bool(true);
        }
        let removed = match self.heap.get_mut(idx) {
            HeapObj::Object(map) => map.remove(key),
            HeapObj::Array(items) => {
                if let Ok(i) = key.parse::<usize>() {
                    if i < items.len() {
                        items[i] = Value::UNDEFINED;
                    }
                    false // array slot stays (a hole); no version bump needed
                } else {
                    // A named (non-index) own property in arr_props.
                    self.arr_props.get_mut(&idx).map_or(false, |m| m.remove(key))
                }
            }
            HeapObj::Class(c) => c.statics.remove(key),
            // A function's assigned own property (`delete fn.x`).
            _ => self.fn_props.get_mut(&idx).map_or(false, |m| m.remove(key)),
        };
        if removed {
            self.heap.bump_version(idx); // a key was removed → slots shifted (IC)
        }
        Value::bool(true)
    }

    pub(crate) fn set_prop(&mut self, obj: Value, key: &str, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        // Proxy `set` trap (or fall through to the target).
        if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'set' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "set")? {
                Some(trap) => {
                    let kv = self.key_to_value(key);
                    self.call_value(trap, handler, &[target, kv, val, obj])?;
                    Ok(())
                }
                None => self.set_prop(target, key, val),
            };
        }
        let idx = obj.heap_index();
        // `o.__proto__ = v` invokes the inherited Object.prototype.__proto__
        // setter: set [[Prototype]] when v is an object or null, else a silent
        // no-op (a primitive value). Mirrors Object.setPrototypeOf; the getter
        // side already works via the inherited accessor.
        if key == "__proto__" {
            if self.is_object_value(val) || val == Value::NULL {
                self.proto_of.insert(idx, val);
            }
            return Ok(());
        }
        // `re.lastIndex = n` — the only writable own property of a RegExp.
        if key == "lastIndex" && matches!(self.heap.get(idx), HeapObj::RegExp { .. }) {
            let n = self.to_number(val)?;
            let li = if n.is_finite() && n >= 0.0 { n as usize } else { 0 };
            self.set_regexp_last_index(idx, li);
            return Ok(());
        }
        // `arr.length = n` truncates (n < len) or extends-with-holes (n > len) a
        // dense array — a very common idiom (`arr.length = 0` clears it). Per JS,
        // n must be a non-negative integer < 2^32, else a RangeError.
        if key == "length" && matches!(self.heap.get(idx), HeapObj::Array(_)) {
            let n = self.to_number(val)?;
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
            return Ok(());
        }
        // A callable's `name`/`length` are non-writable: assignment is a sloppy
        // no-op while the synthesized intrinsic is present. (Once `delete`d it
        // falls through and becomes an ordinary assigned property.)
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            return Ok(());
        }
        // An OWN property's descriptor governs assignment: an accessor invokes its
        // setter; a non-writable data property silently ignores the write (sloppy).
        let own_attr = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| m.attrs[i]),
            // An Array's named (non-index) own properties live in arr_props.
            HeapObj::Array(_) => {
                self.arr_props.get(&idx).and_then(|m| m.pos(key).map(|i| m.attrs[i]))
            }
            _ => None,
        };
        if let Some(a) = own_attr {
            if a.accessor {
                if a.setter != Value::UNDEFINED {
                    self.call_value(a.setter, obj, &[val])?;
                }
                return Ok(()); // accessor with no setter ⇒ no-op (sloppy)
            }
            if !a.writable {
                return Ok(()); // non-writable own data property ⇒ no-op (sloppy)
            }
            // writable own data property → fall through to overwrite its value.
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
            }
        }
        // A function value's own property (`fn.x = …`, e.g. `assert.sameValue`)
        // lives in a side table (functions carry no inline property map).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
        ) {
            // Reassigning `fn.prototype = obj` redirects what `new fn()` / the
            // `.prototype` getter see (the lazily-cached prototype object).
            if key == "prototype" && val.is_heap() {
                self.prototypes.insert(idx, val.heap_index());
            } else {
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
                Some(None) => return Ok(()), // getter-only ⇒ sloppy no-op
                None => {}                    // fall through to a data write
            }
        }
        // An Array's named (non-index) own property — `arr.foo = 1`, a match
        // result's `index`/`input`/`groups` — lives in the arr_props side table
        // (numeric indices + `length` were handled above). Mirrors fn_props.
        if matches!(self.heap.get(idx), HeapObj::Array(_)) {
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
                    return Ok(());
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
