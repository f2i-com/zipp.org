// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'a> FnCompiler<'a> {
    pub(crate) fn yield_expr(&mut self, y: &ox::YieldExpression, dst: Reg) -> R<Reg> {
        if !self.in_generator {
            return Err("`yield` is only valid inside a generator (function*)".into());
        }
        if y.delegate {
            let arg = y.argument.as_ref().ok_or("yield* requires an operand")?;
            if self.in_async {
                // ASYNC `yield*` (delegation inside an `async function*`): drive the
                // operand's ASYNC iterator, awaiting each step exactly like the working
                // `for await` codegen, and async-yield each value; the `yield*`
                // expression evaluates to the inner iterator's final `{value}` once it
                // is done. Uses only existing, proven ops (GetAsyncIterator /
                // ForAwaitNext / Await / Yield) — no VM change, and no ip-0 suspension
                // so the iter-170 resume-delivery hazard does not apply.
                //
                // SCOPE (minimal slice): next-OUT delegation + completion value + async
                // iterator acquisition + error propagation (an abrupt operand / inner
                // next() unwinds the async-gen activation, rejecting its front promise).
                // NOT yet: forwarding the value sent to the OUTER .next(v) into the
                // inner iterator (ForAwaitNext calls next() with no arg, so the sent
                // value is discarded into `sink`), nor delegating the outer
                // .throw()/.return() into the inner iterator (those force-complete the
                // async gen) — both are a separate, larger follow-up.
                let save = self.next_reg;
                // All registers are allocated once and kept stable: the whole window is
                // saved/restored across each suspension, and the non-linear control flow
                // (the throw-delegation catch jumps back into the loop) makes
                // per-iteration reclaim unsafe.
                let iter = self.alloc_reg();
                let v = self.expr_into(arg, iter)?;
                if v != iter {
                    self.emit(Instr::Move { dst: iter, src: v });
                }
                // Whether the INNER iterator is a sync one (the @@iterator
                // fallback / a raw array): its values get the AsyncFromSync
                // await-unwrap before each yield.
                let is_sync = self.alloc_reg();
                self.emit(Instr::GetAsyncIterator { dst: iter, src: iter, sync_dst: is_sync });
                let idx = self.alloc_reg();
                self.emit(Instr::LoadInt { dst: idx, val: 0 });
                // Cache the inner iterator's `next` ONCE (IteratorRecord.[[NextMethod]]),
                // matching the spec's get-next-once ordering for a user iterator.
                let next_fn = self.alloc_reg();
                let next_name = self.string_name("next");
                self.emit(Instr::GetProp { dst: next_fn, obj: iter, name: next_name });
                let excr = self.alloc_reg(); // catch reg for an injected outer .throw()
                let cerr = self.alloc_reg(); // abrupt unwrap completion (close-on-rejection)
                let step = self.alloc_reg();
                let r = self.alloc_reg();
                let done = self.alloc_reg();
                let value = self.alloc_reg();
                // The value sent to the OUTER `.next(v)` — forwarded into the inner
                // iterator's next() each step (initially undefined). The
                // AsyncYieldDelegate resume writes the new sent value here.
                let sent = self.alloc_reg();
                self.emit(Instr::LoadUndefined { dst: sent });
                let tstep = self.alloc_reg();
                let taw = self.alloc_reg();
                let mode = self.alloc_reg(); // resume mode from AsyncYieldDelegate: 0 next / 2 return
                let hasret = self.alloc_reg(); // does the inner iterator have a `return`?
                let done_name = self.string_name("done");
                let value_name = self.string_name("value");
                // --- one next(sent) step: r = await iter.next(sent); require Object ---
                let top = self.here();
                self.emit(Instr::AsyncIterNextStep { dst: step, iter, idx, sent, next_fn });
                self.emit(Instr::Await { dst: r, val: step });
                self.emit(Instr::RequireObject { val: r });
                self.emit(Instr::GetProp { dst: done, obj: r, name: done_name });
                let jdone = self.here();
                self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // done → yield* value (r.value)
                self.emit(Instr::GetProp { dst: value, obj: r, name: value_name });
                // AsyncFromSyncIterator unwrap (AsyncFromSyncIteratorContinuation,
                // closeOnRejection = true): a SYNC inner iterator's stepped value
                // is AWAITED before the async-yield; an abrupt unwrap — the Await's
                // observable PromiseResolve (a poisoned `constructor`) or a
                // rejected value-promise — first does IteratorClose(inner) QUIETLY,
                // then re-throws the ORIGINAL reason (closing the generator and
                // rejecting the front promise). An ASYNC inner iterator's value is
                // yielded as-is (yield-star-promise-not-unwrapped). The
                // throw-delegation's not-done path re-enters here at `unwrap_pt`.
                let unwrap_pt = self.here();
                let jskip_aw = self.here();
                self.emit(Instr::JumpIfFalse { cond: is_sync, target: 0 });
                let ph_aw = self.here();
                self.emit(Instr::PushHandler { catch_target: 0, catch_reg: cerr });
                self.handler_depth += 1;
                self.emit(Instr::Await { dst: value, val: value });
                self.emit(Instr::PopHandler);
                self.handler_depth -= 1;
                let jaw_ok = self.here();
                self.emit(Instr::Jump { target: 0 });
                let close_catch = self.here();
                if let Instr::PushHandler { catch_target, .. } = &mut self.code[ph_aw as usize] {
                    *catch_target = close_catch;
                }
                self.emit(Instr::IterCloseQuiet { iter });
                self.emit(Instr::Throw { src: cerr });
                let after_aw = self.here();
                self.patch_jump(jskip_aw, after_aw);
                self.patch_jump(jaw_ok, after_aw);
                // --- (async-)yield the value, with a handler that delegates an outer
                //     .throw() into the inner iterator's `throw` ---
                let yield_pt = self.here();
                let ph = self.here();
                self.emit(Instr::PushHandler { catch_target: 0, catch_reg: excr });
                self.handler_depth += 1;
                // Suspend; resume delivers (mode, value) into (mode, sent).
                self.emit(Instr::AsyncYieldDelegate { mode_dst: mode, val_dst: sent, val: value });
                self.emit(Instr::PopHandler);
                self.handler_depth -= 1;
                // mode 2 (return) → return-delegation; mode 0 (next, falsy) → loop.
                let jret = self.here();
                self.emit(Instr::JumpIfTrue { cond: mode, target: 0 });
                self.emit(Instr::Jump { target: top });
                // --- return delegation: outer .return(sent). Delegate to inner.return;
                //     no method → outer returns `sent`; else await, then finish-return
                //     (done) or yield the value and continue. ---
                let ret_label = self.here();
                self.patch_jump(jret, ret_label);
                self.emit(Instr::AsyncIterReturnStep { dst: tstep, has_dst: hasret, iter, ret: sent });
                let jhas = self.here();
                self.emit(Instr::JumpIfTrue { cond: hasret, target: 0 });
                // No inner `return` method: the received return value is AWAITED
                // (spec: if return is undefined and generatorKind is async, set
                // received.[[Value]] to ? Await(received.[[Value]])) before the
                // generator returns it — a thenable is adopted (observable `then`
                // read + one tick), and a rejection unwinds instead.
                self.emit(Instr::Await { dst: sent, val: sent });
                self.emit(Instr::Return { src: sent });
                let has_ret = self.here();
                self.patch_jump(jhas, has_ret);
                self.emit(Instr::Await { dst: taw, val: tstep });
                self.emit(Instr::RequireObject { val: taw });
                self.emit(Instr::GetProp { dst: done, obj: taw, name: done_name });
                let jretdone = self.here();
                self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // inner.return done → generator returns value
                self.emit(Instr::GetProp { dst: value, obj: taw, name: value_name });
                self.emit(Instr::Jump { target: yield_pt }); // not done → yield value, continue
                let ret_done = self.here();
                self.patch_jump(jretdone, ret_done);
                self.emit(Instr::GetProp { dst: value, obj: taw, name: value_name });
                // A SYNC inner's `return()` result value is unwrapped by the
                // AsyncFromSync continuation (closeOnRejection = false here):
                // await it before completing (iterator-result-unwrap-promise).
                let jrv = self.here();
                self.emit(Instr::JumpIfFalse { cond: is_sync, target: 0 });
                self.emit(Instr::Await { dst: value, val: value });
                let after_rv = self.here();
                self.patch_jump(jrv, after_rv);
                self.emit(Instr::Return { src: value }); // generator returns inner.return's value
                // --- catch: an outer .throw(excr) was injected here. Delegate to the
                //     inner iterator's throw (or TypeError if it has none), await the
                //     result, then either finish (done) or yield the delegated value. ---
                let catch_label = self.here();
                if let Instr::PushHandler { catch_target, .. } = &mut self.code[ph as usize] {
                    *catch_target = catch_label;
                }
                self.emit(Instr::AsyncIterThrowStep { dst: tstep, iter, exc: excr });
                self.emit(Instr::Await { dst: taw, val: tstep });
                self.emit(Instr::RequireObject { val: taw });
                self.emit(Instr::GetProp { dst: done, obj: taw, name: done_name });
                let jdone2 = self.here();
                self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // inner.throw done → value
                self.emit(Instr::GetProp { dst: value, obj: taw, name: value_name });
                // Not done: a SYNC inner's delegated value takes the same
                // close-on-rejection unwrap as a next() step (the AsyncFromSync
                // throw() continuation also has closeOnRejection = true), then
                // the yield at `yield_pt`.
                self.emit(Instr::Jump { target: unwrap_pt });
                // done via inner.throw: yield* value = taw.value (awaited for a
                // SYNC inner — the continuation unwraps it; done == true means
                // no close-on-rejection, a plain await).
                let done_label2 = self.here();
                self.patch_jump(jdone2, done_label2);
                self.emit(Instr::GetProp { dst, obj: taw, name: value_name });
                let jtv = self.here();
                self.emit(Instr::JumpIfFalse { cond: is_sync, target: 0 });
                self.emit(Instr::Await { dst, val: dst });
                let after_tv = self.here();
                self.patch_jump(jtv, after_tv);
                let jend = self.here();
                self.emit(Instr::Jump { target: 0 });
                // done via next(): yield* value = r.value.
                let done_label = self.here();
                self.patch_jump(jdone, done_label);
                self.emit(Instr::GetProp { dst, obj: r, name: value_name });
                let end = self.here();
                self.patch_jump(jend, end);
                self.next_reg = save;
                return Ok(dst);
            }
            // SYNC `yield*` — the full delegation protocol (spec 14.4.14 step 5):
            // GetIterator, then a loop that drives the inner iterator per the OUTER
            // generator's resume mode (next/throw/return), forwarding sent values,
            // `gen.throw()`, and `gen.return()`. `IterDelegate` performs one step
            // (incl. the missing-method rules) and classifies the outcome;
            // `YieldDelegate` yields and on resume delivers (mode, value).
            let save = self.next_reg;
            let iter = self.alloc_reg();
            let v = self.expr_into(arg, iter)?;
            if v != iter {
                self.emit(Instr::Move { dst: iter, src: v });
            }
            self.emit(Instr::GetIteratorObj { dst: iter, src: iter });
            let mode = self.alloc_reg();
            self.emit(Instr::LoadInt { dst: mode, val: 0 }); // start as a `next`
            let sent = self.alloc_reg();
            self.emit(Instr::LoadUndefined { dst: sent });
            let value = self.alloc_reg();
            let done = self.alloc_reg();
            let ret = self.alloc_reg();
            let top = self.here();
            self.emit(Instr::IterDelegate {
                value_dst: value,
                done_dst: done,
                ret_dst: ret,
                iter,
                mode,
                sent,
            });
            let jret = self.here();
            self.emit(Instr::JumpIfTrue { cond: ret, target: 0 }); // → generator return
            let jdone = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // → yield* completes
            // Neither: yield the value; on resume (mode,sent) are delivered, loop.
            self.emit(Instr::YieldDelegate { mode_dst: mode, val_dst: sent, val: value });
            self.emit(Instr::Jump { target: top });
            // ret: the inner iterator's return (or a missing one) ends the generator.
            let ret_label = self.here();
            self.patch_jump(jret, ret_label);
            self.emit(Instr::Return { src: value });
            // done: the `yield*` expression evaluates to the inner's final value.
            let done_label = self.here();
            self.patch_jump(jdone, done_label);
            if dst != value {
                self.emit(Instr::Move { dst, src: value });
            }
            self.next_reg = save;
            return Ok(dst);
        }
        // Evaluate the yielded value (undefined for a bare `yield`); on resume
        // the value passed to `.next(v)` lands in `dst`.
        let val = match &y.argument {
            Some(e) => self.expr(e)?,
            None => {
                let t = self.temp();
                self.emit(Instr::LoadUndefined { dst: t });
                t
            }
        };
        // In an ASYNC generator, `yield v` is AsyncGeneratorYield: it first AWAITs the
        // value (so `yield Promise.reject(e)` rejects the consumer's `.next()`, and
        // `yield Promise.resolve(x)` yields the unwrapped `x`), then suspends. The
        // Await and the Yield are distinct suspension points, each resumed
        // independently, so the resume `.next(v)` value still lands in `dst`.
        if self.in_async {
            let awaited = self.temp();
            self.emit(Instr::Await { dst: awaited, val });
            self.emit(Instr::Yield { dst, val: awaited });
        } else {
            self.emit(Instr::Yield { dst, val });
        }
        Ok(dst)
    }

    pub(crate) fn await_expr(&mut self, a: &ox::AwaitExpression, dst: Reg) -> R<Reg> {
        if !self.in_async {
            return Err("`await` is only valid inside an async function".into());
        }
        // Evaluate the awaited value; on resume the settled result (or a thrown
        // rejection) lands in `dst`. The VM coerces non-promises via Promise.resolve.
        let val = self.expr(&a.argument)?;
        self.emit(Instr::Await { dst, val });
        Ok(dst)
    }

    pub(crate) fn conditional(&mut self, c: &ox::ConditionalExpression, dst: Reg) -> R<Reg> {
        let cond = self.expr(&c.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        let t = self.expr_into(&c.consequent, dst)?;
        if t != dst {
            self.emit(Instr::Move { dst, src: t });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let else_start = self.here();
        self.patch_jump(jf, else_start);
        let e = self.expr_into(&c.alternate, dst)?;
        if e != dst {
            self.emit(Instr::Move { dst, src: e });
        }
        let end = self.here();
        self.patch_jump(jmp, end);
        Ok(dst)
    }

    /// Build an Error-like object `{ name, message }` for `Error("msg")` (called
    /// either with `new` or bare). `arg` is the optional message argument.
    /// Emit `NewRegExp` for `RegExp(pattern?, flags?)` / `new RegExp(...)` — the
    /// VM coerces a string/RegExp pattern and the flags (undefined → defaults).
    pub(crate) fn emit_regexp(&mut self, args: &[ox::Argument], dst: Reg, is_construct: bool) -> R<Reg> {
        let pt = self.temp();
        match args.first().and_then(|a| a.as_expression()) {
            Some(e) => {
                let v = self.expr_into(e, pt)?;
                if v != pt {
                    self.emit(Instr::Move { dst: pt, src: v });
                }
            }
            None => self.emit(Instr::LoadUndefined { dst: pt }),
        }
        let ft = self.temp();
        match args.get(1).and_then(|a| a.as_expression()) {
            Some(e) => {
                let v = self.expr_into(e, ft)?;
                if v != ft {
                    self.emit(Instr::Move { dst: ft, src: v });
                }
            }
            None => self.emit(Instr::LoadUndefined { dst: ft }),
        }
        self.emit(Instr::NewRegExp { dst, pattern: pt, flags: ft, is_construct });
        self.next_reg -= 2;
        Ok(dst)
    }

    pub(crate) fn build_error(&mut self, kind: &str, args: &[ox::Argument], dst: Reg) -> R<Reg> {
        // `new TypeError(msg)` etc. → a proto-linked error instance (NewError op
        // sets own `name`/`message` and links the prototype so `.constructor`,
        // `.toString`, and `instanceof` resolve). AggregateError takes the message
        // as its SECOND argument (`new AggregateError(errors, message)`).
        let kidx = error_kind_index(kind);
        let msg_pos = if kidx == 7 { 1 } else { 0 };
        // AggregateError's first argument is the `errors` iterable. Evaluate it FIRST
        // (matching left-to-right argument evaluation); the NewError op IterableToList's
        // it into a non-enumerable own `errors` array, AFTER coercing the message.
        let errors = if kidx == 7 {
            match args.first().and_then(|a| a.as_expression()) {
                Some(e) => {
                    let t = self.temp();
                    let v = self.expr_into(e, t)?;
                    if v != t {
                        self.emit(Instr::Move { dst: t, src: v });
                    }
                    Some(t)
                }
                None => None,
            }
        } else {
            None
        };
        let arg = match args.get(msg_pos).and_then(|a| a.as_expression()) {
            Some(e) => {
                let t = self.temp();
                let v = self.expr_into(e, t)?;
                if v != t {
                    self.emit(Instr::Move { dst: t, src: v });
                }
                Some(t)
            }
            None => None,
        };
        // The options object follows the message (`new Error(msg, options)`,
        // `new AggregateError(errors, msg, options)`) — its `cause` becomes the
        // error's `cause` (NewError installs it).
        let opts = match args.get(msg_pos + 1).and_then(|a| a.as_expression()) {
            Some(e) => {
                let t = self.temp();
                let v = self.expr_into(e, t)?;
                if v != t {
                    self.emit(Instr::Move { dst: t, src: v });
                }
                Some(t)
            }
            None => None,
        };
        self.emit(Instr::NewError { dst, kind: kidx, arg, opts, errors });
        // Reclaim the errors + message + options temps (allocated in that order).
        self.next_reg -=
            errors.is_some() as Reg + arg.is_some() as Reg + opts.is_some() as Reg;
        Ok(dst)
    }

    pub(crate) fn call(&mut self, c: &ox::CallExpression, dst: Reg) -> R<Reg> {
        // Optional call `f?.(args)` — EvaluateCall: a MEMBER callee (even
        // through parens) preserves its base as `this`; `super.m?.()` binds the
        // running `this`. The base's own `?.` links short-circuit inside the
        // chain, and a nullish callee bails to undefined WITHOUT evaluating
        // the arguments.
        if c.optional {
            let mut inner: &ox::Expression = &c.callee;
            while let ox::Expression::ParenthesizedExpression(p) = inner {
                inner = &p.expression;
            }
            let has_spread =
                c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_)));
            match inner {
                ox::Expression::StaticMemberExpression(m)
                    if !matches!(&m.object, ox::Expression::Super(_)) && !has_spread =>
                {
                    let o = self.expr(&m.object)?;
                    let obj = self.alloc_reg();
                    if o != obj {
                        self.emit(Instr::Move { dst: obj, src: o });
                    }
                    if m.optional {
                        self.emit_optional_check(obj);
                    }
                    let name = self.string_name(m.property.name.as_str());
                    let callee = self.alloc_reg();
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
                ox::Expression::ComputedMemberExpression(m)
                    if !matches!(&m.object, ox::Expression::Super(_)) && !has_spread =>
                {
                    let o = self.expr(&m.object)?;
                    let obj = self.alloc_reg();
                    if o != obj {
                        self.emit(Instr::Move { dst: obj, src: o });
                    }
                    if m.optional {
                        self.emit_optional_check(obj);
                    }
                    let key = self.expr(&m.expression)?;
                    let callee = self.alloc_reg();
                    self.emit(Instr::GetIndex { dst: callee, obj, key });
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
                ox::Expression::PrivateFieldExpression(p) if !has_spread => {
                    self.check_private_declared(&p.field.name)?;
                    let o = self.expr(&p.object)?;
                    let obj = self.alloc_reg();
                    if o != obj {
                        self.emit(Instr::Move { dst: obj, src: o });
                    }
                    if p.optional {
                        self.emit_optional_check(obj);
                    }
                    let name = self.string_name(&private_key(&p.field.name));
                    let callee = self.alloc_reg();
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
                // `super.m?.()` / `super[k]?.()`: the member lowering performs
                // the read (incl. the this-TDZ check); `this` is frame reg 0.
                ox::Expression::StaticMemberExpression(_)
                | ox::Expression::ComputedMemberExpression(_)
                    if !has_spread =>
                {
                    let callee = self.expr(inner)?;
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: 0, arg_base, argc });
                    return Ok(dst);
                }
                // `(a?.b)?.()`: a parenthesized-chain member callee still
                // binds `this` = base; the inner chain's bail lands the
                // callee at undefined, then the outer `?.()` bails on it.
                ox::Expression::ChainExpression(ce) if !has_spread => {
                    if let Some((callee, obj)) = self.chain_member_callee(ce)? {
                        self.emit_optional_check(callee);
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                        return Ok(dst);
                    }
                }
                _ => {}
            }
            let callee = self.expr(&c.callee)?;
            self.emit_optional_check(callee);
            if has_spread {
                // `fn?.(...xs)`: spread args after the nullish bail.
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallSpread { dst, callee, args: args_arr });
                return Ok(dst);
            }
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::Call { dst, callee, arg_base, argc });
            return Ok(dst);
        }
        // Spread call: `f(...args)`, `obj.m(...args)`, `arr.push(...xs)`, etc.
        // Build the argument list as an array (spreading each `...x` element),
        // then dispatch via CallMethodSpread (method receiver) or CallSpread
        // (plain function value). Spread on a builtin like Math.max(...arr) that
        // isn't a method call is out of scope.
        if c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_))) {
            // `super(...args)` — spread into the superclass constructor. Handled
            // here (before the generic branches) because `super` is not a value
            // and would fail `expr(callee)`.
            if matches!(&c.callee, ox::Expression::Super(_)) {
                if !self.derived_class {
                    return Err("`super(...)` is only valid in a derived class constructor".into());
                }
                let pid = self
                    .super_class
                    .ok_or("`super(...)` is only valid in a derived class constructor")?;
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::SuperCtorSpread { home_class_id: pid, args: args_arr });
                // `super(...)` evaluates to the new bound `this` (BindThisValue's
                // result) — SuperCtorSpread rebinds reg 0 to it (call-expr-value).
                self.emit(Instr::Move { dst, src: 0 });
                return Ok(dst);
            }
            // `Math.max(...arr)` / `Math.min(...arr)` / `Math.hypot(...arr)` —
            // a variadic Math reduction over the spread array.
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                if let ox::Expression::Identifier(obj) = &m.object {
                    if obj.name == "Math" {
                        if let Some(op) = crate::bytecode::MathFn::from_name(m.property.name.as_str()) {
                            let args_arr = self.build_spread_args(&c.arguments)?;
                            self.emit(Instr::MathSpread { dst, op, args: args_arr });
                            return Ok(dst);
                        }
                    }
                }
            }
            // Direct `eval(...args)`: a spread call of the unshadowed global
            // `eval` is STILL a direct eval — the spread list's first element
            // is the code argument (extras are ignored, like eval(a, b)).
            if let ox::Expression::Identifier(id) = &c.callee {
                if id.name == "eval"
                    && matches!(self.resolve("eval"), Binding::Global(_))
                    && self.with_objs_for("eval").is_empty()
                {
                    let args_arr = self.build_spread_args(&c.arguments)?;
                    let arg = self.alloc_reg();
                    let zero = self.alloc_reg();
                    self.emit(Instr::LoadInt { dst: zero, val: 0 });
                    self.emit(Instr::GetIndex { dst: arg, obj: args_arr, key: zero });
                    self.emit_direct_eval(arg, dst, false);
                    return Ok(dst);
                }
            }
            // `super.m(...args)` — a StaticMemberExpression whose object is `super`
            // (which is not a value, so it must be handled before the generic
            // StaticMember case evaluates the object).
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let pid = self
                        .super_class
                        .ok_or("`super.method(...)` is only valid in a derived class")?;
                    self.this_check();
                    let name = self.string_name(m.property.name.as_str());
                    let args_arr = self.build_spread_args(&c.arguments)?;
                    self.emit(Instr::SuperMethodSpread { dst, home_class_id: pid, name, args: args_arr });
                    return Ok(dst);
                }
            }
            // Method call `obj.m(...)` — evaluate the receiver first so `this`
            // binds correctly, then build args, then CallMethodSpread.
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                let obj = self.expr(&m.object)?;
                if m.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(m.property.name.as_str());
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallMethodSpread { dst, obj, name, args: args_arr });
                return Ok(dst);
            }
            // `super[key](...args)` — computed super member with spread args
            // (must precede the generic computed arm: `super` is not a value).
            if let ox::Expression::ComputedMemberExpression(m) = &c.callee {
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let pid = self
                        .super_class
                        .ok_or("`super[...](...)` is only valid in a derived class")?;
                    self.this_check();
                    let key = self.expr(&m.expression)?;
                    let args_arr = self.build_spread_args(&c.arguments)?;
                    self.emit(Instr::SuperMethodComputedSpread {
                        dst,
                        home_class_id: pid,
                        key,
                        args: args_arr,
                    });
                    return Ok(dst);
                }
            }
            // Computed method call `obj[key](...)` — bind `this` = obj (a plain
            // CallSpread on the GET result would lose the receiver).
            if let ox::Expression::ComputedMemberExpression(m) = &c.callee {
                let obj = self.expr(&m.object)?;
                if m.optional {
                    self.emit_optional_check(obj);
                }
                let key = self.expr(&m.expression)?;
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallMethodComputedSpread { dst, obj, key, args: args_arr });
                return Ok(dst);
            }
            let callee = self.expr(&c.callee)?;
            let args_arr = self.build_spread_args(&c.arguments)?;
            self.emit(Instr::CallSpread { dst, callee, args: args_arr });
            return Ok(dst);
        }

        // `super(args)` — run the superclass constructor on the current `this`.
        if matches!(&c.callee, ox::Expression::Super(_)) {
            if !self.derived_class {
                return Err("`super(...)` is only valid in a derived class constructor".into());
            }
            let pid = self
                .super_class
                .ok_or("`super(...)` is only valid in a derived class constructor")?;
            // (Spread `super(...args)` is handled in the spread block above.)
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::SuperCtor { home_class_id: pid, arg_base, argc });
            // `super()` evaluates to the new bound `this` (BindThisValue's
            // result) — SuperCtor rebinds reg 0 to it (call-expr-value).
            self.emit(Instr::Move { dst, src: 0 });
            return Ok(dst);
        }
        // `super.method(args)` — call an inherited method with the current `this`.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if matches!(&m.object, ox::Expression::Super(_)) {
                self.this_check();
                let name = self.string_name(m.property.name.as_str());
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                if let Some(pid) = self.super_class {
                    self.emit(Instr::SuperMethod { dst, home_class_id: pid, name, arg_base, argc });
                } else if self.super_home_obj {
                    self.emit(Instr::SuperMethodObj { dst, name, arg_base, argc });
                } else {
                    return Err("`super.method(...)` is only valid in a method".into());
                }
                return Ok(dst);
            }
        }

        // Inside a `with`, a bare-identifier call resolves through the with
        // chain FIRST — before any builtin special-case lowering, so
        // `with (proxy) { Object() }` fires the `has` trap exactly like a
        // user-function callee. The probe (HasBinding) and the read
        // (GetBindingValue: HasProperty + Get) are both observable; a
        // with-resolved callee is invoked with `this` = the with-object
        // (WithBaseObject). Names no with-object carries fall back to the
        // static binding with `this` = undefined (the dedicated builtin
        // lowerings stay bypassed — the loaded global builtin VALUE is
        // called instead, same semantics).
        if let ox::Expression::Identifier(id) = &c.callee {
            let with_objs = self.with_obj_regs(id.name.as_str());
            if !with_objs.is_empty() {
                // `eval(...)` inside a `with` whose static binding is the
                // unshadowed global %eval%: when NO with-object carries an
                // `eval` binding, the IdentifierReference still resolves to
                // %eval% (through the object envs) and the call IS a DIRECT
                // eval — its program sees the caller's bindings AND the with
                // chain (threaded through the eval-site map's hidden
                // " with-object-N" cells). A with-object that DOES carry the
                // name gets an ordinary call of that value with `this` = the
                // with-object (accepted limit: a with-bound value that happens
                // to BE %eval% is treated as indirect).
                if id.name == "eval" && matches!(self.resolve("eval"), Binding::Global(_)) {
                    let save = self.next_reg;
                    let nidx = self.string_name("eval");
                    let callee_reg = self.alloc_reg();
                    let this_reg = self.alloc_reg();
                    let found = self.alloc_reg();
                    self.emit(Instr::LoadBool { dst: found, val: false });
                    let mut hits = Vec::new();
                    for &obj in &with_objs {
                        let flag = self.temp();
                        self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
                        let jf = self.here();
                        self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
                        self.next_reg -= 1;
                        self.emit(Instr::WithGet {
                            dst: callee_reg,
                            obj,
                            name: nidx,
                            strict: self.cx.in_strict,
                        });
                        self.emit(Instr::Move { dst: this_reg, src: obj });
                        self.emit(Instr::LoadBool { dst: found, val: true });
                        let jd = self.here();
                        self.emit(Instr::Jump { target: 0 });
                        hits.push(jd);
                        let nxt = self.here();
                        self.patch_jump(jf, nxt);
                    }
                    let resolved = self.here();
                    for j in hits {
                        self.patch_jump(j, resolved);
                    }
                    // Arguments evaluate AFTER the callee reference resolved.
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    let jf = self.here();
                    self.emit(Instr::JumpIfFalse { cond: found, target: 0 });
                    self.emit(Instr::CallWithThis {
                        dst,
                        callee: callee_reg,
                        this_v: this_reg,
                        arg_base,
                        argc,
                    });
                    let je = self.here();
                    self.emit(Instr::Jump { target: 0 });
                    let direct = self.here();
                    self.patch_jump(jf, direct);
                    let arg = if argc == 0 {
                        let r = self.temp();
                        self.emit(Instr::LoadUndefined { dst: r });
                        r
                    } else {
                        arg_base
                    };
                    self.emit_direct_eval(arg, dst, false);
                    let end = self.here();
                    self.patch_jump(je, end);
                    self.next_reg = save.max(dst + 1);
                    return Ok(dst);
                }
                let save = self.next_reg;
                let (callee_reg, this_reg) =
                    self.emit_with_callee_chain(id.name.as_str(), &with_objs);
                // Arguments evaluate AFTER the callee reference resolved.
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::CallWithThis {
                    dst,
                    callee: callee_reg,
                    this_v: this_reg,
                    arg_base,
                    argc,
                });
                self.next_reg = save.max(dst + 1);
                return Ok(dst);
            }
        }

        // Bare `Error("msg")` call (no `new`) → same Error object. Only for the
        // pristine global builtin — a user binding named e.g. `TypeError` is an
        // ordinary call.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(kind) = error_ctor(&id.name) {
                if self.builtin_unshadowed(id.name.as_str()) {
                    return self.build_error(kind, &c.arguments, dst);
                }
            }
        }
        // Direct `eval(code)`: the evaluated string runs with the caller's
        // strictness, `this`, new.target permission, home class and private
        // scope. Fires only for the unshadowed global `eval` — an enclosing
        // user binding named `eval` is an ordinary call, and inside a `with`
        // whose object could shadow `eval` the generic dynamic path is kept.
        // The DISPATCH arm re-checks at runtime that the live global `eval`
        // still IS %eval% (a rebound `eval` gets an ordinary call). Indirect
        // eval stays on the generic `Call` → `GLOBAL_EVAL` native.
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "eval"
                && matches!(self.resolve("eval"), Binding::Global(_))
                && self.with_objs_for("eval").is_empty()
            {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                let arg = if argc == 0 {
                    let r = self.temp();
                    self.emit(Instr::LoadUndefined { dst: r });
                    r
                } else {
                    arg_base
                };
                self.emit_direct_eval(arg, dst, false);
                return Ok(dst);
            }
        }
        // `Symbol(desc?)` → a fresh Symbol primitive (MakeSymbol op). `Symbol` is
        // not constructable, so only the call form is lowered here.
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "Symbol" {
                let desc = match c.arguments.first().and_then(|a| a.as_expression()) {
                    Some(e) => {
                        let t = self.temp();
                        let v = self.expr_into(e, t)?;
                        if v != t {
                            self.emit(Instr::Move { dst: t, src: v });
                        }
                        Some(t)
                    }
                    None => None,
                };
                self.emit(Instr::MakeSymbol { dst, desc });
                if desc.is_some() {
                    self.next_reg -= 1;
                }
                return Ok(dst);
            }
        }
        // `RegExp(pattern?, flags?)` (no `new`) → like `new RegExp(...)`, except a
        // RegExp pattern with no flags + a RegExp `constructor` returns it unchanged
        // (is_construct: false signals the runtime short-circuit).
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "RegExp" {
                return self.emit_regexp(&c.arguments, dst, false);
            }
        }
        // `BigInt(x)` → conversion (BigIntFrom op). No arg → undefined (→ TypeError
        // at runtime, matching the spec).
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "BigInt" {
                let t = self.temp();
                match c.arguments.first().and_then(|a| a.as_expression()) {
                    Some(e) => {
                        let v = self.expr_into(e, t)?;
                        if v != t {
                            self.emit(Instr::Move { dst: t, src: v });
                        }
                    }
                    None => self.emit(Instr::LoadUndefined { dst: t }),
                }
                self.emit(Instr::BigIntFrom { dst, arg: t });
                self.next_reg -= 1;
                return Ok(dst);
            }
        }
        // Number(x) / parseInt(s,radix) / parseFloat(s) → GlobalFn op.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(op) = crate::bytecode::GlobalFn::from_name(&id.name) {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::GlobalFn { dst, op, arg_base, argc });
                return Ok(dst);
            }
            // Bare `Array(…)` / `Object()` behave like their `new` forms.
            if id.name == "Array" {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                return Ok(dst);
            }
            if id.name == "Object" {
                // `Object()` → a fresh object; `Object(x)` → ToObject(x).
                if let Some(arg) = c.arguments.first().and_then(|a| a.as_expression()) {
                    let src = self.expr(arg)?;
                    self.emit(Instr::ToObject { dst, src });
                } else {
                    self.emit(Instr::NewObject { dst, hint: 0 });
                }
                return Ok(dst);
            }
        }

        // console.log(...) → Print opcode.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "console"
                    && matches!(m.property.name.as_str(), "log" | "info" | "warn" | "error" | "debug")
                {
                    // node routes console.error / console.warn to stderr.
                    let to_stderr = matches!(m.property.name.as_str(), "error" | "warn");
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::Print { arg_base, argc, to_stderr });
                    self.emit(Instr::LoadUndefined { dst });
                    return Ok(dst);
                }
            }
        }

        // Host `print(...)` → Print to stdout. JS shells (and the test262
        // `doneprintHandle.js` harness, which calls `print('Test262:Async…')` to
        // signal async-test completion) expect it as a global. Yield to a lexical
        // `print` binding (local/param/upvalue) if the program defined one.
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "print"
                && !matches!(
                    self.resolve("print"),
                    Binding::Local(_) | Binding::LocalCell(_) | Binding::Upvalue(_)
                )
            {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::Print { arg_base, argc, to_stderr: false });
                self.emit(Instr::LoadUndefined { dst });
                return Ok(dst);
            }
        }

        // Clock idioms: `performance.now()` and `Date.now()` → Now opcode. They
        // have no real global object in the subset, so recognise the call shape.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                let epoch = match (obj.name.as_str(), m.property.name.as_str()) {
                    ("performance", "now") => Some(false),
                    ("Date", "now") => Some(true),
                    _ => None,
                };
                if let Some(epoch) = epoch {
                    self.emit(Instr::Now { dst, epoch });
                    return Ok(dst);
                }
                // `Date.UTC(y, m0, …)` → ms; `Date.parse(str)` → ms.
                if obj.name == "Date" && m.property.name == "UTC" {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::DateUTC { dst, arg_base, argc });
                    return Ok(dst);
                }
                if obj.name == "Date" && m.property.name == "parse" && c.arguments.len() == 1 {
                    if let Some(ae) = c.arguments[0].as_expression() {
                        let src = self.expr(ae)?;
                        self.emit(Instr::DateParse { dst, src });
                        return Ok(dst);
                    }
                }
            }
        }

        // `JSON.parse(text)` / `JSON.stringify(value)` → fast ops. The forms with
        // a reviver / replacer (2+ args) fall through to the generic call so the
        // `JSON_PARSE` / `JSON_STRINGIFY` natives can honour them.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "JSON" && m.property.name == "parse" && c.arguments.len() == 1 {
                    if let Some(ae) = c.arguments[0].as_expression() {
                        let a = self.expr(ae)?;
                        self.emit(Instr::JsonParse { dst, a });
                        return Ok(dst);
                    }
                }
                if obj.name == "JSON" && m.property.name == "stringify" && c.arguments.len() == 1 {
                    if let Some(ve) = c.arguments[0].as_expression() {
                        let val = self.expr(ve)?;
                        let space = self.temp();
                        self.emit(Instr::LoadUndefined { dst: space });
                        self.emit(Instr::JsonStringify { dst, val, space });
                        return Ok(dst);
                    }
                }
            }
        }

        // `Array.isArray(x)` → IsArray op.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Array" && m.property.name == "isArray" && c.arguments.len() == 1 {
                    if let Some(arg_expr) = c.arguments[0].as_expression() {
                        let a = self.expr(arg_expr)?;
                        self.emit(Instr::IsArray { dst, a });
                        return Ok(dst);
                    }
                }
            }
        }

        // `Object.keys/values/entries(o)` → dedicated ops (Object has no real
        // global object in the subset).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Object" && c.arguments.len() == 1 {
                    let mk = match m.property.name.as_str() {
                        "keys" => Some(0u8),
                        "values" => Some(1u8),
                        "entries" => Some(2u8),
                        _ => None,
                    };
                    if let Some(kind) = mk {
                        if let Some(arg_expr) = c.arguments[0].as_expression() {
                            let o = self.expr(arg_expr)?;
                            self.emit(match kind {
                                0 => Instr::ObjectKeys { dst, obj: o },
                                1 => Instr::ObjectValues { dst, obj: o },
                                _ => Instr::ObjectEntries { dst, obj: o },
                            });
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // `Math.<fn>(args…)` → MathOp. Math has no real global object in the
        // subset, so recognise the call shape (like console.log / Date.now).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Math" {
                    if m.property.name == "random" {
                        self.emit(Instr::Random { dst });
                        return Ok(dst);
                    }
                    if let Some(op) = crate::bytecode::MathFn::from_name(m.property.name.as_str()) {
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        self.emit(Instr::MathOp { dst, op, arg_base, argc });
                        return Ok(dst);
                    }
                }
            }
        }

        // Constructor-namespace static methods with a flat arg list:
        // Object.assign, Array.of, String.fromCharCode, Number.isInteger/… .
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if let Some(op) =
                    crate::bytecode::StaticFn::from_name(&obj.name, m.property.name.as_str())
                {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::StaticFn { dst, op, arg_base, argc });
                    return Ok(dst);
                }
                // `Array.from(src[, mapFn])` — needs iteration + optional callback.
                // Only the 1-/2-arg form is lowered; a 3rd `thisArg` falls through
                // to the general call (the native honours it).
                if obj.name == "Array"
                    && m.property.name == "from"
                    && (1..=2).contains(&c.arguments.len())
                {
                    if let Some(se) = c.arguments[0].as_expression() {
                        let save = self.next_reg;
                        let src = self.expr(se)?;
                        let mapfn = self.temp();
                        match c.arguments.get(1).and_then(|a| a.as_expression()) {
                            Some(fe) => {
                                let f = self.expr_into(fe, mapfn)?;
                                if f != mapfn {
                                    self.emit(Instr::Move { dst: mapfn, src: f });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: mapfn }),
                        }
                        self.emit(Instr::ArrayFrom { dst, src, mapfn });
                        self.next_reg = save.max(dst + 1);
                        return Ok(dst);
                    }
                }
            }
        }

        // A PARENTHESIZED member callee preserves its reference — `(a.b)()` and
        // `(a?.b)()` call with `this` = a (parens are transparent to
        // EvaluateCall; only a comma/assignment breaks the reference).
        let mut peeled: &ox::Expression = &c.callee;
        while let ox::Expression::ParenthesizedExpression(p) = peeled {
            peeled = &p.expression;
        }
        // `(a?.b)(args)`: a parenthesized-CHAIN member callee — the chain ends
        // at the parens (nullish base → undefined callee → the call throws),
        // but the member base still binds `this`.
        if let ox::Expression::ChainExpression(ce) = peeled {
            if !c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_))) {
                if let Some((callee, obj)) = self.chain_member_callee(ce)? {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
            }
        }
        // Method call `obj.name(args…)` → CallMethod, binding `this` to obj.
        // (Computed-member calls `obj[k](…)` fall through to the generic path.)
        if let ox::Expression::StaticMemberExpression(m) = peeled {
            let obj = self.expr(&m.object)?;
            if m.optional {
                self.emit_optional_check(obj); // `obj?.method()` — short-circuit if obj nullish
            } else if matches!(
                &m.object,
                ox::Expression::StaticMemberExpression(_)
                    | ox::Expression::ComputedMemberExpression(_)
                    | ox::Expression::PrivateFieldExpression(_)
            ) {
                // `o.bar.gar(args)`: the callee GET (`.gar` of a possibly-
                // nullish `o.bar`) throws BEFORE the arguments evaluate (spec
                // EvaluateCall: func = GetValue(ref) precedes ArgumentList-
                // Evaluation). The fused CallMethod defers the get past the
                // args, so reject a nullish receiver here. Kept off simple
                // identifier/this receivers (hot path; their get cannot throw
                // earlier than the fused op observes).
                self.emit(Instr::CheckCoercible { src: obj });
            }
            let name = self.string_name(m.property.name.as_str());
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }
        // Private method call `obj.#m(args…)` → CallMethod on the "#m" key.
        if let ox::Expression::PrivateFieldExpression(p) = peeled {
            self.check_private_declared(&p.field.name)?;
            let obj = self.expr(&p.object)?;
            let name = self.string_name(&private_key(&p.field.name));
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }

        // Computed method call `obj[key](args…)` → bind `this` to obj. Evaluate
        // `super[expr](args…)` — computed inherited-method call.
        if let ox::Expression::ComputedMemberExpression(m) = peeled {
            if matches!(&m.object, ox::Expression::Super(_)) {
                let is_class = self.super_class;
                if is_class.is_none() && !self.super_home_obj {
                    return Err("`super[x](...)` is only valid in a method".into());
                }
                self.this_check();
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                if let Some(pid) = is_class {
                    self.emit(Instr::SuperMethodComputed { dst, home_class_id: pid, key: key_reg, arg_base, argc });
                } else {
                    self.emit(Instr::SuperMethodObjComputed { dst, key: key_reg, arg_base, argc });
                }
                return Ok(dst);
            }
        }
        // obj and the key into stable registers (below the contiguous arg block).
        if let ox::Expression::ComputedMemberExpression(m) = peeled {
            let obj = self.expr(&m.object)?;
            let obj_reg = self.alloc_reg();
            if obj != obj_reg {
                self.emit(Instr::Move { dst: obj_reg, src: obj });
            }
            if m.optional {
                self.emit_optional_check(obj_reg);
            }
            let key = self.expr(&m.expression)?;
            let key_reg = self.alloc_reg();
            if key != key_reg {
                self.emit(Instr::Move { dst: key_reg, src: key });
            }
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethodComputed { dst, obj: obj_reg, key: key_reg, arg_base, argc });
            return Ok(dst);
        }

        // (Bare-identifier calls inside a `with` were already routed through the
        // with chain near the top of this function — before the builtin
        // special-cases — so nothing with-related remains here.)

        // General call: evaluate callee, then contiguous args.
        let callee = self.expr(&c.callee)?;
        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
        self.emit(Instr::Call { dst, callee, arg_base, argc });
        Ok(dst)
    }

    /// Evaluate call arguments into a contiguous run of registers and return
    /// (first register, count). The run MUST be contiguous because the
    /// `Call`/`Print`/`NewArray` opcodes address args as
    /// `[arg_base, arg_base+argc)`.
    ///
    /// Correctness subtlety: evaluating one argument may itself allocate scratch
    /// temps (e.g. `a[i]` evaluates `a` and `i` into temps). Those temps must
    /// NOT land inside the still-unfilled argument slots. So we reserve the
    /// whole block first (bumping `next_reg` past it), which forces per-arg
    /// temps to allocate ABOVE the block; we then reclaim them after each arg.
    /// Emit the `DirectEval` op for a direct `eval(<arg>)` call site:
    /// builds the visible-caller-bindings site map and the instruction.
    /// `tail`: the site is a proper-tail-call return position, so a REBOUND
    /// `eval` (ordinary call at runtime) reuses the frame.
    pub(crate) fn emit_direct_eval(&mut self, arg: Reg, dst: Reg, tail: bool) {
                // The visible caller bindings (boxed cells, innermost shadowing
        // first) — the eval program closes over them. An EVAL ROOT also
        // maps its own cell locals AND its seeded caller upvalues (kind
        // 1), so nested evals keep reaching the original caller scope.
        let eval_root = self.is_script && self.cx.eval_locals;
        let site = if self.box_all_locals || eval_root || self.script_eval_lexicals {
            let mut map: Vec<(String, u8, u16)> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for scope in self.scopes.iter().rev() {
                for (n, r) in scope.iter().rev() {
                    if self.cell_regs.contains(r) && seen.insert(n.clone()) {
                        map.push((n.clone(), 0u8, *r));
                    }
                }
            }
            if let Some((n, r)) = self.self_name.clone() {
                if self.cell_regs.contains(&r) && seen.insert(n.clone()) {
                    map.push((n, 0u8, r));
                }
            }
            if eval_root {
                let ups: Vec<String> =
                    self.upvalues.borrow().iter().map(|(n, _)| n.clone()).collect();
                for (i, n) in ups.iter().enumerate() {
                    if seen.insert(n.clone()) {
                        map.push((n.clone(), 1u8, i as u16));
                    }
                }
            }
            // In a parameter default, the eval's sloppy var/function
            // names may not collide with the PARAM scope (the params
            // and, for non-arrows, the implicit `arguments`).
            let param_collisions = if self.in_param_init {
                let mut names = self.param_names.clone();
                if self.arguments_reg.is_some()
                    && !names.iter().any(|n| n == "arguments")
                {
                    names.push("arguments".to_string());
                }
                Some(names)
            } else {
                None
            };
            // LEXICAL caller bindings visible here (the live scope stack
            // at this call site gives the correct block nesting).
            let mut lex: Vec<String> = Vec::new();
            for scope in self.scopes.iter().rev() {
                for (n, r) in scope.iter().rev() {
                    if self.lexical_regs.contains(r) && !lex.iter().any(|e| e == n) {
                        lex.push(n.clone());
                    }
                }
            }
            let s = self.eval_sites.len() as u16;
            self.eval_sites.push((map, param_collisions, lex));
            s
        } else {
            u16::MAX
        };
        // The effective `this` to inherit: a static field initializer holds
        // it in `this_override`, otherwise it is reg 0.
        let this_reg = self.this_override.unwrap_or(0);
        self.emit(Instr::DirectEval {
            dst,
            arg,
            new_target_ok: self.cx.new_target_ok,
            this_reg,
            home_class: self.super_class.unwrap_or(u32::MAX),
            super_static: self.super_static,
            ban_arguments: self.cx.in_field_init,
            strict_caller: self.cx.in_strict,
            super_home_obj: self.super_home_obj,
            // The eval's variable environment: GLOBAL only when the
            // call site is the script top level (a function/arrow/param
            // context keeps the old slot behavior until the dynamic
            // caller-env lands).
            var_env_is_global: self.is_script,
            site,
            tail,
        });
    }

    pub(crate) fn eval_args_contiguous(
        &mut self,
        args: &oxc_allocator::Vec<ox::Argument>,
    ) -> R<(Reg, u16)> {
        let exprs: Vec<&ox::Expression> = args
            .iter()
            .map(|a| {
                a.as_expression()
                    .ok_or_else(|| "spread arguments are not in the zipp-vm subset yet".to_string())
            })
            .collect::<R<Vec<_>>>()?;
        let base = self.eval_contiguous(&exprs)?;
        Ok((base, exprs.len() as u16))
    }

    /// Build a call-argument list containing `...spread` into a fresh array and
    /// return its (live) register. Each plain arg is pushed as one element; each
    /// `...x` arg appends every element of `x` (an array, or a string's chars).
    /// Consumed by `CallSpread` / `CallMethodSpread`.
    pub(crate) fn build_spread_args(&mut self, args: &oxc_allocator::Vec<ox::Argument>) -> R<Reg> {
        let args_arr = self.temp();
        self.emit(Instr::NewArray { dst: args_arr, arg_base: self.next_reg, argc: 0 });
        for a in args {
            let save = self.next_reg;
            match a {
                ox::Argument::SpreadElement(s) => {
                    let v = self.expr(&s.argument)?;
                    self.emit(Instr::ArrayAppend { arr: args_arr, val: v, spread: true });
                }
                other => {
                    let e = other.as_expression().ok_or("unsupported spread-call argument")?;
                    let v = self.expr(e)?;
                    self.emit(Instr::ArrayAppend { arr: args_arr, val: v, spread: false });
                }
            }
            self.next_reg = save;
        }
        Ok(args_arr)
    }

    /// Evaluate `exprs` into the contiguous register block `[base, base+len)`,
    /// reclaiming each expression's scratch temps. Returns `base`.
    pub(crate) fn eval_contiguous(&mut self, exprs: &[&ox::Expression]) -> R<Reg> {
        let base = self.next_reg;
        // Reserve the block up front so arg-evaluation temps allocate above it.
        for _ in exprs {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, e) in exprs.iter().enumerate() {
            let slot = base + i as Reg;
            let v = self.expr_into(e, slot)?;
            if v != slot {
                self.emit(Instr::Move { dst: slot, src: v });
            }
            // Reclaim temps this argument used (everything above the block).
            self.next_reg = block_top;
        }
        Ok(base)
    }
}
