//! Free-variable analysis for closures.
//!
//! Two questions drive closure compilation, both answered by a pure walk of the
//! AST (no register/compiler state):
//!
//! * **`free_vars(fn)`** — names a function references but does not bind itself,
//!   propagated up through nested functions. A name free in an inner function
//!   but not bound by a middle function stays free in the middle function too,
//!   so every level on the path between a capture and its definition lists it.
//!   This is what lets upvalue sourcing resolve one level at a time.
//!
//! * **`captured_locals(fn)`** — the subset of a function's OWN bindings
//!   (params + local declarations + nested function names) that appear free in
//!   some directly-nested function. Those bindings must be boxed into heap cells
//!   at declaration so the closure and the defining scope share one mutable
//!   slot. Bindings not captured stay in plain registers (the fast path).
//!
//! The walk covers the compiled subset; an unknown node simply contributes no
//! bindings/refs, which is conservative-safe (a missed capture would surface as
//! a failing test, not silent corruption, because resolution falls back to a
//! global lookup).

use std::collections::HashSet;

use crate::parse::ast::{
    Arg, ArrayElem, Arrow, ArrowBody, Class, ClassMember, Expr, ForInit, ForTarget, Function,
    MemberProp, ObjectMember, Params, Pattern, PropKey, Stmt, Target,
};

/// Names a function body references but does not bind (propagated through nested
/// functions). `params` are the function's parameters.
pub fn free_vars(params: &[String], body: &[Stmt]) -> HashSet<String> {
    let mut refs = HashSet::new();
    let mut bound: HashSet<String> = params.iter().cloned().collect();
    collect_bound_in_body(body, &mut bound);
    for s in body {
        stmt_refs(s, &mut refs);
    }
    refs.retain(|n| !bound.contains(n));
    refs
}

/// The same, for an ARROW body, which may be a bare expression.
///
/// oxc funnelled `x => x + 1` through a `FunctionBody` holding exactly one
/// synthetic `ExpressionStatement`, so the callers said `&a.body.statements` and
/// got the expression walked as a statement. `ArrowBody::Expr` says it directly
/// and there is no statement list to hand over, hence the extra entry point.
/// The expression arm is byte-identical to what the synthetic statement gave:
/// an expression body declares nothing, so only `params` are bound.
pub fn free_vars_arrow(params: &[String], body: &ArrowBody) -> HashSet<String> {
    match body {
        ArrowBody::Block(b) => free_vars(params, &b.stmts),
        ArrowBody::Expr(e) => {
            let mut refs = HashSet::new();
            let bound: HashSet<String> = params.iter().cloned().collect();
            expr_refs(e, &mut refs);
            refs.retain(|n| !bound.contains(n));
            refs
        }
    }
}

/// Every name an EXPRESSION references, ignoring what it binds internally.
/// Deliberately over-approximate (a name bound by a nested function still shows
/// up): the one caller uses it as a "could this possibly see X?" guard.
pub fn expr_refs_all(e: &Expr) -> HashSet<String> {
    let mut refs = HashSet::new();
    expr_refs(e, &mut refs);
    refs
}

/// Whether any parameter DEFAULT expression references `name` (free) — used
/// to detect a possible direct eval in the parameter scope.
pub fn params_reference(name: &str, params: &Params) -> bool {
    let mut refs = HashSet::new();
    // A parameter default is `Pattern::Assign` and the rest parameter is
    // `Pattern::Rest`, both INSIDE `items` — oxc kept the initializer on
    // `FormalParameter::initializer` and the rest in its own field, so the two
    // side channels the old code had to read separately are now one list.
    for item in &params.items {
        pattern_init_refs(item, &mut refs);
    }
    refs.contains(name)
}

/// Nested functions living inside a binding pattern's DEFAULT expressions.
///
/// Mirrors `pattern_init_refs`, but feeds `collect_nested_free_expr` so a
/// function written as a default (`const {f = () => x} = o`) has its own free
/// names accounted for.
fn collect_nested_free_pattern(pat: &Pattern, out: &mut HashSet<String>) {
    match pat {
        Pattern::Ident(_) => {}
        Pattern::Assign { left, right } => {
            collect_nested_free_expr(right, out);
            collect_nested_free_pattern(left, out);
        }
        Pattern::Rest(inner) => collect_nested_free_pattern(inner, out),
        Pattern::Object { props, rest } => {
            for prop in props {
                if let PropKey::Computed(ke) = &prop.key {
                    collect_nested_free_expr(ke, out);
                }
                collect_nested_free_pattern(&prop.value, out);
            }
            if let Some(rest) = rest {
                collect_nested_free_pattern(rest, out);
            }
        }
        Pattern::Array(elems) => {
            for el in elems.iter().flatten() {
                collect_nested_free_pattern(&el.pat, out);
            }
        }
    }
}

/// Collect every name referenced by a DEFAULT-VALUE expression nested anywhere
/// inside a binding pattern (array/object destructuring element defaults).
fn pattern_init_refs(pat: &Pattern, out: &mut HashSet<String>) {
    match pat {
        Pattern::Ident(_) => {}
        Pattern::Assign { left, right } => {
            expr_refs(right, out);
            pattern_init_refs(left, out);
        }
        Pattern::Rest(inner) => pattern_init_refs(inner, out),
        Pattern::Object { props, rest } => {
            for prop in props {
                // A computed key `{[expr]: v}` evaluates in the param scope —
                // an eval there introduces vars like an element default does.
                if let PropKey::Computed(ke) = &prop.key {
                    expr_refs(ke, out);
                }
                pattern_init_refs(&prop.value, out);
            }
            if let Some(rest) = rest {
                pattern_init_refs(rest, out);
            }
        }
        Pattern::Array(elems) => {
            // A hole is `None`; the rest element, if any, is the last entry and
            // is a `Pattern::Rest` handled by the arm above.
            for el in elems.iter().flatten() {
                pattern_init_refs(&el.pat, out);
            }
        }
    }
}

/// Names a parameter list's DEFAULT VALUES reference freely.
///
/// A default is evaluated in the callee's own scope but can close over the
/// ENCLOSING one, so `function outer(){ const G=1; function i(r=G){} }` makes
/// `G` a captured variable of `outer` just as surely as reading it in the body
/// would. `fn_node_free`/`arrow_free` used to scan only the body, so a name
/// referenced ONLY from a default was never boxed into a cell, the nested
/// function had nothing to capture, and the default threw "G is not defined" at
/// runtime. Minified bundles hit this constantly — `function J(r=G){...}` is a
/// standard shape for a defaulted config argument.
///
/// The parameter names themselves are treated as bound: a default referring to
/// an earlier parameter is not a capture of anything outer.
fn params_free(p: &Params, bound: &[String], out: &mut HashSet<String>) {
    // `pattern_init_refs` already walks a Pattern for every expression that
    // evaluates in the parameter scope — defaults (`Pattern::Assign.right`) at
    // any depth, and computed keys in object patterns. In this AST a defaulted
    // parameter IS a `Pattern::Assign` inside `items`, so what used to need a
    // separate `initializer` field is one pass.
    let mut refs = HashSet::new();
    for item in &p.items {
        pattern_init_refs(item, &mut refs);
    }
    // The parameter names themselves are bound: a default referring to an
    // earlier parameter captures nothing outer.
    for r in refs {
        if !bound.iter().any(|b| *b == r) {
            out.insert(r);
        }
    }
}

/// The function's own bindings that some directly-nested function captures.
pub fn captured_locals(params: &[String], body: &[Stmt]) -> HashSet<String> {
    let mut bound: HashSet<String> = params.iter().cloned().collect();
    collect_bound_in_body(body, &mut bound);

    // Union of free vars of each directly-nested function.
    let mut nested_free = HashSet::new();
    for s in body {
        collect_nested_free(s, &mut nested_free);
    }
    bound.intersection(&nested_free).cloned().collect()
}

/// The same, for an ARROW body — see [`free_vars_arrow`] for why this exists.
pub fn captured_locals_arrow(params: &[String], body: &ArrowBody) -> HashSet<String> {
    match body {
        ArrowBody::Block(b) => captured_locals(params, &b.stmts),
        ArrowBody::Expr(e) => {
            let bound: HashSet<String> = params.iter().cloned().collect();
            let mut nested_free = HashSet::new();
            collect_nested_free_expr(e, &mut nested_free);
            bound.intersection(&nested_free).cloned().collect()
        }
    }
}

/// True when a nested ARROW (transitively, with no intervening ordinary function
/// — ordinary functions bind their own `arguments`, see `fn_node_free`) references
/// `arguments`. The nearest enclosing ordinary function must then materialize and
/// box its `arguments` object so the arrow can capture it lexically as an upvalue.
pub fn nested_uses_arguments(body: &[Stmt]) -> bool {
    let mut nested_free = HashSet::new();
    for s in body {
        collect_nested_free(s, &mut nested_free);
    }
    nested_free.contains("arguments")
}

/// The same, for the PARAMETER list: a closure written as a parameter default
/// (`function f(a = () => arguments.length)`) is created before the body runs
/// and captures the same `arguments` object, so the binding has to be boxed
/// before `bind_params` — the body-only scan above never sees it.
pub fn params_nested_use_arguments(params: &Params) -> bool {
    let mut nested_free = HashSet::new();
    for item in &params.items {
        collect_nested_free_pattern(item, &mut nested_free);
    }
    nested_free.contains("arguments")
}

// ── bound-name collection (this scope only; does NOT descend into nested fns) ──

fn collect_bound_in_body(body: &[Stmt], out: &mut HashSet<String>) {
    for s in body {
        collect_bound_stmt(s, out);
    }
}

fn collect_bound_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::VarDecl(d) => {
            for decl in &d.decls {
                collect_pattern_names(&decl.id, out);
            }
        }
        Stmt::FnDecl(f) => {
            if let Some(n) = &f.name {
                out.insert(n.to_string());
            }
        }
        // A class declaration is a lexical binding like `let`/`const`: a nested
        // closure that captures it must see it boxed (so a forward-materialised
        // function can hold its cell).
        Stmt::ClassDecl(c) => {
            if let Some(n) = &c.name {
                out.insert(n.to_string());
            }
        }
        // Recurse into nested *statements* (blocks, loops, if) but NOT into
        // nested function bodies — those introduce their own scope.
        Stmt::Block(b) => collect_bound_in_body(b, out),
        Stmt::If { cons, alt, .. } => {
            collect_bound_stmt(cons, out);
            if let Some(a) = alt {
                collect_bound_stmt(a, out);
            }
        }
        Stmt::While { body, .. } => collect_bound_stmt(body, out),
        Stmt::DoWhile { body, .. } => collect_bound_stmt(body, out),
        Stmt::For { init, body, .. } => {
            if let Some(ForInit::Var(d)) = init {
                for decl in &d.decls {
                    collect_pattern_names(&decl.id, out);
                }
            }
            collect_bound_stmt(body, out);
        }
        // for-of / for-in declare their loop variable too, so a closure that
        // captures it must see it boxed.
        Stmt::ForOf { left, body, .. } => {
            if let ForTarget::Var(d) = left {
                for decl in &d.decls {
                    collect_pattern_names(&decl.id, out);
                }
            }
            collect_bound_stmt(body, out);
        }
        Stmt::ForIn { left, body, .. } => {
            if let ForTarget::Var(d) = left {
                for decl in &d.decls {
                    collect_pattern_names(&decl.id, out);
                }
            }
            collect_bound_stmt(body, out);
        }
        // Descend into try/switch/labeled bodies so a binding declared inside one
        // that a nested closure captures is detected (and boxed).
        Stmt::Try { block, handler, finalizer } => {
            collect_bound_in_body(block, out);
            if let Some(h) = handler {
                // The catch PARAMETER is a binding of this scope like any other:
                // omitting it kept the name out of `captured`, so
                // `compile_catch_body`'s `if self.captured.contains(n)` never
                // fired and the register was never boxed — a closure over it
                // then had no cell to capture and threw "e is not defined"
                // (`try{throw 5}catch(e){ f = () => e }`). It only appeared to
                // work when a same-named OUTER binding put the name in the set.
                if let Some(p) = &h.param {
                    collect_pattern_names(p, out);
                }
                collect_bound_in_body(&h.body, out);
            }
            if let Some(f) = finalizer {
                collect_bound_in_body(f, out);
            }
        }
        Stmt::Switch { cases, .. } => {
            for case in cases {
                collect_bound_in_body(&case.body, out);
            }
        }
        Stmt::Labeled { body, .. } => collect_bound_stmt(body, out),
        // `var` declarations inside a `with` body hoist to the enclosing fn.
        Stmt::With { body, .. } => collect_bound_stmt(body, out),
        _ => {}
    }
}

/// Insert every name a binding pattern introduces — recursing through object/
/// array destructuring, defaults (`= d`), and rest elements — so a destructured
/// local captured by a nested closure is detected and boxed.
pub(crate) fn collect_pattern_names(pat: &Pattern, out: &mut HashSet<String>) {
    match pat {
        Pattern::Ident(n) => {
            out.insert(n.to_string());
        }
        Pattern::Assign { left, .. } => collect_pattern_names(left, out),
        Pattern::Rest(inner) => collect_pattern_names(inner, out),
        Pattern::Object { props, rest } => {
            for prop in props {
                collect_pattern_names(&prop.value, out);
            }
            if let Some(rest) = rest {
                collect_pattern_names(rest, out);
            }
        }
        Pattern::Array(elems) => {
            // Holes are `None`; the rest element lives in the list as a
            // `Pattern::Rest`, so one pass covers both.
            for el in elems.iter().flatten() {
                collect_pattern_names(&el.pat, out);
            }
        }
    }
}

// ── reference collection (descends into nested functions) ──

fn stmt_refs(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Expr(e) => expr_refs(e, out),
        // The with OBJECT expression and every reference in the body count
        // (an outer local referenced only inside a with body must be captured).
        Stmt::With { object, body } => {
            expr_refs(object, out);
            stmt_refs(body, out);
        }
        Stmt::VarDecl(d) => {
            for decl in &d.decls {
                if let Some(init) = &decl.init {
                    expr_refs(init, out);
                }
                // Destructuring DEFAULTS read names too: `const {bgColor:m=Cb}=o`
                // references `Cb`. Walking only the initializer missed them, so a
                // name used solely as a pattern default was never captured and
                // threw "not defined" — the same gap parameter defaults had.
                pattern_init_refs(&decl.id, out);
            }
        }
        Stmt::Block(b) => {
            for st in b {
                stmt_refs(st, out);
            }
        }
        Stmt::If { test, cons, alt } => {
            expr_refs(test, out);
            stmt_refs(cons, out);
            if let Some(a) = alt {
                stmt_refs(a, out);
            }
        }
        Stmt::While { test, body } => {
            expr_refs(test, out);
            stmt_refs(body, out);
        }
        Stmt::DoWhile { body, test } => {
            stmt_refs(body, out);
            expr_refs(test, out);
        }
        Stmt::For { init, test, update, body } => {
            if let Some(init) = init {
                match init {
                    ForInit::Var(d) => {
                        for decl in &d.decls {
                            if let Some(i) = &decl.init {
                                expr_refs(i, out);
                            }
                        }
                    }
                    ForInit::Expr(e) => expr_refs(e, out),
                }
            }
            if let Some(t) = test {
                expr_refs(t, out);
            }
            if let Some(u) = update {
                expr_refs(u, out);
            }
            stmt_refs(body, out);
        }
        Stmt::Return(r) => {
            if let Some(a) = r {
                expr_refs(a, out);
            }
        }
        Stmt::FnDecl(f) => {
            // A nested function declaration contributes its OWN free vars
            // (minus what it binds), exactly like a function expression would.
            fn_node_free(f, out);
        }
        Stmt::ForOf { left, right, body, .. } => {
            for_head_refs(left, out);
            expr_refs(right, out);
            stmt_refs(body, out);
        }
        Stmt::ForIn { left, right, body } => {
            for_head_refs(left, out);
            expr_refs(right, out);
            stmt_refs(body, out);
        }
        Stmt::Try { block, handler, finalizer } => {
            for st in block {
                stmt_refs(st, out);
            }
            if let Some(h) = handler {
                for st in &h.body {
                    stmt_refs(st, out);
                }
            }
            if let Some(f) = finalizer {
                for st in f {
                    stmt_refs(st, out);
                }
            }
        }
        Stmt::Switch { disc, cases } => {
            expr_refs(disc, out);
            for case in cases {
                if let Some(t) = &case.test {
                    expr_refs(t, out);
                }
                for st in &case.body {
                    stmt_refs(st, out);
                }
            }
        }
        Stmt::Throw(t) => expr_refs(t, out),
        Stmt::Labeled { body, .. } => stmt_refs(body, out),
        Stmt::ClassDecl(c) => class_free(c, out),
        // NOTE: `Stmt::Import`/`Stmt::Export` fall here and contribute nothing.
        // That is unchanged by the port — oxc flattened the module declarations
        // into `Statement` and they hit the same catch-all, so `export function
        // f(){ … }` never contributed its free vars before either. Preserved
        // deliberately: the gate is byte-identical bytecode.
        _ => {}
    }
}

/// Does `e` reference `name` anywhere?
///
/// Used to decide whether an assignment's right-hand side may observe the
/// target register, which matters when a form writes the destination partway
/// through evaluating itself.
pub(crate) fn references_name(e: &Expr, name: &str) -> bool {
    let mut set = HashSet::new();
    expr_refs(e, &mut set);
    set.contains(name)
}

fn expr_refs(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Ident(n) => {
            if &**n != "undefined" {
                out.insert(n.to_string());
            }
        }
        Expr::Binary { left, right, .. } => {
            expr_refs(left, out);
            expr_refs(right, out);
        }
        Expr::Logical { left, right, .. } => {
            expr_refs(left, out);
            expr_refs(right, out);
        }
        Expr::Unary { arg, .. } => expr_refs(arg, out),
        Expr::Update { target, .. } => target_refs(target, out),
        Expr::Assign { target, value, .. } => {
            target_refs(target, out);
            expr_refs(value, out);
        }
        Expr::Cond { test, cons, alt } => {
            expr_refs(test, out);
            expr_refs(cons, out);
            expr_refs(alt, out);
        }
        Expr::Call(c) => {
            expr_refs(&c.callee, out);
            for arg in &c.args {
                // The SPREAD operand counts too (`f(...xs)`): skipping it would
                // drop a variable referenced ONLY inside a spread, so it would
                // not be captured.
                match arg {
                    Arg::Expr(e) | Arg::Spread(e) => expr_refs(e, out),
                }
            }
        }
        Expr::New { callee, args } => {
            expr_refs(callee, out);
            for arg in args {
                match arg {
                    Arg::Expr(e) | Arg::Spread(e) => expr_refs(e, out),
                }
            }
        }
        Expr::Await(a) => expr_refs(a, out),
        Expr::Yield { arg, .. } => {
            if let Some(a) = arg {
                expr_refs(a, out);
            }
        }
        Expr::Seq(exprs) => {
            for e in exprs {
                expr_refs(e, out);
            }
        }
        Expr::Template(t) => {
            for e in &t.exprs {
                expr_refs(e, out);
            }
        }
        Expr::TaggedTemplate { tag, quasi } => {
            expr_refs(tag, out);
            for e in &quasi.exprs {
                expr_refs(e, out);
            }
        }
        // `a.b`, `a.#b` and `a[b]` are one node now; only the computed form has a
        // key expression to walk.
        Expr::Member(m) => {
            expr_refs(&m.object, out);
            if let MemberProp::Computed(k) = &m.prop {
                expr_refs(k, out);
            }
        }
        Expr::Array(els, _) => {
            // A hole is `None` and references nothing. The spread operand is
            // walked for the same reason as a spread argument.
            for el in els.iter().flatten() {
                match el {
                    ArrayElem::Expr(e) | ArrayElem::Spread(e) => expr_refs(e, out),
                }
            }
        }
        Expr::Object(members, _) => {
            for member in members {
                match member {
                    ObjectMember::Prop { key, value, .. } => {
                        // A computed key `{[expr]: v}` references variables too —
                        // they must be captured, not just the value's.
                        prop_key_refs(key, out);
                        expr_refs(value, out);
                        // NOTE: the CoverInitializedName (`{a = 1}`) initializer is
                        // not walked. It is unreachable in a program that compiles:
                        // as an expression `({a = 1})` is a SyntaxError, and as a
                        // destructuring target the initializer moves to
                        // `TargetProp::default`, which this walk does not reach
                        // either (see `target_refs`). oxc kept it outside the tree
                        // entirely, so this matches pre-port behaviour exactly.
                    }
                    // oxc spelled a method/getter/setter as an ordinary property
                    // whose value was a `FunctionExpression`, so these reached
                    // `fn_node_free` through the value arm above. They are named
                    // shapes now, and must still contribute their free vars.
                    ObjectMember::Method { key, func }
                    | ObjectMember::Get { key, func }
                    | ObjectMember::Set { key, func } => {
                        prop_key_refs(key, out);
                        fn_node_free(func, out);
                    }
                    ObjectMember::Spread(e) => expr_refs(e, out),
                }
            }
        }
        Expr::Function(f) => fn_node_free(f, out),
        Expr::Arrow(a) => arrow_free(a, out),
        Expr::Class(c) => class_free(c, out),
        // An optional chain wraps the whole chain, so without this arm every
        // name inside `r()?.getItem(k)` is invisible to the scan.
        //
        // Under-inclusion here is NOT safe. A missed name is not boxed into a
        // cell, so a nested function has nothing to capture and the reference
        // falls back to a global that does not exist — a ReferenceError, not a
        // slower lookup. That is what "r is not defined" was, thrown from a
        // storage adapter whose methods all reached their captured accessor
        // through `?.`.
        Expr::Chain(inner) => expr_refs(inner, out),
        // NOTE: `Expr::PrivateIn` (`#x in o`) and `Expr::ImportCall` still fall
        // into this catch-all, as they did pre-port.
        _ => {}
    }
}

/// The reads performed by an ASSIGNMENT TARGET — the names/objects an enclosing
/// arrow or function must capture in order to write through it.
///
/// One function for both `=` and `++`: oxc split these across `AssignmentTarget`
/// and `SimpleAssignmentTarget` with identical arms, and the destructuring forms
/// only ever reached the assignment side.
fn target_refs(t: &Target, out: &mut HashSet<String>) {
    match t {
        // Not filtered against "undefined", unlike a read: `undefined = 1` names
        // a binding here. Pre-port behaviour, preserved.
        Target::Ident { name, .. } => {
            out.insert(name.to_string());
        }
        // `o.x = v` / `o.#x = v` / `o[k] = v`: the OBJECT (and key) are reads an
        // enclosing arrow/function must capture.
        Target::Member(m) => {
            expr_refs(&m.object, out);
            if let MemberProp::Computed(k) = &m.prop {
                expr_refs(k, out);
            }
        }
        // Annex B `f() = 1` / `f()++`. The call IS evaluated before the
        // ReferenceError is thrown, so its callee and arguments are genuine
        // reads. NOTE: unreachable before the hand-written parser lands — the
        // engine rewrites the source for this construct today
        // (`annexb_call_target_rewrite`) and oxc cannot build the node — so this
        // arm cannot move any bytecode the differential gate compares.
        Target::Call(c) => {
            expr_refs(&c.callee, out);
            for arg in &c.args {
                match arg {
                    Arg::Expr(e) | Arg::Spread(e) => expr_refs(e, out),
                }
            }
        }
        // A DESTRUCTURING target (`[a] = x`, `({k: o.b} = x)`) assigns every leaf
        // it names and evaluates every member base, computed key and default
        // inside it — all references of the enclosing scope, exactly as if each
        // leaf had been written as its own `=`. Missing them meant a closure that
        // only ever wrote an outer local through a pattern (`() => { [a,b] = v }`)
        // did not capture it and silently wrote a global instead.
        Target::Array(elems) => {
            for el in elems.iter().flatten() {
                target_refs(&el.target, out);
                if let Some(d) = &el.default {
                    expr_refs(d, out);
                }
            }
        }
        Target::Object { props, rest } => {
            for p in props {
                prop_key_refs(&p.key, out);
                target_refs(&p.target, out);
                if let Some(d) = &p.default {
                    expr_refs(d, out);
                }
            }
            if let Some(r) = rest {
                target_refs(r, out);
            }
        }
    }
}

/// Functions nested one level down inside an assignment TARGET — only a
/// default, a member base or a computed key can hold one (`[a = () => x] = []`).
fn target_nested_free(t: &Target, out: &mut HashSet<String>) {
    match t {
        Target::Ident { .. } => {}
        Target::Member(m) => {
            collect_nested_free_expr(&m.object, out);
            if let MemberProp::Computed(k) = &m.prop {
                collect_nested_free_expr(k, out);
            }
        }
        Target::Call(c) => {
            collect_nested_free_expr(&c.callee, out);
            for arg in &c.args {
                match arg {
                    Arg::Expr(e) | Arg::Spread(e) => collect_nested_free_expr(e, out),
                }
            }
        }
        Target::Array(elems) => {
            for el in elems.iter().flatten() {
                target_nested_free(&el.target, out);
                if let Some(d) = &el.default {
                    collect_nested_free_expr(d, out);
                }
            }
        }
        Target::Object { props, rest } => {
            for p in props {
                if let PropKey::Computed(e) = &p.key {
                    collect_nested_free_expr(e, out);
                }
                target_nested_free(&p.target, out);
                if let Some(d) = &p.default {
                    collect_nested_free_expr(d, out);
                }
            }
            if let Some(r) = rest {
                target_nested_free(r, out);
            }
        }
    }
}

/// The references a `for (… in/of …)` HEAD makes. A declaration head BINDS its
/// names (subtracted later) but still READS its defaults; a target head assigns
/// EXISTING bindings once per iteration, which is a reference to each of them —
/// the case `Stmt::ForOf`/`Stmt::ForIn` used to drop entirely.
fn for_head_refs(left: &ForTarget, out: &mut HashSet<String>) {
    match left {
        ForTarget::Var(d) => {
            for decl in &d.decls {
                pattern_init_refs(&decl.id, out);
                // Annex B `for (var x = init in obj)`.
                if let Some(i) = &decl.init {
                    expr_refs(i, out);
                }
            }
        }
        ForTarget::Target(t) => target_refs(t, out),
    }
}

/// `for_head_refs` for the nested-function walk.
fn for_head_nested_free(left: &ForTarget, out: &mut HashSet<String>) {
    match left {
        ForTarget::Var(d) => {
            for decl in &d.decls {
                collect_nested_free_pattern(&decl.id, out);
                if let Some(i) = &decl.init {
                    collect_nested_free_expr(i, out);
                }
            }
        }
        ForTarget::Target(t) => target_nested_free(t, out),
    }
}

/// A computed property key evaluates expressions in the surrounding scope; a
/// literal / private key names nothing.
fn prop_key_refs(k: &PropKey, out: &mut HashSet<String>) {
    if let PropKey::Computed(e) = k {
        expr_refs(e, out);
    }
}

/// Add a nested function's free variables to `out`. The nested function's own
/// bindings are subtracted, so only names it captures from further out remain.
fn fn_node_free(f: &Function, out: &mut HashSet<String>) {
    let mut param_names = param_names(&f.params);
    // An ordinary function BINDS its own `arguments` (and `this`), so a reference
    // to `arguments` inside it is NOT free — it must not leak out as a capture of
    // the enclosing scope. (Arrows, handled by `arrow_free`, do not bind it.)
    param_names.push("arguments".to_string());
    let inner = free_vars(&param_names, &f.body.stmts);
    out.extend(inner);
    params_free(&f.params, &param_names, out);
}

fn arrow_free(a: &Arrow, out: &mut HashSet<String>) {
    let param_names = param_names(&a.params);
    let inner = free_vars_arrow(&param_names, &a.body);
    out.extend(inner);
    params_free(&a.params, &param_names, out);
}

/// Names a class body references from OUTSIDE a method's own scope: each
/// method/getter/setter/constructor's free vars (minus its own params), plus
/// field initializers, computed keys, static blocks, and the `extends`
/// expression. The methods/ctor/field-inits are NESTED functions, so an
/// enclosing local they reference must be boxed — hence this feeds both
/// `captured_locals` (boxing) and `free_vars` (transitive capture). Over-
/// inclusion is harmless: it at most boxes an enclosing local used only
/// directly, which is transparent.
fn class_free(class: &Class, out: &mut HashSet<String>) {
    if let Some(sc) = &class.superclass {
        expr_refs(sc, out);
    }
    for el in &class.body {
        match el {
            ClassMember::Method(m) => {
                fn_node_free(&m.func, out);
                prop_key_refs(&m.key, out);
            }
            ClassMember::Field(p) => {
                if let Some(v) = &p.value {
                    expr_refs(v, out);
                }
                prop_key_refs(&p.key, out);
            }
            ClassMember::StaticBlock(b) => {
                out.extend(free_vars(&[], b));
            }
        }
    }
}

/// Union of free vars of functions nested DIRECTLY in `s` (one level down).
fn collect_nested_free(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Expr(e) => collect_nested_free_expr(e, out),
        Stmt::VarDecl(d) => {
            for decl in &d.decls {
                if let Some(init) = &decl.init {
                    collect_nested_free_expr(init, out);
                }
                // A default can itself contain a function
                // (`const {f = () => x} = o`), whose free names must be seen.
                collect_nested_free_pattern(&decl.id, out);
            }
        }
        Stmt::Block(b) => {
            for st in b {
                collect_nested_free(st, out);
            }
        }
        Stmt::If { test, cons, alt } => {
            collect_nested_free_expr(test, out);
            collect_nested_free(cons, out);
            if let Some(a) = alt {
                collect_nested_free(a, out);
            }
        }
        Stmt::While { test, body } => {
            collect_nested_free_expr(test, out);
            collect_nested_free(body, out);
        }
        Stmt::With { object, body } => {
            collect_nested_free_expr(object, out);
            collect_nested_free(body, out);
        }
        Stmt::DoWhile { body, test } => {
            collect_nested_free(body, out);
            collect_nested_free_expr(test, out);
        }
        Stmt::For { init, test, update, body } => {
            // A closure in the INIT (`for (let i = 0, f = () => i; …)`)
            // captures head bindings too — scan declarator initializers.
            if let Some(init) = init {
                match init {
                    ForInit::Var(d) => {
                        for decl in &d.decls {
                            if let Some(i) = &decl.init {
                                collect_nested_free_expr(i, out);
                            }
                        }
                    }
                    ForInit::Expr(e) => collect_nested_free_expr(e, out),
                }
            }
            if let Some(t) = test {
                collect_nested_free_expr(t, out);
            }
            if let Some(u) = update {
                collect_nested_free_expr(u, out);
            }
            collect_nested_free(body, out);
        }
        Stmt::ForOf { left, right, body, .. } => {
            for_head_nested_free(left, out);
            collect_nested_free_expr(right, out);
            collect_nested_free(body, out);
        }
        Stmt::ForIn { left, right, body } => {
            for_head_nested_free(left, out);
            collect_nested_free_expr(right, out);
            collect_nested_free(body, out);
        }
        Stmt::Return(r) => {
            if let Some(a) = r {
                collect_nested_free_expr(a, out);
            }
        }
        Stmt::FnDecl(f) => fn_node_free(f, out),
        Stmt::Try { block, handler, finalizer } => {
            for st in block {
                collect_nested_free(st, out);
            }
            if let Some(h) = handler {
                for st in &h.body {
                    collect_nested_free(st, out);
                }
            }
            if let Some(f) = finalizer {
                for st in f {
                    collect_nested_free(st, out);
                }
            }
        }
        Stmt::Switch { disc, cases } => {
            collect_nested_free_expr(disc, out);
            for case in cases {
                if let Some(t) = &case.test {
                    collect_nested_free_expr(t, out);
                }
                for st in &case.body {
                    collect_nested_free(st, out);
                }
            }
        }
        Stmt::Throw(t) => collect_nested_free_expr(t, out),
        Stmt::Labeled { body, .. } => collect_nested_free(body, out),
        Stmt::ClassDecl(c) => class_free(c, out),
        _ => {}
    }
}

fn collect_nested_free_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Function(f) => fn_node_free(f, out),
        Expr::Arrow(a) => arrow_free(a, out),
        Expr::Class(c) => class_free(c, out),
        Expr::Binary { left, right, .. } => {
            collect_nested_free_expr(left, out);
            collect_nested_free_expr(right, out);
        }
        Expr::Logical { left, right, .. } => {
            collect_nested_free_expr(left, out);
            collect_nested_free_expr(right, out);
        }
        Expr::Unary { arg, .. } => collect_nested_free_expr(arg, out),
        // Only the VALUE side: a target cannot hold a function literal that is
        // not already inside one of its own key expressions, which this walk did
        // not visit before the port either.
        // The TARGET too: `[a = () => x] = []` nests a function in a default.
        Expr::Assign { target, value, .. } => {
            target_nested_free(target, out);
            collect_nested_free_expr(value, out);
        }
        Expr::Cond { test, cons, alt } => {
            collect_nested_free_expr(test, out);
            collect_nested_free_expr(cons, out);
            collect_nested_free_expr(alt, out);
        }
        Expr::Call(c) => {
            collect_nested_free_expr(&c.callee, out);
            for arg in &c.args {
                // NOTE: a SPREAD argument is skipped, unlike in `expr_refs` where
                // it is walked. That asymmetry predates the port (this walk used
                // oxc's `Argument::as_expression`, which returns `None` for a
                // spread, while `expr_refs` used a helper that unwrapped it), and
                // widening it here would box additional locals.
                if let Arg::Expr(e) = arg {
                    collect_nested_free_expr(e, out);
                }
            }
        }
        Expr::New { callee, args } => {
            collect_nested_free_expr(callee, out);
            for arg in args {
                if let Arg::Expr(e) = arg {
                    collect_nested_free_expr(e, out);
                }
            }
        }
        Expr::Await(a) => collect_nested_free_expr(a, out),
        Expr::Yield { arg, .. } => {
            if let Some(a) = arg {
                collect_nested_free_expr(a, out);
            }
        }
        Expr::Seq(exprs) => {
            for e in exprs {
                collect_nested_free_expr(e, out);
            }
        }
        Expr::Template(t) => {
            for e in &t.exprs {
                collect_nested_free_expr(e, out);
            }
        }
        Expr::TaggedTemplate { tag, quasi } => {
            collect_nested_free_expr(tag, out);
            for e in &quasi.exprs {
                collect_nested_free_expr(e, out);
            }
        }
        Expr::Member(m) => {
            collect_nested_free_expr(&m.object, out);
            if let MemberProp::Computed(k) = &m.prop {
                collect_nested_free_expr(k, out);
            }
        }
        Expr::Array(els, _) => {
            for el in els.iter().flatten() {
                // Spread skipped, as for a call argument — same pre-port
                // asymmetry, same reason.
                if let ArrayElem::Expr(e) = el {
                    collect_nested_free_expr(e, out);
                }
            }
        }
        Expr::Object(members, _) => {
            for member in members {
                match member {
                    ObjectMember::Prop { key, value, .. } => {
                        if let PropKey::Computed(ke) = key {
                            collect_nested_free_expr(ke, out);
                        }
                        collect_nested_free_expr(value, out);
                    }
                    // Reached through the property VALUE before the port, when a
                    // method was an `ObjectProperty` holding a `FunctionExpression`.
                    ObjectMember::Method { key, func }
                    | ObjectMember::Get { key, func }
                    | ObjectMember::Set { key, func } => {
                        if let PropKey::Computed(ke) = key {
                            collect_nested_free_expr(ke, out);
                        }
                        fn_node_free(func, out);
                    }
                    ObjectMember::Spread(e) => collect_nested_free_expr(e, out),
                }
            }
        }
        // Same as in `expr_refs`: a nested function inside an optional chain
        // must still be scanned, or the names it captures go unseen.
        Expr::Chain(inner) => collect_nested_free_expr(inner, out),
        // NOTE: `Expr::PrivateIn` / `Expr::ImportCall` still fall into this
        // catch-all, as they did pre-port.
        _ => {}
    }
}

fn param_names(p: &Params) -> Vec<String> {
    // Every name a parameter list binds — plain identifiers, destructuring-pattern
    // leaves, and the rest parameter — so a nested function's own params shadow
    // outer bindings (they must NOT be reported as free / captured). Defaults are
    // `Pattern::Assign` and the rest parameter is `Pattern::Rest`, both inside
    // `items`, so one pass covers what used to need three.
    let mut set = HashSet::new();
    for item in &p.items {
        collect_pattern_names(item, &mut set);
    }
    set.into_iter().collect()
}
