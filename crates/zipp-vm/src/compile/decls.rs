// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'a> FnCompiler<'a> {
    // ── statements ──
    pub(crate) fn stmt(&mut self, s: &ox::Statement) -> R<()> {
        use ox::Statement as S;
        match s {
            S::ExpressionStatement(e) => {
                let r = self.expr(&e.expression)?;
                // eval completion: remember this expression's value (the last one
                // executed wins, matching the spec's expression-completion value).
                if let Some(cr) = self.completion_reg {
                    self.emit(Instr::Move { dst: cr, src: r });
                }
                let _ = r; // value otherwise discarded
            }
            S::VariableDeclaration(d) => self.var_decl(d)?,
            S::BlockStatement(b) => {
                self.push_scope();
                // Pre-create TDZ cells for the block's CAPTURED simple-identifier
                // lexical (`let`/`const`/`class`) declarations, so a closure
                // materialized BEFORE the textual declaration (`{ function f() {
                // x = 1; } f(); let x; }`) captures the block's binding (in its
                // TDZ → ReferenceError) instead of resolving to a global. The
                // textual declaration reuses the register, ending the TDZ.
                // Non-captured names keep plain registers (no runtime cost).
                self.predeclare_lexical_tdz(&b.body);
                // Hoist block-level function declarations: declare each as a local
                // in this block scope first, so `func_decl` binds it (and forward
                // references / calls within the block resolve to the local rather
                // than an undeclared global). Only inside a real function body —
                // at script top level, `func_decl` binds block functions to globals
                // (Annex B hoisting), so a local here would shadow that with an
                // uninitialized slot.
                let mut entry_fns: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for st in &b.body {
                    if let S::FunctionDeclaration(f) = st {
                        if let Some(id) = &f.id {
                            // Inside a function body, block functions are always
                            // block-local. At script level they normally hoist to
                            // a global (Annex B) and so are NOT pre-declared here —
                            // UNLESS the name conflicts with an enclosing-block
                            // lexical binding (conflict-skip), OR the code is STRICT
                            // (Annex B is not honored in strict mode, so the function
                            // stays block-local and does not leak past the block).
                            let nm = id.name.as_str();
                            // Block-local for strict / enclosing-block lexical
                            // conflict / a protected param-lexical-class name / a
                            // B.3.3 var name. A name matching an existing function is
                            // NOT shadowed — it is directly updated (B.3.3).
                            if self.cx.in_strict
                                || self.block_fn_conflicts(nm)
                                || self.protect_names.contains(nm)
                                || self.b33_names.contains(nm)
                            {
                                self.declare_local(nm);
                                entry_fns.insert(nm.to_string());
                            }
                        }
                    }
                }
                if Self::block_has_using(&b.body) {
                    // A block with a top-level `using` declaration disposes its
                    // resources on every exit — desugar onto a synthetic finally.
                    self.compile_using_block(&b.body, false)?;
                } else {
                    // BlockDeclarationInstantiation: the block's function
                    // declarations are materialized at BLOCK ENTRY (a call
                    // before the textual declaration works), while the Annex B
                    // var-binding sync stays at the declaration's textual
                    // position (B.3.3.3 fires at evaluation, not entry).
                    for st in &b.body {
                        if let S::FunctionDeclaration(f) = st {
                            if let Some(id) = &f.id {
                                if entry_fns.contains(id.name.as_str()) {
                                    self.func_decl_inner(f, false)?;
                                }
                            }
                        }
                    }
                    for st in &b.body {
                        if let S::FunctionDeclaration(f) = st {
                            if let Some(id) = &f.id {
                                if entry_fns.contains(id.name.as_str()) {
                                    self.emit_b33_sync(id.name.as_str());
                                    continue;
                                }
                            }
                        }
                        self.stmt(st)?;
                    }
                }
                self.pop_scope();
            }
            S::IfStatement(i) => self.if_stmt(i)?,
            S::WhileStatement(w) => {
                self.reset_loop_completion();
                self.while_stmt(w)?
            }
            S::DoWhileStatement(d) => {
                self.reset_loop_completion();
                self.do_while_statement(d)?
            }
            S::ForStatement(f) => {
                self.reset_loop_completion();
                self.for_stmt(f)?
            }
            S::ForOfStatement(f) => {
                self.reset_loop_completion();
                self.for_of_statement(f)?
            }
            S::ForInStatement(f) => {
                self.reset_loop_completion();
                self.for_in_statement(f)?
            }
            S::BreakStatement(b) => {
                // `break label` targets the labeled loop/switch; bare `break` the
                // innermost.
                let idx = match &b.label {
                    Some(lbl) => self
                        .loop_ctx
                        .iter()
                        .rposition(|c| c.label.as_deref() == Some(lbl.name.as_str())),
                    None => self.loop_ctx.len().checked_sub(1),
                };
                let idx = match idx {
                    Some(i) => i,
                    None => return Err("`break` target not found (outside a loop / unknown label)".into()),
                };
                // Iterators of for-of frames BETWEEN here and the target close
                // (innermost first); the target loop's own exit path closes its.
                let iters: Vec<Reg> = self.loop_ctx[idx + 1..]
                    .iter()
                    .rev()
                    .filter_map(|c| c.iter_close)
                    .collect();
                for it in iters {
                    self.emit(Instr::IterClose { iter: it });
                }
                self.emit_loop_jump(idx, true);
            }
            S::ContinueStatement(c) => {
                // `continue [label]` targets the (labeled) enclosing LOOP, skipping
                // switch frames.
                let idx = match &c.label {
                    Some(lbl) => self
                        .loop_ctx
                        .iter()
                        .rposition(|ctx| ctx.is_loop && ctx.label.as_deref() == Some(lbl.name.as_str())),
                    None => self.loop_ctx.iter().rposition(|ctx| ctx.is_loop),
                };
                let idx = match idx {
                    Some(i) => i,
                    None => return Err("`continue` target not found (outside a loop / unknown label)".into()),
                };
                let iters: Vec<Reg> = self.loop_ctx[idx + 1..]
                    .iter()
                    .rev()
                    .filter_map(|c| c.iter_close)
                    .collect();
                for it in iters {
                    self.emit(Instr::IterClose { iter: it });
                }
                self.emit_loop_jump(idx, false);
            }
            S::LabeledStatement(l) => {
                if let S::BlockStatement(b) = &l.body {
                    // `label: { … break label … }` — a break-only target around a
                    // block (continue to a block label is invalid, and naturally
                    // won't match: the frame is not a loop).
                    self.loop_ctx
                        .push(LoopCtx::switch_frame(Some(l.label.name.to_string()), self.handler_depth));
                    self.push_scope();
                    for s in &b.body {
                        self.stmt(s)?;
                    }
                    self.pop_scope();
                    let ctx = self.loop_ctx.pop().unwrap();
                    let end = self.here();
                    for j in ctx.break_jumps {
                        self.patch_jump(j, end);
                    }
                } else {
                    // A loop/switch consumes the label for break/continue;
                    // cleared afterwards if the body was something else.
                    self.pending_label = Some(l.label.name.to_string());
                    self.stmt(&l.body)?;
                    self.pending_label = None;
                }
            }
            S::SwitchStatement(s) => self.switch_stmt(s)?,
            S::ReturnStatement(r) => {
                // Proper tail call: strict `return <expr with a call in tail
                // position>` in an UNPROTECTED context (no try handlers, no
                // enclosing loop with an iterator to close, no using scope,
                // not generator/async/script/eval) reuses the current frame —
                // constant stack for tail recursion. Tail positions cover the
                // call itself plus conditional arms, logical right operands,
                // sequence finals, parenthesization, and plain-tag tagged
                // templates. The TailCall prefix falls through to the
                // ordinary Call+Return for non-plain callees.
                if let Some(arg) = &r.argument {
                    if self.tail_call_position() && self.expr_has_tail_call(arg) {
                        self.emit_tail_return(arg)?;
                        return Ok(());
                    }
                }
                // Evaluate the return value FIRST (its side effects precede the
                // iterator closes), then run IteratorClose on every enclosing for-of /
                // for-await-of (innermost first) — a `return` is an abrupt completion
                // that closes the iterator (a throwing `return()` then propagates,
                // discarding the value). `break`/`throw` already close via their paths.
                let v = match &r.argument {
                    Some(arg) => {
                        let v = self.expr(arg)?;
                        // In an ASYNC GENERATOR, `return expr;` performs
                        // Await(exprValue) (spec ReturnStatement evaluation): a
                        // thenable operand is adopted (observable `then` read)
                        // and an explicit `return undefined` settles one tick
                        // later than an implicit return. Awaits into a temp so
                        // a returned local's register isn't clobbered.
                        if self.in_generator && self.in_async {
                            let t = self.temp();
                            self.emit(Instr::Await { dst: t, val: v });
                            Some(t)
                        } else {
                            Some(v)
                        }
                    }
                    None => None,
                };
                let iters: Vec<Reg> =
                    self.loop_ctx.iter().rev().filter_map(|c| c.iter_close).collect();
                for it in iters {
                    self.emit(Instr::IterClose { iter: it });
                }
                match v {
                    Some(v) => self.emit(Instr::Return { src: v }),
                    None => self.emit(Instr::ReturnUndefined),
                }
            }
            S::FunctionDeclaration(f) => self.func_decl(f)?,
            S::ThrowStatement(t) => {
                let v = self.expr(&t.argument)?;
                self.emit(Instr::Throw { src: v });
            }
            S::TryStatement(t) => self.try_statement(t)?,
            S::ClassDeclaration(c) => self.class_decl(c)?,
            S::EmptyStatement(_) => {}
            S::DebuggerStatement(_) => {} // `debugger;` is a no-op (no attached debugger)
            S::WithStatement(w) => {
                // `with` is a SyntaxError in strict mode (early error) — preserve
                // that so strict negative tests keep passing.
                if self.cx.in_strict {
                    return Err("SyntaxError: 'with' statements are not allowed in strict mode".into());
                }
                // Completion: UpdateEmpty(C, undefined) — an empty/abrupt body
                // yields undefined for eval, not the previous statement's value.
                self.reset_loop_completion();
                // ToObject(GetValue(object)) becomes the with-environment's binding
                // object. Held in a hidden scope-local so it survives the whole body
                // (per-statement temp resets allocate above it).
                let raw = self.expr(&w.object)?;
                // ToObject(null)/ToObject(undefined) throw a TypeError (the with
                // object must be coercible).
                self.emit(Instr::CheckCoercible { src: raw });
                self.push_scope();
                // UNIQUE hidden name (leading space — uncollidable) so a nested
                // closure can capture THIS with-object across the enclosing
                // chain; boxed into a cell after the value exists (like a catch
                // param) so it is upvalue-capturable. Probe sites unwrap via
                // `with_obj_regs`.
                let wname = format!(" with-object-{}", self.cx.with_name_counter);
                self.cx.with_name_counter += 1;
                let obj_reg = self.declare_local_no_box(&wname);
                self.emit(Instr::ToObject { dst: obj_reg, src: raw });
                self.emit(Instr::MakeCell { reg: obj_reg });
                self.cell_regs.insert(obj_reg);
                let floor = self.scopes.len();
                self.with_stack.push(WithScope { obj_reg, floor });
                let r = self.stmt(&w.body);
                self.with_stack.pop();
                self.pop_scope();
                r?;
            }
            // ── ES module declarations (only reached for SourceType::module, i.e.
            // a fixture loaded by a dynamic `import()`; a script never parses these).
            S::ImportDeclaration(_) => {
                // Handled by the MODULE PRE-PASS (import bindings hoist: a
                // reference or assignment may precede the declaration).
            }
            S::ExportNamedDeclaration(e) => {
                // `export {imported as exported} from './m'` (re-export): record the
                // (exported, imported, specifier) triples so the loader can resolve
                // them against the dependency module. No local binding is created.
                if let Some(src) = &e.source {
                    let spec = src.value.to_string();
                    for spec_item in &e.specifiers {
                        let exported = module_export_name(&spec_item.exported);
                        let imported = module_export_name(&spec_item.local);
                        self.cx.module_reexports.push((exported, imported, spec.clone()));
                    }
                    return Ok(());
                }
                // `export var/let/const/function/class …`: compile the inner
                // declaration normally (its top-level binding becomes a global), then
                // record each bound name as an export (exported name == local name).
                if let Some(decl) = &e.declaration {
                    match decl {
                        ox::Declaration::VariableDeclaration(d) => {
                            self.var_decl(d)?;
                            let mut names = std::collections::HashSet::new();
                            for dd in &d.declarations {
                                capture::collect_pattern_names(&dd.id, &mut names);
                            }
                            for n in names {
                                self.cx.module_exports.push((n.clone(), n));
                            }
                        }
                        ox::Declaration::FunctionDeclaration(f) => {
                            self.func_decl(f)?;
                            if let Some(id) = &f.id {
                                let n = id.name.to_string();
                                self.cx.module_exports.push((n.clone(), n));
                            }
                        }
                        ox::Declaration::ClassDeclaration(c) => {
                            self.class_decl(c)?;
                            if let Some(id) = &c.id {
                                let n = id.name.to_string();
                                self.cx.module_exports.push((n.clone(), n));
                            }
                        }
                        _ => return Err("unsupported export declaration".into()),
                    }
                }
                // `export { local as exported, … }`.
                for spec in &e.specifiers {
                    let local = module_export_name(&spec.local);
                    let exported = module_export_name(&spec.exported);
                    self.cx.module_exports.push((exported, local));
                }
            }
            S::ExportDefaultDeclaration(e) => {
                use ox::ExportDefaultDeclarationKind as K;
                // Bind the default value to a synthetic global "*default*" (not a
                // valid identifier, so no user collision) and export it as "default".
                let slot = self.cx.global_slot("*default*") as u32;
                let tmp = self.temp();
                // `export default function f(){}` / `class C{}` also binds the NAME
                // (f / C) as a module-local declaration, so code in the module can
                // reference it (the slot is module-declared for per-module isolation).
                let mut bind_name: Option<String> = None;
                match &e.declaration {
                    K::FunctionDeclaration(f) => {
                        // A NAMED default hoistable declaration is an ordinary
                        // MUTABLE module binding (`fn = 2` inside the body works,
                        // unlike a function EXPRESSION self-name) and the export
                        // entry LocalName is the NAME — ns.default tracks the
                        // LIVE binding, not a *default* snapshot.
                        if let Some(id) = &f.id {
                            let n = id.name.to_string();
                            self.func_decl(f)?;
                            let nslot = self.cx.global_slot(&n) as u32;
                            self.cx.decl_globals.insert(nslot);
                            self.cx.module_exports.push(("default".to_string(), n));
                            self.next_reg -= 1;
                            return Ok(());
                        }
                        // An ANONYMOUS default-exported function/generator is named
                        // "default" (NamedEvaluation) and HOISTS like any other
                        // function declaration — `f()` before this statement works
                        // through an `import f from './self'` alias.
                        let (id, has_up) =
                            self.compile_func_expr(Some("default".to_string()), f)?;
                        if self.is_script && self.cx.script_binds_globals && !has_up {
                            self.cx.functions[id as usize].name_global = Some(slot as u16);
                            self.cx.decl_globals.insert(slot);
                            self.next_reg -= 1;
                            self.cx
                                .module_exports
                                .push(("default".to_string(), "*default*".to_string()));
                            return Ok(());
                        }
                        self.emit_make_callable(tmp, id, has_up);
                        bind_name = None;
                    }
                    K::ClassDeclaration(c) => {
                        // An ANONYMOUS default-exported class is named "default".
                        let r = self.class_expr(c, tmp, if c.id.is_none() { Some("default") } else { None })?;
                        if r != tmp {
                            self.emit(Instr::Move { dst: tmp, src: r });
                        }
                        bind_name = c.id.as_ref().map(|i| i.name.to_string());
                    }
                    other => {
                        // `export default <AssignmentExpression>`: an anonymous
                        // function/arrow/class expression is named "default"
                        // (NamedEvaluation), like `const default = …` would.
                        let expr = other.as_expression().ok_or("unsupported default export")?;
                        let v = self.compile_named_init(tmp, expr, "default")?;
                        if v != tmp {
                            self.emit(Instr::Move { dst: tmp, src: v });
                        }
                    }
                }
                self.emit(Instr::StoreGlobal { idx: slot, src: tmp });
                if let Some(name) = bind_name {
                    let nslot = self.cx.global_slot(&name) as u32;
                    self.cx.decl_globals.insert(nslot); // module-declared → per-module slot
                    self.emit(Instr::StoreGlobal { idx: nslot, src: tmp });
                }
                self.next_reg -= 1;
                self.cx
                    .module_exports
                    .push(("default".to_string(), "*default*".to_string()));
            }
            S::ExportAllDeclaration(e) => {
                if let Some(exported) = &e.exported {
                    // `export * as ns from './m'` exports the dependency's
                    // NAMESPACE object under `ns` (linked by the loader).
                    self.cx
                        .module_ns_reexports
                        .push((module_export_name(exported), e.source.value.to_string()));
                } else {
                    // `export * from './m'` — copy all of the dependency's exports
                    // (except default) into this module's namespace at link time.
                    self.cx.module_star_reexports.push(e.source.value.to_string());
                }
            }
            _ => return Err("unsupported statement (not in the zipp-vm v1 subset yet)".into()),
        }
        Ok(())
    }

    /// Evaluate an initializer into `dst`, inferring a name for an anonymous
    /// function/arrow assigned to a binding (`const f = () => {}` ⇒ `f.name`
    /// === "f"). A named function expression keeps its own name.
    pub(crate) fn compile_named_init(&mut self, dst: Reg, init: &ox::Expression, name: &str) -> R<Reg> {
        match init {
            ox::Expression::ArrowFunctionExpression(a) => {
                let (id, _has_up) = self.compile_arrow(a, name)?;
                self.emit_make_arrow(dst, id);
                Ok(dst)
            }
            ox::Expression::FunctionExpression(f) if f.id.is_none() => {
                let (id, has_up) = self.compile_func_expr(Some(name.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            // `const C = class {}` / `x = class {}` — an anonymous class takes the
            // binding name (a named `class C {}` keeps its own).
            ox::Expression::ClassExpression(c) if c.id.is_none() => {
                self.class_expr(c, dst, Some(name))
            }
            // NamedEvaluation sees through parentheses: `var f = (function(){})`.
            ox::Expression::ParenthesizedExpression(p) => {
                self.compile_named_init(dst, &p.expression, name)
            }
            _ => self.expr_into(init, dst),
        }
    }

    pub(crate) fn var_decl(&mut self, d: &ox::VariableDeclaration) -> R<()> {
        // A `const` binding is immutable: record its slot/register so a later
        // assignment throws a TypeError (initialization below never goes through
        // store_binding, so it is unaffected). `using`/`await using` bindings
        // are equally immutable (CreateImmutableBinding in the spec).
        let is_const = matches!(
            d.kind,
            ox::VariableDeclarationKind::Const
                | ox::VariableDeclarationKind::Using
                | ox::VariableDeclarationKind::AwaitUsing
        );
        for decl in &d.declarations {
            // Destructuring declaration (`let {a,b} = o`, `let [x,...r] = arr`):
            // declare every leaf binding, evaluate the initializer once into a
            // scratch register, then extract each target from it.
            if !matches!(decl.id, ox::BindingPattern::BindingIdentifier(_)) {
                let init = decl
                    .init
                    .as_ref()
                    .ok_or("a destructuring declaration requires an initializer")?;
                // A block-nested lexical (`let`/`const`) destructuring at script
                // level binds its leaves block-local, not global — same rule as the
                // simple-identifier path below, so `{ let {a} = o; }` doesn't leak.
                let block_local = d.kind.is_lexical() && self.scopes.len() > 1;
                self.pattern_block_local = block_local;
                self.declare_pattern(&decl.id)?;
                let save = self.next_reg;
                // TOP-LEVEL global-bound leaves: INITIALIZE each slot before
                // extraction. The extraction stores are assignment-flavored —
                // in STRICT code (a module body) StoreGlobalStrict throws on
                // an UNINITIALIZED slot, so the leaves' TDZ must end at this
                // statement, not at the store. Lexical leaves also register
                // like their simple-identifier siblings (const immutability,
                // module per-module slots, script-GDI bookkeeping).
                if self.is_script && self.cx.script_binds_globals && !block_local {
                    let mut leaves = std::collections::HashSet::new();
                    capture::collect_pattern_names(&decl.id, &mut leaves);
                    let undef = self.alloc_reg();
                    self.emit(Instr::LoadUndefined { dst: undef });
                    for n in leaves {
                        let slot = self.cx.global_slot(&n) as u32;
                        if d.kind.is_lexical() {
                            self.cx.lexical_globals.insert(slot);
                        }
                        // (NOT const_globals: the extraction itself stores
                        // through store_binding, which throws for a known
                        // const — initialization must stay writable.)
                        self.emit(Instr::StoreGlobal { idx: slot, src: undef });
                    }
                }
                let src = self.alloc_reg();
                // The pattern's leaves are in their TDZ while the INITIALIZER
                // evaluates (`let {a} = a` throws); they leave it before the
                // extraction (a sibling default may read an earlier leaf).
                let tdz_leaves: Vec<String> = if d.kind.is_lexical() {
                    let mut names = std::collections::HashSet::new();
                    capture::collect_pattern_names(&decl.id, &mut names);
                    names.into_iter().filter(|n| self.param_tdz.insert(n.clone())).collect()
                } else {
                    Vec::new()
                };
                let sv = self.expr_into(init, src);
                for n in &tdz_leaves {
                    self.param_tdz.remove(n);
                }
                let sv = sv?;
                if sv != src {
                    self.emit(Instr::Move { dst: src, src: sv });
                }
                self.extract_pattern(&decl.id, src)?;
                self.pattern_block_local = false;
                self.next_reg = save; // reclaim the source + extraction temps
                continue;
            }
            let name = match &decl.id {
                ox::BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                _ => unreachable!("handled above"),
            };
            // Strict mode: `var eval` / `let arguments` etc. are early SyntaxErrors.
            strict_name_err(self.cx.in_strict, name)?;

            // `var` (function-scoped) and a TRUE top-level `let`/`const` bind to
            // GLOBAL slots, so a nested function resolves them via LoadGlobal (a
            // top-level binding is never an upvalue). But a `let`/`const` nested
            // inside a BLOCK is block-scoped and must NOT leak to the global
            // scope (`{ let x = 1; }` leaves `x` undeclared after the block) — it
            // falls through to a block-local binding even at script level.
            let block_scoped_lexical =
                d.kind.is_lexical() && (self.scopes.len() > 1 || self.cx.eval_locals);
            // EVAL root: `var x` where x is a CALLER binding — the declaration
            // is a no-op (the binding exists); an initializer assigns THROUGH
            // the captured cell (sloppy direct eval's var env is the caller's).
            if self.is_script
                && !d.kind.is_lexical()
                && self.scopes.len() == 1
                && self.cx.eval_caller_scope.iter().any(|n| n == name)
            {
                if let Some(init) = &decl.init {
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, name)?;
                    let idx = self
                        .resolve_upvalue(name)
                        .ok_or("eval caller binding upvalue")?;
                    self.emit(Instr::UpvalSet { idx, src: v });
                    self.next_reg -= 1;
                }
                continue;
            }
            // Annex B 3.5: `var foo = init` where `foo` is SHADOWED by a deeper
            // block binding (a catch parameter — the only legal shadow of a
            // var name): the hoisted function/global var binding is untouched
            // here; the INITIALIZER assigns the shadowing binding via ordinary
            // resolution (`catch (foo) { var foo = x; }` writes the parameter).
            if !d.kind.is_lexical()
                && decl.init.is_some()
                && self.scopes.len() > 1
                && self.scopes[1..].iter().flatten().any(|(n, _)| n == name)
            {
                let init = decl.init.as_ref().unwrap();
                let save = self.next_reg;
                // ResolveBinding BEFORE the initializer (two-phase; the probes
                // are observable and the resolved base survives a delete).
                let with_objs = self.with_obj_regs(name);
                if with_objs.is_empty() {
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, name)?;
                    let b = self.resolve(name);
                    self.store_binding(&b, v);
                } else {
                    let target = self.with_resolve_target(name, &with_objs);
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, name)?;
                    self.with_store_resolved(name, target, v);
                }
                self.next_reg = save;
                continue;
            }
            if self.is_script && self.cx.script_binds_globals && !block_scoped_lexical {
                let slot = self.cx.global_slot(name) as u32;
                if is_const {
                    self.cx.const_globals.insert(slot);
                }
                // A bare `var x;` performs NO assignment — the hoisted slot was
                // seeded undefined at startup, and re-storing here would RESET
                // an already-assigned binding (`var x = 5; var x;` keeps 5). A
                // bare lexical (`let x;`) DOES store: that write ends its TDZ.
                if decl.init.is_none() && !d.kind.is_lexical() {
                    continue;
                }
                // `var x = init` inside a `with` whose object has `x`: the
                // declaration's binding is hoisted (the global slot is already
                // undefined), but the INITIALIZER is an assignment evaluated in
                // the with-scope. TWO-PHASE: ResolveBinding (the observable
                // HasProperty probe chain) runs BEFORE the initializer, and the
                // store targets the base resolved THEN (`var x = delete o.x`
                // re-creates `o.x`). A bare `var x;` (no init) performs no
                // assignment, so it never routes here.
                let save = self.next_reg;
                let with_objs = if decl.init.is_some() {
                    self.with_obj_regs(name)
                } else {
                    Vec::new()
                };
                if with_objs.is_empty() {
                    let tmp = self.temp();
                    let v = if let Some(init) = &decl.init {
                        self.compile_named_init(tmp, init, name)?
                    } else {
                        self.emit(Instr::LoadUndefined { dst: tmp });
                        tmp
                    };
                    if self.box_all_locals || self.cx.dyn_global_zone {
                        self.emit(Instr::StoreGlobalDyn { idx: slot, src: v });
                    } else {
                        self.emit(Instr::StoreGlobal { idx: slot, src: v });
                    }
                } else {
                    let target = self.with_resolve_target(name, &with_objs);
                    let tmp = self.temp();
                    let v =
                        self.compile_named_init(tmp, decl.init.as_ref().unwrap(), name)?;
                    self.with_store_resolved(name, target, v);
                }
                self.next_reg = save;
                continue;
            }

            // A `var` inside a function is FUNCTION-scoped, not block-scoped: bind it
            // in the function's BASE scope (scopes[0]) so it survives past its block
            // (`{ var x = 1 } x`, `for (var i…){} i`), reusing an existing binding
            // rather than duplicating. A nested closure over it boxes the slot (the
            // var name is in the capture set). A bare `var x;` never resets `x`.
            if !d.kind.is_lexical() {
                let existing =
                    self.scopes[0].iter().rev().find(|(n, _)| n == name).map(|(_, r)| *r);
                let reg = match existing {
                    Some(r) => r,
                    None => {
                        let r = self.alloc_reg();
                        self.scopes[0].push((name.to_string(), r));
                        if self.captured.contains(name) {
                            self.emit(Instr::MakeCell { reg: r });
                            self.cell_regs.insert(r);
                        }
                        r
                    }
                };
                if let Some(init) = &decl.init {
                    // `var x = init` inside a `with` whose object has `x`: the
                    // declaration is hoisted (the function-scope slot is already
                    // undefined), but the initializer assignment targets the
                    // with-object (falling back to this slot if absent).
                    // TWO-PHASE: the with-chain resolves BEFORE the initializer
                    // evaluates (observable probes; a delete in the initializer
                    // doesn't redirect the store).
                    let save = self.next_reg;
                    let with_objs = self.with_obj_regs(name);
                    if !with_objs.is_empty() {
                        let target = self.with_resolve_target(name, &with_objs);
                        let tmp = self.temp();
                        let v = self.compile_named_init(tmp, init, name)?;
                        self.with_store_resolved(name, target, v);
                        self.next_reg = save;
                    } else if self.cell_regs.contains(&reg) {
                        let tmp = self.temp();
                        let v = self.compile_named_init(tmp, init, name)?;
                        self.emit(Instr::CellSet { cell: reg, src: v });
                        self.next_reg -= 1;
                    } else {
                        let v = self.compile_named_init(reg, init, name)?;
                        if v != reg {
                            self.emit(Instr::Move { dst: reg, src: v });
                        }
                    }
                }
                continue;
            }

            // Allocate the local FIRST so `let x = x`-style self-reference and
            // ordinary declarations land in a stable register. declare_local
            // boxes the register into a cell if a nested function captures it.
            // A captured function-body-level lexical pre-created as a cell at entry
            // (so a forward-referenced function could capture it) is REUSED here
            // rather than shadowed, so the closure and this declaration share one
            // cell; otherwise a fresh binding is allocated.
            let reg = if self.scopes.len() == 1 && self.entry_lexicals.contains(name) {
                let r = self
                    .scopes[0]
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| self.declare_local(name));
                self.entry_tdz_cells.remove(&r); // TDZ ends here
                r
            } else if let Some(r) = self
                .scopes
                .last()
                .unwrap()
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, r)| *r)
                .filter(|r| self.block_tdz_cells.contains(r))
            {
                // A block-entry pre-created TDZ cell (captured forward-referenced
                // lexical): the textual declaration reuses it, ending the TDZ.
                self.block_tdz_cells.remove(&r);
                r
            } else {
                self.declare_local(name)
            };
            if is_const {
                self.const_regs.insert(reg);
            }
            self.lexical_regs.insert(reg);
            let is_cell = self.cell_regs.contains(&reg);
            // A lexical declaration's own initializer reading its binding
            // (`let x = x + 1`, `const c = c`, `using u = u`) is a TDZ
            // ReferenceError: the binding initializes only when the
            // declaration completes.
            let tdz_added = d.kind.is_lexical() && self.param_tdz.insert(name.to_string());
            if let Some(init) = &decl.init {
                if is_cell {
                    // The init value must be written THROUGH the cell.
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, name)?;
                    self.emit(Instr::CellSet { cell: reg, src: v });
                    self.next_reg -= 1; // reclaim tmp
                } else {
                    let v = self.compile_named_init(reg, init, name)?;
                    if v != reg {
                        self.emit(Instr::Move { dst: reg, src: v });
                    }
                }
            }
            if tdz_added {
                self.param_tdz.remove(name);
            }
            if decl.init.is_none() {
                if is_cell {
                    // A bare `let x;` initializes the binding to undefined, exiting its
                    // TDZ. A reused entry-precreated cell starts UNINITIALIZED, so this
                    // is where it becomes legal to read; an ordinary captured cell is
                    // already undefined, so this is a harmless re-set.
                    let t = self.temp();
                    self.emit(Instr::LoadUndefined { dst: t });
                    self.emit(Instr::CellSet { cell: reg, src: t });
                    self.next_reg -= 1;
                } else {
                    self.emit(Instr::LoadUndefined { dst: reg });
                }
            }
            // A `using`/`await using x = init` registers its resource for disposal
            // at block exit (after the binding is stored). `using_scope_reg` is set
            // by the enclosing `compile_using_block`; it is always present for such a
            // declaration (the block/body/try that contains one is wrapped).
            let using_async = match d.kind {
                ox::VariableDeclarationKind::Using => Some(false),
                ox::VariableDeclarationKind::AwaitUsing => Some(true),
                _ => None,
            };
            // `await using` is only legal where `await` is (async function /
            // module top level). Erroring here (instead of mis-compiling a
            // sync disposal) also routes a loader-compiled module entry onto
            // the direct async path via the TLA containment.
            if using_async == Some(true) && !self.in_async {
                return Err("`await` is only valid inside an async function".into());
            }
            if let (Some(is_async_using), Some(scope_reg)) = (using_async, self.using_scope_reg) {
                let src = if is_cell {
                    let t = self.temp();
                    self.emit(Instr::CellGet { dst: t, cell: reg });
                    t
                } else {
                    reg
                };
                if is_async_using {
                    self.emit(Instr::RegisterAsyncDisposable { scope: scope_reg, val: src });
                } else {
                    self.emit(Instr::RegisterDisposable { scope: scope_reg, val: src });
                }
                if is_cell {
                    self.next_reg -= 1;
                }
            }
        }
        Ok(())
    }

    /// Resolve the binding of a `for (var x in/of …)` HEAD: the loop creates
    /// NO binding of its own — it assigns the existing one (a shadowing catch
    /// parameter, the hoisted function-scope var, or the script global). A
    /// first-mention name creates the FUNCTION-scoped var binding here (the
    /// `var` hoist), exactly like `var_decl`'s function path.
    pub(crate) fn head_var_binding(&mut self, name: &str) -> Binding {
        // Innermost existing binding (catch param / pre-declared var / param).
        for scope in self.scopes.iter().rev() {
            for (n, r) in scope.iter().rev() {
                if n == name {
                    return if self.cell_regs.contains(r) {
                        Binding::LocalCell(*r)
                    } else {
                        Binding::Local(*r)
                    };
                }
            }
        }
        if self.is_script && self.cx.script_binds_globals {
            return Binding::Global(self.cx.global_slot(name) as u32);
        }
        // EVAL root with the name in the caller scope: assign through the
        // seeded caller upvalue (mirrors var_decl's eval-caller path).
        if self.is_script && self.cx.eval_caller_scope.iter().any(|n| n == name) {
            if let Some(idx) = self.resolve_upvalue(name) {
                return Binding::Upvalue(idx);
            }
        }
        // First mention: create the function-scoped var binding (scopes[0]).
        let r = self.alloc_reg();
        self.scopes[0].push((name.to_string(), r));
        if self.box_all_locals || self.captured.contains(name) {
            self.emit(Instr::MakeCell { reg: r });
            self.cell_regs.insert(r);
            Binding::LocalCell(r)
        } else {
            Binding::Local(r)
        }
    }

    /// Phase 1 of a destructuring declaration: declare every leaf binding the
    /// pattern introduces, so they occupy stable (low) registers / global slots
    /// and any captured ones are boxed before extraction writes to them.
    pub(crate) fn declare_pattern(&mut self, pat: &ox::BindingPattern) -> R<()> {
        use ox::BindingPattern as P;
        match pat {
            P::BindingIdentifier(id) => {
                if self.is_script && !self.pattern_block_local {
                    self.cx.global_slot(&id.name);
                } else if self.scopes.len() == 1 && self.entry_lexicals.contains(id.name.as_str())
                {
                    // Pre-created as a cell at entry (a captured forward-referenced
                    // lexical); reuse it so extraction and the capturing closure
                    // share one cell rather than shadowing with a fresh binding.
                    //
                    // Its TDZ ends here: the extraction that follows writes
                    // through `store_binding`, and a checked store would throw on
                    // the very cell it is initializing (`const {z} = {z:9}`).
                    if let Some(r) =
                        self.scopes[0].iter().find(|(n, _)| n == id.name.as_str()).map(|(_, r)| *r)
                    {
                        self.entry_tdz_cells.remove(&r);
                    }
                } else {
                    self.declare_local(&id.name);
                }
                Ok(())
            }
            P::AssignmentPattern(ap) => self.declare_pattern(&ap.left),
            P::ObjectPattern(op) => {
                for prop in &op.properties {
                    self.declare_pattern(&prop.value)?;
                }
                if let Some(rest) = &op.rest {
                    self.declare_pattern(&rest.argument)?;
                }
                Ok(())
            }
            P::ArrayPattern(arr) => {
                for el in arr.elements.iter().flatten() {
                    self.declare_pattern(el)?;
                }
                if let Some(rest) = &arr.rest {
                    self.declare_pattern(&rest.argument)?;
                }
                Ok(())
            }
        }
    }

    /// Phase 2: extract values from `src` (the initializer's value) into the
    /// already-declared bindings. Every temp this allocates sits above the
    /// declared locals, so callers reclaim them with a single `next_reg` reset.
    pub(crate) fn extract_pattern(&mut self, pat: &ox::BindingPattern, src: Reg) -> R<()> {
        use ox::BindingPattern as P;
        match pat {
            P::BindingIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, src);
                Ok(())
            }
            // `target = default`: `src` is our scratch temp, so patch the default
            // into it in place when it came out undefined, then bind the target.
            P::AssignmentPattern(ap) => {
                // `[x = function(){}]` ⇒ the default function takes the name "x".
                let name = match &ap.left {
                    ox::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                };
                self.apply_default_in_place_named(src, &ap.right, name.as_deref())?;
                self.extract_pattern(&ap.left, src)
            }
            P::ObjectPattern(op) => {
                // RequireObjectCoercible(src): an object pattern with NO named
                // properties (`{}` or `{...rest}`) never performs a member access,
                // so without this an empty pattern would silently accept null /
                // undefined. (A pattern WITH named properties throws via the
                // GetProp/GetIndex below.)
                if op.properties.is_empty() {
                    self.emit(Instr::CheckCoercible { src });
                }
                // With a `...rest` AND a computed sibling key, the exclusion set
                // isn't known until runtime: evaluate each sibling key once into a
                // contiguous block (reused for extraction + ObjectRestDyn).
                if op.rest.is_some() && op.properties.iter().any(|p| p.computed) {
                    let block_save = self.next_reg;
                    let keys_base = self.next_reg;
                    let n = op.properties.len() as u16;
                    for _ in 0..op.properties.len() {
                        self.alloc_reg();
                    }
                    for (i, prop) in op.properties.iter().enumerate() {
                        let kreg = keys_base + i as Reg;
                        if prop.computed {
                            let e = prop
                                .key
                                .as_expression()
                                .ok_or("unsupported computed destructuring key")?;
                            let v = self.expr_into(e, kreg)?;
                            if v != kreg {
                                self.emit(Instr::Move { dst: kreg, src: v });
                            }
                        } else {
                            let name = class_key_name(&prop.key)?;
                            let idx = self.add_string_const(&name);
                            self.emit(Instr::LoadConst { dst: kreg, idx });
                        }
                    }
                    for (i, prop) in op.properties.iter().enumerate() {
                        let save = self.next_reg;
                        let kreg = keys_base + i as Reg;
                        let val = self.alloc_reg();
                        self.emit(Instr::GetIndex { dst: val, obj: src, key: kreg });
                        self.extract_pattern(&prop.value, val)?;
                        self.next_reg = save;
                    }
                    let rest = op.rest.as_ref().unwrap();
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ObjectRestDyn { dst: val, src, keys_base, n });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                    self.next_reg = block_save;
                    return Ok(());
                }
                for prop in &op.properties {
                    let save = self.next_reg;
                    // KeyedBindingInitialization order for an IDENTIFIER leaf
                    // inside a `with`: PropertyName evaluates first, then the
                    // TARGET binding resolves (the with-chain HasProperty
                    // probes — observable via a Proxy `has` trap), then GetV
                    // reads the property, then a default. The store goes to
                    // the base resolved UP FRONT (reference-record semantics —
                    // it never re-probes the chain).
                    let leaf: Option<(String, Option<&ox::Expression>)> = match &prop.value {
                        P::BindingIdentifier(id) => Some((id.name.to_string(), None)),
                        P::AssignmentPattern(ap) => match &ap.left {
                            P::BindingIdentifier(id) => {
                                Some((id.name.to_string(), Some(&ap.right)))
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some((name, default)) = leaf {
                        let with_objs = self.with_obj_regs(&name);
                        if !with_objs.is_empty() {
                            if prop.computed {
                                // Evaluate + ToPropertyKey the key NOW, so the
                                // later GetIndex coercion is a no-op (single
                                // observable toString) and the target probes
                                // run between them, per spec.
                                let kreg = self.alloc_reg();
                                let e = prop
                                    .key
                                    .as_expression()
                                    .ok_or("unsupported computed destructuring key")?;
                                let v = self.expr_into(e, kreg)?;
                                if v != kreg {
                                    self.emit(Instr::Move { dst: kreg, src: v });
                                }
                                self.emit(Instr::ToPropKey { dst: kreg, obj: src, src: kreg });
                                let target = self.with_resolve_target(&name, &with_objs);
                                let val = self.alloc_reg();
                                self.emit(Instr::GetIndex { dst: val, obj: src, key: kreg });
                                if let Some(d) = default {
                                    self.apply_default_in_place_named(val, d, Some(&name))?;
                                }
                                self.with_store_resolved(&name, target, val);
                            } else {
                                let target = self.with_resolve_target(&name, &with_objs);
                                let val = self.alloc_reg();
                                self.extract_member(src, &prop.key, false, val)?;
                                if let Some(d) = default {
                                    self.apply_default_in_place_named(val, d, Some(&name))?;
                                }
                                self.with_store_resolved(&name, target, val);
                            }
                            self.next_reg = save;
                            continue;
                        }
                        self.next_reg = save;
                    }
                    let val = self.alloc_reg();
                    self.extract_member(src, &prop.key, prop.computed, val)?;
                    self.extract_pattern(&prop.value, val)?;
                    self.next_reg = save;
                }
                // `...rest` — a new object of `src`'s own keys minus the siblings.
                if let Some(rest) = &op.rest {
                    // Lay the excluded (sibling) names out contiguously so the op
                    // can reference them by index range.
                    let exclude_start = self.string_constants.len() as u32;
                    let mut exclude_count = 0u16;
                    for prop in &op.properties {
                        let key = class_key_name(&prop.key)
                            .map_err(|_| "object-rest with a computed sibling key is not in the subset")?;
                        self.string_name(&key);
                        exclude_count += 1;
                    }
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ObjectRest { dst: val, src, exclude_start, exclude_count });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                }
                Ok(())
            }
            P::ArrayPattern(arr) => {
                // JS array destructuring uses the iterator protocol; positional
                // GetIndex matches it for arrays/strings/Map/Set, so we only need
                // to drain a generator / custom iterable into an array first.
                let src = {
                    let norm = self.alloc_reg();
                    // Pull only as many as the fixed elements need (unbounded with
                    // a `...rest`), so destructuring an infinite iterator is fine.
                    let count =
                        if arr.rest.is_some() { u32::MAX } else { arr.elements.len() as u32 };
                    self.emit(Instr::IterToArray { dst: norm, src, count });
                    norm
                };
                for (i, el) in arr.elements.iter().enumerate() {
                    if let Some(p) = el {
                        let save = self.next_reg;
                        let val = self.alloc_reg();
                        let idx = self.alloc_reg();
                        self.emit(Instr::LoadInt { dst: idx, val: i as i32 });
                        self.emit(Instr::GetIndex { dst: val, obj: src, key: idx });
                        self.extract_pattern(p, val)?;
                        self.next_reg = save;
                    }
                    // a hole (`[, x]`) binds nothing
                }
                if let Some(rest) = &arr.rest {
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ArrayRest { dst: val, src, start: arr.elements.len() as u32 });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                }
                Ok(())
            }
        }
    }

    /// Read `obj[key]` into `dst` for a destructuring property. A static key
    /// (identifier / string / number) uses GetProp; a computed `[expr]` key is
    /// evaluated and read with GetIndex.
    pub(crate) fn extract_member(
        &mut self,
        obj: Reg,
        key: &ox::PropertyKey,
        computed: bool,
        dst: Reg,
    ) -> R<()> {
        if computed {
            let e = key
                .as_expression()
                .ok_or("unsupported computed destructuring key")?;
            let save = self.next_reg; // `dst` was allocated below this
            let k = self.expr(e)?;
            self.emit(Instr::GetIndex { dst, obj, key: k });
            self.next_reg = save; // reclaim the key-expression temps
            return Ok(());
        }
        let name = match key {
            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            ox::PropertyKey::StringLiteral(s) => string_literal_key(s),
            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                ox::PropertyKey::BigIntLiteral(b) => b.value.to_string(),
            _ => return Err("unsupported destructuring property key".into()),
        };
        let nidx = self.string_name(&name);
        self.emit(Instr::GetProp { dst, obj, name: nidx });
        Ok(())
    }

    /// `if (reg === undefined) reg = default` — apply a destructuring/parameter
    /// default to a scratch register in place.
    pub(crate) fn apply_default_in_place(&mut self, reg: Reg, default: &ox::Expression) -> R<()> {
        self.apply_default_in_place_named(reg, default, None)
    }

    /// As `apply_default_in_place`, but when the default fills a single named
    /// binding (`[x = function(){}]` ⇒ `x.name === "x"`), infer that name for an
    /// anonymous function/class default (NamedEvaluation).
    pub(crate) fn apply_default_in_place_named(
        &mut self,
        reg: Reg,
        default: &ox::Expression,
        name: Option<&str>,
    ) -> R<()> {
        let save = self.next_reg;
        let undef = self.alloc_reg();
        self.emit(Instr::LoadUndefined { dst: undef });
        let cond = self.alloc_reg();
        self.emit(Instr::Eq { dst: cond, a: reg, b: undef });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // skip default when defined
        let dv = match name {
            Some(n) => self.compile_named_init(reg, default, n)?,
            None => self.expr_into(default, reg)?,
        };
        if dv != reg {
            self.emit(Instr::Move { dst: reg, src: dv });
        }
        let end = self.here();
        self.patch_jump(jf, end);
        self.next_reg = save;
        Ok(())
    }

}
