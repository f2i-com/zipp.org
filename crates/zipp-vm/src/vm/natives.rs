#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// IteratorToList with a PRE-FETCHED @@iterator method (the observable
    /// trace: no second @@iterator get; `next` fetched once and cached; per
    /// step `done` is read BEFORE `value`, and `value` is skipped once done).
    pub(crate) fn iterator_to_list(
        &mut self,
        src: Value,
        method: Value,
    ) -> Result<Vec<Value>, Thrown> {
        let iter = self.call_value(method, src, &[])?;
        if !self.is_object_value(iter) {
            return Err(Thrown("TypeError: iterator is not an object".into()));
        }
        let next = self.get_prop(iter, "next")?;
        let mut out = Vec::new();
        loop {
            let res = self.call_value(next, iter, &[])?;
            if !self.is_object_value(res) {
                return Err(Thrown("TypeError: iterator result is not an object".into()));
            }
            let done = self.get_prop(res, "done")?;
            if self.truthy(done) {
                break;
            }
            out.push(self.get_prop(res, "value")?);
        }
        Ok(out)
    }

    /// %TypedArray%.from's source step: GetMethod(source, @@iterator) once (a
    /// defined non-callable is a TypeError); an iterable source drains to a
    /// list via the pre-fetched method, while an ARRAY-LIKE source only reads
    /// ToLength(length) here — its element Gets are deferred until AFTER the
    /// target is constructed (spec step 12.b runs per-k against the target).
    fn ta_from_source(&mut self, src: Value) -> Result<(usize, Option<Vec<Value>>), Thrown> {
        if src == Value::UNDEFINED || src == Value::NULL {
            return Err(Thrown("TypeError: TypedArray.from source is null or undefined".into()));
        }
        let method = self.get_prop(src, "@@iterator")?;
        if method != Value::UNDEFINED && method != Value::NULL && !self.is_callable(method) {
            return Err(Thrown("TypeError: source is not iterable".into()));
        }
        if self.is_callable(method) {
            let list = self.iterator_to_list(src, method)?;
            return Ok((list.len(), Some(list)));
        }
        let lenv = self.get_prop(src, "length")?;
        // ToLength runs the OBSERVABLE ToNumber (a poisoned valueOf throws).
        let nf = self.to_number_coerce(lenv)?;
        let n = if nf.is_nan() || nf <= 0.0 {
            0
        } else if nf > super::typedarray::MAX_ARRAY_BUFFER_LEN as f64 {
            return Err(Thrown("RangeError: typed array length exceeds the maximum".into()));
        } else {
            nf as usize
        };
        Ok((n, None))
    }

    /// OrdinarySetWithOwnDescriptor's receiver tail for Reflect.set when the
    /// governing descriptor is a writable data property (a TypedArray element
    /// included) but the Receiver differs from its holder: CreateDataProperty
    /// on the RECEIVER with the RAW (uncoerced) value. A TypedArray receiver's
    /// canonical keys instead use its own element semantics (a valid index
    /// writes the coerced element; anything else refuses without coercing);
    /// an accessor / non-writable / non-extensible-and-absent receiver own
    /// descriptor refuses.
    fn reflect_set_on_receiver(
        &mut self,
        receiver: Value,
        kv: Value,
        value: Value,
    ) -> Result<bool, Thrown> {
        if !self.is_object_value(receiver) {
            return Ok(false);
        }
        let key = self.key_of(kv);
        let ridx = receiver.heap_index();
        if matches!(self.heap.get(ridx), HeapObj::TypedArray { .. })
            && self.is_canonical_numeric_index(&key)
        {
            return match self.ta_valid_index(ridx, &key) {
                Some(i) => {
                    self.ta_element_set(ridx, i, value)?;
                    Ok(true)
                }
                None => Ok(false),
            };
        }
        let own = match self.heap.get(ridx) {
            HeapObj::Object(m) => {
                m.pos(&key).map(|i| (m.attrs[i].accessor, m.attrs[i].writable))
            }
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) => {
                self.fn_props
                    .get(&ridx)
                    .and_then(|m| m.pos(&key).map(|i| (m.attrs[i].accessor, m.attrs[i].writable)))
            }
            _ => self
                .arr_props
                .get(&ridx)
                .and_then(|m| m.pos(&key).map(|i| (m.attrs[i].accessor, m.attrs[i].writable))),
        };
        match own {
            Some((true, _)) => Ok(false),
            Some((false, false)) => Ok(false),
            None => {
                let ext = match self.heap.get(ridx) {
                    HeapObj::Object(m) => m.extensible,
                    _ => self.arr_props.get(&ridx).map_or(true, |m| m.extensible),
                };
                if !ext {
                    return Ok(false);
                }
                self.set_index(receiver, kv, value, false)?;
                Ok(true)
            }
            _ => {
                self.set_index(receiver, kv, value, false)?;
                Ok(true)
            }
        }
    }

    /// The bare name of a callable value, for the `function <name>() { [native
    /// code] }` form of `toString`. Synthetic names (`<arrow>`, `<anonymous>`)
    /// and `Class.method` qualifiers are stripped; unknown → empty.
    pub(crate) fn callable_name(&self, v: Value) -> String {
        if !v.is_heap() {
            return String::new();
        }
        let raw: String = match self.heap.get(v.heap_index()) {
            HeapObj::Func(id) => self.func(*id as usize).name.clone(),
            HeapObj::Closure { func, .. } => self.func(*func as usize).name.clone(),
            HeapObj::Class(c) => c.name.clone(),
            HeapObj::Native(nid) => native::static_name_length(*nid)
                .map(|(n, _)| n.to_string())
                .or_else(|| native::proto_method(*nid).map(|(n, _, _)| n.to_string()))
                .unwrap_or_default(),
            _ => String::new(),
        };
        if raw.is_empty() || raw.starts_with('<') {
            String::new()
        } else if raw.contains('[') {
            // A computed "[Symbol.xxx]" name (optionally "get/set [Symbol.xxx]")
            // contains a '.' INSIDE the brackets that must NOT be treated as a
            // namespace-qualifier separator — keep it whole so toString renders
            // `function [Symbol.match]() { [native code] }` per the spec grammar.
            raw
        } else {
            raw.rsplit('.').next().unwrap_or(&raw).to_string()
        }
    }

    /// thisSymbolValue(value): the underlying Symbol primitive for a Symbol
    /// `this`, OR a boxed Symbol wrapper (`Object(sym)` → Boxed{kind:3}); else a
    /// TypeError. Lets the Symbol.prototype methods work on a wrapper receiver.
    fn this_symbol_value(&self, this: Value, method: &str) -> Result<Value, Thrown> {
        if this.is_heap() {
            match self.heap.get(this.heap_index()) {
                HeapObj::Symbol { .. } => return Ok(this),
                HeapObj::Boxed { kind: 3, value } => return Ok(*value),
                _ => {}
            }
        }
        Err(Thrown(format!(
            "TypeError: Symbol.prototype.{method} requires that 'this' be a Symbol"
        )))
    }

    /// Invoke a native (built-in) function by id with `this` and `args`. Backs
    /// first-class builtin values (`Object.defineProperty`, `Array.isArray`,
    /// `Object.prototype.hasOwnProperty`, `Function.prototype.call`, …).
    /// `$262.createRealm()` — a minimal new realm. Returns `{ global }` where
    /// `global` exposes a DISTINCT constructor object (with its own distinct
    /// `prototype`) for each intrinsic, plus fresh namespace objects. The realm
    /// constructors are recognised as constructors (`is_ctor`) so they serve as a
    /// foreign `newTarget` for `Reflect.construct`/`super` (the dominant
    /// cross-realm test shape); they are not yet independently functional.
    pub(crate) fn create_realm(&mut self) -> Value {
        let ne = PropAttr { writable: false, enumerable: false, configurable: true, accessor: false, setter: Value::UNDEFINED };
        let proto_attr = PropAttr { writable: false, enumerable: false, configurable: false, accessor: false, setter: Value::UNDEFINED };
        let data = PropAttr::data();
        // A fresh realm id; realms[r] maps each MAIN-realm intrinsic prototype to
        // this realm's corresponding prototype (for GetPrototypeFromConstructor's
        // GetFunctionRealm fallback).
        let r = self.realms.len() as u32;
        self.realms.push(std::collections::HashMap::new());
        // (constructor name, its MAIN-realm prototype heap index — 0 = no mapping)
        let mut ctors: Vec<(&str, u32)> = vec![
            ("Object", self.obj_proto), ("Array", self.arr_proto), ("Function", self.fn_proto),
            ("String", self.str_proto), ("Number", self.num_proto), ("Boolean", self.bool_proto),
            ("Symbol", self.symbol_proto), ("BigInt", self.bigint_proto),
            ("Error", self.error_protos[0]), ("TypeError", self.error_protos[1]),
            ("RangeError", self.error_protos[2]), ("SyntaxError", self.error_protos[3]),
            ("ReferenceError", self.error_protos[4]), ("EvalError", self.error_protos[5]),
            ("URIError", self.error_protos[6]), ("AggregateError", self.error_protos[7]),
            ("Map", self.map_proto), ("Set", self.set_proto), ("WeakMap", self.weakmap_proto),
            ("WeakSet", self.weakset_proto), ("WeakRef", self.weakref_proto),
            ("FinalizationRegistry", self.finreg_proto), ("RegExp", self.regexp_proto),
            ("Promise", self.promise_proto), ("Date", self.date_proto), ("Proxy", 0),
            ("ArrayBuffer", self.arraybuffer_proto), ("SharedArrayBuffer", self.sab_proto),
            ("DataView", self.dataview_proto), ("Iterator", self.iterator_proto_root),
            ("DisposableStack", self.disposablestack_proto),
            ("AsyncDisposableStack", self.asyncdisposablestack_proto),
            ("SuppressedError", self.suppressederror_proto),
            ("ShadowRealm", self.shadowrealm_proto),
        ];
        for (k, t) in native::TA_KINDS.iter().enumerate() {
            ctors.push((t.0, self.ta_protos[k]));
        }
        let mut g = ObjMap::new();
        for (name, main_proto) in ctors {
            let proto_idx = self.heap.alloc(HeapObj::Object(ObjMap::new()));
            let name_v = self.alloc_str(name.to_string());
            let mut cmap = ObjMap::new();
            cmap.is_ctor = true;
            cmap.define("prototype", Value::heap(proto_idx), proto_attr);
            cmap.define("name", name_v, ne);
            cmap.define("length", Value::int(1), ne);
            let ctor_idx = self.heap.alloc(HeapObj::Object(cmap));
            if let HeapObj::Object(pm) = self.heap.get_mut(proto_idx) {
                pm.define("constructor", Value::heap(ctor_idx), ne);
            }
            // Copy the MAIN ctor's own STATIC props (skip prototype/name/length) so
            // the realm ctor is functional (`OSymbol.for`, `OArray.from`, well-known
            // symbols, …): methods become fresh same-id Natives (distinct identity),
            // data/symbol values are shared by value.
            if let Some(&main_ctor) = self.builtin_globals.get(name) {
                // Route `new other.X()` / `other.X(...)` to the real ctor's logic.
                self.realm_ctor_main.insert(ctor_idx, main_ctor);
                let props: Vec<(String, Value, PropAttr)> = match self.heap.get(main_ctor) {
                    HeapObj::Object(mm) => mm
                        .keys
                        .iter()
                        .zip(mm.vals.iter())
                        .zip(mm.attrs.iter())
                        .filter(|((k, _), _)| {
                            k.as_str() != "prototype" && k.as_str() != "name" && k.as_str() != "length"
                        })
                        .map(|((k, v), a)| (k.clone(), *v, *a))
                        .collect(),
                    _ => Vec::new(),
                };
                for (k, v, mut a) in props {
                    let copy = self.realm_copy_value(v, r);
                    if a.accessor {
                        a.setter = self.realm_copy_value(a.setter, r);
                    }
                    if let HeapObj::Object(cm) = self.heap.get_mut(ctor_idx) {
                        cm.define(&k, copy, a);
                    }
                }
            }
            // Copy the MAIN prototype's own props (methods, accessors like the
            // RegExp flag getters or Error.prototype.stack) onto the realm
            // prototype the same way — `other.RegExp.prototype.global` etc.
            // resolve, with distinct same-behaviour identities. `constructor`
            // stays the realm constructor defined above.
            if main_proto != 0 {
                let props: Vec<(String, Value, PropAttr)> = match self.heap.get(main_proto) {
                    HeapObj::Object(mm) => mm
                        .keys
                        .iter()
                        .zip(mm.vals.iter())
                        .zip(mm.attrs.iter())
                        .filter(|((k, _), _)| k.as_str() != "constructor")
                        .map(|((k, v), a)| (k.clone(), *v, *a))
                        .collect(),
                    _ => Vec::new(),
                };
                for (k, v, mut a) in props {
                    let copy = self.realm_copy_value(v, r);
                    if a.accessor {
                        a.setter = self.realm_copy_value(a.setter, r);
                    }
                    if let HeapObj::Object(pm) = self.heap.get_mut(proto_idx) {
                        pm.define(&k, copy, a);
                    }
                }
            }
            // Tag both objects with this realm, and map the main proto → realm proto
            // so GetFunctionRealm's GetPrototypeFromConstructor fallback works.
            self.obj_realm.insert(ctor_idx, r);
            self.obj_realm.insert(proto_idx, r);
            if main_proto != 0 {
                self.realms[r as usize].insert(main_proto, proto_idx);
            }
            g.define(name, Value::heap(ctor_idx), data);
        }
        // Mirror the MAIN prototype chain structure between the realm prototypes:
        // realm `TypeError.prototype` → realm `Error.prototype`, every other realm
        // prototype → realm `Object.prototype` (which keeps its implicit link to
        // the shared %Object.prototype% — stage-1 single-intrinsics model).
        {
            let realm_obj_proto = self.realms[r as usize].get(&self.obj_proto).copied();
            let realm_err_proto = self.realms[r as usize].get(&self.error_protos[0]).copied();
            let pairs: Vec<(u32, u32)> = self.realms[r as usize]
                .iter()
                .map(|(&m, &p)| (m, p))
                .collect();
            for (main_p, realm_p) in pairs {
                if main_p == self.obj_proto {
                    continue;
                }
                let target = if self.error_protos[1..].contains(&main_p) {
                    realm_err_proto
                } else {
                    realm_obj_proto
                };
                if let Some(t) = target {
                    self.proto_of.insert(realm_p, Value::heap(t));
                }
            }
        }
        // Namespace objects: fresh per realm, with the main namespace's props
        // copied (fresh same-id Natives / shared data) so `other.Reflect.construct`
        // etc. are functional.
        for ns in ["Math", "JSON", "Reflect", "Atomics", "Intl"] {
            let mut m = ObjMap::new();
            if let Some(&main_ns) = self.builtin_globals.get(ns) {
                let props: Vec<(String, Value, PropAttr)> = match self.heap.get(main_ns) {
                    HeapObj::Object(mm) => mm
                        .keys
                        .iter()
                        .zip(mm.vals.iter())
                        .zip(mm.attrs.iter())
                        .map(|((k, v), a)| (k.clone(), *v, *a))
                        .collect(),
                    _ => Vec::new(),
                };
                for (k, v, mut a) in props {
                    let copy = self.realm_copy_value(v, r);
                    if a.accessor {
                        a.setter = self.realm_copy_value(a.setter, r);
                    }
                    m.define(&k, copy, a);
                }
            }
            let o_idx = self.heap.alloc(HeapObj::Object(m));
            self.obj_realm.insert(o_idx, r);
            g.define(ns, Value::heap(o_idx), data);
        }
        // Bare global functions (`other.parseInt`, …) as fresh same-id Natives,
        // and the global value properties (non-writable, non-configurable).
        for name in [
            "parseInt", "parseFloat", "isNaN", "isFinite", "encodeURI",
            "encodeURIComponent", "decodeURI", "decodeURIComponent", "escape",
            "unescape", "print",
        ] {
            if let Some(&main_fn) = self.builtin_globals.get(name) {
                let copy = self.realm_copy_value(Value::heap(main_fn), r);
                self.obj_realm.insert(copy.heap_index(), r);
                g.define(name, copy, data);
            }
        }
        let frozen = PropAttr { writable: false, enumerable: false, configurable: false, accessor: false, setter: Value::UNDEFINED };
        g.define("undefined", Value::UNDEFINED, frozen);
        g.define("NaN", Value::num(f64::NAN), frozen);
        g.define("Infinity", Value::num(f64::INFINITY), frozen);
        let g_idx = self.heap.alloc(HeapObj::Object(g));
        self.obj_realm.insert(g_idx, r);
        // Register as a child-realm global: keys the realm's own binding table
        // (active_realm / realm_globals) and the get/set interception that makes
        // `other.x` read/write the same binding a bare `x` sees inside the realm.
        self.realm_global_objs.insert(g_idx, r);
        // The realm's own `eval`: a distinct function object whose call runs the
        // code against THIS realm's global bindings (call_value intercepts it).
        let eval_idx = self.heap.alloc(HeapObj::Native(native::GLOBAL_EVAL));
        self.obj_realm.insert(eval_idx, r);
        self.realm_fns.insert(eval_idx, (g_idx, 0));
        // `globalThis` is the realm's global object itself.
        if let HeapObj::Object(gm) = self.heap.get_mut(g_idx) {
            gm.define("globalThis", Value::heap(g_idx), data);
            gm.define("eval", Value::heap(eval_idx), data);
        }
        // The realm object mirrors the $262 surface (agent/gc/createRealm/… are
        // realm-agnostic and shared by copy), with a realm-bound `evalScript`
        // and this realm's `global`.
        let mut realm = ObjMap::new();
        if self.dollar262 != 0 {
            let props: Vec<(String, Value, PropAttr)> = match self.heap.get(self.dollar262) {
                HeapObj::Object(mm) => mm
                    .keys
                    .iter()
                    .zip(mm.vals.iter())
                    .zip(mm.attrs.iter())
                    .filter(|((k, _), _)| k.as_str() != "global" && k.as_str() != "evalScript")
                    .map(|((k, v), a)| (k.clone(), *v, *a))
                    .collect(),
                _ => Vec::new(),
            };
            for (k, v, a) in props {
                let copy = self.realm_copy_value(v, r);
                realm.define(&k, copy, a);
            }
        }
        let script_idx = self.heap.alloc(HeapObj::Native(native::DOLLAR262_EVAL_SCRIPT));
        self.obj_realm.insert(script_idx, r);
        self.realm_fns.insert(script_idx, (g_idx, 1));
        realm.define("evalScript", Value::heap(script_idx), PropAttr::data());
        realm.define("global", Value::heap(g_idx), data);
        Value::heap(self.heap.alloc(HeapObj::Object(realm)))
    }

    /// The GLOBAL object of createRealm child `r` (None for the main realm or a
    /// realm id with no registered global — e.g. a pre-stage-1 tag).
    pub(crate) fn realm_global_obj(&self, r: u32) -> Option<u32> {
        if r == 0 {
            return None;
        }
        self.realm_global_objs
            .iter()
            .find(|&(_, &rid)| rid == r)
            .map(|(&g, _)| g)
    }

    /// Whether `main` is one of the dynamic-function constructors — a realm
    /// facade routing to it must compile the source with the realm ACTIVE so the
    /// new function's globals bind in the realm's own table.
    pub(crate) fn realm_main_ctor_is_fn_like(&self, main: u32) -> bool {
        main != 0
            && [self.function_ctor, self.gen_fn_ctor, self.async_fn_ctor, self.asyncgen_fn_ctor]
                .contains(&main)
    }

    /// The createRealm child whose code is CURRENTLY running: the `active_realm`
    /// switch (a realm `eval`/`Function` compile+run) or the realm tag of the
    /// innermost frame's callee — i.e. the spec's "current Realm Record",
    /// approximated. `None` in the main realm (the common case: one map check).
    pub(crate) fn current_realm_id(&self) -> Option<u32> {
        if self.realm_global_objs.is_empty() {
            return None;
        }
        if let Some(rid) = self.active_realm {
            if let Some(&r) = self.realm_global_objs.get(&rid) {
                return Some(r);
            }
        }
        if let Some(f) = self.frames.last() {
            let r = self.get_function_realm(f.callee);
            if r != 0 {
                return Some(r);
            }
        }
        None
    }

    /// The %ThrowTypeError% intrinsic of createRealm child `r` — ONE per realm
    /// (`gOPD(args, 'callee').get` is identical across that realm's strict
    /// arguments objects, distinct from the main realm's), realm-tagged so
    /// calling it throws the CHILD's TypeError. Lazily created, cached + rooted.
    pub(crate) fn realm_throw_type_error(&mut self, r: u32) -> Value {
        if let Some(&i) = self.realm_throw_type_errors.get(&r) {
            return Value::heap(i);
        }
        let idx = self.heap.alloc(HeapObj::Native(native::FN_THROW_TYPE_ERROR));
        self.obj_realm.insert(idx, r);
        self.realm_throw_type_errors.insert(r, idx);
        Value::heap(idx)
    }

    /// The HOME object of the built-in currently executing via `call_value`'s
    /// native dispatch: a realm-COPIED native's home is its realm's image of
    /// `main_proto` (`this === %RegExp.prototype%`-style checks), else the main
    /// intrinsic itself.
    #[inline]
    pub(crate) fn native_home(&self, main_proto: u32) -> u32 {
        if let Some(r) = self.native_callee_realm {
            if let Some(&rp) = self.realms.get(r as usize).and_then(|m| m.get(&main_proto)) {
                return rp;
            }
        }
        main_proto
    }

    /// The CURRENT realm's image of a main intrinsic prototype: primitive member
    /// access inside a createRealm child resolves through the CHILD's prototype
    /// (`other.Number.prototype.x` is visible to `(1).x` inside the child).
    /// Identity in the main realm — the common case is one is_empty check.
    #[inline]
    pub(crate) fn active_realm_proto(&self, main_proto: u32) -> u32 {
        if !self.realm_global_objs.is_empty() {
            if let Some(r) = self.current_realm_id() {
                if let Some(&rp) = self.realms.get(r as usize).and_then(|m| m.get(&main_proto)) {
                    return rp;
                }
            }
        }
        main_proto
    }

    /// Tag a freshly-created function/class with the realm whose code created it
    /// (no-op in the main realm) — functions born inside a child realm's
    /// eval/Function code carry the child's realm for GetFunctionRealm,
    /// OrdinaryCallBindThis, and error-identity purposes.
    #[inline]
    pub(crate) fn realm_tag_new(&mut self, idx: u32) {
        if !self.realm_global_objs.is_empty() {
            if let Some(r) = self.current_realm_id() {
                self.obj_realm.insert(idx, r);
            }
        }
    }

    /// Re-link a freshly-synthesised internal error to the CURRENT realm's facade
    /// prototype (`thrown.constructor === otherRealm.TypeError`): a throw
    /// materializes while the throwing function's frame is still on the stack,
    /// so the frame-derived realm IS the spec's current realm. No-op in the main
    /// realm, or when the error's prototype has no realm image.
    pub(crate) fn realm_adopt_error(&mut self, err: Value) {
        if self.realm_global_objs.is_empty() || !err.is_heap() {
            return;
        }
        let Some(r) = self.current_realm_id() else { return };
        self.realm_adopt_error_to(err, r);
    }

    /// `realm_adopt_error` with an explicit target realm (used where the FUNCTION
    /// whose call failed is known, e.g. calling a realm class without `new`).
    pub(crate) fn realm_adopt_error_to(&mut self, err: Value, r: u32) {
        if !err.is_heap() {
            return;
        }
        let idx = err.heap_index();
        let main_p = match self.proto_of.get(&idx) {
            Some(v) if v.is_heap() => v.heap_index(),
            _ => return,
        };
        if let Some(&rp) = self.realms.get(r as usize).and_then(|m| m.get(&main_p)) {
            self.proto_of.insert(idx, Value::heap(rp));
            self.obj_realm.insert(idx, r);
        }
    }

    /// After boxing a primitive `this` for a sloppy CALLEE from a child realm,
    /// re-link the wrapper to the realm's prototype (`func.call(true)` inside a
    /// realm function sees a Boolean whose `.constructor` is the realm's).
    pub(crate) fn realm_retag_boxed(&mut self, callee: Value, boxed: Value) {
        if self.realm_global_objs.is_empty() || !boxed.is_heap() {
            return;
        }
        let r = self.get_function_realm(callee);
        if r != 0 {
            self.realm_box_proto(boxed, r);
        }
    }

    /// Re-link a primitive wrapper created BY realm `r`'s machinery (sloppy
    /// `this` boxing, `other.Object(bigint)`) to the realm's wrapper prototype.
    pub(crate) fn realm_box_proto(&mut self, boxed: Value, r: u32) {
        if !boxed.is_heap() {
            return;
        }
        let main_p = match self.heap.get(boxed.heap_index()) {
            HeapObj::Boxed { kind: 0, .. } => self.str_proto,
            HeapObj::Boxed { kind: 1, .. } => self.num_proto,
            HeapObj::Boxed { kind: 2, .. } => self.bool_proto,
            HeapObj::Boxed { kind: 3, .. } => self.symbol_proto,
            HeapObj::Boxed { kind: 4, .. } => self.bigint_proto,
            _ => return,
        };
        if let Some(&rp) = self.realms.get(r as usize).and_then(|m| m.get(&main_p)) {
            self.proto_of.insert(boxed.heap_index(), Value::heap(rp));
            self.obj_realm.insert(boxed.heap_index(), r);
        }
    }

    /// `this` for OrdinaryCallBindThis's global substitution: the CALLEE realm's
    /// global object (a createRealm child function binds the CHILD's global).
    pub(crate) fn callee_this_global(&self, callee: Value) -> u32 {
        if !self.realm_global_objs.is_empty() {
            let r = self.get_function_realm(callee);
            if r != 0 {
                if let Some(g) = self.realm_global_obj(r) {
                    return g;
                }
            }
        }
        self.global_this
    }

    /// A child realm's `eval(src)` / `evalScript(src)` (`kind` 0 / 1): compile +
    /// run `src` with the child ACTIVE, so global reads/writes inside it resolve
    /// to the child's binding table and new declarations land on its global.
    /// Mirrors the main GLOBAL_EVAL/DOLLAR262_EVAL_SCRIPT natives otherwise.
    pub(crate) fn realm_eval_call(
        &mut self,
        gidx: u32,
        kind: u8,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let is_str = a0.is_heap()
            && matches!(self.heap.get(a0.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. });
        if !is_str {
            // eval(x): a non-String x is returned unchanged (spec 19.2.1).
            return Ok(a0);
        }
        let code = self.display(a0);
        let exact = self.heap.str_exact_if_not_wellformed(a0.heap_index());
        let prev_realm = self.active_realm;
        self.active_realm = Some(gidx);
        // Both kinds run the EVAL pipeline (global sloppy scope, var/function
        // declarations land on the realm global via var_env_global): unlike the
        // main `$262.evalScript` program path it returns the COMPLETION VALUE,
        // which realm tests rely on (`other.evalScript('(function(){…})')`).
        let _ = kind;
        let evaled = self.do_eval(&code, false, false, None, None, false, false, Value::UNDEFINED, None, true, None, Vec::new(), None, None, exact.as_deref());
        self.active_realm = prev_realm;
        let v = evaled?;
        // The completion value (typically a function/class the caller will use)
        // carries the realm tag for GetFunctionRealm / error identity.
        if v.is_heap() && self.is_callable(v) {
            if let Some(&r) = self.realm_global_objs.get(&gidx) {
                self.obj_realm.insert(v.heap_index(), r);
            }
        }
        Ok(v)
    }

    /// Copy a value into realm `r`: a built-in method (`HeapObj::Native`) becomes
    /// a FRESH Native with the same id (distinct identity, identical behaviour —
    /// so `OSymbol.for !== Symbol.for` while the shared registry still works),
    /// realm-TAGGED so GetFunctionRealm and error-identity adoption see the
    /// child; everything else (well-known symbol values, data) is shared by value.
    fn realm_copy_value(&mut self, v: Value, r: u32) -> Value {
        if v.is_heap() {
            if let HeapObj::Native(id) = self.heap.get(v.heap_index()) {
                let id = *id;
                let idx = self.heap.alloc(HeapObj::Native(id));
                self.obj_realm.insert(idx, r);
                return Value::heap(idx);
            }
        }
        v
    }

    /// GetFunctionRealm — the realm id a constructor/object belongs to (0 = main).
    /// Pierces BOUND functions and PROXIES (the spec recurses to the target —
    /// before the tag check, since `new other.Proxy(mainTarget, …)` realm-tags
    /// the proxy object itself while its function realm is the TARGET's).
    pub(crate) fn get_function_realm(&self, f: Value) -> u32 {
        if f.is_heap() {
            match self.heap.get(f.heap_index()) {
                HeapObj::Bound { target, .. } | HeapObj::Proxy { target, .. } => {
                    return self.get_function_realm(*target);
                }
                _ => {}
            }
            self.obj_realm.get(&f.heap_index()).copied().unwrap_or(0)
        } else {
            0
        }
    }

    pub(crate) fn call_native(&mut self, id: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        use native::*;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        // Temporal prototype field getter: brand-check `this` is a Temporal
        // instance, then read the field (the fast get_member path computes it).
        if (native::TEMPORAL_GETTER_BASE
            ..native::TEMPORAL_GETTER_BASE + native::TEMPORAL_GETTER_FIELDS.len() as u16)
            .contains(&id)
        {
            if !matches!(
                this.is_heap().then(|| self.heap.get(this.heap_index())),
                Some(HeapObj::Temporal { .. })
            ) {
                return Err(Thrown(
                    "TypeError: Temporal field getter called on a non-Temporal receiver".into(),
                ));
            }
            let field = native::TEMPORAL_GETTER_FIELDS[(id - native::TEMPORAL_GETTER_BASE) as usize];
            return self.get_prop(this, field);
        }
        Ok(match id {
            OBJ_DEFINE_PROPERTY => {
                let key = self.to_property_key(a1)?;
                self.object_define_property(a0, &key, args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
                a0
            }
            OBJ_DEFINE_PROPERTIES => {
                // Object.defineProperties(O, Properties): Type(O) must be Object
                // (a number/string/bool primitive throws, not just null/undefined).
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Object.defineProperties called on non-object".into(),
                    ));
                }
                self.object_define_properties(a0, a1)?;
                a0
            }
            OBJ_GET_OWN_DESC => {
                let key = self.to_property_key(a1)?;
                self.require_object_coercible(a0)?; // ToObject(O)
                let o = self.to_object(a0)?;
                self.ns_tdz_check(o, &key)?;
                match self.proxy_gopd(o, &key)? {
                    Some(d) => d,
                    None => self.object_get_own_property_descriptor(o, &key),
                }
            }
            OBJ_GET_OWN_NAMES => {
                self.require_object_coercible(a0)?; // ToObject(O)
                let o = self.to_object(a0)?;
                self.object_own_property_names(o)?
            }
            OBJ_GET_PROTO => {
                self.require_object_coercible(a0)?; // ToObject(O): null/undefined throw
                let o = self.to_object(a0)?;
                self.get_prototype_of_checked(o)?
            }
            OBJ_KEYS => {
                self.require_object_coercible(a0)?; // ToObject(O)
                let o = self.to_object(a0)?;
                self.object_enum_own(o, EnumWhat::Keys)?
            }
            OBJ_VALUES => {
                self.require_object_coercible(a0)?;
                let o = self.to_object(a0)?;
                self.object_enum_own(o, EnumWhat::Values)?
            }
            OBJ_ENTRIES => {
                self.require_object_coercible(a0)?;
                let o = self.to_object(a0)?;
                self.object_enum_own(o, EnumWhat::Entries)?
            }
            OBJ_ASSIGN => self.object_assign(args)?,
            OBJ_CREATE => {
                // Object.create(O, Properties): O must be Object or null.
                if a0 != Value::NULL && !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Object prototype may only be an Object or null".into(),
                    ));
                }
                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                if a0 != Value::UNDEFINED {
                    self.proto_of.insert(o.heap_index(), a0);
                }
                if a1 != Value::UNDEFINED {
                    self.object_define_properties(o, a1)?;
                }
                o
            }
            PROTO_HAS_OWN => {
                let k = self.to_property_key(a0)?;
                self.require_object_coercible(this)?; // ToObject(this)
                Value::bool(self.has_own_property_dyn(this, &k)?)
            }
            PROTO_PROP_ENUM => {
                let k = self.to_property_key(a0)?;
                self.require_object_coercible(this)?; // ToObject(this)
                self.ns_tdz_check(this, &k)?; // [[GetOwnProperty]] of uninit throws
                // On a Proxy, [[GetOwnProperty]] runs the gopd trap (user JS), so
                // own_is_enumerable (&self) can't reach it — consult proxy_gopd.
                if this.is_heap() && self.proxy_parts(this.heap_index()).is_some() {
                    let en = match self.proxy_gopd(this, &k)? {
                        Some(desc) => {
                            let e = self.get_prop(desc, "enumerable")?;
                            self.truthy(e)
                        }
                        None => false,
                    };
                    Value::bool(en)
                } else {
                    Value::bool(self.own_is_enumerable(this, &k))
                }
            }
            // isPrototypeOf: a non-object argument is `false` BEFORE ToObject(this).
            PROTO_IS_PROTO_OF => {
                if self.is_object_value(a0) {
                    self.require_object_coercible(this)?;
                }
                Value::bool(self.is_prototype_of(this, a0))
            }
            PROTO_VALUE_OF => {
                // Object.prototype.valueOf returns ToObject(this): null/undefined
                // throw a TypeError, and a primitive is boxed into its wrapper object
                // (so `Object.prototype.valueOf.call(true)` is a Boolean object).
                self.require_object_coercible(this)?;
                self.to_object(this)?
            }
            PROTO_TO_STRING => {
                let tag = self.object_to_string_tag(this)?;
                self.alloc_str(format!("[object {tag}]"))
            }
            ERROR_TO_STRING => {
                // Step 2: a non-Object receiver is a TypeError (before any Get).
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: Error.prototype.toString called on a non-object".into(),
                    ));
                }
                // `name` (default "Error") + ": " + `message` (default ""), dropping
                // the separator when either part is empty.
                let nv = self.get_prop(this, "name")?;
                let name =
                    if nv == Value::UNDEFINED { "Error".to_string() } else { self.to_js_string(nv)? };
                let mv = self.get_prop(this, "message")?;
                let msg = if mv == Value::UNDEFINED { String::new() } else { self.to_js_string(mv)? };
                let s = if name.is_empty() {
                    msg
                } else if msg.is_empty() {
                    name
                } else {
                    format!("{name}: {msg}")
                };
                self.alloc_str(s)
            }
            SYMBOL_TO_STRING => {
                // `Symbol.prototype.toString` → "Symbol(description)".
                let sym = self.this_symbol_value(this, "toString")?;
                let desc = match self.heap.get(sym.heap_index()) {
                    HeapObj::Symbol { desc, .. } => *desc,
                    _ => Value::UNDEFINED,
                };
                let d = if desc == Value::UNDEFINED { String::new() } else { self.display(desc) };
                self.alloc_str(format!("Symbol({d})"))
            }
            SYMBOL_VALUE_OF => {
                // `Symbol.prototype.valueOf` → the Symbol primitive itself.
                self.this_symbol_value(this, "valueOf")?
            }
            SYMBOL_TO_PRIMITIVE => {
                // `Symbol.prototype[Symbol.toPrimitive](hint)` → the Symbol itself.
                self.this_symbol_value(this, "[Symbol.toPrimitive]")?
            }
            FN_HAS_INSTANCE => {
                // `Function.prototype[Symbol.hasInstance](V)` → OrdinaryHasInstance.
                Value::bool(self.ordinary_has_instance(this, a0)?)
            }
            FN_THROW_TYPE_ERROR => {
                // %ThrowTypeError%: the restricted caller/arguments accessor — read
                // OR write of Function.prototype.caller/arguments throws here.
                return Err(Thrown(
                    "TypeError: 'caller' and 'arguments' may not be accessed on this function".into(),
                ));
            }
            FINALLY_THEN | FINALLY_CATCH => {
                // ThenFinally/CatchFinally (bound to [onFinally, C]); invoked by the
                // receiver's `then` with the fulfilment value / rejection reason as
                // the trailing arg. Per spec: result = onFinally(); promise =
                // PromiseResolve(C, result); return promise.then(thunk), where the
                // thunk passes the original value through (then) or re-throws the
                // original reason (catch) once `onFinally`'s result settles.
                let on_finally = args.first().copied().unwrap_or(Value::UNDEFINED);
                let ctor = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let carried = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                // Hold un-rooted Values across the onFinally / then re-entry.
                let _gc = self.gc_lock_guard();
                let result = self.call_value(on_finally, Value::UNDEFINED, &[])?;
                let promise = self.call_native(PROMISE_RESOLVE, ctor, &[result])?;
                let thunk_id = if id == FINALLY_THEN { FINALLY_VALUE_THUNK } else { FINALLY_THROWER };
                let thunk_fn = Value::heap(self.heap.alloc(HeapObj::Native(thunk_id)));
                let thunk = Value::heap(self.heap.alloc(HeapObj::Bound {
                    target: thunk_fn,
                    this: Value::UNDEFINED,
                    args: vec![carried],
                }));
                let then_fn = self.get_prop(promise, "then")?;
                self.call_value(then_fn, promise, &[thunk])?
            }
            FINALLY_VALUE_THUNK => {
                // `() => value`: ignores its call argument, returns the bound value.
                args.first().copied().unwrap_or(Value::UNDEFINED)
            }
            FINALLY_THROWER => {
                // `() => { throw reason }`: re-throws the bound reason value.
                let reason = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.pending_throw = Some(reason);
                return Err(Thrown(self.throw_message(reason)));
            }
            DATE_TO_PRIMITIVE => {
                // `Date.prototype[Symbol.toPrimitive](hint)`: O must be an Object.
                // hint "string"/"default" → OrdinaryToPrimitive(O, "string"),
                // hint "number" → OrdinaryToPrimitive(O, "number"); any other hint
                // (including a non-string value) is a TypeError. The hint is matched
                // by exact string value (no coercion), per spec.
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: Date.prototype[Symbol.toPrimitive] called on a non-object".into(),
                    ));
                }
                let hint = args.first().copied().unwrap_or(Value::UNDEFINED);
                let hint_s = if hint.is_heap() && self.heap.is_str_like(hint.heap_index()) {
                    self.to_js_string(hint)?
                } else {
                    String::new()
                };
                let order: [&str; 2] = match hint_s.as_str() {
                    "string" | "default" => ["toString", "valueOf"],
                    "number" => ["valueOf", "toString"],
                    _ => {
                        return Err(Thrown(
                            "TypeError: Date.prototype[Symbol.toPrimitive] called with an invalid hint"
                                .into(),
                        ))
                    }
                };
                self.ordinary_to_primitive(this, order)?
            }
            DATE_TO_JSON => {
                // Date.prototype.toJSON(key): generic, NOT Date-branded.
                //   O = ToObject(this); tv = ToPrimitive(O, number);
                //   if tv is a non-finite Number, return null;
                //   return Invoke(O, "toISOString").
                let o = self.to_object(this)?;
                let tv = self.to_primitive_number(o)?;
                if tv.is_double() && !tv.as_f64().is_finite() {
                    Value::NULL
                } else {
                    let m = self.get_prop(o, "toISOString")?;
                    if !self.is_callable(m) {
                        return Err(Thrown(
                            "TypeError: Date.prototype.toJSON: toISOString is not callable".into(),
                        ));
                    }
                    self.call_value(m, o, &[])?
                }
            }
            SYMBOL_DESCRIPTION_GET => {
                // `get Symbol.prototype.description` → the symbol's description.
                let sym = self.this_symbol_value(this, "description")?;
                match self.heap.get(sym.heap_index()) {
                    HeapObj::Symbol { desc, .. } => *desc,
                    _ => Value::UNDEFINED,
                }
            }
            STR_ITERATOR => {
                // `String.prototype[Symbol.iterator]()` — RequireObjectCoercible +
                // ToString, then a String Iterator yielding one code POINT at a time.
                if this.is_nullish() {
                    return Err(Thrown(
                        "TypeError: String.prototype[Symbol.iterator] called on null or undefined"
                            .into(),
                    ));
                }
                // A string receiver iterates its EXACT code points (a lone
                // surrogate yields a real 1-unit surrogate string); other
                // receivers coerce through ToString (observable, may throw).
                let cp_list: Vec<u32> =
                    if this.is_heap() && self.heap.is_str_like(this.heap_index()) {
                        let si = this.heap_index();
                        self.heap.flatten(si);
                        match self.heap.get(si) {
                            HeapObj::Str(js) => js.code_points().collect(),
                            _ => Vec::new(),
                        }
                    } else {
                        let s = self.to_js_string(this)?;
                        s.chars().map(|c| c as u32).collect()
                    };
                let cps: Vec<Value> = cp_list.into_iter().map(|cp| self.str_from_cp(cp)).collect();
                self.make_iterator(cps, self.string_iter_proto)
            }
            SYMBOL_FOR => {
                // `Symbol.for(key)`: shared registry symbol for the ToString(key).
                let key = self.to_js_string(a0)?;
                if let Some(&sym) = self.symbol_registry.get(&key) {
                    sym
                } else {
                    let desc = self.alloc_str(key.clone());
                    let prop_key = format!("@@for:{key}");
                    let sym = self.make_named_symbol(desc, &prop_key);
                    self.symbol_registry.insert(key, sym);
                    sym
                }
            }
            SYMBOL_KEY_FOR => {
                // `Symbol.keyFor(sym)`: the registry key for a registered symbol, else undefined.
                if !matches!(
                    a0.is_heap().then(|| self.heap.get(a0.heap_index())),
                    Some(HeapObj::Symbol { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: Symbol.keyFor requires that the argument be a Symbol".into(),
                    ));
                }
                let key =
                    self.symbol_registry.iter().find(|(_, v)| v.bits() == a0.bits()).map(|(k, _)| k.clone());
                match key {
                    Some(k) => self.alloc_str(k),
                    None => Value::UNDEFINED,
                }
            }
            BIGINT_TO_STRING => {
                let n = match self.this_bigint_val(this) {
                    Some(n) => n,
                    None => {
                        return Err(Thrown(
                            "TypeError: BigInt.prototype.toString requires that 'this' be a BigInt".into(),
                        ))
                    }
                };
                // radix = ToIntegerOrInfinity(arg): ToNumber first, so a BigInt or
                // Symbol radix is a TypeError (the lenient `to_number` would accept a
                // BigInt). The this-value's brand check above still comes first.
                let radix = if a0 == Value::UNDEFINED { 10 } else { self.to_number_strict(a0)? as i64 };
                if !(2..=36).contains(&radix) {
                    return Err(Thrown("RangeError: toString() radix must be between 2 and 36".into()));
                }
                self.alloc_str(n.to_radix_string(radix as u32))
            }
            BIGINT_VALUE_OF => {
                match self.this_bigint_val(this) {
                    Some(n) => self.make_bigint_val(n),
                    None => {
                        return Err(Thrown(
                            "TypeError: BigInt.prototype.valueOf requires that 'this' be a BigInt".into(),
                        ))
                    }
                }
            }
            BIGINT_AS_INTN | BIGINT_AS_UINTN => {
                // bits = ToIndex(bits) (RangeError if negative / > 2^53-1) FIRST,
                // then x = ToBigInt(value) — STRICT: a Number value is a TypeError
                // (unlike the lenient BigInt() ctor coercion `to_bigint` allows).
                let bits = self.to_index(a0)?;
                if a1.is_number() {
                    return Err(Thrown(
                        "TypeError: cannot convert a Number to a BigInt".into(),
                    ));
                }
                let x = self.to_bigint(a1)?;
                if id == BIGINT_AS_INTN {
                    self.bigint_as_intn_val(bits as u64, x)?
                } else {
                    self.bigint_as_uintn_val(bits as u64, x)?
                }
            }
            REGEXP_EXEC => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                    ));
                }
                self.regexp_exec(this.heap_index(), a0)?
            }
            REGEXP_TEST => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.test called on a non-RegExp".into(),
                    ));
                }
                let r = self.regexp_exec(this.heap_index(), a0)?;
                Value::bool(r != Value::NULL)
            }
            REGEXP_COMPILE => {
                // RegExp.prototype.compile(pattern, flags): recompile in place.
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.compile called on a non-RegExp".into(),
                    ));
                }
                // AnnexB: compile is a LEGACY feature — an instance created by a
                // AnnexB step 1c: SameRealm(O, this compile function) — a
                // receiver from another realm is a TypeError (thrown with the
                // FUNCTION's realm identity via the call_value adoption).
                if !self.realm_global_objs.is_empty()
                    && self.get_function_realm(this) != self.native_callee_realm.unwrap_or(0)
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.compile: receiver belongs to another realm".into(),
                    ));
                }
                // RegExp SUBCLASS (its [[Prototype]] is not %RegExp.prototype% —
                // the COMPILE function's own realm's) throws TypeError rather
                // than recompiling.
                if self
                    .proto_of
                    .get(&this.heap_index())
                    .map_or(false, |p| {
                        !p.is_heap() || p.heap_index() != self.native_home(self.regexp_proto)
                    })
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.compile may not be used on a subclass instance".into(),
                    ));
                }
                // AnnexB step 3: if pattern has [[RegExpMatcher]], flags must be
                // undefined (TypeError) — never stringified into a flags string.
                if matches!(
                    a0.is_heap().then(|| self.heap.get(a0.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) && a1 != Value::UNDEFINED
                {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.compile: flags must be undefined when pattern is a RegExp".into(),
                    ));
                }
                // Reuse the constructor path (validates flags, builds the matcher),
                // then move the freshly built fields into the receiver.
                let built = self.build_regexp(a0, a1)?;
                let (source, flags) = match self.heap.get(built.heap_index()) {
                    HeapObj::RegExp { source, flags, .. } => (source.clone(), flags.clone()),
                    _ => unreachable!(),
                };
                // EXACT pattern bytes (lone surrogates): `build_regexp` recorded
                // them for the throwaway `built` — take the entry so it can move
                // to the receiver below (the struct's `source` is lossy).
                let exact = self.regexp_exact_source.remove(&built.heap_index());
                // Rebuild the matcher from the validated source/flags. Pass `u`
                // (Unicode) and `v` (UnicodeSets) through verbatim — `regress` reads
                // them as distinct grammars (kept in sync with `build_regexp`).
                let mut rflags = String::new();
                for c in flags.chars() {
                    match c {
                        'i' | 'm' | 's' | 'u' | 'v' => rflags.push(c),
                        _ => {}
                    }
                }
                // Pattern characters: code points in `u`/`v` mode, UTF-16 code
                // units otherwise (an astral literal is its two surrogate
                // halves; group names recombine pairs) — kept in sync with
                // `build_regexp`, including the exact-bytes (lone-surrogate)
                // form, which feeds the surrogates verbatim.
                let unicode_mode = flags.contains('u') || flags.contains('v');
                let compile_cps: Vec<u32> = match (&exact, unicode_mode) {
                    (Some(b), true) => crate::heap::wtf8_code_points(b).collect(),
                    (Some(b), false) => super::proxy_regexp::nonunicode_pattern_chars(
                        &crate::heap::wtf8_units_iter(b).collect::<Vec<u16>>(),
                    ),
                    (None, true) => source.chars().map(u32::from).collect(),
                    (None, false) => super::proxy_regexp::nonunicode_pattern_chars(
                        &source.encode_utf16().collect::<Vec<u16>>(),
                    ),
                };
                let regex = regress::Regex::from_unicode(compile_cps.iter().copied(), rflags.as_str())
                    .map_err(|e| {
                        Thrown(format!("SyntaxError: Invalid regular expression: /{source}/: {e}"))
                    })?;
                if let HeapObj::RegExp { regex: r, source: s, flags: fl, .. } =
                    self.heap.get_mut(this.heap_index())
                {
                    *r = Box::new(regex);
                    *s = source;
                    *fl = flags;
                }
                // Move the exact-source entry onto the receiver — or clear a
                // stale one when the new pattern is well-formed.
                match exact {
                    Some(b) => {
                        self.regexp_exact_source.insert(this.heap_index(), b);
                    }
                    None => {
                        self.regexp_exact_source.remove(&this.heap_index());
                    }
                }
                // RegExpInitialize step 12: Set(obj, "lastIndex", 0, true) AFTER
                // the slots update — a non-writable lastIndex throws TypeError
                // with source/flags already recompiled.
                self.set_prop(this, "lastIndex", Value::int(0), true)?;
                this
            }
            REGEXP_ESCAPE => {
                // RegExp.escape(S): escape S so it matches itself literally. Throws
                // TypeError unless S is a String (no coercion).
                if !(a0.is_heap() && self.heap.is_str_like(a0.heap_index())) {
                    return Err(Thrown(
                        "TypeError: RegExp.escape called with a non-string argument".into(),
                    ));
                }
                // EXACT code points (the argument is a string by the check
                // above): a lone surrogate must escape as \uNNNN, which the
                // lossy &str view could never carry.
                let bytes: Vec<u8> =
                    self.heap.str_wtf8_cow(a0.heap_index()).unwrap().into_owned();
                // EncodeForRegExpEscape's "other punctuators" / WhiteSpace /
                // LineTerminator / lone-surrogate set: hex-escaped (\xNN if <=0xFF,
                // else \uNNNN per UTF-16 code unit). Tab/VT/FF/LF/CR use the control
                // escapes below, so they are excluded here.
                let other = |u: u32| -> bool {
                    matches!(
                        u,
                        // ,-=<>#&!%:;@~'`"
                        0x2c | 0x2d | 0x3d | 0x3c | 0x3e | 0x23 | 0x26 | 0x21 | 0x25 | 0x3a
                            | 0x3b | 0x40 | 0x7e | 0x27 | 0x60 | 0x22
                        // WhiteSpace (minus tab/VT/FF) + ZWNBSP
                            | 0x20 | 0xA0 | 0x1680 | 0x202F | 0x205F | 0x3000 | 0xFEFF
                        // LineTerminator (minus LF/CR)
                            | 0x2028 | 0x2029
                    ) || (0x2000..=0x200A).contains(&u)
                        || (0xD800..=0xDFFF).contains(&u)
                };
                let mut out = String::new();
                for u in crate::heap::wtf8_code_points(&bytes) {
                    // A LONE surrogate has no `char`: it is in the `other` set —
                    // emit its \uNNNN escape directly.
                    let c = match char::from_u32(u) {
                        Some(c) => c,
                        None => {
                            out.push_str(&format!("\\u{u:04x}"));
                            continue;
                        }
                    };
                    if out.is_empty() && (c.is_ascii_digit() || c.is_ascii_alphabetic()) {
                        // A leading digit/letter is hex-escaped so the escape can't
                        // fuse with a preceding regex token (e.g. \0, a quantifier).
                        out.push_str(&format!("\\x{u:02x}"));
                        continue;
                    }
                    match c {
                        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']'
                        | '{' | '}' | '|' | '/' => {
                            out.push('\\');
                            out.push(c);
                        }
                        '\t' => out.push_str("\\t"),
                        '\n' => out.push_str("\\n"),
                        '\u{0b}' => out.push_str("\\v"),
                        '\u{0c}' => out.push_str("\\f"),
                        '\r' => out.push_str("\\r"),
                        _ if other(u) => {
                            if u <= 0xFF {
                                out.push_str(&format!("\\x{u:02x}"));
                            } else {
                                let mut buf = [0u16; 2];
                                for cu in c.encode_utf16(&mut buf) {
                                    out.push_str(&format!("\\u{cu:04x}"));
                                }
                            }
                        }
                        _ => out.push(c),
                    }
                }
                self.alloc_str(out)
            }
            REGEXP_TO_STRING => {
                let (src, flg) = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::RegExp { source, flags, .. }) => {
                        // Spec reads the `flags` getter, which assembles the
                        // canonical `dgimsuvy` order regardless of storage order.
                        let (s, f) =
                            (source.clone(), super::proxy_regexp::canonical_flags(flags));
                        // Lone-surrogate pattern (exact-source side table):
                        // assemble `/src/flags` over the exact WTF-8 bytes so
                        // toString round-trips the surrogates too.
                        if let Some(b) = self.regexp_exact_source.get(&this.heap_index()) {
                            let esc = self.escaped_source_wtf8(b);
                            let mut out = Vec::with_capacity(esc.len() + f.len() + 2);
                            out.push(b'/');
                            out.extend_from_slice(&esc);
                            out.push(b'/');
                            out.extend_from_slice(f.as_bytes());
                            let js = crate::heap::JsStr::from_wtf8(out);
                            return Ok(Value::heap(self.heap.alloc_js(js)));
                        }
                        (self.escaped_source(&s), f)
                    }
                    _ => {
                        let s = self.get_prop(this, "source")?;
                        let f = self.get_prop(this, "flags")?;
                        (self.to_js_string(s)?, self.to_js_string(f)?)
                    }
                };
                self.alloc_str(format!("/{src}/{flg}"))
            }
            REGEXP_GET_SOURCE => {
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: get source called on a non-object".into(),
                    ));
                }
                let idx = this.heap_index();
                if idx == self.native_home(self.regexp_proto) {
                    self.alloc_str("(?:)".to_string())
                } else if let HeapObj::RegExp { source, .. } = self.heap.get(idx) {
                    let s = source.clone();
                    // Exact-WTF-8 when the side table has the pattern's exact
                    // bytes (lone surrogates round-trip), else lossy.
                    self.regexp_source_value(idx, &s)
                } else {
                    return Err(Thrown(
                        "TypeError: get source called on a non-RegExp object".into(),
                    ));
                }
            }
            REGEXP_GET_FLAGS => {
                // Generic getter: Type(R) must be Object; reads each flag property.
                let is_obj = this.is_heap()
                    && !matches!(
                        self.heap.get(this.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::BigIntBig(_)
                            | HeapObj::Symbol { .. }
                    );
                if !is_obj {
                    return Err(Thrown(
                        "TypeError: get flags called on a non-object".into(),
                    ));
                }
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
                    let v = self.get_prop(this, prop)?;
                    if self.truthy(v) {
                        out.push(ch);
                    }
                }
                self.alloc_str(out)
            }
            REGEXP_SYM_SEARCH => {
                // Spec-generic: Type(this) need only be Object (a plain object with
                // a custom `exec`/`lastIndex` works); RegExpExec dispatches to its exec.
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.search] called on a non-object".into(),
                    ));
                }
                self.regexp_search_impl(this, a0)?
            }
            REGEXP_SYM_MATCH => {
                // Generic over any Object `this` (the observable protocol lives in
                // regexp_match_impl, honouring a user `exec`/`flags`/`lastIndex`).
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.match] called on a non-object".into(),
                    ));
                }
                self.regexp_match_impl(this.heap_index(), a0)?
            }
            REGEXP_SYM_SPLIT => {
                // Generic over any Object `this` (the observable protocol —
                // SpeciesConstructor + sticky splitter — lives in regexp_split_impl).
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.split] called on a non-object".into(),
                    ));
                }
                self.regexp_split_impl(this.heap_index(), a0, a1)?
            }
            REGEXP_SYM_REPLACE => {
                // RegExp.prototype[Symbol.replace] is generic over any Object `this`
                // (a plain object with a custom `exec` works); the observable
                // protocol lives in regexp_symbol_replace.
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.replace] called on a non-object".into(),
                    ));
                }
                self.regexp_symbol_replace(this, a0, a1)?
            }
            REGEXP_SYM_MATCHALL => {
                // RegExp.prototype[Symbol.matchAll](string): an iterator over all
                // matches. The matcher is built via SpeciesConstructor(R, %RegExp%) +
                // Construct(C, «R, flags») (so a custom constructor / @@species is
                // observed), and its lastIndex copies R's via ToLength(Get(R,
                // "lastIndex")). Eagerly computed over the (real-RegExp) clone.
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype[Symbol.matchAll] called on a non-object".into(),
                    ));
                }
                // A custom @@species Construct re-enters the interpreter; hold the
                // un-rooted match Values across it by suspending GC.
                let _gc = self.gc_lock_guard();
                // ToString(string) — IDENTITY for a string value (exact WTF-8).
                let s_val = self.to_str_value(a0)?;
                let flags_v = self.get_prop(this, "flags")?;
                let flags = self.to_js_string(flags_v)?;
                let global = flags.contains('g');
                // fullUnicode is captured HERE per CreateRegExpStringIterator —
                // it drives the driver's AdvanceStringIndex on empty matches.
                let full_unicode = flags.contains('u') || flags.contains('v');
                // C = SpeciesConstructor(R, %RegExp%).
                let default_ctor = Value::heap(self.regexp_ctor);
                let c = {
                    let ctor = self.get_prop(this, "constructor")?;
                    if ctor == Value::UNDEFINED {
                        default_ctor
                    } else if !self.is_object_value(ctor) {
                        return Err(Thrown(
                            "TypeError: RegExp.prototype[Symbol.matchAll]: constructor is not an object".into(),
                        ));
                    } else {
                        let sp = self.get_prop(ctor, "@@species")?;
                        if sp == Value::UNDEFINED || sp == Value::NULL {
                            default_ctor
                        } else if self.is_constructor(sp) {
                            sp
                        } else {
                            return Err(Thrown(
                                "TypeError: RegExp.prototype[Symbol.matchAll]: @@species is not a constructor".into(),
                            ));
                        }
                    }
                };
                // matcher = Construct(C, «R, flags») — `flags` is the ALREADY
                // ToString'd string (a user flags.toString must not run twice).
                let flags_str = self.alloc_str(flags.clone());
                let matcher = self.construct(c, &[this, flags_str])?;
                let matcher_idx = matcher.heap_index();
                // lastIndex = ToLength(Get(R, "lastIndex")); Set(matcher, lastIndex).
                let li_v = self.get_prop(this, "lastIndex")?;
                let li = self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
                self.set_regexp_last_index(matcher_idx, li);
                // Build a LAZY %RegExpStringIterator%: an empty Iterator object whose
                // `next()` runs one RegExpExec (honouring a user `exec`) at a time —
                // exec/lastIndex side effects are observable per step, not up front.
                let proto = self.regexp_string_iter_proto;
                let it =
                    self.heap.alloc(HeapObj::Iterator { items: Vec::new(), index: 0, proto, live: None });
                let fbits = (global as u8) | ((full_unicode as u8) << 1);
                self.regexp_string_iters.insert(it, (matcher_idx, s_val, fbits, false));
                Value::heap(it)
            }
            REGEXP_GET_GLOBAL
            | REGEXP_GET_IGNORECASE
            | REGEXP_GET_MULTILINE
            | REGEXP_GET_DOTALL
            | REGEXP_GET_UNICODE
            | REGEXP_GET_UNICODESETS
            | REGEXP_GET_STICKY
            | REGEXP_GET_HASINDICES => {
                let ch = match id {
                    REGEXP_GET_GLOBAL => 'g',
                    REGEXP_GET_IGNORECASE => 'i',
                    REGEXP_GET_MULTILINE => 'm',
                    REGEXP_GET_DOTALL => 's',
                    REGEXP_GET_UNICODE => 'u',
                    REGEXP_GET_UNICODESETS => 'v',
                    REGEXP_GET_STICKY => 'y',
                    _ => 'd', // REGEXP_GET_HASINDICES
                };
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: RegExp flag getter called on a non-object".into(),
                    ));
                }
                let idx = this.heap_index();
                // `this === %RegExp.prototype%` — the GETTER's own realm's
                // prototype (a realm copy's home is the realm's image).
                if idx == self.native_home(self.regexp_proto) {
                    Value::UNDEFINED
                } else if let HeapObj::RegExp { flags, .. } = self.heap.get(idx) {
                    Value::bool(flags.contains(ch))
                } else {
                    return Err(Thrown(
                        "TypeError: RegExp flag getter called on a non-RegExp object".into(),
                    ));
                }
            }
            FN_CALL => {
                let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                self.call_value(this, a0, rest)?
            }
            FN_APPLY => {
                // argArray null/undefined -> no args; otherwise CreateListFromArrayLike
                // (an array-like, NOT necessarily an iterable).
                let callargs = if a1.is_nullish() {
                    Vec::new()
                } else {
                    self.create_list_from_array_like(a1)?
                };
                self.call_value(this, a0, &callargs)?
            }
            FN_BIND => {
                if !self.is_callable(this) {
                    return Err(Thrown(
                        "TypeError: Function.prototype.bind called on a non-callable".into(),
                    ));
                }
                // Spec 20.2.3.2 steps 3-5: HasOwnProperty(Target, "length") →
                // Get(Target, "length"), then Get(Target, "name") — both
                // OBSERVABLE (a throwing accessor propagates before the bound
                // function exists). The values themselves resolve lazily from
                // the target (existing name/length machinery), so only the Gets
                // are performed here.
                if self.has_own_property(this, "length") {
                    let _ = self.get_prop(this, "length")?;
                }
                let _ = self.get_prop(this, "name")?;
                let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                Value::heap(self.heap.alloc(HeapObj::Bound { target: this, this: a0, args: bound }))
            }
            FN_TO_STRING => {
                if !this.is_heap() {
                    return Err(Thrown(
                        "TypeError: Function.prototype.toString requires that 'this' be a Function"
                            .into(),
                    ));
                }
                // User functions carry their exact source slice; everything else
                // (natives, bound, classes) renders in the `[native code]` form.
                let stored: Option<String> = match self.heap.get(this.heap_index()) {
                    HeapObj::Func(id) => {
                        let s = &self.func(*id as usize).source;
                        (!s.is_empty()).then(|| s.clone())
                    }
                    HeapObj::Closure { func, .. } => {
                        let s = &self.func(*func as usize).source;
                        (!s.is_empty()).then(|| s.clone())
                    }
                    // A class value renders as its whole `class … { … }` source.
                    HeapObj::Class(c) => (!c.source.is_empty()).then(|| c.source.clone()),
                    HeapObj::Native(_) | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } => None,
                    // A constructor global (Array, Date, Temporal.Instant, …) is stored
                    // as an is_ctor Object — it is callable and `typeof` "function", so it
                    // renders in the `[native code]` form rather than throwing. So does
                    // %Function.prototype% itself (a built-in function).
                    HeapObj::Object(m) if m.is_ctor => None,
                    HeapObj::Object(_)
                        if self.fn_proto != 0 && this.heap_index() == self.fn_proto =>
                    {
                        None
                    }
                    // A callable Proxy (its target is callable) is a function for
                    // toString purposes — render the `[native code]` form, not throw.
                    HeapObj::Proxy { target, revoked, .. }
                        if !*revoked && self.is_callable(*target) =>
                    {
                        None
                    }
                    _ => {
                        return Err(Thrown(
                            "TypeError: Function.prototype.toString requires that 'this' be a Function"
                                .into(),
                        ))
                    }
                };
                let out = match stored {
                    Some(s) => s,
                    None => {
                        let name = self.callable_name(this);
                        format!("function {name}() {{ [native code] }}")
                    }
                };
                self.alloc_str(out)
            }
            ARR_IS_ARRAY => Value::bool(self.value_is_array_throwing(a0)?),
            ARR_FROM => self.array_from(this, a0, a1, args.get(2).copied().unwrap_or(Value::UNDEFINED))?,
            ARR_FROM_ASYNC => {
                // Delegate to a lazily-compiled JS polyfill (an async function),
                // invoked with `this` = the receiver constructor C. It returns a
                // Promise; the top-level microtask drain progresses it.
                let f = self.from_async_polyfill()?;
                self.call_value(f, this, args)?
            }
            ASYNC_ITER_DISPOSE => {
                // %AsyncIteratorPrototype%[@@asyncDispose]: delegate to a lazily-
                // compiled JS polyfill, invoked with `this` = the iterator. Returns
                // a Promise that calls+awaits this.return() and resolves undefined.
                let f = self.async_dispose_polyfill()?;
                self.call_value(f, this, args)?
            }
            ARR_OF => {
                // Array.of(...items): A = IsConstructor(this) ? Construct(this,«len»)
                // : ArrayCreate(len); then CreateDataPropertyOrThrow each item. The
                // plain `Array.of(...)` receiver is %Array% → fast dense array.
                let items = args.to_vec();
                let n = items.len();
                let use_default = !self.is_constructor(this)
                    || (this.is_heap()
                        && self.array_ctor != 0
                        && this.heap_index() == self.array_ctor);
                if use_default {
                    Value::heap(self.heap.alloc(HeapObj::Array(items)))
                } else {
                    let _gc = self.gc_lock_guard();
                    let target = self.construct(this, &[Value::num(n as f64)])?;
                    for (i, v) in items.into_iter().enumerate() {
                        self.create_data_property_or_throw(target, i, v)?;
                    }
                    self.set_prop(target, "length", Value::num(n as f64), true)?;
                    target
                }
            }
            // `%TypedArray%.from(src, mapFn?)` / `.of(...items)` — `this` is the
            // concrete kind constructor (Int8Array, …); collect the values into a
            // plain Array, then materialize a typed array of that kind.
            TA_FROM | TA_OF => {
                let kind = self
                    .ta_ctors
                    .iter()
                    .position(|&c| this.is_heap() && c == this.heap_index());
                let kind = match kind {
                    Some(k) => k as u8,
                    None => {
                        // The abstract %TypedArray% intrinsic IS a constructor for
                        // from/of, so its source is evaluated first (any
                        // IterableToList / array-like length+element error
                        // propagates) BEFORE the abstract-construct TypeError fires
                        // at the final TypedArrayCreate step.
                        if this.is_heap()
                            && self.ta_base_ctor != 0
                            && this.heap_index() == self.ta_base_ctor
                        {
                            if id == TA_FROM {
                                self.array_from(
                                    Value::UNDEFINED,
                                    a0,
                                    a1,
                                    args.get(2).copied().unwrap_or(Value::UNDEFINED),
                                )?;
                            }
                            return Err(Thrown(
                                "TypeError: Abstract class TypedArray not directly constructable".into(),
                            ));
                        }
                        // A user-defined constructor as `this`: %TypedArray%.from/of
                        // are generic — collect the source into a list, Construct(this,
                        // «len»), then CreateDataPropertyOrThrow each element (mirrors
                        // Array.of). Only a non-constructor `this` is a TypeError.
                        if self.is_constructor(this) {
                            // Spec order: mapfn validated BEFORE the source is
                            // touched; the source collects WITHOUT mapping; the
                            // target is constructed and write-validated; only then
                            // does the mapper run, interleaved with per-element
                            // [[Set]] writes (coercion throws propagate; writes to
                            // a shrunk/detached target silently drop).
                            let _gc = self.gc_lock_guard();
                            let mapfn = if id == TA_FROM { a1 } else { Value::UNDEFINED };
                            if mapfn != Value::UNDEFINED && !self.is_callable(mapfn) {
                                return Err(Thrown(
                                    "TypeError: TypedArray.from mapfn is not a function".into(),
                                ));
                            }
                            let this_arg = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                            let (n, list): (usize, Option<Vec<Value>>) = if id == TA_FROM {
                                self.ta_from_source(a0)?
                            } else {
                                (args.len(), Some(args.to_vec()))
                            };
                            let target = self.construct(this, &[Value::num(n as f64)])?;
                            // TypedArrayCreate: ValidateTypedArray in write mode —
                            // a non-TypedArray, an immutable-backed result, or one
                            // shorter than requested is a TypeError BEFORE any
                            // element coercion or mapper call.
                            let tidx = match target
                                .is_heap()
                                .then(|| self.heap.get(target.heap_index()))
                            {
                                Some(HeapObj::TypedArray { buffer, .. }) => {
                                    let b = *buffer;
                                    if self.immutable_buffers.contains(&b) {
                                        return Err(Thrown(
                                            "TypeError: TypedArray.from/of constructor returned a TypedArray backed by an immutable ArrayBuffer".into(),
                                        ));
                                    }
                                    target.heap_index()
                                }
                                _ => {
                                    return Err(Thrown(
                                        "TypeError: TypedArray.from/of constructor did not return a TypedArray".into(),
                                    ))
                                }
                            };
                            if self.ta_effective_len(tidx).unwrap_or(0) < n {
                                return Err(Thrown(
                                    "TypeError: TypedArray.from/of constructor returned a TypedArray shorter than the requested length".into(),
                                ));
                            }
                            for i in 0..n {
                                // Array-like sources Get their elements HERE,
                                // after the target exists (spec step 12.b).
                                let v = match &list {
                                    Some(l) => l[i],
                                    None => self.get_index(a0, Value::int(i as i32))?,
                                };
                                let mv = if mapfn != Value::UNDEFINED {
                                    self.call_value(
                                        mapfn,
                                        this_arg,
                                        &[v, Value::num(i as f64)],
                                    )?
                                } else {
                                    v
                                };
                                self.ta_element_set(tidx, i, mv)?;
                            }
                            return Ok(target);
                        }
                        return Err(Thrown(
                            "TypeError: this is not a TypedArray constructor".into(),
                        ));
                    }
                };
                // The built-in ctor path follows the same spec order: validate
                // mapfn, collect the source unmapped, CREATE the target, then run
                // the mapper interleaved with [[Set]] writes — the mapper can
                // detach/shrink the (already-existing) result, whose later writes
                // then silently drop.
                let _gc = self.gc_lock_guard();
                let mapfn = if id == TA_FROM { a1 } else { Value::UNDEFINED };
                if mapfn != Value::UNDEFINED && !self.is_callable(mapfn) {
                    return Err(Thrown(
                        "TypeError: TypedArray.from mapfn is not a function".into(),
                    ));
                }
                let this_arg = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let (n, list): (usize, Option<Vec<Value>>) = if id == TA_FROM {
                    self.ta_from_source(a0)?
                } else {
                    (args.len(), Some(args.to_vec()))
                };
                let size = native::TA_KINDS[kind as usize].1;
                let buf = self.alloc_array_buffer(n * size);
                let target = self.alloc_typed_array(buf, kind, 0, n);
                let tidx = target.heap_index();
                for i in 0..n {
                    let v = match &list {
                        Some(l) => l[i],
                        None => self.get_index(a0, Value::int(i as i32))?,
                    };
                    let mv = if mapfn != Value::UNDEFINED {
                        self.call_value(mapfn, this_arg, &[v, Value::num(i as f64)])?
                    } else {
                        v
                    };
                    self.ta_element_set(tidx, i, mv)?;
                }
                target
            }
            // %TypedArray%.prototype accessor getters. The data accessors throw on a
            // non-TypedArray receiver; @@toStringTag returns undefined instead.
            TA_GET_BUFFER | TA_GET_BYTELENGTH | TA_GET_BYTEOFFSET | TA_GET_LENGTH => {
                let (buffer, kind, byte_offset, length) =
                    match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                        Some(HeapObj::TypedArray { buffer, kind, byte_offset, length }) => {
                            (*buffer, *kind, *byte_offset, *length)
                        }
                        _ => {
                            return Err(Thrown(
                                "TypeError: TypedArray accessor called on a non-TypedArray".into(),
                            ))
                        }
                    };
                let size = native::TA_KINDS[kind as usize].1;
                let detached =
                    matches!(self.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. });
                match id {
                    TA_GET_BUFFER => Value::heap(buffer),
                    TA_GET_BYTELENGTH => Value::num(if detached { 0.0 } else { (length * size) as f64 }),
                    TA_GET_BYTEOFFSET => Value::num(if detached { 0.0 } else { byte_offset as f64 }),
                    _ => Value::num(if detached { 0.0 } else { length as f64 }), // TA_GET_LENGTH
                }
            }
            // `get [Symbol.species]` — returns the receiver constructor unchanged.
            SPECIES_GET => this,
            // Async-module plumbing: `this` carries the deferred module's
            // capability-promise index. OK decrements the pending-dependency
            // count and runs the body on zero; FAIL rejects the capability.
            MODULE_DEP_OK => {
                let cap = this.as_f64() as u32;
                if let Some(st) = self.deferred_mods.get_mut(&cap) {
                    st.remaining = st.remaining.saturating_sub(1);
                    if st.remaining == 0 {
                        let st = self.deferred_mods.remove(&cap).unwrap();
                        self.run_deferred_module(cap, st);
                    }
                }
                Value::UNDEFINED
            }
            AGENT_MONOTONIC_NOW => {
                Value::num(self.vm_start.elapsed().as_secs_f64() * 1000.0)
            }
            AGENT_SET_TIMEOUT => {
                let cb = args.first().copied().unwrap_or(Value::UNDEFINED);
                let ms = self
                    .to_number_coerce(args.get(1).copied().unwrap_or(Value::UNDEFINED))?
                    .max(0.0);
                if self.is_callable(cb) {
                    let due = std::time::Instant::now()
                        + std::time::Duration::from_secs_f64(ms / 1000.0);
                    self.timer_queue.push((due, cb));
                }
                Value::UNDEFINED
            }
            DISPOSE_ASYNC_STEP => {
                let cap = this.as_f64() as u32;
                self.dispose_async_step(cap, None);
                Value::UNDEFINED
            }
            DISPOSE_ASYNC_STEP_REJECT => {
                let cap = this.as_f64() as u32;
                let reason = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.dispose_async_step(cap, Some(reason));
                Value::UNDEFINED
            }
            AGENT_GET_REPORT => match self.agent_pop_report() {
                Some(s) => self.alloc_str(s),
                None => Value::NULL,
            },
            AGENT_START => {
                // Spawn a concurrent worker agent (its own thread + Vm/realm)
                // and BLOCK until it signals it is running (INTERPRETING.md).
                let src =
                    self.to_js_string(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                self.agent_start(src);
                Value::UNDEFINED
            }
            AGENT_BROADCAST => {
                // Blocks until every started agent has RETRIEVED the SAB
                // (workers ack on receipt, before their callback runs).
                let sab = args.first().copied().unwrap_or(Value::UNDEFINED);
                let id = match args.get(1).copied() {
                    Some(v) if v != Value::UNDEFINED => self.to_number_coerce(v)?,
                    _ => 0.0,
                };
                self.agent_broadcast(sab, id)?;
                Value::UNDEFINED
            }
            AGENT_RECEIVE_BROADCAST => {
                // Worker-side: register cb(sab, id) for the next broadcast.
                // GC ROOT (gc.rs traces broadcast_cb).
                self.broadcast_cb = args.first().copied().unwrap_or(Value::UNDEFINED);
                Value::UNDEFINED
            }
            AGENT_REPORT => {
                // ToString(x) onto the shared FIFO the main agent's
                // getReport() pops. Workers report; main pushing is harmless.
                let v = args.first().copied().unwrap_or(Value::UNDEFINED);
                let s = self.to_js_string(v)?;
                self.agent_push_report(s);
                Value::UNDEFINED
            }
            // Marks the worker done; its thread may linger parked on the
            // broadcast channel (process exit reaps it — one test per process).
            AGENT_LEAVING => Value::UNDEFINED,
            GLOBAL_PRINT => {
                // Mirrors the Print instruction (console.log): inspect each
                // argument, join with spaces, append one stdout line.
                let parts: Vec<String> = args.iter().map(|&v| self.inspect(v)).collect();
                self.output.push(parts.join(" "));
                Value::UNDEFINED
            }
            AGENT_SLEEP => {
                let ms = self
                    .to_number_coerce(args.first().copied().unwrap_or(Value::UNDEFINED))?
                    .max(0.0);
                std::thread::sleep(std::time::Duration::from_secs_f64(ms / 1000.0));
                Value::UNDEFINED
            }
            MODULE_DEP_FAIL => {
                let cap = this.as_f64() as u32;
                if self.deferred_mods.remove(&cap).is_some() {
                    let reason = args.first().copied().unwrap_or(Value::UNDEFINED);
                    self.reject(cap, reason);
                }
                Value::UNDEFINED
            }
            // A dynamic import DEFERRED off a static module-link DFS (the
            // ImportCall op queued this microtask): `this` is the import()
            // promise, args = [specifier, type?]. Performs the load now and
            // settles the promise exactly like the inline path (including
            // chaining on a still-pending TLA body promise).
            MODULE_DYN_IMPORT => {
                let p = this.heap_index();
                let spec = self.display(args.first().copied().unwrap_or(Value::UNDEFINED));
                // Trailing slot: the reaction's settled value (always
                // undefined here) follows the bound [specifier, type?] args —
                // only a real string is a type attribute.
                let mtype = args
                    .get(1)
                    .copied()
                    .filter(|t| !t.is_undefined())
                    .map(|t| self.display(t));
                let res: Result<Value, Value> =
                    match self.module_base_dir.as_ref().map(|d| d.join(&spec)) {
                        None => Err(self.make_error(1, None)),
                        Some(path) => match self.import_module(&path, mtype.as_deref()) {
                            Ok(ns) => Ok(ns),
                            Err(Thrown(msg)) => Err(self
                                .pending_throw
                                .take()
                                .unwrap_or_else(|| self.error_from_thrown(&msg))),
                        },
                    };
                match res {
                    Ok(ns) => {
                        let body = self.pending_module_body.take();
                        match body {
                            Some(bp) if bp.is_heap() => {
                                // Still-evaluating TLA module: settle FROM its
                                // body promise (fulfill → the namespace).
                                let _gc = self.gc_lock_guard();
                                let ret_ns = Value::heap(
                                    self.heap.alloc(HeapObj::Native(native::SPECIES_GET)),
                                );
                                let cb = Value::heap(self.heap.alloc(HeapObj::Bound {
                                    target: ret_ns,
                                    this: ns,
                                    args: Vec::new(),
                                }));
                                self.then_internal(
                                    bp.heap_index(),
                                    cb,
                                    Value::UNDEFINED,
                                    Some(p),
                                );
                            }
                            _ => self.resolve(p, ns),
                        }
                    }
                    Err(e) => self.reject(p, e),
                }
                Value::UNDEFINED
            }
            TA_GET_TOSTRINGTAG => {
                match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::TypedArray { kind, .. }) => {
                        let name = native::TA_KINDS[*kind as usize].0.to_string();
                        self.alloc_str(name)
                    }
                    _ => Value::UNDEFINED,
                }
            }
            // `Array.prototype.{join,push}` as values: `this` is the receiver array.
            // join is generic over array-likes (array_method materializes a
            // non-array receiver); push mutates, so it still requires a real array.
            ARR_JOIN => {
                if this.is_heap() {
                    self.array_method(this.heap_index(), "join", args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    // Generic over a primitive `this`: ToObject (null/undefined throw),
                    // then join the boxed array-like (a Boolean/Number wrapper has
                    // length 0, so the result is "").
                    self.require_object_coercible(this)?;
                    let obj = self.to_object(this)?;
                    self.array_method(obj.heap_index(), "join", args)?.unwrap_or(Value::UNDEFINED)
                }
            }
            ARR_PUSH => {
                if this.is_heap() && matches!(self.heap.get(this.heap_index()), HeapObj::Array(_)) {
                    self.array_method(this.heap_index(), "push", args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    // Generic over an array-like `this`
                    // (`Array.prototype.push.call({length, 0:…}, …)`): ToObject then
                    // the abstract Get/Set/ToLength protocol.
                    self.require_object_coercible(this)?;
                    let obj = self.to_object(this)?;
                    self.array_like_mutate(obj, "push", args)?.unwrap_or(Value::UNDEFINED)
                }
            }
            // More Object statics as values.
            OBJ_IS => {
                let a = args.first().copied().unwrap_or(Value::UNDEFINED);
                let b = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Value::bool(self.same_value(a, b))
            }
            OBJ_HAS_OWN => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.require_object_coercible(o)?; // ToObject(O): null/undefined throw
                let k = self.to_property_key(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                Value::bool(self.has_own_property_dyn(o, &k)?)
            }
            OBJ_SET_PROTO_OF => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let proto = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // RequireObjectCoercible(O); proto must be Object or null.
                self.require_object_coercible(o)?;
                if proto != Value::NULL && !self.is_object_value(proto) {
                    return Err(Thrown(
                        "TypeError: Object prototype may only be an Object or null".into(),
                    ));
                }
                // Object.setPrototypeOf returns O, but throws if [[SetPrototypeOf]]
                // rejects the change (non-extensible / cycle / immutable prototype /
                // a Proxy trap returning falsish).
                if !self.ordinary_set_prototype_of(o, proto)? {
                    return Err(Thrown(
                        "TypeError: Object.setPrototypeOf failed (target is non-extensible, the change is cyclic, or it has an immutable prototype)".into(),
                    ));
                }
                o
            }
            OBJ_GET_OWN_SYMBOLS => {
                let _a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.defer_check_all(_a0)?;
                self.require_object_coercible(a0)?; // ToObject(O): null/undefined throw
                // Own symbol-keyed properties: the `@@`-prefixed own keys, mapped
                // back to their Symbol values via the prop_key registry.
                let mut syms: Vec<Value> = Vec::new();
                if a0.is_heap() && self.proxy_parts(a0.heap_index()).is_some() {
                    // A Proxy: drive [[OwnPropertyKeys]] (the ownKeys trap and its
                    // target-key invariant checks — a violation throws) via
                    // object_own_keys, then keep only the Symbol keys.
                    let all = self.object_own_keys(a0)?;
                    for k in self.array_snapshot(all.heap_index()) {
                        if k.is_heap()
                            && matches!(self.heap.get(k.heap_index()), HeapObj::Symbol { .. })
                        {
                            syms.push(k);
                        }
                    }
                } else if a0.is_heap() {
                    if let HeapObj::Object(m) = self.heap.get(a0.heap_index()) {
                        let keys: Vec<String> =
                            m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect();
                        for k in keys {
                            if let Some(&sym) = self.symbol_keys.get(&k) {
                                syms.push(sym);
                            }
                        }
                    } else if let HeapObj::Class(c) = self.heap.get(a0.heap_index()) {
                        // A class constructor's symbol-keyed STATIC elements
                        // (methods + accessors) live in the class's own member
                        // stores, in definition order.
                        let mut keys: Vec<String> = Vec::new();
                        for k in c
                            .statics
                            .keys
                            .iter()
                            .chain(c.static_getters.iter().map(|(n, _)| n))
                            .chain(c.static_setters.iter().map(|(n, _)| n))
                        {
                            if k.starts_with("@@") && !keys.iter().any(|e| e == k) {
                                keys.push(k.clone());
                            }
                        }
                        for k in keys {
                            if let Some(&sym) = self.symbol_keys.get(&k) {
                                syms.push(sym);
                            }
                        }
                        // Symbol keys assigned directly onto the class value
                        // (`C[sym] = v`) land in the arr_props side table.
                        let extra: Vec<String> = self
                            .arr_props
                            .get(&a0.heap_index())
                            .map_or(Vec::new(), |m| {
                                m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect()
                            });
                        for k in extra {
                            if let Some(&sym) = self.symbol_keys.get(&k) {
                                syms.push(sym);
                            }
                        }
                    } else {
                        // Exotic heap kinds (TypedArray, DataView, Map, Date, ...)
                        // keep their own props in the arr_props side table;
                        // callables in fn_props.
                        let table = if matches!(
                            self.heap.get(a0.heap_index()),
                            HeapObj::Func(_)
                                | HeapObj::Closure { .. }
                                | HeapObj::Bound { .. }
                                | HeapObj::Native(_)
                        ) {
                            self.fn_props.get(&a0.heap_index())
                        } else {
                            self.arr_props.get(&a0.heap_index())
                        };
                        let keys: Vec<String> = table.map_or(Vec::new(), |m| {
                            m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect()
                        });
                        for k in keys {
                            if let Some(&sym) = self.symbol_keys.get(&k) {
                                syms.push(sym);
                            }
                        }
                    }
                }
                Value::heap(self.heap.alloc(HeapObj::Array(syms)))
            }
            OBJ_FROM_ENTRIES => {
                let src = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.object_from_entries(src)?
            }
            OBJ_GET_OWN_DESCS => {
                let a = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.require_object_coercible(a)?; // ToObject(O): null/undefined throw
                let o = self.to_object(a)?;
                // [[OwnPropertyKeys]] — STRING and SYMBOL keys (in spec order), not
                // just strings: the result object mirrors every own key. `key_of`
                // yields the internal key (the `@@`-key for a Symbol), so `map.set`
                // recreates a symbol-keyed property.
                let names = self.object_own_keys(o)?;
                let keys: Vec<Value> = match self.heap.get(names.heap_index()) {
                    HeapObj::Array(items) => items.clone(),
                    _ => Vec::new(),
                };
                let mut map = ObjMap::new();
                for kv in keys {
                    let ks = self.key_of(kv);
                    let desc = match self.proxy_gopd(o, &ks)? {
                        Some(d) => d,
                        None => self.object_get_own_property_descriptor(o, &ks),
                    };
                    // CreateDataProperty only for a DEFINED descriptor (a Proxy's gopd
                    // trap may report undefined → that key is omitted from the result).
                    if desc.is_undefined() {
                        continue;
                    }
                    map.set(&ks, desc);
                }
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            // Integrity traits. Non-object arguments pass through unchanged
            // (freeze/seal/preventExtensions) or report as already-locked
            // (isFrozen/isSealed -> true, isExtensible -> false), per ES2015+.
            // Extensibility for an exotic (non-Object) heap value — array, function,
            // Temporal instance, Map/Set/Date/… — is tracked in the `arr_props` side
            // table (its ObjMap carries the `extensible` flag, default true). A fresh
            // exotic is therefore extensible / not-frozen / not-sealed (per spec),
            // and preventExtensions/seal/freeze record it consistently. Plain
            // Objects keep their own `extensible` flag; primitives are immutable.
            OBJ_FREEZE | OBJ_SEAL | OBJ_PREVENT_EXT => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                // SetIntegrityLevel (freeze/seal) AND Object.preventExtensions all
                // call O.[[PreventExtensions]](): for a Proxy this fires the trap and
                // a falsish result is a TypeError (the abrupt path these tests
                // exercise). The full per-key integrity walk on a Proxy is not yet
                // modeled, so on a successful trap freeze/seal fall through to the
                // local flag-set; preventExtensions is then complete.
                if let Some(ok) = self.proxy_prevent_extensions(o)? {
                    if !ok {
                        let m = match id {
                            OBJ_FREEZE => "Object.freeze",
                            OBJ_SEAL => "Object.seal",
                            _ => "Object.preventExtensions",
                        };
                        return Err(Thrown(format!(
                            "TypeError: {m} 'preventExtensions' trap returned falsish"
                        )));
                    }
                    // preventExtensions succeeded. freeze/seal also run the per-key
                    // integrity walk (ownKeys → getOwnPropertyDescriptor →
                    // defineProperty traps) on the Proxy.
                    if id != OBJ_PREVENT_EXT {
                        self.proxy_set_integrity(o, id == OBJ_FREEZE)?;
                    }
                    return Ok(o);
                }
                if o.is_heap() {
                    let idx = o.heap_index();
                    // SetIntegrityLevel on a module namespace: "frozen" first runs
                    // the [[GetOwnProperty]] walk (an uninit export throws), then
                    // defines {writable:false} per export — which the namespace
                    // [[DefineOwnProperty]] rejects (TypeError). "sealed" defines
                    // only {configurable:false}, which matches every export's
                    // actual descriptor — a no-op that SUCCEEDS.
                    if id == OBJ_FREEZE {
                        if let Some(m) = self.module_namespaces.get(&idx) {
                            let has_exports = !m.is_empty();
                            self.ns_tdz_check_all(o)?;
                            if has_exports {
                                return Err(Thrown(
                                    "TypeError: Cannot freeze a module namespace object with exports"
                                        .into(),
                                ));
                            }
                        }
                    }
                    match self.heap.get(idx) {
                        // Heap-but-primitive (string/symbol/bigint): a no-op.
                        HeapObj::Str(_)
                        | HeapObj::Cons { .. }
                        | HeapObj::Symbol { .. }
                        | HeapObj::BigInt(_)
                        | HeapObj::BigIntBig(_) => {}
                        HeapObj::Object(_) => {
                            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                                match id {
                                    OBJ_FREEZE => m.freeze(),
                                    OBJ_SEAL => m.seal(),
                                    _ => m.extensible = false,
                                }
                            }
                        }
                        // A TypedArray on a RESIZABLE buffer can never lose
                        // extensibility: [[PreventExtensions]] returns false
                        // when IsTypedArrayFixedLength is false (a length-
                        // tracking view, or ANY view over a resizable buffer)
                        // — so preventExtensions/seal/freeze all throw. A
                        // fixed-buffer TypedArray with elements still rejects
                        // FREEZE (its indices are writable, non-configurable).
                        HeapObj::TypedArray { buffer, .. }
                            if self.ab_max.contains_key(buffer) =>
                        {
                            return Err(Thrown(
                                "TypeError: Cannot prevent extensions on a TypedArray backed by a resizable ArrayBuffer".into(),
                            ));
                        }
                        _ => {
                            let m = self.arr_props.entry(idx).or_insert_with(ObjMap::new);
                            match id {
                                OBJ_FREEZE => m.freeze(),
                                OBJ_SEAL => m.seal(),
                                _ => m.extensible = false,
                            }
                            // Freezing an Array also makes `length` non-writable (its
                            // descriptor + assignment honour array_length_nonwritable).
                            if id == OBJ_FREEZE
                                && matches!(self.heap.get(idx), HeapObj::Array(_))
                            {
                                self.array_length_nonwritable.insert(idx);
                            }
                            // A callable's own properties live in the fn_props
                            // side map — freeze/seal their ATTRIBUTES there too
                            // (the extensible flag stays in arr_props).
                            if matches!(
                                self.heap.get(idx),
                                HeapObj::Func(_)
                                    | HeapObj::Closure { .. }
                                    | HeapObj::Bound { .. }
                                    | HeapObj::Wrapped { .. }
                                    | HeapObj::Native(_)
                            ) {
                                if let Some(fm) = self.fn_props.get_mut(&idx) {
                                    match id {
                                        OBJ_FREEZE => fm.freeze(),
                                        OBJ_SEAL => fm.seal(),
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                o
            }
            OBJ_IS_FROZEN | OBJ_IS_SEALED | OBJ_IS_EXT => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                if id == OBJ_IS_EXT {
                    if let Some(b) = self.proxy_is_extensible(o)? {
                        return Ok(Value::bool(b));
                    }
                } else if o.is_heap() && self.proxy_parts(o.heap_index()).is_some() {
                    // isFrozen/isSealed on a Proxy: TestIntegrityLevel via the traps.
                    return Ok(Value::bool(self.proxy_test_integrity(o, id == OBJ_IS_FROZEN)?));
                }
                // A non-object (primitive, incl. heap string/symbol/bigint) is
                // non-extensible and vacuously frozen/sealed. An exotic object's
                // flags live in `arr_props` (default: extensible, not frozen/sealed).
                let (frozen, sealed, ext) = if o.is_heap() {
                    match self.heap.get(o.heap_index()) {
                        HeapObj::Object(m) => (m.is_frozen(), m.is_sealed(), m.extensible),
                        HeapObj::Str(_)
                        | HeapObj::Cons { .. }
                        | HeapObj::Symbol { .. }
                        | HeapObj::BigInt(_)
                        | HeapObj::BigIntBig(_) => (true, true, false),
                        // An exotic object (Array / TypedArray / Map / Set / …) whose
                        // elements live outside arr_props: the explicit seal/freeze
                        // markers are authoritative (the vacuous attrs-based check
                        // can't see the dense elements).
                        _ => self.arr_props.get(&o.heap_index()).map_or((false, false, true), |m| {
                            (m.frozen, m.sealed, m.extensible)
                        }),
                    }
                } else {
                    (true, true, false)
                };
                Value::bool(match id {
                    OBJ_IS_FROZEN => frozen,
                    OBJ_IS_SEALED => sealed,
                    _ => ext,
                })
            }
            // Object.groupBy(items, cb) -> null-proto object of arrays keyed by cb's
            // (string) return; Map.groupBy -> a Map keyed by cb's value (SameValueZero).
            OBJ_GROUP_BY | MAP_GROUP_BY => {
                // The accumulating group arrays / keys live in Rust locals (not
                // reachable from the GC roots) while the callback re-enters the
                // interpreter — suspend GC for the scope.
                let _gc = self.gc_lock_guard();
                let src = args.first().copied().unwrap_or(Value::UNDEFINED);
                let cb = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if !(cb.is_heap() && self.heap.as_callable(cb.heap_index()).is_some()) {
                    return Err(Thrown("TypeError: groupBy callback is not callable".into()));
                }
                if !src.is_heap() {
                    return Err(Thrown("TypeError: groupBy items is not iterable".into()));
                }
                let items = self.iterate_to_vec(src)?;
                if id == OBJ_GROUP_BY {
                    let mut map = ObjMap::new();
                    for (i, item) in items.into_iter().enumerate() {
                        let key = self.call_value(cb, Value::UNDEFINED, &[item, Value::int(i as i32)])?;
                        // ToPropertyKey(key) — runs ToPrimitive, so a key whose
                        // toString/@@toPrimitive throws propagates (and a Symbol key
                        // groups under its symbol key), unlike the non-throwing display().
                        let ks = self.to_property_key(key)?;
                        match map.get(&ks) {
                            Some(arr) => {
                                if let HeapObj::Array(a) = self.heap.get_mut(arr.heap_index()) {
                                    a.push(item);
                                }
                            }
                            None => {
                                let arr = Value::heap(self.heap.alloc(HeapObj::Array(vec![item])));
                                map.set(&ks, arr);
                            }
                        }
                    }
                    let result = self.heap.alloc(HeapObj::Object(map));
                    self.proto_of.insert(result, Value::NULL); // null prototype per spec
                    Value::heap(result)
                } else {
                    let mut keys: Vec<Value> = Vec::new();
                    let mut vals: Vec<Value> = Vec::new();
                    for (i, item) in items.into_iter().enumerate() {
                        let mut key = self.call_value(cb, Value::UNDEFINED, &[item, Value::int(i as i32)])?;
                        if key.is_number() && key.as_f64() == 0.0 {
                            key = Value::int(0); // Map normalizes -0 to +0
                        }
                        match keys.iter().position(|k| self.same_value_zero(*k, key)) {
                            Some(pos) => {
                                if let HeapObj::Array(a) = self.heap.get_mut(vals[pos].heap_index()) {
                                    a.push(item);
                                }
                            }
                            None => {
                                keys.push(key);
                                vals.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![item]))));
                            }
                        }
                    }
                    Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }))
                }
            }
            // Promise.withResolvers() -> { promise, resolve, reject }.
            PROMISE_WITH_RESOLVERS => {
                if !self.is_constructor(this) {
                    return Err(Thrown(
                        "TypeError: Promise.withResolvers called on a non-constructor".into(),
                    ));
                }
                // NewPromiseCapability(C): the native Promise builds a native promise
                // + its resolving functions directly; a subclass / custom C must run
                // its executor so `resolve`/`reject` are ITS captured functions and
                // `promise` is the object it produced.
                let (promise, resolve, reject) = if this == self.promise_ctor_value() {
                    let p = self.alloc_promise();
                    let pair = self.new_resolver_pair();
                    let res = Value::heap(
                        self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: false, pair }),
                    );
                    let rej = Value::heap(
                        self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: true, pair }),
                    );
                    (Value::heap(p), res, rej)
                } else {
                    self.new_promise_capability(this)?
                };
                let mut map = ObjMap::new();
                map.set("promise", promise);
                map.set("resolve", resolve);
                map.set("reject", reject);
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            PROMISE_TRY => {
                if !self.is_constructor(this) {
                    return Err(Thrown("TypeError: Promise.try called on a non-constructor".into()));
                }
                let rest: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                if this == self.promise_ctor_value() {
                    let p = self.alloc_promise();
                    match self.call_value(a0, Value::UNDEFINED, &rest) {
                        Ok(v) => self.resolve(p, v),
                        Err(Thrown(msg)) => {
                            // Preserve the actual thrown VALUE, not a reconstructed
                            // error from its message string.
                            let e = match self.pending_throw.take() {
                                Some(v) => v,
                                None => self.alloc_error_from_message(&msg),
                            };
                            self.reject(p, e);
                        }
                    }
                    Value::heap(p)
                } else {
                    // NewPromiseCapability(C): settle through C's captured resolve/reject.
                    let (promise, resolve, reject) = self.new_promise_capability(this)?;
                    match self.call_value(a0, Value::UNDEFINED, &rest) {
                        Ok(v) => {
                            self.call_value(resolve, Value::UNDEFINED, &[v])?;
                        }
                        Err(Thrown(msg)) => {
                            let e = match self.pending_throw.take() {
                                Some(v) => v,
                                None => self.alloc_error_from_message(&msg),
                            };
                            self.call_value(reject, Value::UNDEFINED, &[e])?;
                        }
                    }
                    promise
                }
            }
            // Reflect namespace. apply/construct accept any callable target; the
            // property-reflecting methods require Type(target) === Object (else TypeError).
            REFLECT_APPLY => {
                let target = a0;
                let this_arg = a1;
                let args_list = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                // Reflect.apply requires an array-like argumentsList (CreateListFromArrayLike).
                let arg_vec = self.create_list_from_array_like(args_list)?;
                self.call_value(target, this_arg, &arg_vec)?
            }
            REFLECT_CONSTRUCT => {
                let target = a0;
                if !self.is_constructor(target) {
                    return Err(Thrown("TypeError: Reflect.construct target is not a constructor".into()));
                }
                // An explicit newTarget (3rd arg) must also be a constructor. We
                // don't model newTarget-driven prototype selection, but the throw is
                // what test262's isConstructor relies on.
                if let Some(nt) = args.get(2) {
                    if !self.is_constructor(*nt) {
                        return Err(Thrown(
                            "TypeError: Reflect.construct newTarget is not a constructor".into(),
                        ));
                    }
                }
                // CreateListFromArrayLike(argumentsList): a non-object arglist (incl.
                // a missing 2nd arg) is a TypeError; an array-LIKE is read via Get.
                let arg_vec = self.create_list_from_array_like(a1)?;
                // newTarget defaults to target when the 3rd arg is absent; thread it
                // so a Proxy construct trap (and a trap-less forward) sees the real one.
                let new_target = args.get(2).copied().unwrap_or(target);
                self.construct_with_newtarget(target, &arg_vec, new_target)?
            }
            REFLECT_GET => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.get called on non-object".into()));
                }
                // Reflect.get(target, key, receiver?): an explicit receiver is the
                // `this` for an accessor getter (else the target). Use the index
                // path when there's no distinct receiver (it also reads array
                // elements for numeric keys).
                let receiver = args.get(2).copied().unwrap_or(a0);
                if receiver == a0 {
                    self.get_index(a0, a1)?
                } else {
                    let key = self.to_property_key(a1)?;
                    self.get_member(a0, &key, receiver)?
                }
            }
            REFLECT_SET => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.set called on non-object".into()));
                }
                let value = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                // ToPropertyKey once (an object key may have a side-effecting
                // toString); reuse the coerced key Value for set_index so it isn't
                // coerced a second time.
                let receiver = args.get(3).copied().unwrap_or(a0);
                let kv = self.coerce_index_key(a1)?;
                let key = self.key_of(kv);
                // Module Namespace exotic [[Set]]: always false.
                if self.module_namespaces.contains_key(&a0.heap_index()) {
                    return Ok(Value::bool(false));
                }
                // A Proxy's [[Set]] is its `set` trap: Reflect.set reports the trap's
                // boolean (an assignment swallows a falsish result). With NO trap,
                // [[Set]] forwards to the target with the SAME receiver — the
                // receiver-aware forward also reports the [[Set]] boolean (a
                // rejected exotic write — RegExp flags, fn length, a frozen
                // target — must read false, not true).
                if let Some(b) = self.proxy_set_bool(a0, &key, value, receiver)? {
                    return Ok(Value::bool(b));
                }
                if let Some((t, _, _)) = self.proxy_parts(a0.heap_index()) {
                    let b = self.ordinary_set_with_proxy_receiver(t, receiver, &key, value, false)?;
                    return Ok(Value::bool(b));
                }
                // TypedArray [[Set]] (10.4.5.5) step 1, for any canonical numeric
                // key: SameValue(O, Receiver) runs TypedArraySetElement (coerce the
                // value — observable — then bounds-checked write or silent drop) and
                // returns true; an altered Receiver with an INVALID index returns
                // true with NO coercion; a valid index continues OrdinarySet on the
                // RECEIVER with the RAW value (the element is a writable data prop).
                if matches!(self.heap.get(a0.heap_index()), HeapObj::TypedArray { .. })
                    && self.is_canonical_numeric_index(&key)
                {
                    if self.same_value(a0, receiver) {
                        match self.ta_valid_index(a0.heap_index(), &key) {
                            Some(i) => self.ta_element_set(a0.heap_index(), i, value)?,
                            None => self.ta_coerce_for_set(a0.heap_index(), value)?,
                        }
                        return Ok(Value::bool(true));
                    }
                    if self.ta_valid_index(a0.heap_index(), &key).is_none() {
                        return Ok(Value::bool(true));
                    }
                    let b = self.reflect_set_on_receiver(receiver, kv, value)?;
                    return Ok(Value::bool(b));
                }
                // ArraySetLength via Reflect.set: ToUint32 + ToNumber BOTH run
                // (valueOf twice), non-uint32 is a RangeError, and a non-writable
                // `length` (made so even DURING the coercion) reports FALSE — the
                // ordinary assignment path can only no-op or throw.
                if key == "length" && matches!(self.heap.get(a0.heap_index()), HeapObj::Array(_)) {
                    let nu = self.to_number_coerce(value)?;
                    let u = if nu.is_finite() { (nu.trunc() as i64 as u32) as f64 } else { 0.0 };
                    let number_len = self.to_number_coerce(value)?;
                    if u != number_len {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    let aidx = a0.heap_index();
                    if self.array_length_nonwritable.contains(&aidx)
                        || self.arr_props.get(&aidx).is_some_and(|m| m.frozen)
                    {
                        return Ok(Value::bool(false));
                    }
                    // Mirror the ordinary `arr.length = n` path: stop the shrink
                    // at a non-configurable index, sweep the doomed arr_props
                    // entries, and keep the virtual (sparse) length consistent.
                    let mut final_len = u as usize;
                    if let Some(ki) = self.array_shrink_blocker(aidx, final_len) {
                        final_len = ki + 1;
                    }
                    self.array_apply_length(aidx, final_len);
                    return Ok(Value::bool(true));
                }
                // OrdinarySet([[Set]](P,V,Receiver)): find the governing descriptor
                // (target's own, then up the prototype chain). Only ordinary Object
                // links carry inline descriptors here; a class-instance/exotic link
                // falls back to the simpler target-write below.
                let mut governing: Option<(bool, bool, Value)> = None; // (accessor, writable, setter)
                let mut fell_back = false;
                let mut ta_chain_node: Option<u32> = None;
                let mut cur = a0;
                loop {
                    match self.heap.get(cur.heap_index()) {
                        HeapObj::Object(m) => {
                            if let Some(i) = m.pos(&key) {
                                governing =
                                    Some((m.attrs[i].accessor, m.attrs[i].writable, m.attrs[i].setter));
                                break;
                            }
                            if m.class.is_some() {
                                fell_back = true; // class-chain members aren't inline attrs
                                break;
                            }
                        }
                        // A TypedArray on the prototype chain ABSORBS canonical
                        // numeric keys (its [[Set]] governs); handled below.
                        HeapObj::TypedArray { .. } if self.is_canonical_numeric_index(&key) => {
                            ta_chain_node = Some(cur.heap_index());
                            break;
                        }
                        _ => {
                            fell_back = true;
                            break;
                        }
                    }
                    let p = self.object_get_prototype_of(cur);
                    if !p.is_heap() {
                        break;
                    }
                    cur = p;
                }
                if let Some(c) = ta_chain_node {
                    // parent.[[Set]](P, V, Receiver) reached the TypedArray: same
                    // TA [[Set]] step 1 as above, with the ORIGINAL receiver.
                    if self.same_value(Value::heap(c), receiver) {
                        match self.ta_valid_index(c, &key) {
                            Some(i) => self.ta_element_set(c, i, value)?,
                            None => self.ta_coerce_for_set(c, value)?,
                        }
                        return Ok(Value::bool(true));
                    }
                    if self.ta_valid_index(c, &key).is_none() {
                        return Ok(Value::bool(true));
                    }
                    let b = self.reflect_set_on_receiver(receiver, kv, value)?;
                    return Ok(Value::bool(b));
                }
                let result = if fell_back {
                    let ok = match self.heap.get(a0.heap_index()) {
                        HeapObj::Object(m) => match m.pos(&key) {
                            Some(i) => {
                                if m.attrs[i].accessor {
                                    m.attrs[i].setter != Value::UNDEFINED
                                } else {
                                    m.attrs[i].writable
                                }
                            }
                            None => m.extensible,
                        },
                        // Exotic/callable targets keep defineProperty'd descriptors
                        // in the arr_props/fn_props side tables — honour them:
                        // accessor-without-setter and non-writable data reject, a
                        // NEW named prop on a non-extensible target rejects.
                        _ => {
                            let h = a0.heap_index();
                            let side = match self.heap.get(h) {
                                HeapObj::Func(_)
                                | HeapObj::Closure { .. }
                                | HeapObj::Bound { .. }
                                | HeapObj::Native(_) => self.fn_props.get(&h),
                                _ => self.arr_props.get(&h),
                            };
                            match side.and_then(|m| {
                                m.pos(&key).map(|i| {
                                    (m.attrs[i].accessor, m.attrs[i].writable, m.attrs[i].setter)
                                })
                            }) {
                                Some((true, _, setter)) => setter != Value::UNDEFINED,
                                Some((_, w, _)) => w,
                                None => {
                                    if canonical_index_str(&key).is_some() {
                                        true
                                    } else {
                                        side.map_or(true, |m| m.extensible)
                                    }
                                }
                            }
                        }
                    };
                    if ok {
                        self.set_index(a0, kv, value, false)?;
                    }
                    ok
                } else {
                    match governing {
                        // Accessor: invoke its setter with the RECEIVER as `this`.
                        Some((true, _, setter)) => {
                            if setter == Value::UNDEFINED {
                                false
                            } else {
                                self.call_value(setter, receiver, &[value])?;
                                true
                            }
                        }
                        // Non-writable data property: rejected.
                        Some((false, false, _)) => false,
                        // Writable data property, or a new property: write to the
                        // RECEIVER (CreateDataProperty / overwrite its data prop),
                        // rejecting a non-object receiver or a conflicting own prop.
                        _ => {
                            if !self.is_object_value(receiver) {
                                false
                            } else if self.proxy_parts(receiver.heap_index()).is_some() {
                                // OrdinarySetWithOwnDescriptor on a Proxy receiver:
                                // read its own descriptor ([[GetOwnProperty]] → GOPD
                                // trap), then define ([[DefineOwnProperty]] →
                                // defineProperty trap) — NEVER its set trap (a trap
                                // calling Reflect.set(t,k,v,proxy) would recurse).
                                let existing =
                                    self.proxy_gopd(receiver, &key)?.unwrap_or(Value::UNDEFINED);
                                if self.is_object_value(existing) {
                                    let g = self.get_prop(existing, "get")?;
                                    let s = self.get_prop(existing, "set")?;
                                    let w = self.get_prop(existing, "writable")?;
                                    if g != Value::UNDEFINED || s != Value::UNDEFINED || !self.truthy(w)
                                    {
                                        false
                                    } else {
                                        // valueDesc = { [[Value]]: V } only.
                                        let mut m = crate::heap::ObjMap::new();
                                        m.set("value", value);
                                        let desc =
                                            Value::heap(self.heap.alloc(HeapObj::Object(m)));
                                        self.object_define_property(receiver, &key, desc)?;
                                        true
                                    }
                                } else {
                                    // CreateDataProperty(Receiver, P, V).
                                    let mut m = crate::heap::ObjMap::new();
                                    m.set("value", value);
                                    m.set("writable", Value::TRUE);
                                    m.set("enumerable", Value::TRUE);
                                    m.set("configurable", Value::TRUE);
                                    let desc = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                                    self.object_define_property(receiver, &key, desc)?;
                                    true
                                }
                            } else {
                                let rown = match self.heap.get(receiver.heap_index()) {
                                    HeapObj::Object(m) => {
                                        m.pos(&key).map(|i| (m.attrs[i].accessor, m.attrs[i].writable))
                                    }
                                    _ => None,
                                };
                                match rown {
                                    Some((true, _)) => false,      // accessor own prop on receiver
                                    Some((false, false)) => false, // non-writable data on receiver
                                    _ => {
                                        self.set_index(receiver, kv, value, false)?;
                                        true
                                    }
                                }
                            }
                        }
                    }
                };
                Value::bool(result)
            }
            REFLECT_HAS => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.has called on non-object".into()));
                }
                let kv = self.coerce_index_key(a1)?;
                Value::bool(self.has_property_dyn(a0, kv)?)
            }
            REFLECT_DELETE => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.deleteProperty called on non-object".into()));
                }
                let key = self.to_property_key(a1)?;
                // delete_property (not delete_prop) so a Proxy target's
                // deleteProperty trap runs.
                self.delete_property(a0, &key)?
            }
            REFLECT_OWN_KEYS => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.ownKeys called on non-object".into()));
                }
                self.object_own_keys(a0)?
            }
            REFLECT_GET_PROTO => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.getPrototypeOf called on non-object".into()));
                }
                self.get_prototype_of_checked(a0)?
            }
            REFLECT_SET_PROTO => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.setPrototypeOf called on non-object".into()));
                }
                if a1 != Value::NULL && !self.is_object_value(a1) {
                    return Err(Thrown(
                        "TypeError: Reflect.setPrototypeOf prototype must be an object or null".into(),
                    ));
                }
                Value::bool(self.ordinary_set_prototype_of(a0, a1)?)
            }
            REFLECT_DEFINE => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.defineProperty called on non-object".into()));
                }
                // ToPropertyKey(propertyKey) is step 2 — BEFORE ToPropertyDescriptor
                // (step 3) — so a throwing key coercion propagates ahead of the
                // descriptor's "must be an object" check.
                let key = self.to_property_key(a1)?;
                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_object_value(desc) {
                    return Err(Thrown("TypeError: Property description must be an object".into()));
                }
                // ToPropertyDescriptor(desc) is validated FIRST: an invalid
                // descriptor (a non-callable get/set, or mixed accessor+data) is a
                // THROW that propagates — only a rejected [[DefineOwnProperty]]
                // (non-configurable redefine, non-extensible new key) returns false.
                self.read_descriptor(desc)?;
                // Whether a live defineProperty trap sits anywhere on a0's
                // proxy chain (structural peek — no observable handler Get):
                // a trap's ABRUPT completion (or an invariant violation) must
                // PROPAGATE through Reflect.defineProperty
                // (return-abrupt-from-result), while its falsish result — and
                // any ordinary trap-less rejection — reads as false.
                let mut define_trap = false;
                let mut cur = a0;
                while let Some((t, h, _)) = self.proxy_parts(cur.heap_index()) {
                    if h.is_heap()
                        && matches!(
                            self.heap.get(h.heap_index()),
                            HeapObj::Object(m) if m.get("defineProperty").is_some_and(|v| !v.is_nullish())
                        )
                    {
                        define_trap = true;
                        break;
                    }
                    cur = t;
                }
                match self.object_define_property(a0, &key, desc) {
                    Ok(()) => Value::bool(true),
                    Err(e)
                        if define_trap
                            && !e.0.starts_with(
                                "TypeError: proxy 'defineProperty' trap returned falsish",
                            ) =>
                    {
                        return Err(e);
                    }
                    Err(_) => Value::bool(false),
                }
            }
            REFLECT_GET_OWN_DESC => {
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Reflect.getOwnPropertyDescriptor called on non-object".into(),
                    ));
                }
                let key = self.to_property_key(a1)?;
                self.ns_tdz_check(a0, &key)?;
                match self.proxy_gopd(a0, &key)? {
                    Some(d) => d,
                    None => self.object_get_own_property_descriptor(a0, &key),
                }
            }
            REFLECT_IS_EXT => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.isExtensible called on non-object".into()));
                }
                if let Some(b) = self.proxy_is_extensible(a0)? {
                    return Ok(Value::bool(b));
                }
                let ext = match self.heap.get(a0.heap_index()) {
                    HeapObj::Object(m) => m.extensible,
                    _ => self.arr_props.get(&a0.heap_index()).map_or(true, |m| m.extensible),
                };
                Value::bool(ext)
            }
            REFLECT_PREVENT_EXT => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.preventExtensions called on non-object".into()));
                }
                if let Some(b) = self.proxy_prevent_extensions(a0)? {
                    return Ok(Value::bool(b));
                }
                let idx = a0.heap_index();
                if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.extensible = false;
                    }
                } else {
                    self.arr_props.entry(idx).or_insert_with(ObjMap::new).extensible = false;
                }
                Value::bool(true)
            }
            // JSON namespace methods as values (`JSON.parse`/`JSON.stringify`).
            // (The direct `JSON.parse(x)` call form is compile-lowered to a JSON op;
            // these back the value form + reflection.)
            JSON_PARSE => {
                // EXACT input bytes for a string argument: JSON text may carry
                // raw lone surrogates inside string literals (valid JSON text,
                // preserved in the parsed value). Non-strings ToString-coerce
                // (observable, may throw).
                let s: Vec<u8> = if a0.is_heap() && self.heap.is_str_like(a0.heap_index()) {
                    self.heap.str_wtf8_cow(a0.heap_index()).unwrap().into_owned()
                } else {
                    self.to_js_string(a0)?.into_bytes()
                };
                let reviver = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if self.is_callable(reviver) {
                    let _gc = self.gc_lock_guard();
                    let (parsed, srctree) = self.json_parse_with_src(&s)?;
                    let mut m = crate::heap::ObjMap::new();
                    m.set("", parsed);
                    let wrapper = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                    self.internalize_json(wrapper, "", reviver, Some(&srctree))?
                } else {
                    self.json_parse(&s)?
                }
            }
            JSON_STRINGIFY => {
                let space = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let space = self.json_coerce_space(space)?;
                let indent = self.json_indent(space);
                let replacer = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (replacer_fn, allowlist) = self.json_resolve_replacer(replacer)?;
                // Hold un-rooted Values across toJSON/replacer re-entry; suspend GC.
                let _gc = self.gc_lock_guard();
                let mut m = crate::heap::ObjMap::new();
                m.set("", a0);
                let wrapper = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                let mut visited = Vec::new();
                match self.json_value(
                    wrapper,
                    "",
                    a0,
                    &indent,
                    0,
                    &mut visited,
                    replacer_fn,
                    allowlist.as_deref(),
                )? {
                    Some(s) => self.alloc_str(s),
                    None => Value::UNDEFINED,
                }
            }
            JSON_RAW_JSON => {
                // JSON.rawJSON(text): ToString (throws TypeError for a Symbol),
                // then validate the text is a single non-empty JSON value with no
                // leading/trailing JSON whitespace. The result is a frozen,
                // null-prototype object whose sole own property "rawJSON" holds the
                // text, tagged [[IsRawJSON]] so stringify emits it verbatim.
                let s = self.to_js_string(a0)?;
                let bytes = s.as_bytes();
                let ws = |c: u8| matches!(c, b'\t' | b'\n' | b'\r' | b' ');
                if s.is_empty() || ws(bytes[0]) || ws(bytes[bytes.len() - 1]) {
                    return Err(Thrown(
                        "SyntaxError: JSON.rawJSON text must be non-empty without leading/trailing whitespace".into(),
                    ));
                }
                // Validate it parses as one complete JSON value (checks trailing).
                self.json_parse(s.as_bytes())?;
                let _gc = self.gc_lock_guard();
                let sval = self.alloc_str(s);
                let mut m = crate::heap::ObjMap::new();
                m.is_raw_json = true;
                m.extensible = false;
                m.keys.push("rawJSON".to_string());
                m.vals.push(sval);
                m.attrs.push(crate::heap::PropAttr {
                    writable: false,
                    enumerable: true,
                    configurable: false,
                    accessor: false,
                    setter: Value::UNDEFINED,
                });
                let idx = self.heap.alloc(HeapObj::Object(m));
                self.proto_of.insert(idx, Value::NULL); // OrdinaryObjectCreate(null)
                Value::heap(idx)
            }
            JSON_IS_RAW_JSON => {
                let v = args.first().copied().unwrap_or(Value::UNDEFINED);
                let is = v.is_heap()
                    && matches!(self.heap.get(v.heap_index()), HeapObj::Object(m) if m.is_raw_json);
                Value::bool(is)
            }
            // `Math.random` as a value (the call form uses the Random op). xorshift64*.
            MATH_RANDOM => {
                let mut x = self.rng_state;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.rng_state = x;
                let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                Value::num((r >> 11) as f64 / (1u64 << 53) as f64)
            }
            // `Math.sumPrecise(iterable)`: correctly-rounded sum. Every element must
            // already BE a Number (no coercion) — else a TypeError (the iterator is
            // closed). Spec 2024 proposal.
            MATH_SUM_PRECISE => {
                let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                // Iterate STEP-BY-STEP (the iterable may be infinite) and validate
                // each element is a Number with NO coercion; on a non-number, close
                // the iterator and throw a TypeError.
                let iter = self.get_iterator_direct(a0)?;
                let next = self.get_prop(iter, "next")?;
                let mut nums: Vec<f64> = Vec::new();
                loop {
                    let res = self.call_value(next, iter, &[])?;
                    if !self.is_object_value(res) {
                        return Err(Thrown(
                            "TypeError: Math.sumPrecise: iterator result is not an object".into(),
                        ));
                    }
                    let done = self.get_prop(res, "done")?;
                    if self.truthy(done) {
                        break;
                    }
                    let v = self.get_prop(res, "value")?;
                    if !(v.is_int() || v.is_double()) {
                        let _ = self.iterator_close(iter);
                        return Err(Thrown(
                            "TypeError: Math.sumPrecise: each element must be a Number".into(),
                        ));
                    }
                    nums.push(v.as_f64());
                }
                Value::num(super::helpers_num2::sum_precise(&nums))
            }
            // `Math.f16round(x)`: round ToNumber(x) to the nearest binary16 value.
            MATH_F16ROUND => {
                let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                let x = self.to_number_coerce(a0)?;
                Value::num(super::helpers_num2::f16_round(x))
            }
            // WeakMap/WeakSet methods (brand-checked + object-key validated inside).
            WM_GET => self.weakmap_method(this, "get", args)?,
            WM_SET => self.weakmap_method(this, "set", args)?,
            WM_HAS => self.weakmap_method(this, "has", args)?,
            WM_DELETE => self.weakmap_method(this, "delete", args)?,
            WM_GET_OR_INSERT => self.weakmap_method(this, "getOrInsert", args)?,
            WM_GET_OR_INSERT_COMPUTED => self.weakmap_method(this, "getOrInsertComputed", args)?,
            SET_SIZE_GET => match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                Some(HeapObj::Set(items)) => Value::num(items.len() as f64),
                _ => {
                    return Err(Thrown(
                        "TypeError: get Set.prototype.size called on incompatible receiver".into(),
                    ))
                }
            },
            MAP_SIZE_GET => match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                Some(HeapObj::Map { keys, .. }) => Value::num(keys.len() as f64),
                _ => {
                    return Err(Thrown(
                        "TypeError: get Map.prototype.size called on incompatible receiver".into(),
                    ))
                }
            },
            // GetCapabilitiesExecutor: capture (resolve, reject); a second call
            // (capability.[[Resolve]] already set) is a TypeError.
            CAP_EXECUTOR => {
                let resolve = args.first().copied().unwrap_or(Value::UNDEFINED);
                let reject = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // GetCapabilitiesExecutor: throw ONLY if a prior invocation already
                // captured a non-undefined resolve/reject (capability.[[Resolve]] /
                // [[Reject]] is not undefined). An initial executor(undefined,
                // undefined) leaves them undefined, so a later call may still
                // (re)capture, per spec.
                if let Some((r, j)) = self.cap_capture {
                    if r != Value::UNDEFINED || j != Value::UNDEFINED {
                        return Err(Thrown(
                            "TypeError: Promise capability executor already invoked".into(),
                        ));
                    }
                }
                self.cap_capture = Some((resolve, reject));
                Value::UNDEFINED
            }
            WS_ADD => self.weakset_method(this, "add", args)?,
            WS_HAS => self.weakset_method(this, "has", args)?,
            WS_DELETE => self.weakset_method(this, "delete", args)?,
            WR_DEREF => {
                match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::WeakRef(t)) => *t, // no GC → target always live
                    _ => {
                        return Err(Thrown(
                            "TypeError: WeakRef.prototype.deref called on incompatible receiver".into(),
                        ))
                    }
                }
            }
            FR_REGISTER => self.finreg_method(this, "register", args)?,
            FR_UNREGISTER => self.finreg_method(this, "unregister", args)?,
            ITER_NEXT => {
                let it_idx = this.heap_index();
                let (live, mut index) = match this.is_heap().then(|| self.heap.get(it_idx)) {
                    Some(HeapObj::Iterator { live, index, .. }) => (*live, *index),
                    _ => {
                        return Err(Thrown(
                            "TypeError: Iterator.prototype.next called on incompatible receiver".into(),
                        ))
                    }
                };
                // A lazy %RegExpStringIterator%: run ONE RegExpExec (via the abstract
                // protocol, honouring a user `exec`) per next(). A null result, or the
                // single match of a non-global regex, latches done; a global empty
                // match advances lastIndex (spec AdvanceStringIndex: +1 unit, +2 over
                // an astral surrogate pair when the iterator's fullUnicode bit is set)
                // so the next step makes progress.
                if let Some(&(regexp, string, fbits, done)) = self.regexp_string_iters.get(&it_idx) {
                    let global = fbits & 1 != 0;
                    let full_unicode = fbits & 2 != 0;
                    let (value, ret_done, latch) = if done {
                        (Value::UNDEFINED, true, true)
                    } else {
                        let r = self.regexp_exec_abstract(regexp, string)?;
                        if r == Value::NULL {
                            (Value::UNDEFINED, true, true)
                        } else if !global {
                            (r, false, true)
                        } else {
                            let m0 = self.get_index(r, Value::int(0))?;
                            let m0s = self.to_js_string(m0)?;
                            if m0s.is_empty() {
                                let cur_v = self.get_prop(Value::heap(regexp), "lastIndex")?;
                                // ToLength(Get(R,"lastIndex")) — a throwing
                                // lastIndex.valueOf must propagate, not be swallowed;
                                // the 2^53-1 clamp applies BEFORE the advance.
                                let cur = self
                                    .to_integer_or_zero(cur_v)?
                                    .clamp(0, (1i64 << 53) - 1)
                                    as usize;
                                let next = self.advance_index_on_value(string, cur, full_unicode);
                                self.set_regexp_last_index(regexp, next);
                            }
                            (r, false, false)
                        }
                    };
                    self.regexp_string_iters.insert(it_idx, (regexp, string, fbits, latch));
                    let mut m = ObjMap::new();
                    m.set("value", value);
                    m.set("done", Value::bool(ret_done));
                    return Ok(Value::heap(self.heap.alloc(HeapObj::Object(m))));
                }
                // A live TypedArray iterator (keys/values/entries) re-reads the view's
                // current length each step, so a resizable buffer's grow yields new
                // elements; an out-of-bounds view (detached, or a fixed-length view on
                // a shrunk buffer) throws TypeError per the spec's per-step check.
                if let Some((coll, kind)) = live {
                    if matches!(self.heap.get(coll), HeapObj::TypedArray { .. }) {
                        // An already-exhausted iterator (latched usize::MAX) is
                        // permanently done — the spec's "iterating is finished" check
                        // precedes the out-of-bounds check, so a buffer that became OOB
                        // AFTER exhaustion does NOT throw.
                        let result = if index == usize::MAX {
                            (Value::UNDEFINED, true)
                        } else {
                            match self.ta_effective_len(coll) {
                            None => {
                                return Err(Thrown(
                                    "TypeError: TypedArray iterator: the viewed buffer is out of bounds".into(),
                                ))
                            }
                            Some(n) if index >= n => (Value::UNDEFINED, true),
                            Some(_) => {
                                let v = match kind {
                                    0 => Value::num(index as f64),
                                    1 => self.ta_element_get(coll, index),
                                    _ => {
                                        let e = self.ta_element_get(coll, index);
                                        Value::heap(self.heap.alloc(HeapObj::Array(vec![
                                            Value::num(index as f64),
                                            e,
                                        ])))
                                    }
                                };
                                index += 1;
                                (v, false)
                            }
                            }
                        };
                        let store = if result.1 { usize::MAX } else { index };
                        if let HeapObj::Iterator { index: i, .. } = self.heap.get_mut(it_idx) {
                            *i = store;
                        }
                        let mut m = ObjMap::new();
                        m.set("value", result.0);
                        m.set("done", Value::bool(result.1));
                        return Ok(Value::heap(self.heap.alloc(HeapObj::Object(m))));
                    }
                }
                // A live ARRAY-LIKE iterator (%ArrayIteratorPrototype% over an
                // Array or generic object): the length is re-read EACH step, so
                // mutations made during iteration (push/pop/length-set/element
                // writes) are observed; exhaustion latches permanent-done per
                // the spec generator (a later grow is NOT iterated).
                if let Some((coll, kind)) = live {
                    let it_proto = match self.heap.get(it_idx) {
                        HeapObj::Iterator { proto, .. } => *proto,
                        _ => 0,
                    };
                    if it_proto == self.array_iter_proto
                        && !matches!(self.heap.get(coll), HeapObj::TypedArray { .. })
                    {
                        let result = if index == usize::MAX {
                            (Value::UNDEFINED, true)
                        } else {
                            let n = match self.heap.get(coll) {
                                HeapObj::Array(items) => items.len(),
                                _ => {
                                    let lv = self.get_prop(Value::heap(coll), "length")?;
                                    let lf = self.to_number_coerce(lv)?;
                                    if lf.is_nan() || lf <= 0.0 {
                                        0
                                    } else {
                                        lf.trunc().min(9_007_199_254_740_991.0) as usize
                                    }
                                }
                            };
                            if index >= n {
                                (Value::UNDEFINED, true)
                            } else {
                                let v = match kind {
                                    0 => Value::num(index as f64),
                                    1 => self
                                        .get_index(Value::heap(coll), Value::num(index as f64))?,
                                    _ => {
                                        let e = self
                                            .get_index(Value::heap(coll), Value::num(index as f64))?;
                                        Value::heap(self.heap.alloc(HeapObj::Array(vec![
                                            Value::num(index as f64),
                                            e,
                                        ])))
                                    }
                                };
                                index += 1;
                                (v, false)
                            }
                        };
                        let store = if result.1 { usize::MAX } else { index };
                        if let HeapObj::Iterator { index: i, .. } = self.heap.get_mut(it_idx) {
                            *i = store;
                        }
                        let mut m = ObjMap::new();
                        m.set("value", result.0);
                        m.set("done", Value::bool(result.1));
                        return Ok(Value::heap(self.heap.alloc(HeapObj::Object(m))));
                    }
                }
                let (val, done) = if let Some((coll, kind)) = live {
                    // Live Map/Set iterator: step the backing collection, skipping
                    // tombstoned (deleted) slots; appends made after creation are seen.
                    // Once the cursor reaches the end the iterator is PERMANENTLY done
                    // (a later add is NOT iterated), marked by index == usize::MAX.
                    let mut result = (Value::UNDEFINED, true);
                    if index != usize::MAX {
                        loop {
                            // Copy out the (key, value) at `index` (or stop), releasing
                            // the heap borrow before any allocation below.
                            let pair: Option<(Value, Value)> = match self.heap.get(coll) {
                                HeapObj::Set(items) => {
                                    if index >= items.len() {
                                        break;
                                    }
                                    let v = items[index];
                                    (!v.is_hole()).then_some((v, v))
                                }
                                HeapObj::Map { keys, vals } => {
                                    if index >= keys.len() {
                                        break;
                                    }
                                    let k = keys[index];
                                    (!k.is_hole()).then_some((k, vals[index]))
                                }
                                _ => break, // collection gone
                            };
                            index += 1;
                            if let Some((k, v)) = pair {
                                let yielded = match kind {
                                    0 => k,
                                    1 => v,
                                    _ => Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))),
                                };
                                result = (yielded, false);
                                break;
                            }
                        }
                    }
                    // Persist the cursor; on exhaustion latch permanent-done.
                    let store = if result.1 { usize::MAX } else { index };
                    if let HeapObj::Iterator { index: i, .. } = self.heap.get_mut(it_idx) {
                        *i = store;
                    }
                    result
                } else {
                    match self.heap.get_mut(it_idx) {
                        HeapObj::Iterator { items, index: i, .. } => {
                            if *i < items.len() {
                                let v = items[*i];
                                *i += 1;
                                (v, false)
                            } else {
                                (Value::UNDEFINED, true)
                            }
                        }
                        _ => unreachable!(),
                    }
                };
                let mut m = ObjMap::new();
                m.set("value", val);
                m.set("done", Value::bool(done));
                Value::heap(self.heap.alloc(HeapObj::Object(m)))
            }
            ITER_SELF => this, // `iter[Symbol.iterator]()` returns the iterator itself
            ITER_DISPOSE => {
                // %IteratorPrototype% [ @@dispose ](): GetMethod(O, "return"); if present
                // Call it; return undefined. A non-callable, non-nullish `return` throws.
                let ret = self.get_prop(this, "return")?;
                if !ret.is_nullish() {
                    if !self.is_callable(ret) {
                        return Err(Thrown("TypeError: iterator return is not a function".into()));
                    }
                    self.call_value(ret, this, &[])?;
                }
                Value::UNDEFINED
            }
            // ES2025 Iterator Helpers (%Iterator.prototype%).
            ITER_MAP | ITER_FILTER | ITER_TAKE | ITER_DROP | ITER_FLATMAP | ITER_REDUCE
            | ITER_TOARRAY | ITER_FOREACH | ITER_SOME | ITER_EVERY | ITER_FIND => {
                self.iter_helper_method(id, this, args)?
            }
            ITER_HELPER_NEXT => {
                let kind = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::IterHelper { kind, .. }) => *kind,
                    _ => {
                        return Err(Thrown(
                            "TypeError: Iterator Helper next called on an incompatible receiver".into(),
                        ))
                    }
                };
                // kind 7 = a zip/zipKeyed helper (multi-iterator, lockstep).
                if kind == 7 {
                    self.iter_zip_next(this.heap_index())?
                } else {
                    self.iter_helper_next(this.heap_index())?
                }
            }
            ITER_HELPER_RETURN => {
                {
                    let (kind, source, inner, was_done, started) = match this
                        .is_heap()
                        .then(|| self.heap.get(this.heap_index()))
                    {
                        Some(HeapObj::IterHelper { kind, source, inner, done, running, idx, .. }) => {
                            // Re-entrant return() while a step is executing is a TypeError
                            // (GeneratorValidate), the same as a re-entrant next().
                            if *running {
                                return Err(Thrown(
                                    "TypeError: Iterator is already running".into(),
                                ));
                            }
                            // `idx > 0` ⇒ the helper has yielded (suspended at a yield),
                            // so its return() resumes it in the "executing" state. A
                            // suspended-START return completes WITHOUT that brand, so a
                            // re-entrant next()/return() during its close does not throw.
                            (*kind, *source, *inner, *done, *idx > 0)
                        }
                        _ => {
                            return Err(Thrown(
                                "TypeError: Iterator Helper return called on an incompatible receiver"
                                    .into(),
                            ))
                        }
                    };
                    // kind 5 = the WrapForValidIterator (Iterator.from): its return()
                    // DELEGATES — GetMethod(iterator, "return") and, if present, returns
                    // that method's result verbatim; otherwise CreateIterResultObject(
                    // undefined, true). (It does not close-and-return-done like a helper.)
                    if kind == 5 {
                        let ret = self.get_prop(source, "return")?;
                        if ret.is_nullish() {
                            return Ok(self.iter_result(Value::UNDEFINED, true));
                        }
                        if !self.is_callable(ret) {
                            return Err(Thrown("TypeError: iterator return is not a function".into()));
                        }
                        return self.call_value(ret, source, &[]);
                    }
                    // Mark done first (a re-entrant return is then a no-op), then
                    // close the underlying iterator — `inner` for concat (its
                    // `source` is the pair-array), else `source`. Skip if already
                    // done or not yet started (no live underlying iterator). The
                    // `running` brand is held across the close so that a source's
                    // return() re-entering this helper's return()/next() is a TypeError
                    // (the spec models the close as the closure resuming "executing").
                    if let HeapObj::IterHelper { done, .. } = self.heap.get_mut(this.heap_index()) {
                        *done = true;
                    }
                    if !was_done {
                        // Only a suspended-YIELD return resumes "executing".
                        if started {
                            self.ih_set_running(this.heap_index(), true);
                        }
                        let r = if kind == 7 {
                            // zip: close every still-open input iterator (reverse,
                            // first close error propagates).
                            match self.iz_close_all(this.heap_index()) {
                                Some(e) => Err(e),
                                None => Ok(()),
                            }
                        } else {
                            // flatMap (kind 4) with a LIVE inner iterator (the
                            // current mapper result): close the inner first —
                            // its return() must be observed, and an inner
                            // close error skips the source close (sequential
                            // finally semantics).
                            let inner_first = if kind == 4 && self.is_object_value(inner) {
                                self.iterator_close(inner)
                            } else {
                                Ok(())
                            };
                            if inner_first.is_err() {
                                inner_first
                            } else {
                                let target = if kind == 6 { inner } else { source };
                                if self.is_object_value(target) {
                                    self.iterator_close(target)
                                } else {
                                    Ok(())
                                }
                            }
                        };
                        if started {
                            self.ih_set_running(this.heap_index(), false);
                        }
                        r?;
                    }
                }
                self.iter_result(Value::UNDEFINED, true)
            }
            ITER_FROM => self.iterator_from(a0)?,
            ITER_CONCAT => self.iterator_concat(args)?,
            ITER_ZIP => self.iterator_zip(a0, a1, false)?,
            ITER_ZIPKEYED => self.iterator_zip(a0, a1, true)?,
            // test262 `$262.detachArrayBuffer(ab)` / `$262.gc()`.
            DOLLAR262_DETACH => {
                if let Some(buf) = self.as_array_buffer(a0) {
                    if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(buf) {
                        *detached = true;
                        // resize_bytes(0) == clear for a Local Vec; a (harness-
                        // detached) Shared store drops its visible length to 0.
                        data.resize_bytes(0);
                    }
                }
                Value::NULL
            }
            DOLLAR262_GC => Value::UNDEFINED,
            DOLLAR262_EVAL_SCRIPT => {
                let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                let code = self.to_js_string(a0)?;
                self.eval_script(&code)?
            }
            UNWRAP_ITER_RESULT => {
                // Async-from-Sync unwrap reaction: `this` carries the bound
                // `done` flag; the argument is the awaited element value.
                let v = args.first().copied().unwrap_or(Value::UNDEFINED);
                let done = this == Value::bool(true);
                self.iter_result(v, done)
            }
            AFS_CLOSE_REJECT => {
                // Async-from-Sync close-on-rejection reaction (bound args =
                // [syncIterator], the call arg is the rejection reason):
                // IteratorClose(syncIterator, ThrowCompletion(reason)) — close
                // errors are swallowed (the throw completion wins) and the
                // original reason re-throws; run_microtask's Err arm then
                // rejects the capability promise with it.
                let iter = args.first().copied().unwrap_or(Value::UNDEFINED);
                let reason = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let _gc = self.gc_lock_guard();
                let _ = self.iterator_close(iter);
                self.pending_throw = Some(reason);
                return Err(Thrown(self.throw_message(reason)));
            }
            DOLLAR262_CREATE_REALM => self.create_realm(),
            // Object.prototype Annex-B accessor helpers.
            OBJPROTO_DEFINE_GETTER | OBJPROTO_DEFINE_SETTER => {
                // Spec order: ToObject(this) [step 1], THEN IsCallable(getter) [step 2],
                // THEN ToPropertyKey(P) [step 4]. A null/undefined receiver therefore
                // throws before the getter is type-checked or the key is coerced.
                self.require_object_coercible(this)?;
                if !self.is_callable(a1) {
                    return Err(Thrown(
                        "TypeError: Object.prototype.__define[GS]etter__: expecting a function".into(),
                    ));
                }
                let key = self.to_property_key(a0)?;
                let mut d = ObjMap::new();
                d.set(if id == OBJPROTO_DEFINE_GETTER { "get" } else { "set" }, a1);
                d.set("enumerable", Value::bool(true));
                d.set("configurable", Value::bool(true));
                let desc = Value::heap(self.heap.alloc(HeapObj::Object(d)));
                self.object_define_property(this, &key, desc)?;
                Value::UNDEFINED
            }
            OBJPROTO_LOOKUP_GETTER | OBJPROTO_LOOKUP_SETTER => {
                // ToObject(this) is step 1 — BEFORE ToPropertyKey(P) — so a non-coercible
                // receiver throws without ever coercing the key.
                self.require_object_coercible(this)?;
                let key = self.to_property_key(a0)?;
                // The chain walk uses Proxy-trap-aware [[GetOwnProperty]] /
                // [[GetPrototypeOf]] so a throwing trap propagates.
                self.lookup_accessor_checked(this, &key, id == OBJPROTO_LOOKUP_SETTER)?
            }
            OBJPROTO_PROTO_GET => {
                // `get __proto__`: RequireObjectCoercible(this) before ToObject.
                self.require_object_coercible(this)?;
                self.get_prototype_of_checked(this)?
            }
            OBJPROTO_PROTO_SET => {
                // `set __proto__`: RequireObjectCoercible(this) first; a non-object/
                // non-null value, or a non-object receiver, is a silent no-op;
                // otherwise [[SetPrototypeOf]] runs and a rejected change throws (the
                // shared guards: non-extensible / cycle / immutable prototype / a
                // Proxy trap returning falsish).
                self.require_object_coercible(this)?;
                if (self.is_object_value(a0) || a0 == Value::NULL) && self.is_object_value(this)
                    && !self.ordinary_set_prototype_of(this, a0)?
                {
                    return Err(Thrown(
                        "TypeError: cannot set prototype (target is non-extensible, the change is cyclic, or it has an immutable prototype)".into(),
                    ));
                }
                Value::UNDEFINED
            }
            ERROR_IS_ERROR => {
                // Error.isError(arg) (ES2024): true iff arg is an object carrying
                // the [[ErrorData]] internal slot (a genuine Error instance — NOT a
                // fake with Error.prototype but no slot, nor a primitive).
                Value::bool(a0.is_heap() && self.error_data.contains(&a0.heap_index()))
            }
            ERROR_STACK_GET => {
                // get stack: a non-Object receiver throws; an object without
                // [[ErrorData]] yields undefined; an Error instance yields an
                // implementation-defined stack string.
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: Error.prototype.stack getter called on a non-object".into(),
                    ));
                }
                if this.is_heap() && self.error_data.contains(&this.heap_index()) {
                    self.alloc_str(String::new())
                } else {
                    Value::UNDEFINED
                }
            }
            ERROR_STACK_SET => {
                // set stack(v): SetterThatIgnoresPrototypeProperties(this,
                // %Error.prototype%, "stack", v). A non-Object receiver, a
                // non-String value, or a write whose receiver IS the home
                // object throws TypeError; otherwise create an own data
                // property (none yet) or ordinary-Set the existing own one.
                if !self.is_object_value(this) {
                    return Err(Thrown(
                        "TypeError: Error.prototype.stack setter called on a non-object".into(),
                    ));
                }
                if !self.is_string_value(a0) {
                    return Err(Thrown(
                        "TypeError: Error.prototype.stack setter requires a string value".into(),
                    ));
                }
                // A createRealm child's `Error.prototype` is that realm's HOME
                // object for its copied setter (which would otherwise re-enter
                // itself through the own accessor): the SameValue(home) check
                // applies there too, throwing the CHILD's TypeError.
                if this.is_heap() && !self.realm_global_objs.is_empty() {
                    let ti = this.heap_index();
                    if let Some(&r) = self.obj_realm.get(&ti) {
                        if self.realms.get(r as usize).and_then(|m| m.get(&self.error_protos[0]))
                            == Some(&ti)
                        {
                            let msg = "TypeError: Cannot assign to read only property 'stack'";
                            let e = self.alloc_error_from_message(msg);
                            self.realm_adopt_error_to(e, r);
                            self.pending_throw = Some(e);
                            return Err(Thrown(msg.into()));
                        }
                    }
                }
                self.setter_ignoring_proto_props(this, self.error_protos[0], "stack", a0)?;
                Value::UNDEFINED
            }
            REGEXP_LEGACY_GET => {
                // GetLegacyRegExpStaticProperty: the receiver must be the %RegExp%
                // constructor itself (SameValue(C, thisValue)); else TypeError. The
                // value is the relevant last-match slot — not yet tracked, so the
                // default empty string (the prop-desc / brand tests check shape +
                // brand, not the captured value).
                if !(this.is_heap() && this.heap_index() == self.regexp_ctor) {
                    return Err(Thrown(
                        "TypeError: RegExp legacy static getter called on a non-%RegExp% receiver".into(),
                    ));
                }
                self.alloc_str(String::new())
            }
            REGEXP_LEGACY_SET => {
                // SetLegacyRegExpStaticProperty: same %RegExp%-constructor brand
                // check; the assignment is accepted (not yet tracked → a no-op).
                if !(this.is_heap() && this.heap_index() == self.regexp_ctor) {
                    return Err(Thrown(
                        "TypeError: RegExp legacy static setter called on a non-%RegExp% receiver".into(),
                    ));
                }
                Value::UNDEFINED
            }
            // get %AbstractModuleSource%.prototype[@@toStringTag]: undefined for
            // any receiver without a [[ModuleSourceClassName]] slot — i.e. every
            // value today (never throws, even on primitives).
            ABSTRACT_MODULE_SOURCE_TAG_GET => Value::UNDEFINED,
            // PromiseReactionJob against an EXOTIC capability promise: run the
            // handler (Identity/Thrower when non-callable) and settle through the
            // capability's captured resolve/reject functions IN THE SAME JOB.
            CAP_REACTION => {
                let handler = args.first().copied().unwrap_or(Value::UNDEFINED);
                let cap_resolve = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let cap_reject = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let is_reject = args.get(3).copied().unwrap_or(Value::FALSE) == Value::TRUE;
                let arg = args.get(4).copied().unwrap_or(Value::UNDEFINED);
                // Un-rooted capability functions held across the JS re-entries.
                let _gc = self.gc_lock_guard();
                if !self.is_callable(handler) {
                    // Identity → capResolve(arg); Thrower → capReject(reason).
                    let f = if is_reject { cap_reject } else { cap_resolve };
                    self.call_value(f, Value::UNDEFINED, &[arg])?;
                    return Ok(Value::UNDEFINED);
                }
                match self.call_value(handler, Value::UNDEFINED, &[arg]) {
                    Ok(v) => self.call_value(cap_resolve, Value::UNDEFINED, &[v])?,
                    Err(Thrown(msg)) => {
                        let e = match self.pending_throw.take() {
                            Some(v) => v,
                            None => self.alloc_error_from_message(&msg),
                        };
                        self.call_value(cap_reject, Value::UNDEFINED, &[e])?
                    }
                };
                Value::UNDEFINED
            }
            ITER_TAG_GET => self.alloc_str("Iterator".to_string()),
            ITER_TAG_SET => {
                self.setter_ignoring_proto_props(
                    this,
                    self.iterator_proto_root,
                    "@@toStringTag",
                    a0,
                )?;
                Value::UNDEFINED
            }
            ITER_CTOR_GET => {
                if self.iterator_ctor != 0 {
                    Value::heap(self.iterator_ctor)
                } else {
                    Value::UNDEFINED
                }
            }
            ITER_CTOR_SET => {
                self.setter_ignoring_proto_props(
                    this,
                    self.iterator_proto_root,
                    "constructor",
                    a0,
                )?;
                Value::UNDEFINED
            }
            // Number static methods as values (no coercion, per spec).
            NUM_IS_INTEGER => Value::bool(num_is_integer(a0)),
            NUM_IS_NAN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
            NUM_IS_FINITE => Value::bool(num_is_finite(a0)),
            NUM_IS_SAFE_INTEGER => Value::bool(num_is_safe_integer(a0)),
            // Global functions as values.
            GLOBAL_PARSE_INT => {
                // ToString(string) — observable (calls toString, not valueOf) and
                // abrupt; then radix = ToInt32(ToNumber(radix)) (NaN/±Infinity → 0,
                // i.e. the default base). ToString runs BEFORE the radix coercion.
                let s = self.to_js_string(a0)?;
                let radix = if args.len() >= 2 {
                    let r = self.to_number_coerce(a1)?;
                    if r.is_finite() { r as i64 as i32 } else { 0 }
                } else {
                    0
                };
                Value::num(parse_int(&s, radix))
            }
            GLOBAL_PARSE_FLOAT => {
                let s = self.to_js_string(a0)?;
                Value::num(parse_float(&s))
            }
            // URI codecs. The `extra`/`reserved` byte sets are the ASCII chars
            // kept verbatim beyond uriUnescaped (encode) / left percent-escaped
            // (decode). Malformed input → URIError.
            GLOBAL_ENCODE_URI => {
                let s = self.to_js_string(a0)?;
                // Encode 6.2.c: a LONE surrogate is a URIError. The lossy
                // coercion above hides it (U+FFFD) — check the exact bytes.
                self.uri_check_wellformed(a0)?;
                match uri_encode(&s, b"#;/?:@&=+$,") {
                    Ok(r) => self.alloc_str(r),
                    Err(_) => return Err(Thrown("URIError: URI malformed".into())),
                }
            }
            GLOBAL_ENCODE_URI_COMPONENT => {
                let s = self.to_js_string(a0)?;
                self.uri_check_wellformed(a0)?; // lone surrogate -> URIError
                match uri_encode(&s, b"") {
                    Ok(r) => self.alloc_str(r),
                    Err(_) => return Err(Thrown("URIError: URI malformed".into())),
                }
            }
            GLOBAL_DECODE_URI => {
                // The input must be read EXACTLY (a raw lone surrogate is a
                // non-'%' code unit the Decode algorithm copies verbatim).
                let sv = self.to_str_value(a0)?;
                let bytes = self
                    .heap
                    .str_wtf8_cow(sv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                match uri_decode(&bytes, b";/?:@&=+$,#") {
                    Ok(r) => Value::heap(self.heap.alloc_js(r)),
                    Err(_) => return Err(Thrown("URIError: URI malformed".into())),
                }
            }
            GLOBAL_DECODE_URI_COMPONENT => {
                let sv = self.to_str_value(a0)?;
                let bytes = self
                    .heap
                    .str_wtf8_cow(sv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                match uri_decode(&bytes, b"") {
                    Ok(r) => Value::heap(self.heap.alloc_js(r)),
                    Err(_) => return Err(Thrown("URIError: URI malformed".into())),
                }
            }
            GLOBAL_ESCAPE => {
                let s = self.to_js_string(a0)?;
                let r = escape_str(&s);
                self.alloc_str(r)
            }
            GLOBAL_UNESCAPE => {
                let s = self.to_js_string(a0)?;
                let r = unescape_str(&s);
                self.alloc_str(r)
            }
            U8_TO_BASE64 => {
                let idx = self.u8_brand(this)?;
                let opts = args.first().copied().unwrap_or(Value::UNDEFINED);
                // GetOptionsObject + read alphabet/omitPadding BEFORE the bytes (a
                // getter on the options object may observably mutate the array).
                let (url, omit) = if opts == Value::UNDEFINED {
                    (false, false)
                } else if self.is_object_value(opts) {
                    let a = self.get_prop(opts, "alphabet")?;
                    let url = if a == Value::UNDEFINED {
                        false
                    } else if self.is_string_value(a) {
                        match self.to_js_string(a)?.as_str() {
                            "base64" => false,
                            "base64url" => true,
                            _ => {
                                return Err(Thrown(
                                    "TypeError: toBase64 alphabet must be \"base64\" or \"base64url\"".into(),
                                ))
                            }
                        }
                    } else {
                        return Err(Thrown("TypeError: toBase64 alphabet must be a string".into()));
                    };
                    let op = self.get_prop(opts, "omitPadding")?;
                    (url, self.truthy(op))
                } else {
                    return Err(Thrown("TypeError: toBase64 options must be an object".into()));
                };
                let bytes = self
                    .u8_bytes(idx)
                    .ok_or_else(|| Thrown("TypeError: Uint8Array buffer is detached".into()))?;
                let s = to_base64(&bytes, url, omit);
                self.alloc_str(s)
            }
            U8_FROM_BASE64 => {
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if !self.is_string_value(arg) {
                    return Err(Thrown(
                        "TypeError: Uint8Array.fromBase64 argument must be a string".into(),
                    ));
                }
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (url, lch) = self.read_b64_decode_opts(opts)?;
                let s = self.to_js_string(arg)?;
                let (_, b, err) = from_base64(&s, url, lch, usize::MAX);
                if let Some(e) = err {
                    return Err(e);
                }
                let buf = self.alloc_array_buffer(b.len());
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buf) {
                    data.copy_from_slice(&b);
                }
                self.alloc_typed_array(buf, 1, 0, b.len())
            }
            U8_SET_FROM_BASE64 => {
                let idx = self.u8_brand(this)?;
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if !self.is_string_value(arg) {
                    return Err(Thrown(
                        "TypeError: Uint8Array.prototype.setFromBase64 argument must be a string".into(),
                    ));
                }
                // Writing into an immutable-backed view is a TypeError — verified
                // BEFORE reading the options object (its getters must not run).
                if let HeapObj::TypedArray { buffer, .. } = self.heap.get(idx) {
                    let buffer = *buffer;
                    if self.immutable_buffers.contains(&buffer) {
                        return Err(Thrown(
                            "TypeError: Cannot setFromBase64 into a TypedArray backed by an immutable ArrayBuffer".into(),
                        ));
                    }
                }
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (url, lch) = self.read_b64_decode_opts(opts)?;
                let s = self.to_js_string(arg)?;
                let max = self
                    .ta_effective_len(idx)
                    .ok_or_else(|| Thrown("TypeError: Uint8Array buffer is detached".into()))?;
                let (read, b, err) = from_base64(&s, url, lch, max);
                let written = b.len();
                self.u8_write(idx, &b);
                if let Some(e) = err {
                    return Err(e);
                }
                let mut m = crate::heap::ObjMap::new();
                let attr = crate::heap::PropAttr {
                    writable: true,
                    enumerable: true,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                };
                m.define("read", Value::num(read as f64), attr);
                m.define("written", Value::num(written as f64), attr);
                let obj = self.heap.alloc(HeapObj::Object(m));
                if self.obj_proto != 0 {
                    self.proto_of.insert(obj, Value::heap(self.obj_proto));
                }
                Value::heap(obj)
            }
            U8_TO_HEX => {
                let idx = self.u8_brand(this)?;
                let bytes = self
                    .u8_bytes(idx)
                    .ok_or_else(|| Thrown("TypeError: Uint8Array buffer is detached".into()))?;
                let mut s = String::with_capacity(bytes.len() * 2);
                for b in bytes {
                    let h = hex_lower(b);
                    s.push(h[0] as char);
                    s.push(h[1] as char);
                }
                self.alloc_str(s)
            }
            U8_SET_FROM_HEX => {
                let idx = self.u8_brand(this)?;
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if !self.is_string_value(arg) {
                    return Err(Thrown(
                        "TypeError: Uint8Array.prototype.setFromHex argument must be a string".into(),
                    ));
                }
                // Writing into a TypedArray backed by an immutable ArrayBuffer is a
                // TypeError (checked before any decode/write, so contents are never
                // mutated — even for an empty input).
                if let HeapObj::TypedArray { buffer, .. } = self.heap.get(idx) {
                    let buffer = *buffer;
                    if self.immutable_buffers.contains(&buffer) {
                        return Err(Thrown(
                            "TypeError: Cannot setFromHex into a TypedArray backed by an immutable ArrayBuffer".into(),
                        ));
                    }
                }
                let s = self.to_js_string(arg)?;
                let max = self
                    .ta_effective_len(idx)
                    .ok_or_else(|| Thrown("TypeError: Uint8Array buffer is detached".into()))?;
                let (read, bytes, err) = from_hex(&s, max);
                let written = bytes.len();
                self.u8_write(idx, &bytes);
                if let Some(e) = err {
                    return Err(e);
                }
                // The { read, written } result (an ordinary object).
                let mut m = crate::heap::ObjMap::new();
                let attr = crate::heap::PropAttr {
                    writable: true,
                    enumerable: true,
                    configurable: true,
                    accessor: false,
                    setter: Value::UNDEFINED,
                };
                m.define("read", Value::num(read as f64), attr);
                m.define("written", Value::num(written as f64), attr);
                let obj = self.heap.alloc(HeapObj::Object(m));
                if self.obj_proto != 0 {
                    self.proto_of.insert(obj, Value::heap(self.obj_proto));
                }
                Value::heap(obj)
            }
            U8_FROM_HEX => {
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if !self.is_string_value(arg) {
                    return Err(Thrown(
                        "TypeError: Uint8Array.fromHex argument must be a string".into(),
                    ));
                }
                let s = self.to_js_string(arg)?;
                let (_, bytes, err) = from_hex(&s, usize::MAX);
                if let Some(e) = err {
                    return Err(e);
                }
                let buf = self.alloc_array_buffer(bytes.len());
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buf) {
                    data.copy_from_slice(&bytes);
                }
                self.alloc_typed_array(buf, 1, 0, bytes.len())
            }
            // ? ToNumber(x): coerce objects (@@toPrimitive/valueOf/toString) and
            // propagate abrupt completions (throwing valueOf, Symbol → TypeError).
            GLOBAL_IS_NAN => Value::bool(self.to_number_coerce(a0)?.is_nan()),
            GLOBAL_IS_FINITE => Value::bool(self.to_number_coerce(a0)?.is_finite()),
            GLOBAL_EVAL => {
                // eval(x): if x is not a String, return it unchanged (spec 19.2.1).
                let is_str = a0.is_heap()
                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. });
                if !is_str {
                    a0
                } else {
                    let code = self.display(a0);
                    // EXACT WTF-8 bytes when the code string holds lone
                    // surrogates: the lossy `code` aligns with them
                    // byte-for-byte (U+FFFD and a WTF-8 surrogate are both
                    // 3 bytes), letting regex literals recover their exact
                    // pattern text. None (O(1)) for well-formed sources.
                    let exact = self.heap.str_exact_if_not_wellformed(a0.heap_index());
                    return self.do_eval(&code, false, false, None, None, false, false, Value::UNDEFINED, None, true, None, Vec::new(), None, None, exact.as_deref());
                }
            }
            // String static methods.
            STR_FROM_CHAR_CODE => {
                // ToUint16 per arg; adjacent surrogate halves combine, a lone
                // half stays a real lone surrogate — see `string_from_char_codes`.
                let js = self.string_from_char_codes(args)?;
                Value::heap(self.heap.alloc_js(js))
            }
            STR_FROM_CODE_POINT => {
                // WTF-8 build: a surrogate code point passes through as a real
                // lone surrogate (and a high+low argument pair canonicalizes
                // into the astral scalar, matching the UTF-16 join semantics).
                let mut out: Vec<u8> = Vec::with_capacity(args.len());
                for &v in args {
                    let n = self.to_number_strict(v)?;
                    if !n.is_finite() || n < 0.0 || n > 0x10FFFF as f64 || n.fract() != 0.0 {
                        return Err(Thrown(format!("RangeError: Invalid code point {n}")));
                    }
                    crate::heap::wtf8_push_cp(&mut out, n as u32);
                }
                let js = crate::heap::JsStr::from_wtf8(out);
                Value::heap(self.heap.alloc_js(js))
            }
            // Date static methods as values.
            DATE_NOW => Value::num(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(0.0),
            ),
            DATE_PARSE => Value::num(parse_date(&self.display(a0))),
            DATE_UTC => Value::num(self.date_utc_ms(args)?),
            STR_RAW => {
                // String.raw(template, ...subs): ToObject(template.raw), then
                // interleave ToString(raw[i]) with ToString(subs[i]). ToObject of a
                // nullish `raw` throws TypeError, and ToString throws on a Symbol —
                // both must propagate (display() would have swallowed them).
                self.require_object_coercible(a0)?; // ToObject(template)
                let raw0 = self.get_prop(a0, "raw")?;
                self.require_object_coercible(raw0)?; // ToObject(template.raw)
                let raw = self.to_object(raw0)?;
                let len_v = self.get_prop(raw, "length")?;
                let n = self.to_number(len_v)?;
                let raw_len = if n.is_finite() && n > 0.0 { n as usize } else { 0 };
                let subs = args.get(1..).unwrap_or(&[]);
                let mut out = String::new();
                for i in 0..raw_len {
                    let seg = self.get_index(raw, Value::int(i as i32))?;
                    let seg = self.to_js_string(seg)?;
                    out.push_str(&seg);
                    if i + 1 == raw_len {
                        break;
                    }
                    if let Some(sub) = subs.get(i) {
                        let sub = self.to_js_string(*sub)?;
                        out.push_str(&sub);
                    }
                }
                self.alloc_str(out)
            }
            // Object.prototype.toLocaleString() → this.toString().
            PROTO_TO_LOCALE_STRING => {
                let ts = self.get_prop(this, "toString")?;
                if self.is_callable(ts) {
                    self.call_value(ts, this, &[])?
                } else {
                    return Err(Thrown("TypeError: toString is not callable".into()));
                }
            }
            // `Math.<op>` as a value (`Math.abs`, `Math.max`, …). The direct call
            // form is compile-lowered to MathOp; these back the value form.
            _ if native::math_method(id).is_some() => {
                let (_, op, _) = native::math_method(id).unwrap();
                Value::num(self.eval_math_args(op, args)?)
            }
            // Promise static methods invoked as values (`Promise.resolve`, …).
            PROMISE_RESOLVE | PROMISE_REJECT | PROMISE_ALL | PROMISE_ALLSETTLED | PROMISE_RACE
            | PROMISE_ANY | PROMISE_ALLKEYED | PROMISE_ALLSETTLEDKEYED => {
                // These static methods read `this` as the constructor C (for the
                // result's NewPromiseCapability); a non-constructor `this` is a
                // TypeError. (The single-offset model still builds a native Promise.)
                if !self.is_constructor(this) {
                    return Err(Thrown(
                        "TypeError: Promise static method called on a non-constructor".into(),
                    ));
                }
                let a = args.first().copied().unwrap_or(Value::UNDEFINED);
                match id {
                    PROMISE_RESOLVE => {
                        // Promise.resolve(x): the native Promise ctor takes the fast
                        // path; a custom `this` runs NewPromiseCapability(this) and
                        // observably Calls its resolve — returning x unchanged when x
                        // is already a promise whose .constructor === this.
                        if this == self.promise_ctor_value() {
                            // PromiseResolve step 2: x is returned AS-IS only when
                            // Get(x, "constructor") is C — a promise whose
                            // constructor was changed gets a fresh adopting promise.
                            let is_native_promise = a.is_heap()
                                && matches!(self.heap.get(a.heap_index()), HeapObj::Promise { .. });
                            if is_native_promise && self.get_prop(a, "constructor")? != this {
                                let p = self.alloc_promise();
                                self.resolve(p, a);
                                Value::heap(p)
                            } else {
                                Value::heap(self.to_promise(a))
                            }
                        } else {
                            let already = a.is_heap()
                                && matches!(self.heap.get(a.heap_index()), HeapObj::Promise { .. })
                                && self.get_prop(a, "constructor")? == this;
                            if already {
                                a
                            } else {
                                let (promise, resolve, _) = self.new_promise_capability(this)?;
                                self.call_value(resolve, Value::UNDEFINED, &[a])?;
                                promise
                            }
                        }
                    }
                    PROMISE_REJECT => {
                        // As PROMISE_RESOLVE: a custom `this` settles through its own
                        // capability's reject function.
                        if this == self.promise_ctor_value() {
                            let p = self.alloc_promise();
                            self.reject(p, a);
                            Value::heap(p)
                        } else {
                            let (promise, _, reject) = self.new_promise_capability(this)?;
                            self.call_value(reject, Value::UNDEFINED, &[a])?;
                            promise
                        }
                    }
                    PROMISE_ALL => self.promise_combine(crate::heap::CombKind::All, a, this)?,
                    PROMISE_ALLSETTLED => {
                        self.promise_combine(crate::heap::CombKind::AllSettled, a, this)?
                    }
                    PROMISE_RACE => self.promise_combine(crate::heap::CombKind::Race, a, this)?,
                    PROMISE_ALLKEYED => {
                        self.promise_combine(crate::heap::CombKind::AllKeyed, a, this)?
                    }
                    PROMISE_ALLSETTLEDKEYED => {
                        self.promise_combine(crate::heap::CombKind::AllSettledKeyed, a, this)?
                    }
                    _ => self.promise_combine(crate::heap::CombKind::Any, a, this)?, // PROMISE_ANY
                }
            }
            // `%TypedArray%.prototype.<m>` invoked as a value (`.map.call(ta, …)`).
            _ if (TA_METHOD_BASE..TA_METHOD_BASE + TA_PROTO_METHODS.len() as u16).contains(&id) => {
                let m = TA_PROTO_METHODS[(id - TA_METHOD_BASE) as usize];
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::TypedArray { .. })) {
                    return Err(Thrown(format!(
                        "TypeError: TypedArray.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.typed_array_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (DV_METHOD_BASE..DV_METHOD_BASE + DV_PROTO_METHODS.len() as u16).contains(&id) => {
                let m = DV_PROTO_METHODS[(id - DV_METHOD_BASE) as usize];
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::DataView { .. })) {
                    return Err(Thrown(format!(
                        "TypeError: DataView.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.dataview_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_SLICE => {
                // slice requires a non-shared ArrayBuffer receiver.
                let shared = this.is_heap() && self.shared_buffers.contains(&this.heap_index());
                if shared || !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::ArrayBuffer { .. })) {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer.prototype.slice called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "slice", args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_RESIZE => {
                let shared = this.is_heap() && self.shared_buffers.contains(&this.heap_index());
                if shared || !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::ArrayBuffer { .. })) {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer.prototype.resize called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "resize", args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_TRANSFER_IMMUTABLE | ARRAYBUFFER_SLICE_IMMUTABLE | ARRAYBUFFER_TRANSFER
            | ARRAYBUFFER_TRANSFER_FIXED => {
                let shared = this.is_heap() && self.shared_buffers.contains(&this.heap_index());
                if shared
                    || !matches!(
                        this.is_heap().then(|| self.heap.get(this.heap_index())),
                        Some(HeapObj::ArrayBuffer { .. })
                    )
                {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer transfer method called on incompatible receiver".into(),
                    ));
                }
                let name = match id {
                    ARRAYBUFFER_TRANSFER_IMMUTABLE => "transferToImmutable",
                    ARRAYBUFFER_SLICE_IMMUTABLE => "sliceToImmutable",
                    ARRAYBUFFER_TRANSFER => "transfer",
                    _ => "transferToFixedLength",
                };
                self.arraybuffer_method(this.heap_index(), name, args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_ISVIEW => Value::bool(
                a0.is_heap()
                    && matches!(
                        self.heap.get(a0.heap_index()),
                        HeapObj::TypedArray { .. } | HeapObj::DataView { .. }
                    ),
            ),
            _ if (BUFFER_GETTER_BASE..BUFFER_GETTER_BASE + BUFFER_GETTERS.len() as u16)
                .contains(&id) =>
            {
                let (name, kind) = BUFFER_GETTERS[(id - BUFFER_GETTER_BASE) as usize];
                // The ArrayBuffer getters (kind 0: byteLength/maxByteLength/resizable/
                // detached) require IsSharedArrayBuffer(this) to be false — a
                // SharedArrayBuffer is the same HeapObj internally but must be rejected.
                let shared = this.is_heap() && self.shared_buffers.contains(&this.heap_index());
                let ok = this.is_heap()
                    && matches!(
                        (kind, self.heap.get(this.heap_index())),
                        (0, HeapObj::ArrayBuffer { .. })
                            | (1, HeapObj::TypedArray { .. })
                            | (2, HeapObj::DataView { .. })
                    )
                    && !(kind == 0 && shared);
                if !ok {
                    return Err(Thrown(format!(
                        "TypeError: get {name} called on an incompatible receiver"
                    )));
                }
                // The instance arm of get_member computes the value directly (it
                // never consults this proto accessor, so there's no recursion).
                self.get_member(this, name, this)?
            }
            SAB_GROW => {
                let ok = this.is_heap() && self.shared_buffers.contains(&this.heap_index());
                if !ok {
                    return Err(Thrown(
                        "TypeError: SharedArrayBuffer.prototype.grow called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "grow", args)?.unwrap_or(Value::UNDEFINED)
            }
            SAB_SLICE => {
                // Brand check: a SharedArrayBuffer receiver only (a plain
                // ArrayBuffer must NOT pass through SAB's slice).
                if !(this.is_heap() && self.shared_buffers.contains(&this.heap_index())) {
                    return Err(Thrown(
                        "TypeError: SharedArrayBuffer.prototype.slice called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "slice", args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (ATOMICS_BASE..ATOMICS_BASE + ATOMICS_METHODS.len() as u16).contains(&id) => {
                let (name, _) = ATOMICS_METHODS[(id - ATOMICS_BASE) as usize];
                self.atomics_op(name, args)?
            }
            DISPOSABLE_USE | DISPOSABLE_ADOPT | DISPOSABLE_DEFER | DISPOSABLE_DISPOSE
            | DISPOSABLE_DISPOSE_ASYNC | DISPOSABLE_MOVE | DISPOSABLE_DISPOSED_GET
            | ASYNC_DISPOSABLE_USE | ASYNC_DISPOSABLE_ADOPT | ASYNC_DISPOSABLE_DEFER
            | ASYNC_DISPOSABLE_MOVE => {
                self.disposable_op(id, this, args)?
            }
            GEN_NEXT | GEN_RETURN | GEN_THROW => {
                let name = match id {
                    GEN_NEXT => "next",
                    GEN_RETURN => "return",
                    _ => "throw",
                };
                if !this.is_heap()
                    || !matches!(self.heap.get(this.heap_index()), HeapObj::Generator { .. })
                {
                    return Err(Thrown(format!(
                        "TypeError: {name} called on a non-generator object"
                    )));
                }
                self.generator_method(this.heap_index(), name, args)?
                    .unwrap_or(Value::UNDEFINED)
            }
            ASYNCGEN_NEXT | ASYNCGEN_RETURN | ASYNCGEN_THROW => {
                let name = match id {
                    ASYNCGEN_NEXT => "next",
                    ASYNCGEN_RETURN => "return",
                    _ => "throw",
                };
                if !this.is_heap()
                    || !matches!(self.heap.get(this.heap_index()), HeapObj::AsyncGenerator(_))
                {
                    // AsyncGeneratorEnqueue: a non-async-generator `this` REJECTS the
                    // returned promise with a TypeError, not a synchronous throw.
                    let err = self.alloc_error_from_message(&format!(
                        "TypeError: {name} called on a non-async-generator object"
                    ));
                    let p = self.alloc_promise();
                    self.reject(p, err);
                    Value::heap(p)
                } else {
                    self.async_generator_method(this.heap_index(), name, args)
                        .unwrap_or(Value::UNDEFINED)
                }
            }
            SHADOWREALM_EVALUATE | SHADOWREALM_IMPORTVALUE => {
                self.shadowrealm_op(id, this, args)?
            }
            _ if (SAB_GETTER_BASE..SAB_GETTER_BASE + SAB_GETTERS.len() as u16).contains(&id) => {
                let name = SAB_GETTERS[(id - SAB_GETTER_BASE) as usize];
                if !(this.is_heap() && self.shared_buffers.contains(&this.heap_index())) {
                    return Err(Thrown(format!(
                        "TypeError: get SharedArrayBuffer.prototype.{name} called on incompatible receiver"
                    )));
                }
                // The shared-buffer arm of get_member computes the value directly.
                self.get_member(this, name, this)?
            }
            PROXY_REVOCABLE => {
                // Proxy.revocable(target, handler) → { proxy, revoke }.
                let p = self.make_proxy(a0, a1)?;
                let revoke_fn = self.heap.alloc(HeapObj::Native(PROXY_REVOKE));
                let revoke = Value::heap(self.heap.alloc(HeapObj::Bound {
                    target: Value::heap(revoke_fn),
                    this: p,
                    args: Vec::new(),
                }));
                let mut m = ObjMap::new();
                m.set("proxy", p);
                m.set("revoke", revoke);
                Value::heap(self.heap.alloc(HeapObj::Object(m)))
            }
            PROXY_REVOKE => {
                if this.is_heap() {
                    if let HeapObj::Proxy { revoked, .. } = self.heap.get_mut(this.heap_index()) {
                        *revoked = true;
                    }
                }
                Value::UNDEFINED
            }
            _ if (TEMPORAL_M_BASE..TEMPORAL_M_BASE + TEMPORAL_DURATION_METHODS.len() as u16)
                .contains(&id) =>
            {
                let m = TEMPORAL_DURATION_METHODS[(id - TEMPORAL_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 0, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.Duration.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            TEMPORAL_DURATION_FROM => {
                // The EXACT f64 record: from() must store ℝ(𝔽(field)) untouched
                // by i64 saturation (a 4.5e21-microseconds field survives).
                let f = self.to_duration_f64(a0)?;
                self.make_duration(f)
            }
            TEMPORAL_DURATION_COMPARE => {
                let fa = self.to_duration_f64(a0)?;
                let fb = self.to_duration_f64(a1)?;
                let opts = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                Value::num(self.duration_compare(fa, fb, opts)?)
            }
            _ if (PD_M_BASE..PD_M_BASE + PLAINDATE_METHODS.len() as u16).contains(&id) => {
                let m = PLAINDATE_METHODS[(id - PD_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 1, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainDate.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINDATE_FROM => {
                // Per ToTemporalDate, the item is validated before the overflow
                // option's VALUE is observed: an object reads overflow then resolves
                // its fields, but a string/primitive must parse/reject FIRST (a
                // non-string primitive → TypeError, an invalid ISO string → RangeError),
                // so overflow is read only once the item is known-processable.
                let (y, m, d) = if self.is_object_value(a0) {
                    // Object: read the bag fields, THEN options.overflow (order-of-ops).
                    self.to_plain_date_overflow(a0, Some(a1))?
                } else {
                    let r = self.to_plain_date(a0)?;
                    self.read_overflow(a1)?;
                    r
                };
                self.make_plain_date(y, m, d)?
            }
            PLAINDATE_COMPARE => {
                let a = self.to_plain_date(a0)?;
                let b = self.to_plain_date(a1)?;
                let ea = iso_to_epoch_days(a.0, a.1, a.2);
                let eb = iso_to_epoch_days(b.0, b.1, b.2);
                Value::num(if ea < eb { -1.0 } else if ea > eb { 1.0 } else { 0.0 })
            }
            _ if (PT_M_BASE..PT_M_BASE + PLAINTIME_METHODS.len() as u16).contains(&id) => {
                let m = PLAINTIME_METHODS[(id - PT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 2, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINTIME_FROM => {
                // Validate the item before observing overflow (see PLAINDATE_FROM).
                let f = if self.is_object_value(a0) {
                    // Object: read the bag fields, THEN options.overflow (order-of-ops).
                    self.to_plain_time_overflow(a0, Some(a1))?
                } else {
                    let r = self.to_plain_time(a0)?;
                    self.read_overflow(a1)?;
                    r
                };
                self.make_plain_time(f)?
            }
            PLAINTIME_COMPARE => {
                let a = self.to_plain_time(a0)?;
                let b = self.to_plain_time(a1)?;
                let (ta, tb) = (time_to_ns(&a), time_to_ns(&b));
                Value::num(if ta < tb { -1.0 } else if ta > tb { 1.0 } else { 0.0 })
            }
            _ if (PDT_M_BASE..PDT_M_BASE + PLAINDATETIME_METHODS.len() as u16).contains(&id) => {
                let m = PLAINDATETIME_METHODS[(id - PDT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 3, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainDateTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINDATETIME_FROM => {
                // Validate the item before observing overflow (see PLAINDATE_FROM).
                let f = if self.is_object_value(a0)
                    && !matches!(self.heap.get(a0.heap_index()), HeapObj::Temporal { .. })
                {
                    // Plain bag: one PrepareTemporalFields pass (field gets, then
                    // the required-y/m/d TypeError), THEN options.overflow, THEN
                    // the deferred monthCode/range validation (order-of-ops).
                    let bag = self.read_pdt_bag(a0, false)?;
                    let reject = self.read_overflow(a1)?;
                    Self::finish_pdt_fields(&bag, reject)?
                } else {
                    let r = self.to_plain_date_time(a0)?;
                    self.read_overflow(a1)?;
                    r
                };
                self.make_plain_date_time(f)?
            }
            PLAINDATETIME_COMPARE => {
                let a = self.to_plain_date_time_limited(a0)?;
                let b = self.to_plain_date_time_limited(a1)?;
                let an = iso_to_epoch_days(a[0], a[1], a[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[a[3], a[4], a[5], a[6], a[7], a[8]]);
                let bn = iso_to_epoch_days(b[0], b[1], b[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[b[3], b[4], b[5], b[6], b[7], b[8]]);
                Value::num(if an < bn { -1.0 } else if an > bn { 1.0 } else { 0.0 })
            }
            _ if (INST_M_BASE..INST_M_BASE + INSTANT_METHODS.len() as u16).contains(&id) => {
                let m = INSTANT_METHODS[(id - INST_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 4, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.Instant.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            INST_FROM => {
                let ns = self.to_instant_ns(a0)?;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_MS => {
                // ToNumber (BigInt/Symbol → TypeError) then NumberToBigInt (a
                // non-integral / non-finite Number → RangeError).
                let n = self.to_number_strict(a0)?;
                if !n.is_finite() || n.fract() != 0.0 {
                    return Err(Thrown("RangeError: epochMilliseconds must be an integer".into()));
                }
                self.make_instant((n as i128) * 1_000_000)?
            }
            INST_FROM_EPOCH_SEC => {
                let n = self.to_number_strict(a0)?;
                if !n.is_finite() || n.fract() != 0.0 {
                    return Err(Thrown("RangeError: epochSeconds must be an integer".into()));
                }
                self.make_instant((n as i128) * 1_000_000_000)?
            }
            INST_FROM_EPOCH_NS => {
                // Beyond-i128 saturates (sign preserved) — certainly outside the
                // Instant range, which make_instant validates.
                let ns = self.to_bigint(a0)?.to_i128_sat();
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_US => {
                let ns = self.to_bigint(a0)?.to_i128_sat().saturating_mul(1_000);
                self.make_instant(ns)?
            }
            INST_COMPARE => {
                let a = self.to_instant_ns(a0)?;
                let b = self.to_instant_ns(a1)?;
                Value::num(if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 })
            }
            _ if (PYM_M_BASE..PYM_M_BASE + PLAINYEARMONTH_METHODS.len() as u16).contains(&id) => {
                let m = PLAINYEARMONTH_METHODS[(id - PYM_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 5, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainYearMonth.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINYEARMONTH_FROM => {
                // Validate the item before observing overflow (see PLAINDATE_FROM).
                let (y, m, rd) = if self.is_object_value(a0) {
                    // Object: read the bag fields, THEN options.overflow (order-of-ops).
                    self.to_plain_year_month_overflow(a0, Some(a1))?
                } else {
                    let r = self.to_plain_year_month(a0)?;
                    self.read_overflow(a1)?;
                    r
                };
                self.make_plain_year_month(y, m, rd)?
            }
            PLAINYEARMONTH_COMPARE => {
                let a = self.to_plain_year_month(a0)?;
                let b = self.to_plain_year_month(a1)?;
                // Compare the full ISO reference date (year, month, then reference day).
                let ka = (a.0, a.1, a.2);
                let kb = (b.0, b.1, b.2);
                Value::num(if ka < kb { -1.0 } else if ka > kb { 1.0 } else { 0.0 })
            }
            _ if (PMD_M_BASE..PMD_M_BASE + PLAINMONTHDAY_METHODS.len() as u16).contains(&id) => {
                let m = PLAINMONTHDAY_METHODS[(id - PMD_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 6, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainMonthDay.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (ZDT_M_BASE..ZDT_M_BASE + ZONEDDATETIME_METHODS.len() as u16).contains(&id) => {
                let m = ZONEDDATETIME_METHODS[(id - ZDT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 7, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.ZonedDateTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                match self.temporal_method(this.heap_index(), m, args)? {
                    Some(v) => v,
                    None => {
                        return Err(Thrown(format!(
                            "TypeError: Temporal.ZonedDateTime.prototype.{m} is not yet supported"
                        )))
                    }
                }
            }
            ZDT_FROM => self.zoned_date_time_from(a0, a1)?,
            ZDT_COMPARE => {
                let za = self.zoned_date_time_from(a0, Value::UNDEFINED)?;
                let zb = self.zoned_date_time_from(a1, Value::UNDEFINED)?;
                let na = self.zdt_epoch_ns(za.heap_index()).unwrap_or(0);
                let nb = self.zdt_epoch_ns(zb.heap_index()).unwrap_or(0);
                Value::num(match na.cmp(&nb) {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                })
            }
            PLAINMONTHDAY_FROM => {
                // Validate the item before observing overflow (see PLAINDATE_FROM).
                let (ry, m, d) = if self.is_object_value(a0) {
                    // Object: read the bag fields, THEN options.overflow (order-of-ops).
                    self.to_plain_month_day_overflow(a0, Some(a1))?
                } else {
                    let r = self.to_plain_month_day(a0)?;
                    self.read_overflow(a1)?;
                    r
                };
                self.make_plain_month_day(m, d, ry)?
            }
            // Temporal.Now — no timezone DB, so a named zone reports UTC, but a
            // numeric-offset zone shifts the wall-clock. The time-zone arg is
            // still validated (invalid string -> RangeError, wrong type -> TypeError).
            NOW_INSTANT => {
                let ns = Self::now_epoch_ns();
                self.make_instant(ns)?
            }
            NOW_PLAINDATETIME_ISO => {
                let (_, offset) = self.now_tz_id(a0)?;
                let ns = Self::now_epoch_ns() + offset as i128;
                let days = ns.div_euclid(DAY_NS);
                let t = ns_to_time(ns.rem_euclid(DAY_NS));
                let (y, mo, d) = epoch_days_to_iso(days as i64);
                self.make_plain_date_time([y, mo, d, t[0], t[1], t[2], t[3], t[4], t[5]])?
            }
            NOW_PLAINDATE_ISO => {
                let (_, offset) = self.now_tz_id(a0)?;
                let ns = Self::now_epoch_ns() + offset as i128;
                let (y, mo, d) = epoch_days_to_iso(ns.div_euclid(DAY_NS) as i64);
                self.make_plain_date(y, mo, d)?
            }
            NOW_PLAINTIME_ISO => {
                let (_, offset) = self.now_tz_id(a0)?;
                let ns = Self::now_epoch_ns() + offset as i128;
                self.make_plain_time(ns_to_time(ns.rem_euclid(DAY_NS)))?
            }
            NOW_ZONEDDATETIME_ISO => {
                let (id, offset) = self.now_tz_id(a0)?;
                let ns = Self::now_epoch_ns();
                self.alloc_zdt(ns, offset, id)?
            }
            NOW_TIMEZONE_ID => self.alloc_str("UTC".to_string()),
            // Shared Temporal toLocaleString: no Intl — same string as toString,
            // with a brand check (a non-Temporal receiver is a TypeError).
            TEMPORAL_TO_LOCALE_STRING => {
                let r = if this.is_heap() {
                    self.temporal_method(this.heap_index(), "toString", &[])?
                } else {
                    None
                };
                match r {
                    Some(v) => v,
                    None => {
                        return Err(Thrown(
                            "TypeError: toLocaleString called on a non-Temporal object".into(),
                        ))
                    }
                }
            }
            ZDT_GET_TZ_TRANSITION => {
                if !this.is_heap()
                    || !matches!(self.heap.get(this.heap_index()), HeapObj::Temporal { kind: 7, .. })
                {
                    return Err(Thrown(
                        "TypeError: getTimeZoneTransition called on a non-ZonedDateTime".into(),
                    ));
                }
                let _ = self.read_direction_option(a0)?;
                // Offset / single-offset (UTC) time zones have no transitions.
                Value::NULL
            }
            PDT_WITH_PLAIN_TIME => {
                if !this.is_heap()
                    || !matches!(self.heap.get(this.heap_index()), HeapObj::Temporal { kind: 3, .. })
                {
                    return Err(Thrown(
                        "TypeError: withPlainTime called on a non-PlainDateTime".into(),
                    ));
                }
                self.temporal_method(this.heap_index(), "withPlainTime", args)?
                    .unwrap_or(Value::UNDEFINED)
            }
            // ── Intl ──
            INTL_GET_CANONICAL_LOCALES => {
                let list = self.canonicalize_locale_list(a0)?;
                let items: Vec<Value> = list.into_iter().map(|s| self.alloc_str(s)).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(items)))
            }
            INTL_SUPPORTED_VALUES_OF => {
                let key = self.to_js_string(a0)?;
                let vals: &[&str] = match key.as_str() {
                    "calendar" => &["gregory", "iso8601"],
                    "collation" => &["default"],
                    "currency" => &["USD", "EUR", "GBP", "JPY"],
                    "numberingSystem" => &["latn"],
                    "timeZone" => &["UTC"],
                    "unit" => &["meter", "second", "byte"],
                    _ => {
                        return Err(Thrown(format!(
                            "RangeError: invalid key for supportedValuesOf: {key}"
                        )))
                    }
                };
                let items: Vec<Value> = vals.iter().map(|s| self.alloc_str(s.to_string())).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(items)))
            }
            INTL_SUPPORTED_LOCALES_OF => {
                let list = self.canonicalize_locale_list(a0)?;
                let items: Vec<Value> = list.into_iter().map(|s| self.alloc_str(s)).collect();
                Value::heap(self.heap.alloc(HeapObj::Array(items)))
            }
            INTL_RESOLVED_OPTIONS => {
                let resolved = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Intl { resolved, .. }) => *resolved,
                    _ => {
                        return Err(Thrown(
                            "TypeError: resolvedOptions called on an incompatible receiver".into(),
                        ))
                    }
                };
                self.clone_plain_object(resolved)
            }
            INTL_NF_FORMAT => {
                let resolved = self.intl_this(this, INTL_NUMBERFORMAT, "format")?;
                self.intl_number_format(resolved, a0)?
            }
            INTL_NF_FORMAT_TO_PARTS => {
                let resolved = self.intl_this(this, INTL_NUMBERFORMAT, "formatToParts")?;
                let formatted = self.intl_number_format(resolved, a0)?;
                let mut part = ObjMap::new();
                let ty = self.alloc_str("integer".to_string());
                part.set("type", ty);
                part.set("value", formatted);
                let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
            }
            INTL_DTF_FORMAT => {
                let resolved = self.intl_this(this, INTL_DATETIMEFORMAT, "format")?;
                let ms = if a0 == Value::UNDEFINED {
                    (Self::now_epoch_ns() / 1_000_000) as f64
                } else {
                    self.to_number(a0)?
                };
                let s = self.dtf_format(resolved, ms);
                self.alloc_str(s)
            }
            INTL_DTF_FORMAT_TO_PARTS => {
                let resolved = self.intl_this(this, INTL_DATETIMEFORMAT, "formatToParts")?;
                let ms = if a0 == Value::UNDEFINED {
                    (Self::now_epoch_ns() / 1_000_000) as f64
                } else {
                    self.to_number(a0)?
                };
                let s = self.dtf_format(resolved, ms);
                let mut part = ObjMap::new();
                let ty = self.alloc_str("literal".to_string());
                part.set("type", ty);
                let sv = self.alloc_str(s);
                part.set("value", sv);
                let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
            }
            INTL_COLLATOR_COMPARE => {
                let _ = self.intl_this(this, INTL_COLLATOR, "compare")?;
                let a = self.to_js_string(a0)?;
                let b = self.to_js_string(a1)?;
                Value::num(if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 })
            }
            INTL_PLURAL_SELECT => {
                let _ = self.intl_this(this, INTL_PLURALRULES, "select")?;
                let n = self.to_number(a0)?;
                let cat = if n == 1.0 { "one" } else { "other" };
                self.alloc_str(cat.to_string())
            }
            INTL_PLURAL_SELECT_RANGE => {
                let _ = self.intl_this(this, INTL_PLURALRULES, "selectRange")?;
                self.alloc_str("other".to_string())
            }
            INTL_LIST_FORMAT => {
                let resolved = self.intl_this(this, INTL_LISTFORMAT, "format")?;
                let items = self.iterate_to_vec(a0)?;
                let mut strs: Vec<String> = Vec::with_capacity(items.len());
                for v in items {
                    strs.push(self.to_js_string(v)?);
                }
                let t = self.display(self.intl_slot(resolved, "type"));
                let conj = if t == "disjunction" { "or" } else { "and" };
                let s = format_list_en(&strs, conj);
                self.alloc_str(s)
            }
            INTL_LIST_FORMAT_TO_PARTS => {
                let resolved = self.intl_this(this, INTL_LISTFORMAT, "formatToParts")?;
                let items = self.iterate_to_vec(a0)?;
                let mut strs: Vec<String> = Vec::with_capacity(items.len());
                for v in items {
                    strs.push(self.to_js_string(v)?);
                }
                let t = self.display(self.intl_slot(resolved, "type"));
                let conj = if t == "disjunction" { "or" } else { "and" };
                let s = format_list_en(&strs, conj);
                let mut part = ObjMap::new();
                let ty = self.alloc_str("literal".to_string());
                part.set("type", ty);
                let sv = self.alloc_str(s);
                part.set("value", sv);
                let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
            }
            INTL_RTF_FORMAT | INTL_RTF_FORMAT_TO_PARTS => {
                let _ = self.intl_this(this, INTL_RELATIVETIMEFORMAT, "format")?;
                let v = self.to_number(a0)?;
                let unit = self.to_js_string(a1)?;
                let s = format_relative_time_en(v, &unit);
                if id == INTL_RTF_FORMAT {
                    self.alloc_str(s)
                } else {
                    let mut part = ObjMap::new();
                    let ty = self.alloc_str("literal".to_string());
                    part.set("type", ty);
                    let sv = self.alloc_str(s);
                    part.set("value", sv);
                    let p = Value::heap(self.heap.alloc(HeapObj::Object(part)));
                    Value::heap(self.heap.alloc(HeapObj::Array(vec![p])))
                }
            }
            INTL_DISPLAYNAMES_OF => {
                let resolved = self.intl_this(this, INTL_DISPLAYNAMES, "of")?;
                let code = self.to_js_string(a0)?;
                let fb = self.display(self.intl_slot(resolved, "fallback"));
                if fb == "none" {
                    Value::UNDEFINED
                } else {
                    self.alloc_str(code)
                }
            }
            INTL_LOCALE_TOSTRING => {
                let resolved = self.intl_this(this, INTL_LOCALE, "toString")?;
                self.intl_slot(resolved, "baseName")
            }
            INTL_LOCALE_MAXIMIZE | INTL_LOCALE_MINIMIZE => {
                let resolved = self.intl_this(this, INTL_LOCALE, "maximize")?;
                let bn = self.intl_slot(resolved, "baseName");
                self.make_locale(bn, Value::UNDEFINED)?
            }
            INTL_SEGMENTER_SEGMENT => {
                let _ = self.intl_this(this, INTL_SEGMENTER, "segment")?;
                // Minimal Segments object (full grapheme/word segmentation TBD).
                let s = self.to_js_string(a0)?;
                let mut o = ObjMap::new();
                let sv = self.alloc_str(s);
                o.set("@@seginput", sv);
                Value::heap(self.heap.alloc(HeapObj::Object(o)))
            }
            INTL_DURATION_FORMAT => {
                let _ = self.intl_this(this, INTL_DURATIONFORMAT, "format")?;
                let dur = self.to_duration(a0)?;
                let s = format_duration_en(&dur);
                self.alloc_str(s)
            }
            _ if (INTL_LOCALE_GET_BASE..INTL_LOCALE_GET_BASE + LOCALE_ACCESSORS.len() as u16)
                .contains(&id) =>
            {
                let field = LOCALE_ACCESSORS[(id - INTL_LOCALE_GET_BASE) as usize];
                let resolved = self.intl_this(this, INTL_LOCALE, field)?;
                self.intl_slot(resolved, field)
            }
            // The format/compare bound-function getters: return (and cache) a
            // function bound to the instance, so `nf.format === nf.format`.
            INTL_NF_FORMAT_GET | INTL_DTF_FORMAT_GET | INTL_COLLATOR_COMPARE_GET => {
                let (kind, target_id, svc) = match id {
                    INTL_NF_FORMAT_GET => (INTL_NUMBERFORMAT, INTL_NF_FORMAT, "format"),
                    INTL_DTF_FORMAT_GET => (INTL_DATETIMEFORMAT, INTL_DTF_FORMAT, "format"),
                    _ => (INTL_COLLATOR, INTL_COLLATOR_COMPARE, "compare"),
                };
                let resolved = self.intl_this(this, kind, svc)?;
                let cached = self.intl_slot(resolved, "@@boundfn");
                if cached != Value::UNDEFINED {
                    cached
                } else {
                    let nat = Value::heap(self.heap.alloc(HeapObj::Native(target_id)));
                    let b = Value::heap(self.heap.alloc(HeapObj::Bound {
                        target: nat,
                        this,
                        args: vec![],
                    }));
                    if let HeapObj::Object(m) = self.heap.get_mut(resolved) {
                        m.set("@@boundfn", b);
                    }
                    b
                }
            }
            // `Array.prototype.<m>` / `String.prototype.<m>` invoked as a value
            // (`.call`/`.apply`/`.bind` or `m()`): dispatch on the `this` receiver.
            _ if native::proto_method(id).is_some() => {
                let (m, kind, _len) = native::proto_method(id).unwrap();
                // The raw receiver BEFORE any boxed-primitive unwrap — the
                // symbol-consulting String methods pass it (e.g. a `new String`
                // wrapper) to the argument's @@-method, not the unwrapped primitive.
                let raw_this = this;
                // A boxed primitive receiver unwraps to its [[PrimitiveValue]] so the
                // method runs on the primitive (`new Number(5).toFixed(2)`). The generic
                // Array methods (kind 0) are the exception: a `new Boolean/Number/String`
                // wrapper must stay intact so its own array-like props (length + indexed
                // elements) remain visible and the callback's receiver argument is the
                // original wrapper (`obj instanceof Boolean`, `toString.call(obj)`).
                let this = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Boxed { value, .. }) if kind != 0 => *value,
                    _ => this,
                };
                // TypedArray.prototype.toString IS Array.prototype.toString (one
                // function object), but a TypedArray receiver goes through
                // this.join = %TypedArray%.prototype.join, whose ValidateTypedArray
                // rejects a detached/out-of-bounds view — route it to the TA
                // implementation rather than the generic array-like materialize.
                if kind == 0
                    && m == "toString"
                    && matches!(
                        this.is_heap().then(|| self.heap.get(this.heap_index())),
                        Some(HeapObj::TypedArray { .. })
                    )
                {
                    return Ok(self
                        .typed_array_method(this.heap_index(), m, args)?
                        .unwrap_or(Value::UNDEFINED));
                }
                // Promise.prototype.then is brand-checked (IsPromise); catch and
                // finally are generic (they Invoke `this.then`, so an overridden /
                // non-callable / throwing `this.then`, a thenable receiver, and a
                // custom species constructor are observed), not direct internal-slot
                // operations.
                if kind == 7 {
                    if m == "catch" {
                        let on_r = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let then_fn = self.get_prop(this, "then")?;
                        return Ok(self.call_value(then_fn, this, &[Value::UNDEFINED, on_r])?);
                    }
                    if m == "finally" {
                        let on_finally = args.first().copied().unwrap_or(Value::UNDEFINED);
                        return Ok(self.promise_finally(this, on_finally)?);
                    }
                    if m == "then"
                        && !matches!(
                            this.is_heap().then(|| self.heap.get(this.heap_index())),
                            Some(HeapObj::Promise { .. })
                        )
                    {
                        return Err(Thrown(
                            "TypeError: Promise.prototype.then called on a non-Promise".into(),
                        ));
                    }
                }
                // Array.prototype.toString: func = Get(array, "join"), Call it
                // when callable, else the %Object.prototype.toString% intrinsic
                // (NOT a Get of "toString" — the test deletes it) — 23.1.3.36.
                if kind == 0 && m == "toString" {
                    self.require_object_coercible(this)?;
                    let recv = self.to_object(this)?;
                    let f = self.get_prop(recv, "join")?;
                    return Ok(if self.is_callable(f) {
                        self.call_value(f, recv, &[])?
                    } else {
                        let tag = self.object_to_string_tag(recv)?;
                        self.alloc_str(format!("[object {tag}]"))
                    });
                }
                // Number/Boolean receivers are primitive values; the rest are heap.
                if kind == 2 {
                    self.number_method(this, m, args)?.unwrap_or(Value::UNDEFINED)
                } else if kind == 5 {
                    self.boolean_method(this, m)?
                } else if kind == 1 && matches!(m, "toString" | "valueOf") {
                    // String.prototype.toString/valueOf are NOT generic: thisStringValue
                    // requires `this` to be a String (primitive or wrapper, already
                    // unwrapped above), else TypeError — unlike the coercing methods below.
                    if this.is_heap() && self.heap.is_str_like(this.heap_index()) {
                        this
                    } else if this.is_heap() && self.str_proto != 0 && this.heap_index() == self.str_proto {
                        // %String.prototype% is itself a String exotic whose
                        // [[StringData]] is the empty string.
                        self.alloc_str(String::new())
                    } else {
                        return Err(Thrown(format!(
                            "TypeError: String.prototype.{m} requires that 'this' be a String"
                        )));
                    }
                } else if kind == 1
                    && matches!(m, "replace" | "replaceAll" | "split" | "match" | "search" | "matchAll")
                {
                    // These methods must observe the ARGUMENT's well-known Symbol
                    // method (IsRegExp/flags + GetMethod(@@replace/@@split/@@match/…))
                    // with the RAW receiver BEFORE ToString(this) — a poison `this`
                    // (toString throws) must not be coerced first, and an @@-method
                    // receives the raw receiver. ToString happens only on the
                    // fall-through plain path (inside string_symbol_method).
                    self.string_symbol_method(raw_this, m, args)?
                } else if kind == 1 {
                    // String methods are generic: RequireObjectCoercible(this) then
                    // ToString(this), so `String.prototype.slice.call(123, …)` works.
                    let s_idx = if this.is_heap() && self.heap.is_str_like(this.heap_index()) {
                        this.heap_index()
                    } else if this == Value::UNDEFINED || this == Value::NULL {
                        return Err(Thrown(format!(
                            "TypeError: String.prototype.{m} called on null or undefined"
                        )));
                    } else {
                        let s = self.to_js_string(this)?;
                        self.alloc_str(s).heap_index()
                    };
                    self.string_method(s_idx, m, args)?.unwrap_or(Value::UNDEFINED)
                } else if !this.is_heap() {
                    // Array.prototype methods are generic: a primitive `this` is
                    // ToObject-coerced (a Number/Boolean wrapper has length 0, so
                    // the method runs trivially). null/undefined still throw, as do
                    // primitive receivers for the non-generic kinds.
                    if kind == 0 && this != Value::NULL && this != Value::UNDEFINED {
                        let boxed = self.to_object(this)?;
                        self.array_method(boxed.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
                    } else {
                        return Err(Thrown(format!(
                            "TypeError: prototype method {m} called on {}",
                            self.display(this)
                        )));
                    }
                } else {
                    let r = match kind {
                        // ToObject(this): a heap-allocated PRIMITIVE receiver
                        // (string/symbol/bigint) boxes to its wrapper — generic
                        // methods that return `this` (sort) must return the
                        // wrapper, never the bare primitive. Real objects pass
                        // through to_object unchanged.
                        0 => {
                            let recv = self.to_object(this)?;
                            self.array_method(recv.heap_index(), m, args)?
                        }
                        1 => self.string_method(this.heap_index(), m, args)?,
                        3 => self.set_method(this.heap_index(), m, args)?,
                        4 => self.map_method(this.heap_index(), m, args)?,
                        6 => self.date_method(this.heap_index(), m, args)?,
                        _ => self.promise_method(this.heap_index(), m, args)?, // kind 7
                    };
                    r.unwrap_or(Value::UNDEFINED)
                }
            }
            _ => Value::UNDEFINED,
        })
    }

}

/// The always-unescaped set for the Encode operation: uriAlpha + DecimalDigit +
impl<'p> Vm<'p> {
    /// URIError when `v` is a string VALUE containing a lone surrogate
    /// (Encode step 6.2.c) — the exact WTF-8 check behind the lossy `&str`
    /// view the URI codecs operate on.
    fn uri_check_wellformed(&self, v: Value) -> Result<(), Thrown> {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            let b = self.heap.str_wtf8_cow(v.heap_index()).unwrap();
            if !crate::heap::wtf8_is_wellformed(&b) {
                return Err(Thrown("URIError: URI malformed".into()));
            }
        }
        Ok(())
    }
}

/// uriMark. (encodeURI additionally keeps uriReserved + "#"; those extra chars
/// are passed in by the caller.)
const URI_UNESCAPED: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";

fn hex_upper(b: u8) -> [u8; 2] {
    const H: &[u8; 16] = b"0123456789ABCDEF";
    [H[(b >> 4) as usize], H[(b & 0xF) as usize]]
}

fn hex_val(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(()),
    }
}

/// Encode (ECMA-262 19.2.6.5): every char not in uriUnescaped ∪ `extra` is
/// replaced by the percent-encoded uppercase hex of its UTF-8 bytes. Rust `str`
/// holds only Unicode scalar values, so the lone-surrogate URIError branch is
/// unreachable here.
/// A byte → its two lowercase hex ASCII digits.
fn hex_lower(b: u8) -> [u8; 2] {
    const D: &[u8; 16] = b"0123456789abcdef";
    [D[(b >> 4) as usize], D[(b & 0xf) as usize]]
}

/// FromHex(string, maxLength) (Uint8Array base64/hex proposal): decode hex into
/// at most `maxLength` bytes. Returns (chars read, decoded bytes, optional
/// SyntaxError). An odd-length string decodes NOTHING; an illegal hexit pair
/// decodes the valid prefix then errors; reaching `maxLength` stops without error.
fn from_hex(s: &str, max_len: usize) -> (usize, Vec<u8>, Option<Thrown>) {
    let chars: Vec<char> = s.chars().collect();
    let length = chars.len();
    let err = || Some(Thrown("SyntaxError: invalid hexadecimal string".into()));
    if length % 2 != 0 {
        return (0, Vec::new(), err());
    }
    let hexv = |c: char| -> Option<u8> {
        if c.is_ascii() {
            hex_val(c as u8).ok()
        } else {
            None
        }
    };
    let mut bytes = Vec::new();
    let mut read = 0;
    while read < length && bytes.len() < max_len {
        match (hexv(chars[read]), hexv(chars[read + 1])) {
            (Some(h), Some(l)) => {
                bytes.push((h << 4) | l);
                read += 2;
            }
            _ => return (read, bytes, err()),
        }
    }
    (read, bytes, None)
}

/// ASCII whitespace skipped between base64 characters: tab, LF, FF, CR, space.
fn is_b64_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')
}

/// A base64 character → its 6-bit value, honouring the alphabet (`url` ⇒ `-_`).
fn b64_val(c: char, url: bool) -> Option<u8> {
    match c {
        'A'..='Z' => Some(c as u8 - b'A'),
        'a'..='z' => Some(c as u8 - b'a' + 26),
        '0'..='9' => Some(c as u8 - b'0' + 52),
        '+' if !url => Some(62),
        '/' if !url => Some(63),
        '-' if url => Some(62),
        '_' if url => Some(63),
        _ => None,
    }
}

/// Decode a base64 chunk of 2/3/4 sextets into 1/2/3 bytes. When
/// `throw_on_extra` (strict mode), the unused trailing bits of a 2- or
/// 3-sextet final chunk must be zero (else `None`).
fn decode_chunk(chunk: &[u8], throw_on_extra: bool) -> Option<Vec<u8>> {
    match chunk.len() {
        2 => {
            if throw_on_extra && (chunk[1] & 0x0f) != 0 {
                return None;
            }
            Some(vec![(chunk[0] << 2) | (chunk[1] >> 4)])
        }
        3 => {
            if throw_on_extra && (chunk[2] & 0x03) != 0 {
                return None;
            }
            Some(vec![
                (chunk[0] << 2) | (chunk[1] >> 4),
                ((chunk[1] & 0x0f) << 4) | (chunk[2] >> 2),
            ])
        }
        4 => Some(vec![
            (chunk[0] << 2) | (chunk[1] >> 4),
            ((chunk[1] & 0x0f) << 4) | (chunk[2] >> 2),
            ((chunk[2] & 0x03) << 6) | chunk[3],
        ]),
        _ => None,
    }
}

/// FromBase64(string, alphabet, lastChunkHandling, maxLength) (Uint8Array
/// base64/hex proposal). `lch`: 0=loose, 1=strict, 2=stop-before-partial.
/// Returns (chars read, decoded bytes (≤ maxLength), optional SyntaxError).
/// Chunks are atomic: a chunk whose full output would exceed `max_len` is not
/// decoded (the decode stops before it, without error).
fn from_base64(s: &str, url: bool, lch: u8, max_len: usize) -> (usize, Vec<u8>, Option<Thrown>) {
    let chars: Vec<char> = s.chars().collect();
    let length = chars.len();
    let synerr = || Some(Thrown("SyntaxError: invalid base64 string".into()));
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk: Vec<u8> = Vec::new();
    let mut read = 0usize; // committed position (after the last full chunk)
    let mut index = 0usize;
    if max_len == 0 {
        return (0, bytes, None); // no room — read nothing (trailing garbage ignored)
    }
    loop {
        while index < length && is_b64_ws(chars[index]) {
            index += 1;
        }
        if index == length {
            if chunk.is_empty() {
                return (length, bytes, None);
            }
            // Partial chunk at end (no padding).
            match lch {
                2 => return (read, bytes, None), // stop-before-partial
                1 => return (read, bytes, synerr()), // strict
                _ => {
                    // loose
                    if chunk.len() == 1 {
                        return (read, bytes, synerr());
                    }
                    let nb = chunk.len() - 1;
                    if bytes.len() + nb > max_len {
                        return (read, bytes, None);
                    }
                    match decode_chunk(&chunk, false) {
                        Some(d) => {
                            bytes.extend(d);
                            return (length, bytes, None);
                        }
                        None => return (read, bytes, synerr()),
                    }
                }
            }
        }
        let c = chars[index];
        if c == '=' {
            // Padding: the pending chunk must be 2 or 3 sextets.
            if chunk.len() < 2 {
                return (read, bytes, synerr());
            }
            let need_eq = 4 - chunk.len(); // 2 sextets→"==", 3 sextets→"="
            let mut got = 0;
            let mut j = index;
            while got < need_eq {
                while j < length && is_b64_ws(chars[j]) {
                    j += 1;
                }
                if j < length && chars[j] == '=' {
                    got += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            if got < need_eq {
                // Incomplete padding: stop-before-partial stops here; else error.
                if lch == 2 {
                    return (read, bytes, None);
                }
                return (read, bytes, synerr());
            }
            // Only whitespace may follow the padding.
            let mut k = j;
            while k < length && is_b64_ws(chars[k]) {
                k += 1;
            }
            if k != length {
                return (read, bytes, synerr());
            }
            let nb = chunk.len() - 1;
            if bytes.len() + nb > max_len {
                return (read, bytes, None);
            }
            match decode_chunk(&chunk, lch == 1) {
                Some(d) => {
                    bytes.extend(d);
                    return (length, bytes, None);
                }
                None => return (read, bytes, synerr()),
            }
        }
        match b64_val(c, url) {
            Some(v) => {
                chunk.push(v);
                index += 1;
                if chunk.len() == 4 {
                    if bytes.len() + 3 > max_len {
                        return (read, bytes, None);
                    }
                    if let Some(d) = decode_chunk(&chunk, false) {
                        bytes.extend(d);
                    }
                    chunk.clear();
                    read = index;
                    // Output full: stop here, ignoring any trailing characters.
                    if bytes.len() == max_len {
                        return (read, bytes, None);
                    }
                }
            }
            None => return (read, bytes, synerr()),
        }
    }
}

/// Encode bytes as base64 (Uint8Array.prototype.toBase64). `url` selects the
/// base64url alphabet (`-_` for `+/`); `omit_padding` drops trailing `=`.
fn to_base64(bytes: &[u8], url: bool, omit_padding: bool) -> String {
    const STD: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let t = if url { URL } else { STD };
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let (b0, b1, b2) = (bytes[i], bytes[i + 1], bytes[i + 2]);
        out.push(t[(b0 >> 2) as usize] as char);
        out.push(t[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        out.push(t[(((b1 & 0xf) << 2) | (b2 >> 6)) as usize] as char);
        out.push(t[(b2 & 0x3f) as usize] as char);
        i += 3;
    }
    match bytes.len() - i {
        1 => {
            let b0 = bytes[i];
            out.push(t[(b0 >> 2) as usize] as char);
            out.push(t[((b0 & 3) << 4) as usize] as char);
            if !omit_padding {
                out.push('=');
                out.push('=');
            }
        }
        2 => {
            let (b0, b1) = (bytes[i], bytes[i + 1]);
            out.push(t[(b0 >> 2) as usize] as char);
            out.push(t[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
            out.push(t[((b1 & 0xf) << 2) as usize] as char);
            if !omit_padding {
                out.push('=');
            }
        }
        _ => {}
    }
    out
}

/// `escape(string)` (Annex B B.2.1.1): keep `A-Za-z0-9@*_+-./`, encode any
/// other UTF-16 code unit as `%XX` (unit < 256) or `%uXXXX`. Iterates code
/// units (not chars) so an astral char yields its two surrogate `%uXXXX`s.
fn escape_str(s: &str) -> String {
    // A-Za-z0-9 and @ * _ + - . /
    const KEEP: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789@*_+-./";
    let mut out = String::new();
    for u in s.encode_utf16() {
        if u < 0x80 && KEEP.contains(&(u as u8)) {
            out.push(u as u8 as char);
        } else if u < 0x100 {
            let h = hex_upper(u as u8);
            out.push('%');
            out.push(h[0] as char);
            out.push(h[1] as char);
        } else {
            let hi = hex_upper((u >> 8) as u8);
            let lo = hex_upper((u & 0xff) as u8);
            out.push('%');
            out.push('u');
            out.push(hi[0] as char);
            out.push(hi[1] as char);
            out.push(lo[0] as char);
            out.push(lo[1] as char);
        }
    }
    out
}

/// `unescape(string)` (Annex B B.2.1.2): decode `%uXXXX` (one UTF-16 unit) and
/// `%XX` (a unit < 256); any other character passes through unchanged. A `%`
/// not followed by valid hex is literal.
fn unescape_str(s: &str) -> String {
    let u: Vec<u16> = s.encode_utf16().collect();
    let n = u.len();
    let hexv = |c: u16| -> Option<u16> {
        match c {
            0x30..=0x39 => Some(c - 0x30),
            0x41..=0x46 => Some(c - 0x41 + 10),
            0x61..=0x66 => Some(c - 0x61 + 10),
            _ => None,
        }
    };
    let mut out: Vec<u16> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = u[i];
        if c == b'%' as u16 {
            if i + 5 < n && u[i + 1] == b'u' as u16 {
                if let (Some(a), Some(b), Some(d), Some(e)) =
                    (hexv(u[i + 2]), hexv(u[i + 3]), hexv(u[i + 4]), hexv(u[i + 5]))
                {
                    out.push((a << 12) | (b << 8) | (d << 4) | e);
                    i += 6;
                    continue;
                }
            }
            if i + 2 < n {
                if let (Some(a), Some(b)) = (hexv(u[i + 1]), hexv(u[i + 2])) {
                    out.push((a << 4) | b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    String::from_utf16_lossy(&out)
}

fn uri_encode(s: &str, extra: &[u8]) -> Result<String, ()> {
    let mut out = String::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in s.chars() {
        if c.is_ascii() && (URI_UNESCAPED.contains(&(c as u8)) || extra.contains(&(c as u8))) {
            out.push(c);
        } else {
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                let h = hex_upper(b);
                out.push('%');
                out.push(h[0] as char);
                out.push(h[1] as char);
            }
        }
    }
    Ok(out)
}

/// Decode (ECMA-262 19.2.6.4): each `%XX` escape is decoded; a decoded
/// single-byte char that lies in the `reserved` set is left as its original
/// `%XX` text (decodeURI keeps uriReserved + "#"; decodeURIComponent keeps
/// nothing). Multi-byte sequences are validated as UTF-8. Malformed → Err.
fn uri_decode(bytes: &[u8], reserved: &[u8]) -> Result<crate::heap::JsStr, ()> {
    use crate::heap::{wtf8_push, wtf8_push_cp, JsStr};
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let read = |at: usize| -> Result<u8, ()> {
        if at + 2 >= bytes.len() || bytes[at] != b'%' {
            return Err(());
        }
        Ok((hex_val(bytes[at + 1])? << 4) | hex_val(bytes[at + 2])?)
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let b = bytes[i];
            if b < 0x80 {
                out.push(b);
                i += 1;
            } else {
                // Copy the whole encoded char through the seam-canonicalizing
                // append (a raw low surrogate after a %-decoded high merges).
                let n = if b >> 5 == 0b110 {
                    2
                } else if b >> 4 == 0b1110 {
                    3
                } else {
                    4
                };
                let end = (i + n).min(bytes.len());
                wtf8_push(&mut out, &bytes[i..end]);
                i = end;
            }
            continue;
        }
        let b0 = read(i)?;
        if b0 & 0x80 == 0 {
            if reserved.contains(&b0) {
                out.extend_from_slice(&bytes[i..i + 3]); // keep original %XX
            } else {
                out.push(b0);
            }
            i += 3;
        } else {
            // UTF-8 lead byte: 2/3/4 total octets. The spec Decode algorithm
            // accepts SURROGATE-range 3-byte sequences (each yields its code
            // UNIT — '%ED%A0%80' decodes to the lone surrogate \uD800, and a
            // CESU-8 high+low pair canonicalizes into the astral scalar), so
            // decode manually instead of from_utf8; overlong forms and
            // beyond-U+10FFFF still reject (URIError).
            let (n, mut cp) = if b0 >> 5 == 0b110 {
                (2, (b0 & 0x1F) as u32)
            } else if b0 >> 4 == 0b1110 {
                (3, (b0 & 0x0F) as u32)
            } else if b0 >> 3 == 0b11110 {
                (4, (b0 & 0x07) as u32)
            } else {
                return Err(());
            };
            i += 3;
            for _ in 1..n {
                let cb = read(i)?;
                if cb & 0xC0 != 0x80 {
                    return Err(());
                }
                cp = (cp << 6) | (cb & 0x3F) as u32;
                i += 3;
            }
            let overlong =
                (n == 2 && cp < 0x80) || (n == 3 && cp < 0x800) || (n == 4 && cp < 0x10000);
            // %-escaped sequences are STRICT UTF-8: a surrogate-range escape
            // ('%ED%BF%BF') is a URIError — only RAW lone-surrogate code
            // units (the non-'%' copy path above) pass through verbatim.
            if overlong || cp > 0x10FFFF || (0xD800..=0xDFFF).contains(&cp) {
                return Err(());
            }
            wtf8_push_cp(&mut out, cp);
        }
    }
    Ok(JsStr::from_wtf8(out))
}
