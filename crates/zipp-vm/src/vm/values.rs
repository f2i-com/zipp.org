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
                if let Some(i) = array_index(key) {
                    return i < len;
                }
                let k = self.key_of(key);
                // A canonical numeric-string index ("0", not "01"/"-1") is an
                // array index too.
                if let Ok(n) = k.parse::<u32>() {
                    if n != u32::MAX && n.to_string() == k {
                        return (n as usize) < len;
                    }
                }
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
            HeapObj::Map { .. } | HeapObj::Set(_) => self.display(key) == "size",
            // Static members (data + `static get`/`set` accessors) are own
            // properties of the class value and are inherited up the chain.
            HeapObj::Class(_) => {
                let k = self.key_of(key);
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
            _ => false,
        }
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
            C::Function => {
                matches!(self.heap.get(idx), HeapObj::Func(_) | HeapObj::Closure { .. })
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
        let mut map = ObjMap::new();
        map.set("name", name_v);
        if let Some(mi) = msg_idx {
            map.set("message", Value::heap(mi));
        }
        let obj = self.heap.alloc(HeapObj::Object(map));
        let p = self.error_protos[k];
        if p != 0 {
            self.proto_of.insert(obj, Value::heap(p));
        }
        Value::heap(obj)
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
        if key.is_heap() {
            if let HeapObj::Symbol { prop_key, .. } = self.heap.get(key.heap_index()) {
                return Ok(prop_key.clone());
            }
        }
        self.to_js_string(key)
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
        if v.is_int() {
            return Ok(v.as_int() as i128);
        }
        if v.is_double() {
            let d = v.as_f64();
            if !d.is_finite() || d.fract() != 0.0 {
                return Err(Thrown(
                    "RangeError: The number is not a safe integer and cannot be converted to a BigInt"
                        .into(),
                ));
            }
            return Ok(d as i128);
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
        Err(Thrown("TypeError: Cannot convert this value to a BigInt".into()))
    }

    /// Build a RegExp from a pattern value + flags value (`/x/g`, `new RegExp(p,f)`).
    /// A RegExp pattern contributes its source (+ its flags when none are given);
    /// else ToString. Validates flags + compiles via `regress` (bad → SyntaxError).
    pub(crate) fn build_regexp(&mut self, p: Value, f: Value) -> Result<Value, Thrown> {
        let (source, inherited) = if p.is_heap() {
            if let HeapObj::RegExp { source, flags, .. } = self.heap.get(p.heap_index()) {
                (source.clone(), Some(flags.clone()))
            } else {
                (self.to_js_string(p)?, None)
            }
        } else if p.is_undefined() {
            (String::new(), None)
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
        // The matching flags `regress` understands (g/y/d are JS-level state).
        let mut rflags = String::new();
        for c in flags.chars() {
            match c {
                'i' | 'm' | 's' => rflags.push(c),
                'u' | 'v' if !rflags.contains('u') => rflags.push('u'),
                _ => {}
            }
        }
        let regex = regress::Regex::with_flags(&source, rflags.as_str())
            .map_err(|e| Thrown(format!("SyntaxError: Invalid regular expression: /{source}/: {e}")))?;
        let idx = self
            .heap
            .alloc(HeapObj::RegExp { regex: Box::new(regex), source, flags, last_index: 0 });
        if self.regexp_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.regexp_proto));
        }
        Ok(Value::heap(idx))
    }

    // ── Temporal.Duration ──

}
