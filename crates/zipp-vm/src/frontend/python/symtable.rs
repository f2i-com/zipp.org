//! Scope analysis for the Python frontend: CPython's symbol-table rules.
//!
//! Every function, lambda, comprehension and class body gets a [`Scope`].
//! A name in a function is LOCAL if bound there (assignment, `for` target,
//! `def`/`class`/`import`, parameter, `with ... as`, `except ... as`, walrus)
//! unless declared `global`/`nonlocal`; otherwise it is FREE when some
//! enclosing *function* scope binds it (class scopes are skipped, as in
//! CPython), else GLOBAL. A local that a nested scope reads is a CELL.
//! Class bodies resolve their own bindings through the class namespace and
//! everything else through the enclosing function scopes or the globals;
//! methods that use `super()` or `__class__` get the implicit `__class__` cell.
use ast::Ranged;
use rustpython_parser::ast;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopeKind {
    Module,
    Function,
    Class,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SymKind {
    /// A plain register-resident local.
    Local,
    /// A local captured by a nested scope: lives in a cell object.
    Cell,
    /// Bound in an enclosing function scope: read through a captured cell.
    Free,
    /// Resolved through the module's globals (and the builtins).
    Global,
    /// A class-body binding: lives in the class namespace.
    ClassLocal,
    /// In a comprehension compiled inline: a plain local of the enclosing
    /// code, read and written in that code's own register.
    Outer,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct Scope {
    pub kind: ScopeKind,
    pub name: String,
    pub symbols: BTreeMap<String, SymKind>,
    /// Parameters in signature order (positional, then keyword-only), before
    /// the `*args`/`**kwargs` names which are appended last.
    pub params: Vec<String>,
    /// Free variables in a fixed order: the index into the function's `cells`
    /// array at runtime.
    pub free_order: Vec<String>,
    pub is_generator: bool,
    /// Names a `del` statement unbinds somewhere in this scope.
    pub deleted: BTreeSet<String>,
    /// The scope defines `super`/`__class__` uses (methods) — needs the
    /// implicit `__class__` cell from the enclosing class body.
    pub uses_class_cell: bool,
    /// Names a walrus in this scope's own code binds. A read of such a local
    /// is copied out of its register, so an operand already evaluated keeps
    /// its value when a later operand of the same expression rebinds it.
    pub walrus_targets: BTreeSet<String>,
    /// A list, set or dict comprehension the emitter compiles inline in
    /// the enclosing function or module code (PEP 709) rather than as a
    /// code object of its own: not a generator expression, not in a class
    /// body, with no nested scopes and no `super`. The enclosing code's
    /// locals it uses are [`SymKind::Outer`], not cells.
    pub inline: bool,
    /// Source range (start, end) of the defining node, the emitter's key
    /// into the table.
    pub offset: (u32, u32),
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

pub(super) struct SymTable {
    pub scopes: Vec<Scope>,
    /// Scopes by the full source range of their defining node. A start
    /// offset alone is ambiguous: an unparenthesized generator expression
    /// starts where its element does, and the element may be a
    /// comprehension or a lambda.
    pub by_offset: BTreeMap<(u32, u32), usize>,
}

/// Raw facts gathered by the first pass, resolved by the second.
struct Raw {
    kind: ScopeKind,
    name: String,
    offset: (u32, u32),
    parent: Option<usize>,
    bound: BTreeSet<String>,
    used: BTreeSet<String>,
    globals: BTreeSet<String>,
    nonlocals: BTreeSet<String>,
    params: Vec<String>,
    is_generator: bool,
    is_comprehension: bool,
    deleted: BTreeSet<String>,
    uses_class_cell: bool,
    walrus_targets: BTreeSet<String>,
    children: Vec<usize>,
}

struct Builder {
    raws: Vec<Raw>,
    current: usize,
}

type R<T> = Result<T, String>;

pub(super) fn analyse(module: &[ast::Stmt], name: &str) -> R<SymTable> {
    let mut b = Builder {
        raws: vec![Raw::new(ScopeKind::Module, name, (0, 0), None)],
        current: 0,
    };
    b.stmts(module)?;
    resolve(b.raws)
}

fn range_key(node: &impl Ranged) -> (u32, u32) {
    let range = node.range();
    (u32::from(range.start()), u32::from(range.end()))
}

impl Raw {
    fn new(kind: ScopeKind, name: &str, offset: (u32, u32), parent: Option<usize>) -> Self {
        Raw {
            kind,
            name: name.to_owned(),
            offset,
            parent,
            bound: BTreeSet::new(),
            used: BTreeSet::new(),
            globals: BTreeSet::new(),
            nonlocals: BTreeSet::new(),
            params: Vec::new(),
            is_generator: false,
            is_comprehension: false,
            deleted: BTreeSet::new(),
            uses_class_cell: false,
            walrus_targets: BTreeSet::new(),
            children: Vec::new(),
        }
    }
}

impl Builder {
    fn bind(&mut self, name: &str) {
        self.raws[self.current].bound.insert(name.to_owned());
    }
    fn use_(&mut self, name: &str) {
        self.raws[self.current].used.insert(name.to_owned());
    }
    fn enter(&mut self, kind: ScopeKind, name: &str, offset: (u32, u32)) -> usize {
        let parent = self.current;
        let id = self.raws.len();
        self.raws.push(Raw::new(kind, name, offset, Some(parent)));
        self.raws[parent].children.push(id);
        self.current = id;
        id
    }
    fn leave(&mut self, id: usize) {
        self.current = self.raws[id].parent.expect("nested scope has a parent");
    }
    fn stmts(&mut self, body: &[ast::Stmt]) -> R<()> {
        for s in body {
            self.stmt(s)?;
        }
        Ok(())
    }
    fn stmt(&mut self, s: &ast::Stmt) -> R<()> {
        match s {
            ast::Stmt::FunctionDef(f) => {
                for d in &f.decorator_list {
                    self.expr(d)?;
                }
                self.arg_defaults(&f.args)?;
                // Annotations are expressions of the defining scope.
                for a in f
                    .args
                    .posonlyargs
                    .iter()
                    .chain(&f.args.args)
                    .chain(&f.args.kwonlyargs)
                {
                    if let Some(ann) = &a.def.annotation {
                        self.expr(ann)?;
                    }
                }
                for a in [&f.args.vararg, &f.args.kwarg].into_iter().flatten() {
                    if let Some(ann) = &a.annotation {
                        self.expr(ann)?;
                    }
                }
                if let Some(ret) = &f.returns {
                    self.expr(ret)?;
                }
                self.bind(f.name.as_str());
                self.function(
                    f.name.as_str(),
                    &f.args,
                    &f.body,
                    range_key(f),
                )?;
            }
            ast::Stmt::AsyncFunctionDef(f) => {
                return Err(unsupported(f, "async functions"));
            }
            ast::Stmt::ClassDef(c) => {
                for d in &c.decorator_list {
                    self.expr(d)?;
                }
                for b in &c.bases {
                    self.expr(b)?;
                }
                for k in &c.keywords {
                    self.expr(&k.value)?;
                }
                self.bind(c.name.as_str());
                let id = self.enter(
                    ScopeKind::Class,
                    c.name.as_str(),
                    range_key(c),
                );
                self.stmts(&c.body)?;
                self.leave(id);
            }
            ast::Stmt::Return(r) => {
                if let Some(v) = &r.value {
                    self.expr(v)?;
                }
            }
            ast::Stmt::Delete(d) => {
                for t in &d.targets {
                    self.target(t)?;
                    if let ast::Expr::Name(n) = t {
                        // A deleted local can be unbound anywhere after: its
                        // reads always carry the unbound check.
                        self.raws[self.current].deleted.insert(n.id.to_string());
                    }
                }
            }
            ast::Stmt::Assign(a) => {
                self.expr(&a.value)?;
                for t in &a.targets {
                    self.target(t)?;
                }
            }
            ast::Stmt::AugAssign(a) => {
                self.expr(&a.value)?;
                self.target(&a.target)?;
                if let ast::Expr::Name(n) = a.target.as_ref() {
                    self.use_(n.id.as_str());
                }
            }
            ast::Stmt::AnnAssign(a) => {
                if let Some(v) = &a.value {
                    self.expr(v)?;
                }
                self.expr(&a.annotation)?;
                self.target(&a.target)?;
            }
            ast::Stmt::For(f) => {
                self.expr(&f.iter)?;
                self.target(&f.target)?;
                self.stmts(&f.body)?;
                self.stmts(&f.orelse)?;
            }
            ast::Stmt::While(w) => {
                self.expr(&w.test)?;
                self.stmts(&w.body)?;
                self.stmts(&w.orelse)?;
            }
            ast::Stmt::If(i) => {
                // An `elif` is an If alone in the orelse: walk the chain
                // iteratively so its length costs no native stack.
                let mut arm = i;
                loop {
                    self.expr(&arm.test)?;
                    self.stmts(&arm.body)?;
                    match arm.orelse.as_slice() {
                        [ast::Stmt::If(next)] => arm = next,
                        orelse => {
                            self.stmts(orelse)?;
                            break;
                        }
                    }
                }
            }
            ast::Stmt::With(w) => {
                for item in &w.items {
                    self.expr(&item.context_expr)?;
                    if let Some(v) = &item.optional_vars {
                        self.target(v)?;
                    }
                }
                self.stmts(&w.body)?;
            }
            ast::Stmt::Raise(r) => {
                if let Some(e) = &r.exc {
                    self.expr(e)?;
                }
                if let Some(c) = &r.cause {
                    self.expr(c)?;
                }
            }
            ast::Stmt::Try(t) => {
                self.stmts(&t.body)?;
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    if let Some(ty) = &h.type_ {
                        self.expr(ty)?;
                    }
                    if let Some(name) = &h.name {
                        self.bind(name.as_str());
                    }
                    self.stmts(&h.body)?;
                }
                self.stmts(&t.orelse)?;
                self.stmts(&t.finalbody)?;
            }
            ast::Stmt::Assert(a) => {
                self.expr(&a.test)?;
                if let Some(m) = &a.msg {
                    self.expr(m)?;
                }
            }
            ast::Stmt::Import(i) => {
                for alias in &i.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    // `import a.b` binds `a`.
                    let first = bound.as_str().split('.').next().unwrap_or("");
                    self.bind(first);
                }
            }
            ast::Stmt::ImportFrom(i) => {
                for alias in &i.names {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.bind(bound.as_str());
                }
            }
            ast::Stmt::Global(g) => {
                for n in &g.names {
                    self.raws[self.current].globals.insert(n.to_string());
                }
            }
            ast::Stmt::Nonlocal(g) => {
                for n in &g.names {
                    self.raws[self.current].nonlocals.insert(n.to_string());
                }
            }
            ast::Stmt::Expr(e) => self.expr(&e.value)?,
            ast::Stmt::Pass(_) | ast::Stmt::Break(_) | ast::Stmt::Continue(_) => {}
            ast::Stmt::AsyncFor(x) => return Err(unsupported(x, "async for")),
            ast::Stmt::AsyncWith(x) => return Err(unsupported(x, "async with")),
            ast::Stmt::Match(x) => {
                self.expr(&x.subject)?;
                for case in &x.cases {
                    self.pattern(&case.pattern)?;
                    if let Some(guard) = &case.guard {
                        self.expr(guard)?;
                    }
                    self.stmts(&case.body)?;
                }
            }
            ast::Stmt::TypeAlias(x) => return Err(unsupported(x, "type aliases")),
            ast::Stmt::TryStar(x) => return Err(unsupported(x, "except* groups")),
        }
        Ok(())
    }
    /// Capture patterns bind in the enclosing scope, like assignment targets.
    fn pattern(&mut self, p: &ast::Pattern) -> R<()> {
        match p {
            ast::Pattern::MatchValue(v) => self.expr(&v.value)?,
            ast::Pattern::MatchSingleton(_) => {}
            ast::Pattern::MatchSequence(q) => {
                for x in &q.patterns {
                    self.pattern(x)?;
                }
            }
            ast::Pattern::MatchMapping(m) => {
                for k in &m.keys {
                    self.expr(k)?;
                }
                for x in &m.patterns {
                    self.pattern(x)?;
                }
                if let Some(rest) = &m.rest {
                    self.bind(rest.as_str());
                }
            }
            ast::Pattern::MatchClass(c) => {
                self.expr(&c.cls)?;
                for x in &c.patterns {
                    self.pattern(x)?;
                }
                for x in &c.kwd_patterns {
                    self.pattern(x)?;
                }
            }
            ast::Pattern::MatchStar(s) => {
                if let Some(name) = &s.name {
                    self.bind(name.as_str());
                }
            }
            ast::Pattern::MatchAs(a) => {
                if let Some(inner) = &a.pattern {
                    self.pattern(inner)?;
                }
                if let Some(name) = &a.name {
                    self.bind(name.as_str());
                }
            }
            ast::Pattern::MatchOr(o) => {
                for x in &o.patterns {
                    self.pattern(x)?;
                }
            }
        }
        Ok(())
    }
    fn arg_defaults(&mut self, args: &ast::Arguments) -> R<()> {
        for a in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
        {
            if let Some(d) = &a.default {
                self.expr(d)?;
            }
        }
        Ok(())
    }
    fn function(
        &mut self,
        name: &str,
        args: &ast::Arguments,
        body: &[ast::Stmt],
        offset: (u32, u32),
    ) -> R<()> {
        let id = self.enter(ScopeKind::Function, name, offset);
        let mut params = Vec::new();
        for a in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
        {
            params.push(a.def.arg.to_string());
        }
        if let Some(v) = &args.vararg {
            params.push(v.arg.to_string());
        }
        if let Some(k) = &args.kwarg {
            params.push(k.arg.to_string());
        }
        for p in &params {
            self.bind(p);
        }
        self.raws[id].params = params;
        self.stmts(body)?;
        self.leave(id);
        Ok(())
    }
    fn target(&mut self, t: &ast::Expr) -> R<()> {
        match t {
            ast::Expr::Name(n) => self.bind(n.id.as_str()),
            ast::Expr::Tuple(x) => {
                for e in &x.elts {
                    self.target(e)?;
                }
            }
            ast::Expr::List(x) => {
                for e in &x.elts {
                    self.target(e)?;
                }
            }
            ast::Expr::Starred(s) => self.target(&s.value)?,
            ast::Expr::Attribute(a) => self.expr(&a.value)?,
            ast::Expr::Subscript(s) => {
                self.expr(&s.value)?;
                self.expr(&s.slice)?;
            }
            _ => return Err(unsupported(t, "assignment target")),
        }
        Ok(())
    }
    fn comprehension(
        &mut self,
        kind: &str,
        offset: (u32, u32),
        generators: &[ast::Comprehension],
        elts: &[&ast::Expr],
    ) -> R<()> {
        // The outermost iterable is evaluated in the enclosing scope and passed
        // in as the comprehension function's only argument.
        self.expr(&generators[0].iter)?;
        let id = self.enter(ScopeKind::Function, kind, offset);
        self.raws[id].params = vec![".0".to_owned()];
        self.raws[id].is_comprehension = true;
        self.bind(".0");
        if kind == "<genexpr>" {
            self.raws[id].is_generator = true;
        }
        for (i, g) in generators.iter().enumerate() {
            if g.is_async {
                return Err(unsupported(&g.iter, "async comprehensions"));
            }
            if i > 0 {
                self.expr(&g.iter)?;
            }
            self.target(&g.target)?;
            for c in &g.ifs {
                self.expr(c)?;
            }
        }
        for e in elts {
            self.expr(e)?;
        }
        self.leave(id);
        Ok(())
    }
    fn expr(&mut self, e: &ast::Expr) -> R<()> {
        match e {
            ast::Expr::Name(n) => {
                if n.id.as_str() == "super" || n.id.as_str() == "__class__" {
                    self.raws[self.current].uses_class_cell = true;
                }
                self.use_(n.id.as_str());
            }
            ast::Expr::Constant(_) => {}
            ast::Expr::BoolOp(b) => {
                for v in &b.values {
                    self.expr(v)?;
                }
            }
            ast::Expr::NamedExpr(n) => {
                self.expr(&n.value)?;
                // PEP 572: a walrus inside a comprehension binds in the nearest
                // enclosing non-comprehension scope; the comprehension itself
                // then reaches it as a free variable.
                let ast::Expr::Name(target) = n.target.as_ref() else {
                    return Err(unsupported(n.target.as_ref(), "walrus target"));
                };
                let name = target.id.as_str();
                let mut owner = self.current;
                while self.raws[owner].is_comprehension {
                    owner = self.raws[owner].parent.expect("comprehension has a parent");
                }
                // A read of the target in the owner is copied out of its
                // register (an inline comprehension writes that register).
                self.raws[owner].walrus_targets.insert(name.to_owned());
                if owner == self.current {
                    self.bind(name);
                } else {
                    self.raws[owner].bound.insert(name.to_owned());
                    let module_owner = self.raws[owner].kind == ScopeKind::Module;
                    let mut cur = self.current;
                    while cur != owner {
                        if module_owner {
                            self.raws[cur].globals.insert(name.to_owned());
                        } else {
                            self.raws[cur].nonlocals.insert(name.to_owned());
                        }
                        self.raws[cur].used.insert(name.to_owned());
                        cur = self.raws[cur].parent.expect("comprehension has a parent");
                    }
                }
            }
            ast::Expr::BinOp(b) => {
                // A left-deep operator chain (`a + b + c ...`) is walked
                // iteratively, leftmost operand first.
                let mut rights = vec![b.right.as_ref()];
                let mut left = b.left.as_ref();
                while let ast::Expr::BinOp(inner) = left {
                    rights.push(inner.right.as_ref());
                    left = inner.left.as_ref();
                }
                self.expr(left)?;
                for right in rights.into_iter().rev() {
                    self.expr(right)?;
                }
            }
            ast::Expr::UnaryOp(u) => self.expr(&u.operand)?,
            ast::Expr::Lambda(l) => {
                self.arg_defaults(&l.args)?;
                let body = std::slice::from_ref(&*l.body);
                let id = self.enter(
                    ScopeKind::Function,
                    "<lambda>",
                    range_key(l),
                );
                let mut params = Vec::new();
                for a in l
                    .args
                    .posonlyargs
                    .iter()
                    .chain(&l.args.args)
                    .chain(&l.args.kwonlyargs)
                {
                    params.push(a.def.arg.to_string());
                }
                if let Some(v) = &l.args.vararg {
                    params.push(v.arg.to_string());
                }
                if let Some(k) = &l.args.kwarg {
                    params.push(k.arg.to_string());
                }
                for p in &params {
                    self.bind(p);
                }
                self.raws[id].params = params;
                self.expr(&body[0])?;
                self.leave(id);
            }
            ast::Expr::IfExp(i) => {
                self.expr(&i.test)?;
                self.expr(&i.body)?;
                self.expr(&i.orelse)?;
            }
            ast::Expr::Dict(d) => {
                for k in d.keys.iter().flatten() {
                    self.expr(k)?;
                }
                for v in &d.values {
                    self.expr(v)?;
                }
            }
            ast::Expr::Set(s) => {
                for v in &s.elts {
                    self.expr(v)?;
                }
            }
            ast::Expr::ListComp(c) => self.comprehension(
                "<listcomp>",
                range_key(c),
                &c.generators,
                &[&c.elt],
            )?,
            ast::Expr::SetComp(c) => self.comprehension(
                "<setcomp>",
                range_key(c),
                &c.generators,
                &[&c.elt],
            )?,
            ast::Expr::DictComp(c) => self.comprehension(
                "<dictcomp>",
                range_key(c),
                &c.generators,
                &[&c.key, &c.value],
            )?,
            ast::Expr::GeneratorExp(c) => self.comprehension(
                "<genexpr>",
                range_key(c),
                &c.generators,
                &[&c.elt],
            )?,
            ast::Expr::Await(x) => return Err(unsupported(x, "await")),
            ast::Expr::Yield(y) => {
                self.raws[self.current].is_generator = true;
                if let Some(v) = &y.value {
                    self.expr(v)?;
                }
            }
            ast::Expr::YieldFrom(y) => {
                self.raws[self.current].is_generator = true;
                self.expr(&y.value)?;
            }
            ast::Expr::Compare(c) => {
                self.expr(&c.left)?;
                for x in &c.comparators {
                    self.expr(x)?;
                }
            }
            ast::Expr::Call(c) => {
                self.expr(&c.func)?;
                for a in &c.args {
                    self.expr(a)?;
                }
                for k in &c.keywords {
                    self.expr(&k.value)?;
                }
            }
            ast::Expr::FormattedValue(f) => {
                self.expr(&f.value)?;
                if let Some(s) = &f.format_spec {
                    self.expr(s)?;
                }
            }
            ast::Expr::JoinedStr(j) => {
                for v in &j.values {
                    self.expr(v)?;
                }
            }
            ast::Expr::Attribute(a) => self.expr(&a.value)?,
            ast::Expr::Subscript(s) => {
                self.expr(&s.value)?;
                self.expr(&s.slice)?;
            }
            ast::Expr::Starred(s) => self.expr(&s.value)?,
            ast::Expr::List(l) => {
                for v in &l.elts {
                    self.expr(v)?;
                }
            }
            ast::Expr::Tuple(t) => {
                for v in &t.elts {
                    self.expr(v)?;
                }
            }
            ast::Expr::Slice(s) => {
                for part in [&s.lower, &s.upper, &s.step].into_iter().flatten() {
                    self.expr(part)?;
                }
            }
        }
        Ok(())
    }
}

fn unsupported(node: &impl Ranged, what: &str) -> String {
    format!(
        "Python: {what} are not supported yet (at offset {})",
        u32::from(node.range().start())
    )
}

/// The prefix of a class scope's synthetic symbol for a free variable the
/// class passes through to nested scopes while also binding (or declaring
/// global) the same name itself.
const PASS_THROUGH: &str = "\u{0}free:";

/// Second pass: classify every name in every scope.
fn resolve(raws: Vec<Raw>) -> R<SymTable> {
    let n = raws.len();
    let mut kinds: Vec<BTreeMap<String, SymKind>> = vec![BTreeMap::new(); n];
    // Pass 1: locals/globals/class-locals and free-variable requests.
    for (id, raw) in raws.iter().enumerate() {
        for name in raw.bound.iter().chain(raw.used.iter()) {
            if raw.globals.contains(name) {
                kinds[id].insert(name.clone(), SymKind::Global);
                continue;
            }
            if raw.nonlocals.contains(name) {
                if raw.kind == ScopeKind::Module {
                    return Err(format!("nonlocal declaration at module level: {name}"));
                }
                if !binds_in_enclosing_function(&raws, id, name) {
                    return Err(format!("no binding for nonlocal '{name}' found"));
                }
                kinds[id].insert(name.clone(), SymKind::Free);
                continue;
            }
            let kind = match raw.kind {
                ScopeKind::Module => SymKind::Global,
                ScopeKind::Class => {
                    if raw.bound.contains(name) {
                        SymKind::ClassLocal
                    } else if binds_in_enclosing_function(&raws, id, name) {
                        SymKind::Free
                    } else {
                        SymKind::Global
                    }
                }
                ScopeKind::Function => {
                    if raw.bound.contains(name) {
                        SymKind::Local
                    } else if binds_in_enclosing_function(&raws, id, name) {
                        SymKind::Free
                    } else {
                        SymKind::Global
                    }
                }
            };
            kinds[id].insert(name.clone(), kind);
        }
        if raw.uses_class_cell && raw.kind == ScopeKind::Function {
            // The implicit `__class__` cell comes from the nearest enclosing
            // class body, through any intervening function scopes.
            if enclosing_class(&raws, id).is_some() {
                kinds[id].insert("__class__".to_owned(), SymKind::Free);
            }
        }
    }
    let fast = super::emitter::py_fast_paths();
    let inline: Vec<bool> = raws
        .iter()
        .map(|raw| {
            fast && raw.is_comprehension
                && raw.name != "<genexpr>"
                && raw.children.is_empty()
                && !raw.uses_class_cell
                && raw.parent.is_some_and(|p| raws[p].kind != ScopeKind::Class)
        })
        .collect();
    // Pass 2: a free variable in a scope makes the binding scope's symbol a
    // cell, and every intervening function/class scope passes it through as
    // free too (so cells chain down through nesting). A local of the code an
    // inline comprehension runs in stays a local.
    let mut changed = true;
    while changed {
        changed = false;
        for id in 0..n {
            let frees: Vec<String> = kinds[id]
                .iter()
                .filter(|(_, k)| **k == SymKind::Free)
                .map(|(name, _)| name.clone())
                .collect();
            for key in frees {
                // A class's synthetic pass-through entry stands for the plain
                // name in every scope above it.
                let name = key.strip_prefix(PASS_THROUGH).unwrap_or(&key);
                let mut cur = raws[id].parent;
                while let Some(p) = cur {
                    let entry = kinds[p].get(name).copied();
                    let is_class_cell = name == "__class__" && raws[p].kind == ScopeKind::Class;
                    match entry {
                        Some(SymKind::Local) if inline[id] && Some(p) == raws[id].parent => break,
                        Some(SymKind::Local) => {
                            kinds[p].insert(name.to_owned(), SymKind::Cell);
                            changed = true;
                            break;
                        }
                        Some(SymKind::Cell) => break,
                        Some(SymKind::Free) => break,
                        _ if is_class_cell => {
                            // The class body owns the `__class__` cell.
                            if entry != Some(SymKind::Cell) {
                                kinds[p].insert(name.to_owned(), SymKind::Cell);
                                changed = true;
                            }
                            break;
                        }
                        Some(SymKind::ClassLocal) | Some(SymKind::Global) | None
                            if raws[p].kind != ScopeKind::Module =>
                        {
                            // Pass-through: this scope does not bind it (a class
                            // local is NOT visible to nested scopes).
                            if entry.is_none() {
                                kinds[p].insert(name.to_owned(), SymKind::Free);
                                changed = true;
                            } else {
                                // Both: the class resolves its own binding (or its
                                // `global` declaration) AND passes the outer cell
                                // through, marked by a synthetic entry. Only a new
                                // entry is a change, or the fixpoint never settles.
                                let synthetic = format!("{PASS_THROUGH}{name}");
                                if kinds[p].insert(synthetic, SymKind::Free).is_none() {
                                    changed = true;
                                }
                            }
                            cur = raws[p].parent;
                            continue;
                        }
                        _ => break,
                    }
                }
            }
        }
    }
    // A `del` that reaches its binding through `nonlocal` unbinds the OWNER's
    // variable, so the owner cannot treat the name as definitely assigned
    // either: `y = 1` in a function, a nested `nonlocal y; del y`, and then a
    // read of `y` must raise NameError rather than hand back the unbound
    // sentinel. Only the deleting scope recorded it.
    let mut deleted: Vec<BTreeSet<String>> = raws.iter().map(|r| r.deleted.clone()).collect();
    for id in 0..n {
        for name in deleted[id].clone() {
            if kinds[id].get(&name) != Some(&SymKind::Free) {
                continue;
            }
            let mut cur = raws[id].parent;
            while let Some(p) = cur {
                match kinds[p].get(&name) {
                    Some(SymKind::Cell) | Some(SymKind::Local) => {
                        deleted[p].insert(name.clone());
                        break;
                    }
                    _ => cur = raws[p].parent,
                }
            }
        }
    }
    for id in (0..n).filter(|id| inline[*id]) {
        let parent = raws[id].parent.expect("a comprehension has a parent");
        let outer: Vec<String> = kinds[id]
            .iter()
            .filter(|(name, k)| **k == SymKind::Free && kinds[parent].get(*name) == Some(&SymKind::Local))
            .map(|(name, _)| name.clone())
            .collect();
        for name in outer {
            kinds[id].insert(name, SymKind::Outer);
        }
    }
    let mut scopes = Vec::with_capacity(n);
    let mut by_offset = BTreeMap::new();
    for (id, raw) in raws.into_iter().enumerate() {
        let symbols = std::mem::take(&mut kinds[id]);
        let mut free_order: Vec<String> = symbols
            .iter()
            .filter(|(_, k)| **k == SymKind::Free)
            .map(|(name, _)| name.strip_prefix(PASS_THROUGH).unwrap_or(name).to_owned())
            .collect();
        free_order.sort();
        free_order.dedup();
        if raw.kind != ScopeKind::Module && by_offset.insert(raw.offset, id).is_some() {
            return Err(format!(
                "Python: internal: two scopes share the source range {:?}",
                raw.offset
            ));
        }
        scopes.push(Scope {
            kind: raw.kind,
            name: raw.name,
            symbols,
            params: raw.params,
            free_order,
            is_generator: raw.is_generator,
            deleted: std::mem::take(&mut deleted[id]),
            uses_class_cell: raw.uses_class_cell,
            walrus_targets: raw.walrus_targets,
            inline: inline[id],
            offset: raw.offset,
            parent: raw.parent,
            children: raw.children,
        });
    }
    Ok(SymTable { scopes, by_offset })
}

fn binds_in_enclosing_function(raws: &[Raw], id: usize, name: &str) -> bool {
    let mut cur = raws[id].parent;
    while let Some(p) = cur {
        match raws[p].kind {
            ScopeKind::Module => return false,
            ScopeKind::Class => {}
            ScopeKind::Function => {
                if raws[p].globals.contains(name) {
                    return false;
                }
                if raws[p].bound.contains(name) || raws[p].nonlocals.contains(name) {
                    return true;
                }
            }
        }
        cur = raws[p].parent;
    }
    false
}

fn enclosing_class(raws: &[Raw], id: usize) -> Option<usize> {
    let mut cur = raws[id].parent;
    while let Some(p) = cur {
        if raws[p].kind == ScopeKind::Class {
            return Some(p);
        }
        cur = raws[p].parent;
    }
    None
}
