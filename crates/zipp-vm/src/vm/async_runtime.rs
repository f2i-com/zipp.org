#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// How a SYNC generator is being resumed (`gen_resume`). Kept separate from the
/// async `Resume` enum so the async drive paths are entirely unaffected.
enum GenResumeMode {
    /// `gen.next(v)` — deliver `v` at the yield point.
    Next(Value),
    /// `gen.throw(e)` — throw `e` at the yield point (into try/catch/finally).
    Throw(Value),
    /// `gen.return(v)` — return `v`, running any `finally` on the way out.
    Return(Value),
}

impl<'p> Vm<'p> {
    /// Calling a `function*` does NOT run its body — it allocates a suspended
    /// Generator whose DETACHED register window holds `this` + the bound args
    /// (incl. a rest array). Resumed later by `generator_method`.
    pub(crate) fn alloc_generator(
        &mut self,
        func_id: u32,
        closure: u32,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        let gen_callee = std::mem::replace(&mut self.pending_gen_callee, Value::UNDEFINED);
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let arguments_reg = proto.arguments_reg;
        // OrdinaryCallBindThis: a SLOPPY (non-arrow) generator/async invoked
        // with a nullish `this` binds the global object; a primitive boxes.
        let this = if !proto.lexical_this
            && !proto.is_strict
            && this.is_nullish()
            && self.global_this != 0
        {
            Value::heap(self.global_this)
        } else if !proto.lexical_this
            && !proto.is_strict
            && !self.is_object_value(this)
            && self.global_this != 0
        {
            self.to_object(this).unwrap_or(this)
        } else {
            this
        };
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        // `arguments` (a generator that references it): an array of ALL actual args,
        // built here at call time like the ordinary frame setup does.
        if let Some(areg) = arguments_reg {
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
            regs[areg as usize] = arr;
        }
        // FunctionDeclarationInstantiation runs eagerly at CALL time: splice the
        // window onto the live register file and run the parameter prologue
        // (defaults + destructuring) up to the `GenStart` body-entry marker. A
        // destructuring throw or a default's side effect therefore happens at the
        // call, per spec — not at the first `.next()`. The generator is then parked
        // AT the marker; the first `.next()` resumes just past it to run the body.
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.extend_from_slice(&regs);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: Vec::new(),
            new_target: Value::UNDEFINED,
            callee: gen_callee,
        });
        let outcome = self.run_loop(stop);
        if let Some((_v, genstart_ip)) = self.pending_yield.take() {
            // Suspended at `GenStart`: the post-prologue window is live at the top.
            let back = self.regs.split_off(new_base);
            let handlers = std::mem::take(&mut self.pending_yield_handlers);
            let esc = std::mem::replace(&mut self.pending_yield_eval_scope, u32::MAX);
            let g = self.heap.alloc(HeapObj::Generator {
                func: func_id,
                closure,
                state: GenState::Suspended(genstart_ip),
                regs: back,
                handlers,
            });
            // OrdinaryCreateFromConstructor: the instance's [[Prototype]] is
            // the callee's `prototype` object (a replaced g.prototype flows
            // through); the HeapObj::Generator fallback stays %GeneratorProto%.
            if let Some(p) = self.prototype_of(gen_callee) {
                self.proto_of.insert(g, p);
            }
            if gen_callee != Value::UNDEFINED {
                // Resumes re-bind this as Frame.callee (LoadCallee identity).
                self.gen_callee.insert(g, gen_callee);
            }
            // A param-prologue direct eval created a dynamic EvalScope on the
            // (now-parked) frame: keep it resolvable across suspensions.
            if esc != u32::MAX {
                self.closure_eval_scope.insert(g, esc);
            }
            return Ok(Value::heap(g));
        }
        // No marker was reached — either the prologue threw (propagate at the call
        // site) or, defensively (a proto with no `GenStart`), the body ran to
        // completion. Either way the window is reclaimed.
        self.regs.truncate(new_base);
        match outcome {
            Ok(_) => Ok(Value::heap(self.heap.alloc(HeapObj::Generator {
                func: func_id,
                closure,
                state: GenState::Completed,
                regs: Vec::new(),
                handlers: Vec::new(),
            }))),
            Err(t) => Err(t),
        }
    }

    /// Resume / query a generator (`gen.next(v)` / `gen.return(v)` / `gen.throw(e)`).
    /// Returns an iterator-result object `{value, done}` (or propagates a throw).
    pub(crate) fn generator_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let (state, fid, closure) = match self.heap.get(idx) {
            HeapObj::Generator { state, func, closure, .. } => (*state, *func, *closure),
            _ => return Ok(None),
        };
        match name {
            "return" => match state {
                // A completed generator just echoes {value, done:true}.
                GenState::Completed => Ok(Some(self.iter_result(arg0, true))),
                GenState::Running => Err(Thrown("TypeError: generator is already running".into())),
                // Resume the suspended body with a RETURN completion so any `finally`
                // spanning the yield runs (and a `finally { yield }` can re-suspend).
                GenState::Suspended(ip) => self.gen_resume(idx, fid, closure, ip, GenResumeMode::Return(arg0)),
            },
            "throw" => match state {
                // Throwing into a completed generator just re-throws at the call site.
                GenState::Completed => Err(Thrown(self.throw_message(arg0))),
                GenState::Running => Err(Thrown("TypeError: generator is already running".into())),
                // Resume the suspended body, injecting the throw at the yield point so
                // an enclosing `try`/`catch` (whose handlers we parked) can catch it.
                GenState::Suspended(ip) => self.gen_resume(idx, fid, closure, ip, GenResumeMode::Throw(arg0)),
            },
            "next" => match state {
                GenState::Completed => Ok(Some(self.iter_result(Value::UNDEFINED, true))),
                GenState::Running => Err(Thrown("TypeError: generator is already running".into())),
                GenState::Suspended(ip) => self.gen_resume(idx, fid, closure, ip, GenResumeMode::Next(arg0)),
            },
            _ => Ok(None),
        }
    }

    /// Splice a suspended generator's saved register window back onto the live
    /// register file, RESTORE its parked `try` handlers, and resume it with one of
    /// three completions at the yield point: `Next(v)` delivers a sent value (from
    /// `gen.next(v)`); `Throw(e)` throws a value in there (`gen.throw(e)`), which
    /// unwinds through the body's try/catch/finally; `Return(v)` injects a return
    /// completion (`gen.return(v)`), running any `finally` on the way out. On a
    /// further yield it re-parks the window + handlers; on completion or an
    /// uncaught throw it clears them. Returns the iterator-result `{value,done}`.
    fn gen_resume(
        &mut self,
        idx: u32,
        fid: u32,
        closure: u32,
        resume_ip: usize,
        input: GenResumeMode,
    ) -> Result<Option<Value>, Thrown> {
        let (saved, saved_handlers) = match self.heap.get_mut(idx) {
            HeapObj::Generator { state, regs, handlers, .. } => {
                *state = GenState::Running;
                (std::mem::take(regs), std::mem::take(handlers))
            }
            _ => return Ok(None),
        };
        let reg_count = saved.len();
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            if let HeapObj::Generator { state, regs, handlers, .. } = self.heap.get_mut(idx) {
                *state = GenState::Suspended(resume_ip);
                *regs = saved;
                *handlers = saved_handlers;
            }
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
            func: fid,
            base: new_base,
            ip: 0, // set below per resume kind
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
            new_target: Value::UNDEFINED,
            // The creating call's function value (named fn-expression
            // self-name identity survives suspension).
            callee: self.gen_callee.get(&idx).copied().unwrap_or(Value::UNDEFINED),
        });
        // Restore the activation's dynamic EvalScope (created by a direct eval
        // in the param prologue or an earlier resume), parked on the generator.
        if let Some(&sc) = self.closure_eval_scope.get(&idx) {
            self.frames[stop].eval_scope = sc;
        }
        // A `yield*` suspension point (YieldDelegate) consumes ALL three resume
        // modes by DELIVERING (mode-code, value) into the loop's registers — the
        // loop then forwards next/throw/return to the inner iterator. This must NOT
        // inject a body throw/finally (that's a plain Yield's behaviour).
        let resumes_delegate = matches!(
            self.func(fid as usize).code.get(resume_ip),
            Some(Instr::YieldDelegate { .. })
        );
        let outcome = if resumes_delegate {
            if let Instr::YieldDelegate { mode_dst, val_dst, .. } =
                self.func(fid as usize).code[resume_ip]
            {
                let (m, v) = match input {
                    GenResumeMode::Next(v) => (0, v),
                    GenResumeMode::Throw(v) => (1, v),
                    GenResumeMode::Return(v) => (2, v),
                };
                self.regs[new_base + mode_dst as usize] = Value::int(m);
                self.regs[new_base + val_dst as usize] = v;
            }
            self.frames[stop].ip = resume_ip + 1;
            self.run_loop(stop)
        } else {
            match input {
            GenResumeMode::Next(v) => {
                // First next() runs from ip 0 (legacy proto with no marker); a later
                // one resumes just past the Yield/GenStart, delivering the sent value
                // into a Yield's dst (GenStart delivers nothing).
                let ip = if resume_ip == usize::MAX {
                    0
                } else {
                    if let Instr::Yield { dst, .. } = self.func(fid as usize).code[resume_ip] {
                        self.regs[new_base + dst as usize] = v;
                    }
                    resume_ip + 1
                };
                self.frames[stop].ip = ip;
                self.run_loop(stop)
            }
            GenResumeMode::Throw(e) => {
                // Throw at the suspension point: unwind into the body's handlers.
                self.pending_throw = Some(e);
                if self.unwind_to_handler(e, stop) {
                    self.pending_throw = None;
                    self.run_loop(stop)
                } else {
                    // Uncaught in the body: unwind_to_handler already popped the frame
                    // and truncated its window. Complete the generator and surface the
                    // throw at the `gen.throw(e)` call site (value via pending_throw).
                    if let HeapObj::Generator { state, regs, handlers, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Completed;
                        regs.clear();
                        handlers.clear();
                    }
                    return Err(Thrown(self.throw_message(e)));
                }
            }
            GenResumeMode::Return(v) => {
                // Return completion at the suspension point: run any `finally` that
                // spans the yield (route_through_finally, kind 1=return), exactly as
                // a `return` statement in the body would. EndFinally then completes
                // the return (or an outer finally), and a `finally { yield }` makes
                // the generator re-suspend below.
                if let Some(target) = self.route_through_finally(1, v) {
                    self.frames[stop].ip = target as usize;
                    self.run_loop(stop)
                } else {
                    // No `finally` pending: discard the spliced window/frame and
                    // complete the generator with the return value.
                    self.frames.pop();
                    self.regs.truncate(new_base);
                    if let HeapObj::Generator { state, regs, handlers, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Completed;
                        regs.clear();
                        handlers.clear();
                    }
                    return Ok(Some(self.iter_result(v, true)));
                }
            }
            }
        };
        if let Some((y, yield_ip)) = self.pending_yield.take() {
            // Re-suspended at another yield: park the window AND the live handlers.
            let raw = std::mem::replace(&mut self.pending_yield_raw, false);
            let back = self.regs.split_off(new_base);
            let parked = std::mem::take(&mut self.pending_yield_handlers);
            let esc = std::mem::replace(&mut self.pending_yield_eval_scope, u32::MAX);
            if esc != u32::MAX {
                self.closure_eval_scope.insert(idx, esc);
            }
            if let HeapObj::Generator { state, regs, handlers, .. } = self.heap.get_mut(idx) {
                *state = GenState::Suspended(yield_ip);
                *regs = back;
                *handlers = parked;
            }
            // A `yield*` delegation step forwards the inner iterator's result
            // object VERBATIM (spec GeneratorYield(innerResult)); a plain yield
            // wraps the value in a fresh `{value, done: false}`.
            if raw {
                return Ok(Some(y));
            }
            return Ok(Some(self.iter_result(y, false)));
        }
        match outcome {
            Ok(ret) => {
                if let HeapObj::Generator { state, regs, handlers, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                    handlers.clear();
                }
                Ok(Some(self.iter_result(ret, true)))
            }
            Err(t) => {
                self.regs.truncate(new_base);
                if let HeapObj::Generator { state, regs, handlers, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                    handlers.clear();
                }
                Err(t)
            }
        }
    }

    /// Build an iterator-result object `{ value, done }` (insertion order matches
    /// the spec / node).
    pub(crate) fn iter_result(&mut self, value: Value, done: bool) -> Value {
        let mut map = ObjMap::new();
        map.set("value", value);
        map.set("done", Value::bool(done));
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    // ── promises / microtasks ──

    pub(crate) fn alloc_promise(&mut self) -> u32 {
        self.heap.alloc(HeapObj::Promise {
            state: PromiseState::Pending,
            result: Value::UNDEFINED,
            fulfill: Vec::new(),
            reject: Vec::new(),
            handled: false,
        })
    }

    /// Settle a pending promise (no-op if already settled — the one-shot guard
    /// covers double-resolve / resolve-then-reject / race losers), scheduling its
    /// matching reactions as microtasks.
    pub(crate) fn settle(&mut self, p: u32, state: PromiseState, val: Value) {
        let reactions = match self.heap.get_mut(p) {
            HeapObj::Promise { state: s, result, fulfill, reject, .. } => {
                if *s != PromiseState::Pending {
                    return;
                }
                *s = state;
                *result = val;
                match state {
                    PromiseState::Fulfilled => std::mem::take(fulfill),
                    PromiseState::Rejected => std::mem::take(reject),
                    PromiseState::Pending => return,
                }
            }
            _ => return,
        };
        let kind = if state == PromiseState::Fulfilled {
            ReactionKind::Fulfill
        } else {
            ReactionKind::Reject
        };
        for r in reactions {
            if r.is_async {
                // `dependent` is a suspended async activation; resume it with the
                // value (fulfill) or by throwing the reason in (reject).
                let input = match kind {
                    ReactionKind::Fulfill => Resume::Value(val),
                    ReactionKind::Reject => Resume::Throw(val),
                };
                self.microtasks
                    .push_back(Microtask::AsyncResume { activation: r.dependent, input });
            } else {
                self.microtasks.push_back(Microtask::Reaction {
                    callback: r.callback,
                    arg: val,
                    dependent: r.dependent,
                    kind,
                    finally: r.finally,
                });
            }
        }
    }

    /// JS `[[Resolve]]`: a thenable/Promise value is ADOPTED (p forwards when it
    /// settles); a self-resolution rejects with a TypeError; else fulfill.
    pub(crate) fn resolve(&mut self, p: u32, value: Value) {
        if value.is_heap() {
            if value.heap_index() == p {
                let e = self.alloc_error_from_message("TypeError: Chaining cycle detected for promise");
                self.reject(p, e);
                return;
            }
            if matches!(self.heap.get(value.heap_index()), HeapObj::Promise { .. })
                && !self.has_own_property(value, "then")
            {
                // A native promise with the INTRINSIC `then` adopts state directly
                // (a valid optimisation of the resolve procedure). An OWN/overridden
                // `then` must be OBSERVED instead — the spec does
                // Get(resolution,"then") and enqueues PromiseResolveThenableJob — so
                // a shadowed `then` falls through to the generic thenable adoption.
                let inner = value.heap_index();
                self.then_internal(inner, Value::UNDEFINED, Value::UNDEFINED, Some(p));
                return;
            }
            // Thenable adoption (the Promise resolve function, steps 8-12): an
            // object with a callable `then` is adopted — defer
            // `then.call(value, resolveFn, rejectFn)` to a microtask (spec ordering).
            // A throwing `then` GETTER rejects the promise.
            if self.is_object_value(value) {
                let then = match self.get_prop(value, "then") {
                    Ok(t) => t,
                    Err(Thrown(msg)) => {
                        let e = match self.pending_throw.take() {
                            Some(v) => v,
                            None => self.alloc_error_from_message(&msg),
                        };
                        self.reject(p, e);
                        return;
                    }
                };
                if self.is_callable(then) {
                    self.microtasks.push_back(Microtask::ThenableJob {
                        thenable: value,
                        then,
                        promise: p,
                    });
                    return;
                }
            }
        }
        self.settle(p, PromiseState::Fulfilled, value);
    }

    pub(crate) fn reject(&mut self, p: u32, reason: Value) {
        self.settle(p, PromiseState::Rejected, reason);
    }

    /// Register reactions on `p` (creating/reusing the dependent promise `into`),
    /// or schedule a microtask immediately if `p` is already settled. Returns the
    /// dependent promise's heap index. The basis of `.then`/`.catch`/`.finally`
    /// and of internal promise adoption.
    pub(crate) fn then_internal(&mut self, p: u32, on_f: Value, on_r: Value, into: Option<u32>) -> u32 {
        let dep = into.unwrap_or_else(|| self.alloc_promise());
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => return dep,
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, handled, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction { callback: on_f, dependent: dep, finally: false, is_async: false });
                    reject.push(Reaction { callback: on_r, dependent: dep, finally: false, is_async: false });
                    if !on_r.is_undefined() {
                        *handled = true;
                    }
                }
            }
            PromiseState::Fulfilled => {
                self.microtasks.push_back(Microtask::Reaction {
                    callback: on_f,
                    arg: result,
                    dependent: dep,
                    kind: ReactionKind::Fulfill,
                    finally: false,
                });
            }
            PromiseState::Rejected => {
                if let HeapObj::Promise { handled, .. } = self.heap.get_mut(p) {
                    *handled = true;
                }
                self.microtasks.push_back(Microtask::Reaction {
                    callback: on_r,
                    arg: result,
                    dependent: dep,
                    kind: ReactionKind::Reject,
                    finally: false,
                });
            }
        }
        dep
    }

    /// `Promise.prototype.{then,catch}` PerformPromiseThen with SpeciesConstructor.
    /// C = SpeciesConstructor(promise, %Promise%) — a throwing/poisoned/non-object
    /// `constructor` or a throwing `@@species` propagates here (before any reaction
    /// is queued). When C is the built-in %Promise% the native fast path is kept
    /// (a plain dependent promise, no capability). Otherwise NewPromiseCapability(C)
    /// builds the result; for a Promise subclass the capability promise is a native
    /// HeapObj::Promise, so the reactions settle it directly (matching
    /// `promise_combine`); the constructed promise is returned verbatim.
    pub(crate) fn perform_promise_then(
        &mut self,
        idx: u32,
        on_f: Value,
        on_r: Value,
    ) -> Result<Value, Thrown> {
        let c = self.promise_species_constructor(Value::heap(idx))?;
        if c == self.promise_ctor_value() {
            let dep = self.then_internal(idx, on_f, on_r, None);
            return Ok(Value::heap(dep));
        }
        let (cap_promise, _resolve, _reject) = self.new_promise_capability(c)?;
        if cap_promise.is_heap()
            && matches!(self.heap.get(cap_promise.heap_index()), HeapObj::Promise { .. })
        {
            self.then_internal(idx, on_f, on_r, Some(cap_promise.heap_index()));
        }
        Ok(cap_promise)
    }

    /// SpeciesConstructor(promise, %Promise%) for `finally`: the receiver's
    /// `constructor`'s `@@species` (defaulting to %Promise% when either is absent),
    /// throwing a TypeError for a non-object `constructor` or a non-constructor
    /// `@@species`.
    pub(crate) fn promise_species_constructor(&mut self, this: Value) -> Result<Value, Thrown> {
        let c = self.get_prop(this, "constructor")?;
        if c == Value::UNDEFINED {
            return Ok(self.promise_ctor_value());
        }
        if !self.is_object_value(c) {
            return Err(Thrown(
                "TypeError: Promise.prototype.finally: constructor is not an object".into(),
            ));
        }
        let s = self.get_prop(c, "@@species")?;
        if s == Value::UNDEFINED || s == Value::NULL {
            return Ok(self.promise_ctor_value());
        }
        if !self.is_constructor(s) {
            return Err(Thrown(
                "TypeError: Promise.prototype.finally: @@species is not a constructor".into(),
            ));
        }
        Ok(s)
    }

    /// `Promise.prototype.finally(onFinally)` — the generic spec algorithm. Rather
    /// than touching internal slots, it Invokes the receiver's OWN `then` with
    /// ThenFinally/CatchFinally wrappers, so an overridden / non-callable / poisoned
    /// `then`, a thenable receiver, and a custom species constructor are all
    /// observed. When `onFinally` is not callable it is passed through as both
    /// handlers (matching the spec). The wrappers are native FINALLY_THEN/CATCH
    /// functions bound to `[onFinally, C]` (see `call_native`).
    pub(crate) fn promise_finally(&mut self, this: Value, on_finally: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(this) {
            return Err(Thrown(
                "TypeError: Promise.prototype.finally called on a non-object".into(),
            ));
        }
        let ctor = self.promise_species_constructor(this)?;
        let (then_finally, catch_finally) = if !self.is_callable(on_finally) {
            (on_finally, on_finally)
        } else {
            let then_native = Value::heap(self.heap.alloc(HeapObj::Native(native::FINALLY_THEN)));
            let catch_native = Value::heap(self.heap.alloc(HeapObj::Native(native::FINALLY_CATCH)));
            let tf = Value::heap(self.heap.alloc(HeapObj::Bound {
                target: then_native,
                this: Value::UNDEFINED,
                args: vec![on_finally, ctor],
            }));
            let cf = Value::heap(self.heap.alloc(HeapObj::Bound {
                target: catch_native,
                this: Value::UNDEFINED,
                args: vec![on_finally, ctor],
            }));
            (tf, cf)
        };
        // Hold the un-rooted wrapper Values across the `then` getter / invocation.
        let _gc = self.gc_lock_guard();
        let then = self.get_prop(this, "then")?;
        self.call_value(then, this, &[then_finally, catch_finally])
    }

    // ── async functions ──

    /// Build a suspended `async function` activation and run it synchronously up
    /// to its first `await` (or to completion / a throw). Returns the activation's
    /// result Promise — the value an `async` call evaluates to.
    pub(crate) fn alloc_async(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let gen_callee = std::mem::replace(&mut self.pending_gen_callee, Value::UNDEFINED);
        // An async arrow captures `this` lexically (call sites pass UNDEFINED/recv).
        let this = self.rebind_arrow_this(func_id, closure, this);
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let arguments_reg = proto.arguments_reg;
        // OrdinaryCallBindThis: a SLOPPY (non-arrow) generator/async invoked
        // with a nullish `this` binds the global object; a primitive boxes.
        let this = if !proto.lexical_this
            && !proto.is_strict
            && this.is_nullish()
            && self.global_this != 0
        {
            Value::heap(self.global_this)
        } else if !proto.lexical_this
            && !proto.is_strict
            && !self.is_object_value(this)
            && self.global_this != 0
        {
            self.to_object(this).unwrap_or(this)
        } else {
            this
        };
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        // `arguments` (an async function that references it).
        if let Some(areg) = arguments_reg {
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
            regs[areg as usize] = arr;
        }
        let result = self.alloc_promise();
        // A MODULE BODY activation: its result promise settles with undefined
        // (spec Evaluate() capability), never the body's completion value.
        if std::mem::take(&mut self.pending_module_body_marker) {
            self.module_body_results.insert(result);
        }
        let idx = self.heap.alloc(HeapObj::AsyncState(Box::new(AsyncStateData {
            func: func_id,
            closure,
            // `usize::MAX` = not-yet-started — distinct from a genuine yield/await
            // parked at ip 0 (which previously collided with this sentinel and made
            // the resume re-run from the top).
            state: GenState::Suspended(usize::MAX),
            regs,
            result,
            handlers: Vec::new(),
        })));
        if gen_callee != Value::UNDEFINED {
            // Every drive/resume binds this as Frame.callee (LoadCallee identity).
            self.gen_callee.insert(idx, gen_callee);
        }
        // Run from the top until the first await suspends it (or it finishes —
        // settling `result` either way).
        self.drive_async(idx, Resume::Value(Value::UNDEFINED));
        Value::heap(result)
    }

    /// Calling an `async function*` builds a suspended AsyncGenerator (an async
    /// iterator). Like a sync generator, its parameter prologue (defaults +
    /// destructuring) runs EAGERLY here at call time — so a destructuring throw
    /// surfaces synchronously from the call — and it is parked at the `GenStart`
    /// body-entry marker; the body itself does NOT run until the first `.next()`.
    pub(crate) fn alloc_async_generator(
        &mut self,
        func_id: u32,
        closure: u32,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        let gen_callee = std::mem::replace(&mut self.pending_gen_callee, Value::UNDEFINED);
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let arguments_reg = proto.arguments_reg;
        // OrdinaryCallBindThis: a SLOPPY (non-arrow) generator/async invoked
        // with a nullish `this` binds the global object; a primitive boxes.
        let this = if !proto.lexical_this
            && !proto.is_strict
            && this.is_nullish()
            && self.global_this != 0
        {
            Value::heap(self.global_this)
        } else if !proto.lexical_this
            && !proto.is_strict
            && !self.is_object_value(this)
            && self.global_this != 0
        {
            self.to_object(this).unwrap_or(this)
        } else {
            this
        };
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        // `arguments` (an async generator that references it).
        if let Some(areg) = arguments_reg {
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
            regs[areg as usize] = arr;
        }
        // Run the parameter prologue up to `GenStart` (see `alloc_generator`).
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.extend_from_slice(&regs);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: Vec::new(),
            new_target: Value::UNDEFINED,
            callee: gen_callee,
        });
        let outcome = self.run_loop(stop);
        self.pending_yield_eval_scope = u32::MAX;
        let (state, regs) = if let Some((_v, genstart_ip)) = self.pending_yield.take() {
            // Suspended at `GenStart`: park at the marker, body runs on first next().
            (GenState::Suspended(genstart_ip), self.regs.split_off(new_base))
        } else {
            // The prologue threw (propagate at the call) or, defensively, the body
            // ran to completion (a proto with no `GenStart`).
            self.regs.truncate(new_base);
            match outcome {
                Ok(_) => (GenState::Completed, Vec::new()),
                Err(t) => return Err(t),
            }
        };
        let ag = self.heap.alloc(HeapObj::AsyncGenerator(Box::new(AsyncGenState {
            func: func_id,
            closure,
            state,
            regs,
            handlers: Vec::new(),
            queue: Vec::new(),
            awaiting_return: false,
        })));
        // OrdinaryCreateFromConstructor: honor the callee's `prototype`
        // object; the HeapObj::AsyncGenerator fallback stays %AsyncGenProto%.
        if let Some(p) = self.prototype_of(gen_callee) {
            self.proto_of.insert(ag, p);
        }
        if gen_callee != Value::UNDEFINED {
            // Resumes re-bind this as Frame.callee (LoadCallee identity).
            self.gen_callee.insert(ag, gen_callee);
        }
        Ok(Value::heap(ag))
    }

    /// `.next()`/`.return()`/`.throw()` on an async generator. Each returns a
    /// Promise that settles when its request is serviced (AsyncGeneratorEnqueue:
    /// EVERY request — including return/throw on a running or completed
    /// generator — is appended to the FIFO queue; `async_gen_service_queue`
    /// then makes progress unless a step is already in flight).
    pub(crate) fn async_generator_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Option<Value> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let kind = match name {
            "next" => 0u8,
            "throw" => 1,
            "return" => 2,
            _ => return None,
        };
        let p = self.alloc_promise();
        match self.heap.get_mut(idx) {
            HeapObj::AsyncGenerator(g) => {
                g.queue.push(crate::heap::AsyncGenRequest { kind, arg: arg0, promise: p })
            }
            _ => return None,
        }
        self.async_gen_service_queue(idx);
        Some(Value::heap(p))
    }

    /// Service the request queue (AsyncGeneratorResumeNext + DrainQueue): settle
    /// requests a COMPLETED generator answers directly (next → `{undefined,
    /// done:true}`, throw → reject), and start at most ONE resumption when the
    /// generator is idle at a suspension point — `next`/`throw` resume the body,
    /// `return` first awaits its argument (`async_gen_await_return`). A generator
    /// that is Running, parked at a body `await`, or already awaiting-return is
    /// left alone: the in-flight step re-enters here when it settles.
    pub(crate) fn async_gen_service_queue(&mut self, idx: u32) {
        loop {
            let (state, fid, awaiting, front) = match self.heap.get(idx) {
                HeapObj::AsyncGenerator(g) => (
                    g.state,
                    g.func,
                    g.awaiting_return,
                    g.queue.first().map(|r| (r.kind, r.arg, r.promise)),
                ),
                _ => return,
            };
            if awaiting {
                return;
            }
            let Some((kind, arg, p)) = front else { return };
            match state {
                GenState::Running => return,
                GenState::Completed => match kind {
                    0 => {
                        self.async_gen_pop_front(idx);
                        let r = self.iter_result(Value::UNDEFINED, true);
                        self.resolve(p, r);
                    }
                    1 => {
                        self.async_gen_pop_front(idx);
                        self.reject(p, arg);
                    }
                    _ => {
                        // 'awaiting-return': even a completed generator awaits the
                        // return value before resolving `{value, done:true}`.
                        self.async_gen_await_return(idx, arg);
                        return;
                    }
                },
                GenState::Suspended(ip) => {
                    let op = self.func(fid as usize).code.get(ip);
                    let at_start = ip == 0 || matches!(op, Some(Instr::GenStart));
                    let at_yield = matches!(
                        op,
                        Some(Instr::Yield { .. } | Instr::AsyncYieldDelegate { .. })
                    );
                    if at_start {
                        match kind {
                            0 => return self.drive_async_gen(idx, Resume::Value(arg)),
                            1 => {
                                // throw before the body starts: no handler can exist
                                // yet — complete and reject (the body never runs).
                                self.async_gen_complete(idx);
                                self.async_gen_pop_front(idx);
                                self.reject(p, arg);
                            }
                            _ => {
                                self.async_gen_await_return(idx, arg);
                                return;
                            }
                        }
                    } else if at_yield {
                        match kind {
                            0 => return self.drive_async_gen(idx, Resume::Value(arg)),
                            // throw into the suspended body: the parked handlers make
                            // try/catch/finally spanning the yield run.
                            1 => return self.drive_async_gen(idx, Resume::Throw(arg)),
                            // AsyncGeneratorUnwrapYieldResumption: Await(arg) FIRST,
                            // then deliver a return completion (or the rejection as a
                            // throw) — drive_async_gen's awaiting-return entry.
                            _ => {
                                self.async_gen_await_return(idx, arg);
                                return;
                            }
                        }
                    } else {
                        // Parked at a body `await`: a request is in flight; its
                        // settlement resumes the body and re-services the queue.
                        return;
                    }
                }
            }
        }
    }

    /// Pop the front request (its promise is being settled by the caller).
    fn async_gen_pop_front(&mut self, idx: u32) {
        if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
            if !g.queue.is_empty() {
                g.queue.remove(0);
            }
        }
    }

    /// Mark the generator completed, discarding its parked window + handlers.
    fn async_gen_complete(&mut self, idx: u32) {
        if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
            g.state = GenState::Completed;
            g.regs.clear();
            g.handlers.clear();
        }
    }

    /// AsyncGeneratorAwaitReturn / the Await step of UnwrapYieldResumption: start
    /// the one-tick await of the front `.return(value)` argument. Marks the
    /// generator awaiting-return (requests arriving meanwhile only enqueue) and
    /// subscribes it to PromiseResolve(value) — replicating the `Await` op's
    /// OBSERVABLE constructor read for a native promise here, in the caller's
    /// (native / microtask) context, never inside the drive completion path. The
    /// settlement re-enters `drive_async_gen`, whose awaiting-return entry routes
    /// it; an abrupt PromiseResolve delivers an immediate throw completion.
    fn async_gen_await_return(&mut self, idx: u32, arg: Value) {
        if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
            g.awaiting_return = true;
        }
        let is_promise = arg.is_heap()
            && matches!(self.heap.get(arg.heap_index()), HeapObj::Promise { .. });
        let p = if is_promise {
            match self.get_prop(arg, "constructor") {
                Ok(c) => {
                    let intrinsic = self.global_by_name("Promise").unwrap_or(Value::UNDEFINED);
                    if c == intrinsic {
                        arg.heap_index()
                    } else {
                        let p = self.alloc_promise();
                        self.resolve(p, arg);
                        p
                    }
                }
                Err(Thrown(msg)) => {
                    let e = self.take_thrown(&msg);
                    self.drive_async_gen(idx, Resume::Throw(e));
                    return;
                }
            }
        } else {
            self.to_promise(arg)
        };
        self.settle_subscribe(p, idx);
    }

    /// Advance an async generator: run its body until the next `yield` (resolve
    /// the front queued promise with `{value, done:false}`), `await` (park +
    /// subscribe, the promise stays pending), or return/throw (settle), then
    /// service any remaining queued requests. `input` delivers the `.next()`
    /// argument, a settled awaited value/throw, or a return completion
    /// (`Resume::Return`, from `.return()` once its argument has been awaited).
    pub(crate) fn drive_async_gen(&mut self, idx: u32, mut input: Resume) {
        // An awaiting-return settlement (AsyncGeneratorAwaitReturn / the Await of
        // UnwrapYieldResumption — `async_gen_await_return`): route per the parked
        // state. Suspended at a yield ⇒ resume the body with a RETURN completion
        // (or the rejection as a throw, into try/catch/finally); otherwise
        // (suspendedStart / completed) settle the front `.return()` request
        // directly with `{awaited, done: true}` / the rejection.
        let awaiting = matches!(
            self.heap.get(idx),
            HeapObj::AsyncGenerator(g) if g.awaiting_return
        );
        if awaiting {
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                g.awaiting_return = false;
            }
            let at_yield = match self.heap.get(idx) {
                HeapObj::AsyncGenerator(g) => {
                    let fid = g.func;
                    matches!(g.state, GenState::Suspended(ip)
                        if matches!(self.func(fid as usize).code.get(ip),
                            Some(Instr::Yield { .. } | Instr::AsyncYieldDelegate { .. })))
                }
                _ => false,
            };
            if at_yield {
                input = match input {
                    Resume::Value(v) => Resume::Return(v),
                    other => other, // a rejection throws in at the yield point
                };
                // fall through to the normal resume below
            } else {
                self.async_gen_complete(idx);
                let front = match self.heap.get(idx) {
                    HeapObj::AsyncGenerator(g) => g.queue.first().map(|r| r.promise),
                    _ => None,
                };
                self.async_gen_pop_front(idx);
                if let Some(p) = front {
                    match input {
                        Resume::Value(v) | Resume::Return(v) => {
                            let r = self.iter_result(v, true);
                            self.resolve(p, r);
                        }
                        Resume::Throw(e) => self.reject(p, e),
                    }
                }
                self.async_gen_service_queue(idx);
                return;
            }
        }
        let (state, fid, closure) = match self.heap.get(idx) {
            HeapObj::AsyncGenerator(g) => (g.state, g.func, g.closure),
            _ => return,
        };
        let resume_ip = match state {
            GenState::Completed => return self.async_gen_service_queue(idx),
            GenState::Running => return, // re-entrant; will resume when current settles
            GenState::Suspended(ip) => ip,
        };
        // Nothing queued ⇒ idle until a `.next()` arrives.
        if matches!(self.heap.get(idx), HeapObj::AsyncGenerator(g) if g.queue.is_empty()) {
            return;
        }
        let (saved, saved_handlers) = match self.heap.get_mut(idx) {
            HeapObj::AsyncGenerator(g) => {
                g.state = GenState::Running;
                (std::mem::take(&mut g.regs), std::mem::take(&mut g.handlers))
            }
            _ => return,
        };
        let reg_count = saved.len();
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                g.state = GenState::Completed;
                g.regs.clear();
            }
            let e = self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            let front = match self.heap.get(idx) {
                HeapObj::AsyncGenerator(g) => g.queue.first().map(|r| r.promise),
                _ => None,
            };
            if let Some(p) = front {
                self.async_gen_pop_front(idx);
                self.reject(p, e);
            }
            self.async_gen_service_queue(idx);
            return;
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
            new_target: Value::UNDEFINED,
            // The creating call's function value (named fn-expression
            // self-name identity survives suspension).
            callee: self.gen_callee.get(&idx).copied().unwrap_or(Value::UNDEFINED),
        });
        // Resume after the suspending op, delivering the sent/awaited value. The
        // op at `resume_ip` is a Yield (resumed by `.next(v)`) or Await (resumed
        // by a settled promise) — both write the value into the op's `dst`.
        // A `GenStart` marker means freshly constructed (the prologue already ran
        // eagerly at the call): run the body from just past it with no value
        // delivery. (The legacy `0` sentinel — a proto with no marker — runs from
        // the top.)
        let is_genstart = matches!(
            self.func(fid as usize).code.get(resume_ip),
            Some(Instr::GenStart)
        );
        let outcome = if is_genstart {
            self.frames[stop].ip = resume_ip + 1;
            self.run_loop(stop)
        } else if resume_ip == 0 {
            self.run_loop(stop)
        } else {
            match input {
                Resume::Value(v) => {
                    // A `.next(v)` resume: deliver v. At an AsyncYieldDelegate also set
                    // its mode register to 0 (next) so the yield* loop continues.
                    match self.func(fid as usize).code[resume_ip] {
                        Instr::AsyncYieldDelegate { mode_dst, val_dst, .. } => {
                            self.regs[new_base + mode_dst as usize] = Value::int(0);
                            self.regs[new_base + val_dst as usize] = v;
                        }
                        Instr::Yield { dst, .. } | Instr::Await { dst, .. } => {
                            self.regs[new_base + dst as usize] = v;
                        }
                        _ => {}
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
                }
                Resume::Return(v) => {
                    // A `.return(v)` resume (its value already awaited). At an async
                    // yield* delegate: set mode 2 (return) + the value, so the loop
                    // delegates to inner.return. At a plain Yield: inject a RETURN
                    // completion — run any `finally` spanning the yield (the body
                    // may re-suspend / override the completion there); with none,
                    // the generator completes with `v` (the Ok arm below resolves
                    // the front request `{v, done: true}`).
                    if let Instr::AsyncYieldDelegate { mode_dst, val_dst, .. } =
                        self.func(fid as usize).code[resume_ip]
                    {
                        self.regs[new_base + mode_dst as usize] = Value::int(2);
                        self.regs[new_base + val_dst as usize] = v;
                        self.frames[stop].ip = resume_ip + 1;
                        self.run_loop(stop)
                    } else if let Some(target) = self.route_through_finally(1, v) {
                        self.frames[stop].ip = target as usize;
                        self.run_loop(stop)
                    } else {
                        self.frames.pop();
                        self.regs.truncate(new_base);
                        Ok(v)
                    }
                }
                Resume::Throw(e) => {
                    self.pending_throw = Some(e);
                    if self.unwind_to_handler(e, stop) {
                        self.pending_throw = None;
                        self.run_loop(stop)
                    } else {
                        Err(Thrown(String::new()))
                    }
                }
            }
        };
        // Yielded a value → resolve the front queued promise with {value, done:false}.
        if let Some((y, yield_ip)) = self.pending_yield.take() {
            // Preserve the body's `try` handlers across the suspension so a later
            // `.throw()`/`.return()` (or a yield* throw-delegation) can unwind into
            // them on resume — the await path already does this; the yield path used
            // to drop them.
            let handlers = std::mem::take(&mut self.pending_yield_handlers);
            self.pending_yield_eval_scope = u32::MAX;
            let back = self.regs.split_off(new_base);
            let front = match self.heap.get_mut(idx) {
                HeapObj::AsyncGenerator(g) => {
                    g.state = GenState::Suspended(yield_ip);
                    g.regs = back;
                    g.handlers = handlers;
                    (!g.queue.is_empty()).then(|| g.queue.remove(0))
                }
                _ => None,
            };
            if let Some(req) = front {
                let r = self.iter_result(y, false);
                self.resolve(req.promise, r);
            }
            // More requests already queued → service the next one now, honoring
            // its kind (a queued `.next(v)` delivers ITS argument to this yield;
            // a queued return/throw resumes with that completion).
            self.async_gen_service_queue(idx);
            return;
        }
        // Awaited → park and subscribe; the front promise stays pending.
        if let Some((awaited, await_ip, handlers)) = self.pending_await.take() {
            let back = self.regs.split_off(new_base);
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                g.state = GenState::Suspended(await_ip);
                g.regs = back;
                g.handlers = handlers;
            }
            let p = self.to_promise(awaited);
            self.settle_subscribe(p, idx);
            return;
        }
        // Returned / fell off the end, or threw.
        match outcome {
            Ok(ret) => {
                let front = match self.heap.get_mut(idx) {
                    HeapObj::AsyncGenerator(g) => {
                        g.state = GenState::Completed;
                        g.regs.clear();
                        g.handlers.clear();
                        (!g.queue.is_empty()).then(|| g.queue.remove(0))
                    }
                    _ => None,
                };
                if let Some(req) = front {
                    let r = self.iter_result(ret, true);
                    self.resolve(req.promise, r);
                }
                self.async_gen_service_queue(idx);
            }
            Err(t) => {
                self.regs.truncate(new_base);
                let reason = self.pending_throw.take().unwrap_or_else(|| {
                    self.alloc_error_from_message(&t.0)
                });
                let front = match self.heap.get_mut(idx) {
                    HeapObj::AsyncGenerator(g) => {
                        g.state = GenState::Completed;
                        g.regs.clear();
                        g.handlers.clear();
                        (!g.queue.is_empty()).then(|| g.queue.remove(0))
                    }
                    _ => None,
                };
                if let Some(req) = front {
                    self.reject(req.promise, reason);
                }
                self.async_gen_service_queue(idx);
            }
        }
    }

    /// `Promise.resolve` as an internal helper: a Promise passes through (identity
    /// preserved); any other value is wrapped in a fulfilled promise. The basis of
    /// awaiting a non-promise (`await 5` still yields a microtask tick).
    pub(crate) fn to_promise(&mut self, v: Value) -> u32 {
        if v.is_heap() {
            if matches!(self.heap.get(v.heap_index()), HeapObj::Promise { .. }) {
                return v.heap_index();
            }
        }
        let p = self.alloc_promise();
        self.resolve(p, v);
        p
    }

    /// Subscribe a suspended async `activation` to promise `p`: when `p` settles,
    /// the activation resumes with the value, or has the reason thrown back in at
    /// the await point. If `p` is already settled, schedule the resume as a
    /// microtask (so `await` always yields to the queue, per spec).
    pub(crate) fn settle_subscribe(&mut self, p: u32, activation: u32) {
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => {
                self.microtasks.push_back(Microtask::AsyncResume {
                    activation,
                    input: Resume::Value(Value::UNDEFINED),
                });
                return;
            }
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, handled, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction {
                        callback: Value::UNDEFINED,
                        dependent: activation,
                        finally: false,
                        is_async: true,
                    });
                    reject.push(Reaction {
                        callback: Value::UNDEFINED,
                        dependent: activation,
                        finally: false,
                        is_async: true,
                    });
                    *handled = true; // an `await` consumes the rejection
                }
            }
            PromiseState::Fulfilled => self.microtasks.push_back(Microtask::AsyncResume {
                activation,
                input: Resume::Value(result),
            }),
            PromiseState::Rejected => {
                if let HeapObj::Promise { handled, .. } = self.heap.get_mut(p) {
                    *handled = true;
                }
                self.microtasks.push_back(Microtask::AsyncResume {
                    activation,
                    input: Resume::Throw(result),
                });
            }
        }
    }

    // ── Promise combinators ──

    /// `Promise.all/allSettled/race/any(iterable)`. Coerces each input to a
    /// promise and subscribes a native combinator reaction; the shared
    /// `Combinator` state settles the returned promise per the combinator's rule.
    /// NewPromiseCapability(C): construct `C` with a capturing executor and return
    /// `(promise, resolve, reject)`. Works for the native Promise and any subclass
    /// that chains through `super()` (the executor is invoked synchronously with
    /// callable resolve/reject). Throws if C isn't a constructor, the executor
    /// isn't called, or resolve/reject aren't callable.
    pub(crate) fn new_promise_capability(
        &mut self,
        c: Value,
    ) -> Result<(Value, Value, Value), Thrown> {
        if !self.is_constructor(c) {
            return Err(Thrown("TypeError: NewPromiseCapability requires a constructor".into()));
        }
        // Save/restore for a nested capability; clear so the executor's
        // already-called check starts fresh.
        let saved = self.cap_capture.take();
        let executor = Value::heap(self.heap.alloc(HeapObj::Native(native::CAP_EXECUTOR)));
        let constructed = self.construct(c, &[executor]);
        let captured = self.cap_capture.take();
        self.cap_capture = saved;
        let promise = constructed?;
        let (resolve, reject) = match captured {
            Some(rr) => rr,
            None => {
                return Err(Thrown(
                    "TypeError: Promise resolve/reject were not set by the executor".into(),
                ))
            }
        };
        if !self.is_callable(resolve) || !self.is_callable(reject) {
            return Err(Thrown("TypeError: Promise resolve or reject is not callable".into()));
        }
        Ok((promise, resolve, reject))
    }

    /// The `Promise` constructor value, for the lowered `Promise.all/race/...`
    /// ops where the receiver `this` isn't threaded. Resolved via
    /// `Promise.prototype.constructor` (so `Get(C, "resolve")` reaches an
    /// overridden `Promise.resolve`).
    pub(crate) fn promise_ctor_value(&mut self) -> Value {
        if self.promise_proto == 0 {
            return Value::UNDEFINED;
        }
        self.get_prop(Value::heap(self.promise_proto), "constructor")
            .unwrap_or(Value::UNDEFINED)
    }

    pub(crate) fn promise_combine(
        &mut self,
        kind: crate::heap::CombKind,
        iterable: Value,
        ctor: Value,
    ) -> Result<Value, Thrown> {
        use crate::heap::CombKind;
        // NewPromiseCapability(C) — the result is a C-typed promise (a subclass
        // instance when `this` is a Promise subclass). A throwing/invalid capability
        // (executor not called / called twice / non-callable resolve-reject) throws
        // synchronously per ReturnIfAbrupt. The instance is a HeapObj::Promise, so
        // the existing self.resolve/reject(result) settle it directly.
        let (cap_promise, cap_resolve, cap_reject) = self.new_promise_capability(ctor)?;
        let result = cap_promise.heap_index();
        // GetPromiseResolve(C): `promiseResolve = Get(C, "resolve")`, which must be
        // callable. This is the spec-observable step the tests check (overriding
        // `Promise.resolve` is visible, and a non-callable resolve rejects). When C
        // isn't a usable object we fall back to internal coercion. An abrupt Get or
        // a non-callable resolve rejects the result (IfAbruptRejectPromise).
        let promise_resolve = if self.is_object_value(ctor) {
            match self.get_prop(ctor, "resolve") {
                Ok(r) => Some(r),
                Err(Thrown(msg)) => {
                    let err = match self.pending_throw.take() {
                        Some(v) => v,
                        None => self.alloc_error_from_message(&msg),
                    };
                    self.reject(result, err);
                    return Ok(Value::heap(result));
                }
            }
        } else {
            None
        };
        if let Some(pr) = promise_resolve {
            if !self.is_callable(pr) {
                let err = self.alloc_error_from_message("TypeError: Promise.resolve is not a function");
                self.reject(result, err);
                return Ok(Value::heap(result));
            }
        }
        // Keyed combinators (allKeyed/allSettledKeyed) operate over an OBJECT's own
        // enumerable keys (incl. Symbols): validate it's an object, snapshot the
        // keys, and iterate the corresponding VALUES (reusing the array-iteration
        // path below); the keys are stored on the Combinator (Symbols as their
        // "@@…" form) to build the keyed result on settle.
        let mut comb_keys: Vec<String> = Vec::new();
        let iterable = if matches!(kind, CombKind::AllKeyed | CombKind::AllSettledKeyed) {
            if !self.is_object_value(iterable) {
                let e = self.alloc_error_from_message(
                    "TypeError: Promise.allKeyed argument is not an object",
                );
                self.reject(result, e);
                return Ok(Value::heap(result));
            }
            // OwnPropertyKeys (incl. Symbols), filtered to enumerable own props —
            // per PerformPromiseAllKeyed step "For each key of ? OwnPropertyKeys,
            // if desc.[[Enumerable]]". A Symbol key round-trips via its internal
            // "@@…" key string (key_of), so `map.set` in combinator_finish carries
            // it back as a real Symbol on the keyed result object.
            let karr = match self.object_own_keys(iterable) {
                Ok(a) => a,
                Err(Thrown(msg)) => {
                    self.reject_with_thrown(result, &msg);
                    return Ok(Value::heap(result));
                }
            };
            let kvals = self.array_snapshot(karr.heap_index());
            let mut vals = Vec::with_capacity(kvals.len());
            for kv in kvals {
                let ks = self.key_of(kv);
                if !self.own_is_enumerable(iterable, &ks) {
                    continue;
                }
                let v = match self.get_member(iterable, &ks, iterable) {
                    Ok(v) => v,
                    Err(Thrown(msg)) => {
                        self.reject_with_thrown(result, &msg);
                        return Ok(Value::heap(result));
                    }
                };
                comb_keys.push(ks);
                vals.push(v);
            }
            Value::heap(self.heap.alloc(HeapObj::Array(vals)))
        } else {
            iterable
        };
        // GetIterator(iterable): invoke @@iterator to obtain a REAL iterator object
        // (not the array fast-path, which `get_iterator` returns un-stepped) so the
        // lazy step + IteratorClose below work. A non-callable @@iterator / abrupt →
        // reject (IfAbruptRejectPromise).
        let iter = match self.get_prop(iterable, "@@iterator") {
            Ok(m) if self.is_callable(m) => match self.call_value(m, iterable, &[]) {
                Ok(it) => it,
                Err(Thrown(msg)) => {
                    self.reject_with_thrown(result, &msg);
                    return Ok(Value::heap(result));
                }
            },
            Ok(_) => {
                let disp = self.display(iterable);
                let e = self.alloc_error_from_message(&format!("TypeError: {disp} is not iterable"));
                self.reject(result, e);
                return Ok(Value::heap(result));
            }
            Err(Thrown(msg)) => {
                self.reject_with_thrown(result, &msg);
                return Ok(Value::heap(result));
            }
        };
        // remainingElementsCount starts at 1 (the iteration itself); each element
        // bumps it, each settling element decrements it, and completing the
        // iteration decrements it — when it hits 0 the combinator resolves/rejects.
        // results/settled grow as the iterator is stepped (LAZY, so an abrupt
        // Call(promiseResolve)/Invoke(.then) can IteratorClose the live iterator).
        let comb = self.heap.alloc(HeapObj::Combinator {
            kind,
            results: Vec::new(),
            remaining: 1,
            result,
            settled: Vec::new(),
            cap_resolve,
            cap_reject,
            keys: comb_keys,
        });
        loop {
            // IteratorStep; a throwing next() leaves the iterator done → no close.
            let val = match self.iterator_step(iter) {
                Ok(Some(v)) => v,
                Ok(None) => break,
                Err(Thrown(msg)) => {
                    self.reject_with_thrown(result, &msg);
                    return Ok(Value::heap(result));
                }
            };
            let index = match self.heap.get_mut(comb) {
                HeapObj::Combinator { results, settled, remaining, .. } => {
                    let i = results.len() as u32;
                    results.push(Value::UNDEFINED);
                    settled.push(false);
                    *remaining += 1;
                    i
                }
                _ => break,
            };
            // nextPromise = Call(promiseResolve, C, «val») — OBSERVABLE. An abrupt
            // here closes the (still-open) iterator, then rejects with the ORIGINAL
            // error (IteratorClose preserves the throw).
            let next = match promise_resolve {
                Some(pr) => match self.call_value(pr, ctor, &[val]) {
                    Ok(v) => v,
                    Err(Thrown(msg)) => {
                        let err = self.take_thrown(&msg);
                        let _ = self.iterator_close(iter);
                        // IfAbruptRejectPromise: settle through the capability's
                        // observable [[Reject]] (a custom constructor's reject is
                        // visibly invoked), not the internal reject.
                        self.call_value(cap_reject, Value::UNDEFINED, &[err])?;
                        return Ok(Value::heap(result));
                    }
                },
                // No usable constructor: coerce to a native promise for the Invoke.
                None => Value::heap(self.to_promise(val)),
            };
            // onFulfilled / onRejected per PerformPromise{All,AllSettled,Race,Any}: a
            // per-index "resolveElement"/"rejectElement" (CombinatorResolver) is used
            // only where the algorithm collects a per-input outcome; otherwise the
            // SHARED result-capability [[Resolve]]/[[Reject]] is passed directly. So
            // every Promise.all element sees the SAME reject function, Promise.race
            // uses the capability pair directly, and Promise.any shares the resolve.
            let element_resolver = |vm: &mut Self, is_reject: bool| {
                Value::heap(vm.heap.alloc(HeapObj::CombinatorResolver { combinator: comb, index, is_reject }))
            };
            let res_f = match kind {
                CombKind::Race | CombKind::Any => cap_resolve,
                CombKind::All
                | CombKind::AllSettled
                | CombKind::AllKeyed
                | CombKind::AllSettledKeyed => element_resolver(self, false),
            };
            let res_r = match kind {
                CombKind::All | CombKind::Race | CombKind::AllKeyed => cap_reject,
                CombKind::Any | CombKind::AllSettled | CombKind::AllSettledKeyed => {
                    element_resolver(self, true)
                }
            };
            // Invoke(nextPromise, "then", «res_f, res_r») — OBSERVABLE; abrupt →
            // close + reject with the original error.
            let outcome = match self.get_prop(next, "then") {
                Ok(then_fn) => self.call_value(then_fn, next, &[res_f, res_r]),
                Err(e) => Err(e),
            };
            if let Err(Thrown(msg)) = outcome {
                let err = self.take_thrown(&msg);
                let _ = self.iterator_close(iter);
                self.reject(result, err);
                return Ok(Value::heap(result));
            }
        }
        // Iteration complete: drop the initial count. Race never uses the counter
        // (it settles on the first element), so its empty form stays pending.
        if !matches!(kind, CombKind::Race) {
            if let HeapObj::Combinator { remaining, .. } = self.heap.get_mut(comb) {
                *remaining -= 1;
            }
            self.combinator_finish(comb);
        }
        Ok(Value::heap(result))
    }

    /// Reject promise `p` with the live thrown value (`pending_throw`), falling
    /// back to reconstructing an error from the message.
    fn reject_with_thrown(&mut self, p: u32, msg: &str) {
        let e = self.take_thrown(msg);
        self.reject(p, e);
    }

    /// The live thrown value (`pending_throw`), or a fresh error from the message.
    fn take_thrown(&mut self, msg: &str) -> Value {
        match self.pending_throw.take() {
            Some(v) => v,
            None => self.alloc_error_from_message(msg),
        }
    }

    /// When a combinator's remainingElementsCount reaches 0, settle its result:
    /// All/AllSettled resolve with the collected array, Any rejects with an
    /// AggregateError of the collected reasons. A no-op while count > 0 or for Race.
    fn combinator_finish(&mut self, comb: u32) {
        use crate::heap::CombKind;
        let (ckind, remaining, cap_resolve, cap_reject) = match self.heap.get(comb) {
            HeapObj::Combinator { kind, remaining, cap_resolve, cap_reject, .. } => {
                (*kind, *remaining, *cap_resolve, *cap_reject)
            }
            _ => return,
        };
        if remaining != 0 || ckind == CombKind::Race {
            return;
        }
        let (collected, comb_keys) = match self.heap.get(comb) {
            HeapObj::Combinator { results, keys, .. } => (results.clone(), keys.clone()),
            _ => return,
        };
        // Settle THROUGH the result capability (per spec): Any rejects with an
        // AggregateError of the collected reasons; All/AllSettled resolve with the
        // collected array. (On the native path the cap functions are BoundResolvers
        // bound to `result`, so this is the same as self.resolve/reject(result, …).)
        match ckind {
            CombKind::Any => {
                let e = self.alloc_aggregate_error(collected);
                let _ = self.call_value(cap_reject, Value::UNDEFINED, &[e]);
            }
            CombKind::AllKeyed | CombKind::AllSettledKeyed => {
                // Build a NULL-prototype object mapping each key to its value/record.
                let mut map = ObjMap::new();
                for (k, v) in comb_keys.iter().zip(collected.iter()) {
                    map.set(k, *v);
                }
                let obj = self.heap.alloc(HeapObj::Object(map));
                self.proto_of.insert(obj, Value::NULL);
                if let Err(Thrown(msg)) =
                    self.call_value(cap_resolve, Value::UNDEFINED, &[Value::heap(obj)])
                {
                    self.combinator_reject_on_resolve_throw(cap_reject, msg);
                }
            }
            _ => {
                let arr = Value::heap(self.heap.alloc(HeapObj::Array(collected)));
                if let Err(Thrown(msg)) = self.call_value(cap_resolve, Value::UNDEFINED, &[arr]) {
                    self.combinator_reject_on_resolve_throw(cap_reject, msg);
                }
            }
        }
    }

    /// A combinator's result capability `resolve` threw (a custom subclass may
    /// install a throwing resolve): per IfAbruptRejectPromise, reject the result
    /// promise with the thrown VALUE (preserved via `pending_throw`, not a
    /// reconstructed error).
    fn combinator_reject_on_resolve_throw(&mut self, cap_reject: Value, msg: String) {
        let e = match self.pending_throw.take() {
            Some(v) => v,
            None => self.alloc_error_from_message(&msg),
        };
        let _ = self.call_value(cap_reject, Value::UNDEFINED, &[e]);
    }

    /// Perform one combinator step: the input at `index` settled (`kind`) with
    /// `value`. Updates the shared state and settles the combinator's promise
    /// when its rule is met (the one-shot `settle` guard absorbs later inputs).
    pub(crate) fn combinator_step(&mut self, comb: u32, index: u32, kind: ReactionKind, value: Value) {
        use crate::heap::CombKind;
        let (ckind, cap_resolve, cap_reject) = match self.heap.get(comb) {
            HeapObj::Combinator { kind, cap_resolve, cap_reject, .. } => {
                (*kind, *cap_resolve, *cap_reject)
            }
            _ => return,
        };
        // [[AlreadyCalled]]: a custom thenable may invoke a resolve/reject element
        // (or both) more than once for the same index — only the first counts.
        let already = match self.heap.get_mut(comb) {
            HeapObj::Combinator { settled, .. } => {
                let s = &mut settled[index as usize];
                if *s {
                    true
                } else {
                    *s = true;
                    false
                }
            }
            _ => return,
        };
        if already {
            return;
        }
        // Each immediate settle goes THROUGH the result capability's
        // [[Resolve]]/[[Reject]] (observable for a custom `this`-constructor; on the
        // native path the cap functions resolve/reject `result` directly).
        match (ckind, kind) {
            (CombKind::Race, ReactionKind::Fulfill) => {
                let _ = self.call_value(cap_resolve, Value::UNDEFINED, &[value]);
            }
            (CombKind::Race, ReactionKind::Reject) => {
                let _ = self.call_value(cap_reject, Value::UNDEFINED, &[value]);
            }
            (CombKind::All | CombKind::AllKeyed, ReactionKind::Reject) => {
                let _ = self.call_value(cap_reject, Value::UNDEFINED, &[value]);
            }
            (CombKind::Any, ReactionKind::Fulfill) => {
                let _ = self.call_value(cap_resolve, Value::UNDEFINED, &[value]);
            }
            (CombKind::All | CombKind::AllKeyed, ReactionKind::Fulfill)
            | (CombKind::Any, ReactionKind::Reject)
            | (CombKind::AllSettled | CombKind::AllSettledKeyed, _) => {
                // Record the per-input outcome and decrement the outstanding count;
                // combinator_finish settles the result when it reaches 0.
                let stored = if matches!(ckind, CombKind::AllSettled | CombKind::AllSettledKeyed) {
                    self.make_settled_record(kind, value)
                } else {
                    value
                };
                if let HeapObj::Combinator { results, remaining, .. } = self.heap.get_mut(comb) {
                    results[index as usize] = stored;
                    *remaining -= 1;
                }
                self.combinator_finish(comb);
            }
        }
    }

    /// Build a `Promise.allSettled` record: `{status:'fulfilled', value}` or
    /// `{status:'rejected', reason}`.
    pub(crate) fn make_settled_record(&mut self, kind: ReactionKind, value: Value) -> Value {
        let mut map = ObjMap::new();
        match kind {
            ReactionKind::Fulfill => {
                let s = self.alloc_str("fulfilled".to_string());
                map.set("status", s);
                map.set("value", value);
            }
            ReactionKind::Reject => {
                let s = self.alloc_str("rejected".to_string());
                map.set("status", s);
                map.set("reason", value);
            }
        }
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    /// Build an `AggregateError`-like object `{name, message, errors}` for a
    /// failed `Promise.any`.
    pub(crate) fn alloc_aggregate_error(&mut self, errors: Vec<Value>) -> Value {
        // name/message/errors are all non-enumerable own data properties.
        let attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut map = ObjMap::new();
        let name = self.alloc_str("AggregateError".to_string());
        map.define("name", name, attr);
        let msg = self.alloc_str("All promises were rejected".to_string());
        map.define("message", msg, attr);
        let errs = Value::heap(self.heap.alloc(HeapObj::Array(errors)));
        map.define("errors", errs, attr);
        let idx = self.heap.alloc(HeapObj::Object(map));
        self.error_data.insert(idx); // [[ErrorData]] internal slot
        // Link [[Prototype]] to %AggregateError.prototype% (error_protos[7]) so the
        // internally-created error is a real AggregateError instance:
        // getPrototypeOf === AggregateError.prototype and `.constructor` resolves to
        // AggregateError (instanceof already held via the error-ctor name check).
        if self.error_protos[7] != 0 {
            self.proto_of.insert(idx, Value::heap(self.error_protos[7]));
        }
        Value::heap(idx)
    }

    /// Resume (or start) a suspended async activation `idx` with `input` — the
    /// awaited value (fulfill) or the reason to throw in at the await point
    /// (reject). Runs until the next `await` (re-parks the window + subscribes to
    /// the awaited promise), a normal return (resolves the result Promise), or an
    /// uncaught throw (rejects it). Mirrors `generator_method`'s resume path, but
    /// restores the activation's `try` handlers so a rejection can be caught.
    pub(crate) fn drive_async(&mut self, idx: u32, input: Resume) {
        let (state, fid, closure, result) = match self.heap.get(idx) {
            HeapObj::AsyncState(a) => (a.state, a.func, a.closure, a.result),
            _ => return,
        };
        let resume_ip = match state {
            GenState::Completed | GenState::Running => return,
            GenState::Suspended(ip) => ip,
        };
        // Detach the saved window + handlers, then splice the window onto the top
        // of the live register file.
        let (saved, saved_handlers) = match self.heap.get_mut(idx) {
            HeapObj::AsyncState(a) => {
                a.state = GenState::Running;
                (std::mem::take(&mut a.regs), std::mem::take(&mut a.handlers))
            }
            _ => return,
        };
        let reg_count = saved.len();
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            // Can't make progress — abandon the activation and reject its result.
            if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                a.state = GenState::Completed;
                a.regs.clear();
                a.handlers.clear();
            }
            let e = self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.reject(result, e);
            return;
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
            new_target: Value::UNDEFINED,
            // The creating call's function value (named fn-expression
            // self-name identity survives suspension).
            callee: self.gen_callee.get(&idx).copied().unwrap_or(Value::UNDEFINED),
        });
        // Position the resume point and deliver the awaited value / rejection.
        let outcome = if resume_ip == usize::MAX {
            self.run_loop(stop)
        } else {
            match input {
                Resume::Value(v) => {
                    if let Instr::Await { dst, .. } =
                        self.func(fid as usize).code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = v;
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
                }
                Resume::Throw(e) => {
                    // Throw the rejection in at the await point: unwind to a
                    // handler within this activation (down to `stop`). If caught,
                    // resume at the catch; otherwise it propagates out as the
                    // function's rejection (pending_throw stays set for the Err
                    // arm below).
                    self.pending_throw = Some(e);
                    if self.unwind_to_handler(e, stop) {
                        self.pending_throw = None;
                        self.run_loop(stop)
                    } else {
                        Err(Thrown(String::new()))
                    }
                }
                // Resume::Return is only produced for an async GENERATOR at a yield*
                // delegate; a plain async function never receives it. Treat the value
                // like a normal await resumption defensively.
                Resume::Return(v) => {
                    if let Instr::Await { dst, .. } =
                        self.func(fid as usize).code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = v;
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
                }
            }
        };
        // Suspended again at an await?
        if let Some((awaited, await_ip, handlers)) = self.pending_await.take() {
            let back = self.regs.split_off(new_base);
            if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                a.state = GenState::Suspended(await_ip);
                a.regs = back;
                a.handlers = handlers;
            }
            let p = self.to_promise(awaited);
            self.settle_subscribe(p, idx);
            return;
        }
        // Otherwise the activation finished — settle `result`.
        match outcome {
            Ok(ret) => {
                if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                    a.state = GenState::Completed;
                    a.regs.clear();
                    a.handlers.clear();
                }
                if self.module_body_results.remove(&result) {
                    // Module body: the capability resolves with undefined —
                    // a thenable completion value must not be adopted
                    // (adoption makes a completed module look suspended and
                    // deadlocks its importers).
                    self.resolve(result, Value::UNDEFINED);
                } else {
                    self.resolve(result, ret);
                }
            }
            Err(_) => {
                let e = match self.pending_throw.take() {
                    Some(v) => v,
                    None => self.alloc_error_from_message("Error"),
                };
                // The unwind already truncated the window; keep regs consistent.
                self.regs.truncate(new_base);
                if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                    a.state = GenState::Completed;
                    a.regs.clear();
                    a.handlers.clear();
                }
                self.reject(result, e);
            }
        }
    }

    /// Run one microtask. A reaction's callback may be a JS function (re-enters
    /// the VM; a throw REJECTS the dependent, never unwinds the drain), a native
    /// BoundResolver, or undefined (pass-through). `AsyncResume` resumes an async
    /// activation (Stage 2).
    pub(crate) fn run_microtask(&mut self, t: Microtask) {
        match t {
            Microtask::Reaction { callback, arg, dependent, kind, finally } => {
                if finally {
                    // Run cb (no args) for its side effect, then forward the
                    // original value/reason — unless cb itself throws.
                    if !callback.is_undefined() {
                        if let Err(_) = self.call_value(callback, Value::UNDEFINED, &[]) {
                            let r = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                            self.reject(dependent, r);
                            return;
                        }
                    }
                    match kind {
                        ReactionKind::Fulfill => self.resolve(dependent, arg),
                        ReactionKind::Reject => self.reject(dependent, arg),
                    }
                    return;
                }
                if callback.is_undefined() {
                    match kind {
                        ReactionKind::Fulfill => self.resolve(dependent, arg),
                        ReactionKind::Reject => self.reject(dependent, arg),
                    }
                    return;
                }
                if callback.is_heap() {
                    if let HeapObj::BoundResolver { promise, is_reject } =
                        self.heap.get(callback.heap_index())
                    {
                        let (pr, isr) = (*promise, *is_reject);
                        if isr {
                            self.reject(pr, arg);
                        } else {
                            self.resolve(pr, arg);
                        }
                        return;
                    }
                    // A combinator reaction (Promise.all/allSettled/race/any). The
                    // kind comes from the reaction list (is_reject is for the
                    // direct-call path only).
                    if let HeapObj::CombinatorResolver { combinator, index, .. } =
                        self.heap.get(callback.heap_index())
                    {
                        let (c, i) = (*combinator, *index);
                        self.combinator_step(c, i, kind, arg);
                        return;
                    }
                }
                match self.call_value(callback, Value::UNDEFINED, &[arg]) {
                    Ok(ret) => self.resolve(dependent, ret),
                    Err(_) => {
                        let r = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                        self.reject(dependent, r);
                    }
                }
            }
            // Resumes a suspended async activation with the settled value (or by
            // throwing the rejection reason in at the await point). An async
            // generator routes to its own driver.
            Microtask::AsyncResume { activation, input } => {
                if matches!(self.heap.get(activation), HeapObj::AsyncGenerator(_)) {
                    self.drive_async_gen(activation, input);
                } else {
                    self.drive_async(activation, input);
                }
            }
            // PromiseResolveThenableJob: run then.call(thenable, resolveFn, rejectFn).
            // The resolving functions settle `promise` (one-shot via settle's Pending
            // guard, so a thenable that calls both / twice is handled). A throwing
            // `then` rejects the promise (a no-op if it already settled).
            Microtask::ThenableJob { thenable, then, promise } => {
                let res = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise, is_reject: false }),
                );
                let rej = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise, is_reject: true }),
                );
                if let Err(Thrown(msg)) = self.call_value(then, thenable, &[res, rej]) {
                    let e = match self.pending_throw.take() {
                        Some(v) => v,
                        None => self.alloc_error_from_message(&msg),
                    };
                    self.reject(promise, e);
                }
            }
        }
    }

    /// Drain the microtask queue to empty (FIFO; tasks enqueued during the drain
    /// run in the same drain). The whole event loop.
    pub(crate) fn drain_microtasks(&mut self) {
        while let Some(t) = self.microtasks.pop_front() {
            // The popped microtask `t` holds Values (callback/arg/dependent) in a
            // Rust local that are NOT reachable from the GC roots while
            // run_microtask re-enters the interpreter — suspend GC for its scope.
            {
                let _gc = self.gc_lock_guard();
                self.run_microtask(t);
            }
            // Between microtasks no un-rooted local survives, so reclaim now.
            self.maybe_gc();
        }
    }

    /// Deliver cross-thread `Atomics.notify` wakes: drain this Vm's mailbox
    /// and resolve each matching waitAsync promise "ok" (the notifier already
    /// removed the global-registry entry; this removes the local bookkeeping).
    /// An id with no surviving entry is dropped.
    fn drain_mailbox(&mut self) {
        let ids = std::mem::take(&mut *self.mailbox.wakes.lock().unwrap());
        if ids.is_empty() {
            return;
        }
        let mut to_wake: Vec<u32> = Vec::new();
        self.async_waiters.retain(|&(_, _, p, _, id)| {
            if id != 0 && ids.contains(&id) {
                to_wake.push(p);
                false
            } else {
                true
            }
        });
        for p in to_wake {
            let _gc = self.gc_lock_guard();
            let v = self.alloc_str("ok".to_string());
            self.resolve(p, v);
        }
    }

    /// The full event loop: deliver cross-thread waitAsync wakes, drain
    /// microtasks, then — while `$262.agent` timers or FINITE-deadline
    /// waitAsync waiters remain — sleep until the earliest due time ON THE
    /// MAILBOX CONDVAR (a remote `Atomics.notify` wakes the loop immediately
    /// instead of after the full timeout), fire what is due, and go again.
    /// Infinite-deadline waiters do not keep a Main loop alive; a WORKER
    /// agent parks for them — the main agent's notify is what ends them.
    pub(crate) fn run_event_loop(&mut self) {
        loop {
            // Cross-thread wakes FIRST: an "ok" resolution beats both the
            // microtask drain and the deadline bookkeeping below.
            self.drain_mailbox();
            self.drain_microtasks();
            let now = std::time::Instant::now();
            let next_timer = self.timer_queue.iter().map(|(d, _)| *d).min();
            let next_wait = self
                .async_waiters
                .iter()
                .filter_map(|(_, _, _, d, _)| *d)
                .min();
            let due = match (next_timer, next_wait) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let Some(due) = due else {
                if self.agent_role == agents::AgentRole::Worker
                    && !self.async_waiters.is_empty()
                {
                    // Worker with only infinite-deadline waiters: park until a
                    // notify lands in the mailbox (re-check under the lock —
                    // a wake may have arrived since the drain above).
                    let wakes = self.mailbox.wakes.lock().unwrap();
                    if wakes.is_empty() {
                        drop(self.mailbox.cv.wait(wakes).unwrap());
                    }
                    continue;
                }
                break;
            };
            if due > now {
                // Interruptible sleep: notify pushes into the mailbox and
                // signals this condvar (spurious wakeups just re-loop).
                let wakes = self.mailbox.wakes.lock().unwrap();
                if wakes.is_empty() {
                    drop(self.mailbox.cv.wait_timeout(wakes, due - now).unwrap().0);
                }
            }
            let now = std::time::Instant::now();
            // Fire due timers (FIFO among due ones by due-time order).
            let mut due_timers: Vec<usize> = (0..self.timer_queue.len())
                .filter(|&i| self.timer_queue[i].0 <= now)
                .collect();
            due_timers.sort_by_key(|&i| self.timer_queue[i].0);
            // Remove from the back so earlier indices stay valid.
            let mut fired: Vec<Value> = Vec::new();
            for &i in due_timers.iter().rev() {
                fired.push(self.timer_queue.remove(i).1);
            }
            fired.reverse();
            for cb in fired {
                let _gc = self.gc_lock_guard();
                let _ = self.call_value(cb, Value::UNDEFINED, &[]);
            }
            // Resolve due waitAsync waiters "timed-out" — DEREGISTERING under
            // the registry lock first. An entry already gone means a notify
            // won the race: skip the resolution and keep the local tuple; the
            // "ok" wake is already in the mailbox (notify delivers under the
            // registry lock) and the next iteration's drain resolves it.
            let waiters = std::mem::take(&mut self.async_waiters);
            let mut due_waiters: Vec<u32> = Vec::new();
            let mut kept = Vec::with_capacity(waiters.len());
            for w in waiters {
                let (buf, addr, p, d, id) = w;
                if !d.is_some_and(|d| d <= now) {
                    kept.push(w);
                    continue;
                }
                let timed_out = id == 0 || {
                    let mem_key = match self.heap.get(buf) {
                        HeapObj::ArrayBuffer { data, .. } => {
                            data.shared().map(|m| std::sync::Arc::as_ptr(m) as usize)
                        }
                        _ => None,
                    };
                    match mem_key {
                        Some(k) => agents::take_async_waiter((k, addr), id),
                        None => true,
                    }
                };
                if timed_out {
                    due_waiters.push(p);
                } else {
                    kept.push(w);
                }
            }
            self.async_waiters = kept;
            for p in due_waiters {
                let _gc = self.gc_lock_guard();
                let v = self.alloc_str("timed-out".to_string());
                self.resolve(p, v);
            }
        }
    }

    // ── property / index access ──

}
