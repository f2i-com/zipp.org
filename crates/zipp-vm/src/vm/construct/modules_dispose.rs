// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

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
    pub(crate) fn build_function_kind(
        &mut self,
        args: &[Value],
        kind: u8,
    ) -> Result<Value, Thrown> {
        self.preflight_native_iteration_work(args.len() as u64)?;
        let (params, body) = if args.is_empty() {
            (String::new(), String::new())
        } else {
            // CreateDynamicFunction coerces the PARAMETER strings in argument
            // order first, then the body (a throwing toString surfaces in that
            // order — observable). Stream the comma-joined parameter source into
            // one admitted buffer: retaining every converted part and then
            // joining could duplicate an attacker-controlled aggregate before
            // the dynamic-source meter had a chance to reject it.
            let mut params = String::new();
            for (i, a) in args[..args.len() - 1].iter().enumerate() {
                if i != 0 {
                    self.append_guest_string(&mut params, ",")?;
                }
                let part = self.to_js_string(*a)?;
                self.append_guest_string(&mut params, &part)?;
            }
            let body = self.to_js_string(args[args.len() - 1])?;
            (params, body)
        };
        let prefix = match kind {
            1 => "function* ",
            2 => "async function ",
            3 => "async function* ",
            _ => "function ",
        };
        // Account for the COMPLETE source before joining the parameter pieces,
        // parsing them, or allocating the assembled wrapper. In particular, a
        // malformed standalone parameter list must still consume one dynamic
        // compilation attempt; otherwise repeated caught `Function("(")` calls
        // spend parser work while bypassing both lifetime counters.
        const SOURCE_OPEN: &str = "(";
        const NAME_OPEN: &str = "anonymous(";
        const PARAM_BODY_SEP: &str = "\n) {\n";
        const SOURCE_CLOSE: &str = "\n})";
        let params_len = params.len();
        let source_len = Some(params_len).and_then(|params_len| {
            SOURCE_OPEN
                .len()
                .checked_add(prefix.len())
                .and_then(|n| n.checked_add(NAME_OPEN.len()))
                .and_then(|n| n.checked_add(params_len))
                .and_then(|n| n.checked_add(PARAM_BODY_SEP.len()))
                .and_then(|n| n.checked_add(body.len()))
                .and_then(|n| n.checked_add(SOURCE_CLOSE.len()))
        });
        #[cfg(feature = "instrument")]
        self.instrument_dynamic_code_attempt(source_len.unwrap_or(usize::MAX))
            .map_err(|message| Thrown(message.into()))?;
        let source_len = source_len
            .ok_or_else(|| Thrown("RangeError: dynamic code source length overflow".into()))?;

        // CreateDynamicFunction parses the parameter STRING on its own as
        // FormalParameters (whole string consumed) before the assembled
        // source: a `/*`, backtick, or `)` in the params could otherwise
        // swallow the wrapper's own `) {` and slip past the combined parse
        // (invalid-parameter-list.js). Lexer-level messages carry no
        // "SyntaxError:" prefix — add it so the error classifies correctly.
        crate::parse::stmt::parse_standalone_params(&params).map_err(|e| {
            Thrown(if e.msg.starts_with("SyntaxError") {
                e.msg
            } else {
                format!("SyntaxError: {}", e.msg)
            })
        })?;
        // The newline before `)` defends against a `//` comment in the last
        // parameter; the wrapper parens make the body a function EXPRESSION whose
        // value (the function) becomes the eval completion value.
        let mut source = self.guest_string_with_capacity(source_len)?;
        source.push_str(SOURCE_OPEN);
        source.push_str(prefix);
        source.push_str(NAME_OPEN);
        source.push_str(&params);
        source.push_str(PARAM_BODY_SEP);
        source.push_str(&body);
        source.push_str(SOURCE_CLOSE);
        debug_assert_eq!(source.len(), source_len);
        // CreateDynamicFunction builds the function from the separately-parsed
        // params/body, so the "anonymous" name is SetFunctionName only — NO
        // self-name binding (`typeof anonymous` inside is "undefined",
        // constructor-binding.js). Flag the upcoming compile (one-shot). The
        // same flag tells `do_eval` this exact attempt was already charged
        // above; setting it only after standalone parsing means a parse failure
        // cannot leak the exemption into a later, unrelated eval.
        self.pending_fn_ctor_eval = true;
        self.do_eval(
            &source,
            false,
            false,
            None,
            None,
            false,
            false,
            Value::UNDEFINED,
            None,
            false,
            None,
            Vec::new(),
            None,
            None,
            None,
        )
    }

    /// `ShadowRealm.prototype.evaluate` / `.importValue`. NOTE: not truly isolated
    /// (evaluate reuses the shared global eval path), so cross-realm isolation is
    /// not enforced; argument/return-type validation and primitive results are.
    pub(crate) fn shadowrealm_op(
        &mut self,
        op: u16,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        if !(this.is_heap() && self.shadow_realms.contains(&this.heap_index())) {
            return Err(Thrown("TypeError: receiver is not a ShadowRealm".into()));
        }
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match op {
            native::SHADOWREALM_EVALUATE => {
                let is_str = a0.is_heap()
                    && matches!(
                        self.heap.get(a0.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. }
                    );
                if !is_str {
                    return Err(Thrown(
                        "TypeError: ShadowRealm.prototype.evaluate expects a string".into(),
                    ));
                }
                let code = self.display(a0);
                // An error thrown by the evaluated code can't cross the realm
                // boundary, so it surfaces as a TypeError in the calling realm.
                let prev_realm = self.active_realm;
                self.active_realm = Some(this.heap_index());
                let evaled = self.do_eval(
                    &code,
                    false,
                    false,
                    None,
                    None,
                    false,
                    false,
                    Value::UNDEFINED,
                    None,
                    false,
                    None,
                    Vec::new(),
                    None,
                    None,
                    None,
                );
                self.active_realm = prev_realm;
                let result = match evaled {
                    Ok(r) => r,
                    Err(Thrown(msg)) => {
                        // PerformShadowRealmEval step 3: a PARSE failure throws
                        // a SyntaxError in the CALLER realm; a runtime throw
                        // (pending_throw set) wraps as the boundary TypeError.
                        let runtime_throw = self.pending_throw.take().is_some();
                        if !runtime_throw && msg.starts_with("SyntaxError") {
                            return Err(Thrown(msg));
                        }
                        return Err(Thrown(
                            "TypeError: ShadowRealm evaluate threw (error wrapped at the realm boundary)".into(),
                        ));
                    }
                };
                // Only primitives and callables may cross the boundary; a
                // callable crosses as a fresh WrappedFunction exotic.
                if result.is_heap() {
                    if self.is_callable(result) {
                        // Remember the callable's HOME realm: a later call
                        // through the wrapper re-enters with this realm active
                        // (its `globalThis.x` binds the realm slots).
                        self.shadow_fn_realm
                            .insert(result.heap_index(), this.heap_index());
                        return self.wrapped_function_create(result);
                    }
                    if matches!(
                        self.heap.get(result.heap_index()),
                        HeapObj::Str(_)
                            | HeapObj::Cons { .. }
                            | HeapObj::Symbol { .. }
                            | HeapObj::BigInt(_)
                            | HeapObj::BigIntBig(_)
                    ) {
                        return Ok(result);
                    }
                    return Err(Thrown(
                        "TypeError: ShadowRealm evaluate result is not a primitive or callable"
                            .into(),
                    ));
                }
                Ok(result)
            }
            native::SHADOWREALM_IMPORTVALUE => {
                // Steps 3-4 run SYNCHRONOUSLY before any promise is built:
                // ToString(specifier) (a poisoned valueOf throws here), and the
                // exportName must already BE a String (no coercion).
                let spec = self.to_js_string(a0)?;
                let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let name_is_str = a1.is_heap()
                    && matches!(
                        self.heap.get(a1.heap_index()),
                        HeapObj::Str(_) | HeapObj::Cons { .. }
                    );
                if !name_is_str {
                    return Err(Thrown(
                        "TypeError: ShadowRealm.prototype.importValue exportName must be a string"
                            .into(),
                    ));
                }
                // ShadowRealmImportValue: load + evaluate the module through the
                // existing module pipeline (resolved against the running
                // script's directory, evaluated with the realm active), then
                // ExportGetter: the named export must exist; a callable crosses
                // as a WrappedFunction, a primitive as-is, anything else is a
                // TypeError. EVERY failure after argument validation REJECTS
                // the returned promise (never throws synchronously).
                let export_name = self.display(a1);
                let settle: Result<Value, Value> = match self
                    .module_base_dir
                    .as_ref()
                    .map(|d| d.join(&spec))
                {
                    None => {
                        let e = self.alloc_error_from_message(
                            "TypeError: ShadowRealm.prototype.importValue: no module base",
                        );
                        Err(e)
                    }
                    Some(path) => {
                        let prev_realm = self.active_realm;
                        self.active_realm = Some(this.heap_index());
                        let imported = self.import_module(&path, None);
                        self.active_realm = prev_realm;
                        // A TLA module body promise is not awaited here
                        // (boundary: sync-completing modules only).
                        self.pending_module_body = None;
                        match imported {
                            Ok(ns) => {
                                if !self.has_own_property(ns, &export_name) {
                                    let e = self.alloc_error_from_message(&format!(
                                            "TypeError: ShadowRealm.prototype.importValue: no export named '{export_name}'"
                                        ));
                                    Err(e)
                                } else {
                                    match self.get_prop(ns, &export_name) {
                                        Ok(v) if v.is_heap() && self.is_callable(v) => {
                                            self.shadow_fn_realm
                                                .insert(v.heap_index(), this.heap_index());
                                            match self.wrapped_function_create(v) {
                                                Ok(w) => Ok(w),
                                                Err(Thrown(msg)) => {
                                                    let e = match self.pending_throw.take() {
                                                        Some(t) => t,
                                                        None => self.alloc_error_from_message(&msg),
                                                    };
                                                    Err(e)
                                                }
                                            }
                                        }
                                        Ok(v) if self.is_object_value(v) => {
                                            let e = self.alloc_error_from_message(
                                                    "TypeError: ShadowRealm.prototype.importValue: export is not a primitive or callable",
                                                );
                                            Err(e)
                                        }
                                        Ok(v) => Ok(v),
                                        Err(Thrown(msg)) => {
                                            let e = match self.pending_throw.take() {
                                                Some(t) => t,
                                                None => self.alloc_error_from_message(&msg),
                                            };
                                            Err(e)
                                        }
                                    }
                                }
                            }
                            Err(Thrown(_)) => {
                                // The module failed to load/evaluate: the
                                // error cannot cross the boundary — reject
                                // with a caller-realm TypeError.
                                self.pending_throw.take();
                                let e = self.alloc_error_from_message(
                                        "TypeError: ShadowRealm.prototype.importValue: module evaluation failed",
                                    );
                                Err(e)
                            }
                        }
                    }
                };
                let _gc = self.gc_lock_guard(); // settle value held across alloc
                let p = self.alloc_promise();
                match settle {
                    Ok(v) => self.resolve(p, v),
                    Err(e) => self.reject(p, e),
                }
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
            Some(self.to_str_value(message)?)
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
        let idx = self.heap.alloc(HeapObj::Object(Box::new(m)));
        if self.suppressederror_proto != 0 {
            self.proto_of
                .insert(idx, Value::heap(self.suppressederror_proto));
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

    /// Allocate a promise rejected with a fresh TypeError — the async-method
    /// "throw becomes rejection" shape (IfAbruptRejectPromise).
    pub(crate) fn reject_with_type_error(&mut self, msg: &str) -> Value {
        let m = self.alloc_str(msg.to_string());
        let ev = self.make_error(1, Some(m));
        let p = self.alloc_promise();
        self.reject(p, ev);
        Value::heap(p)
    }

    /// Drive an in-flight `disposeAsync` (state keyed by capability promise
    /// `cap`): run disposers LIFO, AWAITING each result via DISPOSE_ASYNC_STEP
    /// reactions; rejections/throws fold into the SuppressedError chain. On
    /// exhaustion the capability settles — rejected with the chain (a single
    /// error as-is), or resolved undefined after an extra Await(undefined)
    /// tick when only nullish resources were recorded.
    pub(crate) fn dispose_async_step(&mut self, cap: u32, rejection: Option<Value>) {
        if let Some(reason) = rejection {
            if let Some(st) = self.dispose_async_state.get_mut(&cap) {
                let prior = st.error_chain.take();
                let folded = match prior {
                    Some(p) => self
                        .build_suppressed_error(&[reason, p, Value::UNDEFINED])
                        .unwrap_or(reason),
                    None => reason,
                };
                if let Some(st) = self.dispose_async_state.get_mut(&cap) {
                    st.error_chain = Some(folded);
                }
            }
        }
        self.dispose_async_drive(cap);
    }

    pub(crate) fn dispose_async_drive(&mut self, cap: u32) {
        // Locals (the popped disposer, error values) cross allocating calls —
        // hold GC for the synchronous stretch (dropped at each suspension).
        let _gc = self.gc_lock_guard();
        loop {
            let Some(st) = self.dispose_async_state.get_mut(&cap) else {
                return;
            };
            match st.remaining.pop() {
                None => {
                    let st = self.dispose_async_state.remove(&cap).unwrap();
                    match st.error_chain {
                        Some(e) => self.reject(cap, e),
                        None if st.needs_await && !st.has_awaited => {
                            // Await(undefined): one reaction-job hop, then
                            // resolve. then_internal(into: cap) chains it.
                            let p = self.alloc_promise();
                            self.resolve(p, Value::UNDEFINED);
                            self.then_internal(p, Value::UNDEFINED, Value::UNDEFINED, Some(cap));
                        }
                        None => self.resolve(cap, Value::UNDEFINED),
                    }
                    return;
                }
                Some(d) if d == Value::NULL => {
                    st.needs_await = true;
                    continue;
                }
                Some(d) => match self.call_value(d, Value::UNDEFINED, &[]) {
                    Err(_) => {
                        // Abrupt Call: NO Await (Dispose step 3a is skipped on
                        // throw) — fold into the chain and continue the loop.
                        let ev = self
                            .pending_throw
                            .take()
                            .unwrap_or_else(|| self.make_error(1, None));
                        let prior = self
                            .dispose_async_state
                            .get_mut(&cap)
                            .and_then(|st| st.error_chain.take());
                        let folded = match prior {
                            Some(p) => self
                                .build_suppressed_error(&[ev, p, Value::UNDEFINED])
                                .unwrap_or(ev),
                            None => ev,
                        };
                        if let Some(st) = self.dispose_async_state.get_mut(&cap) {
                            st.error_chain = Some(folded);
                        }
                        continue;
                    }
                    Ok(r) => {
                        if let Some(st) = self.dispose_async_state.get_mut(&cap) {
                            st.has_awaited = true;
                        }
                        // Await(result): wrap in a promise (thenables adopt),
                        // continue from its settlement.
                        let p = self.alloc_promise();
                        self.resolve(p, r);
                        let step_t = Value::heap(
                            self.heap.alloc(HeapObj::Native(native::DISPOSE_ASYNC_STEP)),
                        );
                        let on_ok = Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: step_t,
                            this: Value::num(cap as f64),
                            args: Vec::new(),
                        }));
                        let rej_t = Value::heap(
                            self.heap
                                .alloc(HeapObj::Native(native::DISPOSE_ASYNC_STEP_REJECT)),
                        );
                        let on_rej = Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: rej_t,
                            this: Value::num(cap as f64),
                            args: Vec::new(),
                        }));
                        self.then_internal(p, on_ok, on_rej, None);
                        return;
                    }
                },
            }
        }
    }

    /// Allocate a fresh `DisposableStack` instance (a plain object linked to
    /// %DisposableStack.prototype%, with an empty, not-yet-disposed disposer stack).
    pub(crate) fn alloc_disposable_stack(&mut self, is_async: bool) -> u32 {
        let idx = self.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
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
    /// Validate import() options per EnumerableOwnPropertyNames: Symbol keys
    /// are SKIPPED, each string key's descriptor is consulted (proxy GOPD trap
    /// fires), and [[Get]] runs ONLY for present ENUMERABLE keys. Returns the
    /// validated `type` attribute, if any.
    pub(crate) fn validate_import_options(&mut self, ov: Value) -> Result<Option<String>, Value> {
        let _gc = self.gc_lock_guard();
        if !self.is_object_value(ov) {
            return Err(self.make_error(1, None)); // TypeError: options not an object
        }
        let mut mtype: Option<String> = None;
        for key in ["with", "assert"] {
            let attrs = match self.get_prop(ov, key) {
                Ok(v) => v,
                Err(_) => {
                    return Err(self
                        .pending_throw
                        .take()
                        .unwrap_or_else(|| self.make_error(1, None)))
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
                    return Err(self
                        .pending_throw
                        .take()
                        .unwrap_or_else(|| self.make_error(1, None)))
                }
            };
            let names = self.array_snapshot(names_v.heap_index());
            for nv in names {
                // EnumerableOwnPropertyNames operates on STRING keys only.
                if nv.is_heap() && matches!(self.heap.get(nv.heap_index()), HeapObj::Symbol { .. })
                {
                    continue;
                }
                let ks = self.display(nv);
                if ks.starts_with("@@") {
                    continue; // engine-encoded symbol key
                }
                // [[GetOwnProperty]] (proxy trap fires): absent or
                // non-enumerable keys are skipped WITHOUT a [[Get]].
                let desc = match self.proxy_gopd(attrs, &ks) {
                    Ok(Some(d)) => d,
                    Ok(None) => self.object_get_own_property_descriptor(attrs, &ks),
                    Err(_) => {
                        return Err(self
                            .pending_throw
                            .take()
                            .unwrap_or_else(|| self.make_error(1, None)))
                    }
                };
                if !self.is_object_value(desc) {
                    continue;
                }
                let en = match self.get_prop(desc, "enumerable") {
                    Ok(v) => v,
                    Err(_) => {
                        return Err(self
                            .pending_throw
                            .take()
                            .unwrap_or_else(|| self.make_error(1, None)))
                    }
                };
                if !self.truthy(en) {
                    continue;
                }
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
                if ks == "type" {
                    mtype = Some(self.display(val));
                }
            }
        }
        Ok(mtype)
    }

    /// Allocate an EMPTY Module Namespace exotic (null proto, no keys yet) and an empty
    /// live-slot map. Used to register a module in the loader cache BEFORE its body
    /// runs (so a self/cyclic `import` returns the same object); the caller then calls
    /// `populate_module_namespace` once the export slots are known. The slot map can be
    /// seeded immediately (own exports) so a self-import observes live bindings during
    /// evaluation.
    pub(crate) fn alloc_empty_namespace(&mut self) -> u32 {
        let m = ObjMap::new();
        let idx = self.heap.alloc(HeapObj::Object(Box::new(m)));
        self.proto_of.insert(idx, Value::NULL);
        self.module_namespaces
            .insert(idx, std::collections::HashMap::new());
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
            *slot = Box::new(m);
        }
        // The whole map (and its vals Vec) was replaced: invalidate any JIT
        // inline cache that captured the old vals pointer.
        self.heap.bump_version(idx);
        self.module_namespaces.insert(idx, slot_map);
    }

    pub(crate) fn disposable_op(
        &mut self,
        op: u16,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
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
            // disposeAsync REJECTS its returned promise on a bad receiver
            // (spec step 2 runs inside the async method); others throw.
            if op == DISPOSABLE_DISPOSE_ASYNC {
                return Ok(self.reject_with_type_error("receiver is not an AsyncDisposableStack"));
            }
            return Err(Thrown(
                "TypeError: receiver is not a DisposableStack".into(),
            ));
        }
        let ti = this.heap_index();
        if let Some(want) = want_async {
            if self.async_stacks.contains(&ti) != want {
                if op == DISPOSABLE_DISPOSE_ASYNC {
                    return Ok(
                        self.reject_with_type_error("receiver is not an AsyncDisposableStack")
                    );
                }
                return Err(Thrown(
                    "TypeError: receiver is the wrong kind of disposable stack".into(),
                ));
            }
        }
        let disposed = self
            .dispose_stacks
            .get(&ti)
            .map(|(_, d)| *d)
            .unwrap_or(true);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Mutating methods reject a disposed stack with a ReferenceError.
        if matches!(
            op,
            DISPOSABLE_USE | DISPOSABLE_ADOPT | DISPOSABLE_DEFER | DISPOSABLE_MOVE
        ) && disposed
        {
            return Err(Thrown(
                "ReferenceError: the DisposableStack has been disposed".into(),
            ));
        }
        match op {
            DISPOSABLE_DISPOSED_GET => Ok(Value::bool(disposed)),
            DISPOSABLE_USE => {
                if a0.is_nullish() {
                    // An ASYNC stack still records the nullish resource: its
                    // disposal contributes one Await(undefined) tick (the
                    // NULL marker calls nothing in dispose_async_drive).
                    if self.async_stacks.contains(&ti) {
                        if let Some((d, _)) = self.dispose_stacks.get_mut(&ti) {
                            d.push(Value::NULL);
                        }
                    }
                    return Ok(a0);
                }
                // Async stacks prefer @@asyncDispose, falling back to @@dispose.
                let is_async = self.async_stacks.contains(&ti);
                let mut method = if is_async {
                    self.get_member(a0, "@@asyncDispose", a0)?
                } else {
                    Value::UNDEFINED
                };
                // An ASYNC stack that fell back to @@dispose stores the spec's
                // result-discarding wrapper, not the sync method — see
                // `sync_dispose_shim`. A SYNC stack never wraps: its
                // `dispose()` ignores the result anyway.
                let mut sync_fallback = false;
                if !self.is_callable(method) {
                    method = self.get_member(a0, "@@dispose", a0)?;
                    sync_fallback = is_async;
                }
                if !self.is_callable(method) {
                    return Err(Thrown(
                        "TypeError: value is not disposable (its [Symbol.dispose] is not a function)".into(),
                    ));
                }
                let disposer = if sync_fallback {
                    let shim = self.sync_dispose_shim()?;
                    Value::heap(self.heap.alloc(HeapObj::Bound {
                        target: shim,
                        this: method,
                        args: vec![a0],
                    }))
                } else {
                    Value::heap(self.heap.alloc(HeapObj::Bound {
                        target: method,
                        this: a0,
                        args: Vec::new(),
                    }))
                };
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
                // DisposeResources: LIFO, each later error wraps the pending
                // completion as SuppressedError{error: new, suppressed: prior}.
                match self.dispose_resource_list(disposers, None)? {
                    Some(v) => {
                        let msg = self.throw_message(v);
                        self.pending_throw = Some(v);
                        Err(Thrown(msg))
                    }
                    None => Ok(Value::UNDEFINED),
                }
            }
            DISPOSABLE_DISPOSE_ASYNC => {
                // Idempotent; returns a Promise. Each disposer's result is
                // AWAITED before the next runs (spec Dispose step 3a), so the
                // loop is a continuation-driven state machine keyed by the
                // capability promise (see dispose_async_drive).
                let already = self
                    .dispose_stacks
                    .get(&ti)
                    .map(|(_, d)| *d)
                    .unwrap_or(true);
                let disposers = if already {
                    Vec::new()
                } else if let Some((d, dd)) = self.dispose_stacks.get_mut(&ti) {
                    *dd = true;
                    std::mem::take(d)
                } else {
                    Vec::new()
                };
                let cap = self.alloc_promise();
                self.dispose_async_state.insert(
                    cap,
                    DisposeAsyncState {
                        remaining: disposers,
                        error_chain: None,
                        has_awaited: false,
                        needs_await: false,
                    },
                );
                self.dispose_async_drive(cap);
                Ok(Value::heap(cap))
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
}
