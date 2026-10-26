//! Deferred compilation of function bodies.
//!
//! Most of a large bundle never runs. Of the functions a run of the
//! TypeScript compiler's bundle compiles, the ones it ever enters hold 26% of
//! the instructions (Vue SSR's 31%, Prettier's 52%), and compiling is 11-19%
//! of those runs. A deferred body is compiled later, from the AST and a
//! snapshot of the context it was met in: at its function's first call when
//! the program runs ([`LazySession`]), or at the end of the same compile in
//! the modes that check the machinery. This module decides which bodies may
//! wait, settles at the deferral point what their compile would have changed
//! in their ancestors (the upvalues forwarded down to them, the function ids
//! of the functions nested in them), and compiles the deferred bodies.
//!
//! `ZIPP_LAZY` selects the mode for the process:
//! - unset, or `run`: a main program of [`LAZY_MIN_FUNCTIONS`] or more
//!   functions (which runs in the interpreter alone) compiles each deferred
//!   body at its first call; every other compile compiles every body where
//!   it is met;
//! - `run-all`: every main program defers, its JIT off;
//! - `off`: every body compiles where it is met;
//! - `seeded`: every body compiles where it is met, but each nested function
//!   starts with its free names resolved, in name order, the way a deferred
//!   body must: the reference a deferred compile is compared against;
//! - `end`: eligible bodies are deferred and compiled at the end of the same
//!   compile;
//! - `verify`: both, compared; every difference goes to stderr and the `end`
//!   program is the one returned. A main script also compiles the way a run
//!   does, then every stub the way first calls would, compared too.
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
    /// Deferred bodies wait for their first call: each stub is runnable, and
    /// the functions nested in it have their ids reserved in front of it.
    Run,
}

/// Which programs compile their deferred bodies at first call (`ZIPP_LAZY`'s
/// `run` and `run-all`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RunLazy {
    Off,
    /// Programs the interpreter runs alone.
    Large,
    /// Every program, with its JIT off.
    All,
}

/// `ZIPP_LAZY`, latched for the process: 1-3 the compile modes, 4-5 the
/// [`RunLazy`] ones (4, `run`, unless it says otherwise), 6 none.
fn lazy_setting() -> u8 {
    use std::sync::atomic::{AtomicU8, Ordering};
    static MODE: AtomicU8 = AtomicU8::new(u8::MAX);
    let mut m = MODE.load(Ordering::Relaxed);
    if m == u8::MAX {
        m = match std::env::var("ZIPP_LAZY").as_deref() {
            Ok("seeded") => 1,
            Ok("end") => 2,
            Ok("verify") => 3,
            Ok("run-all") => 5,
            Ok("off") => 6,
            // `run`, and the default.
            _ => 4,
        };
        MODE.store(m, Ordering::Relaxed);
    }
    m
}

/// The mode every compile runs in (`run` and `run-all` leave it off: they
/// apply to the main program alone, through [`compile_main_program_lazy`]).
pub(crate) fn lazy_mode() -> LazyMode {
    match lazy_setting() {
        1 => LazyMode::Seeded,
        2 => LazyMode::End,
        3 => LazyMode::Verify,
        _ => LazyMode::Off,
    }
}

/// A main program with at least this many functions defers its bodies to
/// their first call by default: the size at which a program runs in the
/// interpreter alone (`vm::engine::boot`'s `JIT_LARGE_PROGRAM_FUNCS`). Below
/// it, compiling is a sliver of a run.
pub(crate) const LAZY_MIN_FUNCTIONS: usize = 256;

/// Which main programs defer their bodies to their first call.
pub(crate) fn run_lazy() -> RunLazy {
    match lazy_setting() {
        4 => RunLazy::Large,
        5 => RunLazy::All,
        _ => RunLazy::Off,
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
    /// In [`LazyMode::Run`], the ids reserved in front of the stub for the
    /// functions nested in the body (as many as its subtree holds), which
    /// its compile hands out in the order an eager compile would.
    reserved: u32,
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
        (!dup).into()
    });
    dup
}

/// The scan a deferred subtree must pass, node by node: nothing in it,
/// nested functions included, may need a compile-time decision that depends
/// on more than the saved context, nor raise a compile-time error (those must
/// not wait for a first call: a never-called function's early error still
/// stops the program). Over-cautious by design: a rejection only keeps a body
/// eager. What in `n` keeps the subtree eager, named for verify mode's census.
fn subtree_node_block(n: Node<'_>) -> Option<&'static str> {
    use ast::{Expr as E, Stmt as S};
    let why = match n {
        Node::Function(f) if f.is_async => "async function",
        Node::Function(f) if f.is_generator => "generator",
        Node::Function(f) if duplicate_params(&f.params) => "duplicate parameters",
        Node::Arrow(a) if a.is_async => "async arrow",
        Node::Class(_) => "class",
        Node::Stmt(S::With { .. }) => "with",
        Node::Stmt(S::Import(_) | S::Export(_)) => "import/export",
        Node::Stmt(S::ClassDecl(_)) => "class",
        Node::Stmt(S::ForOf { is_await: true, .. }) => "for await",
        Node::Expr(e) => match e {
            // A literal's early error is checked before a compile that defers
            // (`compile_main_program_lazy`), or in it (`drain_lazy`).
            E::Regex { .. } => return None,
            E::Super => "super",
            E::NewTarget => "new.target",
            E::ImportMeta | E::ImportCall { .. } => "import()",
            E::Yield { .. } => "yield",
            E::Await(_) => "await",
            E::PrivateIn { .. } => "private name",
            E::Class(_) => "class",
            E::Ident(name) if restricted_name(name) => restricted_why(name),
            E::Unary { op: ast::UnaryOp::Delete, arg } if matches!(**arg, E::Ident(_)) => "delete identifier",
            E::Member(m) if matches!(m.prop, ast::MemberProp::Private(_)) => "private name",
            _ => return None,
        },
        Node::Pattern(p) => match p {
            ast::Pattern::Ident(name) if restricted_name(name) => restricted_why(name),
            ast::Pattern::Object { props, .. } if props.len() >= 1000 => "huge object pattern",
            _ => return None,
        },
        Node::Target(t) => match t {
            ast::Target::Ident { name, .. } if restricted_name(name) => restricted_why(name),
            ast::Target::Member(m) if matches!(m.prop, ast::MemberProp::Private(_)) => "private name",
            ast::Target::Object { props, .. } if props.len() >= 1000 => "huge object pattern",
            _ => return None,
        },
        Node::PropKey(ast::PropKey::Private(_)) => "private name",
        _ => return None,
    };
    Some(why)
}

/// [`subtree_node_block`]'s name for restricted identifier `name`.
fn restricted_why(name: &str) -> &'static str {
    match name {
        "arguments" => "arguments",
        "eval" => "eval",
        _ => "reserved identifier",
    }
}

impl FnCompiler<'_> {
    /// Whether a nested function met here may be deferred: nothing in force
    /// at this point that the saved context ([`SavedCx`]) does not carry.
    /// What this function's own compiler knows of `super`, a derived class
    /// and an overridden `this` reaches an arrow only (`compile_arrow_body`
    /// is handed it; `compile_function_body`, which an ordinary function's
    /// body compiles through, reads none of it), so it holds back arrows
    /// alone: the functions a bundler nests in an object-literal method, its
    /// module wrapper, are deferred as any others.
    fn lazy_context_ok(&self, is_arrow: bool) -> bool {
        let cx = &*self.cx;
        let fn_context_ok = !is_arrow
            || (self.super_class.is_none()
                && !self.super_home_obj
                && !self.super_static
                && !self.derived_class
                && !self.in_derived_ctor
                && self.heritage_class.is_none()
                && self.this_override.is_none());
        fn_context_ok
            && !cx.eval_mode
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
            && !self.box_all_locals
    }

    /// Prepare the compile of a nested function's body. With the mode off this
    /// does nothing. Otherwise the body's free names are resolved now, in name
    /// order, against `enclosing` (forwarding any upvalue its ancestors must
    /// pass down, exactly as its compile would), and either the body is
    /// deferred, giving back the stub that stands for it until it compiles,
    /// or the compile about to run is seeded with those upvalues. `nested`
    /// is what [`Compiler::lazy_scan_fn`] (or `lazy_scan_arrow`) found:
    /// `None` when the body may not wait.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lazy_prepare(
        &mut self,
        kind: LazyKind,
        free: Vec<String>,
        nested: Option<u32>,
        span: (u32, u32),
        stub_name: &str,
        is_arrow: bool,
        params: &ast::Params,
        own_strict: bool,
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
        // A stub a call runs must set its frame up as the body will want it.
        // `setup_call` reads only the frame's layout: the formal parameters'
        // registers (defaults and patterns are the body's own prologue), the
        // rest array right after them, and no `arguments` object, which no
        // deferred function's own scope reads; so the stub's counts are the
        // body's.
        let run = mode == LazyMode::Run;
        let has_rest = matches!(params.items.last(), Some(ast::Pattern::Rest(_)));
        let formals = params.items.len() - has_rest as usize;
        let defer = match mode {
            LazyMode::End => nested.is_some(),
            LazyMode::Run => nested.is_some() && formals < 0xFF,
            _ => false,
        };
        if !defer || !self.lazy_context_ok(is_arrow) {
            if run && nested.is_some() {
                let bytes = span.1 - span.0;
                note_eager(Some(if defer { "context" } else { "parameters" }), bytes);
            }
            self.cx.lazy_seed = Some(seed);
            return None;
        }
        let reserved = if run { nested.unwrap_or(0) } else { 0 };
        for _ in 0..reserved {
            self.cx.functions.push(placeholder(""));
        }
        let params_n = if run { formals as u16 } else { 0 };
        let rest_reg = (run && has_rest).then_some(params_n + 1);
        let stub = FuncProto {
            name: stub_name.to_string(),
            code: if run { vec![Instr::LazyCompile] } else { Vec::new() },
            reg_count: if run { params_n + 1 + has_rest as u16 } else { 0 },
            param_count: params_n,
            length: if run { expected_arg_count(params) } else { 0 },
            rest_reg,
            arguments_reg: None,
            is_generator: false,
            is_async: false,
            non_constructable: is_arrow,
            lexical_this: is_arrow,
            super_static: false,
            is_strict: self.cx.in_strict || own_strict,
            simple_params: run && params.simple,
            constants: Vec::new(),
            string_constants: Vec::new(),
            static_key_plans: Vec::new(),
            bigint_consts: Vec::new(),
            wtf8_consts: Vec::new(),
            name_global: None,
            upvalues: seed.iter().map(|(_, s)| *s).collect(),
            eval_sites: Vec::new(),
            eval_calls: Box::default(),
            source: SourceText::default(),
            fused: Default::default(),
            call_sites: Box::new([]),
        };
        self.cx.lazy_jobs.push_back(LazyJob {
            func_id: self.cx.functions.len() as u32,
            reserved,
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

/// What the eligibility scan found in one function's subtree (its own node,
/// and every node and function nested in it).
#[derive(Clone, Copy)]
pub(super) struct ScanFacts {
    /// Nothing in the subtree keeps it eager ([`subtree_node_block`]).
    ok: bool,
    /// What does, when something does.
    why: Option<&'static str>,
    /// The functions and arrows nested in it: the functions its compile adds
    /// (each such node is one; a class, whose members and field initializers
    /// would add more, keeps the subtree eager).
    nested: u32,
    /// Whether `arguments` is read anywhere in it.
    reads_arguments: bool,
}

impl Compiler {
    /// Whether function `f`'s body may be deferred, as far as its own subtree
    /// goes, and if so how many functions are nested in it.
    pub(super) fn lazy_scan_fn(&mut self, f: &ast::Function) -> Option<u32> {
        let facts = self.scan_facts(FnNode::Function(f));
        // Read in a function nested in this one, `arguments` is that
        // function's own; read in this one's scope (an arrow's included), it
        // is this frame's object, which a stub does not make.
        let own = facts.ok
            && facts.reads_arguments
            && (capture::stmts_refs_all(&f.body.stmts).contains("arguments")
                || capture::params_reference("arguments", &f.params));
        note_eager(if own { Some("arguments") } else { facts.why }, f.span.end - f.span.start);
        (facts.ok && !own).then_some(facts.nested)
    }

    /// [`Compiler::lazy_scan_fn`] for an arrow.
    pub(super) fn lazy_scan_arrow(&mut self, a: &ast::Arrow) -> Option<u32> {
        let facts = self.scan_facts(FnNode::Arrow(a));
        // An arrow's own `arguments` is its enclosing function's.
        let own = facts.ok && facts.reads_arguments && capture::arrow_free_names(a).iter().any(|n| n == "arguments");
        note_eager(if own { Some("arguments") } else { facts.why }, a.span.end - a.span.start);
        (facts.ok && !own).then_some(facts.nested)
    }

    /// The scan of `node`'s subtree, each function's taken once: a function
    /// is met again as its enclosing functions are scanned and compiled, and
    /// walking it again at each level made the scan quadratic in nesting.
    fn scan_facts<'a>(&mut self, node: FnNode<'a>) -> ScanFacts {
        let key = node.key();
        if let Some(&facts) = self.lazy_scan.get(&key) {
            return facts;
        }
        let mut facts = ScanFacts { ok: true, why: None, nested: 0, reads_arguments: false };
        let mut children: Vec<FnNode<'a>> = Vec::new();
        let mut offer = |n: Node<'a>| -> visit::Flow {
            let child = match n {
                Node::Function(g) => Some(FnNode::Function(g)),
                Node::Arrow(a) => Some(FnNode::Arrow(a)),
                _ => None,
            };
            if let Some(c) = child.filter(|c| c.key() != key) {
                children.push(c);
                return visit::Flow::Skip;
            }
            if is_arguments_read(n) {
                facts.reads_arguments = true;
                return visit::Flow::Go;
            }
            match subtree_node_block(n) {
                Some(why) => {
                    facts.ok = false;
                    facts.why = Some(why);
                    visit::Flow::Stop
                }
                None => visit::Flow::Go,
            }
        };
        match node {
            FnNode::Function(f) => visit::function(f, &mut offer),
            FnNode::Arrow(a) => visit::arrow(a, &mut offer),
        };
        for c in children {
            if !facts.ok {
                break;
            }
            let cf = self.scan_facts(c);
            facts.nested += cf.nested + 1;
            facts.reads_arguments |= cf.reads_arguments;
            if !cf.ok {
                facts.ok = false;
                facts.why = cf.why;
            }
        }
        self.lazy_scan.insert(key, facts);
        facts
    }
}

/// Whether `node` reads `arguments` (a binding or assignment of the name
/// keeps its subtree eager, see [`restricted_name`]).
fn is_arguments_read(node: Node<'_>) -> bool {
    matches!(node, Node::Expr(ast::Expr::Ident(name)) if &**name == "arguments")
}

thread_local! {
    /// Verify mode's census of what kept bodies eager: per reason, the
    /// bodies and their source bytes (a body inside another counts in both).
    static EAGER_WHY: std::cell::RefCell<rustc_hash::FxHashMap<&'static str, (u64, u64)>> =
        std::cell::RefCell::new(rustc_hash::FxHashMap::default());
}

/// Count a body `why` kept eager, `bytes` of source long (verify mode only).
fn note_eager(why: Option<&'static str>, bytes: u32) {
    if let Some(why) = why {
        if lazy_setting() == 3 {
            EAGER_WHY.with(|m| {
                let mut m = m.borrow_mut();
                let e = m.entry(why).or_insert((0, 0));
                e.0 += 1;
                e.1 += u64::from(bytes);
            });
        }
    }
}

/// Verify mode's census so far, most source bytes first, and cleared.
pub(super) fn take_eager_census() -> Vec<(&'static str, u64, u64)> {
    let mut v: Vec<(&'static str, u64, u64)> =
        EAGER_WHY.with(|m| m.borrow_mut().drain().map(|(k, (n, b))| (k, n, b)).collect());
    v.sort_by(|a, b| b.2.cmp(&a.2));
    v
}

/// A function or arrow node of the program being compiled.
#[derive(Clone, Copy)]
enum FnNode<'a> {
    Function(&'a ast::Function),
    Arrow(&'a ast::Arrow),
}

impl FnNode<'_> {
    /// The node's address and kind: what the scan's memo knows it by, for
    /// as long as the syntax tree it is a node of lives.
    fn key(self) -> (usize, u8) {
        match self {
            FnNode::Function(f) => (f as *const ast::Function as usize, 0),
            FnNode::Arrow(a) => (a as *const ast::Arrow as usize, 1),
        }
    }
}

/// What one walk of a program finds for a compile that defers: every
/// function and arrow by source range (how a deferred body's node is found
/// again) and every regular expression literal (whose early errors are
/// checked before anything waits).
pub(super) struct ProgramIndex<'a> {
    nodes: rustc_hash::FxHashMap<(u32, u32), FnNode<'a>>,
    regexes: Vec<&'a ast::Expr>,
}

impl<'a> ProgramIndex<'a> {
    pub(super) fn of(prog: &'a ast::Program) -> ProgramIndex<'a> {
        let mut index = ProgramIndex { nodes: rustc_hash::FxHashMap::default(), regexes: Vec::new() };
        visit::stmts(&prog.body, &mut |n| {
            match n {
                Node::Function(f) => {
                    index.nodes.insert((f.span.start, f.span.end), FnNode::Function(f));
                }
                Node::Arrow(a) => {
                    index.nodes.insert((a.span.start, a.span.end), FnNode::Arrow(a));
                }
                Node::Expr(e @ ast::Expr::Regex { .. }) => index.regexes.push(e),
                _ => {}
            }
            visit::Flow::Go
        });
        index
    }

    /// How many functions and arrows the program has.
    pub(super) fn functions(&self) -> usize {
        self.nodes.len()
    }

    /// The first early error among the program's regular expression literals.
    pub(super) fn check_regexes(&self) -> R<()> {
        for e in &self.regexes {
            if let ast::Expr::Regex { pattern, flags } = e {
                super::exprs::check_regex_literal(&pattern.to_lossy_string(), flags)?;
            }
        }
        Ok(())
    }
}

impl Compiler {
    /// Compile every deferred body (and the bodies they defer in turn), each
    /// into the id its stub holds.
    pub(super) fn drain_lazy(&mut self, prog: &ast::Program) -> R<()> {
        if self.lazy_jobs.is_empty() {
            return Ok(());
        }
        let nodes = ProgramIndex::of(prog).nodes;
        while let Some(job) = self.lazy_jobs.pop_front() {
            self.lazy_deferred += 1;
            let node = *nodes
                .get(&job.span)
                .ok_or_else(|| format!("internal: no function at {:?} for a deferred body", job.span))?;
            let func_id = job.func_id;
            let (mut proto, start) = self.compile_job(job, node)?;
            // What the callers set on the stub after their compile call.
            let stub = &self.functions[func_id as usize];
            proto.name = stub.name.clone();
            proto.source = stub.source.clone();
            proto.name_global = stub.name_global;
            proto.non_constructable |= stub.non_constructable;
            self.mark_preamble_fn(&mut proto, start);
            self.functions[func_id as usize] = proto;
        }
        Ok(())
    }

    /// Compile deferred body `job` (its function node `node`) in the context
    /// it was met in: its proto and the source offset the function starts at.
    fn compile_job(&mut self, job: LazyJob, node: FnNode<'_>) -> R<(FuncProto, u32)> {
        job.cx.restore(self);
        self.lazy_seed = Some(job.seed);
        self.lazy_freeze = true;
        let r = self.compile_job_body(job.kind, job.span, job.captured, job.enclosing, node);
        self.lazy_freeze = false;
        // Taken by the body's `FnCompiler::new`, unless the compile failed first.
        self.lazy_seed = None;
        r
    }

    fn compile_job_body(
        &mut self,
        kind: LazyKind,
        span: (u32, u32),
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
        node: FnNode<'_>,
    ) -> R<(FuncProto, u32)> {
        Ok(match (kind, node) {
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
                    captured,
                    enclosing,
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
                    captured,
                    enclosing,
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
                    captured,
                    enclosing,
                    None,
                    false,
                    false,
                    false,
                    false,
                )?;
                (p, a.span.start)
            }
            _ => return Err(format!("internal: a deferred body at {span:?} changed kind")),
        })
    }
}

/// The function-id operands of `code` (ids its compile handed out from 0)
/// moved up by `base`: the ids they are installed at.
fn rebase_func_ids(code: &mut [Instr], base: u32) {
    for ins in code {
        match ins {
            Instr::MakeFunc { func_id, .. } | Instr::MakeClosure { func_id, .. } | Instr::MakeArrow { func_id, .. } => {
                *func_id += base
            }
            Instr::ClassAddMember { func, .. } => *func += base,
            _ => {}
        }
    }
}

/// The compiler of a running main program's deferred bodies, kept from its
/// compile: each body compiles at its function's first call, in the context
/// the main compile met it in ([`LazyJob`]), from the program's syntax tree.
///
/// Function ids a body's compile hands out run from 0; they are moved to the
/// ids reserved for them in front of its stub ([`LazyJob::reserved`]), or, if
/// the compile added more functions than its subtree held nodes, to the end
/// of the running function table. Global slots are the main compile's,
/// continued: a slot from `main_globals` on names a global the main program
/// never named, which the VM looks up by [`LazySession::global_name`].
pub(crate) struct LazySession<'a> {
    cx: Compiler,
    nodes: rustc_hash::FxHashMap<(u32, u32), FnNode<'a>>,
    jobs: rustc_hash::FxHashMap<u32, LazyJob>,
    main_globals: u32,
}

/// One compiled deferred body, ready to install.
pub(crate) struct LazyBody {
    /// The id `nested[0]` is installed at; the rest follow it.
    pub base: u32,
    /// The function itself, for its stub's id (with the stub's name, source
    /// and global binding still to copy over).
    pub root: FuncProto,
    /// The functions its compile added, in id order.
    pub nested: Vec<FuncProto>,
}

impl<'a> LazySession<'a> {
    /// Whether `fid` is a stub whose body has not compiled yet.
    pub(crate) fn pending(&self, fid: u32) -> bool {
        self.jobs.contains_key(&fid)
    }

    /// The ids of every stub whose body has not compiled yet, in order.
    pub(crate) fn pending_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.jobs.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// How many bodies this session has compiled, and how many wait.
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.cx.lazy_deferred, self.jobs.len())
    }

    /// The number of global slots the main program has.
    pub(crate) fn main_globals(&self) -> u32 {
        self.main_globals
    }

    /// The name of compile-time global slot `slot`.
    pub(crate) fn global_name(&self, slot: u32) -> Option<&str> {
        self.cx.globals.get(slot as usize).map(String::as_str)
    }

    /// Compile the body of stub `fid`. `next_free` is the first id past the
    /// running function table, where the body's functions go if it outgrew
    /// its reservation.
    pub(crate) fn compile(&mut self, fid: u32, next_free: u32) -> R<LazyBody> {
        let job = self.jobs.remove(&fid).ok_or_else(|| format!("internal: function {fid} has no deferred body"))?;
        let node = *self
            .nodes
            .get(&job.span)
            .ok_or_else(|| format!("internal: no function at {:?} for a deferred body", job.span))?;
        let (reserved, span) = (job.reserved, job.span);
        // The memo's names point into this session's syntax tree.
        self.cx.capture_memo.activate();
        let compiled = self.cx.compile_job(job, node);
        let misses = std::mem::take(&mut self.cx.lazy_misses);
        let mut nested = std::mem::take(&mut self.cx.functions);
        let (mut root, start) = match compiled {
            Ok(r) if misses.is_empty() => r,
            Ok(_) => {
                self.cx.lazy_jobs.clear();
                return Err(format!("internal: the deferred body at {span:?} resolved {misses:?} after its deferral"));
            }
            Err(e) => {
                self.cx.lazy_jobs.clear();
                return Err(e);
            }
        };
        self.cx.lazy_deferred += 1;
        let added = nested.len() as u32;
        let base = if added <= reserved { fid - reserved } else { next_free };
        for f in nested.iter_mut() {
            rebase_func_ids(&mut f.code, base);
            rewrite_string_accumulators(f, false);
            rewrite_local_accumulators(f);
            rewrite_append_indexes(f);
        }
        rebase_func_ids(&mut root.code, base);
        rewrite_string_accumulators(&mut root, false);
        rewrite_local_accumulators(&mut root);
        rewrite_append_indexes(&mut root);
        self.cx.mark_preamble_fn(&mut root, start);
        for mut j in self.cx.lazy_jobs.drain(..) {
            j.func_id += base;
            self.jobs.insert(j.func_id, j);
        }
        Ok(LazyBody { base, root, nested })
    }
}

/// How stub `stub` would have set up a frame, or captured its upvalues,
/// differently from its compiled body `body`: `None` when it would not.
pub(crate) fn stub_mismatch(stub: &FuncProto, body: &FuncProto) -> Option<String> {
    let same = stub.param_count == body.param_count
        && stub.length == body.length
        && stub.is_strict == body.is_strict
        && stub.lexical_this == body.lexical_this
        && stub.simple_params == body.simple_params
        && stub.rest_reg == body.rest_reg
        && stub.arguments_reg == body.arguments_reg
        && stub.is_generator == body.is_generator
        && stub.is_async == body.is_async
        && stub.super_static == body.super_static
        && stub.upvalues == body.upvalues
        && body.reg_count >= stub.reg_count;
    if same {
        return None;
    }
    let iface = |f: &FuncProto| {
        format!(
            "params {} len {} strict {} lt {} simple {} rest {:?} args {:?} gen {} async {} ss {} up {:?}",
            f.param_count,
            f.length,
            f.is_strict,
            f.lexical_this,
            f.simple_params,
            f.rest_reg,
            f.arguments_reg,
            f.is_generator,
            f.is_async,
            f.super_static,
            f.upvalues,
        )
    };
    let (s, b) = (iface(stub), iface(body));
    if s != b {
        return Some(format!("{s} vs {b}"));
    }
    if body.reg_count < stub.reg_count {
        return Some(format!("{} registers, fewer than the stub's {}", body.reg_count, stub.reg_count));
    }
    None
}

/// `program` (a [`LazyMode::Run`] compile) with every stub's body compiled
/// through `session` as first calls would compile them, and installed the way
/// the VM installs them: what verify mode compares with the eager compile.
/// Also every way a body or its stub disagreed with what was reserved for it.
pub(super) fn compile_every_stub(mut program: Program, session: &mut LazySession<'_>) -> R<(Program, Vec<String>)> {
    let mut funcs: Vec<FuncProto> = std::mem::take(&mut program.functions).into_iter().collect();
    let mut notes = Vec::new();
    let mut deferred_code = 0usize;
    loop {
        let ids = session.pending_ids();
        if ids.is_empty() {
            break;
        }
        for fid in ids {
            if !session.pending(fid) {
                continue;
            }
            let LazyBody { base, mut root, nested } = session.compile(fid, funcs.len() as u32)?;
            deferred_code += root.code.len() + nested.iter().map(|f| f.code.len()).sum::<usize>();
            let stub = &funcs[fid as usize];
            if let Some(why) = stub_mismatch(stub, &root) {
                notes.push(format!("stub {fid} ({}) does not match its body: {why}", stub.name));
            }
            root.name = stub.name.clone();
            root.source = stub.source.clone();
            root.name_global = stub.name_global;
            root.non_constructable |= stub.non_constructable;
            if base as usize == funcs.len() && !nested.is_empty() {
                notes.push(format!("stub {fid} ({}): its body added {} functions, more than were reserved", root.name, nested.len()));
                funcs.extend(nested);
            } else {
                let reserved_left = fid - base - nested.len() as u32;
                if reserved_left != 0 {
                    notes.push(format!("stub {fid} ({}): {reserved_left} reserved ids unused", root.name));
                }
                for (i, f) in nested.into_iter().enumerate() {
                    funcs[base as usize + i] = f;
                }
            }
            funcs[fid as usize] = root;
        }
    }
    notes.push(format!(
        "stat: {deferred_code} of {} instructions were in deferred bodies",
        funcs.iter().map(|f| f.code.len()).sum::<usize>()
    ));
    program.functions = funcs.into();
    program.global_names = session.cx.globals.clone();
    program.global_count = program.global_names.len() as u32;
    Ok((program, notes))
}

impl Compiler {
    /// This main compile's deferred bodies, with the compiler and syntax tree
    /// that will compile them (see [`LazySession`]). `None` when nothing waits.
    pub(super) fn into_lazy_session(mut self, index: ProgramIndex<'_>, main_globals: u32) -> Option<LazySession<'_>> {
        if self.lazy_jobs.is_empty() {
            return None;
        }
        let jobs = self.lazy_jobs.drain(..).map(|j| (j.func_id, j)).collect();
        self.lazy_deferred = 0;
        Some(LazySession { cx: self, nodes: index.nodes, jobs, main_globals })
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
            "function f(s) { return /a+b/g.test(s); }",
            "function f() { return function g() { return arguments.length; }; }",
            "var o = { m(x) { return function inner(y) { return x + y; }; } };",
        ];
        for src in sources {
            let (diffs, deferred) = both(src);
            assert!(diffs.is_empty(), "{src}\n{}", diffs.join("\n"));
            assert!(deferred > 0, "nothing was deferred in: {src}");
        }
    }

    /// A body whose subtree holds a construct with a compile-time rule of its
    /// own stays eager, and so does everything that contains it; so does one
    /// that reads its own `arguments` (an arrow in it reads the same).
    #[test]
    fn early_error_constructs_keep_bodies_eager() {
        for src in [
            "function f() { return arguments.length; }",
            "function f() { return () => arguments[0]; }",
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

    /// `src` run with every body compiled first, and with the bodies it may
    /// defer compiled at their first calls: each run's output (or error), and
    /// how many bodies the second compiled and left waiting.
    fn run_both(src: &str) -> (String, String, (usize, usize)) {
        let prog = crate::front::parse_script(src).expect("parses");
        let eager = super::super::entry::compile_main_program(&prog, src).expect("compiles");
        let mut vm = crate::vm::Vm::new(&eager);
        let a = match vm.run() {
            Ok(_) => vm.output.join("\n"),
            Err(e) => format!("{}\nerror: {}", vm.output.join("\n"), e.0),
        };
        let (lazy, session) = super::super::entry::compile_main_program_lazy(&prog, src, 0).expect("compiles");
        let mut vm = crate::vm::Vm::new(&lazy);
        vm.set_lazy_session(session.expect("something waits"));
        let b = match vm.run() {
            Ok(_) => vm.output.join("\n"),
            Err(e) => format!("{}\nerror: {}", vm.output.join("\n"), e.0),
        };
        let counts = vm.lazy_counts().expect("a session");
        (a, b, counts)
    }

    /// Programs whose functions compile at their first calls behave as they
    /// do compiled whole: closures over every kind of enclosing binding,
    /// parameter forms, `this` binding, construction, recursion, reflection
    /// on functions before and after their first call, the calls that come
    /// from builtins, globals only deferred code names, and throws.
    #[test]
    fn first_call_compiles_behave_as_whole_compiles() {
        let sources = [
            // Captures several levels up, the functions between forwarding.
            "var g = 1; function outer(a) { var b = a + g; function mid() { return function inner(c) { return a + b + c + g; }; } return mid; } console.log(outer(1)()(2), outer(5)()(6));",
            // Loop-iteration cells, closures made before any of them runs.
            "function f() { var fs = []; for (let i = 0; i < 3; i++) { fs.push(function () { return i * 10; }); } return fs; } console.log(f().map(function (h) { return h(); }).join());",
            // Defaults, destructuring and rest, reading earlier parameters.
            "function d(x, y = x + 1, { z } = { z: y * 2 }, [w] = [z + 1], ...rest) { return [x, y, z, w, rest.length]; } console.log(d(1).join(), d(1, 2, { z: 3 }, [4], 5, 6).join(), d.length);",
            "var r = (a, ...more) => a + more.length; console.log(r(1, 2, 3), r.length);",
            // A nested function's own `arguments`, inside a deferred body.
            "function wrap() { return function () { return arguments.length + ':' + arguments[0]; }; } console.log(wrap()('x', 'y'));",
            // `this`: sloppy gets the global object, strict gets what it was given.
            "function sloppy() { return this === globalThis; } function strict() { 'use strict'; return this; } console.log(sloppy(), strict(), strict.call(7));",
            // Construction, prototype methods, `instanceof`.
            "function P(x) { this.x = x; } P.prototype.get = function () { return this.x * 2; }; var p = new P(21); console.log(p.get(), p instanceof P, Object.keys(p).join());",
            // Recursion through a named expression and between declarations.
            "var fact = function me(n) { return n < 2 ? 1 : n * me(n - 1); }; function even(n) { return n === 0 ? true : odd(n - 1); } function odd(n) { return n === 0 ? false : even(n - 1); } console.log(fact(10), even(10), odd(7));",
            // Reflection on a function before and after its first call.
            "function refl(a, b) { return a + b; } var before = [refl.name, refl.length, String(refl)].join('|'); refl(1, 2); console.log(before === [refl.name, refl.length, String(refl)].join('|'), refl.length);",
            // Calls from builtins: callbacks, `call`/`apply`/`bind`, a comparator.
            "function add(a, b) { return a + b; } function cmp(a, b) { return b - a; } console.log([3, 1, 2].sort(cmp).join(), [1, 2, 3].reduce(add, 0), add.call(null, 1, 2), add.apply(null, [3, 4]), add.bind(null, 10)(5));",
            // Globals that only deferred code names, an implicit one included.
            "function setIt() { made = JSON.stringify({ a: [1, 2] }); } function readIt() { return typeof notThere + ':' + made + ':' + Math.max(3, 4); } setIt(); console.log(readIt());",
            // A regular expression literal in a deferred body.
            "function m(s) { return /b+/g.exec(s)[0].length; } console.log(m('abbbc'));",
            // Throws out of deferred code, caught and uncaught by it.
            "function boom() { throw new TypeError('no ' + 'way'); } function guard() { try { boom(); } catch (e) { return e.name + ': ' + e.message; } } console.log(guard()); boom();",
            // Accessors and methods of an object made in a deferred body.
            "function mk() { var v = 1; return { get v() { return v; }, set v(n) { v = n * 2; }, bump() { return ++v; } }; } var o = mk(); o.v = 5; console.log(o.v, o.bump());",
            // A hot function: many calls after its first, from several sites.
            "function sq(x) { return x * x; } var s = 0; for (var i = 0; i < 2000; i++) { s += sq(i) - sq(i - 1); } console.log(s);",
            // Bodies deferred inside bodies that are themselves first-call compiles.
            "function a1() { function a2() { function a3() { return 'deep'; } return a3; } return a2()(); } console.log(a1(), a1());",
        ];
        for src in sources {
            let (eager, lazy, (compiled, _)) = run_both(src);
            assert_eq!(eager, lazy, "{src}");
            assert!(compiled > 0, "nothing compiled at a first call: {src}");
        }
    }

    /// A function that never runs still has its regular expression literals
    /// checked: an invalid one is the program's syntax error.
    #[test]
    fn a_waiting_body_keeps_its_regex_early_error() {
        let src = "function never() { return /a{2,1}/.test('a'); } 1;";
        let prog = crate::front::parse_script(src).expect("parses");
        let err = super::super::entry::compile_main_program_lazy(&prog, src, 0).err().expect("an early error");
        assert!(err.contains("SyntaxError"), "{err}");
    }
}