// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// `Object.assign(target, ...sources)`: copy each source's own enumerable
    /// keys (object keys, or an array's index strings) onto `target`; returns
    /// `target`. Primitive (incl. null/undefined) sources contribute nothing.
    pub(crate) fn object_assign(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // target = ToObject(arg0): a primitive boxes (so `Object.assign("x")`
        // returns a String wrapper), null/undefined throw.
        self.require_object_coercible(arg0)?;
        let target = self.to_object(arg0)?;
        let tidx = target.heap_index();
        let mut added_any = false;
        for &src in &args[1..] {
            if !src.is_heap() {
                continue;
            }
            // Gather (key, val) pairs under the immutable borrow, then write.
            // (A string source spreads as index→1-UNIT string, like an array —
            // the String exotic's own keys are unit positions; a surrogate half
            // is a REAL 1-unit lone-surrogate string.)
            let str_units: Option<Vec<u16>> = match self.heap.get(src.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => Some(
                    crate::heap::wtf8_units_iter(
                        &self.heap.str_wtf8_cow(src.heap_index()).unwrap(),
                    )
                    .collect(),
                ),
                _ => None,
            };
            let pairs: Vec<(String, Value)> = if let Some(units) = str_units {
                units
                    .into_iter()
                    .enumerate()
                    .map(|(i, u)| (i.to_string(), self.str_from_unit(u)))
                    .collect()
            } else {
                // CopyDataProperties: ? from.[[OwnPropertyKeys]]() (integer, string,
                // THEN symbol — symbols INCLUDED), and per key ? [[GetOwnProperty]]
                // (skip absent / non-enumerable) then ? [[Get]]. object_own_keys +
                // proxy_gopd route through a Proxy's ownKeys/gopd traps and a getter
                // fires (its abrupt propagates), so a snapshot can't swallow them.
                let keys_v = self.object_own_keys(src)?;
                let keys: Vec<Value> = match self.heap.get(keys_v.heap_index()) {
                    HeapObj::Array(a) => a.clone(),
                    _ => Vec::new(),
                };
                let mut pv = Vec::with_capacity(keys.len());
                for k in keys {
                    let ks = self.key_of(k);
                    let desc = match self.proxy_gopd(src, &ks)? {
                        Some(d) => d,
                        None => self.object_get_own_property_descriptor(src, &ks),
                    };
                    if desc.is_undefined() {
                        continue;
                    }
                    let en = self.get_prop(desc, "enumerable")?;
                    if !self.truthy(en) {
                        continue;
                    }
                    let v = self.get_member(src, &ks, src)?;
                    pv.push((ks, v));
                }
                pv
            };
            for (k, v) in pairs {
                // Set(to, key, value, true) — STRICT, per CopyDataProperties: a
                // setter is invoked (and a throwing setter propagates), and a write
                // rejected by the target's descriptor (non-writable data, setter-
                // less accessor) or a frozen / sealed / non-extensible target throws
                // a TypeError rather than silently no-op'ing.
                if let HeapObj::Boxed { kind: 0, value } = self.heap.get(tidx) {
                    // A String wrapper's canonical index properties (and "length")
                    // are read-only.
                    let slen = self.heap_str_units(value.heap_index());
                    let readonly = k == "length"
                        || k.parse::<usize>().ok().filter(|n| n.to_string() == k).map_or(false, |n| n < slen);
                    if readonly {
                        return Err(Thrown(format!(
                            "TypeError: Cannot assign to read-only property '{k}' of a String"
                        )));
                    }
                }
                self.set_prop(target, &k, v, true)?;
                added_any = true;
            }
        }
        if added_any {
            self.heap.bump_version(tidx);
        }
        Ok(target)
    }

    /// CopyDataProperties for an object REST pattern (`{a, ...rest} = src`):
    /// trap-aware like `object_assign` (ownKeys → per-key [[GetOwnProperty]]
    /// → enumerable → [[Get]]), but skipping the destructured sibling keys
    /// WITHOUT calling gopd/get on them, collecting into a fresh map.
    pub(crate) fn copy_data_properties_rest(
        &mut self,
        src: Value,
        excluded: &[String],
    ) -> Result<ObjMap, Thrown> {
        let mut m = ObjMap::new();
        if !src.is_heap() {
            return Ok(m);
        }
        // A string source spreads as index → 1-UNIT string (unit-position keys;
        // a surrogate half is a REAL 1-unit lone-surrogate string).
        if matches!(self.heap.get(src.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. }) {
            let units: Vec<u16> = crate::heap::wtf8_units_iter(
                &self.heap.str_wtf8_cow(src.heap_index()).unwrap(),
            )
            .collect();
            for (i, u) in units.into_iter().enumerate() {
                let k = i.to_string();
                if excluded.iter().any(|e| *e == k) {
                    continue;
                }
                let v = self.str_from_unit(u);
                m.set(&k, v);
            }
            return Ok(m);
        }
        let keys_v = self.object_own_keys(src)?;
        let keys: Vec<Value> = match self.heap.get(keys_v.heap_index()) {
            HeapObj::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        for k in keys {
            let ks = self.key_of(k);
            if excluded.iter().any(|e| *e == ks) {
                continue;
            }
            let desc = match self.proxy_gopd(src, &ks)? {
                Some(d) => d,
                None => self.object_get_own_property_descriptor(src, &ks),
            };
            if desc.is_undefined() {
                continue;
            }
            let en = self.get_prop(desc, "enumerable")?;
            if !self.truthy(en) {
                continue;
            }
            let v = self.get_member(src, &ks, src)?;
            m.set(&ks, v);
        }
        Ok(m)
    }

    /// `Array.from(src[, mapFn])`: build an array from an array, a string's
    /// chars, or an array-like (`{length, 0:…}`), applying `mapFn(value, index)`
    /// when it is a function.
    /// Materialize a value's iteration elements: an array or set → its items, a
    /// string → its chars (as 1-char strings), a map → fresh `[key, value]` entry
    /// arrays. Throws a TypeError for a non-iterable. Allocations happen after the
    /// heap borrow is released (two phases).
    /// Whether `v` is a user-callable value (function or closure).
    /// A built-in constructor object invoked WITHOUT `new` — e.g. passed as a
    /// `map`/`filter` callback or called via `.call`/`.apply`. String/Number/
    /// Boolean coerce their argument to a primitive (matching the compiler's
    /// lowered direct-call form); every other core constructor constructs.
    pub(crate) fn call_ctor_as_function(&mut self, callee: Value, args: &[Value]) -> Result<Value, Thrown> {
        // The dynamic-function constructors called WITHOUT `new` behave exactly
        // like `new <Ctor>(...)` — both compile and return a fresh function.
        let ci = callee.heap_index();
        if ci == self.function_ctor && self.function_ctor != 0 {
            return self.build_function(args);
        }
        if ci == self.gen_fn_ctor && self.gen_fn_ctor != 0 {
            return self.build_function_kind(args, 1);
        }
        if ci == self.async_fn_ctor && self.async_fn_ctor != 0 {
            return self.build_function_kind(args, 2);
        }
        if ci == self.asyncgen_fn_ctor && self.asyncgen_fn_ctor != 0 {
            return self.build_function_kind(args, 3);
        }
        if ci == self.suppressederror_ctor && self.suppressederror_ctor != 0 {
            return self.build_suppressed_error(args);
        }
        // `Symbol(desc)` / `BigInt(x)` reached as VALUES (a callback, `.call`, a
        // bound target — the direct forms are compile-lowered to MakeSymbol /
        // BigIntFrom): both call, neither constructs.
        if ci == self.symbol_ctor && self.symbol_ctor != 0 {
            let d = match args.first().copied() {
                None => Value::UNDEFINED,
                Some(v) if v == Value::UNDEFINED => Value::UNDEFINED,
                Some(v) => self.to_str_value(v)?,
            };
            return Ok(self.make_symbol(d));
        }
        if ci == self.bigint_ctor && self.bigint_ctor != 0 {
            let n = self.bigint_from(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            return Ok(self.make_bigint_val(n));
        }
        let proto = match self.heap.get(callee.heap_index()) {
            HeapObj::Object(m) => m.get("prototype").filter(|p| p.is_heap()).map(|p| p.heap_index()),
            _ => None,
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        if let Some(p) = proto {
            if p == self.str_proto && self.str_proto != 0 {
                let s = if args.is_empty() {
                    String::new()
                } else if a0.is_heap()
                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Symbol { .. })
                {
                    // String(symbol) yields its "Symbol(desc)" text, not a TypeError.
                    self.display(a0)
                } else {
                    self.to_js_string(a0)?
                };
                return Ok(self.alloc_str(s));
            }
            if p == self.num_proto && self.num_proto != 0 {
                let n = if args.is_empty() { 0.0 } else { self.to_number_coerce(a0)? };
                return Ok(Value::num(n));
            }
            if p == self.bool_proto && self.bool_proto != 0 {
                return Ok(Value::bool(!args.is_empty() && self.truthy(a0)));
            }
            if p == self.date_proto && self.date_proto != 0 {
                // Date() as a function ignores its args and returns the string
                // form of the current time.
                let now = self.construct(callee, &[])?;
                let s = self.to_js_string(now)?;
                return Ok(self.alloc_str(s));
            }
            // Array/Object/RegExp construct identically when called as a plain
            // function: `Array(1,2)`===`new Array(1,2)`, `Object(x)`===ToObject(x),
            // `RegExp(p,f)`===`new RegExp(p,f)`. (String/Number/Boolean above
            // return PRIMITIVES, not wrappers, so they stay special-cased.)
            if (p == self.arr_proto && self.arr_proto != 0)
                || (p == self.obj_proto && self.obj_proto != 0)
                || (p == self.regexp_proto && self.regexp_proto != 0)
            {
                return self.construct(callee, args);
            }
        }
        // The error constructors also construct when called without `new`
        // (`TypeError(msg)` === `new TypeError(msg)`).
        if self.error_ctors.iter().any(|&c| c == ci && c != 0) {
            return self.construct(callee, args);
        }
        // Other core constructors (Map/Set/Promise/Temporal/…) require `new`;
        // calling them as a function is a TypeError. (Legacy call-without-new
        // forms like Array()/Object()/Error() are compiler-lowered elsewhere and
        // never reach here.)
        let name = match self.heap.get(callee.heap_index()) {
            HeapObj::Object(m) => m
                .get("name")
                .and_then(|n| self.heap.str_cow(n.heap_index()).map(|s| s.into_owned()))
                .unwrap_or_default(),
            _ => String::new(),
        };
        Err(Thrown(format!("TypeError: constructor {name} requires 'new'")))
    }

    /// GetWrappedValue: a primitive crosses the realm boundary as-is, a
    /// callable crosses as a FRESH WrappedFunction (no identity or property
    /// sharing), anything else is a TypeError.
    pub(crate) fn wrap_realm_value(&mut self, v: Value) -> Result<Value, Thrown> {
        if !v.is_heap() {
            return Ok(v);
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
        ) {
            return Ok(v);
        }
        if self.is_callable(v) {
            return self.wrapped_function_create(v);
        }
        Err(Thrown(
            "TypeError: value crossing the ShadowRealm boundary is not a primitive or callable"
                .into(),
        ))
    }

    /// WrappedFunctionCreate + CopyNameAndLength: snapshot the target's
    /// `length` (own-property check, then Get; only a Number counts) and
    /// `name` (Get; only a String counts) — ANY abrupt completion during the
    /// copy (revoked-proxy HasProperty, throwing getter) is a TypeError. The
    /// wrapper's prototype is the caller realm's %Function.prototype%.
    pub(crate) fn wrapped_function_create(&mut self, target: Value) -> Result<Value, Thrown> {
        // `target` lives only in this Rust frame while the CopyNameAndLength
        // Gets below may run USER GETTERS (arbitrary allocating JS): suspend
        // GC for the whole create so it cannot be collected mid-flight.
        let _gc = self.gc_lock_guard();
        // ANY abrupt completion (throwing getter, revoked-proxy probe) maps to
        // a caller-realm TypeError — incl. dropping the realm-side pending
        // error OBJECT, which must not cross the boundary.
        macro_rules! copy_step {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(_) => {
                        self.pending_throw.take();
                        return Err(Thrown(
                            "TypeError: WrappedFunction: copying target name/length failed"
                                .into(),
                        ));
                    }
                }
            };
        }
        let mut length = 0.0;
        let has_len = copy_step!(self.has_own_property_dyn(target, "length"));
        if has_len {
            let lv = copy_step!(self.get_prop(target, "length"));
            if lv.is_number() {
                let n = lv.as_f64();
                // max(ToIntegerOrInfinity(targetLen), 0); +Inf stays.
                length = if n.is_nan() { 0.0 } else { n.trunc().max(0.0) };
            }
        }
        let nv = copy_step!(self.get_prop(target, "name"));
        let name = if nv.is_heap() && self.heap.is_str_like(nv.heap_index()) {
            self.display(nv)
        } else {
            String::new()
        };
        let idx = self.heap.alloc(HeapObj::Wrapped { target, name, length });
        // The wrapper is created in the CALLER realm — the realm of the
        // `evaluate` function running now (a realm copy wraps with ITS realm's
        // %Function.prototype%; the main realm's is the identity).
        let home = self.native_home(self.fn_proto);
        self.proto_of.insert(idx, Value::heap(home));
        if let Some(r) = self.native_callee_realm {
            self.obj_realm.insert(idx, r);
        }
        Ok(Value::heap(idx))
    }

    pub(crate) fn is_callable(&self, v: Value) -> bool {
        // An [[IsHTMLDDA]] exotic (`document.all`) is callable (returns undefined).
        if v.is_heap() && !self.is_htmldda.is_empty() && self.is_htmldda.contains(&v.heap_index()) {
            return true;
        }
        v.is_heap()
            && match self.heap.get(v.heap_index()) {
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) => {
                    true
                }
                // The native resolve/reject functions (new Promise executor args,
                // capability functions) and combinator elements are functions.
                HeapObj::BoundResolver { .. } | HeapObj::CombinatorResolver { .. } => true,
                // A class constructor IS callable (typeof is "function"): it can be
                // bound (`C.bind()`) and passed where a function is expected. Calling
                // one without `new` still throws (the Call op / call_value route a
                // Class to resolve_callable, which rejects it) — per spec that throw
                // is "class constructor cannot be invoked without 'new'".
                HeapObj::Class(_) => true,
                // A built-in constructor object (String/Number/Array/…) is callable
                // (typeof is "function"); so is %Function.prototype% itself (a
                // built-in function that accepts any args and returns undefined).
                HeapObj::Object(m) => {
                    m.is_ctor || (self.fn_proto != 0 && v.heap_index() == self.fn_proto)
                }
                // A Proxy is callable iff its target is — the [[Call]] slot is
                // fixed at creation, and REVOCATION does not change callability
                // (the target field survives revocation behind the flag).
                HeapObj::Proxy { target, .. } => self.is_callable(*target),
                _ => false,
            }
    }

    /// `obj.hasOwnProperty(key)` — own data/accessor property, array index/length,
    /// or string index/length.
    /// `hasOwnProperty` for an ARRAY receiver with a NUMERIC key, answered
    /// without ever spelling the index. `None` means "not this shape — use the
    /// general path".
    ///
    /// The general path is `to_property_key` (which allocates a `String` for
    /// the index) followed by `has_own_property` (which parses it straight
    /// back), and that round trip IS the cost of the probe: a
    /// `hasOwn.call(a, i)` loop over a 1000-element array measured 204ms
    /// against node's 16ms, while `i in a` over the same array — which never
    /// spells the key — was 17ms.
    ///
    /// Mirrors the `HeapObj::Array` arm of `has_own_property` exactly: a hole is
    /// an absent element, and the `arr_props` side table can still carry an
    /// index the dense storage does not.
    pub(crate) fn has_own_index_fast(&self, obj: Value, key: Value) -> Option<bool> {
        if !obj.is_heap() {
            return None;
        }
        let idx = obj.heap_index();
        let HeapObj::Array(items) = self.heap.get(idx) else {
            return None;
        };
        // A non-integral or negative key spells something `has_own_property`'s
        // `parse::<usize>()` would reject; leave those to the general path
        // rather than duplicate the rejection.
        let i = crate::vm::array_index(key)?;
        if i < items.len() && !items[i].is_hole() {
            return Some(true);
        }
        match self.arr_props.get(&idx) {
            Some(m) => {
                let mut buf = [0u8; 20];
                Some(m.pos(crate::heap::index_key(&mut buf, i)).is_some())
            }
            None => Some(false),
        }
    }

    pub(crate) fn has_own_property(&self, obj: Value, key: &str) -> bool {
        // (Real private fields live in the side table and are invisible here;
        // a PUBLIC computed "#..." string key is an ordinary reflectable prop.)
        if !obj.is_heap() {
            return false;
        }
        // %Array.prototype%'s exotic own length.
        if key == "length" && self.arr_proto != 0 && obj.heap_index() == self.arr_proto {
            return true;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => {
                m.pos(key).is_some()
                    // globalThis own properties are the reserved global bindings.
                    || (obj.heap_index() == self.global_this
                        && self.global_this != 0
                        && self.global_by_name(key).is_some())
            }
            HeapObj::Array(items) => {
                // An ARGUMENTS object's `length` is an ordinary (deletable)
                // arr_props prop — covered by the arr_props clause below.
                (key == "length" && !self.arguments_objs.contains_key(&obj.heap_index()))
                    // A hole is an absent element — not an own property.
                    || // A canonical index only: `"02"` parses to 2 but is an ordinary
                    // string key, and no element answers it.
                    canonical_index_str(key).is_some_and(|i| i < items.len() && !items[i].is_hole())
                    || self.arr_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
            }
            HeapObj::Str(s) => {
                key == "length" || canonical_index_str(key).is_some_and(|i| i < s.units())
            }
            HeapObj::Cons { len, .. } => {
                key == "length" || canonical_index_str(key).is_some_and(|i| i < *len)
            }
            // A class value: own statics (data + `static get`/`set`) + name/length
            // + the synthesized `prototype` (a class always has one).
            HeapObj::Class(c) => {
                // Private STATICS live textually in the statics map but are
                // not reflectable own properties.
                !is_private_key(key)
                    && (c.statics.pos(key).is_some()
                        || c.static_getters.iter().any(|(n, _)| n == key)
                        || c.static_setters.iter().any(|(n, _)| n == key)
                        || self.callable_has_intrinsic(obj, key)
                        || (key == "prototype" && self.callable_has_prototype(obj)))
            }
            // Functions/closures + the native resolve/reject + combinator element
            // functions: assigned own props (`fn.x`) + the synthesized name/length.
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Wrapped { .. }
            | HeapObj::Native(_)
            | HeapObj::BoundResolver { .. }
            | HeapObj::CombinatorResolver { .. } => {
                self.fn_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
                    || self.callable_has_intrinsic(obj, key)
                    || (key == "prototype" && self.callable_has_prototype(obj))
                    // A legacy sloppy function OWNS `caller`/`arguments`.
                    || self.fn_has_legacy_caller_prop(obj.heap_index(), key)
            }
            // Exotic objects (boxed primitives, Date, Promise, RegExp, Weak*, …)
            // keep their named own props in the arr_props side table; a boxed String
            // also owns the wrapped string's chars + `length`.
            _ => {
                if self.arr_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some()) {
                    return true;
                }
                // A RegExp owns `lastIndex` (a writable data property).
                if key == "lastIndex"
                    && matches!(self.heap.get(obj.heap_index()), HeapObj::RegExp { .. })
                {
                    return true;
                }
                if let HeapObj::Boxed { kind: 0, value } = self.heap.get(obj.heap_index()) {
                    let clen = match self.heap.get(value.heap_index()) {
                        HeapObj::Str(s) => Some(s.units()),
                        HeapObj::Cons { len, .. } => Some(*len),
                        _ => None,
                    };
                    if let Some(n) = clen {
                        if key == "length" {
                            return true;
                        }
                        if let Ok(i) = key.parse::<usize>() {
                            if i.to_string() == key && i < n {
                                return true;
                            }
                        }
                    }
                }
                false
            }
        }
    }

    /// `[[GetOwnProperty]] is not undefined` honouring a Proxy — `Object.hasOwn` /
    /// `Object.prototype.hasOwnProperty` on a Proxy consult its
    /// `getOwnPropertyDescriptor` trap (or its target) rather than reporting `false`.
    /// Non-proxies fall back to the ordinary own-property check.
    pub(crate) fn has_own_property_dyn(&mut self, obj: Value, key: &str) -> Result<bool, Thrown> {
        self.defer_check(obj, key)?;
        // HasOwnProperty is [[GetOwnProperty]]-based: an uninitialized
        // namespace export throws (unlike the `in` operator's [[HasProperty]]).
        self.ns_tdz_check(obj, key)?;
        if obj.is_heap() {
            if let Some(desc) = self.proxy_gopd(obj, key)? {
                return Ok(desc != Value::UNDEFINED);
            }
        }
        Ok(self.has_own_property(obj, key))
    }

    /// `[[GetOwnProperty]](key).[[Enumerable]]` honouring a Proxy — the gopd trap
    /// is user JS, so the infallible `own_is_enumerable` (&self) can't reach it.
    /// A trap that reports the property absent (`undefined` descriptor) is `false`,
    /// not a deref of nothing.
    pub(crate) fn own_is_enumerable_dyn(&mut self, obj: Value, key: &str) -> Result<bool, Thrown> {
        if obj.is_heap() && self.proxy_parts(obj.heap_index()).is_some() {
            return Ok(match self.proxy_gopd(obj, key)? {
                Some(desc) if desc != Value::UNDEFINED => {
                    let e = self.get_prop(desc, "enumerable")?;
                    self.truthy(e)
                }
                _ => false,
            });
        }
        Ok(self.own_is_enumerable(obj, key))
    }

    /// `obj.propertyIsEnumerable(key)` — true if `key` is an own enumerable
    /// property. Array indices are enumerable; `length` is not.
    pub(crate) fn own_is_enumerable(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() {
            return false;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => {
                if let Some(i) = m.pos(key) {
                    m.attrs[i].enumerable
                } else if obj.heap_index() == self.global_this && self.global_this != 0 {
                    // Slot-backed SCRIPT-declared var/function globals are
                    // enumerable bindings of the global object.
                    self.program
                        .global_names
                        .iter()
                        .position(|n| n == key)
                        .is_some_and(|i| {
                            (self.program.hoisted_globals.contains(&(i as u32))
                                || self.program.decl_globals.contains(&(i as u32)))
                                && !self.globals[i].is_uninitialized()
                        })
                } else {
                    false
                }
            }
            HeapObj::Array(items) => {
                key.parse::<usize>().map_or(false, |i| i < items.len())
                    || self
                        .arr_props
                        .get(&obj.heap_index())
                        .and_then(|m| m.pos(key).map(|i| m.attrs[i].enumerable))
                        .unwrap_or(false)
            }
            // A TypedArray: a canonical in-bounds integer index is an own enumerable
            // element; a non-canonical defineProperty'd key lives in arr_props.
            HeapObj::TypedArray { .. } => {
                self.ta_valid_index(obj.heap_index(), key).is_some()
                    || self
                        .arr_props
                        .get(&obj.heap_index())
                        .and_then(|m| m.pos(key).map(|i| m.attrs[i].enumerable))
                        .unwrap_or(false)
            }
            // A function's assigned own properties live in `fn_props`.
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Native(_) => self
                .fn_props
                .get(&obj.heap_index())
                .and_then(|m| m.pos(key).map(|i| m.attrs[i].enumerable))
                .unwrap_or(false),
            // A class's own (static) properties live in `ClassData.statics`.
            HeapObj::Class(c) => {
                !is_private_key(key)
                    && c.statics.pos(key).map_or(false, |i| c.statics.attrs[i].enumerable)
            }
            // A String wrapper's char indices are own ENUMERABLE props; `length` is
            // non-enumerable; an assigned own prop lives in the arr_props side table.
            // A raw string PRIMITIVE takes the same path — the callers ToObject it
            // first, and ToObject("s") is a String exotic object with those indices.
            HeapObj::Boxed { kind: 0, .. } | HeapObj::Str(_) | HeapObj::Cons { .. } => {
                if key == "length" {
                    false
                } else if self
                    .string_exotic_chars(obj)
                    .and_then(|(_, len)| canonical_index_str(key).map(|i| i < len))
                    .unwrap_or(false)
                {
                    true
                } else {
                    self.arr_props
                        .get(&obj.heap_index())
                        .and_then(|m| m.pos(key).map(|i| m.attrs[i].enumerable))
                        .unwrap_or(false)
                }
            }
            _ => false,
        }
    }

    /// `proto.isPrototypeOf(obj)` — is `proto` anywhere in `obj`'s prototype chain?
    pub(crate) fn is_prototype_of(&mut self, proto: Value, obj: Value) -> bool {
        let mut cur = self.object_get_prototype_of(obj);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return false;
            }
            if cur == proto {
                return true;
            }
            cur = self.object_get_prototype_of(cur);
        }
        false
    }

    /// Resolve an iterable's iterator: a plain object with a `@@iterator` method
    /// (a custom iterable) yields `obj[@@iterator]()`; everything else (arrays,
    /// strings, Map/Set, generators) iterates directly and passes through.
    /// GetIterator that ALWAYS returns a real iterator OBJECT by invoking
    /// `v[@@iterator]()` — unlike `get_iterator`, which fast-paths arrays/strings/
    /// Map/Set to the raw value (driven positionally by `IterNext`). `yield*`
    /// delegation needs a genuine iterator so it can call `.next`/`.throw`/`.return`
    /// on it (an Array Iterator has `.next` but no `.throw`/`.return`, exactly as the
    /// spec requires).
    pub(crate) fn get_iterator_direct(&mut self, v: Value) -> Result<Value, Thrown> {
        let m = self.get_prop(v, "@@iterator")?;
        if !self.is_callable(m) {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        }
        let it = self.call_value(m, v, &[])?;
        if !self.is_object_value(it) {
            return Err(Thrown("TypeError: iterator is not an object".into()));
        }
        Ok(it)
    }

    pub(crate) fn get_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Object(_) | HeapObj::Proxy { .. } => {
                    let m = self.get_prop(v, "@@iterator")?;
                    if self.is_callable(m) {
                        let it = self.call_value(m, v, &[])?;
                        // GetIterator step 5: a non-object iterator is a TypeError.
                        if !self.is_object_value(it) {
                            return Err(Thrown("TypeError: iterator is not an object".into()));
                        }
                        return Ok(it);
                    }
                }
                // A plain array: fast-path the default iterator (IterNext walks the
                // array directly), but honour a replaced Array.prototype[@@iterator]
                // by invoking it (so for-of uses the overridden iterator).
                HeapObj::Array(_) => {
                    let m = self.get_prop(v, "@@iterator")?;
                    // The fast path also requires the PRISTINE
                    // %ArrayIteratorPrototype%.next — a patched next must be
                    // honoured by going through the real iterator protocol.
                    let next_intact = match self.heap.get(self.array_iter_proto) {
                        HeapObj::Object(p) => {
                            p.get("next") == Some(self.default_array_iter_next)
                        }
                        _ => false,
                    };
                    if m.bits() != self.default_array_iter.bits() || !next_intact {
                        if self.is_callable(m) {
                            return self.call_value(m, v, &[]);
                        }
                        // @@iterator deleted/poisoned (undefined / non-callable):
                        // GetIterator throws rather than falling back to the dense
                        // positional walk.
                        return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
                    }
                }
                _ => {}
            }
        }
        Ok(v)
    }

    /// `for await`: resolve the ASYNC iterator. An async generator is its own
    /// iterator; a plain object uses `@@asyncIterator` (an async iterable) or, as
    /// the spec's async-from-sync fallback, `@@iterator`; everything else (arrays,
    /// strings, Map/Set, sync generators) passes through (ForAwaitNext drives it).
    /// Returns the iterator and whether it is a SYNC one (the @@iterator
    /// fallback or a raw array/string) — whose stepped VALUES get the
    /// AsyncFromSyncIterator await-unwrap in async contexts.
    pub(crate) fn get_async_iterator(&mut self, v: Value) -> Result<(Value, bool), Thrown> {
        // An async generator is its OWN async iterator: its yielded values are
        // already settled by the generator machinery — NOT sync (no extra
        // value-await, which would shift promise interleaving by a tick).
        if v.is_heap()
            && matches!(self.heap.get(v.heap_index()), HeapObj::AsyncGenerator(_))
        {
            return Ok((v, false));
        }
        if v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Object(_) | HeapObj::Proxy { .. }
            )
        {
            // GetMethod(@@asyncIterator): undefined/null ⇒ absent (fall back to the
            // sync iterator); present-but-not-callable ⇒ TypeError (do NOT fall back —
            // reading @@iterator could run a getter the spec must not trigger). A
            // returned iterator must be an Object.
            let am = self.get_prop(v, "@@asyncIterator")?;
            if !am.is_nullish() {
                if !self.is_callable(am) {
                    return Err(Thrown(
                        "TypeError: [Symbol.asyncIterator] is not a function".into(),
                    ));
                }
                let it = self.call_value(am, v, &[])?;
                if !self.is_object_value(it) {
                    return Err(Thrown(
                        "TypeError: [Symbol.asyncIterator]() returned a non-object".into(),
                    ));
                }
                return Ok((it, false));
            }
            let sm = self.get_prop(v, "@@iterator")?;
            if !sm.is_nullish() {
                if !self.is_callable(sm) {
                    return Err(Thrown("TypeError: [Symbol.iterator] is not a function".into()));
                }
                let it = self.call_value(sm, v, &[])?;
                if !self.is_object_value(it) {
                    return Err(Thrown(
                        "TypeError: [Symbol.iterator]() returned a non-object".into(),
                    ));
                }
                return Ok((it, true));
            }
        }
        Ok((v, true))
    }

    /// Normalize a destructuring source to a positionally-indexable value: a
    /// generator or a custom iterable (object with `@@iterator`) is drained into a
    /// fresh array — LAZILY, at most `max` elements (so `let [a,b] = infinite`
    /// pulls 2, not forever); everything else (arrays/strings/Map/Set, or a
    /// non-iterable) passes through unchanged.
    pub(crate) fn iter_to_array(&mut self, v: Value, max: u32) -> Result<Value, Thrown> {
        // Array destructuring uses GetIterator(value), which first does
        // RequireObjectCoercible — so null/undefined throw a TypeError even for an
        // empty pattern (`[] = null`), before any element is read.
        if v == Value::NULL || v == Value::UNDEFINED {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        }
        // A non-iterable PRIMITIVE — a number or boolean (non-heap), or a Symbol /
        // BigInt (heap primitives) — has no `@@iterator`, so GetIterator throws.
        // (Strings are heap and ARE iterable; they fall through to the positional
        // fast path below. Plain objects without `@@iterator` are left lenient.)
        if !v.is_heap() {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        }
        if matches!(
            self.heap.get(v.heap_index()),
            HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
        ) {
            return Err(Thrown("TypeError: value is not iterable".into()));
        }
        let drain = match self.heap.get(v.heap_index()) {
            HeapObj::Generator { .. } => true,
            HeapObj::Object(_) => {
                let it = self.get_prop(v, "@@iterator")?;
                self.is_callable(it)
            }
            // Array destructuring's GetIterator reaches
            // %TypedArray%.prototype.values, whose ValidateTypedArray throws for
            // a detached or out-of-bounds view (a fixed-length view over a
            // shrunk resizable buffer). The positional fast path below must
            // surface the same TypeError instead of reading undefineds.
            HeapObj::TypedArray { .. } => {
                if self.ta_effective_len(v.heap_index()).is_none() {
                    return Err(Thrown(
                        "TypeError: TypedArray is detached or out of bounds".into(),
                    ));
                }
                false
            }
            // A plain array: fast-path the default iterator (direct indexing), but
            // honour a replaced Array.prototype[Symbol.iterator] by draining via
            // the iterator protocol (array destructuring uses it per spec).
            HeapObj::Array(_) => {
                let it = self.get_prop(v, "@@iterator")?;
                if it.bits() == self.default_array_iter.bits() {
                    false // the default array iterator → direct positional indexing
                } else if self.is_callable(it) {
                    true // a replaced, callable @@iterator → drain via the protocol
                } else {
                    // @@iterator was deleted or poisoned (undefined / non-callable):
                    // GetIterator throws a TypeError rather than silently falling
                    // back to positional indexing.
                    return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
                }
            }
            // An ITERATOR OBJECT is iterable through the protocol and nothing
            // else — `%ArrayIteratorPrototype%[Symbol.iterator]` returns `this`.
            // Without this arm it fell to the positional fast path below, which
            // reads `it[0]`, `it[1]`, … off the ITERATOR, so
            // `var [p, q] = [10, 20].values()` bound two `undefined`s. Arrays,
            // Sets, strings and generators all worked, which is why it survived:
            // the broken shape is the one nobody writes by hand.
            HeapObj::Iterator { .. } | HeapObj::IterHelper { .. } => true,
            _ => false,
        };
        if !drain {
            return Ok(v);
        }
        // Hold the not-yet-rooted drained values across the `.next()`/`.return()`
        // user re-entries.
        let _gc = self.gc_lock_guard();
        // generator → itself; iterable → its iterator. An array only reaches here
        // when its @@iterator was replaced, so call that explicitly (get_iterator
        // returns a plain array unchanged).
        let iter = if matches!(self.heap.get(v.heap_index()), HeapObj::Array(_)) {
            let m = self.get_prop(v, "@@iterator")?;
            self.call_value(m, v, &[])?
        } else {
            self.get_iterator(v)?
        };
        let is_gen = matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. });
        let lim = max as usize;
        let mut out = Vec::new();
        let mut iter_done = false;
        while out.len() < lim {
            let res = if is_gen {
                self.generator_method(iter.heap_index(), "next", &[])?
                    .unwrap_or(Value::UNDEFINED)
            } else {
                let next = self.get_prop(iter, "next")?;
                if !self.is_callable(next) {
                    iter_done = true;
                    break;
                }
                self.call_value(next, iter, &[])?
            };
            // IteratorNext step 4: a non-Object result is a TypeError. Without it
            // `done`/`value` were read off a primitive as undefined, so `var [a] =
            // it` bound undefined instead of throwing — and with a `...rest` (no
            // element bound) `max` is unbounded, so the never-done loop spun until
            // it exhausted memory.
            if !self.is_object_value(res) {
                return Err(Thrown("TypeError: iterator result is not an object".into()));
            }
            let done = self.get_prop(res, "done")?;
            if self.truthy(done) {
                iter_done = true;
                break;
            }
            out.push(self.get_prop(res, "value")?);
        }
        // IteratorClose (normal completion): destructuring took the fixed number of
        // elements it needed; if the iterator isn't exhausted, close it. With a
        // `...rest` present `max` is unbounded so the loop ran to `done` and we skip.
        let _ = is_gen;
        if !iter_done {
            // Lenient close: the eager drain runs before the pattern's per-element
            // defaults/targets, so a non-callable `return` must not pre-empt a
            // later default-expression throw (which is the value the spec keeps).
            self.iterator_close_inner(iter, false)?;
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// IteratorClose(iterator, normal): call the iterator's `return()` once if it
    /// has one, requiring an Object result (TypeError otherwise). Skips generators
    /// (driven directly via generator_method, not a prototype `return`) and
    /// non-objects. Shared by destructuring and `for-of` break.
    /// GetIterator(iterable) via @@iterator — returns a REAL iterator object (not
    /// `get_iterator`'s array fast-path, which can't be stepped by iterator_step).
    /// Used by the Map/Set constructors' AddEntriesFromIterable.
    pub(crate) fn get_iterator_object(&mut self, iterable: Value) -> Result<Value, Thrown> {
        let m = self.get_prop(iterable, "@@iterator")?;
        if !self.is_callable(m) {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(iterable))));
        }
        self.call_value(m, iterable, &[])
    }

    /// Whether `v` can begin iterator-protocol iteration: a built-in with
    /// positional iteration, or any object whose @@iterator resolves to a
    /// callable (the Get may run a getter / throw — propagated).
    pub(crate) fn value_is_iterable(&mut self, v: Value) -> Result<bool, Thrown> {
        if !v.is_heap() {
            return Ok(false);
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Str(_)
            | HeapObj::Cons { .. }
            | HeapObj::Array(_)
            | HeapObj::TypedArray { .. }
            | HeapObj::Map { .. }
            | HeapObj::Set(_)
            | HeapObj::Generator { .. }
            | HeapObj::AsyncGenerator(_)
            | HeapObj::Iterator { .. }
            | HeapObj::IterHelper { .. } => Ok(true),
            HeapObj::Object(_) | HeapObj::Proxy { .. } | HeapObj::Boxed { .. } => {
                let m = self.get_prop(v, "@@iterator")?;
                Ok(self.is_callable(m))
            }
            _ => Ok(false),
        }
    }

    pub(crate) fn iterator_close(&mut self, iter: Value) -> Result<(), Thrown> {
        self.iterator_close_inner(iter, true)
    }

    /// IteratorClose on an ALREADY-failing path (IfAbruptCloseIterator): any
    /// throw from `return()` is swallowed AND the original failure's
    /// `pending_throw` VALUE is preserved, so the close's own error cannot
    /// clobber the identity of the error being propagated.
    pub(crate) fn iterator_close_quiet(&mut self, iter: Value) {
        let saved = self.pending_throw.take();
        let _ = self.iterator_close(iter);
        self.pending_throw = saved;
    }

    /// `Object.fromEntries(iterable)` = AddEntriesFromIterable: RequireObjectCoercible,
    /// GetIterator, then step-by-step — per entry read [0]/[1] via [[Get]] and add the
    /// property IMMEDIATELY. Any abrupt while processing an entry closes the iterator
    /// (keeping the original thrown value), per IfAbruptCloseIterator.
    pub(crate) fn object_from_entries(&mut self, src: Value) -> Result<Value, Thrown> {
        self.require_object_coercible(src)?;
        let _gc = self.gc_lock_guard();
        // get_iterator_object (not get_iterator) so an array yields a real, steppable
        // iterator object rather than the dense fast-path value.
        let iter = self.get_iterator_object(src)?;
        let mut map = ObjMap::new();
        macro_rules! close_and_throw {
            ($e:expr) => {{
                let saved = self.pending_throw;
                let _ = self.iterator_close(iter);
                self.pending_throw = saved;
                return Err($e);
            }};
        }
        loop {
            let entry = match self.iterator_step(iter)? {
                Some(e) => e,
                None => break,
            };
            if !self.is_object_value(entry) {
                let msg = self.alloc_str("Iterator value is not an entry object".to_string());
                let te = self.make_error(1, Some(msg));
                self.pending_throw = Some(te);
                close_and_throw!(Thrown("TypeError: Iterator value is not an entry object".into()));
            }
            let k = match self.get_index(entry, Value::int(0)) {
                Ok(k) => k,
                Err(e) => close_and_throw!(e),
            };
            let v = match self.get_index(entry, Value::int(1)) {
                Ok(v) => v,
                Err(e) => close_and_throw!(e),
            };
            let pk = match self.to_property_key(k) {
                Ok(pk) => pk,
                Err(e) => close_and_throw!(e),
            };
            map.set(&pk, v);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(map)))))
    }

    /// AddEntriesFromIterable using a collection's OBSERVABLE adder (Map/WeakMap
    /// `set` for `pair`, Set/WeakSet `add` otherwise): the adder is read once and
    /// called per entry, so a custom/overridden adder and its validation (e.g.
    /// CanBeHeldWeakly for a WeakMap, which now accepts non-registered symbols) run,
    /// and any abrupt closes the iterator keeping the original thrown value.
    pub(crate) fn add_entries_via_adder(
        &mut self,
        coll: Value,
        iterable: Value,
        pair: bool,
    ) -> Result<(), Thrown> {
        let adder_name = if pair { "set" } else { "add" };
        let adder = self.get_member(coll, adder_name, coll)?;
        if !self.is_callable(adder) {
            return Err(Thrown(format!("TypeError: {adder_name} is not a function")));
        }
        let _gc = self.gc_lock_guard();
        let iter = self.get_iterator_object(iterable)?;
        macro_rules! close_and_throw {
            ($e:expr) => {{
                let saved = self.pending_throw;
                let _ = self.iterator_close(iter);
                self.pending_throw = saved;
                return Err($e);
            }};
        }
        loop {
            let entry = match self.iterator_step(iter)? {
                Some(e) => e,
                None => break,
            };
            if pair {
                if !self.is_object_value(entry) {
                    let msg = self.alloc_str("Iterator value is not an entry object".to_string());
                    let te = self.make_error(1, Some(msg));
                    self.pending_throw = Some(te);
                    close_and_throw!(Thrown(
                        "TypeError: Iterator value is not an entry object".into()
                    ));
                }
                let k = match self.get_index(entry, Value::int(0)) {
                    Ok(k) => k,
                    Err(e) => close_and_throw!(e),
                };
                let v = match self.get_index(entry, Value::int(1)) {
                    Ok(v) => v,
                    Err(e) => close_and_throw!(e),
                };
                if let Err(e) = self.call_value(adder, coll, &[k, v]) {
                    close_and_throw!(e);
                }
            } else if let Err(e) = self.call_value(adder, coll, &[entry]) {
                close_and_throw!(e);
            }
        }
        Ok(())
    }

    /// IteratorClose 7.4.x. `strict` selects GetMethod semantics for the `return`
    /// method: when true (for-of/for-await break+normal, Iterator helpers), a
    /// PRESENT but non-callable `return` is a TypeError; when false (the eager
    /// destructuring drain in `iter_to_array`, which closes BEFORE the per-element
    /// defaults/targets run — so it cannot know the spec completion type yet), a
    /// non-callable `return` is skipped, preserving the original behaviour so a
    /// later default-expression throw is the value that propagates (the spec's
    /// "if completion is a throw completion, return completion").
    pub(crate) fn iterator_close_inner(&mut self, iter: Value, strict: bool) -> Result<(), Thrown> {
        if !iter.is_heap() {
            return Ok(());
        }
        // A generator's `return` resumes the suspended body with a RETURN completion
        // so any `finally` spanning the yield runs (GeneratorResume semantics) — not
        // a no-op. The {value,done} result is discarded.
        if matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. }) {
            self.generator_method(iter.heap_index(), "return", &[])?;
            return Ok(());
        }
        let ret = self.get_prop(iter, "return")?;
        if ret.is_nullish() {
            return Ok(());
        }
        if !self.is_callable(ret) {
            if strict {
                return Err(Thrown("TypeError: iterator return() is not callable".into()));
            }
            return Ok(());
        }
        let r = self.call_value(ret, iter, &[])?;
        if !self.is_object_value(r) {
            return Err(Thrown("TypeError: iterator return() result is not an object".into()));
        }
        Ok(())
    }

    /// An array's elements as ITERATION produces them, which is not the same as
    /// its backing store.
    ///
    /// The array iterator reads each index with `Get`, so a hole is not carried
    /// through as a hole: it resolves against the prototype chain (`undefined`
    /// unless someone installed an indexed property on `Array.prototype`) and
    /// the consumer stores it with CreateDataProperty. Cloning the dense store
    /// verbatim instead propagates the hole SENTINEL, and the result is a sparse
    /// array where the spec requires a dense one — observable through `in`,
    /// `hasOwnProperty`, and every method that skips holes, which is why
    /// `[...Array(3)].map((_, i) => i)` returned three holes rather than
    /// `[0,1,2]`.
    ///
    /// The scan is the whole cost in the common case: a dense array (no hole
    /// anywhere) clones and returns, exactly as before.
    pub(crate) fn spread_array_elements(&mut self, arr_idx: u32) -> Result<Vec<Value>, Thrown> {
        let items = match self.heap.get(arr_idx) {
            HeapObj::Array(items) => items.clone(),
            _ => return Ok(Vec::new()),
        };
        if !items.iter().any(|x| x.is_hole()) {
            return Ok(items);
        }
        // Resolving a hole can run a getter installed on `Array.prototype`, i.e.
        // user code that re-enters the VM while `out` holds values no root can
        // reach yet.
        let _gc = self.gc_lock_guard();
        let arr = Value::heap(arr_idx);
        let mut out = Vec::with_capacity(items.len());
        for (i, x) in items.iter().enumerate() {
            out.push(if x.is_hole() {
                // `i` is bounded by MAX_DENSE_ARRAY_LEN (2^20), so it fits an i32.
                self.get_index(arr, Value::int(i as i32))?
            } else {
                *x
            });
        }
        Ok(out)
    }

    pub(crate) fn iterate_to_vec(&mut self, v: Value) -> Result<Vec<Value>, Thrown> {
        // The accumulating result Vec holds values yielded by `.next()` that are
        // not yet reachable from the GC roots, while `.next()` (user code) keeps
        // re-entering the interpreter — suspend GC for the scope.
        let _gc = self.gc_lock_guard();
        // A TypedArray iterates positionally over its elements.
        if let Some(ta) = self.as_typed_array(v) {
            let n = match self.heap.get(ta) {
                HeapObj::TypedArray { length, .. } => *length,
                _ => 0,
            };
            return Ok((0..n).map(|i| self.ta_element_get(ta, i)).collect());
        }
        let v = self.get_iterator(v)?;
        // A generator is drained eagerly via repeated next() (spread / Array.from
        // produce a buffer; an infinite generator hangs here, matching V8).
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Generator { .. }) {
            let gidx = v.heap_index();
            let mut out = Vec::new();
            loop {
                let res = self
                    .generator_method(gidx, "next", &[])?
                    .unwrap_or(Value::UNDEFINED);
                let done = self.get_prop(res, "done")?;
                if self.truthy(done) {
                    break;
                }
                out.push(self.get_prop(res, "value")?);
                if out.len() > crate::vm::MAX_DENSE_ARRAY_LEN {
                    return Err(Thrown(
                        "RangeError: iterator produced more values than the engine's limit".into(),
                    ));
                }
            }
            return Ok(out);
        }
        // A user iterator object (one with a `next()` method) or a built-in
        // Iterator: drain it.
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_) | HeapObj::Proxy { .. } | HeapObj::Iterator { .. } | HeapObj::IterHelper { .. }) {
            let next = self.get_prop(v, "next")?;
            if self.is_callable(next) {
                let mut out = Vec::new();
                loop {
                    let res = self.call_value(next, v, &[])?;
                    // IteratorNext step 3: a non-object result is a TypeError.
                    if !self.is_object_value(res) {
                        return Err(Thrown(
                            "TypeError: iterator.next() returned a non-object".into(),
                        ));
                    }
                    let done = self.get_prop(res, "done")?;
                    if self.truthy(done) {
                        break;
                    }
                    out.push(self.get_prop(res, "value")?);
                    if out.len() > crate::vm::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: iterator produced more values than the engine's limit".into(),
                        ));
                    }
                }
                return Ok(out);
            }
        }
        enum Plan {
            Vals(Vec<Value>),
            Chars(Vec<char>),
            Pairs(Vec<(Value, Value)>),
            /// Deferred: materializing an array's elements can run a prototype
            /// getter for a hole, so it must happen outside the heap borrow.
            ArrayAt(u32),
        }
        let plan = if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(_) => Plan::ArrayAt(v.heap_index()),
                // A Set's tombstoned (deleted) slots are skipped.
                HeapObj::Set(items) => {
                    Plan::Vals(items.iter().copied().filter(|v| !v.is_hole()).collect())
                }
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Plan::Chars(self.heap.str_cow(v.heap_index()).unwrap().chars().collect())
                }
                HeapObj::Map { keys, vals } => Plan::Pairs(
                    keys.iter()
                        .copied()
                        .zip(vals.iter().copied())
                        .filter(|(k, _)| !k.is_hole())
                        .collect(),
                ),
                _ => return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v)))),
            }
        } else {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        };
        Ok(match plan {
            Plan::Vals(v) => v,
            Plan::ArrayAt(idx) => self.spread_array_elements(idx)?,
            Plan::Chars(cs) => cs.into_iter().map(|c| self.alloc_str(c.to_string())).collect(),
            Plan::Pairs(ps) => ps
                .into_iter()
                .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                .collect(),
        })
    }

    pub(crate) fn array_from(
        &mut self,
        this_ctor: Value,
        src: Value,
        mapfn: Value,
        this_arg: Value,
    ) -> Result<Value, Thrown> {
        // Holds an un-rooted `elems` Vec while the mapfn / iterator re-enters the
        // interpreter — suspend GC for the scope.
        let _gc = self.gc_lock_guard();
        // A given (non-undefined) mapfn must be callable; null/undefined source is
        // not coercible to an object (ToObject throws).
        if mapfn != Value::UNDEFINED && !self.is_callable(mapfn) {
            return Err(Thrown("TypeError: Array.from mapfn is not a function".into()));
        }
        if src.is_nullish() {
            return Err(Thrown(
                "TypeError: Array.from requires an array-like or iterable object".into(),
            ));
        }
        // usingIterator = GetMethod(items, @@iterator): one observable Get; a
        // non-callable non-nullish @@iterator is a TypeError.
        let using_iter = self.get_prop(src, "@@iterator")?;
        if !using_iter.is_nullish() && !self.is_callable(using_iter) {
            return Err(Thrown("TypeError: @@iterator is not a function".into()));
        }
        let mapping = mapfn != Value::UNDEFINED;
        // The %Array% intrinsic (or a non-constructor receiver) builds a plain
        // dense Array; any OTHER constructor receiver is constructed and
        // receives its elements via CreateDataPropertyOrThrow.
        let is_array_global = this_ctor.is_heap()
            && matches!(self.heap.get(this_ctor.heap_index()), HeapObj::Object(m)
                if m.get("prototype").is_some_and(|p| p.is_heap() && p.heap_index() == self.arr_proto));
        let custom_ctor = !is_array_global && self.is_constructor(this_ctor);
        if self.is_callable(using_iter) {
            // Iterator path, in spec order: A = Construct(C) — NO arguments —
            // BEFORE the iterator is obtained; then drive next() manually with
            // mapfn interleaved per element (its mutations of the source are
            // observed) and IteratorClose on an abrupt mapfn/define.
            let dest = if custom_ctor { Some(self.construct(this_ctor, &[])?) } else { None };
            let iter = self.call_value(using_iter, src, &[])?;
            if !self.is_object_value(iter) {
                return Err(Thrown("TypeError: iterator is not an object".into()));
            }
            let next_fn = self.get_prop(iter, "next")?;
            let mut out: Vec<Value> = Vec::new();
            let mut k: usize = 0;
            loop {
                let result = self.call_value(next_fn, iter, &[])?;
                if !self.is_object_value(result) {
                    return Err(Thrown("TypeError: iterator result is not an object".into()));
                }
                let done = self.get_prop(result, "done")?;
                if self.truthy(done) {
                    break;
                }
                let k_value = self.get_prop(result, "value")?;
                let mapped = if mapping {
                    match self.call_value(mapfn, this_arg, &[k_value, Value::num(k as f64)]) {
                        Ok(v) => v,
                        Err(e) => {
                            // IteratorClose(iteratorRecord, error): the original
                            // throw wins; a throwing return() is ignored.
                            let _ = self.iterator_close(iter);
                            return Err(e);
                        }
                    }
                } else {
                    k_value
                };
                match dest {
                    Some(a) => {
                        if let Err(e) = self.create_data_property_or_throw(a, k, mapped) {
                            let _ = self.iterator_close(iter);
                            return Err(e);
                        }
                    }
                    None => {
                        if out.len() >= crate::vm::MAX_DENSE_ARRAY_LEN {
                            return Err(Thrown(
                                "RangeError: iterator produced more values than the engine's limit"
                                    .into(),
                            ));
                        }
                        out.push(mapped);
                    }
                }
                k += 1;
            }
            return match dest {
                Some(a) => {
                    self.set_prop(a, "length", Value::num(k as f64), true)?;
                    Ok(a)
                }
                None => Ok(Value::heap(self.heap.alloc(HeapObj::Array(out)))),
            };
        }
        // Natively-iterable kinds whose prototype carries no VISIBLE @@iterator
        // in this engine keep the internal positional drain.
        if src.is_heap()
            && matches!(
                self.heap.get(src.heap_index()),
                HeapObj::Str(_)
                    | HeapObj::Cons { .. }
                    | HeapObj::Set(_)
                    | HeapObj::Map { .. }
                    | HeapObj::TypedArray { .. }
                    | HeapObj::Generator { .. }
                    | HeapObj::Iterator { .. }
                    | HeapObj::IterHelper { .. }
            )
        {
            let mut elems = self.iterate_to_vec(src)?;
            if mapping {
                for (i, slot) in elems.iter_mut().enumerate() {
                    let args = [*slot, Value::int(i as i32)];
                    *slot = self.call_value(mapfn, this_arg, &args)?;
                }
            }
            if custom_ctor {
                let len = elems.len();
                let a = self.construct(this_ctor, &[Value::num(len as f64)])?;
                for (i, v) in elems.iter().enumerate() {
                    self.create_data_property_or_throw(a, i, *v)?;
                }
                self.set_prop(a, "length", Value::num(len as f64), true)?;
                return Ok(a);
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))));
        }
        // Array-like path: arrayLike = ToObject(items); len = ToLength(Get(O,
        // 'length')); elements are read live and DEFINED on the result
        // (CreateDataPropertyOrThrow — a non-extensible receiver or a
        // non-configurable index throws; a writable:false one is redefined).
        let obj = self.to_object(src)?;
        let len_v = self.get_prop(obj, "length")?;
        let n_i = self.to_integer_or_zero(len_v)?;
        let n = if n_i > 0 { (n_i as u64).min((1u64 << 53) - 1) as usize } else { 0 };
        if custom_ctor {
            let a = self.construct(this_ctor, &[Value::num(n as f64)])?;
            for i in 0..n {
                let v = self.get_index(obj, Value::num(i as f64))?;
                let mapped = if mapping {
                    self.call_value(mapfn, this_arg, &[v, Value::num(i as f64)])?
                } else {
                    v
                };
                self.create_data_property_or_throw(a, i, mapped)?;
            }
            self.set_prop(a, "length", Value::num(n as f64), true)?;
            return Ok(a);
        }
        if n > crate::vm::MAX_DENSE_ARRAY_LEN {
            return Err(Thrown(
                "RangeError: array length exceeds the engine's dense-array limit".into(),
            ));
        }
        let mut out = Vec::with_capacity(n.min(4096));
        for i in 0..n {
            let v = self.get_index(obj, Value::num(i as f64))?;
            let mapped = if mapping {
                self.call_value(mapfn, this_arg, &[v, Value::num(i as f64)])?
            } else {
                v
            };
            out.push(mapped);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

}
