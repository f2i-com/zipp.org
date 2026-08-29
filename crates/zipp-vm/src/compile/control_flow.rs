// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

// NOTE: `ast::` is `crate::parse::ast`, brought in through `use super::*` from
// `compile/mod.rs` (which replaces its `use oxc_ast::ast as ox;` with
// `use crate::parse::ast;`). Nothing in this file references `ox` any more.
//
// NOTE (signatures): the statement nodes this file used to take —
// `ox::IfStatement`, `WhileStatement`, `DoWhileStatement`, `ForStatement`,
// `ForOfStatement`, `ForInStatement`, `SwitchStatement`, `TryStatement` — have
// no struct in the zipp AST; their payload lives in the `ast::Stmt` variant. So
// each entry point now takes the VARIANT'S FIELDS, in declaration order, and
// `decls.rs`'s `stmt` destructures at the call site:
//   S::If { test, cons, alt }            -> if_stmt(test, cons, alt.as_deref())
//   S::While { test, body }              -> while_stmt(test, body)
//   S::DoWhile { body, test }            -> do_while_statement(body, test)
//   S::For { init, test, update, body }  -> for_stmt(init.as_ref(), test.as_ref(),
//                                                    update.as_ref(), body)
//   S::ForIn { left, right, body }       -> for_in_statement(left, right, body)
//   S::ForOf { left, right, body, is_await }
//                                        -> for_of_statement(left, right, body, *is_await)
//   S::Switch { disc, cases }            -> switch_stmt(disc, cases)
//   S::Try { block, handler, finalizer } -> try_statement(block, handler.as_ref(),
//                                                         finalizer.as_deref())
//
// NOTE (Target::Call): this file never matches on `ast::Target` variants — a
// for-in/for-of assignment head is handed straight to `assign_target`, which
// owns the Annex B `f() = 1` arm. Nothing to add here.

impl<'a> FnCompiler<'a> {
    /// Compile an `if`/`else` BRANCH. Annex B B.3.3: a bare FunctionDeclaration
    /// used directly as a branch (not inside a `{ }` block) is block-scoped to
    /// that branch. Declare it block-local first — using the SAME guard as the
    /// BlockStatement hoisting pre-pass — so `func_decl` binds the local instead
    /// of overwriting an enclosing parameter / lexical of the same name. The
    /// Annex B function-scoped `var` assignment (b33_names / s0reg in func_decl)
    /// still runs for the non-conflicting case.
    pub(crate) fn branch_stmt(&mut self, s: &ast::Stmt) -> R<()> {
        if let ast::Stmt::FnDecl(f) = s {
            if let Some(id) = &f.name {
                let nm = &**id;
                self.push_scope();
                // Block-local when the name must stay block-scoped: strict mode, an
                // enclosing-block lexical conflict, a protected param/lexical/class
                // (skip), or a B.3.3 var name (block-local + func-scope sync). NOT
                // for a same-named existing function (it is directly updated).
                if self.cx.in_strict
                    || self.block_fn_conflicts(nm)
                    || self.protect_names.contains(nm)
                    || self.b33_names.contains(nm)
                {
                    self.declare_local(nm);
                }
                let r = self.stmt(s);
                self.pop_scope();
                return r;
            }
        }
        self.stmt(s)
    }

    pub(crate) fn if_stmt(
        &mut self,
        test: &ast::Expr,
        cons: &ast::Stmt,
        alt: Option<&ast::Stmt>,
    ) -> R<()> {
        // The statement's completion V starts as undefined (a not-taken / empty
        // branch yields undefined, not the prior statement's value). No-op outside
        // eval mode.
        self.reset_loop_completion();
        let jf = self.emit_test_jump(test)?; // patched
        self.branch_stmt(cons)?;
        if let Some(alt) = alt {
            let jmp = self.here();
            self.emit(Instr::Jump { target: 0 }); // patched
            let else_start = self.here();
            self.patch_jump(jf, else_start);
            self.branch_stmt(alt)?;
            let end = self.here();
            self.patch_jump(jmp, end);
        } else {
            let end = self.here();
            self.patch_jump(jf, end);
        }
        Ok(())
    }

    /// In eval mode, a loop's completion value starts as `undefined` (spec: the
    /// loop's V is initialized to undefined, then updated by each non-empty body
    /// completion). Emitting this once before the loop makes
    /// `eval('1; do { break; } while(false)')` undefined rather than 1, while
    /// `eval('do { 3 } while(false)')` stays 3. No-op outside eval mode.
    pub(crate) fn reset_loop_completion(&mut self) {
        if let Some(cr) = self.completion_reg {
            self.emit(Instr::LoadUndefined { dst: cr });
        }
    }

    pub(crate) fn while_stmt(&mut self, test: &ast::Expr, body: &ast::Stmt) -> R<()> {
        let top = self.here();
        let jf = self.emit_test_jump(test)?;
        self.loop_ctx.push(LoopCtx::loop_frame(
            self.pending_label.take(),
            self.handler_depth,
        ));
        self.stmt(body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        for c in ctx.continue_jumps {
            self.patch_jump(c, top); // continue → re-test
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        Ok(())
    }

    /// Look up an in-scope local's register (innermost scope first), if any.
    pub(crate) fn local_reg(&self, name: &str) -> Option<Reg> {
        for scope in self.scopes.iter().rev() {
            for (n, r) in scope.iter().rev() {
                if n == name {
                    return Some(*r);
                }
            }
        }
        None
    }

    /// The registers of a declaration's bindings that are cell-boxed (captured by
    /// a nested closure) — the ones needing per-iteration freshening in a loop head.
    ///
    /// 14.7.4.1 perIterationBindings is `BoundNames of LexicalDeclaration`, which
    /// includes every name a DESTRUCTURING pattern introduces. Only the
    /// `Pattern::Ident` arm was walked, so `for (let [i] = [0]; i < 4; i++)
    /// fns.push(() => i)` shared a single cell and every closure observed the final
    /// value (4,4,4,4 instead of 0,1,2,3).
    pub(crate) fn captured_decl_regs(&self, d: &ast::VarDecl) -> Vec<Reg> {
        let mut regs = Vec::new();
        let mut names = Vec::new();
        for decl in &d.decls {
            names.clear();
            crate::parse::stmt::collect_pattern_names(&decl.id, &mut names);
            for id in &names {
                if let Some(reg) = self.local_reg(id) {
                    if self.cell_regs.contains(&reg) && !regs.contains(&reg) {
                        regs.push(reg);
                    }
                }
            }
        }
        regs
    }

    /// Rebind each register to a FRESH cell holding its current value (read the
    /// old cell, re-wrap). Used for per-iteration `let` loop bindings.
    pub(crate) fn emit_freshen_cells(&mut self, regs: &[Reg]) {
        for &reg in regs {
            let tmp = self.alloc_reg();
            self.emit(Instr::CellGet {
                dst: tmp,
                cell: reg,
            });
            self.emit(Instr::Move { dst: reg, src: tmp });
            self.emit(Instr::MakeCell { reg });
            self.next_reg -= 1; // free tmp
        }
    }

    pub(crate) fn for_stmt(
        &mut self,
        init: Option<&ast::ForInit>,
        test: Option<&ast::Expr>,
        update: Option<&ast::Expr>,
        body: &ast::Stmt,
    ) -> R<()> {
        self.push_scope();
        // `for (using x = r; …) body` / `for (await using …)`: the resource is
        // LOOP-scoped (disposed ONCE when the for-statement completes, normal or
        // abrupt), unlike for-of which disposes per-iteration. Open a scope + a
        // finally wrapping the whole loop; the `using` init registers x into it.
        let using_async: Option<bool> = match init {
            Some(ast::ForInit::Var(d)) => match d.kind {
                ast::VarKind::Using => Some(false),
                ast::VarKind::AwaitUsing => Some(true),
                _ => None,
            },
            _ => None,
        };
        let using_ctx = if let Some(is_async) = using_async {
            let sreg = self.declare_local("<for.uscope>");
            let kreg = self.declare_local("<for.ukind>");
            let vreg = self.declare_local("<for.uval>");
            self.emit(Instr::OpenUsingScope { dst: sreg });
            let push_at = self.here();
            self.emit(Instr::PushFinally {
                target: 0,
                kind_reg: kreg,
                val_reg: vreg,
            });
            self.handler_depth += 1;
            Some((is_async, sreg, kreg, vreg, push_at))
        } else {
            None
        };
        // init
        if let Some(init) = init {
            match init {
                ast::ForInit::Var(d) => {
                    // While compiling the `using` init, route its RegisterDisposable
                    // to the loop scope (restored after, so a using-block in the body
                    // uses its own scope).
                    let prev = using_ctx.map(|(_, s, _, _, _)| self.using_scope_reg.replace(s));
                    self.var_decl(d)?;
                    if let Some(p) = prev {
                        self.using_scope_reg = p;
                    }
                }
                // NOTE: `ForInit` has exactly two variants, so the old
                // `as_expression().ok_or("unsupported for-init")?` guard is gone —
                // there is no third shape left to reject. Unreachable before, so no
                // bytecode changes.
                ast::ForInit::Expr(e) => {
                    self.expr(e)?;
                }
            }
        }
        // Per-iteration bindings: a `for (let i …)` loop variable captured by a
        // closure in the body gets a FRESH binding each iteration (JS semantics:
        // `for(let i…) fns.push(()=>i)` yields 0,1,2 not 3,3,3). Only when the var
        // is actually boxed (captured) — otherwise the plain-register fast path
        // (and the hot-loop JIT) is preserved. `var` is function-scoped → no
        // freshening.
        let fresh_regs: Vec<Reg> = match init {
            Some(ast::ForInit::Var(d)) if d.kind != ast::VarKind::Var => self.captured_decl_regs(d),
            _ => Vec::new(),
        };
        // CreatePerIterationEnvironment runs once BEFORE the first test (13.7.4.8
        // step 2): a closure made in the INIT keeps the loop-head binding, which
        // the test/body/update (operating on the first iteration's copy) never
        // mutate.
        self.emit_freshen_cells(&fresh_regs);
        let top = self.here();
        let jf = match test {
            Some(t) => Some(self.emit_test_jump(t)?),
            None => None,
        };
        self.loop_ctx.push(LoopCtx::loop_frame(
            self.pending_label.take(),
            self.handler_depth,
        ));
        self.stmt(body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → freshen, run the update, re-test
        }
        // End of iteration: copy each captured loop var into a fresh cell, so the
        // body's closures (which captured the OLD cell) keep this iteration's
        // value and the update mutates the new binding.
        self.emit_freshen_cells(&fresh_regs);
        if let Some(update) = update {
            // A `for` head's update expression is evaluated for effect only —
            // even in an eval program, where the completion value comes from the
            // BODY. See `expr_discarded`.
            self.expr_discarded(update)?;
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        if let Some(j) = jf {
            self.patch_jump(j, end);
        }
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        // Loop-scoped `using` disposal: the normal exit (test false) and `break`
        // land at `end` and run DisposeScope once; a throw/return out of the body
        // routes through the finally handler. (break/continue are plain jumps —
        // loop_ctx's floor is the handler depth INCLUDING this finally.)
        if let Some((is_async, sreg, kreg, vreg, push_at)) = using_ctx {
            self.emit_leave_finally_normal(kreg);
            self.handler_depth -= 1;
            let jto = self.here();
            self.emit(Instr::Jump { target: 0 });
            let fin = self.here();
            if let Instr::PushFinally { target, .. } = &mut self.code[push_at as usize] {
                *target = fin;
            }
            self.patch_jump(jto, fin);
            if is_async {
                self.emit_async_dispose_loop(sreg, kreg, vreg);
            } else {
                self.emit(Instr::DisposeScope {
                    scope: sreg,
                    kind_reg: kreg,
                    val_reg: vreg,
                });
            }
            self.emit(Instr::EndFinally {
                kind_reg: kreg,
                val_reg: vreg,
            });
        }
        self.pop_scope();
        Ok(())
    }

    /// `try { … } catch (e) { … } finally { … }`.
    ///
    /// Without a `finally`: `PushHandler(catch, e_reg)` ; try-body ; `PopHandler` ;
    /// jump past catch. The catch lands the thrown value in `e_reg` and runs.
    ///
    /// With a `finally`: a `PushFinally` wraps the whole construct. Every exit from
    /// the try/catch — normal completion, `return`, a throw (caught here or
    /// propagating), or a `break`/`continue` that leaves the construct — routes
    /// through the single finally block, which `EndFinally` closes by resuming the
    /// recorded completion (fall through / re-return / re-throw / resume-jump),
    /// chaining through any outer finally. `break`/`continue` exit via `JumpFinally`
    /// (emitted by `emit_loop_jump` when the target's handler depth is below the
    /// current one).
    pub(crate) fn try_statement(
        &mut self,
        block: &[ast::Stmt],
        handler: Option<&ast::CatchClause>,
        finalizer: Option<&[ast::Stmt]>,
    ) -> R<()> {
        match finalizer {
            Some(finalizer) => self.try_with_finally(block, handler, finalizer),
            None => self.try_catch_only(block, handler),
        }
    }

    /// `try { … } catch (e) { … }` (no finalizer).
    pub(crate) fn try_catch_only(
        &mut self,
        block: &[ast::Stmt],
        handler: Option<&ast::CatchClause>,
    ) -> R<()> {
        // The statement's completion V starts at undefined (an empty try/catch
        // yields undefined, not the prior statement's value).
        self.reset_loop_completion();
        let push_at = self.here();
        // catch_reg/target patched once known.
        self.emit(Instr::PushHandler {
            catch_target: 0,
            catch_reg: 0,
        });

        // The catch handler is active throughout the try body (a `break`/`continue`
        // exiting it must pop the stale handler — see `emit_loop_jump`).
        self.handler_depth += 1;
        self.push_scope();
        self.predeclare_lexical_tdz(block);
        self.compile_stmt_list(block, false)?;
        self.pop_scope();
        // After the try body the catch handler is no longer active (the catch body
        // runs with it already popped by the unwind).
        self.handler_depth -= 1;

        let handler = handler.ok_or("try requires catch or finally")?;
        // Normal completion of the try: pop the handler, skip the catch.
        self.emit(Instr::PopHandler);
        let skip = self.here();
        self.emit(Instr::Jump { target: 0 });

        let catch_start = self.here();
        self.compile_catch_body(handler, push_at)?;

        let after = self.here();
        self.patch_jump(skip, after);
        let _ = catch_start;
        Ok(())
    }

    /// BlockDeclarationInstantiation's TDZ half for a block-like statement list:
    /// pre-create TDZ cells for the list's CAPTURED simple-identifier lexical
    /// (`let`/`const`/`class`) declarations in the CURRENT scope, so a closure
    /// materialized before the textual declaration captures the block's binding
    /// (in its TDZ) instead of resolving to an outer/global one. The textual
    /// declaration reuses the register, ending the TDZ (see the lexical arm of
    /// `var_decl`). Non-captured names keep plain registers (no runtime cost).
    /// Shared by plain blocks, switch case-blocks, and try/catch/finally bodies
    /// (a switch's CaseBlock calls it once per clause — the in-scope dedup makes
    /// the clauses share one block-level binding).
    pub(crate) fn predeclare_lexical_tdz(&mut self, stmts: &[ast::Stmt]) {
        // Every block-like scope funnels through here right after `push_scope`,
        // so this is also where the scope's lexical NAMES are recorded for the
        // Annex B B.3.3 blocker test — which must see a `let` written below a
        // nested block, not just the bindings compiled so far.
        self.note_block_lexicals(stmts);
        for st in stmts {
            let pre = |fc: &mut Self, name: &str| {
                // The immutable global value names must also exist as TDZ
                // bindings from block entry when shadowed by a local lexical.
                // Otherwise a read before `let undefined`/`NaN`/`Infinity`
                // incorrectly takes the compiler's constant fallback.  These
                // declarations are rare, so giving just them the cell-backed
                // TDZ path has no cost for ordinary blocks.
                if (fc.captured.contains(name) || matches!(name, "undefined" | "NaN" | "Infinity"))
                    && !fc.scopes.last().unwrap().iter().any(|(n, _)| n == name)
                {
                    let r = fc.alloc_reg();
                    fc.scopes.last_mut().unwrap().push((name.to_string(), r));
                    fc.emit(Instr::MakeCellTdz { reg: r });
                    fc.cell_regs.insert(r);
                    fc.block_tdz_cells.insert(r);
                }
            };
            match st {
                ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                    for decl in &d.decls {
                        if let ast::Pattern::Ident(id) = &decl.id {
                            pre(self, id);
                        }
                    }
                }
                ast::Stmt::ClassDecl(c) => {
                    if let Some(id) = &c.name {
                        pre(self, id);
                    }
                }
                _ => {}
            }
        }
    }

    /// True iff `body` declares a top-level `using`/`await using` resource. Only
    /// such blocks/bodies get the disposal scaffolding — every other block keeps
    /// its byte-for-byte-identical fast path (the zero-regression gate).
    pub(crate) fn block_has_using(body: &[ast::Stmt]) -> bool {
        body.iter().any(|st| {
            matches!(st,
                ast::Stmt::VarDecl(d)
                    if matches!(d.kind, ast::VarKind::Using | ast::VarKind::AwaitUsing))
        })
    }

    /// True iff `body` declares a top-level `await using` — such a scope disposes
    /// ASYNCHRONOUSLY (each dispose result is awaited), so its finally epilogue is
    /// the awaited disposal loop rather than the single sync `DisposeScope` op.
    pub(crate) fn block_has_await_using(body: &[ast::Stmt]) -> bool {
        body.iter()
            .any(|st| matches!(st, ast::Stmt::VarDecl(d) if d.kind == ast::VarKind::AwaitUsing))
    }

    /// Emit the async-disposal epilogue (the finally body for an `await using`
    /// scope): a loop that pops each disposer LIFO, calls it, and AWAITs the result,
    /// catching a sync throw or an awaited rejection and merging it into the
    /// completion (`kind_reg`/`val_reg`) as a SuppressedError chain. An inert
    /// (null-initializer) record still performs one `Await(undefined)`, so an
    /// evaluated `await using x = null` yields a microtask tick; a scope that was
    /// opened but registered nothing (a `break` before the declaration) runs the
    /// loop zero times and awaits nothing.
    pub(crate) fn emit_async_dispose_loop(&mut self, scope_reg: Reg, kind_reg: Reg, val_reg: Reg) {
        let save = self.next_reg;
        let res = self.alloc_reg();
        let done = self.alloc_reg();
        let exc = self.alloc_reg();

        let loop_top = self.here();
        let push_at = self.here();
        self.emit(Instr::PushHandler {
            catch_target: 0,
            catch_reg: exc,
        });
        self.emit(Instr::AsyncDisposeNext {
            scope: scope_reg,
            res,
            done,
        });
        let jdone = self.here();
        self.emit(Instr::JumpIfTrue {
            cond: done,
            target: 0,
        });
        self.emit(Instr::Await { dst: res, val: res });
        self.emit(Instr::PopHandler);
        self.emit(Instr::Jump { target: loop_top });

        let done_path = self.here();
        self.emit(Instr::PopHandler);
        let jafter = self.here();
        self.emit(Instr::Jump { target: 0 });

        let cat = self.here();
        self.emit(Instr::MergeDispose {
            kind_reg,
            val_reg,
            err: exc,
        });
        self.emit(Instr::Jump { target: loop_top });

        let after = self.here();
        if let Instr::PushHandler { catch_target, .. } = &mut self.code[push_at as usize] {
            *catch_target = cat;
        }
        self.patch_jump(jdone, done_path);
        self.patch_jump(jafter, after);
        self.next_reg = save; // reclaim res/done/exc
    }

    /// Compile a statement list that contains `using` declarations: desugar it onto
    /// the existing `PushFinally`/`EndFinally` machinery so the registered resources
    /// are disposed (LIFO, SuppressedError-chained) on EVERY exit — normal, throw,
    /// break, continue, return. A fresh runtime resource-scope id lives in
    /// `scope_reg` and becomes `using_scope_reg` for the body, so each `using`'s
    /// `RegisterDisposable` pushes onto it. (Sync `using` only this iteration;
    /// `await using` still binds like `let` but is not yet disposed.)
    pub(crate) fn compile_using_block(&mut self, body: &[ast::Stmt], skip_fn_decls: bool) -> R<()> {
        // NB: unlike `try_with_finally`, do NOT reset the completion register — a
        // `using` block's own completion is empty (UpdateEmpty), so it must
        // PRESERVE the prior eval completion (`4; {using x=null;}` ⇒ 4). The
        // DisposeScope/EndFinally epilogue never writes the completion register.
        let scope_reg = self.alloc_reg();
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();
        self.emit(Instr::OpenUsingScope { dst: scope_reg });

        let fin_push = self.here();
        self.emit(Instr::PushFinally {
            target: 0,
            kind_reg,
            val_reg,
        });
        self.handler_depth += 1;

        let prev_scope = self.using_scope_reg.replace(scope_reg);
        for st in body {
            // Function-body callers materialise top-level function declarations at
            // entry and skip them here (mirrors the non-using body loop).
            if skip_fn_decls {
                if let ast::Stmt::FnDecl(_) = st {
                    continue;
                }
            }
            self.stmt(st)?;
        }
        self.using_scope_reg = prev_scope;

        self.emit_leave_finally_normal(kind_reg);
        let normal_jump = self.here();
        self.emit(Instr::Jump { target: 0 });

        self.handler_depth -= 1;
        let fin_start = self.here();
        if let Instr::PushFinally { target, .. } = &mut self.code[fin_push as usize] {
            *target = fin_start;
        }
        self.patch_jump(normal_jump, fin_start);

        // An `await using` scope (in an async context) disposes asynchronously —
        // emit the awaited disposal loop; otherwise the single sync DisposeScope op.
        if Self::block_has_await_using(body) && self.in_async {
            self.emit_async_dispose_loop(scope_reg, kind_reg, val_reg);
        } else {
            self.emit(Instr::DisposeScope {
                scope: scope_reg,
                kind_reg,
                val_reg,
            });
        }
        self.emit(Instr::EndFinally { kind_reg, val_reg });
        self.next_reg -= 3; // reclaim scope_reg / kind_reg / val_reg
        Ok(())
    }

    /// Compile a statement list, transparently adding `using`-disposal scaffolding
    /// when the list declares a top-level `using` (else the byte-for-byte-identical
    /// plain loop). Used by the statement-list contexts that are NOT the
    /// `BlockStatement` arm — try/catch/finally bodies — so `using` inside them is
    /// disposed at the end of that block, like any other block.
    pub(crate) fn compile_stmt_list(&mut self, body: &[ast::Stmt], skip_fn_decls: bool) -> R<()> {
        if Self::block_has_using(body) {
            self.compile_using_block(body, skip_fn_decls)
        } else {
            for s in body {
                if skip_fn_decls {
                    if let ast::Stmt::FnDecl(_) = s {
                        continue;
                    }
                }
                self.stmt(s)?;
            }
            Ok(())
        }
    }

    /// `try … finally { F }` (with or without a catch). The finally runs on every
    /// exit path via a `PushFinally` handler + `EndFinally` epilogue.
    pub(crate) fn try_with_finally(
        &mut self,
        block: &[ast::Stmt],
        handler: Option<&ast::CatchClause>,
        finalizer: &[ast::Stmt],
    ) -> R<()> {
        // The statement's completion V starts at undefined (an empty try/finally
        // yields undefined). The try/catch body value is then preserved through a
        // normally-completing finally (the finally body's own value is not reset
        // here, matching "if F is normal, set F to B").
        self.reset_loop_completion();
        // Two persistent registers carry the completion record (kind + value)
        // from each exit path into the shared finally block. Allocated for the
        // whole construct; reclaimed after `EndFinally`.
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();

        let fin_push = self.here();
        self.emit(Instr::PushFinally {
            target: 0,
            kind_reg,
            val_reg,
        });
        // The finally handler is active for the whole try/catch (a `break`/
        // `continue` exiting it must route through the finally — see
        // `emit_loop_jump`).
        self.handler_depth += 1;

        let has_catch = handler.is_some();
        let catch_push = if has_catch {
            let at = self.here();
            self.emit(Instr::PushHandler {
                catch_target: 0,
                catch_reg: 0,
            });
            self.handler_depth += 1; // catch handler active during the try body
            Some(at)
        } else {
            None
        };

        // Try body.
        self.push_scope();
        self.predeclare_lexical_tdz(block);
        self.compile_stmt_list(block, false)?;
        self.pop_scope();

        // Normal-completion jumps (from the try body and, if present, the catch
        // body) land at the finally entry below.
        let mut normal_jumps: Vec<u32> = Vec::new();
        if has_catch {
            self.emit(Instr::PopHandler);
            self.handler_depth -= 1; // catch popped; finally still active for catch body
        }
        self.emit_leave_finally_normal(kind_reg);
        normal_jumps.push(self.here());
        self.emit(Instr::Jump { target: 0 });

        // Catch body (entered by the unwind, which already popped the Catch
        // handler; the Finally handler is still active for throws inside it).
        if let (Some(catch_push), Some(handler)) = (catch_push, handler) {
            self.compile_catch_body(handler, catch_push)?;
            self.emit_leave_finally_normal(kind_reg);
            normal_jumps.push(self.here());
            self.emit(Instr::Jump { target: 0 });
        }

        // The finally handler is popped before its own body runs (a `break`/
        // `return` inside the finally routes to OUTER handlers).
        self.handler_depth -= 1;

        // Finally entry.
        let fin_start = self.here();
        if let Instr::PushFinally { target, .. } = &mut self.code[fin_push as usize] {
            *target = fin_start;
        }
        for j in normal_jumps {
            self.patch_jump(j, fin_start);
        }

        // Finally body, then resume whatever completion brought us here. In eval
        // mode a NORMALLY-completing `finally` discards its own completion value —
        // the try/catch block's value is the result (`try{39}finally{1}` ⇒ 39) — so
        // save the accumulated completion across the finally body and restore it.
        let saved_cmpl = self.completion_reg.map(|cr| {
            let r = self.alloc_reg();
            self.emit(Instr::Move { dst: r, src: cr });
            // The finally body's own completion V starts EMPTY: an abrupt exit
            // (break/continue) from inside it carries only the values its own
            // statements produced — `finally { break }` yields undefined while
            // `finally { 42; break }` yields 42 — never the try block's value.
            // (The normal fall-through path below restores the saved value.)
            self.emit(Instr::LoadUndefined { dst: cr });
            r
        });
        self.push_scope();
        self.predeclare_lexical_tdz(finalizer);
        for s in finalizer {
            self.stmt(s)?;
        }
        self.pop_scope();
        if let (Some(cr), Some(r)) = (self.completion_reg, saved_cmpl) {
            self.emit(Instr::Move { dst: cr, src: r });
            self.next_reg -= 1; // reclaim the saved-completion temp
        }
        self.emit(Instr::EndFinally { kind_reg, val_reg });

        self.next_reg -= 2; // reclaim kind_reg / val_reg
        Ok(())
    }

    /// Emit the normal-completion prologue to a finally: record kind 0 (normal)
    /// and pop the still-active `Finally` handler (abnormal paths pop it via the
    /// unwind / Return op instead).
    pub(crate) fn emit_leave_finally_normal(&mut self, kind_reg: Reg) {
        self.emit(Instr::LoadInt {
            dst: kind_reg,
            val: 0,
        });
        self.emit(Instr::PopFinally);
    }

    /// Compile a `catch (e) { … }` body and patch its `PushHandler` (at `push_at`)
    /// to land here with the thrown value in the catch register.
    pub(crate) fn compile_catch_body(&mut self, handler: &ast::CatchClause, push_at: u32) -> R<()> {
        // The VM deposits the thrown value into the catch register directly, so
        // reserve it WITHOUT auto-boxing; box it after if a nested closure
        // captures the binding (the value is present by then).
        let catch_start = self.here();
        // The try block's completion value dies with it: `try Block Catch` returns
        // UpdateEmpty(CatchClauseEvaluation, undefined) when B is a throw
        // completion — B.[[Value]] is the exception, never the block's V. Only the
        // unwind reaches here, so resetting V at catch entry discards exactly the
        // abandoned try-block value (`eval("try{'t';throw 0}catch(e){}")` ⇒
        // undefined) while a normally-completing try keeps its own ('t').
        self.reset_loop_completion();
        self.push_scope();
        // The VM deposits the thrown value into `e_reg`. For `catch (id)` that IS
        // the binding; for `catch ([a,b])` / `catch ({e})` it's a scratch slot we
        // destructure into the pattern's bindings.
        let (e_reg, e_name, pattern) = match &handler.param {
            Some(p) => match p {
                ast::Pattern::Ident(id) => {
                    // Strict mode: `catch (eval)` / `catch (arguments)` is an early SyntaxError.
                    strict_name_err(self.cx.in_strict, id)?;
                    let r = self.declare_local_no_box(id);
                    // Mark the catch-PARAMETER binding: Annex B B.3.5 lets a
                    // same-named `var` / block function coexist with it (NOT an
                    // early error), so the B.3.3 promotion check must ignore it.
                    self.catch_param_regs.insert(r);
                    (r, Some(id.to_string()), None)
                }
                pat => (self.declare_local_no_box("<catch.val>"), None, Some(pat)),
            },
            None => (self.declare_local_no_box("<catch.ignored>"), None, None),
        };
        if let Instr::PushHandler {
            catch_target,
            catch_reg,
        } = &mut self.code[push_at as usize]
        {
            *catch_target = catch_start;
            *catch_reg = e_reg;
        }
        if let Some(pat) = pattern {
            // Catch-parameter leaves are CATCH-SCOPE locals (never script
            // globals): a destructured name must not leak past the catch nor
            // hoist a same-named block function to a global (B.3.5 skip).
            self.pattern_block_local = true;
            let r = self
                .declare_pattern(pat, false)
                .and_then(|_| self.extract_pattern(pat, e_reg));
            self.pattern_block_local = false;
            r?;
        }
        if let Some(n) = &e_name {
            // Same boxing rule as `declare_local`, just deferred until the value
            // is present: a nested closure captures it, or the function may
            // direct-eval (the eval site map is built from `cell_regs`, so an
            // unboxed catch param was invisible to `eval("e")` inside the catch).
            if self.captured.contains(n) || self.box_all_locals || self.script_eval_lexicals {
                self.emit(Instr::MakeCell { reg: e_reg });
                self.cell_regs.insert(e_reg);
            }
        }
        // The catch BLOCK's captured lexicals get block-entry TDZ cells — after
        // the parameter above, so a closure in a destructuring default captures
        // the OUTER binding while closures in the block capture the block's.
        self.predeclare_lexical_tdz(&handler.body);
        self.compile_stmt_list(&handler.body, false)?;
        self.pop_scope();
        Ok(())
    }

    pub(crate) fn do_while_statement(&mut self, body: &ast::Stmt, test: &ast::Expr) -> R<()> {
        let top = self.here();
        self.loop_ctx.push(LoopCtx::loop_frame(
            self.pending_label.take(),
            self.handler_depth,
        ));
        self.stmt(body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → re-evaluate the condition
        }
        let cond = self.expr(test)?;
        // Loop back to top while the condition is truthy.
        self.emit(Instr::JumpIfTrue { cond, target: top });
        let end = self.here();
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        Ok(())
    }

    /// `switch (disc) { case A: …; default: …; … }`. Two passes: emit the
    /// `disc === testᵢ` comparison jumps in order (and remember `default`), then
    /// emit the case bodies consecutively so fall-through is natural; `break`
    /// (collected in a non-loop frame) jumps to the end.
    pub(crate) fn switch_stmt(&mut self, disc: &ast::Expr, cases: &[ast::SwitchCase]) -> R<()> {
        // CaseBlockEvaluation starts the completion V at undefined (a no-match /
        // all-empty switch yields undefined, not the prior statement's value).
        self.reset_loop_completion();
        // The discriminant evaluates in the OUTER environment — before the
        // CaseBlock's block scope opens (NewDeclarativeEnvironment happens after
        // GetValue(exprRef)), so a closure in the discriminant captures outer
        // bindings, not the case-block's.
        let disc = self.expr(disc)?;
        self.push_scope();
        // A switch CaseBlock is one block scope. Pre-declare its lexically-scoped
        // declarations as block-local so they don't leak past the switch:
        //  - class / generator / async(-generator) function declarations are ALWAYS
        //    lexical (no Annex B), so block-local in both strict and sloppy;
        //  - an ordinary function declaration is block-local only in strict (sloppy
        //    keeps the Annex B hoist to the function/global scope).
        for c in cases {
            for st in &c.body {
                match st {
                    ast::Stmt::FnDecl(f) => {
                        if let Some(id) = &f.name {
                            // Block-local in strict / generator / async (always
                            // lexical), on a lexical conflict, or for a protected
                            // param/lexical/class or a B.3.3 var name — so Annex B
                            // param/lexical skip-leak applies in case bodies too. A
                            // same-named existing function is directly updated, not
                            // shadowed.
                            let nm = &**id;
                            if self.cx.in_strict
                                || f.is_generator
                                || f.is_async
                                || self.block_fn_conflicts(nm)
                                || self.protect_names.contains(nm)
                                || self.b33_names.contains(nm)
                            {
                                self.declare_local(nm);
                            }
                        }
                    }
                    ast::Stmt::ClassDecl(cd) => {
                        if let Some(id) = &cd.name {
                            self.declare_local(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        // BlockDeclarationInstantiation's TDZ half: captured `let`/`const` in any
        // clause get block-entry TDZ cells, so a closure in a case selector or an
        // earlier clause captures the CaseBlock's binding, not an outer one.
        for c in cases {
            self.predeclare_lexical_tdz(&c.body);
        }

        // Pass 1: comparison jumps (strict `===`, like JS). `default` is recorded
        // and dispatched after the others fail.
        let mut case_jumps: Vec<(usize, u32)> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (i, c) in cases.iter().enumerate() {
            match &c.test {
                Some(t) => {
                    let save = self.next_reg;
                    let tv = self.expr(t)?;
                    let cond = self.temp();
                    self.emit(Instr::Eq {
                        dst: cond,
                        a: disc,
                        b: tv,
                    });
                    let j = self.here();
                    self.emit(Instr::JumpIfTrue { cond, target: 0 });
                    self.next_reg = save; // reclaim test/cond temps (disc survives)
                    case_jumps.push((i, j));
                }
                None => default_index = Some(i),
            }
        }
        // No case matched → default body (if present) else the end.
        let dispatch_default = self.here();
        self.emit(Instr::Jump { target: 0 });

        // Pass 2: case bodies, in source order (fall-through is natural).
        self.loop_ctx.push(LoopCtx::switch_frame(
            self.pending_label.take(),
            self.handler_depth,
        ));
        let mut body_start: Vec<u32> = Vec::with_capacity(cases.len());
        for c in cases {
            body_start.push(self.here());
            for st in &c.body {
                self.stmt(st)?;
            }
        }
        let ctx = self.loop_ctx.pop().unwrap();
        let end = self.here();

        for (i, j) in case_jumps {
            self.patch_jump(j, body_start[i]);
        }
        let default_target = default_index.map(|i| body_start[i]).unwrap_or(end);
        self.patch_jump(dispatch_default, default_target);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        self.pop_scope();
        Ok(())
    }

    /// `for (const x of iter) body` — desugars to an index loop over an
    /// array/string: `let i=0; while (i < len(iter)) { x = iter[i]; body; i++ }`.
    /// (Generic iterables/iterators are not in the subset; arrays and strings
    /// cover the corpus and common code.)
    pub(crate) fn for_of_statement(
        &mut self,
        left: &ast::ForTarget,
        right: &ast::Expr,
        body: &ast::Stmt,
        is_await: bool,
    ) -> R<()> {
        if is_await && !self.in_async {
            return Err("`for await` is only valid in an async function".into());
        }
        self.push_scope();

        // A `for (let [a,b] of …)` / `for (let {x} of …)` head destructures each
        // element; a plain `for (let x of …)` binds it to one variable. A head
        // that's NOT a declaration (`for (x of …)`, `for ([a,b] of …)`,
        // `for (obj.k of …)`) ASSIGNS each element to an existing target.
        let decl_pat = match left {
            ast::ForTarget::Var(d) => Some(&d.decls[0].id),
            _ => None,
        };
        let pattern = decl_pat.filter(|p| !matches!(p, ast::Pattern::Ident(_)));
        let assign_tgt = match left {
            ast::ForTarget::Target(t) => Some(t),
            _ => None,
        };

        // Evaluate the iterable into a stable scratch local; `idx` is the cursor
        // IterNext advances for array/string/Map/Set (ignored for a generator,
        // which it drives via `.next()`). One loop shape handles every iterable.
        let iter_reg = self.declare_local("<forof.iter>");
        // The iterable expression is evaluated with the loop's `let`/`const` binding(s)
        // already in scope but in their TDZ — so `for (let x of [x]) {}` throws a
        // ReferenceError (the inner `x` shadows, uninitialized), per
        // ForIn/OfHeadEvaluation. Mark the names as TDZ for the duration of `right`.
        let tdz_added: Vec<String> = match left {
            ast::ForTarget::Var(d) if d.kind.is_lexical() => {
                let mut names = std::collections::HashSet::new();
                capture::collect_pattern_names(&d.decls[0].id, &mut names);
                names
                    .into_iter()
                    .filter(|n| self.param_tdz.insert(n.clone()))
                    .collect()
            }
            _ => Vec::new(),
        };
        // A RUNTIME TDZ scope over the head expression too: a CLOSURE created
        // in `right` captures an uninitialized cell for each head name and
        // throws ReferenceError when it later reads it (the head env binding
        // is never initialized), instead of capturing the outer binding.
        let head_tdz_scope = !tdz_added.is_empty();
        if head_tdz_scope {
            self.push_scope();
            for n in &tdz_added {
                let r = self.alloc_reg();
                self.scopes.last_mut().unwrap().push((n.clone(), r));
                self.emit(Instr::MakeCellTdz { reg: r });
                self.cell_regs.insert(r);
            }
        }
        let v = self.expr_into(right, iter_reg)?;
        if head_tdz_scope {
            self.pop_scope();
        }
        for n in &tdz_added {
            self.param_tdz.remove(n);
        }
        if v != iter_reg {
            self.emit(Instr::Move {
                dst: iter_reg,
                src: v,
            });
        }
        // Resolve the iterator. `for await` uses the ASYNC iterator (@@asyncIterator
        // → @@iterator fallback); plain `for of` uses @@iterator. Built-ins/async
        // generators pass through and are driven by IterNext / ForAwaitNext.
        let sync_reg = if is_await {
            // The sync flag must survive the whole loop (read each iteration
            // for the AsyncFromSyncIteratorContinuation value-await below).
            let s = self.declare_local("<forof.sync>");
            self.emit(Instr::GetAsyncIterator {
                dst: iter_reg,
                src: iter_reg,
                sync_dst: s,
            });
            Some(s)
        } else {
            self.emit(Instr::GetIterator {
                dst: iter_reg,
                src: iter_reg,
            });
            None
        };
        // GetIterator's iterator RECORD caches the `next` method ONCE in the
        // prologue (a mid-loop redefinition of `iterator.next` is not observed
        // — iterator-next-reference). Sync loops only; user-object iterators
        // only (built-ins keep the registerless cursor fast path, with no
        // observable prologue get).
        let next_reg = if is_await {
            None
        } else {
            let n = self.declare_local("<forof.next>");
            self.emit(Instr::IterPrime {
                dst: n,
                iter: iter_reg,
            });
            Some(n)
        };
        let idx_reg = self.declare_local("<forof.idx>");
        self.emit(Instr::LoadInt {
            dst: idx_reg,
            val: 0,
        });
        // Close-on-abrupt-exit (sync for-of only): the body runs under a `finally`
        // whose block is `IterCloseFinally` — the ONE place every abrupt exit meets
        // the iterator, so the close runs after the body's own catch/finally
        // handlers (7.4.11 is part of the for-of statement, not of its body) and a
        // `gen.return()` resumed at a `yield` in the body reaches it too. The
        // completion record lands in (`kind_reg`, `val_reg`).
        // (for-await iterates asynchronously — left on the inline-close path.)
        let (kind_reg, exc_reg) = if is_await {
            (None, None)
        } else {
            (
                Some(self.declare_local("<forof.kind>")),
                Some(self.declare_local("<forof.cval>")),
            )
        };

        // The loop binding: a destructuring pattern's leaves, an assignment to an
        // existing target, or a single (possibly cell-boxed) declared variable.
        let head_lexical = matches!(left, ast::ForTarget::Var(d) if d.kind.is_lexical());
        let head_const = matches!(left, ast::ForTarget::Var(d) if matches!(
            d.kind,
            ast::VarKind::Const | ast::VarKind::Using | ast::VarKind::AwaitUsing
        ));
        // A `var` head whose binding is NOT a plain current-function register
        // (catch-param cell / global slot / upvalue): per-iteration writes go
        // through `store_binding` — the loop creates NO binding of its own
        // (Annex B catch-redeclared-for-of-var).
        let mut var_binding: Option<Binding> = None;
        let (var_reg, var_is_cell) = match (pattern, assign_tgt) {
            (Some(p), _) => {
                // A `let`/`const` head pattern binds HEAD-SCOPE locals (a
                // script-level `var` pattern still binds globals); a `var` head
                // pattern binds nothing — it reuses the hoisted var bindings.
                self.pattern_block_local = head_lexical;
                let r = self.declare_pattern(p, !head_lexical);
                self.pattern_block_local = false;
                r?;
                (0, false)
            }
            (None, Some(_)) => (0, false), // assignment target: nothing to declare
            (None, None) => {
                let var_name = for_left_name(left)?;
                if matches!(left, ast::ForTarget::Var(d) if d.kind == ast::VarKind::Var) {
                    // `for (var x of …)`: resolve the EXISTING binding instead
                    // of declaring a loop-local; a first-mention var creates
                    // its FUNCTION-scoped binding.
                    match self.head_var_binding(&var_name) {
                        Binding::Local(r) => (r, false),
                        other => {
                            var_binding = Some(other);
                            (0, false)
                        }
                    }
                } else {
                    let r = self.declare_local(&var_name);
                    // A `const`/`using`/`await using` loop variable is immutable
                    // WITHIN an iteration: a body assignment throws TypeError.
                    if let ast::ForTarget::Var(d) = left {
                        if matches!(
                            d.kind,
                            ast::VarKind::Const | ast::VarKind::Using | ast::VarKind::AwaitUsing
                        ) {
                            self.const_regs.insert(r);
                        }
                    }
                    (r, self.cell_regs.contains(&r))
                }
            }
        };

        // `for (using x of it)` / `for (await using x of it)`: each iteration
        // disposes the loop variable's resource at the end of the iteration (a
        // per-iteration disposal scope). Allocate the scope/completion registers
        // once (stable across iterations); OpenUsingScope writes a fresh scope id
        // each turn. Only a simple-identifier head is a using declaration.
        let using_async: Option<bool> = match left {
            ast::ForTarget::Var(d) => match d.kind {
                ast::VarKind::Using => Some(false),
                ast::VarKind::AwaitUsing => Some(true),
                _ => None,
            },
            _ => None,
        };
        let using_regs = using_async.map(|_| {
            (
                self.declare_local("<forof.uscope>"),
                self.declare_local("<forof.ukind>"),
                self.declare_local("<forof.uval>"),
            )
        });

        let top = self.here();
        let save = self.next_reg;
        let done = self.alloc_reg();
        // Write the element straight into a plain-local loop var; use a temp for a
        // destructuring pattern, an assignment target, a cell-boxed var, or a
        // store-through `var` binding (catch-param / global / upvalue).
        let elem =
            if pattern.is_some() || assign_tgt.is_some() || var_is_cell || var_binding.is_some() {
                self.alloc_reg()
            } else {
                var_reg
            };
        let jdone = if is_await {
            // `r = await <next step>; done = r.done; value = r.value`. ForAwaitNext
            // yields a Promise (async iterator) or a {value,done} (sync) — awaiting
            // suspends on the former and passes the latter straight through.
            let step = self.alloc_reg();
            self.emit(Instr::ForAwaitNext {
                dst: step,
                iter: iter_reg,
                idx: idx_reg,
            });
            // AsyncFromSyncIteratorContinuation: a SYNC source's raw
            // {value,done} becomes the spec's capability promise resolving
            // to { value: await value, done } — built synchronously inside
            // this turn (observable constructor read + the one-job unwrap
            // hop), so the single Await below covers both iterator kinds.
            if let Some(s) = sync_reg {
                let jskip = self.here();
                self.emit(Instr::JumpIfFalse { cond: s, target: 0 });
                self.emit(Instr::AsyncFromSyncStep {
                    dst: step,
                    step,
                    iter: iter_reg,
                });
                let after = self.here();
                self.patch_jump(jskip, after);
            }
            let r = self.alloc_reg();
            self.emit(Instr::Await { dst: r, val: step });
            let done_name = self.string_name("done");
            self.emit(Instr::GetProp {
                dst: done,
                obj: r,
                name: done_name,
            });
            let j = self.here();
            self.emit(Instr::JumpIfTrue {
                cond: done,
                target: 0,
            }); // done → exit
            let value_name = self.string_name("value");
            self.emit(Instr::GetProp {
                dst: elem,
                obj: r,
                name: value_name,
            });
            j
        } else {
            self.emit(Instr::IterNext {
                value_dst: elem,
                done_dst: done,
                iter: iter_reg,
                idx: idx_reg,
                next: next_reg.unwrap_or(Reg::MAX),
            });
            let j = self.here();
            self.emit(Instr::JumpIfTrue {
                cond: done,
                target: 0,
            }); // done → exit
            j
        };
        // Enter the loop body's break/continue scope, then install the per-iteration
        // close handler. The scope's `handler_depth` floor is recorded BEFORE the
        // push so a `break`/`continue` (handler_depth > floor) unwinds through it
        // (popping the handler) on its way to the break/continue target.
        // The handler is OUTSIDE the IterNext/done-check above, so a throwing
        // `next()` (or normal exhaustion) does NOT close the iterator (per spec).
        self.loop_ctx.push(LoopCtx::loop_frame(
            self.pending_label.take(),
            self.handler_depth,
        ));
        // Record the iterator so a `break`/`continue` LEAVING this loop from inside a
        // nested one closes it, and (for-await only, which has no close handler) so
        // does a `return`.
        self.loop_ctx.last_mut().unwrap().iter_close = Some(iter_reg);
        let close_push = if let (Some(kr), Some(er)) = (kind_reg, exc_reg) {
            // The close handler subsumes the `return` case, so `return` must NOT
            // also emit an inline IterClose for this frame — that would close twice.
            self.loop_ctx.last_mut().unwrap().close_via_finally = true;
            let at = self.here();
            self.emit(Instr::PushFinally {
                target: 0,
                kind_reg: kr,
                val_reg: er,
            });
            self.handler_depth += 1;
            Some(at)
        } else {
            None
        };
        if let Some(p) = pattern {
            // Per-iteration bindings: captured LEXICAL leaves get a FRESH cell
            // each iteration (LoadUndefined resets the reg so MakeCell boxes a
            // new cell rather than nesting the old one), then the extraction
            // CellSets this iteration's values into it.
            if head_lexical {
                for r in self.pattern_leaf_regs(p) {
                    if self.cell_regs.contains(&r) {
                        self.emit(Instr::LoadUndefined { dst: r });
                        self.emit(Instr::MakeCell { reg: r });
                    }
                }
            }
            self.extract_pattern(p, elem)?;
            // A `const` head PATTERN's leaves are immutable within the iteration,
            // exactly like the simple-identifier head above — but marked only
            // AFTER the extraction, whose per-iteration stores go through
            // `store_binding` and would otherwise throw on the initialization.
            if head_const {
                for r in self.pattern_leaf_regs(p) {
                    self.const_regs.insert(r);
                    // The fresh per-iteration cell is re-tagged each turn so a
                    // closure created in the body can't write through it.
                    if self.cell_regs.contains(&r) {
                        self.emit(Instr::MarkCellConst { reg: r });
                    }
                }
            }
        } else if let Some(tgt) = assign_tgt {
            self.assign_target(tgt, elem)?;
        } else if let Some(b) = &var_binding {
            // `for (var x of …)` writing a non-register binding (catch-param
            // cell / global slot / upvalue): plain assignment, NO fresh cell —
            // `var` is function-scoped (one binding for the whole loop).
            self.store_binding(b, elem);
        } else if var_is_cell {
            // Per-iteration binding: a FRESH cell each iteration so a closure in
            // the body captures THIS element, not the last one (for-of let).
            self.emit(Instr::Move {
                dst: var_reg,
                src: elem,
            });
            self.emit(Instr::MakeCell { reg: var_reg });
            if head_const {
                self.emit(Instr::MarkCellConst { reg: var_reg });
            }
        }
        self.next_reg = save; // reclaim done + elem temps

        // Per-iteration `using` disposal: open a fresh scope, register the loop
        // variable's value, and wrap the body in a finally that disposes it on every
        // iteration exit (normal/throw/break/continue). Nested INSIDE the for-of's
        // close-on-throw handler, so a body throw disposes the resource first, then
        // closes the iterator.
        let using_fin =
            if let (Some(is_async), Some((sreg, kreg, vreg))) = (using_async, using_regs) {
                self.emit(Instr::OpenUsingScope { dst: sreg });
                let src = if var_is_cell {
                    let t = self.alloc_reg();
                    self.emit(Instr::CellGet {
                        dst: t,
                        cell: var_reg,
                    });
                    t
                } else {
                    var_reg
                };
                if is_async {
                    self.emit(Instr::RegisterAsyncDisposable {
                        scope: sreg,
                        val: src,
                    });
                } else {
                    self.emit(Instr::RegisterDisposable {
                        scope: sreg,
                        val: src,
                    });
                }
                if var_is_cell {
                    self.next_reg -= 1;
                }
                let push_at = self.here();
                self.emit(Instr::PushFinally {
                    target: 0,
                    kind_reg: kreg,
                    val_reg: vreg,
                });
                self.handler_depth += 1;
                Some((is_async, sreg, kreg, vreg, push_at))
            } else {
                None
            };

        self.stmt(body)?;

        // Close the per-iteration `using` finally (its DisposeScope/await-loop runs
        // on the normal path here, and on abrupt exits via the finally handler).
        if let Some((is_async, sreg, kreg, vreg, push_at)) = using_fin {
            self.emit_leave_finally_normal(kreg);
            self.handler_depth -= 1;
            let jto_fin = self.here();
            self.emit(Instr::Jump { target: 0 });
            let fin_start = self.here();
            if let Instr::PushFinally { target, .. } = &mut self.code[push_at as usize] {
                *target = fin_start;
            }
            self.patch_jump(jto_fin, fin_start);
            if is_async {
                self.emit_async_dispose_loop(sreg, kreg, vreg);
            } else {
                self.emit(Instr::DisposeScope {
                    scope: sreg,
                    kind_reg: kreg,
                    val_reg: vreg,
                });
            }
            self.emit(Instr::EndFinally {
                kind_reg: kreg,
                val_reg: vreg,
            });
        }

        // Normal iteration completion: pop the close handler before looping (the
        // block is never entered on this path, so no completion record is needed).
        if close_push.is_some() {
            self.emit(Instr::PopFinally);
            self.handler_depth -= 1;
        }
        let ctx = self.loop_ctx.pop().unwrap();
        for c in ctx.continue_jumps {
            self.patch_jump(c, top); // continue → re-run IterNext (advance + test)
        }
        self.emit(Instr::Jump { target: top });
        // The close handler's block: reached only by an abrupt exit out of the
        // element binding or body — a throw (unwind_to_handler), a `return`
        // (route_through_finally, including the one a `gen.return()` at a `yield`
        // in the body synthesises), or a `break`/`continue` that leaves this
        // handler (route_jump_through_finally). `IterCloseFinally` decides which
        // of those actually closes; `EndFinally` then resumes the completion.
        if let (Some(at), Some(kr), Some(er)) = (close_push, kind_reg, exc_reg) {
            let fin_start = self.here();
            self.emit(Instr::IterCloseFinally {
                iter: iter_reg,
                kind_reg: kr,
            });
            self.emit(Instr::EndFinally {
                kind_reg: kr,
                val_reg: er,
            });
            if let Instr::PushFinally { target, .. } = &mut self.code[at as usize] {
                *target = fin_start;
            }
        }
        // A `break` out of the loop closes the (not-yet-exhausted) iterator via a
        // block reached only from break jumps; the normal `done` exit skips it
        // (the iterator already signalled completion).
        let end = if ctx.break_jumps.is_empty() {
            let e = self.here();
            self.patch_jump(jdone, e);
            e
        } else {
            let brk_target = self.here();
            self.emit(Instr::IterClose { iter: iter_reg });
            let e = self.here();
            self.patch_jump(jdone, e);
            for b in ctx.break_jumps {
                self.patch_jump(b, brk_target);
            }
            e
        };
        let _ = end;
        self.pop_scope();
        Ok(())
    }

    /// `for (const k in obj) body` — iterate the object's own enumerable string
    /// keys (or an array's index strings), via the ObjectKeys op + an index loop.
    pub(crate) fn for_in_statement(
        &mut self,
        left: &ast::ForTarget,
        right: &ast::Expr,
        body: &ast::Stmt,
    ) -> R<()> {
        self.push_scope();
        // Mirror for-of: `for (let k in …)` declares, `for (let [a,b] in …)`
        // destructures, `for (k in …)` / `for (obj.k in …)` assigns to a target.
        let decl_pat = match left {
            ast::ForTarget::Var(d) => Some(&d.decls[0].id),
            _ => None,
        };
        let pattern = decl_pat.filter(|p| !matches!(p, ast::Pattern::Ident(_)));
        let assign_tgt = match left {
            ast::ForTarget::Target(t) => Some(t),
            _ => None,
        };

        // Annex B B.3.6: `for (var a = init in obj)` evaluates the initializer
        // ONCE and assigns it to the (function-scoped / shadowing) binding
        // BEFORE the object expression runs.
        if let ast::ForTarget::Var(d) = left {
            if d.kind == ast::VarKind::Var {
                if let (Some(init), ast::Pattern::Ident(id)) = (&d.decls[0].init, &d.decls[0].id) {
                    // ResolveBinding FIRST (head_var_binding may permanently
                    // allocate the function-scoped register — it must stay
                    // below the temp watermark we reclaim to), then evaluate
                    // the initializer, then PutValue.
                    let b = self.head_var_binding(id);
                    let save = self.next_reg;
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, id)?;
                    let with_objs = self.with_obj_regs(id);
                    if with_objs.is_empty() {
                        self.store_binding(&b, v);
                    } else {
                        self.store_with(id, &with_objs, v);
                    }
                    self.next_reg = save;
                }
            }
        }

        let obj_reg = self.declare_local("<forin.obj>");
        // The right-hand expression sees the loop's `let`/`const` binding(s) in their
        // TDZ (`for (let x in x) {}` throws a ReferenceError), per ForIn/OfHeadEvaluation.
        let tdz_added: Vec<String> = match left {
            ast::ForTarget::Var(d) if d.kind.is_lexical() => {
                let mut names = std::collections::HashSet::new();
                capture::collect_pattern_names(&d.decls[0].id, &mut names);
                names
                    .into_iter()
                    .filter(|n| self.param_tdz.insert(n.clone()))
                    .collect()
            }
            _ => Vec::new(),
        };
        // A RUNTIME TDZ scope over the head expression too: a CLOSURE created
        // in `right` captures an uninitialized cell for each head name and
        // throws ReferenceError when it later reads it (the head env binding
        // is never initialized), instead of capturing the outer binding.
        let head_tdz_scope = !tdz_added.is_empty();
        if head_tdz_scope {
            self.push_scope();
            for n in &tdz_added {
                let r = self.alloc_reg();
                self.scopes.last_mut().unwrap().push((n.clone(), r));
                self.emit(Instr::MakeCellTdz { reg: r });
                self.cell_regs.insert(r);
            }
        }
        let v = self.expr_into(right, obj_reg)?;
        if head_tdz_scope {
            self.pop_scope();
        }
        for n in &tdz_added {
            self.param_tdz.remove(n);
        }
        if v != obj_reg {
            self.emit(Instr::Move {
                dst: obj_reg,
                src: v,
            });
        }
        let keys_reg = self.declare_local("<forin.keys>");
        self.emit(Instr::ForInKeys {
            dst: keys_reg,
            obj: obj_reg,
        });
        let len_reg = self.declare_local("<forin.len>");
        self.emit(Instr::LenOf {
            dst: len_reg,
            obj: keys_reg,
        });
        let idx_reg = self.declare_local("<forin.idx>");
        self.emit(Instr::LoadInt {
            dst: idx_reg,
            // `ForInKeys` carries its receiver/version guard in an engine-private
            // prefix. JavaScript-visible keys start after it.
            val: crate::bytecode::FORIN_SNAPSHOT_PREFIX as i32,
        });

        let head_lexical = matches!(left, ast::ForTarget::Var(d) if d.kind.is_lexical());
        let head_const = matches!(left, ast::ForTarget::Var(d) if matches!(
            d.kind,
            ast::VarKind::Const | ast::VarKind::Using | ast::VarKind::AwaitUsing
        ));
        // A `var` head whose binding is NOT a plain current-function register —
        // a shadowing catch parameter cell, a global slot, an upvalue: the
        // per-iteration write goes through `store_binding` (the loop creates NO
        // binding of its own — Annex B catch-redeclared-for-in-var).
        let mut var_binding: Option<Binding> = None;
        let (var_reg, var_is_cell) = match (pattern, assign_tgt) {
            (Some(p), _) => {
                // As in for-of: a `var` head pattern reuses the hoisted var
                // bindings rather than declaring loop-local shadows.
                self.pattern_block_local = head_lexical;
                let r = self.declare_pattern(p, !head_lexical);
                self.pattern_block_local = false;
                r?;
                (0, false)
            }
            (None, Some(_)) => (0, false),
            (None, None) => {
                let var_name = for_left_name(left)?;
                if matches!(left, ast::ForTarget::Var(d) if d.kind == ast::VarKind::Var) {
                    // `for (var x in …)`: resolve the EXISTING binding (the
                    // hoisted function-scope var, a shadowing catch param, or
                    // the global slot) instead of declaring a loop-local; a
                    // first-mention var creates its FUNCTION-scoped binding.
                    match self.head_var_binding(&var_name) {
                        Binding::Local(r) => (r, false),
                        other => {
                            var_binding = Some(other);
                            (0, false)
                        }
                    }
                } else {
                    let r = self.declare_local(&var_name);
                    // A `const` loop variable is immutable WITHIN an iteration:
                    // a body assignment throws TypeError (mirrors for-of).
                    if let ast::ForTarget::Var(d) = left {
                        if matches!(
                            d.kind,
                            ast::VarKind::Const | ast::VarKind::Using | ast::VarKind::AwaitUsing
                        ) {
                            self.const_regs.insert(r);
                        }
                    }
                    (r, self.cell_regs.contains(&r))
                }
            }
        };

        let top = self.here();
        // The `idx < len` guard over the key snapshot: both operands are ints
        // the compiler owns, so the fused form is the unfused pair minus the
        // boolean temp.
        let jf;
        if exprs::fused_cmp_jump_enabled() {
            jf = self.here();
            self.emit(Instr::JumpIfNotLt {
                a: idx_reg,
                b: len_reg,
                target: 0,
            });
        } else {
            let cond = self.temp();
            self.emit(Instr::Lt {
                dst: cond,
                a: idx_reg,
                b: len_reg,
            });
            jf = self.here();
            self.emit(Instr::JumpIfFalse { cond, target: 0 });
            self.next_reg -= 1;
        }

        let save = self.next_reg;
        // Read the current key into a temp (pattern / assignment target /
        // store-through binding) or straight into the loop var (a
        // per-iteration cell var is boxed after).
        let key_dst = if pattern.is_some() || assign_tgt.is_some() || var_binding.is_some() {
            self.alloc_reg()
        } else {
            var_reg
        };
        self.emit(Instr::GetIndex {
            dst: key_dst,
            obj: keys_reg,
            key: idx_reg,
        });
        // EnumerateObjectProperties: a not-yet-visited key DELETED during the
        // loop is skipped — re-check liveness against the receiver each
        // iteration (the snapshot in keys_reg is taken once) and jump to the
        // increment when the key is gone.
        let live = self.temp();
        // Pass the snapshot, not just the receiver: its private prefix owns the
        // exact receiver + versions captured by THIS enumeration. That is what
        // keeps nested/re-entrant walks from sharing mutable guard state.
        self.emit(Instr::ForInLive {
            dst: live,
            obj: keys_reg,
            key: key_dst,
        });
        let live_jf = self.here();
        self.emit(Instr::JumpIfFalse {
            cond: live,
            target: 0,
        });
        self.next_reg -= 1;
        if let Some(p) = pattern {
            if head_lexical {
                for r in self.pattern_leaf_regs(p) {
                    if self.cell_regs.contains(&r) {
                        self.emit(Instr::LoadUndefined { dst: r });
                        self.emit(Instr::MakeCell { reg: r });
                    }
                }
            }
            self.extract_pattern(p, key_dst)?;
            // Marked after the extraction, as in for-of: the per-iteration
            // stores that initialize the leaves go through `store_binding`.
            if head_const {
                for r in self.pattern_leaf_regs(p) {
                    self.const_regs.insert(r);
                    if self.cell_regs.contains(&r) {
                        self.emit(Instr::MarkCellConst { reg: r });
                    }
                }
            }
        } else if let Some(tgt) = assign_tgt {
            self.assign_target(tgt, key_dst)?;
        } else if let Some(b) = &var_binding {
            // `for (var x in …)` writing a non-register binding (catch-param
            // cell / global slot / upvalue): plain assignment, NO fresh cell —
            // `var` is function-scoped (one binding for the whole loop).
            self.store_binding(b, key_dst);
        } else if var_is_cell {
            // Per-iteration binding: a FRESH cell each iteration (for-in let).
            self.emit(Instr::MakeCell { reg: var_reg });
            if head_const {
                self.emit(Instr::MarkCellConst { reg: var_reg });
            }
        }
        self.next_reg = save;

        self.loop_ctx.push(LoopCtx::loop_frame(
            self.pending_label.take(),
            self.handler_depth,
        ));
        self.stmt(body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        self.patch_jump(live_jf, cont); // dead (deleted) key → skip to increment
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → increment + re-test
        }
        self.emit(Instr::AddInt {
            dst: idx_reg,
            a: idx_reg,
            imm: 1,
            upd: false,
        });
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        self.pop_scope();
        Ok(())
    }

    pub(crate) fn patch_jump(&mut self, at: u32, target: u32) {
        match &mut self.code[at as usize] {
            Instr::Jump { target: t }
            | Instr::JumpIfFalse { target: t, .. }
            | Instr::JumpIfTrue { target: t, .. }
            | Instr::JumpIfNotLt { target: t, .. }
            | Instr::JumpIfNotLe { target: t, .. }
            | Instr::JumpFinally { target: t, .. } => *t = target,
            _ => panic!("patch_jump on non-jump"),
        }
    }

    /// Emit a `break`/`continue` jump targeting `loop_ctx[idx]`. When the target's
    /// handler depth is below the current one, the jump exits one or more `try`
    /// blocks: emit `JumpFinally` (routes through each intervening `finally`,
    /// popping any intervening `catch`); otherwise a plain `Jump` (so loops without
    /// an enclosing `try` stay JIT-eligible). Records the jump ip so the loop
    /// epilogue patches its target.
    pub(crate) fn emit_loop_jump(&mut self, idx: usize, is_break: bool) {
        let floor = self.loop_ctx[idx].handler_depth;
        let j = self.here();
        if self.handler_depth > floor {
            self.emit(Instr::JumpFinally {
                target: 0,
                floor: floor as u16,
            });
        } else {
            self.emit(Instr::Jump { target: 0 });
        }
        if is_break {
            self.loop_ctx[idx].break_jumps.push(j);
        } else {
            self.loop_ctx[idx].continue_jumps.push(j);
        }
    }
}
