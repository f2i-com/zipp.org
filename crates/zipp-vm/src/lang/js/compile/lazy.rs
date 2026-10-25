//! Deferred compilation of function bodies.
//!
//! Most of a large bundle never runs. Of the functions a run of the
//! TypeScript compiler's bundle compiles, the ones it ever enters hold 26% of
//! the instructions (Vue SSR's 31%, Prettier's 52%), and compiling is 11-19%
//! of those runs. A deferred body is compiled after the rest, from the AST and
//! a snapshot of the context it was met in. This module decides which bodies
//! may wait, settles at the deferral point what their compile would have
//! changed in their ancestors (the upvalues forwarded down to them), and
//! compiles the deferred bodies.
//!
//! `ZIPP_LAZY` selects the mode for the process ([`lazy_mode`]):
//! - unset: every body compiles where it is met;
//! - `seeded`: every body compiles where it is met, but each nested function
//!   starts with its free names resolved, in name order, the way a deferred
//!   body must: the reference a deferred compile is compared against;
//! - `end`: eligible bodies are deferred and compiled at the end of the same
//!   compile;
//! - `verify`: both, compared; every difference goes to stderr and the `end`
//!   program is the one returned.
#![allow(unused_imports)]
use super::*;
use crate::parse::visit::{self, Node};

/// How this compile treats nested function bodies (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LazyMode {
    Off,
    Seeded,
    End,
    Verify,
}

/// `ZIPP_LAZY`, latched for the process.
pub(crate) fn lazy_mode() -> LazyMode {
    use std::sync::atomic::{AtomicU8, Ordering};
    static MODE: AtomicU8 = AtomicU8::new(u8::MAX);
    let mut m = MODE.load(Ordering::Relaxed);
    if m == u8::MAX {
        m = match std::env::var("ZIPP_LAZY").as_deref() {
            Ok("seeded") => 1,
            Ok("end") => 2,
            Ok("verify") => 3,
            _ => 0,
        };
        MODE.store(m, Ordering::Relaxed);
    }
    match m {
        1 => LazyMode::Seeded,
        2 => LazyMode::End,
        3 => LazyMode::Verify,
        _ => LazyMode::Off,
    }
}

/// Which compile path a deferred body takes, with what that path was given.
pub(super) enum LazyKind {
    /// `compile_func_expr`.
    FuncExpr { name: Option<String>, self_name: Option<String> },
    /// `func_decl_inner`.
    FuncDecl { name: Option<String> },
    /// `compile_arrow`.
    Arrow,
}

/// The compiler-wide context a body is compiled in, as the deferral point saw
/// it. Restored before the deferred body compiles.
#[derive(Clone, Copy)]
pub(super) struct SavedCx {
    in_strict: bool,
    in_field_init: bool,
    in_derived_ctor: bool,
    new_target_ok: bool,
    force_new_target_ok: bool,
    fn_ctor_no_self_name: bool,
    strict_expr_region: u32,
    class_derived: bool,
    compiling_ctor: bool,
    obj_method_super: bool,
    dyn_global_zone: bool,
    eval_fn_context: bool,
}

impl SavedCx {
    fn of(cx: &Compiler) -> SavedCx {
        SavedCx {
            in_strict: cx.in_strict,
            in_field_init: cx.in_field_init,
            in_derived_ctor: cx.in_derived_ctor,
            new_target_ok: cx.new_target_ok,
            force_new_target_ok: cx.force_new_target_ok,
            fn_ctor_no_self_name: cx.fn_ctor_no_self_name,
            strict_expr_region: cx.strict_expr_region,
            class_derived: cx.class_derived,
            compiling_ctor: cx.compiling_ctor,
            obj_method_super: cx.obj_method_super,
            dyn_global_zone: cx.dyn_global_zone,
            eval_fn_context: cx.eval_fn_context,
        }
    }

    fn restore(self, cx: &mut Compiler) {
        cx.in_strict = self.in_strict;
        cx.in_field_init = self.in_field_init;
        cx.in_derived_ctor = self.in_derived_ctor;
        cx.new_target_ok = self.new_target_ok;
        cx.force_new_target_ok = self.force_new_target_ok;
        cx.fn_ctor_no_self_name = self.fn_ctor_no_self_name;
        cx.strict_expr_region = self.strict_expr_region;
        cx.class_derived = self.class_derived;
        cx.compiling_ctor = self.compiling_ctor;
        cx.obj_method_super = self.obj_method_super;
        cx.dyn_global_zone = self.dyn_global_zone;
        cx.eval_fn_context = self.eval_fn_context;
    }
}

/// One deferred body.
pub(super) struct LazyJob {
    /// The id its stub holds in `Compiler::functions`.
    func_id: u32,
    /// The function node's source range: how the body is found again.
    span: (u32, u32),
    kind: LazyKind,
    captured: HashSet<String>,
    enclosing: Vec<EnclosingFn>,
    /// Its upvalues, resolved at the deferral point.
    seed: Vec<(String, UpvalSource)>,
    cx: SavedCx,
}

/// Names a strict body may not bind or assign, `eval` and `arguments` (whose
/// uses carry compile-time rules of their own), and the contextual keywords:
/// a deferred subtree mentioning any of them stays eager, so no compile-time
/// early error about them moves to a function's first call.
fn restricted_name(n: &str) -> bool {
    matches!(
        n,
        "eval"
            | "arguments"
            | "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
            | "await"
    )
}

/// Whether a parameter list names one binding twice (a strict-mode early
/// error the compiler raises).
fn duplicate_params(p: &ast::Params) -> bool {
    let mut seen = rustc_hash::FxHashSet::default();
    let mut dup = false;
    visit::params(p, &mut |n| {
        if let Node::Pattern(ast::Pattern::Ident(name)) = n {
            if !seen.insert(&**name) {
                dup = true;
            }
        }
        !dup
    });
    dup
}

/// The scan a deferred subtree must pass: nothing in it, nested functions
/// included, may need a compile-time decision that depends on more than the
/// saved context, nor raise a compile-time error (those must not wait for a
/// first call: a never-called function's early error still stops the
/// program). Over-cautious by design: a rejection only keeps a body eager.
fn subtree_node_ok(n: Node<'_>) -> bool {
    use ast::{Expr as E, Stmt as S};
    match n {
        Node::Function(f) => !f.is_async && !f.is_generator && !duplicate_params(&f.params),
        Node::Arrow(a) => !a.is_async,
        Node::Class(_) => false,
        Node::Stmt(s) => !matches!(
            s,
            S::With { .. } | S::Import(_) | S::Export(_) | S::ClassDecl(_) | S::ForOf { is_await: true, .. }
        ),
        Node::Expr(e) => match e {
            E::Regex { .. }
            | E::Super
            | E::NewTarget
            | E::ImportMeta
            | E::ImportCall { .. }
            | E::Yield { .. }
            | E::Await(_)
            | E::PrivateIn { .. }
            | E::Class(_) => false,
            E::Ident(name) => !restricted_name(name),
            E::Unary { op: ast::UnaryOp::Delete, arg } => !matches!(**arg, E::Ident(_)),
            E::Member(m) => !matches!(m.prop, ast::MemberProp::Private(_)),
            _ => true,
        },
        Node::Pattern(p) => match p {
            ast::Pattern::Ident(name) => !restricted_name(name),
            ast::Pattern::Object { props, .. } => props.len() < 1000,
            _ => true,
        },
        Node::Target(t) => match t {
            ast::Target::Ident { name, .. } => !restricted_name(name),
            ast::Target::Member(m) => !matches!(m.prop, ast::MemberProp::Private(_)),
            ast::Target::Object { props, .. } => props.len() < 1000,
            _ => true,
        },
        Node::PropKey(k) => !matches!(k, ast::PropKey::Private(_)),
    }
}

impl FnCompiler<'_> {
    /// Whether a nested function met here may be deferred: nothing in force
    /// at this point that the saved context ([`SavedCx`]) does not carry.
    fn lazy_context_ok(&self) -> bool {
        let cx = &*self.cx;
        !cx.eval_mode
            && !cx.module_mode
            && !cx.eval_locals
            && !cx.dyn_global_zone
            && !cx.eval_fn_context
            && !cx.force_new_target_ok
            && cx.eval_inherit_super.is_none()
            && cx.eval_caller_scope.is_empty()
            && cx.eval_outer_scope.is_empty()
            && cx.eval_catch_params.is_empty()
            && cx.eval_dynamic_names.is_empty()
            && cx.eval_visible_privates.is_empty()
            && !cx.in_field_init
            && !cx.in_derived_ctor
            && !cx.class_derived
            && !cx.compiling_ctor
            && !cx.obj_method_super
            && !cx.fn_ctor_no_self_name
            && cx.strict_expr_region == 0
            && cx.class_names.is_empty()
            && cx.heritage_classes.is_empty()
            && cx.private_names_stack.is_empty()
            && cx.class_enclosing.is_empty()
            && cx.pending_with_shadows.is_empty()
            && self.with_stack.is_empty()
            && self.inherited_with_shadows.is_empty()
            && self.super_class.is_none()
            && !self.super_home_obj
            && !self.super_static
            && !self.derived_class
            && !self.in_derived_ctor
            && self.heritage_class.is_none()
            && self.this_override.is_none()
            && !self.box_all_locals
    }

    /// Prepare the compile of a nested function's body. With the mode off this
    /// does nothing. Otherwise the body's free names are resolved now, in name
    /// order, against `enclosing` (forwarding any upvalue its ancestors must
    /// pass down, exactly as its compile would), and either the body is
    /// deferred, giving back the stub that stands for it until it compiles,
    /// or the compile about to run is seeded with those upvalues.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lazy_prepare(
        &mut self,
        kind: LazyKind,
        free: Vec<String>,
        deferrable: bool,
        span: (u32, u32),
        stub_name: &str,
        is_arrow: bool,
        captured: &HashSet<String>,
        enclosing: &[EnclosingFn],
    ) -> Option<FuncProto> {
        let mode = self.cx.lazy_mode;
        if mode == LazyMode::Off {
            return None;
        }
        let own_name = match &kind {
            LazyKind::FuncExpr { self_name, .. } => self_name.clone(),
            _ => None,
        };
        let mut seed = Vec::new();
        for n in free {
            // A named function expression's own name resolves to the function.
            if own_name.as_deref() == Some(n.as_str()) {
                continue;
            }
            if let Some(src) = capture_source(enclosing, &n) {
                seed.push((n, src));
            }
        }
        if mode != LazyMode::End || !deferrable || !self.lazy_context_ok() {
            self.cx.lazy_seed = Some(seed);
            return None;
        }
        let stub = FuncProto {
            name: stub_name.to_string(),
            code: Vec::new(),
            reg_count: 0,
            param_count: 0,
            length: 0,
            rest_reg: None,
            arguments_reg: None,
            is_generator: false,
            is_async: false,
            non_constructable: is_arrow,
            lexical_this: is_arrow,
            super_static: false,
            is_strict: self.cx.in_strict,
            simple_params: false,
            constants: Vec::new(),
            string_constants: Vec::new(),
            static_key_plans: Vec::new(),
            bigint_consts: Vec::new(),
            wtf8_consts: Vec::new(),
            name_global: None,
            upvalues: seed.iter().map(|(_, s)| *s).collect(),
            eval_sites: Vec::new(),
            source: SourceText::default(),
            fused: Default::default(),
            call_sites: Box::new([]),
        };
        self.cx.lazy_jobs.push_back(LazyJob {
            func_id: self.cx.functions.len() as u32,
            span,
            kind,
            captured: captured.clone(),
            enclosing: enclosing.to_vec(),
            seed,
            cx: SavedCx::of(self.cx),
        });
        Some(stub)
    }
}

/// Whether function `f`'s body may be deferred, as far as its own subtree goes.
pub(super) fn fn_subtree_ok(f: &ast::Function) -> bool {
    visit::function(f, &mut subtree_node_ok)
}

/// [`fn_subtree_ok`] for an arrow.
pub(super) fn arrow_subtree_ok(a: &ast::Arrow) -> bool {
    visit::arrow(a, &mut subtree_node_ok)
}

/// A function or arrow node of the program being compiled.
#[derive(Clone, Copy)]
enum FnNode<'a> {
    Function(&'a ast::Function),
    Arrow(&'a ast::Arrow),
}

impl Compiler {
    /// Compile every deferred body (and the bodies they defer in turn), each
    /// into the id its stub holds.
    pub(super) fn drain_lazy(&mut self, prog: &ast::Program) -> R<()> {
        if self.lazy_jobs.is_empty() {
            return Ok(());
        }
        let mut nodes: rustc_hash::FxHashMap<(u32, u32), FnNode<'_>> = rustc_hash::FxHashMap::default();
        visit::stmts(&prog.body, &mut |n| {
            match n {
                Node::Function(f) => {
                    nodes.insert((f.span.start, f.span.end), FnNode::Function(f));
                }
                Node::Arrow(a) => {
                    nodes.insert((a.span.start, a.span.end), FnNode::Arrow(a));
                }
                _ => {}
            }
            true
        });
        while let Some(job) = self.lazy_jobs.pop_front() {
            self.lazy_deferred += 1;
            let node = *nodes
                .get(&job.span)
                .ok_or_else(|| format!("internal: no function at {:?} for a deferred body", job.span))?;
            job.cx.restore(self);
            self.lazy_seed = Some(job.seed);
            self.lazy_freeze = true;
            let (mut proto, start) = match (job.kind, node) {
                (LazyKind::FuncExpr { name, self_name }, FnNode::Function(f)) => {
                    let (params, rest, body) = function_parts(f)?;
                    let p = self.compile_function_body(
                        name.as_deref(),
                        self_name.as_deref(),
                        &params,
                        rest.as_deref(),
                        Some(&f.params),
                        body,
                        fn_directives(f),
                        false,
                        f.is_generator,
                        f.is_async,
                        job.captured,
                        job.enclosing,
                    )?;
                    (p, f.span.start)
                }
                (LazyKind::FuncDecl { name }, FnNode::Function(f)) => {
                    let (params, rest, body) = function_parts(f)?;
                    let p = self.compile_function_body(
                        name.as_deref(),
                        None,
                        &params,
                        rest.as_deref(),
                        Some(&f.params),
                        body,
                        fn_directives(f),
                        false,
                        f.is_generator,
                        f.is_async,
                        job.captured,
                        job.enclosing,
                    )?;
                    (p, f.span.start)
                }
                (LazyKind::Arrow, FnNode::Arrow(a)) => {
                    let params = param_slot_names(&a.params)?;
                    let rest = rest_name(&a.params)?;
                    let p = self.compile_arrow_body(
                        &params,
                        rest.as_deref(),
                        a,
                        job.captured,
                        job.enclosing,
                        None,
                        false,
                        false,
                        false,
                        false,
                    )?;
                    (p, a.span.start)
                }
                _ => return Err(format!("internal: a deferred body at {:?} changed kind", job.span)),
            };
            self.lazy_freeze = false;
            // What the callers set on the stub after their compile call.
            let stub = &self.functions[job.func_id as usize];
            proto.name = stub.name.clone();
            proto.source = stub.source.clone();
            proto.name_global = stub.name_global;
            proto.non_constructable |= stub.non_constructable;
            self.mark_preamble_fn(&mut proto, start);
            self.functions[job.func_id as usize] = proto;
        }
        Ok(())
    }
}

/// Every difference between `a` (an eager, seeded compile) and `b` (the same
/// program with bodies deferred), function by function. Functions pair by
/// source range; `b`'s function ids and global slots are read through what
/// they name (`a`'s id of the same range, `a`'s slot of the same name), since
/// deferral changes the order both are handed out in.
pub(super) fn diff_programs(a: &Program, b: &Program, misses: &[String]) -> Vec<String> {
    let mut out: Vec<String> = misses.iter().map(|n| format!("upvalue resolved after deferral: {n}")).collect();
    let span = |f: &FuncProto| {
        let (_, s, e) = f.source.parts();
        (s, e)
    };
    // A function without a source range of its own (a class's field
    // initializer thunk) shares its key with the others: pair those by order,
    // the nth such in `a` with the nth in `b`. Classes are never deferred, so
    // both compiles meet them in the same order.
    let keys = |p: &Program| -> Vec<((usize, usize), usize)> {
        let mut seen: rustc_hash::FxHashMap<(usize, usize), usize> = rustc_hash::FxHashMap::default();
        p.functions
            .iter()
            .map(|f| {
                let k = span(f);
                let n = seen.entry(k).or_insert(0);
                *n += 1;
                (k, *n)
            })
            .collect()
    };
    let (a_keys, b_keys) = (keys(a), keys(b));
    let a_by_key: rustc_hash::FxHashMap<((usize, usize), usize), u32> =
        a_keys.iter().enumerate().skip(1).map(|(i, k)| (*k, i as u32)).collect();
    let a_slot: rustc_hash::FxHashMap<&str, u32> =
        a.global_names.iter().enumerate().map(|(i, n)| (n.as_str(), i as u32)).collect();
    let fmap = |id: u32| -> u32 {
        if id == 0 {
            return 0;
        }
        b_keys
            .get(id as usize)
            .and_then(|k| a_by_key.get(k).copied())
            .unwrap_or(u32::MAX - 1)
    };
    let gmap = |slot: u32| -> u32 {
        b.global_names
            .get(slot as usize)
            .and_then(|n| a_slot.get(n.as_str()).copied())
            .unwrap_or(u32::MAX - 1)
    };
    if a.functions.len() != b.functions.len() {
        out.push(format!("function count {} vs {}", a.functions.len(), b.functions.len()));
    }
    for (bi, bf) in b.functions.iter().enumerate() {
        let ai = if bi == 0 { 0 } else { fmap(bi as u32) };
        let Some(af) = a.functions.get(ai as usize) else {
            out.push(format!("deferred-compile function {bi} ({}) has no eager twin", bf.name));
            continue;
        };
        let who = format!("{} @{:?}", af.name, span(af));
        let shape = |f: &FuncProto, g: &dyn Fn(u32) -> u32| {
            format!(
                "regs {} params {} len {} rest {:?} args {:?} gen {} async {} nc {} lt {} ss {} strict {} simple {} ng {:?} up {:?} consts {} strs {:?} plans {} big {} wtf8 {:?} eval {}",
                f.reg_count,
                f.param_count,
                f.length,
                f.rest_reg,
                f.arguments_reg,
                f.is_generator,
                f.is_async,
                f.non_constructable,
                f.lexical_this,
                f.super_static,
                f.is_strict,
                f.simple_params,
                f.name_global.map(g),
                f.upvalues,
                f.constants.len(),
                f.string_constants,
                f.static_key_plans.len(),
                f.bigint_consts.len(),
                f.wtf8_consts,
                f.eval_sites.len(),
            )
        };
        let sa = shape(af, &|s| s);
        let sb = shape(bf, &gmap);
        if sa != sb {
            out.push(format!("{who}: shape\n  eager:    {sa}\n  deferred: {sb}"));
        }
        if af.code.len() != bf.code.len() {
            out.push(format!("{who}: {} instructions vs {}", af.code.len(), bf.code.len()));
            continue;
        }
        for (ip, (ia, ib)) in af.code.iter().zip(&bf.code).enumerate() {
            let tb = format!("{:?}", translate(ib, &fmap, &gmap));
            let ta = format!("{ia:?}");
            if ta != tb {
                out.push(format!("{who} @{ip}: {ta} vs {tb}"));
                break;
            }
        }
    }
    out
}

/// `ins` with its function ids and global slots read through `f` and `g`.
fn translate(ins: &Instr, f: &dyn Fn(u32) -> u32, g: &dyn Fn(u32) -> u32) -> Instr {
    let mut ins = *ins;
    match &mut ins {
        Instr::MakeFunc { func_id, .. } | Instr::MakeClosure { func_id, .. } | Instr::MakeArrow { func_id, .. } => {
            *func_id = f(*func_id)
        }
        Instr::ClassAddMember { func, .. } => *func = f(*func),
        Instr::LoadGlobal { idx, .. }
        | Instr::LoadGlobalOrUndefined { idx, .. }
        | Instr::StoreGlobal { idx, .. }
        | Instr::StoreGlobalStrict { idx, .. }
        | Instr::StoreGlobalResolved { idx, .. }
        | Instr::LoadGlobalDyn { idx, .. }
        | Instr::LoadGlobalOrUndefinedDyn { idx, .. }
        | Instr::StoreGlobalDyn { idx, .. }
        | Instr::EvalScopeHas { idx, .. }
        | Instr::EvalScopeSet { idx, .. } => *idx = g(*idx),
        Instr::DeleteGlobal { slot, .. } => *slot = g(*slot),
        Instr::LoadUpvalDyn { name, .. } | Instr::StoreUpvalDyn { name, .. } => *name = g(*name),
        Instr::MathOp {
            callee: crate::bytecode::NO_REG,
            this_v,
            ..
        } if *this_v < crate::bytecode::BARE_MATH_BY_NAME => {
            *this_v = u16::try_from(g(u32::from(*this_v))).unwrap_or(u16::MAX)
        }
        _ => {}
    }
    ins
}

#[cfg(test)]
mod lazy_tests {
    use super::*;

    /// `src` compiled seeded and with bodies deferred: the differences, and
    /// how many bodies the deferred compile deferred.
    fn both(src: &str) -> (Vec<String>, usize) {
        let prog = crate::front::parse_script(src).expect("parses");
        let (a, _) = super::super::entry::compile_program_mode(&prog, src, false, true, false, None, LazyMode::Seeded)
            .expect("seeded compile");
        let (b, misses) = super::super::entry::compile_program_mode(&prog, src, false, true, false, None, LazyMode::End)
            .expect("deferred compile");
        (diff_programs(&a, &b, &misses), super::super::entry::last_deferred())
    }

    /// Closures over every kind of enclosing binding compile the same whether
    /// their bodies wait or not: captures several levels up (which the
    /// functions between must forward), loop-iteration `let` cells, a named
    /// function expression calling itself, defaults reading outer names,
    /// nested declarations, arrows, and functions deferred inside deferred ones.
    #[test]
    fn deferred_bodies_compile_as_seeded_ones() {
        let sources = [
            "var g = 1; function outer(a) { var b = a + g; function mid() { return function inner(c) { return a + b + c + g; }; } return mid; }",
            "function f() { let fs = []; for (let i = 0; i < 3; i++) { fs.push(() => i); } return fs; }",
            "var fact = function me(n) { return n < 2 ? 1 : n * me(n - 1); };",
            "function d(x, y = x + 1, z = () => x * y) { return z(); } function e() { return d(1); }",
            "(() => { var cache = {}; function get(k) { return cache[k]; } function put(k, v) { cache[k] = v; return get(k); } this.api = { get, put }; })();",
            "function a() { var x = 1; { let y = 2; var h = function () { return x + y; }; } return h; }",
            "function outer() { var t = 0; return { inc: function () { t++; return t; }, get: () => t }; }",
            "implicitGlobal = 3; function readIt() { return implicitGlobal + undeclaredYet; }",
        ];
        for src in sources {
            let (diffs, deferred) = both(src);
            assert!(diffs.is_empty(), "{src}\n{}", diffs.join("\n"));
            assert!(deferred > 0, "nothing was deferred in: {src}");
        }
    }

    /// A body whose subtree holds a construct with a compile-time rule of its
    /// own stays eager, and so does everything that contains it.
    #[test]
    fn early_error_constructs_keep_bodies_eager() {
        for src in [
            "function f() { return /a+/.test('aa'); }",
            "function f() { return function g() { return arguments.length; }; }",
            "function f() { with ({}) {} }",
            "function f() { class C {} return C; }",
            "function f() { return eval('1'); }",
            "function* f() { yield 1; }",
            "async function f() { await 1; }",
        ] {
            let (diffs, deferred) = both(src);
            assert!(diffs.is_empty(), "{src}\n{}", diffs.join("\n"));
            assert_eq!(deferred, 0, "deferred a body it must not: {src}");
        }
    }
}