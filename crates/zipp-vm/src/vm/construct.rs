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
        self.do_eval(&source, false, false, None, None, false)
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
                let result = match self.do_eval(&code, false, false, None, None, false) {
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
        self.error_data.insert(idx); // [[ErrorData]] internal slot
        Ok(Value::heap(idx))
    }

    /// DisposeResources (§9.5.6): run `disposers` in REVERSE (LIFO), merging any
    /// thrown error into the running `completion` — when a completion is already
    /// pending, the new error becomes a `SuppressedError{error: new, suppressed:
    /// prior}`; otherwise the error becomes the completion as-is. `completion` is
    /// `None` for a normal incoming completion or `Some(v)` for a pending throw `v`.
    /// Returns `Ok(None)` (normal) or `Ok(Some(v))` (the merged completion is a
    /// throw of `v`). Each disposer is a `Bound{method, this}` so it is called with
    /// `this` = its resource value and no arguments.
    pub(crate) fn dispose_resource_list(
        &mut self,
        disposers: Vec<Value>,
        mut completion: Option<Value>,
    ) -> Result<Option<Value>, Thrown> {
        // The drained disposer list and the running `completion` are Rust locals
        // (not in a register / map), so a GC during a disposer call could sweep
        // them — suspend GC for the loop (the established hold-Values-across-a-
        // callback pattern).
        let _gc = self.gc_lock_guard();
        for d in disposers.into_iter().rev() {
            if self.call_value(d, Value::UNDEFINED, &[]).is_err() {
                // Thrown carries only a message; recapture the REAL thrown Value.
                let ev = self
                    .pending_throw
                    .take()
                    .unwrap_or_else(|| self.make_error(1, None));
                completion = Some(match completion {
                    Some(prior) => self.build_suppressed_error(&[ev, prior, Value::UNDEFINED])?,
                    None => ev,
                });
            }
        }
        Ok(completion)
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
    /// Build a (snapshot) Module Namespace object for a dynamic `import()`: a
    /// null-prototype, non-extensible object whose own data properties are the
    /// module's exports (each `{ writable:true, enumerable:true, configurable:false }`,
    /// keyed by exported name, sorted), plus `@@toStringTag = "Module"`
    /// ({ writable:false, enumerable:false, configurable:false }). Export VALUES are
    /// read from the module's top-level (eval-global) bindings — a SNAPSHOT (live
    /// bindings + the namespace exotic [[Set]]/[[Delete]] are a later phase).
    /// Validate the (non-undefined) 2nd argument of a dynamic `import(x, options)`
    /// per EvaluateImportCall: `options` must be an Object, and its `with`/`assert`
    /// import-attributes (if present) must each be an Object whose own enumerable
    /// values are all Strings. Returns `Err(reason)` (a value to reject the import
    /// promise with) on any violation or a throwing getter; `Ok(())` if valid.
    pub(crate) fn validate_import_options(&mut self, ov: Value) -> Result<(), Value> {
        let _gc = self.gc_lock_guard();
        if !self.is_object_value(ov) {
            return Err(self.make_error(1, None)); // TypeError: options not an object
        }
        for key in ["with", "assert"] {
            let attrs = match self.get_prop(ov, key) {
                Ok(v) => v,
                Err(_) => {
                    return Err(self.pending_throw.take().unwrap_or_else(|| self.make_error(1, None)))
                }
            };
            if attrs == Value::UNDEFINED {
                continue;
            }
            if !self.is_object_value(attrs) {
                return Err(self.make_error(1, None)); // TypeError: attributes not an object
            }
            let names_v = match self.object_own_property_names(attrs) {
                Ok(v) => v,
                Err(_) => {
                    return Err(self.pending_throw.take().unwrap_or_else(|| self.make_error(1, None)))
                }
            };
            let names = self.array_snapshot(names_v.heap_index());
            for nv in names {
                let ks = self.display(nv);
                let val = match self.get_prop(attrs, &ks) {
                    Ok(v) => v,
                    Err(_) => {
                        return Err(self
                            .pending_throw
                            .take()
                            .unwrap_or_else(|| self.make_error(1, None)))
                    }
                };
                if !(val.is_heap() && self.heap.is_str_like(val.heap_index())) {
                    return Err(self.make_error(1, None)); // TypeError: attribute value not a string
                }
            }
        }
        Ok(())
    }

    /// Build a Module Namespace exotic object from the module's exports, where each
    /// entry pairs the exported name with the LIVE per-module global slot holding the
    /// binding. The ObjMap stores a SNAPSHOT value (for key order, descriptors, and
    /// reflection); the namespace's slot map registered in `module_namespaces` is what
    /// the live `[[Get]]` (get_member) reads, so re-assignments inside the module are
    /// observed through `ns.x` (live bindings).
    pub(crate) fn build_module_namespace(&mut self, exports: &[(String, u32)]) -> Value {
        let idx = self.alloc_empty_namespace();
        self.populate_module_namespace(idx, exports);
        Value::heap(idx)
    }

    /// Allocate an EMPTY Module Namespace exotic (null proto, no keys yet) and an empty
    /// live-slot map. Used to register a module in the loader cache BEFORE its body
    /// runs (so a self/cyclic `import` returns the same object); the caller then calls
    /// `populate_module_namespace` once the export slots are known. The slot map can be
    /// seeded immediately (own exports) so a self-import observes live bindings during
    /// evaluation.
    pub(crate) fn alloc_empty_namespace(&mut self) -> u32 {
        let m = ObjMap::new();
        let idx = self.heap.alloc(HeapObj::Object(m));
        self.proto_of.insert(idx, Value::NULL);
        self.module_namespaces.insert(idx, std::collections::HashMap::new());
        idx
    }

    /// Fill a (possibly pre-registered) Module Namespace exotic `idx` with its exports:
    /// the ObjMap stores a SNAPSHOT value (for key order / descriptors / reflection,
    /// read AFTER the body runs) and `@@toStringTag`, becomes non-extensible; the
    /// `module_namespaces` slot map (read by the live `[[Get]]`) is set to the full
    /// export -> live-slot mapping.
    pub(crate) fn populate_module_namespace(&mut self, idx: u32, exports: &[(String, u32)]) {
        let _gc = self.gc_lock_guard();
        // Resolve each export's value (first export of a name wins), then sort the
        // names (the spec orders namespace keys via Array.prototype.sort default).
        let mut pairs: Vec<(String, Value, u32)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (exported, slot) in exports {
            if !seen.insert(exported.clone()) {
                continue;
            }
            let val = self
                .globals
                .get(*slot as usize)
                .copied()
                .unwrap_or(Value::UNDEFINED);
            pairs.push((exported.clone(), val, *slot));
        }
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let tag = self.alloc_str("Module".to_string());
        let mut m = ObjMap::new();
        let data_attr = PropAttr {
            writable: true,
            enumerable: true,
            configurable: false,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut slot_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for (name, val, slot) in pairs {
            m.define(&name, val, data_attr);
            slot_map.insert(name, slot);
        }
        m.define(
            "@@toStringTag",
            tag,
            PropAttr {
                writable: false,
                enumerable: false,
                configurable: false,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        m.extensible = false;
        if let HeapObj::Object(slot) = self.heap.get_mut(idx) {
            *slot = m;
        }
        self.module_namespaces.insert(idx, slot_map);
    }

    pub(crate) fn disposable_op(&mut self, op: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        use native::*;
        // Normalize an AsyncDisposableStack method to its shared behaviour op, and
        // record the required stack KIND: a sync method (DisposableStack.*) needs a
        // sync stack, an async method (AsyncDisposableStack.*) needs an async stack —
        // a cross-brand call (e.g. DisposableStack.prototype.move on an async stack)
        // is a TypeError. (`disposed` getter is shared → no kind requirement.)
        let (op, want_async): (u16, Option<bool>) = match op {
            ASYNC_DISPOSABLE_USE => (DISPOSABLE_USE, Some(true)),
            ASYNC_DISPOSABLE_ADOPT => (DISPOSABLE_ADOPT, Some(true)),
            ASYNC_DISPOSABLE_DEFER => (DISPOSABLE_DEFER, Some(true)),
            ASYNC_DISPOSABLE_MOVE => (DISPOSABLE_MOVE, Some(true)),
            DISPOSABLE_DISPOSE_ASYNC => (op, Some(true)),
            DISPOSABLE_DISPOSE | DISPOSABLE_USE | DISPOSABLE_ADOPT | DISPOSABLE_DEFER
            | DISPOSABLE_MOVE => (op, Some(false)),
            _ => (op, None),
        };
        if !(this.is_heap() && self.dispose_stacks.contains_key(&this.heap_index())) {
            return Err(Thrown("TypeError: receiver is not a DisposableStack".into()));
        }
        let ti = this.heap_index();
        if let Some(want) = want_async {
            if self.async_stacks.contains(&ti) != want {
                return Err(Thrown(
                    "TypeError: receiver is the wrong kind of disposable stack".into(),
                ));
            }
        }
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

    /// OrdinaryCreateFromConstructor's prototype selection: when `new_target`
    /// differs from the base constructor `cval` (a `Reflect.construct(c, args,
    /// newTarget)` or a derived-class `super()`), the instance's [[Prototype]] is
    /// `Get(new_target, "prototype")` when that is an object, else `default`. For
    /// the common `new C()` case (`new_target == cval`) the default — `cval`'s own
    /// prototype — is used unchanged (no extra Get on the hot path).
    pub(crate) fn newtarget_proto(
        &mut self,
        new_target: Value,
        cval: Value,
        default: Value,
    ) -> Result<Value, Thrown> {
        if new_target.is_heap() && new_target != cval {
            let p = self.get_prop(new_target, "prototype")?;
            if self.is_object_value(p) {
                return Ok(p);
            }
            // Non-object prototype: GetPrototypeFromConstructor falls back to
            // GetFunctionRealm(newTarget)'s intrinsic prototype.
            if default.is_heap() {
                if let Some(rp) = self.realm_proto_fallback(new_target, default.heap_index()) {
                    return Ok(Value::heap(rp));
                }
            }
        }
        Ok(default)
    }

    /// For a cross-realm `new_target` whose `prototype` is not an object, the
    /// realm's copy of `main_proto` (the intrinsic default proto) — else None.
    pub(crate) fn realm_proto_fallback(&self, new_target: Value, main_proto: u32) -> Option<u32> {
        let r = self.get_function_realm(new_target) as usize;
        if r != 0 {
            return self.realms.get(r).and_then(|m| m.get(&main_proto).copied());
        }
        None
    }

    /// `Get(new_target, "prototype")` when it is an object and `new_target`
    /// differs from the base constructor — the [[Prototype]] override a built-in
    /// constructor must apply when built via `Reflect.construct(C, args, newTarget)`
    /// / a derived `super()` / a cross-realm newTarget. For a cross-realm newTarget
    /// with a non-object prototype, falls back to that realm's `%default_proto%`.
    /// `None` for the ordinary `new C()` case (use the built-in's default prototype).
    pub(crate) fn newtarget_proto_override(
        &mut self,
        new_target: Value,
        cv: Value,
        default_proto: u32,
    ) -> Result<Option<Value>, Thrown> {
        if new_target.is_heap() && new_target != cv {
            let p = self.get_prop(new_target, "prototype")?;
            if self.is_object_value(p) {
                return Ok(Some(p));
            }
            if let Some(rp) = self.realm_proto_fallback(new_target, default_proto) {
                return Ok(Some(Value::heap(rp)));
            }
        }
        Ok(None)
    }

    /// Apply a newTarget [[Prototype]] override to a freshly-built built-in
    /// instance (an Array/Object/Map/Date/Boxed/… created by `Reflect.construct`
    /// with a foreign newTarget). A no-op when `over` is `None`.
    pub(crate) fn set_ctor_proto(&mut self, result: Value, over: Option<Value>) -> Value {
        if let Some(p) = over {
            if result.is_heap() {
                self.proto_of.insert(result.heap_index(), p);
            }
        }
        result
    }

    /// [[Construct]](argumentsList, newTarget). `new_target` is threaded to a Proxy
    /// `construct` trap (its 3rd argument), through a trap-less Proxy's forward to
    /// the target, and into the instance's [[Prototype]] via OrdinaryCreateFrom
    /// Constructor (see `newtarget_proto`) for the Func/Class paths.
    pub(crate) fn construct_with_newtarget(
        &mut self,
        cv: Value,
        args: &[Value],
        new_target: Value,
    ) -> Result<Value, Thrown> {
        if !cv.is_heap() {
            return Err(Thrown("TypeError: value is not a constructor".into()));
        }
        // A constructor from a `$262.createRealm()` realm. A realm-TAGGED Proxy
        // (built by `new otherRealm.Proxy(t, h)`, which tags the instance with the
        // realm) is still a Proxy: its [[Construct]] must run the construct trap
        // below, not this generic realm-ctor path — so exclude proxies here.
        let cr = self.get_function_realm(cv);
        if cr != 0 && self.proxy_parts(cv.heap_index()).is_none() {
            // If we know the MAIN-realm constructor it mirrors, build a REAL instance
            // by delegating to it with `cv` as newTarget (so the instance's
            // [[Prototype]] is the realm's `X.prototype`), then tag it with the realm.
            // (`fn.prototype` is now assignable to a non-object, so a real
            // `new other.Function()` works as a settable-prototype newTarget.)
            if let Some(&main) = self.realm_ctor_main.get(&cv.heap_index()) {
                let res = self.construct_with_newtarget(Value::heap(main), args, cv)?;
                if res.is_heap() {
                    self.obj_realm.insert(res.heap_index(), cr);
                }
                return Ok(res);
            }
            // Otherwise a fresh realm-tagged, function-like object (a valid foreign
            // newTarget / GetFunctionRealm subject, e.g. `new other.Function()`).
            let proto_idx = self.heap.alloc(HeapObj::Object(ObjMap::new()));
            let mut m = ObjMap::new();
            m.is_ctor = true;
            m.define("prototype", Value::heap(proto_idx), PropAttr::data());
            let idx = self.heap.alloc(HeapObj::Object(m));
            self.obj_realm.insert(idx, cr);
            self.obj_realm.insert(proto_idx, cr);
            return Ok(Value::heap(idx));
        }
        // A built-in error constructor used as a VALUE (`var E = TypeError; new E()`,
        // `Reflect.construct(RangeError, [msg])`). Mirrors the compile-lowered
        // `new TypeError(msg)` path. AggregateError takes the message as arg[1].
        if let Some(k) = self.error_ctors.iter().position(|&c| c == cv.heap_index()) {
            let over = self.newtarget_proto_override(new_target, cv, self.error_protos[k])?;
            // AggregateError (k==7) takes its message as arg[1] and coerces it with a
            // real ToString (observable / abrupt) before iterating arg[0] into `errors`.
            let e = if k == 7 {
                let msg = match args.get(1).copied() {
                    Some(m) if m != Value::UNDEFINED => {
                        let s = self.to_js_string(m)?;
                        Some(self.alloc_str(s))
                    }
                    _ => None,
                };
                let e = self.make_error(7, msg);
                let errors_arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.install_agg_errors(e, errors_arg)?;
                e
            } else {
                // Coerce `message` with a real ToString FIRST (observable / abrupt):
                // a Symbol message throws TypeError, and a throwing toString /
                // @@toPrimitive propagates — before the error object is allocated.
                let msg = match args.first().copied() {
                    Some(m) if m != Value::UNDEFINED => {
                        let s = self.to_js_string(m)?;
                        Some(self.alloc_str(s))
                    }
                    _ => None,
                };
                self.make_error(k as u8, msg)
            };
            return Ok(self.set_ctor_proto(e, over));
        }
        // ArrayBuffer / DataView / TypedArray constructors used as values.
        let ci = cv.heap_index();
        if ci == self.function_ctor && ci != 0 {
            let over = self.newtarget_proto_override(new_target, cv, self.fn_proto)?;
            let r = self.build_function(args)?;
            return Ok(self.set_ctor_proto(r, over));
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
            // The observable argument coercions run BEFORE newTarget.prototype
            // is read (OrdinaryCreateFromConstructor), and allocation after both.
            let (n, max) = self.validate_array_buffer_args(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.arraybuffer_proto)?;
            if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
            }
            let buf = self.alloc_array_buffer(n);
            if let Some(m) = max {
                self.ab_max.insert(buf, m);
            }
            return Ok(self.set_ctor_proto(Value::heap(buf), over));
        }
        if ci == self.sab_ctor && ci != 0 {
            let (n, max) = self.validate_array_buffer_args(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.sab_proto)?;
            if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
            }
            let buf = self.alloc_array_buffer(n);
            if let Some(m) = max {
                self.ab_max.insert(buf, m);
            }
            self.shared_buffers.insert(buf);
            if self.sab_proto != 0 {
                self.proto_of.insert(buf, Value::heap(self.sab_proto));
            }
            return Ok(self.set_ctor_proto(Value::heap(buf), over));
        }
        if ci == self.disposablestack_ctor && ci != 0 {
            let r = Value::heap(self.alloc_disposable_stack(false));
            let over = self.newtarget_proto_override(new_target, cv, self.disposablestack_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.asyncdisposablestack_ctor && ci != 0 {
            let r = Value::heap(self.alloc_disposable_stack(true));
            let over =
                self.newtarget_proto_override(new_target, cv, self.asyncdisposablestack_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.suppressederror_ctor && ci != 0 {
            let r = self.build_suppressed_error(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.suppressederror_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.weakmap_ctor && ci != 0 {
            let over = self.newtarget_proto_override(new_target, cv, self.weakmap_proto)?;
            let wm = Value::heap(
                self.heap.alloc(HeapObj::WeakMap { keys: Vec::new(), vals: Vec::new() }),
            );
            let wm = self.set_ctor_proto(wm, over);
            let it = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !it.is_nullish() {
                self.add_entries_via_adder(wm, it, true)?;
            }
            return Ok(wm);
        }
        if ci == self.weakset_ctor && ci != 0 {
            let over = self.newtarget_proto_override(new_target, cv, self.weakset_proto)?;
            let ws = Value::heap(self.heap.alloc(HeapObj::WeakSet(Vec::new())));
            let ws = self.set_ctor_proto(ws, over);
            let it = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !it.is_nullish() {
                self.add_entries_via_adder(ws, it, false)?;
            }
            return Ok(ws);
        }
        if ci == self.weakref_ctor && ci != 0 {
            let t = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !self.is_object_value(t) {
                return Err(Thrown("TypeError: WeakRef: target must be an object".into()));
            }
            let over = self.newtarget_proto_override(new_target, cv, self.weakref_proto)?;
            let wr = Value::heap(self.heap.alloc(HeapObj::WeakRef(t)));
            return Ok(self.set_ctor_proto(wr, over));
        }
        if ci == self.finreg_ctor && ci != 0 {
            let cb = args.first().copied().unwrap_or(Value::UNDEFINED);
            if self.type_of(cb) != "function" {
                return Err(Thrown(
                    "TypeError: FinalizationRegistry: cleanup callback must be callable".into(),
                ));
            }
            let over = self.newtarget_proto_override(new_target, cv, self.finreg_proto)?;
            let fr = Value::heap(
                self.heap.alloc(HeapObj::FinalizationRegistry { cleanup: cb, tokens: Vec::new() }),
            );
            return Ok(self.set_ctor_proto(fr, over));
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
            let r = self.build_data_view(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.dataview_proto)?;
            // OrdinaryCreateFromConstructor read newTarget.prototype (a user
            // getter may have detached or shrunk the buffer): re-validate the
            // view per GetViewByteLength before exposing it.
            if let HeapObj::DataView { buffer, byte_offset, byte_length } =
                *self.heap.get(r.heap_index())
            {
                if matches!(self.heap.get(buffer), HeapObj::ArrayBuffer { detached: true, .. }) {
                    return Err(Thrown(
                        "TypeError: Cannot construct a DataView on a detached ArrayBuffer".into(),
                    ));
                }
                let bl = self.array_buffer_len(buffer);
                if byte_offset > bl {
                    return Err(Thrown("RangeError: invalid DataView offset".into()));
                }
                let explicit = matches!(args.get(2), Some(&v) if v != Value::UNDEFINED);
                if explicit && byte_offset + byte_length > bl {
                    return Err(Thrown("RangeError: invalid DataView offset/length".into()));
                }
            }
            return Ok(self.set_ctor_proto(r, over));
        }
        if let Some(k) = self.ta_ctors.iter().position(|&c| c == ci && ci != 0) {
            let r = self.build_typed_array(k as u8, args)?;
            // OrdinaryCreateFromConstructor: a foreign/derived newTarget sets the
            // instance's [[Prototype]] (cross-realm intrinsic fallback when its
            // .prototype is not an object).
            let over = self.newtarget_proto_override(new_target, cv, self.ta_protos[k])?;
            return Ok(self.set_ctor_proto(r, over));
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
            let r = self.build_duration(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.duration_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plaindate_ctor && ci != 0 {
            let y = self.temporal_ctor_int(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let m = self.temporal_ctor_int(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
            let d = self.temporal_ctor_int(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            self.validate_calendar_identifier(args.get(3).copied().unwrap_or(Value::UNDEFINED))?;
            let r = self.make_plain_date(y, m, d)?;
            let over = self.newtarget_proto_override(new_target, cv, self.plaindate_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.plaintime_ctor && ci != 0 {
            let mut f = [0i64; 6];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.temporal_ctor_int(v)?;
                }
            }
            let r = self.make_plain_time(f)?;
            let over = self.newtarget_proto_override(new_target, cv, self.plaintime_proto)?;
            return Ok(self.set_ctor_proto(r, over));
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
            let r = self.make_plain_date_time(f)?;
            let over = self.newtarget_proto_override(new_target, cv, self.plaindatetime_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.instant_ctor && ci != 0 {
            let ns = self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            let r = self.make_instant(ns)?;
            let over = self.newtarget_proto_override(new_target, cv, self.instant_proto)?;
            return Ok(self.set_ctor_proto(r, over));
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
            let r = self.make_plain_year_month(y, m, rd)?;
            let over = self.newtarget_proto_override(new_target, cv, self.plainyearmonth_proto)?;
            return Ok(self.set_ctor_proto(r, over));
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
            let r = self.make_plain_month_day(m, d, ry)?;
            let over = self.newtarget_proto_override(new_target, cv, self.plainmonthday_proto)?;
            return Ok(self.set_ctor_proto(r, over));
        }
        if ci == self.zoneddatetime_ctor && ci != 0 {
            self.validate_calendar_identifier(args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
            let r = self.make_zoned_date_time(args)?;
            let over = self.newtarget_proto_override(new_target, cv, self.zoneddatetime_proto)?;
            return Ok(self.set_ctor_proto(r, over));
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
            // Promise(executor): IsCallable(executor) (step 2) is checked BEFORE
            // OrdinaryCreateFromConstructor reads newTarget.prototype (step 3) — so a
            // non-callable executor throws even when newTarget.prototype would throw.
            if p == self.promise_proto && self.promise_proto != 0 && !self.is_callable(a0) {
                return Err(Thrown(format!(
                    "TypeError: Promise resolver {} is not a function",
                    self.display(a0)
                )));
            }
            // OrdinaryCreateFromConstructor: a foreign newTarget (Reflect.construct /
            // cross-realm / derived super) sets the instance's [[Prototype]] to
            // newTarget.prototype rather than the built-in's default `p`.
            let over = self.newtarget_proto_override(new_target, cv, p)?;
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
                let r = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.obj_proto && self.obj_proto != 0 {
                // `Object(value)` with a non-nullish value ignores newTarget and
                // returns ToObject(value); only `new Object()` / nullish honours it.
                if over.is_some() && a0.is_nullish() {
                    let r = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                    return Ok(self.set_ctor_proto(r, over));
                }
                return self.to_object(a0);
            }
            if p == self.num_proto && self.num_proto != 0 {
                // ToNumber(value) — observable (a user valueOf/toString runs) and
                // abrupt; `to_number` alone returns NaN for a plain object.
                let n = if args.is_empty() { 0.0 } else { self.to_number_coerce(a0)? };
                let r = Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 1, value: Value::num(n) }));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.bool_proto && self.bool_proto != 0 {
                let b = !args.is_empty() && self.truthy(a0);
                let r = Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 2, value: Value::bool(b) }));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.str_proto && self.str_proto != 0 {
                let s = if args.is_empty() { String::new() } else { self.to_js_string(a0)? };
                let sv = self.alloc_str(s);
                let r = Value::heap(self.heap.alloc(HeapObj::Boxed { kind: 0, value: sv }));
                return Ok(self.set_ctor_proto(r, over));
            }
            if p == self.regexp_proto && self.regexp_proto != 0 {
                let r = self.build_regexp(a0, args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                return Ok(self.set_ctor_proto(r, over));
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
                return Ok(self.set_ctor_proto(map_v, over));
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
                return Ok(self.set_ctor_proto(set_v, over));
            }
            if p == self.date_proto && self.date_proto != 0 {
                let ms = self.date_new_ms(args)?;
                let r = Value::heap(self.heap.alloc(HeapObj::Date(ms)));
                return Ok(self.set_ctor_proto(r, over));
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
                return Ok(self.set_ctor_proto(Value::heap(prom), over));
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
            // The instance's [[Prototype]] is newTarget.prototype (OrdinaryCreate
            // FromConstructor); for the common `new F()` case this is F.prototype.
            let default = self.prototype_of(cv).unwrap_or(Value::UNDEFINED);
            let proto = self.newtarget_proto(new_target, cv, default)?;
            let obj = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
            if proto.is_heap() {
                self.proto_of.insert(obj.heap_index(), proto);
            }
            // `new.target` for the constructor body (the next frame entered).
            self.pending_new_target = new_target;
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
            // Bound [[Construct]]: substitute the target for newTarget only when
            // newTarget is the bound function itself; otherwise keep the caller's
            // newTarget so OrdinaryCreateFromConstructor uses its prototype.
            let nt = if new_target == cv { target } else { new_target };
            return self.construct_with_newtarget(target, &combined, nt);
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
        // OrdinaryCreateFromConstructor: a `Reflect.construct(Class, args, NT)` (or
        // any newTarget other than the class) gives the instance NT.prototype as its
        // [[Prototype]], overriding the class-derived default (proto_of is consulted
        // first by object_get_prototype_of / instanceof). `new Class()` is unchanged.
        if new_target.is_heap() && new_target != cv {
            let p = self.get_prop(new_target, "prototype")?;
            if self.is_object_value(p) {
                self.proto_of.insert(obj.heap_index(), p);
            }
        }
        if has_explicit {
            // The explicit constructor runs its own `super(...)`; a ctor that
            // returns an object/array replaces the instance.
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                // The ctor (incl. field initializers) runs in the class body's private
                // scope: give its function value the class's lexical brand chain so
                // `this.#x` + classes defined in field initializers resolve.
                if let Some(brands) = self.method_brand.get(&cv.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                // `new.target` for the class constructor body (the next frame entered).
                self.pending_new_target = new_target;
                let result = self.call_value(f, obj, args);
                // Capture + clear the super() signal BEFORE propagating any throw,
                // so a constructor that threw never leaves a stale entry (the heap
                // index could later be reused by another instance).
                let super_called = self.super_called.remove(&obj.heap_index());
                let ret = result?;
                // Any object return replaces the new instance.
                if self.is_object_value(ret) {
                    // A return-override result receives this class's private brand.
                    self.brand_instance(ret, cv);
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
            // No own constructor: run the parent's ctor (implicit `super(...args)`),
            // threading its PRODUCED `this` (a base ctor's object-return becomes the
            // instance), then this class's field initializers on it.
            let mut inst = obj;
            if let Some(pidx) = parent {
                inst = self.run_class_ctor(Value::heap(pidx), inst, args, new_target)?;
            }
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                if let Some(brands) = self.method_brand.get(&cv.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                self.call_value(f, inst, &[])?;
            }
            // This class's field initializers ran on `inst` — brand it (covers a
            // return-override instance from the parent chain).
            self.brand_instance(inst, cv);
            // Clear any super() mark a nested parent ctor left on this instance.
            self.super_called.remove(&inst.heap_index());
            return Ok(inst);
        }
        Ok(obj)
    }

    /// `super(...)` to a built-in EXOTIC parent (`class X extends Set/Map/…`): re-brand
    /// the plain-Object instance `obj` with the builtin's internal representation so its
    /// prototype methods operate and `instanceof` the builtin holds. The instance's own
    /// (subclass) prototype is captured first and re-recorded in `proto_of` (so subclass
    /// methods/fields still resolve — exotic get_member walks proto_of when present), and
    /// later field initializers write named props into the exotic `arr_props` side table.
    /// Returns `true` when `cval` was a recognised builtin exotic ctor (and obj branded).
    pub(crate) fn brand_builtin_subclass(
        &mut self,
        cval: Value,
        obj: Value,
        args: &[Value],
    ) -> Result<bool, Thrown> {
        let oidx = obj.heap_index();
        // `class S extends Symbol/BigInt`: super() must throw — neither is a
        // constructor ([[Construct]] is absent). Checked FIRST: these ctors have
        // no .prototype mapping, so the pidx lookup below would bail before it.
        if cval.is_heap() {
            let ci = cval.heap_index();
            if ci != 0 && ci == self.symbol_ctor {
                return Err(Thrown("TypeError: Symbol is not a constructor".into()));
            }
            if ci != 0 && ci == self.bigint_ctor {
                return Err(Thrown("TypeError: BigInt is not a constructor".into()));
            }
        }
        // Only re-brand a class instance that is still a plain Object (not already a
        // builtin variant from a deeper super() in the chain).
        if !matches!(self.heap.get(oidx), HeapObj::Object(_)) {
            return Ok(false);
        }
        let pidx = match self.prototype_of(cval) {
            Some(p) if p.is_heap() => p.heap_index(),
            _ => return Ok(false),
        };
        // Capture the subclass prototype before re-branding loses the map.class link.
        let sub_proto = self.object_get_prototype_of(obj);
        // `class T extends Uint8Array` (or any TypedArray kind): build a real typed
        // array through the builtin ctor (handling every arg form — length, array,
        // (buffer, byteOffset, length) on a fixed/resizable buffer) and move it into
        // the instance. The TA references a freshly-constructed ArrayBuffer, which is
        // correct to share. Detected by the parent being a TypedArray constructor.
        if cval.is_heap() && self.ta_ctors.iter().any(|&c| c != 0 && c == cval.heap_index()) {
            let tv = self.construct(cval, args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            // Carry over the length-tracking flag (a `new T(rab[, offset])` view with
            // no explicit length follows the resizable buffer): it lives in a side
            // set keyed by heap index, which the clone above does NOT move.
            if self.ta_tracking.contains(&tvi) {
                self.ta_tracking.insert(oidx);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class B extends ArrayBuffer`: materialize a REAL ArrayBuffer into the
        // instance slot (so byteLength/slice's brand checks pass); the resizable
        // max lives in the ab_max side table, keyed by the instance's heap index.
        if cval.is_heap() && cval.heap_index() == self.arraybuffer_ctor && self.arraybuffer_ctor != 0
        {
            let (n, max) = self.validate_array_buffer_args(args)?;
            if n > super::typedarray::MAX_ARRAY_BUFFER_LEN as usize {
                return Err(Thrown("RangeError: ArrayBuffer length exceeds the maximum".into()));
            }
            *self.heap.get_mut(oidx) = HeapObj::ArrayBuffer { data: vec![0u8; n], detached: false };
            if let Some(m) = max {
                self.ab_max.insert(oidx, m);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class D extends DataView`: build a real DataView through the builtin
        // ctor and move it into the instance (the buffer heap index is shared
        // correctly by the clone; the dv_tracking side-set flag is carried like
        // the TypedArray arm carries ta_tracking).
        if cval.is_heap() && cval.heap_index() == self.dataview_ctor && self.dataview_ctor != 0 {
            let tv = self.build_data_view(args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            if self.dv_tracking.contains(&tvi) {
                self.dv_tracking.insert(oidx);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class B extends Boolean/Number/String/Date/RegExp`: construct() already
        // implements each builtin's argument semantics (truthy boxing, ToNumber,
        // ToString, the Date overloads, pattern/flags); clone the built heap
        // object into the instance — Boxed/Date/RegExp carry no heap-index-keyed
        // side state that matters here.
        if pidx != 0
            && [self.bool_proto, self.num_proto, self.str_proto, self.date_proto, self.regexp_proto]
                .contains(&pidx)
        {
            let tv = self.construct(cval, args)?;
            let cloned = self.heap.get(tv.heap_index()).clone();
            *self.heap.get_mut(oidx) = cloned;
            // Carry any named own props the build recorded (e.g. a RegExp's
            // side-table entries) from the temp object to the instance.
            if let Some(m) = self.arr_props.remove(&tv.heap_index()) {
                self.arr_props.insert(oidx, m);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class F extends Function/GeneratorFunction/Async(Generator)Function`:
        // build the dynamic function through the builtin ctor (construct routes
        // each ctor to its build_function_kind) and move it into the instance,
        // carrying the function-keyed side tables so name/length/prototype and
        // callability follow the instance's heap index.
        if cval.is_heap()
            && [self.function_ctor, self.gen_fn_ctor, self.async_fn_ctor, self.asyncgen_fn_ctor]
                .contains(&cval.heap_index())
            && cval.heap_index() != 0
        {
            let tv = self.construct(cval, args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            if let Some(m) = self.fn_props.remove(&tvi) {
                self.fn_props.insert(oidx, m);
            }
            if let Some(p) = self.prototypes.remove(&tvi) {
                self.prototypes.insert(oidx, p);
            }
            if let Some(v) = self.fn_proto_override.remove(&tvi) {
                self.fn_proto_override.insert(oidx, v);
            }
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        // `class W extends WeakMap/WeakSet`: brand first (so the adder operates
        // on the real variant), then add iterable entries via the instance's
        // adder (honouring a subclass override) — modeled on the Map/Set arms.
        if pidx == self.weakmap_proto && self.weakmap_proto != 0 {
            *self.heap.get_mut(oidx) = HeapObj::WeakMap { keys: Vec::new(), vals: Vec::new() };
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                let adder = self.get_member(obj, "set", obj)?;
                if !self.is_callable(adder) {
                    return Err(Thrown("TypeError: WeakMap.prototype.set is not callable".into()));
                }
                for e in self.iterate_to_vec(a0)? {
                    let k = self.get_index(e, Value::int(0))?;
                    let v = self.get_index(e, Value::int(1))?;
                    self.call_value(adder, obj, &[k, v])?;
                }
            }
            return Ok(true);
        }
        if pidx == self.weakset_proto && self.weakset_proto != 0 {
            *self.heap.get_mut(oidx) = HeapObj::WeakSet(Vec::new());
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                let adder = self.get_member(obj, "add", obj)?;
                if !self.is_callable(adder) {
                    return Err(Thrown("TypeError: WeakSet.prototype.add is not callable".into()));
                }
                for e in self.iterate_to_vec(a0)? {
                    self.call_value(adder, obj, &[e])?;
                }
            }
            return Ok(true);
        }
        if pidx == self.set_proto && self.set_proto != 0 {
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            let mut items: Vec<Value> = Vec::new();
            if !a0.is_nullish() {
                for e in self.iterate_to_vec(a0)? {
                    if !items.iter().any(|v| self.same_value_zero(*v, e)) {
                        items.push(e);
                    }
                }
            }
            *self.heap.get_mut(oidx) = HeapObj::Set(items);
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        if pidx == self.arr_proto && self.arr_proto != 0 {
            // `class A extends Array`: build a fresh array via the existing ctor dispatch
            // (the p==arr_proto path, incl. the single-number `new Array(n)` length form)
            // and clone it into the instance. Array is exotic (length) but a plain
            // Vec<Value> with no back-references, so the clone is safe.
            let tv = self.construct(cval, args)?;
            let cloned = self.heap.get(tv.heap_index()).clone();
            *self.heap.get_mut(oidx) = cloned;
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            return Ok(true);
        }
        if pidx == self.map_proto && self.map_proto != 0 {
            // Brand first so the `set` adder operates on a real Map, then add entries
            // via the adder resolved off the instance (honouring a subclass override).
            *self.heap.get_mut(oidx) = HeapObj::Map { keys: Vec::new(), vals: Vec::new() };
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !a0.is_nullish() {
                let adder = self.get_member(obj, "set", obj)?;
                if !self.is_callable(adder) {
                    return Err(Thrown("TypeError: Map.prototype.set is not callable".into()));
                }
                for e in self.iterate_to_vec(a0)? {
                    let k = self.get_index(e, Value::int(0))?;
                    let v = self.get_index(e, Value::int(1))?;
                    self.call_value(adder, obj, &[k, v])?;
                }
            }
            return Ok(true);
        }
        if pidx == self.promise_proto && self.promise_proto != 0 {
            // `class P extends Promise`: brand the instance AS the promise (its heap
            // index IS the promise), bind resolve/reject to it, and run the executor —
            // so NewPromiseCapability(P) (construct -> super(executor)) yields a branded P.
            let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
            if !self.is_callable(a0) {
                return Err(Thrown(format!(
                    "TypeError: Promise resolver {} is not a function",
                    self.display(a0)
                )));
            }
            *self.heap.get_mut(oidx) = HeapObj::Promise {
                state: PromiseState::Pending,
                result: Value::UNDEFINED,
                fulfill: Vec::new(),
                reject: Vec::new(),
                handled: false,
            };
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            let res =
                Value::heap(self.heap.alloc(HeapObj::BoundResolver { promise: oidx, is_reject: false }));
            let rej =
                Value::heap(self.heap.alloc(HeapObj::BoundResolver { promise: oidx, is_reject: true }));
            if self.call_value(a0, Value::UNDEFINED, &[res, rej]).is_err() {
                let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                self.reject(oidx, reason);
            }
            return Ok(true);
        }
        // Temporal value types (immutable kind+fields, no back-references): the parent
        // ctor is one of the 8 Temporal constructors. Build a fresh instance through the
        // existing ctor dispatch, then clone its representation into the instance —
        // `construct` reuses all the per-kind arg parsing/validation. A ZonedDateTime
        // also carries its time-zone in the `zdt_tz` side table.
        let ci = cval.heap_index();
        let is_temporal_ctor = ci != 0
            && (ci == self.duration_ctor
                || ci == self.plaindate_ctor
                || ci == self.plaintime_ctor
                || ci == self.plaindatetime_ctor
                || ci == self.instant_ctor
                || ci == self.plainyearmonth_ctor
                || ci == self.plainmonthday_ctor
                || ci == self.zoneddatetime_ctor);
        if is_temporal_ctor {
            let tv = self.construct(cval, args)?;
            let tvi = tv.heap_index();
            let cloned = self.heap.get(tvi).clone();
            *self.heap.get_mut(oidx) = cloned;
            if sub_proto.is_heap() {
                self.proto_of.insert(oidx, sub_proto);
            }
            if let Some(tz) = self.zdt_tz.get(&tvi).copied() {
                self.zdt_tz.insert(oidx, tz);
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Build the callable for a class constructor: a plain `Func`, or a `Closure`
    /// over the cells the ctor captured (at class-definition time) when it closes
    /// over an enclosing-function local.
    pub(crate) fn ctor_value(&mut self, fid: u32, ups: &[u32]) -> Value {
        if ups.is_empty() {
            Value::heap(self.heap.alloc(HeapObj::Func(fid)))
        } else {
            Value::heap(self.heap.alloc(HeapObj::Closure { func: fid, upvalues: ups.to_vec(), this_val: Value::UNDEFINED }))
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
        // Walk V's prototype chain via [[GetPrototypeOf]] (proxy-trap-aware): a
        // throwing getPrototypeOf trap propagates per OrdinaryHasInstance step 7.b.
        let mut cur = self.get_prototype_of_checked(v)?;
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return Ok(false);
            }
            if cur == p {
                return Ok(true);
            }
            cur = self.get_prototype_of_checked(cur)?;
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
    pub(crate) fn super_base(&mut self, home_class_id: u32, is_static: bool) -> Value {
        let home = match self.class_values.get(home_class_id as usize).copied().flatten() {
            Some(c) => c,
            None => return Value::UNDEFINED,
        };
        // A STATIC element's HomeObject is the class value itself, so its super base
        // is GetPrototypeOf(class) — the PARENT CLASS for `class C extends B` (or
        // %Function.prototype% for a base class) — letting `super.x` reach inherited
        // STATIC members. An instance element's HomeObject is the class prototype.
        if is_static {
            return match self.super_parent(home_class_id) {
                Some(parent) => parent,
                None if self.fn_proto != 0 => Value::heap(self.fn_proto),
                None => Value::UNDEFINED,
            };
        }
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
        is_static: bool,
    ) -> Result<(), Thrown> {
        let proto = self.super_base(home_class_id, is_static);
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

    /// The super BASE for an OBJECT-method `super.x`: GetPrototypeOf([[HomeObject]]),
    /// where the home object is looked up by the executing closure (`callee`) in
    /// `closure_home`. `UNDEFINED` if there is no home (then the caller's
    /// RequireObjectCoercible throws — shouldn't happen for well-formed object super).
    pub(crate) fn obj_super_base(&mut self, callee: Value) -> Value {
        let home = if callee.is_heap() {
            self.closure_home.get(&callee.heap_index()).copied().unwrap_or(Value::UNDEFINED)
        } else {
            Value::UNDEFINED
        };
        if home.is_heap() {
            self.object_get_prototype_of(home)
        } else {
            Value::UNDEFINED
        }
    }

    /// `super.key = v` for an OBJECT method: like `super_set` but the base prototype
    /// is already resolved from the home object. An inherited setter on the chain is
    /// invoked with `this` = the receiver; otherwise the value is set on the receiver.
    pub(crate) fn super_set_obj(
        &mut self,
        proto: Value,
        key: &str,
        this: Value,
        v: Value,
    ) -> Result<(), Thrown> {
        self.require_object_coercible(proto)?;
        let setter = self.lookup_accessor(proto, key, true);
        if self.is_callable(setter) {
            self.call_value(setter, this, &[v])?;
        } else {
            self.set_prop(this, key, v, true)?;
        }
        Ok(())
    }

    /// Run a class's constructor contribution on an existing instance `obj` —
    /// for `super(...)` and the implicit-super chain. An explicit ctor runs its
    /// own `super`; an implicit one runs the parent chain then its fields.
    pub(crate) fn run_class_ctor(&mut self, cval: Value, obj: Value, args: &[Value], new_target: Value) -> Result<Value, Thrown> {
        if !cval.is_heap() {
            return Ok(obj);
        }
        let (ctor, ctor_ups, has_explicit, parent) = match self.heap.get(cval.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.ctor_upvalues.clone(), c.has_explicit_ctor, c.parent),
            // `super(...)` to a BUILT-IN parent (`class X extends Error`). We model
            // the Error family: set `message` on the instance from the argument
            // (AggregateError takes it as the 2nd arg). The instance's prototype
            // chain already reaches the error prototype (so name/toString/
            // instanceof resolve), so nothing else is needed here.
            _ => {
                // `super(...)` to a BUILT-IN EXOTIC parent (`class X extends Set/…`):
                // brand the plain-Object instance with the builtin's internal
                // representation so its methods work and it is a real instanceof. The
                // instance keeps its own (subclass) prototype, recorded in proto_of.
                if self.brand_builtin_subclass(cval, obj, args)? {
                    return Ok(obj);
                }
                if let Some(k) = self.error_ctors.iter().position(|&c| c == cval.heap_index()) {
                    // `class X extends Error` instance: it has the [[ErrorData]] slot.
                    self.error_data.insert(obj.heap_index());
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
                // `class X extends f` where f is a PLAIN user function: super(...)
                // must actually INVOKE the parent with this = the instance and the
                // subclass new.target (a non-constructor parent is a TypeError; an
                // object return is the return-override instance).
                if matches!(
                    self.heap.get(cval.heap_index()),
                    HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. }
                ) {
                    if !self.is_constructor(cval) {
                        return Err(Thrown(
                            "TypeError: the superclass is not a constructor".into(),
                        ));
                    }
                    self.pending_new_target = new_target;
                    let r = self.call_value(cval, obj, args)?;
                    if self.is_object_value(r) {
                        return Ok(r);
                    }
                }
                return Ok(obj);
            }
        };
        if has_explicit {
            // An explicit ctor produces `this`: its object-return (return-override)
            // becomes the effective instance; a non-object/undefined return keeps obj.
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                if let Some(brands) = self.method_brand.get(&cval.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                // `new.target` propagates unchanged through super() to the parent ctor.
                self.pending_new_target = new_target;
                let r = self.call_value(f, obj, args)?;
                let result = if self.is_object_value(r) { r } else { obj };
                // A return-override result receives this class's private brand.
                self.brand_instance(result, cval);
                return Ok(result);
            }
            Ok(obj)
        } else {
            // Implicit ctor: run the parent chain (threading its produced `this`),
            // then this class's field initializers on it.
            let mut eff = obj;
            if let Some(pidx) = parent {
                eff = self.run_class_ctor(Value::heap(pidx), eff, args, new_target)?;
            }
            if let Some(fid) = ctor {
                let f = self.ctor_value(fid, &ctor_ups);
                if let Some(brands) = self.method_brand.get(&cval.heap_index()).cloned() {
                    if f.is_heap() {
                        self.method_brand.insert(f.heap_index(), brands);
                    }
                }
                self.pending_new_target = new_target;
                let r = self.call_value(f, eff, &[])?;
                if self.is_object_value(r) {
                    eff = r;
                }
            }
            // This class's field initializers ran on `eff` — brand it (covers a
            // return-override instance produced by the parent chain).
            self.brand_instance(eff, cval);
            Ok(eff)
        }
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
        // An [[IsHTMLDDA]] exotic (`document.all`) is callable (returns undefined).
        if v.is_heap() && !self.is_htmldda.is_empty() && self.is_htmldda.contains(&v.heap_index()) {
            return true;
        }
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
                // (typeof is "function"); so is %Function.prototype% itself (a
                // built-in function that accepts any args and returns undefined).
                HeapObj::Object(m) => {
                    m.is_ctor || (self.fn_proto != 0 && v.heap_index() == self.fn_proto)
                }
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
            // A class value: own statics (data + `static get`/`set`) + name/length
            // + the synthesized `prototype` (a class always has one).
            HeapObj::Class(c) => {
                c.statics.pos(key).is_some()
                    || c.static_getters.iter().any(|(n, _)| n == key)
                    || c.static_setters.iter().any(|(n, _)| n == key)
                    || self.callable_has_intrinsic(obj, key)
                    || (key == "prototype" && self.callable_has_prototype(obj))
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
                    || (key == "prototype" && self.callable_has_prototype(obj))
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
                HeapObj::Object(_) => {
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
    pub(crate) fn get_async_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
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
                return Ok(it);
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
                return Ok(it);
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

    pub(crate) fn iterator_close(&mut self, iter: Value) -> Result<(), Thrown> {
        self.iterator_close_inner(iter, true)
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
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(map))))
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
    fn iterator_close_inner(&mut self, iter: Value, strict: bool) -> Result<(), Thrown> {
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
        }
        let plan = if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(items) => Plan::Vals(items.clone()),
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
