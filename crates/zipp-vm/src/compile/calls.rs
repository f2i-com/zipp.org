// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;
// The AST this module consumes. Imported explicitly rather than relying on how
// the parent spells its own import, so this file resolves `Expr`/`CallExpr`/…
// on its own terms. NOTE: `ast::Program` and `crate::bytecode::Program` are both
// in scope through globs, so `Program` is deliberately never named in this file.
use crate::parse::ast::*;

/// The expression of a non-spread argument — the replacement for oxc's
/// `Argument::as_expression()`. `None` for `...x`, which is what every fast-path
/// lowering below uses to decide it does not apply.
fn arg_expr(a: &Arg) -> Option<&Expr> {
    match a {
        Arg::Expr(e) => Some(e),
        Arg::Spread(_) => None,
    }
}

/// `ZIPP_NO_PAD2_COND_FUSE=1` restores the ordinary conditional lowering
/// byte-for-byte. This is a separate latch from `ZIPP_NO_PAD2_CACHE`: the
/// latter disables the whole two-digit cache package and therefore also gates
/// this fusion at its use site.
#[inline]
fn pad2_cond_fuse_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_PAD2_COND_FUSE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_STRICT_CALL_ORDER=1` confines method-call fusion to the PROVABLE
/// argument class (see `FnCompiler::arg_order_transparent`): every other
/// `obj.name(args)` then takes the captured `GetProp` + `CallWithThis`
/// lowering. A compile-time switch, read once.
#[inline]
fn strict_call_order() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_STRICT_CALL_ORDER").is_some() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Conservative: can evaluating `e` assign the binding `name`? Only an
/// `Assign`/`Update` whose target names it (or a destructuring/call target,
/// which are not analysed) can — a closure literal that wrote it would have
/// made the binding a cell rather than a register, and a call cannot reach a
/// register local. Anything unfamiliar answers `true`.
fn expr_may_assign_name(e: &Expr, name: &str) -> bool {
    let rec = |x: &Expr| expr_may_assign_name(x, name);
    let args_rec = |args: &[Arg]| {
        args.iter().any(|a| match a {
            Arg::Expr(x) | Arg::Spread(x) => rec(x),
        })
    };
    let key_rec = |k: &PropKey| match k {
        PropKey::Computed(x) => rec(x),
        _ => false,
    };
    match e {
        Expr::Assign { target, value, .. } => target_may_assign_name(target, name) || rec(value),
        Expr::Update { target, .. } => target_may_assign_name(target, name),
        Expr::Ident(_)
        | Expr::This
        | Expr::Super
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Num(_)
        | Expr::BigInt(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::NewTarget
        | Expr::ImportMeta
        | Expr::Arrow(_)
        | Expr::Function(_) => false,
        Expr::Unary { arg, .. } => rec(arg),
        Expr::Await(x) | Expr::Chain(x) => rec(x),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            rec(left) || rec(right)
        }
        Expr::Cond { test, cons, alt } => rec(test) || rec(cons) || rec(alt),
        Expr::Seq(v) => v.iter().any(rec),
        Expr::Array(elems, _) => elems.iter().flatten().any(|el| match el {
            ArrayElem::Expr(x) | ArrayElem::Spread(x) => rec(x),
        }),
        Expr::Object(members, _) => members.iter().any(|mbr| match mbr {
            ObjectMember::Prop {
                key, value, init, ..
            } => key_rec(key) || rec(value) || init.as_ref().is_some_and(rec),
            ObjectMember::Method { key, .. }
            | ObjectMember::Get { key, .. }
            | ObjectMember::Set { key, .. } => key_rec(key),
            ObjectMember::Spread(x) => rec(x),
        }),
        Expr::Template(t) => t.exprs.iter().any(rec),
        Expr::TaggedTemplate { tag, quasi } => rec(tag) || quasi.exprs.iter().any(rec),
        Expr::Call(c) => rec(&c.callee) || args_rec(&c.args),
        Expr::New { callee, args } => rec(callee) || args_rec(args),
        Expr::Member(m) => {
            rec(&m.object)
                || match &m.prop {
                    MemberProp::Computed(k) => rec(k),
                    _ => false,
                }
        }
        Expr::PrivateIn { object, .. } => rec(object),
        Expr::ImportCall { spec, options, .. } => {
            rec(spec) || options.as_ref().is_some_and(|o| rec(o))
        }
        Expr::Yield { arg, .. } => arg.as_ref().is_some_and(|a| rec(a)),
        // Computed keys and static blocks evaluate inline.
        Expr::Class(_) => true,
    }
}

fn target_may_assign_name(t: &Target, name: &str) -> bool {
    match t {
        Target::Ident { name: n, .. } => &**n == name,
        Target::Member(m) => {
            expr_may_assign_name(&m.object, name)
                || match &m.prop {
                    MemberProp::Computed(k) => expr_may_assign_name(k, name),
                    _ => false,
                }
        }
        // Destructuring and Annex-B call targets are not analysed.
        Target::Call(_) | Target::Array(_) | Target::Object { .. } => true,
    }
}

/// Recognise only `n < 10 ? "0" + n : "" + n`, with the SAME identifier at
/// all three reads. Parentheses are absent from this AST and are semantically
/// transparent. Binding stability is checked separately by `conditional`.
fn pad2_cond_ident<'e>(test: &'e Expr, cons: &'e Expr, alt: &'e Expr) -> Option<&'e str> {
    let name = match test {
        Expr::Binary {
            op: BinaryOp::Lt,
            left,
            right,
        } if matches!(right.as_ref(), Expr::Num(n) if *n == 10.0) => match left.as_ref() {
            Expr::Ident(n) => &**n,
            _ => return None,
        },
        _ => return None,
    };
    let add_ident = |e: &'e Expr, want_zero: bool| -> Option<&'e str> {
        match e {
            Expr::Binary {
                op: BinaryOp::Add,
                left,
                right,
            } => {
                let literal_ok = match left.as_ref() {
                    Expr::Str(StrVal::Utf8(s)) => {
                        if want_zero {
                            &**s == "0"
                        } else {
                            s.is_empty()
                        }
                    }
                    _ => false,
                };
                match (literal_ok, right.as_ref()) {
                    (true, Expr::Ident(n)) => Some(&**n),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    let cons_name = add_ident(cons, true)?;
    let alt_name = add_ident(alt, false)?;
    (name == cons_name && name == alt_name).then_some(name)
}

impl<'a> FnCompiler<'a> {
    // NOTE: signature. `ox::YieldExpression` has no struct counterpart — the
    // payload lives on `Expr::Yield { arg, delegate }` — so this takes the two
    // fields. The caller (`compile/exprs.rs`) passes `arg.as_deref()`.
    pub(crate) fn yield_expr(&mut self, arg: Option<&Expr>, delegate: bool, dst: Reg) -> R<Reg> {
        if !self.in_generator {
            return Err("`yield` is only valid inside a generator (function*)".into());
        }
        if delegate {
            let arg = arg.ok_or("yield* requires an operand")?;
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
                self.emit(Instr::GetAsyncIterator {
                    dst: iter,
                    src: iter,
                    sync_dst: is_sync,
                });
                let idx = self.alloc_reg();
                self.emit(Instr::LoadInt { dst: idx, val: 0 });
                // Cache the inner iterator's `next` ONCE (IteratorRecord.[[NextMethod]]),
                // matching the spec's get-next-once ordering for a user iterator.
                let next_fn = self.alloc_reg();
                let next_name = self.string_name("next");
                self.emit(Instr::GetProp {
                    dst: next_fn,
                    obj: iter,
                    name: next_name,
                });
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
                self.emit(Instr::AsyncIterNextStep {
                    dst: step,
                    iter,
                    idx,
                    sent,
                    next_fn,
                });
                self.emit(Instr::Await { dst: r, val: step });
                self.emit(Instr::RequireObject { val: r });
                self.emit(Instr::GetProp {
                    dst: done,
                    obj: r,
                    name: done_name,
                });
                let jdone = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: done,
                    target: 0,
                }); // done → yield* value (r.value)
                self.emit(Instr::GetProp {
                    dst: value,
                    obj: r,
                    name: value_name,
                });
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
                self.emit(Instr::JumpIfFalse {
                    cond: is_sync,
                    target: 0,
                });
                let ph_aw = self.here();
                self.emit(Instr::PushHandler {
                    catch_target: 0,
                    catch_reg: cerr,
                });
                self.handler_depth += 1;
                self.emit(Instr::Await {
                    dst: value,
                    val: value,
                });
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
                self.emit(Instr::PushHandler {
                    catch_target: 0,
                    catch_reg: excr,
                });
                self.handler_depth += 1;
                // Suspend; resume delivers (mode, value) into (mode, sent).
                self.emit(Instr::AsyncYieldDelegate {
                    mode_dst: mode,
                    val_dst: sent,
                    val: value,
                });
                self.emit(Instr::PopHandler);
                self.handler_depth -= 1;
                // mode 2 (return) → return-delegation; mode 0 (next, falsy) → loop.
                let jret = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: mode,
                    target: 0,
                });
                self.emit(Instr::Jump { target: top });
                // --- return delegation: outer .return(sent). Delegate to inner.return;
                //     no method → outer returns `sent`; else await, then finish-return
                //     (done) or yield the value and continue. ---
                let ret_label = self.here();
                self.patch_jump(jret, ret_label);
                self.emit(Instr::AsyncIterReturnStep {
                    dst: tstep,
                    has_dst: hasret,
                    iter,
                    ret: sent,
                });
                let jhas = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: hasret,
                    target: 0,
                });
                // No inner `return` method: the received return value is AWAITED
                // (spec: if return is undefined and generatorKind is async, set
                // received.[[Value]] to ? Await(received.[[Value]])) before the
                // generator returns it — a thenable is adopted (observable `then`
                // read + one tick), and a rejection unwinds instead.
                self.emit(Instr::Await {
                    dst: sent,
                    val: sent,
                });
                self.emit(Instr::Return { src: sent });
                let has_ret = self.here();
                self.patch_jump(jhas, has_ret);
                self.emit(Instr::Await {
                    dst: taw,
                    val: tstep,
                });
                self.emit(Instr::RequireObject { val: taw });
                self.emit(Instr::GetProp {
                    dst: done,
                    obj: taw,
                    name: done_name,
                });
                let jretdone = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: done,
                    target: 0,
                }); // inner.return done → generator returns value
                self.emit(Instr::GetProp {
                    dst: value,
                    obj: taw,
                    name: value_name,
                });
                self.emit(Instr::Jump { target: yield_pt }); // not done → yield value, continue
                let ret_done = self.here();
                self.patch_jump(jretdone, ret_done);
                self.emit(Instr::GetProp {
                    dst: value,
                    obj: taw,
                    name: value_name,
                });
                // A SYNC inner's `return()` result value is unwrapped by the
                // AsyncFromSync continuation (closeOnRejection = false here):
                // await it before completing (iterator-result-unwrap-promise).
                let jrv = self.here();
                self.emit(Instr::JumpIfFalse {
                    cond: is_sync,
                    target: 0,
                });
                self.emit(Instr::Await {
                    dst: value,
                    val: value,
                });
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
                self.emit(Instr::AsyncIterThrowStep {
                    dst: tstep,
                    iter,
                    exc: excr,
                });
                self.emit(Instr::Await {
                    dst: taw,
                    val: tstep,
                });
                self.emit(Instr::RequireObject { val: taw });
                self.emit(Instr::GetProp {
                    dst: done,
                    obj: taw,
                    name: done_name,
                });
                let jdone2 = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: done,
                    target: 0,
                }); // inner.throw done → value
                self.emit(Instr::GetProp {
                    dst: value,
                    obj: taw,
                    name: value_name,
                });
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
                self.emit(Instr::GetProp {
                    dst,
                    obj: taw,
                    name: value_name,
                });
                let jtv = self.here();
                self.emit(Instr::JumpIfFalse {
                    cond: is_sync,
                    target: 0,
                });
                self.emit(Instr::Await { dst, val: dst });
                let after_tv = self.here();
                self.patch_jump(jtv, after_tv);
                let jend = self.here();
                self.emit(Instr::Jump { target: 0 });
                // done via next(): yield* value = r.value.
                let done_label = self.here();
                self.patch_jump(jdone, done_label);
                self.emit(Instr::GetProp {
                    dst,
                    obj: r,
                    name: value_name,
                });
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
            self.emit(Instr::GetIteratorObj {
                dst: iter,
                src: iter,
            });
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
            self.emit(Instr::JumpIfTrue {
                cond: ret,
                target: 0,
            }); // → generator return
            let jdone = self.here();
            self.emit(Instr::JumpIfTrue {
                cond: done,
                target: 0,
            }); // → yield* completes
                // Neither: yield the value; on resume (mode,sent) are delivered, loop.
            self.emit(Instr::YieldDelegate {
                mode_dst: mode,
                val_dst: sent,
                val: value,
            });
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
        let val = match arg {
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

    // NOTE: signature. `Expr::Await(Box<Expr>)` inlines the operand, so this
    // takes the awaited expression instead of an `AwaitExpression` node.
    pub(crate) fn await_expr(&mut self, arg: &Expr, dst: Reg) -> R<Reg> {
        if !self.in_async {
            return Err("`await` is only valid inside an async function".into());
        }
        // Evaluate the awaited value; on resume the settled result (or a thrown
        // rejection) lands in `dst`. The VM coerces non-promises via Promise.resolve.
        let val = self.expr(arg)?;
        self.emit(Instr::Await { dst, val });
        Ok(dst)
    }

    // NOTE: signature. `Expr::Cond { test, cons, alt }` inlines the three
    // operands, so this takes them directly.
    pub(crate) fn conditional(&mut self, test: &Expr, cons: &Expr, alt: &Expr, dst: Reg) -> R<Reg> {
        // Whole pad2 conditional: for a stable plain binding, a tagged Int in
        // 0..99 can select the canonical result without materialising the 10,
        // the Bool, either control edge, or a second register load. A miss is
        // handled by the opcode's exact relational-then-Add fallback.
        //
        // Captured/direct-eval bindings are cells and fail `Binding::Local`;
        // active `with` scopes fail explicitly. Sloppy parameters are declined
        // because a mapped `arguments[0]` write during valueOf could change the
        // binding between the condition and selected arm. Strict parameters
        // and ordinary non-parameter locals have no such observable alias.
        if super::exprs::pad2_cache_enabled() && pad2_cond_fuse_enabled() {
            if let Some(name) = pad2_cond_ident(test, cons, alt) {
                let special = matches!(name, "undefined" | "NaN" | "Infinity" | "arguments");
                // Eligibility must be a pure query. `with_obj_regs` emits a
                // CellGet while materialising each with-object, which would
                // leave dead bytecode behind when this recogniser declines.
                // An eventual Binding::Local plus `in_scope` proves the name
                // is bound in this function, so an inherited with cannot
                // apply; only this function's applicable with stack matters.
                let shadowable = !self.with_objs_for(name).is_empty();
                let bad_tdz = self.param_tdz.contains(name);
                let bad_strict_name = self.cx.in_strict && is_strict_reserved_word(name);
                if !special && !shadowable && !bad_tdz && !bad_strict_name && !self.box_all_locals {
                    if let Binding::Local(src) = self.resolve(name) {
                        // Exclude a named-function-expression self binding:
                        // "plain local/parameter" means a binding in `scopes`.
                        let in_scope =
                            self.scopes.iter().rev().any(|scope| {
                                scope.iter().rev().any(|(n, r)| n == name && *r == src)
                            });
                        let is_param = src != 0 && (src as usize) <= self.param_names.len();
                        if in_scope && (!is_param || self.cx.in_strict) {
                            self.emit(Instr::Pad2Conditional { dst, src });
                            return Ok(dst);
                        }
                    }
                }
            }
        }
        // A conditional expression consumes its test exactly like `if` and
        // the loop heads do: the comparison result is not observable.  Route
        // it through the shared test emitter so a bare `<` / `<=` can use the
        // existing compare-and-branch opcode instead of materialising a Bool
        // and dispatching a second `JumpIfFalse` instruction.  Every other
        // expression keeps the byte-for-byte generic lowering in
        // `emit_test_jump`.
        let jfs = self.emit_test_jumps(test)?;
        let t = self.expr_into(cons, dst)?;
        if t != dst {
            self.emit(Instr::Move { dst, src: t });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let else_start = self.here();
        for jf in jfs {
            self.patch_jump(jf, else_start);
        }
        let e = self.expr_into(alt, dst)?;
        if e != dst {
            self.emit(Instr::Move { dst, src: e });
        }
        let end = self.here();
        self.patch_jump(jmp, end);
        Ok(dst)
    }

    /// Evaluate a non-member callee into a dedicated stable register. An
    /// identifier expression may otherwise return its live local register;
    /// argument evaluation is allowed to assign that binding, but must not
    /// redirect the call whose callee value was already obtained.
    pub(crate) fn capture_plain_callee(&mut self, callee_expr: &Expr) -> R<Reg> {
        // NOT allocated above the high-water mark (unlike a captured member
        // call's receiver/callee): a script body has dozens of plain call
        // sites, and lifting each one pushed parse-large-js's script past the
        // 64-register splice window, after which no INT splice was attempted
        // at all.
        let callee = self.alloc_reg();
        let keep = self.next_reg;
        let live = self.expr_into(callee_expr, callee)?;
        if live != callee {
            self.emit(Instr::Move {
                dst: callee,
                src: live,
            });
        }
        // The result has been copied into `callee`; scratch used while resolving
        // a compound expression is dead before ArgumentListEvaluation starts.
        self.next_reg = keep;
        Ok(callee)
    }

    /// `capture_plain_callee` for a call whose argument list is known: a
    /// register-resident local that no argument can reassign IS its own
    /// stable snapshot, so the call reads the binding's register directly
    /// (the pre-hardening lowering). The snapshot `Move` is not free: it is
    /// one more def in the loop body for every planner that keys on a
    /// register's writer, and one more live temporary across the arguments.
    pub(crate) fn capture_plain_callee_for_call(
        &mut self,
        callee_expr: &Expr,
        args: &[Arg],
    ) -> R<Reg> {
        if let Expr::Ident(id) = callee_expr {
            if self.stable_register_receiver(id, args) {
                return self.expr(callee_expr);
            }
        }
        self.capture_plain_callee(callee_expr)
    }

    /// Evaluate a member CALLEE reference into stable registers, in the exact
    /// EvaluateCall order: receiver/reference first, then GetValue (including
    /// RequireObjectCoercible, ToPropertyKey, Proxy traps, accessors and private
    /// brand checks), all before the argument list.  The returned pair is the
    /// exact callable value and `this` value which must be used later; an
    /// argument is allowed to replace the property without changing this call.
    ///
    /// Keeping this in one helper prevents the ordinary, spread, optional,
    /// specialised-static and tagged-template paths from quietly acquiring
    /// different reference-order semantics again.
    pub(crate) fn capture_member_callee(&mut self, m: &Member) -> R<(Reg, Reg)> {
        self.capture_member_callee_impl(m, false, false)
    }

    /// `capture_member_callee` for a call whose argument list is known: when
    /// the receiver is a register-resident local that no argument can
    /// reassign, the receiver register itself is the captured `this` — no
    /// snapshot `Move`. The `Move` is not free: a pinned-receiver plan keys on
    /// the register's WRITER, and a `Move` writer declined the pin (the hostile
    /// bytecode-vm's dispatch loop fell from INT to MEM, 59ms -> 267ms).
    pub(crate) fn capture_member_callee_for_call(
        &mut self,
        m: &Member,
        args: &[Arg],
    ) -> R<(Reg, Reg)> {
        let stable = match &m.object {
            Expr::Ident(id) => self.stable_register_receiver(id, args),
            _ => false,
        };
        // Only a NAMED member call gets the above-high-water callee register:
        // a computed call's callee is consumed by the computed splice (its
        // GetIndex def is dropped there), and the builtin/Math special cases
        // that share this helper must keep their register layout (the wide
        // leaf and computed-leaf lanes are sensitive to it).
        let fresh_callee = matches!(m.prop, MemberProp::Ident(_) | MemberProp::Private(_));
        self.capture_member_callee_impl(m, stable, fresh_callee)
    }

    /// The `LoadGlobal` index of `Math` when a bare `Math.<op>(…)` may read it
    /// straight from the global slot: `Math` resolves to a global (not a local,
    /// cell, upvalue or class name), no `with` object can shadow it, the
    /// function is not a dynamic (eval) zone, and the index fits the op's
    /// `this_v` field. Anything else takes the captured form.
    fn bare_math_global(&mut self) -> Option<Reg> {
        if self.box_all_locals || self.cx.dyn_global_zone {
            return None;
        }
        if !self.with_objs_for("Math").is_empty() {
            return None;
        }
        let bound_here = self.scopes.iter().flatten().any(|(n, _)| n == "Math")
            || self.self_name.as_ref().is_some_and(|(n, _)| n == "Math");
        if !bound_here && self.inherited_with_shadows.contains_key("Math") {
            return None;
        }
        match self.resolve("Math") {
            Binding::Global(idx) => Reg::try_from(idx)
                .ok()
                .filter(|&r| r != crate::bytecode::NO_REG),
            _ => None,
        }
    }

    /// Whether `name` reads as a bare register (a never-captured local — only
    /// an assignment/update in the SAME expression can write it, since a
    /// closure that wrote it would have made it a cell) and none of `args`
    /// can assign it. With-shadowable and non-local names are never stable.
    fn stable_register_receiver(&mut self, name: &str, args: &[Arg]) -> bool {
        if name == "arguments" || !self.with_objs_for(name).is_empty() {
            return false;
        }
        let bound_here = self.scopes.iter().flatten().any(|(n, _)| n == name)
            || self.self_name.as_ref().is_some_and(|(n, _)| n == name);
        if !bound_here && self.inherited_with_shadows.contains_key(name) {
            return false;
        }
        let Binding::Local(reg) = self.resolve(name) else {
            return false;
        };
        // Fixed/rest parameter registers are not stable snapshots. In a
        // sloppy function with simple parameters, an argument expression can
        // write `arguments[i]` and thereby replace the mapped parameter without
        // naming it directly. Excluding every parameter is the conservative
        // rule and matches the direct named-method argument lane below.
        let parameter_top = self.param_names.len() as Reg;
        if reg != 0 && reg <= parameter_top {
            return false;
        }
        !args.iter().any(|a| match a {
            Arg::Expr(e) | Arg::Spread(e) => expr_may_assign_name(e, name),
        })
    }

    fn capture_member_callee_impl(
        &mut self,
        m: &Member,
        stable_receiver: bool,
        fresh_callee: bool,
    ) -> R<(Reg, Reg)> {
        if matches!(&m.object, Expr::Super) {
            self.this_check();
            let this_v = self.this_override.unwrap_or(0);
            let callee = self.alloc_reg();
            match &m.prop {
                MemberProp::Ident(prop) => {
                    let name = self.string_name(prop);
                    if let Some(home_class_id) = self.super_class {
                        if self.this_override.is_some() {
                            self.emit(Instr::SuperGetRef {
                                dst: callee,
                                home_class_id,
                                name,
                                receiver: this_v,
                                is_static: self.super_static,
                            });
                        } else {
                            // Ordinary methods have receiver reg 0 and their
                            // static/instance bit is baked into FuncProto, so
                            // retain the existing JIT-supported read opcode.
                            self.emit(Instr::SuperGet {
                                dst: callee,
                                home_class_id,
                                name,
                            });
                        }
                    } else if self.super_home_obj {
                        self.emit(Instr::SuperGetObj { dst: callee, name });
                    } else {
                        return Err("`super.method(...)` is only valid in a method".into());
                    }
                }
                MemberProp::Computed(key_expr) => {
                    // The expression is evaluated before GetSuperBase; the
                    // SuperGet op then performs ToPropertyKey and the property
                    // Get now, before any argument expression is evaluated.
                    let key = self.expr(key_expr)?;
                    if let Some(home_class_id) = self.super_class {
                        if self.this_override.is_some() {
                            self.emit(Instr::SuperGetRefComputed {
                                dst: callee,
                                home_class_id,
                                key,
                                receiver: this_v,
                                is_static: self.super_static,
                            });
                        } else {
                            self.emit(Instr::SuperGetComputed {
                                dst: callee,
                                home_class_id,
                                key,
                            });
                        }
                    } else if self.super_home_obj {
                        self.emit(Instr::SuperGetObjComputed { dst: callee, key });
                    } else {
                        return Err("`super[expr](...)` is only valid in a method".into());
                    }
                }
                MemberProp::Private(_) => {
                    return Err("a private name may not be accessed through `super`".into());
                }
            }
            return Ok((callee, this_v));
        }

        // Snapshot the receiver even when the object expression happens to be a
        // local register: an argument can assign that local before the call —
        // unless the caller proved it cannot (`stable_receiver`: a bare
        // register no argument assigns), in which case the register IS the
        // captured value and the snapshot `Move` would only cost a pin.
        let this_v = if stable_receiver {
            self.expr(&m.object)?
        } else {
            // A named call's receiver snapshot is single-def by construction:
            // allocated above the high-water mark (see the callee below), it
            // is never a recycled temporary that some other statement's
            // receiver or argument also defines — the pin planner declines a
            // pinned receiver with a second def ("not cleanly excludable").
            if fresh_callee {
                self.next_reg = self.next_reg.max(self.max_reg);
            }
            let t = self.alloc_reg();
            let receiver = self.expr_into(&m.object, t)?;
            if receiver != t {
                self.emit(Instr::Move {
                    dst: t,
                    src: receiver,
                });
            }
            t
        };
        if m.optional {
            self.emit_optional_check(this_v);
        }
        // The captured callee gets a register ABOVE the function's high-water
        // mark, so its `GetProp`/`GetIndex` def never lands on a register an
        // earlier statement used for a pinned receiver. The region planners
        // tolerate a receiver register that is also a `LoadGlobal` temp (the
        // split-home path), but a property-read def on such a register is
        // "not cleanly excludable" and demotes the whole loop to the boxed
        // tier — parse-large-js's mix loop, 95ms -> 267ms, from one such
        // collision. Costs one frame slot per captured site.
        if fresh_callee {
            self.next_reg = self.next_reg.max(self.max_reg);
        }
        let callee = self.alloc_reg();
        match &m.prop {
            MemberProp::Ident(prop) => {
                let name = self.string_name(prop);
                self.emit(Instr::GetProp {
                    dst: callee,
                    obj: this_v,
                    name,
                });
            }
            MemberProp::Computed(key_expr) => {
                let key = self.expr(key_expr)?;
                self.emit(Instr::GetIndex {
                    dst: callee,
                    obj: this_v,
                    key,
                });
            }
            MemberProp::Private(field) => {
                self.check_private_declared(field)?;
                let name = self.string_name(&private_key(field));
                self.emit(Instr::GetProp {
                    dst: callee,
                    obj: this_v,
                    name,
                });
            }
        }
        Ok((callee, this_v))
    }

    /// Emit an exact captured-reference call, retaining a RegExp-heavy method
    /// spelling only as a runtime specialization hint. The callee Get has
    /// already happened, so an argument may replace the property without
    /// redirecting this call; `RegExpMethod` validates that captured Value and
    /// otherwise has the same semantics as `CallWithThis`.
    fn emit_captured_member_call(
        &mut self,
        m: &Member,
        dst: Reg,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
    ) {
        let op = match &m.prop {
            MemberProp::Ident(name) => crate::bytecode::RegExpMethod::from_name(name),
            _ => None,
        };
        if let Some(op) = op {
            self.emit(Instr::RegExpMethod {
                dst,
                op,
                callee,
                this_v,
                arg_base,
                argc,
            });
        } else {
            self.emit(Instr::CallWithThis {
                dst,
                callee,
                this_v,
                arg_base,
                argc,
            });
        }
    }

    // NOTE: `Target::Call` (Annex B `f() = 1`) has no arm to add here — neither
    // function in this file matches on an assignment target. Its lowering
    // (evaluate the call, then throw a ReferenceError) belongs to the assignment
    // module and reaches this `call` unchanged.
    pub(crate) fn call(&mut self, c: &CallExpr, dst: Reg) -> R<Reg> {
        // Optional call `f?.(args)` — EvaluateCall: a MEMBER callee (even
        // through parens) preserves its base as `this`; `super.m?.()` binds the
        // running `this`. The base's own `?.` links short-circuit inside the
        // chain, and a nullish callee bails to undefined WITHOUT evaluating
        // the arguments.
        if c.optional {
            // Parenthesization is not a node in this AST, so a parenthesized
            // callee IS the inner expression — there is nothing to peel.
            let inner: &Expr = &c.callee;
            let has_spread = c.args.iter().any(|a| matches!(a, Arg::Spread(_)));
            match inner {
                Expr::Member(m) => {
                    let save = self.next_reg;
                    let (callee, this_v) = self.capture_member_callee(m)?;
                    self.emit_optional_check(callee);
                    if has_spread {
                        let args = self.build_spread_args(&c.args)?;
                        self.emit(Instr::CallWithThisSpread {
                            dst,
                            callee,
                            this_v,
                            args,
                        });
                    } else {
                        let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                        self.emit_captured_member_call(m, dst, callee, this_v, arg_base, argc);
                    }
                    self.next_reg = save.max(dst + 1);
                    return Ok(dst);
                }
                // `(a?.b)?.()`: a parenthesized-chain member callee still
                // binds `this` = base; the inner chain's bail lands the
                // callee at undefined, then the outer `?.()` bails on it.
                // The parens are gone, but the `Chain` node they produced is
                // exactly the boundary they established.
                Expr::Chain(ce) => {
                    if let Some((callee, obj)) = self.chain_member_callee(ce)? {
                        self.emit_optional_check(callee);
                        if has_spread {
                            let args = self.build_spread_args(&c.args)?;
                            self.emit(Instr::CallWithThisSpread {
                                dst,
                                callee,
                                this_v: obj,
                                args,
                            });
                        } else {
                            let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                            self.emit(Instr::CallWithThis {
                                dst,
                                callee,
                                this_v: obj,
                                arg_base,
                                argc,
                            });
                        }
                        return Ok(dst);
                    }
                }
                _ => {}
            }
            let save = self.next_reg;
            let callee = self.capture_plain_callee(&c.callee)?;
            self.emit_optional_check(callee);
            if has_spread {
                // `fn?.(...xs)`: spread args after the nullish bail.
                let args_arr = self.build_spread_args(&c.args)?;
                self.emit(Instr::CallSpread {
                    dst,
                    callee,
                    args: args_arr,
                });
                self.next_reg = save.max(dst + 1);
                return Ok(dst);
            }
            let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
            self.emit(Instr::Call {
                dst,
                callee,
                arg_base,
                argc,
            });
            self.next_reg = save.max(dst + 1);
            return Ok(dst);
        }
        // Spread call: `f(...args)`, `obj.m(...args)`, `arr.push(...xs)`, etc.
        // Build the argument list as an array (spreading each `...x` element),
        // then dispatch via CallMethodSpread (method receiver) or CallSpread
        // (plain function value). Spread on a builtin like Math.max(...arr) that
        // isn't a method call is out of scope.
        if c.args.iter().any(|a| matches!(a, Arg::Spread(_))) {
            // `super(...args)` — spread into the superclass constructor. Handled
            // here (before the generic branches) because `super` is not a value
            // and would fail `expr(callee)`.
            if matches!(&c.callee, Expr::Super) {
                if !self.derived_class {
                    return Err("`super(...)` is only valid in a derived class constructor".into());
                }
                let pid = self
                    .super_class
                    .ok_or("`super(...)` is only valid in a derived class constructor")?;
                // GetSuperConstructor BEFORE the args (spec SuperCall order).
                let ctor = self.temp();
                self.emit(Instr::SuperCtorFetch {
                    dst: ctor,
                    home_class_id: pid,
                });
                let args_arr = self.build_spread_args(&c.args)?;
                self.emit(Instr::SuperCtorSpread {
                    ctor,
                    home_class_id: pid,
                    args: args_arr,
                });
                // `super(...)` evaluates to the new bound `this` (BindThisValue's
                // result) — SuperCtorSpread rebinds reg 0 to it (call-expr-value).
                self.emit(Instr::Move { dst, src: 0 });
                return Ok(dst);
            }
            // `Math.max(...arr)` / `Math.min(...arr)` / `Math.hypot(...arr)` —
            // a variadic Math reduction over the spread array. Capture both the
            // live method and receiver before iterating the spread arguments;
            // the opcode validates them before taking its intrinsic fast path.
            if let Expr::Member(m) = &c.callee {
                if let MemberProp::Ident(prop) = &m.prop {
                    if let Expr::Ident(obj) = &m.object {
                        if &**obj == "Math" {
                            if let Some(op) = crate::bytecode::MathFn::from_name(prop) {
                                let save = self.next_reg;
                                let (callee, this_v) = self.capture_member_callee(m)?;
                                let args_arr = self.build_spread_args(&c.args)?;
                                self.emit(Instr::MathSpread {
                                    dst,
                                    op,
                                    callee,
                                    this_v,
                                    args: args_arr,
                                });
                                self.next_reg = save.max(dst + 1);
                                return Ok(dst);
                            }
                        }
                    }
                }
            }
            // A syntactic `eval(...args)` remains a direct-eval candidate no
            // matter which environment record supplied the named reference.
            // Capture the exact callee (and WithBaseObject, when applicable)
            // before spread iteration: an iterator may rebind `eval`, but it
            // cannot redirect this already-resolved call.
            if let Expr::Ident(id) = &c.callee {
                if &**id == "eval" {
                    let save = self.next_reg;
                    let with_objs = self.with_obj_regs(id);
                    let (callee, this_v) = if with_objs.is_empty() {
                        let callee = self.alloc_reg();
                        let live = self.expr_into(&c.callee, callee)?;
                        if live != callee {
                            self.emit(Instr::Move {
                                dst: callee,
                                src: live,
                            });
                        }
                        let this_v = self.alloc_reg();
                        self.emit(Instr::LoadUndefined { dst: this_v });
                        (callee, this_v)
                    } else {
                        self.emit_with_callee_chain(id, &with_objs)
                    };
                    let args_arr = self.build_spread_args(&c.args)?;
                    self.emit_direct_eval(callee, this_v, args_arr, 0, true, dst, false);
                    self.next_reg = save.max(dst + 1);
                    return Ok(dst);
                }
            }
            // Every member reference is fully resolved before spread iteration.
            // Carry the exact callable and receiver through the argument-list
            // build: an iterator, getter, proxy trap or value conversion may
            // replace the property without changing this call's target.
            if let Expr::Member(m) = &c.callee {
                let save = self.next_reg;
                let (callee, this_v) = self.capture_member_callee(m)?;
                let args = self.build_spread_args(&c.args)?;
                self.emit(Instr::CallWithThisSpread {
                    dst,
                    callee,
                    this_v,
                    args,
                });
                self.next_reg = save.max(dst + 1);
                return Ok(dst);
            }
            // `(obj?.method)(...args)` retains the member reference across the
            // chain boundary. A nullish inner base produces an undefined
            // callee; because this outer call is not optional, its spread list
            // is still evaluated before the ensuing not-callable error.
            if let Expr::Chain(ce) = &c.callee {
                if let Some((callee, this_v)) = self.chain_member_callee(ce)? {
                    let args = self.build_spread_args(&c.args)?;
                    self.emit(Instr::CallWithThisSpread {
                        dst,
                        callee,
                        this_v,
                        args,
                    });
                    return Ok(dst);
                }
            }
            let save = self.next_reg;
            let callee = self.capture_plain_callee(&c.callee)?;
            let args_arr = self.build_spread_args(&c.args)?;
            self.emit(Instr::CallSpread {
                dst,
                callee,
                args: args_arr,
            });
            self.next_reg = save.max(dst + 1);
            return Ok(dst);
        }

        // `super(args)` — run the superclass constructor on the current `this`.
        if matches!(&c.callee, Expr::Super) {
            if !self.derived_class {
                return Err("`super(...)` is only valid in a derived class constructor".into());
            }
            let pid = self
                .super_class
                .ok_or("`super(...)` is only valid in a derived class constructor")?;
            // (Spread `super(...args)` is handled in the spread block above.)
            // GetSuperConstructor BEFORE the args (spec SuperCall order: the
            // fetch, then ArgumentListEvaluation, then the IsConstructor check).
            let ctor = self.temp();
            self.emit(Instr::SuperCtorFetch {
                dst: ctor,
                home_class_id: pid,
            });
            let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
            self.emit(Instr::SuperCtor {
                ctor,
                home_class_id: pid,
                arg_base,
                argc,
            });
            // `super()` evaluates to the new bound `this` (BindThisValue's
            // result) — SuperCtor rebinds reg 0 to it (call-expr-value).
            self.emit(Instr::Move { dst, src: 0 });
            return Ok(dst);
        }
        // A syntactic `eval(args)` is a direct-eval candidate for global,
        // local, parameter, upvalue and `with` bindings alike. Resolve and
        // SNAPSHOT the reference before evaluating any argument. DirectEval
        // validates the captured value against this realm's %eval%; a miss
        // ordinary-calls that exact value with every evaluated argument.
        if let Expr::Ident(id) = &c.callee {
            if &**id == "eval" {
                let save = self.next_reg;
                let with_objs = self.with_obj_regs(id);
                let (callee, this_v) = if with_objs.is_empty() {
                    let callee = self.alloc_reg();
                    let live = self.expr_into(&c.callee, callee)?;
                    if live != callee {
                        self.emit(Instr::Move {
                            dst: callee,
                            src: live,
                        });
                    }
                    let this_v = self.alloc_reg();
                    self.emit(Instr::LoadUndefined { dst: this_v });
                    (callee, this_v)
                } else {
                    self.emit_with_callee_chain(id, &with_objs)
                };
                let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                self.emit_direct_eval(callee, this_v, arg_base, argc, false, dst, false);
                self.next_reg = save.max(dst + 1);
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
        if let Expr::Ident(id) = &c.callee {
            let with_objs = self.with_obj_regs(id);
            if !with_objs.is_empty() {
                let save = self.next_reg;
                let (callee_reg, this_reg) = self.emit_with_callee_chain(id, &with_objs);
                // Arguments evaluate AFTER the callee reference resolved.
                let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
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

        // Keep only the two hot bare-builtin bytecodes. Every lower-frequency
        // constructor/function (Error, Symbol, RegExp, BigInt, Object and the
        // host `print`) deliberately reaches the captured generic Call below.
        // Besides avoiding a large family of guards, that path evaluates every
        // extra argument and naturally preserves replacement/proxy semantics.
        // Number(x) / parseInt(s,radix) / parseFloat(s) → guarded GlobalFn op.
        if let Expr::Ident(id) = &c.callee {
            if let Some(op) =
                crate::bytecode::GlobalFn::from_name(id).filter(|_| self.builtin_unshadowed(id))
            {
                let save = self.next_reg;
                let callee = self.capture_plain_callee(&c.callee)?;
                let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                self.emit(Instr::GlobalFn {
                    dst,
                    op,
                    callee,
                    arg_base,
                    argc,
                });
                self.next_reg = save.max(dst + 1);
                return Ok(dst);
            }
            if &**id == "Array" && self.builtin_unshadowed(id) {
                let save = self.next_reg;
                let callee = self.capture_plain_callee(&c.callee)?;
                let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                self.emit(Instr::ArrayCtor {
                    dst,
                    callee: Some(callee),
                    arg_base,
                    argc,
                    is_construct: false,
                });
                self.next_reg = save.max(dst + 1);
                return Ok(dst);
            }
        }

        // console.log(...) → Print opcode.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    if &**obj == "console"
                        && matches!(&**prop, "log" | "info" | "warn" | "error" | "debug")
                        && self.builtin_unshadowed("console")
                    {
                        // node routes console.error / console.warn to stderr.
                        let to_stderr = matches!(&**prop, "error" | "warn");
                        let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                        self.emit(Instr::Print {
                            arg_base,
                            argc,
                            to_stderr,
                        });
                        self.emit(Instr::LoadUndefined { dst });
                        return Ok(dst);
                    }
                }
            }
        }

        // The host-only `performance.now()` pseudo-namespace has no live object
        // to capture. Date is a real namespace and therefore deliberately falls
        // through to the captured ordinary-call path below, so replacements,
        // accessors and a rebound global remain observable.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    let epoch = match (&**obj, &**prop) {
                        ("performance", "now") => Some(false),
                        _ => None,
                    };
                    if let Some(epoch) = epoch.filter(|_| self.builtin_unshadowed("performance")) {
                        // Although the host pseudo ignores its arguments, call
                        // argument expressions are still evaluated in order.
                        let _ = self.eval_args_contiguous(&c.args)?;
                        self.emit(Instr::Now { dst, epoch });
                        return Ok(dst);
                    }
                }
            }
        }

        // `JSON.parse(text)` / `JSON.stringify(value)` → fast ops. The forms with
        // a reviver / replacer (2+ args) fall through to the generic call so the
        // `JSON_PARSE` / `JSON_STRINGIFY` natives can honour them.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    if &**obj == "JSON" && &**prop == "parse" && c.args.len() == 1 {
                        if arg_expr(&c.args[0]).is_some() {
                            let save = self.next_reg;
                            let (callee, this_v) = self.capture_member_callee(m)?;
                            let (arg_base, _) = self.eval_args_contiguous(&c.args)?;
                            self.emit(Instr::JsonParse {
                                dst,
                                a: arg_base,
                                callee,
                                this_v,
                            });
                            self.next_reg = save.max(dst + 1);
                            return Ok(dst);
                        }
                    }
                    if &**obj == "JSON" && &**prop == "stringify" && c.args.len() == 1 {
                        if arg_expr(&c.args[0]).is_some() {
                            let save = self.next_reg;
                            let (callee, this_v) = self.capture_member_callee(m)?;
                            let (arg_base, _) = self.eval_args_contiguous(&c.args)?;
                            let space = self.alloc_reg();
                            self.emit(Instr::LoadUndefined { dst: space });
                            self.emit(Instr::JsonStringify {
                                dst,
                                val: arg_base,
                                space,
                                callee,
                                this_v,
                            });
                            self.next_reg = save.max(dst + 1);
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // Guarded `Array.isArray(x)`: capture the live member before `x`, then
        // validate the intrinsic identities inside the opcode.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    if &**obj == "Array" && &**prop == "isArray" && c.args.len() == 1 {
                        if arg_expr(&c.args[0]).is_some() {
                            let save = self.next_reg;
                            let (callee, this_v) = self.capture_member_callee(m)?;
                            let (arg_base, _) = self.eval_args_contiguous(&c.args)?;
                            self.emit(Instr::IsArray {
                                dst,
                                a: arg_base,
                                callee,
                                this_v,
                            });
                            self.next_reg = save.max(dst + 1);
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // Guarded Object.keys/values/entries. Keep the dedicated opcodes (and
        // Object.keys reducers) while giving every miss an ordinary-call exit.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    let kind = if &**obj == "Object" && c.args.len() == 1 {
                        match &**prop {
                            "keys" => Some(0u8),
                            "values" => Some(1u8),
                            "entries" => Some(2u8),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if let Some(kind) = kind {
                        if arg_expr(&c.args[0]).is_some() {
                            let save = self.next_reg;
                            let (callee, this_v) = self.capture_member_callee(m)?;
                            let (arg_base, _) = self.eval_args_contiguous(&c.args)?;
                            self.emit(match kind {
                                0 => Instr::ObjectKeys {
                                    dst,
                                    obj: arg_base,
                                    callee,
                                    this_v,
                                },
                                1 => Instr::ObjectValues {
                                    dst,
                                    obj: arg_base,
                                    callee,
                                    this_v,
                                },
                                _ => Instr::ObjectEntries {
                                    dst,
                                    obj: arg_base,
                                    callee,
                                    this_v,
                                },
                            });
                            self.next_reg = save.max(dst + 1);
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // `Math.<fn>(args…)` → MathOp. These legacy intrinsics predate the
        // first-class Math namespace object. Do not add `random` here: unlike a
        // pure arithmetic shortcut, a syntactic `Math.random()` must observe an
        // own-property replacement (`Math.random = seededRandom`). The former
        // unconditional `Random` lowering silently ignored that replacement in
        // both the interpreter and JIT because the decision was made at compile
        // time. Let the ordinary member-call path resolve `random` dynamically.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    if &**obj == "Math" {
                        if let Some(op) = crate::bytecode::MathFn::from_name(prop) {
                            // BARE form (see `Instr::MathOp`): every argument is
                            // order-transparent and `Math` is a plain global
                            // read, so no receiver/callee pair is captured and
                            // the op validates the live global + own slot at
                            // execution. The captured pair was two registers
                            // per site in the loop body, and its recycling was
                            // what turned pinned receivers into split receivers
                            // (the hostile bytecode-vm's dispatch loop fell from
                            // INT-GPR to MEM, 68ms -> 260ms; multi_split's string
                            // receiver lost its split; a wide leaf lost its
                            // register budget). `ZIPP_STRICT_CALL_ORDER=1`
                            // narrows the transparent class exactly as it does
                            // for `CallMethod`.
                            let bare = c.args.iter().all(|a| match a {
                                Arg::Expr(e) => self.arg_order_transparent(e),
                                Arg::Spread(_) => false,
                            });
                            if bare {
                                if let Some(gidx) = self.bare_math_global() {
                                    let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                                    self.emit(Instr::MathOp {
                                        dst,
                                        op,
                                        callee: crate::bytecode::NO_REG,
                                        this_v: gidx,
                                        arg_base,
                                        argc,
                                    });
                                    return Ok(dst);
                                }
                            }
                            // CAPTURED form. NOTE: no `next_reg` reset after the
                            // op: recycling the pair as numeric temporaries made
                            // the planner see a "recycled pinned receiver" and
                            // route it through the split machinery.
                            let (callee, this_v) = self.capture_member_callee(m)?;
                            let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                            self.emit(Instr::MathOp {
                                dst,
                                op,
                                callee,
                                this_v,
                                arg_base,
                                argc,
                            });
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // Constructor-namespace static methods with a flat argument list.
        //
        // EvaluateCall resolves the callee reference BEFORE evaluating any
        // argument. Snapshot both the live namespace receiver and its method in
        // stable registers, then let StaticFn use the specialised implementation
        // only when both identify this realm's pristine intrinsic. A replacement,
        // accessor, lexical shadow, or rebound global takes the ordinary call path
        // with the captured callee/receiver; an argument which mutates the method
        // therefore cannot retroactively change which function is invoked.
        if let Expr::Member(m) = &c.callee {
            if let MemberProp::Ident(prop) = &m.prop {
                if let Expr::Ident(obj) = &m.object {
                    if let Some(op) = crate::bytecode::StaticFn::from_name(obj, prop) {
                        let save = self.next_reg;
                        let (callee, this_v) = self.capture_member_callee(m)?;
                        let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                        self.emit(Instr::StaticFn {
                            dst,
                            op,
                            callee,
                            this_v,
                            arg_base,
                            argc,
                        });
                        self.next_reg = save.max(dst + 1);
                        return Ok(dst);
                    }
                    // `Array.from(src[, mapFn])` — needs iteration + optional
                    // callback. The 3rd `thisArg` form stays generic so the native
                    // receives it; 1-/2-arg forms keep the dedicated opcode.
                    if &**obj == "Array" && &**prop == "from" && (1..=2).contains(&c.args.len()) {
                        if c.args.iter().all(|a| arg_expr(a).is_some()) {
                            let save = self.next_reg;
                            let (callee, this_v) = self.capture_member_callee(m)?;
                            let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                            let mapfn = if argc == 2 {
                                arg_base + 1
                            } else {
                                let r = self.alloc_reg();
                                self.emit(Instr::LoadUndefined { dst: r });
                                r
                            };
                            self.emit(Instr::ArrayFrom {
                                dst,
                                src: arg_base,
                                mapfn,
                                callee,
                                this_v,
                                argc,
                            });
                            self.next_reg = save.max(dst + 1);
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // A parenthesized member callee preserves its reference: `(a.b)()` and
        // `(a?.b)()` call with `this` = a. Parentheses are absent from this AST;
        // the optional-chain boundary is the only surviving wrapper.
        let peeled: &Expr = &c.callee;
        if let Expr::Chain(ce) = peeled {
            if let Some((callee, this_v)) = self.chain_member_callee(ce)? {
                let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                self.emit(Instr::CallWithThis {
                    dst,
                    callee,
                    this_v,
                    arg_base,
                    argc,
                });
                return Ok(dst);
            }
        }

        // Fused method call: `obj.name(args…)` / `obj.#m(args…)` whose
        // arguments are ALL order-transparent (`arg_order_transparent`; the
        // classes and the `ZIPP_STRICT_CALL_ORDER` latch are documented
        // there). The fused `CallMethod` performs the property Get AFTER the
        // arguments are in their registers; for such arguments that is
        // indistinguishable from EvaluateCall's Get-before-
        // ArgumentListEvaluation order. The fused op is what every method-call
        // lane in the interpreter and both JIT tiers keys on (method ICs, the
        // intrinsic push/charCodeAt arms, method inlining, the cross-call
        // lane); the split GetProp+CallWithThis form below has none of them.
        // Every other argument shape — a call, a `new`, a spread, an
        // assignment — keeps the captured path, whose ordering
        // `tests/call_reference_order.rs` pins.
        if let Expr::Member(m) = peeled {
            // A RegExp-flavoured spelling (`test`/`exec`/`matchAll`/`replace`)
            // keeps the captured path even with transparent arguments: its
            // `RegExpMethod` op carries the regexp / string-regexp direct lanes
            // (`regexp_call_direct`, `string_regexp_call_direct`, the matchAll
            // batch), which the fused arm no longer routes — measured as the
            // whole of regex-log-scan's matchAll phase (58ms -> 309ms fused).
            let regexp_spelling = match &m.prop {
                MemberProp::Ident(prop) => crate::bytecode::RegExpMethod::from_name(prop).is_some(),
                _ => false,
            };
            if !matches!(m.object, Expr::Super)
                && !regexp_spelling
                && c.args.iter().all(|a| match a {
                    Arg::Expr(e) => self.arg_order_transparent(e),
                    Arg::Spread(_) => false,
                })
            {
                let obj = self.expr(&m.object)?;
                if m.optional {
                    // `obj?.method(args)` — short-circuit on a nullish receiver.
                    self.emit_optional_check(obj);
                }
                if let MemberProp::Computed(key_expr) = &m.prop {
                    // Fused computed call `obj[key](args…)`: the receiver
                    // and key are snapshotted into fresh temporaries and the
                    // op performs ToPropertyKey + Get at call time. With
                    // transparent arguments nothing between the snapshot and
                    // the op can observe that order. The INT computed splice
                    // (dense small-int keys over an array of closures) and
                    // the MEM computed lane key on this op; the split
                    // GetIndex + CallWithThis form only has the generic path.
                    let obj_reg = self.alloc_reg();
                    if obj != obj_reg {
                        self.emit(Instr::Move {
                            dst: obj_reg,
                            src: obj,
                        });
                    }
                    let key = self.expr(key_expr)?;
                    let key_reg = self.alloc_reg();
                    if key != key_reg {
                        self.emit(Instr::Move {
                            dst: key_reg,
                            src: key,
                        });
                    }
                    let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
                    self.emit(Instr::CallMethodComputed {
                        dst,
                        obj: obj_reg,
                        key: key_reg,
                        arg_base,
                        argc,
                    });
                    return Ok(dst);
                }
                let name = match &m.prop {
                    MemberProp::Ident(prop) => self.string_name(prop),
                    MemberProp::Private(field) => {
                        self.check_private_declared(field)?;
                        self.string_name(&private_key(field))
                    }
                    MemberProp::Computed(_) => unreachable!("handled above"),
                };
                let (arg_base, argc) = self.eval_named_method_args(&c.args, obj, dst)?;
                // NOTE: no `next_reg` reset here, deliberately — the fused
                // lowering has always left its receiver/argument temporaries
                // allocated. Recycling them (as the captured path below does)
                // merges live ranges across a loop body: the register tier's
                // glob-range pass then sees loop-carried homes that do not
                // exist and declines INT-GPR (typedarray-math's DataView loop
                // went 10 -> 12 homes and fell to MEM, 159ms -> 571ms).
                self.emit(Instr::CallMethod {
                    dst,
                    obj,
                    name,
                    arg_base,
                    argc,
                });
                return Ok(dst);
            }
        }

        // Resolve every member reference completely before arguments, and call
        // the captured value with the captured receiver. Besides enforcing
        // EvaluateCall ordering, this prevents property replacement during an
        // argument from redirecting the invocation.
        // NOTE: no `next_reg` reset after the call (the fused lowering above
        // and the plain call below never had one either). Recycling the
        // callee/receiver temporaries lets the next statement of a loop body
        // redefine them, and the register tier's glob-range pass then sees
        // one merged live range across the call instead of two short ones:
        // the hostile bytecode-vm's spliced dispatch loop went 15 -> 16 homes,
        // one over the pool, and fell from INT-GPR to MEM (68ms -> 260ms).
        if let Expr::Member(m) = peeled {
            let (callee, this_v) = self.capture_member_callee_for_call(m, &c.args)?;
            let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
            self.emit_captured_member_call(m, dst, callee, this_v, arg_base, argc);
            return Ok(dst);
        }

        // (Bare-identifier calls inside a `with` were already routed through the
        // with chain near the top of this function — before the builtin
        // special-cases — so nothing with-related remains here.)

        // General call: snapshot GetValue(callee), then evaluate the complete
        // argument list. In particular, `f(f = replacement)` still invokes the
        // original function value.
        //
        // NOTE: no `next_reg` reset after the call, deliberately (the plain
        // call has never had one). Recycling the callee/argument temporaries
        // across the statements of a loop body makes the register the INT
        // splice pins for the callee's identity multi-def, and the planner
        // then declines the whole loop ("not cleanly excludable"): parse-
        // large-js's `mix(...)` loop fell from INT to MEM, 95ms -> 267ms.
        let callee = self.capture_plain_callee_for_call(&c.callee, &c.args)?;
        let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
        self.emit(Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        });
        Ok(dst)
    }

    /// Emit the `DirectEval` op for a syntactic `eval(<args>)` call site:
    /// builds the visible-caller-bindings site map and the instruction.
    /// The exact callee/reference receiver have already been captured, before
    /// any argument evaluation. `args_array` selects the dynamically-sized
    /// spread array held in `arg_base`; otherwise the complete argument window
    /// is `[arg_base, arg_base + argc)`. `tail` lets a non-intrinsic captured
    /// callee reuse the frame as an ordinary proper-tail call.
    pub(crate) fn emit_direct_eval(
        &mut self,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
        args_array: bool,
        dst: Reg,
        tail: bool,
    ) {
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
                        // Kind 3 marks a CATCH PARAMETER: the eval closes over
                        // its cell like any caller binding (kind 0), but an
                        // eval'd `var` of the same name still declares into the
                        // function's varEnv (Annex B.3.5) instead of being
                        // absorbed by the caller binding.
                        let kind = if self.catch_param_regs.contains(r) {
                            3u8
                        } else {
                            0u8
                        };
                        map.push((n.clone(), kind, *r));
                    }
                }
            }
            if let Some((n, r)) = self.self_name.clone() {
                if self.cell_regs.contains(&r) && seen.insert(n.clone()) {
                    map.push((n, 0u8, r));
                }
            }
            // ENCLOSING function scopes. `free_vars` cannot look inside the eval
            // STRING, so nothing made this function capture the enclosing
            // activation's bindings — and with no upvalue there is no cell to
            // hand the eval program, which then resolved the name as a global
            // (`function o(){ var a=1; return function(){ return eval("a"); }; }`
            // threw ReferenceError). Force one upvalue per visible enclosing
            // binding, innermost first, so the shadowing order the map wants is
            // the order they are pushed in. Only reached in a body that
            // references `eval`, so ordinary functions keep their exact
            // capture set.
            if !eval_root {
                let outer: Vec<String> = self
                    .enclosing
                    .iter()
                    .rev()
                    .flat_map(|enc| {
                        let mut ns: Vec<String> = enc
                            .cell_locals
                            .iter()
                            .rev()
                            .map(|(n, _)| n.clone())
                            .collect();
                        ns.extend(enc.upvalues.borrow().iter().map(|(n, _)| n.clone()));
                        ns
                    })
                    .collect();
                for n in outer {
                    if seen.contains(&n) {
                        continue;
                    }
                    if self.resolve_upvalue(&n).is_some() {
                        seen.insert(n);
                    }
                }
            }
            // Every upvalue this closure holds is a live caller binding the eval
            // may name: the ones just forced, the ones the body genuinely
            // captures, and — at an eval ROOT — its seeded caller scope, so
            // nested evals keep reaching the original caller. Kind 1 at an eval
            // root (those cells ARE the eval's variable environment); kind 2
            // otherwise — an ENCLOSING function's binding is readable but a
            // top-level `var` of that name shadows it instead of assigning it.
            let ups: Vec<String> = self
                .upvalues
                .borrow()
                .iter()
                .map(|(n, _)| n.clone())
                .collect();
            let kind = if eval_root { 1u8 } else { 2u8 };
            for (i, n) in ups.iter().enumerate() {
                if !map.iter().any(|(m, _, _)| m == n) {
                    map.push((n.clone(), kind, i as u16));
                }
            }
            // In a parameter default, the eval's sloppy var/function
            // names may not collide with the PARAM scope (the params
            // and, for non-arrows, the implicit `arguments`).
            let param_collisions = if self.in_param_init {
                let mut names = self.param_names.clone();
                if self.arguments_reg.is_some() && !names.iter().any(|n| n == "arguments") {
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
            callee,
            this_v,
            arg_base,
            argc,
            args_array,
            new_target_ok: self.cx.new_target_ok,
            this_reg,
            home_class: self.super_class.unwrap_or(u32::MAX),
            super_static: self.super_static,
            // `super()` in a direct eval needs the CONSTRUCTOR of a derived
            // class (`in_derived_ctor`, which arrows inherit) — `derived_class`
            // is also true in that class's methods, where the eval rules "as
            // outside a constructor" apply. A field INITIALIZER runs inside the
            // ctor's compiler but is likewise "outside a constructor" for these
            // early errors (derived-cls-direct-eval-err-contains-supercall).
            derived_ctor: self.in_derived_ctor && !self.cx.in_field_init,
            class_name_ok: self.class_inner_name_visible(),
            ban_arguments: self.cx.in_field_init,
            strict_caller: self.cx.in_strict,
            super_home_obj: self.super_home_obj,
            // The eval's variable environment: GLOBAL only when the
            // call site is the script top level (a function/arrow/param
            // context keeps the old slot behavior until the dynamic
            // caller-env lands). A sloppy FUNCTION-context eval root is
            // NOT global either: its varEnv is the caller activation's
            // EvalScope, so a direct eval nested inside it declares there
            // too — `eval("eval('var x=1')")` in a function must not leak
            // `x` to the realm global.
            var_env_is_global: self.is_script && !self.cx.eval_fn_context,
            site,
            tail,
        });
    }

    /// Whether evaluating argument `e` is ORDER-TRANSPARENT with respect to a
    /// method call's property Get, so `call` may emit the fused `CallMethod`
    /// (Get AFTER the arguments) in place of EvaluateCall's Get-before-
    /// arguments capture without changing what any program can observe.
    ///
    /// Two classes, one latch:
    ///
    /// * The PROVABLE class — always accepted. The argument cannot run user
    ///   code, cannot throw and reads nothing a getter could mutate: a literal,
    ///   `this` outside a derived constructor's TDZ, a register-resident local
    ///   (never captured, so no closure — and no getter — can write it), the
    ///   `undefined`/`NaN`/`Infinity` constant folds, and `!`/`typeof`/`void`,
    ///   `&&`/`||`/`??` and `?:` over such operands (ToBoolean runs no code).
    /// * The PRIMITIVE-OPERAND class — accepted unless `ZIPP_STRICT_CALL_ORDER`
    ///   is set. Reads of globals, cells and upvalues; arithmetic /
    ///   comparison / string-concatenation / template forms over transparent
    ///   operands; array and plain-keyed object literals of transparent parts;
    ///   closure literals; property, element and private reads over
    ///   transparent parts. These run user code ONLY through an object
    ///   operand's `valueOf`/`toString`/`Symbol.toPrimitive` or an accessor /
    ///   proxy trap on a read's own base, throw only on a TDZ, a never-declared
    ///   name, a nullish base or a private brand miss, and read state only a
    ///   side-effecting
    ///   accessor on the CALLEE property could have changed first. Every one of
    ///   those needs an exotic partner at the same site (a getter or proxy trap
    ///   on `obj.name` that mutates the caller's state, or an object operand
    ///   with a callee-replacing coercion) — patterns without a legitimate
    ///   use, and exactly what the pre-hardening engine assumed at every call
    ///   site. The latch exists so a conformance or differential run can prove
    ///   the split lowering against the provable class alone.
    ///
    /// Everything else stays captured: a
    /// call, `new`, an assignment/update, a spread, `in`/`instanceof` (proxy
    /// `has` / `Symbol.hasInstance` traps), a with-shadowable name, a dynamic
    /// (eval) zone read, `arguments`, a parameter in its own default's TDZ, a
    /// class name (LoadClassValue's TDZ). A new kind must prove its class.
    pub(crate) fn arg_order_transparent(&mut self, e: &Expr) -> bool {
        let relaxed = !strict_call_order();
        self.arg_transparent_in(e, relaxed)
    }

    fn arg_transparent_in(&mut self, e: &Expr, relaxed: bool) -> bool {
        match e {
            Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => true,
            // `this` is a plain register read, except in a derived constructor
            // (before `super()` it is in TDZ: `this_check` emits a throwing
            // check) unless a static initializer redirected it.
            Expr::This => !self.in_derived_ctor || self.this_override.is_some(),
            // ToBoolean / typeof / void never invoke user code or throw.
            Expr::Unary {
                op: UnaryOp::Not | UnaryOp::Typeof | UnaryOp::Void,
                arg,
            } => self.arg_transparent_in(arg, relaxed),
            Expr::Logical { left, right, .. } => {
                self.arg_transparent_in(left, relaxed) && self.arg_transparent_in(right, relaxed)
            }
            Expr::Cond { test, cons, alt } => {
                self.arg_transparent_in(test, relaxed)
                    && self.arg_transparent_in(cons, relaxed)
                    && self.arg_transparent_in(alt, relaxed)
            }
            Expr::Ident(id) => self.ident_order_transparent(id, relaxed),
            // ── primitive-operand class ──────────────────────────────────
            // ToNumber / ToPrimitive on an OBJECT operand is user code.
            Expr::Unary {
                op: UnaryOp::Minus | UnaryOp::Plus | UnaryOp::BitNot,
                arg,
            } => relaxed && self.arg_transparent_in(arg, relaxed),
            // `in` probes (proxy `has`) and `instanceof` (Symbol.hasInstance)
            // run user code on ordinary operands; every other operator only
            // through an object operand's coercion.
            Expr::Binary { op, left, right } => {
                relaxed
                    && !matches!(op, BinaryOp::In | BinaryOp::Instanceof)
                    && self.arg_transparent_in(left, relaxed)
                    && self.arg_transparent_in(right, relaxed)
            }
            // ToString of an object substitution is user code.
            Expr::Template(t) => {
                relaxed && t.exprs.iter().all(|x| self.arg_transparent_in(x, relaxed))
            }
            // An allocation; holes are fine, a spread iterates user code.
            Expr::Array(elems, _) => {
                relaxed
                    && elems.iter().all(|el| match el {
                        None => true,
                        Some(ArrayElem::Expr(x)) => self.arg_transparent_in(x, relaxed),
                        Some(ArrayElem::Spread(_)) => false,
                    })
            }
            // An allocation with plain (identifier / string / number) keys and
            // transparent values; a method/accessor DEFINITION only allocates
            // its closure. A computed key runs ToPropertyKey and a spread
            // enumerates (ownKeys/get traps) — both user code.
            Expr::Object(members, _) => {
                relaxed
                    && members.iter().all(|mbr| match mbr {
                        ObjectMember::Prop {
                            key,
                            value,
                            init: None,
                            ..
                        } => {
                            matches!(key, PropKey::Ident(_) | PropKey::Str(_) | PropKey::Num(_))
                                && self.arg_transparent_in(value, relaxed)
                        }
                        ObjectMember::Method { key, .. }
                        | ObjectMember::Get { key, .. }
                        | ObjectMember::Set { key, .. } => {
                            matches!(key, PropKey::Ident(_) | PropKey::Str(_) | PropKey::Num(_))
                        }
                        _ => false,
                    })
            }
            // A closure literal is an allocation (its captures are already
            // cells); a class expression evaluates computed keys and static
            // blocks, so it stays captured.
            Expr::Arrow(_) | Expr::Function(_) => relaxed,
            // A plain property / element / private read (optional or not)
            // over transparent parts: `x.prop`, `x[i]`, `this.#p`. It runs
            // user code ONLY through an accessor or proxy trap on the
            // ARGUMENT's own base — an exotic partner in exactly the sense
            // of the class above — and throws only on a nullish base or a
            // private brand miss. `super.x` is refused by the object
            // recursion (`Expr::Super` is not transparent).
            Expr::Member(m) => {
                relaxed
                    && self.arg_transparent_in(&m.object, relaxed)
                    && match &m.prop {
                        MemberProp::Ident(_) | MemberProp::Private(_) => true,
                        MemberProp::Computed(k) => self.arg_transparent_in(k, relaxed),
                    }
            }
            _ => false,
        }
    }

    /// The identifier case of `arg_transparent_in`. Provable: a bare register
    /// reference or a constant fold. Primitive-operand class (`relaxed`): a
    /// `LoadGlobal`, `CellGet` or `UpvalGet` — a data read whose only failure
    /// is a TDZ / never-declared ReferenceError.
    fn ident_order_transparent(&mut self, name: &str, relaxed: bool) -> bool {
        // `arguments` materializes the arguments object (and `resolve` flags
        // the function); a parameter in its own default's TDZ throws.
        if name == "arguments" || self.param_tdz.contains(name) {
            return false;
        }
        // A with-object — own or inherited from an enclosing function — may
        // shadow the name; its HasBinding/Get probes are observable. This
        // mirrors `with_obj_regs` without materializing its probe registers.
        if !self.with_objs_for(name).is_empty() {
            return false;
        }
        let bound_here = self.scopes.iter().flatten().any(|(n, _)| n == name)
            || self.self_name.as_ref().is_some_and(|(n, _)| n == name);
        if !bound_here && self.inherited_with_shadows.contains_key(name) {
            return false;
        }
        match self.resolve(name) {
            // Register-resident: the read is the register itself.
            Binding::Local(_) => true,
            Binding::Global(_) => {
                // A dynamic (eval) zone resolves through LoadGlobalDyn, whose
                // EvalScope probe may find a TDZ binding.
                if self.box_all_locals || self.cx.dyn_global_zone {
                    return false;
                }
                // `undefined`/`NaN`/`Infinity` fold to constant loads under
                // exactly these conditions (see the identifier read in exprs.rs).
                if matches!(name, "undefined" | "NaN" | "Infinity") {
                    return true;
                }
                relaxed
            }
            Binding::LocalCell(_) | Binding::Upvalue(_) => relaxed,
            // Lexical: LoadClassValue throws in the class's TDZ.
            Binding::ClassName(_) => false,
        }
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
    pub(crate) fn eval_args_contiguous(&mut self, args: &[Arg]) -> R<(Reg, u16)> {
        let exprs: Vec<&Expr> = args
            .iter()
            .map(|a| {
                arg_expr(a)
                    .ok_or_else(|| "spread arguments are not in the zipp-vm subset yet".to_string())
            })
            .collect::<R<Vec<_>>>()?;
        let base = self.eval_contiguous(&exprs)?;
        Ok((base, exprs.len() as u16))
    }

    /// Evaluate the arguments for a fused, statically-named `CallMethod`.
    ///
    /// For exactly one bare identifier that resolves to an ordinary local, the
    /// local itself is already a valid one-register argument window.  Reusing
    /// it removes the otherwise unconditional `Move local -> arg_temp` from hot
    /// calls such as `src.charCodeAt(i)` and `items.push(value)`.
    ///
    /// The exclusions are part of the correctness proof:
    ///
    /// * fixed and rest parameters are refused because sloppy mapped
    ///   `arguments` can mutate their registers from a property getter/proxy;
    /// * captured and direct-eval-visible bindings resolve as `LocalCell`, not
    ///   `Local`, and therefore never enter this branch;
    /// * receiver/result overlap is refused, keeping the argument window
    ///   invariant across lookup, call setup, and the legacy active
    ///   `Function#arguments` view.
    ///
    /// The caller has already required `arg_order_transparent`, so `with`, TDZ,
    /// and dynamic-name cases cannot reach this helper's direct branch.
    fn eval_named_method_args(&mut self, args: &[Arg], obj: Reg, dst: Reg) -> R<(Reg, u16)> {
        if let [Arg::Expr(e @ Expr::Ident(name))] = args {
            if let Binding::Local(reg) = self.resolve(name) {
                let parameter_top = self.param_names.len() as Reg;
                if reg > parameter_top && reg != obj && reg != dst {
                    // Run the ordinary identifier compiler for its strict-name
                    // and other validation, but target the value's existing
                    // register so no scratch slot or bytecode Move is created.
                    let actual = self.expr_into(e, reg)?;
                    debug_assert_eq!(actual, reg);
                    return Ok((reg, 1));
                }
            }
        }
        self.eval_args_contiguous(args)
    }

    /// Build a call-argument list containing `...spread` into a fresh array and
    /// return its (live) register. Each plain arg is pushed as one element; each
    /// `...x` arg appends every element of `x` (an array, or a string's chars).
    /// Consumed by `CallSpread` / `CallMethodSpread`.
    pub(crate) fn build_spread_args(&mut self, args: &[Arg]) -> R<Reg> {
        let args_arr = self.temp();
        self.emit(Instr::NewArray {
            dst: args_arr,
            arg_base: self.next_reg,
            argc: 0,
        });
        for a in args {
            let save = self.next_reg;
            // `Arg` has exactly the two forms, so the old
            // "unsupported spread-call argument" arm (oxc's `Argument` could be
            // neither) is gone rather than kept as dead code.
            match a {
                Arg::Spread(s) => {
                    let v = self.expr(s)?;
                    self.emit(Instr::ArrayAppend {
                        arr: args_arr,
                        val: v,
                        spread: true,
                    });
                }
                Arg::Expr(e) => {
                    let v = self.expr(e)?;
                    self.emit(Instr::ArrayAppend {
                        arr: args_arr,
                        val: v,
                        spread: false,
                    });
                }
            }
            self.next_reg = save;
        }
        Ok(args_arr)
    }

    /// Evaluate `exprs` into the contiguous register block `[base, base+len)`,
    /// reclaiming each expression's scratch temps. Returns `base`.
    pub(crate) fn eval_contiguous(&mut self, exprs: &[&Expr]) -> R<Reg> {
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
