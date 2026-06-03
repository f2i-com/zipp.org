#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Calling a `function*` does NOT run its body — it allocates a suspended
    /// Generator whose DETACHED register window holds `this` + the bound args
    /// (incl. a rest array). Resumed later by `generator_method`.
    pub(crate) fn alloc_generator(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        Value::heap(self.heap.alloc(HeapObj::Generator {
            func: func_id,
            closure,
            // `usize::MAX` = not-yet-started — distinct from a genuine yield/await
            // parked at ip 0 (which previously collided with this sentinel and made
            // the resume re-run from the top).
            state: GenState::Suspended(usize::MAX),
            regs,
        }))
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
            "return" => {
                // Complete the generator (v1 does not run finally blocks).
                if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                }
                Ok(Some(self.iter_result(arg0, true)))
            }
            "throw" => {
                if matches!(state, GenState::Completed) {
                    return Err(Thrown(self.throw_message(arg0)));
                }
                // v1: complete the generator and surface the throw at the call
                // site (no resume into a `try` inside the body).
                if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                }
                self.pending_throw = Some(arg0);
                Err(Thrown(self.throw_message(arg0)))
            }
            "next" => {
                let resume_ip = match state {
                    GenState::Completed => return Ok(Some(self.iter_result(Value::UNDEFINED, true))),
                    GenState::Running => {
                        return Err(Thrown("TypeError: generator is already running".into()))
                    }
                    GenState::Suspended(ip) => ip,
                };
                // Take the saved window out of the heap object and splice it onto
                // the top of the live register file.
                let saved = match self.heap.get_mut(idx) {
                    HeapObj::Generator { state, regs, .. } => {
                        *state = GenState::Running;
                        std::mem::take(regs)
                    }
                    _ => return Ok(None),
                };
                let reg_count = saved.len();
                let new_base = self.regs.len();
                if self.regs_would_overflow(new_base + reg_count) {
                    if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Suspended(resume_ip);
                        *regs = saved;
                    }
                    return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
                }
                self.regs.extend_from_slice(&saved);
                if new_base + reg_count > self.regs_hw {
                    self.regs_hw = new_base + reg_count;
                }
                // First next() runs from ip 0; a later one resumes after the Yield,
                // delivering the sent value into the yield expression's dst.
                let ip = if resume_ip == usize::MAX {
                    0
                } else {
                    if let Instr::Yield { dst, .. } =
                        self.func(fid as usize).code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = arg0;
                    }
                    resume_ip + 1
                };
                let stop = self.frames.len();
                self.frames.push(Frame {
                    func: fid,
                    base: new_base,
                    ip,
                    ret_dst: 0,
                    closure,
                    handlers: Vec::new(),
                });
                let outcome = self.run_loop(stop);
                if let Some((y, yield_ip)) = self.pending_yield.take() {
                    // Suspended: the window is still live at [new_base..]; park it.
                    let back = self.regs.split_off(new_base);
                    if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Suspended(yield_ip);
                        *regs = back;
                    }
                    return Ok(Some(self.iter_result(y, false)));
                }
                match outcome {
                    Ok(ret) => {
                        // Returned / fell off the end (pop_frame_with already truncated).
                        if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                            *state = GenState::Completed;
                            regs.clear();
                        }
                        Ok(Some(self.iter_result(ret, true)))
                    }
                    Err(t) => {
                        self.regs.truncate(new_base);
                        if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                            *state = GenState::Completed;
                            regs.clear();
                        }
                        Err(t)
                    }
                }
            }
            _ => Ok(None),
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
            if matches!(self.heap.get(value.heap_index()), HeapObj::Promise { .. }) {
                let inner = value.heap_index();
                self.then_internal(inner, Value::UNDEFINED, Value::UNDEFINED, Some(p));
                return;
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

    /// `p.finally(cb)`: register a finally reaction on both settle paths (or
    /// schedule immediately if already settled). Returns the dependent promise.
    pub(crate) fn finally_internal(&mut self, p: u32, cb: Value) -> u32 {
        let dep = self.alloc_promise();
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => return dep,
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction { callback: cb, dependent: dep, finally: true, is_async: false });
                    reject.push(Reaction { callback: cb, dependent: dep, finally: true, is_async: false });
                }
            }
            PromiseState::Fulfilled => self.microtasks.push_back(Microtask::Reaction {
                callback: cb,
                arg: result,
                dependent: dep,
                kind: ReactionKind::Fulfill,
                finally: true,
            }),
            PromiseState::Rejected => self.microtasks.push_back(Microtask::Reaction {
                callback: cb,
                arg: result,
                dependent: dep,
                kind: ReactionKind::Reject,
                finally: true,
            }),
        }
        dep
    }

    // ── async functions ──

    /// Build a suspended `async function` activation and run it synchronously up
    /// to its first `await` (or to completion / a throw). Returns the activation's
    /// result Promise — the value an `async` call evaluates to.
    pub(crate) fn alloc_async(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        let result = self.alloc_promise();
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
        // Run from the top until the first await suspends it (or it finishes —
        // settling `result` either way).
        self.drive_async(idx, Resume::Value(Value::UNDEFINED));
        Value::heap(result)
    }

    /// Calling an `async function*` builds a suspended AsyncGenerator (an async
    /// iterator). It does NOT run until the first `.next()`.
    pub(crate) fn alloc_async_generator(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        Value::heap(self.heap.alloc(HeapObj::AsyncGenerator(Box::new(AsyncGenState {
            func: func_id,
            closure,
            // NOTE: async generators keep the legacy `Suspended(0)` "fresh"
            // sentinel for now — switching them to `usize::MAX` (as plain
            // generators / async functions do) regressed AsyncGeneratorPrototype,
            // so their first-instruction-yield edge stays a known limitation.
            state: GenState::Suspended(0),
            regs,
            handlers: Vec::new(),
            queue: Vec::new(),
        }))))
    }

    /// `.next()`/`.return()`/`.throw()` on an async generator. Each returns a
    /// Promise that settles when the body next yields/returns/throws. The result
    /// promise is queued; the driver services the queue FIFO.
    pub(crate) fn async_generator_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Option<Value> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let p = self.alloc_promise();
        match name {
            "next" => {
                if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                    g.queue.push(p);
                }
                // Only kick the driver if the generator is idle at a yield (or not
                // started, or completed-to-drain). If it's awaiting a promise or
                // already running, the in-flight resume services the queue when it
                // next yields — resuming now would deliver the wrong value.
                if self.async_gen_should_drive(idx) {
                    self.drive_async_gen(idx, Resume::Value(arg0));
                }
            }
            "return" => {
                // Force completion: settle with { value: arg, done: true }. (v1
                // does not resume `finally` blocks inside the body.)
                if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                    g.state = GenState::Completed;
                    g.regs.clear();
                    g.handlers.clear();
                }
                let r = self.iter_result(arg0, true);
                self.resolve(p, r);
            }
            "throw" => {
                if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                    g.state = GenState::Completed;
                    g.regs.clear();
                    g.handlers.clear();
                }
                self.reject(p, arg0);
            }
            _ => return None,
        }
        Some(Value::heap(p))
    }

    /// Whether a fresh `.next()` should immediately drive the async generator: yes
    /// if it's suspended at a `yield` (or hasn't started, or has completed — to
    /// drain the queued promise as done); NO if it's awaiting a promise or already
    /// running (the in-flight resume will service the queue at its next yield).
    pub(crate) fn async_gen_should_drive(&self, idx: u32) -> bool {
        match self.heap.get(idx) {
            HeapObj::AsyncGenerator(g) => match g.state {
                GenState::Completed => true,
                GenState::Running => false,
                GenState::Suspended(ip) => {
                    ip == 0
                        || matches!(
                            self.func(g.func as usize).code.get(ip),
                            Some(Instr::Yield { .. })
                        )
                }
            },
            _ => false,
        }
    }

    /// Resolve every still-queued `.next()` promise with `{ value: undefined,
    /// done: true }` — called once the async generator has completed.
    pub(crate) fn async_gen_drain_done(&mut self, idx: u32) {
        loop {
            let p = match self.heap.get_mut(idx) {
                HeapObj::AsyncGenerator(g) if !g.queue.is_empty() => g.queue.remove(0),
                _ => break,
            };
            let r = self.iter_result(Value::UNDEFINED, true);
            self.resolve(p, r);
        }
    }

    /// Advance an async generator: run its body until the next `yield` (resolve
    /// the front queued promise with `{value, done:false}`), `await` (park +
    /// subscribe, the promise stays pending), or return/throw (settle + drain).
    /// `input` delivers the `.next()` argument or a settled awaited value/throw.
    pub(crate) fn drive_async_gen(&mut self, idx: u32, input: Resume) {
        let (state, fid, closure) = match self.heap.get(idx) {
            HeapObj::AsyncGenerator(g) => (g.state, g.func, g.closure),
            _ => return,
        };
        let resume_ip = match state {
            GenState::Completed => return self.async_gen_drain_done(idx),
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
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                if !g.queue.is_empty() {
                    let p = g.queue.remove(0);
                    self.reject(p, e);
                }
            }
            self.async_gen_drain_done(idx);
            return;
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame {
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
        });
        // Resume after the suspending op, delivering the sent/awaited value. The
        // op at `resume_ip` is a Yield (resumed by `.next(v)`) or Await (resumed
        // by a settled promise) — both write the value into the op's `dst`.
        // (Async generators use the legacy `0` fresh sentinel — see alloc note.)
        let outcome = if resume_ip == 0 {
            self.run_loop(stop)
        } else {
            match input {
                Resume::Value(v) => {
                    let dst = match self.func(fid as usize).code[resume_ip] {
                        Instr::Yield { dst, .. } => Some(dst),
                        Instr::Await { dst, .. } => Some(dst),
                        _ => None,
                    };
                    if let Some(d) = dst {
                        self.regs[new_base + d as usize] = v;
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
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
            let back = self.regs.split_off(new_base);
            let front = match self.heap.get_mut(idx) {
                HeapObj::AsyncGenerator(g) => {
                    g.state = GenState::Suspended(yield_ip);
                    g.regs = back;
                    (!g.queue.is_empty()).then(|| g.queue.remove(0))
                }
                _ => None,
            };
            if let Some(p) = front {
                let r = self.iter_result(y, false);
                self.resolve(p, r);
            }
            // More `.next()` calls already queued → service the next one now.
            if matches!(self.heap.get(idx), HeapObj::AsyncGenerator(g) if !g.queue.is_empty()) {
                self.drive_async_gen(idx, Resume::Value(Value::UNDEFINED));
            }
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
                if let Some(p) = front {
                    let r = self.iter_result(ret, true);
                    self.resolve(p, r);
                }
                self.async_gen_drain_done(idx);
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
                if let Some(p) = front {
                    self.reject(p, reason);
                }
                self.async_gen_drain_done(idx);
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
    pub(crate) fn promise_combine(&mut self, kind: crate::heap::CombKind, iterable: Value) -> Result<Value, Thrown> {
        use crate::heap::CombKind;
        // GetIterator / iteration abrupt completion → a REJECTED promise, not a
        // synchronous throw (IfAbruptRejectPromise): `Promise.all(1)` rejects with
        // a TypeError rather than throwing out of the call.
        let inputs = match self.iterate_to_vec(iterable) {
            Ok(v) => v,
            Err(Thrown(msg)) => {
                let result = self.alloc_promise();
                let err = self.alloc_error_from_message(&msg);
                self.reject(result, err);
                return Ok(Value::heap(result));
            }
        };
        let total = inputs.len() as u32;
        let result = self.alloc_promise();
        if total == 0 {
            // Empty-iterable terminal cases (race stays pending forever).
            match kind {
                CombKind::All | CombKind::AllSettled => {
                    let arr = Value::heap(self.heap.alloc(HeapObj::Array(Vec::new())));
                    self.resolve(result, arr);
                }
                CombKind::Any => {
                    let e = self.alloc_aggregate_error(Vec::new());
                    self.reject(result, e);
                }
                CombKind::Race => {}
            }
            return Ok(Value::heap(result));
        }
        let comb = self.heap.alloc(HeapObj::Combinator {
            kind,
            results: vec![Value::UNDEFINED; total as usize],
            remaining: total,
            result,
        });
        for (i, inp) in inputs.into_iter().enumerate() {
            let p = self.to_promise(inp);
            let resolver = Value::heap(self.heap.alloc(HeapObj::CombinatorResolver {
                combinator: comb,
                index: i as u32,
            }));
            // Both settle paths route to the resolver (it dispatches on the kind).
            self.then_internal(p, resolver, resolver, None);
        }
        Ok(Value::heap(result))
    }

    /// Perform one combinator step: the input at `index` settled (`kind`) with
    /// `value`. Updates the shared state and settles the combinator's promise
    /// when its rule is met (the one-shot `settle` guard absorbs later inputs).
    pub(crate) fn combinator_step(&mut self, comb: u32, index: u32, kind: ReactionKind, value: Value) {
        use crate::heap::CombKind;
        let (ckind, result) = match self.heap.get(comb) {
            HeapObj::Combinator { kind, result, .. } => (*kind, *result),
            _ => return,
        };
        match (ckind, kind) {
            (CombKind::Race, ReactionKind::Fulfill) => self.resolve(result, value),
            (CombKind::Race, ReactionKind::Reject) => self.reject(result, value),
            (CombKind::All, ReactionKind::Reject) => self.reject(result, value),
            (CombKind::Any, ReactionKind::Fulfill) => self.resolve(result, value),
            (CombKind::All, ReactionKind::Fulfill)
            | (CombKind::Any, ReactionKind::Reject)
            | (CombKind::AllSettled, _) => {
                // Record the per-input outcome and decrement the outstanding count.
                let stored = if ckind == CombKind::AllSettled {
                    self.make_settled_record(kind, value)
                } else {
                    value
                };
                let done = if let HeapObj::Combinator { results, remaining, .. } =
                    self.heap.get_mut(comb)
                {
                    results[index as usize] = stored;
                    *remaining -= 1;
                    *remaining == 0
                } else {
                    false
                };
                if done {
                    let collected = match self.heap.get(comb) {
                        HeapObj::Combinator { results, .. } => results.clone(),
                        _ => Vec::new(),
                    };
                    match ckind {
                        CombKind::Any => {
                            // All inputs rejected → AggregateError of the reasons.
                            let e = self.alloc_aggregate_error(collected);
                            self.reject(result, e);
                        }
                        _ => {
                            let arr = Value::heap(self.heap.alloc(HeapObj::Array(collected)));
                            self.resolve(result, arr);
                        }
                    }
                }
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
        let mut map = ObjMap::new();
        let name = self.alloc_str("AggregateError".to_string());
        map.set("name", name);
        let msg = self.alloc_str("All promises were rejected".to_string());
        map.set("message", msg);
        let errs = Value::heap(self.heap.alloc(HeapObj::Array(errors)));
        map.set("errors", errs);
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
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
        self.frames.push(Frame {
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
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
                self.resolve(result, ret);
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
                    // A combinator reaction (Promise.all/allSettled/race/any).
                    if let HeapObj::CombinatorResolver { combinator, index } =
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

    // ── property / index access ──

}
