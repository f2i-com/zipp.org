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
    /// `new Function(p1, …, pN, body)` / `Function(...)`: the leading arguments
    /// are parameter source (comma-joined exactly as written, so `("a,b","c")`
    /// works too); the last is the function body. Reuses `do_eval`: the
    /// completion value of evaluating a parenthesized function expression IS the
    /// function. A malformed body/parameter list surfaces as the parser's
    /// SyntaxError. The result runs in global scope (it captures nothing).
    pub(crate) fn build_function(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        self.build_function_kind(args, 0)
    }

    /// Shared by all four dynamic-function constructors. `kind`: 0=Function,
    /// 1=GeneratorFunction, 2=AsyncFunction, 3=AsyncGeneratorFunction — selecting
    /// the `function` / `function*` / `async function` / `async function*` wrapper
    /// keyword so the eval completion value is a function of the right kind.
    pub(crate) fn build_function_kind(&mut self, args: &[Value], kind: u8) -> Result<Value, Thrown> {
        let (params, body) = if args.is_empty() {
            (String::new(), String::new())
        } else {
            let body = self.to_js_string(args[args.len() - 1])?;
            let mut parts: Vec<String> = Vec::with_capacity(args.len() - 1);
            for a in &args[..args.len() - 1] {
                parts.push(self.to_js_string(*a)?);
            }
            (parts.join(","), body)
        };
        let prefix = match kind {
            1 => "function* ",
            2 => "async function ",
            3 => "async function* ",
            _ => "function ",
        };
        // The newline before `)` defends against a `//` comment in the last
        // parameter; the wrapper parens make the body a function EXPRESSION whose
        // value (the function) becomes the eval completion value.
        let source = format!("({prefix}anonymous({params}\n) {{\n{body}\n}})");
        self.do_eval(&source)
    }

    /// `ShadowRealm.prototype.evaluate` / `.importValue`. NOTE: not truly isolated
    /// (evaluate reuses the shared global eval path), so cross-realm isolation is
    /// not enforced; argument/return-type validation and primitive results are.
    pub(crate) fn shadowrealm_op(&mut self, op: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        if !(this.is_heap() && self.shadow_realms.contains(&this.heap_index())) {
            return Err(Thrown("TypeError: receiver is not a ShadowRealm".into()));
        }
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match op {
            native::SHADOWREALM_EVALUATE => {
                let is_str = a0.is_heap()
                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. });
                if !is_str {
                    return Err(Thrown(
                        "TypeError: ShadowRealm.prototype.evaluate expects a string".into(),
                    ));
                }
                let code = self.display(a0);
                // An error thrown by the evaluated code can't cross the realm
                // boundary, so it surfaces as a TypeError in the calling realm.
                let result = match self.do_eval(&code) {
                    Ok(r) => r,
                    Err(_) => {
                        return Err(Thrown(
                            "TypeError: ShadowRealm evaluate threw (error wrapped at the realm boundary)".into(),
                        ))
                    }
                };
                // Only primitives and callables may cross the boundary.
                if result.is_heap() {
                    if self.is_callable(result) {
                        return Ok(result); // v1: returned unwrapped (WrappedFunction TBD)
                    }
                    if matches!(
                        self.heap.get(result.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_)
                    ) {
                        return Ok(result);
                    }
                    return Err(Thrown(
                        "TypeError: ShadowRealm evaluate result is not a primitive or callable".into(),
                    ));
                }
                Ok(result)
            }
            native::SHADOWREALM_IMPORTVALUE => {
                // Module loading is unsupported; return a rejected promise.
                let p = self.alloc_promise();
                let e = self.alloc_error_from_message(
                    "TypeError: ShadowRealm.prototype.importValue is not supported",
                );
                self.reject(p, e);
                Ok(Value::heap(p))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `new SuppressedError(error, suppressed, message)`: an error object with
    /// own `error` + `suppressed` (always) and `message` (only if provided),
    /// linked to %SuppressedError.prototype%.
    pub(crate) fn build_suppressed_error(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let error = args.first().copied().unwrap_or(Value::UNDEFINED);
        let suppressed = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        let message = args.get(2).copied().unwrap_or(Value::UNDEFINED);
        // message ToString runs first (may execute user code) — before any alloc.
        let msg_val = if message != Value::UNDEFINED {
            let s = self.to_js_string(message)?;
            Some(self.alloc_str(s))
        } else {
            None
        };
        let attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut m = ObjMap::new();
        if let Some(mv) = msg_val {
            m.define("message", mv, attr);
        }
        m.define("error", error, attr);
        m.define("suppressed", suppressed, attr);
        let idx = self.heap.alloc(HeapObj::Object(m));
        if self.suppressederror_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.suppressederror_proto));
        }
        Ok(Value::heap(idx))
    }

    /// Allocate a fresh `DisposableStack` instance (a plain object linked to
    /// %DisposableStack.prototype%, with an empty, not-yet-disposed disposer stack).
    pub(crate) fn alloc_disposable_stack(&mut self, is_async: bool) -> u32 {
        let idx = self.heap.alloc(HeapObj::Object(ObjMap::new()));
        let proto = if is_async {
            self.asyncdisposablestack_proto
        } else {
            self.disposablestack_proto
        };
        if proto != 0 {
            self.proto_of.insert(idx, Value::heap(proto));
        }
        self.dispose_stacks.insert(idx, (Vec::new(), false));
        if is_async {
            self.async_stacks.insert(idx);
        }
        idx
    }

    /// Dispatch a `DisposableStack.prototype` method / `disposed` getter. `op` is
    /// one of the `DISPOSABLE_*` native ids.
    pub(crate) fn disposable_op(&mut self, op: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        use native::*;
        if !(this.is_heap() && self.dispose_stacks.contains_key(&this.heap_index())) {
            return Err(Thrown("TypeError: receiver is not a DisposableStack".into()));
        }
        let ti = this.heap_index();
        let disposed = self.dispose_stacks.get(&ti).map(|(_, d)| *d).unwrap_or(true);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Mutating methods reject a disposed stack with a ReferenceError.
        if matches!(op, DISPOSABLE_USE | DISPOSABLE_ADOPT | DISPOSABLE_DEFER | DISPOSABLE_MOVE) && disposed
        {
            return Err(Thrown("ReferenceError: the DisposableStack has been disposed".into()));
        }
        match op {
            DISPOSABLE_DISPOSED_GET => Ok(Value::bool(disposed)),
            DISPOSABLE_USE => {
                if a0.is_nullish() {
                    return Ok(a0);
                }
                // Async stacks prefer @@asyncDispose, falling back to @@dispose.
                let is_async = self.async_stacks.contains(&ti);
                let mut method = if is_async {
                    self.get_member(a0, "@@asyncDispose", a0)?
                } else {
                    Value::UNDEFINED
                };
                if !self.is_callable(method) {
                    method = self.get_member(a0, "@@dispose", a0)?;
                }
                if !self.is_callable(method) {
                    return Err(Thrown(
                        "TypeError: value is not disposable (its [Symbol.dispose] is not a function)".into(),
                    ));
                }
                let disposer = Value::heap(self.heap.alloc(HeapObj::Bound {
                    target: method,
                    this: a0,
                    args: Vec::new(),
                }));
                if let Some((d, _)) = self.dispose_stacks.get_mut(&ti) {
                    d.push(disposer);
                }
                Ok(a0)
            }
            DISPOSABLE_ADOPT => {
                let on = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_callable(on) {
                    return Err(Thrown("TypeError: onDispose is not callable".into()));
                }
                let disposer = Value::heap(self.heap.alloc(HeapObj::Bound {
                    target: on,
                    this: Value::UNDEFINED,
                    args: vec![a0],
                }));
                if let Some((d, _)) = self.dispose_stacks.get_mut(&ti) {
                    d.push(disposer);
                }
                Ok(a0)
            }
            DISPOSABLE_DEFER => {
                if !self.is_callable(a0) {
                    return Err(Thrown("TypeError: onDispose is not callable".into()));
                }
                if let Some((d, _)) = self.dispose_stacks.get_mut(&ti) {
                    d.push(a0);
                }
                Ok(Value::UNDEFINED)
            }
            DISPOSABLE_DISPOSE => {
                // Idempotent: a second dispose() is a no-op.
                let disposers = match self.dispose_stacks.get_mut(&ti) {
                    Some((d, dd)) if !*dd => {
                        *dd = true;
                        std::mem::take(d)
                    }
                    _ => return Ok(Value::UNDEFINED),
                };
                // Run disposers in LIFO order. Spec aggregates thrown errors into a
                // SuppressedError; v1 runs them all and re-throws the last one.
                let mut pending: Option<Thrown> = None;
                for disposer in disposers.into_iter().rev() {
                    if let Err(e) = self.call_value(disposer, Value::UNDEFINED, &[]) {
                        pending = Some(e);
                    }
                }
                match pending {
                    Some(e) => Err(e),
                    None => Ok(Value::UNDEFINED),
                }
            }
            DISPOSABLE_DISPOSE_ASYNC => {
                // Idempotent; returns a Promise. v1 runs the disposers eagerly
                // (LIFO) then settles the promise — true per-disposer awaiting is a
                // follow-on. Errors reject the returned promise.
                let already = self.dispose_stacks.get(&ti).map(|(_, d)| *d).unwrap_or(true);
                let disposers = if already {
                    Vec::new()
                } else if let Some((d, dd)) = self.dispose_stacks.get_mut(&ti) {
                    *dd = true;
                    std::mem::take(d)
                } else {
                    Vec::new()
                };
                let mut pending: Option<Thrown> = None;
                for disposer in disposers.into_iter().rev() {
                    if let Err(e) = self.call_value(disposer, Value::UNDEFINED, &[]) {
                        pending = Some(e);
                    }
                }
                let p = self.alloc_promise();
                match pending {
                    Some(e) => {
                        let ev = self.alloc_error_from_message(&e.0);
                        self.reject(p, ev);
                    }
                    None => self.resolve(p, Value::UNDEFINED),
                }
                Ok(Value::heap(p))
            }
            DISPOSABLE_MOVE => {
                let is_async = self.async_stacks.contains(&ti);
                let disposers = match self.dispose_stacks.get_mut(&ti) {
                    Some((d, dd)) => {
                        let taken = std::mem::take(d);
                        *dd = true;
                        taken
                    }
                    None => Vec::new(),
                };
                let new_idx = self.alloc_disposable_stack(is_async);
                if let Some((d, _)) = self.dispose_stacks.get_mut(&new_idx) {
                    *d = disposers;
                }
                Ok(Value::heap(new_idx))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `new cv(args)` with newTarget defaulting to the constructor itself (the
    /// common case for `new` / a plain `Reflect.construct(cv, args)`).
    pub(crate) fn construct(&mut self, cv: Value, args: &[Value]) -> Result<Value, Thrown> {
        self.construct_with_newtarget(cv, args, cv)
    }

    /// [[Construct]](argumentsList, newTarget). `new_target` is threaded to a Proxy
    /// `construct` trap (its 3rd argument) and through a trap-less Proxy's forward to
    /// the target; the ordinary Func/Class paths build the instance from `cv` (using
    /// `new_target` for the instance's [[Prototype]] is a separate, larger fix).
    pub(crate) fn construct_with_newtarget(
        &mut self,
        cv: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, Thrown> {
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
        if ci == self.function_ctor && ci != 0 {
            return self.build_function(args);
        }
        if ci == self.gen_fn_ctor && ci != 0 {
            return self.build_function_kind(args, 1);
        }
        if ci == self.async_fn_ctor && ci != 0 {
            return self.build_function_kind(args, 2);
        }
        if ci == self.asyncgen_fn_ctor && ci != 0 {
            return self.build_function_kind(args, 3);
        }
        if ci == self.arraybuffer_ctor && ci != 0 {
            return self.build_array_buffer(args);
        }
        if ci == self.sab_ctor && ci != 0 {
            return self.build_shared_array_buffer(args);
        }
        if ci == self.disposablestack_ctor && ci != 0 {
            return Ok(Value::heap(self.alloc_disposable_stack(false)));
        }
        if ci == self.asyncdisposablestack_ctor && ci != 0 {
            return Ok(Value::heap(self.alloc_disposable_stack(true)));
        }
        if ci == self.suppressederror_ctor && ci != 0 {
            return self.build_suppressed_error(args);
        }
        if ci == self.shadowrealm_ctor && ci != 0 {
            let idx = self.heap.alloc(HeapObj::Object(ObjMap::new()));
            if self.shadowrealm_proto != 0 {
                self.proto_of.insert(idx, Value::heap(self.shadowrealm_proto));
            }
            self.shadow_realms.insert(idx);
            return Ok(Value::heap(idx));
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
            let y = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let m = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            let d = self.temporal_ctor_int(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            self.validate_calendar_identifier(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
            return self.make_plain_date(y, m, d);
        }
        if ci == self.plaintime_ctor && ci != 0 {
            let mut f = [0i64; 6];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.temporal_ctor_int(v)?;
                }
            }
            return self.make_plain_time(f);
        }
        if ci == self.plaindatetime_ctor && ci != 0 {
            // year/month/day are required: an undefined coerces to NaN → RangeError.
            // The time fields (i >= 3) default to 0 when undefined.
            let mut f = [0i64; 9];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if i < 3 || v != Value::UNDEFINED {
                    *slot = self.temporal_ctor_int(v)?;
                }
            }
            self.validate_calendar_identifier(args.get(9).copied().unwrap_or(Value::UNDEFINED))?;
            return self.make_plain_date_time(f);
        }
        if ci == self.instant_ctor && ci != 0 {
            let ns = self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            return self.make_instant(ns);
        }
        if ci == self.plainyearmonth_ctor && ci != 0 {
            // (year, month, calendar?, referenceISODay=1)
            let y = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let m = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let rd = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.temporal_ctor_int(v)?,
                _ => 1,
            };
            return self.make_plain_year_month(y, m, rd);
        }
        if ci == self.plainmonthday_ctor && ci != 0 {
            // (month, day, calendar?, referenceISOYear=1972)
            let m = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let d = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let ry = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.temporal_ctor_int(v)?,
                _ => 1972,
            };
            return self.make_plain_month_day(m, d, ry);
        }
        if ci == self.zoneddatetime_ctor && ci != 0 {
            self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
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
                    // The trap's 3rd arg is the REAL newTarget, not the proxy itself.
                    let res = self.call_value(trap, handler, &[target, arr, new_target])?;
                    // ProxyConstruct: the trap result must be an Object.
                    if !self.is_object_value(res) {
                        return Err(Thrown(
                            "TypeError: proxy [[Construct]] must return an object".into(),
                        ));
                    }
                    Ok(res)
                }
                // No trap: forward to the target's [[Construct]], preserving newTarget.
                None => self.construct_with_newtarget(target, args, new_target),
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
                    // `new Array(n)` / `Array(n)` creates n HOLES (absent elements),
                    // not n present `undefined`s.
                    vec![Value::HOLE; n as usize]
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
                // Per spec the entries are added via the `set` adder resolved off the
                // new map — so an overridden `set` (or a subclass's) is honoured.
                let map_v = Value::heap(self.heap.alloc(HeapObj::Map { keys: Vec::new(), vals: Vec::new() }));
                if !a0.is_nullish() {
                    let adder = self.get_member(map_v, "set", map_v)?;
                    if !self.is_callable(adder) {
                        return Err(Thrown("TypeError: Map.prototype.set is not callable".into()));
                    }
                    for e in self.iterate_to_vec(a0)? {
                        let k = self.get_index(e, Value::int(0))?;
                        let v = self.get_index(e, Value::int(1))?;
                        self.call_value(adder, map_v, &[k, v])?;
                    }
                }
                return Ok(map_v);
            }
            if p == self.set_proto && self.set_proto != 0 {
                let set_v = Value::heap(self.heap.alloc(HeapObj::Set(Vec::new())));
                if !a0.is_nullish() {
                    let adder = self.get_member(set_v, "add", set_v)?;
                    if !self.is_callable(adder) {
                        return Err(Thrown("TypeError: Set.prototype.add is not callable".into()));
                    }
                    for e in self.iterate_to_vec(a0)? {
                        self.call_value(adder, set_v, &[e])?;
                    }
                }
                return Ok(set_v);
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
        // A user function with no [[Construct]] (generator, async, arrow, or a
        // concise method) — `new` on it is a TypeError. Gated to Func/Closure so
        // built-in Native ctors, classes, and bound functions are untouched.
        if matches!(self.heap.get(cv.heap_index()), HeapObj::Func(_) | HeapObj::Closure { .. })
            && !self.is_constructor(cv)
        {
            return Err(Thrown("TypeError: function is not a constructor".into()));
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
            // A constructor that returns ANY object (TypedArray/Map/Date/… too, not
            // just a plain object/array) replaces the new instance with it.
            if self.is_object_value(ret) {
                return Ok(ret);
            }
            return Ok(obj);
        }
        // `new (boundFn)(...)`: [[Construct]] forwards to the bound target with the
        // bound arguments prepended (the bound `this` is ignored for construction).
        let bound_parts = match self.heap.get(cv.heap_index()) {
            HeapObj::Bound { target, args: bargs, .. } => Some((*target, bargs.clone())),
            _ => None,
        };
        if let Some((target, bargs)) = bound_parts {
            let combined: Vec<Value> = bargs.into_iter().chain(args.iter().copied()).collect();
            return self.construct(target, &combined);
        }
        let (ctor, ctor_ups, has_explicit, parent) = match self.heap.get(cv.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.ctor_upvalues.clone(), c.has_explicit_ctor, c.parent),
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
                let f = self.ctor_value(fid, &ctor_ups);
                let result = self.call_value(f, obj, args);
                // Capture + clear the super() signal BEFORE propagating any throw,
                // so a constructor that threw never leaves a stale entry (the heap
                // index could later be reused by another instance).
                let super_called = self.super_called.remove(&obj.heap_index());
                let ret = result?;
                // Any object return replaces the new instance.
                if self.is_object_value(ret) {
                    return Ok(ret);
                }
                if parent.is_some() {
                    // A DERIVED class constructor may only return an object or
                    // undefined — any other value throws (a base class silently
                    // ignores a primitive return and yields `this`).
                    if ret != Value::UNDEFINED {
                        return Err(Thrown(
                            "TypeError: Derived constructors may only return object or undefined".into(),
                        ));
                    }
                    // …and `this` must have been initialised by `super(...)`.
                    if !super_called {
                        return Err(Thrown(
                            "ReferenceError: Must call super constructor in derived class before returning from derived constructor".into(),
                        ));
                    }
                }
            }
        } else {
            // No own constructor: run the parent's ctor (implicit `super(...args)`)
            // then this class's field initializers.
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                self.call_value(f, obj, &[])?;
            }
            // Clear any super() mark a nested parent ctor left on this instance.
            self.super_called.remove(&obj.heap_index());
        }
        Ok(obj)
    }

    /// Build the callable for a class constructor: a plain `Func`, or a `Closure`
    /// over the cells the ctor captured (at class-definition time) when it closes
    /// over an enclosing-function local.
    pub(crate) fn ctor_value(&mut self, fid: u32, ups: &[u32]) -> Value {
        if ups.is_empty() {
            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
        } else {
            Value::heap(self.heap.alloc(HeapObj::Closure { func: fid, upvalues: ups.to_vec() }))
        }
    }

    /// `v instanceof F` for a constructor FUNCTION `F`: true iff `F.prototype` is
    /// somewhere in `v`'s prototype chain.
    pub(crate) fn instanceof_via_proto(&mut self, v: Value, ctor: Value) -> bool {
        // A bound function's instanceof uses the [[BoundTargetFunction]]'s
        // prototype (OrdinaryHasInstance step 2) — unwrap the bind chain.
        let mut ctor = ctor;
        for _ in 0..1000 {
            match ctor.is_heap().then(|| self.heap.get(ctor.heap_index())) {
                Some(HeapObj::Bound { target, .. }) => ctor = *target,
                _ => break,
            }
        }
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

    /// OrdinaryHasInstance(C, O) — the algorithm behind both the default
    /// `Function.prototype[Symbol.hasInstance]` method and the `instanceof`
    /// operator's non-overridden path. A bound function resolves to its target; a
    /// throwing `prototype` getter / non-object prototype propagates per spec
    /// (unlike the operator's cached-prototype fast path).
    pub(crate) fn ordinary_has_instance(&mut self, c: Value, v: Value) -> Result<bool, Thrown> {
        if !self.is_callable(c) {
            return Ok(false);
        }
        // A bound function uses its [[BoundTargetFunction]] (recursively).
        if let Some(HeapObj::Bound { target, .. }) =
            c.is_heap().then(|| self.heap.get(c.heap_index()))
        {
            let t = *target;
            return self.ordinary_has_instance(t, v);
        }
        if !self.is_object_value(v) {
            return Ok(false);
        }
        // C.prototype: a non-object DATA reassignment (`f.prototype = undefined`)
        // is recorded in fn_props because the lazy prototype cache only holds
        // object prototypes (get_prop returns that cache), so read such an override
        // directly. An accessor (`defineProperty` get) or an object prototype still
        // goes through get_prop — so a throwing prototype getter propagates.
        let data_override = if c.is_heap() {
            self.fn_props.get(&c.heap_index()).and_then(|m| {
                m.pos("prototype")
                    .and_then(|i| (!m.attrs[i].accessor).then(|| m.vals[i]))
            })
        } else {
            None
        };
        let p = match data_override {
            Some(v) => v,
            None => self.get_prop(c, "prototype")?,
        };
        if !self.is_object_value(p) {
            return Err(Thrown(
                "TypeError: Function has non-object prototype in instanceof check".into(),
            ));
        }
        let mut cur = self.object_get_prototype_of(v);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return Ok(false);
            }
            if cur == p {
                return Ok(true);
            }
            cur = self.object_get_prototype_of(cur);
        }
        Ok(false)
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

    /// The super BASE object for a `super.x` reference inside a method of class
    /// `home_class_id`: GetPrototypeOf(HomeObject), where HomeObject is the class's
    /// own `prototype`. For `class C extends B` this is `B.prototype`; for a BASE
    /// class it is `%Object.prototype%` (so `super.toString()` etc. resolve). Returns
    /// UNDEFINED if unresolvable. (Unlike `super_parent`, this does not require a
    /// parent class, so `super.x` works in base-class methods.)
    pub(crate) fn super_base(&mut self, home_class_id: u32) -> Value {
        let home = match self.class_values.get(home_class_id as usize).copied().flatten() {
            Some(c) => c,
            None => return Value::UNDEFINED,
        };
        let home_proto = match self.prototype_of(home) {
            Some(p) => p,
            None => return Value::UNDEFINED,
        };
        self.object_get_prototype_of(home_proto)
    }

    /// `super.key = v`: PutValue on a super reference. If the super base's prototype
    /// chain exposes a setter for `key`, invoke it with `this` = the receiver;
    /// otherwise create/update an own property on the receiver itself (the spec sets
    /// on the receiver, not the prototype).
    pub(crate) fn super_set(
        &mut self,
        home_class_id: u32,
        key: &str,
        this: Value,
        v: Value,
    ) -> Result<(), Thrown> {
        let proto = self.super_base(home_class_id);
        // MakeSuperPropertyReference: RequireObjectCoercible(GetSuperBase()).
        self.require_object_coercible(proto)?;
        let setter = self.lookup_accessor(proto, key, true);
        if self.is_callable(setter) {
            self.call_value(setter, this, &[v])?;
        } else {
            // `super.x = v` PutValue sets on the receiver. `super` only appears in
            // class methods, which are always strict — so a failed [[Set]] (e.g. a
            // frozen receiver) is a TypeError, not a silent no-op.
            self.set_prop(this, key, v, true)?;
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
        let (ctor, ctor_ups, has_explicit, parent) = match self.heap.get(cval.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.ctor_upvalues.clone(), c.has_explicit_ctor, c.parent),
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
                            // `message` is a non-enumerable own data property.
                            map.define(
                                "message",
                                Value::heap(mi),
                                PropAttr {
                                    writable: true,
                                    enumerable: false,
                                    configurable: true,
                                    accessor: false,
                                    setter: Value::UNDEFINED,
                                },
                            );
                        }
                    }
                }
                return Ok(());
            }
        };
        if has_explicit {
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                self.call_value(f, obj, args)?;
            }
        } else {
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(())
    }

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
                // Collect the source's own ENUMERABLE keys, then Get each — so a
                // getter is invoked and its VALUE is copied (not the accessor
                // function), and a throwing getter propagates, per CopyDataProperties.
                let keys: Vec<String> = match self.heap.get(src.heap_index()) {
                    HeapObj::Object(map) => spec_key_order(&map.keys)
                        .into_iter()
                        .filter(|&i| map.attrs[i].enumerable)
                        .map(|i| map.keys[i].clone())
                        .collect(),
                    HeapObj::Array(items) => (0..items.len()).map(|i| i.to_string()).collect(),
                    _ => Vec::new(),
                };
                let mut pv = Vec::with_capacity(keys.len());
                for k in keys {
                    let v = self.get_prop(src, &k)?;
                    pv.push((k, v));
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
                    let slen = self.heap_char_len(value.heap_index());
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

    pub(crate) fn is_callable(&self, v: Value) -> bool {
        v.is_heap()
            && match self.heap.get(v.heap_index()) {
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
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
                    // A hole is an absent element — not an own property.
                    || key.parse::<usize>().map_or(false, |i| i < items.len() && !items[i].is_hole())
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
            // Functions/closures + the native resolve/reject + combinator element
            // functions: assigned own props (`fn.x`) + the synthesized name/length.
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Native(_)
            | HeapObj::BoundResolver { .. }
            | HeapObj::CombinatorResolver { .. } => {
                self.fn_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
                    || self.callable_has_intrinsic(obj, key)
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
                        HeapObj::Str(s) => Some(s.char_len),
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
        if obj.is_heap() {
            if let Some(desc) = self.proxy_gopd(obj, key)? {
                return Ok(desc != Value::UNDEFINED);
            }
        }
        Ok(self.has_own_property(obj, key))
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
            HeapObj::Class(c) => c.statics.pos(key).map_or(false, |i| c.statics.attrs[i].enumerable),
            // A String wrapper's char indices are own ENUMERABLE props; `length` is
            // non-enumerable; an assigned own prop lives in the arr_props side table.
            HeapObj::Boxed { kind: 0, .. } => {
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
    pub(crate) fn get_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Object(_) => {
                    let m = self.get_prop(v, "@@iterator")?;
                    if self.is_callable(m) {
                        return self.call_value(m, v, &[]);
                    }
                }
                // A plain array: fast-path the default iterator (IterNext walks the
                // array directly), but honour a replaced Array.prototype[@@iterator]
                // by invoking it (so for-of uses the overridden iterator).
                HeapObj::Array(_) => {
                    let m = self.get_prop(v, "@@iterator")?;
                    if m.bits() != self.default_array_iter.bits() {
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
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. } | HeapObj::BigInt(_)) {
            return Err(Thrown("TypeError: value is not iterable".into()));
        }
        let drain = match self.heap.get(v.heap_index()) {
            HeapObj::Generator { .. } => true,
            HeapObj::Object(_) => {
                let it = self.get_prop(v, "@@iterator")?;
                self.is_callable(it)
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
                | HeapObj::Generator { .. }
                | HeapObj::Iterator { .. }
                | HeapObj::IterHelper { .. } => Kind::Iterable,
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
                self.set_index(a, Value::num(i as f64), *v, false)?;
            }
            self.set_prop(a, "length", Value::num(len as f64), false)?;
            return Ok(a);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

}
