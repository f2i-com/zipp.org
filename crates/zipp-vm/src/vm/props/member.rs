// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// The RegExp flag character a per-flag accessor name reports, if the name is
/// one of the eight. Lets a `re.global`-style read be answered from the flag
/// string in place, with no allocation and no prototype walk.
fn regexp_flag_char(key: &str) -> Option<char> {
    Some(match key {
        "hasIndices" => 'd',
        "global" => 'g',
        "ignoreCase" => 'i',
        "multiline" => 'm',
        "dotAll" => 's',
        "unicode" => 'u',
        "unicodeSets" => 'v',
        "sticky" => 'y',
        _ => return None,
    })
}

/// The %RegExp.prototype% getter a RegExp instance's `key` resolves to. These
/// ten names are ACCESSORS on the prototype — an instance owns none of them —
/// so `Get(re, key)` is an ordinary prototype-chain lookup, not an internal-slot
/// read.
fn regexp_intrinsic_accessor(key: &str) -> Option<u16> {
    Some(match key {
        "source" => native::REGEXP_GET_SOURCE,
        "flags" => native::REGEXP_GET_FLAGS,
        "global" => native::REGEXP_GET_GLOBAL,
        "ignoreCase" => native::REGEXP_GET_IGNORECASE,
        "multiline" => native::REGEXP_GET_MULTILINE,
        "dotAll" => native::REGEXP_GET_DOTALL,
        "unicode" => native::REGEXP_GET_UNICODE,
        "unicodeSets" => native::REGEXP_GET_UNICODESETS,
        "sticky" => native::REGEXP_GET_STICKY,
        "hasIndices" => native::REGEXP_GET_HASINDICES,
        _ => return None,
    })
}

/// Where `Get(re, key)` lands for one of those ten names when the instance has
/// no own override.
enum RegExpAccessor {
    /// The chain still holds the intrinsic getter — answer from the slots.
    Intrinsic,
    /// Something shadows it (a subclass getter, a redefined prototype slot).
    Override(Value, PropAttr),
    /// No such property anywhere on the chain — Get is `undefined`.
    Absent,
}

impl<'p> Vm<'p> {
    /// Walk a RegExp instance's PROTOTYPE CHAIN for one of %RegExp.prototype%'s
    /// accessor names, reporting whether the intrinsic getter is still what a
    /// spec `Get` would reach. The common instance sits directly on
    /// %RegExp.prototype% with the slot untouched, so this is one map lookup.
    fn regexp_accessor_source(
        &self,
        obj_idx: u32,
        key: &str,
        getter_id: u16,
    ) -> RegExpAccessor {
        let mut p = self.proto_of.get(&obj_idx).copied().unwrap_or(
            if self.regexp_proto != 0 {
                Value::heap(self.regexp_proto)
            } else {
                Value::NULL
            },
        );
        for _ in 0..32 {
            if !p.is_heap() {
                break;
            }
            let pi = p.heap_index();
            let entry = match self.heap.get(pi) {
                HeapObj::Object(m) => m.pos(key).map(|i| (m.vals[i], m.attrs[i])),
                _ => None,
            };
            if let Some((raw, attr)) = entry {
                let intrinsic = raw.is_heap()
                    && matches!(self.heap.get(raw.heap_index()),
                        HeapObj::Native(id) if *id == getter_id);
                return if intrinsic {
                    RegExpAccessor::Intrinsic
                } else {
                    RegExpAccessor::Override(raw, attr)
                };
            }
            p = self.proto_of.get(&pi).copied().unwrap_or(Value::NULL);
        }
        RegExpAccessor::Absent
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
            // `own_member` only sees ordinary storage, so a PROXY spliced into the
            // chain (`Object.setPrototypeOf(Number.prototype, p); (5).x`) would be
            // walked straight past and its `get` trap never run. Hand the rest of
            // the chain to the ordinary member path, which runs the trap with the
            // original receiver.
            if matches!(self.heap.get(cur), HeapObj::Proxy { .. }) {
                return self.get_member(Value::heap(cur), key, receiver);
            }
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

    /// AddRestrictedFunctionProperties poison: whether `caller`/`arguments` access
    /// on this function hits the inherited %ThrowTypeError% accessors. Everything
    /// except a LEGACY sloppy ordinary function (the only kind engines give own
    /// caller/arguments) is restricted: strict, generator, async, arrow,
    /// concise-method, and bound functions.
    pub(crate) fn fn_restricted_caller(&self, idx: u32) -> bool {
        let fid = match self.heap.get(idx) {
            HeapObj::Bound { .. } => return true,
            HeapObj::Func(fid) => *fid,
            HeapObj::Closure { func, .. } => *func,
            // Built-ins (`Function.prototype.bind.caller`), realm-WrappedFunctions
            // and the internal resolver closures are not legacy functions either:
            // 16.2 Forbidden Extensions gives own caller/arguments ONLY to a
            // sloppy ordinary function declaration/expression.
            _ => return true,
        };
        let f = self.func(fid as usize);
        f.is_strict || f.is_generator || f.is_async || f.lexical_this || f.non_constructable
    }

    /// Whether `key` is a LEGACY own `caller`/`arguments` of this callable — the
    /// pair a sloppy ORDINARY function keeps as non-writable, non-enumerable,
    /// non-configurable own data properties. Everything `fn_restricted_caller`
    /// rejects (strict/generator/async/arrow/method/bound), and everything that
    /// is not an ordinary function object at all (native, class, wrapped), has
    /// none — for those the pair is the inherited %ThrowTypeError% accessor.
    pub(crate) fn fn_has_legacy_caller_prop(&self, idx: u32, key: &str) -> bool {
        (key == "caller" || key == "arguments")
            && matches!(self.heap.get(idx), HeapObj::Func(_) | HeapObj::Closure { .. })
            && !self.fn_restricted_caller(idx)
    }

    /// The func-proto id a callable heap object runs, if it is an ordinary
    /// function (not a native/bound/class).
    fn callable_func_id(&self, idx: u32) -> Option<u32> {
        match self.heap.get(idx) {
            HeapObj::Func(f) => Some(*f),
            HeapObj::Closure { func, .. } => Some(*func),
            _ => None,
        }
    }

    /// The function VALUE a frame is executing, or UNDEFINED when the frame
    /// cannot name one (the top-level script; a JIT bail-out window, which
    /// records only the func id).
    fn frame_callee(&self, i: usize) -> Value {
        let fr = &self.frames[i];
        if fr.callee.is_heap() {
            fr.callee
        } else if fr.closure != NO_CLOSURE {
            Value::heap(fr.closure)
        } else {
            Value::UNDEFINED
        }
    }

    /// Index of the TOPMOST live activation of the callable at `fidx`, if any.
    /// Identity first (`Frame::callee` is the very object the caller invoked, so
    /// two clones of the same source function stay distinguishable — the whole
    /// point of regress-577648-1.js); the func-proto id is only a fallback for
    /// frames that carry no callee value.
    fn topmost_activation(&self, fidx: u32) -> Option<usize> {
        let target = Value::heap(fidx);
        let fid = self.callable_func_id(fidx);
        (0..self.frames.len()).rev().find(|&i| {
            let fr = &self.frames[i];
            if fr.callee.is_heap() || fr.closure != NO_CLOSURE {
                self.frame_callee(i) == target
            } else {
                fid.is_some_and(|f| f == fr.func)
            }
        })
    }

    /// Legacy `f.caller` (Annex B "forbidden extensions" carves this out for a
    /// sloppy ordinary function): the function that invoked `f`'s topmost live
    /// activation, or `null`. NEVER undefined — no live activation, a strict
    /// caller, and a caller the engine cannot name (top-level script code) all
    /// report null, so the value is never mistaken for an ordinary miss.
    pub(crate) fn legacy_fn_caller(&mut self, fidx: u32) -> Value {
        let Some(i) = self.topmost_activation(fidx) else {
            return Value::NULL;
        };
        let mut j = i;
        while j > 0 {
            j -= 1;
            if self.frames[j].is_eval {
                continue; // an eval is transparent to the caller chain
            }
            let callee = self.frame_callee(j);
            if !callee.is_heap() {
                return Value::NULL; // top-level script: no caller function
            }
            // A RESTRICTED caller is censored — the legacy accessor must never
            // leak a strict, generator, async, arrow, concise-method, bound, or
            // native activation (censor-strict-caller.js,
            // function-caller-restrictions.js).
            let censored = match self.callable_func_id(callee.heap_index()) {
                Some(f) => {
                    let fd = self.func(f as usize);
                    fd.is_strict || fd.is_generator || fd.is_async || fd.lexical_this || fd.non_constructable
                }
                None => true, // bound/native/class: not a legacy caller
            };
            return if censored { Value::NULL } else { callee };
        }
        Value::NULL
    }

    /// Legacy `f.arguments`: the arguments of `f`'s topmost live activation, or
    /// `null` when it has none. Materialized ON DEMAND — a function that never
    /// mentions `arguments` gets no object at call time, so the frame's recorded
    /// argument window is replayed into a fresh unmapped one.
    pub(crate) fn legacy_fn_arguments(&mut self, fidx: u32) -> Value {
        let Some(i) = self.topmost_activation(fidx) else {
            return Value::NULL;
        };
        let (arg_win, argc, base, func) = {
            let fr = &self.frames[i];
            (fr.arg_win as usize, fr.argc as usize, fr.base, fr.func)
        };
        // The body already built one (it references `arguments`): hand back that
        // object, so the two views of the same activation agree.
        if let Some(areg) = self.func(func as usize).arguments_reg {
            return self.regs[base + areg as usize];
        }
        let pcount = self.func(func as usize).param_count as usize;
        let args: Vec<Value> = if arg_win != u32::MAX as usize {
            // A formal's CURRENT register wins over the staged copy for the
            // indices a mapped arguments object would alias, so a parameter
            // reassigned since entry is what `f.arguments[i]` reports.
            (0..argc)
                .map(|k| if k < pcount { self.regs[base + 1 + k] } else { self.regs[arg_win + k] })
                .collect()
        } else {
            // Entered from native code (`call_value`), where the arguments were
            // never staged in a register window — the bound formals are all that
            // survives.
            let pc = self.func(func as usize).param_count as usize;
            (0..pc).map(|k| self.regs[base + 1 + k]).collect()
        };
        self.build_arguments_object(args, Value::heap(fidx), false, None)
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
            // A WrappedFunction reports the CopyNameAndLength snapshot taken
            // when it crossed the realm boundary (never the live target's).
            HeapObj::Wrapped { name, length, .. } => Some((name.clone(), *length)),
            // The resolve/reject functions of `new Promise(executor)`, and the
            // Promise.all/allSettled/any resolve/reject ELEMENT functions: anonymous
            // (name ""), length 1, with %Function.prototype% as [[Prototype]].
            HeapObj::BoundResolver { .. } | HeapObj::CombinatorResolver { .. } => {
                Some((String::new(), 1.0))
            }
            // CreateBuiltinFunction(closure, length, name, …) — a state-carrying
            // builtin carries both inline, since it has no static-table entry.
            HeapObj::NativeClosure { name, length, .. } => {
                Some(((*name).to_string(), *length as f64))
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
                        // Anonymous internal closures (spec name ""): the
                        // Proxy.revocable revoker and the finally wrappers
                        // (ThenFinally/CatchFinally have length 1).
                        native::PROXY_REVOKE => return Some((String::new(), 0.0)),
                        native::FINALLY_THEN | native::FINALLY_CATCH => {
                            return Some((String::new(), 1.0));
                        }
                        native::FINALLY_VALUE_THUNK | native::FINALLY_THROWER => {
                            return Some((String::new(), 0.0));
                        }
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
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. }
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
        self.fn_props.entry(fi).or_insert_with(ObjMap::new_side_table).define(
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
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => {
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

    pub(crate) fn func_has_prototype(&self, id: u32) -> bool {
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
    pub(crate) fn exotic_own_or_proto(&mut self, obj: Value, proto: u32, key: &str) -> Result<Value, Thrown> {
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
        // FAST PATH: walk plain-Object own maps + the `proto_of` chain directly.
        // A read on a plain (non-constructor, non-global, non-namespace) Object —
        // the overwhelmingly common case — pays NONE of the exotic machinery in
        // the slow path (deferred-namespace trigger, realm globals, boxed-string
        // indices, callable name/length/prototype intrinsics, the RegExp/
        // TypedArray/Temporal/Intl discriminant probe chain: all vacuous for
        // it). Every condition that could make one of those branches fire for
        // an Object receiver BAILS to the slow path, and the walk performs no
        // side effects, so a mid-chain bail delegates the CURRENT chain object
        // to the slow path exactly like the Object arm's own recursion would.
        let mut cur = obj;
        if self.deferred_ns_state.is_empty() {
            while cur.is_heap() {
                let ci = cur.heap_index();
                // The global object and realm/namespace exotics have LIVE slot
                // semantics layered over their ObjMap, and %Array.prototype% is
                // an Array exotic Object with a synthesized virtual `length` —
                // slow path for all of them.
                if (ci == self.global_this && self.global_this != 0)
                    || (ci == self.arr_proto && self.arr_proto != 0)
                    || (!self.module_namespaces.is_empty()
                        && self.module_namespaces.contains_key(&ci))
                    || (!self.realm_global_objs.is_empty()
                        && self.realm_global_objs.contains_key(&ci))
                {
                    break;
                }
                // Ok(hit) | Err(class of the instance on an own miss).
                let step = {
                    let m = match self.heap.get(ci) {
                        // A builtin-constructor Object synthesizes `prototype`
                        // ahead of its own map — slow path.
                        HeapObj::Object(m) if !m.is_ctor => m,
                        _ => break,
                    };
                    match m.pos(key) {
                        Some(i) => Ok((m.attrs[i], m.vals[i])),
                        None => Err(m.class),
                    }
                };
                match step {
                    Ok((a, raw)) => {
                        if a.accessor {
                            // `raw` is the getter (UNDEFINED ⇒ no getter ⇒ undefined).
                            return if raw == Value::UNDEFINED {
                                Ok(Value::UNDEFINED)
                            } else {
                                self.call_value(raw, receiver, &[])
                            };
                        }
                        return Ok(raw);
                    }
                    Err(Some(class)) => {
                        // Class-INSTANCE own miss: resolve an inherited method/
                        // getter on the class chain inline — identical to the
                        // Object arm's resolution in the slow path, which would
                        // otherwise charge the whole exotic preamble to every
                        // hot `obj.method()` / class-getter read. A chain miss
                        // (or a non-Class link) bails to the slow path.
                        let (mut method, mut getter) = (None, None);
                        let mut c2 = Some(class);
                        while let Some(cidx) = c2 {
                            match self.heap.get(cidx) {
                                HeapObj::Class(c) => {
                                    if let Some((_, v)) =
                                        c.methods.iter().find(|(k, _)| k == key)
                                    {
                                        method = Some(*v);
                                        break;
                                    }
                                    if let Some((_, v)) =
                                        c.getters.iter().find(|(k, _)| k == key)
                                    {
                                        getter = Some(*v);
                                        break;
                                    }
                                    c2 = c.parent;
                                }
                                _ => break,
                            }
                        }
                        if let Some(mv) = method {
                            return Ok(mv);
                        }
                        if let Some(g) = getter {
                            return self.call_value(g, receiver, &[]);
                        }
                        break;
                    }
                    Err(None) => {}
                }
                // [[Prototype]] step — the Object arm's resolution for a plain
                // object: an explicit proto_of entry, else %Object.prototype%.
                match self.proto_of.get(&ci) {
                    Some(&p) => {
                        if !p.is_heap() {
                            return Ok(Value::UNDEFINED); // null-prototype chain end
                        }
                        cur = p;
                    }
                    None => {
                        if self.obj_proto == 0 || ci == self.obj_proto {
                            return Ok(Value::UNDEFINED);
                        }
                        cur = Value::heap(self.obj_proto);
                    }
                }
            }
        }
        self.get_member_slow(cur, key, receiver)
    }

    /// The full (exotic-aware) property GET — see [`Vm::get_member`], whose
    /// fast path handles the plain-Object chain and delegates everything else
    /// (and every chain object it bails on) here.
    pub(crate) fn get_member_slow(&mut self, obj: Value, key: &str, receiver: Value) -> Result<Value, Thrown> {
        self.defer_check(obj, key)?; // a deferred-namespace Get may evaluate
        // Inside a ShadowRealm's evaluate, `globalThis.x` reads the REALM's
        // own binding for x when one exists (bare `x` and `globalThis.x`
        // alias the same realm slot).
        if let Some(rid) = self.active_realm {
            if obj.is_heap()
                && self.global_this != 0
                && obj.heap_index() == self.global_this
            {
                if let Some(&s) = self.realm_globals.get(&rid).and_then(|m| m.get(key)) {
                    let v = self.globals.get(s as usize).copied().unwrap_or(Value::UNDEFINED);
                    return Ok(if v.is_uninitialized() { Value::UNDEFINED } else { v });
                }
            }
        }
        // A createRealm child's GLOBAL object (read from ANY realm): a name the
        // child's own binding table holds (a `var` from its eval/Function code,
        // or a value previously written through this object) reads the LIVE
        // binding — `other.x` and a bare `x` inside the child alias one slot.
        // An UNINITIALIZED slot (a reference that never wrote) falls through to
        // the ordinary own-property path.
        if !self.realm_global_objs.is_empty()
            && obj.is_heap()
            && self.realm_global_objs.contains_key(&obj.heap_index())
        {
            if let Some(&s) = self.realm_globals.get(&obj.heap_index()).and_then(|m| m.get(key)) {
                let v = self.globals.get(s as usize).copied().unwrap_or(Value::UNDEFINED);
                if !v.is_uninitialized() {
                    return Ok(v);
                }
            }
        }
        // Proxy `get` trap (or fall through to the target).
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'get' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "get")? {
                    Some(trap) => {
                        let kv = self.key_to_value(key);
                        // 10.5.8 [[Get]](P, Receiver): the trap's third argument is
                        // the RECEIVER, which is only the proxy itself for a direct
                        // read — reading through a child (`Object.create(p).x`) or
                        // `Reflect.get(p, k, other)` must hand the trap that object.
                        let r = self.call_value(trap, handler, &[target, kv, receiver])?;
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
                    let v = self
                        .globals
                        .get(slot as usize)
                        .copied()
                        .unwrap_or(Value::UNDEFINED);
                    // A TDZ (uninitialized let/const/class) binding read
                    // through the namespace throws ReferenceError.
                    if v.is_uninitialized() {
                        return Err(Thrown(format!(
                            "ReferenceError: Cannot access '{key}' before initialization"
                        )));
                    }
                    return Ok(v);
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
            // Number/Boolean.prototype (`(5).toFixed`, `true.valueOf`) — through
            // the accessor-AWARE walk, so a defineProperty'd getter on the
            // prototype is invoked with the primitive as receiver.
            if obj.is_number() {
                // Inside a createRealm child, primitive member access resolves
                // through the CHILD's prototype image (active_realm_proto is the
                // identity in the main realm).
                let p = self.active_realm_proto(self.num_proto);
                return self.proto_member_get(p, key, obj);
            }
            if obj.is_bool() {
                let p = self.active_realm_proto(self.bool_proto);
                return self.proto_member_get(p, key, obj);
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
        // Only callables that OWN a `prototype` (class / ordinary function /
        // generator) synthesize one — an arrow / async fn / concise method has no
        // `prototype` property at all and falls through to the proto-chain walk
        // (undefined), matching the own-property reporting.
        if key == "prototype" {
            if let Some(&v) = self.fn_proto_override.get(&obj.heap_index()) {
                return Ok(v);
            }
            if self.callable_has_prototype(obj)
                || matches!(self.heap.get(obj.heap_index()), HeapObj::Object(m) if m.is_ctor)
            {
                if let Some(p) = self.prototype_of(obj) {
                    return Ok(p);
                }
            }
        }
        // A RegExp's accessor-like own properties (source/flags/lastIndex + the
        // flag booleans) and its match-result Array's `.index`/`.input`/`.groups`.
        // Cloned out of the heap borrow before any allocation.
        if matches!(self.heap.get(obj.heap_index()), HeapObj::RegExp { .. }) {
            // A custom own property (`re.exec = fn`, `re.x = …`, or an
            // Object.defineProperty'd `flags`/`source`/flag-boolean) in the side
            // table shadows the prototype AND the synthesized intrinsic accessor —
            // an own property is more specific than the `%RegExp.prototype%` getter.
            // `lastIndex` is the exception: it is a struct-backed own data property
            // (the single source of truth shared with `exec`), so it always resolves
            // directly from the RegExp record, never a side-table entry.
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
            // `source`, `flags` and the eight flag booleans are ACCESSORS on
            // %RegExp.prototype%, so the spec Get resolves them through the
            // prototype chain. Answering them from the internal slots outright
            // made `class R extends RegExp { get global() { return true; } }`
            // report the pattern's own flags, and left `re.flags` as `""` after
            // `delete RegExp.prototype.flags` (it must be `undefined` — that is
            // what turns `"a".split(/a/)` into the spec's SyntaxError).
            if let Some(getter_id) = regexp_intrinsic_accessor(key) {
                match self.regexp_accessor_source(obj.heap_index(), key, getter_id) {
                    RegExpAccessor::Absent => return Ok(Value::UNDEFINED),
                    RegExpAccessor::Override(raw, attr) => {
                        return if attr.accessor {
                            if raw == Value::UNDEFINED {
                                Ok(Value::UNDEFINED)
                            } else {
                                self.call_value(raw, receiver, &[])
                            }
                        } else {
                            Ok(raw)
                        };
                    }
                    // Intrinsic: fall through to the slot-backed answers below.
                    RegExpAccessor::Intrinsic => {}
                }
            }
            // `get RegExp.prototype.flags` (intrinsic): build the string by
            // reading each per-flag accessor off the RECEIVER in canonical order — so
            // a throwing `global`/`unicode`/… getter or a per-flag own override is
            // observed (e.g. by `@@match`/`@@replace`, which read Get(rx,"flags")),
            // rather than synthesizing from the internal flag string.
            if key == "flags" {
                // ── pristine shortcut ──
                // The eight reads below are the whole cost of this property, and it
                // is not a small one: `re.flags` measured **200ns against node's
                // 10ns**, because each is a full RegExp-exotic `get_member_slow`
                // traversal. It is also on a hot path nobody looks at —
                // `String.prototype.matchAll` reads `flags` purely to test for `g`,
                // which made those eight reads ~175ns of a 493ns `matchAll()` call
                // (node: 43ns).
                //
                // When every per-flag accessor is still the intrinsic and the
                // receiver has no own shadow, all eight reads are unobservable and
                // their answers are exactly the internal flag string — which is
                // already stored in canonical order. So return it directly.
                //
                // `regexp_flag_accessors_pristine` proves that with eight `pos()`
                // probes against %RegExp.prototype% instead of eight property
                // traversals: same guarantee, and the guard is what keeps a
                // `Object.defineProperty(RegExp.prototype, "global", {get(){...}})`
                // or a per-instance `re.global = false` observable, which
                // `@@match`/`@@replace` depend on.
                if let Some(f) = self.regexp_pristine_flags(obj.heap_index(), receiver) {
                    return Ok(self.alloc_str(f));
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
                    let v = self.get_prop(receiver, prop)?;
                    if self.truthy(v) {
                        out.push(ch);
                    }
                }
                return Ok(self.alloc_str(out));
            }
            // Answer everything that needs no OWNED text while the heap borrow is
            // still open: `lastIndex`, the eight flag booleans, and -- the case
            // that actually matters -- every other key, which is a prototype walk
            // (`re.test`, `re.exec`, `re.constructor`). Cloning `source` and
            // `flags` up front made every property read on a RegExp cost two heap
            // allocations sized by the PATTERN TEXT: measured 31ns for a 1-char
            // pattern against 120ns for a 20,000-char one, on a read that returns
            // an integer. `re.flags` read nine such properties, so it cost 227ns
            // against node's 3ns.
            if let HeapObj::RegExp { flags, last_index, .. } = self.heap.get(obj.heap_index()) {
                if key == "lastIndex" {
                    return Ok(*last_index);
                }
                if let Some(c) = regexp_flag_char(key) {
                    return Ok(Value::bool(flags.contains(c)));
                }
            }
            let eff = self
                .proto_of
                .get(&obj.heap_index())
                .and_then(|p| p.is_heap().then(|| p.heap_index()))
                .unwrap_or(self.regexp_proto);
            // `source` is the one intrinsic left that has to own its text.
            // (`flags` never reaches here -- the spec-mandated per-flag-getter
            // synthesis above always returns.)
            if key == "source" {
                // The clone is an Arc refcount bump; the `&str` deref below is what
                // `regexp_source_value` wants, so nothing is materialised here.
                // An Arc bump, then a `&str` deref — `regexp_source_value` wants the
                // latter, so no text is materialised here.
                let src: Option<std::sync::Arc<str>> = match self.heap.get(obj.heap_index()) {
                    HeapObj::RegExp { source, .. } => Some(source.clone()),
                    _ => None,
                };
                return Ok(self.regexp_source_value(obj.heap_index(), src.as_deref().unwrap_or("")));
            }
            // Accessor-aware: an `exec`/`test`/… turned into a getter on
            // %RegExp.prototype% (staging/sm/String/matchAll.js) is INVOKED with
            // the RegExp as receiver — `proto_member` would hand back the raw
            // getter function, and a method call would then invoke the GETTER
            // with the call's arguments instead of its result.
            return self.proto_member_get(eff, key, receiver);
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
            // A LIVE-mapped arguments index whose attrs were tweaked (e.g.
            // configurable:false) still reads the formal's register — the
            // arr_props value is only the escape store.
            if let Some(i) = canonical_index_str(key) {
                if let Some(v) = self.args_mapped_get(obj.heap_index(), i) {
                    return Ok(v);
                }
            }
            return Ok(raw);
        }
        // A pristine RegExp match result keeps its standard named data
        // properties in a compact fixed record. They have the ordinary default
        // attributes, so a direct read needs no descriptor materialisation.
        if let Some(raw) = self.regexp_result_prop(obj.heap_index(), key) {
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
            // …but a FOREIGN receiver never gets them. 10.4.5 integer-indexed
            // exotics own no `length`/`byteLength`/`byteOffset`/`buffer`: those
            // are %TypedArray%.prototype accessors whose first step is
            // ValidateTypedArray/RequireInternalSlot on the `this` value.
            // Arriving here with `receiver != obj` means the lookup walked INTO
            // this TypedArray from an ordinary object — `Object.create(ta).buffer`,
            // `F.prototype = ta; new F().slice()`, `Reflect.get(ta, k, other)` —
            // so the real getter would run on that receiver and throw
            // (staging/sm/regress/regress-571014.js). BYTES_PER_ELEMENT is a
            // genuine inherited DATA property and @@toStringTag returns undefined
            // rather than throwing, so both stay reachable.
            if receiver != obj && matches!(key, "length" | "byteLength" | "byteOffset" | "buffer") {
                let msg = format!(
                    "TypeError: get %TypedArray%.prototype.{key} called on a value that is not a TypedArray"
                );
                return Err(self.realm_thrown_from_proto(obj.heap_index(), msg));
            }
            // KNOWN DEVIATION: these six names are answered from the instance
            // without consulting the prototype chain, so
            // `Object.setPrototypeOf(ta, {length: 7}); ta.length` reports the
            // TypedArray's length instead of 7 (V8 reports 7).
            //
            // Gating them on a chain lookup (`ta_named_is_intrinsic`, kept in
            // vm/typedarray.rs) is the spec-correct shape, but it cannot be
            // enabled yet: `$262.createRealm()` does not build the
            // %TypedArray%.prototype level for the new realm — a cross-realm
            // TypedArray's prototype chain is `OtherUint8Array.prototype ->
            // Object.prototype`, carrying none of these accessors — so a
            // faithful lookup returns `undefined` and breaks 24 cross-realm
            // tests that currently pass only because this path ignores the
            // chain. Fix the realm setup first, then flip this back.
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
            // Same rule as the TypedArray arm above: an ArrayBuffer owns none of
            // these — they are %ArrayBuffer.prototype% accessors that begin with
            // RequireInternalSlot(this, [[ArrayBufferData]]). A lookup that
            // reached this buffer through some OTHER object's prototype chain
            // (`F.prototype = new ArrayBuffer(1); new F().byteLength`) must throw
            // the getter's TypeError (staging/sm/regress/regress-571014.js).
            if receiver != obj
                && matches!(
                    key,
                    "byteLength"
                        | "maxByteLength"
                        | "resizable"
                        | "growable"
                        | "detached"
                        | "immutable"
                )
            {
                return Err(Thrown(format!(
                    "TypeError: get ArrayBuffer.prototype.{key} called on a value that is not an ArrayBuffer"
                )));
            }
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
            // %DataView.prototype%'s three accessors all start with
            // RequireInternalSlot(this, [[DataView]]); a foreign receiver that
            // merely inherits from this view must get that TypeError, not the
            // view's own numbers (same rule as the TypedArray/ArrayBuffer arms).
            if receiver != obj && matches!(key, "byteLength" | "byteOffset" | "buffer") {
                let msg = format!(
                    "TypeError: get DataView.prototype.{key} called on a value that is not a DataView"
                );
                return Err(self.realm_thrown_from_proto(obj.heap_index(), msg));
            }
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
            let cal = self.cal_of(obj.heap_index());
            if let Some(v) = self.cal_date_getter(cal, (y, m, d), key) {
                return Ok(v);
            }
            return Ok(match key {
                // The weekday is calendar-independent: every calendar implemented
                // here shares the ISO seven-day week.
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "daysInWeek" => Value::num(7.0),
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
            let cal = self.cal_of(obj.heap_index());
            if let Some(v) = self.cal_date_getter(cal, (y, m, d), key) {
                return Ok(v);
            }
            return Ok(match key {
                "hour" => Value::num(f[3] as f64),
                "minute" => Value::num(f[4] as f64),
                "second" => Value::num(f[5] as f64),
                "millisecond" => Value::num(f[6] as f64),
                "microsecond" => Value::num(f[7] as f64),
                "nanosecond" => Value::num(f[8] as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "daysInWeek" => Value::num(7.0),
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
            let (y, m, rd) = (fields[0], fields[1], *fields.get(2).unwrap_or(&1));
            let cal = self.cal_of(obj.heap_index());
            // The reference ISO day is day 1 OF THE CALENDAR MONTH, so projecting
            // the stored ISO date lands on the right calendar year/month.
            if let Some(v) = self.cal_date_getter(cal, (y, m, rd), key) {
                return Ok(v);
            }
            return Ok(self.proto_member(self.plainyearmonth_proto, key));
        }
        // Temporal.PlainMonthDay getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 6, fields } = self.heap.get(obj.heap_index()) {
            let (y, m, d) = (fields[0], fields[1], fields[2]);
            let cal = self.cal_of(obj.heap_index());
            // PlainMonthDay exposes only monthCode/day/calendarId; the stored
            // reference ISO year is what makes the projection well-defined.
            if matches!(key, "monthCode" | "day" | "calendarId") {
                if let Some(v) = self.cal_date_getter(cal, (y, m, d), key) {
                    return Ok(v);
                }
            }
            return Ok(self.proto_member(self.plainmonthday_proto, key));
        }
        // Temporal.ZonedDateTime getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 7, .. } = self.heap.get(obj.heap_index()) {
            let idx = obj.heap_index();
            let f = self.zdt_local(idx); // [y,mo,d,h,mi,s,ms,us,ns]
            let (y, m, d) = (f[0], f[1], f[2]);
            let epoch = self.zdt_epoch_ns(idx).unwrap_or(0);
            let off = self.zdt_offset_ns(idx);
            let cal = self.cal_of(idx);
            if let Some(v) = self.cal_date_getter(cal, (y, m, d), key) {
                return Ok(v);
            }
            return Ok(match key {
                "hour" => Value::num(f[3] as f64),
                "minute" => Value::num(f[4] as f64),
                "second" => Value::num(f[5] as f64),
                "millisecond" => Value::num(f[6] as f64),
                "microsecond" => Value::num(f[7] as f64),
                "nanosecond" => Value::num(f[8] as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "daysInWeek" => Value::num(7.0),
                "hoursInDay" => {
                    // (startOfDay(tomorrow) − startOfDay(today)) / 1h. Both ends go
                    // through the zone, so a DST day is 23 or 25 hours (or 23.5, in
                    // Lord Howe); a fixed-offset zone's day is exactly 24.
                    const DAY_NS_I: i128 = 86_400_000_000_000;
                    const NS_MAX: i128 = 8_640_000_000_000_000_000_000;
                    let tz = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                    let today_local = iso_to_epoch_days(y, m, d) as i128 * DAY_NS_I;
                    let start = crate::vm::temporal::tz_start_of_day;
                    let today_start = start(&tz, today_local)?;
                    let tomorrow_start = start(&tz, today_local + DAY_NS_I)?;
                    // GetStartOfDay throws for BOTH boundaries: a nonzero offset can
                    // push today's local midnight itself past the instant range.
                    if today_start.abs() > NS_MAX || tomorrow_start.abs() > NS_MAX {
                        return Err(Thrown(
                            "RangeError: ZonedDateTime hoursInDay is outside the representable range"
                                .into(),
                        ));
                    }
                    Value::num(
                        (tomorrow_start - today_start) as f64 / 3_600_000_000_000.0,
                    )
                }
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
            // An Intl instance is an ordinary object as far as property access
            // goes: an assigned own property lives in the generic arr_props side
            // table (see `set_prop`) and shadows the prototype.
            let own = self.arr_props.get(&obj.heap_index()).and_then(|m| {
                m.pos(key).map(|i| (m.attrs[i], m.vals[i]))
            });
            if let Some((attr, raw)) = own {
                if attr.accessor {
                    return if raw == Value::UNDEFINED {
                        Ok(Value::UNDEFINED)
                    } else {
                        self.call_value(raw, receiver, &[])
                    };
                }
                return Ok(raw);
            }
            // Walk the REAL chain when a subclass / setPrototypeOf replaced it
            // (`class F extends Intl.ListFormat {}` puts F.prototype in front of
            // the service prototype); otherwise start at the service prototype.
            let start = match self.proto_of.get(&obj.heap_index()) {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => proto,
            };
            return self.proto_member_get(start, key, receiver);
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
                if key == "length" && !self.arguments_objs.contains_key(&obj.heap_index()) {
                    // A sparse array's JS length lives in the side table.
                    Ok(len_value(self.js_array_len(obj.heap_index())))
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
                        // A LIVE-mapped arguments index reads the formal's register.
                        if let Some(v) = self.args_mapped_get(obj.heap_index(), i as usize) {
                            return Ok(v);
                        }
                        Ok(own.unwrap())
                    } else {
                        // A SPARSE-overlay element (or a defineProperty'd index whose
                        // dense placeholder is a hole) lives in arr_props — it is an
                        // own property, consulted before the prototype chain.
                        if let Some((a, v)) =
                            self.array_index_override(obj.heap_index(), i as usize)
                        {
                            if a.accessor {
                                return if v == Value::UNDEFINED {
                                    Ok(Value::UNDEFINED)
                                } else {
                                    self.call_value(v, receiver, &[])
                                };
                            }
                            return Ok(v);
                        }
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
                    // Accessor-AWARE: a defineProperty'd getter on Array.prototype
                    // is invoked with the array as receiver (15.2.3.6-4-579).
                    let eff = self.array_eff_proto(obj.heap_index());
                    self.proto_member_get(eff, key, receiver)
                }
            }
            HeapObj::Str(s) => {
                if key == "length" {
                    Ok(len_value(s.units()))
                } else {
                    let p = self.active_realm_proto(self.str_proto);
                    self.proto_member_get(p, key, obj)
                }
            }
            HeapObj::Cons { len, .. } => {
                if key == "length" {
                    Ok(len_value(*len))
                } else {
                    let p = self.active_realm_proto(self.str_proto);
                    self.proto_member_get(p, key, obj)
                }
            }
            HeapObj::Object(map) => {
                if let Some(v) = map.get(key) {
                    return Ok(v);
                }
                // %Array.prototype% is an Array exotic object with an own
                // `length` (tracks its integer-index definitions).
                if obj.heap_index() == self.arr_proto && key == "length" {
                    return Ok(Value::num(self.arr_proto_len as f64));
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
                    // OrdinaryGet step 6: an accessor's getter runs with the
                    // RECEIVER, not the object it was found on — `Sub.staticGetter`
                    // must see `this === Sub`, and `Reflect.get(C, k, other)` must
                    // see `other`.
                    return self.call_value(g, receiver, &[]);
                }
                // A setter-only own static accessor (`static set name(_)`) is an own
                // property: reading it returns undefined and does NOT fall through to
                // %Function.prototype% (e.g. so it doesn't pick up Fp's "" name / 0
                // length).
                if c.static_setters.iter().any(|(k, _)| k == key) {
                    return Ok(Value::UNDEFINED);
                }
                // The parent class IS this constructor's [[Prototype]], so an own
                // miss is an ORDINARY prototype-chain step: delegate the whole
                // parent lookup to `get_member` instead of re-scanning statics
                // here. A statics-only scan could not see the parent's SYNTHESIZED
                // `name`/`length` (so `delete C.name; C.name` reported "" from
                // %Function.prototype% instead of the parent's name — className.js),
                // and it invoked an inherited `static get` with the parent rather
                // than the original receiver. The same call also covers a non-Class
                // parent (a built-in constructor or plain function, so
                // `class X extends Temporal.Y {}` inherits `Y.from`).
                if let Some(pidx) = c.parent {
                    return self.get_member(Value::heap(pidx), key, receiver);
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
            // A generator instance delegates to its [[Prototype]] — the
            // callee's `prototype` object when one was installed at call time
            // (proto_of), else %GeneratorPrototype% (next/return/throw +
            // @@iterator) which chains to %Iterator.prototype% (the helper
            // methods). So `g().next`, `g().map`, `g()[Symbol.iterator]` all
            // resolve, and a REPLACED g.prototype is honored.
            HeapObj::Generator { .. } => {
                let p = match self.proto_of.get(&obj.heap_index()) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => return Ok(Value::UNDEFINED), // null proto: no chain
                    None => self.gen_proto,
                };
                self.proto_chain_get(p, key, obj)
            }
            HeapObj::AsyncGenerator(_) => {
                let p = match self.proto_of.get(&obj.heap_index()) {
                    Some(p) if p.is_heap() => p.heap_index(),
                    Some(_) => return Ok(Value::UNDEFINED),
                    None => self.asyncgen_proto,
                };
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
                let p = self.active_realm_proto(self.symbol_proto);
                self.proto_member_get(p, key, obj)
            }
            // A BigInt: methods (toString/valueOf/constructor) via BigInt.prototype.
            HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                let p = self.active_realm_proto(self.bigint_proto);
                self.proto_member_get(p, key, obj)
            }
            // Functions / natives / bound functions: own props set on them
            // (`assert.sameValue`), then Function.prototype (`call`/`apply`/`bind`).
            _ if matches!(
                self.heap.get(obj.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Wrapped { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. }
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
                // Poison-pill: `caller`/`arguments` on a restricted function are
                // the %ThrowTypeError% accessors (AddRestrictedFunctionProperties).
                // A LEGACY sloppy ordinary function instead owns them as live
                // properties — handled here so the inherited throwing accessor on
                // Function.prototype is neither run nor leaked by the walk below.
                if key == "caller" || key == "arguments" {
                    if self.fn_restricted_caller(obj.heap_index()) {
                        return Err(Thrown(format!(
                            "TypeError: '{key}' may not be accessed on strict-mode or bound functions"
                        )));
                    }
                    let i = obj.heap_index();
                    return Ok(if key == "caller" {
                        self.legacy_fn_caller(i)
                    } else {
                        self.legacy_fn_arguments(i)
                    });
                }
                // Inherited methods: an explicit [[Prototype]] override (a
                // Reflect.construct(Function, …, foreignNewTarget) function whose
                // proto is another realm's %Function.prototype%) wins; else a
                // generator/async function starts at its dynamic-function
                // intrinsic prototype (so `gen.constructor` is
                // %GeneratorFunction%), else %Function.prototype% (call/apply/bind),
                // then up to Object.prototype (toString/valueOf/hasOwnProperty/…).
                let start = self
                    .proto_of
                    .get(&obj.heap_index())
                    .and_then(|p| p.is_heap().then(|| p.heap_index()))
                    .or_else(|| self.callable_dynfn_proto(obj.heap_index()))
                    .unwrap_or(self.fn_proto);
                // Accessor-aware so an inherited getter on Function.prototype (or a
                // dynamic-function intrinsic) is invoked with this = receiver.
                self.proto_member_get(start, key, receiver)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

}
