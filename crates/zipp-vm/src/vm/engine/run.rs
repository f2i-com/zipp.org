// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// `ZIPP_NO_CALLVALUE_FLAT=1` restores the sequential exotic-receiver cascade
/// `call_value` walked in full before reaching the plain-function tail. See
/// the comment at `call_value`'s fast-path match for why hoisting the
/// Func/Closure test is sound; this exists purely so the change is A/B-able
/// and bisectable on one binary.
#[inline]
fn callvalue_flat_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_CALLVALUE_FLAT").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

impl<'p> Vm<'p> {
    /// Run the top-level function (id 0) to completion.
    pub fn run(&mut self) -> Result<Value, Thrown> {
        self.run_with_prelude(None)
    }

    /// Run the program, optionally evaluating `prelude` as a SEPARATE realm
    /// script first — the test262 harness shape, and the reason this is not just
    /// `eval_script` on the other side.
    ///
    /// The program under test stays the MAIN program: its top-level `var`s
    /// initialize real global slots, its direct/indirect `eval` sees a genuine
    /// script context, and `this` at top level is the global object. Putting the
    /// TEST through the eval-script path instead looks equivalent and is not —
    /// script GlobalDeclarationInstantiation parks eval-script `var`s on the
    /// global object with the slot left UNINITIALIZED, which is observably
    /// different for global-code, identifier-resolution and direct-eval tests
    /// (~250 test262 executions disagree, four of them by crashing).
    ///
    /// So the HARNESS takes the eval-script path: it only has to publish helper
    /// functions into the realm, which that path does correctly. It runs after
    /// `setup_globals` but BEFORE `hoist_functions`, matching real
    /// script-after-script ordering — each harness file is its own complete
    /// script, and the test's own GDI runs last, so a same-named declaration in
    /// the test wins.
    pub fn run_with_prelude(&mut self, prelude: Option<&str>) -> Result<Value, Thrown> {
        // Inject the built-in global objects (Object/Array/Function + their
        // prototypes) into their reserved slots BEFORE hoisting, so a user
        // declaration of the same name shadows the builtin.
        self.setup_globals();
        if let Some(src) = prelude {
            self.eval_prelude_mode = true;
            let r = self.eval_script(src);
            self.eval_prelude_mode = false;
            r?;
        }
        // Materialise function objects for every top-level function into the
        // globals that the compiler reserved for them. The compiler records,
        // per function, the global slot its name binds to (or u32::MAX if it is
        // an anonymous/nested function not hoisted to a global).
        self.hoist_functions();

        let top = self.func(0);
        let base = 0usize;
        let top_regs = top.reg_count as usize;
        self.regs.resize(top_regs, Value::UNDEFINED);
        // A Script's top-level `this` is the global object (a Module's would be
        // undefined). Reg 0 is `this`; seed it with globalThis so sloppy code like
        // `this.x = 1` at the top level targets the global object.
        if self.global_this != 0 {
            self.regs[base] = Value::heap(self.global_this);
        }
        // Reserve register-file capacity up front so JIT self-recursion can
        // append callee windows without reallocating `self.regs` (which would
        // dangle the native code's window pointer). Must happen while regs holds
        // only the top frame so the reservation math is relative to a known base.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.reserve_jit_regs();
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, arg_win: u32::MAX, argc: 0, is_eval: false, func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE, handlers: Vec::new(), new_target: Value::UNDEFINED, callee: Value::UNDEFINED });
        // Everything allocated so far (interned strings, all built-ins, hoisted
        // top-level functions) is pinned: the GC never collects below this floor.
        self.set_gc_floor();
        // Run until the top-level frame returns (frames drains back to 0), then
        // run the event loop: drain queued microtasks (promise reactions, async
        // resumes) to empty. Drains even on a main throw (matches node ordering),
        // then returns the original result.
        let main = self.run_loop(0);
        self.run_event_loop();
        main
    }

    /// Run a MODULE as the program entry. The top-level body (func 0) is an async
    /// activation so top-level `await` works; the event loop then drains to
    /// completion (settling promises, running queued async tests). An uncaught
    /// top-level rejection is surfaced as the program error.
    /// Run an ENTRY module that contains STATIC IMPORTS through the module
    /// loader: dependencies link before the body evaluates. The loader path
    /// is synchronous, so a top-level await in such an entry surfaces the
    /// explicit not-yet-supported TypeError (B15 lifts that).
    pub fn run_module_entry(&mut self, path: &std::path::Path) -> Result<Value, Thrown> {
        // The host (harness) script may have already run on this Vm — do NOT
        // re-setup (re-hoisting would re-materialize host functions, losing
        // properties assigned to them, e.g. assert.sameValue).
        if self.global_this == 0 {
            self.setup_globals();
            self.hoist_functions();
            self.set_gc_floor();
        }
        let r = self.import_module(path, None);
        // An ENTRY whose top-level await suspended finishes through the
        // microtask drain below; if its body promise then REJECTED, that
        // rejection IS the program's error (the entry module failed).
        let body = self.pending_module_body.take();
        self.run_event_loop();
        if let Some(bp) = body {
            if bp.is_heap() {
                let st = match self.heap.get(bp.heap_index()) {
                    HeapObj::Promise { state, result, .. } => Some((*state, *result)),
                    _ => None,
                };
                if let Some((crate::heap::PromiseState::Rejected, reason)) = st {
                    if let HeapObj::Promise { handled, .. } =
                        self.heap.get_mut(bp.heap_index())
                    {
                        *handled = true;
                    }
                    let msg = self.throw_message(reason);
                    self.pending_throw = Some(reason);
                    return Err(Thrown(msg));
                }
            }
        }
        r.map(|_| Value::UNDEFINED)
    }

    /// `import_module` for a STATIC link site: a dependency that SUSPENDED at
    /// top-level await is COLLECTED (the importer defers its own body until
    /// every pending dependency settles — async module evaluation). Bindings
    /// are already linked; the values arrive through the shared live slots.
    pub(crate) fn import_module_sync(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        let r = self.import_module(raw_path, mtype)?;
        if let Some(bp) = self.pending_module_body.take() {
            self.link_pending_deps.push(bp);
        }
        Ok(r)
    }

    /// Execute a module body whose dependencies have all settled, and settle
    /// its capability promise: a fulfilled body refreshes the namespace
    /// snapshot and resolves; a rejected/thrown body rejects; a body that
    /// itself suspends at top-level await is ADOPTED (pass-through reactions).
    pub(crate) fn run_deferred_module(&mut self, cap: u32, st: DeferredModuleExec) {
        let exec = self.execute_eval_program(
            st.base_func,
            Some(Value::UNDEFINED),
            None,
            Value::UNDEFINED,
            None,
            None,
            None,
        );
        match exec {
            Ok(v) => {
                let state = if v.is_heap() {
                    match self.heap.get(v.heap_index()) {
                        HeapObj::Promise { state, result, .. } => Some((*state, *result)),
                        _ => None,
                    }
                } else {
                    None
                };
                match state {
                    Some((crate::heap::PromiseState::Rejected, r)) => {
                        if let HeapObj::Promise { handled, .. } =
                            self.heap.get_mut(v.heap_index())
                        {
                            *handled = true;
                        }
                        self.reject(cap, r);
                    }
                    Some((crate::heap::PromiseState::Pending, _)) => {
                        self.then_internal(
                            v.heap_index(),
                            Value::UNDEFINED,
                            Value::UNDEFINED,
                            Some(cap),
                        );
                    }
                    _ => {
                        self.populate_module_namespace(st.ns_idx, &st.full2);
                        self.resolve(cap, Value::UNDEFINED);
                    }
                }
            }
            Err(Thrown(msg)) => {
                let reason = self
                    .pending_throw
                    .take()
                    .unwrap_or_else(|| self.error_from_thrown(&msg));
                self.reject(cap, reason);
            }
        }
    }

    pub fn run_module(&mut self) -> Result<Value, Thrown> {
        use crate::heap::PromiseState;
        self.setup_globals();
        self.hoist_functions();
        self.set_gc_floor();
        // Module top-level `this` is undefined. alloc_async builds + drives the
        // activation to its first await; drain_microtasks runs it to completion.
        let p = self.alloc_async(0, NO_CLOSURE, Value::UNDEFINED, &[]);
        self.run_event_loop();
        if p.is_heap() {
            if let HeapObj::Promise { state: PromiseState::Rejected, result, .. } =
                self.heap.get(p.heap_index())
            {
                let reason = *result;
                // Render the rejection like an uncaught throw ("Name: message")
                // rather than display() (which gives "[object Object]" for an Error).
                let msg = self.throw_message(reason);
                return Err(Thrown(msg));
            }
        }
        Ok(Value::UNDEFINED)
    }

    /// Invoke a callable `Value` with `this` and `args`, running it to
    /// completion, and return its result. Used by builtin methods that take
    /// callbacks (`map`/`filter`/`reduce`/`sort`). The callee executes on the
    /// explicit frame stack like any other call; we run a nested dispatch loop
    /// that returns when the callee's frame pops back to the current depth.
    ///
    /// Note: this re-enters `run_loop` on the native stack, so deeply *nested
    /// callbacks* use native recursion. Ordinary JS recursion (a function
    /// calling itself) does NOT — it stays on the frame stack. The frame cap
    /// still bounds total depth.
    /// For an arrow callee (`lexical_this` proto), return the `this` it captured
    /// lexically — which replaces any `this` the caller supplied and suppresses
    /// OrdinaryCallBindThis. Returns `this` unchanged for non-arrows. `closure`
    /// is the callee's `Closure` heap index (arrows are always closures) or
    /// `NO_CLOSURE`.
    pub(crate) fn rebind_arrow_this(&self, func_id: u32, closure: u32, this: Value) -> Value {
        if closure != NO_CLOSURE && self.func(func_id as usize).lexical_this {
            if let HeapObj::Closure { this_val, .. } = self.heap.get(closure) {
                return *this_val;
            }
        }
        this
    }

    /// If `callee` is an arrow function value, its lexically-captured `this`; else
    /// `None`. Used by call paths that hold the callee Value rather than its
    /// `(func_id, closure)` pair.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn arrow_captured_this(&self, callee: Value) -> Option<Value> {
        if callee.is_heap() {
            if let HeapObj::Closure { func, this_val, .. } = self.heap.get(callee.heap_index()) {
                if self.func(*func as usize).lexical_this {
                    return Some(*this_val);
                }
            }
        }
        None
    }

    /// Gather `argc` argument registers and hand them to `f` as a slice, without
    /// a heap allocation for the common small call.
    ///
    /// `call_value(&mut self, ..., args: &[Value])` cannot be passed
    /// `&self.regs[base + arg_base..]` directly -- that slice borrows `self`
    /// immutably across a `&mut self` call -- which is why every generic arm
    /// materialised a `Vec` first. A stack buffer sidesteps the borrow for the
    /// same reason `try_builtin_method`'s does, with no `Vm`-owned scratch to be
    /// clobbered by re-entry (`call_value` re-enters the interpreter).
    ///
    /// Rooting is unchanged: a local `Vec` was never a GC root either, and in
    /// every arm the values remain live in the caller's register window.
    #[inline]
    pub(crate) fn with_argv<R>(
        &mut self,
        base: usize,
        arg_base: u16,
        argc: u16,
        f: impl FnOnce(&mut Self, &[Value]) -> R,
    ) -> R {
        const INLINE_ARGS: usize = 8;
        let n = argc as usize;
        if n <= INLINE_ARGS {
            let mut buf = [Value::UNDEFINED; INLINE_ARGS];
            for (i, slot) in buf[..n].iter_mut().enumerate() {
                *slot = self.get(base, arg_base + i as u16);
            }
            f(self, &buf[..n])
        } else {
            let buf: Vec<Value> = (0..argc).map(|i| self.get(base, arg_base + i)).collect();
            f(self, &buf)
        }
    }

    pub(crate) fn call_value(&mut self, callee: Value, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        // The overwhelmingly common callee — a plain JS function or closure —
        // is decided by ONE heap-discriminant load. Every exotic receiver the
        // cascade in `call_value_exotic` distinguishes is keyed either on a
        // heap variant disjoint from `Func`/`Closure` (Proxy / Wrapped / Bound
        // / Native / NativeClosure / BoundResolver / CombinatorResolver /
        // Object), or on a side table whose keys are only ever Object/Native
        // indices (`is_htmldda`, `realm_fns`, `fn_proto`, `intl_ctors`,
        // `realm_ctor_main`) — all pinned under the GC floor or explicitly
        // rooted, so a live Func/Closure index can never collide with one.
        // Hoisting the Func/Closure test above the whole cascade is therefore
        // a pure reorder: no arm that could fire for these variants is
        // skipped. The Rust-side microtask drain (every promise reaction and
        // ThenableJob) enters here where no inliner can help, so the cascade's
        // ~8 discriminant loads + a hash probe per plain callback were pure
        // overhead.
        if callvalue_flat_enabled() && callee.is_heap() {
            match self.heap.get(callee.heap_index()) {
                HeapObj::Func(id) => {
                    let func_id = *id;
                    return self.call_value_plain(callee, this, args, func_id, NO_CLOSURE);
                }
                HeapObj::Closure { func, .. } => {
                    let func_id = *func;
                    let closure = callee.heap_index();
                    return self.call_value_plain(callee, this, args, func_id, closure);
                }
                _ => {}
            }
        }
        self.call_value_exotic(callee, this, args)
    }

    /// The rare-receiver tail of [`call_value`]: the pre-flattening cascade,
    /// arm for arm in its original order (some arms are order-dependent —
    /// `realm_fns` must be probed before the plain `Native` arm because its
    /// keys ARE `Native` allocations). With `ZIPP_NO_CALLVALUE_FLAT=1` every
    /// call routes here, which IS the old path verbatim.
    #[cold]
    #[inline(never)]
    pub(crate) fn call_value_exotic(&mut self, callee: Value, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        // An [[IsHTMLDDA]] exotic (`document.all`) is callable: its [[Call]] returns
        // null when called with NO arguments or a first argument that is the empty
        // String, and undefined otherwise (Annex B).
        if callee.is_heap() && !self.is_htmldda.is_empty() && self.is_htmldda.contains(&callee.heap_index()) {
            let first_is_empty_str = args.first().is_some_and(|a| {
                a.is_heap()
                    && self.heap.is_str_like(a.heap_index())
                    && self.heap.str_cow(a.heap_index()).is_some_and(|s| s.is_empty())
            });
            return Ok(if args.is_empty() || first_is_empty_str {
                Value::NULL
            } else {
                Value::UNDEFINED
            });
        }
        // A callable Proxy: `apply` trap (or call the target).
        if callee.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(callee.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'apply' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "apply")? {
                    Some(trap) => {
                        let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
                        self.call_value(trap, handler, &[target, this, arr])
                    }
                    None => self.call_value(target, this, args),
                };
            }
        }
        // A ShadowRealm WrappedFunction: wrap each argument across the
        // boundary (a non-primitive non-callable argument is a TypeError),
        // call the target with `this` = undefined, wrap the result, and map
        // ANY abrupt target completion to a caller-realm TypeError.
        if callee.is_heap() {
            if let HeapObj::Wrapped { target, .. } = self.heap.get(callee.heap_index()) {
                let t = *target;
                let _gc = self.gc_lock_guard(); // wargs held across allocating calls
                // The wrapper's CALLER realm: a wrapper built by a createRealm
                // child's `evaluate` re-wraps callable results (and throws its
                // boundary TypeErrors) with the CHILD's identities.
                let wr = self.get_function_realm(callee);
                let prev_ncr = self.native_callee_realm;
                let adopt = |vm: &mut Self, msg: &str| {
                    let e = vm.alloc_error_from_message(msg);
                    if wr != 0 {
                        vm.realm_adopt_error_to(e, wr);
                    }
                    vm.pending_throw = Some(e);
                };
                // ARGUMENTS wrap into the TARGET (shadow) realm — main-modeled,
                // so no realm context; boundary TypeErrors still carry the
                // CALLER realm's identity.
                self.native_callee_realm = None;
                let mut wargs = Vec::with_capacity(args.len());
                for &a in args {
                    match self.wrap_realm_value(a) {
                        Ok(w) => wargs.push(w),
                        Err(t) => {
                            self.native_callee_realm = prev_ncr;
                            if self.pending_throw.is_none() {
                                adopt(self, &t.0);
                            }
                            return Err(t);
                        }
                    }
                }
                self.native_callee_realm = prev_ncr;
                // OrdinaryCallEvaluateBody runs in the TARGET's realm: a
                // ShadowRealm-born callable re-enters with ITS realm active so
                // `globalThis.x` in the body binds that realm's slots; an
                // untagged (main-realm) target runs with no realm active.
                let prev_ar = self.active_realm;
                self.active_realm = self.shadow_fn_realm.get(&t.heap_index()).copied();
                let call_res = self.call_value(t, Value::UNDEFINED, &wargs);
                self.active_realm = prev_ar;
                let res = match call_res {
                    Ok(v) => {
                        // The RESULT wraps back into the CALLER realm — the
                        // realm of the wrapper being invoked.
                        self.native_callee_realm = (wr != 0).then_some(wr);
                        let w = self.wrap_realm_value(v);
                        self.native_callee_realm = prev_ncr;
                        match w {
                            Ok(w) => Ok(w),
                            Err(t) => {
                                if self.pending_throw.is_none() {
                                    adopt(self, &t.0);
                                }
                                Err(t)
                            }
                        }
                    }
                    Err(_) => {
                        self.pending_throw.take();
                        let msg =
                            "TypeError: WrappedFunction call threw (error wrapped at the realm boundary)";
                        adopt(self, msg);
                        Err(Thrown(msg.into()))
                    }
                };
                return res;
            }
        }
        // A bound function: invoke its target with the fixed `this` and the bound
        // arguments prepended (handles bind-of-bind by recursing).
        if callee.is_heap() {
            if let HeapObj::Bound { target, this: bthis, args: bargs } = self.heap.get(callee.heap_index()) {
                let (t, th) = (*target, *bthis);
                let mut all = Vec::with_capacity(bargs.len() + args.len());
                all.extend_from_slice(bargs);
                all.extend_from_slice(args);
                return self.call_value(t, th, &all);
            }
            // A createRealm child's `eval` / `evalScript`: run the code against
            // the CHILD realm's global bindings (the active_realm switch, the
            // same machinery ShadowRealm.prototype.evaluate uses).
            if !self.realm_fns.is_empty() {
                if let Some(&(gidx, kind)) = self.realm_fns.get(&callee.heap_index()) {
                    return self.realm_eval_call(gidx, kind, args);
                }
            }
            if let HeapObj::Native(id) = self.heap.get(callee.heap_index()) {
                let id = *id;
                if !self.realm_global_objs.is_empty() {
                    // A realm-COPIED builtin (`other.Function.prototype.apply`,
                    // `other.RegExp.prototype` flag getters, …): run with the
                    // COPY's realm as the native-callee context (HOME-object
                    // checks resolve against the realm's image), and an internal
                    // throw from it carries the CHILD's error-constructor
                    // identity (the spec's realm of the throwing function).
                    let r = self.get_function_realm(callee);
                    let prev = self.native_callee_realm;
                    self.native_callee_realm = (r != 0).then_some(r);
                    let res = self.call_native(id, this, args);
                    self.native_callee_realm = prev;
                    if r != 0 {
                        if let Err(ref t) = res {
                            if self.pending_throw.is_none() {
                                let e = self.alloc_error_from_message(&t.0);
                                self.realm_adopt_error_to(e, r);
                                self.pending_throw = Some(e);
                            }
                        }
                    }
                    return res;
                }
                return self.call_native(id, this, args);
            }
            // A state-carrying builtin (a decorator context's `addInitializer` /
            // `access.get` / …): the captured state is cloned out before the call
            // so the heap is not borrowed across it — the closure body re-enters
            // the VM (it calls user code) and may allocate or GC.
            if let HeapObj::NativeClosure { id, state, .. } = self.heap.get(callee.heap_index()) {
                let (id, state) = (*id, state.clone());
                return self.call_native_closure(id, &state, this, args);
            }
        }
        // A native resolve/reject function settles its bound promise.
        if callee.is_heap() {
            if let HeapObj::BoundResolver { promise, is_reject, pair } = self.heap.get(callee.heap_index()) {
                let (p, isr, pr) = (*promise, *is_reject, *pair);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                // [[AlreadyResolved]]: only the pair's FIRST call acts.
                if self.resolver_pair_fire(pr) {
                    if isr {
                        self.reject(p, arg);
                    } else {
                        self.resolve(p, arg);
                    }
                }
                return Ok(Value::UNDEFINED);
            }
            // A combinator resolve/reject element invoked directly (a custom
            // thenable calling the `then` callback): run the combinator step.
            if let HeapObj::CombinatorResolver { combinator, index, is_reject } =
                self.heap.get(callee.heap_index())
            {
                let (c, i, isr) = (*combinator, *index, *is_reject);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                let kind = if isr { ReactionKind::Reject } else { ReactionKind::Fulfill };
                self.combinator_step(c, i, kind, arg);
                return Ok(Value::UNDEFINED);
            }
        }
        // %Function.prototype% is itself a callable that returns undefined.
        if callee.is_heap() && self.fn_proto != 0 && callee.heap_index() == self.fn_proto {
            return Ok(Value::UNDEFINED);
        }
        // An Intl constructor invoked without `new`: NumberFormat/DateTimeFormat/
        // Collator are spec'd to construct anyway; the other Intl services throw.
        if self.intl_ctors[0] != 0 && callee.is_heap() {
            let ci = callee.heap_index();
            if let Some(kind) = self.intl_ctors.iter().position(|&c| c == ci) {
                if matches!(
                    kind as u8,
                    native::INTL_NUMBERFORMAT | native::INTL_DATETIMEFORMAT | native::INTL_COLLATOR
                ) {
                    let built = self.construct(callee, args)?;
                    // ChainNumberFormat / ChainDateTimeFormat (normative
                    // optional, and only these two have it — Collator does not):
                    // `Intl.DateTimeFormat.call(obj)` returns `obj` carrying the
                    // service under %Intl%.[[FallbackSymbol]].
                    if matches!(kind as u8, native::INTL_NUMBERFORMAT | native::INTL_DATETIMEFORMAT)
                    {
                        return self.intl_chain_legacy(callee, built, this);
                    }
                    return Ok(built);
                }
                return Err(Thrown(
                    "TypeError: Constructor Intl service requires 'new'".into(),
                ));
            }
        }
        // A realm constructor called as a plain function (`other.Symbol('x')`,
        // `other.Array(1, 2)`): route to the MAIN ctor's call behaviour, tagging
        // the result with the realm. `other.Function(src)` compiles `src` with
        // the CHILD realm active so its globals bind in the child's table.
        if callee.is_heap() {
            if let Some(&main) = self.realm_ctor_main.get(&callee.heap_index()) {
                let cr = self.get_function_realm(callee);
                let prev_realm = self.active_realm;
                if self.realm_main_ctor_is_fn_like(main) {
                    if let Some(g) = self.realm_global_obj(cr) {
                        self.active_realm = Some(g);
                    }
                }
                let r = self.call_value(Value::heap(main), this, args);
                self.active_realm = prev_realm;
                // An error raised by the realm's OWN built-in is created with that
                // realm's error constructors (the spec's "the current Realm Record"
                // during the built-in's execution): `g.Function("'use strict'; var
                // yield = 3;")` must throw `g.SyntaxError`, not the main realm's
                // (staging/sm/syntax/yield-as-identifier.js line 22). Only an
                // INTERNAL throw is re-homed — a `pending_throw` already carries a
                // user-thrown object whose realm is whatever created it.
                if cr != 0 {
                    if let Err(ref t) = r {
                        if self.pending_throw.is_none() {
                            let e = self.alloc_error_from_message(&t.0);
                            self.realm_adopt_error_to(e, cr);
                            self.pending_throw = Some(e);
                        }
                    }
                }
                let r = r?;
                if r.is_heap() && cr != 0 {
                    self.obj_realm.insert(r.heap_index(), cr);
                    // `other.Object(primitive)` boxes with the REALM's wrapper
                    // prototype (no-op for a non-Boxed result).
                    self.realm_box_proto(r, cr);
                }
                return Ok(r);
            }
        }
        // A built-in constructor object called as a plain function (passed as a
        // callback, or via .call/.apply). String/Number/Boolean coerce; the rest
        // construct. (Direct `String(x)` calls are compiler-lowered, so this only
        // fires for indirect invocations.)
        if callee.is_heap() {
            if let HeapObj::Object(m) = self.heap.get(callee.heap_index()) {
                if m.is_ctor {
                    return self.call_ctor_as_function(callee, args);
                }
            }
        }
        let (func_id, closure) = self.resolve_callable_realm(callee)?;
        self.call_value_plain(callee, this, args, func_id, closure)
    }

    /// The plain-function tail of [`call_value`]: this-binding, the
    /// generator/async builds, and the frame push + `run_loop` for an ordinary
    /// function. `(func_id, closure)` is what `resolve_callable` reports for
    /// `callee` — the fast path passes it straight off the discriminant match
    /// instead of resolving twice.
    #[inline]
    pub(crate) fn call_value_plain(
        &mut self,
        callee: Value,
        this: Value,
        args: &[Value],
        func_id: u32,
        closure: u32,
    ) -> Result<Value, Thrown> {
        let (is_gen, is_async, is_strict) = {
            let p = self.func(func_id as usize);
            (p.is_generator, p.is_async, p.is_strict)
        };
        // An arrow ignores the supplied `this` and uses the one it captured
        // lexically (and skips OrdinaryCallBindThis). Otherwise OrdinaryCallBindThis:
        // a sloppy (non-strict) function called with a nullish `this` binds the
        // global object instead. Strict functions — and built-ins, which never
        // reach here — receive `this` as passed.
        let this = if closure != NO_CLOSURE && self.func(func_id as usize).lexical_this {
            self.rebind_arrow_this(func_id, closure, this)
        } else if !is_strict && this.is_nullish() && self.global_this != 0 {
            // OrdinaryCallBindThis: the global of the CALLEE's realm — a function
            // born in a $262.createRealm child binds the CHILD's global object.
            Value::heap(self.callee_this_global(callee))
        } else if !is_strict && !self.is_object_value(this) && self.global_this != 0 {
            // OrdinaryCallBindThis: a sloppy function boxes a primitive `this`
            // (number/string/boolean/symbol/bigint) to its wrapper object —
            // with the CALLEE realm's wrapper prototype for a realm function.
            let b = self.to_object(this)?;
            self.realm_retag_boxed(callee, b);
            b
        } else {
            this
        };
        // An `async function*` builds a suspended AsyncGenerator (an async
        // iterator); it doesn't run until `.next()` (but its parameter prologue
        // runs eagerly here, so a destructuring throw propagates from the call).
        if is_gen && is_async {
            self.pending_gen_callee = callee;
            return self.alloc_async_generator(func_id, closure, this, args);
        }
        // Calling a generator function builds a suspended Generator, not a frame.
        // (The parameter prologue runs eagerly here, so a destructuring throw
        // propagates from the call.)
        if is_gen {
            self.pending_gen_callee = callee;
            return self.alloc_generator(func_id, closure, this, args);
        }
        // Calling an async function runs synchronously up to the first `await`,
        // then returns its result Promise.
        if is_async {
            self.pending_gen_callee = callee;
            return Ok(self.alloc_async(func_id, closure, this, args));
        }
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        // Copy the scalar layout fields out so the FuncProto borrow (which now
        // spans the whole `&self` via `func()`) ends before the `self.regs` /
        // `self.heap` mutations below.
        let (callee_regs, callee_params, rest_reg, arguments_reg) = {
            let proto = self.func(func_id as usize);
            ((proto.reg_count as usize).max(1), proto.param_count as usize, proto.rest_reg, proto.arguments_reg)
        };

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this; // reg 0 = this
        let n = args.len().min(callee_params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = args[i];
        }
        // Rest parameter: gather any args beyond the fixed params into an array.
        if let Some(rreg) = rest_reg {
            let extra: Vec<Value> = args.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: ALL actual args (not just the declared params), so a
        // callback invoked here (e.g. an array-method callback that reads
        // `arguments[2]`) sees every argument — matching the direct Call op.
        let mut args_obj = u32::MAX;
        if let Some(areg) = arguments_reg {
            let (is_strict, simple) = {
                let p = self.func(func_id as usize);
                (p.is_strict, p.simple_params)
            };
            // Sloppy + simple params ⇒ MAPPED: aliases the formal registers of
            // the frame about to be pushed (frames.len() is its index).
            let mapinfo =
                (!is_strict && simple).then(|| (self.frames.len(), new_base, callee_params));
            let arr = self.build_arguments_object(args.to_vec(), callee, is_strict, mapinfo);
            self.regs[new_base + areg as usize] = arr;
            if mapinfo.is_some() {
                args_obj = arr.heap_index();
            }
        }

        let stop_depth = self.frames.len();
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        // `execute_eval_program` reaches exactly this push (its script value is a
        // fresh Func/Closure, so every earlier arm of `call_value` falls through),
        // so this is where the one-shot eval-frame marker is consumed.
        let is_eval = std::mem::take(&mut self.pending_eval_frame);
        // The arguments arrived as a slice, not as a caller register window —
        // `f.arguments` falls back to the frame's own `arguments` object / params.
        self.frames.push(Frame { super_done: false, args_obj, eval_scope: u32::MAX, arg_win: u32::MAX, argc: 0, is_eval, func: func_id, base: new_base, ip: 0, ret_dst: 0, closure, handlers: Vec::new(), new_target, callee });
        self.run_loop(stop_depth)
    }

    /// Bind each named top-level function to its reserved global slot as a
    /// heap function object, so `Call` of a global resolves correctly. The
    /// compiler marks function-name globals; here we fill them.
    pub(crate) fn hoist_functions(&mut self) {
        for (id, f) in self.program.functions.iter().enumerate() {
            if let Some(slot) = function_global_slot(f) {
                let v = Value::heap(self.heap.alloc(HeapObj::Func(id as u32)));
                if (slot as usize) < self.globals.len() {
                    self.globals[slot as usize] = v;
                    self.bump_global_gen(slot);
                }
            }
        }
    }

    /// Resolve a global NAME referenced inside an `eval` to a live global slot.
    /// Names already in the compile-time program reuse their slot; genuinely new
    /// names (sloppy `x = 1`, `var x`, hoisted fns, or builtins the program never
    /// named) draw a fresh EVAL_POOL slot, seeded UNINITIALIZED so a read before a
    /// write is a ReferenceError (matching sloppy global-scope semantics).
    /// Get-or-create the live global slot for `name` in ShadowRealm `rid`'s
    /// own binding table (fresh slots start UNINITIALIZED). For a
    /// `$262.createRealm()` child (rid = its global object's heap index) a fresh
    /// slot is SEEDED from the child global object's own property — the facade
    /// intrinsics (`Object`, `TypeError`, the realm's own `eval`/`Function`) and
    /// any value main-realm code put there (`other.x = 1`) — falling back to the
    /// shared main-realm builtin (stage-1: intrinsics without realm identity),
    /// else UNINITIALIZED (a read before any write is a ReferenceError).
    pub(crate) fn realm_global_slot(&mut self, rid: u32, name: &str) -> Result<u32, Thrown> {
        if let Some(&s) = self.realm_globals.get(&rid).and_then(|m| m.get(name)) {
            return Ok(s);
        }
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        if self.eval_global_next >= cap {
            return Err(Thrown(
                "EvalError: too many distinct globals introduced by eval".into(),
            ));
        }
        let seed = if self.realm_global_objs.contains_key(&rid) {
            let own = match self.heap.get(rid) {
                HeapObj::Object(m) => m.get(name),
                _ => None,
            };
            match own {
                Some(v) => v,
                None => match self.builtin_globals.get(name) {
                    Some(&b) => Value::heap(b),
                    None => Value::UNINITIALIZED,
                },
            }
        } else {
            Value::UNINITIALIZED
        };
        let s = self.eval_global_next;
        self.eval_global_next += 1;
        self.globals[s as usize] = seed;
        self.bump_global_gen(s);
        self.realm_globals.entry(rid).or_default().insert(name.to_string(), s);
        Ok(s)
    }

    pub(crate) fn eval_global_slot(&mut self, name: &str) -> Result<u32, Thrown> {
        // Code evaluating inside a ShadowRealm binds NON-BUILTIN names to the
        // realm's OWN slot table — its `var x` never collides with (or sees)
        // the incubating realm's `x`. Builtins stay shared (single-intrinsics
        // model; per-realm intrinsics are a separate feature).
        if let Some(rid) = self.active_realm {
            // A $262.createRealm child binds EVERY name (builtins included) in
            // its own table — `realm_global_slot` seeds fresh slots from the
            // child's global object, so `TypeError` resolves to the CHILD's
            // facade constructor and `var x` lands on the child global.
            if self.realm_global_objs.contains_key(&rid) {
                return self.realm_global_slot(rid, name);
            }
            if !self.builtin_globals.contains_key(name) {
                return self.realm_global_slot(rid, name);
            }
        }
        if let Some(i) = self.program.global_names.iter().position(|n| n == name) {
            return Ok(i as u32);
        }
        if let Some(&s) = self.eval_global_map.get(name) {
            return Ok(s);
        }
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        if self.eval_global_next >= cap {
            return Err(Thrown(
                "EvalError: too many distinct globals introduced by eval".into(),
            ));
        }
        let s = self.eval_global_next;
        self.eval_global_next += 1;
        self.eval_global_map.insert(name.to_string(), s);
        // A builtin the main program never referenced still resolves in eval'd
        // code (`eval("new RangeError()")`, `eval("Object.keys(x)")`): seed the
        // fresh slot with the builtin value rather than the never-declared
        // sentinel. A genuinely-undeclared name stays UNINITIALIZED → ReferenceError.
        self.globals[s as usize] = match self.builtin_globals.get(name) {
            Some(&v) => Value::heap(v),
            None => Value::UNINITIALIZED,
        };
        self.bump_global_gen(s);
        Ok(s)
    }

    /// Parse, compile, and run an `eval` code string (indirect eval — global,
    /// sloppy scope), returning its completion value. ADDITIVE: the broader suite
    /// never reaches this (calling `eval` previously threw ReferenceError), so it
    /// cannot regress non-eval programs. Classes inside eval are supported via the
    /// `eval_classes` runtime class table (class-id operands re-indexed like funcs).
    /// The NAME behind a global slot: a main-program global, or an
    /// EVAL_POOL slot recorded in eval_global_map.
    pub(crate) fn global_slot_name(&self, idx: u32) -> Option<String> {
        self.program
            .global_names
            .get(idx as usize)
            .cloned()
            .or_else(|| {
                self.eval_global_map
                    .iter()
                    .find(|(_, &v)| v == idx)
                    .map(|(k, _)| k.clone())
            })
    }

    /// The global SLOT behind a name — the inverse of `global_slot_name`. Main-program
    /// compile-time names first, then bindings a later script/eval introduced.
    pub(crate) fn global_slot_of_name(&self, name: &str) -> Option<u32> {
        if let Some(i) = self.program.global_names.iter().position(|n| n == name) {
            return Some(i as u32);
        }
        self.eval_global_map.get(name).copied()
    }

    /// The own-property descriptor a SLOT-BACKED global binding PRESENTS as, when the
    /// global object has no real `ObjMap` entry for it yet. `None` when the name is not
    /// a live global binding at all.
    ///
    /// The global object's properties and the global slots are two views of one thing,
    /// and this is the conversion between them. Both consumers need it:
    ///
    /// * `object_get_own_property_descriptor`, to report a binding that lives only in a
    ///   slot;
    /// * `object_define_property`, to MATERIALIZE that binding before applying a partial
    ///   descriptor on top of it. Without the materialization step,
    ///   `Object.defineProperty(globalThis, "foo", {writable: false})` on the live
    ///   binding `foo = 1` created a fresh property with NO value — so `foo` read
    ///   `undefined` where node reads `1`, and the property lost its enumerability
    ///   (staging/sm/Proxy/regress-bug1037770). Latent while `LoadGlobal` still read
    ///   the raw slot; live once it routes through the real own descriptor.
    ///
    /// The three classifications, which is why this is one function and not three:
    /// a SCRIPT-declared `var`/function binding is `{writable, enumerable,
    /// NON-configurable}`; an IMPLICIT global (sloppy assignment to an undeclared name)
    /// is CreateDataProperty — `{writable, enumerable, configurable}`; a BUILT-IN is
    /// `{writable, non-enumerable, configurable}`; and `NaN`/`Infinity`/`undefined` are
    /// frozen.
    pub(crate) fn global_slot_binding_descriptor(
        &self,
        key: &str,
    ) -> Option<(Value, crate::heap::PropAttr)> {
        if self.global_this == 0 {
            return None;
        }
        let v = self.global_by_name(key)?;
        let named = self.program.global_names.iter().position(|n| n == key);
        let script_decl = named.is_some_and(|i| {
            self.program.hoisted_globals.contains(&(i as u32))
                || self.program.decl_globals.contains(&(i as u32))
        });
        let implicit = !script_decl
            && !self.builtin_globals.contains_key(key)
            && named.and_then(|i| self.globals.get(i)).is_some_and(|v| !v.is_uninitialized());
        let attr = |writable, enumerable, configurable| crate::heap::PropAttr {
            writable,
            enumerable,
            configurable,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        Some(match key {
            "NaN" | "Infinity" | "undefined" => (v, attr(false, false, false)),
            _ if script_decl => (v, attr(true, true, false)),
            _ if implicit => (v, attr(true, true, true)),
            _ => (v, attr(true, false, true)),
        })
    }

    /// The CHEAP precondition for `global_real_own_route`: does the global object carry
    /// any real own property at all? One heap load and an `is_empty`. False for every
    /// program that never writes `globalThis.x = …` or `defineProperty(globalThis, …)`,
    /// which keeps the O(#globals) name lookup inside `global_real_own_route` off
    /// `LoadGlobal`'s hot path.
    #[inline]
    pub(crate) fn global_obj_has_own_keys(&self) -> bool {
        self.global_this != 0
            && matches!(self.heap.get(self.global_this), HeapObj::Object(m) if !m.keys.is_empty())
    }

    /// True when the global OBJECT carries a REAL own property (an ObjMap entry,
    /// e.g. from `Object.defineProperty(this, "x", {writable:false})`) behind
    /// global slot `idx`. The slot fast path models every slot-backed binding as
    /// a plain writable data property, so once real attributes exist the write
    /// must route through [[Set]] on the global object instead. Cheap gate: one
    /// heap load, with the name lookup only when the object has any real keys.
    pub(crate) fn global_real_own_route(&self, idx: u32) -> bool {
        if self.global_this == 0 {
            return false;
        }
        let HeapObj::Object(m) = self.heap.get(self.global_this) else {
            return false;
        };
        if m.keys.is_empty() {
            return false;
        }
        let name = self
            .program
            .global_names
            .get(idx as usize)
            .or_else(|| {
                self.eval_global_map
                    .iter()
                    .find(|(_, &v)| v == idx)
                    .map(|(k, _)| k)
            });
        match name {
            Some(n) => m.pos(n).is_some(),
            None => false,
        }
    }

    /// Return global slot `slot` to the never-initialized sentinel — the ONE way a
    /// slot may go backwards — and bump [`Vm::global_route_epoch`] so already-compiled
    /// code stops trusting its compile-time "this slot is live" finding.
    ///
    /// Every writer of `Value::UNINITIALIZED` into `globals` AFTER startup must come
    /// through here. `delete implicitG` (the `DeleteGlobal` op) and
    /// `delete globalThis.implicitG` (`delete_prop`'s global arm) are the two callers;
    /// they used to disagree — only the first cleared the slot at all, so the second
    /// left `read()` answering `5` in BOTH tiers where node throws.
    pub(crate) fn uninitialize_global(&mut self, slot: u32) {
        if let Some(g) = self.globals.get_mut(slot as usize) {
            *g = Value::UNINITIALIZED;
        }
        self.bump_global_gen(slot);
        self.note_global_route_change();
    }

    /// Bump global slot `slot`'s generation. Every NON-BYTECODE Rust write to
    /// `globals[slot]` must call this (all such writers are cold paths); the
    /// baked `slot_guard` generation compare in spliced leaf calls then misses
    /// and the site degrades to the per-call helper — exactly what a baked
    /// bits-guard miss does today. Slots past the table (late `globals.push`
    /// module slots) are never keyable — no-op. Wrapping keeps compares exact.
    #[inline]
    pub(crate) fn bump_global_gen(&mut self, slot: u32) {
        if let Some(g) = self.global_gens.get_mut(slot as usize) {
            *g = g.wrapping_add(1);
        }
    }

    /// Bump the global-route epoch: some global slot no longer matches what compiled
    /// code assumes about it. Saturating rather than wrapping — a wrap could land back
    /// on an epoch a stale memo entry recorded as valid.
    #[inline]
    pub(crate) fn note_global_route_change(&mut self) {
        self.global_route_epoch = self.global_route_epoch.saturating_add(1);
        self.jit_global_route_ok.clear();
    }

    /// A REAL own property has appeared on (or been redefined on) the global object.
    /// If it shadows a live global SLOT, every store to that slot is now an
    /// OrdinarySet through `[[Set]]` — so already-compiled code that emitted a raw
    /// slot write must stop being entered.
    ///
    /// The compile-time gates catch this when the property exists *before* the
    /// compile. This is the after-the-fact half: with a region already compiled,
    /// `Object.defineProperty(globalThis, "x", {set})` left the setter unrun where the
    /// interpreter ran it on every iteration.
    ///
    /// Narrow on purpose — the shadowing test means `globalThis.someNewName = v` in a
    /// loop does not churn the epoch (and so does not repeatedly clear the memo).
    #[inline]
    pub(crate) fn note_global_own_property_change(&mut self, obj_idx: u32, key: &str) {
        if self.global_this == 0 || obj_idx != self.global_this {
            return;
        }
        if self.global_slot_of_name(key).is_some() {
            self.note_global_route_change();
        }
    }

    /// The per-slot predicate that EVERY JIT admission gate and the entry-time
    /// revalidation share, so they can never drift apart — the single source B66 asked
    /// for. A raw `[r12 + slot*8]` access is observationally identical to the
    /// interpreter's Load/StoreGlobal only when both hold:
    ///
    /// * the slot is LIVE. An `UNINITIALIZED` slot is not an empty binding: a script's
    ///   GlobalDeclarationInstantiation parks var/function bindings as own properties
    ///   of the global object and leaves the slot sentinel on purpose, and `delete`
    ///   puts the sentinel back on a binding that must now throw.
    /// * the global object does NOT shadow it with a real own property. Such a
    ///   property IS the binding: a load must run its getter and a store its setter,
    ///   and a raw slot access does neither.
    #[inline]
    pub(crate) fn global_slot_directly_routable(&self, slot: u32) -> bool {
        if self.globals.get(slot as usize).is_none_or(|v| v.is_uninitialized()) {
            return false;
        }
        if !self.global_obj_has_own_keys() || !self.global_real_own_route(slot) {
            return true;
        }
        // A global LEXICAL (`let x` / `const x` at top level) is NOT a property of the
        // global object, so a same-named own entry does not govern its write and the
        // slot stays authoritative. `StoreGlobal`'s interpreter arm makes exactly this
        // exception (`!global_name_is_lexical`); omitting it here would decline
        // compilation for `let Array`-style shadowing, which is merely slower — but
        // the two must agree, because this predicate is what claims to BE the
        // interpreter's rule.
        let name = self.global_slot_name(slot).unwrap_or_default();
        self.global_name_is_lexical(&name)
    }

    /// Entry-time revalidation for Tier A/C: may `func_id`'s compiled body still be
    /// entered, given what has happened to the globals it accesses directly?
    ///
    /// Cost in a normal program is the `!= 0` compare in the caller — this function is
    /// not even called. Once some global HAS gone backwards, the answer is memoized per
    /// `(func_id, epoch)`, so a hot survivor rescans its body once per epoch, not once
    /// per call.
    ///
    /// Returning `false` does NOT evict: the compiled code stays installed and this
    /// call interprets, exactly like the SROA shape guard in `try_run_osr`. If the
    /// binding is later re-created the next epoch bump re-validates it and native
    /// execution resumes on its own.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_globals_still_routable(&mut self, func_id: u32) -> bool {
        let epoch = self.global_route_epoch;
        if let Some(&(seen, ok)) = self.jit_global_route_ok.get(&func_id) {
            if seen == epoch {
                return ok;
            }
        }
        let proto: *const crate::bytecode::FuncProto = self.func(func_id as usize);
        // SAFETY: program and leaked eval FuncProtos are immutable while running.
        let ok = unsafe { &*proto }.code.iter().all(|ins| match *ins {
            Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                self.global_slot_directly_routable(idx)
            }
            Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. } => {
                self.global_slot_directly_routable(idx)
            }
            _ => true,
        });
        self.jit_global_route_ok.insert(func_id, (epoch, ok));
        ok
    }

    pub(crate) fn do_eval(
        &mut self,
        code: &str,
        force_strict: bool,
        force_new_target_ok: bool,
        this_override: Option<Value>,
        // The caller's class context, when the call site is inside a class
        // element: (home class id, static element, derived-class constructor,
        // the class's inner NAME binding is unshadowed here).
        inherit_super: Option<(u32, bool, bool, bool)>,
        ban_arguments: bool,
        direct: bool,
        caller_new_target: Value,
        caller_home_obj: Option<Value>,
        var_env_global: bool,
        param_collisions: Option<Vec<String>>,
        lexical_collisions: Vec<String>,
        // (ordered caller bindings, their cells, the subset that lives in a
        // function ENCLOSING the caller — readable but not the eval's varEnv —
        // and the subset that is a CATCH PARAMETER of the caller: an eval'd
        // `var` of that name still declares into the caller's varEnv (Annex
        // B.3.5) while reads/writes keep resolving to the param cell).
        caller_scope: Option<(Vec<String>, Vec<Value>, Vec<String>, Vec<String>)>,
        eval_scope_idx: Option<u32>,
        exact_src: Option<&[u8]>,
    ) -> Result<Value, Thrown> {
        // One-shot CreateDynamicFunction marker (set by build_function_kind):
        // consumed HERE, at entry, so a parse failure below cannot leak it
        // into an unrelated later eval. Dynamic functions charge their exact
        // assembled source before standalone parameter parsing/allocation, so
        // the marker also suppresses exactly this matching second charge.
        let fn_ctor = std::mem::take(&mut self.pending_fn_ctor_eval);
        // This is the common gate for every runtime compiler entry: direct and
        // indirect eval, Function constructors, ShadowRealm.evaluate, and the
        // embedder's global-context eval helper all arrive here. Charge before
        // parsing so repeated syntax errors cannot spend unaccounted parser
        // work. The sticky recorder status makes the limit terminal even when
        // guest code catches the returned RangeError.
        #[cfg(feature = "instrument")]
        if !fn_ctor {
            self.instrument_dynamic_code_attempt(code.len())
                .map_err(|message| Thrown(message.into()))?;
        }
        // 1. Parse: the true Script goal plus what only the call site knows
        // (caller strictness, new.target / super validity). `import`/`export`
        // are rejected by the parser itself under this goal, so the old
        // explicit statement scan is gone.
        let ast = crate::front::parse_eval(
            code,
            exact_src,
            crate::front::EvalFlags {
                force_strict,
                allow_new_target: force_new_target_ok,
                allow_super: inherit_super.is_some() || caller_home_obj.is_some(),
            },
        )
        .map_err(Thrown)?;
        // A direct eval in a PARAMETER DEFAULT: its sloppy var/function names
        // may not collide with the param-scope bindings (params + implicit
        // `arguments`) — SyntaxError BEFORE anything runs or is declared.
        if param_collisions.is_some() || !lexical_collisions.is_empty() {
            // `ast.strict` folds force_strict and the directive prologue.
            if !ast.strict {
                for n in crate::compile::eval_var_and_fn_names(&ast) {
                    if param_collisions.as_ref().map_or(false, |c| c.iter().any(|c| *c == n)) {
                        return Err(Thrown(format!(
                            "SyntaxError: Identifier '{n}' has already been declared"
                        )));
                    }
                    // EvalDeclarationInstantiation step 5: a var/function name
                    // colliding with a LEXICAL binding between the eval's
                    // lexEnv and its varEnv (the calling function) is a
                    // SyntaxError, before any binding is created.
                    if lexical_collisions.iter().any(|c| *c == n) {
                        return Err(Thrown(format!(
                            "SyntaxError: Identifier '{n}' has already been declared"
                        )));
                    }
                }
            }
        }
        // UsingDeclaration is not allowed at eval top level (eval-code is not
        // a "using"-eligible scope: spec UsingDeclaration static semantics).
        if ast.body.iter().any(|s| {
            matches!(s, crate::parse::ast::Stmt::VarDecl(d)
                if matches!(d.kind,
                    crate::parse::ast::VarKind::Using
                        | crate::parse::ast::VarKind::AwaitUsing))
        }) {
            return Err(Thrown(
                "SyntaxError: using declarations may not appear at eval top level".into(),
            ));
        }
        // A DIRECT eval sees the caller's lexical private scope: the declared
        // NAMES gate the compile-time early error; the brand CHAIN drives the
        // runtime declaring-class resolution inside the eval'd code.
        let (visible, caller_chain) = if direct {
            match self.current_private_brands() {
                Some(ch) => {
                    let mut s = std::collections::HashSet::new();
                    for b in ch {
                        if let Some(names) = self.brand_private_names.get(b) {
                            for (n, _) in names {
                                s.insert(n.clone());
                            }
                        }
                    }
                    (s, Some(ch.clone()))
                }
                None => (std::collections::HashSet::new(), None),
            }
        } else {
            (std::collections::HashSet::new(), None)
        };
        // 2. Compile in eval mode (top-level returns its completion value).
        let eval_prog = match crate::compile::compile_eval(
            &ast,
            code,
            force_strict,
            force_new_target_ok,
            // The caller class's inner NAME comes from its ClassDef, so a class
            // EXPRESSION's name (which has no outer binding at all) is visible
            // to the eval'd code too.
            inherit_super.map(|(cid, stat, derived, name_ok)| crate::compile::EvalClassCtx {
                static_ctx: stat,
                derived_ctor: derived,
                name: Some(self.class_def(cid as usize).name.clone())
                    .filter(|n| name_ok && !n.is_empty()),
            }),
            ban_arguments,
            visible,
            false,
            caller_home_obj.is_some(),
            caller_scope
                .as_ref()
                .map(|(n, _, _, _)| n.clone())
                .unwrap_or_default(),
            caller_scope
                .as_ref()
                .map(|(_, _, o, _)| o.clone())
                .unwrap_or_default(),
            caller_scope
                .as_ref()
                .map(|(_, _, _, cp)| cp.clone())
                .unwrap_or_default(),
            eval_scope_idx.is_some(),
            // CreateDynamicFunction (`new Function` & kin): suppress the
            // "anonymous" wrapper's self-name binding — one-shot, consumed
            // at do_eval entry above.
            fn_ctor,
        ) {
            Ok(p) => p,
            Err(e) => return Err(Thrown(format!("SyntaxError: {e}"))),
        };
        self.run_eval_program(
            eval_prog,
            this_override,
            false,
            inherit_super.map(|(h, _, _, _)| h),
            caller_chain,
            caller_new_target,
            caller_home_obj,
            var_env_global,
            caller_scope.map(|(_, c, _, _)| c),
            eval_scope_idx,
        )
        .map(|(v, _)| v)
    }

    /// The `Array.fromAsync` implementation, as a lazily-compiled JS polyfill
    /// (an async function value). Spec behaviour expressed in JS so it reuses the
    /// engine's `for await`/`await` machinery; compiled once via `do_eval`, then
    /// cached + GC-rooted. Called with `this` = the receiver constructor C; returns
    /// a Promise the top-level microtask drain progresses.
    pub(crate) fn from_async_polyfill(&mut self) -> Result<Value, Thrown> {
        if let Some(f) = self.from_async_fn {
            return Ok(f);
        }
        // Drives iteration manually (it.next()/await) instead of `for await`, so
        // the observable Get/Call sequence matches the proposal exactly: ONE
        // GetMethod per iterator symbol, the async-from-sync VALUE await, and
        // AsyncIteratorClose on exactly the abrupt completions the spec closes on
        // (k-limit, sync-value await, mapfn, define) — never on next() itself.
        //
        // The iterator keys are the ENGINE-INTERNAL well-known-symbol strings
        // ("@@asyncIterator"/"@@iterator" — see `well_known_symbol_key`), NOT
        // `Symbol.asyncIterator`. The spec says %Symbol.asyncIterator%, an
        // intrinsic; reading it off the global `Symbol` binding lets user code
        // redirect it:
        //
        //     globalThis.Symbol = { asyncIterator: Symbol("asyncIterator"), … };
        //     Array.fromAsync(objWithThatFakeKey)   // must NOT call the fake
        //
        // (`built-ins/Array/fromAsync/asyncitems-uses-intrinsic-iterator-symbols`
        // is exactly that test.) This was latent only because a bare `Symbol` in a
        // polyfill used to read the raw global SLOT and so could not see an own
        // property shadowing it on the global object; fixing `LoadGlobal` to route
        // through a real own descriptor made the shadow visible and the bug live.
        //
        // Any other intrinsic a JS-implemented polyfill reads off a global name
        // (`TypeError`, `Object`, `Array`) is shadowable the same way. Not fixed
        // here — only the symbols are test262-visible today — but the pattern is
        // the hazard, not these two lines.
        const SRC: &str = r#"(async function fromAsync(items, mapfn, thisArg) {
  'use strict';
  var C = this;
  if (items === undefined || items === null)
    throw new TypeError('Array.fromAsync requires an array-like or iterable object');
  var mapping = mapfn !== undefined;
  if (mapping && typeof mapfn !== 'function')
    throw new TypeError('Array.fromAsync mapper is not a function');
  var method = items['@@asyncIterator'];
  if (method === undefined || method === null) method = undefined;
  else if (typeof method !== 'function') throw new TypeError('@@asyncIterator is not a function');
  var isSync = false;
  if (method === undefined) {
    var syncMethod = items['@@iterator'];
    if (syncMethod === undefined || syncMethod === null) syncMethod = undefined;
    else if (typeof syncMethod !== 'function') throw new TypeError('@@iterator is not a function');
    if (syncMethod !== undefined) { method = syncMethod; isSync = true; }
  }
  if (method !== undefined) {
    var it = method.call(items);
    if (Object(it) !== it) throw new TypeError('iterator is not an object');
    var A = (typeof C === 'function') ? new C() : [];
    var k = 0;
    var closing = false;
    try {
      for (;;) {
        closing = false;
        if (k >= 9007199254740991) {
          closing = true;
          throw new TypeError('Array.fromAsync result exceeds the maximum length');
        }
        var res = await it.next();
        if (Object(res) !== res) throw new TypeError('iterator result is not an object');
        if (res.done) break;
        var v = res.value;
        closing = true;
        if (isSync) v = await v;
        var mapped = mapping ? await mapfn.call(thisArg, v, k) : v;
        Object.defineProperty(A, k, { value: mapped, writable: true, enumerable: true, configurable: true });
        k = k + 1;
      }
    } catch (e) {
      if (closing) {
        try {
          var ret = it.return;
          if (ret !== undefined && ret !== null) await ret.call(it);
        } catch (_ignored) {}
      }
      throw e;
    }
    A.length = k;
    return A;
  } else {
    var arrayLike = Object(items);
    var ln = +arrayLike.length;
    var len = ln !== ln ? 0 : Math.max(0, Math.min(Math.trunc(ln), 9007199254740991));
    var A = (typeof C === 'function') ? new C(len) : new Array(len);
    var k = 0;
    while (k < len) {
      var kValue = await arrayLike[k];
      var mapped = mapping ? await mapfn.call(thisArg, kValue, k) : kValue;
      Object.defineProperty(A, k, { value: mapped, writable: true, enumerable: true, configurable: true });
      k = k + 1;
    }
    A.length = len;
    return A;
  }
})"#;
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, Vec::new(), None, None, None)?;
        self.from_async_fn = Some(f);
        Ok(f)
    }

    /// `%AsyncIteratorPrototype%[@@asyncDispose]`, as a lazily-compiled JS polyfill
    /// (an async function). Reads `this.return`; if nullish, resolves to undefined;
    /// a present non-callable `return` rejects with a TypeError; otherwise calls it
    /// and awaits the result (so a rejected result rejects), resolving to undefined.
    /// Compiled once via `do_eval`, cached + GC-rooted; called with `this` = the
    /// iterator and returns a Promise.
    pub(crate) fn async_dispose_polyfill(&mut self) -> Result<Value, Thrown> {
        if let Some(f) = self.async_dispose_fn {
            return Ok(f);
        }
        const SRC: &str = r#"(async function() {
  var O = this;
  var ret = O.return;
  if (ret === undefined || ret === null) return undefined;
  if (typeof ret !== 'function')
    throw new TypeError('the iterator [Symbol.iterator] return method is not callable');
  await ret.call(O);
  return undefined;
})"#;
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, Vec::new(), None, None, None)?;
        self.async_dispose_fn = Some(f);
        Ok(f)
    }

    /// GetDisposeMethod(V, ~async-dispose~) step 1.b.iii: when only `@@dispose`
    /// exists, the method the spec stores is NOT the sync method itself but a
    /// wrapper that calls it and resolves with **undefined** — the sync method's
    /// return value is deliberately discarded. Handing the raw sync method to the
    /// async disposal loop instead made `Dispose`'s `Await(result)` await whatever
    /// it returned, so a `[Symbol.dispose]` returning a never-settling promise hung
    /// `await using` and `AsyncDisposableStack.prototype.disposeAsync` forever
    /// (staging/explicit-resource-management/async-disposal-from-sync-method-
    /// returning-a-promise.js).
    ///
    /// Returns the shim BODY, to be used as
    /// `Bound { target: shim, this: syncMethod, args: vec![resource] }`: calling
    /// that thunk runs `syncMethod.call(resource)` and yields undefined.
    pub(crate) fn sync_dispose_shim(&mut self) -> Result<Value, Thrown> {
        if let Some(f) = self.sync_dispose_shim_fn {
            return Ok(f);
        }
        const SRC: &str = r#"(function(O) { "use strict"; this.call(O); })"#;
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, Vec::new(), None, None, None)?;
        self.sync_dispose_shim_fn = Some(f);
        Ok(f)
    }
}
