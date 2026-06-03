#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// `new <class>(args)`: build a plain object, install the class's methods as
    /// own Func properties, then run the constructor (if any) with `this` = the
    /// new object. A constructor that returns an object/array replaces the
    /// instance (JS semantics); otherwise the instance is returned.
    pub(crate) fn construct(&mut self, cv: Value, args: &[Value]) -> Result<Value, Thrown> {
        if !cv.is_heap() {
            return Err(Thrown("TypeError: value is not a constructor".into()));
        }
        // A built-in error constructor used as a VALUE (`var E = TypeError; new E()`,
        // `Reflect.construct(RangeError, [msg])`). Mirrors the compile-lowered
        // `new TypeError(msg)` path. AggregateError takes the message as arg[1].
        if let Some(k) = self.error_ctors.iter().position(|&c| c == cv.heap_index()) {
            let msg = if k == 7 { args.get(1).copied() } else { args.first().copied() };
            return Ok(self.make_error(k as u8, msg));
        }
        // ArrayBuffer / DataView / TypedArray constructors used as values.
        let ci = cv.heap_index();
        if ci == self.arraybuffer_ctor && ci != 0 {
            return self.build_array_buffer(args);
        }
        if ci == self.dataview_ctor && ci != 0 {
            return self.build_data_view(args);
        }
        if let Some(k) = self.ta_ctors.iter().position(|&c| c == ci && ci != 0) {
            return self.build_typed_array(k as u8, args);
        }
        if ci == self.ta_base_ctor && ci != 0 {
            return Err(Thrown("TypeError: Abstract class TypedArray not directly constructable".into()));
        }
        if ci == self.iterator_ctor && ci != 0 {
            return Err(Thrown(
                "TypeError: Abstract class Iterator not directly constructable".into(),
            ));
        }
        if ci == self.proxy_ctor && ci != 0 {
            return self.make_proxy(
                args.first().copied().unwrap_or(Value::UNDEFINED),
                args.get(1).copied().unwrap_or(Value::UNDEFINED),
            );
        }
        if ci == self.duration_ctor && ci != 0 {
            return self.build_duration(args);
        }
        if ci == self.plaindate_ctor && ci != 0 {
            let y = self.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
            let m = self.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))? as i64;
            let d = self.to_number(args.get(2).copied().unwrap_or(Value::UNDEFINED))? as i64;
            return self.make_plain_date(y, m, d);
        }
        if ci == self.plaintime_ctor && ci != 0 {
            let mut f = [0i64; 6];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.to_number(v)? as i64;
                }
            }
            return self.make_plain_time(f);
        }
        if ci == self.plaindatetime_ctor && ci != 0 {
            // year/month/day required (omitted → 0 → RangeError); time fields default 0.
            let mut f = [0i64; 9];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.to_number(v)? as i64;
                }
            }
            return self.make_plain_date_time(f);
        }
        if ci == self.instant_ctor && ci != 0 {
            let ns = self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            return self.make_instant(ns);
        }
        if ci == self.plainyearmonth_ctor && ci != 0 {
            // (year, month, calendar?, referenceISODay=1)
            let y = self.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
            let m = self.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))? as i64;
            let rd = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.to_number(v)? as i64,
                _ => 1,
            };
            return self.make_plain_year_month(y, m, rd);
        }
        if ci == self.plainmonthday_ctor && ci != 0 {
            // (month, day, calendar?, referenceISOYear=1972)
            let m = self.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
            let d = self.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))? as i64;
            let ry = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.to_number(v)? as i64,
                _ => 1972,
            };
            return self.make_plain_month_day(m, d, ry);
        }
        if ci == self.zoneddatetime_ctor && ci != 0 {
            return self.make_zoned_date_time(args);
        }
        // Intl.<service> constructors.
        if self.intl_ctors[0] != 0 {
            if let Some(kind) = self.intl_ctors.iter().position(|&c| c == ci) {
                let locales = args.first().copied().unwrap_or(Value::UNDEFINED);
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                return self.make_intl(kind as u8, locales, options);
            }
        }
        // Constructing through a Proxy: `construct` trap (or construct the target).
        if let Some((target, handler, revoked)) = self.proxy_parts(ci) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'construct' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "construct")? {
                Some(trap) => {
                    let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
                    self.call_value(trap, handler, &[target, arr, cv])
                }
                None => self.construct(target, args),
            };
        }
        // A core built-in constructor used as a VALUE (`new C()` where C is the
        // Array/Object/Map/… constructor reached via a variable, `.constructor`,
        // or a species lookup — not the compile-lowered `new Array()` literal).
        // Identify it by its own `prototype` (the canonical proto object), so it
        // works however the constructor was obtained.
        let builtin_proto = match self.heap.get(ci) {
            HeapObj::Object(m) if m.is_ctor => {
                m.get("prototype").filter(|p| p.is_heap()).map(|p| p.heap_index())
            }
            _ => None,
        };
        if let Some(p) = builtin_proto {
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if p == self.arr_proto && self.arr_proto != 0 {
                let arr = if args.len() == 1 && a0.is_number() {
                    let n = a0.as_f64();
                    if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    if n as usize > super::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: array length exceeds the engine's dense-array limit".into(),
                        ));
                    }
                    vec![Value::UNDEFINED; n as usize]
                } else {
                    args.to_vec()
                };
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(arr))));
            }
            if p == self.obj_proto && self.obj_proto != 0 {
                return self.to_object(a0);
            }
            if p == self.num_proto && self.num_proto != 0 {
                let n = if args.is_empty() { 0.0 } else { self.to_number(a0)? };
                return Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 1, value: Value::num(n) })));
            }
            if p == self.bool_proto && self.bool_proto != 0 {
                let b = !args.is_empty() && self.truthy(a0);
                return Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 2, value: Value::bool(b) })));
            }
            if p == self.str_proto && self.str_proto != 0 {
                let s = if args.is_empty() { String::new() } else { self.to_js_string(a0)? };
                let sv = self.alloc_str(s);
                return Ok(Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 0, value: sv })));
            }
            if p == self.regexp_proto && self.regexp_proto != 0 {
                return self.build_regexp(a0, args.get(1).copied().unwrap_or(Value::UNDEFINED));
            }
            if p == self.map_proto && self.map_proto != 0 {
                let (mut keys, mut vals): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
                if !a0.is_nullish() {
                    for e in self.iterate_to_vec(a0)? {
                        let k = normalize_zero(self.get_index(e, Value::int(0))?);
                        let v = self.get_index(e, Value::int(1))?;
                        match keys.iter().position(|kk| self.same_value_zero(*kk, k)) {
                            Some(i) => vals[i] = v,
                            None => {
                                keys.push(k);
                                vals.push(v);
                            }
                        }
                    }
                }
                return Ok(Value::heap(self.heap.alloc(HeapObj::Map { keys, vals })));
            }
            if p == self.set_proto && self.set_proto != 0 {
                let mut items: Vec<Value> = Vec::new();
                if !a0.is_nullish() {
                    for e in self.iterate_to_vec(a0)? {
                        let v = normalize_zero(e);
                        if !items.iter().any(|x| self.same_value_zero(*x, v)) {
                            items.push(v);
                        }
                    }
                }
                return Ok(Value::heap(self.heap.alloc(HeapObj::Set(items))));
            }
            if p == self.date_proto && self.date_proto != 0 {
                let ms = self.date_new_ms(args)?;
                return Ok(Value::heap(self.heap.alloc(HeapObj::Date(ms))));
            }
            if p == self.promise_proto && self.promise_proto != 0 {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!(
                        "TypeError: Promise resolver {} is not a function",
                        self.display(a0)
                    )));
                }
                let prom = self.alloc_promise();
                let res = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: prom, is_reject: false }),
                );
                let rej = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: prom, is_reject: true }),
                );
                if self.call_value(a0, Value::UNDEFINED, &[res, rej]).is_err() {
                    let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                    self.reject(prom, reason);
                }
                return Ok(Value::heap(prom));
            }
        }
        // Constructor FUNCTION (`new F()`, the pre-class OOP idiom): make an object
        // whose [[Prototype]] is `F.prototype` (so its methods + `constructor`
        // resolve), run `F` with `this` = that object, and use F's return value if
        // it returns an object (else the new object).
        if matches!(
            self.heap.get(cv.heap_index()),
            HeapObj::Func(_) | HeapObj::Closure { .. }
        ) {
            let proto = self.prototype_of(cv).unwrap_or(Value::UNDEFINED);
            let obj = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
            if proto.is_heap() {
                self.proto_of.insert(obj.heap_index(), proto);
            }
            let ret = self.call_value(cv, obj, args)?;
            if ret.is_heap()
                && matches!(self.heap.get(ret.heap_index()), HeapObj::Object(_) | HeapObj::Array(_))
            {
                return Ok(ret);
            }
            return Ok(obj);
        }
        let (ctor, has_explicit, parent) = match self.heap.get(cv.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.has_explicit_ctor, c.parent),
            _ => return Err(Thrown("TypeError: value is not a constructor".into())),
        };
        // The instance links to its class for method lookup + instanceof; its own
        // keys hold only the fields (so enumeration / JSON stay method-free).
        let mut map = ObjMap::new();
        map.class = Some(cv.heap_index());
        let obj = Value::heap(self.heap.alloc(HeapObj::Object(map)));
        if has_explicit {
            // The explicit constructor runs its own `super(...)`; a ctor that
            // returns an object/array replaces the instance.
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                let ret = self.call_value(f, obj, args)?;
                if ret.is_heap()
                    && matches!(self.heap.get(ret.heap_index()), HeapObj::Object(_) | HeapObj::Array(_))
                {
                    return Ok(ret);
                }
            }
        } else {
            // No own constructor: run the parent's ctor (implicit `super(...args)`)
            // then this class's field initializers.
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(obj)
    }

    /// `v instanceof F` for a constructor FUNCTION `F`: true iff `F.prototype` is
    /// somewhere in `v`'s prototype chain.
    pub(crate) fn instanceof_via_proto(&mut self, v: Value, ctor: Value) -> bool {
        let target = match self.prototype_of(ctor) {
            Some(p) => p,
            None => return false,
        };
        let mut cur = self.object_get_prototype_of(v);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return false;
            }
            if cur == target {
                return true;
            }
            cur = self.object_get_prototype_of(cur);
        }
        false
    }

    /// True iff `v` is an object whose class chain includes the class at heap
    /// index `class_idx` (`v instanceof C`, walking `extends` links).
    pub(crate) fn instance_of_class(&self, v: Value, class_idx: u32) -> bool {
        if !v.is_heap() {
            return false;
        }
        let mut cur = match self.heap.get(v.heap_index()) {
            HeapObj::Object(m) => m.class,
            _ => None,
        };
        while let Some(cidx) = cur {
            if cidx == class_idx {
                return true;
            }
            cur = match self.heap.get(cidx) {
                HeapObj::Class(c) => c.parent,
                _ => None,
            };
        }
        false
    }

    /// The superclass value for a `super` reference inside a method of class
    /// `home_class_id`: that class's runtime `ClassData.parent` (linked by
    /// MakeClass from the evaluated `extends` expression), or None.
    pub(crate) fn super_parent(&self, home_class_id: u32) -> Option<Value> {
        let home = (*self.class_values.get(home_class_id as usize)?)?;
        match self.heap.get(home.heap_index()) {
            HeapObj::Class(c) => c.parent.map(Value::heap),
            _ => None,
        }
    }

    /// `super.key = v`: PutValue on a super reference. If the superclass's
    /// prototype chain exposes a setter for `key`, invoke it with `this` = the
    /// receiver; otherwise create/update an own property on the receiver itself
    /// (the spec sets on the receiver, not the prototype).
    pub(crate) fn super_set(
        &mut self,
        home_class_id: u32,
        key: &str,
        this: Value,
        v: Value,
    ) -> Result<(), Thrown> {
        let parent = self
            .super_parent(home_class_id)
            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
        let proto = self.prototype_of(parent).unwrap_or(Value::UNDEFINED);
        let setter = self.lookup_accessor(proto, key, true);
        if self.is_callable(setter) {
            self.call_value(setter, this, &[v])?;
        } else {
            self.set_prop(this, key, v)?;
        }
        Ok(())
    }

    /// Run a class's constructor contribution on an existing instance `obj` —
    /// for `super(...)` and the implicit-super chain. An explicit ctor runs its
    /// own `super`; an implicit one runs the parent chain then its fields.
    pub(crate) fn run_class_ctor(&mut self, cval: Value, obj: Value, args: &[Value]) -> Result<(), Thrown> {
        if !cval.is_heap() {
            return Ok(());
        }
        let (ctor, has_explicit, parent) = match self.heap.get(cval.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.has_explicit_ctor, c.parent),
            // `super(...)` to a BUILT-IN parent (`class X extends Error`). We model
            // the Error family: set `message` on the instance from the argument
            // (AggregateError takes it as the 2nd arg). The instance's prototype
            // chain already reaches the error prototype (so name/toString/
            // instanceof resolve), so nothing else is needed here.
            _ => {
                if let Some(k) = self.error_ctors.iter().position(|&c| c == cval.heap_index()) {
                    let msg = if k == 7 { args.get(1).copied() } else { args.first().copied() };
                    if let Some(m) = msg.filter(|m| *m != Value::UNDEFINED) {
                        let mi = self.to_str_idx(m);
                        if let HeapObj::Object(map) = self.heap.get_mut(obj.heap_index()) {
                            map.set("message", Value::heap(mi));
                        }
                    }
                }
                return Ok(());
            }
        };
        if has_explicit {
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, args)?;
            }
        } else {
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(())
    }

    /// `Object.assign(target, ...sources)`: copy each source's own enumerable
    /// keys (object keys, or an array's index strings) onto `target`; returns
    /// `target`. Primitive (incl. null/undefined) sources contribute nothing.
    pub(crate) fn object_assign(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let target = args.first().copied().unwrap_or(Value::UNDEFINED);
        if !target.is_heap() || !matches!(self.heap.get(target.heap_index()), HeapObj::Object(_)) {
            return Err(Thrown("TypeError: Object.assign target must be an object".into()));
        }
        let tidx = target.heap_index();
        let mut added_any = false;
        for &src in &args[1..] {
            if !src.is_heap() {
                continue;
            }
            // Gather (key, val) pairs under the immutable borrow, then write.
            // (A string source spreads as index→char, like an array.)
            let str_chars: Option<Vec<char>> = match self.heap.get(src.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Some(self.heap.str_cow(src.heap_index()).unwrap().chars().collect())
                }
                _ => None,
            };
            let pairs: Vec<(String, Value)> = if let Some(chars) = str_chars {
                chars
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| (i.to_string(), self.alloc_str(c.to_string())))
                    .collect()
            } else {
                match self.heap.get(src.heap_index()) {
                    // Object.assign copies own ENUMERABLE properties only.
                    HeapObj::Object(map) => spec_key_order(&map.keys)
                        .into_iter()
                        .filter(|&i| map.attrs[i].enumerable)
                        .map(|i| (map.keys[i].clone(), map.vals[i]))
                        .collect(),
                    HeapObj::Array(items) => {
                        items.iter().enumerate().map(|(i, &v)| (i.to_string(), v)).collect()
                    }
                    _ => Vec::new(),
                }
            };
            for (k, v) in pairs {
                if let HeapObj::Object(map) = self.heap.get_mut(tidx) {
                    added_any |= map.set(&k, v);
                }
            }
        }
        if added_any {
            self.heap.bump_version(tidx);
        }
        Ok(target)
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

    pub(crate) fn is_callable(&self, v: Value) -> bool {
        v.is_heap()
            && match self.heap.get(v.heap_index()) {
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                    true
                }
                // A built-in constructor object (String/Number/Array/…) is callable
                // (typeof is "function") — it can be passed as a callback.
                HeapObj::Object(m) => m.is_ctor,
                _ => false,
            }
    }

    /// `obj.hasOwnProperty(key)` — own data/accessor property, array index/length,
    /// or string index/length.
    pub(crate) fn has_own_property(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() || is_private_key(key) {
            return false; // private names aren't reflectable own properties
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
                key == "length"
                    || key.parse::<usize>().map_or(false, |i| i < items.len())
                    || self.arr_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
            }
            HeapObj::Str(s) => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < s.char_len)
            }
            HeapObj::Cons { len, .. } => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < *len)
            }
            // A class value: own statics (data + `static get`/`set`) + name/length.
            HeapObj::Class(c) => {
                c.statics.pos(key).is_some()
                    || c.static_getters.iter().any(|(n, _)| n == key)
                    || c.static_setters.iter().any(|(n, _)| n == key)
                    || self.callable_has_intrinsic(obj, key)
            }
            // Functions/closures/etc.: assigned own props (`fn.x`) + name/length.
            _ => {
                self.fn_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
                    || self.callable_has_intrinsic(obj, key)
            }
        }
    }

    /// `obj.propertyIsEnumerable(key)` — true if `key` is an own enumerable
    /// property. Array indices are enumerable; `length` is not.
    pub(crate) fn own_is_enumerable(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() || is_private_key(key) {
            return false;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).map_or(false, |i| m.attrs[i].enumerable),
            HeapObj::Array(items) => {
                key.parse::<usize>().map_or(false, |i| i < items.len())
                    || self
                        .arr_props
                        .get(&obj.heap_index())
                        .and_then(|m| m.pos(key).map(|i| m.attrs[i].enumerable))
                        .unwrap_or(false)
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
    pub(crate) fn get_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let m = self.get_prop(v, "@@iterator")?;
            if self.is_callable(m) {
                return self.call_value(m, v, &[]);
            }
        }
        Ok(v)
    }

    /// `for await`: resolve the ASYNC iterator. An async generator is its own
    /// iterator; a plain object uses `@@asyncIterator` (an async iterable) or, as
    /// the spec's async-from-sync fallback, `@@iterator`; everything else (arrays,
    /// strings, Map/Set, sync generators) passes through (ForAwaitNext drives it).
    pub(crate) fn get_async_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let am = self.get_prop(v, "@@asyncIterator")?;
            if self.is_callable(am) {
                return self.call_value(am, v, &[]);
            }
            let sm = self.get_prop(v, "@@iterator")?;
            if self.is_callable(sm) {
                return self.call_value(sm, v, &[]);
            }
        }
        Ok(v)
    }

    /// Normalize a destructuring source to a positionally-indexable value: a
    /// generator or a custom iterable (object with `@@iterator`) is drained into a
    /// fresh array — LAZILY, at most `max` elements (so `let [a,b] = infinite`
    /// pulls 2, not forever); everything else (arrays/strings/Map/Set, or a
    /// non-iterable) passes through unchanged.
    pub(crate) fn iter_to_array(&mut self, v: Value, max: u32) -> Result<Value, Thrown> {
        if !v.is_heap() {
            return Ok(v);
        }
        let drain = match self.heap.get(v.heap_index()) {
            HeapObj::Generator { .. } => true,
            HeapObj::Object(_) => {
                let it = self.get_prop(v, "@@iterator")?;
                self.is_callable(it)
            }
            _ => false,
        };
        if !drain {
            return Ok(v);
        }
        // Hold the not-yet-rooted drained values across the `.next()`/`.return()`
        // user re-entries.
        let _gc = self.gc_lock_guard();
        let iter = self.get_iterator(v)?; // generator → itself; iterable → its iterator
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
            self.iterator_close(iter)?;
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// IteratorClose(iterator, normal): call the iterator's `return()` once if it
    /// has one, requiring an Object result (TypeError otherwise). Skips generators
    /// (driven directly via generator_method, not a prototype `return`) and
    /// non-objects. Shared by destructuring and `for-of` break.
    pub(crate) fn iterator_close(&mut self, iter: Value) -> Result<(), Thrown> {
        if !iter.is_heap() {
            return Ok(());
        }
        if matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. }) {
            return Ok(());
        }
        let ret = self.get_prop(iter, "return")?;
        if self.is_callable(ret) {
            let r = self.call_value(ret, iter, &[])?;
            if !self.is_object_value(r) {
                return Err(Thrown("TypeError: iterator return() result is not an object".into()));
            }
        }
        Ok(())
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
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_) | HeapObj::Iterator { .. } | HeapObj::IterHelper { .. }) {
            let next = self.get_prop(v, "next")?;
            if self.is_callable(next) {
                let mut out = Vec::new();
                loop {
                    let res = self.call_value(next, v, &[])?;
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
        }
        let plan = if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(items) => Plan::Vals(items.clone()),
                HeapObj::Set(items) => Plan::Vals(items.clone()),
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Plan::Chars(self.heap.str_cow(v.heap_index()).unwrap().chars().collect())
                }
                HeapObj::Map { keys, vals } => {
                    Plan::Pairs(keys.iter().copied().zip(vals.iter().copied()).collect())
                }
                _ => return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v)))),
            }
        } else {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        };
        Ok(match plan {
            Plan::Vals(v) => v,
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
        // Classify the source under a short-lived borrow, then materialize its
        // elements (the object/array-like path needs &mut self for get_prop).
        enum Kind {
            Iterable,
            Obj,
            Other,
        }
        let mut elems: Vec<Value> = Vec::new();
        let kind = if src.is_heap() {
            match self.heap.get(src.heap_index()) {
                HeapObj::Array(_)
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
                | HeapObj::Set(_)
                | HeapObj::Map { .. }
                | HeapObj::TypedArray { .. }
                | HeapObj::Generator { .. } => Kind::Iterable,
                HeapObj::Object(_) => Kind::Obj,
                _ => Kind::Other,
            }
        } else {
            Kind::Other
        };
        match kind {
            Kind::Iterable => elems = self.iterate_to_vec(src)?,
            Kind::Obj => {
                // A custom iterable object (`@@iterator`) → iterate it; otherwise
                // treat it as array-like (read `length`, then indices 0..length).
                let it = self.get_prop(src, "@@iterator")?;
                if self.is_callable(it) {
                    elems = self.iterate_to_vec(src)?;
                } else {
                    let len = self.get_prop(src, "length")?;
                    // ToLength: ToInteger(length) clamped to >= 0 (so a string/
                    // boolean length like {length:"3"} is honoured).
                    let n_i = self.to_integer_or_zero(len)?;
                    let n = if n_i > 0 { n_i as usize } else { 0 };
                    if n > crate::vm::MAX_DENSE_ARRAY_LEN {
                        return Err(Thrown(
                            "RangeError: array length exceeds the engine's dense-array limit".into(),
                        ));
                    }
                    for i in 0..n {
                        elems.push(self.get_index(src, Value::int(i as i32))?);
                    }
                }
            }
            Kind::Other => {}
        }
        // Apply the map callback, if given (validated callable above), with the
        // supplied thisArg.
        if mapfn != Value::UNDEFINED {
            for (i, slot) in elems.iter_mut().enumerate() {
                let args = [*slot, Value::int(i as i32)];
                *slot = self.call_value(mapfn, this_arg, &args)?;
            }
        }
        // When `Array.from` is called with a custom constructor as `this`
        // (Array.from.call(C, …) / a subclass), build the result via
        // Construct(C, «len») and define each element on it, rather than always
        // returning a plain Array. The Array global itself keeps the fast path.
        let is_array_global = this_ctor.is_heap()
            && matches!(self.heap.get(this_ctor.heap_index()), HeapObj::Object(m)
                if m.get("prototype").is_some_and(|p| p.is_heap() && p.heap_index() == self.arr_proto));
        if !is_array_global && self.is_constructor(this_ctor) {
            let len = elems.len();
            let a = self.construct(this_ctor, &[Value::num(len as f64)])?;
            for (i, v) in elems.iter().enumerate() {
                self.set_index(a, Value::num(i as f64), *v)?;
            }
            self.set_prop(a, "length", Value::num(len as f64))?;
            return Ok(a);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

}
