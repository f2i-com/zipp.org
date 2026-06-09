#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// `key in obj` — does `obj` have the property `key`? Own object keys, a
    /// class instance's inherited methods/getters, array indices / `length`,
    /// Map/Set `size`, and class static members. `in` on a primitive throws
    /// in JS; here it's `false` (rare).
    /// `[[HasProperty]]` for a fixed string key — walks the prototype chain
    /// (unlike has_own_property). Allocates a transient key string; used by
    /// ToPropertyDescriptor, where an inherited/accessor field counts as present.
    pub(crate) fn has_property_str(&mut self, obj: Value, key: &str) -> bool {
        let k = self.alloc_str(key.to_string());
        self.has_property(obj, k)
    }

    pub(crate) fn has_property(&self, obj: Value, key: Value) -> bool {
        if !obj.is_heap() {
            return false;
        }
        let idx = obj.heap_index();
        match self.heap.get(idx) {
            HeapObj::Object(map) => {
                let k = self.key_of(key);
                if map.get(&k).is_some() {
                    return true;
                }
                // Inherited method/getter/setter through the class chain.
                let class = map.class;
                let mut cur = class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.methods.iter().any(|(n, _)| *n == k)
                                || c.getters.iter().any(|(n, _)| *n == k)
                                || c.setters.iter().any(|(n, _)| *n == k)
                            {
                                return true;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                // [[HasProperty]] continues up the prototype chain: an explicit
                // `Object.create` proto, then the base Object.prototype (which
                // carries toString/hasOwnProperty/valueOf/…). Mirrors get_member's
                // proto resolution, minus class-instance C.prototype (its methods
                // are already covered by the class-chain walk above).
                let proto = if let Some(&p) = self.proto_of.get(&idx) {
                    p.is_heap().then_some(p)
                } else if self.obj_proto != 0 && idx != self.obj_proto {
                    Some(Value::heap(self.obj_proto))
                } else {
                    None
                };
                match proto {
                    Some(p) => self.has_property(p, key),
                    None => false,
                }
            }
            HeapObj::Array(items) => {
                let len = items.len();
                // A canonical integer index: a numeric Value, or a canonical numeric
                // string ("0", not "01"/"-1").
                let int_index = array_index(key).or_else(|| {
                    let k = self.key_of(key);
                    match k.parse::<u32>() {
                        Ok(n) if n != u32::MAX && n.to_string() == k => Some(n as usize),
                        _ => None,
                    }
                });
                if let Some(i) = int_index {
                    // An in-range slot is present iff it is not a hole.
                    if i < len && !items[i].is_hole() {
                        return true;
                    }
                    // A hole OR an out-of-range index is not an own element, but it may
                    // be overridden (a defineProperty'd index in arr_props) or inherited
                    // from the prototype chain — [[HasProperty]] must keep walking (an
                    // out-of-range `i` was previously reported absent without this check).
                    let k = self.key_of(key);
                    if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                        return true;
                    }
                    return self.arr_proto != 0 && self.has_property(Value::heap(self.arr_proto), key);
                }
                let k = self.key_of(key);
                if k == "length" {
                    return true;
                }
                if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                    return true;
                }
                // Inherited: Array.prototype (push/map/…) then Object.prototype.
                self.arr_proto != 0 && self.has_property(Value::heap(self.arr_proto), key)
            }
            HeapObj::Str(s) => match array_index(key) {
                Some(i) => i < s.char_len,
                None => self.display(key) == "length",
            },
            HeapObj::Cons { len, .. } => match array_index(key) {
                Some(i) => i < *len,
                None => self.display(key) == "length",
            },
            // A TypedArray's integer-indexed exotic own properties (`0 in ta`),
            // then any named own prop (`ta.constructor` override in arr_props),
            // then the %TypedArray%.prototype chain (`"subarray" in ta`).
            HeapObj::TypedArray { .. } => {
                let k = self.key_of(key);
                // A CanonicalNumericIndexString is absorbed by the integer-indexed
                // exotic [[HasProperty]]: present iff it's a VALID integer index,
                // and never inherited from the prototype (so `TA.prototype[5]`
                // does not make `5 in ta` true on a shorter array).
                if self.is_canonical_numeric_index(&k) {
                    return self.ta_valid_index(idx, &k).is_some();
                }
                if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                    return true;
                }
                match self.proto_of.get(&idx).copied().filter(|p| p.is_heap()) {
                    Some(p) => self.has_property(p, key),
                    None => false,
                }
            }
            HeapObj::Map { .. } | HeapObj::Set(_) => self.display(key) == "size",
            // Static members (data + `static get`/`set` accessors) are own
            // properties of the class value and are inherited up the chain.
            HeapObj::Class(_) => {
                let k = self.key_of(key);
                // A class always owns `prototype`.
                if k == "prototype" && self.callable_has_prototype(obj) {
                    return true;
                }
                let mut cur = Some(idx);
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.statics.get(&k).is_some()
                                || c.static_getters.iter().any(|(n, _)| *n == k)
                                || c.static_setters.iter().any(|(n, _)| *n == k)
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
            _ => {
                let k = self.key_of(key);
                // Exotic objects (boxed primitives, Date, Promise, RegExp, Weak*,
                // …) keep their named own props in the arr_props side table.
                if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                    return true;
                }
                // A callable's assigned own properties (`fn.x = …`) live in the
                // SEPARATE fn_props side table — `"x" in fn` must see them too
                // (mirrors get_own_property; read_descriptor relies on this for a
                // Function-object descriptor).
                if matches!(
                    self.heap.get(idx),
                    HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
                ) && self.fn_props.get(&idx).map_or(false, |m| m.pos(&k).is_some())
                {
                    return true;
                }
                // A function's synthesized `prototype` own property (ordinary
                // functions + generators; not arrows/methods/async/bound/native).
                if k == "prototype" && self.callable_has_prototype(obj) {
                    return true;
                }
                // A boxed String wrapper exposes the wrapped string's chars +
                // `length` as integer-indexed own properties.
                if let HeapObj::Boxed { kind: 0, value } = self.heap.get(idx) {
                    let v = *value;
                    let clen = match self.heap.get(v.heap_index()) {
                        HeapObj::Str(s) => Some(s.char_len),
                        HeapObj::Cons { len, .. } => Some(*len),
                        _ => None,
                    };
                    if let Some(n) = clen {
                        if let Some(i) = array_index(key) {
                            if i < n {
                                return true;
                            }
                        }
                        if k == "length" {
                            return true;
                        }
                    }
                }
                // The prototype chain: an explicit `proto_of`, else the intrinsic
                // prototype for this object's kind — so inherited methods/accessors
                // are visible to `in` (`"toFixed" in Object(2.5)`, `"call" in fn`),
                // mirroring get_member. Without this fallback `in` saw only own
                // props on a boxed primitive / callable / Date / Promise / …
                let proto = if let Some(&p) = self.proto_of.get(&idx) {
                    p.is_heap().then_some(p)
                } else {
                    let bp = match self.heap.get(idx) {
                        HeapObj::Func(_)
                        | HeapObj::Closure { .. }
                        | HeapObj::Bound { .. }
                        | HeapObj::Native(_) => self.fn_proto,
                        HeapObj::Boxed { kind: 0, .. } => self.str_proto,
                        HeapObj::Boxed { kind: 1, .. } => self.num_proto,
                        HeapObj::Boxed { kind: 2, .. } => self.bool_proto,
                        HeapObj::Boxed { kind: 3, .. } => self.symbol_proto,
                        HeapObj::Boxed { kind: 4, .. } => self.bigint_proto,
                        HeapObj::Date(_) => self.date_proto,
                        HeapObj::Promise { .. } => self.promise_proto,
                        HeapObj::RegExp { .. } => self.regexp_proto,
                        HeapObj::WeakMap { .. } => self.weakmap_proto,
                        HeapObj::WeakSet(_) => self.weakset_proto,
                        HeapObj::WeakRef(_) => self.weakref_proto,
                        HeapObj::FinalizationRegistry { .. } => self.finreg_proto,
                        _ => 0,
                    };
                    (bp != 0).then_some(Value::heap(bp))
                };
                match proto {
                    Some(p) => self.has_property(p, key),
                    None => false,
                }
            }
        }
    }

    /// The ORDERED lexical private-brand chain of the class body currently
    /// executing — resolved from the running frame's callee (a method/getter/setter
    /// VALUE, or the class value for the ctor/field-init/static block; both recorded
    /// in `method_brand` at MakeClass). `None` outside a class body.
    pub(crate) fn current_private_brands(&self) -> Option<&Vec<u64>> {
        let callee = self.frames.last()?.callee;
        if !callee.is_heap() {
            return None;
        }
        self.method_brand.get(&callee.heap_index())
    }

    /// Whether `receiver` was constructed by a class evaluation carrying `brand`
    /// (or one of its ancestors) — i.e. the private brand is installed on it.
    /// Walks the instance's class chain collecting each ClassData.private_brand.
    pub(crate) fn instance_has_brand(&self, receiver: Value, brand: u64) -> bool {
        if !receiver.is_heap() {
            return false;
        }
        let mut cur = match self.heap.get(receiver.heap_index()) {
            HeapObj::Object(m) => m.class,
            // A static private member's receiver IS the class value itself.
            HeapObj::Class(_) => Some(receiver.heap_index()),
            _ => None,
        };
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if c.private_brand == brand {
                        return true;
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        // Extra brands installed on a return-override instance (not covered by its
        // class chain).
        self.instance_brand.get(&receiver.heap_index()).is_some_and(|bs| bs.contains(&brand))
    }

    /// Install a class's own private brand on a constructor RETURN-OVERRIDE instance
    /// — one whose `map.class` chain does not already carry the brand (a normal
    /// instance is branded via its class link, so this is a no-op for it).
    pub(crate) fn brand_instance(&mut self, inst: Value, classval: Value) {
        if !inst.is_heap() || !classval.is_heap() {
            return;
        }
        let own = self.method_brand.get(&classval.heap_index()).and_then(|c| c.first()).copied();
        if let Some(own) = own {
            if !self.instance_has_brand(inst, own) {
                self.instance_brand.entry(inst.heap_index()).or_default().push(own);
            }
        }
    }

    /// Brand-aware private presence for accessing private name `key`:
    /// `Some(true/false)` when the accessing class body's brand chain is
    /// resolvable, `None` when not (the caller keeps its textual check).
    ///
    /// Resolution is name-precise: walk the lexical chain INNERMOST-first and, for
    /// the first brand whose class actually DECLARES `key`, require the receiver to
    /// carry THAT specific brand (so a `#x` declared in an enclosing/shadowing
    /// class resolves to the right class, not merely "some class in scope"). If no
    /// chain brand is known to declare `key` (e.g. the name set is incomplete),
    /// fall back to the lenient any-brand check — never tighter than the chain, so
    /// this can only ADD precision, never reject a previously-accepted access.
    pub(crate) fn private_brand_ok(&self, receiver: Value, key: &str) -> Option<bool> {
        let chain = self.current_private_brands()?;
        for &b in chain.iter() {
            if self
                .brand_private_names
                .get(&b)
                .is_some_and(|names| names.iter().any(|n| n == key))
            {
                return Some(self.instance_has_brand(receiver, b));
            }
        }
        Some(chain.iter().any(|&b| self.instance_has_brand(receiver, b)))
    }

    /// Proxy-aware [[HasProperty]] (`in` / Reflect.has). Mirrors `has_property`
    /// but is `&mut` so it can dispatch a `has` trap — both when `obj` itself is a
    /// Proxy AND when a Proxy sits in the prototype chain (e.g.
    /// `Object.create(proxy)`), which the immutable `has_property` cannot do.
    pub(crate) fn has_property_dyn(&mut self, obj: Value, key: Value) -> Result<bool, Thrown> {
        if !obj.is_heap() {
            return Ok(false);
        }
        let idx = obj.heap_index();
        // A Proxy: dispatch the `has` trap (with the post-trap invariant), or
        // forward to the target's [[HasProperty]] when there is no trap.
        if let Some((target, handler, revoked)) = self.proxy_parts(idx) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'has' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "has")? {
                Some(trap) => {
                    let ks = self.key_of(key);
                    let kv = self.key_to_value(&ks);
                    let res = self.call_value(trap, handler, &[target, kv])?;
                    let present = self.truthy(res);
                    // A `false` result is illegal when the target has the own
                    // property non-configurable, or the target is non-extensible.
                    if !present {
                        let desc = self.object_get_own_property_descriptor(target, &ks);
                        if desc != Value::UNDEFINED {
                            let cfg = self.get_prop(desc, "configurable")?;
                            if !self.truthy(cfg) || !self.is_extensible(target)? {
                                return Err(Thrown(
                                    "TypeError: proxy 'has' returned false for a non-configurable / non-extensible-target own property".into(),
                                ));
                            }
                        }
                    }
                    Ok(present)
                }
                None => self.has_property_dyn(target, key),
            };
        }
        // A plain object: own data property or an inherited method/getter/setter
        // on the class chain; else walk the [[Prototype]] (which may be a Proxy)
        // via has_property_dyn, using has_property's proto resolution. Each
        // `self.heap.get` borrow is scoped so the recursive &mut call is free.
        if matches!(self.heap.get(idx), HeapObj::Object(_)) {
            let k = self.key_of(key);
            if matches!(self.heap.get(idx), HeapObj::Object(m) if m.get(&k).is_some()) {
                return Ok(true);
            }
            let mut cur = match self.heap.get(idx) {
                HeapObj::Object(m) => m.class,
                _ => None,
            };
            while let Some(cidx) = cur {
                let step = match self.heap.get(cidx) {
                    HeapObj::Class(c) => Some((
                        c.methods.iter().any(|(n, _)| *n == k)
                            || c.getters.iter().any(|(n, _)| *n == k)
                            || c.setters.iter().any(|(n, _)| *n == k),
                        c.parent,
                    )),
                    _ => None,
                };
                match step {
                    Some((true, _)) => return Ok(true),
                    Some((false, parent)) => cur = parent,
                    None => break,
                }
            }
            let proto = if let Some(&p) = self.proto_of.get(&idx) {
                p.is_heap().then_some(p)
            } else if self.obj_proto != 0 && idx != self.obj_proto {
                Some(Value::heap(self.obj_proto))
            } else {
                None
            };
            return match proto {
                Some(p) => self.has_property_dyn(p, key),
                None => Ok(false),
            };
        }
        // A TypedArray with a USER prototype chain can have a Proxy in it whose
        // 'has' trap must fire (the immutable walk below treats it as inert):
        // absorb canonical numeric indices, check arr_props own props, then
        // recurse dynamically up the chain.
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            let k = self.key_of(key);
            if self.is_canonical_numeric_index(&k) {
                return Ok(self.ta_valid_index(idx, &k).is_some());
            }
            if self.arr_props.get(&idx).map_or(false, |m| m.pos(&k).is_some()) {
                return Ok(true);
            }
            return match self.proto_of.get(&idx).copied().filter(|p| p.is_heap()) {
                Some(p) => self.has_property_dyn(p, key),
                None => Ok(false),
            };
        }
        // Other heap kinds (Array / Str / …) carry no Proxy in their
        // built-in prototype chain, so the exact immutable walk suffices.
        Ok(self.has_property(obj, key))
    }

    /// `val instanceof <built-in ctor>`. With no user prototype chain the result
    /// is structural: by heap kind for Array/Object/Function, and by the `name`
    /// field for the Error family (any error subtype satisfies `instanceof
    /// Error`). Primitives are never an instance of anything.
    pub(crate) fn eval_instanceof(&mut self, val: Value, ctor: InstanceCtor) -> bool {
        use InstanceCtor as C;
        if !val.is_heap() {
            return false;
        }
        let idx = val.heap_index();
        match ctor {
            C::Array => matches!(self.heap.get(idx), HeapObj::Array(_)),
            // Spec instanceof: is %Function.prototype% in `val`'s prototype chain?
            // Catches plain functions/closures AND bound functions, natives, and
            // the builtin constructor objects (Array/Object/Map/…) — all of which
            // chain to %Function.prototype% — not just literal Func/Closure values.
            C::Function => {
                self.fn_proto != 0 && self.is_prototype_of(Value::heap(self.fn_proto), val)
            }
            // Every non-primitive (array, object, function, error) is an Object.
            C::Object => matches!(
                self.heap.get(idx),
                HeapObj::Array(_) | HeapObj::Object(_) | HeapObj::Func(_) | HeapObj::Closure { .. }
            ),
            // An error ctor: a canonical-named error instance (internal throw /
            // `new TypeError`) OR — for `class X extends TypeError` / `Object.
            // create(TypeError.prototype)` — the matching error prototype is in
            // `val`'s prototype chain.
            C::Error => self.error_name(idx).is_some() || self.error_proto_in_chain(val, "Error"),
            C::TypeError => {
                self.error_name(idx).as_deref() == Some("TypeError")
                    || self.error_proto_in_chain(val, "TypeError")
            }
            C::RangeError => {
                self.error_name(idx).as_deref() == Some("RangeError")
                    || self.error_proto_in_chain(val, "RangeError")
            }
            C::SyntaxError => {
                self.error_name(idx).as_deref() == Some("SyntaxError")
                    || self.error_proto_in_chain(val, "SyntaxError")
            }
            C::ReferenceError => {
                self.error_name(idx).as_deref() == Some("ReferenceError")
                    || self.error_proto_in_chain(val, "ReferenceError")
            }
            C::EvalError => {
                self.error_name(idx).as_deref() == Some("EvalError")
                    || self.error_proto_in_chain(val, "EvalError")
            }
            C::UriError => {
                self.error_name(idx).as_deref() == Some("URIError")
                    || self.error_proto_in_chain(val, "URIError")
            }
            C::AggregateError => {
                self.error_name(idx).as_deref() == Some("AggregateError")
                    || self.error_proto_in_chain(val, "AggregateError")
            }
        }
    }

    /// Whether the error prototype named `name` (e.g. "TypeError") is in `val`'s
    /// prototype chain — the proto-based half of `instanceof <ErrorCtor>`, which
    /// catches subclasses and `Object.create(XError.prototype)`.
    fn error_proto_in_chain(&mut self, val: Value, name: &str) -> bool {
        match native::ERROR_NAMES.iter().position(|&n| n == name) {
            Some(k) if self.error_protos[k] != 0 => {
                self.is_prototype_of(Value::heap(self.error_protos[k]), val)
            }
            _ => false,
        }
    }

    /// Build an Error object from an internal throw message. A message like
    /// `"TypeError: cannot read …"` splits into `name="TypeError"` and
    /// `message="cannot read …"`; anything else becomes a generic `Error` whose
    /// message is the whole text. Mirrors the `{name, message}` shape the
    /// compiler emits for `new TypeError(…)`, so both catch paths are uniform.
    pub(crate) fn alloc_error_from_message(&mut self, raw: &str) -> Value {
        // Internal errors are formatted "Name: message"; recover the kind so the
        // synthesised object links to the right prototype (and `e instanceof X`,
        // `e.constructor` work). Anything unrecognised is a base `Error`.
        let (kind, message) = match raw.split_once(": ") {
            Some((pre, rest)) => match native::ERROR_NAMES.iter().position(|&n| n == pre) {
                Some(i) => (i as u8, rest.to_string()),
                None => (0, raw.to_string()),
            },
            None => (0, raw.to_string()),
        };
        let msg_v = self.alloc_str(message);
        self.make_error(kind, Some(msg_v))
    }

    /// Allocate a proto-linked error instance of the given kind (0=Error … 7=
    /// AggregateError). `name` is set own (so the structural `instanceof`/`error_name`
    /// path keeps working); `message` is set own only when supplied and not
    /// `undefined` (else inherited as "" from the prototype). The prototype link
    /// gives `.constructor`, `.toString`, and value-`instanceof` resolution.
    pub(crate) fn make_error(&mut self, kind: u8, msg: Option<Value>) -> Value {
        let k = (kind as usize).min(7);
        let name_v = self.alloc_str(native::ERROR_NAMES[k].to_string());
        let msg_idx = match msg {
            Some(m) if m != Value::UNDEFINED => Some(self.to_str_idx(m)),
            _ => None,
        };
        // `message` is a non-enumerable own data property (ES: CreateNonEnumerable-
        // DataPropertyOrThrow). `name` is normally inherited from the prototype, but
        // zipp keeps it own for the structural error_name/instanceof path — also
        // non-enumerable, so `Object.keys(err)` is `[]` as the spec requires.
        let attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut map = ObjMap::new();
        map.define("name", name_v, attr);
        if let Some(mi) = msg_idx {
            map.define("message", Value::heap(mi), attr);
        }
        let obj = self.heap.alloc(HeapObj::Object(map));
        let p = self.error_protos[k];
        if p != 0 {
            self.proto_of.insert(obj, Value::heap(p));
        }
        self.error_data.insert(obj); // [[ErrorData]] internal slot
        Value::heap(obj)
    }

    /// AggregateError(errors, …): install the `errors` data property — a
    /// non-enumerable, writable, configurable own array built from
    /// IterableToList(errorsArg) (spec 20.5.7.1 steps 4-5). `iterate_to_vec` runs the
    /// argument's iterator (user code) under a GC lock, so `err` stays live across it.
    pub(crate) fn install_agg_errors(&mut self, err: Value, errors_arg: Value) -> Result<(), Thrown> {
        let list = self.iterate_to_vec(errors_arg)?;
        let arr = Value::heap(self.heap.alloc(HeapObj::Array(list)));
        if let HeapObj::Object(m) = self.heap.get_mut(err.heap_index()) {
            m.define(
                "errors",
                arr,
                PropAttr {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                },
            );
        }
        Ok(())
    }

    /// Build the `arguments` object for a (non-arrow) function activation. The
    /// element store is still a dense Array (so `arguments[i]`/`.length` stay fast),
    /// but it is given the spec-mandated ordinary shape:
    ///   - [[Prototype]] = %Object.prototype% (arguments is NOT an Array — it must
    ///     not inherit Array.prototype methods);
    ///   - own @@iterator = %Array.prototype.values% { w:t, e:f, c:t };
    ///   - own `callee`: a sloppy function gets a data property = the function
    ///     { w:t, e:f, c:t }; a strict function gets the %ThrowTypeError% poison-pill
    ///     accessor { e:f, c:f } (the unmapped-arguments callee).
    /// (The exotic-vs-ordinary `length` distinction is left as-is for now.)
    pub(crate) fn build_arguments_object(
        &mut self,
        args: Vec<Value>,
        callee: Value,
        is_strict: bool,
    ) -> Value {
        let obj_proto = self.obj_proto;
        let array_values = self.default_array_iter;
        // The canonical %ThrowTypeError% (set up at init); fall back to a fresh one
        // only in the unlikely event a strict arguments object is built pre-setup.
        let thrower = if is_strict {
            if self.throw_type_error != Value::UNDEFINED {
                self.throw_type_error
            } else {
                Value::heap(self.heap.alloc(HeapObj::Native(native::FN_THROW_TYPE_ERROR)))
            }
        } else {
            Value::UNDEFINED
        };
        let idx = self.heap.alloc(HeapObj::Array(args));
        self.arguments_objs.insert(idx); // [[ParameterMap]] marker (toString tag)
        if obj_proto != 0 {
            self.proto_of.insert(idx, Value::heap(obj_proto));
        }
        let m = self.arr_props.entry(idx).or_insert_with(ObjMap::new);
        m.define(
            "@@iterator",
            array_values,
            PropAttr {
                writable: true,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        if is_strict {
            m.define(
                "callee",
                thrower,
                PropAttr {
                    writable: false,
                    enumerable: false,
                    configurable: false,
                    accessor: true,
                    setter: thrower,
                },
            );
        } else {
            m.define(
                "callee",
                callee,
                PropAttr {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                },
            );
        }
        Value::heap(idx)
    }

    /// Allocate a fresh unique `Symbol` with description `desc` (a string Value or
    /// UNDEFINED) and a unique internal prop_key (`@@sym:N`). Recorded in
    /// `symbol_keys` so the symbol can be reflected from an own property key.
    pub(crate) fn make_symbol(&mut self, desc: Value) -> Value {
        self.symbol_counter += 1;
        let prop_key = format!("@@sym:{}", self.symbol_counter);
        let v = Value::heap(self.heap.alloc(HeapObj::Symbol { desc, prop_key: prop_key.clone() }));
        self.symbol_keys.insert(prop_key, v);
        v
    }

    /// Allocate a symbol with a FIXED prop_key (well-known `@@iterator` etc., or a
    /// `Symbol.for` registry key) — so distinct call sites share the same key.
    pub(crate) fn make_named_symbol(&mut self, desc: Value, prop_key: &str) -> Value {
        let v = Value::heap(
            self.heap.alloc(HeapObj::Symbol { desc, prop_key: prop_key.to_string() }),
        );
        self.symbol_keys.insert(prop_key.to_string(), v);
        v
    }

    /// Coerce a Value used as a PROPERTY KEY to its string form: a Symbol → its
    /// internal `prop_key` (`@@iterator` / `@@sym:N`), anything else → `display`.
    pub(crate) fn key_of(&self, key: Value) -> String {
        if key.is_heap() {
            if let HeapObj::Symbol { prop_key, .. } = self.heap.get(key.heap_index()) {
                return prop_key.clone();
            }
        }
        self.display(key)
    }

    /// `ToPropertyKey(key)` (7.1.19): a Symbol maps to its registry key; anything
    /// else is `ToString`-coerced (invoking `toString`/`valueOf` on an object and
    /// throwing TypeError for a Symbol-returning conversion). Unlike [`key_of`]
    /// this runs user coercion, so it is `&mut self` and fallible — use it for a
    /// caller-supplied property-name argument (e.g. `Object.defineProperty`).
    pub(crate) fn to_property_key(&mut self, key: Value) -> Result<String, Thrown> {
        // ToPropertyKey: ToPrimitive(key, hint String) FIRST — a Symbol result
        // (from a plain Symbol, or an object whose @@toPrimitive/toString returns a
        // Symbol) is the property key (its "@@…" form); any other primitive is
        // ToString'd. (Without the ToPrimitive step, an object key resolving to a
        // Symbol would wrongly throw "Cannot convert a Symbol value to a string".)
        let prim = self.to_primitive_string(key)?;
        if prim.is_heap() {
            if let HeapObj::Symbol { prop_key, .. } = self.heap.get(prim.heap_index()) {
                return Ok(prop_key.clone());
            }
        }
        self.to_js_string(prim)
    }

    /// Allocate a BigInt value.
    pub(crate) fn make_bigint(&mut self, v: i128) -> Value {
        Value::heap(self.heap.alloc(HeapObj::BigInt(v)))
    }

    /// The i128 of a BigInt value, else None.
    pub(crate) fn bigint_value(&self, v: Value) -> Option<i128> {
        if v.is_heap() {
            if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
                return Some(*n);
            }
        }
        None
    }

    /// thisBigIntValue(v): the i128 of a BigInt primitive OR a boxed BigInt wrapper
    /// (`Object(1n)`, a Boxed of kind 4); `None` otherwise. Backs
    /// BigInt.prototype.{toString,valueOf}, which accept the wrapper object.
    pub(crate) fn this_bigint_value(&self, v: Value) -> Option<i128> {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::BigInt(n) => return Some(*n),
                HeapObj::Boxed { kind: 4, value } => return self.bigint_value(*value),
                _ => {}
            }
        }
        None
    }

    /// `ToBigInt(v)` (used by `BigInt(x)`, asIntN/asUintN, and `==`). A non-integer
    /// number → RangeError; symbol/null/undefined/object → TypeError; a bad numeric
    /// string → SyntaxError.
    pub(crate) fn to_bigint(&mut self, v: Value) -> Result<i128, Thrown> {
        if let Some(n) = self.bigint_value(v) {
            return Ok(n);
        }
        if v.is_bool() {
            return Ok(if v.as_bool() { 1 } else { 0 });
        }
        // ToBigInt of a Number is a TypeError — a Number is only accepted by the
        // BigInt() constructor's NumberToBigInt step (see `bigint_from`). This covers
        // a boxed Number and an object whose ToPrimitive yields a Number (via the
        // object branch below), not just a bare numeric literal.
        if v.is_number() {
            return Err(Thrown("TypeError: Cannot convert a Number to a BigInt".into()));
        }
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
            let t = s.trim();
            if t.is_empty() {
                return Ok(0);
            }
            return parse_bigint_str(t)
                .ok_or_else(|| Thrown(format!("SyntaxError: Cannot convert {t} to a BigInt")));
        }
        // ToBigInt step 1: an object is first taken through ToPrimitive(number)
        // (honouring Symbol.toPrimitive / valueOf / toString), then re-dispatched.
        // to_primitive_number always yields a primitive or throws, so this recurses
        // at most once into a non-object branch above.
        if self.is_object_value(v) {
            let prim = self.to_primitive_number(v)?;
            return self.to_bigint(prim);
        }
        Err(Thrown("TypeError: Cannot convert this value to a BigInt".into()))
    }

    /// The `BigInt(value)` constructor coercion (NOT the abstract ToBigInt): the
    /// value is taken through ToPrimitive(number); an integral Number is accepted
    /// via NumberToBigInt (a non-integral Number is a RangeError), and any other
    /// primitive falls through to the strict ToBigInt (Boolean/String/BigInt).
    pub(crate) fn bigint_from(&mut self, v: Value) -> Result<i128, Thrown> {
        let prim = if self.is_object_value(v) {
            self.to_primitive_number(v)?
        } else {
            v
        };
        if prim.is_number() {
            let d = prim.as_f64();
            if !d.is_finite() || d.fract() != 0.0 {
                return Err(Thrown(
                    "RangeError: The number is not a safe integer and cannot be converted to a BigInt"
                        .into(),
                ));
            }
            return Ok(d as i128);
        }
        self.to_bigint(prim)
    }

    /// Build a RegExp from a pattern value + flags value (`/x/g`, `new RegExp(p,f)`).
    /// A RegExp pattern contributes its source (+ its flags when none are given);
    /// else ToString. Validates flags + compiles via `regress` (bad → SyntaxError).
    pub(crate) fn build_regexp(&mut self, p: Value, f: Value) -> Result<Value, Thrown> {
        // A real RegExp exotic contributes its [[OriginalSource]] (+ flags when none
        // are given). Computed first so the heap borrow is released before the
        // observable `is_regexp`/`Get` calls below.
        let real_regexp = match p.is_heap().then(|| self.heap.get(p.heap_index())) {
            Some(HeapObj::RegExp { source, flags, .. }) => Some((source.clone(), flags.clone())),
            _ => None,
        };
        let (source, inherited) = if let Some((src, fl)) = real_regexp {
            (src, Some(fl))
        } else if p.is_undefined() {
            (String::new(), None)
        } else if self.is_regexp(p)? {
            // A RegExp-LIKE object (truthy `@@match`, but not a real RegExp exotic):
            // read `source`/`flags` via Get (observable, may throw) per the RegExp
            // constructor, instead of ToString(pattern).
            let src_v = self.get_prop(p, "source")?;
            let src = if src_v.is_undefined() { "(?:)".to_string() } else { self.to_js_string(src_v)? };
            let inh = if f.is_undefined() {
                let fl_v = self.get_prop(p, "flags")?;
                Some(if fl_v.is_undefined() { String::new() } else { self.to_js_string(fl_v)? })
            } else {
                None
            };
            (src, inh)
        } else {
            (self.to_js_string(p)?, None)
        };
        let flags = if f.is_undefined() {
            inherited.unwrap_or_default()
        } else {
            self.to_js_string(f)?
        };
        // Validate: only g/i/m/s/u/y/d/v, each at most once.
        let mut seen = std::collections::HashSet::new();
        for c in flags.chars() {
            if !"gimsuyvd".contains(c) || !seen.insert(c) {
                return Err(Thrown(format!(
                    "SyntaxError: Invalid flags supplied to RegExp constructor '{flags}'"
                )));
            }
        }
        // `u` (Unicode) and `v` (UnicodeSets) select mutually-exclusive grammars;
        // enabling both is a SyntaxError (ParsePattern). (Literal `/x/uv` is caught
        // earlier by the parser; this guards the `new RegExp(p, "uv")` path.)
        if seen.contains(&'u') && seen.contains(&'v') {
            return Err(Thrown(format!(
                "SyntaxError: Invalid flags supplied to RegExp constructor '{flags}'"
            )));
        }
        // The matching flags `regress` understands (g/y/d are JS-level state).
        // `u` enables Unicode mode and `v` the distinct UnicodeSets grammar (set
        // operations, nested classes, `\q{…}`, properties-of-strings) — pass each
        // through verbatim rather than collapsing `v` into `u`.
        let mut rflags = String::new();
        for c in flags.chars() {
            match c {
                'i' | 'm' | 's' | 'u' | 'v' => rflags.push(c),
                _ => {}
            }
        }
        let regex = regress::Regex::with_flags(&source, rflags.as_str())
            .map_err(|e| Thrown(format!("SyntaxError: Invalid regular expression: /{source}/: {e}")))?;
        let idx = self
            .heap
            .alloc(HeapObj::RegExp { regex: Box::new(regex), source, flags, last_index: Value::int(0) });
        if self.regexp_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.regexp_proto));
        }
        Ok(Value::heap(idx))
    }

    // ── Temporal.Duration ──

}
